//! Reviewed rename of one macro parameter inside its own `{% macro %}` definition.
//! A FileAction with a confirmation preview, not a native Rename anchor: every
//! ambiguous construct (shadowing, strings, comments, nested scopes) refuses the whole edit.
use crate::*;
use openidea_plugin::file_actions::FileAction;

pub const PREFIX: &str = "macro-param:";
const NOT_PARAM: &str = "Place the caret on a parameter name in a {% macro name(...) %} tag.";
const RESERVED: &[&str] = &[
    "true", "false", "null", "none", "and", "or", "not", "in", "is", "matches", "starts", "ends", "with", "same", "as", "divisible",
    "by", "constant", "defined", "empty", "even", "odd", "iterable", "_self", "_context", "_charset", "loop",
];

pub struct Collected {
    pub name: String,
    pub uses: Vec<Range<usize>>,
    params: Vec<String>,
    defaults: Vec<Range<usize>>,
    body: Range<usize>,
}

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 128
}

fn ends_word(text: &str, word: &str) -> bool {
    text.ends_with(word) && text[..text.len() - word.len()].bytes().next_back().is_none_or(|b| !is_word(b))
}

fn find_word(text: &str, word: &str) -> Option<usize> {
    text.match_indices(word).map(|(at, _)| at).find(|at| {
        text[..*at].bytes().next_back().is_none_or(|b| !is_word(b)) && text[at + word.len()..].bytes().next().is_none_or(|b| !is_word(b))
    })
}

/// Walks one expression; bare variable uses of `name` go to `out`. In strict mode anything that could
/// hide another meaning of the word (string, `name:`, `name =`, arrow functions) is an error.
fn scan_expr(text: &str, range: Range<usize>, name: &str, strict: bool, out: &mut Vec<Range<usize>>) -> Result<(), String> {
    let bytes = text.as_bytes();
    let arrow = text[range.clone()].contains("=>");
    let mut i = range.start;
    while i < range.end {
        let c = bytes[i];
        if c == b'\'' || c == b'"' {
            let mut j = i + 1;
            while j < range.end && bytes[j] != c {
                j += usize::from(bytes[j] == b'\\') + 1;
            }
            let j = j.min(range.end);
            if strict && find_word(&text[i..j], name).is_some() {
                return Err(format!("`{name}` also appears inside a string; rename refused."));
            }
            i = j + 1;
            continue;
        }
        if c.is_ascii_digit() {
            while i < range.end && is_word(bytes[i]) {
                i += 1;
            }
            continue;
        }
        if !(c.is_ascii_alphabetic() || c == b'_' || c >= 128) {
            i += 1;
            continue;
        }
        let start = i;
        while i < range.end && is_word(bytes[i]) {
            i += 1;
        }
        if &text[start..i] != name {
            continue;
        }
        let before = text[range.start..start].trim_end();
        let after = text[i..range.end].trim_start();
        let attribute = before.ends_with('.') && !before.ends_with("..");
        let filter = before.ends_with('|');
        let test = ends_word(before, "is") || (ends_word(before, "not") && ends_word(before[..before.len() - 3].trim_end(), "is"));
        if attribute || filter || test || after.starts_with('(') {
            continue;
        }
        let ambiguous = (after.starts_with(':') && !after.starts_with("::")) || (after.starts_with('=') && !after.starts_with("=="));
        if strict && ambiguous {
            return Err(format!("`{name}` is used as a hash key, named argument or assignment here; rename refused."));
        }
        if strict && arrow {
            return Err(format!("An arrow function in the same expression may shadow `{name}`; rename refused."));
        }
        out.push(start..i);
    }
    Ok(())
}

/// A binding position: any mention is a shadow in strict mode and a collision otherwise.
fn scan_binding(text: &str, range: Range<usize>, name: &str, strict: bool, out: &mut Vec<Range<usize>>) -> Result<(), String> {
    let mut found = Vec::new();
    scan_expr(text, range, name, false, &mut found)?;
    if strict && !found.is_empty() {
        return Err(format!("`{name}` is rebound by a set/for/import/from tag; rename refused."));
    }
    out.extend(found);
    Ok(())
}

fn scan_code(text: &str, range: Range<usize>, is_tag: bool, name: &str, strict: bool, out: &mut Vec<Range<usize>>) -> Result<(), String> {
    if !is_tag {
        return scan_expr(text, range, name, strict, out);
    }
    let lead = text[range.clone()].len() - text[range.clone()].trim_start().len();
    let word_start = range.start + lead;
    let word_len = text[word_start..range.end].bytes().take_while(|b| is_word(*b)).count();
    let tag = &text[word_start..word_start + word_len];
    let rest = word_start + word_len..range.end;
    let rest_text = &text[rest.clone()];
    match tag {
        "verbatim" | "raw" if strict => Err("A verbatim block inside the macro is not analysed; rename refused.".into()),
        "embed" | "block" | "macro" if strict => Err(format!("A nested {tag} construct inside the macro; rename refused.")),
        "import" | "from" => scan_binding(text, rest, name, strict, out),
        "set" => {
            let split = rest_text.find('=').map_or(rest.end, |at| rest.start + at);
            scan_binding(text, rest.start..split, name, strict, out)?;
            scan_expr(text, (split + 1).min(rest.end)..rest.end, name, strict, out)
        }
        "for" => {
            let split = find_word(rest_text, "in").map_or(rest.end, |at| rest.start + at);
            scan_binding(text, rest.start..split, name, strict, out)?;
            scan_expr(text, split..rest.end, name, strict, out)
        }
        "with" if strict && find_word(rest_text, "only").is_some() => Err("A `with ... only` scope hides outer names; rename refused.".into()),
        _ => scan_expr(text, rest, name, strict, out),
    }
}

/// Closer search that ignores delimiters inside quotes; returns where code ends and where the closer ends.
fn find_close(text: &str, from: usize, to: usize, close: &str) -> Option<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut quote = 0u8;
    let mut j = from;
    while j < to {
        let c = bytes[j];
        if quote != 0 {
            j += usize::from(c == b'\\');
            if c == quote {
                quote = 0;
            }
        } else if c == b'\'' || c == b'"' {
            quote = c;
        } else if text[j..to].starts_with(close) {
            return Some((j, j + close.len()));
        } else if matches!(c, b'-' | b'~') && text[j + 1..to].starts_with(close) {
            return Some((j, j + 1 + close.len()));
        }
        j += 1;
    }
    None
}

fn scan_region(text: &str, range: Range<usize>, name: &str, strict: bool, out: &mut Vec<Range<usize>>) -> Result<(), String> {
    let v = delimiters::Syntax::current().values;
    let mut i = range.start;
    while i < range.end {
        let rest = &text[i..range.end];
        let Some((at, open)) = [0usize, 2, 4].into_iter().filter_map(|o| rest.find(&v[o]).map(|p| (p, o))).min() else {
            break;
        };
        let mut content = i + at + v[open].len();
        if open != 4 && text[content..range.end].starts_with(['-', '~']) {
            content += 1;
        }
        let close = &v[open + 1];
        let Some((code_end, after)) = find_close(text, content, range.end, close) else {
            return Err("An unterminated Twig delimiter inside the macro; rename refused.".into());
        };
        if open == 4 {
            if strict && find_word(&text[content..code_end], name).is_some() {
                return Err(format!("`{name}` also appears inside a comment; rename refused."));
            }
        } else {
            scan_code(text, content..code_end, open == 0, name, strict, out)?;
        }
        i = after;
    }
    Ok(())
}

pub fn collect(text: &str, at: usize) -> Result<Collected, String> {
    if text.len() > 64 << 10 {
        return Err("The template is too large to analyse for a reviewed rename.".into());
    }
    let parsed = crate::lex(text);
    if parsed.tags.len() > 4096 || !parsed.unterminated.is_empty() {
        return Err("The template has unterminated delimiters; rename refused.".into());
    }
    let (index, tag) = parsed
        .tags
        .iter()
        .enumerate()
        .find(|(_, t)| t.name == "macro" && t.range.start <= at && at <= t.range.end)
        .ok_or(NOT_PARAM)?;
    let mut end = None;
    for t in &parsed.tags[index + 1..] {
        match t.name.as_str() {
            "macro" => return Err("Nested macro definitions are not supported; rename refused.".into()),
            "endmacro" => {
                end = Some(t.range.start);
                break;
            }
            _ => {}
        }
    }
    let end = end.ok_or("The macro has no {% endmacro %}; rename refused.")?;

    let v = delimiters::Syntax::current().values;
    let bytes = text.as_bytes();
    let mut p = tag.range.start + v[0].len();
    let skip_space = |mut p: usize| {
        while p < tag.range.end && bytes[p].is_ascii_whitespace() {
            p += 1;
        }
        p
    };
    if text[p..].starts_with(['-', '~']) {
        p += 1;
    }
    p = skip_space(p);
    if !text[p..].starts_with("macro") {
        return Err(NOT_PARAM.into());
    }
    p = skip_space(p + 5);
    while p < tag.range.end && is_word(bytes[p]) {
        p += 1;
    }
    p = skip_space(p);
    if bytes.get(p) != Some(&b'(') {
        return Err(NOT_PARAM.into());
    }
    let (mut j, mut depth, mut quote) = (p + 1, 0usize, 0u8);
    let mut part_start = j;
    let mut parts = Vec::new();
    loop {
        if j >= tag.range.end {
            return Err("The macro parameter list is not closed; rename refused.".into());
        }
        let c = bytes[j];
        if quote != 0 {
            j += usize::from(c == b'\\');
            if c == quote {
                quote = 0;
            }
        } else {
            match c {
                b'\'' | b'"' => quote = c,
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' if depth > 0 => depth -= 1,
                b')' => {
                    parts.push(part_start..j);
                    break;
                }
                b',' if depth == 0 => {
                    parts.push(part_start..j);
                    part_start = j + 1;
                }
                b']' | b'}' => return Err("Unbalanced brackets in the macro parameter list; rename refused.".into()),
                _ => {}
            }
        }
        j += 1;
    }
    let body = tag.range.end..end;
    let mut params = Vec::new();
    let mut idents = Vec::new();
    let mut defaults = Vec::new();
    for part in parts {
        let raw = &text[part.clone()];
        if raw.trim().is_empty() {
            continue;
        }
        let start = part.start + raw.len() - raw.trim_start().len();
        let len = text[start..part.end].bytes().take_while(|b| is_word(*b)).count();
        let ident = &text[start..start + len];
        if !macro_arguments::identifier(ident) {
            return Err("A macro parameter is not a plain identifier; rename refused.".into());
        }
        let after = text[start + len..part.end].trim_start();
        if !after.is_empty() {
            let Some(default) = after.strip_prefix('=').filter(|d| !d.starts_with(['=', '>'])) else {
                return Err("A macro parameter has an unsupported default; rename refused.".into());
            };
            defaults.push(part.end - default.len()..part.end);
        }
        if params.iter().any(|p| p == ident) {
            return Err("Duplicate macro parameter names; rename refused.".into());
        }
        params.push(ident.to_owned());
        idents.push(start..start + len);
    }
    let selected = idents.iter().position(|r| r.start <= at && at <= r.end).ok_or(NOT_PARAM)?;
    let name = params[selected].clone();
    let mut uses = vec![idents[selected].clone()];
    for default in &defaults {
        scan_expr(text, default.clone(), &name, true, &mut uses)?;
    }
    scan_region(text, body.clone(), &name, true, &mut uses)?;
    uses.sort_by_key(|r| r.start);
    Ok(Collected { name, uses, params, defaults, body })
}

fn collides(text: &str, found: &Collected, new: &str) -> Result<(), String> {
    let name = &found.name;
    if !macro_arguments::identifier(new) || new.len() > 64 || RESERVED.contains(&new) || new == name {
        return Err("Choose a different non-reserved Twig identifier (letters, digits, underscore).".into());
    }
    let mut hits = Vec::new();
    for default in &found.defaults {
        scan_expr(text, default.clone(), new, false, &mut hits)?;
    }
    scan_region(text, found.body.clone(), new, false, &mut hits)?;
    if found.params.iter().any(|p| p == new) || !hits.is_empty() {
        return Err(format!("`{new}` is already a parameter or variable in this macro; rename refused."));
    }
    Ok(())
}

pub fn offered(doc: &Document, selection: EditorRange) -> Option<FileAction> {
    if doc.language != "twig" || !doc.path.ends_with(".twig") {
        return None;
    }
    let at = offset(&doc.text, selection.start)?;
    let found = collect(&doc.text, at);
    let on_parameter = !matches!(&found, Err(message) if message == NOT_PARAM);
    let anchor = found.as_ref().map_or(at, |f| f.uses[0].start);
    on_parameter.then(|| FileAction {
        title: "Preview Rename Macro Parameter…".into(),
        kind: Some("refactor.rename".into()),
        data: Some(format!("{PREFIX}{}:{anchor}", doc.version)),
        ..FileAction::default()
    })
}

pub fn resolve(doc: &Document, action: FileAction) -> Result<FileAction, Error> {
    let data = action.data.as_deref().and_then(|d| d.strip_prefix(PREFIX)).ok_or(Error::Stale)?;
    let (version, at) = data.split_once(':').ok_or(Error::Stale)?;
    let (version, at): (u64, usize) = (version.parse().map_err(|_| Error::Stale)?, at.parse().map_err(|_| Error::Stale)?);
    if u64::from(doc.version) != version {
        return Err(Error::Stale);
    }
    let found = collect(&doc.text, at).map_err(Error::Invalid)?;
    let new = host::input("New macro parameter name", Some(&found.name))?.ok_or(Error::Cancelled)?;
    let new = new.trim().to_owned();
    collides(&doc.text, &found, &new).map_err(Error::Invalid)?;

    let mut after = doc.text.clone();
    for r in found.uses.iter().rev() {
        after.replace_range(r.clone(), &new);
    }
    let again = collect(&after, found.uses[0].start).map_err(Error::Invalid)?;
    if again.name != new || again.uses.len() != found.uses.len() || !checks::check(&after).is_empty() {
        return Err(Error::Invalid("The rename would change the macro's meaning or structure; refused.".into()));
    }

    let mut preview = format!("Macro parameter {} → {new}\n", found.name);
    let mut edits = Vec::new();
    for r in &found.uses {
        let line = position(&doc.text, r.start).line;
        let line_text = doc.text.lines().nth(line as usize).unwrap_or_default().trim();
        preview.push_str(&format!("{}: {}\n", line + 1, line_text.chars().take(100).collect::<String>()));
        edits.push(openidea_plugin::TextEdit { range: EditorRange::new(position(&doc.text, r.start), position(&doc.text, r.end)), text: new.clone() });
    }
    preview.push_str("\nCall sites that pass this parameter by name (name: value) are not updated.");
    if !host::confirm("Rename this macro parameter?", &preview)? {
        return Err(Error::Cancelled);
    }
    Ok(FileAction {
        title: "Rename macro parameter".into(),
        kind: Some("refactor.rename".into()),
        edits: vec![(doc.path.clone(), edits)],
        ..FileAction::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn renamed(text: &str, caret: &str, new: &str) -> Result<String, String> {
        let host = openidea_plugin::mock::MockHost::install();
        renamed_in(&host, text, caret, new)
    }

    fn renamed_in(_host: &openidea_plugin::mock::MockHost, text: &str, caret: &str, new: &str) -> Result<String, String> {
        let _scope = delimiters::Syntax::scope();
        let found = collect(text, text.find(caret).unwrap() + 1)?;
        collides(text, &found, new)?;
        let mut out = text.to_owned();
        for r in found.uses.iter().rev() {
            out.replace_range(r.clone(), new);
        }
        Ok(out)
    }

    #[test]
    fn renames_parameter_default_references_and_body_uses_but_not_attributes_filters_or_tests() {
        let text = "{% macro field(label, value = label) %}<i>label</i>{{ label|upper }}{{ x.label }}{{ y|label }}{{ z is label }}{{ label ~ value }}{% if label %}{% endif %}{% endmacro %}";
        let out = renamed(text, "label,", "title").unwrap();
        assert_eq!(
            out,
            "{% macro field(title, value = title) %}<i>label</i>{{ title|upper }}{{ x.label }}{{ y|label }}{{ z is label }}{{ title ~ value }}{% if title %}{% endif %}{% endmacro %}"
        );
    }

    #[test]
    fn leaves_other_macros_and_text_outside_the_definition_alone() {
        let text = "{{ a }}{% macro one(a) %}{{ a }}{% endmacro %}{% macro two(a) %}{{ a }}{% endmacro %}";
        assert_eq!(
            renamed(text, "a) %}{{ a }}{% endmacro %}{% macro two", "b").unwrap(),
            "{{ a }}{% macro one(b) %}{{ b }}{% endmacro %}{% macro two(a) %}{{ a }}{% endmacro %}"
        );
    }

    #[test]
    fn refuses_shadowing_strings_comments_and_ambiguous_syntax() {
        for body in [
            "{% set a = 1 %}{{ a }}",
            "{% set a %}x{% endset %}",
            "{% for a in items %}{{ a }}{% endfor %}",
            "{% import 'x.twig' as a %}",
            "{% from 'x.twig' import a %}",
            "{{ 'a' }}",
            "{{ \"v: #{a}\" }}",
            "{# a #}",
            "{{ c ? a : b }}",
            "{{ {a: 1} }}",
            "{{ f(a: 1) }}",
            "{{ items|map(x => a) }}",
            "{% embed 'x.twig' %}{% endembed %}",
            "{% with {k: 1} only %}{{ a }}{% endwith %}",
            "{% verbatim %}{{ a }}{% endverbatim %}",
            "{{ a",
        ] {
            let text = format!("{{% macro m(a, b) %}}{body}{{% endmacro %}}");
            assert!(renamed(&text, "a,", "z").is_err(), "{body}");
        }
    }

    #[test]
    fn allows_other_names_in_binding_tags_and_uses_in_loop_sources() {
        let text = "{% macro m(a) %}{% set n = a %}{% for x in a %}{{ x }}{{ n }}{% endfor %}{% endmacro %}";
        assert_eq!(
            renamed(text, "a)", "list").unwrap(),
            "{% macro m(list) %}{% set n = list %}{% for x in list %}{{ x }}{{ n }}{% endfor %}{% endmacro %}"
        );
    }

    #[test]
    fn refuses_collisions_reserved_names_and_non_parameters() {
        let text = "{% macro m(a, b) %}{% set n = 1 %}{{ n }}{% endmacro %}";
        for new in ["b", "n", "true", "1x", "a b", "a"] {
            assert!(renamed(text, "a,", new).is_err(), "{new}");
        }
        assert!(renamed(text, "n = 1", "z").is_err());
        assert!(renamed(text, "m(", "z").is_err());
        assert!(renamed("{% macro m(a) %}{% macro n(a) %}{% endmacro %}{% endmacro %}", "a)", "z").is_err());
        assert!(renamed("{% macro m(a) %}", "a)", "z").is_err());
        assert!(renamed("{% macro m(a, a) %}{% endmacro %}", "a,", "z").is_err());
    }

    #[test]
    fn honours_unicode_and_custom_delimiters() {
        let text = "é {% macro m(a) %}é{{ a }}{% endmacro %}😀";
        assert_eq!(renamed(text, "a)", "x").unwrap(), "é {% macro m(x) %}é{{ x }}{% endmacro %}😀");
        let host = openidea_plugin::mock::MockHost::install();
        for (key, value) in [("tagOpen", "<%"), ("tagClose", "%>"), ("variableOpen", "<<"), ("variableClose", ">>")] {
            host.set_setting(key, &format!("\"{value}\""));
        }
        let custom = "<% macro m(a) %><< a >>{{ a }}<% endmacro %>";
        assert_eq!(renamed_in(&host, custom, "a)", "b").unwrap(), "<% macro m(b) %><< b >>{{ a }}<% endmacro %>");
    }

    #[test]
    fn file_action_previews_edits_and_confirms_before_returning_them() {
        use openidea_plugin::{file_actions::FileActions, mock::MockHost};
        let text = "{% macro m(label) %}{{ label }}{% endmacro %}";
        let host = MockHost::install();
        host.add_file("a.twig", text);
        let doc = Document { path: "/project/a.twig".into(), language: "twig".into(), text: text.into(), version: 3 };
        let mut twig = Twig::default();
        let at = position(text, text.find("label").unwrap() + 1);
        let actions = twig.file_actions(&doc, EditorRange::new(at, at));
        assert_eq!(actions.len(), 1);
        assert!(actions[0].edits.is_empty());
        let outside = position(text, text.len());
        assert!(twig.file_actions(&doc, EditorRange::new(outside, outside)).is_empty());
        host.answer_next_prompt(Some(&["title"]));
        host.answer_next_prompt(Some(&["yes"]));
        let resolved = twig.resolve_file_action(&doc, actions[0].clone()).unwrap();
        assert_eq!(resolved.edits[0].1.len(), 2);
        host.answer_next_prompt(Some(&["title"]));
        let stale = Document { version: 4, ..doc };
        assert!(twig.resolve_file_action(&stale, actions[0].clone()).is_err());
    }
}
