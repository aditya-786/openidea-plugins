//! Deliberate physical-source Batch transition linking; no job execution.
use crate::*;
use crate::batch_navigation::{LinkContext, action_context};
use openidea_plugin::file_actions::FileAction;
#[derive(Clone)]
struct Pending {
    root: String,
    doc: Document,
    at: Position,
    context: LinkContext,
}
#[derive(Default)]
pub(crate) struct Links {
    next: u64,
    pending: BTreeMap<String, Pending>,
}
use crate::batch_step_rename::normalized;
fn current(pending: &Pending, doc: &Document) -> Result<(), Error> {
    if *doc != pending.doc { return Err(Error::Stale); }
    let project = host::project_of(&doc.path).ok_or(Error::Stale)?;
    if project.root()? != pending.root || project.read_file(&doc.path)? != doc.text
        || action_context(doc, pending.at).as_ref() != Some(&pending.context) { return Err(Error::Stale); }
    Ok(())
}
impl Links {
    pub(crate) fn actions(&mut self, doc: &Document, selection: Range) -> Vec<FileAction> {
        let Some(context) = action_context(doc, selection.start) else { return vec![]; };
        let Some(root) = host::project_of(&doc.path).and_then(|p| p.root().ok()) else { return vec![]; };
        if !normalized(&root) || !normalized(&doc.path) || !doc.path.starts_with(&format!("{root}/")) { return vec![]; }
        let Some(next) = self.next.checked_add(1) else { return vec![]; };
        self.next = next;
        if self.pending.len() >= 4 { self.pending.clear(); }
        let token = format!("batch-transition:{next}:{}", doc.version);
        self.pending.insert(token.clone(), Pending { root, doc: doc.clone(), at: selection.start, context });
        vec![FileAction { title: "Link Batch step next to a local state…".into(), kind: Some("refactor".into()), data: Some(token), ..Default::default() }]
    }
    pub(crate) fn resolve(&mut self, doc: &Document, action: FileAction) -> Result<FileAction, Error> {
        let pending = self.pending.remove(action.data.as_deref().ok_or(Error::Stale)?).ok_or(Error::Stale)?;
        if *doc!=pending.doc {return Err(Error::Stale);}
        // The full BEFORE source is mandatory in the concrete review. If it
        // alone cannot fit, refuse before prompts or budgeted reparsing; a
        // scheduling timeout must not obscure this deterministic size limit.
        if doc.text.len()>ui_kit::prompts::MAX_CONFIRM_BYTES {
            return Err(Error::Invalid("The complete preview exceeds the host's confirmation limit; reduce the operation before applying it.".into()));
        }
        current(&pending, doc)?;
        let items: Vec<_> = pending.context.candidates.iter().map(|name| openidea_plugin::PickItem::new(name, name).description("Unique direct state in the same physical source job/flow")).collect();
        let Some(selected) = host::pick("Batch step next: choose another local state", &items)? else { return Err(Error::Cancelled); };
        if !pending.context.candidates.contains(&selected) { return Err(Error::Stale); }
        current(&pending, doc)?;
        let start = openidea_plugin::text::offset(&doc.text, pending.context.range.start).ok_or(Error::Stale)?;
        let end = openidea_plugin::text::offset(&doc.text, pending.context.range.end).ok_or(Error::Stale)?;
        let replacement = if pending.context.insert { format!(" next='{selected}'") } else { selected.clone() };
        let mut after = doc.text.clone(); after.replace_range(start..end, &replacement);
        let preview = format!("Source: {}\nVersion: {}\nPhysical scope: {}\nStep: {}\nExisting next: {}\nChosen next: {selected}\n\nAdd or replace only this explicit step next attribute. No job execution, inherited/imported ownership, step type validation or reachability/cycle analysis is performed. Existing step/tasklet content remains unchanged.\n\nExact resulting XML ({} bytes):\n{after}", doc.path, doc.version, pending.context.scope, pending.context.owner, if pending.context.insert { "(absent)" } else { &pending.context.value }, after.len());
        if !ui_kit::prompts::confirm_exact("Link Batch step transition?", &preview)? { return Err(Error::Cancelled); }
        current(&pending, doc)?;
        Ok(FileAction { title: format!("Link Batch step {} to {selected}", pending.context.owner), kind: Some("refactor".into()), edits: vec![(doc.path.clone(), vec![openidea_plugin::TextEdit { range: pending.context.range, text: replacement }])], ..Default::default() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{file_actions::FileActions, mock::MockHost};
    const NS: &str = "http://www.springframework.org/schema/batch";
    fn doc(body: &str) -> Document {
        Document { path: "/project/jobs.xml".into(), language: "xml".into(), version: 8, text: format!("<beans xmlns='http://www.springframework.org/schema/beans' xmlns:b='{NS}'>{body}</beans>") }
    }
    fn at(d: &Document) -> Range {
        let position = openidea_plugin::text::position(&d.text, d.text.find("next='").unwrap() + 6);
        Range::new(position, position)
    }
    fn action(owner: &mut impl FileActions, d: &Document) -> FileAction {
        owner.file_actions(d, at(d)).into_iter().find(|a| a.title.starts_with("Link Batch step")).expect("Batch link")
    }
    fn step_opening(d: &Document) -> Range {
        let p = openidea_plugin::text::position(&d.text, d.text.find("id='a'").unwrap() + 4);
        Range::new(p,p)
    }
    fn insertion_action(owner: &mut impl FileActions, d: &Document) -> FileAction {
        owner.file_actions(d, step_opening(d)).into_iter().find(|a| a.title.starts_with("Link Batch step")).expect("missing next link")
    }
    #[test]
    fn registered_missing_next_link_inserts_one_attribute_without_rewriting_step_content() {
        for step in ["<b:step id='a'/>", "<b:step id='a'><b:tasklet ref='worker'/></b:step>", "<b:step id='a'  />"] {
            let d=doc(&format!("<!--😀 café-->\r\n<b:job id='j'>{step}<b:step id='b'/></b:job>\r\n"));
            let h=MockHost::install();h.add_file(&d.path,&d.text);let mut owner=structure::Structure::default();
            let lazy=insertion_action(&mut owner,&d);h.answer_next_prompt(Some(&["b"]));h.answer_next_prompt(Some(&["yes"]));
            let resolved=owner.resolve_file_action(&d,lazy).unwrap();assert!(resolved.operations.is_empty());
            let [(path,edits)]=resolved.edits.as_slice() else{panic!("one source")};let [edit]=edits.as_slice() else{panic!("one insert")};
            assert_eq!(path,&d.path);assert_eq!(edit.range.start,edit.range.end);assert_eq!(edit.text," next='b'");
            let offset=openidea_plugin::text::offset(&d.text,edit.range.start).unwrap();let mut after=d.text.clone();after.insert_str(offset,&edit.text);
            let closing=if step.ends_with("/>"){step.len()-2}else{step.find('>').unwrap()};let mut expected=step.to_owned();expected.insert_str(closing," next='b'");
            assert_eq!(after,d.text.replace(step,&expected));
            assert_eq!(h.invocation().project().unwrap().read_file(&d.path).unwrap(),d.text);
        }
    }
    #[test]
    fn absent_next_link_rejects_existing_or_conditional_attributes_and_unsupported_owners() {
        let h=MockHost::install();let mut owner=structure::Structure::default();
        for step in [
            "<b:step id='a' next='b'/>",
            "<b:step id='a' b:next='b'/>",
            "<b:step id='a'><b:next on='*' to='b'/></b:step>",
            "<b:step id='a' parent='template'/>",
            "<b:step id='a' abstract='true'/>",
        ] {
            let d=doc(&format!("<b:job id='j'>{step}<b:step id='b'/></b:job>"));h.add_file(&d.path,&d.text);
            assert!(!owner.file_actions(&d,step_opening(&d)).iter().any(|a|a.title.starts_with("Link Batch step")),"{step}");
        }
        let d=doc("<b:job id='j'><b:step id='a'><b:tasklet ref='worker'/></b:step><b:step id='b'/></b:job>");
        let p=openidea_plugin::text::position(&d.text,d.text.find("worker").unwrap());
        assert!(!owner.file_actions(&d,Range::new(p,p)).iter().any(|a|a.title.starts_with("Link Batch step")),"only opening tag activates insertion");
    }
    #[test]
    fn missing_next_context_keeps_original_source_ranges_and_is_stale_after_source_changes() {
        let d=doc("<b:job id='j'><b:step id='a'/><b:step id='b'/></b:job>");let h=MockHost::install();h.add_file(&d.path,&d.text);let mut owner=structure::Structure::default();
        let lazy=insertion_action(&mut owner,&d);
        let changed=Document{text:d.text.replace("<b:step id='a'/>","<b:step id='a' next='b'/>"),..d.clone()};
        assert!(matches!(owner.resolve_file_action(&changed,lazy),Err(Error::Stale)));
        assert_eq!(h.invocation().project().unwrap().read_file(&d.path).unwrap(),d.text);
        let bad=Document{text:d.text.replace("</b:job>",""),..d.clone()};assert!(action_context(&bad,step_opening(&bad).start).is_none());
        let large=Document{text:format!("<!--{}-->{}","x".repeat(33<<10),d.text),..d};assert!(action_context(&large,step_opening(&large).start).is_none());
    }
    #[test]
    fn registered_batch_link_is_one_exact_utf16_current_source_edit_with_full_preview() {
        let mut d = doc("\r\n<!-- 😀 café -->\r\n<b:job id='j'><b:step id='first' next='missing'><b:tasklet ref='worker'/></b:step><b:step id='finish'/></b:job>\r\n");
        d.text = d.text.replace("<b:job", "\r\n<b:job");
        let h = MockHost::install(); h.add_file(&d.path, "old saved source"); h.open_editor(&d.path, "xml", &d.text, &[]);
        let mut owner = structure::Structure::default(); let lazy = action(&mut owner, &d);
        assert!(lazy.edits.is_empty() && lazy.operations.is_empty());
        h.answer_next_prompt(Some(&["finish"])); h.answer_next_prompt(Some(&["yes"]));
        let resolved = owner.resolve_file_action(&d, lazy).unwrap();
        assert!(resolved.operations.is_empty());
        let [(path, edits)] = resolved.edits.as_slice() else { panic!("one source"); };
        let [edit] = edits.as_slice() else { panic!("one range"); };
        assert_eq!(path, &d.path); assert_eq!(edit.text, "finish");
        let start = openidea_plugin::text::offset(&d.text, edit.range.start).unwrap();
        let end = openidea_plugin::text::offset(&d.text, edit.range.end).unwrap();
        assert_eq!(&d.text[start..end], "missing");
        let mut after = d.text.clone(); after.replace_range(start..end, &edit.text);
        assert_eq!(after, d.text.replace("next='missing'", "next='finish'"));
        assert!(after.contains("<b:tasklet ref='worker'/>") && after.contains("😀 café"));
        assert_eq!(h.editor_text(), d.text); assert!(h.requests().is_empty() && h.process_runs().is_empty());
    }
    #[test]
    fn nested_job_flow_state_picker_rejects_self_other_scope_and_inherited_targets() {
        let d = doc("<b:job id='j'><b:flow id='nested'><b:step id='first' next=''/><b:step id='inside'/></b:flow><b:step id='outside'/></b:job><b:job id='other'><b:step id='elsewhere'/></b:job>");
        for choice in ["first", "outside", "elsewhere", "forged"] {
            let h = MockHost::install(); h.add_file(&d.path, &d.text); let mut owner = structure::Structure::default();
            let lazy = action(&mut owner, &d); h.answer_next_prompt(Some(&[choice]));
            assert!(owner.resolve_file_action(&d, lazy).is_err(), "{choice}");
        }
        let h = MockHost::install(); h.add_file(&d.path, &d.text); let mut owner = structure::Structure::default();
        let lazy = action(&mut owner, &d); h.answer_next_prompt(Some(&["inside"])); h.answer_next_prompt(Some(&["yes"]));
        assert_eq!(owner.resolve_file_action(&d, lazy).unwrap().edits[0].1[0].text, "inside");
    }
    #[test]
    fn duplicate_inherited_conditional_qualified_dynamic_and_malformed_contexts_stay_quiet() {
        let h = MockHost::install(); let mut owner = structure::Structure::default();
        for body in [
            "<b:job id='j'><b:step id='a' next=''/><b:step id='a'/><b:step id='b'/></b:job>",
            "<b:job id='j'><b:step id='a' next=''/><b:step id='b'/><b:step id='b'/></b:job>",
            "<b:job id='j'><b:step id='a' next='' parent='base'/><b:step id='b'/></b:job>",
            "<b:job id='j' parent='base'><b:step id='a' next=''/><b:step id='b'/></b:job>",
            "<b:job id='j'><b:step id='a' next=''/><b:step id='b' parent='base'/></b:job>",
            "<b:job id='j'><b:step id='a' next=''/><b:step id='b' parent='base'/><f:step xmlns:f='urn:other' id='b'/></b:job>",
            "<b:job id='j'><b:step id='a' next=''><b:next on='FAILED' to='b'/></b:step><b:step id='b'/></b:job>",
            "<b:job id='j'><b:step id='a' b:next=''/><b:step id='b'/></b:job>",
            "<b:job id='j'><b:step id='a' next='${dynamic}'/><b:step id='b'/></b:job>",
            "<?xml version='1.0'?><b:job id='j'><b:step id='a' next=''/><b:step id='b'/></b:job>",
            "<b:job id='j'><b:step id='a' next=''/><b:step id='b'/>no close",
        ] {
            let d = doc(body); h.add_file(&d.path, &d.text);
            assert!(!owner.file_actions(&d, at(&d)).iter().any(|a| a.title.starts_with("Link Batch step")), "{body}");
        }
        let d = doc("<b:job id='j'><b:step id='a' next=''/><b:step id='b'/></b:job>");
        let duplicate_decl = Document { text: format!("<?xml version='1.0'?><?xml version='1.0'?>{}", d.text), ..d.clone() };
        assert!(owner.file_actions(&duplicate_decl, at(&duplicate_decl)).is_empty());
        for prefix in [" ", "<!--before-->"] {
            let misplaced = Document { text: format!("{prefix}<?xml version='1.0'?>{}", d.text), ..d.clone() };
            assert!(owner.file_actions(&misplaced, at(&misplaced)).is_empty());
        }
        let legal_decl = Document { text: format!("<?xml version='1.0'?>{}", d.text), ..d.clone() };
        assert!(owner.file_actions(&legal_decl, at(&legal_decl)).iter().any(|a| a.title.starts_with("Link Batch step")));
        let foreign = Document { text: d.text.replace(NS,"urn:foreign"), ..d.clone() };
        assert!(owner.file_actions(&foreign, at(&foreign)).is_empty());
        let oversized = Document { text: format!("<!--{}-->{}","x".repeat(33<<10),d.text), ..d };
        assert!(owner.file_actions(&oversized, at(&oversized)).is_empty());
    }
    #[test]
    fn batch_link_cancel_forged_and_replayed_intents_never_mutate_source() {
        for accepted in 0..2 {
            let d = doc("<b:job id='j'><b:step id='a' next=''/><b:step id='b'/></b:job>");
            let h = MockHost::install(); h.add_file(&d.path, &d.text); let mut owner = structure::Structure::default();
            let lazy = action(&mut owner, &d); let replay = lazy.clone();
            if accepted == 1 { h.answer_next_prompt(Some(&["b"])); }
            assert!(matches!(owner.resolve_file_action(&d,lazy),Err(Error::Cancelled)));
            assert!(matches!(owner.resolve_file_action(&d,replay),Err(Error::Stale)));
            assert!(owner.resolve_file_action(&d,FileAction { data:Some("batch-transition:forged".into()),..Default::default() }).is_err());
            assert_eq!(h.invocation().project().unwrap().read_file(&d.path).unwrap(),d.text);
        }
    }
    #[test]
    fn batch_link_source_version_disk_owner_and_denied_contexts_refuse_all_edits() {
        let d = doc("<b:job id='j'><b:step id='a' next=''/><b:step id='b'/></b:job>");
        for change in 0..3 {
            let h = MockHost::install(); h.add_file(&d.path,&d.text); let mut owner=structure::Structure::default(); let lazy=action(&mut owner,&d);
            match change {
                0 => assert!(matches!(owner.resolve_file_action(&Document { version:d.version+1,..d.clone() },lazy),Err(Error::Stale))),
                1 => { h.add_file(&d.path,"changed"); assert!(matches!(owner.resolve_file_action(&d,lazy),Err(Error::Stale))); },
                _ => { h.set_project_root(Some("/other")); assert!(owner.resolve_file_action(&d,lazy).is_err()); },
            }
        }
        let _h=MockHost::install().with_capabilities(&[]);
        assert!(structure::Structure::default().file_actions(&d,at(&d)).is_empty());
    }
    #[test]
    fn exact_batch_link_preview_refuses_document_larger_than_native_confirmation_limit() {
        let d = doc(&format!("<!--{}--><b:job id='j'><b:step id='a' next=''/><b:step id='b'/></b:job>","x".repeat(20<<10)));
        let h=MockHost::install();h.add_file(&d.path,&d.text);let mut owner=structure::Structure::default();let lazy=action(&mut owner,&d);
        h.answer_next_prompt(Some(&["b"]));h.answer_next_prompt(Some(&["yes"]));
        let result=owner.resolve_file_action(&d,lazy);
        assert!(matches!(&result,Err(Error::Invalid(message)) if message.contains("preview")),"{result:?}");
        assert_eq!(host::pick("Choice still pending",&[openidea_plugin::PickItem::new("b","b")]).unwrap(),Some("b".into()));
        assert!(host::confirm("Still pending", "Oversized review did not consume confirmation").unwrap());
        assert_eq!(h.invocation().project().unwrap().read_file(&d.path).unwrap(),d.text);
    }
}
