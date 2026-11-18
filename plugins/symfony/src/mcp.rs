//! Read-only project tools over the cached source index, as compact CSV. Nothing here runs PHP;
//! profiler requests come from the CSV index only and show the same redacted URLs as the tool window.
use std::collections::BTreeMap;

use crate::*;
use serde::Deserialize;

const MAX_LIMIT: usize = 200;
const USAGE_FILES: usize = 3000;
const USAGE_BYTES: usize = 4 * 1024 * 1024;
const USAGE_TAGS: [&str; 9] = ["extends", "include", "embed", "import", "from", "use", "form_theme", "include(", "source("];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Listing {
    #[serde(default)]
    query: String,
    #[serde(default)]
    offset: usize,
    #[serde(default = "default_limit")]
    limit: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Usage {
    template: String,
}

fn default_limit() -> usize {
    100
}

fn decode<T: serde::de::DeserializeOwned>(arguments: &str) -> Result<T, Error> {
    if arguments.len() > 16 * 1024 {
        return Err(Error::Invalid("Tool arguments exceed 16 KiB.".into()));
    }
    let value = strict_json::parse(if arguments.trim().is_empty() { "{}" } else { arguments }, 16 * 1024)?;
    serde_json::from_value(value).map_err(|error| Error::Invalid(error.to_string()))
}

fn cell(value: &str) -> String {
    let clean: String = value.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    match clean.contains([',', '"']) {
        true => format!("\"{}\"", clean.replace('"', "\"\"")),
        false => clean,
    }
}

/// A page of rows as CSV with a header, and a trailing note when more rows match.
fn table(columns: &[&str], rows: Vec<Vec<String>>, request: &Listing) -> Result<String, Error> {
    if request.limit == 0 || request.limit > MAX_LIMIT || request.offset > 10_000 || request.query.len() > 200 {
        return Err(Error::Invalid("Invalid pagination or query.".into()));
    }
    let needle = request.query.to_lowercase();
    let matching: Vec<&Vec<String>> = rows.iter().filter(|row| needle.is_empty() || row.iter().any(|value| value.to_lowercase().contains(&needle))).collect();
    let mut out = columns.join(",");
    for row in matching.iter().skip(request.offset).take(request.limit) {
        out.push('\n');
        out.push_str(&row.iter().map(|value| cell(value)).collect::<Vec<_>>().join(","));
    }
    let shown_to = request.offset + request.limit;
    if matching.len() > shown_to {
        out.push_str(&format!("\n# {} more rows; call again with offset={shown_to}", matching.len() - shown_to));
    }
    Ok(out)
}

fn relative(root: &str, path: &str) -> String {
    path.strip_prefix(&format!("{root}/")).unwrap_or(path).to_owned()
}

impl Main {
    fn route_rows(&self, index: &Index, root: &str) -> Vec<Vec<String>> {
        let mut grouped: BTreeMap<(String, String, String, String), Vec<String>> = BTreeMap::new();
        for route in self.routes(index) {
            let controller = match &route.handler.member {
                Some(member) => format!("{}::{member}", route.handler.class),
                None => route.handler.class.clone(),
            };
            let location = route.location.as_ref().map(|location| format!("{}:{}", relative(root, &location.path), location.range.start.line + 1)).unwrap_or_default();
            grouped.entry((route.name.clone().unwrap_or_default(), route.path.clone(), controller, location)).or_default().push(route.method.clone());
        }
        grouped.into_iter().map(|((name, path, controller, location), methods)| vec![name, methods.join("|"), path, controller, location]).collect()
    }

    fn name_rows(names: &Names, root: &str, with_detail: bool) -> Vec<Vec<String>> {
        names
            .iter()
            .map(|(name, entry)| {
                let location = match entry.path.is_empty() {
                    true => String::new(),
                    false => format!("{}:{}", relative(root, &entry.path), entry.at.line + 1),
                };
                match with_detail {
                    true => vec![name.clone(), entry.detail.clone().unwrap_or_default(), location],
                    false => vec![name.clone(), location],
                }
            })
            .collect()
    }

    fn entity_rows(index: &Index, root: &str) -> Vec<Vec<String>> {
        index
            .entities
            .values()
            .map(|entity| {
                let fields = entity.fields.iter().map(|field| format!("{}:{}{}{}", field.name, field.type_name, if field.nullable { "?" } else { "" }, if field.id { "#id" } else { "" })).collect::<Vec<_>>().join("|");
                let relations = entity.relations.iter().map(|relation| format!("{}:{}>{}", relation.name, relation.kind, relation.target.as_deref().unwrap_or("?"))).collect::<Vec<_>>().join("|");
                vec![
                    entity.class.clone(),
                    entity.table.clone(),
                    entity.repository.clone().unwrap_or_default(),
                    fields,
                    relations,
                    format!("{}:{}", relative(root, &entity.entry.path), entity.entry.at.line + 1),
                ]
            })
            .collect()
    }

    fn usage_rows(&self, index: &Index, project: &Project, root: &str, template: &str) -> Result<Vec<Vec<String>>, Error> {
        if !index.templates.contains_key(template) {
            return Err(Error::NotFound(format!("template {template}")));
        }
        let mut rows = Vec::new();
        for render in index.renders.iter().filter(|render| render.template == template) {
            rows.push(vec!["controller".into(), render.caller.clone(), format!("{}:{}", relative(root, &render.path), render.at.line + 1)]);
        }
        for include in index.includes.iter().filter(|include| include.target == template) {
            rows.push(vec!["include-with".into(), include.caller.clone(), String::new()]);
        }

        let (mut files, mut bytes) = (0usize, 0usize);
        let quoted = [format!("'{template}'"), format!("\"{template}\"")];
        for (name, entry) in index.templates.iter().filter(|(_, entry)| !entry.path.is_empty() && entry.path.starts_with(root)) {
            if files >= USAGE_FILES || bytes >= USAGE_BYTES || name == template {
                continue;
            }
            files += 1;
            let Ok(text) = project.read_file(&entry.path) else { continue };
            bytes += text.len();
            for (number, line) in text.lines().enumerate() {
                let Some(at) = quoted.iter().find_map(|quote| line.find(quote.as_str())) else { continue };
                if let Some(kind) = USAGE_TAGS.iter().rev().find(|tag| line[..at].contains(**tag)) {
                    rows.push(vec![kind.trim_end_matches('(').to_owned(), name.clone(), format!("{}:{}", relative(root, &entry.path), number + 1)]);
                }
            }
        }
        Ok(rows)
    }

    fn call_tool(&mut self, tool: &str, arguments: &str, ctx: &Invocation) -> Result<String, Error> {
        let project = ctx.project().ok_or("Open a Symfony project first.")?;
        let root = project.root()?;
        if root.is_empty() || !host::projects()?.iter().any(|candidate| candidate.root().ok().as_deref() == Some(root.as_str())) {
            return Err(Error::Stale);
        }

        if tool == "profiler_requests" {
            let request: Listing = decode(arguments)?;
            self.profiler_snapshot.refresh(ctx);
            let rows = self.profiler_snapshot.requests().ok_or("No profiler requests loaded. Choose a profiler source first.")?;
            let rows = rows.into_iter().map(|(token, method, url, status, time)| vec![token, method, url, status, profiler::timestamp(time)]).collect();
            return table(&["token", "method", "url", "status", "time_utc"], rows, &request);
        }

        let index = self.view_index(ctx).ok_or("Index this Symfony project before using its tools.")?;
        match tool {
            "routes" => table(&["name", "methods", "path", "controller", "file"], self.route_rows(index, &root), &decode(arguments)?),
            "services" => table(&["id", "class", "file"], Self::name_rows(&index.services, &root, true), &decode(arguments)?),
            "commands" => table(&["name", "class", "file"], Self::name_rows(&index.commands, &root, true), &decode(arguments)?),
            "templates" => table(&["name", "file"], Self::name_rows(&index.templates, &root, false), &decode(arguments)?),
            "parameters" => table(&["name", "file"], Self::name_rows(&index.parameters, &root, false), &decode(arguments)?),
            "env" => table(&["name", "defined_in", "file"], Self::name_rows(&index.env, &root, true), &decode(arguments)?),
            "entities" => table(&["class", "table", "repository", "fields", "relations", "file"], Self::entity_rows(index, &root), &decode(arguments)?),
            "template_usage" => {
                let request: Usage = decode(arguments)?;
                let rows = self.usage_rows(index, &project, &root, &request.template)?;
                table(&["kind", "from", "file"], rows, &Listing { query: String::new(), offset: 0, limit: MAX_LIMIT })
            }
            _ => Err(Error::NotFound(format!("tool {tool}"))),
        }
    }
}

impl openidea_plugin::McpProvider for Main {
    fn call(&mut self, tool: &str, arguments: &str, ctx: &Invocation) -> Result<String, Error> {
        self.call_tool(tool, arguments, ctx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::McpProvider;
    use openidea_plugin::mock::MockHost;

    fn setup() -> (MockHost, Main) {
        let host = MockHost::install();
        host.add_file("composer.json", r#"{"require": {"symfony/framework-bundle": "^7.0"}, "autoload": {"psr-4": {"App\\": "src/"}}}"#);
        host.add_file("templates/base.html.twig", "<html>{% block body %}{% endblock %}</html>");
        host.add_file("templates/blog/index.html.twig", "{% extends 'base.html.twig' %}\n{% include 'blog/_card.html.twig' %}\n");
        host.add_file("templates/blog/_card.html.twig", "{{ include('base.html.twig') }}\n");
        host.add_file("config/services.yaml", "services:\n    app.mailer:\n        class: App\\Service\\Mailer\n");
        host.add_file("src/Command/SyncCommand.php", "<?php\nnamespace App\\Command;\n#[AsCommand(name: 'app:sync')]\nclass SyncCommand {}\n");
        host.add_file(
            "src/Controller/BlogController.php",
            "<?php\nnamespace App\\Controller;\nclass BlogController\n{\n    #[Route('/blog', name: 'blog_index', methods: ['GET', 'POST'])]\n    public function index()\n    {\n        return $this->render('blog/index.html.twig', []);\n    }\n}\n",
        );
        let mut plugin = Main::default();
        plugin.activate(&host.context("aritti.symfony")).unwrap();
        (host, plugin)
    }

    #[test]
    fn inventories_are_compact_csv_with_filters_and_pagination() {
        let (host, mut plugin) = setup();
        let ctx = host.invocation();
        let routes = plugin.call("routes", "{}", &ctx).unwrap();
        assert!(routes.starts_with("name,methods,path,controller,file\nblog_index,GET|POST,/blog,BlogController::index,"), "{routes}");
        assert!(plugin.call("services", r#"{"query":"mailer"}"#, &ctx).unwrap().contains("app.mailer,App\\Service\\Mailer,config/services.yaml:2"));
        assert!(plugin.call("commands", "{}", &ctx).unwrap().contains("app:sync"));
        let templates = plugin.call("templates", r#"{"limit":1}"#, &ctx).unwrap();
        assert_eq!(templates.lines().count(), 3, "{templates}");
        assert!(templates.ends_with("# 2 more rows; call again with offset=1"), "{templates}");
        assert!(plugin.call("routes", r#"{"limit":0}"#, &ctx).is_err());
        assert!(plugin.call("routes", r#"{"unknown":1}"#, &ctx).is_err());
        assert!(plugin.call("nope", "{}", &ctx).is_err());
    }

    #[test]
    fn template_usage_lists_controllers_and_template_references_by_tag() {
        let (host, mut plugin) = setup();
        let ctx = host.invocation();
        let usage = plugin.call("template_usage", r#"{"template":"blog/index.html.twig"}"#, &ctx).unwrap();
        assert!(usage.contains("controller,BlogController::index,src/Controller/BlogController.php:8"), "{usage}");
        let base = plugin.call("template_usage", r#"{"template":"base.html.twig"}"#, &ctx).unwrap();
        assert!(base.contains("extends,blog/index.html.twig,templates/blog/index.html.twig:1"), "{base}");
        assert!(base.contains("include,blog/_card.html.twig,templates/blog/_card.html.twig:1"), "{base}");
        assert!(plugin.call("template_usage", r#"{"template":"missing.twig"}"#, &ctx).is_err());
    }

    #[test]
    fn a_project_that_is_not_symfony_has_no_tools() {
        let host = MockHost::install();
        host.add_file("composer.json", r#"{"require": {"laravel/framework": "^11.0"}}"#);
        let mut plugin = Main::default();
        plugin.activate(&host.context("aritti.symfony")).unwrap();
        assert!(plugin.call("routes", "{}", &host.invocation()).is_err());
    }

    #[test]
    fn entities_parameters_and_env_names_are_listed_without_values() {
        let (host, mut plugin) = setup();
        host.add_file("src/Entity/Post.php", "<?php\nnamespace App\\Entity;\nuse Doctrine\\ORM\\Mapping as ORM;\n#[ORM\\Entity]\nclass Post\n{\n    #[ORM\\Id, ORM\\Column]\n    private ?int $id = null;\n    #[ORM\\ManyToOne(targetEntity: User::class)]\n    private ?User $author = null;\n}\n");
        host.add_file("config/packages/params.yaml", "parameters:\n    app.name: Blog\n");
        host.add_file(".env", "DATABASE_URL=postgresql://user:secret@db/app\n");
        plugin.run_command("reindex", &host.invocation(), None).unwrap();
        let ctx = host.invocation();
        let entities = plugin.call("entities", "{}", &ctx).unwrap();
        assert!(entities.contains("App\\Entity\\Post,post,,id:int#id,author:ManyToOne>App\\Entity\\User,src/Entity/Post.php:5"), "{entities}");
        assert!(plugin.call("parameters", r#"{"query":"app.name"}"#, &ctx).unwrap().contains("app.name,config/packages/params.yaml:2"));
        let env = plugin.call("env", "{}", &ctx).unwrap();
        assert!(env.contains("DATABASE_URL,defined in .env,.env:1") && !env.contains("secret"), "{env}");
    }
}
