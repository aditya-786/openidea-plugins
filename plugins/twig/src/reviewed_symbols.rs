//! Read-only native occurrences and reviewed source-policy refactoring.
use crate::*;
use openidea_plugin::{
    file_actions::{FileAction, FileActions},
    indexing::{Document as IndexedDocument, Edges},
    language_features::{CodeLens, Highlight},
};
use std::{
    collections::BTreeSet,
    hash::BuildHasher,
    time::{Duration, Instant},
};
#[derive(Clone)]
struct Pending {
    root: String,
    doc: Document,
    key: String,
    range: EditorRange,
    syntax: [String; 6],
}
#[derive(Default)]
pub struct State {
    pending: BTreeMap<String, Pending>,
}
fn graph(doc: &Document) -> Edges {
    if doc.language != "twig" || !doc.path.ends_with(".twig") || doc.text.len() > 64 << 10 {
        return Edges::default();
    }
    indexing::source_edges(&IndexedDocument {
        path: doc.path.clone(),
        text: doc.text.clone(),
        symbols: vec![],
    })
}
fn root(doc: &Document) -> Option<String> {
    let root = project_of(&doc.path)?.root().ok()?;
    (canonical(&root, &doc.path) && same_owner(&root, &doc.path)).then_some(root)
}
fn selected(edges: &Edges, at: openidea_plugin::Position) -> Option<(&str, EditorRange)> {
    let found: Vec<_> = edges
        .definitions
        .iter()
        .chain(&edges.references)
        .filter(|a| a.range.start <= at && at < a.range.end)
        .collect();
    (found.len() == 1).then(|| (found[0].key.as_str(), found[0].range))
}
fn spans(edges: &Edges, key: &str) -> Vec<EditorRange> {
    let mut spans: Vec<_> = edges
        .definitions
        .iter()
        .chain(&edges.references)
        .filter(|a| a.key == key)
        .map(|a| a.range)
        .collect();
    spans.sort_by_key(|r| (r.start.line, r.start.character, r.end.line, r.end.character));
    spans.dedup();
    spans
}
fn text_at<'a>(text: &'a str, range: EditorRange) -> Option<&'a str> {
    text.get(offset(text, range.start)?..offset(text, range.end)?)
}
fn lease(state: &mut State, doc: &Document, key: &str, range: EditorRange) -> Option<String> {
    let root = root(doc)?;
    if state.pending.len() >= 128 {
        state.pending.clear();
    }
    let random = std::collections::hash_map::RandomState::new();
    let token = format!(
        "twig-source:{:016x}{:016x}",
        random.hash_one(0u8),
        random.hash_one(1u8)
    );
    state.pending.insert(
        token.clone(),
        Pending {
            root,
            doc: doc.clone(),
            key: key.into(),
            range,
            syntax: delimiters::Syntax::current().values,
        },
    );
    Some(token)
}
fn fresh(doc: &Document, p: &Pending) -> Result<Project, Error> {
    let _latest_syntax = delimiters::Syntax::scope().ok_or(Error::Stale)?;
    if doc.path != p.doc.path
        || doc.language != p.doc.language
        || doc.version != p.doc.version
        || doc.text != p.doc.text
        || root(doc).as_deref() != Some(p.root.as_str())
        || delimiters::Syntax::current().values != p.syntax
    {
        return Err(Error::Stale);
    }
    let project = project_of(&doc.path).ok_or(Error::Stale)?;
    let edges = graph(doc);
    if project.read_file(&doc.path)? != doc.text
        || !edges
            .definitions
            .iter()
            .chain(&edges.references)
            .any(|a| a.key == p.key && a.range == p.range)
    {
        return Err(Error::Stale);
    }
    Ok(project)
}
pub fn highlights(doc: &Document, at: openidea_plugin::Position) -> Vec<Highlight> {
    let Some(_scope) = delimiters::Syntax::scope() else {
        return vec![];
    };
    let edges = graph(doc);
    let Some((key, _)) = selected(&edges, at) else {
        return vec![];
    };
    spans(&edges, key)
        .into_iter()
        .map(|range| Highlight {
            range,
            kind: if edges
                .definitions
                .iter()
                .any(|a| a.key == key && a.range == range)
            {
                "write"
            } else {
                "read"
            }
            .into(),
        })
        .collect()
}
pub fn lenses(state: &mut State, doc: &Document) -> Vec<CodeLens> {
    let Some(_scope) = delimiters::Syntax::scope() else {
        return vec![];
    };
    if root(doc).is_none() {
        return vec![];
    }
    let edges = graph(doc);
    let mut out = vec![];
    if state.pending.len() + edges.definitions.len().min(64) > 128 {
        state.pending.clear();
    }
    for a in edges.definitions.iter().take(64) {
        if let Some(token) = lease(state, doc, &a.key, a.range) {
            let count = spans(&edges, &a.key).len();
            out.push(CodeLens {
                range: a.range,
                title: if count == 1 { "1 occurrence in this file".into() } else { format!("{count} occurrences in this file") },
                command: "source-symbol-occurrences".into(),
                argument: Some(token),
            });
        }
    }
    out
}
/// Preserve exact model navigation without exposing the model to unchecked host Rename.
pub fn definition(doc: &Document, at: openidea_plugin::Position) -> Vec<Location> {
    let edges = graph(doc);
    let Some((key, range)) = selected(&edges, at) else {
        return vec![];
    };
    if key.starts_with("twig:macro-alias:") {
        let Some(end) = offset(&doc.text, range.end) else {
            return vec![];
        };
        if doc.text[end..].trim_start().starts_with('(') {
            let Some(start) = offset(&doc.text, range.start) else {
                return vec![];
            };
            let Some(name) = text_at(&doc.text, range) else {
                return vec![];
            };
            if resolved_macro(doc, start, None, name).is_none() {
                return vec![];
            }
        }
    }
    let local: Vec<_> = edges
        .definitions
        .iter()
        .filter(|a| a.key == key)
        .map(|a| Location {
            path: doc.path.clone(),
            range: a.range,
        })
        .collect();
    if !local.is_empty() {
        return local;
    }
    let key = key.to_owned();
    let Some(rest) = key
        .strip_prefix("twig:macro:")
        .or_else(|| key.strip_prefix("twig:block:"))
    else {
        return vec![];
    };
    let Some((length, rest)) = rest.split_once(':') else {
        return vec![];
    };
    let Some(length) = length
        .parse::<usize>()
        .ok()
        .filter(|n| *n < rest.len() && rest.is_char_boundary(*n))
    else {
        return vec![];
    };
    let path = &rest[..length];
    let Some(project) = project_of(&doc.path) else {
        return vec![];
    };
    let Ok(root) = project.root() else {
        return vec![];
    };
    if !same_owner(&root, path)
        || !project
            .stat(path)
            .is_ok_and(|s| s.kind == openidea_plugin::FileKind::File && s.size <= 64 << 10)
    {
        return vec![];
    }
    let Ok(text) = project.read_file(path) else {
        return vec![];
    };
    let target = Document {
        path: path.into(),
        language: "twig".into(),
        text,
        version: 0,
    };
    graph(&target)
        .definitions
        .into_iter()
        .filter(|a| a.key == key)
        .map(|a| Location {
            path: path.into(),
            range: a.range,
        })
        .collect()
}
fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    !name.is_empty()
        && name.len() <= 64
        && chars.next().is_some_and(|c| c == '_' || c.is_alphabetic())
        && chars.all(|c| c == '_' || c.is_alphanumeric())
        && ![
            "true",
            "false",
            "null",
            "none",
            "_self",
            "_context",
            "loop",
            "varargs",
            "and",
            "or",
            "not",
            "in",
            "is",
            "matches",
            "starts",
            "ends",
            "with",
            "as",
            "only",
            "ignore",
            "missing",
            "if",
            "else",
            "elseif",
            "for",
            "macro",
            "block",
            "set",
            "endmacro",
            "endblock",
            "endfor",
            "import",
            "from",
            "extends",
            "include",
            "use",
            "parent",
            "attribute",
            "dump",
        ]
        .contains(&name)
}
struct Inventory {
    files: BTreeMap<String, String>,
    paths: Vec<String>,
}
fn inventory(project: &Project, p: &Pending) -> Result<Inventory, Error> {
    if project.root()? != p.root {
        return Err(Error::Stale);
    }
    let global = p.key.starts_with("twig:macro:") || p.key.starts_with("twig:block:");
    let mut paths = if global {
        project.find_files("templates/**/*.twig", 65)?
    } else {
        vec![]
    };
    paths.push(p.doc.path.clone());
    paths.sort();
    paths.dedup();
    if paths.len() > 64 {
        return Err(Error::Invalid(
            "Static Twig rename exceeds 64 files; no partial edit was produced.".into(),
        ));
    }
    let mut files = BTreeMap::new();
    let mut bytes = 0;
    let start = Instant::now();
    for path in &paths {
        if start.elapsed() > Duration::from_secs(3)
            || !canonical(&p.root, path)
            || !same_owner(&p.root, path)
        {
            return Err(Error::Stale);
        }
        let stat = project.stat(path)?;
        if stat.kind != openidea_plugin::FileKind::File || stat.size > 64 << 10 {
            return Err(Error::Invalid("A static Twig source is unavailable or exceeds 64 KiB; no partial edit was produced.".into()));
        }
        let text = project.read_file(path)?;
        bytes += text.len();
        if text.len() > 64 << 10 || bytes > 2 << 20 {
            return Err(Error::Invalid(
                "Static Twig rename exceeds its 2 MiB source budget.".into(),
            ));
        }
        files.insert(path.clone(), text);
    }
    if files.get(&p.doc.path) != Some(&p.doc.text) {
        return Err(Error::Stale);
    }
    Ok(Inventory { files, paths })
}
fn unchanged(project: &Project, p: &Pending, inventory: &Inventory) -> Result<(), Error> {
    let fresh = inventory_fn(project, p)?;
    if fresh.paths != inventory.paths || fresh.files != inventory.files {
        return Err(Error::Stale);
    }
    Ok(())
}
fn inventory_fn(project: &Project, p: &Pending) -> Result<Inventory, Error> {
    inventory(project, p)
}
fn model(path: &str, text: &str) -> Document {
    Document {
        path: path.into(),
        text: text.into(),
        language: "twig".into(),
        version: 0,
    }
}
struct Plan {
    edits: Vec<(String, Vec<openidea_plugin::TextEdit>)>,
    preview: String,
}
fn plan(p: &Pending, inventory: &Inventory, new_name: &str) -> Result<Plan, Error> {
    let old = text_at(&p.doc.text, p.range).ok_or(Error::Stale)?;
    if !valid_name(new_name) || new_name == old {
        return Err(Error::Invalid("Choose a different non-reserved Twig identifier (letters/digits/underscore, leading letter/underscore; at most 64 UTF-8 bytes). Quotes, whitespace and interpolation are not names.".into()));
    }
    let mut edited = inventory.files.clone();
    let mut edits = vec![];
    let mut expected = BTreeSet::new();
    let mut selected_range = None;
    let mut preview = format!(
        "Static source rename only: {old} → {new_name}\nDynamic, unresolved and runtime-generated usages are excluded.\n\n"
    );
    let mut total = 0;
    for (path, text) in &inventory.files {
        let edges = graph(&model(path, text));
        let ranges = spans(&edges, &p.key);
        let covered: BTreeSet<_> = ranges
            .iter()
            .filter_map(|r| Some((offset(text, r.start)?, offset(text, r.end)?)))
            .collect();
        let other = text
            .match_indices(old)
            .filter(|(at, _)| !covered.contains(&(*at, *at + old.len())))
            .count();
        if other > 0 {
            preview.push_str(&format!("Not renamed in {}: {other} other matches of the same text (probably unrelated or dynamic)\n",path.strip_prefix(&format!("{}/",p.root)).unwrap_or(path)));
        }
        if ranges.is_empty() {
            continue;
        }
        if ranges.iter().any(|r| text_at(text, *r) != Some(old)) {
            return Err(Error::Stale);
        }
        let mut after = text.clone();
        let mut changes = vec![];
        let mut delta: isize = 0;
        for range in &ranges {
            let start = offset(text, range.start).ok_or(Error::Stale)?;
            let end = offset(text, range.end).ok_or(Error::Stale)?;
            let shifted = (start as isize + delta) as usize;
            expected.insert((path.clone(), shifted, shifted + new_name.len()));
            if path == &p.doc.path && *range == p.range {
                selected_range = Some((shifted, shifted + new_name.len()));
            }
            delta += new_name.len() as isize - (end - start) as isize;
            preview.push_str(&format!(
                "{}:{}:{}  {old} → {new_name}\n",
                path.strip_prefix(&format!("{}/", p.root)).unwrap_or(path),
                range.start.line + 1,
                range.start.character + 1
            ));
            changes.push(openidea_plugin::TextEdit {
                range: *range,
                text: new_name.into(),
            });
        }
        for edit in changes.iter().rev() {
            after.replace_range(
                offset(text, edit.range.start).ok_or(Error::Stale)?
                    ..offset(text, edit.range.end).ok_or(Error::Stale)?,
                &edit.text,
            );
        }
        if !checks::check(&after).is_empty() {
            return Err(Error::Invalid(
                "The rename would make this Twig source structurally invalid.".into(),
            ));
        }
        total += changes.len();
        if total > 256 || edits.len() >= 16 || preview.len() > 15 << 10 {
            return Err(Error::Invalid(
                "Static Twig rename exceeds review bounds; no partial edit was produced.".into(),
            ));
        }
        edited.insert(path.clone(), after);
        edits.push((path.clone(), changes));
    }
    let (selected_start, selected_end) = selected_range.ok_or(Error::Stale)?;
    template_index::with_sources(edited.clone(), || {
        let selected_text = edited.get(&p.doc.path).ok_or(Error::Stale)?;
        let edges = graph(&model(&p.doc.path, selected_text));
        let matches: Vec<_> = edges
            .definitions
            .iter()
            .chain(&edges.references)
            .filter(|a| {
                offset(selected_text, a.range.start) == Some(selected_start)
                    && offset(selected_text, a.range.end) == Some(selected_end)
            })
            .collect();
        if matches.len() != 1 {
            return Err(Error::Invalid(
                "The new name collides with or loses the selected Twig binding.".into(),
            ));
        }
        let key = &matches[0].key;
        let mut actual = BTreeSet::new();
        for (path, text) in &edited {
            for range in spans(&graph(&model(path, text)), key) {
                actual.insert((
                    path.clone(),
                    offset(text, range.start).ok_or(Error::Stale)?,
                    offset(text, range.end).ok_or(Error::Stale)?,
                ));
            }
        }
        if actual != expected {
            return Err(Error::Invalid("The new name captures, collides with or disconnects a Twig source binding; rename refused.".into()));
        }
        Ok(())
    })?;
    preview.push_str(&format!("\n{} Twig files scanned; {total} changes in {} files. Review them before applying.\nUsages built at runtime are not found.",inventory.files.len(),edits.len()));
    if preview.len() > 15 << 10 {
        return Err(Error::Invalid(
            "The exact rename preview exceeds its review bound.".into(),
        ));
    }
    Ok(Plan { edits, preview })
}
impl FileActions for Twig {
    fn file_actions(&mut self, doc: &Document, selection: EditorRange) -> Vec<FileAction> {
        let Some(_scope) = delimiters::Syntax::scope() else {
            return vec![];
        };
        let edges = graph(doc);
        let Some((key, range)) = selected(&edges, selection.start) else {
            return crate::macro_rename::offered(doc, selection).into_iter().collect();
        };
        let Some(token) = lease(&mut self.reviewed_symbols, doc, key, range) else {
            return vec![];
        };
        vec![FileAction {
            title: "Preview Rename Static Twig Symbol…".into(),
            kind: Some("refactor.rename".into()),
            data: Some(token),
            ..FileAction::default()
        }]
    }
    fn resolve_file_action(
        &mut self,
        doc: &Document,
        action: FileAction,
    ) -> Result<FileAction, Error> {
        let _scope = delimiters::Syntax::scope().ok_or(Error::Stale)?;
        if action.data.as_deref().is_some_and(|d| d.starts_with(crate::macro_rename::PREFIX)) {
            return crate::macro_rename::resolve(doc, action);
        }
        let token = action.data.as_deref().ok_or(Error::Stale)?;
        let pending = self
            .reviewed_symbols
            .pending
            .get(token)
            .cloned()
            .ok_or(Error::Stale)?;
        let project = fresh(doc, &pending)?;
        let inventory = inventory(&project, &pending)?;
        let old = text_at(&doc.text, pending.range).ok_or(Error::Stale)?;
        let new=host::input("New static Twig identifier (letters/digits/underscore; no quotes, whitespace, reserved names or collisions)",Some(old))?.ok_or(Error::Cancelled)?;
        let planned = plan(&pending, &inventory, &new)?;
        if !host::confirm("Rename Twig symbol?", &planned.preview)? {
            return Err(Error::Cancelled);
        }
        fresh(doc, &pending)?;
        unchanged(&project, &pending, &inventory)?;
        self.reviewed_symbols.pending.remove(token);
        Ok(FileAction {
            title: "Rename Static Twig Symbol".into(),
            kind: Some("refactor.rename".into()),
            edits: planned.edits,
            ..FileAction::default()
        })
    }
}
pub fn occurrences_at_caret(state: &mut State, ctx: &Invocation) -> Result<Option<String>, Error> {
    let _scope = delimiters::Syntax::scope()
        .ok_or_else(|| Error::Invalid("Correct the Twig delimiter settings first.".into()))?;
    let editor = ctx.editor().ok_or_else(|| Error::Invalid("Open a Twig template first.".into()))?;
    let denied = |e: Error| ui_kit::denied::map(e, "Show Occurrences", "editor.read", "Twig");
    let current = editor.document().map_err(denied)?;
    let doc = Document {
        path: editor.path(),
        language: editor.language(),
        text: current.text,
        version: current.version,
    };
    let at = editor.selections().map_err(denied)?.first().ok_or_else(|| Error::Invalid("Put the caret on a symbol first.".into()))?.start;
    let edges = graph(&doc);
    let (key, range) = selected(&edges, at).ok_or_else(|| {
        Error::Invalid("Put the caret on a loop variable, import alias, macro or block name.".into())
    })?;
    let token = lease(state, &doc, key, range)
        .ok_or_else(|| Error::Invalid("Show Occurrences works on Twig files inside the open project.".into()))?;
    occurrences(state, ctx, &token)
}

pub fn occurrences(
    state: &mut State,
    ctx: &Invocation,
    arg: &str,
) -> Result<Option<String>, Error> {
    let _scope = delimiters::Syntax::scope().ok_or(Error::Stale)?;
    let pending = state.pending.get(arg).cloned().ok_or(Error::Stale)?;
    let editor = ctx.editor().ok_or(Error::Stale)?;
    let current = editor.document()?;
    let doc = Document {
        path: editor.path(),
        language: editor.language(),
        text: current.text,
        version: current.version,
    };
    let project = fresh(&doc, &pending)?;
    if ctx.project().and_then(|p| p.root().ok()).as_deref() != Some(pending.root.as_str()) {
        return Err(Error::Stale);
    }
    let inventory = inventory(&project, &pending)?;
    let mut found = vec![];
    for (path, text) in &inventory.files {
        for range in spans(&graph(&model(path, text)), &pending.key) {
            found.push((path.clone(), range));
            if found.len() > 256 {
                return Err(Error::Invalid("Occurrence review exceeds 256 spans.".into()));
            }
        }
    }
    let items: Vec<_> = found
        .iter()
        .enumerate()
        .map(|(i, (path, range))| {
            openidea_plugin::PickItem::new(
                &i.to_string(),
                &format!(
                    "{}:{}:{}",
                    path,
                    range.start.line + 1,
                    range.start.character + 1
                ),
            )
        })
        .collect();
    let Some(key) = host::pick(
        "Occurrences (usages built at runtime are not listed)",
        &items,
    )?
    else {
        return Ok(None);
    };
    let (path, range) = found
        .get(key.parse::<usize>().map_err(|_| Error::Stale)?)
        .ok_or(Error::Stale)?;
    fresh(&doc, &pending)?;
    unchanged(&project, &pending, &inventory)?;
    host::open_location(path, range.start.line, range.start.character)?;
    Ok(None)
}
#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{
        language_features::LanguageFeatures, mock::MockHost,
    };
    fn doc(path: &str, text: &str) -> Document {
        Document {
            path: path.into(),
            language: "twig".into(),
            text: text.into(),
            version: 1,
        }
    }
    fn install(path: &str, text: &str) -> MockHost {
        let host = MockHost::install();
        host.add_file(path, text);
        host.open_editor(&format!("/project/{path}"), "twig", text, &[(0, 0, 0, 0)]);
        host
    }
    #[test]
    fn show_occurrences_off_a_symbol_explains_where_to_put_the_caret() {
        let host = install("templates/a.twig", "plain text");
        let host = host.with_capabilities(&["project.read", "editor.read"]);
        let err = occurrences_at_caret(&mut State::default(), &host.invocation()).unwrap_err();
        assert!(matches!(err, Error::Invalid(m) if m.starts_with("Put the caret on a loop variable")));
    }
    fn action(plugin: &mut Twig, doc: &Document, needle: &str) -> FileAction {
        let at = position(&doc.text, doc.text.find(needle).unwrap() + 1);
        let actions = plugin.file_actions(doc, EditorRange::new(at, at));
        assert_eq!(actions.len(), 1);
        assert!(actions[0].edits.is_empty());
        actions[0].clone()
    }
    #[test]
    fn loop_navigation_highlights_lenses_and_reviewed_rename_work() {
        let source = "😀 {% for item in items %}{{ item.name }}{% endfor %}";
        let host = install("a.twig", source);
        let d = doc("/project/a.twig", source);
        let mut plugin = Twig::default();
        let at = position(source, source.rfind("item.name").unwrap() + 1);
        assert_eq!(
            plugin.definition(&d, at)[0].range.start,
            position(source, source.find("item in").unwrap())
        );
        assert_eq!(plugin.document_highlights(&d, at).len(), 2);
        let lens = plugin.code_lenses(&d).remove(0);
        assert_eq!(lens.title, "2 occurrences in this file");
        host.answer_next_prompt(Some(&["1"]));
        plugin
            .run_command(
                "source-symbol-occurrences",
                &host.invocation(),
                lens.argument.as_deref(),
            )
            .unwrap();
        assert_eq!(host.opened_locations().len(), 1);
        let pending = action(&mut plugin, &d, "item in");
        let replay = pending.clone();
        host.answer_next_prompt(Some(&["entry"]));
        host.answer_next_prompt(Some(&["yes"]));
        let resolved = plugin.resolve_file_action(&d, pending).unwrap();
        assert_eq!(resolved.edits[0].1.len(), 2);
        assert_eq!(host.editor_text(), source);
        assert!(plugin.resolve_file_action(&d, replay).is_err());
    }
    #[test]
    fn quotes_interpolation_reserved_names_and_scope_capture_cannot_create_workspace_edits() {
        let source = "{% for item in items %}{{ item }}{% for other in values %}{{ item }}{{ other }}{% endfor %}{% endfor %}";
        let host = install("a.twig", source);
        let d = doc("/project/a.twig", source);
        let mut plugin = Twig::default();
        for name in [
            "a'b",
            "a\"b",
            "#{value}",
            "$value",
            "two names",
            "if",
            "other",
        ] {
            let pending = action(&mut plugin, &d, "item in");
            host.answer_next_prompt(Some(&[name]));
            assert!(
                matches!(
                    plugin.resolve_file_action(&d, pending),
                    Err(Error::Invalid(_))
                ),
                "{name}"
            );
            assert_eq!(host.editor_text(), source);
        }
    }
    #[test]
    fn imported_macro_rename_is_atomic_grammar_validated_and_collision_checked() {
        let source = "{% import 'forms.twig' as forms %}{% from 'forms.twig' import badge as show %}{{ forms.badge('x') }}{{ show('y') }}";
        let host = install("templates/a.twig", source);
        host.add_file("templates/forms.twig","{% macro badge(label) %}{{ label }}{% endmacro badge %}{% macro button() %}{% endmacro %}");
        let d = doc("/project/templates/a.twig", source);
        let mut plugin = Twig::default();
        let pending = action(&mut plugin, &d, "badge as");
        host.answer_next_prompt(Some(&["button"]));
        assert!(plugin.resolve_file_action(&d, pending).is_err());
        let pending = action(&mut plugin, &d, "badge as");
        host.answer_next_prompt(Some(&["pill"]));
        host.answer_next_prompt(Some(&["yes"]));
        let resolved = plugin.resolve_file_action(&d, pending).unwrap();
        assert_eq!(resolved.edits.len(), 2);
        assert_eq!(
            resolved.edits.iter().map(|(_, e)| e.len()).sum::<usize>(),
            4
        );
        assert!(
            resolved
                .edits
                .iter()
                .flat_map(|(_, e)| e)
                .all(|e| e.text == "pill")
        );
        assert!(host.editor_text().contains("badge"));
    }
    #[test]
    fn inherited_block_string_rename_updates_exact_family_but_rejects_delimiter_injection() {
        let source =
            "{% extends 'base.twig' %}{% block body %}{{ block('body') }}{% endblock body %}";
        let host = install("templates/child.twig", source);
        host.add_file(
            "templates/base.twig",
            "{% block body %}base{% endblock body %}",
        );
        let d = doc("/project/templates/child.twig", source);
        let mut plugin = Twig::default();
        let pending = action(&mut plugin, &d, "body %}");
        host.answer_next_prompt(Some(&["x' ~ malicious ~ '"]));
        assert!(matches!(
            plugin.resolve_file_action(&d, pending),
            Err(Error::Invalid(_))
        ));
        let pending = action(&mut plugin, &d, "body %}");
        host.answer_next_prompt(Some(&["content"]));
        host.answer_next_prompt(Some(&["yes"]));
        let resolved = plugin.resolve_file_action(&d, pending).unwrap();
        assert_eq!(resolved.edits.len(), 2);
        assert_eq!(
            resolved.edits.iter().map(|(_, e)| e.len()).sum::<usize>(),
            5
        );
    }
    #[test]
    fn oversized_exact_preview_is_refused_before_native_confirm_can_clip_it() {
        let source = "{% macro badge() %}{% endmacro %}{{ _self.badge() }}";
        let host = install("templates/a.twig", source);
        let d = doc("/project/templates/a.twig", source);
        let mut plugin = Twig::default();
        let action = action(&mut plugin, &d, "badge()");
        let pending = plugin
            .reviewed_symbols
            .pending
            .get(action.data.as_ref().unwrap())
            .unwrap()
            .clone();
        let mut files = BTreeMap::from([(d.path.clone(), d.text.clone())]);
        for i in 0..63 {
            files.insert(
                format!("/project/templates/{}-{i}.twig", "x".repeat(240)),
                "badge outside admitted Twig code".into(),
            );
        }
        let inventory = Inventory {
            paths: files.keys().cloned().collect(),
            files,
        };
        assert!(
            matches!(plan(&pending,&inventory,"pill"),Err(Error::Invalid(message)) if message.contains("review"))
        );
        assert_eq!(host.editor_text(), source);
    }
    #[test]
    fn changed_delimiter_settings_invalidate_even_a_still_matching_declaration_snapshot() {
        let source = "{% for item in items %}{{ item }}{% endfor %}";
        let host = install("a.twig", source);
        let d = doc("/project/a.twig", source);
        let mut plugin = Twig::default();
        let pending = action(&mut plugin, &d, "item in");
        host.set_setting("variableOpen", r#""[[""#);
        host.set_setting("variableClose", r#""]]""#);
        assert!(matches!(
            plugin.resolve_file_action(&d, pending),
            Err(Error::Stale)
        ));
        assert_eq!(host.editor_text(), source);
    }
    #[test]
    fn stale_snapshot_cancellation_and_project_reownership_refuse_edits() {
        let source = "{% for item in items %}{{ item }}{% endfor %}";
        let host = install("a.twig", source);
        let d = doc("/project/a.twig", source);
        let mut plugin = Twig::default();
        let pending = action(&mut plugin, &d, "item in");
        host.answer_next_prompt(None);
        assert!(matches!(
            plugin.resolve_file_action(&d, pending),
            Err(Error::Cancelled)
        ));
        let pending = action(&mut plugin, &d, "item in");
        host.open_editor(&d.path, "twig", "changed", &[]);
        assert!(matches!(
            plugin.resolve_file_action(&d, pending),
            Err(Error::Stale)
        ));
        host.open_editor(&d.path, "twig", source, &[]);
        let pending = action(&mut plugin, &d, "item in");
        host.set_project_root(Some("/other"));
        assert!(matches!(
            plugin.resolve_file_action(&d, pending),
            Err(Error::Stale)
        ));
    }
}
