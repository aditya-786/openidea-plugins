//! Written OXM/CXF class roles resolved only to fresh, owned project source types.
use crate::*;
use std::collections::BTreeMap;
const BEANS: &str = "http://www.springframework.org/schema/beans";
const OXM: &str = "http://www.springframework.org/schema/oxm";
const CXF: &str = "http://cxf.apache.org/jaxws";
#[derive(Clone)]
pub(crate) struct SourceType {
    name: String,
    range: Range,
    class: bool,
    package: String,
    package_range: Range,
}
pub(crate) fn scan_source(path: &str, text: &str) -> Option<Vec<SourceType>> {
    if text.len() > MAX_DOCUMENT_BYTES || !(path.ends_with(".java") || path.ends_with(".kt")) {
        return None;
    }
    let file = parse_source(path, text);
    let tokens = java::tokens(text);
    let package_range = tokens
        .iter()
        .position(|t| t.s == "package")
        .and_then(|at| {
            let mut name = String::new();
            let mut end = at + 1;
            while end < tokens.len() && (tokens[end].is_ident() || tokens[end].s == ".") {
                name.push_str(tokens[end].s);
                end += 1;
                if name == file.package {
                    return Some(range(text, tokens[at + 1].at, tokens[end - 1].end()));
                }
            }
            None
        })
        .unwrap_or_default();
    let types = file
        .types
        .iter()
        .enumerate()
        .filter_map(|(i, t)| {
            if t.outer.is_some()
                || !matches!(t.kind.as_str(), "class" | "interface" | "enum" | "record")
            {
                return None;
            }
            Some(SourceType {
                name: file.qualified(i),
                range: range(text, t.at, t.at + t.name.len()),
                class: t.kind != "interface",
                package: file.package.clone(),
                package_range,
            })
        })
        .collect::<Vec<_>>();
    (!types.is_empty()).then_some(types)
}
struct Site {
    value: String,
    range: Range,
    class_only: bool,
    package: bool,
}
fn sites(doc: &Document) -> Vec<Site> {
    if doc.language != "xml"
        || doc.text.len() > MAX_DOCUMENT_BYTES
        || !generate::xml_valid(&doc.text)
    {
        return vec![];
    }
    let es = xml::elements(&doc.text);
    if es.len() > 512 {
        return vec![];
    }
    let top = |i: usize| {
        es[i].parent.is_none()
            || es[i].parent.is_some_and(|p| {
                p == 0 && es[p].local() == "beans" && xml_namespace(&es, p) == Some(BEANS)
            })
    };
    let mut out = vec![];
    for (i, e) in es.iter().enumerate() {
        let ns = xml_namespace(&es, i);
        if ns == Some(OXM) && e.local() == "jaxb2-marshaller" && top(i) {
            if let Some(a) = e.attrs.iter().find(|a| a.name == "context-path") {
                let mut at = a.value_at;
                for component in a.value.split(':') {
                    if component.len() <= 256
                        && component
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"._".contains(&b))
                    {
                        out.push(Site {
                            value: component.into(),
                            range: range(&doc.text, at, at + component.len()),
                            class_only: false,
                            package: true,
                        });
                    }
                    at += component.len() + 1;
                }
            }
        }
        let role = if ns == Some(OXM)
            && e.local() == "class-to-be-bound"
            && e.parent.is_some_and(|p| {
                es[p].local() == "jaxb2-marshaller" && xml_namespace(&es, p) == Some(OXM) && top(p)
            }) {
            Some((&["name"][..], true))
        } else if ns == Some(CXF) && top(i) {
            match e.local() {
                "endpoint" => Some((&["implementorClass", "implementor"][..], true)),
                "server" => Some((&["serviceClass", "serviceBean"][..], false)),
                "client" => Some((&["serviceClass"][..], false)),
                _ => None,
            }
        } else {
            None
        };
        let Some((names, class_only)) = role else {
            continue;
        };
        for a in e.attrs.iter().filter(|a| names.contains(&a.name.as_str())) {
            if a.value.len() > 256
                || a.value.contains(['&', '$', '#', '\\'])
                || !a
                    .value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._".contains(&b))
            {
                continue;
            }
            out.push(Site {
                value: a.value.clone(),
                range: range(&doc.text, a.value_at, a.value_end),
                class_only,
                package: false,
            });
        }
    }
    out
}
fn inventory(
    nav: &mut WsTools,
    doc: &Document,
) -> Option<BTreeMap<String, Vec<(String, SourceType)>>> {
    if !nav.watching
        || doc.path.contains('\\')
        || doc.path.split('/').any(|p| matches!(p, "." | ".."))
    {
        return None;
    }
    let index = nav.source.for_cached_document_warm(&doc.path, &doc.text, PROVIDER_BUDGET)?;
    if !index.fresh() {
        return None;
    }
    let mut out: BTreeMap<String, Vec<(String, SourceType)>> = BTreeMap::new();
    for (path, facts) in index.iter() {
        for ty in &facts.types {
            out.entry(ty.name.clone())
                .or_default()
                .push((path.into(), ty.clone()));
        }
    }
    Some(out)
}
pub(crate) fn definition(nav: &mut WsTools, doc: &Document, at: Position) -> Option<Vec<Location>> {
    let site = sites(doc).into_iter().find(|s| contains(s.range, at))?;
    let Some(inventory) = inventory(nav, doc) else {
        return Some(vec![]);
    };
    if site.package {
        let mut locations = BTreeMap::new();
        for values in inventory.values() {
            for (path, ty) in values {
                if ty.package == site.value && !ty.package.is_empty() {
                    locations.insert(path.clone(), ty.package_range);
                }
            }
        }
        return Some(
            locations
                .into_iter()
                .take(64)
                .map(|(path, range)| Location { path, range })
                .collect(),
        );
    }
    let Some(values) = inventory.get(&site.value) else {
        return Some(vec![]);
    };
    let [(path, ty)] = values.as_slice() else {
        return Some(vec![]);
    };
    Some(if site.class_only && !ty.class {
        vec![]
    } else {
        vec![Location {
            path: path.clone(),
            range: ty.range,
        }]
    })
}
pub(crate) fn completion(
    nav: &mut WsTools,
    doc: &Document,
    at: Position,
) -> Option<Vec<CompletionItem>> {
    let site = sites(doc)
        .into_iter()
        .find(|s| s.range.start <= at && at <= s.range.end)?;
    let start = openidea_plugin::text::offset(&doc.text, site.range.start)?;
    let caret = openidea_plugin::text::offset(&doc.text, at)?;
    let prefix = doc.text.get(start..caret)?;
    let Some(inventory) = inventory(nav, doc) else {
        return Some(vec![]);
    };
    if site.package {
        let packages = inventory
            .values()
            .flat_map(|values| values.iter().map(|(_, ty)| ty.package.clone()))
            .filter(|p| !p.is_empty() && p.starts_with(prefix))
            .collect::<BTreeSet<_>>();
        return Some(
            packages
                .into_iter()
                .take(64)
                .map(|p| CompletionItem {
                    replace: Some(site.range),
                    detail: Some(
                        "Declared project source package; JAXB classpath discovery not resolved"
                            .into(),
                    ),
                    ..CompletionItem::new(&p, CompletionKind::Module)
                })
                .collect(),
        );
    }
    Some(inventory.into_iter().filter(|(name,types)|name.starts_with(prefix)&&types.len()==1&&(!site.class_only||types[0].1.class)).take(64).map(|(name,_)|CompletionItem{replace:Some(site.range),detail:Some("Declared project source type; effective classpath/runtime binding not resolved".into()),..CompletionItem::new(&name,CompletionKind::Class)}).collect())
}
#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{
        mock::MockHost,
        text::{offset, position},
    };
    fn document(body: &str) -> Document {
        Document {
            path: "/project/context.xml".into(),
            text: format!(
                "<b:beans xmlns:b='{BEANS}' xmlns:o='{OXM}' xmlns:c='{CXF}'>{body}</b:beans>"
            ),
            language: "xml".into(),
            version: 1,
        }
    }
    fn setup() -> (MockHost, WsTools) {
        let h = MockHost::install();
        h.add_file(
            "src/main/java/Model.java",
            "package com.acme; public class Model {}",
        );
        h.add_file(
            "src/main/java/Service.java",
            "package com.acme; public interface Service {}",
        );
        let mut nav = WsTools::default();
        nav.activate(&h.context("aritti.spring-ws")).unwrap();
        (h, nav)
    }
    #[test]
    fn oxm_and_cxf_written_class_roles_navigate_owned_source_and_filter_class_only_candidates() {
        let (h, mut nav) = setup();
        for body in [
            "<o:jaxb2-marshaller><o:class-to-be-bound name='com.acme.Model'/></o:jaxb2-marshaller>",
            "<c:endpoint implementorClass='com.acme.Model'/>",
            "<c:endpoint implementor='com.acme.Model'/>",
            "<c:server serviceClass='com.acme.Service'/>",
            "<c:client serviceClass='com.acme.Service'/>",
        ] {
            let d = document(body);
            let at = position(&d.text, d.text.find("com.acme.").unwrap());
            let targets = nav.definition(&d, at);
            assert_eq!(targets.len(), 1, "{body}");
            let values = nav.completion(&d, at, None);
            assert_eq!(
                values.len(),
                if body.contains("class-to-be-bound") || body.contains("endpoint") {
                    1
                } else {
                    2
                }
            );
            for value in values {
                let r = value.replace.unwrap();
                assert!(
                    d.text[offset(&d.text, r.start).unwrap()..offset(&d.text, r.end).unwrap()]
                        .starts_with("com.acme.")
                );
            }
        }
        assert!(h.requests().is_empty() && h.process_runs().is_empty());
    }
    #[test]
    fn cold_ambiguous_foreign_and_changed_type_facts_withhold_guessed_targets() {
        let (h, mut nav) = setup();
        let d = document("<c:server serviceClass='com.acme.Model'/>");
        let at = position(&d.text, d.text.find("com.acme.Model").unwrap());
        assert!(WsTools::default().definition(&d, at).is_empty());
        h.add_file(
            "src/main/java/Duplicate.java",
            "package com.acme; class Model {}",
        );
        nav.handle_events(
            &[Event::FileSaved(
                "/project/src/main/java/Duplicate.java".into(),
            )],
            &h.invocation(),
        );
        assert!(nav.definition(&d, at).is_empty());
        h.add_file(
            "src/main/java/Duplicate.java",
            "package com.acme; class Different {}",
        );
        nav.handle_events(
            &[Event::FileSaved(
                "/project/src/main/java/Duplicate.java".into(),
            )],
            &h.invocation(),
        );
        assert_eq!(nav.definition(&d, at).len(), 1);
        let foreign = Document {
            text: d.text.replace("c:server", "b:server"),
            ..d.clone()
        };
        assert!(nav.definition(&foreign, at).is_empty());
        h.add_file("src/main/java/Model.java", "package other; class Model {}");
        nav.handle_events(
            &[Event::FileSaved("/project/src/main/java/Model.java".into())],
            &h.invocation(),
        );
        assert!(nav.definition(&d, at).is_empty());
    }
}

#[cfg(test)]
mod refresh_command_tests {
    use super::*;
    use openidea_plugin::{mock::MockHost, text::position};
    #[test]
    fn explicit_refresh_warms_owned_source_models_without_an_editor_provider_scan() {
        let h = MockHost::install();
        h.add_file(
            "src/main/java/Model.java",
            "package com.acme; public class Model {}",
        );
        let doc = Document {
            path: "/project/context.xml".into(),
            language: "xml".into(),
            version: 1,
            text: format!(
                "<o:jaxb2-marshaller xmlns:o='{OXM}'><o:class-to-be-bound name='com.acme.Model'/></o:jaxb2-marshaller>"
            ),
        };
        let at = position(&doc.text, doc.text.find("com.acme.Model").unwrap());
        let mut nav = WsTools::default();
        assert!(nav.definition(&doc, at).is_empty());
        assert_eq!(
            nav.run_command("refresh-models", &h.invocation(), None)
                .unwrap()
                .as_deref(),
            Some("Schemas and endpoints refreshed.")
        );
        assert_eq!(nav.definition(&doc, at).len(), 1);
        assert!(h.requests().is_empty() && h.process_runs().is_empty());
    }
}

#[cfg(test)]
mod context_path_tests {
    use super::*;
    use openidea_plugin::{
        mock::MockHost,
        text::{offset, position},
    };
    #[test]
    fn jaxb_context_path_lists_complete_only_the_selected_colon_component_and_navigate_observed_packages()
     {
        let h = MockHost::install();
        h.add_file(
            "src/main/java/Model.java",
            "package com.acme; public class Model {}",
        );
        h.add_file(
            "src/main/java/Other.java",
            "package com.other; public class Other {}",
        );
        let text = format!("<o:jaxb2-marshaller xmlns:o='{OXM}' context-path='com.acme:com.ot'/>");
        let doc = Document {
            path: "/project/context.xml".into(),
            language: "xml".into(),
            text: text.clone(),
            version: 1,
        };
        let mut nav = WsTools::default();
        nav.activate(&h.context("aritti.spring-ws")).unwrap();
        let package_at = position(&text, text.find("com.acme").unwrap());
        let targets = nav.definition(&doc, package_at);
        assert_eq!(targets.len(), 1);
        assert!(targets[0].path.ends_with("Model.java"));
        let part = text.find("com.ot").unwrap();
        let values = nav.completion(&doc, position(&text, part + 6), None);
        assert_eq!(values.len(), 1);
        assert_eq!(values[0].label, "com.other");
        let r = values[0].replace.unwrap();
        assert_eq!(
            &text[offset(&text, r.start).unwrap()..offset(&text, r.end).unwrap()],
            "com.ot"
        );
        assert_eq!(text.as_bytes()[offset(&text, r.start).unwrap() - 1], b':');
        assert!(h.requests().is_empty() && h.process_runs().is_empty());
    }
}
