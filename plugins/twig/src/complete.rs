use openidea_plugin::providers::{CompletionItem, CompletionKind};
use openidea_plugin::Range;
use openidea_plugin::text::position;

enum Slot {
    Tag,
    Filter,
    Function,
    Test,
    Html,
}

const TAGS: &[&str] = &[
    "apply", "autoescape", "block", "cache", "deprecated", "do", "else", "elseif", "embed", "extends", "flush", "for", "from", "if", "import",
    "include", "macro", "sandbox", "set", "types", "use", "verbatim", "with", "endapply", "endautoescape", "endblock", "endcache", "endembed",
    "endfor", "endif", "endmacro", "endsandbox", "endset", "endverbatim", "endwith",
];

const FILTERS: &[&str] = &[
    "abs", "batch", "capitalize", "column", "convert_encoding", "country_name", "currency_name", "currency_symbol", "data_uri", "date",
    "date_modify", "default", "e", "escape", "filter", "find", "first", "format", "format_currency", "format_date", "format_datetime",
    "format_number", "format_time", "inline_css", "join", "json_encode", "keys", "language_name", "last", "length", "locale_name", "lower", "map",
    "markdown_to_html", "merge", "nl2br", "number_format", "raw", "reduce", "replace", "reverse", "round", "slice", "slug", "sort", "spaceless",
    "split", "striptags", "sum", "timezone_name", "title", "trim", "u", "upper", "url_encode",
];

const FUNCTIONS: &[&str] = &[
    "attribute", "block", "constant", "cycle", "date", "dump", "enum", "enum_cases", "html_classes", "include", "max", "min", "parent", "random",
    "range", "source", "template_from_string",
];

const TESTS: &[&str] = &["constant", "defined", "divisible by", "empty", "even", "iterable", "mapping", "null", "odd", "same as"];

const SNIPPETS: &[(&str, &str)] = &[
    ("blk", "{% block ${1:name} %}$0{% endblock %}"),
    ("mcr", "{% macro ${1:name}(${2:arguments}) %}$0{% endmacro %}"),
    ("esc", "{% autoescape '${1:html}' %}$0{% endautoescape %}"),
    ("emb", "{% embed '${1:template}' %}$0{% endembed %}"),
    ("ext", "{% extends '${1:base.html.twig}' %}"),
    ("for", "{% for ${1:item} in ${2:items} %}$0{% endfor %}"),
    ("from", "{% from '${1:template}' import ${2:name} %}"),
    ("if", "{% if ${1:condition} %}$0{% endif %}"),
    ("imp", "{% import '${1:template}' as ${2:alias} %}"),
    ("inc", "{% include '${1:template}' %}"),
    ("sbx", "{% sandbox %}{% include '${1:template}' %}{% endsandbox %}"),
    ("spl", "{% apply spaceless %}$0{% endapply %}"),
    ("verb", "{% verbatim %}$0{% endverbatim %}"),
];

fn is_identifier(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn slot_at(text: &str, start: usize) -> Option<Slot> {
    let syntax=crate::delimiters::Syntax::current();
    let before = &text[..start];
    let opener = [syntax.values[2].as_str(),syntax.values[0].as_str(),syntax.values[4].as_str()].iter().filter_map(|o| before.rfind(o).map(|at| (at, *o))).max_by_key(|(at, _)| *at);
    let closer = [syntax.values[3].as_str(),syntax.values[1].as_str(),syntax.values[5].as_str()].iter().filter_map(|c| before.rfind(c)).max();

    let Some((open_at, opener)) = opener.filter(|(at, _)| closer.is_none_or(|c| c < *at)) else {
        return Some(Slot::Html);
    };
    if opener == syntax.values[4] {
        return None;
    }

    let inside = before[open_at + opener.len()..].trim_start_matches(['-', '~']);
    if inside.bytes().filter(|b| matches!(b, b'\'' | b'"')).count() % 2 == 1 {
        return None;
    }

    let inside = inside.trim_end();
    if opener == syntax.values[0] && inside.is_empty() {
        return Some(Slot::Tag);
    }

    let last_word = inside.rsplit(char::is_whitespace).next().unwrap_or_default();
    let is_test = last_word == "is" || (last_word == "not" && inside.trim_end_matches("not").trim_end().ends_with(" is"));
    match inside.bytes().last() {
        Some(b'|') => Some(Slot::Filter),
        Some(b'.') => None,
        _ if is_test => Some(Slot::Test),
        _ => Some(Slot::Function),
    }
}

/// Whether the caret sits where a plain variable can be typed (not a tag, filter, test or member).
pub fn variable_slot(text: &str, offset: usize) -> bool {
    let start = (0..offset).rev().take_while(|&i| is_identifier(text.as_bytes()[i])).last().unwrap_or(offset);
    matches!(slot_at(text, start), Some(Slot::Function))
}

#[derive(Default)]
pub struct Extra<'a> {
    pub filters: Vec<&'a str>,
    pub functions: Vec<&'a str>,
    pub tests: Vec<&'a str>,
    pub variables: Vec<String>,
}

#[cfg(test)]
pub fn items(text: &str, offset: usize, with_names: bool) -> Vec<CompletionItem> {
    items_with(text, offset, with_names, &Extra::default())
}

pub fn items_with(text: &str, offset: usize, with_names: bool, extra: &Extra) -> Vec<CompletionItem> {
    let start = (0..offset).rev().take_while(|&i| is_identifier(text.as_bytes()[i])).last().unwrap_or(offset);
    let end = (offset..text.len()).take_while(|&i| is_identifier(text.as_bytes()[i])).last().map_or(offset, |i| i + 1);
    if text.as_bytes().get(start).is_some_and(u8::is_ascii_digit) {
        return Vec::new();
    }

    let Some(slot) = slot_at(text, start) else { return Vec::new() };
    let replace = Range::new(position(text, start), position(text, end));

    let variables = || -> Vec<CompletionItem> {
        let mut seen: Vec<&String> = extra.variables.iter().collect();
        seen.sort_unstable();
        seen.dedup();
        seen.into_iter()
            .map(|name| {
                let mut item = CompletionItem::new(name, CompletionKind::Variable);
                item.replace = Some(replace);
                item
            })
            .collect()
    };
    if matches!(slot, Slot::Function) && !with_names {
        return variables();
    }

    let (names, kind, project): (&[&str], _, &[&str]) = match slot {
        Slot::Html => return snippets(replace, start == end),
        _ if !with_names => return Vec::new(),
        Slot::Tag => (TAGS, CompletionKind::Keyword, &[]),
        Slot::Filter => (FILTERS, CompletionKind::Function, &extra.filters),
        Slot::Function => (FUNCTIONS, CompletionKind::Function, &extra.functions),
        Slot::Test => (TESTS, CompletionKind::Keyword, &extra.tests),
    };

    let mut items: Vec<CompletionItem> = names
        .iter()
        .chain(project.iter().filter(|name| !names.contains(name)))
        .map(|name| {
            let mut item = CompletionItem::new(name, kind);
            item.replace = Some(replace);
            item
        })
        .collect();
    if matches!(slot, Slot::Function) {
        items.extend(variables());
    }
    items
}

fn snippets(replace: Range, empty_word: bool) -> Vec<CompletionItem> {
    if empty_word {
        return Vec::new();
    }

    SNIPPETS
        .iter()
        .map(|(label, body)| {
            let mut item = CompletionItem::new(label, CompletionKind::Snippet);
            item.snippet = true;
            item.insert_text = crate::delimiters::Syntax::current().snippet(body);
            item.replace = Some(replace);
            item
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(marked: &str, with_names: bool) -> Vec<String> {
        let offset = marked.find('^').unwrap();
        let text = marked.replacen('^', "", 1);
        items(&text, offset, with_names).into_iter().map(|i| i.label).collect()
    }

    #[test]
    fn offers_all_thirteen_snippets_in_html_with_a_typed_prefix() {
        let found = labels("<div>b^", true);
        assert_eq!(found.len(), 13);
        assert!(["blk", "mcr", "esc", "emb", "ext", "for", "from", "if", "imp", "inc", "sbx", "spl", "verb"].iter().all(|s| found.contains(&(*s).to_owned())));
        assert!(labels("<div>^", true).is_empty());
    }

    #[test]
    fn snippet_inserts_a_tab_stop_body() {
        let text = "<p>blk";
        let blk = items(text, text.len(), true).into_iter().find(|i| i.label == "blk").unwrap();
        assert!(blk.snippet);
        assert_eq!(blk.insert_text, "{% block ${1:name} %}$0{% endblock %}");
        assert_eq!(blk.replace.unwrap().start.character, 3);
    }

    #[test]
    fn completes_names_by_slot() {
        assert!(labels("{% i^", true).contains(&"endif".to_owned()));
        assert!(labels("{{ name|u^", true).contains(&"upper".to_owned()));
        assert!(labels("{{ ma^", true).contains(&"max".to_owned()));
        assert!(labels("{% if x is not e^", true).contains(&"even".to_owned()));
        assert!(!labels("{{ name|u^", true).contains(&"if".to_owned()));
    }

    #[test]
    fn names_are_left_to_symfony_when_switched_off_and_silent_in_strings_and_comments() {
        assert!(labels("{% i^", false).is_empty());
        assert!(labels("{{ 'a^", true).is_empty());
        assert!(labels("{# no^", true).is_empty());
        assert!(labels("{{ user.na^", true).is_empty());
        assert!(labels("{{ x }}<p>{{ a^", true).contains(&"attribute".to_owned()));
    }
}
