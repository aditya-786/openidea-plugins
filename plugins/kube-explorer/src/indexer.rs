//! Read-only resource associations. Exact source spans/group/namespace are
//! useful navigation facts, but incomplete references never enter Rename.
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::{Duration, Instant};

use crate::KubeExplorer;
use openidea_plugin::indexing::{Anchor, Document, DocumentIndexer, Edges};
use openidea_plugin::{Position, Range};

const MAX_ANCHORS: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scope {
    Namespaced,
    Cluster,
}

/// This table identifies scope only for known built-in group/kind pairs. A CRD
/// with the same Kind in another API group does not inherit this scope.
fn builtin_scope(group: &str, kind: &str) -> Option<Scope> {
    let scope = match (group, kind) {
        ("", "Namespace" | "Node" | "PersistentVolume")
        | ("rbac.authorization.k8s.io", "ClusterRole" | "ClusterRoleBinding")
        | ("apiextensions.k8s.io", "CustomResourceDefinition")
        | ("storage.k8s.io", "StorageClass" | "CSIDriver" | "CSINode" | "VolumeAttachment")
        | ("scheduling.k8s.io", "PriorityClass")
        | ("node.k8s.io", "RuntimeClass")
        | ("networking.k8s.io", "IngressClass")
        | ("apiregistration.k8s.io", "APIService")
        | (
            "admissionregistration.k8s.io",
            "MutatingWebhookConfiguration"
            | "ValidatingWebhookConfiguration"
            | "ValidatingAdmissionPolicy"
            | "ValidatingAdmissionPolicyBinding",
        )
        | ("certificates.k8s.io", "CertificateSigningRequest") => Scope::Cluster,
        (
            "",
            "Pod"
            | "Service"
            | "ConfigMap"
            | "Secret"
            | "ServiceAccount"
            | "PersistentVolumeClaim"
            | "ReplicationController"
            | "ResourceQuota"
            | "LimitRange"
            | "Event"
            | "Endpoints",
        )
        | ("apps", "Deployment" | "ReplicaSet" | "DaemonSet" | "StatefulSet" | "ControllerRevision")
        | ("extensions", "Deployment" | "ReplicaSet" | "DaemonSet" | "Ingress" | "NetworkPolicy")
        | ("batch", "Job" | "CronJob")
        | ("autoscaling", "HorizontalPodAutoscaler")
        | ("networking.k8s.io", "Ingress" | "NetworkPolicy")
        | ("rbac.authorization.k8s.io", "Role" | "RoleBinding")
        | ("policy", "PodDisruptionBudget")
        | ("coordination.k8s.io", "Lease")
        | ("discovery.k8s.io", "EndpointSlice")
        | ("events.k8s.io", "Event")
        | ("storage.k8s.io", "CSIStorageCapacity") => Scope::Namespaced,
        _ => return None,
    };
    Some(scope)
}

pub(super) fn namespace_kind(version: &str, kind: &str) -> bool {
    api_group(version).and_then(|group| builtin_scope(group, kind)) == Some(Scope::Namespaced)
}

fn api_group(version: &str) -> Option<&str> {
    if version == "v1" {
        return Some("");
    }
    let (group, version) = version.split_once('/')?;
    (crate::is_dns_subdomain(group) && crate::is_dns_label(version) && !version.contains('/')).then_some(group)
}

pub(crate) fn users_key(resource_key: &str) -> String {
    format!("kube-users:{}", resource_key.trim_start_matches("kube:"))
}

struct Value {
    text: String,
    range: Range,
}
fn value_of(line: &str, line_number: u32, after_colon: usize) -> Option<Value> {
    let rest = &line[after_colon..];
    let offset = after_colon + (rest.len() - rest.trim_start().len());
    let mut raw = line[offset..].trim_end();
    let mut start = offset;
    let quoted = raw.starts_with(['"', '\'']);
    let mut unescaped = None;
    if quoted {
        let (text, end) = crate::label_refs::unquote(line, offset)?;
        let tail = line[end + 1..].trim_start();
        if !tail.is_empty() && !tail.starts_with('#') {
            return None;
        }
        start += 1;
        raw = &line[start..end];
        unescaped = Some(text);
    } else if let Some((before, _)) = raw.split_once(" #") {
        raw = before.trim_end();
    }
    let text = unescaped.unwrap_or_else(|| raw.to_owned());
    let numeric = raw.replace('_', "");
    let typed_scalar =
        matches!(raw.to_ascii_lowercase().as_str(), "null" | "~" | "true" | "false" | "yes" | "no" | "on" | "off" | "y" | "n")
            || numeric.parse::<f64>().is_ok()
            || numeric.strip_prefix("0x").is_some_and(|v| u64::from_str_radix(v, 16).is_ok())
            || numeric.strip_prefix("0o").is_some_and(|v| u64::from_str_radix(v, 8).is_ok());
    if !quoted && typed_scalar || text.is_empty() || text.contains("{{") || text.contains("${") {
        return None;
    }
    let column = line[..start].encode_utf16().count() as u32;
    Some(Value {
        text,
        range: Range::new(Position::new(line_number, column), Position::new(line_number, column + raw.encode_utf16().count() as u32)),
    })
}

struct Reference {
    group: String,
    kind: String,
    name: String,
    range: Range,
}
#[derive(Default)]
struct Manifest {
    api_version: Option<Option<String>>,
    kind: Option<String>,
    namespace: Option<Option<String>>,
    name: Option<(String, Range)>,
    invalid: bool,
    identity_fields: BTreeSet<Vec<String>>,
    crd_group: Option<String>,
    crd_kind: Option<String>,
    crd_scope: Option<Scope>,
    scale_api: Option<String>,
    scale_kind: Option<String>,
    scale_name: Option<(String, Range)>,
    references: Vec<(Vec<String>, String, Value, Range)>,
}

/// Identity is version-independent within an API group, but never group-free.
fn scoped_key(
    group: &str,
    kind: &str,
    name: &str,
    namespace: &Option<Option<String>>,
    scope: Scope,
    path: &str,
    isolated: bool,
) -> Option<String> {
    if !crate::is_dns_subdomain(name)
        || !kind.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        || !kind.bytes().all(|c| c.is_ascii_alphanumeric())
    {
        return None;
    }
    let file_owner = format!("file:{}:{path}", path.len());
    let owner = match scope {
        Scope::Cluster => "cluster".to_owned(),
        Scope::Namespaced => match namespace {
            Some(Some(ns)) if crate::is_dns_label(ns) => format!("namespace:{ns}"),
            Some(_) => return None,
            None => file_owner.clone(),
        },
    };
    let owner = if isolated && owner != file_owner { format!("{file_owner}:{owner}") } else { owner };
    let api = if group.is_empty() { "core".to_owned() } else { format!("group:{group}") };
    Some(format!("kube:{api}:{owner}:{kind}/{name}"))
}

/// Pod-reference fields have meaning only at schema-known paths, not when the
/// same words occur in a ConfigMap, annotations or arbitrary custom resource.
fn typed_reference(group: &str, kind: &str, parents: &[String], field: &str, value: &Value, range: Range) -> Option<Reference> {
    let path: Vec<&str> = parents.iter().map(String::as_str).collect();
    let pod = match (group, kind) {
        ("", "Pod") => Some(&["spec"][..]),
        ("", "ReplicationController")
        | ("apps" | "extensions", "Deployment" | "ReplicaSet" | "DaemonSet" | "StatefulSet")
        | ("batch", "Job") => Some(&["spec", "template", "spec"][..]),
        ("batch", "CronJob") => Some(&["spec", "jobTemplate", "spec", "template", "spec"][..]),
        _ => None,
    };
    let target = if let Some(base) = pod.and_then(|base| path.strip_prefix(base)) {
        match (base, field) {
            ([], "serviceAccountName") => Some(("", "ServiceAccount")),
            (["volumes", "secret"], "secretName") | (["imagePullSecrets"], "name") => Some(("", "Secret")),
            (["volumes", "configMap"], "name") => Some(("", "ConfigMap")),
            (["volumes", "persistentVolumeClaim"], "claimName") => Some(("", "PersistentVolumeClaim")),
            (["containers" | "initContainers" | "ephemeralContainers", "env", "valueFrom", "secretKeyRef"], "name")
            | (["containers" | "initContainers" | "ephemeralContainers", "envFrom", "secretRef"], "name") => Some(("", "Secret")),
            (["containers" | "initContainers" | "ephemeralContainers", "env", "valueFrom", "configMapKeyRef"], "name")
            | (["containers" | "initContainers" | "ephemeralContainers", "envFrom", "configMapRef"], "name") => Some(("", "ConfigMap")),
            _ => None,
        }
    } else if matches!((group, kind), ("networking.k8s.io" | "extensions", "Ingress")) && path == ["spec", "tls"] && field == "secretName" {
        Some(("", "Secret"))
    } else if matches!((group, kind), ("apps", "StatefulSet")) && path == ["spec"] && field == "serviceName" {
        Some(("", "Service"))
    } else if matches!((group, kind), ("networking.k8s.io" | "extensions", "Ingress"))
        && field == "name"
        && matches!(path.as_slice(), ["spec", "defaultBackend", "service"] | ["spec", "rules", "http", "paths", "backend", "service"])
    {
        Some(("", "Service"))
    } else {
        None
    }?;
    Some(Reference { group: target.0.into(), kind: target.1.into(), name: value.text.clone(), range })
}

fn parse(text: &str) -> Option<Vec<Manifest>> {
    let mut docs = Vec::new();
    let mut doc = Manifest::default();
    let mut stack: Vec<(usize, String)> = Vec::new();
    for (line_number, line) in text.split_inclusive('\n').enumerate() {
        let body = line.trim_end_matches(['\r', '\n']);
        if crate::manifest::is_separator(body) || matches!(body, "...") || body.starts_with("... #") {
            if docs.len() == MAX_ANCHORS {
                return None;
            }
            docs.push(std::mem::take(&mut doc));
            stack.clear();
            continue;
        }
        let mut indent = body.len() - body.trim_start().len();
        let mut trimmed = body.trim_start();
        while let Some(rest) = trimmed.strip_prefix("- ") {
            indent += 2;
            trimmed = rest;
        }
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((field, _)) = trimmed.split_once(':') else { continue };
        if field.starts_with(['"', '\'']) {
            let key = field.trim_matches(['"', '\'']);
            if indent == 0 && matches!(key, "apiVersion" | "kind" | "metadata")
                || stack.last().is_some_and(|(_, name)| name == "metadata") && matches!(key, "name" | "namespace")
            {
                doc.invalid = true;
            }
            continue;
        }
        if field.contains(' ') {
            continue;
        }
        while stack.last().is_some_and(|(at, _)| *at >= indent) {
            stack.pop();
        }
        let after_colon = body.len() - trimmed.len() + field.len() + 1;
        let value = value_of(body, line_number as u32, after_colon);
        let parents: Vec<String> = stack.iter().map(|(_, name)| name.clone()).collect();
        let parent: Vec<&str> = parents.iter().map(String::as_str).collect();
        if field == "metadata" && indent == 0 && value.is_some() {
            // Flow-style/alias metadata may hide a namespace. Do not treat it
            // as an omitted namespace or combine it with other stream objects.
            doc.invalid = true;
        }
        if matches!(
            (field, parent.as_slice()),
            ("apiVersion" | "kind", [])
                | ("name" | "namespace", ["metadata"])
                | ("group" | "scope", ["spec"])
                | ("kind", ["spec", "names"])
                | ("apiVersion" | "kind" | "name", ["spec", "scaleTargetRef"])
        ) {
            let mut key = parents.clone();
            key.push(field.into());
            doc.invalid |= !doc.identity_fields.insert(key);
        }
        if indent == 0 && field == "apiVersion" {
            doc.invalid |= doc.api_version.is_some();
            doc.api_version = Some(value.as_ref().map(|v| v.text.clone()));
        }
        if field == "namespace" && parent == ["metadata"] {
            doc.invalid |= doc.namespace.is_some();
            let scalar = body[after_colon..].trim_start();
            let yaml_null = !scalar.starts_with(['"', '\''])
                && value.as_ref().is_some_and(|v| matches!(v.text.as_str(), "null" | "Null" | "NULL" | "~"));
            doc.namespace = Some(value.as_ref().filter(|_| !yaml_null).map(|v| v.text.clone()));
        }
        if let Some(value) = value {
            match (field, parent.as_slice()) {
                ("kind", []) if indent == 0 => {
                    doc.invalid |= doc.kind.is_some();
                    doc.kind = Some(value.text.clone());
                }
                ("name", ["metadata"]) => {
                    doc.invalid |= doc.name.is_some();
                    doc.name = Some((value.text.clone(), value.range));
                }
                ("group", ["spec"]) => {
                    doc.invalid |= doc.crd_group.is_some();
                    doc.crd_group = Some(value.text.clone());
                }
                ("kind", ["spec", "names"]) => {
                    doc.invalid |= doc.crd_kind.is_some();
                    doc.crd_kind = Some(value.text.clone());
                }
                ("scope", ["spec"]) => {
                    doc.crd_scope = match value.text.as_str() {
                        "Namespaced" => Some(Scope::Namespaced),
                        "Cluster" => Some(Scope::Cluster),
                        _ => None,
                    }
                }
                ("apiVersion", ["spec", "scaleTargetRef"]) => doc.scale_api = Some(value.text.clone()),
                ("kind", ["spec", "scaleTargetRef"]) => doc.scale_kind = Some(value.text.clone()),
                ("name", ["spec", "scaleTargetRef"]) => doc.scale_name = Some((value.text.clone(), value.range)),
                _ => {}
            }
            if matches!(field, "name" | "secretName" | "serviceName" | "claimName" | "serviceAccountName" | "namespace") {
                if doc.references.len() == MAX_ANCHORS {
                    return None;
                }
                let range = value.range;
                doc.references.push((parents, field.into(), value, range));
            }
        } else {
            stack.push((indent, field.to_owned()));
        }
    }
    docs.push(doc);
    Some(docs)
}

/// Completion candidates reuse the exact schema-known reference paths without
/// creating index graph edges or claiming a source deployment target.
pub(crate) fn completion_reference(text:&str,at:Position)->Option<(String,String,String,Range,bool)> {
    if text.len()>64<<10||text.contains("{{"){return None;}
    let line=text.lines().nth(at.line as usize)?;
    let colon=line.find(':')?;let after=colon+1;
    let start=after+line[after..].len()-line[after..].trim_start().len();
    let raw=line[start..].trim_end();
    let quoted=raw.starts_with(['\'','"']);
    let (start,end)=if quoted {
        let quote=raw.chars().next()?;let value=&raw[1..];let end=value.find(quote)?;
        let tail=value[end+1..].trim_start();if !tail.is_empty()&&!tail.starts_with('#'){return None;}
        (start+1,start+1+end)
    }else{
        let value=raw.split_once(" #").map_or(raw,|(before,_)|before).trim_end();
        (start,start+value.len())
    };
    let range=Range::new(Position::new(at.line,line[..start].encode_utf16().count() as u32),Position::new(at.line,line[..end].encode_utf16().count() as u32));
    let offset=openidea_plugin::text::offset(text,at)?;
    let from=openidea_plugin::text::offset(text,range.start)?;let to=openidea_plugin::text::offset(text,range.end)?;
    if !(from..=to).contains(&offset)||!text[from..to].bytes().all(|b|b.is_ascii_lowercase()||b.is_ascii_digit()||matches!(b,b'-'|b'.')){return None;}
    let prefix=text[from..offset].to_owned();
    // Replacing only the candidate scalar lets an empty/partly typed reference
    // use the existing path recognizer; no synthetic result enters the graph.
    const SENTINEL:&str="completion-placeholder";
    let current=format!("{}{}{}",&text[..from],SENTINEL,&text[to..]);
    crate::convert::source_documents(&current)?;
    for doc in parse(&current)? {
        if !doc.references.iter().any(|(_,_,value,span)|span.start==range.start&&value.text==SENTINEL){continue;}
        if doc.invalid {return None;}
        let group=api_group(doc.api_version.as_ref()?.as_deref()?)?;
        let kind=doc.kind.as_deref()?;
        if builtin_scope(group,kind)!=Some(Scope::Namespaced){continue;}
        let namespace=doc.namespace.as_ref()?.as_ref()?.clone();
        if !crate::is_dns_label(&namespace){continue;}
        for (parents,field,value,span) in &doc.references {
            if span.start!=range.start||value.text!=SENTINEL {continue;}
            let reference=typed_reference(group,kind,parents,field,value,*span)?;
            if !reference.group.is_empty(){return None;}
            return Some((reference.kind,namespace,prefix,range,quoted));
        }
    }
    None
}

pub(crate) fn edges_in(path: &str, text: &str, isolated: bool) -> Edges {
    let Some(docs) = parse(text) else {
        return Edges::default();
    };
    // Only same-stream CRD facts are available to this document indexer. Conflicts
    // erase scope instead of choosing one declaration or querying the cluster.
    let mut custom: BTreeMap<(String, String), Option<Scope>> = BTreeMap::new();
    for doc in &docs {
        if doc.invalid
            || doc.api_version.as_ref().and_then(|v| v.as_deref()).and_then(api_group) != Some("apiextensions.k8s.io")
            || doc.kind.as_deref() != Some("CustomResourceDefinition")
        {
            continue;
        }
        if let (Some(group), Some(kind)) = (&doc.crd_group, &doc.crd_kind) {
            if !crate::is_dns_subdomain(group) || builtin_scope(group, kind).is_some() {
                continue;
            }
            custom
                .entry((group.clone(), kind.clone()))
                .and_modify(|scope| {
                    if *scope != doc.crd_scope {
                        *scope = None;
                    }
                })
                .or_insert(doc.crd_scope);
        }
    }
    let lookup =
        |group: &str, kind: &str| builtin_scope(group, kind).or_else(|| custom.get(&(group.into(), kind.into())).copied().flatten());
    let (mut definitions, mut references) = (Vec::new(), Vec::new());
    for doc in docs {
        if doc.invalid {
            continue;
        }
        let Some(group) = doc.api_version.as_ref().and_then(|v| v.as_deref()).and_then(api_group) else { continue };
        let Some(kind) = doc.kind.as_deref() else { continue };
        let Some(scope) = lookup(group, kind) else { continue };
        // A custom resource's scope is known only within this YAML stream; do not
        // bind a same-name CR from another file without its defining CRD facts.
        let local = isolated || builtin_scope(group, kind).is_none();
        let Some((name, range)) = &doc.name else {
            continue;
        };
        let Some(key) = scoped_key(group, kind, name, &doc.namespace, scope, path, local) else {
            continue;
        };
        definitions.push(Anchor { key, range: *range });
        for (parents, field, value, range) in &doc.references {
            let reference = if scope == Scope::Namespaced
                && parents == &["metadata".to_owned()]
                && field == "namespace"
                && crate::is_dns_label(&value.text)
            {
                Some(Reference { group: String::new(), kind: "Namespace".into(), name: value.text.clone(), range: *range })
            } else {
                typed_reference(group, kind, parents, field, value, *range)
            };
            if let Some(reference) = reference {
                if let Some(target_scope) = lookup(&reference.group, &reference.kind) {
                    if let Some(key) =
                        scoped_key(&reference.group, &reference.kind, &reference.name, &doc.namespace, target_scope, path, isolated)
                    {
                        references.push(Anchor { key, range: reference.range });
                    }
                }
            }
        }
        if group == "autoscaling" && kind == "HorizontalPodAutoscaler" {
            if let (Some(api), Some(kind), Some((name, range))) = (doc.scale_api, doc.scale_kind, doc.scale_name) {
                if let Some((group, scope)) = api_group(&api).and_then(|group| lookup(group, &kind).map(|scope| (group, scope))) {
                    // Known scale targets only. A CRD scope alone does not prove
                    // that the referenced version exposes a scale subresource.
                    if !matches!(
                        (group, kind.as_str()),
                        ("", "ReplicationController") | ("apps" | "extensions", "Deployment" | "ReplicaSet" | "StatefulSet")
                    ) {
                        continue;
                    }
                    let local = isolated || builtin_scope(group, &kind).is_none();
                    if let Some(key) = scoped_key(group, &kind, &name, &doc.namespace, scope, path, local) {
                        references.push(Anchor { key, range });
                    }
                }
            }
        }
    }
    // Bound the private read-only model before adding reverse relationships.
    if definitions.len() + references.len() > MAX_ANCHORS {
        return Edges::default();
    }
    let mut markers = references.clone();
    markers.extend(definitions.iter().map(|d| Anchor { key: users_key(&d.key), range: d.range }));
    definitions.extend(references.iter().map(|r| Anchor { key: users_key(&r.key), range: r.range }));
    markers.truncate(MAX_ANCHORS);
    definitions.truncate(MAX_ANCHORS);
    Edges { definitions, references, markers, ..Edges::default() }
}

#[cfg(test)]
fn edges(text: &str) -> Edges {
    edges_at("/fixture.yaml", text)
}
#[cfg(test)]
fn edges_at(path: &str, text: &str) -> Edges {
    edges_in(path, text, false)
}

const TRANSFORMER_MARKERS: [&str; 4] = ["kustomization.yaml", "kustomization.yml", "Kustomization", "Chart.yaml"];

pub(crate) fn transformer_marker(path: &str) -> bool {
    path.rsplit('/').next().is_some_and(|name| TRANSFORMER_MARKERS.contains(&name))
}

/// Per project root: whether any kustomization or chart exists, found once instead of once per indexed file.
#[derive(Default)]
pub(crate) struct TransformerFlags(BTreeMap<String, bool>);

impl TransformerFlags {
    pub(crate) fn clear(&mut self) {
        self.0.clear();
    }

    fn present(&mut self, project: &openidea_plugin::Project, root: &str) -> bool {
        if let Some(present) = self.0.get(root) {
            return *present;
        }
        let present = ["**/kustomization.yaml", "**/kustomization.yml", "**/Kustomization", "**/Chart.yaml"]
            .iter()
            .any(|pattern| !matches!(project.find_files(pattern, 1), Ok(found) if found.is_empty()));
        self.0.insert(root.into(), present);
        present
    }
}

/// Overlay/Helm transforms can rewrite even explicitly written namespaces/names.
/// Isolate affected source files; reaching no known root or a probe/time bound
/// is also conservative, never evidence that no overlay exists.
pub(crate) fn deployment_isolation(path: &str, flags: &mut TransformerFlags) -> Option<bool> {
    let project = openidea_plugin::host::project_of(path)?;
    let root = project.root().ok()?;
    // A sibling overlay can reference a base outside its own ancestor chain.
    // A detected deployment transformer anywhere in this indexed project makes
    // source relationships file-local; do not attempt to evaluate overlays.
    if flags.present(&project, &root) {
        return Some(true);
    }
    let root = Path::new(&root);
    let mut directory = Path::new(path).parent()?;
    if !directory.starts_with(root) {
        return None;
    }
    let started = Instant::now();
    for _ in 0..32 {
        for marker in TRANSFORMER_MARKERS {
            let candidate = directory.join(marker);
            match project.exists(candidate.to_str()?) {
                Ok(true) => return Some(true),
                Ok(false) => {}
                Err(_) => return Some(true),
            }
        }
        if directory == root {
            return Some(false);
        }
        if started.elapsed() > Duration::from_millis(20) {
            return Some(true);
        }
        directory = directory.parent()?;
    }
    Some(true)
}

/// Warms the private caches for every indexed YAML file, opened or not. It publishes no
/// anchors or markers: a users key would have definitions only at usages, which Rename cannot close.
impl DocumentIndexer for KubeExplorer {
    fn index(&mut self, doc: &Document) -> Edges {
        self.navigation.warm(&doc.path, &doc.text);
        self.crd_schema.warm(&doc.path, &doc.text);
        self.label_refs.warm(&doc.path, &doc.text);
        self.kustomize_paths.warm(&doc.path);
        self.helm_assistance.warm(&doc.path, &doc.text);
        Edges::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(anchors: &[Anchor]) -> Vec<&str> {
        anchors.iter().map(|a| a.key.as_str()).collect()
    }

    #[test]
    fn value_of_handles_escaped_quote() {
        let line = r#"name: "a\"b" # c"#;
        let value = value_of(line, 0, 5).unwrap();
        assert_eq!(value.text, "a\"b");
        assert_eq!(value.range, Range::new(Position::new(0, 7), Position::new(0, 11)));
        assert_eq!(value_of("name: 'it''s'", 0, 5).unwrap().text, "it's");
        assert!(value_of("name: \"a\" x", 0, 5).is_none());
    }

    #[test]
    fn separator_with_trailing_space_or_tag() {
        for separator in ["--- ", "--- !tag", "---\t"] {
            let text = format!("apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: a\n  namespace: ns\n{separator}\napiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: b\n  namespace: ns\n");
            let defined: Vec<_> = edges(&text).definitions.iter().filter(|d| d.key.starts_with("kube:")).map(|d| d.key.clone()).collect();
            assert_eq!(defined.len(), 2, "{separator:?}: {defined:?}");
        }
    }

    #[test]
    fn metadata_name_defines_and_fields_reference() {
        let text = "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: web\nspec:\n  template:\n    spec:\n      serviceAccountName: runner\n      containers:\n        - name: app\n          envFrom:\n            - configMapRef:\n                name: \"app-config\"\n          env:\n            - name: A\n              valueFrom:\n                secretKeyRef:\n                  name: db\n                  key: pw\n      volumes:\n        - name: v\n          secret:\n            secretName: certs\n---\napiVersion: autoscaling/v2\nkind: HorizontalPodAutoscaler\nmetadata:\n  name: web-hpa\nspec:\n  scaleTargetRef:\n    apiVersion: apps/v1\n    kind: Deployment\n    name: web\n";
        let found = edges(text);
        assert_eq!(
            keys(&found.definitions[..2]),
            [
                "kube:group:apps:file:13:/fixture.yaml:Deployment/web",
                "kube:group:autoscaling:file:13:/fixture.yaml:HorizontalPodAutoscaler/web-hpa"
            ]
        );
        assert_eq!(
            keys(&found.references),
            [
                "kube:core:file:13:/fixture.yaml:ServiceAccount/runner",
                "kube:core:file:13:/fixture.yaml:ConfigMap/app-config",
                "kube:core:file:13:/fixture.yaml:Secret/db",
                "kube:core:file:13:/fixture.yaml:Secret/certs",
                "kube:group:apps:file:13:/fixture.yaml:Deployment/web"
            ]
        );
        assert_eq!(found.definitions[0].range.start.line, 3);
        assert_eq!(found.definitions[0].range.start.character, 8);
    }

    #[test]
    fn markers_link_a_resource_to_its_users_and_back() {
        let found = edges(
            "apiVersion: v1\nkind: Secret\nmetadata:\n  name: db\n---\napiVersion: v1\nkind: Pod\nmetadata:\n  name: app\nspec:\n  containers:\n    - envFrom:\n        - secretRef:\n            name: db\n",
        );
        assert_eq!(
            keys(&found.markers),
            [
                "kube:core:file:13:/fixture.yaml:Secret/db",
                "kube-users:core:file:13:/fixture.yaml:Secret/db",
                "kube-users:core:file:13:/fixture.yaml:Pod/app"
            ]
        );
        assert_eq!(
            keys(&found.definitions),
            [
                "kube:core:file:13:/fixture.yaml:Secret/db",
                "kube:core:file:13:/fixture.yaml:Pod/app",
                "kube-users:core:file:13:/fixture.yaml:Secret/db"
            ]
        );
        assert_eq!(found.markers[0].range.start.line, 13);
        assert_eq!(found.markers[1].range.start.line, 3);
        assert_eq!(found.definitions[2].range.start.line, 13);
    }

    #[test]
    fn a_manifest_without_references_marks_only_its_definitions() {
        let found = edges("apiVersion: v1\nkind: Service\nmetadata:\n  name: web\n");
        assert_eq!(keys(&found.markers), ["kube-users:core:file:13:/fixture.yaml:Service/web"]);
        assert!(found.references.is_empty());
    }

    #[test]
    fn templated_names_and_container_names_are_ignored() {
        let found = edges("apiVersion: v1\nkind: Service\nmetadata:\n  name: {{ .Release.Name }}\nspec:\n  containers:\n    - name: app\n");
        assert!(found.definitions.is_empty());
        assert!(found.references.is_empty());
    }
    #[test]
    fn namespace_after_spec_still_scopes_all_references() {
        let first = edges_at(
            "/p/a.yaml",
            "apiVersion: v1\nkind: Pod\nspec:\n  volumes:\n    - secret:\n        secretName: db\nmetadata:\n  name: web\n  namespace: team-a\n",
        );
        let other = edges_at("/p/b.yaml", "apiVersion: v1\nkind: Secret\nmetadata:\n  namespace: team-b\n  name: db\n");
        let same = edges_at("/p/c.yaml", "apiVersion: v1\nkind: Secret\nmetadata:\n  namespace: team-a\n  name: db\n");
        assert_eq!(first.references[0].key, same.definitions[0].key);
        assert_ne!(first.references[0].key, other.definitions[0].key);
        assert!(first.references[0].key.contains("namespace:team-a:Secret/db"));
    }

    #[test]
    fn omitted_namespace_is_file_local_and_not_explicit_default() {
        let source = "apiVersion: v1\nkind: Secret\nmetadata:\n  name: db\n";
        let a = edges_at("/p/a.yaml", source);
        let b = edges_at("/p/b.yaml", source);
        let explicit = edges_at("/p/a.yaml", &format!("{source}  namespace: default\n"));
        assert_ne!(a.definitions[0].key, b.definitions[0].key);
        assert_ne!(a.definitions[0].key, explicit.definitions[0].key);
    }

    #[test]
    fn dynamic_invalid_and_empty_namespaces_do_not_publish_namespaced_anchors() {
        for namespace in ["{{ .Values.namespace }}", "${NAMESPACE}", "bad namespace", "", "null", "Null", "NULL", "~"] {
            let source = format!(
                "apiVersion: v1\nkind: Pod\nmetadata:\n  name: web\n  namespace: {namespace}\nspec:\n  serviceAccountName: runner\n"
            );
            let found = edges_at("/p/a.yaml", &source);
            assert!(found.definitions.is_empty(), "{namespace}");
            assert!(found.references.is_empty(), "{namespace}");
        }
    }

    #[test]
    fn cluster_resources_are_not_partitioned_by_namespace() {
        let a = edges_at("/p/a.yaml", "apiVersion: v1\nkind: Namespace\nmetadata:\n  name: team-a\n");
        let b = edges_at("/p/b.yaml", "apiVersion: v1\nkind: Namespace\nmetadata:\n  name: team-a\n  namespace: ignored\n");
        assert_eq!(a.definitions[0].key, b.definitions[0].key);
        assert_eq!(a.definitions[0].key, "kube:core:cluster:Namespace/team-a");
    }

    #[test]
    fn quoted_names_cover_only_the_exact_literal_with_utf16_positions() {
        let found = edges_at("/p/a.yaml", "# 🦀\napiVersion: v1\nkind: Secret\nmetadata:\n  namespace: team-a\n  name: \"db\"\n");
        let range = found.definitions[0].range;
        assert_eq!(range.start, openidea_plugin::Position::new(5, 9));
        assert_eq!(range.end, openidea_plugin::Position::new(5, 11));
        let escaped = edges_at(
            "/p/a.yaml",
            r#"apiVersion: v1
kind: Secret
metadata:
  namespace: team-a
  name: "\u0064b"
"#,
        );
        assert!(escaped.definitions.is_empty());
        for name in ["'db''extra'", "\"db\"extra", "'db"] {
            let malformed =
                edges_at("/p/a.yaml", &format!("apiVersion: v1\nkind: Secret\nmetadata:\n  namespace: team-a\n  name: {name}\n"));
            assert!(malformed.definitions.is_empty(), "{name}");
        }
    }
    fn resource(api: &str, kind: &str, namespace: Option<&str>, name: &str) -> String {
        format!(
            "apiVersion: {api}\nkind: {kind}\nmetadata:\n  name: {name}\n{}",
            namespace.map(|ns| format!("  namespace: {ns}\n")).unwrap_or_default()
        )
    }

    fn crd(group: &str, kind: &str, scope: &str) -> String {
        format!(
            "apiVersion: apiextensions.k8s.io/v1\nkind: CustomResourceDefinition\nmetadata:\n  name: widgets.{group}\nspec:\n  group: {group}\n  names:\n    kind: {kind}\n    plural: widgets\n  scope: {scope}\n"
        )
    }

    #[test]
    fn api_group_is_part_of_resource_identity_and_versions_are_not() {
        let core = edges_at("/p/a.yaml", &resource("v1", "Event", Some("team"), "same"));
        let events = edges_at("/p/b.yaml", &resource("events.k8s.io/v1", "Event", Some("team"), "same"));
        assert_ne!(core.definitions[0].key, events.definitions[0].key);
        let v1 = edges_at("/p/a.yaml", &resource("apps/v1", "Deployment", Some("team"), "same"));
        let beta = edges_at("/p/b.yaml", &resource("apps/v1beta1", "Deployment", Some("team"), "same"));
        assert_eq!(v1.definitions[0].key, beta.definitions[0].key);
    }

    #[test]
    fn missing_unknown_dynamic_or_malformed_api_identity_does_not_create_anchors() {
        for api in ["", "{{ .Values.api }}", "apps/v1/extra", "example.invalid/v1", "v2"] {
            let found = edges(&resource(api, "Deployment", Some("team"), "same"));
            assert!(found.definitions.is_empty() && found.references.is_empty(), "{api}");
        }
        let missing = edges("kind: Secret\nmetadata:\n  name: db\n");
        assert!(missing.definitions.is_empty());
    }

    #[test]
    fn custom_kinds_do_not_inherit_builtin_scope_by_kind_name() {
        let custom = edges(&resource("example.com/v1", "Namespace", Some("team"), "same"));
        assert!(custom.definitions.is_empty());
        let declared = format!(
            "{}---\n{}",
            crd("example.com", "Namespace", "Namespaced"),
            resource("example.com/v1", "Namespace", Some("team"), "same")
        );
        let declared = edges(&declared);
        assert!(declared.definitions.iter().any(|d| d.key.contains("example.com:file:13:/fixture.yaml:namespace:team:Namespace/same")));
        assert!(!declared.definitions.iter().any(|d| d.key.contains("example.com:cluster:Namespace/same")));
    }

    #[test]
    fn custom_scope_is_same_stream_order_independent_and_conflicts_stay_unknown() {
        let object = resource("example.com/v1", "Widget", None, "one");
        let document = format!("{object}---\n{}", crd("example.com", "Widget", "Cluster"));
        let cluster = edges_at("/p/a.yaml", &document);
        assert!(cluster.definitions.iter().any(|d| d.key.contains("example.com:file:9:/p/a.yaml:cluster:Widget/one")));
        assert!(edges_at("/p/other.yaml", &object).definitions.is_empty(), "a CRD from another indexing call must not leak scope");
        let conflict = edges(&format!("{document}---\n{}", crd("example.com", "Widget", "Namespaced")));
        assert!(!conflict.definitions.iter().any(|d| d.key.contains("Widget/one")));
    }

    #[test]
    fn arbitrary_custom_fields_and_configmap_data_are_not_resource_references() {
        let custom = format!(
            "{}---\n{}spec:\n  secretName: db\n  serviceAccountName: runner\n  secretRef:\n    name: db\n",
            crd("example.com", "Widget", "Namespaced"),
            resource("example.com/v1", "Widget", Some("team"), "one")
        );
        assert!(edges(&custom).references.iter().all(|r| r.key.ends_with("Namespace/team")));
        let config =
            format!("{}data:\n  secretName: db\n  configMapRef:\n    name: config\n", resource("v1", "ConfigMap", Some("team"), "one"));
        assert!(edges(&config).references.iter().all(|r| r.key.ends_with("Namespace/team")));
    }

    #[test]
    fn scale_references_use_the_declared_target_group_and_only_known_scale_targets() {
        let hpa = "apiVersion: autoscaling/v2\nkind: HorizontalPodAutoscaler\nmetadata:\n  namespace: team\n  name: hpa\nspec:\n  scaleTargetRef:\n    apiVersion: apps/v1\n    kind: Deployment\n    name: web\n";
        let good = edges(hpa);
        assert!(good.references.iter().any(|r| r.key == "kube:group:apps:namespace:team:Deployment/web"));
        assert!(
            edges(&hpa.replace("apiVersion: apps/v1", "apiVersion: evil.example/v1"))
                .references
                .iter()
                .all(|r| r.key.ends_with("Namespace/team"))
        );
        assert!(
            edges(&hpa.replace("kind: Deployment", "kind: ConfigMap").replace("apiVersion: apps/v1", "apiVersion: v1"))
                .references
                .iter()
                .all(|r| r.key.ends_with("Namespace/team"))
        );
    }

    #[test]
    fn duplicate_identity_fields_suppress_the_document() {
        let source = "apiVersion: v1\nkind: Secret\nmetadata:\n  name: db\n  namespace: team\n  namespace: other\n";
        assert!(edges(source).definitions.is_empty());
        let source = "apiVersion: v1\napiVersion: other/v1\nkind: Secret\nmetadata:\n  name: db\n";
        assert!(edges(source).definitions.is_empty());
    }

    #[test]
    fn service_references_use_supported_schema_paths() {
        let ingress = "apiVersion: networking.k8s.io/v1\nkind: Ingress\nmetadata:\n  namespace: team\n  name: web\nspec:\n  defaultBackend:\n    service:\n      name: backend\n      port:\n        number: 80\n";
        assert!(edges(ingress).references.iter().any(|r| r.key == "kube:core:namespace:team:Service/backend"));
        let stateful =
            "apiVersion: apps/v1\nkind: StatefulSet\nmetadata:\n  namespace: team\n  name: web\nspec:\n  serviceName: headless\n";
        assert!(edges(stateful).references.iter().any(|r| r.key == "kube:core:namespace:team:Service/headless"));
        let unrelated = "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  namespace: team\n  name: web\nspec:\n  serviceName: not-a-deployment-reference\n";
        assert!(edges(unrelated).references.iter().all(|r| r.key.ends_with("Namespace/team")));
    }

    fn indexed(kube: &mut KubeExplorer, path: &str, text: &str) -> Edges {
        let _ = kube;
        deployment_isolation(path, &mut TransformerFlags::default()).map_or_else(Edges::default, |isolated| edges_in(path, text, isolated))
    }

    #[test]
    fn known_project_overlay_or_chart_isolates_read_only_associations() {
        use openidea_plugin::mock::MockHost;
        for marker in ["overlays/prod/kustomization.yaml", "charts/app/Chart.yaml"] {
            let host = MockHost::install();
            host.add_file(marker, "namespace: prod\n");
            let mut kube = KubeExplorer::default();
            let source = resource("v1", "Secret", Some("team"), "same");
            let first = indexed(&mut kube, "/project/common/a.yaml", &source);
            let second = indexed(&mut kube, "/project/common/b.yaml", &source);
            assert_ne!(first.definitions[0].key, second.definitions[0].key, "{marker}");
        }
    }

    #[test]
    fn read_only_explicit_namespace_candidates_are_project_owned() {
        use openidea_plugin::mock::MockHost;
        let host = MockHost::install();
        let mut kube = KubeExplorer::default();
        let source = resource("v1", "Secret", Some("team"), "same");
        let first = indexed(&mut kube, "/project/a.yaml", &source);
        let second = indexed(&mut kube, "/project/b.yaml", &source);
        assert_eq!(first.definitions[0].key, second.definitions[0].key);
        let _host = host.with_capabilities(&[]);
        assert!(indexed(&mut kube, "/project/a.yaml", &source).definitions.is_empty());
    }

    #[test]
    fn oversized_candidate_stream_is_not_published_as_a_partial_rename_graph() {
        let mut source = resource("v1", "Pod", Some("team"), "web");
        source.push_str("spec:\n  imagePullSecrets:\n");
        for i in 0..MAX_ANCHORS {
            source.push_str(&format!("    - name: secret-{i}\n"));
        }
        let found = edges(&source);
        assert!(found.definitions.is_empty() && found.references.is_empty());
    }
    #[test]
    fn yaml_typed_names_are_not_equated_with_quoted_string_names() {
        for scalar in ["123", "true", "yes", "on", "null", "0xabc", "1e3"] {
            assert!(edges(&resource("v1", "Secret", Some("team"), scalar)).definitions.is_empty(), "{scalar}");
            let quoted = edges(&resource("v1", "Secret", Some("team"), &format!("\"{scalar}\"")));
            assert!(!quoted.definitions.is_empty(), "quoted {scalar}");
        }
    }
    #[test]
    fn key_tags_and_path_lengths_prevent_sentinel_and_path_collisions() {
        let ns = Some(Some("team".into()));
        let core = scoped_key("", "Secret", "db", &ns, Scope::Namespaced, "/p", false).unwrap();
        let custom = scoped_key("core", "Secret", "db", &ns, Scope::Namespaced, "/p", false).unwrap();
        assert_ne!(core, custom);
        let implicit = scoped_key("", "Secret", "db", &None, Scope::Namespaced, "/p:namespace:team", false).unwrap();
        let transformed = scoped_key("", "Secret", "db", &ns, Scope::Namespaced, "/p", true).unwrap();
        assert_ne!(implicit, transformed);
    }
    #[test]
    fn ambiguous_crd_scope_and_hidden_metadata_do_not_establish_names() {
        let duplicate = crd("example.com", "Widget", "Cluster").replace("  scope: Cluster", "  scope: Cluster\n  scope: Namespaced");
        let source = format!("{duplicate}---\n{}", resource("example.com/v1", "Widget", Some("team"), "one"));
        assert!(edges(&source).definitions.is_empty());
        for metadata in [
            "metadata: {name: web, namespace: team}",
            "\"metadata\": {name: web, namespace: team}",
            "metadata:\n  name: web\n  \"namespace\": team",
        ] {
            let source = format!("apiVersion: v1\nkind: Pod\n{metadata}\nspec:\n  serviceAccountName: runner\n");
            let found = edges(&source);
            assert!(found.definitions.is_empty() && found.references.is_empty(), "{metadata}");
        }
    }
}
