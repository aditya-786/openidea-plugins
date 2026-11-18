//! Reviewed single-class NamedInterface annotation, without moving files/types.
use super::*;
use openidea_plugin::file_actions::FileAction;
const TITLE:&str="Expose imported type as a named interface…";
#[derive(Clone,Debug,PartialEq,Eq)]
struct Target {path:String,qualified:String,module:String}
#[derive(Clone)]
struct Pending {root:String,doc:Document,selection:Range,target:Target,open_module:bool}
#[derive(Default)]
pub(super) struct Exposes {next:u64,pending:BTreeMap<String,Pending>}
fn target(index:&Units<'_>,doc:&Document,selection:Range)->Option<Target> {
    if !matches!(doc.language.as_str(),"java"|"kotlin")||is_test(&doc.path)||doc.text.len()>256*1024||!index.fresh()||index.len()>1000 {return None;}
    let root=host::project_of(&doc.path)?.root().ok()?;
    if index.iter().any(|(path,_)|host::project_of(path).and_then(|p|p.root().ok()).as_deref()!=Some(root.as_str())) {return None;}
    let f=spring_text::parse_source(&doc.path,&doc.text);let model=Model::build(index);let (from,_)=model.module_of(&f.package)?;
    let at=offset(&doc.text,selection.start)?;
    let import=f.imports.iter().find(|i|!i.is_static&&!i.path.ends_with(".*")&&i.at<=at&&at<i.end)?;
    let (module,violation)=model.check(&from,&import.path)?;if violation!=Some(Violation::Internal) {return None;}
    let targets:Vec<_>=index.iter().filter(|(path,u)|!is_test(path)&&u.types.iter().any(|(name,_,_)|u.public_types.contains(name)&&format!("{}.{name}",u.package)==import.path)).collect();
    let [(path,_)]=targets.as_slice() else{return None;};
    Some(Target{path:(*path).into(),qualified:import.path.clone(),module:module.name.clone()})
}
fn snapshot(pending:&Pending)->Result<(String,Model),Error> {
    if host::project_of(&pending.doc.path).and_then(|p|p.root().ok()).as_deref()!=Some(pending.root.as_str())||host::project_of(&pending.target.path).and_then(|p|p.root().ok()).as_deref()!=Some(pending.root.as_str()) {return Err(Error::Stale);}
    let project=host::project_of(&pending.doc.path).ok_or(Error::Stale)?;
    if project.read_file(&pending.doc.path)?!=pending.doc.text {return Err(Error::Stale);}
    spring_text::scope::scope_path(&pending.root).ok_or(Error::Stale)?;let mut index=spring_text::index::Index::new(&["**/*.java","**/*.kt"],scan).excluding_nested_projects();index.scan(&project,Duration::from_millis(150));let index=index.view(|unit|Some(unit));
    if target(&index,&pending.doc,pending.selection).as_ref()!=Some(&pending.target) {return Err(Error::Stale);}
    let source=project.read_file(&pending.target.path)?;if source.len()>64*1024 {return Err(Error::Invalid("The imported class is too large to edit (over 64 KiB).".into()));}
    Ok((source,Model::build(&index)))
}
fn insertion(path:&str,source:&str,qualified:&str,name:&str)->Result<(String,TextEdit),Error> {
    let kotlin=path.ends_with(".kt");let file=spring_text::parse_source(path,source);let (package,ty)=qualified.rsplit_once('.').ok_or(Error::Stale)?;
    if file.package!=package||file.types.iter().filter(|t|t.outer.is_none()&&t.name==ty).count()!=1 {return Err(Error::Stale);}
    let decl=file.types.iter().find(|t|t.outer.is_none()&&t.name==ty).ok_or(Error::Stale)?;
    if (!kotlin&&!decl.modifiers.iter().any(|m|m=="public")||kotlin&&decl.modifiers.iter().any(|m|matches!(m.as_str(),"private"|"protected"|"internal")))||decl.annotations.iter().any(|a|a.name=="NamedInterface") {return Err(Error::Invalid("Use a public top-level type without an existing NamedInterface annotation.".into()));}
    // Fully qualify the framework annotation: no imports are replaced or added.
    if file.types.iter().any(|t|t.name=="org")||file.imports.iter().any(|i|i.path.rsplit('.').next()==Some("org")) {return Err(Error::Invalid("The org qualifier is shadowed in this source.".into()));}
    let eol=if source.contains("\r\n") {"\r\n"}else{"\n"};
    let edit=TextEdit{range:range(source,decl.start,decl.start),text:format!("@org.springframework.modulith.NamedInterface(\"{name}\"){eol}")};
    let mut after=source.to_owned();after.insert_str(decl.start,&edit.text);
    let parsed=scan(path,&after).ok_or(Error::Stale)?;
    if !parsed.types.iter().any(|(n,_,named)|n==ty&&named.as_deref()==Some(name)) {return Err(Error::Stale);}
    Ok((after,edit))
}
fn open_module(pending:&Pending,before:&str,model:&Model)->Result<FileAction,Error> {
    let module=model.modules.get(&pending.target.module).ok_or(Error::Stale)?;let package=java::parse(before).package;
    if model.declared_types.contains(&format!("{}.org",module.base)) {return Err(Error::Invalid("The org package qualifier is shadowed by a project type in the module package.".into()));}
    let directory=pending.target.path.rsplit_once('/').ok_or(Error::Stale)?.0;let package_path=package.replace('.',"/");
    let source=directory.strip_suffix(&package_path).filter(|p|p.ends_with('/')).ok_or("Target source path does not establish the declared Java package directory.")?;
    let parent=format!("{source}{}",module.base.replace('.',"/"));let path=format!("{parent}/package-info.java");
    let project=host::project_of(&path).ok_or(Error::Stale)?;if project.root()?!=pending.root||!path.starts_with(&format!("{}/",pending.root))||!project.stat(&parent).is_ok_and(|s|s.kind==openidea_plugin::FileKind::Directory) {return Err(Error::Stale);}
    if project.exists(&path)? {return Err(Error::Invalid("The module package descriptor already exists; it will not be overwritten.".into()));}
    let text=format!("@org.springframework.modulith.ApplicationModule(type = org.springframework.modulith.ApplicationModule.Type.OPEN)\npackage {};\n",module.base);
    if !scan(&path,&text).and_then(|u|u.info).and_then(|i|i.module).is_some_and(|m|m.open) {return Err(Error::Stale);}
    let review=format!("Create module descriptor: {path}\nModule: {}\nOrigin: {} imports {}\n\nThis opens the entire module, exposing all its internal types. It does not move types or alter Java visibility. The descriptor is created only when absent; one workspace undo removes it.\n\nNEW FILE:\n{text}",pending.target.module,pending.doc.path,pending.target.qualified);
    if !ui_kit::prompts::confirm_exact("Open this entire application module?",&review)? {return Err(Error::Cancelled);}
    if snapshot(pending)?.0!=before||project.exists(&path)?||host::project_of(&path).and_then(|p|p.root().ok()).as_deref()!=Some(pending.root.as_str()) {return Err(Error::Stale);}
    Ok(FileAction{title:format!("Open module {}",pending.target.module),kind:Some("quickfix".into()),operations:vec![openidea_plugin::file_actions::FileOperation::Create{path:path.clone(),text,overwrite:false}],open:Some(path),..Default::default()})
}

impl Exposes {
    pub(super) fn actions(&mut self,index:&Units<'_>,doc:&Document,selection:Range)->Vec<FileAction> {
        let Some(target)=target(index,doc,selection) else{return vec![];};let Some(root)=host::project_of(&doc.path).and_then(|p|p.root().ok()) else{return vec![];};
        if host::project_of(&target.path).and_then(|p|p.root().ok()).as_deref()!=Some(root.as_str()) {return vec![];}
        self.next=self.next.wrapping_add(1);if self.pending.len()>=3 {self.pending.clear();}let key=format!("modulith-expose-interface:{}",self.next);
        let model=Model::build(index);let missing_descriptor=target.path.ends_with(".java")&&model.modules.get(&target.module).is_some_and(|module|!index.iter().any(|(path,unit)|path.ends_with("/package-info.java")&&unit.package==module.base));
        self.pending.insert(key.clone(),Pending{root:root.clone(),doc:doc.clone(),selection,target:target.clone(),open_module:false});
        let mut actions=vec![FileAction{title:TITLE.into(),kind:Some("quickfix".into()),data:Some(key.clone()),..Default::default()}];
        if missing_descriptor {let key=format!("{key}:open");self.pending.insert(key.clone(),Pending{root,doc:doc.clone(),selection,target,open_module:true});actions.push(FileAction{title:"Open imported type's module…".into(),kind:Some("quickfix".into()),data:Some(key),..Default::default()});}
        actions
    }
    pub(super) fn resolve(&mut self,doc:&Document,action:FileAction)->Result<FileAction,Error> {
        let pending=self.pending.remove(action.data.as_deref().ok_or(Error::Stale)?).ok_or(Error::Stale)?;if pending.doc!=*doc {return Err(Error::Stale);}
        let (before,model)=snapshot(&pending)?;
        if pending.open_module {return open_module(&pending,&before,&model);}
        if model.declared_types.contains(&format!("{}.org",spring_text::parse_source(&pending.target.path,&before).package)) {return Err(Error::Invalid("The org qualifier is shadowed by a project type in the target package.".into()));}
        let Some(name)=host::input("Name of the new interface exposing only this type",Some("api"))? else{return Err(Error::Cancelled);};
        if name.is_empty()||name.len()>64||!name.bytes().all(|b|b.is_ascii_alphanumeric()||b"_.-".contains(&b))||!name.as_bytes()[0].is_ascii_alphabetic()||model.modules.get(&pending.target.module).is_none_or(|m|m.interface_names().contains(name.as_str())) {return Err(Error::Invalid("Use a new unique interface name (letter first; letters/digits/_.-; at most 64 bytes).".into()));}
        let (after,edit)=insertion(&pending.target.path,&before,&pending.target.qualified,&name)?;
        if snapshot(&pending)?.0!=before {return Err(Error::Stale);}
        let review=format!("Expose one project type: {}\nTarget module: {}\nNamed interface: {}::{}\nSource: {}\n\nThis adds a fully qualified framework annotation to this public type only. Imports, Java visibility, package layout and other types remain unchanged. Calling modules may still need to allow the new named interface.\n\nBEFORE:\n{before}\n\nAFTER:\n{after}",pending.target.qualified,pending.target.module,pending.target.module,name,pending.target.path);
        if !ui_kit::prompts::confirm_exact("Expose this type as a named interface?",&review)? {return Err(Error::Cancelled);}
        if snapshot(&pending)?.0!=before {return Err(Error::Stale);}
        Ok(FileAction{title:format!("Expose {} as {}::{name}",pending.target.qualified,pending.target.module),kind:Some("quickfix".into()),edits:vec![(pending.target.path,vec![edit])],..Default::default()})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::ModulithTools;
    use openidea_plugin::{mock::MockHost,text::{position,offset}};
    fn setup()->(MockHost,ModulithTools,Document) {
        let host=MockHost::install();
        host.add_file("/project/src/app/Main.java","package app; @SpringBootApplication public class Main {}");
        host.add_file("/project/src/app/orders/internal/Repository.java","package app.orders.internal;\r\n\r\n/*😀*/ public class Repository {}\r\n");
        let doc=Document{path:"/project/src/app/shipping/Shipping.java".into(),language:"java".into(),version:2,text:"package app.shipping;\nimport app.orders.internal.Repository;\npublic class Shipping {}".into()};host.add_file(&doc.path,&doc.text);
        let mut tools=ModulithTools::default();tools.warm_project(&host.invocation().project().unwrap(),COMMAND_BUDGET);(host,tools,doc)
    }
    fn selected(doc:&Document)->Range {let p=position(&doc.text,doc.text.find("app.orders.internal.Repository").unwrap()+1);Range::new(p,p)}
    #[test]
    fn expose_returns_reviewed_single_type_edit_without_rewriting_imports_or_files() {
        let (host,mut tools,doc)=setup();let a=tools.expose_actions(&doc,selected(&doc)).remove(0);let replay=a.clone();assert!(a.edits.is_empty());host.answer_next_prompt(Some(&["repository"]));host.answer_next_prompt(Some(&["yes"]));
        let result=tools.resolve_expose(&doc,a).unwrap();assert!(result.operations.is_empty());assert_eq!(result.edits.len(),1);let (path,edits)=&result.edits[0];assert_eq!(path,"/project/src/app/orders/internal/Repository.java");let before=host.invocation().project().unwrap().read_file(path).unwrap();let mut after=before.clone();after.insert_str(offset(&before,edits[0].range.start).unwrap(),&edits[0].text);
        assert!(after.contains("@org.springframework.modulith.NamedInterface(\"repository\")\r\npublic class Repository"));assert!(after.contains("/*😀*/"));assert_eq!(host.invocation().project().unwrap().read_file(path).unwrap(),before);assert!(tools.resolve_expose(&doc,replay).is_err());assert!(host.process_runs().is_empty()&&host.requests().is_empty());
        let unit=scan(path,&after).unwrap();assert_eq!(unit.types[0].2.as_deref(),Some("repository"));
        host.add_file(path,&after);tools.reset_live();tools.warm_project(&host.invocation().project().unwrap(),COMMAND_BUDGET);
        assert!(tools.live_diagnostics(&doc).is_empty(),"The exposed type no longer violates the source model's module boundary when callers have no allowlist.");
    }
    #[test]
    fn stale_foreign_colliding_cancelled_existing_and_nonpublic_targets_refuse() {
        for variant in 0..7 {
            let (host,mut tools,doc)=setup();let a=tools.expose_actions(&doc,selected(&doc)).remove(0);
            match variant {
                0=>host.add_file(&doc.path,"package changed; class Other {}"),
                1=>host.add_project("/project/src/app/orders"),
                2=>{host.answer_next_prompt(Some(&["bad\"name"]));},
                3=>{host.answer_next_prompt(Some(&["repository"]));},
                4=>host.add_file("/project/src/app/orders/internal/Repository.java","package app.orders.internal; @org.springframework.modulith.NamedInterface(\"already\") public class Repository {}"),
                5=>host.add_file("/project/src/app/orders/internal/Repository.java","package app.orders.internal; class Repository {}"),
                _=>{host.answer_next_prompt(Some(&["repository"]));host.answer_next_prompt(None);}
            }
            assert!(tools.resolve_expose(&doc,a).is_err(),"variant {variant}");assert_eq!(host.invocation().project().unwrap().read_file(&doc.path).unwrap(),if variant==0 {"package changed; class Other {}"}else{&doc.text});
        }
    }
    #[test]
    fn missing_module_descriptor_is_created_after_review_without_overwrite() {
        let (host,mut tools,doc)=setup();host.add_dir("/project/src/app/orders");let action=tools.expose_actions(&doc,selected(&doc)).into_iter().find(|a|a.title=="Open imported type's module…").unwrap();
        host.answer_next_prompt(Some(&["yes"]));let resolved=tools.resolve_expose(&doc,action).unwrap();assert!(resolved.edits.is_empty());let [openidea_plugin::file_actions::FileOperation::Create{path,text,overwrite:false}]=resolved.operations.as_slice() else{panic!("one non-overwriting descriptor create");};assert_eq!(path,"/project/src/app/orders/package-info.java");assert!(text.contains("org.springframework.modulith.ApplicationModule.Type.OPEN"));assert!(text.ends_with("package app.orders;\n"));assert!(!host.invocation().project().unwrap().exists(path).unwrap());
        host.add_file(path,text);tools.reset_live();tools.warm_project(&host.invocation().project().unwrap(),COMMAND_BUDGET);assert!(tools.live_diagnostics(&doc).is_empty());
    }
    #[test]
    fn module_descriptor_creation_refuses_existing_path_and_cancelled_review() {
        for existing in [false,true] {
            let (host,mut tools,doc)=setup();host.add_dir("/project/src/app/orders");let action=tools.expose_actions(&doc,selected(&doc)).into_iter().find(|a|a.title=="Open imported type's module…").unwrap();
            if existing {host.add_file("/project/src/app/orders/package-info.java","/* existing */ package app.orders;");}
            assert!(tools.resolve_expose(&doc,action).is_err());let project=host.invocation().project().unwrap();if existing {assert_eq!(project.read_file("/project/src/app/orders/package-info.java").unwrap(),"/* existing */ package app.orders;");}else {assert!(!project.exists("/project/src/app/orders/package-info.java").unwrap());}
        }
    }
    #[test]
    fn generated_framework_qualifiers_refuse_same_package_project_type_shadowing() {
        for open in [false,true] {
            let (host,mut tools,doc)=setup();host.add_dir("/project/src/app/orders");let wanted=if open {"Open imported type's module…"}else{TITLE};let action=tools.expose_actions(&doc,selected(&doc)).into_iter().find(|a|a.title==wanted).unwrap();
            let (path,source)=if open {("/project/src/app/orders/org.java","package app.orders; class org {}") }else{("/project/src/app/orders/internal/org.java","package app.orders.internal; class org {}")};host.add_file(path,source);
            assert!(tools.resolve_expose(&doc,action).is_err());assert!(!host.invocation().project().unwrap().exists("/project/src/app/orders/package-info.java").unwrap());
        }
    }
    #[test]
    fn kotlin_public_type_exposure_uses_original_ranges_and_preserves_imports() {
        let host=MockHost::install();host.add_file("/project/src/app/Main.kt","package app\n@SpringBootApplication class Main");host.add_file("/project/src/app/orders/internal/Repository.kt","package app.orders.internal\n/*😀*/ class Repository\n");
        let doc=Document{path:"/project/src/app/shipping/Shipping.kt".into(),language:"kotlin".into(),version:1,text:"package app.shipping\nimport app.orders.internal.Repository\nclass Shipping".into()};host.add_file(&doc.path,&doc.text);let mut tools=ModulithTools::default();tools.warm_project(&host.invocation().project().unwrap(),COMMAND_BUDGET);let action=tools.expose_actions(&doc,selected(&doc)).into_iter().find(|a|a.title==TITLE).unwrap();host.answer_next_prompt(Some(&["repository"]));host.answer_next_prompt(Some(&["yes"]));let resolved=tools.resolve_expose(&doc,action).unwrap();let (path,edits)=&resolved.edits[0];assert!(path.ends_with("Repository.kt"));let before=host.invocation().project().unwrap().read_file(path).unwrap();let mut after=before.clone();after.insert_str(offset(&before,edits[0].range.start).unwrap(),&edits[0].text);assert!(after.contains("@org.springframework.modulith.NamedInterface"));assert!(after.contains("/*😀*/"));host.add_file(path,&after);tools.reset_live();tools.warm_project(&host.invocation().project().unwrap(),COMMAND_BUDGET);assert!(tools.live_diagnostics(&doc).is_empty());
    }
    #[test]
    fn typed_annotation_and_namespace_shadowing_are_not_overwritten() {
        for source in ["package app.orders.internal; @NamedInterface public class Repository {}","package app.orders.internal; public class Repository {} class org {}"] {
            assert!(insertion("Type.java",source,"app.orders.internal.Repository","repository").is_err());
        }
        assert!(insertion("Type.java","package other; public class Repository {}","app.orders.internal.Repository","repository").is_err());
    }
}
