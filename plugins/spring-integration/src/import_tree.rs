//! Explicit physical XML import-tree rename, not an effective Spring context.
use crate::*;
use openidea_plugin::{file_actions::FileAction,TextEdit,text::offset};
const BEANS:&str="http://www.springframework.org/schema/beans";
const TITLE:&str="Rename channel in this XML import tree…";
#[derive(Clone,Debug,PartialEq,Eq)]
struct Site {path:String,from:usize,to:usize,name:String,declaration:bool}
#[derive(Clone,Debug,PartialEq,Eq)]
struct Tree {files:BTreeMap<String,String>,sites:Vec<Site>,aliases:BTreeMap<String,String>,occupied:BTreeSet<String>}
use spring_text::provenance::literal;
fn imported(root:&str,base:&str,value:&str)->Option<String> {
    if value.is_empty() || value.len()>512 || value.contains(['\\',':','*','?','[',']','{','}']) || value.starts_with('/') || value.chars().any(char::is_control) {return None;}
    let mut parts:Vec<_>=base.rsplit_once('/')?.0.split('/').collect();
    for part in value.split('/') {match part {""|"."=>return None,".."=>{parts.pop()?;},p=>parts.push(p)}}
    let path=parts.join("/");(path.starts_with(&format!("{root}/"))&&path.ends_with(".xml")).then_some(path)
}
fn read_tree(root:&str,path:&str)->Result<Tree,Error> {
    let project=host::project_of(path).ok_or(Error::Stale)?;if project.root()?!=root {return Err(Error::Stale);}
    let mut tree=Tree{files:BTreeMap::new(),sites:Vec::new(),aliases:BTreeMap::new(),occupied:BTreeSet::new()};let mut queue=vec![path.to_owned()];let mut bytes=0;
    while let Some(path)=queue.pop() {
        if tree.files.contains_key(&path) {continue;}
        if tree.files.len()>=16 || spring_text::index::owning_root(&path).is_none() || host::project_of(&path).and_then(|p|p.root().ok()).as_deref()!=Some(root) {return Err(Error::Invalid("XML import tree exceeds this project's supported scope.".into()));}
        let text=project.read_file(&path)?;bytes+=text.len();
        if bytes>256*1024 || text.len()>32*1024 || !spring_text::xml::generation_valid(&text) || (text.contains("${") || text.contains("#{")) {return Err(Error::Invalid("Use a bounded, literal XML import tree without expressions or encoded values.".into()));}
        let elements=xml::elements(&text);let first=elements.first().ok_or(Error::Stale)?;
        if first.local()!="beans" || xml::namespace(&elements,0)!=Some(BEANS) || first.attrs.iter().any(|a|!a.name.starts_with("xmlns")&&a.name!="xsi:schemaLocation") {return Err(Error::Invalid("Only unprofiled beans roots are supported.".into()));}
        for (i,e) in elements.iter().enumerate().skip(1) {
            if e.parent!=Some(0) {return Err(Error::Invalid("Nested XML elements are outside this import-tree refactoring.".into()));}
            let ns=xml::namespace(&elements,i);
            if ns==Some(BEANS)&&e.local()=="import" {
                if e.attrs.len()!=1 || e.attrs[0].name!="resource" {return Err(Error::Invalid("Use one literal relative resource per import.".into()));}
                queue.push(imported(root,&path,&e.attrs[0].value).ok_or("Import must resolve to a relative XML file inside this project.")?);continue;
            }
            if ns==Some(BEANS)&&e.local()=="alias" {
                if e.attrs.len()!=2 || e.attrs.iter().any(|a|!matches!(a.name.as_str(),"name"|"alias")||!literal(&a.value)) {return Err(Error::Invalid("Unsupported alias declaration.".into()));}
                let name=e.attr("name").ok_or(Error::Stale)?;let alias=e.attr("alias").ok_or(Error::Stale)?;
                if tree.aliases.insert(alias.value.clone(),name.value.clone()).is_some() {return Err(Error::Invalid("Duplicate alias declaration.".into()));}
                tree.sites.push(Site{path:path.clone(),from:name.value_at,to:name.value_end,name:name.value.clone(),declaration:false});
                tree.sites.push(Site{path:path.clone(),from:alias.value_at,to:alias.value_end,name:alias.value.clone(),declaration:false});continue;
            }
            if ns!=Some(NAMESPACE) {return Err(Error::Invalid("The import tree contains elements other than channels, Integration endpoints, imports and aliases, so it can't be searched safely.".into()));}
            let channel=matches!(e.local(),"channel"|"publish-subscribe-channel");
            let allowed:&[&str]=match e.local() {
                "channel"|"publish-subscribe-channel"=>&["id"],
                "bridge"=>&["id","input-channel","output-channel","error-channel"],
                "service-activator"|"transformer"|"filter"|"splitter"|"aggregator"=>&["id","input-channel","output-channel","error-channel","discard-channel","ref","method"],
                _=>return Err(Error::Invalid("Unsupported endpoint in XML import tree.".into())),
            };
            if e.attrs.iter().any(|a|!allowed.contains(&a.name.as_str())||!literal(&a.value)) {return Err(Error::Invalid("Unsupported or dynamic endpoint attribute.".into()));}
            for a in &e.attrs {
                if !channel && a.name=="id" {tree.occupied.insert(a.value.clone());}
                if a.name=="id" || a.name.ends_with("-channel") {
                    tree.sites.push(Site{path:path.clone(),from:a.value_at,to:a.value_end,name:a.value.clone(),declaration:channel&&a.name=="id"});
                }
            }
        }
        tree.files.insert(path,text);
    }
    if tree.sites.len()>256 || tree.aliases.len()>32 {return Err(Error::Invalid("XML import tree has too many symbols.".into()));}
    if tree.aliases.keys().any(|name|tree.occupied.contains(name)||tree.sites.iter().any(|s|s.declaration&&s.name==*name)) {return Err(Error::Invalid("Alias name collides with a bean declaration.".into()));}
    Ok(tree)
}
fn canonical<'a>(tree:&'a Tree,name:&'a str)->Option<&'a str> {
    if tree.occupied.contains(name) {return None;}
    let mut target=name;let mut visited=BTreeSet::new();
    while let Some(next)=tree.aliases.get(target) {if !visited.insert(target) {return None;}target=next;}
    (!tree.occupied.contains(target)&&tree.sites.iter().filter(|s|s.declaration&&s.name==target).count()==1&&!tree.aliases.contains_key(target)).then_some(target)
}
pub(crate) fn occurrences(ctx:&Invocation)->Result<Option<String>,Error> {
    let root=ctx.project().ok_or("Open a project first.")?.root()?;
    let editor=ctx.editor().ok_or_else(||format!("Open the XML file that imports the others first. {}",ui_kit::denied::message("Find Channel Usages in XML Import Tree","editor.read","Spring Structure Tools")))?;
    let source=editor.document()?;let doc=Document{path:editor.path(),language:editor.language(),text:source.text,version:source.version};
    if doc.language!="xml" {return Err(Error::Invalid("Open an XML import-tree root.".into()));}
    let selections=editor.selections()?;let [selection]=selections.as_slice() else{return Err(Error::Stale);};
    if selection.start!=selection.end {return Err(Error::Invalid("Use a single caret on a channel or alias.".into()));}
    let tree=read_tree(&root,&doc.path)?;if tree.files.get(&doc.path)!=Some(&doc.text) {return Err(Error::Stale);}
    let at=offset(&doc.text,selection.start).ok_or(Error::Stale)?;
    let site=tree.sites.iter().find(|s|s.path==doc.path&&s.from<=at&&at<s.to).ok_or("Select a channel or alias occurrence.")?;
    let owner=canonical(&tree,&site.name).ok_or("This channel or alias has no unique declaration in this XML import tree.")?;
    let mut sites:Vec<_>=tree.sites.iter().filter(|s|canonical(&tree,&s.name)==Some(owner)).collect();sites.sort_by_key(|s|(&s.path,s.from));
    let items:Vec<_>=sites.iter().enumerate().map(|(i,s)| {
        let r=range(&tree.files[&s.path],s.from,s.to);
        openidea_plugin::PickItem::new(&format!("site:{i}"),&format!("{} · {}:{} · {}",s.path,r.start.line+1,r.start.character+1,s.name))
            .description(if s.declaration {"Channel declaration · explicit XML import tree"}else{"Channel reference or alias · explicit XML import tree"})
    }).collect();
    let Some(choice)=host::pick(&format!("Channel {owner}: {} physical occurrences in {} imported XML files",sites.len(),tree.files.len()),&items)? else{return Ok(None);};
    let chosen=choice.strip_prefix("site:").and_then(|v|v.parse::<usize>().ok()).and_then(|i|sites.get(i)).ok_or(Error::Stale)?;
    let now=editor.document()?;if editor.path()!=doc.path||now.text!=doc.text||now.version!=doc.version||read_tree(&root,&doc.path)?!=tree {return Err(Error::Stale);}
    let p=range(&tree.files[&chosen.path],chosen.from,chosen.to).start;host::open_location(&chosen.path,p.line,p.character)?;
    Ok(Some("Navigated within the explicit forward XML import tree, including proved alias chains. Reverse imports, other contexts and Java/Kotlin/runtime references are excluded.".into()))
}

#[derive(Default)]
pub(crate) struct Renames {next:u64,pending:BTreeMap<String,(String,Document,Range)>}
impl Renames {
    pub(crate) fn actions(&mut self,doc:&Document,selection:Range)->Vec<FileAction> {
        if selection.start!=selection.end || doc.language!="xml" || doc.text.len()>32*1024 {return vec![];}
        let elements=xml::elements(&doc.text);if !elements.iter().any(|e|e.local()=="import") {return vec![];}
        let Some(at)=offset(&doc.text,selection.start) else{return vec![];};
        if !elements.iter().any(|e|e.attrs.iter().any(|a|a.holds(at)&&(matches!(a.name.as_str(),"id"|"input-channel"|"output-channel"|"error-channel"|"discard-channel")||e.local()=="alias"&&matches!(a.name.as_str(),"name"|"alias")))) {return vec![];}
        let Some(root)=host::project_of(&doc.path).and_then(|p|p.root().ok()) else{return vec![];};
        self.next=self.next.wrapping_add(1);if self.pending.len()>=4 {self.pending.clear();}
        let key=format!("integration-import-tree:{}",self.next);self.pending.insert(key.clone(),(root,doc.clone(),selection));
        vec![FileAction{title:TITLE.into(),kind:Some("refactor".into()),data:Some(key),..Default::default()}]
    }
    pub(crate) fn resolve(&mut self,doc:&Document,action:FileAction)->Result<FileAction,Error> {
        let (root,captured,selection)=self.pending.remove(action.data.as_deref().ok_or(Error::Stale)?).ok_or(Error::Stale)?;
        if captured!=*doc {return Err(Error::Stale);}
        let tree=read_tree(&root,&doc.path)?;if tree.files.get(&doc.path)!=Some(&doc.text) {return Err(Error::Stale);}
        let at=offset(&doc.text,selection.start).ok_or(Error::Stale)?;
        let selected=tree.sites.iter().find(|s|s.path==doc.path&&s.from<=at&&at<s.to).ok_or("Select a channel declaration or channel-valued endpoint reference.")?;
        let old=selected.name.clone();
        let alias=tree.aliases.contains_key(&old);
        if matches!(old.as_str(),"errorChannel"|"nullChannel") || canonical(&tree,&old).is_none() {return Err(Error::Invalid("Select a uniquely owned channel or alias, not an implicit channel.".into()));}
        // Alias targets may point at this channel; alias declarations themselves
        // stay stable. Every alias chain must terminate at a proved channel.
        for alias in tree.aliases.keys() {
            let mut target=alias;let mut visited=BTreeSet::new();
            while let Some(next)=tree.aliases.get(target) {if !visited.insert(target) {return Err(Error::Invalid("Cyclic alias tree.".into()));}target=next;}
            if tree.sites.iter().filter(|s|s.declaration&&s.name==*target).count()!=1 {return Err(Error::Invalid("Unresolved or ambiguous alias target.".into()));}
        }
        // IDs and handler references are bean names too. Refuse collisions or
        // a handler contract that this channel-only refactoring cannot prove.
        for text in tree.files.values() {for e in xml::elements(text) {for a in &e.attrs {
            if a.name=="ref"&&a.value==old || a.name=="id"&&a.value==old && !matches!(e.local(),"channel"|"publish-subscribe-channel") {return Err(Error::Invalid("Channel name collides with an endpoint/handler bean.".into()));}
        }}}
        let Some(new)=host::input(if alias {"New alias name in this explicit XML import tree"}else{"New channel ID in this explicit XML import tree"},Some(&old))? else{return Err(Error::Cancelled);};
        if !literal(&new)||new==old||matches!(new.as_str(),"errorChannel"|"nullChannel")||tree.aliases.contains_key(&new)||tree.files.values().any(|text|xml::elements(text).iter().flat_map(|e|&e.attrs).any(|a|a.value==new)) {return Err(Error::Invalid("Use a different unique literal channel ID.".into()));}
        let sites:Vec<_>=tree.sites.iter().filter(|s|s.name==old).collect();let mut edits:BTreeMap<String,Vec<TextEdit>>=BTreeMap::new();let mut review=format!("Explicit physical XML import tree rooted at {}\n{} files; {} exact edits\nChannel {old} → {new}\n\nOnly this forward import tree is covered. Reverse imports, other contexts, Java/Kotlin and runtime references are excluded. {}\n",doc.path,tree.files.len(),sites.len(),if alias {"This renames the selected alias declaration and its direct users; the underlying channel ID remains unchanged."}else{"Alias names remain unchanged; alias targets are updated."});
        for site in sites {let text=&tree.files[&site.path];edits.entry(site.path.clone()).or_default().push(TextEdit{range:range(text,site.from,site.to),text:new.clone()});}
        for (path,changes) in &edits {let before=&tree.files[path];let mut after=before.clone();for e in changes.iter().rev() {after.replace_range(offset(before,e.range.start).ok_or(Error::Stale)?..offset(before,e.range.end).ok_or(Error::Stale)?,&e.text);}review.push_str(&format!("\n{path}\nBEFORE:\n{before}\nAFTER:\n{after}\n"));}
        if read_tree(&root,&doc.path)?!=tree {return Err(Error::Stale);}
        if !ui_kit::prompts::confirm_exact("Rename channel in this explicit XML import tree?",&review)? {return Err(Error::Cancelled);}
        if read_tree(&root,&doc.path)?!=tree {return Err(Error::Stale);}
        Ok(FileAction{title:format!("Rename channel {old} to {new} in XML import tree"),kind:Some("refactor".into()),edits:edits.into_iter().collect(),..Default::default()})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{file_actions::FileActions,mock::MockHost,text::position};
    fn setup()->(MockHost,Document) {
        let host=MockHost::install();
        let doc=Document{path:"/project/config/root.xml".into(),language:"xml".into(),version:1,text:format!("<beans xmlns='{BEANS}' xmlns:i='{NAMESPACE}'><!--😀--><import resource='channels.xml'/><alias name='orders' alias='input'/><i:bridge input-channel='orders' output-channel='input'/></beans>")};
        host.add_file(&doc.path,&doc.text);host.add_file("/project/config/channels.xml",&format!("<beans xmlns='{BEANS}' xmlns:i='{NAMESPACE}'><i:channel id='orders'/><i:service-activator input-channel='orders' ref='worker' method='orders'/></beans>"));(host,doc)
    }
    fn action(owner:&mut structure::Structure,doc:&Document)->FileAction {
        let at=position(&doc.text,doc.text.find("input-channel='orders'").unwrap()+15);
        owner.file_actions(doc,Range::new(at,at)).into_iter().find(|a|a.title==TITLE).unwrap()
    }
    fn applied(doc:&str,edits:&[TextEdit])->String {let mut out=doc.to_owned();for e in edits.iter().rev(){out.replace_range(offset(doc,e.range.start).unwrap()..offset(doc,e.range.end).unwrap(),&e.text);}out}
    #[test]
    fn imported_channel_and_alias_target_rename_atomically_after_review() {
        let (host,doc)=setup();let mut owner=structure::Structure::default();let action=action(&mut owner,&doc);let replay=action.clone();
        assert!(action.edits.is_empty());host.answer_next_prompt(Some(&["incoming"]));host.answer_next_prompt(Some(&["yes"]));
        let result=owner.resolve_file_action(&doc,action).unwrap();assert_eq!(result.edits.len(),2);assert!(result.operations.is_empty());
        let project=host.invocation().project().unwrap();
        let output:BTreeMap<_,_>=result.edits.iter().map(|(path,edits)|(path.clone(),applied(&project.read_file(path).unwrap(),edits))).collect();
        assert!(output[&doc.path].contains("name='incoming' alias='input'"));
        assert!(output[&doc.path].contains("input-channel='incoming' output-channel='input'"));
        assert!(output["/project/config/channels.xml"].contains("id='incoming'"));
        assert!(output["/project/config/channels.xml"].contains("method='orders'"));
        assert_eq!(project.read_file(&doc.path).unwrap(),doc.text);assert!(owner.resolve_file_action(&doc,replay).is_err());
        assert!(host.process_runs().is_empty()&&host.requests().is_empty());
    }
    #[test]
    fn alias_rename_changes_only_its_declaration_and_direct_users() {
        let (host,mut doc)=setup();doc.text=doc.text.replace("</beans>","<alias name='input' alias='second'/></beans>");host.add_file(&doc.path,&doc.text);let mut owner=structure::Structure::default();
        let at=position(&doc.text,doc.text.find("alias='input'").unwrap()+7);let a=owner.file_actions(&doc,Range::new(at,at)).into_iter().find(|a|a.title==TITLE).unwrap();
        host.answer_next_prompt(Some(&["entry"]));host.answer_next_prompt(Some(&["yes"]));let result=owner.resolve_file_action(&doc,a).unwrap();assert_eq!(result.edits.len(),1);
        let after=applied(&doc.text,&result.edits[0].1);assert!(after.contains("name='orders' alias='entry'"));assert!(after.contains("output-channel='entry'"));assert!(after.contains("name='entry' alias='second'"));assert!(after.contains("input-channel='orders'"));
    }
    #[test]
    fn registered_import_occurrences_follow_alias_chains_without_mutation() {
        let (host,mut doc)=setup();doc.text=doc.text.replace("output-channel='input'","output-channel='second'").replace("</beans>","<alias name='input' alias='second'/></beans>");host.add_file(&doc.path,&doc.text);
        let p=position(&doc.text,doc.text.find("output-channel='second'").unwrap()+16);
        host.open_editor(&doc.path,"xml",&doc.text,&[(p.line,p.character,p.line,p.character)]);
        host.answer_next_prompt(Some(&["site:0"]));let mut owner=structure::Structure::default();
        let result=owner.run_command("integration.import-tree-occurrences",&host.invocation(),None).unwrap().unwrap();assert!(result.contains("alias chains"));
        let tree=read_tree("/project",&doc.path).unwrap();assert_eq!(canonical(&tree,"second"),Some("orders"));
        assert_eq!(host.opened_locations().len(),1);assert_eq!(host.opened_locations()[0].0,"/project/config/channels.xml");
        assert_eq!(host.invocation().project().unwrap().read_file(&doc.path).unwrap(),doc.text);assert!(host.process_runs().is_empty());
    }
    #[test]
    fn import_occurrences_cancel_forged_unknown_and_denied_editor_do_not_navigate() {
        for forged in [false,true] {
            let (host,doc)=setup();let p=position(&doc.text,doc.text.find("input-channel='orders'").unwrap()+15);host.open_editor(&doc.path,"xml",&doc.text,&[(p.line,p.character,p.line,p.character)]);
            if forged {host.answer_next_prompt(Some(&["site:99999"]));}
            let result=occurrences(&host.invocation());if forged {assert!(result.is_err());}else {assert!(result.unwrap().is_none());}assert!(host.opened_locations().is_empty());
        }
        let host=MockHost::install().with_capabilities(&["project.read"]);assert!(occurrences(&host.invocation()).is_err());assert!(host.opened_locations().is_empty());
    }
    #[test]
    fn imports_are_confined_deduplicated_and_forward_only() {
        let (host,doc)=setup();host.add_file("/project/reverse.xml",&format!("<beans xmlns='{BEANS}'><import resource='config/root.xml'/></beans>"));
        let tree=read_tree("/project",&doc.path).unwrap();assert_eq!(tree.files.len(),2);assert!(!tree.files.contains_key("/project/reverse.xml"));
        assert_eq!(imported("/project","/project/config/root.xml","../shared.xml").as_deref(),Some("/project/shared.xml"));
        for value in ["../../outside.xml","classpath:shared.xml","*.xml","/project/x.xml","./x.xml","${location}.xml"] {assert!(imported("/project",&doc.path,value).is_none(),"{value}");}
        let cyc=doc.text.replace("<import resource='channels.xml'/>","<import resource='channels.xml'/><import resource='root.xml'/>");host.add_file(&doc.path,&cyc);assert_eq!(read_tree("/project",&doc.path).unwrap().files.len(),2);
    }
    #[test]
    fn unknown_profile_nested_encoded_and_cross_project_imports_refuse() {
        for variant in 0..6 {
            let (host,doc)=setup();let replacement=match variant {
                0=>format!("<beans xmlns='{BEANS}' profile='dev'/>") ,
                1=>format!("<beans xmlns='{BEANS}'><bean id='worker'/></beans>"),
                2=>format!("<beans xmlns='{BEANS}' xmlns:i='{NAMESPACE}'><i:channel id='ord&#101;rs'/></beans>"),
                3=>format!("<beans xmlns='{BEANS}' xmlns:i='{NAMESPACE}'><i:channel id='orders'><i:queue/></i:channel></beans>"),
                4=>format!("<beans xmlns='{BEANS}'><import resource='missing.xml'/></beans>"),
                _=>{host.add_project("/project/config");format!("<beans xmlns='{BEANS}'/>")}
            };host.add_file("/project/config/channels.xml",&replacement);assert!(read_tree("/project",&doc.path).is_err(),"variant {variant}");
        }
    }
    #[test]
    fn alias_cycles_collisions_cancel_and_stale_source_produce_no_edits() {
        for variant in 0..5 {
            let (host,mut doc)=setup();if variant==0 {doc.text=doc.text.replace("alias='input'","alias='orders'");host.add_file(&doc.path,&doc.text);}
            if variant==1 {doc.text=doc.text.replace("name='orders' alias='input'","name='other' alias='input'").replace("</beans>","<alias name='input' alias='other'/></beans>");host.add_file(&doc.path,&doc.text);}
            let mut owner=structure::Structure::default();let a=action(&mut owner,&doc);
            match variant {2=>{host.answer_next_prompt(Some(&["input"]));},3=>{host.answer_next_prompt(Some(&["new"]));},4=>{host.add_file("/project/config/channels.xml","<changed/>");},_=>{}}
            assert!(owner.resolve_file_action(&doc,a).is_err());assert_eq!(host.invocation().project().unwrap().read_file(&doc.path).unwrap(),doc.text);
        }
    }
}
