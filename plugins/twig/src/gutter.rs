//! Related-template gutter icons. The open template supplies its own parent and blocks; the templates that extend,
//! include, embed, import or use it come from `Twig::templates`, which the background scan fills, so no call reads
//! the project. Markers sharing a line and an icon become one gutter button: one target opens directly, several open
//! the IDE's chooser. Nothing here is published to the document index, so no marker becomes a rename anchor.
use crate::*;
use openidea_plugin::providers::LineMarker;

pub(crate) const RELATED: &str = "icons/twig-logo.svg";
const OVERRIDING: &str = "overriding";
const OVERRIDDEN: &str = "overridden";
const MAX_DEPTH: usize = 8;
const MAX_TARGETS: usize = 64;

#[derive(Clone)]
pub(crate) struct Ref {
    /// `extends`, `include`, `embed`, `import`, `from` or `use`; `include()` counts as `include`.
    tag: &'static str,
    name: String,
    /// The name inside the quotes.
    at: EditorRange,
}

#[derive(Clone, Default)]
pub(crate) struct Facts {
    refs: Vec<Ref>,
    /// Blocks of the template itself; blocks inside an `embed` belong to the embedded template.
    blocks: Vec<(String, EditorRange)>,
}

impl Facts {
    fn parent(&self) -> Option<&Ref> {
        self.refs.iter().find(|r| r.tag == "extends")
    }
}

/// Literal template references and top-level blocks. Callers hold a delimiter scope.
pub(crate) fn facts(text: &str) -> Facts {
    let mut out = Facts::default();
    if text.len() > 128 << 10 {
        return out;
    }
    let parsed = lex(text);
    if parsed.tags.len() > MAX_STRUCTURE_TAGS || parsed.tokens.len() > MAX_SYNTAX_TOKENS {
        return out;
    }
    let range = |r: Range<usize>| EditorRange::new(position(text, r.start), position(text, r.end));
    let mut embeds = 0usize;
    for tag in &parsed.tags {
        match tag.name.as_str() {
            "embed" => embeds += 1,
            "endembed" => embeds = embeds.saturating_sub(1),
            "block" if embeds == 0 => {
                if let Some(r) = &tag.argument {
                    out.blocks.push((text[r.clone()].to_owned(), range(r.clone())));
                }
            }
            _ => {}
        }
    }
    for token in parsed.tokens.iter().filter(|t| t.kind == Highlight::String) {
        let before = text[..token.range.start].trim_end();
        let tag = ["extends", "include", "embed", "import", "from", "use"]
            .into_iter()
            .find(|tag| before.ends_with(tag))
            .or_else(|| before.strip_suffix('(').is_some_and(|b| b.trim_end().ends_with("include")).then_some("include"));
        let Some(tag) = tag else { continue };
        let Some((name, at)) = navigate::template_at(text, token.range.start + 1) else { continue };
        out.refs.push(Ref { tag, name, at: range(at) });
        if out.refs.len() >= MAX_TARGETS {
            break;
        }
    }
    out
}

/// The names that load `path`: relative to each main-namespace directory and each `@namespace/` mapping, kept only
/// when the loader would pick this very file for them.
fn names_of(project: &Project, root: &str, path: &str) -> Vec<String> {
    let Some(relative) = path.strip_prefix(&format!("{root}/")) else { return Vec::new() };
    let under = |dir: &str| relative.strip_prefix(&format!("{dir}/")).map(str::to_owned);
    let mut names: Vec<String> = main_template_dirs(project)
        .map(|(fixed, variable)| fixed.iter().chain(&variable).filter_map(|d| under(d)).collect())
        .unwrap_or_default();
    names.extend(merged_template_namespaces(project).iter().filter_map(|(ns, dir)| Some(format!("@{ns}/{}", under(dir)?))));
    names.sort();
    names.dedup();
    names.retain(|name| template_path(project, name).as_deref() == Some(path));
    names
}

/// Cached templates of this project that reference `path`.
fn users_of<'a>(cache: &'a BTreeMap<String, Facts>, project: &Project, root: &str, path: &str) -> Vec<(&'a String, &'a Ref)> {
    let names = names_of(project, root, path);
    if names.is_empty() {
        return Vec::new();
    }
    let prefix = format!("{root}/");
    cache
        .iter()
        // ponytail: a nested project's templates count for the outer one too; use project_of per path if that matters.
        .filter(|(p, _)| p.starts_with(&prefix) && p.as_str() != path)
        .flat_map(|(p, facts)| facts.refs.iter().filter(|r| names.contains(&r.name)).map(move |r| (p, r)))
        .take(MAX_TARGETS)
        .collect()
}

fn read_facts(cache: &BTreeMap<String, Facts>, project: &Project, path: &str) -> Option<Facts> {
    if let Some(known) = cache.get(path) {
        return Some(known.clone());
    }
    if project.stat(path).ok()?.size > 128 << 10 {
        return None;
    }
    Some(facts(&project.read_file(path).ok()?))
}

pub(crate) fn markers(cache: &BTreeMap<String, Facts>, doc: &Document) -> Vec<LineMarker> {
    let Some(_syntax) = delimiters::Syntax::scope() else { return Vec::new() };
    if doc.language != "twig" || !doc.path.ends_with(".twig") || doc.text.len() > 128 << 10 {
        return Vec::new();
    }
    let Some(project) = project_of(&doc.path) else { return Vec::new() };
    let Ok(root) = project.root() else { return Vec::new() };
    let relative = |path: &str| path.strip_prefix(&format!("{root}/")).unwrap_or(path).to_owned();
    let mark = |line: u32, icon: &str, tooltip: String, path: &str, range: EditorRange| LineMarker {
        line,
        icon: Some(icon.into()),
        tooltip,
        target: Some(Location { path: path.to_owned(), range }),
    };
    let own = facts(&doc.text);
    let mut out = Vec::new();

    let parent = own.parent();
    let line = parent.map_or(0, |r| r.at.start.line);
    let mut ancestors: Vec<(String, Facts)> = Vec::new();
    let mut next = parent.and_then(|r| template_path(&project, &r.name));
    if let (Some(r), Some(path)) = (parent, &next) {
        let top = EditorRange::new(position("", 0), position("", 0));
        out.push(mark(line, RELATED, format!("Extends {}", r.name), path, top));
    }
    while let Some(path) = next.take() {
        if ancestors.len() >= MAX_DEPTH || path == doc.path || ancestors.iter().any(|(p, _)| *p == path) {
            break;
        }
        let Some(facts) = read_facts(cache, &project, &path) else { break };
        next = facts.parent().and_then(|r| template_path(&project, &r.name));
        ancestors.push((path, facts));
    }

    let users = users_of(cache, &project, &root, &doc.path);
    for (path, r) in &users {
        let verb = match r.tag {
            "extends" => "Extended by",
            "include" => "Included by",
            "embed" => "Embedded by",
            "use" => "Used by",
            _ => "Imported by",
        };
        out.push(mark(line, RELATED, format!("{verb} {}", relative(path)), path, r.at));
    }

    let mut descendants: Vec<(&String, &Facts)> = Vec::new();
    let mut level: Vec<String> = vec![doc.path.clone()];
    for _ in 0..MAX_DEPTH {
        let mut found = Vec::new();
        for path in &level {
            for (child, r) in users_of(cache, &project, &root, path) {
                if r.tag == "extends" && *child != doc.path && !descendants.iter().any(|(p, _)| *p == child) && descendants.len() < MAX_TARGETS {
                    descendants.push((child, &cache[child]));
                    found.push(child.clone());
                }
            }
        }
        if found.is_empty() {
            break;
        }
        level = found;
    }

    for (name, at) in &own.blocks {
        let line = at.start.line;
        let overridden = ancestors.iter().find_map(|(path, facts)| Some((path, facts.blocks.iter().find(|(n, _)| n == name)?.1)));
        if let Some((path, range)) = overridden {
            out.push(mark(line, OVERRIDING, format!("Overrides block {name} of {}", relative(path)), path, range));
        }
        for (path, facts) in &descendants {
            for (_, range) in facts.blocks.iter().filter(|(n, _)| n == name) {
                out.push(mark(line, OVERRIDDEN, format!("Overridden in {}", relative(path)), path, *range));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    use openidea_plugin::providers::Providers;

    const BASE: &str = "<html>\n{% block title %}T{% endblock %}\n{% block body %}{% endblock %}\n";
    const CHILD: &str = "{% extends 'base.html.twig' %}\n{% block body %}child{% endblock %}\n";
    const GRAND: &str = "{# grand #}\n{% extends \"child.html.twig\" %}\n{% block body %}g{% endblock %}\n{% block title %}x{% endblock %}\n";
    const PAGE: &str = "{{ include('base.html.twig') }}\n{% embed '@ui/card.twig' %}{% block body %}{% endblock %}{% endembed %}\n{% import 'base.html.twig' as b %}\n";
    const CARD: &str = "<div>{% block body %}{% endblock %}</div>\n";

    fn host() -> MockHost {
        let host = MockHost::install();
        host.add_file("composer.json", r#"{"require": {"twig/twig": "^3"}}"#);
        host.set_setting("templateNamespaces", r#"[{"namespace":"ui","path":"lib/ui"}]"#);
        for (path, text) in [("base", BASE), ("child", CHILD), ("grand", GRAND), ("page", PAGE)] {
            host.add_file(&format!("templates/{path}.html.twig"), text);
        }
        host.add_file("lib/ui/card.twig", CARD);
        host
    }

    fn open(path: &str, text: &str) -> Document {
        Document { path: format!("/project/{path}"), language: "twig".into(), version: 1, text: text.into() }
    }

    fn active(host: &MockHost) -> Twig {
        let mut twig = Twig::default();
        twig.activate(&host.context("aritti.twig")).unwrap();
        twig
    }

    fn summary(markers: &[LineMarker]) -> Vec<(u32, String, String, String)> {
        let mut found: Vec<_> = markers
            .iter()
            .map(|m| {
                let target = m.target.as_ref().unwrap();
                (m.line, m.icon.clone().unwrap(), m.tooltip.clone(), target.path.trim_start_matches("/project/").to_owned())
            })
            .collect();
        found.sort();
        found
    }

    fn target_text(host: &MockHost, marker: &LineMarker) -> String {
        let target = marker.target.as_ref().unwrap();
        let text = host.invocation().project().unwrap().read_file(&target.path).unwrap();
        text[offset(&text, target.range.start).unwrap()..offset(&text, target.range.end).unwrap()].to_owned()
    }

    #[test]
    fn a_parent_template_lists_children_users_and_overriding_blocks() {
        let host = host();
        let mut twig = active(&host);
        let markers = twig.line_markers(&open("templates/base.html.twig", BASE));
        let s = |line: u32, icon: &str, tooltip: &str, path: &str| (line, icon.to_owned(), tooltip.to_owned(), path.to_owned());
        assert_eq!(
            summary(&markers),
            [
                s(0, RELATED, "Extended by templates/child.html.twig", "templates/child.html.twig"),
                s(0, RELATED, "Imported by templates/page.html.twig", "templates/page.html.twig"),
                s(0, RELATED, "Included by templates/page.html.twig", "templates/page.html.twig"),
                s(1, OVERRIDDEN, "Overridden in templates/grand.html.twig", "templates/grand.html.twig"),
                s(2, OVERRIDDEN, "Overridden in templates/child.html.twig", "templates/child.html.twig"),
                s(2, OVERRIDDEN, "Overridden in templates/grand.html.twig", "templates/grand.html.twig"),
            ]
        );
        for marker in &markers {
            let expected = if marker.line == 0 { "base.html.twig" } else if marker.line == 1 { "title" } else { "body" };
            assert_eq!(target_text(&host, marker), expected);
        }
    }

    #[test]
    fn a_child_template_links_its_parent_children_and_both_block_directions_on_one_button_per_line() {
        let host = host();
        let mut twig = active(&host);
        let markers = twig.line_markers(&open("templates/child.html.twig", CHILD));
        let s = |line: u32, icon: &str, tooltip: &str, path: &str| (line, icon.to_owned(), tooltip.to_owned(), path.to_owned());
        assert_eq!(
            summary(&markers),
            [
                s(0, RELATED, "Extended by templates/grand.html.twig", "templates/grand.html.twig"),
                s(0, RELATED, "Extends base.html.twig", "templates/base.html.twig"),
                s(1, OVERRIDDEN, "Overridden in templates/grand.html.twig", "templates/grand.html.twig"),
                s(1, OVERRIDING, "Overrides block body of templates/base.html.twig", "templates/base.html.twig"),
            ]
        );
        let grand = twig.line_markers(&open("templates/grand.html.twig", GRAND));
        assert!(grand.iter().filter(|m| m.icon.as_deref() == Some(RELATED)).all(|m| m.line == 1));
        let title: Vec<_> = grand.iter().filter(|m| m.line == 3).collect();
        assert_eq!(title.len(), 1, "title is inherited through child from base");
        assert!(title[0].target.as_ref().unwrap().path.ends_with("/base.html.twig"));
        assert_eq!(target_text(&host, title[0]), "title");
    }

    #[test]
    fn namespaced_names_resolve_users_and_embed_blocks_stay_with_the_embedded_template() {
        let host = host();
        let mut twig = active(&host);
        let card = twig.line_markers(&open("lib/ui/card.twig", CARD));
        assert_eq!(card.len(), 1);
        assert_eq!((card[0].tooltip.as_str(), card[0].line), ("Embedded by templates/page.html.twig", 0));
        assert_eq!(target_text(&host, &card[0]), "@ui/card.twig");
        let page = twig.line_markers(&open("templates/page.html.twig", PAGE));
        assert!(page.is_empty(), "includes have Ctrl+click and the embed's block is not the page's own");
        host.set_setting("templateNamespaces", "[]");
        assert!(twig.line_markers(&open("lib/ui/card.twig", CARD)).is_empty());
    }

    #[test]
    fn saved_and_created_templates_refresh_reverse_links_and_unsaved_text_drives_forward_ones() {
        let host = host();
        let mut twig = active(&host);
        host.add_file("templates/extra.html.twig", "{% use 'card.html.twig' %}\n");
        host.add_file("templates/card.html.twig", "x");
        assert!(twig.line_markers(&open("templates/card.html.twig", "x")).is_empty());
        twig.handle_events(&[Event::FileSaved("/project/templates/extra.html.twig".into())], &host.invocation());
        let card = twig.line_markers(&open("templates/card.html.twig", "x"));
        assert_eq!(card.len(), 1);
        assert_eq!(card[0].tooltip, "Used by templates/extra.html.twig");
        host.add_file("templates/extra.html.twig", "nothing");
        twig.handle_events(
            &[Event::FilesChanged(openidea_plugin::FilesChanged { created: vec![], modified: vec!["/project/templates/extra.html.twig".into()], removed: vec![] })],
            &host.invocation(),
        );
        assert!(twig.line_markers(&open("templates/card.html.twig", "x")).is_empty());
        let unsaved = twig.line_markers(&open("templates/card.html.twig", "\n\n{% extends 'base.html.twig' %}"));
        assert_eq!(summary(&unsaved)[0].2, "Extends base.html.twig");
        assert_eq!(unsaved[0].line, 2);
    }

    #[test]
    fn dynamic_names_cycles_and_markers_stay_out_of_the_index() {
        let host = host();
        host.add_file("templates/a.twig", "{% extends 'b.twig' %}{% block x %}{% endblock %}");
        host.add_file("templates/b.twig", "{% extends 'a.twig' %}{% block x %}{% endblock %}");
        let mut twig = active(&host);
        let a = twig.line_markers(&open("templates/a.twig", "{% extends 'b.twig' %}{% block x %}{% endblock %}"));
        assert!(a.len() <= 4, "a cycle stays bounded");
        for text in ["{% extends 'ba' ~ 'se.html.twig' %}", "{% include name %}", "{% extends 'missing.twig' %}"] {
            assert!(twig.line_markers(&open("templates/z.twig", text)).is_empty(), "{text}");
        }
        let manifest = include_str!("../plugin.toml");
        assert!(manifest.contains(&format!("icon = \"{RELATED}\"")) && Path::new(env!("CARGO_MANIFEST_DIR")).join(RELATED).is_file());
        assert!(!manifest.contains("documentIndexers"), "markers never reach the host's rename graph");
        let edges = crate::indexing::source_edges(&openidea_plugin::indexing::Document { path: "/project/templates/child.html.twig".into(), text: CHILD.into(), symbols: vec![] });
        assert!(edges.markers.is_empty());
    }
}
