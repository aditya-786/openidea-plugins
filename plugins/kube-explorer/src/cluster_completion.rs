//! Explicit positive namespace/resource name snapshots; providers never invoke kubectl.
use crate::{REQUEST_TIMEOUT, forward, manifest};
use openidea_plugin::providers::{CompletionItem, CompletionKind, Document};
use openidea_plugin::{Error, Invocation, Position, Range, host, text};
use std::collections::{BTreeMap,BTreeSet};
use std::time::{Duration, Instant};
const MAX_TEXT: usize = 64 << 10;
const TTL: Duration = Duration::from_secs(60);
#[derive(Default)]
pub(crate) struct Cache { snapshot: Option<Snapshot>, resources:BTreeMap<String,Snapshot> }
struct Snapshot {
    root: String,
    context: Option<String>,
    namespace: Option<String>,
    scope: forward::Scope,
    resolved_namespace:Option<String>,
    names: BTreeSet<String>,
    at: Instant,
}
impl Cache {
    pub(crate) fn clear(&mut self) { self.snapshot = None; self.resources.clear(); }
    pub(crate) fn read(&mut self, ctx: &Invocation, context: Option<&str>, namespace: Option<&str>) -> Result<usize, Error> {
        let (project, root) = forward::owner(Some(ctx))?;
        if self.snapshot.as_ref().is_some_and(|s| s.root == root) { self.snapshot = None; }
        // Namespaces are cluster-scoped. A fixed valid namespace flag avoids
        // interpreting the browser's all-namespaces sentinel as an API name.
        let scope = forward::Scope::capture_for_project(context, Some("default"), &project)?;
        let args = scope.args(&["get", "namespaces", "-o", "name", REQUEST_TIMEOUT]);
        let output = project.run_process("kubectl", &args, None)?;
        if output.status != Some(0) { return Err("Namespace query failed; no new completion snapshot was accepted.".into()); }
        let names = parse(&output.stdout)?;
        if forward::owner(Some(ctx))?.1 != root || !scope.configuration_current()
            || forward::Scope::capture_for_project(context, Some("default"), &project)? != scope { return Err(Error::Stale); }
        let count = names.len();
        self.snapshot = Some(Snapshot { root, context: context.map(str::to_owned), namespace: namespace.map(str::to_owned), scope, resolved_namespace:None, names, at: Instant::now() });
        Ok(count)
    }
    pub(crate) fn read_resources(&mut self,ctx:&Invocation,context:Option<&str>,namespace:Option<&str>)->Result<usize,Error> {
        let (project,root)=forward::owner(Some(ctx))?;
        if namespace==Some(crate::ALL_NAMESPACES){return Err("Select one namespace before reading resource completion names.".into());}
        let items:Vec<_>=RESOURCE_KINDS.iter().map(|(kind,_)|openidea_plugin::PickItem::new(kind,kind)).collect();
        let kind=host::pick("Read resource names for completion (no Secret values)",&items)?.ok_or(Error::Cancelled)?;
        let (_,resource)=RESOURCE_KINDS.iter().find(|(known,_)|*known==kind).ok_or(Error::Stale)?;
        if forward::owner(Some(ctx))?.1!=root {return Err(Error::Stale);}
        if self.resources.get(&kind).is_some_and(|snapshot|snapshot.root==root){self.resources.remove(&kind);}
        let scope=forward::Scope::capture_for_project(context,namespace,&project)?;
        let resolved_namespace=scope.args(&[]).into_iter().find_map(|arg|arg.strip_prefix("--namespace=").map(str::to_owned)).ok_or(Error::Stale)?;
        let out=project.run_process("kubectl",&scope.args(&["get",resource,"-o","name",REQUEST_TIMEOUT]),None).map_err(|error|crate::tool_error("Reading resource names",context,error))?;
        if out.status!=Some(0){return Err(Error::Failed(crate::failure_reason("Reading resource names",context,&out.stderr)));}
        let names=parse_resources(&out.stdout,resource)?;
        if forward::owner(Some(ctx))?.1!=root||!scope.configuration_current()
            ||forward::Scope::capture_for_project(context,namespace,&project)?!=scope{return Err(Error::Stale);}
        let count=names.len();
        self.resources.insert(kind,Snapshot{root,context:context.map(str::to_owned),namespace:namespace.map(str::to_owned),scope,resolved_namespace:Some(resolved_namespace),names,at:Instant::now()});
        Ok(count)
    }
    pub(crate) fn items(&self,doc:&Document,at:Position,context:&Option<String>,namespace:&Option<String>)->Vec<CompletionItem> {
        let mut items=self.namespace_items(doc,at,context,namespace);
        if self.resources.is_empty()||doc.language!="yaml" {return items;}
        let Some((kind,source_namespace,prefix,range,quoted))=crate::indexer::completion_reference(&doc.text,at) else{return items};
        let Some(snapshot)=self.resources.get(&kind) else{return items};
        if snapshot.at.elapsed()>=TTL||&snapshot.context!=context||&snapshot.namespace!=namespace||!snapshot.scope.configuration_current()
            ||snapshot.resolved_namespace.as_deref()!=Some(source_namespace.as_str())
            ||host::project_of(&doc.path).and_then(|p|p.root().ok()).as_deref()!=Some(snapshot.root.as_str()){return items;}
        items.extend(snapshot.names.iter().filter(|name|name.starts_with(&prefix)).take(200).map(|name|{
            let mut item=CompletionItem::new(name,CompletionKind::Reference);item.replace=Some(range);
            item.insert_text=if quoted{name.clone()}else{format!("'{name}'")};
            item.detail=Some(format!("Captured {kind} name · namespace {source_namespace} · {} · {}s old · deployment target unresolved",snapshot.scope.label(),snapshot.at.elapsed().as_secs()));item
        }));items
    }
    fn namespace_items(&self, doc: &Document, at: Position, context: &Option<String>, namespace: &Option<String>) -> Vec<CompletionItem> {
        let Some(s) = &self.snapshot else { return Vec::new() };
        if s.at.elapsed() >= TTL || &s.context != context || &s.namespace != namespace || !s.scope.configuration_current()
            || host::project_of(&doc.path).and_then(|p| p.root().ok()).as_deref() != Some(s.root.as_str()) { return Vec::new(); }
        let Some((prefix, range, quoted)) = field(doc, at) else { return Vec::new() };
        s.names.iter().filter(|name| name.starts_with(&prefix)).take(200).map(|name| {
            let mut item = CompletionItem::new(name, CompletionKind::Value);
            item.replace = Some(range);
            item.insert_text = if quoted { name.clone() } else { format!("'{name}'") };
            item.detail = Some(format!("Captured namespace · {} · {}s old · deployment target unresolved", s.scope.label(), s.at.elapsed().as_secs()));
            item
        }).collect()
    }
}
const RESOURCE_KINDS:&[(&str,&str)]=&[("Secret","secret"),("ConfigMap","configmap"),("Service","service"),("PersistentVolumeClaim","persistentvolumeclaim"),("ServiceAccount","serviceaccount")];
fn parse_resources(output:&str,kind:&str)->Result<BTreeSet<String>,Error> {
    if output.len()>MAX_TEXT{return Err("Resource completion snapshot exceeds 64KiB.".into());}
    let mut names=BTreeSet::new();
    for line in output.lines(){
        let name=line.strip_prefix(kind).and_then(|tail|tail.strip_prefix('/')).filter(|name|crate::is_dns_subdomain(name)).ok_or("Unsupported resource name output; no snapshot accepted.")?;
        if !names.insert(name.into())||names.len()>512{return Err("Resource names are duplicate or exceed512 names.".into());}
    }
    Ok(names)
}
fn parse(output: &str) -> Result<BTreeSet<String>, Error> {
    if output.len() > MAX_TEXT { return Err("Namespace snapshot exceeds 64 KiB.".into()); }
    let mut names = BTreeSet::new();
    for line in output.lines() {
        let name = line.strip_prefix("namespace/").filter(|name| crate::is_dns_label(name)).ok_or("Unsupported namespace query output; no snapshot accepted.")?;
        if !names.insert(name.into()) || names.len() > 512 { return Err("Namespace snapshot is duplicate or exceeds 512 names.".into()); }
    }
    Ok(names)
}
fn field(doc: &Document, at: Position) -> Option<(String, Range, bool)> {
    if doc.language != "yaml" || doc.text.len() > MAX_TEXT || doc.text.contains(['\t', '&', '*', '!']) || doc.text.contains("{{") { return None; }
    let offset = text::offset(&doc.text, at)?;
    let documents = manifest::documents(&doc.text);
    let d = documents.iter().find(|d| d.range.start <= offset && offset <= d.range.end)?;
    if d.name_unknown || ["apiVersion", "kind", "metadata"].iter().any(|key| d.top_keys.iter().filter(|k| k == key).count() != 1) { return None; }
    if !crate::indexer::namespace_kind(&d.api_version.as_ref()?.text, &d.kind.as_ref()?.text) { return None; }
    // Parse current document only to reject malformed syntax. Anchors/tags are
    // conservatively excluded above; duplicate owner keys are excluded below.
    let parsed = crate::kubeconfig_import::parse(&doc.text[d.range.clone()]).ok()?;
    if !parsed["metadata"].is_object() || !(parsed["metadata"]["namespace"].is_string() || parsed["metadata"]["namespace"].is_null()) { return None; }
    let fields: Vec<_> = d.metadata_keys.iter().filter(|(key,_,_)| key == "namespace").collect();
    let [(.., key, value)] = fields.as_slice() else { return None };
    let (start, end) = if let Some(value) = value {
        (value.start, value.end)
    } else {
        let end = doc.text[key.end..].find('\n').map_or(doc.text.len(), |i| key.end + i);
        let rest = doc.text[key.end..end].strip_prefix(':')?;
        if !rest.chars().all(|c| c == ' ' || c == '\r') { return None; }
        let start = key.end + 1 + rest.trim_end_matches('\r').len();
        (start, start)
    };
    if !(start..=end).contains(&offset) { return None; }
    let typed = &doc.text[start..offset];
    if typed.len() > 63 || !doc.text[start..end].bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') { return None; }
    let quoted = start > 0 && matches!(doc.text.as_bytes()[start - 1], b'\'' | b'"');
    Some((typed.into(), Range::new(text::position(&doc.text, start), text::position(&doc.text, end)), quoted))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::KubeExplorer;
    use openidea_plugin::{Plugin, ProcessOutput, mock::MockHost, providers::Providers};
    fn setup() -> (MockHost, KubeExplorer) {
        let host = MockHost::install().with_capabilities(&["project.read", "process:kubectl", "storage"]);
        host.on_process("kubectl", |args,_| {
            let stdout = if args.iter().any(|a| a == "get") { "namespace/default\nnamespace/dev\nnamespace/development\n" }
            else { r#"{"contexts":[{"name":"dev","context":{"cluster":"local","namespace":"default"}}],"clusters":[{"name":"local","cluster":{"server":"https://dev.invalid"}}]}"# };
            Ok(ProcessOutput { status: Some(0), stdout: stdout.into(), stderr: String::new() })
        });
        (host, KubeExplorer { context: Some("dev".into()), namespace: Some("team".into()), ..Default::default() })
    }
    fn doc(value: &str) -> (Document, Position) {
        let text = format!("# 😀 current buffer\r\napiVersion: v1\r\nkind: Pod\r\nmetadata:\r\n  namespace: {value}\r\n");
        let at = text::position(&text, text.find(&format!("namespace: {value}")).unwrap() + "namespace: ".len() + value.len());
        (Document { path: "/project/pod.yaml".into(), language: "yaml".into(), version: 7, text }, at)
    }
    fn resource_doc(value:&str)->(Document,Position) {
        let text=format!("# 😀 current buffer\r\napiVersion: v1\r\nkind: Pod\r\nmetadata:\r\n  namespace: team\r\nspec:\r\n  volumes:\r\n    - secret:\r\n        secretName: {value}\r\n");
        let point=text.find("secretName: ").unwrap()+"secretName: ".len()+value.len();
        let at=text::position(&text,point);
        (Document{path:"/project/pod.yaml".into(),language:"yaml".into(),version:3,text},at)
    }
    fn resource_host()->(MockHost,KubeExplorer) {
        let (h,k)=setup();h.on_process("kubectl",|args,_|{
            let stdout=if args.iter().any(|arg|arg=="config") {r#"{"contexts":[{"name":"dev","context":{"cluster":"local","namespace":"default"}}],"clusters":[{"name":"local","cluster":{"server":"https://dev.invalid"}}]}"#}
                else if args.iter().any(|arg|arg=="secret") {"secret/db\nsecret/db-prod\nsecret/true\n"}
                else if args.iter().any(|arg|arg=="configmap"){"configmap/db-config\n"}
                else if args.iter().any(|arg|arg=="service"){"service/api\n"}
                else if args.iter().any(|arg|arg=="persistentvolumeclaim"){"persistentvolumeclaim/data\n"}
                else if args.iter().any(|arg|arg=="serviceaccount"){"serviceaccount/runner\n"}
                else {""};
            Ok(ProcessOutput{status:Some(0),stdout:stdout.into(),stderr:String::new()})
        });(h,k)
    }
    fn read_kind(h:&MockHost,k:&mut KubeExplorer,kind:&str) {
        h.answer_next_prompt(Some(&[kind]));k.run_command("cache-cluster-resources",&h.invocation(),None).unwrap();
    }
    #[test]
    fn resource_reference_names_are_explicit_scoped_cached_and_insert_exact_current_strings() {
        let (h,mut k)=resource_host();let (d,at)=resource_doc("db");
        assert!(k.completion(&d,at,None).is_empty());assert!(h.process_runs().is_empty());
        read_kind(&h,&mut k,"Secret");let before=h.process_runs().len();
        assert!(h.process_runs().iter().all(|run|run.cwd.as_deref()==Some("/project")));
        assert!(h.process_runs().iter().any(|run|run.args.iter().any(|arg|arg=="--namespace=team")&&run.args.iter().any(|arg|arg=="secret")));
        let items=k.completion(&d,at,None);assert_eq!(items.iter().map(|item|item.label.as_str()).collect::<Vec<_>>(),["db","db-prod"]);
        for item in items {
            let span=item.replace.unwrap();assert_eq!(&d.text[text::offset(&d.text,span.start).unwrap()..text::offset(&d.text,span.end).unwrap()],"db");
            assert_eq!(item.insert_text,format!("'{}'",item.label));assert!(item.detail.unwrap().contains("deployment target unresolved"));
        }
        let (blank,at)=resource_doc("");assert_eq!(k.completion(&blank,at,None).len(),3);
        let (quoted,at)=resource_doc("'tr'");let items=k.completion(&quoted,Position::new(at.line,at.character-1),None);assert_eq!(items[0].insert_text,"true");
        assert_eq!(h.process_runs().len(),before);
        read_kind(&h,&mut k,"ConfigMap");
        let config=Document{text:d.text.replace("- secret:","- configMap:").replace("secretName: db","name: db"),..d.clone()};
        let at=text::position(&config.text,config.text.rfind("name: db").unwrap()+"name: db".len());
        assert_eq!(k.completion(&config,at,None)[0].label,"db-config");
        let (secret,at)=resource_doc("db");assert_eq!(k.completion(&secret,at,None).len(),2);
        let stream=format!("apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: preceding\n---\n{}",secret.text);
        let point=stream.rfind("secretName: db").unwrap()+"secretName: db".len();
        let at=text::position(&stream,point);let before=h.process_runs().len();
        assert_eq!(k.completion(&Document{text:stream,..secret.clone()},at,None).len(),2);
        assert_eq!(h.process_runs().len(),before);

        for (kind,manifest,field,wanted) in [
            ("Service","apiVersion: apps/v1\nkind: StatefulSet\nmetadata:\n  namespace: team\nspec:\n  serviceName: a\n","serviceName: a","api"),
            ("PersistentVolumeClaim","apiVersion: v1\nkind: Pod\nmetadata:\n  namespace: team\nspec:\n  volumes:\n    - persistentVolumeClaim:\n        claimName: d\n","claimName: d","data"),
            ("ServiceAccount","apiVersion: v1\nkind: Pod\nmetadata:\n  namespace: team\nspec:\n  serviceAccountName: r\n","serviceAccountName: r","runner"),
        ] {
            read_kind(&h,&mut k,kind);let at=text::position(manifest,manifest.find(field).unwrap()+field.len());
            let doc=Document{text:manifest.into(),..d.clone()};let before=h.process_runs().len();
            assert_eq!(k.completion(&doc,at,None)[0].label,wanted);assert_eq!(h.process_runs().len(),before);
        }

    }
    #[test]
    fn resource_completion_refuses_foreign_namespace_kind_path_expiry_and_scope_without_queries() {
        let (h,mut k)=resource_host();read_kind(&h,&mut k,"Secret");let (d,at)=resource_doc("db");let before=h.process_runs().len();
        for text in [d.text.replace("namespace: team","namespace: other"),d.text.replace("kind: Pod","kind: ConfigMap"),d.text.replace("apiVersion: v1","apiVersion: custom.example/v1"),d.text.replace("  namespace: team\r\n",""),d.text.replace("spec:","data:"),d.text.replace("namespace: team","namespace: team\r\n  namespace: team")] {
            let offset=text.rfind("secretName: db").unwrap()+"secretName: db".len();let pos=text::position(&text,offset);
            assert!(k.completion(&Document{text,..d.clone()},pos,None).is_empty());
        }
        let mut foreign=d.clone();foreign.path="/other/pod.yaml".into();assert!(k.completion(&foreign,at,None).is_empty());
        h.add_project("/project/nested");foreign.path="/project/nested/pod.yaml".into();assert!(k.completion(&foreign,at,None).is_empty());
        k.context=Some("other".into());assert!(k.completion(&d,at,None).is_empty());k.context=Some("dev".into());
        h.set_setting("kubeconfig","\"other-config\"");assert!(k.completion(&d,at,None).is_empty());h.set_setting("kubeconfig","\"\"");
        k.cluster_completion.resources.get_mut("Secret").unwrap().at=Instant::now()-TTL;assert!(k.completion(&d,at,None).is_empty());
        assert_eq!(h.process_runs().len(),before);
        for output in ["db", "secret/db\nsecret/db", "secret/--bad", "secret/db\nwarning", "configmap/db"] {assert!(parse_resources(output,"secret").is_err());}
    }
    #[test]
    fn resource_read_failure_retires_that_kind_and_all_namespace_mode_is_inert() {
        let (h,mut k)=resource_host();read_kind(&h,&mut k,"Secret");let (d,at)=resource_doc("db");assert!(!k.completion(&d,at,None).is_empty());
        h.on_process("kubectl",|_,_|Ok(ProcessOutput{status:Some(1),stdout:"not-accepted".into(),stderr:"raw-private-error".into()}));
        h.set_project_root(Some("/other"));h.answer_next_prompt(Some(&["Secret"]));
        assert!(k.run_command("cache-cluster-resources",&h.invocation(),None).is_err());
        assert!(k.cluster_completion.resources.get("Secret").is_some_and(|snapshot|snapshot.root=="/project"));
        h.set_project_root(Some("/project"));assert!(!k.completion(&d,at,None).is_empty());
        h.answer_next_prompt(Some(&["Secret"]));assert!(k.run_command("cache-cluster-resources",&h.invocation(),None).is_err());assert!(k.completion(&d,at,None).is_empty());
        k.namespace=Some(crate::ALL_NAMESPACES.into());let before=h.process_runs().len();assert!(k.run_command("cache-cluster-resources",&h.invocation(),None).is_err());assert_eq!(h.process_runs().len(),before);
    }
    #[test]
    fn explicit_read_warms_positive_names_and_provider_uses_only_cache_with_utf16_current_ranges() {
        let (host, mut kube) = setup(); let (d, at) = doc("de");
        assert!(kube.completion(&d, at, None).is_empty()); assert!(host.process_runs().is_empty());
        kube.run_command("cache-cluster-namespaces", &host.invocation(), None).unwrap();
        let before = host.process_runs().len();
        assert!(host.process_runs().iter().all(|r| r.cwd.as_deref() == Some("/project")));
        let items = kube.completion(&d, at, None);
        assert_eq!(items.iter().map(|i|i.label.as_str()).collect::<Vec<_>>(), ["default","dev","development"]);
        for item in &items {
            let range = item.replace.unwrap();
            assert_eq!(&d.text[text::offset(&d.text,range.start).unwrap()..text::offset(&d.text,range.end).unwrap()], "de");
            assert!(item.detail.as_deref().unwrap().contains("deployment target unresolved"));
            assert!(item.insert_text.starts_with('\''));
        }
        assert_eq!(host.process_runs().len(), before);
        let (empty, at) = doc(""); assert_eq!(kube.completion(&empty, at, None).len(), 3);
        assert_eq!(host.process_runs().len(), before);
    }
    #[test]
    fn foreign_nested_expired_and_changed_choices_are_quiet_without_processes() {
        let (host, mut kube) = setup(); let (d, at) = doc("de");
        kube.run_command("cache-cluster-namespaces", &host.invocation(), None).unwrap();
        let before = host.process_runs().len();
        let mut foreign = d.clone(); foreign.path = "/other/pod.yaml".into();
        assert!(kube.completion(&foreign, at, None).is_empty());
        host.add_project("/project/nested"); foreign.path = "/project/nested/pod.yaml".into();
        assert!(kube.completion(&foreign, at, None).is_empty());
        kube.context = Some("other".into()); assert!(kube.completion(&d, at, None).is_empty()); kube.context = Some("dev".into());
        host.set_setting("kubeconfig", "\"/different/config\""); assert!(kube.completion(&d, at, None).is_empty()); host.set_setting("kubeconfig", "\"\"");
        kube.cluster_completion.snapshot.as_mut().unwrap().at = Instant::now() - TTL;
        assert!(kube.completion(&d, at, None).is_empty()); assert_eq!(host.process_runs().len(), before);
    }
    #[test]
    fn malformed_cluster_output_and_unknown_source_contexts_do_not_publish_guesses() {
        for output in ["default\n", "namespace/default\nnamespace/default", "namespace/UPPER", "namespace/dev\nwarning: something"] { assert!(parse(output).is_err()); }
        assert!(parse(&"x".repeat(MAX_TEXT + 1)).is_err());
        let (host, mut kube) = setup(); kube.run_command("cache-cluster-namespaces", &host.invocation(), None).unwrap();
        let (d, at) = doc("de");
        for text in [d.text.replace("kind: Pod", "kind: Namespace"),d.text.replace("v1", "custom.example/v1"),d.text.replace("metadata:", "metadata: {}\nx:"),d.text.replace("  namespace: de", "  namespace: de\n  namespace: prod"),d.text.replace("  namespace: de", "  namespace: {{ .Values.namespace }}"),d.text.replace("kind: Pod", "kind: [" )] {
            assert!(field(&Document {text, ..d.clone()}, at).is_none());
        }
        assert!(kube.forward_session.is_none());
    }
    #[test]
    fn quoted_insertions_preserve_yaml_string_types_and_bounded_parser_rejects_deep_sources() {
        let (host, mut kube) = setup(); kube.run_command("cache-cluster-namespaces", &host.invocation(), None).unwrap();
        kube.cluster_completion.snapshot.as_mut().unwrap().names = BTreeSet::from(["123".into(), "true".into(), "null".into()]);
        let (blank, at) = doc("");
        for item in kube.completion(&blank, at, None) {
            let range = item.replace.unwrap();
            let from = text::offset(&blank.text, range.start).unwrap(); let to = text::offset(&blank.text, range.end).unwrap();
            let replaced = format!("{}{}{}", &blank.text[..from], item.insert_text, &blank.text[to..]);
            assert_eq!(crate::kubeconfig_import::parse(&replaced).unwrap()["metadata"]["namespace"].as_str(), Some(item.label.as_str()));
        }
        let (quoted, at) = doc("'tr'");
        let items = kube.completion(&quoted, Position::new(at.line, at.character - 1), None);
        assert_eq!(items[0].insert_text, "true");
        let (d, at) = doc("de");
        let deep = format!("{}data: {}x{}\n", d.text, "[".repeat(32), "]".repeat(32));
        assert!(field(&Document { text: deep, ..d }, at).is_none());
    }
    #[test]
    fn failed_explicit_refresh_retires_same_owner_completion_and_deactivation_clears_it() {
        let (host, mut kube) = setup(); kube.run_command("cache-cluster-namespaces", &host.invocation(), None).unwrap();
        host.on_process("kubectl", |_,_| Err(Error::Failed("synthetic read failure".into())));
        assert!(kube.run_command("cache-cluster-namespaces", &host.invocation(), None).is_err());
        assert!(kube.cluster_completion.snapshot.is_none());
        kube.deactivate(); assert!(kube.cluster_completion.snapshot.is_none());
    }
}
