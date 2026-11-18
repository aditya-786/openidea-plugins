//! Exact static template-source symbols. Dynamic/runtime identity is never inferred.
use crate::*;
use openidea_plugin::indexing::{Anchor, Document as IndexedDocument, Edges};
#[derive(Clone)]
struct Binding {
    name: String,
    remote: Option<String>,
    at: std::ops::Range<usize>,
    end: usize,
    owner: Option<usize>,
}
#[derive(Clone, Copy)]
struct Frame<'a> {
    name: &'a str,
    start: usize,
}
fn key(kind: &str, path: &str, name: &str) -> String {
    format!("twig:{kind}:{}:{path}:{name}", path.len())
}
fn anchor(key: String, text: &str, range: std::ops::Range<usize>) -> Anchor {
    Anchor {
        key,
        range: EditorRange::new(position(text, range.start), position(text, range.end)),
    }
}
fn identifier(name: &str) -> bool {
    name.as_bytes()
        .first()
        .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_' || *b >= 128)
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b >= 128)
        && !["_self", "_context", "loop", "varargs"].contains(&name)
}
thread_local! {static REVIEW_SOURCES:std::cell::RefCell<BTreeMap<String,String>>=std::cell::RefCell::new(BTreeMap::new());}
struct SourceGuard(BTreeMap<String, String>);
impl Drop for SourceGuard {
    fn drop(&mut self) {
        REVIEW_SOURCES.with(|v| *v.borrow_mut() = std::mem::take(&mut self.0));
    }
}
pub(crate) fn with_sources<T>(sources: BTreeMap<String, String>, f: impl FnOnce() -> T) -> T {
    let old = REVIEW_SOURCES.with(|v| v.replace(sources));
    let _guard = SourceGuard(old);
    f()
}
fn reviewed_source(path: &str) -> Option<String> {
    REVIEW_SOURCES.with(|v| v.borrow().get(path).cloned())
}

fn target(project: Option<&Project>, doc: &Document, name: &str) -> Option<(String, String)> {
    if name == "_self" {
        return Some((doc.path.clone(), doc.text.clone()));
    }
    let project = project?;
    if is_symfony(project) {
        return None;
    }
    let path = template_path(project, name)?;
    if let Some(source) = reviewed_source(&path) {
        return Some((path, source));
    }
    macros_of(project, doc, name)
}
struct Source {
    doc: Document,
    macros: BTreeMap<String, usize>,
    blocks: BTreeMap<String, usize>,
    parent: Option<Option<String>>,
}
struct Sources<'a> {
    project: Option<&'a Project>,
    cache: std::cell::RefCell<BTreeMap<String, Option<std::rc::Rc<Source>>>>,
    bytes: std::cell::Cell<usize>,
}
impl Sources<'_> {
    fn get(&self, doc: &Document, template: &str) -> Option<std::rc::Rc<Source>> {
        let id = format!("{}:{template}", doc.path);
        if let Some(source) = self.cache.borrow().get(&id) {
            return source.clone();
        }
        if self.cache.borrow().len() >= 16 {
            return None;
        }
        let source = target(self.project, doc, template).and_then(|(path, text)| {
            if self.bytes.get() + text.len() > 1 << 20 {
                return None;
            }
            self.bytes.set(self.bytes.get() + text.len());
            let parsed = lex(&text);
            if parsed.tags.len() > 1024
                || parsed.tokens.len() > 8192
                || !parsed.unterminated.is_empty()
                || !checks::check(&text).is_empty()
            {
                return None;
            }
            let mut macros = BTreeMap::new();
            for m in navigate::macros(&text) {
                *macros.entry(m.name).or_insert(0) += 1;
            }
            let mut blocks = BTreeMap::new();
            for tag in parsed.tags.iter().filter(|t| t.name == "block") {
                if let Some(r) = &tag.argument {
                    *blocks.entry(text[r.clone()].to_owned()).or_insert(0) += 1;
                }
            }
            let parent = parent_name(&text);
            Some(std::rc::Rc::new(Source {
                doc: Document {
                    path,
                    text,
                    language: "twig".into(),
                    version: 0,
                },
                macros,
                blocks,
                parent,
            }))
        });
        self.cache.borrow_mut().insert(id, source.clone());
        source
    }
}
fn macro_key(sources: &Sources, doc: &Document, template: &str, name: &str) -> Option<String> {
    let source = sources.get(doc, template)?;
    (source.macros.get(name) == Some(&1)).then(|| key("macro", &source.doc.path, name))
}
fn parent_name(text: &str) -> Option<Option<String>> {
    let parsed = lex(text);
    let mut parent = None;
    for tag in parsed.tags {
        if matches!(tag.name.as_str(), "use" | "embed") {
            return None;
        }
        if tag.name == "extends" {
            if parent.is_some() {
                return None;
            }
            let string = parsed.tokens.iter().find(|t| {
                t.kind == Highlight::String
                    && tag.range.start <= t.range.start
                    && t.range.end <= tag.range.end
            })?;
            let (name, _) = navigate::template_at(text, string.range.start + 1)?;
            parent = Some(name);
        }
    }
    Some(parent)
}
fn block_key(sources: &Sources, doc: &Document, name: &str) -> Option<String> {
    let mut source = sources.get(doc, "_self")?;
    let mut owner = None;
    let mut seen = BTreeMap::new();
    for _ in 0..8 {
        if seen.insert(source.doc.path.clone(), ()).is_some() {
            return None;
        }
        match source.blocks.get(name) {
            Some(1) => owner = Some(source.doc.path.clone()),
            Some(_) => return None,
            None => {}
        }
        match source.parent.as_ref()? {
            None => return owner.map(|path| key("block", &path, name)),
            Some(parent) => source = sources.get(&source.doc, parent)?,
        }
    }
    None
}
pub(crate) fn index(input: &IndexedDocument) -> Edges {
    let mut out = Edges::default();
    if !host::setting_bool("staticTemplateReferences").unwrap_or(true)
        || input.text.len() > 64 << 10
    {
        return out;
    }
    let doc = Document {
        path: input.path.clone(),
        text: input.text.clone(),
        language: "twig".into(),
        version: 0,
    };
    let text = &doc.text;
    let parsed = lex(text);
    if parsed.tags.len() > 1024
        || parsed.tokens.len() > 8192
        || !parsed.unterminated.is_empty()
        || !checks::check(text).is_empty()
    {
        return out;
    }
    if parsed.tokens.iter().any(|t| {
        matches!(&text[t.range.clone()], "_context" | "attribute")
            || t.kind == Highlight::Operator
                && &text[t.range.clone()] == "."
                && text[t.range.end..].trim_start().starts_with('(')
            || t.kind == Highlight::String && text[t.range.clone()].contains("#{")
    }) {
        return out;
    }
    let project = project_of(&doc.path);
    let sources = Sources {
        project: project.as_ref(),
        cache: std::cell::RefCell::new(BTreeMap::new()),
        bytes: std::cell::Cell::new(0),
    };
    let mut bindings = vec![];
    let mut stack: Vec<Frame> = vec![];
    let mut states = vec![(0, None, false)];
    let containers = ["block", "macro", "embed"];
    let known = [
        "apply",
        "autoescape",
        "block",
        "cache",
        "embed",
        "for",
        "if",
        "macro",
        "sandbox",
        "set",
        "verbatim",
        "raw",
        "filter",
        "spaceless",
        "with",
    ];
    for tag in &parsed.tags {
        let owner = stack
            .iter()
            .rev()
            .find(|f| containers.contains(&f.name))
            .map(|f| f.start);
        let conditional = stack.iter().any(|f| !containers.contains(&f.name));
        if matches!(tag.name.as_str(), "import" | "from") {
            let imports = navigate::imports(&text[tag.range.clone()]);
            if imports.len() != 1 || conditional {
                return Edges::default();
            }
            let tokens: Vec<_> = parsed
                .tokens
                .iter()
                .filter(|t| {
                    tag.range.start <= t.range.start
                        && t.range.end <= tag.range.end
                        && !parsed.boundaries.iter().any(|(r, _)| *r == t.range)
                })
                .collect();
            match &imports[0] {
                navigate::Import::Alias { template: _, alias } => {
                    let Some(last) = tokens.last() else {
                        return Edges::default();
                    };
                    if !identifier(alias) || &text[last.range.clone()] != alias {
                        return Edges::default();
                    }
                    bindings.push(Binding {
                        name: alias.clone(),
                        remote: None,
                        at: last.range.clone(),
                        end: tag.range.end,
                        owner,
                    });
                }
                navigate::Import::Names { template, names } => {
                    let mut at = 3usize;
                    for (remote, local) in names {
                        let Some(source) = tokens.get(at) else {
                            return Edges::default();
                        };
                        if &text[source.range.clone()] != remote {
                            return Edges::default();
                        }
                        if let Some(key) = macro_key(&sources, &doc, template, remote) {
                            out.references.push(anchor(key, text, source.range.clone()));
                        }
                        at += 1;
                        if local != remote
                            || tokens
                                .get(at)
                                .is_some_and(|t| &text[t.range.clone()] == "as")
                        {
                            let Some(alias) = tokens.get(at + 1) else {
                                return Edges::default();
                            };
                            if !identifier(local) || &text[alias.range.clone()] != local {
                                return Edges::default();
                            }
                            bindings.push(Binding {
                                name: local.clone(),
                                remote: Some(remote.clone()),
                                at: alias.range.clone(),
                                end: tag.range.end,
                                owner,
                            });
                            at += 2;
                        }
                        if tokens
                            .get(at)
                            .is_some_and(|t| &text[t.range.clone()] == ",")
                        {
                            at += 1;
                        }
                    }
                    if at != tokens.len() {
                        return Edges::default();
                    }
                }
            }
        }
        if let Some(name) = tag.name.strip_prefix("end").filter(|n| known.contains(n)) {
            if stack.pop().is_none_or(|f| f.name != name) {
                return Edges::default();
            }
        } else if tag.paired && known.contains(&tag.name.as_str()) {
            stack.push(Frame {
                name: &tag.name,
                start: tag.range.start,
            });
        } else if !known.contains(&tag.name.as_str())
            && !matches!(
                tag.name.as_str(),
                "import"
                    | "from"
                    | "else"
                    | "elseif"
                    | "extends"
                    | "include"
                    | "use"
                    | "do"
                    | "flush"
                    | "deprecated"
                    | "types"
            )
        {
            return Edges::default();
        }
        states.push((
            tag.range.end,
            stack
                .iter()
                .rev()
                .find(|f| containers.contains(&f.name))
                .map(|f| f.start),
            stack.iter().any(|f| matches!(f.name, "macro" | "embed")),
        ));
    }
    if !stack.is_empty() {
        return Edges::default();
    }
    if bindings.len() > 128 {
        return Edges::default();
    }
    if !bindings.is_empty()
        && parsed
            .tags
            .iter()
            .filter(|tag| matches!(tag.name.as_str(), "set" | "for"))
            .any(|tag| {
                parsed.tokens.iter().any(|t| {
                    tag.range.start <= t.range.start
                        && t.range.end <= tag.range.end
                        && bindings.iter().any(|b| b.name == text[t.range.clone()])
                })
            })
    {
        return Edges::default();
    }
    let choose = |name: &str, at: usize| -> Option<&Binding> {
        let state = states.get(
            states
                .partition_point(|(offset, _, _)| *offset <= at)
                .saturating_sub(1),
        )?;
        let local: Vec<_> = bindings
            .iter()
            .filter(|b| b.name == name && b.owner == state.1)
            .collect();
        let chosen = if local.is_empty() && !state.2 {
            bindings
                .iter()
                .filter(|b| b.name == name && b.owner.is_none())
                .collect()
        } else {
            local
        };
        if chosen.len() != 1 || chosen[0].end > at {
            return None;
        }
        Some(chosen[0])
    };
    let mut invalid = BTreeMap::new();
    for binding in &bindings {
        if bindings
            .iter()
            .filter(|b| b.name == binding.name && b.owner == binding.owner)
            .count()
            != 1
        {
            invalid.insert(binding.at.start, ());
        }
    }
    let mut alias_refs: Vec<(usize, Range<usize>)> = vec![];
    for token in &parsed.tokens {
        let raw = &text[token.range.clone()];
        if bindings.iter().any(|b| b.at == token.range) {
            continue;
        }
        if matches!(
            token.kind,
            Highlight::Variable | Highlight::SelfVariable | Highlight::FunctionCall
        ) {
            if let Some(binding) = choose(raw, token.range.start) {
                let after = text[token.range.end..].trim_start();
                if binding.remote.is_none() && after.starts_with('.') && !after.starts_with(".(")
                    || binding.remote.is_some() && after.starts_with('(')
                {
                    alias_refs.push((binding.at.start, token.range.clone()));
                } else {
                    invalid.insert(binding.at.start, ());
                }
            }
        }
    }
    for binding in &bindings {
        if invalid.contains_key(&binding.at.start) {
            continue;
        }
        let alias_key = key(
            "macro-alias",
            &doc.path,
            &format!("{}:{}", binding.at.start, binding.name),
        );
        out.definitions
            .push(anchor(alias_key.clone(), text, binding.at.clone()));
        out.references.extend(
            alias_refs
                .iter()
                .filter(|(start, _)| *start == binding.at.start)
                .map(|(_, range)| anchor(alias_key.clone(), text, range.clone())),
        );
    }
    let macros = navigate::macros(text);
    for declaration in &macros {
        if macros.iter().filter(|m| m.name == declaration.name).count() == 1 {
            out.definitions.push(anchor(
                key("macro", &doc.path, &declaration.name),
                text,
                declaration.name_range.clone(),
            ));
        }
    }
    let mut block_stack = vec![];
    let mut macro_stack = vec![];
    for tag in &parsed.tags {
        if tag.name == "macro" {
            if let Some(range) = &tag.argument {
                macro_stack.push(text[range.clone()].to_owned());
            }
        }
        if tag.name == "endmacro" {
            if let Some(name) = macro_stack.pop() {
                if let Some(range) = &tag.argument {
                    if &text[range.clone()] != name {
                        return Edges::default();
                    }
                    if macros.iter().filter(|m| m.name == name).count() == 1 {
                        out.references.push(anchor(
                            key("macro", &doc.path, &name),
                            text,
                            range.clone(),
                        ));
                    }
                }
            }
        }
        if tag.name == "block" {
            if let Some(range) = &tag.argument {
                let name = &text[range.clone()];
                if let Some(key) = block_key(&sources, &doc, name) {
                    out.definitions.push(anchor(key, text, range.clone()));
                }
                if tag.paired {
                    block_stack.push(name.to_owned());
                }
            }
        }
        if tag.name == "endblock" {
            if let Some(name) = block_stack.pop() {
                if let Some(range) = &tag.argument {
                    if &text[range.clone()] != name {
                        return Edges::default();
                    }
                    if let Some(key) = block_key(&sources, &doc, &name) {
                        out.references.push(anchor(key, text, range.clone()));
                    }
                }
            }
        }
    }
    for (i, token) in parsed.tokens.iter().enumerate() {
        if parsed
            .tags
            .get(
                parsed
                    .tags
                    .partition_point(|tag| tag.range.end <= token.range.start),
            )
            .is_some_and(|tag| {
                matches!(
                    tag.name.as_str(),
                    "macro"
                        | "endmacro"
                        | "import"
                        | "from"
                        | "block"
                        | "endblock"
                        | "extends"
                        | "use"
                ) && tag.range.start <= token.range.start
                    && token.range.end <= tag.range.end
            })
        {
            continue;
        }
        let raw = &text[token.range.clone()];
        if token.kind == Highlight::String
            && i >= 2
            && parsed
                .tokens
                .get(i + 1)
                .is_some_and(|t| &text[t.range.clone()] == ")")
            && &text[parsed.tokens[i - 1].range.clone()] == "("
            && &text[parsed.tokens[i - 2].range.clone()] == "block"
            && (i < 3 || &text[parsed.tokens[i - 3].range.clone()] != ".")
        {
            if raw.len() >= 2 {
                let name = &raw[1..raw.len() - 1];
                if identifier(name) {
                    if let Some(key) = block_key(&sources, &doc, name) {
                        out.references.push(anchor(
                            key,
                            text,
                            token.range.start + 1..token.range.end - 1,
                        ));
                    }
                }
            }
            continue;
        }
        if !matches!(
            token.kind,
            Highlight::FunctionCall | Highlight::Function | Highlight::Property
        ) {
            continue;
        }
        let Some((receiver, name, range)) = navigate::dotted_at(text, token.range.start) else {
            continue;
        };
        if range != token.range || !navigate::code_at(text, range.start, false) {
            continue;
        }
        if let Some(alias) = &receiver {
            let Some(start) = range.start.checked_sub(alias.len() + 1) else {
                continue;
            };
            if text[..start]
                .chars()
                .last()
                .is_some_and(|c| c == '.' || c == '_' || c.is_alphanumeric())
            {
                continue;
            }
            let template = if alias == "_self" {
                Some("_self".into())
            } else {
                navigate::binding(text, range.start, Some(alias), &name).map(|b| b.0)
            };
            if let Some(template) = template {
                if let Some(key) = macro_key(&sources, &doc, &template, &name) {
                    out.references.push(anchor(key, text, range));
                }
            }
        } else if text[range.end..].trim_start().starts_with('(') {
            if choose(&name, range.start).is_some_and(|b| b.remote.is_some()) {
                continue;
            }
            if let Some((template, remote)) = navigate::binding(text, range.start, None, &name) {
                if let Some(key) = macro_key(&sources, &doc, &template, &remote) {
                    out.references.push(anchor(key, text, range));
                }
            }
        }
    }
    if out.definitions.len() + out.references.len() > 4096 {
        return Edges::default();
    }
    if let Some(project) = project.as_ref() {
        let Some(root) = project.root().ok() else {
            return Edges::default();
        };
        if host::project_of(&doc.path)
            .and_then(|p| p.root().ok())
            .as_deref()
            != Some(root.as_str())
        {
            return Edges::default();
        }
        for source in sources.cache.borrow().values().flatten() {
            if host::project_of(&source.doc.path)
                .and_then(|p| p.root().ok())
                .as_deref()
                != Some(root.as_str())
                || source.doc.path != doc.path
                    && reviewed_source(&source.doc.path)
                        .or_else(|| project.read_file(&source.doc.path).ok())
                        .as_deref()
                        != Some(source.doc.text.as_str())
            {
                return Edges::default();
            }
        }
    }
    out
}
#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    fn doc(path: &str, text: &str) -> IndexedDocument {
        IndexedDocument {
            path: path.into(),
            text: text.into(),
            symbols: vec![],
        }
    }
    fn selected(text: &str, a: &Anchor) -> String {
        text[offset(text, a.range.start).unwrap()..offset(text, a.range.end).unwrap()].to_owned()
    }
    #[test]
    fn current_macro_and_named_end_have_one_physical_identity() {
        let _host = MockHost::install();
        let source =
            "😀 {% macro badge(label) %}{{ label }}{% endmacro badge %}{{ _self.badge('x') }}";
        let found = crate::indexing::source_edges(&doc("/project/templates/a.twig", source));
        assert_eq!(found.definitions.len(), 1);
        assert_eq!(found.references.len(), 2);
        assert_eq!(selected(source, &found.definitions[0]), "badge");
        assert!(
            found
                .references
                .iter()
                .all(|a| a.key == found.definitions[0].key && selected(source, a) == "badge")
        );
    }
    #[test]
    fn unicode_block_literal_and_named_end_share_exact_owner_ranges() {
        let _host = MockHost::install();
        let source = "😀 {% block café %}{{ block('café') }}{% endblock café %}";
        let found = crate::indexing::source_edges(&doc("/project/templates/a.twig", source));
        assert_eq!(found.definitions.len(), 1);
        assert_eq!(found.references.len(), 2);
        assert!(
            found
                .references
                .iter()
                .all(|a| a.key == found.definitions[0].key && selected(source, a) == "café")
        );
    }
    #[test]
    fn literal_namespace_and_from_aliases_separate_local_binding_from_exported_macro() {
        let host = MockHost::install();
        host.add_file(
            "templates/forms.twig",
            "{% macro badge(label) %}{{ label }}{% endmacro %}",
        );
        let source = "{% import 'forms.twig' as forms %}{% from 'forms.twig' import badge as show %}{{ forms.badge('x') }}{{ show('y') }}";
        let found = crate::indexing::source_edges(&doc("/project/templates/a.twig", source));
        assert_eq!(found.definitions.len(), 2);
        let namespace = found
            .definitions
            .iter()
            .find(|a| selected(source, a) == "forms")
            .unwrap();
        let local = found
            .definitions
            .iter()
            .find(|a| selected(source, a) == "show")
            .unwrap();
        assert_eq!(
            found
                .references
                .iter()
                .filter(|a| a.key == namespace.key)
                .count(),
            1
        );
        assert_eq!(
            found
                .references
                .iter()
                .filter(|a| a.key == local.key)
                .count(),
            1
        );
        assert_eq!(
            found
                .references
                .iter()
                .filter(|a| a.key.contains("twig:macro:") && selected(source, a) == "badge")
                .count(),
            2
        );
        assert_ne!(namespace.key, local.key);
    }
    #[test]
    fn literal_inheritance_groups_overrides_and_named_block_ends_with_base_definition() {
        let host = MockHost::install();
        let base = "{% block body %}base{% endblock body %}";
        host.add_file("templates/base.twig", base);
        let base_edges = crate::indexing::source_edges(&doc("/project/templates/base.twig", base));
        assert_eq!(base_edges.definitions.len(), 1);
        let child =
            "😀 {% extends 'base.twig' %}{% block body %}{{ block('body') }}{% endblock body %}";
        let child_edges =
            crate::indexing::source_edges(&doc("/project/templates/child.twig", child));
        assert_eq!(child_edges.definitions.len(), 1);
        assert_eq!(child_edges.references.len(), 2);
        assert_eq!(
            base_edges.definitions[0].key,
            child_edges.definitions[0].key
        );
        assert!(
            child_edges
                .references
                .iter()
                .all(|a| a.key == base_edges.definitions[0].key && selected(child, a) == "body")
        );
    }
    #[test]
    fn duplicate_dynamic_shadowed_and_foreign_sources_never_acquire_guessed_identity() {
        let host = MockHost::install();
        for source in [
            "{% macro badge() %}{% endmacro %}{% macro badge() %}{% endmacro %}{{ _self.badge() }}",
            "{% import dynamic as forms %}{{ forms.badge() }}",
            "{% import 'a.twig' as forms %}{% import 'b.twig' as forms %}{{ forms.badge() }}",
            "{% import 'forms.twig' as forms %}{% set forms = other %}{{ forms.badge() }}",
            "{% macro badge() %}{% endmacro %}{{ _self.(dynamic)() }}",
        ] {
            let found = crate::indexing::source_edges(&doc("/project/templates/a.twig", source));
            assert!(
                found.definitions.is_empty() && found.references.is_empty(),
                "{source}"
            );
        }
        host.add_file(
            "templates/forms.twig",
            "{% macro badge() %}{% endmacro %}{% macro badge() %}{% endmacro %}",
        );
        let source = "{% from 'forms.twig' import badge %}{{ badge() }}";
        let found = crate::indexing::source_edges(&doc("/project/templates/a.twig", source));
        assert!(found.references.is_empty());
    }
}
