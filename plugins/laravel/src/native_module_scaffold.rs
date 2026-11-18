//! Native reviewed module/package scaffolds; Composer, PHP and module activation
//! never run here. nWidart shape is pinned to its public v12 layout:
//! https://laravelmodules.com/docs/12/basic-usage/creating-a-module
//! https://laravelmodules.com/docs/12/getting-started/installation-and-setup
//! Laravel package discovery: https://laravel.com/docs/12.x/packages
use crate::{generator_plan, generator_presets};
use openidea_plugin::{
    Error, FileKind, Invocation, Position, Project, Range, TextEdit,
    file_actions::{FileAction, FileOperation},
    host,
    providers::Document,
    text,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
const CONFIG: &str = ".openidea/laravel-generators.json";
const LIMIT: usize = 128 * 1024;
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Input {
    kind: String,
    name: String,
    root: String,
    namespace: String,
    package: String,
    laravel_major: u32,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Change {
    path: String,
    before: Option<String>,
    after: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Plan {
    changes: Vec<Change>,
    dependencies: BTreeMap<String, Option<String>>,
    instructions: Vec<String>,
}
struct Pending {
    key: String,
    input: Input,
    plan: Plan,
    anchor: String,
    anchor_text: String,
}
#[derive(Default)]
pub(crate) struct State {
    next: u64,
    pending: BTreeMap<String, Pending>,
}
fn owned(root: &str, path: &str) -> bool {
    host::project_of(path)
        .and_then(|project| project.root().ok())
        .as_deref()
        == Some(root)
}
fn owner(ctx: &Invocation) -> Result<(Project, String), Error> {
    let project = ctx.project().ok_or(Error::Stale)?;
    let root = project.root()?;
    if root.is_empty()
        || root.chars().any(char::is_control)
        || !host::projects()?
            .iter()
            .any(|p| p.root().ok().as_deref() == Some(&root))
    {
        return Err(Error::Stale);
    }
    Ok((project, root))
}
fn parents(project: &Project, root: &str, relative: &str) -> Result<(), Error> {
    if !generator_plan::relative(relative) {
        return Err(Error::Invalid(
            "Scaffold paths must be safe project-relative paths".into(),
        ));
    }
    let mut prefix = String::new();
    for component in relative
        .split('/')
        .take(relative.split('/').count().saturating_sub(1))
    {
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(component);
        if !owned(root, &format!("{root}/{prefix}")) {
            return Err(Error::Stale);
        }
        match project.stat(&prefix) {
            Ok(stat) if stat.kind != FileKind::Directory => {
                return Err(Error::Invalid(format!(
                    "{prefix} must be an ordinary directory"
                )));
            }
            Err(Error::NotFound(_)) => {}
            Err(error) => return Err(error),
            _ => {}
        }
    }
    Ok(())
}
fn read(project: &Project, root: &str, relative: &str) -> Result<Option<String>, Error> {
    parents(project, root, relative)?;
    if !owned(root, &format!("{root}/{relative}")) {
        return Err(Error::Stale);
    }
    match project.stat(relative) {
        Err(Error::NotFound(_)) => Ok(None),
        Err(error) => Err(error),
        Ok(stat) if stat.kind == FileKind::File => {
            let value = project.read_file(relative)?;
            let limit = if relative == "composer.lock" {
                4 * 1024 * 1024
            } else {
                LIMIT
            };
            if value.len() > limit {
                return Err(Error::Invalid(format!("{relative} exceeds 128 KiB")));
            }
            Ok(Some(value))
        }
        Ok(_) => Err(Error::Invalid(format!("{relative} must be a regular file"))),
    }
}
fn package_name(value: &str) -> bool {
    let parts: Vec<_> = value.split('/').collect();
    if parts.len() != 2 || value.len() > 160 {
        return false;
    }
    parts.iter().enumerate().all(|(ordinal, part)| {
        let bytes = part.as_bytes();
        if bytes.is_empty() || !bytes[0].is_ascii_lowercase() && !bytes[0].is_ascii_digit() {
            return false;
        }
        let mut at = 0;
        while at < bytes.len() {
            if bytes[at].is_ascii_lowercase() || bytes[at].is_ascii_digit() {
                at += 1;
                continue;
            }
            if !matches!(bytes[at], b'-' | b'_' | b'.') {
                return false;
            }
            if ordinal == 1 && bytes[at] == b'-' && bytes.get(at + 1) == Some(&b'-') {
                at += 1;
            }
            at += 1;
            if bytes
                .get(at)
                .is_none_or(|next| !next.is_ascii_lowercase() && !next.is_ascii_digit())
            {
                return false;
            }
        }
        true
    })
}

fn pretty(value: &Value) -> Result<String, Error> {
    serde_json::to_string_pretty(value)
        .map(|text| text + "\n")
        .map_err(|error| Error::Invalid(error.to_string()))
}
fn decode(value: Value) -> Result<Input, Error> {
    serde_json::from_value(value).map_err(|error| Error::Invalid(error.to_string()))
}
fn dependency(
    project: &Project,
    root: &str,
    path: &str,
    dependencies: &mut BTreeMap<String, Option<String>>,
) -> Result<Option<String>, Error> {
    let value = read(project, root, path)?;
    dependencies.insert(path.into(), value.clone());
    Ok(value)
}
fn build(project: &Project, root: &str, input: &Input) -> Result<Plan, Error> {
    if !matches!(input.kind.as_str(), "nwidart-v12" | "composer-package")
        || !matches!(input.laravel_major, 12 | 13)
        || !generator_plan::qualified(&input.name)
        || input.name.contains('\\')
        || !input.name.starts_with(|c: char| c.is_ascii_uppercase())
        || !generator_plan::qualified(&input.namespace)
        || input.namespace.starts_with('\\')
        || !package_name(&input.package)
        || !generator_plan::relative(&input.root)
        || input.root.len() > 240
        || input.namespace.len() > 240
    {
        return Err(Error::Invalid("Choose a supported scaffold, Laravel 12/13, Studly module name, explicit namespace, package name and relative root".into()));
    }
    if input.kind == "composer-package" && !input.root.starts_with("packages/") {
        return Err(Error::Invalid(
            "Native Composer packages must use an explicit packages/<directory> root".into(),
        ));
    }
    if input.kind == "nwidart-v12"
        && (input.root != format!("Modules/{}", input.name)
            || input.namespace != format!("Modules\\{}", input.name)
            || input.laravel_major != 12)
    {
        return Err(Error::Invalid("Native nWidart v12 scaffolds support the default Modules/Name, Modules\\Name, Laravel 12 layout only".into()));
    }
    parents(project, root, &format!("{}/composer.json", input.root))?;
    if !owned(root, &format!("{root}/{}", input.root)) {
        return Err(Error::Stale);
    }
    match project.stat(&input.root) {
        Err(Error::NotFound(_)) => {}
        Err(error) => return Err(error),
        Ok(_) => {
            return Err(Error::Invalid(
                "The scaffold root must not already exist".into(),
            ));
        }
    }
    if !project
        .find_files(&format!("{}/**/*", input.root), 1)?
        .is_empty()
    {
        return Err(Error::Invalid(
            "The scaffold root already contains files".into(),
        ));
    }
    let mut dependencies = BTreeMap::new();
    let composer_text = dependency(project, root, "composer.json", &mut dependencies)?.ok_or(
        Error::Invalid("An owned root composer.json is required".into()),
    )?;
    let composer = crate::strict_json::parse(&composer_text, LIMIT)?;
    if !composer.is_object()
        || composer["autoload"]["psr-4"]
            .as_object()
            .is_none_or(|map| map.is_empty())
    {
        return Err(Error::Invalid(
            "Root Composer PSR-4 metadata is required; Composer is never rewritten by this action"
                .into(),
        ));
    }
    let mut instructions = Vec::new();
    if input.kind == "nwidart-v12" {
        let lock_text = dependency(project, root, "composer.lock", &mut dependencies)?.ok_or(
            Error::Invalid(
                "Capture an installed nwidart/laravel-modules 12.x in composer.lock first".into(),
            ),
        )?;
        let lock = crate::strict_json::parse_with_budget(&lock_text, 4 * 1024 * 1024, 200_000)?;
        let packages = lock["packages"]
            .as_array()
            .ok_or(Error::Invalid("Composer lock packages are missing".into()))?;
        let versions: Vec<_> = packages
            .iter()
            .filter(|package| package["name"] == "nwidart/laravel-modules")
            .filter_map(|package| package["version"].as_str())
            .collect();
        if versions.len() != 1
            || !versions[0].trim_start_matches('v').starts_with("12.")
            || composer["require"]["nwidart/laravel-modules"]
                .as_str()
                .is_none()
        {
            return Err(Error::Invalid("Only an explicitly required, locked nWidart 12.x package is supported natively; use captured Artisan forms for other versions".into()));
        }
        if dependency(project, root, "config/modules.php", &mut dependencies)?.is_some() {
            return Err(Error::Invalid("Published modules.php may customize discovery/stubs; use captured module:make rather than guessing its executable configuration".into()));
        }
        let include = composer["extra"]["merge-plugin"]["include"]
            .as_array()
            .is_some_and(|paths| paths.iter().any(|path| path == "Modules/*/composer.json"));
        let allowed =
            composer["config"]["allow-plugins"]["wikimedia/composer-merge-plugin"] == true;
        if !include || !allowed {
            instructions.push("Before loading this module, review root Composer merge-plugin include Modules/*/composer.json and explicitly allow wikimedia/composer-merge-plugin. This action does not alter or enable Composer plugins.".into());
        }
        instructions.push("After reviewing autoload integration, explicitly run composer dump-autoload and enable the module through your nWidart workflow. Root composer.json and module activation statuses are unchanged.".into());
    } else {
        instructions.push(format!("Install {} through a reviewed Composer path repository pointing to {} and an explicit require; package discovery only takes effect after installation. Root composer.json is unchanged.",input.package,input.root));
    }
    let existing = dependency(project, root, CONFIG, &mut dependencies)?;
    let mut config = if let Some(existing) = &existing {
        generator_presets::parse(existing).map_err(Error::Invalid)?;
        crate::strict_json::parse(existing, LIMIT)?
    } else {
        json!({"version":1,"presets":[],"modules":[]})
    };
    if config.get("modules").is_none() {
        config["modules"] = json!([]);
    }
    let modules = config["modules"].as_array_mut().ok_or(Error::Stale)?;
    let id = input.name.to_ascii_lowercase();
    if modules.iter().any(|module| {
        module["id"]
            .as_str()
            .is_some_and(|known| known.eq_ignore_ascii_case(&id))
            || module["namespace"]
                .as_str()
                .is_some_and(|known| known.eq_ignore_ascii_case(&input.namespace))
            || [
                "sourceRoot",
                "migrationRoot",
                "viewRoot",
                "factoryRoot",
                "seederRoot",
            ]
            .iter()
            .any(|field| {
                module[*field].as_str().is_some_and(|known| {
                    let known = known.to_ascii_lowercase();
                    let next = input.root.to_ascii_lowercase();
                    known == next
                        || known.starts_with(&format!("{next}/"))
                        || next.starts_with(&format!("{known}/"))
                })
            })
    }) {
        return Err(Error::Invalid(
            "A module already uses this id, namespace or source root".into(),
        ));
    }
    let folder = if input.kind == "nwidart-v12" {
        "app"
    } else {
        "src"
    };
    let source_root = format!("{}/{folder}", input.root);
    modules.push(json!({"id":id,"label":input.name,"namespace":input.namespace,"sourceRoot":source_root,"migrationRoot":format!("{}/database/migrations",input.root),"viewRoot":format!("{}/resources/views",input.root),"factoryRoot":format!("{}/database/factories",input.root),"factoryNamespace":format!("{}\\Database\\Factories",input.namespace),"seederRoot":format!("{}/database/seeders",input.root),"seederNamespace":format!("{}\\Database\\Seeders",input.namespace)}));
    let next_config = pretty(&config)?;
    generator_presets::parse(&next_config).map_err(Error::Invalid)?;
    let provider = format!(
        "{}\\Providers\\{}ServiceProvider",
        input.namespace, input.name
    );
    let mut psr4 = serde_json::Map::new();
    psr4.insert(
        format!("{}\\", input.namespace),
        json!(format!("{folder}/")),
    );
    psr4.insert(
        format!("{}\\Database\\Factories\\", input.namespace),
        json!("database/factories/"),
    );
    psr4.insert(
        format!("{}\\Database\\Seeders\\", input.namespace),
        json!("database/seeders/"),
    );
    let mut manifest = json!({"name":input.package,"description":format!("{} module",input.name),"type":"library","require":{"illuminate/support":format!("^{}.0",input.laravel_major)},"autoload":{"psr-4":psr4},"autoload-dev":{"psr-4":{format!("{}\\Tests\\",input.namespace):"tests/"}},"extra":{"laravel":{"providers":[],"aliases":{}}}});
    if input.kind == "composer-package" {
        manifest["extra"]["laravel"]["providers"] = json!([provider]);
    }
    let mut outputs = BTreeMap::new();
    outputs.insert("composer.json".to_owned(), pretty(&manifest)?);
    if input.kind == "nwidart-v12" {
        outputs.insert("module.json".into(),pretty(&json!({"name":input.name,"alias":id,"description":"","keywords":[],"priority":0,"providers":[provider],"files":[]}))?);
    }
    let provider_source = format!(
        "<?php\n\ndeclare(strict_types=1);\n\nnamespace {}\\Providers;\n\nuse Illuminate\\Support\\ServiceProvider;\n\nfinal class {}ServiceProvider extends ServiceProvider\n{{\n    public function register(): void\n    {{\n        $this->mergeConfigFrom(__DIR__ . '/../../config/config.php', '{}');\n    }}\n\n    public function boot(): void\n    {{\n        $this->loadRoutesFrom(__DIR__ . '/../../routes/web.php');\n        $this->loadViewsFrom(__DIR__ . '/../../resources/views', '{}');\n        $this->loadTranslationsFrom(__DIR__ . '/../../lang', '{}');\n        $this->loadMigrationsFrom(__DIR__ . '/../../database/migrations');\n    }}\n}}\n",
        input.namespace, input.name, id, id, id
    );
    outputs.insert(
        format!("{folder}/Providers/{}ServiceProvider.php", input.name),
        provider_source,
    );
    outputs.insert("config/config.php".into(), "<?php\n\nreturn [];\n".into());
    outputs.insert(
        "routes/web.php".into(),
        "<?php\n\n// Register module routes here.\n".into(),
    );
    for directory in [
        "database/migrations",
        "database/factories",
        "database/seeders",
        "resources/views",
        "lang",
        "tests",
    ] {
        outputs.insert(format!("{directory}/.gitkeep"), String::new());
    }
    outputs.insert("README.md".into(),format!("# {}\n\nNative OpenIDEA {} scaffold for Laravel {}.\n\n{}\n\nThis scaffold does not reproduce customized Artisan stubs, activate modules, install packages or execute migrations.\n",input.name,input.kind,input.laravel_major,instructions.join("\n\n")));
    // Verify each generated class prefix maps into its own owned manifest. There
    // are no root autoload edits or claims that a path package is already installed.
    if manifest["autoload"]["psr-4"][format!("{}\\", input.namespace)].as_str()
        != Some(&format!("{folder}/"))
    {
        return Err(Error::Stale);
    }
    let mut changes = vec![Change {
        path: CONFIG.into(),
        before: existing,
        after: next_config,
    }];
    for (relative, after) in outputs {
        let path = format!("{}/{relative}", input.root);
        let before = dependency(project, root, &path, &mut dependencies)?;
        if before.is_some() {
            return Err(Error::Invalid(format!("Refusing to overwrite {path}")));
        }
        changes.push(Change {
            path,
            before,
            after,
        });
    }
    Ok(Plan {
        changes,
        dependencies,
        instructions,
    })
}
fn localized_instructions(plan: &Plan) -> Vec<String> {
    plan.instructions.iter().map(|instruction|match instruction.as_str(){
    "Before loading this module, review root Composer merge-plugin include Modules/*/composer.json and explicitly allow wikimedia/composer-merge-plugin. This action does not alter or enable Composer plugins." => crate::ui_text::t("generator.scaffold.merge", instruction),
    "After reviewing autoload integration, explicitly run composer dump-autoload and enable the module through your nWidart workflow. Root composer.json and module activation statuses are unchanged." => crate::ui_text::t("generator.scaffold.autoload", instruction),
    _ => crate::ui_text::owned_error(instruction, &[("generator.scaffold.install","Install {package} through a reviewed Composer path repository pointing to {root} and an explicit require; package discovery only takes effect after installation. Root composer.json is unchanged.")]).unwrap_or_else(||instruction.clone()),
}).collect()
}
fn preview(plan: &Plan) -> Value {
    json!({"files":plan.changes.iter().map(|change|json!({"path":change.path,"before":change.before,"after":change.after,"operation":if change.before.is_none(){"create"}else{"edit"}})).collect::<Vec<_>>(),"instructions":localized_instructions(plan)})
}
impl State {
    pub(crate) fn handle(&mut self, ctx: &Invocation, message: &str) -> Result<Value, Error> {
        let value = crate::strict_json::parse(message, LIMIT)?;
        let op = value["op"].as_str().ok_or(Error::Stale)?;
        let (project, root) = owner(ctx)?;
        match op {
            "module-scaffold-discard" => {
                self.pending.remove(&root);
                Ok(json!({"discarded":true}))
            }
            "module-scaffold-preview" | "module-scaffold-prepare" => {
                // Any new draft evaluation retires an earlier prepared transaction,
                // including when the replacement draft is invalid.
                self.pending.remove(&root);
                let input = decode(value["draft"].clone())?;
                let plan = build(&project, &root, &input)?;
                let mut reply = preview(&plan);
                if op.ends_with("prepare") {
                    if self.pending.len() >= 8 && !self.pending.contains_key(&root) {
                        return Err(Error::Invalid("Too many prepared module projects".into()));
                    }
                    let editors = host::open_editors()?;
                    if plan.changes.iter().any(|change| {
                        editors.iter().any(|editor| {
                            editor.path == format!("{root}/{}", change.path) && editor.dirty
                        })
                    }) {
                        return Err(Error::Invalid(
                            "Save scaffold destination/config editors first".into(),
                        ));
                    }
                    let anchor = editors
                        .iter()
                        .find(|editor| {
                            editor.active
                                && !editor.dirty
                                && editor.language == "php"
                                && owned(&root, &editor.path)
                        })
                        .or_else(|| {
                            editors.iter().find(|editor| {
                                !editor.dirty
                                    && editor.language == "php"
                                    && owned(&root, &editor.path)
                            })
                        })
                        .map(|editor| editor.path.clone())
                        .unwrap_or_else(|| format!("{root}/routes/web.php"));
                    if !owned(&root, &anchor) {
                        return Err(Error::Stale);
                    }
                    let anchor_text = project.read_file(&anchor)?;
                    if anchor_text.len() > LIMIT {
                        return Err(Error::Stale);
                    }
                    self.next = self.next.checked_add(1).ok_or(Error::Stale)?;
                    let key = format!("module-scaffold:{}", self.next);
                    host::open_editor(&anchor, None)?;
                    self.pending.insert(
                        root,
                        Pending {
                            key,
                            input,
                            plan,
                            anchor,
                            anchor_text,
                        },
                    );
                    reply["prepared"] = json!(true);
                    reply["instruction"] = json!(crate::ui_text::t(
                        "generator.scaffold.prepared",
                        "In the opened PHP editor choose Apply prepared Laravel module scaffold…; one undo reverses all file creation and generator configuration changes."
                    ));
                }
                Ok(reply)
            }
            _ => Err(Error::Invalid("Unknown module scaffold operation".into())),
        }
    }
    pub(crate) fn prepared(&self, root: &str) -> Option<(String, String)> {
        self.pending.get(root).map(|pending| (pending.key.clone(), pending.anchor.clone()))
    }
    pub(crate) fn actions(&self, doc: &Document) -> Vec<FileAction> {
        self.pending
            .iter()
            .find_map(|(root, pending)| {
                (pending.anchor == doc.path
                    && pending.anchor_text == doc.text
                    && owned(root, &doc.path))
                .then(|| FileAction {
                    title: crate::ui_text::t(
                        "generator.scaffold.apply",
                        "Apply prepared Laravel module scaffold…",
                    ),
                    kind: Some("refactor".into()),
                    data: Some(pending.key.clone()),
                    ..Default::default()
                })
            })
            .into_iter()
            .collect()
    }
    pub(crate) fn resolve(
        &mut self,
        doc: &Document,
        action: FileAction,
    ) -> Result<FileAction, Error> {
        let project = host::project_of(&doc.path).ok_or(Error::Stale)?;
        let root = project.root()?;
        let pending = self.pending.get(&root).ok_or(Error::Stale)?;
        let unchanged = || -> Result<(), Error> {
            if action.data.as_deref() != Some(&pending.key)
                || pending.anchor != doc.path
                || pending.anchor_text != doc.text
                || project.read_file(&doc.path)? != doc.text
                || build(&project, &root, &pending.input)? != pending.plan
            {
                return Err(Error::Stale);
            }
            let editors = host::open_editors()?;
            if pending.plan.changes.iter().any(|change| {
                editors
                    .iter()
                    .any(|editor| editor.path == format!("{root}/{}", change.path) && editor.dirty)
            }) {
                return Err(Error::Stale);
            }
            Ok(())
        };
        unchanged()?;
        let exact = pending
            .plan
            .changes
            .iter()
            .map(|change| {
                crate::ui_text::format(
                    "generator.review.change",
                    "{path}\nBefore:\n{before}\nAfter:\n{after}",
                    &[
                        ("path", &change.path),
                        (
                            "before",
                            change.before.as_deref().unwrap_or(&crate::ui_text::t(
                                "generator.review.absent",
                                "(absent)",
                            )),
                        ),
                        ("after", &change.after),
                    ],
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n")
            + "\n\n"
            + &localized_instructions(&pending.plan).join("\n\n");
        if !ui_kit::prompts::confirm_exact(
            &crate::ui_text::t(
                "generator.scaffold.confirm",
                "Apply Laravel module scaffold?",
            ),
            &exact,
        )? {
            return Err(Error::Cancelled);
        }
        unchanged()?;
        let pending = self.pending.remove(&root).ok_or(Error::Stale)?;
        let mut result = FileAction {
            title: action.title,
            open: Some(format!("{root}/{}/composer.json", pending.input.root)),
            ..Default::default()
        };
        for change in pending.plan.changes {
            let path = format!("{root}/{}", change.path);
            if let Some(before) = change.before {
                result.edits.push((
                    path,
                    vec![TextEdit {
                        range: Range::new(
                            Position::new(0, 0),
                            text::position(&before, before.len()),
                        ),
                        text: change.after,
                    }],
                ));
            } else {
                result.operations.push(FileOperation::Create {
                    path,
                    text: change.after,
                    overwrite: false,
                });
            }
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    fn fixture() -> (MockHost, State, Document) {
        let h = MockHost::install();
        h.add_file("composer.json",r#"{"require":{"laravel/framework":"^12.0","nwidart/laravel-modules":"^12.0"},"autoload":{"psr-4":{"App\\":"app/"}}}"#);
        h.add_file(
            "composer.lock",
            r#"{"packages":[{"name":"nwidart/laravel-modules","version":"v12.0.4"}]}"#,
        );
        h.add_file("routes/web.php", "<?php // anchor");
        h.open_editor("/project/routes/web.php", "php", "<?php // anchor", &[]);
        let doc = Document {
            path: "/project/routes/web.php".into(),
            language: "php".into(),
            version: 1,
            text: "<?php // anchor".into(),
        };
        (h, State::default(), doc)
    }
    fn draft(kind: &str) -> Value {
        if kind == "nwidart-v12" {
            json!({"kind":kind,"name":"Blog","root":"Modules/Blog","namespace":"Modules\\Blog","package":"acme/blog","laravelMajor":12})
        } else {
            json!({"kind":kind,"name":"Billing","root":"packages/billing","namespace":"Acme\\Billing","package":"acme/billing","laravelMajor":13})
        }
    }
    fn request(op: &str, draft: Value) -> String {
        json!({"op":op,"draft":draft}).to_string()
    }
    #[test]
    fn nwidart_preview_uses_locked_v12_schema_and_preserves_existing_generator_config() {
        let (h, mut state, _) = fixture();
        let previous = json!({"version":1,"presets":[{"id":"thing","label":"Thing","files":[{"path":"app/Thing.php","template":"thing.php"}]}],"modules":[]});
        h.add_file(CONFIG, &previous.to_string());
        let preview = state
            .handle(
                &h.invocation(),
                &request("module-scaffold-preview", draft("nwidart-v12")),
            )
            .unwrap();
        let files = preview["files"].as_array().unwrap();
        let contents = |path: &str| {
            files.iter().find(|file| file["path"] == path).unwrap()["after"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        let manifest: Value =
            serde_json::from_str(&contents("Modules/Blog/composer.json")).unwrap();
        assert_eq!(manifest["autoload"]["psr-4"]["Modules\\Blog\\"], "app/");
        assert_eq!(
            manifest["autoload"]["psr-4"]["Modules\\Blog\\Database\\Factories\\"],
            "database/factories/"
        );
        let module: Value = serde_json::from_str(&contents("Modules/Blog/module.json")).unwrap();
        assert_eq!(
            module["providers"][0],
            "Modules\\Blog\\Providers\\BlogServiceProvider"
        );
        let config: Value = serde_json::from_str(&contents(CONFIG)).unwrap();
        assert_eq!(config["presets"], previous["presets"]);
        assert_eq!(config["modules"][0]["sourceRoot"], "Modules/Blog/app");
        assert!(
            preview["instructions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|step| step.as_str().unwrap().contains("merge-plugin"))
        );
        assert!(
            !h.invocation()
                .project()
                .unwrap()
                .exists("Modules/Blog/composer.json")
                .unwrap()
        );
        assert!(h.process_runs().is_empty());
    }
    #[test]
    fn package_is_one_reviewed_atomic_create_plus_configuration_edit_without_root_composer_mutation()
     {
        let (h, mut state, doc) = fixture();
        h.add_file(CONFIG, r#"{"version":1,"modules":[],"presets":[]}"#);
        let initial = h
            .invocation()
            .project()
            .unwrap()
            .read_file("composer.json")
            .unwrap();
        state
            .handle(
                &h.invocation(),
                &request("module-scaffold-prepare", draft("composer-package")),
            )
            .unwrap();
        let lazy = state.actions(&doc).remove(0);
        h.answer_next_prompt(Some(&["yes"]));
        let action = state.resolve(&doc, lazy.clone()).unwrap();
        assert_eq!(action.edits.len(), 1);
        assert_eq!(
            action.edits[0].0,
            "/project/.openidea/laravel-generators.json"
        );
        assert!(action.operations.len() > 8);
        let composer = action
            .operations
            .iter()
            .find_map(|operation| match operation {
                FileOperation::Create {
                    path,
                    text,
                    overwrite: false,
                } if path == "/project/packages/billing/composer.json" => Some(text),
                _ => None,
            })
            .unwrap();
        let composer: Value = serde_json::from_str(composer).unwrap();
        assert_eq!(
            composer["extra"]["laravel"]["providers"][0],
            "Acme\\Billing\\Providers\\BillingServiceProvider"
        );
        assert!(action.operations.iter().all(|operation| matches!(
            operation,
            FileOperation::Create {
                overwrite: false,
                ..
            }
        )));
        assert!(state.resolve(&doc, lazy).is_err());
        assert_eq!(
            h.invocation()
                .project()
                .unwrap()
                .read_file("composer.json")
                .unwrap(),
            initial
        );
        assert!(h.process_runs().is_empty());
    }
    #[test]
    fn unsupported_versions_custom_config_and_existing_or_foreign_roots_are_refused() {
        let (h, mut state, _) = fixture();
        h.add_file(
            "composer.lock",
            r#"{"packages":[{"name":"nwidart/laravel-modules","version":"v13.0.0"}]}"#,
        );
        assert!(
            state
                .handle(
                    &h.invocation(),
                    &request("module-scaffold-preview", draft("nwidart-v12"))
                )
                .is_err()
        );
        let (h, mut state, _) = fixture();
        h.add_file("config/modules.php", "<?php return [];");
        assert!(
            state
                .handle(
                    &h.invocation(),
                    &request("module-scaffold-preview", draft("nwidart-v12"))
                )
                .is_err()
        );
        let (h, mut state, _) = fixture();
        h.add_file("packages/billing/KEEP", "keep");
        assert!(
            state
                .handle(
                    &h.invocation(),
                    &request("module-scaffold-preview", draft("composer-package"))
                )
                .is_err()
        );
        let (h, mut state, _) = fixture();
        h.add_project("/project/packages");
        assert!(
            state
                .handle(
                    &h.invocation(),
                    &request("module-scaffold-preview", draft("composer-package"))
                )
                .is_err()
        );
    }
    #[test]
    fn changed_composer_config_collisions_and_cancel_never_return_partial_transactions() {
        for mode in ["composer", "config", "collision", "cancel"] {
            let (h, mut state, doc) = fixture();
            state
                .handle(
                    &h.invocation(),
                    &request("module-scaffold-prepare", draft("composer-package")),
                )
                .unwrap();
            let lazy = state.actions(&doc).remove(0);
            match mode {
                "composer" => h.add_file(
                    "composer.json",
                    r#"{"autoload":{"psr-4":{"Other\\":"other/"}}}"#,
                ),
                "config" => h.add_file(CONFIG, r#"{"version":1,"modules":[]}"#),
                "collision" => h.add_file("packages/billing/KEEP", "keep"),
                _ => h.answer_next_prompt(None),
            }
            assert!(state.resolve(&doc, lazy).is_err(), "{mode}");
            assert!(
                !h.invocation()
                    .project()
                    .unwrap()
                    .exists("packages/billing/composer.json")
                    .unwrap()
            );
            assert!(h.process_runs().is_empty());
        }
    }
    #[test]
    fn unsafe_inputs_duplicate_modules_and_discard_are_rejected() {
        assert!(package_name("acme/blog--module"));
        assert!(!package_name("acme--vendor/blog"));
        assert!(!package_name("acme/blog...module"));
        let (h, mut state, _doc) = fixture();
        let mut invalid = draft("composer-package");
        invalid["root"] = json!("packages/../../escape");
        assert!(
            state
                .handle(
                    &h.invocation(),
                    &request("module-scaffold-preview", invalid)
                )
                .is_err()
        );
        let mut invalid = draft("composer-package");
        invalid["package"] = json!("Acme/evil'");
        assert!(
            state
                .handle(
                    &h.invocation(),
                    &request("module-scaffold-preview", invalid)
                )
                .is_err()
        );
        h.add_file(CONFIG,r#"{"version":1,"modules":[{"id":"billing","label":"Old","namespace":"Old\\Billing","sourceRoot":"old/src","migrationRoot":"old/database/migrations","viewRoot":"old/views"}]}"#);
        assert!(
            state
                .handle(
                    &h.invocation(),
                    &request("module-scaffold-preview", draft("composer-package"))
                )
                .is_err()
        );
        let (h, mut state, doc) = fixture();
        state
            .handle(
                &h.invocation(),
                &request("module-scaffold-prepare", draft("composer-package")),
            )
            .unwrap();
        state
            .handle(&h.invocation(), r#"{"op":"module-scaffold-discard"}"#)
            .unwrap();
        assert!(state.actions(&doc).is_empty());
        assert!(h.process_runs().is_empty());
    }
    #[test]
    fn evaluating_a_new_invalid_draft_retires_prepared_transaction() {
        let (h, mut state, doc) = fixture();
        state
            .handle(
                &h.invocation(),
                &request("module-scaffold-prepare", draft("composer-package")),
            )
            .unwrap();
        let old = state.actions(&doc).remove(0);
        let mut next = draft("composer-package");
        next["root"] = json!("../escape");
        assert!(
            state
                .handle(&h.invocation(), &request("module-scaffold-preview", next))
                .is_err()
        );
        assert!(state.actions(&doc).is_empty());
        assert!(state.resolve(&doc, old).is_err());
    }
}
