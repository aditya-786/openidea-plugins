//! Per-invocation process cwd and resource-key correlation, never a last-root fallback.
use crate::*;
use std::cell::RefCell;
use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;
use std::rc::Rc;
#[derive(Default)]
struct State {root:Option<String>,depth:usize}
#[derive(Default)]
pub(crate) struct Binding(Rc<RefCell<State>>);
pub(crate) struct Guard {state:Rc<RefCell<State>>,previous:State}
impl Drop for Guard {fn drop(&mut self){*self.state.borrow_mut()=std::mem::take(&mut self.previous);}}
impl Binding {
    fn enter(&self,root:Option<String>)->Guard {
        let depth=self.0.borrow().depth.saturating_add(1);
        let previous=self.0.replace(State{root,depth});Guard{state:self.0.clone(),previous}
    }
    pub(crate) fn root(&self)->Option<String>{self.0.borrow().root.clone()}
    fn active_owned(&self)->bool {let state=self.0.borrow();state.depth!=0&&state.root.is_some()}
}
#[derive(Default)]
pub(crate) struct Resources {
    owner:Option<Option<String>>,
    choices:(Option<String>,Option<String>,Option<String>),
    epoch:u64,
    hasher:RandomState,
}
impl Resources {
    fn bind(&mut self,root:Option<String>,choices:(Option<String>,Option<String>,Option<String>))->bool {
        let changed=self.owner.as_ref().is_some_and(|old|old!=&root||self.choices!=choices);
        if changed {self.epoch=self.epoch.wrapping_add(1);self.hasher=RandomState::new();}
        self.owner=Some(root);self.choices=choices;changed
    }
    fn proof(&self,key:&str)->String {
        format!("{:016x}{:016x}",self.hasher.hash_one((self.epoch,0u8,key)),self.hasher.hash_one((self.epoch,1u8,key)))
    }
    fn key(&self,bare:&str)->String {format!("{bare}~{}",self.proof(bare))}
}
impl KubeExplorer {
    pub(crate) fn with_global_process_scope<T>(&mut self,work:impl FnOnce(&mut Self)->Result<T,Error>)->Result<T,Error>{
        let _guard=self.process_scope.enter(None);work(self)
    }
    pub(crate) fn with_process_scope<T>(&mut self,ctx:&Invocation,work:impl FnOnce(&mut Self)->Result<T,Error>)->Result<T,Error> {
        let root=if ctx.project().is_some(){match forward::owner(Some(ctx)) {
            Ok((_,root))=>Some(root),Err(error)=>{self.expire_rich_context(Some(ctx));return Err(error)},
        }}else{if self.process_scope.active_owned(){return Err(Error::Stale);}None};
        if self.resource_scope.bind(root.clone(),(self.context.clone(),self.namespace.clone(),config::kubeconfig())) {
            self.children.clear();self.expanded.clear();self.current_context=None;self.kubectl_failed=false;
            self.selected=None;self.confirm_delete=false;self.rich_callback=None;self.rich_fields.get_mut().clear();
        }
        let _guard=self.process_scope.enter(root);
        work(self)
    }
    pub(crate) fn process(&self,program:&str,args:&[String],stdin:Option<&str>)->Result<openidea_plugin::ProcessOutput,Error> {
        if let Some(root)=self.process_scope.root() {
            let project=host::projects()?.into_iter().find(|p|p.root().ok().as_deref()==Some(root.as_str())).ok_or(Error::Stale)?;
            project.run_process(program,args,stdin)
        }else{host::run_process(program,args,stdin)}
    }
    pub(crate) fn check_editor_owner(&self,path:&str)->Result<(),Error> {
        if let Some(root)=self.process_scope.root() {
            if host::project_of(path).and_then(|p|p.root().ok()).as_deref()!=Some(root.as_str()) {return Err(Error::Stale);}
        }
        Ok(())
    }
    pub(crate) fn service_action_key(&self, payload:&str, action:&str)->String {
        format!("service-action:{}:{action}", self.resource_scope.proof(&format!("{payload}:{action}")))
    }
    pub(crate) fn resource_key(&self,kind:&str,row:&str)->String {
        let bare=format!("res:{kind}/{row}");
        if self.process_scope.root().is_some(){self.resource_scope.key(&bare)}else{bare}
    }
    pub(crate) fn retire_tree_callbacks(&mut self) {
        self.resource_scope.epoch=self.resource_scope.epoch.wrapping_add(1);
        self.resource_scope.hasher=RandomState::new();
    }
    pub(crate) fn node_key(&self,bare:&str)->String {
        if self.process_scope.root().is_some(){format!("node:{}",self.resource_scope.key(bare))}else{bare.into()}
    }
    pub(crate) fn protect_tree_view(&self,mut view:View)->View {
        if self.process_scope.root().is_none(){return view;}
        if let Body::Tree(items)=&mut view.body {for item in items {
            if !item.key.starts_with("res:")&&!item.key.starts_with("choice:")&&!item.key.starts_with("node:"){item.key=self.node_key(&item.key);}
            if let Some(parent)=&mut item.parent {if !parent.starts_with("res:")&&!parent.starts_with("choice:")&&!parent.starts_with("node:"){*parent=self.node_key(parent);}}
        }}
        view
    }
    pub(crate) fn checked_tree_key(&self,key:&str)->Result<String,Error> {
        let key=self.service_payload(key)?;
        if key.len()>1024{return Err(Error::Stale);}
        if let Some(payload)=key.strip_prefix("node:"){return Ok(self.checked_owned_key(payload)?.into());}
        if key.starts_with("res:")||key.starts_with("choice:")||self.process_scope.root().is_none(){return Ok(key.into());}
        Err(Error::Stale)
    }
    pub(crate) fn checked_tree_event(&self,event:ViewEvent)->Result<ViewEvent,Error> {
        match event {
            ViewEvent::Expand(key)=>Ok(ViewEvent::Expand(self.checked_tree_key(&key)?)),
            ViewEvent::Collapse(key)=>Ok(ViewEvent::Collapse(self.checked_tree_key(&key)?)),
            ViewEvent::Select(key)=>Ok(ViewEvent::Select(self.checked_tree_key(&key)?)),
            ViewEvent::Activate(key)=>Ok(ViewEvent::Activate(self.checked_tree_key(&key)?)),
            other=>Ok(other),
        }
    }
    pub(crate) fn choice_key(&self,parent:&str,name:&str)->String {
        let bare=format!("{parent}/{name}");
        if self.process_scope.root().is_some(){format!("choice:{}",self.resource_scope.key(&bare))}else{bare}
    }
    fn checked_owned_key<'a>(&self,key:&'a str)->Result<&'a str,Error> {
        let root=self.process_scope.root().ok_or(Error::Stale)?;
        if self.resource_scope.owner.as_ref()!=Some(&Some(root)){return Err(Error::Stale);}
        let (bare,proof)=key.rsplit_once('~').ok_or(Error::Stale)?;
        if proof!=self.resource_scope.proof(bare){return Err(Error::Stale);}
        Ok(bare)
    }
    pub(crate) fn checked_choice(&self,key:&str)->Result<Option<(String,String)>,Error> {
        let key=self.service_payload(key)?;
        if key.len()>1024{return Err(Error::Stale);}
        let bare=if let Some(payload)=key.strip_prefix("choice:"){self.checked_owned_key(payload)?}else{
            if !key.starts_with("ctx/")&&!key.starts_with("ns/"){return Ok(None);}
            if self.process_scope.root().is_some(){return Err(Error::Stale);}
            key
        };
        let (parent,name)=bare.split_once('/').ok_or(Error::Stale)?;
        if !matches!(parent,"ctx"|"ns")||name.is_empty()||name.chars().any(char::is_control)
            ||parent=="ns"&&name!=ALL_NAMESPACES&&!is_dns_label(name){return Err(Error::Stale);}
        Ok(Some((parent.into(),name.into())))
    }
    pub(crate) fn details_proof(&self,nonce:u64)->String {self.resource_scope.proof(&format!("details:{nonce}"))}
    pub(crate) fn checked_resource(&self,key:&str)->Result<Option<(String,String)>,Error> {
        let key=self.service_payload(key)?;
        if !key.starts_with("res:"){return Ok(None);}
        if key.len()>1024{return Err(Error::Stale);}
        if self.process_scope.root().is_some() {
            Ok(resource(self.checked_owned_key(key)?))
        }else{
            if key.contains('~'){return Err(Error::Stale);}
            Ok(resource(key))
        }
    }
    #[cfg(test)]
    pub(crate) fn test_node_key(&self,bare:&str,ctx:&Invocation)->String {if forward::owner(Some(ctx)).is_ok(){format!("node:{}",self.resource_scope.key(bare))}else{bare.into()}}
    #[cfg(test)]
    pub(crate) fn test_choice_key(&self,bare:&str,ctx:&Invocation)->String{if forward::owner(Some(ctx)).is_ok(){format!("choice:{}",self.resource_scope.key(bare))}else{bare.into()}}
    #[cfg(test)]
    pub(crate) fn test_resource_key(&self,bare:&str,ctx:&Invocation)->String{if forward::owner(Some(ctx)).is_ok(){self.resource_scope.key(bare)}else{bare.into()}}
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{Plugin,ProcessOutput,mock::MockHost,views::{Views,ViewEvent,Body},details::Details as _};
    fn setup()->(MockHost,KubeExplorer) {
        let h=MockHost::install().with_capabilities(&["project.read","editor.read","editor.write","storage","process:kubectl","process:helm"]);
        h.set_setting("kubeconfig","\"configs/relative-config\"");
        h.on_process(KUBECTL,|args,_| {
            let stdout=if args.iter().any(|a|a=="current-context"){ "dev" }
                else if args.iter().any(|a|a=="config"){r#"{"contexts":[{"name":"dev","context":{"cluster":"local","namespace":"shop"}}],"clusters":[{"name":"local","cluster":{"server":"https://dev.invalid"}}]}"#}
                else if args.iter().any(|a|a=="describe"){ "resource metadata" }
                else if args.iter().any(|a|a=="get")&&args.iter().any(|a|a=="json"){r#"{"apiVersion":"v1","kind":"Secret","metadata":{"name":"demo","namespace":"shop","resourceVersion":"1"},"data":{"password":"b2xk"}}"#}
                else if args.iter().any(|a|a=="get")&&args.iter().any(|a|a=="yaml"){ "apiVersion: v1\nkind: Pod\nmetadata:\n  name: web\n  namespace: shop\n" }
                else if args.iter().any(|a|a=="namespaces"){ "namespace/default\nnamespace/shop\n" }
                else if args.iter().any(|a|a=="secrets"){ "secret/demo\n" }
                else if args.iter().any(|a|a=="get"){ "pod/web\n" }
                else { "ok" };
            Ok(ProcessOutput{status:Some(0),stdout:stdout.into(),stderr:String::new()})
        });
        h.on_process(HELM,|_,_|Ok(ProcessOutput{status:Some(0),stdout:"repositories: []\n".into(),stderr:String::new()}));
        let mut kube=KubeExplorer{context:Some("dev".into()),namespace:Some("shop".into()),..Default::default()};
        kube.new_rich_callback();(h,kube)
    }
    fn row(kube:&mut KubeExplorer,h:&MockHost,kind:&str,name:&str)->String {
        kube.render(TREE,None,&h.invocation()).unwrap();
        kube.handle(TREE,ViewEvent::Expand(kube.test_node_key(&format!("kind:{kind}"),&h.invocation())),&h.invocation()).unwrap();
        let Body::Tree(items)=kube.render(TREE,None,&h.invocation()).unwrap().body else{panic!("tree")};
        items.into_iter().find(|i|test_resource(&i.key)==Some((kind.into(),name.into()))).unwrap().key
    }
    #[test]
    fn exported_browser_secret_and_namespace_discovery_share_the_current_project_cwd() {
        let (h,mut kube)=setup();h.set_setting("allowRevealSecrets","true");
        let key=row(&mut kube,&h,"pods","web");
        kube.run_command("resource-describe",&h.invocation(),Some(&key)).unwrap();
        let secret=row(&mut kube,&h,"secrets","demo");
        kube.run_command("resource-show-secret",&h.invocation(),Some(&secret)).unwrap();
        kube.run_command("resource-edit-secret",&h.invocation(),Some(&secret)).unwrap();
        kube.handle(PANEL,ViewEvent::Field(kube.test_control_key("secret-edit:password"),"changed".into()),&h.invocation()).unwrap();
        h.answer_next_prompt(Some(&[]));
        kube.handle(PANEL,ViewEvent::Button(kube.test_control_key("submit-secret-edit")),&h.invocation()).unwrap();
        kube.run_command("customize-namespaces",&h.invocation(),None).unwrap();
        kube.run_command("cache-cluster-namespaces",&h.invocation(),None).unwrap();
        assert!(h.process_runs().iter().all(|r|r.cwd.as_deref()==Some("/project")),"{:?}",h.process_runs());
        assert!(h.process_runs().iter().all(|r|r.args.iter().any(|a|a=="--kubeconfig=configs/relative-config")));
        assert!(kube.process_scope.root().is_none());
    }
    #[test]
    fn source_manifest_kustomize_helm_and_reload_commands_use_the_editor_owner() {
        let (h,mut kube)=setup();h.add_file("charts/app/Chart.yaml","name: app\n");
        h.open_editor("/project/charts/app/templates/pod.yaml","yaml","apiVersion: v1\nkind: Pod\nmetadata:\n  name: web\n  namespace: shop\n",&[]);
        host::storage::set("helm-values","values/relative.yaml").unwrap();
        for id in ["apply","dry-run","diff","kustomize-render","kustomize-apply","helm-template","helm-lint","helm-dependency-update","reload-from-cluster"] {
            if matches!(id,"apply"|"kustomize-apply") {h.answer_next_prompt(Some(&["apply"]));}
            kube.run_command(id,&h.invocation(),None).unwrap();assert!(kube.process_scope.root().is_none());
        }
        assert!(h.process_runs().iter().all(|r|r.cwd.as_deref()==Some("/project")),"{:?}",h.process_runs());
        assert!(h.process_runs().iter().any(|r|r.program==HELM&&r.args.contains(&"--values=values/relative.yaml".into())));
    }
    #[test]
    fn top_level_known_project_denial_refuses_source_and_cached_browser_before_process_or_edit() {
        let (h,mut kube)=setup();
        let key=row(&mut kube,&h,"pods","web");
        h.add_file("charts/app/Chart.yaml","name: app\n");
        h.open_editor("/project/charts/app/templates/pod.yaml","yaml","apiVersion: v1\nkind: Pod\nmetadata:\n  name: web\n",&[]);
        let invocation=h.invocation();assert!(invocation.project().is_some());
        let h=h.with_capabilities(&["editor.read","editor.write","storage","process:kubectl","process:helm"]);
        let count=h.process_runs().len();let text=h.editor_text();
        for id in ["apply","dry-run","helm-template","helm-lint","reload-from-cluster","resource-describe"] {
            assert!(matches!(kube.run_command(id,&invocation,Some(&key)),Err(Error::Denied(_))),"{id}");
            assert_eq!(h.process_runs().len(),count);assert_eq!(h.editor_text(),text);
        }
        assert!(matches!(kube.handle(TREE,ViewEvent::Select(key.clone()),&invocation),Err(Error::Denied(_))));
        assert!(matches!(kube.detail(TREE,&key,&invocation),Err(Error::Denied(_))));
        assert_eq!(h.process_runs().len(),count);assert_eq!(h.editor_text(),text);
        assert!(kube.process_scope.root().is_none());
    }
    #[test]
    fn services_view_denied_shows_row() {
        let (h,mut kube)=setup();
        let invocation=h.invocation();
        let h=h.with_capabilities(&["storage","process:kubectl"]);
        let rendered=kube.render(TREE,None,&invocation).unwrap();
        let Body::Tree(rows)=rendered.body else{panic!("a status row, not a message")};
        assert_eq!(rows.len(),1);
        assert!(rows[0].detail.as_deref().is_some_and(|detail|detail.contains("project.read")));
        assert!(h.process_runs().is_empty());
    }
    #[test]
    fn rendering_a_denied_invoking_project_is_inert_without_global_process_fallback() {
        let (h,mut kube)=setup();
        let key=row(&mut kube,&h,"pods","web");
        let invocation=h.invocation();
        let h=h.with_capabilities(&["storage","process:kubectl"]);
        let before=h.process_runs().len();
        let rendered=kube.render(PANEL,None,&invocation).unwrap();
        let Body::Message(message)=rendered.body else{panic!("denied views must be inert")};
        assert!(message.contains("project.read"));
        assert!(rendered.toolbar.is_empty());
        assert_eq!(h.process_runs().len(),before);
        let rendered=kube.render(TREE,None,&invocation).unwrap();
        assert!(matches!(rendered.body,Body::Tree(ref rows) if rows.len()==1));
        assert!(rendered.toolbar.is_empty());
        assert_eq!(h.process_runs().len(),before);
        assert!(matches!(kube.handle(TREE,ViewEvent::Select(key),&invocation),Err(Error::Denied(_))));
        assert!(kube.process_scope.root().is_none());
        assert_eq!(h.process_runs().len(),before);
    }

    #[test]
    fn foreign_and_nested_editor_owners_refuse_before_source_process_or_edit() {
        let (h,mut kube)=setup();h.add_project("/project/nested");
        for path in ["/foreign/pod.yaml","/project/nested/pod.yaml"] {
            h.open_editor(path,"yaml","apiVersion: v1\nkind: Pod\nmetadata:\n  name: web\n",&[]);
            let text=h.editor_text();let count=h.process_runs().len();
            for id in ["apply","dry-run","diff","kustomize-apply","helm-template","reload-from-cluster"] {
                assert!(matches!(kube.run_command(id,&h.invocation(),None),Err(Error::Stale)),"{path} {id}");
                assert!(kube.process_scope.root().is_none());
            }
            assert_eq!(h.process_runs().len(),count);assert_eq!(h.editor_text(),text);
        }
    }
    #[test]
    fn raii_binding_restores_nested_error_panic_and_unbound_modes_without_last_root_fallback() {
        let (h,mut kube)=setup();h.add_project("/project");
        kube.with_process_scope(&h.invocation(),|outer| {
            outer.process(KUBECTL,&["first".into()],None)?;
            h.set_project_root(Some("/other"));
            let result:Result<(),Error>=outer.with_process_scope(&h.invocation(),|inner| {
                inner.process(KUBECTL,&["nested".into()],None)?;Err(Error::Failed("injected".into()))
            });
            assert!(result.is_err());assert_eq!(outer.process_scope.root().as_deref(),Some("/project"));
            outer.process(KUBECTL,&["restored".into()],None)?;Ok(())
        }).unwrap();
        assert!(kube.process_scope.root().is_none());
        h.set_project_root(None);
        kube.with_process_scope(&h.invocation(),|global|global.process(KUBECTL,&["unbound".into()],None).map(|_|())).unwrap();
        h.set_project_root(Some("/project"));
        let panic=std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _:Result<(),Error>=kube.with_process_scope(&h.invocation(),|_|panic!("injected unwind"));
        }));assert!(panic.is_err());assert!(kube.process_scope.root().is_none());
        kube.process(KUBECTL,&["global-after-error".into()],None).unwrap();
        let runs=h.process_runs();let scoped:Vec<_>=runs.iter().map(|r|r.cwd.as_deref()).collect();
        assert_eq!(scoped,[Some("/project"),Some("/other"),Some("/project"),None,None]);
    }
    #[test]
    fn owned_nested_requests_cannot_downgrade_to_global_after_identity_or_grant_loss() {
        let (h,mut kube)=setup();
        kube.with_process_scope(&h.invocation(),|outer| {
            h.set_project_root(None);
            let result=outer.with_process_scope(&h.invocation(),|inner|inner.process(KUBECTL,&["must-not-run".into()],None));
            assert!(matches!(result,Err(Error::Stale)));assert_eq!(outer.process_scope.root().as_deref(),Some("/project"));Ok(())
        }).unwrap();
        let (h,mut kube)=setup();
        kube.with_process_scope(&h.invocation(),|outer| {
            let h=h.with_capabilities(&["process:kubectl","storage"]);
            assert!(outer.with_process_scope(&h.invocation(),|inner|inner.process(KUBECTL,&["must-not-run".into()],None)).is_err());
            assert!(h.process_runs().is_empty());Ok(())
        }).unwrap();assert!(kube.process_scope.root().is_none());
    }
    #[test]
    fn actual_resource_keys_reject_same_string_callbacks_across_project_and_global_switches() {
        let (h,mut kube)=setup();let old=row(&mut kube,&h,"pods","web");
        kube.handle(TREE,ViewEvent::Select(old.clone()),&h.invocation()).unwrap();
        h.add_project("/project");h.set_project_root(Some("/other"));let count=h.process_runs().len();
        for action in ["resource-delete","resource-logs","resource-describe"] {assert!(matches!(kube.run_command(action,&h.invocation(),Some(&old)),Err(Error::Stale)));}
        assert!(matches!(kube.handle(TREE,ViewEvent::Select(old.clone()),&h.invocation()),Err(Error::Stale)));
        assert!(matches!(kube.detail(TREE,&old,&h.invocation()),Err(Error::Stale)));
        assert!(kube.selected.is_none());assert!(h.terminals().is_empty());assert_eq!(h.process_runs().len(),count);
        let current=row(&mut kube,&h,"pods","web");assert_ne!(current,old);assert_eq!(test_resource(&current),test_resource(&old));
        kube.handle(TREE,ViewEvent::Select(current.clone()),&h.invocation()).unwrap();
        let forged=current.replace("res:pods/web~","res:pods/other~");assert!(matches!(kube.handle(TREE,ViewEvent::Select(forged),&h.invocation()),Err(Error::Stale)));
        assert!(matches!(kube.run_command("resource-delete",&h.invocation(),Some(&old)),Err(Error::Stale)));
        h.set_project_root(None);
        assert!(matches!(kube.run_command("resource-delete",&h.invocation(),Some(&current)),Err(Error::Stale)));
        kube.handle(TREE,ViewEvent::Expand(kube.test_node_key("kind:pods",&h.invocation())),&h.invocation()).unwrap();
        let Body::Tree(items)=kube.render(TREE,None,&h.invocation()).unwrap().body else{panic!("global tree")};
        assert!(items.iter().any(|i|i.key=="svc-pod:res:pods/web"));
        assert!(h.process_runs().last().unwrap().cwd.is_none());
    }
    #[test]
    fn context_namespace_and_config_choice_changes_retire_old_resource_key_proofs() {
        for change in 0..3 {
            let (h,mut kube)=setup();let old=row(&mut kube,&h,"pods","web");let count=h.process_runs().len();
            match change {0=>kube.context=Some("another".into()),1=>kube.namespace=Some("another".into()),_=>h.set_setting("kubeconfig","\"configs/another\"")};
            assert!(matches!(kube.run_command("resource-delete",&h.invocation(),Some(&old)),Err(Error::Stale)));
            assert_eq!(h.process_runs().len(),count);assert!(kube.selected.is_none());
        }
    }
    #[test]
    fn closed_captured_owner_never_falls_back_global_inside_a_scoped_request() {
        let (h,mut kube)=setup();
        let result=kube.with_process_scope(&h.invocation(),|owned| {
            h.set_project_root(None);owned.process(KUBECTL,&["must-not-run".into()],None).map(|_|())
        });assert!(matches!(result,Err(Error::Stale)));assert!(h.process_runs().is_empty());assert!(kube.process_scope.root().is_none());
    }
    #[test]
    fn successful_panel_navigation_must_show_the_requested_secret_not_an_old_edit_form() {
        let (h,mut kube)=setup();h.set_setting("allowRevealSecrets","true");h.set_setting("yamlInEditor","false");
        let fail=std::rc::Rc::new(std::cell::Cell::new(false));let failing=fail.clone();
        h.on_process(KUBECTL,move |args,_| {
            let name=if args.iter().any(|arg|arg=="beta"){"beta"}else{"alpha"};
            if failing.get()&&name=="beta"&&args.iter().any(|arg|arg=="json"){return Err(Error::Failed("private-beta-read-error".into()));}
            let stdout=if args.iter().any(|arg|arg=="config") {
                if args.iter().any(|arg|arg=="current-context"){"dev".into()}else{r#"{"contexts":[{"name":"dev","context":{"cluster":"local","namespace":"shop"}}],"clusters":[{"name":"local","cluster":{"server":"https://dev.invalid"}}]}"#.into()}
            }else if args.iter().any(|arg|arg=="logs"){"2026-11-09T00:00:00Z pod-current\n".into()}
            else if args.iter().any(|arg|arg=="yaml"){format!("apiVersion: v1\nkind: Secret\nmetadata:\n  name: {name}\n  namespace: shop\ndata:\n  password: YmV0YQ==\n")}
            else if args.iter().any(|arg|arg=="json") {
                serde_json::json!({"apiVersion":"v1","kind":"Secret","metadata":{"name":name,"namespace":"shop","resourceVersion":"1"},"data":{"password":secrets::b64_encode(format!("{name}-remote").as_bytes())}}).to_string()
            }else if args.iter().any(|arg|arg=="secrets") {"secret/alpha\nsecret/beta\n".into()}else if args.iter().any(|arg|arg=="get"){"pod/web\n".into()}else{"metadata".into()};
            Ok(ProcessOutput{status:Some(0),stdout,stderr:String::new()})
        });
        let alpha=row(&mut kube,&h,"secrets","alpha");let beta=row(&mut kube,&h,"secrets","beta");
        kube.run_command("resource-edit-secret",&h.invocation(),Some(&alpha)).unwrap();
        let _=kube.render_in(PANEL,None,&h.invocation()).unwrap();
        kube.handle(PANEL,ViewEvent::Field(kube.test_control_key("secret-edit:password"),"alpha-unsaved".into()),&h.invocation()).unwrap();
        let old_save=kube.test_control_key("submit-secret-edit");
        fail.set(true);assert!(kube.run_command("resource-show-secret",&h.invocation(),Some(&beta)).is_err());
        let failed=kube.render_in(PANEL,None,&h.invocation()).unwrap();let Body::Form(fields)=failed.body else{panic!("retained form")};
        assert!(fields.iter().any(|field|field.value==openidea_plugin::views::FieldValue::Multiline("alpha-unsaved".into())));
        fail.set(false);assert!(matches!(kube.run_command("resource-show-secret",&h.invocation(),Some(&beta)),Err(Error::Invalid(_))));
        let retained=kube.render_in(PANEL,None,&h.invocation()).unwrap();let Body::Form(fields)=retained.body else{panic!("dirty form retained")};
        assert!(fields.iter().any(|field|field.value==openidea_plugin::views::FieldValue::Multiline("alpha-unsaved".into())));
        kube.handle(PANEL,ViewEvent::Button(kube.test_control_key("back")),&h.invocation()).unwrap();
        // A clean form may be replaced by the requested reveal.
        kube.run_command("resource-edit-secret",&h.invocation(),Some(&alpha)).unwrap();
        kube.run_command("resource-show-secret",&h.invocation(),Some(&beta)).unwrap();
        let view=kube.render_in(PANEL,None,&h.invocation()).unwrap();
        assert!(view.status.as_deref().is_some_and(|status|status.starts_with("Secret beta:")),"requested beta but rendered {:?}",view.status);
        assert!(kube.editing_secret.is_none());assert!(!view.toolbar.iter().any(|button|button.label=="Save"));
        let before=h.process_runs().len();assert!(matches!(kube.handle(PANEL,ViewEvent::Button(old_save),&h.invocation()),Err(Error::Stale)));assert_eq!(h.process_runs().len(),before);
        kube.run_command("resource-yaml",&h.invocation(),Some(&beta)).unwrap();
        assert!(matches!(kube.render_in(PANEL,None,&h.invocation()).unwrap().body,Body::Html(_)));assert!(kube.revealed.is_none());
        let pod=row(&mut kube,&h,"pods","web");kube.run_command("resource-log-view",&h.invocation(),Some(&pod)).unwrap();
        assert!(kube.details.is_none()&&kube.log_view.is_some());
        let browser=kube.render(TREE,None,&h.invocation()).unwrap();
        let Body::Tree(items)=browser.body else{panic!("browser tree")};
        kube.handle(TREE,ViewEvent::Select(items[0].key.clone()),&h.invocation()).unwrap();
        let browser=kube.render(TREE,None,&h.invocation()).unwrap();let create=browser.toolbar.iter().find(|button|button.label=="Create Secret").unwrap().id.clone();
        kube.handle(TREE,ViewEvent::Button(create),&h.invocation()).unwrap();assert!(kube.draft.is_some()&&kube.log_view.is_none());
        kube.run_command("resource-describe",&h.invocation(),Some(&pod)).unwrap();
        assert!(kube.details.is_some()&&kube.draft.is_none()&&kube.revealed.is_none()&&kube.editing_secret.is_none());
    }
    #[test]
    fn emitted_tree_keys_never_exceed_the_pinned_host_transport_bound() {
        let (h,mut kube)=setup();let long="x".repeat(1100);
        let contexts=format!("{long}\n");let pods=format!("pod/{long}\n");
        h.on_process(KUBECTL,move |args,_|Ok(ProcessOutput{status:Some(0),stderr:String::new(),stdout:if args.iter().any(|arg|arg=="get-contexts"){contexts.clone()}else if args.iter().any(|arg|arg=="get"){pods.clone()}else{"dev".into()}}));
        for parent in ["ctx","kind:pods"] {kube.handle(TREE,ViewEvent::Expand(kube.test_node_key(parent,&h.invocation())),&h.invocation()).unwrap();}
        let Body::Tree(items)=kube.render(TREE,None,&h.invocation()).unwrap().body else{panic!("tree")};
        assert!(items.iter().all(|item|item.key.len()<=1024),"native host clips longer tree keys");
        for parent in ["ctx","kind:pods"] {
            assert!(items.iter().any(|item|item.parent.as_deref().is_some_and(|key|test_tree_key(key)==parent)&&item.detail.as_deref().is_some_and(|detail|detail.contains("too long"))));
        }
    }
    #[test]
    fn native_field_key_clipping_cannot_redirect_a_long_secret_key_edit_to_its_prefix() {
        let (h,mut kube)=setup();h.set_setting("allowRevealSecrets","true");
        let key=row(&mut kube,&h,"secrets","demo");
        let short="x".repeat(78);let long=format!("{short}-suffix");
        let response=serde_json::json!({"apiVersion":"v1","kind":"Secret","metadata":{"name":"demo","namespace":"shop","resourceVersion":"1"},"data":{short.clone():"c2hvcnQ=",long.clone():"bG9uZw=="}}).to_string();
        h.on_process(KUBECTL,move |_,_|Ok(ProcessOutput{status:Some(0),stderr:String::new(),stdout:response.clone()}));
        kube.run_command("resource-edit-secret",&h.invocation(),Some(&key)).unwrap();
        let view=kube.render_in(PANEL,None,&h.invocation()).unwrap();let Body::Form(fields)=view.body else{panic!("form")};
        let field=fields.iter().find(|field|field.label==long).unwrap();
        // Pinned host runtime.rs clips native Field.key to128 bytes. All fixture
        // IDs here are ASCII; this models that exact transport without native UI.
        let native_key=field.key[..field.key.len().min(128)].to_owned();
        kube.handle(PANEL,ViewEvent::Field(native_key,"edited-long".into()),&h.invocation()).unwrap();
        let view=kube.render_in(PANEL,None,&h.invocation()).unwrap();let Body::Form(fields)=view.body else{panic!("form")};
        assert_eq!(fields.iter().find(|field|field.label==short).unwrap().value,openidea_plugin::views::FieldValue::Multiline("short".into()));
        assert_eq!(fields.iter().find(|field|field.label==long).unwrap().value,openidea_plugin::views::FieldValue::Multiline("edited-long".into()));
        assert!(fields.iter().all(|field|field.key.len()<=128));
        let unique:std::collections::BTreeSet<_>=fields.iter().map(|field|&field.key).collect();assert_eq!(unique.len(),fields.len());
        let legacy=format!("rich:{}:secret-edit:{short}",kube.rich_callback.as_ref().unwrap());
        assert!(matches!(kube.handle(PANEL,ViewEvent::Field(legacy,"forged".into()),&h.invocation()),Err(Error::Stale)));
    }
    #[test]
    fn old_context_and_namespace_choices_cannot_retarget_a_new_project_with_same_names() {
        let (h,mut kube)=setup();
        h.on_process(KUBECTL,|args,_|Ok(ProcessOutput{status:Some(0),stderr:String::new(),stdout:if args.iter().any(|arg|arg=="get-contexts"){"dev\nshared/path~0123456789abcdef0123456789abcdef\n".into()}else if args.iter().any(|arg|arg=="namespaces"){"namespace/shop\nnamespace/other\n".into()}else if args.iter().any(|arg|arg=="get"){"pod/web\n".into()}else{"dev".into()}}));
        for key in ["ctx","ns"] {kube.handle(TREE,ViewEvent::Expand(kube.test_node_key(key,&h.invocation())),&h.invocation()).unwrap();}
        let Body::Tree(items)=kube.render(TREE,None,&h.invocation()).unwrap().body else{panic!("choices")};
        let old_context=items.iter().find(|item|item.parent.as_deref().is_some_and(|key|test_tree_key(key)=="ctx")&&item.label=="dev").unwrap().key.clone();
        let old_namespace=items.iter().find(|item|item.parent.as_deref().is_some_and(|key|test_tree_key(key)=="ns")&&item.label=="other").unwrap().key.clone();
        let choices=(kube.context.clone(),kube.namespace.clone());let before=h.process_runs().len();
        h.set_project_root(Some("/other"));
        assert!(matches!(kube.handle(TREE,ViewEvent::Activate(old_context.clone()),&h.invocation()),Err(Error::Stale)));
        assert!(matches!(kube.handle(TREE,ViewEvent::Activate(old_namespace.clone()),&h.invocation()),Err(Error::Stale)));
        assert_eq!((kube.context.clone(),kube.namespace.clone()),choices);assert_eq!(h.process_runs().len(),before);
        for key in ["ctx","ns"] {kube.handle(TREE,ViewEvent::Expand(kube.test_node_key(key,&h.invocation())),&h.invocation()).unwrap();}
        let current=row(&mut kube,&h,"pods","web");
        kube.handle(TREE,ViewEvent::Select(current),&h.invocation()).unwrap();let selected=kube.selected.clone();
        assert!(matches!(kube.handle(TREE,ViewEvent::Select(old_namespace),&h.invocation()),Err(Error::Stale)));
        assert_eq!(kube.selected,selected);
        let Body::Tree(items)=kube.render(TREE,None,&h.invocation()).unwrap().body else{panic!("fresh choices")};
        let unusual="shared/path~0123456789abcdef0123456789abcdef";
        let current=items.iter().find(|item|item.parent.as_deref().is_some_and(|key|test_tree_key(key)=="ctx")&&item.label==unusual).unwrap().key.clone();
        let prior_namespace=items.iter().find(|item|item.parent.as_deref().is_some_and(|key|test_tree_key(key)=="ns")&&item.label=="other").unwrap().key.clone();
        kube.handle(TREE,ViewEvent::Select(current.clone()),&h.invocation()).unwrap();
        kube.handle(TREE,ViewEvent::Activate(current),&h.invocation()).unwrap();
        assert_eq!(kube.context.as_deref(),Some(unusual));assert!(kube.namespace.is_none());
        assert!(matches!(kube.handle(TREE,ViewEvent::Activate(prior_namespace),&h.invocation()),Err(Error::Stale)));
        kube.handle(TREE,ViewEvent::Expand(kube.test_node_key("ns",&h.invocation())),&h.invocation()).unwrap();
        let Body::Tree(items)=kube.render(TREE,None,&h.invocation()).unwrap().body else{panic!("new namespace choices")};
        let current=items.iter().find(|item|item.parent.as_deref().is_some_and(|key|test_tree_key(key)=="ns")&&item.label=="other").unwrap().key.clone();
        kube.handle(TREE,ViewEvent::Activate(current),&h.invocation()).unwrap();assert_eq!(kube.namespace.as_deref(),Some("other"));
        h.set_project_root(None);let before=h.process_runs().len();
        assert!(matches!(kube.handle(TREE,ViewEvent::Activate(old_context),&h.invocation()),Err(Error::Stale)));
        kube.handle(TREE,ViewEvent::Expand(kube.test_node_key("ctx",&h.invocation())),&h.invocation()).unwrap();
        let Body::Tree(items)=kube.render(TREE,None,&h.invocation()).unwrap().body else{panic!("global choices")};
        let current=items.iter().find(|item|item.parent.as_deref().is_some_and(|key|test_tree_key(key)=="ctx")&&item.label==unusual).unwrap().key.clone();
        assert_eq!(current,format!("svc-active-context:ctx/{unusual}"));
        kube.handle(TREE,ViewEvent::Activate(current),&h.invocation()).unwrap();assert_eq!(kube.context.as_deref(),Some(unusual));
        assert!(h.process_runs()[before..].iter().all(|run|run.cwd.is_none()));
    }
    #[test]
    fn verified_resource_key_must_not_truncate_an_admitted_name_at_its_proof_delimiter() {
        for name in ["web~canary","web~0123456789abcdef0123456789abcdef","web~a~b"] {
            let (h,mut kube)=setup();let response=format!("pod/{name}\n");
            h.on_process(KUBECTL,move |args,_|Ok(ProcessOutput{status:Some(0),stderr:String::new(),stdout:if args.iter().any(|arg|arg=="get"){response.clone()}else{"metadata".into()}}));
            kube.handle(TREE,ViewEvent::Expand(kube.test_node_key("kind:pods",&h.invocation())),&h.invocation()).unwrap();
            let Body::Tree(items)=kube.render(TREE,None,&h.invocation()).unwrap().body else{panic!("tree")};
            let key=items.iter().find(|item|item.label==name).unwrap().key.clone();
            let before=h.process_runs().len();kube.run_command("resource-describe",&h.invocation(),Some(&key)).unwrap();
            let runs=h.process_runs();let describe=runs[before..].iter().find(|run|run.args.iter().any(|arg|arg=="describe")).unwrap();
            assert!(describe.args.iter().any(|arg|arg==name),"admitted {name} became {:?}",describe.args);
            assert!(!describe.args.iter().any(|arg|arg=="web"));
            h.answer_next_prompt(Some(&[]));
            kube.run_command("resource-delete",&h.invocation(),Some(&key)).unwrap();
            let runs=h.process_runs();let delete=runs.iter().find(|run|run.args.iter().any(|arg|arg=="delete")).unwrap();
            assert!(delete.args.iter().any(|arg|arg==name));assert!(!delete.args.iter().any(|arg|arg=="web"));
        }
    }
    #[test]
    fn old_back_control_cannot_discard_a_fresh_project_form_and_its_unsaved_values() {
        let (h,mut kube)=setup();
        let browser=kube.render(TREE,None,&h.invocation()).unwrap();
        let old_create=browser.toolbar.iter().find(|button|button.label=="Create Secret").unwrap().id.clone();
        kube.handle(TREE,ViewEvent::Button(old_create.clone()),&h.invocation()).unwrap();
        let view=kube.render_in(PANEL,None,&h.invocation()).unwrap();
        let old=view.toolbar.iter().find(|button|button.label=="Back").unwrap().id.clone();
        h.set_project_root(Some("/other"));
        kube.render(TREE,None,&h.invocation()).unwrap();
        kube.handle(PANEL,ViewEvent::Button(kube.test_control_key("create-secret")),&h.invocation()).unwrap();
        kube.handle(PANEL,ViewEvent::Field(kube.test_control_key("secret-value"),"fresh-unsaved-work".into()),&h.invocation()).unwrap();
        let before=h.process_runs().len();
        assert!(matches!(kube.handle(PANEL,ViewEvent::Button(old_create),&h.invocation()),Err(Error::Stale)));
        assert!(matches!(kube.handle(PANEL,ViewEvent::Button(old),&h.invocation()),Err(Error::Stale)));
        assert_eq!(kube.draft.as_ref().unwrap().value,"fresh-unsaved-work");
        let current=kube.test_control_key("back");
        kube.handle(PANEL,ViewEvent::Button(current.clone()),&h.invocation()).unwrap();
        assert!(kube.draft.is_none());
        assert!(matches!(kube.handle(PANEL,ViewEvent::Button(current),&h.invocation()),Err(Error::Stale)));
        assert_eq!(h.process_runs().len(),before);
    }
    #[test]
    fn old_snapshot_controls_cannot_copy_or_refresh_a_new_same_target_snapshot() {
        let (h,mut kube)=setup();
        let h=h.with_capabilities(&["project.read","editor.read","editor.write","storage","process:kubectl","process:helm","clipboard"]);
        let key=row(&mut kube,&h,"pods","web");
        let generation=std::rc::Rc::new(std::cell::Cell::new(1));let emitted=generation.clone();
        h.on_process(KUBECTL,move |args,_|Ok(ProcessOutput{status:Some(0),stderr:String::new(),stdout:if args.iter().any(|arg|arg=="logs"){format!("snapshot-{}\n",emitted.get())}else if args.iter().any(|arg|arg=="current-context"){"dev".into()}else{r#"{"contexts":[{"name":"dev","context":{"cluster":"local","namespace":"shop"}}],"clusters":[{"name":"local","cluster":{"server":"https://dev.invalid"}}]}"#.into()}}));
        kube.run_command("resource-log-view",&h.invocation(),Some(&key)).unwrap();
        let view=kube.render_in(PANEL,None,&h.invocation()).unwrap();
        let old=view.toolbar.iter().find(|button|button.label=="Copy").unwrap().id.clone();
        generation.set(2);kube.run_command("resource-log-view",&h.invocation(),Some(&key)).unwrap();
        let before=h.process_runs().len();
        assert!(matches!(kube.handle(PANEL,ViewEvent::Button(old),&h.invocation()),Err(Error::Stale)));
        assert!(h.clipboard().is_none());assert_eq!(h.process_runs().len(),before);
        let view=kube.render_in(PANEL,None,&h.invocation()).unwrap();
        let copy=view.toolbar.iter().find(|button|button.label=="Copy").unwrap().id.clone();
        let refresh=view.toolbar.iter().find(|button|button.label=="Refresh").unwrap().id.clone();
        kube.handle(PANEL,ViewEvent::Button(copy),&h.invocation()).unwrap();
        assert!(h.clipboard().unwrap().contains("snapshot-2"));
        generation.set(3);kube.handle(PANEL,ViewEvent::Button(refresh.clone()),&h.invocation()).unwrap();
        let accepted=h.process_runs().len();
        assert!(matches!(kube.handle(PANEL,ViewEvent::Button(refresh),&h.invocation()),Err(Error::Stale)));
        assert_eq!(h.process_runs().len(),accepted);
        let retry=kube.test_control_key("log-refresh");
        h.on_process(KUBECTL,|args,_|if args.iter().any(|arg|arg=="logs"){Err(Error::Failed("raw-private-error".into()))}else{Ok(ProcessOutput{status:Some(0),stderr:String::new(),stdout:if args.iter().any(|arg|arg=="current-context"){"dev".into()}else{r#"{"contexts":[{"name":"dev","context":{"cluster":"local","namespace":"shop"}}],"clusters":[{"name":"local","cluster":{"server":"https://dev.invalid"}}]}"#.into()}})});
        assert!(kube.handle(PANEL,ViewEvent::Button(retry.clone()),&h.invocation()).is_err());
        assert_eq!(kube.test_control_key("log-refresh"),retry);
        kube.handle(PANEL,ViewEvent::Button(kube.test_control_key("log-copy")),&h.invocation()).unwrap();
        assert!(h.clipboard().unwrap().contains("snapshot-3"));
    }
    #[test]
    fn rich_create_delete_and_selection_callbacks_retire_before_new_state_can_authorize_them() {
        let (h,mut kube)=setup();
        kube.handle(PANEL,ViewEvent::Button(kube.test_control_key("create-secret")),&h.invocation()).unwrap();
        let view=kube.render_in(PANEL,None,&h.invocation()).unwrap();
        let Body::Form(fields)=view.body else{panic!("create form")};
        let old_field=fields[0].key.clone();let old_submit=view.toolbar.iter().find(|b|b.label=="Create").unwrap().id.clone();
        h.set_project_root(Some("/other"));
        kube.render(TREE,None,&h.invocation()).unwrap();
        kube.handle(PANEL,ViewEvent::Button(kube.test_control_key("create-secret")),&h.invocation()).unwrap();
        let before=h.process_runs().len();
        assert!(matches!(kube.handle(PANEL,ViewEvent::Field(old_field,"stale".into()),&h.invocation()),Err(Error::Stale)));
        assert!(matches!(kube.handle(PANEL,ViewEvent::Button(old_submit),&h.invocation()),Err(Error::Stale)));
        assert!(matches!(kube.handle(PANEL,ViewEvent::Button("submit-secret".into()),&h.invocation()),Err(Error::Stale)));
        assert_eq!(h.process_runs().len(),before);
        for (key,value) in [("secret-name","fresh"),("secret-key","password"),("secret-value","fresh-value")] {
            kube.handle(PANEL,ViewEvent::Field(kube.test_control_key(key),value.into()),&h.invocation()).unwrap();
        }
        kube.handle(PANEL,ViewEvent::Button(kube.test_control_key("submit-secret")),&h.invocation()).unwrap();
        assert!(h.process_runs().last().unwrap().args.iter().any(|arg|arg=="create"));
        assert_eq!(h.process_runs().last().unwrap().cwd.as_deref(),Some("/other"));
        h.open_editor("/other/delete.yaml","yaml","kind: ConfigMap\n",&[]);
        kube.run_command("delete-manifest",&h.invocation(),None).unwrap();
        let old_confirm=kube.test_control_key("confirm-delete-manifest");
        kube.run_command("delete-manifest",&h.invocation(),None).unwrap();let before=h.process_runs().len();
        assert!(matches!(kube.handle(PANEL,ViewEvent::Button(old_confirm),&h.invocation()),Err(Error::Stale)));
        assert_eq!(h.process_runs().len(),before);
        h.answer_next_prompt(Some(&[]));
        kube.handle(PANEL,ViewEvent::Button(kube.test_control_key("confirm-delete-manifest")),&h.invocation()).unwrap();
        let key=row(&mut kube,&h,"pods","web");
        kube.handle(TREE,ViewEvent::Select(key.clone()),&h.invocation()).unwrap();
        let first=kube.test_control_key("delete");
        kube.handle(TREE,ViewEvent::Button(first.clone()),&h.invocation()).unwrap();let before=h.process_runs().len();
        assert!(matches!(kube.handle(TREE,ViewEvent::Button(first),&h.invocation()),Err(Error::Stale)));
        assert_eq!(h.process_runs().len(),before);
        let confirmed=kube.test_control_key("delete");
        kube.handle(TREE,ViewEvent::Select(key),&h.invocation()).unwrap();
        assert!(matches!(kube.handle(TREE,ViewEvent::Button(confirmed),&h.invocation()),Err(Error::Stale)));
        assert_eq!(h.process_runs().len(),before);
    }
    #[test]
    fn old_secret_form_fields_and_submit_cannot_authorize_a_new_same_target_form() {
        for switch_project in [false,true] {
            let (h,mut kube)=setup();h.set_setting("allowRevealSecrets","true");
            let key=row(&mut kube,&h,"secrets","demo");
            kube.run_command("resource-edit-secret",&h.invocation(),Some(&key)).unwrap();
            let view=kube.render_in(PANEL,None,&h.invocation()).unwrap();
            let Body::Form(fields)=view.body else{panic!("edit form")};
            let old_field=fields.iter().find(|field|field.label=="password").unwrap().key.clone();
            let old_submit=view.toolbar.iter().find(|button|button.label=="Save").unwrap().id.clone();
            if switch_project {h.set_project_root(Some("/other"));}
            let key=row(&mut kube,&h,"secrets","demo");
            kube.run_command("resource-edit-secret",&h.invocation(),Some(&key)).unwrap();
            let before=h.process_runs().len();
            assert!(matches!(kube.handle(PANEL,ViewEvent::Field(old_field,"late-old-value".into()),&h.invocation()),Err(Error::Stale)));
            assert!(matches!(kube.handle(PANEL,ViewEvent::Button(old_submit),&h.invocation()),Err(Error::Stale)));
            assert_eq!(h.process_runs().len(),before);
            let view=kube.render_in(PANEL,None,&h.invocation()).unwrap();
            let Body::Form(fields)=view.body else{panic!("current form")};
            assert!(fields.iter().all(|field|field.value!=openidea_plugin::views::FieldValue::Multiline("late-old-value".into())));
            let current_field=fields.iter().find(|field|field.label=="password").unwrap().key.clone();
            let current_save=view.toolbar.iter().find(|button|button.label=="Save").unwrap().id.clone();
            kube.handle(PANEL,ViewEvent::Field(current_field,"current-value".into()),&h.invocation()).unwrap();
            h.answer_next_prompt(Some(&[]));
            kube.handle(PANEL,ViewEvent::Button(current_save.clone()),&h.invocation()).unwrap();
            let accepted=h.process_runs().len();assert!(accepted>before);assert!(kube.editing_secret.is_none());
            assert!(matches!(kube.handle(PANEL,ViewEvent::Button(current_save),&h.invocation()),Err(Error::Stale)));
            assert_eq!(h.process_runs().len(),accepted);
        }
    }
    #[test]
    fn secret_yaml_failed_read_and_apply_diagnostics_are_withheld_while_edits_remain() {
        for process_error in [false,true] {
            let (h,mut kube)=setup();h.set_setting("allowRevealSecrets","true");h.set_setting("yamlInEditor","false");
            let key=row(&mut kube,&h,"secrets","demo");
            h.on_process(KUBECTL,move |_,_|if process_error {Err(Error::Failed("private-yaml-error c2VjcmV0".into()))}else{Ok(ProcessOutput{status:Some(1),stdout:String::new(),stderr:"private-yaml-error c2VjcmV0".into()})});
            let error=format!("{:?}",kube.run_command("resource-yaml",&h.invocation(),Some(&key)).unwrap_err());
            assert!(!error.contains("private-yaml-error")&&!error.contains("c2VjcmV0"));assert!(kube.details.is_none());
            h.on_process(KUBECTL,|_,_|Ok(ProcessOutput{status:Some(0),stdout:"kind: Secret\ndata:\n  password: c2VjcmV0\n".into(),stderr:String::new()}));
            kube.run_command("resource-yaml",&h.invocation(),Some(&key)).unwrap();
            let message=kube.test_apply_message("kind: Secret # edited");
            h.on_process(KUBECTL,move |_,_|if process_error {Err(Error::Failed("private-yaml-error c2VjcmV0".into()))}else{Ok(ProcessOutput{status:Some(1),stdout:String::new(),stderr:"private-yaml-error c2VjcmV0".into()})});
            h.answer_next_prompt(Some(&[]));
            kube.handle(PANEL,ViewEvent::Message(message.clone()),&h.invocation()).unwrap();
            let error=h.view_messages().last().unwrap().1.clone();
            assert!(!error.contains("private-yaml-error")&&!error.contains("c2VjcmV0"),"{error}");
            assert_eq!(kube.test_apply_message("kind: Secret # edited"),message);
        }
    }
    #[test]
    fn revoked_secret_visibility_retires_yaml_cache_and_page_apply_before_any_process() {
        for render_first in [false,true] {
            let (h,mut kube)=setup();h.set_setting("allowRevealSecrets","true");h.set_setting("yamlInEditor","false");
            let key=row(&mut kube,&h,"secrets","demo");
            h.on_process(KUBECTL,|_,_|Ok(ProcessOutput{status:Some(0),stdout:"apiVersion: v1\nkind: Secret\nmetadata:\n  name: demo\n  namespace: shop\ndata:\n  password: cHJpdmF0ZS12YWx1ZQ==\n".into(),stderr:String::new()}));
            kube.run_command("resource-yaml",&h.invocation(),Some(&key)).unwrap();
            let message=kube.test_apply_message("kind: Secret # old page");
            h.set_setting("allowRevealSecrets","false");let before=h.process_runs().len();
            if render_first {
                let view=kube.render(PANEL,None,&h.invocation()).unwrap();
                assert!(!format!("{:?}",view.body).contains("cHJpdmF0ZS12YWx1ZQ=="));
            }
            assert!(matches!(kube.handle(PANEL,ViewEvent::Message(message),&h.invocation()),Err(Error::Stale)));
            assert!(kube.details.is_none());assert_eq!(h.process_runs().len(),before);
        }
    }
    #[test]
    fn secret_write_failures_do_not_disclose_binary_or_process_error_payloads() {
        for process_error in [false,true] {
            let (h,mut kube)=setup();h.set_setting("allowRevealSecrets","true");
            let key=row(&mut kube,&h,"secrets","demo");
            h.on_process(KUBECTL,|_,_|Ok(ProcessOutput{status:Some(0),stdout:r#"{"apiVersion":"v1","kind":"Secret","metadata":{"name":"demo","namespace":"shop","resourceVersion":"1"},"data":{"blob":"/+8=","password":"b2xk"}}"#.into(),stderr:String::new()}));
            kube.run_command("resource-edit-secret",&h.invocation(),Some(&key)).unwrap();
            h.on_process(KUBECTL,move |_,_|if process_error {Err(Error::Failed("private-command-error /+8= old".into()))}else{Ok(ProcessOutput{status:Some(1),stdout:String::new(),stderr:"private-command-error /+8= old".into()})});
            let error=format!("{:?}",kube.handle(PANEL,ViewEvent::Button(kube.test_control_key("submit-secret-edit")),&h.invocation()).unwrap_err());
            assert!(!error.contains("/+8=")&&!error.contains("private-command-error")&&!error.contains("old"),"{error}");
            assert!(kube.editing_secret.is_some());
            for command in ["resource-show-secret","resource-edit-secret"] {
                let error=format!("{:?}",kube.run_command(command,&h.invocation(),Some(&key)).unwrap_err());
                assert!(!error.contains("/+8=")&&!error.contains("private-command-error")&&!error.contains("old"),"{error}");
            }
            let (h,mut kube)=setup();
            kube.handle(PANEL,ViewEvent::Button(kube.test_control_key("create-secret")),&h.invocation()).unwrap();
            for (field,value) in [("secret-name","new-secret"),("secret-key","password"),("secret-value","old")] {
                kube.handle(PANEL,ViewEvent::Field(kube.test_control_key(field),value.into()),&h.invocation()).unwrap();
            }
            h.on_process(KUBECTL,move |_,_|if process_error {Err(Error::Failed("private-command-error old b2xk".into()))}else{Ok(ProcessOutput{status:Some(1),stdout:String::new(),stderr:"private-command-error old b2xk".into()})});
            let error=format!("{:?}",kube.handle(PANEL,ViewEvent::Button(kube.test_control_key("submit-secret")),&h.invocation()).unwrap_err());
            assert!(!error.contains("b2xk")&&!error.contains("private-command-error")&&!error.contains("old"),"{error}");
        }
    }
    #[test]
    fn malformed_secret_base64_cannot_silently_become_an_editable_empty_or_changed_value() {
        for encoded in ["a","YQ","Y=Q=","YQ===","YR==","Y Q=="] {
            let (h,mut kube)=setup();h.set_setting("allowRevealSecrets","true");
            let key=row(&mut kube,&h,"secrets","demo");
            let response=serde_json::json!({"apiVersion":"v1","kind":"Secret","metadata":{"name":"demo","namespace":"shop","resourceVersion":"1"},"data":{"password":encoded}}).to_string();
            h.on_process(KUBECTL,move |_,_|Ok(ProcessOutput{status:Some(0),stdout:response.clone(),stderr:String::new()}));
            assert!(kube.run_command("resource-edit-secret",&h.invocation(),Some(&key)).is_err(),"{encoded}");
            assert!(kube.editing_secret.is_none());
        }
        for (encoded,decoded) in [("",b"".as_slice()),("YQ==\r\n",b"a".as_slice()),("YWI=",b"ab".as_slice()),("YWJj",b"abc".as_slice()),("/+8=",b"\xff\xef".as_slice())] {
            assert_eq!(secrets::b64_decode(encoded),Some(decoded.to_vec()));
        }
    }
    #[test]
    fn chart_directory_default_release_cannot_become_a_helm_template_option() {
        for folder in ["--validate","--dry-run=server","--kube-context=other"] {
            let (h,mut kube)=setup();
            h.add_file(&format!("charts/{folder}/Chart.yaml"),"apiVersion: v2\nname: app\nversion: 1.0.0\n");
            h.open_editor(&format!("/project/charts/{folder}/templates/pod.yaml"),"yaml","kind: Pod\n",&[]);
            assert!(kube.run_command("helm-template",&h.invocation(),None).is_err(),"{folder}");
            assert!(h.process_runs().is_empty());
            // An explicit safe release still permits a chart in that directory;
            // path-only lint/dependency tools do not consume the basename.
            host::storage::set("helm-release","reviewed").unwrap();
            kube.run_command("helm-template",&h.invocation(),None).unwrap();
            kube.run_command("helm-lint",&h.invocation(),None).unwrap();
            assert_eq!(h.process_runs()[0].args[1],"reviewed");
            assert!(h.process_runs().iter().all(|run|run.cwd.as_deref()==Some("/project")));
        }
        let (h,mut kube)=setup();h.add_file("charts/app/Chart.yaml","name: app\n");
        h.open_editor("/project/charts/app/templates/pod.yaml","yaml","kind: Pod\n",&[]);
        host::storage::set("helm-release","--kube-context=other").unwrap();
        assert!(kube.run_command("helm-template",&h.invocation(),None).is_err());
        assert!(h.process_runs().is_empty());
    }
    #[test]
    fn secret_edit_response_must_match_captured_target_and_keep_a_resource_version_guard() {
        for (kind,name,namespace,version) in [("Secret","other","shop","1"),("Secret","demo","foreign","1"),("ConfigMap","demo","shop","1"),("Secret","demo","shop","")] {
            let (h,mut kube)=setup();h.set_setting("allowRevealSecrets","true");
            let key=row(&mut kube,&h,"secrets","demo");
            let response=serde_json::json!({"apiVersion":"v1","kind":kind,"metadata":{"name":name,"namespace":namespace,"resourceVersion":version},"data":{"password":"b2xk"}}).to_string();
            h.on_process(KUBECTL,move |_,_|Ok(ProcessOutput{status:Some(0),stdout:response.clone(),stderr:String::new()}));
            assert!(kube.run_command("resource-edit-secret",&h.invocation(),Some(&key)).is_err(),"{kind} {name} {namespace} {version}");
            assert!(kube.editing_secret.is_none());
            if !version.is_empty() {
                assert!(kube.run_command("resource-show-secret",&h.invocation(),Some(&key)).is_err());
                assert!(kube.revealed.is_none());
            }
            assert!(!h.process_runs().iter().any(|r|r.args.iter().any(|a|a=="replace")));
        }
    }
    #[test]
    fn malformed_or_deep_secret_response_cannot_publish_plaintext_or_an_edit_form() {
        let deep=format!("{{\"ignored\":{}0{},\"data\":{{\"password\":\"b2xk\"}}}}","[".repeat(160),"]".repeat(160));
        for response in [r#"{"data":{"password":"b2xk"} "kind":"Secret"}"#.into(),deep,
            r#"{"data":[]}"#.into(),r#"{"data":{"password":"b2xk","password":"bmV3"}}"#.into(),
            r#"{"data":{"password":"b2xk"},"data":{"other":"bmV3"}}"#.into(),
            format!("{{\"ignored\":\"{}\",\"data\":{{}}}}","x".repeat(2<<20))] {
            for command in ["resource-show-secret","resource-edit-secret"] {
                let (h,mut kube)=setup();h.set_setting("allowRevealSecrets","true");
                let key=row(&mut kube,&h,"secrets","demo");
                let response=response.clone();h.on_process(KUBECTL,move |_,_|Ok(ProcessOutput{status:Some(0),stdout:response.clone(),stderr:String::new()}));
                assert!(kube.run_command(command,&h.invocation(),Some(&key)).is_err(),"{command}");
                assert!(kube.revealed.is_none()&&kube.editing_secret.is_none());
            }
        }
    }
    #[test]
    fn delayed_yaml_page_apply_cannot_authorize_against_a_new_same_project_details_target() {
        let (h,mut kube)=setup();h.set_setting("yamlInEditor","false");
        let key=row(&mut kube,&h,"pods","web");
        kube.run_command("resource-yaml",&h.invocation(),Some(&key)).unwrap();
        // Capture the message format of the actually rendered page, including its
        // callback token when available. The old page has no token at all.
        let Body::Html(page)=kube.render_in(PANEL,None,&h.invocation()).unwrap().body else{panic!("yaml page")};
        let callback=page.split("data-callback=\"").nth(1).and_then(|s|s.split('"').next());
        let message=callback.map_or("apply\nkind: Pod # old page".into(),|token|format!("apply\n{token}\nkind: Pod # old page"));
        // Open a fresh page for the same literal target, retiring the old page.
        kube.run_command("resource-yaml",&h.invocation(),Some(&key)).unwrap();
        let before=h.process_runs().len();
        assert!(matches!(kube.handle(PANEL,ViewEvent::Message(message),&h.invocation()),Err(Error::Stale)));
        assert_eq!(h.process_runs().len(),before);
        let current=kube.test_apply_message("kind: Pod # current page");
        assert!(matches!(kube.handle(PANEL,ViewEvent::Message("apply\nkind: Pod # unproved".into()),&h.invocation()),Err(Error::Stale)));
        assert_eq!(h.process_runs().len(),before);
        h.answer_next_prompt(Some(&[]));
        kube.handle(PANEL,ViewEvent::Message(current.clone()),&h.invocation()).unwrap();
        let accepted=h.process_runs().len();assert!(accepted>before);
        assert!(matches!(kube.handle(PANEL,ViewEvent::Message(current),&h.invocation()),Err(Error::Stale)));
        assert_eq!(h.process_runs().len(),accepted);
    }
    #[test]
    fn captured_single_namespace_row_cannot_expand_to_multiple_or_alternate_resource_targets() {
        for bad in ["web,other","other/web","../web","web%2fother","web\tother"] {
            let (h,mut kube)=setup();let response=format!("pod/{bad}\n");
            h.on_process(KUBECTL,move |_,_|Ok(ProcessOutput{status:Some(0),stdout:response.clone(),stderr:String::new()}));
            let key=row(&mut kube,&h,"pods",bad);let before=h.process_runs().len();
            assert!(kube.run_command("resource-describe",&h.invocation(),Some(&key)).is_err(),"{bad}");
            assert!(kube.detail(TREE,&key,&h.invocation()).is_err(),"{bad}");
            assert_eq!(h.process_runs().len(),before,"{bad}");assert!(h.terminals().is_empty());
        }
        let (_,kube)=setup();assert_eq!(kube.split_scope("clusterroles","system:aggregate-to-admin").unwrap().1,"system:aggregate-to-admin");
    }
    #[test]
    fn malformed_discovered_resource_types_cannot_become_context_override_arguments() {
        let(h,mut kube)=setup();
        h.on_process(KUBECTL,|args,_|Ok(ProcessOutput{status:Some(0),stdout:if args.iter().any(|a|a=="api-resources"){"--context=other.example\ncertificates.cert-manager.io\n".into()}else{"pod/web\n".into()},stderr:String::new()}));
        kube.handle(TREE,ViewEvent::Expand(kube.test_node_key("crds",&h.invocation())),&h.invocation()).unwrap();
        let Body::Tree(items)=kube.render(TREE,None,&h.invocation()).unwrap().body else{panic!("tree")};
        assert!(!items.iter().any(|row|row.key.contains("--context=other")));
        assert!(matches!(kube.children.get("crds"),Some(Err(message)) if message.contains("no resource-type inventory")));
        let count=h.process_runs().len();
        kube.handle(TREE,ViewEvent::Expand(kube.test_node_key("kind:--context=other.example",&h.invocation())),&h.invocation()).unwrap();
        assert_eq!(h.process_runs().len(),count,"untrusted kind must be refused before kubectl");
        assert!(custom_resource_names(vec!["pods.core".into(),"evil=arg.example".into(),"-bad.example".into(),"valid.kind.example".into()]).iter().all(|kind|!kind.contains('=')&&!kind.starts_with('-')));
    }
    #[test]
    fn disconnected_import_and_global_helm_repository_operations_do_not_acquire_project_execution() {
        let (h,mut kube)=setup();h.set_project_root(Some("/"));h.answer_next_prompt(None);
        kube.run_command("import-pasted-kubeconfig",&h.invocation(),None).unwrap();
        assert!(h.process_runs().is_empty());
        kube.run_command("helm-repo-cache",&h.invocation(),None).unwrap();
        kube.run_command("helm-repo-update",&h.invocation(),None).unwrap();
        h.set_project_root(Some("/project"));
        kube.with_process_scope(&h.invocation(),|outer| {
            outer.run_command("helm-repo-cache",&h.invocation(),None)?;
            assert_eq!(outer.process_scope.root().as_deref(),Some("/project"));Ok(())
        }).unwrap();
        assert!(h.process_runs().iter().all(|r|r.program==HELM&&r.cwd.is_none()));
        assert!(kube.process_scope.root().is_none());
    }
}
