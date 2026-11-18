//! Private source-owned resource copies. Background Save never mutates a cluster.
use crate::{KUBECTL, REQUEST_TIMEOUT, forward, kubeconfig_import, schema};
use openidea_plugin::providers::Document;
use openidea_plugin::{Error, Invocation, Project, host};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::hash::BuildHasher;
use std::time::{Duration, Instant};
const MAX_TEXT: usize = 64 << 10;
const MAX_LEASES: usize = 16;
const MAX_FILES: usize = 64;
const PREFIX: &str = "resource-editor-";
const TTL: Duration = Duration::from_secs(60);
// Source-established modern built-ins only; unsupported objects stay in the panel.
const TYPES: &[(&str, &str, &str)] = &[
    ("pods", "Pod", "v1"),
    ("services", "Service", "v1"),
    ("configmaps", "ConfigMap", "v1"),
    ("serviceaccounts", "ServiceAccount", "v1"),
    ("persistentvolumeclaims", "PersistentVolumeClaim", "v1"),
    ("namespaces", "Namespace", "v1"),
    ("nodes", "Node", "v1"),
    ("persistentvolumes", "PersistentVolume", "v1"),
    ("deployments", "Deployment", "apps/v1"),
    ("replicasets", "ReplicaSet", "apps/v1"),
    ("daemonsets", "DaemonSet", "apps/v1"),
    ("statefulsets", "StatefulSet", "apps/v1"),
    ("jobs", "Job", "batch/v1"),
    ("cronjobs", "CronJob", "batch/v1"),
    ("ingresses", "Ingress", "networking.k8s.io/v1"),
    ("networkpolicies", "NetworkPolicy", "networking.k8s.io/v1"),
    ("roles", "Role", "rbac.authorization.k8s.io/v1"),
    (
        "rolebindings",
        "RoleBinding",
        "rbac.authorization.k8s.io/v1",
    ),
    (
        "clusterroles",
        "ClusterRole",
        "rbac.authorization.k8s.io/v1",
    ),
    (
        "clusterrolebindings",
        "ClusterRoleBinding",
        "rbac.authorization.k8s.io/v1",
    ),
];
#[derive(Clone, PartialEq, Eq)]
struct Target {
    api: String,
    kind: String,
    name: String,
    namespace: Option<String>,
    uid: String,
    version: String,
}
fn invalid(message: &str) -> Error {
    Error::Invalid(message.into())
}
fn digest(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}
fn target(text: &str) -> Result<Target, Error> {
    if text.len() > MAX_TEXT {
        return Err(invalid("Private YAML exceeds 64 KiB."));
    }
    kubeconfig_import::bounded_structure(text)?;
    let documents = yaml_rust2::YamlLoader::load_from_str(text)
        .map_err(|_| invalid("Resource YAML is malformed."))?;
    let value = documents.first().ok_or(Error::Stale)?;
    let field = |value: &yaml_rust2::Yaml, key: &str, max| {
        value[key].as_str().filter(|s|!s.is_empty()&&s.len()<=max&&!s.chars().any(char::is_control)).map(str::to_owned).ok_or_else(||invalid("Resource identity needs bounded literal strings, including UID/resourceVersion."))
    };
    let api = field(value, "apiVersion", 128)?;
    let kind = field(value, "kind", 128)?;
    let metadata = &value["metadata"];
    let name = field(metadata, "name", 253)?;
    if !crate::is_dns_subdomain(&name) {
        return Err(Error::Stale);
    }
    let namespace = match &metadata["namespace"] {
        yaml_rust2::Yaml::BadValue => None,
        yaml_rust2::Yaml::String(s) if crate::is_dns_label(s) => Some(s.clone()),
        _ => return Err(invalid("Resource namespace is unsupported.")),
    };
    Ok(Target {
        api,
        kind,
        name,
        namespace,
        uid: field(metadata, "uid", 128)?,
        version: field(metadata, "resourceVersion", 256)?,
    })
}
struct Pending {
    doc: Document,
    at: Instant,
}
struct Lease {
    root: String,
    scope: forward::Scope,
    context: String,
    namespace: String,
    target: Target,
    initial: String,
    marker: String,
    pending: Option<Pending>,
}
#[derive(Default)]
pub(crate) struct Editors {
    leases: BTreeMap<String, Lease>,
    serial: u64,
}
fn project(root: &str) -> Result<Project, Error> {
    host::projects()?
        .into_iter()
        .find(|p| p.root().ok().as_deref() == Some(root))
        .ok_or(Error::Stale)
}
fn same(ctx: &Invocation, doc: &Document, root: &str) -> Result<(), Error> {
    if forward::owner(Some(ctx))?.1 != root {
        return Err(Error::Stale);
    }
    let editor = ctx.editor().ok_or(Error::Stale)?;
    let current = editor.document()?;
    if editor.path() != doc.path || current.version != doc.version || current.text != doc.text {
        return Err(Error::Stale);
    }
    Ok(())
}
fn current(ctx: &Invocation) -> Result<Document, Error> {
    let e = ctx
        .editor()
        .ok_or("Open a live private Kubernetes YAML editor first.")?;
    let d = e.document()?;
    Ok(Document {
        path: e.path(),
        language: e.language(),
        text: d.text,
        version: d.version,
    })
}
fn validate(lease: &Lease, doc: &Document, ctx: &Invocation) -> Result<Project, Error> {
    same(ctx, doc, &lease.root)?;
    if !doc.text.starts_with(&lease.marker) || target(&doc.text)? != lease.target {
        return Err(invalid(
            "Resource identity/header changed; reopen the resource instead of changing API/kind/name/namespace/UID/resourceVersion.",
        ));
    }
    let p = project(&lease.root)?;
    if !lease.scope.configuration_current()
        || forward::Scope::capture_for_project(Some(&lease.context), Some(&lease.namespace), &p)?
            != lease.scope
    {
        return Err(Error::Stale);
    }
    Ok(p)
}
impl Editors {
    pub(crate) fn open(
        &mut self,
        root: &str,
        context: Option<&str>,
        namespace: Option<&str>,
        resource: &str,
        name: &str,
    ) -> Result<(), Error> {
        if self.leases.len() >= MAX_LEASES {
            return Err(invalid(
                "At most 16 private editor leases; retire one before opening another.",
            ));
        }
        let (plural, group) = resource
            .split_once('.')
            .map_or((resource, None), |(p, g)| (p, Some(g)));
        let (_,kind,api)=TYPES.iter().find(|(r,_,a)|*r==plural&&group.is_none_or(|g|a.split_once('/').map(|(g,_)|g)==Some(g))).ok_or("This API/kind stays in the panel; private editor requires a supported built-in identity.")?;
        let p = project(root)?;
        let scope = forward::Scope::capture_for_project(context, namespace, &p)?;
        let out = p.run_process(
            KUBECTL,
            &scope.args(&["get", resource, name, "-o", "yaml", REQUEST_TIMEOUT]),
            None,
        )?;
        if out.status != Some(0) {
            return Err(invalid(&crate::failure_reason("Reading the resource", context, &out.stderr)));
        }
        let identity = target(&out.stdout)?;
        let scoped = scope.args(&[]);
        let context = scoped
            .iter()
            .find_map(|a| a.strip_prefix("--context="))
            .ok_or(Error::Stale)?
            .to_owned();
        let namespace = scoped
            .iter()
            .find_map(|a| a.strip_prefix("--namespace="))
            .ok_or(Error::Stale)?
            .to_owned();
        if identity.name != name
            || identity.kind != *kind
            || identity.api != *api
            || (crate::indexer::namespace_kind(api, kind)
                && identity.namespace.as_deref() != Some(namespace.as_str()))
            || (!crate::indexer::namespace_kind(api, kind) && identity.namespace.is_some())
        {
            return Err(Error::Stale);
        }
        if !scope.configuration_current()
            || forward::Scope::capture_for_project(Some(&context), Some(&namespace), &p)? != scope
        {
            return Err(Error::Stale);
        }
        let directory = host::fs::plugin_dir()?;
        let files = host::fs::read_dir(&directory)?;
        if files.len() >= 4096
            || files.iter().filter(|f| f.name.starts_with(PREFIX)).count() >= MAX_FILES
        {
            return Err(invalid(
                "Private editor backing-file limit reached; existing copies are retained, not overwritten.",
            ));
        }
        self.serial = self.serial.wrapping_add(1);
        let random = std::collections::hash_map::RandomState::new();
        let nonce = format!(
            "{:016x}{:016x}",
            random.hash_one((root, self.serial, "editor")),
            random.hash_one((root, self.serial, "lease"))
        );
        let file = format!("{PREFIX}{nonce}.yaml");
        if files.iter().any(|f| f.name == file) {
            return Err(Error::Stale);
        }
        let path = format!("{directory}/{file}");
        let marker = format!("# Kubernetes private editor lease {nonce}\n");
        let text = format!(
            "{marker}# Origin project: {root}\n# Context: {context}; object: {kind} {}/{}\n# Saving is LOCAL ONLY. Review Private YAML Apply, then Apply Private YAML.\n{}{}",
            identity.namespace.as_deref().unwrap_or("(cluster)"),
            name,
            schema::modeline(&out.stdout).unwrap_or_default(),
            out.stdout
        );
        if text.len() > MAX_TEXT {
            return Err(invalid("Private YAML plus source header exceeds 64 KiB."));
        }
        host::fs::write_bytes(&path, text.as_bytes())?;
        if host::fs::read_bytes(&path)? != text.as_bytes() {
            return Err(Error::Stale);
        }
        self.leases.insert(
            path.clone(),
            Lease {
                root: root.into(),
                scope,
                context,
                namespace,
                target: identity,
                initial: digest(&text),
                marker,
                pending: None,
            },
        );
        if let Err(error) = host::open_location(&path, 0, 0) {
            self.leases.remove(&path);
            let _ = host::fs::write_bytes(&path, &[]);
            return Err(error);
        }
        Ok(())
    }
    pub(crate) fn review(&mut self, ctx: &Invocation) -> Result<(), Error> {
        let doc = current(ctx)?;
        let lease = self
            .leases
            .get_mut(&doc.path)
            .ok_or("This private editor has no live lease; reopen the resource from Services.")?;
        same(ctx, &doc, &lease.root)?;
        lease.pending = None;
        validate(lease, &doc, ctx)?;
        let body = format!(
            "Project: {}\nContext: {}\nTarget: {} {}/{}\nUID: {}\nResource version: {}\nEditor version: {}\nExact UTF-8 bytes: {}\nSHA-256: {}\n\nPrepare replacement from this exact editor text? This is a digest/target review, not a truncated content preview. No mutation yet. Within 60 seconds invoke Apply Private YAML in this same file. That separate event rechecks source/root/scope. kubectl replace uses verified text on stdin, never rereads the private path. Server resourceVersion conflict protection remains; no overwrite/rollback/Undo promise.",
            lease.root,
            lease.context,
            lease.target.kind,
            lease.target.namespace.as_deref().unwrap_or("(cluster)"),
            lease.target.name,
            lease.target.uid,
            lease.target.version,
            doc.version,
            doc.text.len(),
            digest(&doc.text)
        );
        if !ui_kit::prompts::confirm_exact("Review private YAML replacement?", &body)? {
            return Ok(());
        }
        validate(lease, &doc, ctx)?;
        lease.pending = Some(Pending {
            doc,
            at: Instant::now(),
        });
        host::notify(
            openidea_plugin::NotificationLevel::Info,
            "Private YAML review",
            "Prepared only. Invoke Apply Private YAML in this file within 60 seconds; Save alone never applies.",
        );
        Ok(())
    }
    pub(crate) fn apply(&mut self, ctx: &Invocation) -> Result<(), Error> {
        let doc = current(ctx)?;
        let lease = self.leases.get_mut(&doc.path).ok_or(Error::Stale)?;
        same(ctx, &doc, &lease.root)?;
        let pending = lease
            .pending
            .take()
            .ok_or("Review this private YAML replacement first.")?;
        if pending.at.elapsed() >= TTL || pending.doc != doc {
            return Err(Error::Stale);
        }
        let p = validate(lease, &doc, ctx)?;
        if pending.at.elapsed() >= TTL || !lease.scope.configuration_current() {
            return Err(Error::Stale);
        }
        same(ctx, &doc, &lease.root)?;
        let out = p.run_process(
            KUBECTL,
            &lease.scope.args(&["replace", "-f", "-", REQUEST_TIMEOUT]),
            Some(&doc.text),
        )?;
        if out.status != Some(0) {
            return Err(invalid(&format!(
                "{} Your editor text is kept.",
                crate::failure_reason("Applying the YAML", Some(&lease.context), &out.stderr)
            )));
        }
        self.leases.remove(&doc.path);
        host::notify(
            openidea_plugin::NotificationLevel::Info,
            "Private YAML replacement",
            "Command exited successfully. Lease consumed; reopen the resource before another apply. Private editor text was retained; no file/cluster rollback is claimed.",
        );
        Ok(())
    }
    pub(crate) fn retire(&mut self, ctx: &Invocation) -> Result<(), Error> {
        let editor = ctx.editor().ok_or(Error::Stale)?;
        let path = editor.path();
        let lease = self.leases.get(&path).ok_or(Error::Stale)?;
        same(ctx, &current(ctx)?, &lease.root)?;
        let lease = self.leases.remove(&path).ok_or(Error::Stale)?;
        // Only explicitly retired, unchanged generated copies may be blanked.
        // Edited/dirty work stays; no private delete API exists in SDK1.3.
        if editor
            .document()
            .ok()
            .is_some_and(|d| digest(&d.text) == lease.initial)
            && host::fs::read_bytes(&path)
                .ok()
                .is_some_and(|b| format!("{:x}", Sha256::digest(&b)) == lease.initial)
        {
            let _ = host::fs::write_bytes(&path, &[]);
        }
        host::notify(
            openidea_plugin::NotificationLevel::Info,
            "Private editor retired",
            "Apply authority removed. Edited/dirty copies are retained; unchanged generated backing bytes may be cleared. No cluster command ran.",
        );
        Ok(())
    }
    pub(crate) fn clear(&mut self) {
        self.leases.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{Plugin, ProcessOutput, mock::MockHost};
    const YAML: &str = "apiVersion: v1\nkind: Pod\nmetadata:\n  name: web\n  namespace: shop\n  uid: uid-web\n  resourceVersion: \"7\"\nspec:\n  priority: 0.5\n";
    fn setup() -> (MockHost, Editors, String) {
        let h = MockHost::install().with_capabilities(&[
            "project.read",
            "process:kubectl",
            "editor.read",
            "storage",
        ]);
        handler(&h, YAML, false, "https://local.invalid");
        let mut editors = Editors::default();
        editors
            .open("/project", Some("dev"), Some("shop"), "pods", "web")
            .unwrap();
        let path = h.opened_locations()[0].0.clone();
        let text = host::fs::read_file(&path).unwrap();
        h.open_editor(&path, "yaml", &text, &[]);
        (h, editors, path)
    }
    fn handler(h: &MockHost, yaml: &str, fail: bool, server: &str) {
        let yaml = yaml.to_owned();
        let server = server.to_owned();
        h.on_process(KUBECTL,move |args,stdin| {
            let stdout=if args.iter().any(|a|a=="config") {format!(r#"{{"contexts":[{{"name":"dev","context":{{"cluster":"local","namespace":"shop"}}}}],"clusters":[{{"name":"local","cluster":{{"server":"{server}"}}}}]}}"#)} else if args.iter().any(|a|a=="replace") {
                assert_eq!(args[args.len()-4..],["replace","-f","-",REQUEST_TIMEOUT]);assert!(stdin.unwrap().contains("resourceVersion: \"7\""));String::new()
            } else {yaml.clone()};
            Ok(ProcessOutput{status:Some(if fail&&args.iter().any(|a|a=="replace"){1}else{0}),stdout,stderr:if fail{"private-error-secret".into()}else{String::new()}})
        });
    }
    fn prepared(h: &MockHost, e: &mut Editors) {
        h.answer_next_prompt(Some(&[]));
        e.review(&h.invocation()).unwrap();
    }
    fn mutations(h: &MockHost) -> usize {
        h.process_runs()
            .iter()
            .filter(|r| r.args.iter().any(|a| a == "replace"))
            .count()
    }
    #[test]
    fn explicit_review_is_inert_apply_is_exact_stdin_project_scoped_and_single_use() {
        let (h, mut e, path) = setup();
        h.type_in_editor("# local edit 😀\n");
        let text = h.editor_text();
        prepared(&h, &mut e);
        assert_eq!(mutations(&h), 0);
        let exact = text.clone();
        h.on_process(KUBECTL,move |args,stdin| {
            if args.iter().any(|a|a=="replace") {assert_eq!(stdin,Some(exact.as_str()));}
            Ok(ProcessOutput{status:Some(0),stdout:r#"{"contexts":[{"name":"dev","context":{"cluster":"local","namespace":"shop"}}],"clusters":[{"name":"local","cluster":{"server":"https://local.invalid"}}]}"#.into(),stderr:String::new()})
        });
        e.apply(&h.invocation()).unwrap();
        assert_eq!(mutations(&h), 1);
        let r = h
            .process_runs()
            .into_iter()
            .find(|r| r.args.iter().any(|a| a == "replace"))
            .unwrap();
        assert!(text.ends_with("# local edit 😀\n"));
        assert_eq!(r.cwd.as_deref(), Some("/project"));
        assert!(
            h.process_runs()
                .iter()
                .all(|r| r.cwd.as_deref() == Some("/project"))
        );
        assert!(e.apply(&h.invocation()).is_err());
        assert_eq!(mutations(&h), 1);
        assert!(host::fs::read_bytes(&path).unwrap().len() > 0);
    }
    #[test]
    fn cancelled_review_version_changes_and_expiry_cannot_apply() {
        let (h, mut e, path) = setup();
        h.answer_next_prompt(None);
        e.review(&h.invocation()).unwrap();
        assert!(e.apply(&h.invocation()).is_err());
        prepared(&h, &mut e);
        h.type_in_editor("# changed\n");
        assert!(e.apply(&h.invocation()).is_err());
        prepared(&h, &mut e);
        e.leases
            .get_mut(&path)
            .unwrap()
            .pending
            .as_mut()
            .unwrap()
            .at = Instant::now() - TTL;
        assert!(e.apply(&h.invocation()).is_err());
        assert_eq!(mutations(&h), 0);
    }
    #[test]
    fn foreign_project_changed_config_identity_and_replayed_ticket_are_quiet() {
        let (h, mut e, _) = setup();
        prepared(&h, &mut e);
        h.add_project("/project");
        h.set_project_root(Some("/other"));
        assert!(e.apply(&h.invocation()).is_err());
        assert!(e.retire(&h.invocation()).is_err());
        assert_eq!(e.leases.len(), 1);
        h.set_project_root(Some("/project"));
        assert!(e.leases.values().next().unwrap().pending.is_some());
        prepared(&h, &mut e);
        handler(&h, YAML, false, "https://changed.invalid");
        assert!(e.apply(&h.invocation()).is_err());
        assert_eq!(mutations(&h), 0);
    }
    #[test]
    fn metadata_marker_and_multiple_documents_cannot_change_replacement_identity() {
        let (h, mut e, path) = setup();
        let initial = h.editor_text();
        for text in [
            initial.replace("uid-web", "uid-other"),
            initial.replace("resourceVersion: \"7\"", "resourceVersion: \"8\""),
            initial.replace("namespace: shop", "namespace: foreign"),
            initial.replace("# Kubernetes private editor lease", "# changed lease"),
            format!("{initial}\n---\n{YAML}"),
        ] {
            h.open_editor(&path, "yaml", &text, &[]);
            assert!(e.review(&h.invocation()).is_err());
        }
        assert_eq!(mutations(&h), 0);
    }
    #[test]
    fn failed_replace_retains_work_consumes_approval_and_never_discloses_tool_error() {
        let (h, mut e, path) = setup();
        h.type_in_editor("# keep my work\n");
        prepared(&h, &mut e);
        handler(&h, YAML, true, "https://local.invalid");
        let error = format!("{:?}", e.apply(&h.invocation()).unwrap_err());
        assert!(!error.contains("private-error-secret"));
        assert!(e.leases.contains_key(&path));
        assert!(h.editor_text().contains("keep my work"));
        assert!(e.apply(&h.invocation()).is_err());
        assert_eq!(mutations(&h), 1);
    }
    #[test]
    fn retirement_and_deactivation_keep_edited_copies_but_remove_apply_authority() {
        let (h, mut e, path) = setup();
        let initial = host::fs::read_bytes(&path).unwrap();
        h.type_in_editor("# unsaved work\n");
        e.retire(&h.invocation()).unwrap();
        assert!(e.leases.is_empty());
        assert_eq!(host::fs::read_bytes(&path).unwrap(), initial);
        let (h, mut e, path) = setup();
        prepared(&h, &mut e);
        e.clear();
        assert!(e.apply(&h.invocation()).is_err());
        assert!(!host::fs::read_bytes(&path).unwrap().is_empty());
        assert_eq!(mutations(&h), 0);
    }
    #[test]
    fn invalid_api_uid_namespace_duplicate_and_unsupported_kinds_do_not_create_files() {
        let (h, _, _) = setup();
        let count = h.opened_locations().len();
        for yaml in [
            YAML.replace("uid: uid-web", "other: uid-web"),
            YAML.replace("kind: Pod", "kind: Secret"),
            YAML.replace("namespace: shop", "namespace: foreign"),
            YAML.replace("name: web", "name: web\n  name: other"),
            YAML.replace("apiVersion: v1", "apiVersion: unknown/v1"),
        ] {
            handler(&h, &yaml, false, "https://local.invalid");
            assert!(
                Editors::default()
                    .open("/project", Some("dev"), Some("shop"), "pods", "web")
                    .is_err()
            );
        }
        assert!(
            Editors::default()
                .open("/project", Some("dev"), Some("shop"), "secrets", "web")
                .is_err()
        );
        assert_eq!(h.opened_locations().len(), count);
    }
    #[test]
    fn saved_events_cannot_recover_old_persistent_autosave_authority() {
        let (h, mut e, path) = setup();
        prepared(&h, &mut e);
        host::storage::set(&format!("edit:{path}"), "prod\nshop\npods\nweb").unwrap();
        let mut k = crate::KubeExplorer::default();
        k.yaml_editors = e;
        k.handle_events(&[openidea_plugin::Event::FileSaved(path)], &h.invocation());
        assert_eq!(mutations(&h), 0);
        k.deactivate();
        assert!(k.yaml_editors.leases.is_empty());
    }
    #[test]
    fn file_and_lease_caps_refuse_without_reusing_or_overwriting_paths() {
        let (h, mut e, path) = setup();
        let original = host::fs::read_bytes(&path).unwrap();
        for _ in 1..MAX_LEASES {
            e.open("/project", Some("dev"), Some("shop"), "pods", "web")
                .unwrap();
        }
        assert!(
            e.open("/project", Some("dev"), Some("shop"), "pods", "web")
                .is_err()
        );
        assert_eq!(host::fs::read_bytes(&path).unwrap(), original);
        let dir = host::fs::plugin_dir().unwrap();
        for n in 0..MAX_FILES {
            h.add_external_file(&format!("{dir}/{PREFIX}existing-{n}.yaml"), "keep");
        }
        assert!(
            Editors::default()
                .open("/project", Some("dev"), Some("shop"), "pods", "web")
                .is_err()
        );
        assert_eq!(
            host::fs::read_file(&format!("{dir}/{PREFIX}existing-0.yaml")).unwrap(),
            "keep"
        );
    }
    #[test]
    fn explicit_unchanged_retirement_blanks_generated_bytes_without_cluster_mutation() {
        let (h, mut e, path) = setup();
        e.retire(&h.invocation()).unwrap();
        assert!(e.leases.is_empty());
        assert!(host::fs::read_bytes(&path).unwrap().is_empty());
        assert_eq!(mutations(&h), 0);
    }
}
