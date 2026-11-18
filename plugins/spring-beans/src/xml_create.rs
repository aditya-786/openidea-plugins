//! Explicit source bean declaration for a selected literal XML reference.
use crate::*;
use openidea_plugin::file_actions::FileAction;
use spring_text::xml;
const PREFIX: &str = "spring-xml-create:";
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
fn literal_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}
struct Candidate {
    id: String,
    insert: usize,
    prefix: String,
    indent: String,
    newline: &'static str,
}
fn candidate(doc: &Document, at: usize) -> Option<Candidate> {
    if !xml_file_actions::generation_document(doc) {
        return None;
    }
    let elements = xml::elements(&doc.text);
    let root = elements.first()?;
    // Only exact unqualified ref attributes on simple Spring value holders.
    let (index, element, reference) = elements.iter().enumerate().find_map(|(i, e)| {
        if !matches!(e.local(), "property" | "constructor-arg")
            || !xml_checks::in_namespace(&elements, i, xml_checks::BEANS_NS)
            || !doc.text.get(e.start..e.tag_end)?.trim_end().ends_with("/>")
            || e.attrs.iter().any(|a| a.name == "value")
        {
            return None;
        }
        let reference = e
            .attrs
            .iter()
            .find(|a| a.name == "ref" && a.holds(at) && literal_id(&a.value))?;
        Some((i, e, reference))
    })?;
    // Positive current-document declarations already own this name; no absence
    // assumption or cross-file I/O is needed to suppress an unnecessary offer.
    let local = model::parse(&doc.path, &doc.text)?;
    if local
        .beans
        .iter()
        .flat_map(checks::names_of)
        .any(|name| name == reference.value)
    {
        return None;
    }
    let parent = element.parent?;
    if elements[parent].local() != "bean"
        || !xml_checks::in_namespace(&elements, parent, xml_checks::BEANS_NS)
    {
        return None;
    }
    if elements.iter().any(|e| e.parent == Some(index)) {
        return None;
    }
    let close = doc
        .text
        .get(..root.end)?
        .rfind(&format!("</{}", root.name))?;
    if close < root.tag_end {
        return None;
    }
    let line = doc.text[..close].rfind('\n').map_or(0, |n| n + 1);
    let root_indent = &doc.text[line..close];
    let (insert, indent) = if root_indent.chars().all(|c| matches!(c, ' ' | '\t')) {
        (line, format!("{root_indent}    "))
    } else {
        (close, "    ".into())
    };
    let newline = if doc.text.contains("\r\n") {
        if doc.text.replace("\r\n", "").contains(['\r', '\n']) {
            return None;
        }
        "\r\n"
    } else {
        if doc.text.contains('\r') {
            return None;
        }
        "\n"
    };
    let prefix = root
        .name
        .rsplit_once(':')
        .map_or(String::new(), |(p, _)| format!("{p}:"));
    Some(Candidate {
        id: reference.value.clone(),
        insert,
        prefix,
        indent,
        newline,
    })
}
fn current(pending: &Pending, doc: &Document) -> Result<openidea_plugin::Project, Error> {
    if doc != &pending.doc {
        return Err(Error::Stale);
    }
    let project = host::project_of(&doc.path).ok_or(Error::Stale)?;
    if project.root()? != pending.root || project.read_file(&doc.path)? != doc.text {
        return Err(Error::Stale);
    }
    Ok(project)
}
pub(crate) fn owns(action: &FileAction) -> bool {
    action
        .data
        .as_deref()
        .is_some_and(|d| d.starts_with(PREFIX))
}
pub(crate) fn actions(nav: &mut Navigator, doc: &Document, selection: Range) -> Vec<FileAction> {
    let Some(at) = offset(&doc.text, selection.start) else {
        return vec![];
    };
    if candidate(doc, at).is_none() {
        return vec![];
    }
    let Some(root) = host::project_of(&doc.path).and_then(|p| p.root().ok()) else {
        return vec![];
    };
    if !xml_file_actions::normalized(&root) || !cache::owned(&doc.path, &root) {
        return vec![];
    }
    if nav.xml_creation.pending.len() >= 8 {
        nav.xml_creation.pending.clear();
    }
    let Some(next) = nav.xml_creation.next.checked_add(1) else {
        return vec![];
    };
    nav.xml_creation.next = next;
    let key = format!("{PREFIX}{next}:{}", doc.version);
    nav.xml_creation.pending.insert(
        key.clone(),
        Pending {
            root,
            doc: doc.clone(),
            at,
        },
    );
    vec![FileAction {
        title: "Create XML bean for literal reference…".into(),
        kind: Some("refactor".into()),
        data: Some(key),
        ..Default::default()
    }]
}
pub(crate) fn resolve(
    nav: &mut Navigator,
    doc: &Document,
    action: FileAction,
) -> Result<FileAction, Error> {
    let pending = nav
        .xml_creation
        .pending
        .remove(action.data.as_deref().ok_or(Error::Stale)?)
        .ok_or(Error::Stale)?;
    current(&pending, doc)?;
    let candidate = candidate(doc, pending.at).ok_or(Error::Stale)?;
    let names = xml_file_actions::names(nav, doc, COMMAND_BUDGET)?;
    let inventory = xml_file_actions::fingerprint(&names);
    if names.contains(&candidate.id) {
        return Err("This bean ID/alias already exists in the source inventory; no definition will be replaced.".into());
    }
    let class = host::input(
        "Explicit bean class spelling (qualified source name; resolution not checked)",
        None,
    )?
    .ok_or(Error::Cancelled)?;
    if !xml_file_actions::qualified(&class) || !class.contains('.') {
        return Err("Enter a literal qualified ASCII class name (for example com.acme.Worker); classpath/constructors are not inferred.".into());
    }
    let leading = if candidate.insert > 0 && !doc.text[..candidate.insert].ends_with('\n') {
        candidate.newline
    } else {
        ""
    };
    let added = format!(
        "{leading}{}<{}bean id=\"{}\" class=\"{}\"/>{}",
        candidate.indent, candidate.prefix, candidate.id, class, candidate.newline
    );
    let insert = TextEdit { range: range(&doc.text, candidate.insert, candidate.insert), text: added };
    let (_, changes) = crate::reviewed(&doc.text, std::slice::from_ref(&insert))?;
    let preview = format!(
        "Project: {}\nSource: {}\n\nAdd source bean ID {:?} for the selected literal reference, with the explicitly supplied class spelling {:?}. No existing declaration/file is replaced. Source inventory absence is not runtime absence; classpath, constructor, imports, profiles and effective bean activation are unverified.\n\nChanged lines of the resulting XML:\n{changes}",
        pending.root, doc.path, candidate.id, class
    );
    if !ui_kit::prompts::confirm_exact("Review XML bean creation", &preview)? {
        return Err(Error::Cancelled);
    }
    current(&pending, doc)?;
    if xml_file_actions::fingerprint(&xml_file_actions::names(nav, doc, std::time::Duration::ZERO)?) != inventory {
        return Err(Error::Stale);
    }
    Ok(FileAction {
        title: "Create XML bean for literal reference".into(),
        kind: Some("refactor".into()),
        edits: vec![(doc.path.clone(), vec![insert])],
        ..Default::default()
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{file_actions::FileActions, mock::MockHost};
    const PATH: &str = "/project/beans.xml";
    const BODY: &str = "<bean id=\"owner\" class=\"app.Owner\"><property name=\"worker\" ref=\"missingWorker\"/></bean>";
    fn text() -> String {
        format!(
            "<beans xmlns=\"{}\">\n    {}\n</beans>\n",
            xml_checks::BEANS_NS,
            BODY
        )
    }
    fn doc(text: &str) -> Document {
        Document {
            path: PATH.into(),
            language: "xml".into(),
            version: 1,
            text: text.into(),
        }
    }
    fn selection(doc: &Document) -> Range {
        let at = doc.text.find("missingWorker").unwrap();
        range(&doc.text, at, at)
    }
    fn action(nav: &mut Navigator, doc: &Document) -> FileAction {
        nav.file_actions(doc, selection(doc))
            .into_iter()
            .find(owns)
            .unwrap()
    }
    fn setup(text: &str) -> (MockHost, Document, Navigator) {
        let h = MockHost::install();
        h.add_file(PATH, text);
        (h, doc(text), Navigator::default())
    }
    fn resolve(h: &MockHost, nav: &mut Navigator, doc: &Document) -> FileAction {
        let ticket = action(nav, doc);
        h.answer_next_prompt(Some(&["app.Worker"]));
        h.answer_next_prompt(Some(&["ok"]));
        nav.resolve_file_action(doc, ticket).unwrap()
    }
    fn result(doc: &Document, action: &FileAction) -> String {
        assert!(
            action.operations.is_empty() && action.edits.len() == 1 && action.edits[0].0 == PATH
        );
        let edit = &action.edits[0].1[0];
        let at = offset(&doc.text, edit.range.start).unwrap();
        assert_eq!(edit.range.start, edit.range.end);
        let mut text = doc.text.clone();
        text.insert_str(at, &edit.text);
        text
    }
    #[test]
    fn registered_action_creates_declared_bean_without_replacing_reference_or_source() {
        let (h, d, mut nav) = setup(&text());
        let edit = resolve(&h, &mut nav, &d);
        let after = result(&d, &edit);
        assert!(
            after.contains("<bean id=\"missingWorker\" class=\"app.Worker\"/>")
                && after.contains(BODY)
        );
        assert!(xml_file_actions::generation_document(&doc(&after)));
        assert_eq!(
            host::project_of(PATH).unwrap().read_file(PATH).unwrap(),
            d.text
        );
        assert!(h.process_runs().is_empty() && h.clipboard().is_none());
        let model = model::parse(PATH, &after).unwrap();
        assert!(
            model
                .beans
                .iter()
                .any(|b| b.name == "missingWorker" && b.ty == "Worker")
        );
    }
    #[test]
    fn prefixed_foreign_default_namespace_crlf_and_unicode_are_preserved() {
        let xml = format!(
            "<b:beans xmlns:b=\"{}\" xmlns=\"urn:foreign\">\r\n  <!-- 😀 café -->\r\n  <b:bean id=\"owner\" class=\"app.Owner\"><b:constructor-arg ref=\"missingWorker\"/></b:bean>\r\n</b:beans>\r\n",
            xml_checks::BEANS_NS
        );
        let (h, d, mut nav) = setup(&xml);
        let edit = resolve(&h, &mut nav, &d);
        let after = result(&d, &edit);
        assert!(
            after.contains("<b:bean id=\"missingWorker\" class=\"app.Worker\"/>")
                && after.contains("😀 café")
        );
        assert!(!after.replace("\r\n", "").contains('\n'));
        assert!(xml_file_actions::generation_document(&doc(&after)));
        assert_eq!(after.replace(&edit.edits[0].1[0].text, ""), xml);
    }
    #[test]
    fn existing_source_id_alias_and_incomplete_inventory_refuse_without_overwrite() {
        for other in [
            format!(
                "<beans xmlns=\"{}\"><bean id=\"missingWorker\" class=\"app.Existing\"/></beans>",
                xml_checks::BEANS_NS
            ),
            format!(
                "<beans xmlns=\"{}\"><bean id=\"existing\" class=\"app.Existing\"/><alias name=\"existing\" alias=\"missingWorker\"/></beans>",
                xml_checks::BEANS_NS
            ),
        ] {
            let (h, d, mut nav) = setup(&text());
            h.add_file("other.xml", &other);
            let ticket = action(&mut nav, &d);
            assert!(nav.resolve_file_action(&d, ticket).is_err());
            assert_eq!(
                host::project_of(PATH).unwrap().read_file(PATH).unwrap(),
                d.text
            );
        }
        let (h, d, mut nav) = setup(&text());
        for i in 0..10000 {
            h.add_file(&format!("src/C{i}.java"), "");
        }
        let ticket = action(&mut nav, &d);
        assert!(
            matches!(nav.resolve_file_action(&d,ticket),Err(Error::Failed(message)) if message.contains("inventory incomplete"))
        );
    }
    #[test]
    fn malformed_dynamic_foreign_local_namespace_profile_and_value_holders_stay_quiet() {
        let valid = text();
        for xml in [
            valid.replace("missingWorker", "${dynamic}"),
            valid.replace("ref=", "p:ref="),
            valid.replace("<property ", "<property xmlns=\"urn:foreign\" "),
            valid.replace("ref=\"missingWorker\"", "value=\"x\" ref=\"missingWorker\""),
            valid.replace("<beans ", "<beans xmlns:xml=\"urn:invalid\" "),
            valid.replace("<property ", "<foreign:property "),
            valid
                .replace("    <bean", "    <beans profile=\"prod\"><bean")
                .replace("</bean>", "</bean></beans>"),
            format!("<!DOCTYPE beans [<!ENTITY x 'unsafe'>]>{valid}"),
            valid.replace("</beans>", "</wrong>"),
            valid.replace("</beans>", "</beans>\r"),
        ] {
            let (_h, mut d, mut nav) = setup(&xml);
            let at = xml.find("ref=").unwrap_or(0) + 5;
            d.text = xml.clone();
            assert!(
                nav.file_actions(&d, range(&xml, at, at)).is_empty(),
                "{xml}"
            );
        }
    }
    #[test]
    fn input_review_cancel_invalid_classes_and_replay_are_inert() {
        for confirm in [false, true] {
            let (h, d, mut nav) = setup(&text());
            let ticket = action(&mut nav, &d);
            if confirm {
                h.answer_next_prompt(Some(&["app.Worker"]));
            }
            h.answer_next_prompt(None);
            assert!(matches!(
                nav.resolve_file_action(&d, ticket.clone()),
                Err(Error::Cancelled)
            ));
            assert!(matches!(
                nav.resolve_file_action(&d, ticket),
                Err(Error::Stale)
            ));
        }
        for class in [
            "Worker",
            "app.Worker<X>",
            "app.Worker\"/>",
            "app..Worker",
            " app.Worker",
            "1app.Worker",
            "app.Worker;bad",
        ] {
            let (h, d, mut nav) = setup(&text());
            let ticket = action(&mut nav, &d);
            h.answer_next_prompt(Some(&[class]));
            assert!(nav.resolve_file_action(&d, ticket).is_err(), "{class}");
        }
    }
    #[test]
    fn source_version_content_owner_and_ticket_overflow_are_guarded() {
        let (h, d, mut nav) = setup(&text());
        let ticket = action(&mut nav, &d);
        let changed = Document {
            version: 2,
            ..d.clone()
        };
        assert!(matches!(
            nav.resolve_file_action(&changed, ticket),
            Err(Error::Stale)
        ));
        let ticket = action(&mut nav, &d);
        h.add_file(PATH, "changed");
        assert!(matches!(
            nav.resolve_file_action(&d, ticket),
            Err(Error::Stale)
        ));
        h.add_file(PATH, &d.text);
        let ticket = action(&mut nav, &d);
        h.set_project_root(Some("/other"));
        assert!(nav.resolve_file_action(&d, ticket).is_err());
        let (_h, d, mut nav) = setup(&text());
        let old = action(&mut nav, &d);
        for _ in 0..8 {
            action(&mut nav, &d);
        }
        assert!(matches!(
            nav.resolve_file_action(&d, old),
            Err(Error::Stale)
        ));
        assert!(matches!(
            nav.resolve_file_action(
                &d,
                FileAction {
                    data: Some("spring-xml-create:forged".into()),
                    ..Default::default()
                }
            ),
            Err(Error::Stale)
        ));
    }
    #[test]
    fn unsaved_current_source_is_authority_and_known_local_definitions_are_quiet() {
        let xml = text();
        let (h, mut d, mut nav) = setup(&xml);
        h.add_file(PATH, "old saved XML");
        h.open_editor(PATH, "xml", &xml, &[]);
        d.version = h.editor_version();
        let edit = resolve(&h, &mut nav, &d);
        assert!(result(&d, &edit).contains("id=\"missingWorker\""));
        assert_eq!(h.editor_text(), xml);
        let existing = xml.replace(
            "</beans>",
            "    <bean id=\"missingWorker\" class=\"app.Existing\"/></beans>",
        );
        let (_h, d, mut nav) = setup(&existing);
        assert!(nav.file_actions(&d, selection(&d)).is_empty());
        let _denied = MockHost::install().with_capabilities(&[]);
        let mut nav = Navigator::default();
        let d = doc(&xml);
        if let Some(ticket) = nav.file_actions(&d, selection(&d)).into_iter().find(owns) {
            assert!(nav.resolve_file_action(&d, ticket).is_err());
        }
    }
}
