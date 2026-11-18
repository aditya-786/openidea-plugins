//! Bounded closed-source MVC schema profiles, not a complete XML/XSD validator.
use openidea_plugin::{
    Location, Position,
    providers::{CompletionItem, CompletionKind, Document},
    text::offset,
};
use spring_text::{
    contains,
    finding::Finding,
    range,
    xml::{self, Element},
};
use std::collections::{BTreeMap, BTreeSet};
const MVC: &str = "http://www.springframework.org/schema/mvc";
const BEANS: &str = "http://www.springframework.org/schema/beans";
const XSI: &str = "http://www.w3.org/2001/XMLSchema-instance";
const XML: &str = "http://www.w3.org/XML/1998/namespace";
struct Rule {
    path: &'static str,
    profiles: u16,
    required: &'static [&'static str],
    booleans: &'static [&'static str],
    integers: &'static [&'static str],
    longs: &'static [&'static str],
    refs: &'static [&'static str],
}
#[path = "mvc_xml_tables.rs"]
mod tables;
fn uri<'a>(es: &'a [Element], mut i: usize, prefix: &str) -> Option<&'a str> {
    if prefix == "xml" {
        return Some(XML);
    }
    let key = if prefix.is_empty() {
        "xmlns".into()
    } else {
        format!("xmlns:{prefix}")
    };
    loop {
        if let Some(a) = es[i].attrs.iter().find(|a| a.name == key) {
            return Some(&a.value);
        }
        i = es[i].parent?;
    }
}
fn closed(text: &str, es: &[Element]) -> bool {
    text.len() <= 256 << 10 && !es.is_empty() && es.len() <= 2048 && !text.contains("<!DOCTYPE") && spring_text::xml::analysis_valid(text)
}
fn profile(es: &[Element]) -> Option<u16> {
    let mut selected = None;
    for (i, e) in es.iter().enumerate() {
        for a in &e.attrs {
            let Some((prefix, local)) = a.name.split_once(':') else {
                continue;
            };
            if local != "schemaLocation" || uri(es, i, prefix) != Some(XSI) {
                continue;
            }
            let pairs = a.value.split_whitespace().collect::<Vec<_>>();
            if pairs.len() % 2 != 0 {
                return None;
            }
            for pair in pairs.chunks_exact(2).filter(|p| p[0] == MVC) {
                let file = pair[1].rsplit('/').next()?;
                let bit = match file {
                    "spring-mvc-3.0.xsd" => 0,
                    "spring-mvc-3.1.xsd" => 1,
                    "spring-mvc-3.2.xsd" => 2,
                    "spring-mvc-4.0.xsd" => 3,
                    "spring-mvc-4.1.xsd" => 4,
                    "spring-mvc-4.2.xsd" => 5,
                    "spring-mvc-4.3.xsd" => 6,
                    "spring-mvc-5.3.xsd" => 7,
                    "spring-mvc-6.2.xsd" => 8,
                    "spring-mvc.xsd" | "spring-mvc-7.0.xsd" | "spring-mvc-7.1.xsd" => 9,
                    _ => return None,
                };
                let value = 1 << bit;
                if selected.is_some_and(|old| old != value) {
                    return None;
                }
                selected = Some(value);
            }
        }
    }
    Some(selected.unwrap_or(1 << 9))
}
fn path(es: &[Element], mut i: usize) -> Option<String> {
    let mut names = vec![];
    loop {
        if xml::namespace(es, i) != Some(MVC) {
            return None;
        }
        names.push(es[i].local());
        if names.len() > 16 {
            return None;
        }
        match es[i].parent {
            None => break,
            Some(parent) if xml::namespace(es, parent) == Some(MVC) => i = parent,
            Some(parent)
                if es[parent].local() == "beans"
                    && matches!(xml::namespace(es, parent), Some(BEANS) | None) =>
            {
                break;
            }
            _ => return None,
        }
    }
    names.reverse();
    Some(names.join("/"))
}
fn model(text: &str) -> Option<(Vec<Element>, u16)> {
    let es = xml::elements(text);
    if !closed(text, &es) {
        return None;
    }
    let profile = profile(&es)?;
    Some((es, profile))
}
fn dynamic(s: &str) -> bool {
    s.contains("${") || s.contains("#{") || s.contains('\\')
}
pub(super) fn check(text: &str) -> Vec<Finding> {
    let Some((es, profile)) = model(text) else {
        return vec![];
    };
    let mut out = vec![];
    for (i, e) in es.iter().enumerate() {
        let Some(path) = path(&es, i) else {
            continue;
        };
        for rule in tables::RULES
            .iter()
            .filter(|r| r.path == path && r.profiles & profile != 0)
        {
            for required in rule.required {
                if !e.attrs.iter().any(|a| a.name == *required) {
                    out.push(Finding::new(
                        e.at,
                        e.at + e.name.len(),
                        "missing-attribute",
                        format!("<{}> needs the attribute \"{}\".", e.name, required),
                    ));
                }
            }
            for a in &e.attrs {
                if dynamic(&a.value) {
                    continue;
                }
                let value = a.value.trim();
                if rule.booleans.contains(&a.name.as_str())
                    && !matches!(value, "true" | "false" | "1" | "0")
                {
                    out.push(Finding::new(
                        a.value_at,
                        a.value_end,
                        "invalid-attribute-value",
                        format!(
                            "\"{}\" is not an XML Schema boolean; use true, false, 1 or 0.",
                            a.value
                        ),
                    ));
                }
                if rule.integers.contains(&a.name.as_str()) && value.parse::<i32>().is_err()
                    || rule.longs.contains(&a.name.as_str()) && value.parse::<i64>().is_err()
                {
                    out.push(Finding::new(
                        a.value_at,
                        a.value_end,
                        "invalid-attribute-value",
                        format!(
                            "\"{}\" is not an XML Schema integer in the declared range.",
                            a.value
                        ),
                    ));
                }
            }
        }
        if out.len() > 256 {
            out.truncate(256);
            break;
        }
    }
    out
}
fn plain(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
}
/// Bean-reference attribute values as (value, start, end) bytes.
fn ref_sites(es: &[Element], profile: u16) -> Vec<(String, usize, usize)> {
    let mut refs = vec![];
    for (i, e) in es.iter().enumerate() {
        if let Some(path) = path(es, i) {
            for rule in tables::RULES
                .iter()
                .filter(|r| r.path == path && r.profiles & profile != 0)
            {
                for a in e
                    .attrs
                    .iter()
                    .filter(|a| rule.refs.contains(&a.name.as_str()))
                {
                    if a.value.is_empty() || plain(&a.value) {
                        refs.push((a.value.clone(), a.value_at, a.value_end));
                    }
                }
            }
        }
        if e.local() == "ref"
            && xml::namespace(es, i) == Some(BEANS)
            && e.parent.and_then(|p| path(es, p)).is_some_and(|path| {
                tables::REF_PARENTS
                    .iter()
                    .any(|(p, mask)| *p == path && mask & profile != 0)
            })
            && e.attrs.len() == 1
        {
            if let Some(a) = e.attrs.iter().find(|a| a.name == "bean") {
                if a.value.is_empty() || plain(&a.value) {
                    refs.push((a.value.clone(), a.value_at, a.value_end));
                }
            }
        }
    }
    refs
}
#[path = "mvc_xml_context.rs"]
mod context;
/// Findings needing file access: `read` returns a file's text, `find` the files matching a glob (None when the host cannot say),
/// `imported_elsewhere` whether another file may import this one (true when unknown).
pub(super) fn context_findings(
    path: &str,
    text: &str,
    read: &dyn Fn(&str) -> Option<String>,
    find: &dyn Fn(&str) -> Option<Vec<String>>,
    imported_elsewhere: &dyn Fn() -> bool,
) -> Vec<Finding> {
    let mut out = context::check(path, text, read, imported_elsewhere);
    if path.contains("/src/main/") {
        for (location, start, end) in context::resource_locations(text) {
            let Some(globs) = context::location_globs(&location) else {
                continue;
            };
            let found: Option<Vec<bool>> = globs.iter().map(|g| find(g).map(|f| !f.is_empty())).collect();
            if found.is_some_and(|f| !f.contains(&true)) {
                out.push(
                    Finding::new(start, end, "mvc-xml-missing-resource-location", format!("Resource location \"{location}\" was not found under src/main/resources or src/main/webapp."))
                        .with_severity(openidea_plugin::Severity::Info),
                );
            }
        }
    }
    out
}
fn refs(
    es: &[Element],
    profile: u16,
    text: &str,
) -> Option<(
    BTreeMap<String, openidea_plugin::Range>,
    Vec<(String, openidea_plugin::Range)>,
)> {
    if es[0].local() != "beans"
        || xml::namespace(es, 0) != Some(BEANS)
        || es[0].attrs.iter().any(|a| a.name == "profile")
    {
        return None;
    }
    if es.iter().enumerate().skip(1).any(|(i, e)| {
        xml::namespace(es, i) == Some(BEANS) && matches!(e.local(), "import" | "alias" | "beans")
    }) {
        return None;
    }
    let mut beans = BTreeMap::new();
    let mut duplicates = BTreeSet::new();
    for (i, e) in es.iter().enumerate() {
        if e.parent == Some(0) && xml::namespace(es, i) == Some(BEANS) && e.local() == "bean" {
            if e.attrs.iter().any(|a| a.name == "name") {
                return None;
            }
            if let Some(id) = e.attrs.iter().find(|a| a.name == "id") {
                if !plain(&id.value) {
                    return None;
                }
                if beans
                    .insert(id.value.clone(), range(text, id.value_at, id.value_end))
                    .is_some()
                {
                    duplicates.insert(id.value.clone());
                }
            }
        }
    }
    let refs = ref_sites(es, profile)
        .into_iter()
        .map(|(v, s, e)| (v, range(text, s, e)))
        .collect();
    for duplicate in duplicates {
        beans.remove(&duplicate);
    }
    Some((beans, refs))
}
pub(super) fn view_names(text: &str) -> Vec<spring_text::java::Str> {
    let Some((es, _)) = model(text) else {
        return vec![];
    };
    (0..es.len())
        .filter(|i| path(&es, *i).as_deref() == Some("view-controller"))
        .filter_map(|i| es[i].attrs.iter().find(|a| a.name == "view-name"))
        .filter(|a| !a.value.trim().is_empty() && !dynamic(&a.value) && !a.value.starts_with("redirect:") && !a.value.starts_with("forward:"))
        .map(|a| spring_text::java::Str { value: a.value.clone(), at: a.value_at, end: a.value_end })
        .collect()
}
pub(super) fn definition(doc: &Document, at: Position) -> Vec<Location> {
    let Some((es, profile)) = model(&doc.text) else {
        return vec![];
    };
    let Some((beans, refs)) = refs(&es, profile, &doc.text) else {
        return vec![];
    };
    let Some((name, _)) = refs.iter().find(|(_, r)| contains(*r, at)) else {
        return vec![];
    };
    beans
        .get(name)
        .map(|range| {
            vec![Location {
                path: doc.path.clone(),
                range: *range,
            }]
        })
        .unwrap_or_default()
}
pub(super) fn completion(doc: &Document, at: Position) -> Vec<CompletionItem> {
    let Some((es, profile)) = model(&doc.text) else {
        return vec![];
    };
    for (i, e) in es.iter().enumerate() {
        if let Some(path) = path(&es, i) {
            for rule in tables::RULES
                .iter()
                .filter(|r| r.path == path && r.profiles & profile != 0)
            {
                for a in e
                    .attrs
                    .iter()
                    .filter(|a| rule.booleans.contains(&a.name.as_str()))
                {
                    let span = range(&doc.text, a.value_at, a.value_end);
                    if span.start <= at && at <= span.end && !dynamic(&a.value) {
                        let Some(end) = offset(&doc.text, at) else {
                            return vec![];
                        };
                        let prefix = &doc.text[a.value_at..end];
                        return ["true", "false", "1", "0"]
                            .into_iter()
                            .filter(|v| v.starts_with(prefix))
                            .map(|v| CompletionItem {
                                replace: Some(span),
                                detail: Some("MVC XML Schema boolean".into()),
                                ..CompletionItem::new(v, CompletionKind::Value)
                            })
                            .collect();
                    }
                }
            }
        }
    }
    let Some((beans, refs)) = refs(&es, profile, &doc.text) else {
        return vec![];
    };
    let Some((_, span)) = refs.iter().find(|(_, r)| r.start <= at && at <= r.end) else {
        return vec![];
    };
    let Some(start) = offset(&doc.text, span.start) else {
        return vec![];
    };
    let Some(end) = offset(&doc.text, at) else {
        return vec![];
    };
    let prefix = &doc.text[start..end];
    beans
        .into_keys()
        .filter(|name| name.starts_with(prefix))
        .take(64)
        .map(|name| CompletionItem {
            replace: Some(*span),
            detail: Some(
                "Declared bean ID in this XML document; effective type/context not resolved".into(),
            ),
            ..CompletionItem::new(&name, CompletionKind::Reference)
        })
        .collect()
}
