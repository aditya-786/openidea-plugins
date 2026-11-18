//! Source-derived extraction only: no inferred classpath or runtime bean factory.
use crate::*;
use openidea_plugin::Project;
use openidea_plugin::file_actions::{FileAction, FileActions};
use quick_xml::{events::Event as XmlEvent, reader::NsReader};
use spring_text::xml;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::Duration;
#[derive(Clone)]
struct Pending {
    root: String,
    doc: Document,
    at: usize,
}
#[derive(Default)]
pub(crate) struct Generation {
    next: u64,
    pending: BTreeMap<String, Pending>,
}
pub(crate) fn normalized(path: &str) -> bool {
    path.starts_with('/')
        && path.len() <= 512
        && !path.contains(['\\', '*', '?', '[', ']', '{', '}'])
        && !path.chars().any(char::is_control)
        && !path.split('/').any(|part| part == "." || part == "..")
}
pub(crate) fn qualified(name: &str) -> bool {
    name.len() <= 512
        && name.split(['.', '$']).all(|part| {
            let mut chars = part.bytes();
            chars
                .next()
                .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
                && chars.all(|b| b.is_ascii_alphanumeric() || b == b'_')
        })
}
/// Validate XML before using the permissive navigation scanner. No DTD/entity
/// expansion, inherited local namespace rebinding, or nested profile scopes.
pub(crate) fn generation_document(doc: &Document) -> bool {
    if doc.language != "xml"
        || !doc.path.ends_with(".xml")
        || !normalized(&doc.path)
    {
        return false;
    }
    let mut reader = NsReader::from_str(&doc.text);
    let mut depth = 0usize;
    let mut roots = 0usize;
    let mut count = 0usize;
    loop {
        count += 1;
        if count > 65_536 {
            return false;
        }
        match reader.read_event() {
            Ok(XmlEvent::Start(element)) => {
                if depth == 0 {
                    roots += 1;
                }
                if !crate::xml_namespace::binding_valid(&reader, &element) {
                    return false;
                }
                depth += 1;
            }
            Ok(XmlEvent::Empty(element)) => {
                if depth == 0 {
                    roots += 1;
                }
                if !crate::xml_namespace::binding_valid(&reader, &element) {
                    return false;
                }
            }
            Ok(XmlEvent::End(_)) => {
                let Some(next) = depth.checked_sub(1) else {
                    return false;
                };
                depth = next;
            }
            Ok(XmlEvent::Text(text))
                if depth == 0 && text.as_ref().bytes().any(|b| !b.is_ascii_whitespace()) =>
            {
                return false;
            }
            Ok(XmlEvent::DocType(_) | XmlEvent::GeneralRef(_)) => return false,
            Ok(XmlEvent::Eof) => break,
            Err(_) => return false,
            _ => {}
        }
    }
    if depth != 0 || roots != 1 {
        return false;
    }
    let elements = xml::elements(&doc.text);
    let Some(root) = elements.first() else {
        return false;
    };
    if root.local() != "beans" || !xml_checks::in_namespace(&elements, 0, xml_checks::BEANS_NS) {
        return false;
    }
    if elements.iter().skip(1).any(|element| {
        element.local() == "beans"
            || element.attrs.iter().any(|a| {
                a.name == "xmlns" || a.name.starts_with("xmlns:") || a.name.starts_with("xml:")
            })
    }) {
        return false;
    }
    let declared: BTreeSet<&str> = root
        .attrs
        .iter()
        .filter_map(|attribute| attribute.name.strip_prefix("xmlns:"))
        .chain(std::iter::once("xml"))
        .collect();
    if elements.iter().any(|element| {
        element
            .name
            .split_once(':')
            .is_some_and(|(prefix, _)| !declared.contains(prefix))
            || element.attrs.iter().any(|attribute| {
                attribute
                    .name
                    .split_once(':')
                    .is_some_and(|(prefix, _)| prefix != "xmlns" && !declared.contains(prefix))
            })
    }) {
        return false;
    }
    true
}
fn eligible(doc: &Document, at: usize) -> bool {
    if !generation_document(doc) { return false; }
    let elements = xml::elements(&doc.text);
    let Some(index) = xml::element_at(&elements, at) else {
        return false;
    };
    let bean = &elements[index];
    let Some(parent_index) = bean.parent else {
        return false;
    };
    let parent = &elements[parent_index];
    let Some(tail) = doc.text[..parent.end]
        .rfind("</")
        .filter(|tail| *tail >= bean.end)
    else {
        return false;
    };
    if !["property", "constructor-arg"].contains(&parent.local())
        || !xml_checks::in_namespace(&elements, parent_index, xml_checks::BEANS_NS)
        || parent.attr("ref").is_some()
        || parent.attr("value").is_some()
        || !doc.text[parent.tag_end..bean.start].trim().is_empty()
        || !doc.text[bean.end..tail].trim().is_empty()
    {
        return false;
    }
    bean.local() == "bean"
        && xml_checks::in_namespace(&elements, index, xml_checks::BEANS_NS)
        && bean.attr("name").is_none()
        && bean.value("class").is_some_and(qualified)
        && xml_actions::introduce_bean(&doc.text, at, BTreeSet::new).is_some()
}
fn current(pending: &Pending, doc: &Document) -> Result<Project, Error> {
    if doc.path != pending.doc.path
        || doc.language != pending.doc.language
        || doc.version != pending.doc.version
        || doc.text != pending.doc.text
    {
        return Err(Error::Stale);
    }
    let project = host::project_of(&doc.path).ok_or(Error::Stale)?;
    if project.root()? != pending.root || project.read_file(&doc.path)? != doc.text {
        return Err(Error::Stale);
    }
    Ok(project)
}
/// Every bean name and alias of the project's warm source cache, read on for
/// at most `budget`; refused unless the inventory is complete.
pub(crate) fn names(nav: &mut Navigator, doc: &Document, budget: Duration) -> Result<BTreeSet<String>, Error> {
    let cache = nav.indexes.for_document(&doc.path, &doc.text, budget).ok_or(Error::Stale)?;
    if !cache.complete() {
        return Err("Project source inventory incomplete; cannot safely choose a bean ID.".into());
    }
    Ok(cache.files().values().flat_map(|model| &model.beans).flat_map(checks::names_of).map(str::to_owned).collect())
}

/// A fingerprint of a bean-name inventory, compared again after the user confirms.
pub(crate) fn fingerprint(names: &BTreeSet<String>) -> u64 {
    let mut hasher = DefaultHasher::new();
    names.hash(&mut hasher);
    hasher.finish()
}
#[cfg(test)]
fn resulting(doc: &Document, edits: &[TextEdit]) -> Result<String, Error> {
    crate::reviewed(&doc.text, edits).map(|(text, _)| text)
}
impl FileActions for Navigator {
    fn file_actions(&mut self, doc: &Document, selection: Range) -> Vec<FileAction> {
        if doc.language == "java" { return crate::property_file_actions::actions(self, doc, selection); }
        let Some(at) = offset(&doc.text, selection.start) else {
            return vec![];
        };
        if !eligible(doc, at) {
            return crate::xml_create::actions(self, doc, selection);
        }
        let Some(root) = host::project_of(&doc.path).and_then(|project| project.root().ok()) else {
            return vec![];
        };
        if !normalized(&root)
            || !doc
                .path
                .strip_prefix(&root)
                .is_some_and(|tail| tail.starts_with('/'))
        {
            return vec![];
        }
        if self.xml_generation.pending.len() >= 8 {
            self.xml_generation.pending.clear();
        }
        let Some(next) = self.xml_generation.next.checked_add(1) else {
            return vec![];
        };
        self.xml_generation.next = next;
        let key = format!("spring-xml-introduce:{next}:{}", doc.version);
        self.xml_generation.pending.insert(
            key.clone(),
            Pending {
                root,
                doc: doc.clone(),
                at,
            },
        );
        vec![FileAction {
            title: "Introduce bean…".into(),
            kind: Some("refactor".into()),
            data: Some(key),
            ..Default::default()
        }]
    }
    fn resolve_file_action(
        &mut self,
        doc: &Document,
        action: FileAction,
    ) -> Result<FileAction, Error> {
        if crate::xml_create::owns(&action) { return crate::xml_create::resolve(self, doc, action); }
        if crate::property_file_actions::owns(&action) { return crate::property_file_actions::resolve(self, doc, action); }
        let pending = self
            .xml_generation
            .pending
            .remove(action.data.as_deref().ok_or(Error::Stale)?)
            .ok_or(Error::Stale)?;
        current(&pending, doc)?;
        if !eligible(doc, pending.at) {
            return Err(Error::Stale);
        }
        let before = names(self, doc, COMMAND_BUDGET)?;
        let inventory = fingerprint(&before);
        let suggestion = xml_actions::suggested_bean_id(&doc.text, pending.at, before.clone())
            .ok_or(Error::Stale)?;
        let id = host::input(
            "ID for extracted Spring bean (unique literal name)",
            Some(&suggestion),
        )?
        .ok_or(Error::Cancelled)?;
        let edits=xml_actions::introduce_bean_named(&doc.text,pending.at,||before.clone(),Some(&id))
            .ok_or("Use a new literal ASCII bean ID (letter/underscore, then letters/digits/underscore/dot/hyphen), absent from the source inventory.")?;
        let (_, changes) = crate::reviewed(&doc.text, &edits)?;
        let preview = format!(
            "Source: {}\nVersion: {}\n\nExtract the selected inner bean to this same root <beans>, choosing an ID absent from the current source inventory. This is source refactoring, not a runtime classpath claim. No files are created or overwritten.\n\nChanged lines of the resulting XML:\n{}",
            doc.path,
            doc.version,
            changes
        );
        if !ui_kit::prompts::confirm_exact("Introduce Spring XML bean?", &preview)? {
            return Err(Error::Cancelled);
        }
        current(&pending, doc)?;
        if fingerprint(&names(self, doc, Duration::ZERO)?) != inventory {
            return Err(Error::Stale);
        }
        Ok(FileAction {
            title: "Introduce bean".into(),
            kind: Some("refactor".into()),
            edits: vec![(doc.path.clone(), edits)],
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    const PATH: &str = "/project/beans.xml";
    fn document(body: &str) -> Document {
        Document {
            path: PATH.into(),
            language: "xml".into(),
            version: 1,
            text: format!(
                "<beans xmlns=\"{}\">\n{body}\n</beans>",
                xml_checks::BEANS_NS
            ),
        }
    }
    fn source() -> Document {
        document(
            "<!-- é😀 -->\n<bean id=\"holder\" class=\"a.Holder\"><property name=\"child\"><bean class=\"a.Child\"/></property></bean>",
        )
    }
    fn action(nav: &mut Navigator, doc: &Document) -> FileAction {
        let at = position(&doc.text, doc.text.find("<bean class").unwrap() + 2);
        nav.file_actions(doc, Range::new(at, at)).remove(0)
    }
    #[test]
    fn exact_atomic_extraction_preserves_unsaved_unicode_and_crlf() {
        let host = MockHost::install();
        let mut doc = source();
        doc.text = doc.text.replace('\n', "\r\n");
        host.add_file(PATH, &doc.text);
        host.open_editor(PATH, "xml", &doc.text, &[]);
        let mut nav = Navigator::default();
        let lazy = action(&mut nav, &doc);
        assert!(lazy.edits.is_empty() && lazy.operations.is_empty());
        host.answer_next_prompt(Some(&["child"]));
        host.answer_next_prompt(Some(&["yes"]));
        let resolved = nav.resolve_file_action(&doc, lazy).unwrap();
        assert!(resolved.operations.is_empty());
        assert_eq!(resolved.edits.len(), 1);
        assert_eq!(resolved.edits[0].0, PATH);
        let result = resulting(&doc, &resolved.edits[0].1).unwrap();
        assert!(result.contains("é😀"));
        assert!(result.contains("ref=\"child\""));
        assert!(result.contains("id=\"child\""));
        assert!(!result.replace("\r\n", "").contains('\n'));
        assert_eq!(
            host.editor_text(),
            doc.text,
            "provider returns edits, never directly mutates"
        );
    }
    #[test]
    fn cancelled_single_use_action_cannot_be_replayed_or_forged() {
        let host = MockHost::install();
        let doc = source();
        host.add_file(PATH, &doc.text);
        let mut nav = Navigator::default();
        let lazy = action(&mut nav, &doc);
        let replay = lazy.clone();
        assert!(matches!(
            nav.resolve_file_action(&doc, lazy),
            Err(Error::Cancelled)
        ));
        assert!(matches!(
            nav.resolve_file_action(&doc, replay),
            Err(Error::Stale)
        ));
        assert!(
            nav.resolve_file_action(
                &doc,
                FileAction {
                    data: Some("forged".into()),
                    ..Default::default()
                }
            )
            .is_err()
        );
    }
    #[test]
    fn source_revision_content_root_and_permission_changes_are_refused() {
        let host = MockHost::install();
        let doc = source();
        host.add_file(PATH, &doc.text);
        let mut nav = Navigator::default();
        let lazy = action(&mut nav, &doc);
        let mut changed = doc.clone();
        changed.version += 1;
        assert!(matches!(
            nav.resolve_file_action(&changed, lazy),
            Err(Error::Stale)
        ));
        let lazy = action(&mut nav, &doc);
        host.add_file(PATH, "changed");
        assert!(matches!(
            nav.resolve_file_action(&doc, lazy),
            Err(Error::Stale)
        ));
        host.add_file(PATH, &doc.text);
        let lazy = action(&mut nav, &doc);
        host.set_project_root(Some("/other"));
        assert!(nav.resolve_file_action(&doc, lazy).is_err());
        let _denied = MockHost::install().with_capabilities(&[]);
        assert!(
            Navigator::default()
                .file_actions(&doc, Range::new(Position::new(0, 0), Position::new(0, 0)))
                .is_empty()
        );
    }
    #[test]
    fn namespace_profile_malformed_dynamic_alias_and_bounds_are_quiet() {
        let _host = MockHost::install();
        let good = source();
        let at = good.text.find("<bean class").unwrap() + 2;
        assert!(eligible(&good, at));
        for doc in [
            document("<beans profile=\"dev\"><bean class=\"a.Child\"/></beans>"),
            document(
                "<bean class=\"a.H\"><property name=\"x\"><bean xmlns=\"other\" class=\"a.Child\"/></property></bean>",
            ),
            document("<bean class=\"a.H\"><property><bean class=\"${type}\"/></property></bean>"),
            document(
                "<bean class=\"a.H\"><property><bean name=\"alias\" class=\"a.Child\"/></property></bean>",
            ),
            document("<bean class=\"a.H\"><property><bean class=\"a.Child\"/></wrong></bean>"),
            document(
                "<bean class=\"a.H\"><property><bad:bean class=\"a.Child\"/></property></bean>",
            ),
            Document {
                text: good.text.replace("</beans>", "</beans><extra/>"),
                ..good.clone()
            },
            Document {
                text: good.text.replace("a.Child", "a.Child&amp;Other"),
                ..good.clone()
            },
        ] {
            for element in xml::elements(&doc.text)
                .iter()
                .filter(|element| element.local() == "bean")
            {
                assert!(!eligible(&doc, element.at), "{}", doc.text);
            }
        }
        let dtd = Document {
            text: format!("<!DOCTYPE beans SYSTEM \"x\">{}", good.text),
            ..good.clone()
        };
        assert!(!eligible(&dtd, dtd.text.find("<bean class").unwrap() + 2));
    }
    #[test]
    fn oversized_path_is_quiet_and_unsaved_source_is_authority() {
        let host = MockHost::install();
        let mut oversized = source();
        oversized.path = format!("/project/{}.xml", "a".repeat(513));
        let mut nav = Navigator::default();
        let at = position(
            &oversized.text,
            oversized.text.find("<bean class").unwrap() + 2,
        );
        assert!(nav.file_actions(&oversized, Range::new(at, at)).is_empty());
        let doc = source();
        host.add_file(PATH, "old saved content");
        host.open_editor(PATH, "xml", &doc.text, &[]);
        let lazy = action(&mut nav, &doc);
        host.answer_next_prompt(Some(&["child"]));
        host.answer_next_prompt(Some(&["yes"]));
        assert!(nav.resolve_file_action(&doc, lazy).is_ok());
    }
    #[test]
    fn incomplete_inventory_does_not_choose_a_potentially_colliding_id() {
        let host = MockHost::install();
        let doc = source();
        host.add_file(PATH, &doc.text);
        for index in 0..10_000 {
            host.add_file(&format!("src/C{index}.java"), "");
        }
        let mut nav = Navigator::default();
        let lazy = action(&mut nav, &doc);
        host.answer_next_prompt(Some(&["child"]));
        host.answer_next_prompt(Some(&["yes"]));
        assert!(
            matches!(nav.resolve_file_action(&doc,lazy), Err(Error::Failed(message)) if message.contains("inventory incomplete"))
        );
    }
    #[test]
    fn prompted_id_and_prefix_only_namespace_preserve_exact_source_context() {
        let host = MockHost::install();
        let mut doc = source();
        doc.text = doc
            .text
            .replace(
                "<beans ",
                "<b:beans xmlns:b=\"http://www.springframework.org/schema/beans\" ",
            )
            .replace("</beans>", "</b:beans>")
            .replace(
                "xmlns=\"http://www.springframework.org/schema/beans\"",
                "xmlns=\"urn:other\"",
            )
            .replace("<bean ", "<b:bean ")
            .replace("</bean>", "</b:bean>")
            .replace("<property ", "<b:property ")
            .replace("</property>", "</b:property>")
            .replace('\n', "\r\n");
        host.add_file(PATH, &doc.text);
        host.open_editor(PATH, "xml", &doc.text, &[]);
        let mut nav = Navigator::default();
        let at = position(&doc.text, doc.text.find("<b:bean class").unwrap() + 3);
        let lazy = nav.file_actions(&doc, Range::new(at, at)).remove(0);
        host.answer_next_prompt(Some(&["child.worker-2"]));
        host.answer_next_prompt(Some(&[]));
        let resolved = nav.resolve_file_action(&doc, lazy).unwrap();
        let after = resulting(&doc, &resolved.edits[0].1).unwrap();
        assert!(
            after.contains("<b:property name=\"child\" ref=\"child.worker-2\"/>")
                && after.contains("<b:bean id=\"child.worker-2\" class=\"a.Child\"/>")
        );
        assert!(
            after.contains("xmlns=\"urn:other\"")
                && after.contains("xmlns:b=\"http://www.springframework.org/schema/beans\"")
                && after.contains("é😀")
        );
        assert!(!after.replace("\r\n", "").contains('\n'));
        assert_eq!(host.editor_text(), doc.text);
    }
    #[test]
    fn invalid_or_colliding_prompted_ids_never_return_an_edit() {
        for id in [
            "",
            "1child",
            "child,alias",
            "child&value",
            "a:b",
            "${id}",
            "holder",
            "taken",
            "busy",
        ] {
            let host = MockHost::install();
            let doc = source();
            host.add_file(PATH, &doc.text);
            let mut nav = Navigator::default();
            let lazy = action(&mut nav, &doc);
            // New declarations after listing must be read independently of panel cache.
            host.add_file(
                "other.xml",
                &document("<bean id=\"taken\" name=\"busy\" class=\"a.Taken\"/>").text,
            );
            host.answer_next_prompt(Some(&[id]));
            host.answer_next_prompt(Some(&[]));
            assert!(nav.resolve_file_action(&doc, lazy).is_err(), "{id}");
        }
    }
    #[test]
    fn input_and_final_preview_cancellation_are_both_inert_and_single_use() {
        for preview in [false, true] {
            let host = MockHost::install();
            let doc = source();
            host.add_file(PATH, &doc.text);
            host.open_editor(PATH, "xml", &doc.text, &[]);
            let mut nav = Navigator::default();
            let lazy = action(&mut nav, &doc);
            if preview {
                host.answer_next_prompt(Some(&["pickedChild"]));
            }
            host.answer_next_prompt(None);
            assert!(matches!(
                nav.resolve_file_action(&doc, lazy.clone()),
                Err(Error::Cancelled)
            ));
            assert!(nav.resolve_file_action(&doc, lazy).is_err());
            assert_eq!(host.editor_text(), doc.text);
        }
    }
    #[test]
    fn reserved_and_unbound_namespace_bindings_cannot_offer_extraction() {
        let host = MockHost::install();
        let good = source();
        let mut nav = Navigator::default();
        for binding in [
            "xmlns:xml=\"urn:foreign\"",
            "xmlns:xmlns=\"urn:foreign\"",
            "xmlns:q=\"http://www.w3.org/XML/1998/namespace\"",
            "bad:flag=\"x\"",
        ] {
            let doc = Document {
                text: good
                    .text
                    .replacen("<beans ", &format!("<beans {binding} "), 1),
                ..good.clone()
            };
            host.add_file(PATH, &doc.text);
            let at = position(&doc.text, doc.text.find("<bean class").unwrap() + 2);
            assert!(
                nav.file_actions(&doc, Range::new(at, at)).is_empty(),
                "{binding}"
            );
        }
        let legal = Document {
            text: good.text.replacen(
                "<beans ",
                "<beans xmlns:xml=\"http://www.w3.org/XML/1998/namespace\" xml:lang=\"en\" ",
                1,
            ),
            ..good
        };
        let at = position(&legal.text, legal.text.find("<bean class").unwrap() + 2);
        assert!(!nav.file_actions(&legal, Range::new(at, at)).is_empty());
    }
    #[test]
    fn introduce_bean_uses_warm_cache() {
        let host = MockHost::install();
        let doc = source();
        host.add_file(PATH, &doc.text);
        host.add_file("other.xml", &document("<bean id=\"taken\" class=\"a.Taken\"/>").text);
        let mut nav = Navigator::default();
        nav.run_command("reindex", &host.invocation(), None).unwrap();
        host.add_file("other.xml", &document("").text);
        let lazy = action(&mut nav, &doc);
        host.answer_next_prompt(Some(&["taken"]));
        assert!(
            matches!(nav.resolve_file_action(&doc, lazy), Err(Error::Failed(message)) if message.contains("absent from the source inventory")),
            "the warm inventory still holds taken: no rescan"
        );
        let lazy = action(&mut nav, &doc);
        host.answer_next_prompt(Some(&["fresh"]));
        host.answer_next_prompt(Some(&[]));
        assert!(nav.resolve_file_action(&doc, lazy).is_ok());
    }
}
