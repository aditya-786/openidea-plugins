//! Current-buffer only, previewed transition creation. No inherited/runtime flow model.
use super::*;
use openidea_plugin::file_actions::{FileAction, FileActions};
use openidea_plugin::{TextEdit, text::offset};
const MAX_TEXT: usize = 128 << 10;
#[derive(Default)]
pub(crate) struct Actions {
    next: u64,
    pending: BTreeMap<String, Pending>,
}
#[derive(Clone)]
struct Pending {
    doc: Document,
    root: String,
    state: usize,
}
fn literal(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes().enumerate().all(|(i, b)| {
            b.is_ascii_alphabetic()
                || b == b'_'
                || i > 0 && (b.is_ascii_digit() || b".-".contains(&b))
        })
}
fn root(doc: &Document) -> Option<String> {
    let root = host::project_of(&doc.path)?.root().ok()?;
    if !root.starts_with('/')
        || root == "/"
        || root.ends_with('/')
        || root.len() > 4096
        || doc.path.len() > 4096
        || root.chars().chain(doc.path.chars()).any(char::is_control)
        || root.contains('\\')
        || doc.path.contains('\\')
        || root.contains("//")
        || doc.path.contains("//")
        || root
            .split('/')
            .chain(doc.path.split('/'))
            .any(|p| matches!(p, "." | ".."))
        || !doc.path.starts_with(&format!("{root}/"))
    {
        return None;
    }
    Some(root)
}
pub(crate) fn valid(text: &str) -> bool {
    use quick_xml::{events::Event, name::ResolveResult, reader::NsReader};
    if text.len() > MAX_TEXT {
        return false;
    }
    let mut r = NsReader::from_str(text);
    r.config_mut().check_comments = true;
    let (mut depth, mut roots, mut events) = (0usize, 0usize, 0usize);
    loop {
        events += 1;
        if events > 8192 || depth > 64 {
            return false;
        }
        let Ok((ns, event)) = r.read_resolved_event() else {
            return false;
        };
        if matches!(ns, ResolveResult::Unknown(_)) {
            return false;
        }
        match event {
            Event::Start(ref e) | Event::Empty(ref e) => {
                if depth == 0 {
                    roots += 1;
                    if e.local_name().as_ref() != "flow"
                        || !matches!(ns,ResolveResult::Bound(n) if n.0==NAMESPACE)
                    {
                        return false;
                    }
                }
                for a in e.attributes() {
                    let Ok(a) = a else {
                        return false;
                    };
                    let Ok(value) = a.normalized_value(quick_xml::XmlVersion::Implicit1_0) else {
                        return false;
                    };
                    let key: &str = a.key.as_ref();
                    const XML: &str = "http://www.w3.org/XML/1998/namespace";
                    const XMLNS: &str = "http://www.w3.org/2000/xmlns/";
                    if key == "xmlns" || key.starts_with("xmlns:") {
                        if key == "xmlns:xmlns"
                            || value == XMLNS
                            || (value == XML && key != "xmlns:xml")
                            || (key == "xmlns:xml" && value != XML)
                            || (key.starts_with("xmlns:") && value.is_empty())
                        {
                            return false;
                        }
                    } else if key.contains(':')
                        && matches!(
                            r.resolver().resolve_attribute(a.key),
                            (ResolveResult::Unknown(_), _)
                        )
                    {
                        return false;
                    }
                }
                if matches!(event, Event::Start(_)) {
                    depth += 1;
                }
            }
            Event::End(_) => {
                let Some(d) = depth.checked_sub(1) else {
                    return false;
                };
                depth = d;
            }
            Event::Text(t) if depth == 0 && !t.as_ref().trim().is_empty() => return false,
            Event::DocType(_) | Event::GeneralRef(_) => return false,
            Event::CData(_) if depth == 0 => return false,
            Event::Decl(_) if roots != 0 || depth != 0 => return false,
            Event::Eof => return roots == 1 && depth == 0,
            _ => {}
        }
    }
}
fn model(doc: &Document) -> Option<Vec<xml::Element>> {
    if doc.language != "xml"
        || !doc.path.ends_with(".xml")
        || !valid(&doc.text)
        || doc.text.contains('&')
    {
        return None;
    }
    let els = xml::elements(&doc.text);
    let first = els.first()?;
    if first.attr("parent").is_some() || first.value("abstract").is_some_and(|v| v != "false") {
        return None;
    }
    if els.iter().enumerate().any(|(i, e)| {
        xml::namespace(&els, i) != Some(NAMESPACE)
            || e.attr("parent").is_some()
            || e.attrs
                .iter()
                .any(|a| a.name.contains(':') && !a.name.starts_with("xmlns:"))
    }) {
        return None;
    }
    let mut ids = BTreeSet::new();
    for e in els.iter().filter(|e| e.parent == Some(0)) {
        if !STATES.contains(&e.local()) {
            return None;
        }
        let id = e.value("id")?;
        if !literal(id) || !ids.insert(id) {
            return None;
        }
    }
    if ids.is_empty() || ids.len() > 128 {
        return None;
    }
    Some(els)
}
fn candidate(doc: &Document, selection: Range) -> Option<usize> {
    let els = model(doc)?;
    let at = offset(&doc.text, selection.start)?;
    els.iter().enumerate().find_map(|(i, e)| {
        if e.parent != Some(0) || e.local() != "view-state" || !(e.start <= at && at < e.end) {
            return None;
        }
        let children: Vec<_> = els.iter().filter(|c| c.parent == Some(i)).collect();
        let mut events = BTreeSet::new();
        if children
            .iter()
            .any(|c| c.value("on").is_some_and(|on| !events.insert(on)))
        {
            return None;
        }
        if children.iter().any(|c| {
            c.local() != "transition"
                || !c.value("on").is_some_and(literal)
                || !c.value("to").is_some_and(|v| {
                    els.iter()
                        .any(|s| s.parent == Some(0) && s.value("id") == Some(v))
                })
                || c.attrs
                    .iter()
                    .any(|a| !matches!(a.name.as_str(), "on" | "to"))
                || c.end != c.tag_end
                || !doc.text[c.start..c.tag_end].ends_with("/>")
        }) {
            return None;
        }
        // Only XML whitespace and known empty transition tags may occur in the body.
        let mut cursor = e.tag_end;
        for c in children {
            if !doc.text[cursor..c.start].trim().is_empty() {
                return None;
            }
            cursor = c.end;
        }
        if e.end > e.tag_end {
            let close = doc.text[..e.end].rfind("</")?;
            if !doc.text[cursor..close].trim().is_empty() {
                return None;
            }
        }
        Some(i)
    })
}
fn unchanged(p: &Pending, doc: &Document) -> Result<(), Error> {
    if doc.path != p.doc.path
        || doc.language != p.doc.language
        || doc.version != p.doc.version
        || doc.text != p.doc.text
        || root(doc).as_deref() != Some(p.root.as_str())
    {
        return Err(Error::Stale);
    }
    let project = host::project_of(&doc.path).ok_or(Error::Stale)?;
    if project.read_file(&doc.path)? != doc.text {
        return Err(Error::Stale);
    }
    Ok(())
}
impl FileActions for WebFlow {
    fn file_actions(&mut self, doc: &Document, selection: Range) -> Vec<FileAction> {
        let Some(root) = root(doc) else {
            return vec![];
        };
        let Some(state) = candidate(doc, selection) else {
            return vec![];
        };
        if self.actions.pending.len() >= 8 {
            self.actions.pending.clear();
        }
        let Some(next) = self.actions.next.checked_add(1) else {
            return vec![];
        };
        self.actions.next = next;
        let key = format!("webflow-transition:{next}:{}", doc.version);
        self.actions.pending.insert(
            key.clone(),
            Pending {
                doc: doc.clone(),
                root,
                state,
            },
        );
        vec![FileAction {
            title: "Add Web Flow transition…".into(),
            kind: Some("refactor.rewrite".into()),
            data: Some(key),
            ..Default::default()
        }]
    }
    fn resolve_file_action(
        &mut self,
        doc: &Document,
        action: FileAction,
    ) -> Result<FileAction, Error> {
        let key = action.data.as_deref().ok_or(Error::Stale)?;
        let p = self.actions.pending.get(key).cloned().ok_or(Error::Stale)?;
        unchanged(&p, doc)?;
        let els = model(doc).ok_or(Error::Stale)?;
        let state = els.get(p.state).ok_or(Error::Stale)?;
        let select = range(&doc.text, state.start, state.start);
        if candidate(doc, select) != Some(p.state) {
            return Err(Error::Stale);
        }
        let choices: Vec<_> = els
            .iter()
            .filter(|e| e.parent == Some(0))
            .map(|e| openidea_plugin::PickItem::new(e.value("id").unwrap(), e.value("id").unwrap()))
            .collect();
        let to = host::pick("Transition target (current flow only)", &choices)?
            .ok_or(Error::Cancelled)?;
        unchanged(&p, doc)?;
        if !choices.iter().any(|c| c.key == to) {
            return Err(Error::Stale);
        }
        let event =
            host::input("Transition event (literal name)", None)?.ok_or(Error::Cancelled)?;
        unchanged(&p, doc)?;
        if !literal(&event)
            || els
                .iter()
                .any(|c| c.parent == Some(p.state) && c.value("on") == Some(event.as_str()))
        {
            return Err(Error::Invalid("Use a new literal event name; dynamic, wildcard and duplicate events are unsupported.".into()));
        }
        let prefix = state
            .name
            .rsplit_once(':')
            .map_or(String::new(), |(p, _)| format!("{p}:"));
        let eol = if doc.text.contains("\r\n") {
            "\r\n"
        } else {
            "\n"
        };
        let line = doc.text[..state.start].rfind('\n').map_or(0, |n| n + 1);
        let indent: String = doc.text[line..state.start]
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        let child = format!("{indent}  ");
        let transition = format!("<{prefix}transition on=\"{event}\" to=\"{to}\"/>");
        let (from, end, replacement) = if doc.text[state.start..state.tag_end].ends_with("/>") {
            (
                state.tag_end - 2,
                state.tag_end,
                format!(">{eol}{child}{transition}{eol}{indent}</{}>", state.name),
            )
        } else {
            let close = doc.text[..state.end].rfind("</").ok_or(Error::Stale)?;
            (close, close, format!("{child}{transition}{eol}{indent}"))
        };
        let mut after = doc.text.clone();
        after.replace_range(from..end, &replacement);
        if !valid(&after) {
            return Err(Error::Stale);
        }
        let preview = format!(
            "File: {}\nVersion: {}\nSource state: {}\nEvent: {}\nTarget: {}\n\nReplace exactly:\n{}\n\nWith:\n{}\n\nCurrent-file literal transition only. Inherited flow, external state, event binding, controller/view/runtime execution and reachability are not resolved.\n\nApply this undoable edit?",
            doc.path,
            doc.version,
            state.value("id").unwrap(),
            event,
            to,
            &doc.text[from..end],
            replacement
        );
        if !ui_kit::prompts::confirm_exact("Create local Web Flow transition?", &preview)? {
            return Err(Error::Cancelled);
        }
        unchanged(&p, doc)?;
        if candidate(doc, select) != Some(p.state) {
            return Err(Error::Stale);
        }
        self.actions.pending.remove(key);
        Ok(FileAction {
            title: "Add local Web Flow transition".into(),
            edits: vec![(
                doc.path.clone(),
                vec![TextEdit {
                    range: range(&doc.text, from, end),
                    text: replacement,
                }],
            )],
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{mock::MockHost, text::position};
    fn doc(body: &str) -> Document {
        Document {
            path: "/project/flow.xml".into(),
            language: "xml".into(),
            version: 7,
            text: format!(
                "<wf:flow xmlns:wf=\"{NAMESPACE}\">\r\n  {body}\r\n  <wf:end-state id=\"done\"/>\r\n</wf:flow>"
            ),
        }
    }
    fn selection(d: &Document) -> Range {
        let p = position(&d.text, d.text.find("view-state").unwrap());
        Range::new(p, p)
    }
    #[test]
    fn exact_transition_preview_expands_empty_state_and_preserves_utf16_crlf_source_without_write()
    {
        let h = MockHost::install();
        let d = doc("<wf:view-state id=\"edit\"/>");
        h.add_file("flow.xml", &d.text);
        let mut w = WebFlow::default();
        let a = w.file_actions(&d, selection(&d)).remove(0);
        assert!(a.edits.is_empty());
        let replay = a.clone();
        for answer in ["done", "submit", "yes"] {
            h.answer_next_prompt(Some(&[answer]));
        }
        let resolved = w.resolve_file_action(&d, a).unwrap();
        assert_eq!(resolved.edits.len(), 1);
        assert!(resolved.operations.is_empty());
        let e = &resolved.edits[0].1[0];
        let mut after = d.text.clone();
        after.replace_range(
            offset(&d.text, e.range.start).unwrap()..offset(&d.text, e.range.end).unwrap(),
            &e.text,
        );
        assert!(valid(&after));
        assert!(after.contains("<wf:transition on=\"submit\" to=\"done\"/>\r\n"));
        assert_eq!(
            host::project_of(&d.path)
                .unwrap()
                .read_file(&d.path)
                .unwrap(),
            d.text
        );
        assert!(w.resolve_file_action(&d, replay).is_err());
        assert!(h.process_runs().is_empty());
        assert_eq!(h.fetches(), 0);
    }
    #[test]
    fn cancelled_stale_foreign_forged_and_duplicate_transitions_never_return_edits() {
        for mode in 0..5 {
            let h = MockHost::install();
            let mut d = doc(
                "<wf:view-state id=\"edit\"><wf:transition on=\"submit\" to=\"done\"/></wf:view-state>",
            );
            h.add_file("flow.xml", &d.text);
            let mut w = WebFlow::default();
            let a = w.file_actions(&d, selection(&d)).remove(0);
            match mode {
                0 => h.answer_next_prompt(None),
                1 => {
                    d.version += 1;
                }
                2 => h.set_project_root(Some("/other")),
                3 => h.answer_next_prompt(Some(&["forged"])),
                _ => {
                    h.answer_next_prompt(Some(&["done"]));
                    h.answer_next_prompt(Some(&["submit"]));
                }
            }
            assert!(w.resolve_file_action(&d, a).is_err());
            assert!(h.process_runs().is_empty());
        }
    }
    #[test]
    fn unsaved_paired_state_and_unsupported_shapes_are_conservative() {
        let h = MockHost::install();
        let d = doc("<!-- 😀 --><wf:view-state id=\"edit\">\r\n  </wf:view-state>");
        h.add_file("flow.xml", "disk differs");
        h.open_editor(&d.path, "xml", &d.text, &[]);
        let mut w = WebFlow::default();
        let a = w.file_actions(&d, selection(&d)).remove(0);
        for answer in ["done", "next", "yes"] {
            h.answer_next_prompt(Some(&[answer]));
        }
        assert_eq!(w.resolve_file_action(&d, a).unwrap().edits.len(), 1);
        for body in [
            "<wf:view-state id=\"edit\" parent=\"parent#edit\"/>",
            "<wf:view-state id=\"edit\"><wf:on-entry/></wf:view-state>",
            "<wf:view-state id=\"edit\"><wf:transition on=\"*\" to=\"done\"/></wf:view-state>",
            "<wf:view-state id=\"edit\"/><wf:view-state id=\"edit\"/>",
        ] {
            let d = doc(body);
            assert!(w.file_actions(&d, selection(&d)).is_empty(), "{body}");
        }
    }
}

#[cfg(test)]
mod graph_tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    #[test]
    fn packaged_graph_has_external_style_no_inline_script_and_opens_current_declarations() {
        let h = MockHost::install();
        let text = format!(
            "<flow xmlns=\"{NAMESPACE}\"><view-state id=\"edit\"><transition on=\"submit\" to=\"done\"/></view-state><end-state id=\"done\"/></flow>"
        );
        h.add_file("flow.xml", &text);
        h.open_editor("/project/flow.xml", "xml", &text, &[]);
        let mut w = WebFlow::default();
        let view = w.render(VIEW, None, &h.invocation()).unwrap();
        let Body::Html(html) = view.body else {
            panic!("graph")
        };
        if cfg!(feature = "embedded") {
            assert!(html.contains("webflow-graph.css"));
            assert!(!html.contains("<script>"));
            assert!(!html.contains("<style>"));
            assert!(html.contains("webflow-edge:0"));
        }
        w.handle(VIEW, ViewEvent::Message("edit".into()), &h.invocation())
            .unwrap();
        w.handle(
            VIEW,
            ViewEvent::Message("webflow-edge:0".into()),
            &h.invocation(),
        )
        .unwrap();
        assert_eq!(h.opened_locations().len(), 2);
        assert!(
            w.handle(VIEW, ViewEvent::Message("forged".into()), &h.invocation())
                .is_err()
        );
        assert!(h.process_runs().is_empty());
        assert_eq!(h.fetches(), 0);
    }
}
