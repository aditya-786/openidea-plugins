//! Explicit flat, single-job physical-source step/decision rename. No indexed Rename.
use crate::*;
use openidea_plugin::file_actions::FileAction;
use openidea_plugin::text::{offset, position};
use std::time::{Duration, Instant};
const NS: &str = "http://www.springframework.org/schema/batch";
const BEANS: &str = "http://www.springframework.org/schema/beans";
const TITLE: &str = "Rename Batch step in this document…";
#[derive(Clone, Debug, PartialEq, Eq)]
struct Site { from: usize, to: usize, role: String }
#[derive(Clone, Debug, PartialEq, Eq)]
struct Context {
    job: String,
    old: String,
    kind: String,
    declaration: Site,
    references: Vec<Site>,
    states: Vec<(String, String)>,
    occupied: BTreeSet<String>,
}
use spring_text::provenance::literal;
fn descendant(elements: &[xml::Element], mut at: usize, owner: usize) -> bool {
    while let Some(parent) = elements[at].parent {
        if parent == owner { return true; }
        at = parent;
    }
    false
}
fn context(doc: &Document, selection: Range) -> Option<Context> {
    if selection.start != selection.end || doc.language != "xml" || !doc.path.ends_with(".xml")
        || beans::ignored(&doc.path) || doc.text.len() > 32 * 1024 || doc.text.contains('&')
        || !spring_text::xml::generation_valid(&doc.text) { return None; }
    let deadline = Instant::now() + Duration::from_millis(10);
    let mut reader = quick_xml::Reader::from_str(&doc.text);
    let mut complete = false;
    for _ in 0..4096 {
        if Instant::now() >= deadline { return None; }
        match reader.read_event() {
            Ok(quick_xml::events::Event::Eof) => { complete = true; break; }
            Err(_) => return None,
            _ => {}
        }
    }
    if !complete { return None; }
    let elements = xml::elements(&doc.text);
    if elements.len() > 512 || elements.iter().map(|e| e.attrs.len()).sum::<usize>() > 256 { return None; }
    let jobs: Vec<_> = elements.iter().enumerate().filter(|(i,e)| e.local() == "job" && xml::namespace(&elements,*i) == Some(NS)).collect();
    let [(job_index,job)] = jobs.as_slice() else { return None; };
    let job_index = *job_index;
    if job.parent.is_some_and(|parent| elements[parent].parent.is_some() || elements[parent].local() != "beans" || xml::namespace(&elements,parent) != Some(BEANS)) { return None; }
    if job.attrs.iter().any(|a| matches!(a.local(),"parent"|"abstract"|"ref"|"merge"|"next"|"to"|"restart") || a.local()=="id" && a.name!="id") { return None; }
    let job_id = job.attrs.iter().find(|a| a.name == "id" && literal(&a.value))?.value.clone();
    let caret = offset(&doc.text, selection.start)?;
    let mut states = Vec::new();
    let mut counts = BTreeMap::<String,usize>::new();
    let mut selected = None;
    let mut occupied = BTreeSet::new();
    for (i, element) in elements.iter().enumerate() {
        if Instant::now() >= deadline { return None; }
        for attr in &element.attrs {
            if matches!(attr.local(),"id"|"name"|"alias") {
                for name in attr.value.split(|c:char| c.is_whitespace() || matches!(c,','|';')) { occupied.insert(name.to_owned()); }
            }
        }
        if !descendant(&elements,i,job_index) { continue; }
        if xml::namespace(&elements,i) != Some(NS) { return None; }
        if matches!(element.local(),"job"|"flow"|"split") { return None; }
        if matches!(element.local(),"step"|"decision") {
            if element.parent != Some(job_index) || element.attrs.iter().any(|a| matches!(a.local(),"parent"|"abstract"|"ref"|"merge") || a.local()=="id" && a.name!="id") { return None; }
            let id = element.attrs.iter().find(|a| a.name=="id" && literal(&a.value))?;
            *counts.entry(id.value.clone()).or_default() += 1;
            states.push((element.local().to_owned(),id.value.clone()));
            if id.holds(caret) {
                selected=Some((element.local().to_owned(),id.value.clone(),Site { from:id.value_at,to:id.value_end,role:format!("{} declaration id",element.local()) }));
            }
        }
    }
    if counts.values().any(|count|*count!=1) { return None; }
    let (kind,old,declaration)=selected?;
    // Refuse another physical ID outside this direct-state identity too.
    if elements.iter().flat_map(|e|e.attrs.iter()).filter(|a|a.local()=="id" && a.value==old).count()!=1 {return None;}
    let mut references=Vec::new();
    for (i,element) in elements.iter().enumerate().filter(|(i,_)|descendant(&elements,*i,job_index)) {
        if Instant::now() >= deadline { return None; }
        for attr in element.attrs.iter().filter(|a|matches!(a.local(),"next"|"to"|"restart")) {
            if attr.name!=attr.local() || !literal(&attr.value) { return None; }
            let direct = element.parent==Some(job_index) && matches!(element.local(),"step"|"decision");
            let transition = element.parent.is_some_and(|parent| elements[parent].parent==Some(job_index) && matches!(elements[parent].local(),"step"|"decision"));
            let supported = attr.name=="next" && direct || attr.name=="to" && element.local()=="next" && transition || attr.name=="restart" && element.local()=="stop" && transition;
            if !supported || xml::namespace(&elements,i)!=Some(NS) { return None; }
            occupied.insert(attr.value.clone());
            if attr.value==old { references.push(Site {from:attr.value_at,to:attr.value_end,role:format!("{} @{}",element.local(),attr.name)}); }
        }
    }
    if references.len()+1>64 { return None; }
    Some(Context {job:job_id,old,kind,declaration,references,states,occupied})
}
pub(crate) fn normalized(path:&str)->bool {
    path.starts_with('/') && path.len()<=512 && path!="/" && !path.contains(['\\','*','?','[',']','{','}'])
        && !path.chars().any(char::is_control) && path.split('/').skip(1).all(|p|!matches!(p,""|"."|".."))
}
/// Reuse the rename inventory from either its declaration or one of its proved
/// references. This model is pure current text, not a host reference graph.
fn usage_context(doc:&Document,selection:Range)->Option<Context> {
    if let Some(found)=context(doc,selection) {return Some(found);}
    if selection.start!=selection.end || doc.language!="xml" || doc.text.len()>32*1024 {return None;}
    let caret=offset(&doc.text,selection.start)?;let elements=xml::elements(&doc.text);
    let value=elements.iter().flat_map(|e|e.attrs.iter()).find(|a|a.holds(caret) && matches!(a.name.as_str(),"next"|"to"|"restart"))?.value.as_str();
    for element in elements.iter().filter(|e|matches!(e.local(),"step"|"decision")) {
        if let Some(id)=element.attrs.iter().find(|a|a.name=="id" && a.value==value) {
            let p=position(&doc.text,id.value_at);let found=context(doc,Range::new(p,p))?;
            if found.references.iter().any(|site|site.from<=caret && caret<=site.to) {return Some(found);}
        }
    }
    None
}
fn focused_source(ctx:&Invocation)->Result<(String,Document,Range),Error> {
    let root=ctx.project().ok_or(Error::Stale)?.root()?;
    let editor=ctx.editor().ok_or("Open a supported XML job and grant optional editor.read to find this document's step references.")?;
    let source=editor.document()?;let selections=editor.selections()?;
    let [selection]=selections.as_slice() else{return Err(Error::Invalid("Use one collapsed caret on a step ID or a proved direct target reference.".into()));};
    let doc=Document {path:editor.path(),language:editor.language(),text:source.text,version:source.version};
    if !normalized(&root) || !normalized(&doc.path) || !doc.path.starts_with(&format!("{root}/"))
        || spring_text::index::owning_root(&doc.path).is_none()
        || host::project_of(&doc.path).and_then(|p|p.root().ok()).as_deref()!=Some(root.as_str()) {return Err(Error::Stale);}
    Ok((root,doc,*selection))
}
fn usage_items(doc:&Document,model:&Context,sites:&[Site])->Vec<openidea_plugin::PickItem> {
    sites.iter().enumerate().map(|(index,site)| {
        let r=range(&doc.text,site.from,site.to);
        openidea_plugin::PickItem::new(&format!("site:{index}"),&format!("{} · {}:{}–{}:{}",site.role,r.start.line+1,r.start.character+1,r.end.line+1,r.end.character+1))
            .description(&format!("{} · UTF-16 range · source version {} · physical job {} · {} {}",doc.path,doc.version,model.job,model.kind,model.old))
    }).collect()
}
fn unchanged_usage_source(ctx:&Invocation,root:&str,doc:&Document,selection:Range,model:&Context)->Result<(),Error> {
    let (current_root,current,_)=focused_source(ctx)?;
    if current_root!=root || current!=*doc || usage_context(&current,selection).as_ref()!=Some(model) {return Err(Error::Stale);}
    Ok(())
}
/// Explicit read-only current-document usage UI. No source discovery or reads
/// from Providers; only this user command reads its focused editor snapshot.
pub(crate) fn usages(ctx:&Invocation)->Result<Option<String>,Error> {
    let (root,doc,selection)=focused_source(ctx)?;
    let model=usage_context(&doc,selection).ok_or("Place one caret on a unique direct step/decision ID or its proved next/to/restart reference in a supported flat single-job XML document.")?;
    let mut sites=vec![model.declaration.clone()];sites.extend(model.references.clone());sites.sort_by_key(|site|site.from);
    let items=usage_items(&doc,&model,&sites);
    let Some(choice)=host::pick(&format!("This document only: {} {} · job {} · {} reference(s) + declaration",model.kind,model.old,model.job,model.references.len()),&items)? else{return Ok(None);};
    let site=choice.strip_prefix("site:").and_then(|id|id.parse::<usize>().ok()).and_then(|index|sites.get(index)).ok_or(Error::Stale)?;
    unchanged_usage_source(ctx,&root,&doc,selection,&model)?;
    let destination=range(&doc.text,site.from,site.to).start;
    host::open_location(&doc.path,destination.line,destination.character)?;
    Ok(Some("Navigated to a proved source-local Batch state occurrence in this document. Other documents, bean references, job/channel/module usages and imported/inherited/runtime contexts are excluded; no global indexed Find Usages/Rename graph is published.".into()))
}
#[derive(Clone)]
struct Pending { root:String, doc:Document, selection:Range, context:Context }
fn current(pending:&Pending,doc:&Document)->Result<(),Error> {
    if *doc!=pending.doc || spring_text::index::owning_root(&doc.path).is_none() {return Err(Error::Stale);}
    let project=host::project_of(&doc.path).ok_or(Error::Stale)?;
    if project.root()?!=pending.root || project.read_file(&doc.path)?!=doc.text || usage_context(doc,pending.selection).as_ref()!=Some(&pending.context) {return Err(Error::Stale);}
    Ok(())
}
#[derive(Default)]
pub(crate) struct Rename { next:u64, pending:BTreeMap<String,Pending> }
impl Rename {
    pub(crate) fn actions(&mut self,doc:&Document,selection:Range)->Vec<FileAction> {
        let Some(context)=usage_context(doc,selection) else{return vec![];};
        if spring_text::index::owning_root(&doc.path).is_none() {return vec![];}
        let Some(root)=host::project_of(&doc.path).and_then(|p|p.root().ok()) else{return vec![];};
        if !normalized(&root) || !normalized(&doc.path) || !doc.path.starts_with(&format!("{root}/")) {return vec![];}
        let Some(next)=self.next.checked_add(1) else{return vec![];};self.next=next;
        if self.pending.len()>=4 {self.pending.clear();}
        let token=format!("batch-step-rename:{next}:{}",doc.version);
        let title=if context.kind=="decision" {"Rename Batch decision in this document…"} else {TITLE};
        self.pending.insert(token.clone(),Pending {root,doc:doc.clone(),selection,context});
        vec![FileAction {title:title.into(),kind:Some("refactor".into()),data:Some(token),..Default::default()}]
    }
    pub(crate) fn resolve(&mut self,doc:&Document,action:FileAction)->Result<FileAction,Error> {
        let pending=self.pending.remove(action.data.as_deref().ok_or(Error::Stale)?).ok_or(Error::Stale)?;
        current(&pending,doc)?;
        let Some(new)=host::input(&format!("New literal {} ID in this document",pending.context.kind),Some(&pending.context.old))? else{return Err(Error::Cancelled);};
        if !literal(&new) || new==pending.context.old || pending.context.occupied.contains(&new) {return Err(Error::Invalid("Use a different unique ASCII step ID (1–128 bytes; letter/underscore first, then letters/digits/_.-). Existing IDs, names, aliases and target literals cannot be reused.".into()));}
        current(&pending,doc)?;
        let mut sites=vec![pending.context.declaration.clone()];sites.extend(pending.context.references.clone());sites.sort_by_key(|site|site.from);
        if sites.windows(2).any(|pair|pair[0].to>pair[1].from) {return Err(Error::Stale);}
        let mut after=doc.text.clone();
        let mut delta=String::new();
        for site in &sites {
            if doc.text.get(site.from..site.to)!=Some(pending.context.old.as_str()) {return Err(Error::Stale);}
            delta.push_str(&format!("Line {}: {}: {} → {}\n",position(&doc.text,site.from).line+1,site.role,pending.context.old,new));
        }
        for site in sites.iter().rev() {after.replace_range(site.from..site.to,&new);}
        let shift:isize=sites.iter().filter(|site|site.from<pending.context.declaration.from).map(|site|new.len() as isize-(site.to-site.from) as isize).sum();
        let at=pending.context.declaration.from.checked_add_signed(shift).ok_or(Error::Stale)?;
        let p=position(&after,at);let changed=Document {text:after.clone(),..doc.clone()};
        let rebuilt=context(&changed,Range::new(p,p)).ok_or(Error::Stale)?;
        let expected:Vec<_>=pending.context.states.iter().map(|(kind,name)|(kind.clone(),if *name==pending.context.old {new.clone()}else{name.clone()})).collect();
        if rebuilt.old!=new || rebuilt.job!=pending.context.job || rebuilt.states!=expected || rebuilt.references.len()!=pending.context.references.len() {return Err(Error::Stale);}
        let review=format!("Source: {}\nVersion: {}\nPhysical single job: {}\nState: {} → {}\nExact edits: {} (one declaration, {} direct transition references)\n\n{delta}\nScope: only this document's flat physical job and direct step/decision next/to/restart literals. Other files, bean references, Java/Kotlin, imported/inherited/runtime contexts are excluded. No global Rename, execution, reachability or type validation.\n\nComplete BEFORE XML ({} bytes):\n{}\n\nComplete AFTER XML ({} bytes):\n{after}",doc.path,doc.version,pending.context.job,pending.context.old,new,sites.len(),pending.context.references.len(),doc.text.len(),doc.text,after.len());
        if !ui_kit::prompts::confirm_exact(&format!("Rename Batch {} in this document?",pending.context.kind),&review)? {return Err(Error::Cancelled);}
        current(&pending,doc)?;
        let edits=sites.into_iter().map(|site|openidea_plugin::TextEdit {range:range(&doc.text,site.from,site.to),text:new.clone()}).collect();
        Ok(FileAction {title:format!("Rename local Batch {} {} to {new}",pending.context.kind,pending.context.old),kind:Some("refactor".into()),edits:vec![(doc.path.clone(),edits)],..Default::default()})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{file_actions::FileActions,mock::MockHost};
    const BODY:&str="<b:job id='job'><b:step id='load' next='process'/><b:step id='process' next='finish'><b:tasklet ref='worker'/></b:step><b:decision id='branch' decider='chooser'><b:next on='RETRY' to='process'/><b:stop on='PAUSE' restart='process'/></b:decision><b:step id='again' next='process'/><b:step id='finish'/></b:job>";
    fn document(body:&str)->Document {Document {path:"/project/jobs.xml".into(),language:"xml".into(),version:7,text:format!("<beans xmlns='{BEANS}' xmlns:b='{NS}'><!--😀 café-->\r\n{body}\r\n</beans>")}}
    fn selection(doc:&Document)->Range {let p=position(&doc.text,doc.text.find("id='process'").unwrap()+4);Range::new(p,p)}
    fn lazy(owner:&mut structure::Structure,doc:&Document)->FileAction {owner.file_actions(doc,selection(doc)).into_iter().find(|a|a.title==TITLE).expect("explicit rename")}
    fn apply(doc:&Document,action:&FileAction)->String {
        let [(path,edits)]=action.edits.as_slice() else{panic!("one document")};assert_eq!(path,&doc.path);assert!(action.operations.is_empty());
        let mut ranges:Vec<_>=edits.iter().map(|edit|(offset(&doc.text,edit.range.start).unwrap(),offset(&doc.text,edit.range.end).unwrap(),&edit.text)).collect();ranges.sort_by_key(|r|r.0);
        let mut text=doc.text.clone();for (from,to,value) in ranges.into_iter().rev(){text.replace_range(from..to,value);}text
    }
    #[test]
    fn registered_step_rename_updates_exact_declaration_and_all_five_sites_after_review() {
        let d=document(BODY);let host=MockHost::install();host.add_file(&d.path,&d.text);let mut owner=structure::Structure::default();let action=lazy(&mut owner,&d);let replay=action.clone();
        assert!(action.edits.is_empty() && action.operations.is_empty());host.answer_next_prompt(Some(&["renamed"]));host.answer_next_prompt(Some(&["yes"]));
        let resolved=owner.resolve_file_action(&d,action).unwrap();assert_eq!(resolved.edits[0].1.len(),5);
        assert_eq!(apply(&d,&resolved),d.text.replace("'process'","'renamed'"));assert!(apply(&d,&resolved).contains("next='finish'"));
        assert_eq!(host.invocation().project().unwrap().read_file(&d.path).unwrap(),d.text);assert!(host.process_runs().is_empty() && host.requests().is_empty());
        assert!(matches!(owner.resolve_file_action(&d,replay),Err(Error::Stale)));
    }
    #[test]
    fn decision_rename_from_incoming_reference_preserves_step_and_decider_names() {
        let d=document("<b:job id='job'><b:step id='load' next='branch'/><b:decision id='branch' decider='branch'><b:next on='CONTINUE' to='finish'/><b:stop on='PAUSE' restart='branch'/></b:decision><b:step id='finish'/></b:job>");
        let h=MockHost::install();h.add_file(&d.path,&d.text);
        let at=position(&d.text,d.text.find("next='branch'").unwrap()+6);
        let mut owner=structure::Structure::default();
        let action=owner.file_actions(&d,Range::new(at,at)).into_iter().find(|a|a.title=="Rename Batch decision in this document…").unwrap();
        h.answer_next_prompt(Some(&["choose_path"]));h.answer_next_prompt(Some(&["yes"]));
        let resolved=owner.resolve_file_action(&d,action).unwrap();
        assert_eq!(resolved.edits[0].1.len(),3);
        let after=apply(&d,&resolved);
        assert!(after.contains("id='choose_path' decider='branch'"));
        assert!(after.contains("next='choose_path'"));assert!(after.contains("restart='choose_path'"));
        assert!(after.contains("to='finish'"));assert_eq!(h.invocation().project().unwrap().read_file(&d.path).unwrap(),d.text);
    }
    #[test]
    fn step_rename_can_start_at_each_proved_transition_reference() {
        let d=document(BODY);let model=context(&d,selection(&d)).unwrap();
        for site in model.references {
            let h=MockHost::install();h.add_file(&d.path,&d.text);let mut owner=structure::Structure::default();
            let at=position(&d.text,site.from);let action=owner.file_actions(&d,Range::new(at,at)).into_iter().find(|a|a.title==TITLE).unwrap();
            h.answer_next_prompt(Some(&["updated"]));h.answer_next_prompt(Some(&["yes"]));
            assert_eq!(apply(&d,&owner.resolve_file_action(&d,action).unwrap()),d.text.replace("'process'","'updated'"));
        }
    }
    #[test]
    fn self_loop_zero_incoming_default_namespace_and_unrelated_strings_preserve_exact_bytes() {
        for body in ["<b:job id='job'><b:step id='process' next='process'><b:tasklet ref='process'/></b:step></b:job>","<b:job id='job'><b:step id='process'/></b:job>"] {
            let d=document(body);let h=MockHost::install();h.add_file(&d.path,&d.text);let mut owner=structure::Structure::default();let action=lazy(&mut owner,&d);h.answer_next_prompt(Some(&["new_step-1.0"]));h.answer_next_prompt(Some(&["yes"]));let resolved=owner.resolve_file_action(&d,action).unwrap();
            let expected=d.text.replace("id='process'","id='new_step-1.0'").replace("next='process'","next='new_step-1.0'");assert_eq!(apply(&d,&resolved),expected);assert_eq!(resolved.edits[0].1.len(),if body.contains("next="){2}else{1});
        }
        let d=Document {text:format!("<job xmlns='{NS}' id='job'><step id='process'/></job>"),..document("")};assert!(context(&d,selection(&d)).is_some());
    }
    #[test]
    fn duplicate_nested_inherited_dynamic_encoded_foreign_and_unknown_target_roles_refuse() {
        let mut bodies=vec![
            BODY.replace("id='again'","id='process'"),BODY.replace("id='branch'","id='process'"),format!("{BODY}{BODY}"),
            BODY.replace("id='job'","id='job' parent='base'"),BODY.replace("id='process' next=","id='process' abstract='true' next="),
            BODY.replace("id='process' next=","id='process' ref='external' next="),BODY.replace("<b:step id='again' next='process'/>","<b:flow id='flow'><b:step id='nested'/></b:flow>"),
            BODY.replace("next='process'","next='${target}'"),BODY.replace("to='process'","b:to='process'"),BODY.replace("restart='process'","restart='pro&#99;ess'"),
            BODY.replace("<b:tasklet ref='worker'/>","<b:tasklet ref='worker' to='process'/>") ,BODY.replace("<b:tasklet ref='worker'/>","<x:tasklet xmlns:x='urn:foreign' ref='worker'/>")];
        bodies.push(BODY.replace("</b:job>",""));
        for body in bodies {let d=document(&body);assert!(context(&d,selection(&d)).is_none(),"{body}");}
        let d=document(BODY);let p=position(&d.text,d.text.find("next='process'").unwrap()+6);assert!(context(&d,Range::new(p,p)).is_none());
        let mut selected=selection(&d);selected.end.character+=1;assert!(context(&d,selected).is_none());
    }
    #[test]
    fn invalid_colliding_and_preexisting_dangling_destination_literals_refuse_before_confirm() {
        for name in ["process","finish","job","worker space","1bad","é","bad'quote","${dynamic}","",&"x".repeat(129)] {
            let d=document(BODY);let h=MockHost::install();h.add_file(&d.path,&d.text);let mut owner=structure::Structure::default();let action=lazy(&mut owner,&d);h.answer_next_prompt(Some(&[name]));
            assert!(owner.resolve_file_action(&d,action).is_err(),"{name}");assert_eq!(h.invocation().project().unwrap().read_file(&d.path).unwrap(),d.text);
        }
        let d=document(&BODY.replace("next='finish'","next='dangling'"));let h=MockHost::install();h.add_file(&d.path,&d.text);let mut owner=structure::Structure::default();let action=lazy(&mut owner,&d);h.answer_next_prompt(Some(&["dangling"]));assert!(owner.resolve_file_action(&d,action).is_err());
    }
    #[test]
    fn cancellation_stale_source_owner_tokens_and_full_preview_limit_return_no_edits() {
        for stage in 0..7 {
            let d=document(BODY);let h=MockHost::install();h.add_file(&d.path,&d.text);let mut owner=structure::Structure::default();let action=lazy(&mut owner,&d);
            let result=match stage {
                0=>owner.resolve_file_action(&d,action),
                1=>{h.answer_next_prompt(Some(&["renamed"]));owner.resolve_file_action(&d,action)},
                2=>owner.resolve_file_action(&Document {version:8,..d.clone()},action),
                3=>{h.add_file(&d.path,"changed");owner.resolve_file_action(&d,action)},
                4=>{h.set_project_root(Some("/other"));owner.resolve_file_action(&d,action)},
                5=>{h.add_project("/project/jobs.xml");owner.resolve_file_action(&d,action)},
                _=>owner.resolve_file_action(&d,FileAction {data:Some("batch-step-rename:forged".into()),..Default::default()}),
            };assert!(result.is_err());
        }
        let d=document(&format!("<!--{}-->{BODY}","x".repeat(9<<10)));let h=MockHost::install();h.add_file(&d.path,&d.text);let mut owner=structure::Structure::default();let action=lazy(&mut owner,&d);h.answer_next_prompt(Some(&["renamed"]));h.answer_next_prompt(Some(&["yes"]));
        assert!(matches!(owner.resolve_file_action(&d,action),Err(Error::Invalid(_))));assert!(host::confirm("Pending", "Oversized complete review consumed no confirmation").unwrap());
    }
    #[test]
    fn inventory_bounds_and_opaque_pending_eviction_are_fail_closed() {
        let mut d=document(BODY);d.text=format!("<!--{}-->{}","x".repeat(33<<10),d.text);assert!(context(&d,selection(&d)).is_none());
        let links="<b:next on='*' to='process'/>".repeat(64);let d=document(&format!("<b:job id='job'><b:step id='process'>{links}</b:step></b:job>"));assert!(context(&d,selection(&d)).is_none());
        let d=document(BODY);let h=MockHost::install();h.add_file(&d.path,&d.text);let mut owner=structure::Structure::default();let first=lazy(&mut owner,&d);for _ in 0..4{lazy(&mut owner,&d);}assert!(matches!(owner.resolve_file_action(&d,first),Err(Error::Stale)));
    }
    fn focus(host:&MockHost,doc:&Document,at:Position) {host.open_editor(&doc.path,"xml",&doc.text,&[(at.line,at.character,at.line,at.character)]);}
    #[test]
    fn registered_usage_picker_navigates_every_exact_site_without_file_reads_or_mutation() {
        let d=document(BODY);let model=context(&d,selection(&d)).unwrap();let mut sites=vec![model.declaration.clone()];sites.extend(model.references.clone());sites.sort_by_key(|site|site.from);
        for (index,site) in sites.iter().enumerate() {
            let h=MockHost::install().with_capabilities(&["project.read","editor.read"]);focus(&h,&d,selection(&d).start);let mut owner=structure::Structure::default();h.answer_next_prompt(Some(&[&format!("site:{index}")]));
            let result=owner.run_command("batch.current-file-step-usages",&h.invocation(),None).unwrap().unwrap();assert!(result.contains("source-local"));
            let p=position(&d.text,site.from);assert_eq!(h.opened_locations(),[(d.path.clone(),p.line,p.character)]);assert_eq!(h.editor_text(),d.text);assert_eq!(h.editor_version(),1);
            assert!(h.requests().is_empty() && h.process_runs().is_empty());
        }
    }
    #[test]
    fn usage_items_expose_path_utf16_range_kind_and_reference_entry_context() {
        let d=document(BODY);let model=context(&d,selection(&d)).unwrap();let mut sites=vec![model.declaration.clone()];sites.extend(model.references.clone());let items=usage_items(&d,&model,&sites);
        assert_eq!(items.len(),5);assert!(items[0].label.contains("step declaration id"));assert!(items.iter().any(|item|item.label.contains("stop @restart")));
        for item in items {let detail=item.description.unwrap();assert!(detail.contains(&d.path) && detail.contains("UTF-16 range") && detail.contains("physical job job") && detail.contains("step process"));}
        for site in &model.references {let p=position(&d.text,site.from);assert_eq!(usage_context(&d,Range::new(p,p)).unwrap(),model);}
        let other=position(&d.text,d.text.find("ref='worker'").unwrap()+5);assert!(usage_context(&d,Range::new(other,other)).is_none());
    }
    #[test]
    fn usage_cancel_forged_denied_unsupported_and_no_reference_cases_are_read_only() {
        for answer in [None,Some("forged")] {
            let d=document(BODY);let h=MockHost::install();focus(&h,&d,selection(&d).start);let mut owner=structure::Structure::default();if let Some(answer)=answer {h.answer_next_prompt(Some(&[answer]));}
            let result=owner.run_command("batch.current-file-step-usages",&h.invocation(),None);if answer.is_none(){assert_eq!(result.unwrap(),None);}else{assert!(result.is_err());}assert!(h.opened_locations().is_empty());assert_eq!(h.editor_text(),d.text);
        }
        let d=document(BODY);let h=MockHost::install().with_capabilities(&["project.read"]);focus(&h,&d,selection(&d).start);assert!(usages(&h.invocation()).is_err());assert!(h.opened_locations().is_empty());
        let d=document("<b:job id='job'><b:step id='process'/></b:job>");let h=MockHost::install();focus(&h,&d,selection(&d).start);h.answer_next_prompt(Some(&["site:0"]));assert!(usages(&h.invocation()).unwrap().is_some());assert_eq!(h.opened_locations().len(),1);
        let d=document(&BODY.replace("id='again'","id='process'"));let h=MockHost::install();focus(&h,&d,selection(&d).start);assert!(usages(&h.invocation()).is_err());assert!(h.opened_locations().is_empty());
    }
    #[test]
    fn navigation_freshness_rechecks_actual_editor_version_path_and_project_owner() {
        for change in 0..4 {
            let d=document(BODY);let h=MockHost::install();focus(&h,&d,selection(&d).start);let invocation=h.invocation();let (root,captured,selected)=focused_source(&invocation).unwrap();let model=usage_context(&captured,selected).unwrap();
            match change {
                0=>h.type_in_editor(" "),
                1=>h.set_project_root(Some("/other")),
                2=>h.add_project("/project/jobs.xml"),
                _=>{let different=Document {path:"/project/other.xml".into(),..d};focus(&h,&different,selection(&different).start);},
            }
            assert!(unchanged_usage_source(&invocation,&root,&captured,selected,&model).is_err());assert!(h.opened_locations().is_empty());
        }
    }
}
