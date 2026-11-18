//! Cached literal resource candidates for established source scalar roles.
use crate::*;
use std::collections::BTreeMap;
const MAX_GROUPS: usize = 64;
fn prefix(value: &str) -> Option<(bool, &str)> {
    if value.len() > 512 {
        return None;
    }
    let path = value.strip_prefix("classpath:")?;
    let slash = path.starts_with('/');
    let path = path.strip_prefix('/').unwrap_or(path);
    if !path
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"/_-.".contains(&b))
    {
        return None;
    }
    if !path.is_empty() {
        let parts: Vec<_> = path.split('/').collect();
        if parts
            .iter()
            .enumerate()
            .any(|(i, p)| matches!(*p, "." | "..") || (p.is_empty() && i + 1 < parts.len()))
        {
            return None;
        }
    }
    Some((slash, path))
}
pub(crate) fn completion(
    nav: &mut WsTools,
    doc: &Document,
    at: Position,
) -> Option<Vec<CompletionItem>> {
    let site = xml_resources::sites(doc)
        .into_iter()
        .find(|site| site.range.start <= at && at <= site.range.end)?;
    let empty = || Some(Vec::new());
    let Some((slash, _)) = prefix(&site.value) else {
        return empty();
    };
    let Some(start) = openidea_plugin::text::offset(&doc.text, site.range.start) else {
        return empty();
    };
    let Some(end) = openidea_plugin::text::offset(&doc.text, at) else {
        return empty();
    };
    let Some(typed) = doc.text.get(start..end) else {
        return empty();
    };
    let Some((_, typed)) = prefix(typed) else {
        return empty();
    };
    if !nav.watching
        || doc.path.contains('\\')
        || doc.path.split('/').any(|p| matches!(p, "." | ".."))
    {
        return empty();
    }
    let Some(root) = project_of(&doc.path).and_then(|p| p.root().ok()) else {
        return empty();
    };
    let root_prefix = format!("{}/", root.trim_end_matches('/'));
    if root.is_empty() || root.chars().any(char::is_control) || !doc.path.starts_with(&root_prefix)
    {
        return empty();
    }
    if let Some(project) = project_of(&doc.path) {
        nav.resource_paths.warm(&project, PROVIDER_BUDGET);
    }
    let Some(paths) = nav.resource_paths.paths(&root, site.extension) else {
        return empty();
    };
    let mut groups: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for file in paths {
        if !file.starts_with(&root_prefix) {
            continue;
        }
        for directory in RESOURCE_DIRS {
            let marker = format!("/{directory}/");
            let Some((_, relative)) = file.rsplit_once(&marker) else {
                continue;
            };
            if !relative.starts_with(typed) {
                continue;
            }
            let uri = format!("classpath:{}{relative}", if slash { "/" } else { "" });
            if xml_resources::literal_path(&uri, site.extension).is_some() {
                groups
                    .entry(relative.into())
                    .or_default()
                    .insert(file.clone());
            }
        }
    }
    let mut result = vec![];
    for (relative, targets) in groups.into_iter().take(MAX_GROUPS) {
        if targets.len() != 1 {
            continue;
        }
        let target = targets.into_iter().next().unwrap();
        if project_of(&target).and_then(|p| p.root().ok()).as_deref() != Some(root.as_str()) {
            continue;
        }
        let uri = format!("classpath:{}{relative}", if slash { "/" } else { "" });
        result.push(CompletionItem{detail:Some("Unique cached source resource candidate; effective classpath/content/type not resolved".into()),replace:Some(site.range),filter_text:Some(uri.clone()),..CompletionItem::new(&uri,CompletionKind::File)});
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{
        FilesChanged, Plugin,
        mock::MockHost,
        providers::Providers,
        text::{offset, position},
    };
    const CLASS: &str =
        "org.springframework.ws.server.endpoint.interceptor.PayloadTransformingInterceptor";
    fn doc(value: &str) -> Document {
        Document {
            path: "/project/context.xml".into(),
            language: "xml".into(),
            version: 1,
            text: format!(
                "<b:beans xmlns:b='http://www.springframework.org/schema/beans'>\r\n<!-- 😀 café --> <b:bean id='transformer' class='{CLASS}'><b:property name='requestXslt' value='{value}'/></b:bean>\r\n</b:beans>"
            ),
        }
    }
    fn host() -> MockHost {
        let h = MockHost::install();
        h.add_file("src/main/resources/a/resource.xslt", "opaque");
        h.add_file("src/main/resources/b/resource.xslt", "opaque");
        h
    }
    fn warm(h: &MockHost) -> WsTools {
        let mut p = WsTools::default();
        p.activate(&h.context("aritti.spring-ws")).unwrap();
        p
    }
    fn end(d: &Document, value: &str) -> Position {
        position(&d.text, d.text.find(value).unwrap() + value.len())
    }
    #[test]
    fn prefix_slash_style_and_utf16_full_value_replacement_follow_current_unsaved_source() {
        let h = host();
        let mut p = warm(&h);
        let d = doc("classpath:a/reTAIL");
        let cursor = end(&d, "classpath:a/re");
        let items = p.completion(&d, cursor, None);
        assert_eq!(items.len(), 1);
        let item = &items[0];
        assert_eq!(item.insert_text, "classpath:a/resource.xslt");
        let from = d.text.find("classpath:a/reTAIL").unwrap();
        assert_eq!(
            item.replace,
            Some(range(&d.text, from, from + "classpath:a/reTAIL".len()))
        );
        let r = item.replace.unwrap();
        assert_eq!(offset(&d.text, r.start), Some(from));
        assert_eq!(
            r.start.character as usize,
            d.text[d.text[..from].rfind('\n').unwrap() + 1..from]
                .encode_utf16()
                .count()
        );
        assert!(item.additional_edits.is_empty() && !item.snippet);
        let changed = Document {
            version: 2,
            text: d.text.replace("classpath:a/reTAIL", "classpath:/b/re"),
            ..d
        };
        let result = p.completion(&changed, end(&changed, "classpath:/b/re"), None);
        assert_eq!(result[0].insert_text, "classpath:/b/resource.xslt");
    }
    #[test]
    fn cold_unwatched_ambiguous_foreign_and_missing_root_contexts_do_not_offer_paths() {
        let h = host();
        let d = doc("classpath:/a/re");
        assert!(
            WsTools::default()
                .completion(&d, end(&d, "classpath:/a/re"), None)
                .is_empty()
        );
        let h = h.with_capabilities(&["project.read"]);
        let mut p = warm(&h);
        assert!(
            p.completion(&d, end(&d, "classpath:/a/re"), None)
                .is_empty()
        );
        let h = host();
        h.add_file("src/test/resources/a/resource.xslt", "opaque");
        let mut p = warm(&h);
        assert!(
            p.completion(&d, end(&d, "classpath:/a/re"), None)
                .is_empty()
        );
        let h = host();
        h.add_project("/project/src/main/resources");
        let mut p = warm(&h);
        assert!(
            p.completion(&d, end(&d, "classpath:/a/re"), None)
                .is_empty()
        );
        let h = host();
        let mut p = warm(&h);
        h.set_project_root(None);
        assert!(
            p.completion(&d, end(&d, "classpath:/a/re"), None)
                .is_empty()
        );
    }
    #[test]
    fn unsafe_full_values_foreign_namespaces_and_unknown_scalar_owners_are_quiet() {
        let h = host();
        let mut p = warm(&h);
        for value in [
            "classpath://a/re",
            "classpath:/a/../re",
            "classpath:/a/re${computed}",
            "classpath:/a/re?query",
            "https://example.invalid/a/re",
            "classpath:/a/re%20",
            "classpath:/a/.",
        ] {
            let d = doc(value);
            assert!(p.completion(&d, end(&d, value), None).is_empty(), "{value}");
        }
        let d = doc("classpath:/a/re${computed}");
        assert!(
            p.completion(&d, end(&d, "classpath:/a/re"), None)
                .is_empty(),
            "computed suffix beyond caret cannot be overwritten"
        );
        let good = doc("classpath:/a/re");
        for text in [
            good.text
                .replace(CLASS, "example.PayloadTransformingInterceptor"),
            good.text
                .replace("<b:property ", "<b:property xmlns:b='urn:foreign' "),
            good.text.replace("value=", "b:value="),
            good.text.replace("name='requestXslt'", "name='unknown'"),
        ] {
            let d = Document {
                text,
                ..good.clone()
            };
            assert!(
                p.completion(&d, end(&d, "classpath:/a/re"), None)
                    .is_empty()
            );
        }
    }
    #[test]
    fn candidate_limit_extension_filter_and_lifecycle_only_discovery_are_preserved() {
        let h = MockHost::install();
        for i in 0..70 {
            h.add_file(&format!("src/main/resources/a/r{i:02}.xslt"), "opaque");
        }
        h.add_file("src/main/resources/a/r.xml", "opaque");
        let mut p = warm(&h);
        let d = doc("classpath:/a/");
        let items = p.completion(&d, end(&d, "classpath:/a/"), None);
        assert_eq!(items.len(), 64);
        assert!(items.iter().all(|i| i.insert_text.ends_with(".xslt")));
        h.add_file("src/main/resources/a/new.xslt", "opaque");
        let d = doc("classpath:/a/new");
        assert!(
            p.completion(&d, end(&d, "classpath:/a/new"), None)
                .is_empty(),
            "completion cannot discover newly added files"
        );
        p.handle_events(
            &[Event::FilesChanged(FilesChanged {
                created: vec!["/project/src/main/resources/a/new.xslt".into()],
                ..Default::default()
            })],
            &h.invocation(),
        );
        let items = p.completion(&d, end(&d, "classpath:/a/new"), None);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].insert_text, "classpath:/a/new.xslt");
        assert!(h.requests().is_empty() && h.process_runs().is_empty());
    }
    #[test]
    fn direct_value_child_completion_replaces_only_its_literal_text() {
        let h=host();let mut p=warm(&h);let value="classpath:a/reTAIL";let mut d=doc(value);
        d.text=d.text.replace(&format!(" value='{value}'/>"),&format!("><b:value>{value}</b:value></b:property>"));
        let items=p.completion(&d,end(&d,"classpath:a/re"),None);assert_eq!(items.len(),1);assert_eq!(items[0].insert_text,"classpath:a/resource.xslt");
        let from=d.text.find(value).unwrap();assert_eq!(items[0].replace,Some(spring_text::range(&d.text,from,from+value.len())));
    }
    #[test]
    fn resource_completion_after_ttl() {
        let h = host();
        let mut p = warm(&h);
        p.resource_paths.expire();
        h.add_file("src/main/resources/c/resource.xslt", "opaque");
        let d = doc("classpath:c/re");
        let items = p.completion(&d, end(&d, "classpath:c/re"), None);
        assert_eq!(items.len(), 1);
    }
}
