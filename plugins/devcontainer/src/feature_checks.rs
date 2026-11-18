//! Option checks for local `./feature` folders that ship a literal devcontainer-feature.json.
//! Remote (OCI, tarball) and dynamic ids are never fetched or judged.
use openidea_plugin::{Error, host};
use serde_json::Value;

use crate::checks::{Finding, FEATURES_MALFORMED, feature_members};

thread_local! {
    static MANIFESTS: std::cell::RefCell<std::collections::HashMap<String, (u64, Option<Value>)>> = Default::default();
}

/// Parsed manifest, cached per path and content hash.
fn manifest_value(path: &str, source: &str) -> Option<Value> {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut hasher);
    let hash = hasher.finish();
    if let Some((cached, value)) = MANIFESTS.with(|cache| cache.borrow().get(path).cloned()) {
        if cached == hash {
            return value;
        }
    }
    let value = serde_json::from_str::<Value>(source).ok();
    MANIFESTS.with(|cache| cache.borrow_mut().insert(path.to_owned(), (hash, value.clone())));
    value
}

pub(crate) fn local_feature_findings(doc_path: &str, text: &str) -> Vec<Finding> {
    let features = feature_members(text);
    if !features.iter().any(|(f, _)| f.key.starts_with("./") || f.key.starts_with("../")) {
        return vec![];
    }
    let Some(project) = host::project_of(doc_path) else { return vec![] };
    let Ok(root) = project.root() else { return vec![] };
    let prefix = format!("{}/", root.replace('\\', "/").trim_end_matches('/'));
    let doc_path = doc_path.replace('\\', "/");
    let Some((folder, _)) = doc_path.rsplit_once('/') else { return vec![] };
    let mut findings = vec![];
    for (feature, options) in &features {
        if !(feature.key.starts_with("./") || feature.key.starts_with("../")) {
            continue;
        }
        let Some(dir) = crate::compose::resolve(folder, &feature.key, &prefix) else { continue };
        let manifest_path = format!("{dir}/devcontainer-feature.json");
        let manifest = match project.read_file(&manifest_path) {
            Ok(source) if source.len() <= 256 * 1024 => manifest_value(&manifest_path, &source),
            Ok(_) => None,
            Err(Error::NotFound(_) | Error::Invalid(_)) => {
                findings.push(Finding::warning(feature.key_range.clone(), FEATURES_MALFORMED, format!("Local feature `{}` has no devcontainer-feature.json.", feature.key)));
                continue;
            }
            Err(_) => None,
        };
        let Some(declared) = manifest.as_ref().and_then(|m| m.get("options")).and_then(Value::as_object) else { continue };
        for option in options {
            let Some(spec) = declared.get(&option.key) else { continue };
            let Ok(value) = serde_json::from_str::<Value>(&text[option.value_range.clone()]) else { continue };
            let problem = match spec.get("type").and_then(Value::as_str) {
                Some("boolean") if !value.is_boolean() => Some("a boolean".to_owned()),
                Some("string") => match spec.get("enum").and_then(Value::as_array) {
                    Some(allowed) if !allowed.contains(&value) => Some(format!("one of {}", allowed.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "))),
                    None if !value.is_string() => Some("a string".to_owned()),
                    _ => None,
                },
                _ => None,
            };
            if let Some(expected) = problem {
                findings.push(Finding::warning(option.value_range.clone(), FEATURES_MALFORMED, format!("Option `{}` of local feature `{}` expects {expected}.", option.key, feature.key)));
            }
        }
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;

    #[test]
    fn local_feature_options_are_checked_against_the_manifest() {
        let host = MockHost::install();
        host.add_file(".devcontainer/devcontainer.json", "");
        host.add_file(".devcontainer/f/devcontainer-feature.json", r#"{"id":"f","options":{"on":{"type":"boolean","default":false},"v":{"type":"string","enum":["a","b"],"default":"a"},"s":{"type":"string"}}}"#);
        host.add_file(".devcontainer/g/devcontainer-feature.json", "{ not json");
        let text = r#"{"features":{"./f":{"on":"yes","v":"c","s":1,"unknown":1},"./g":{"on":1},"./missing":{},"ghcr.io/o/r/x:1":{"on":1}}}"#;
        let messages: Vec<String> = local_feature_findings("/project/.devcontainer/devcontainer.json", text).into_iter().map(|f| f.message).collect();
        assert_eq!(messages.len(), 4, "{messages:?}");
        assert!(messages.iter().any(|m| m.contains("`on`") && m.contains("boolean")));
        assert!(messages.iter().any(|m| m.contains("one of a, b")));
        assert!(messages.iter().any(|m| m.contains("`s`") && m.contains("string")));
        assert!(messages.iter().any(|m| m.contains("./missing")));
        let ok = r#"{"features":{"./f":{"on":true,"v":"b","s":"x"}}}"#;
        assert!(local_feature_findings("/project/.devcontainer/devcontainer.json", ok).is_empty());
    }

    #[test]
    fn oci_ids_flag_only_provable_shape_errors() {
        let ids = ["ghcr.io/Org/repo/x:1", "ghcr.io/org/repo/x:", "ghcr.io/org/repo/x:1", "ghcr.io/org/repo/x@sha256:abc", "localhost:5000/org/x", "https://e.com/f.tgz", "./local", "owner/repo/f@v1"];
        let text = format!("{{\"features\":{{{}}}}}", ids.map(|i| format!("\"{i}\":{{}}")).join(","));
        let found: Vec<String> = crate::checks::check(&text).into_iter().map(|f| f.message).collect();
        assert_eq!(found.len(), 2, "{found:?}");
    }
}
