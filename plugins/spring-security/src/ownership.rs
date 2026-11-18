//! Read-only source candidates. Semantic Spring bean ownership stays separate.
use super::*;
use openidea_plugin::hover::Hover;
const SECURITY: &str = "http://www.springframework.org/schema/security";
const BEANS: &str = "http://www.springframework.org/schema/beans";
const MAX_TEXT: usize = 32 << 10;
fn literal(name: &str) -> bool {
    let mut bytes = name.bytes();
    name.len() <= 128
        && bytes.next().is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
}
struct Candidate {
    name: String,
    reference: Range,
    targets: Vec<Range>,
}
fn candidate(doc: &Document, at: Position) -> Option<Candidate> {
    if doc.language != "xml"
        || !doc.path.ends_with(".xml")
        || doc.text.len() > MAX_TEXT
        || doc.text.contains('&')
        || spring_text::index::owning_root(&doc.path).is_none()
        || !xml::analysis_valid(&doc.text)
    {
        return None;
    }
    let elements = xml::elements(&doc.text);
    let root = elements.first()?;
    if root.local() != "beans" || root.parent.is_some() || xml::namespace(&elements, 0) != Some(BEANS) || elements.len() > 512 {
        return None;
    }
    let off = offset(&doc.text, at)?;
    let mut selected = None;
    for (i, e) in elements.iter().enumerate() {
        if xml::namespace(&elements, i) != Some(SECURITY) {
            continue;
        }
        let allowed = refs::xml_reference_attributes(e.local());
        for a in &e.attrs {
            if allowed.contains(&a.name.as_str()) && literal(&a.value) && a.value_at <= off && off < a.value_end {
                selected = Some(a);
            }
        }
    }
    let selected = selected?;
    let mut targets = Vec::new();
    let mut conflict = false;
    for (i, e) in elements.iter().enumerate().filter(|(_, e)| e.parent == Some(0)) {
        if e.value("id") == Some(selected.value.as_str()) && (xml::namespace(&elements, i) != Some(BEANS) || e.local() != "bean") {
            conflict = true;
        }
        if xml::namespace(&elements, i) != Some(BEANS) {
            continue;
        }
        if e.local() == "alias" && e.value("alias") == Some(selected.value.as_str()) {
            conflict = true;
        }
        if e.local() != "bean" {
            continue;
        }
        if e.value("name")
            .is_some_and(|names| names.split(|c: char| c == ',' || c == ';' || c.is_whitespace()).any(|n| n == selected.value))
        {
            conflict = true;
        }
        if let Some(id) = e.attr("id").filter(|id| id.name == "id" && id.value == selected.value && literal(&id.value)) {
            targets.push(range(&doc.text, id.value_at, id.value_end));
        }
    }
    if elements.iter().enumerate().any(|(i, e)| {
        e.parent != Some(0)
            && xml::namespace(&elements, i) == Some(BEANS)
            && e.local() == "bean"
            && e.value("id") == Some(selected.value.as_str())
    }) {
        conflict = true;
    }
    if conflict || targets.len() != 1 {
        targets.clear();
    }
    Some(Candidate { name: selected.value.clone(), reference: range(&doc.text, selected.value_at, selected.value_end), targets })
}
pub(crate) fn definition(doc: &Document, at: Position) -> Option<Vec<Location>> {
    let c = candidate(doc, at)?;
    Some(c.targets.into_iter().map(|range| Location { path: doc.path.clone(), range }).collect())
}
pub(crate) fn hover(doc: &Document, at: Position) -> Option<Hover> {
    let c = candidate(doc, at)?;
    let owner = match c.targets.as_slice() {
        [one] => format!("One exact direct bean id in this current file at line {}.", one.start.line + 1),
        _ => "No unique supported local bean-id candidate; aliases, profiles, imports, Java and library beans remain unresolved.".into(),
    };
    Some(Hover::new(format!("**Security bean reference** `{}`\n\n{owner}\n\nCurrent unsaved XML source candidate only. Bean type, effective Spring context and security policy are not resolved. Spring Beans owns its broader bean model; no additional host Find Usages/Rename graph is published here.",c.name)).range(c.reference))
}
#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{hover::HoverProvider, mock::MockHost};
    fn doc(body: &str) -> Document {
        Document {
            path: "/project/security.xml".into(),
            language: "xml".into(),
            version: 3,
            text: format!("<b:beans xmlns:b=\"{BEANS}\" xmlns:s=\"{SECURITY}\">{body}</b:beans>"),
        }
    }
    fn at(doc: &Document) -> Position {
        let at = doc.text.rfind("authz").unwrap();
        range(&doc.text, at, at + 5).start
    }
    #[test]
    fn precise_current_unsaved_utf16_candidates_without_cached_discovery() {
        let host = MockHost::install();
        let d = doc("<!--😀--><b:bean id=\"authz\" class=\"PrivateType\"/><s:http authentication-manager-ref=\"authz\"/>");
        let mut tools = SecurityTools::default();
        let found = tools.definition(&d, at(&d));
        assert_eq!(found.len(), 1);
        let from = d.text.find("id=\"authz").unwrap() + 4;
        assert_eq!(found[0].range.start.character, d.text[..from].encode_utf16().count() as u32);
        assert_eq!(found[0].range.end.character - found[0].range.start.character, 5);
        let h = tools.hover(&d, at(&d)).unwrap();
        assert!(h.markdown.contains("Spring Beans"));
        assert!(!h.markdown.contains("PrivateType"));
        assert!(host.requests().is_empty());
        let changed = Document { text: d.text.replacen("id=\"authz\"", "id=\"other\"", 1), ..d };
        assert!(definition(&changed, at(&changed)).unwrap().is_empty());
    }
    #[test]
    fn duplicate_alias_name_and_profile_candidates_are_not_unique_owners() {
        let _host = MockHost::install();
        for declarations in [
            "<b:bean id=\"authz\"/><b:bean id=\"authz\"/>",
            "<b:bean id=\"authz\"/><b:alias name=\"other\" alias=\"authz\"/>",
            "<b:bean id=\"authz\"/><b:bean name=\"other,authz\"/>",
            "<b:bean id=\"authz\"/><s:authentication-manager id=\"authz\"/>",
            "<b:bean id=\"authz\"/><b:beans profile=\"a\"><b:bean id=\"authz\"/></b:beans>",
            "<b:beans profile=\"a\"><b:bean id=\"authz\"/></b:beans>",
        ] {
            let d = doc(&format!("{declarations}<s:http authentication-manager-ref=\"authz\"/>"));
            assert!(definition(&d, at(&d)).unwrap().is_empty());
        }
    }
    #[test]
    fn exact_namespaces_attributes_literal_names_and_balanced_source() {
        let _host = MockHost::install();
        for body in [
            "<s:unknown authentication-manager-ref=\"authz\"/>",
            "<s:http not-a-ref=\"authz\"/>",
            "<s:http authentication-manager-ref=\"${authz}\"/>",
            "<s:http authentication-manager-ref=\"&amp;authz\"/>",
            "<s:http authentication-manager-ref=\"authz\">",
        ] {
            let d = doc(body);
            assert!(definition(&d, at(&d)).is_none(), "{body}");
        }
        let d = doc("<s:http authentication-manager-ref=\"authz\"/>");
        let d = Document { text: d.text.replace(SECURITY, &format!("{SECURITY}-fake")), ..d };
        assert!(definition(&d, at(&d)).is_none());
    }
    #[test]
    fn project_bounds_and_half_open_token_limits() {
        let host = MockHost::install();
        let d = doc("<s:http authentication-manager-ref=\"authz\"/>");
        let end = range(&d.text, d.text.rfind("authz").unwrap() + 5, d.text.rfind("authz").unwrap() + 5).start;
        assert!(definition(&d, end).is_none());
        host.set_project_root(Some("/other"));
        assert!(definition(&d, at(&d)).is_none());
        host.set_project_root(Some("/project"));
        let d = doc(&" ".repeat(MAX_TEXT));
        assert!(candidate(&d, Position::default()).is_none());
    }
    #[test]
    fn no_automatic_role_or_bean_rename_graph() {
        assert!(!include_str!("../plugin.toml").contains("contributes.documentIndexers"));
        let registration = include_str!("lib.rs").lines().find(|s| s.starts_with("register_plugin!(")).unwrap();
        assert!(!registration.contains("indexing"));
    }
    #[test]
    fn reserved_rebinding_withholds_public_local_navigation_and_hover() {
        let _host = MockHost::install();
        let mut owner = SecurityTools::default();
        let d = doc("<b:bean id='authz'/><s:http authentication-manager-ref='authz'/>");
        let bad = Document { text: d.text.replace("xmlns:s=", "xmlns:xml=").replace("<s:http", "<xml:http"), ..d.clone() };
        assert!(owner.definition(&bad, at(&bad)).is_empty());
        assert!(owner.hover(&bad, at(&bad)).is_none());
        let legal =
            Document { text: d.text.replace("<b:beans ", "<b:beans xmlns:xml='http://www.w3.org/XML/1998/namespace' xml:lang='en' "), ..d };
        assert_eq!(owner.definition(&legal, at(&legal)).len(), 1);
    }
}
