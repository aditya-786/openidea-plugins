//! Bounded MVC XML contracts from official current and versioned schema profiles.
//! These source checks do not implement a complete XML DOM/XSD/type resolver.
#[path = "mvc_xml_model.rs"]
mod model;
use openidea_plugin::{
    Location, Position,
    providers::{CompletionItem, Document},
};
use spring_text::finding::Finding;
pub fn check(text: &str) -> Vec<Finding> {
    model::check(text)
}
/// `check` plus findings that need the project: import closure bean refs and `mvc:resources` locations.
pub(crate) fn check_in_project(doc: &Document, imported_elsewhere: &dyn Fn() -> bool) -> Vec<Finding> {
    let mut out = check(&doc.text);
    if let Some(project) = spring_text::index::project_of(&doc.path) {
        let read = |p: &str| project.read_file(p).ok().filter(|t| t.len() <= 256 << 10);
        let find = |glob: &str| project.find_files(glob, 200).ok();
        out.extend(model::context_findings(&doc.path, &doc.text, &read, &find, imported_elsewhere));
    }
    out
}

/// The `resource` of every `<import>` of a Spring XML file.
pub(crate) fn imports(_path: &str, text: &str) -> Option<Vec<String>> {
    if !text.contains("import") {
        return None;
    }
    let found: Vec<String> = spring_text::xml::elements(text)
        .iter()
        .filter(|e| e.local() == "import")
        .filter_map(|e| e.value("resource").map(str::to_owned))
        .collect();
    (!found.is_empty()).then_some(found)
}
pub(crate) fn definition(doc: &Document, at: Position) -> Vec<Location> {
    model::definition(doc, at)
}
/// Literal `view-name` values of `<mvc:view-controller>` elements.
pub(crate) fn view_names(text: &str) -> Vec<spring_text::java::Str> {
    model::view_names(text)
}
pub(crate) fn completion(doc: &Document, at: Position) -> Vec<CompletionItem> {
    model::completion(doc, at)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{
        mock::MockHost,
        text::{offset, position},
    };
    const MVC: &str = "http://www.springframework.org/schema/mvc";
    const BEANS: &str = "http://www.springframework.org/schema/beans";
    fn doc(body: &str) -> Document {
        Document {
            path: "/project/context.xml".into(),
            text: format!("<b:beans xmlns:b='{BEANS}' xmlns:m='{MVC}'>{body}</b:beans>"),
            language: "xml".into(),
            version: 1,
        }
    }
    fn versioned(body: &str, version: &str) -> Document {
        let mut d = doc(body);
        d.text=d.text.replacen("<b:beans",&format!("<b:beans xmlns:xsi='http://www.w3.org/2001/XMLSchema-instance' xsi:schemaLocation='{MVC} https://www.springframework.org/schema/mvc/spring-mvc-{version}.xsd'"),1);
        d
    }
    #[test]
    fn current_schema_checks_required_fields_in_their_actual_parent_roles() {
        let d = doc(
            "<m:view-controller/><m:resources mapping='/static/**'><m:resource-chain><m:resolvers><m:version-resolver><m:fixed-version-strategy/></m:version-resolver></m:resolvers></m:resource-chain></m:resources><m:cors><m:mapping/></m:cors><m:script-template-configurer><m:script/></m:script-template-configurer>",
        );
        let values = check(&d.text);
        assert_eq!(
            values
                .iter()
                .filter(|f| f.code == "missing-attribute")
                .count(),
            9
        );
        assert!(values.iter().any(|f| f.message.contains("engine-name")));
        assert!(values.iter().any(|f| f.message.contains("patterns")));
        assert!(values.iter().any(|f| f.message.contains("render-function")));
        let misplaced = doc(
            "<m:mapping/><m:resource-chain/><m:resources mapping='/x' location='/x'><m:mapping/></m:resources>",
        );
        assert!(check(&misplaced.text).is_empty());
    }
    #[test]
    fn booleans_accept_schema_lexical_forms_and_integers_keep_their_declared_width() {
        let d = doc(
            "<m:annotation-driven enable-matrix-variables='1' ignore-default-model-on-redirect=' false '><m:message-converters register-defaults='0'/><m:async-support default-timeout='2147483648'/></m:annotation-driven><m:status-controller path='/x' status-code='+200'/>",
        );
        assert!(check(&d.text).is_empty());
        let bad = doc(
            "<m:annotation-driven ignore-default-model-on-redirect='yes'><m:async-support default-timeout='9223372036854775808'/></m:annotation-driven><m:status-controller path='/x' status-code='2147483648'/>",
        );
        assert_eq!(check(&bad.text).len(), 3);
        let dynamic = doc(
            "<m:annotation-driven ignore-default-model-on-redirect='${redirect}'><m:async-support default-timeout='#{timeout}'/></m:annotation-driven>",
        );
        assert!(check(&dynamic.text).is_empty());
    }
    #[test]
    fn versioned_profiles_do_not_apply_renamed_or_later_attribute_contracts() {
        let legacy = versioned(
            "<m:annotation-driven ignoreDefaultModelOnRedirect='wrong' ignore-default-model-on-redirect='wrong'/>",
            "3.2",
        );
        let found = check(&legacy.text);
        assert_eq!(found.len(), 1);
        assert!(found[0].message.contains("boolean"));
        let older = versioned(
            "<m:resources mapping='/x' location='/x'><m:resource-chain/></m:resources>",
            "4.0",
        );
        assert!(check(&older.text).is_empty());
        let modern = versioned(
            "<m:resources mapping='/x' location='/x'><m:resource-chain/></m:resources>",
            "4.1",
        );
        assert_eq!(check(&modern.text).len(), 1);
        let unsupported = versioned("<m:view-controller/>", "99.0");
        assert!(check(&unsupported.text).is_empty());
        let current = doc(
            "<m:annotation-driven><m:path-matching suffix-pattern='invalid'/></m:annotation-driven>",
        );
        assert!(check(&current.text).is_empty());
        let six = versioned(
            "<m:annotation-driven><m:path-matching suffix-pattern='invalid'/></m:annotation-driven>",
            "6.2",
        );
        assert_eq!(check(&six.text).len(), 1);
    }
    #[test]
    fn closed_xml_namespace_and_context_guards_suppress_guessed_schema_roles() {
        for text in [
            format!("<r xmlns:m='{MVC}'><m:view-controller/></r>"),
            format!("<beans xmlns:m='{MVC}'><m:view-controller></beans>"),
            format!("<beans xmlns:m='{MVC}'><m:view-controller path='/a' path='/b'/></beans>"),
            format!("<beans xmlns:m='{MVC}' xmlns:q='urn:foreign'><q:view-controller/></beans>"),
            format!("<beans xmlns:xml='{MVC}'><xml:view-controller/></beans>"),
            format!("<!DOCTYPE beans []><beans xmlns:m='{MVC}'><m:view-controller/></beans>"),
            format!("<beans xmlns:m='{MVC}'><m:view-controller path='&name;'/></beans>"),
            format!("<beans xmlns:m='{MVC}'><m:view-controller path='/x' broken/></beans>"),
        ] {
            assert!(check(&text).is_empty(), "{text}");
        }
        let snippet = format!("<beans xmlns:m='{MVC}'><m:view-controller/></beans>");
        assert_eq!(check(&snippet).len(), 1);
        let other =
            format!("<beans xmlns='urn:foreign' xmlns:m='{MVC}'><m:view-controller/></beans>");
        assert!(check(&other).is_empty());
    }
    #[test]
    fn jdbc_url_amp_keeps_mvc_xml_checks() {
        let d = doc("<m:view-controller/><bean url='jdbc:h2:mem:db?a=1&amp;b=2'/>");
        assert_eq!(check(&d.text).len(), 1);
    }
    #[test]
    fn completion_uses_current_unsaved_utf16_value_spans_without_host_access() {
        let _h = MockHost::install().with_capabilities(&[]);
        let d = doc("<!--😀-->\r\n<m:annotation-driven ignore-default-model-on-redirect='fa'/>");
        let start = d.text.find("='fa'").unwrap() + 2;
        let values = completion(&d, position(&d.text, start + 2));
        assert_eq!(values.len(), 1);
        assert_eq!(values[0].label, "false");
        let r = values[0].replace.unwrap();
        assert_eq!(
            &d.text[offset(&d.text, r.start).unwrap()..offset(&d.text, r.end).unwrap()],
            "fa"
        );
        let old = versioned(
            "<m:annotation-driven ignore-default-model-on-redirect='fa'/>",
            "3.2",
        );
        let start = old.text.find("='fa'").unwrap() + 2;
        assert!(completion(&old, position(&old.text, start)).is_empty());
    }
    #[test]
    fn reference_roles_navigate_and_complete_only_unambiguous_declared_local_ids() {
        let _h = MockHost::install().with_capabilities(&[]);
        let d = doc(
            "<b:bean id='validator'/><b:bean id='converter'/><m:annotation-driven validator='validator'><m:message-converters><b:ref bean='converter'/></m:message-converters></m:annotation-driven><m:interceptors><b:ref bean='converter'/></m:interceptors>",
        );
        for needle in ["validator='validator'", "bean='converter'"] {
            let start = d.text.find(needle).unwrap() + needle.find('=').unwrap() + 2;
            let at = position(&d.text, start);
            let targets = definition(&d, at);
            assert_eq!(targets.len(), 1);
            assert_eq!(targets[0].path, d.path);
            let values = completion(&d, at);
            assert_eq!(values.len(), 2);
        }
        let duplicate = Document {
            text: d.text.replace(
                "<b:bean id='converter'/>",
                "<b:bean id='converter'/><b:bean id='converter'/>",
            ),
            ..d.clone()
        };
        let at = position(
            &duplicate.text,
            duplicate.text.find("bean='converter'").unwrap() + 6,
        );
        assert!(definition(&duplicate, at).is_empty());
        let imported = Document {
            text: d.text.replace(
                "<b:bean id='converter'/>",
                "<b:import resource='other.xml'/><b:bean id='converter'/>",
            ),
            ..d.clone()
        };
        let at = position(
            &imported.text,
            imported.text.find("validator='validator'").unwrap() + 11,
        );
        assert!(definition(&imported, at).is_empty());
        let snippet = Document {
            text: d
                .text
                .replace(&format!("xmlns:b='{BEANS}'"), "xmlns:b='urn:foreign'"),
            ..d
        };
        let at = position(
            &snippet.text,
            snippet.text.find("validator='validator'").unwrap() + 11,
        );
        assert!(definition(&snippet, at).is_empty());
    }
    #[test]
    fn reference_absence_is_never_reported_as_missing_runtime_bean() {
        let d = doc("<m:annotation-driven validator='external'/>");
        assert!(check(&d.text).is_empty());
    }

    const ROOT: &str = "/p/src/main/resources/mvc.xml";
    fn findings(
        path: &str,
        body: &str,
        files: &[(&str, &str)],
        listed: &[&str],
    ) -> Vec<(&'static str, String)> {
        let d = doc(body);
        let files: Vec<(String, String)> = files
            .iter()
            .map(|(p, b)| (p.to_string(), doc(b).text))
            .collect();
        let read = |p: &str| files.iter().find(|f| f.0 == p).map(|f| f.1.clone());
        let find = |g: &str| {
            Some(if g == "**/*.xml" {
                files.iter().map(|f| f.0.clone()).collect()
            } else {
                listed
                    .iter()
                    .filter(|l| g.trim_end_matches("/**").replace("**/", "") == l.to_string())
                    .map(|l| l.to_string())
                    .collect()
            })
        };
        let file = path.rsplit('/').next().unwrap_or(path);
        let imported_elsewhere = || {
            let Some(listed): Option<Vec<String>> = find("**/*.xml") else {
                return true;
            };
            listed.iter().filter(|f| f.as_str() != path).any(|f| read(f).is_none_or(|t| t.contains(file) && t.contains("import")))
        };
        model::context_findings(path, &d.text, &read, &find, &imported_elsewhere)
            .into_iter()
            .map(|f| (f.code, d.text[f.start..f.end].to_string()))
            .collect()
    }
    #[test]
    fn imported_beans_resolve_through_classpath_relative_and_cyclic_imports() {
        let main = "<b:import resource='classpath:a.xml'/><m:annotation-driven validator='v' conversion-service='c'/>";
        let a = "<b:import resource='sub/b.xml'/><b:import resource='classpath:mvc.xml'/><b:bean id='v'/>";
        let b = "<b:bean id='c'/>";
        let files = [
            ("/p/src/main/resources/a.xml", a),
            ("/p/src/main/resources/sub/b.xml", b),
        ];
        assert!(findings(ROOT, main, &files, &[]).is_empty());
        let none = findings(ROOT, main, &files[..1], &[]);
        assert!(none.is_empty(), "unreadable import keeps the context unresolved");
    }
    #[test]
    fn unknown_beans_are_reported_only_for_a_fully_resolved_closure() {
        let body = "<m:annotation-driven validator='nope'/>";
        let found = findings(ROOT, body, &[], &[]);
        assert_eq!(found, [("mvc-xml-unknown-bean", "nope".to_string())]);
        for quiet in [
            "<b:import resource='http://x/y.xml'/>",
            "<b:import resource='${cfg}.xml'/>",
            "<b:import resource='missing.xml'/>",
            "<b:import resource='/abs.xml'/>",
        ] {
            assert!(findings(ROOT, &format!("{quiet}{body}"), &[], &[]).is_empty(), "{quiet}");
        }
        let importer = [("/p/src/main/resources/root.xml", "<b:import resource='mvc.xml'/>")];
        assert!(findings(ROOT, body, &importer, &[]).is_empty());
        let foreign = format!("<x:component-scan xmlns:x='urn:ctx'/>{body}");
        assert!(findings(ROOT, &foreign, &[], &[]).is_empty());
    }
    #[test]
    fn beans_declared_only_inside_profile_blocks_are_reported_as_such() {
        let body = "<b:beans profile='dev'><b:bean id='v'/></b:beans><b:beans profile='prod'><b:bean id='v'/><b:bean name='x, y'/></b:beans><m:annotation-driven validator='v' message-codes-resolver='x'/>";
        let found = findings(ROOT, body, &[], &[]);
        assert_eq!(found.len(), 2);
        assert!(found.iter().all(|f| f.0 == "mvc-xml-bean-in-profile"));
        let both = format!("<b:bean id='v'/>{body}");
        assert_eq!(findings(ROOT, &both, &[], &[]).len(), 1);
        let nested = "<b:beans profile='dev'><m:annotation-driven validator='v'/></b:beans><b:bean id='v'/>";
        assert!(findings(ROOT, nested, &[], &[]).is_empty());
        let imported = "<b:beans profile='dev'><b:import resource='a.xml'/></b:beans><m:annotation-driven validator='v'/>";
        let a = [("/p/src/main/resources/a.xml", "<b:bean id='v'/>")];
        assert_eq!(findings(ROOT, imported, &a, &[])[0].0, "mvc-xml-bean-in-profile");
    }
    #[test]
    fn interceptor_refs_use_the_same_context() {
        let body = "<m:interceptors><b:ref bean='gone'/></m:interceptors>";
        assert_eq!(findings(ROOT, body, &[], &[])[0].0, "mvc-xml-unknown-bean");
    }
    #[test]
    fn resource_locations_are_checked_against_resources_and_webapp_only_when_provable() {
        let body = |l: &str| format!("<m:resources mapping='/s/**' location='{l}'/>");
        let missing = findings(ROOT, &body("classpath:/static/, /assets/"), &[], &["src/main/resources/static"]);
        assert_eq!(missing, [("mvc-xml-missing-resource-location", "/assets/".to_string())]);
        assert!(findings(ROOT, &body("/assets/"), &[], &["src/main/webapp/assets"]).is_empty());
        for quiet in [
            "classpath:/META-INF/resources/webjars/",
            "classpath*:/x/",
            "file:/tmp/x/",
            "classpath:/${dir}/",
            "classpath:/st*/",
            "/",
        ] {
            assert!(findings(ROOT, &body(quiet), &[], &[]).is_empty(), "{quiet}");
        }
        assert!(findings("/p/src/test/resources/mvc.xml", &body("classpath:/x/"), &[], &[]).is_empty());
    }
}
