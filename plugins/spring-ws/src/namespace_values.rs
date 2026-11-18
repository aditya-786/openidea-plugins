//! Explicit XPath adapter namespace URI properties; no XPath/effective context evaluation.
use crate::*;
use openidea_plugin::text::offset;
use std::collections::BTreeMap;
const BEANS: &str = "http://www.springframework.org/schema/beans";
const ADAPTER: &str =
    "org.springframework.ws.server.endpoint.adapter.XPathParamAnnotationMethodEndpointAdapter";
fn plain_uri(s: &str) -> bool {
    s.len() <= 1024
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b":/_.-#?=%+~".contains(&b))
}
fn whitespace_children(text: &str, parent: &xml::Element, children: &[&xml::Element]) -> bool {
    let mut at = parent.tag_end;
    for child in children {
        if text
            .get(at..child.start)
            .is_none_or(|s| !s.trim().is_empty())
        {
            return false;
        }
        at = child.end;
    }
    let Some(tail) = text.get(at..parent.end) else {
        return false;
    };
    let Some(close) = tail.find('<') else {
        return false;
    };
    tail[..close].trim().is_empty() && tail[close..] == format!("</{}>", parent.name)
}
fn xml_sites(text: &str) -> Vec<(String, Range)> {
    if text.len() > 32768 || !generate::xml_valid(text) {
        return vec![];
    }
    let es = xml::elements(text);
    if es.len() > 512 {
        return vec![];
    }
    let Some(root) = es.first() else {
        return vec![];
    };
    if root.local() != "beans"
        || xml_namespace(&es, 0) != Some(BEANS)
        || root.attrs.iter().any(|a| a.name == "profile")
    {
        return vec![];
    }
    let mut depth = Vec::new();
    for (i, e) in es.iter().enumerate() {
        let d = e.parent.map_or(0, |p| depth[p] + 1);
        if d > 64 {
            return vec![];
        }
        depth.push(d);
        if i > 0
            && xml_namespace(&es, i) == Some(BEANS)
            && matches!(e.local(), "beans" | "import" | "alias")
        {
            return vec![];
        }
    }
    let mut result = vec![];
    for (bi, b) in es.iter().enumerate().filter(|(_, e)| e.parent == Some(0)) {
        if b.local() != "bean"
            || xml_namespace(&es, bi) != Some(BEANS)
            || b.attrs.iter().any(|a| {
                !matches!(a.name.as_str(), "id" | "class" | "xmlns")
                    && !a.name.starts_with("xmlns:")
            })
            || b.attrs
                .iter()
                .find(|a| a.name == "class")
                .map(|a| a.value.as_str())
                != Some(ADAPTER)
        {
            continue;
        }
        let ps: Vec<_> = es
            .iter()
            .enumerate()
            .filter(|(i, e)| {
                e.parent == Some(bi)
                    && e.local() == "property"
                    && xml_namespace(&es, *i) == Some(BEANS)
                    && e.attrs
                        .iter()
                        .any(|a| a.name == "name" && a.value == "namespaces")
            })
            .collect();
        let [(pi, p)] = ps.as_slice() else {
            continue;
        };
        if p.attrs.len() != 1 {
            continue;
        }
        let children: Vec<_> = es
            .iter()
            .enumerate()
            .filter(|(_, e)| e.parent == Some(*pi))
            .collect();
        let [(props_i, props)] = children.as_slice() else {
            continue;
        };
        if props.local() != "props"
            || xml_namespace(&es, *props_i) != Some(BEANS)
            || !props.attrs.is_empty()
            || !whitespace_children(text, p, &[*props])
        {
            continue;
        }
        let children: Vec<_> = es.iter().filter(|e| e.parent == Some(*props_i)).collect();
        if !whitespace_children(text, props, &children) {
            continue;
        }
        let mut prefixes = BTreeSet::new();
        let mut values = vec![];
        let mut valid = true;
        for (i, e) in es
            .iter()
            .enumerate()
            .filter(|(_, e)| e.parent == Some(*props_i))
        {
            let Some(key) = e.attrs.iter().find(|a| a.name == "key") else {
                valid = false;
                break;
            };
            if e.local() != "prop"
                || xml_namespace(&es, i) != Some(BEANS)
                || e.attrs.len() != 1
                || !schema_qnames::local(&key.value)
                || matches!(key.value.as_str(), "xml" | "xmlns")
                || !prefixes.insert(key.value.clone())
                || es.iter().any(|c| c.parent == Some(i))
            {
                valid = false;
                break;
            }
            let Some(tail) = text.get(e.tag_end..e.end) else {
                valid = false;
                break;
            };
            let Some(end) = tail.find('<') else {
                valid = false;
                break;
            };
            if &tail[end..] != format!("</{}>", e.name) || !plain_uri(&tail[..end]) {
                valid = false;
                break;
            }
            values.push((
                tail[..end].to_owned(),
                range(text, e.tag_end, e.tag_end + end),
            ));
        }
        if valid && values.len() <= 128 {
            result.extend(values);
        }
    }
    result
}
fn sites(doc:&Document)->Vec<(String,Range)> {
    if doc.language=="xml" {xml_sites(&doc.text)} else {xpath::namespace_sites(doc)}
}
fn inventory(nav: &mut WsTools, doc: &Document) -> Option<BTreeMap<String, BTreeSet<String>>> {
    if !nav.watching {
        return None;
    }
    let root = project_of(&doc.path)?.root().ok()?;
    if root.is_empty()
        || !doc
            .path
            .starts_with(&format!("{}/", root.trim_end_matches('/')))
        || doc.path.contains('\\')
        || doc.path.split('/').any(|p| matches!(p, "." | ".."))
    {
        return None;
    }
    let cache = nav.schemas.for_cached_document(&doc.path, &doc.text)?;
    if !complete_schemas(cache) || cache.iter().any(|(_, f)| !f.qnames_complete) {
        return None;
    }
    let mut result: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (path, file) in cache.iter() {
        for schema in &file.schemas {
            if !schema.namespace.is_empty() && plain_uri(&schema.namespace) {
                result
                    .entry(schema.namespace.clone())
                    .or_default()
                    .insert(path.to_owned());
            }
        }
    }
    Some(result)
}
pub(crate) fn definition(nav: &mut WsTools, doc: &Document, at: Position) -> Option<Vec<Location>> {
    let (uri, _) = sites(doc)
        .into_iter()
        .find(|(_, r)| contains(*r, at))?;
    let Some(map) = inventory(nav, doc) else {
        return Some(vec![]);
    };
    let Some(files) = map.get(&uri) else {
        return Some(vec![]);
    };
    if files.len() != 1 {
        return Some(vec![]);
    }
    Some(vec![Location {
        path: files.iter().next().unwrap().clone(),
        range: Range::default(),
    }])
}
pub(crate) fn completion(
    nav: &mut WsTools,
    doc: &Document,
    at: Position,
) -> Option<Vec<CompletionItem>> {
    let (value, r) = sites(doc)
        .into_iter()
        .find(|(_, r)| r.start <= at && at <= r.end)?;
    let start = offset(&doc.text, r.start)?;
    let caret = offset(&doc.text, at)?;
    let Some(prefix) = value.get(..caret.checked_sub(start)?) else {
        return Some(vec![]);
    };
    let Some(map) = inventory(nav, doc) else {
        return Some(vec![]);
    };
    Some(
        map.into_iter()
            .filter(|(uri, files)| files.len() == 1 && uri.starts_with(prefix))
            .take(64)
            .map(|(uri, files)| CompletionItem {
                detail: Some(format!(
                    "Cached schema namespace — {}",
                    file_name(files.iter().next().unwrap())
                )),
                replace: Some(r),
                ..CompletionItem::new(&uri, CompletionKind::Value)
            })
            .collect(),
    )
}
