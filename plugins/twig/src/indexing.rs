//! Closed local for-loop binding scopes, including key/value loops and conditional bodies. Other tag families and dynamic context
//! access are deliberately not admitted; equal spellings do not imply identity.
//! Models are private: API1.4 cannot validate arbitrary native Rename replacements.
use super::*;
use openidea_plugin::indexing::{Anchor, Document as IndexedDocument, Edges};
struct Binding {
    name: String,
    at: Range<usize>,
    uses: Vec<Range<usize>>,
}
struct Scope {
    kind: &'static str,
    bindings: Vec<usize>,
    has_else: bool,
}
fn collect(text: &str) -> Option<Vec<Binding>> {
    if text.len() > 64 << 10 {
        return None;
    }
    let parsed = lex(text);
    if !parsed.unterminated.is_empty() || parsed.tags.len() > 512 || parsed.tokens.len() > 8192 {
        return None;
    }
    if parsed.tokens.iter().any(|t| {
        let raw = &text[t.range.clone()];
        matches!(raw, "_context" | "attribute" | "dump" | "include")
            || t.kind == Highlight::String && raw.contains("#{")
            || t.kind == Highlight::Operator && text[t.range.start..].starts_with("=>")
    }) {
        return None;
    }
    let mut bindings: Vec<Binding> = Vec::new();
    let mut stack: Vec<Scope> = Vec::new();
    let mut cursor = 0;
    let uses =
        |from: usize, to: usize, stack: &[Scope], bindings: &mut Vec<Binding>| {
            for token in parsed.tokens.iter().filter(|t| {
                from <= t.range.start && t.range.end <= to && t.kind == Highlight::Variable
            }) {
                let name = &text[token.range.clone()];
                if let Some(i) = stack
                    .iter()
                    .rev()
                    .flat_map(|scope| scope.bindings.iter().rev().copied())
                    .find(|i| bindings[*i].name == name)
                {
                    bindings[i].uses.push(token.range.clone());
                }
            }
        };
    for tag in &parsed.tags {
        let body = crate::delimiters::Syntax::current()
            .tag_body(&text[tag.range.clone()])?
            .trim_matches([' ', '\t', '\r', '\n', '-', '~']);
        if matches!(tag.name.as_str(), "if" | "elseif")
            && body.strip_prefix(&tag.name)?.trim().is_empty()
        {
            return None;
        }
        if matches!(tag.name.as_str(), "else" | "endif") && body != tag.name {
            return None;
        }
        uses(cursor, tag.range.start, &stack, &mut bindings);
        match tag.name.as_str() {
            "for" => {
                if stack.len() >= 32 {
                    return None;
                }
                let tokens: Vec<_> = parsed
                    .tokens
                    .iter()
                    .filter(|t| tag.range.start <= t.range.start && t.range.end <= tag.range.end)
                    .collect();
                let keyword = tokens
                    .iter()
                    .position(|t| &text[t.range.clone()] == "for")?;
                let mut names = vec![*tokens.get(keyword + 1)?];
                let mut next = keyword + 2;
                if &text[tokens.get(next)?.range.clone()] == "," {
                    names.push(*tokens.get(next + 1)?);
                    next += 2;
                }
                let mut unique = std::collections::BTreeSet::new();
                for name in &names {
                    let word = &text[name.range.clone()];
                    if name.kind != Highlight::Variable
                        || word == "loop"
                        || !word
                            .as_bytes()
                            .first()
                            .is_some_and(|b| is_identifier_start(*b))
                        || !word.bytes().all(is_identifier)
                        || !unique.insert(word)
                    {
                        return None;
                    }
                }
                if &text[tokens.get(next)?.range.clone()] != "in" || tokens.len() <= next + 2 {
                    return None;
                }
                if tokens
                    .iter()
                    .skip(next + 1)
                    .any(|t| matches!(&text[t.range.clone()], "if" | "recursive"))
                {
                    return None;
                }
                uses(tokens[next].range.end, tag.range.end, &stack, &mut bindings);
                let mut indices = vec![];
                for name in names {
                    indices.push(bindings.len());
                    bindings.push(Binding {
                        name: text[name.range.clone()].into(),
                        at: name.range.clone(),
                        uses: vec![],
                    });
                }
                stack.push(Scope {
                    kind: "for",
                    bindings: indices,
                    has_else: false,
                });
            }
            "endfor" => {
                // A closing tag has no expressions or alternate binding syntax.
                let raw = &text[tag.range.clone()];
                if raw
                    .trim_start_matches(crate::delimiters::Syntax::current().values[0].as_str())
                    .trim_end_matches(crate::delimiters::Syntax::current().values[1].as_str())
                    .trim_matches([' ', '\t', '\r', '\n', '-', '~'])
                    != "endfor"
                {
                    return None;
                }
                if stack.pop()?.kind != "for" {
                    return None;
                }
            }
            "if" => {
                if stack.len() >= 32 {
                    return None;
                }
                uses(tag.range.start, tag.range.end, &stack, &mut bindings);
                stack.push(Scope {
                    kind: "if",
                    bindings: vec![],
                    has_else: false,
                });
            }
            "elseif" | "else" => {
                let frame = stack.last_mut()?;
                if frame.kind != "if" || frame.has_else {
                    return None;
                }
                if tag.name == "else" {
                    frame.has_else = true;
                }
                uses(tag.range.start, tag.range.end, &stack, &mut bindings);
            }
            "endif" => {
                if stack.pop()?.kind != "if" {
                    return None;
                }
            }
            _ => return None,
        }
        cursor = tag.range.end;
    }
    uses(cursor, text.len(), &stack, &mut bindings);
    if !stack.is_empty() || bindings.iter().map(|b| 1 + b.uses.len()).sum::<usize>() > 4096 {
        return None;
    }
    Some(bindings)
}
pub(crate) fn source_edges(doc: &IndexedDocument) -> Edges {
    let Some(_syntax) = crate::delimiters::Syntax::scope() else {
        return Edges::default();
    };
    let mut edges = crate::template_index::index(doc);
    if !doc.path.ends_with(".twig") {
        return edges;
    }
    let Some(bindings) = collect(&doc.text) else {
        return edges;
    };
    for b in bindings {
        let key = format!(
            "twig:loop:{}:{}:{}:{}",
            doc.path.len(),
            doc.path,
            b.at.start,
            b.name
        );
        let anchor = |r: Range<usize>| Anchor {
            key: key.clone(),
            range: EditorRange::new(position(&doc.text, r.start), position(&doc.text, r.end)),
        };
        edges.definitions.push(anchor(b.at));
        edges.references.extend(b.uses.into_iter().map(anchor));
    }
    edges
}

#[cfg(test)]
mod tests {
    use super::*;
    fn doc(text: &str) -> IndexedDocument {
        IndexedDocument {
            path: "/p/a.twig".into(),
            text: text.into(),
            symbols: vec![],
        }
    }
    #[test]
    fn key_value_loop_and_conditionals_publish_exact_local_owners() {
        let _host = openidea_plugin::mock::MockHost::install();
        let text = "😀{% for key, item in items %}{% if item.active %}{{ key }} {{ item.name }}{% elseif item.hidden %}{{ item }}{% else %}{{ key }}{% endif %}{% endfor %}{{ key }}";
        let edges = source_edges(&doc(text));
        assert_eq!(edges.definitions.len(), 2);
        let keys = edges.definitions[0].key.clone();
        let items = edges.definitions[1].key.clone();
        assert_eq!(edges.references.iter().filter(|a| a.key == keys).count(), 2);
        assert_eq!(
            edges.references.iter().filter(|a| a.key == items).count(),
            4
        );
        for a in &edges.definitions {
            let raw =
                &text[offset(text, a.range.start).unwrap()..offset(text, a.range.end).unwrap()];
            assert!(["key", "item"].contains(&raw));
        }
        for text in [
            "{% for x, x in items %}{{ x }}{% endfor %}",
            "{% for key, value in items %}{% if value %}{{ key }}{% endfor %}{% endif %}",
            "{% for key, value in items %}{% else %}{{ key }}{% endfor %}",
            "{% for item in items %}{% if %}{{ item }}{% endif %}{% endfor %}",
            "{% for item in items %}{% if item %}{{ item }}{% endif item %}{% endfor %}",
        ] {
            assert!(source_edges(&doc(text)).definitions.is_empty(), "{text}");
        }
    }
    #[test]
    fn nested_shadowing_and_iterable_use_outer_binding() {
        let _host = openidea_plugin::mock::MockHost::install();
        let text = "😀{% for item in items %}{{ item.name }}{{ object.item }}{% for item in item.children %}{{ item }}{% endfor %}{{ item }}{% endfor %}{{ item }}";
        let edges = source_edges(&doc(text));
        assert_eq!(edges.definitions.len(), 2);
        assert_ne!(edges.definitions[0].key, edges.definitions[1].key);
        assert_eq!(
            edges
                .references
                .iter()
                .filter(|a| a.key == edges.definitions[0].key)
                .count(),
            3
        );
        assert_eq!(
            edges
                .references
                .iter()
                .filter(|a| a.key == edges.definitions[1].key)
                .count(),
            1
        );
        for a in edges.definitions.iter().chain(&edges.references) {
            assert_eq!(
                &text[offset(text, a.range.start).unwrap()..offset(text, a.range.end).unwrap()],
                "item"
            );
        }
    }
    #[test]
    fn dynamic_incomplete_and_assignment_scopes_do_not_publish() {
        let _host = openidea_plugin::mock::MockHost::install();
        for text in [
            "{% for item in items %}{{ item }}",
            "{% for item in items %}{{ other|map(item => item.name) }}{% endfor %}",
            "{% for item in items %}{% set item = other %}{{ item }}{% endfor %}",
            "{% for item in items %}{{ _context['item'] }}{% endfor %}",
            "{% for item in items if item.active %}{{ item }}{% endfor %}",
            "{% for item in items %}{{ \"#{item}\" }}{% endfor %}",
            "{% for item in items %}{% else %}{{ item }}{% endfor %}",
        ] {
            assert!(source_edges(&doc(text)).definitions.is_empty(), "{text}");
        }
    }
}
