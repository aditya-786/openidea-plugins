//! Complete, opt-in source-directory snapshots; no inventory in editor providers.
use super::*;
use std::ops::Range as Span;
use std::time::{Duration,Instant,SystemTime,UNIX_EPOCH};
const HELPER:&str=include_str!("../adapters/complete_inventory.py");
const TTL:Duration=Duration::from_secs(120);
#[derive(Default)]
pub(crate) struct State { snapshot:Option<Snapshot>, watching:bool, serial:u64 }
struct Snapshot {root:String,base:String,doc:Document,serial:u64,at:Instant,unused:Vec<Span<usize>>,count:usize,unsupported:usize}
fn owned(root:&str,path:&str)->bool {host::project_of(path).and_then(|p|p.root().ok()).as_deref()==Some(root)}
fn inventory(text:&str,base:&str)->Result<Vec<(String,bool)>,Error> {
    if text.len()>1<<20 {return Err("Complete inventory output exceeds 1 MiB.".into());}
    let data:serde_json::Value=serde_json::from_str(text).map_err(|_|"Invalid complete inventory output.")?;
    if data["schema"]!="ignore-complete-v1"||data["root"].as_str()!=Some(base)||data["complete"]!=true
        ||data["fingerprint"].as_str().is_none_or(|s|s.len()!=64||!s.bytes().all(|b|b.is_ascii_hexdigit())) {return Err("Incomplete or foreign inventory output.".into());}
    let entries=data["entries"].as_array().filter(|p|p.len()<=4096&&data["count"].as_u64()==Some(p.len() as u64)).ok_or("Incomplete inventory entry count.")?;
    let mut out=Vec::new();let mut seen=HashSet::new();
    for entry in entries {
        let fields=entry.as_array().filter(|p|p.len()==2).ok_or("Invalid inventory entry.")?;
        let path=fields[0].as_str().filter(|p|!p.is_empty()&&p.len()<=1024&&!p.starts_with('/')&&!p.contains('\\')&&!p.chars().any(char::is_control)&&!p.split('/').any(|n|n.is_empty()||matches!(n,"."|".."))).ok_or("Invalid inventory path.")?;
        let kind=fields[1].as_str().filter(|k|matches!(*k,"file"|"directory"|"symlink")).ok_or("Unknown inventory file kind.")?;
        if !seen.insert(path.to_owned()) {return Err("Duplicate inventory path.".into());}
        out.push((path.to_owned(),kind=="directory"));
    }
    Ok(out)
}
fn matcher(rule:&matcher::Rule)->Option<globset::GlobMatcher> {
    let mut pattern=String::new();let mut escaped=false;
    for ch in rule.pattern.chars() {
        if escaped {pattern.push('\\');pattern.push(ch);escaped=false;}
        else if ch=='\\' {escaped=true;}
        else {if matches!(ch,'{'|'}'){pattern.push('\\');}pattern.push(ch);}
    }
    if escaped {return None;}
    globset::GlobBuilder::new(&pattern).literal_separator(true).backslash_escape(true).case_insensitive(true).build().ok().map(|g|g.compile_matcher())
}
fn matched(rule:&matcher::Rule,glob:&globset::GlobMatcher,path:&str,is_dir:bool)->bool {
    path.match_indices('/').map(|(at,_)|at).chain([path.len()]).any(|end| {
        if rule.dir_only&&end==path.len()&&!is_dir {return false;}
        let prefix=&path[..end];let candidate=if rule.anchored{prefix}else{prefix.rsplit('/').next().unwrap_or(prefix)};
        glob.is_match(candidate)
    })
}
fn find_unused(text:&str,entries:&[(String,bool)])->Result<(Vec<Span<usize>>,usize),Error> {
    if text.len()>128<<10 {return Err("Ignore source exceeds 128 KiB.".into());}
    let mut out=Vec::new();let mut offset=0;let mut rules=0;let mut unsupported=0;let deadline=Instant::now()+Duration::from_secs(2);
    for line in text.split_inclusive('\n') {
        let body=line.trim_end_matches(['\r','\n']);let start=offset;offset+=line.len();
        let Some(rule)=matcher::Rule::parse(body) else {continue;};
        rules+=1;if rules>512||Instant::now()>=deadline {return Err("Unused-rule analysis exceeded its rule/time limit; no partial results.".into());}
        if rule.pattern.len()>512||rule.pattern.contains("[[:")||(!rule.pattern.is_ascii()&&rule.pattern.contains(['?','['])) {unsupported+=1;continue;}
        let Some(glob)=matcher(&rule) else {unsupported+=1;continue;};
        let mut found=false;
        for (path,dir) in entries {
            if Instant::now()>=deadline{return Err("Unused-rule analysis exceeded its time limit; no partial results.".into());}
            if matched(&rule,&glob,path,*dir){found=true;break;}
        }
        // Git matches bytes and does not prove NFC/NFD equivalence here: a miss against a non-ASCII pattern is only trusted when no inventory path is non-ASCII.
        if !found&&!rule.pattern.is_ascii()&&entries.iter().any(|(p,_)|!p.is_ascii()) {unsupported+=1;continue;}
        if !found {out.push(start..start+body.len());}
    }
    Ok((out,unsupported))
}
impl State {
    pub fn configure(&mut self,watching:bool){self.watching=watching;if !watching{self.snapshot=None;}}
    pub fn invalidate(&mut self,events:&[Event]) {
        if events.iter().any(|e|matches!(e,Event::FilesChanged(_)|Event::FileSaved(_)|Event::VcsChanged(_))||matches!(e,Event::Other{kind,..}if kind=="settings-changed")) {
            self.snapshot=None;
        }
    }
    fn valid(&self,doc:&Document)->Option<&Snapshot> {
        self.snapshot.as_ref().filter(|s|self.watching&&s.at.elapsed()<TTL&&s.doc==*doc&&owned(&s.root,&doc.path))
    }
    pub fn diagnostics(&self,doc:&Document)->Vec<Diagnostic> {
        let Some(s)=self.valid(doc) else{return Vec::new();};
        s.unused.iter().map(|span|{let mut diagnostic=Diagnostic::new(range(&doc.text,span),Severity::Hint,"No file or directory matched this entry in the complete source-directory snapshot. It may intentionally cover future files.");diagnostic.code=Some("unused-entry-snapshot".into());diagnostic}).collect()
    }
    pub fn actions(&self,doc:&Document,selection:Range)->Vec<CodeAction> {
        let Some(s)=self.valid(doc) else{return Vec::new();};
        s.unused.iter().filter(|span|{let r=range(&doc.text,span);r.start<=selection.end&&selection.start<=r.end}).map(|span|CodeAction{title:"Remove unmatched ignore entry (complete snapshot)…".into(),kind:Some("quickfix".into()),preferred:false,edits:Vec::new(),data:Some(serde_json::json!(["unused-snapshot",s.serial,span.start,span.end]).to_string())}).collect()
    }
    pub fn resolve(&self,doc:&Document,action:CodeAction)->Result<CodeAction,Error> {
        let data:serde_json::Value=serde_json::from_str(action.data.as_deref().ok_or(Error::Stale)?).map_err(|_|Error::Stale)?;
        let s=self.valid(doc).ok_or(Error::Stale)?;
        if data[0]!="unused-snapshot"||data[1].as_u64()!=Some(s.serial){return Err(Error::Stale);}
        let span=data[2].as_u64().zip(data[3].as_u64()).and_then(|(a,b)|s.unused.iter().find(|span|span.start as u64==a&&span.end as u64==b)).ok_or(Error::Stale)?;
        let project=host::project_of(&doc.path).ok_or(Error::Stale)?;
        if project.read_file(&doc.path)?!=doc.text {return Err(Error::Stale);}
        if !ui_kit::prompts::confirm_exact("Remove unmatched ignore entry?",&format!("File: {}\nScanned folder: {}\nEntries scanned: {}\n\nEntry: {}\n\nThis entry matched no file when the folder was scanned. Files generated later may still need it. Remove this line?",doc.path,s.base,s.count,serde_json::to_string(&doc.text[span.clone()]).unwrap_or_default()))?{return Err(Error::Cancelled);}
        if self.valid(doc).is_none()||project.read_file(&doc.path)?!=doc.text{return Err(Error::Stale);}
        let mut end=span.end;
        if doc.text.as_bytes().get(end)==Some(&b'\r'){end+=1;}
        if doc.text.as_bytes().get(end)==Some(&b'\n'){end+=1;}
        Ok(CodeAction{title:action.title,kind:action.kind,preferred:false,edits:vec![(doc.path.clone(),vec![TextEdit{range:range(&doc.text,&(span.start..end)),text:String::new()}])],data:None})
    }
    pub fn capture(&mut self,ctx:&Invocation)->Result<Option<String>,Error> {
        if !self.watching{return Err("Complete unused-entry checks require delivered filesystem events; reload Ignore after granting events.".into());}
        let editor=ctx.editor().ok_or("Focus a project .gitignore first.")?;
        let path=editor.path();if !preferences::checks(&path){return Err("Enable checks for this ignore file before collecting unused-entry hints.".into());}if path.rsplit('/').next()!=Some(".gitignore"){return Err("Complete unused-entry analysis currently supports .gitignore files.".into());}
        let project=ctx.project().ok_or(Error::Stale)?;let root=project.root()?;
        if !owned(&root,&path){return Err(Error::Stale);}
        let source=editor.document()?;let doc=Document{path:path.clone(),language:editor.language(),text:source.text,version:source.version};
        if doc.text.len()>128<<10{return Err("Ignore source exceeds 128 KiB.".into());}
        let base=path.rsplit_once('/').ok_or(Error::Stale)?.0.to_owned();
        if !ui_kit::prompts::confirm_exact("Find unused entries?",&format!("Folder: {base}\nIgnore file: {path}\n\nRuns python3 to list every file in this folder, including hidden and ignored ones. Large folders (over 4096 entries) are refused. No file is changed; entries that match nothing get a hint."))? {return Ok(None);}
        let current=editor.document()?;if current.version!=doc.version||current.text!=doc.text||!owned(&root,&path){return Err(Error::Stale);}
        self.serial=self.serial.checked_add(1).ok_or(Error::Stale)?;
        let private=host::fs::plugin_dir()?;let helper=format!("{private}/complete_inventory.py");
        let stamp=SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();let output=format!("{private}/complete-inventory-{}-{stamp}.json",self.serial);
        host::fs::write_file(&helper,HELPER)?;host::fs::write_file(&output,"")?;
        let task=host::begin_task("Complete ignore inventory",true);let command=host::Command::new("python3").args(["-I","-S",helper.as_str(),base.as_str(),output.as_str()]);
        let child=host::start(&command,Some(&project))?;let _=child.close_stdin();let deadline=Instant::now()+Duration::from_secs(10);
        let status=loop {if task.is_cancelled()||Instant::now()>=deadline {let _=child.kill();let _=host::fs::write_file(&output,"");return Err("Complete inventory cancelled or timed out; no absence conclusions.".into());}
            match child.wait(100) {Ok(chunk)=>if let Some(status)=chunk.status{break status;},Err(error)=>{let _=child.kill();let _=host::fs::write_file(&output,"");return Err(error);}}
        };
        let result=if status==0{host::fs::read_file(&output)}else{Err("Complete inventory refused an error, bound or concurrent change; no absence conclusions.".into())};
        let _=host::fs::write_file(&output,"");let output=result?;let entries=inventory(&output,&base)?;
        let current=editor.document()?;if current.version!=doc.version||current.text!=doc.text||!owned(&root,&path){return Err(Error::Stale);}
        let (unused,unsupported)=find_unused(&doc.text,&entries)?;
        self.snapshot=Some(Snapshot{root,base,doc:doc.clone(),serial:self.serial,at:Instant::now(),unused,count:entries.len(),unsupported});
        host::notify(NotificationLevel::Info,"Complete unused-entry snapshot",&format!("{} paths captured; {} unmatched entries; {} unsupported rules withheld. Glob matching conservatively ignores case; non-ASCII patterns are judged only by exact bytes and withheld when non-ASCII paths exist without a match, or when they use ? or [. Snapshot eligibility lasts 120 seconds and delivered filesystem/source changes withdraw it. Results describe capture time; rerun to refresh. Source unchanged.",entries.len(),self.snapshot.as_ref().unwrap().unused.len(),self.snapshot.as_ref().unwrap().unsupported));
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{mock::MockHost,diagnostics::DiagnosticsProvider,Event,FilesChanged};
    fn cache(doc:&Document)->State {
        State{watching:true,serial:1,snapshot:Some(Snapshot{root:"/project".into(),base:"/project".into(),doc:doc.clone(),serial:1,at:Instant::now(),unused:find_unused(&doc.text,&[("present.log".into(),false),("empty-dir".into(),true)]).unwrap().0,count:2,unsupported:0})}
    }
    #[test]
    fn complete_inventory_matches_globs_directories_negations_and_escaped_literals() {
        let text="*.log\nempty-dir/\nmissing.*\n!future.txt\n\\#literal\nname{literal}\n";
        let entries=vec![("present.log".into(),false),("empty-dir".into(),true),("#literal".into(),false),("name{literal}".into(),false)];
        let (spans,unsupported)=find_unused(text,&entries).unwrap();assert_eq!(unsupported,0);
        assert_eq!(spans.iter().map(|r|&text[r.clone()]).collect::<Vec<_>>(),["missing.*","!future.txt"]);
        assert!(find_unused(&"a\n".repeat(513),&[]).is_err());
    }
    #[test]
    fn non_ascii_patterns_match_exact_bytes_and_refuse_unprovable_misses() {
        let entries=vec![("caf\u{e9}.log".into(),false),("\u{65e5}\u{672c}/a.txt".into(),false)];
        let text="caf\u{e9}.log\n\u{65e5}\u{672c}/\n*\u{e9}.log\nna\u{ef}ve.txt\ncaf??.log\ncaf[\u{e9}].log\n";
        let (spans,unsupported)=find_unused(text,&entries).unwrap();
        assert_eq!(unsupported,2);
        assert!(spans.is_empty(),"{spans:?}");
        let (spans,unsupported)=find_unused("na\u{ef}ve.txt\nmissing\n",&[("a.txt".into(),false)]).unwrap();
        assert_eq!(unsupported,0);
        assert_eq!(spans.len(),2);
        let (spans,_)=find_unused("caf?.log\n",&entries).unwrap();
        assert_eq!(spans.len(),1,"? is one byte, as in Git");
        let (spans,unsupported)=find_unused("missing\n",&entries).unwrap();
        assert_eq!((spans.len(),unsupported),(1,0));
    }
    #[test]
    fn snapshots_gate_native_diagnostics_and_reviewed_source_removal() {
        let h=MockHost::install();let text="*.log\nmissing.*\n";
        h.open_editor("/project/.gitignore","ignore",text,&[]);
        let doc=Document{path:"/project/.gitignore".into(),language:"ignore".into(),text:text.into(),version:1};
        let mut plugin=IgnorePlugin{unused:cache(&doc),..Default::default()};
        let diagnostics=plugin.diagnostics(&doc);
        assert!(diagnostics.iter().any(|d|d.code.as_deref()==Some("unused-entry-snapshot")));
        let range=Range::new(Position::new(1,0),Position::new(1,0));
        let action=plugin.code_actions(&doc,range).into_iter().find(|a|a.title.starts_with("Remove unmatched")).unwrap();
        h.answer_next_prompt(None);assert!(matches!(plugin.resolve_code_action(&doc,action.clone()),Err(Error::Cancelled)));
        h.answer_next_prompt(Some(&[]));let resolved=plugin.resolve_code_action(&doc,action.clone()).unwrap();
        assert_eq!(resolved.edits[0].1[0].range,Range::new(Position::new(1,0),Position::new(2,0)));
        assert_eq!(h.editor_text(),text,"resolver returns an undoable edit without writing source");
        plugin.unused.invalidate(&[Event::FilesChanged(FilesChanged{created:vec!["/project/missing.txt".into()],..Default::default()})]);
        assert!(plugin.resolve_code_action(&doc,action).is_err());assert!(plugin.unused.diagnostics(&doc).is_empty());
    }
    #[test]
    fn foreign_expired_source_and_incomplete_protocol_never_publish_absence() {
        let h=MockHost::install();let doc=Document{path:"/project/.gitignore".into(),language:"ignore".into(),text:"missing\n".into(),version:1};
        let mut state=cache(&doc);state.snapshot.as_mut().unwrap().at=Instant::now()-TTL-Duration::from_secs(1);
        assert!(state.diagnostics(&doc).is_empty());state=cache(&doc);
        let changed=Document{version:2,..doc.clone()};assert!(state.diagnostics(&changed).is_empty());
        h.add_project("/project/.gitignore");assert!(state.diagnostics(&doc).is_empty());
        for value in [serde_json::json!({}),serde_json::json!({"schema":"ignore-complete-v1","root":"/project","complete":false,"count":0,"fingerprint":"a".repeat(64),"entries":[]}),serde_json::json!({"schema":"ignore-complete-v1","root":"/project","complete":true,"count":1,"fingerprint":"a".repeat(64),"entries":[["../outside","file"]]})] {
            assert!(inventory(&value.to_string(),"/project").is_err());
        }
    }
    #[test]
    fn actual_python_capture_includes_hidden_and_ignored_paths_and_refuses_overflow() {
        if std::process::Command::new("python3").arg("--version").output().is_err(){eprintln!("Python 3 unavailable; optional helper test skipped");return;}
        let unique=SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let temporary=std::env::temp_dir().join(format!("ignore-complete-{unique}"));std::fs::create_dir_all(&temporary).unwrap();
        let temporary=temporary.canonicalize().unwrap();let base=temporary.join("project");std::fs::create_dir_all(base.join("ignored")).unwrap();
        std::fs::write(base.join(".gitignore"),"ignored/\n").unwrap();std::fs::write(base.join(".hidden"),"").unwrap();std::fs::write(base.join("ignored/a.log"),"").unwrap();
        let helper=temporary.join("helper.py");let output=temporary.join("result.json");std::fs::write(&helper,HELPER).unwrap();
        let result=std::process::Command::new("python3").args(["-I","-S"]).arg(&helper).arg(&base).arg(&output).output().unwrap();
        assert!(result.status.success(),"{}",String::from_utf8_lossy(&result.stderr));
        let entries=inventory(&std::fs::read_to_string(&output).unwrap(),base.to_str().unwrap()).unwrap();
        assert!(entries.contains(&(".hidden".into(),false)));assert!(entries.contains(&("ignored/a.log".into(),false)));
        assert!(find_unused("ignored/\n",&entries).unwrap().0.is_empty());
        for n in 0..4097 {std::fs::write(base.join(format!("file-{n}")),"").unwrap();}
        std::fs::remove_file(&output).unwrap();
        let result=std::process::Command::new("python3").args(["-I","-S"]).arg(&helper).arg(&base).arg(&output).output().unwrap();
        assert!(!result.status.success());assert!(!output.exists(),"no incomplete result artifact");
        std::fs::remove_dir_all(&temporary).unwrap();
    }
}
