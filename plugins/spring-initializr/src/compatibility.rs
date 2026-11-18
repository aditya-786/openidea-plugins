//! Compatibility comes from the selected server snapshot, never a guessed
//! Boot-to-JDK/build table. Missing range facts do not establish compatibility.
use crate::{Launcher, START, dependencies, starters};
use openidea_plugin::Error;

fn version(value: &str) -> bool {
    let (numbers, qualifier) = value.split_once('-').unwrap_or((value, ""));
    let numeric = numbers.split('.').all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    numeric
        && dependencies::compare_versions(value,value).is_some()
        && (qualifier.is_empty()
            || qualifier == "SNAPSHOT"
            || ["M", "RC"]
                .iter()
                .any(|prefix| qualifier.strip_prefix(prefix).is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))))
}

/// Supported Initializr lower-bound and interval forms. Unknown syntax remains
/// unknown rather than accidentally becoming an incompatibility assertion.
pub(crate) fn allows(boot: &str, range: &str) -> Option<bool> {
    if !version(boot) {
        return None;
    }
    let range = range.trim();
    if range.starts_with(['[', '(']) && range.ends_with([']', ')']) {
        let (low, high) = range[1..range.len() - 1].split_once(',')?;
        if (!low.trim().is_empty() && !version(low.trim())) || (!high.trim().is_empty() && !version(high.trim())) {
            return None;
        }
        if low.trim().is_empty() && high.trim().is_empty() {
            return None;
        }
        if !low.trim().is_empty()&&!high.trim().is_empty() {
            match dependencies::compare_versions(low.trim(),high.trim())? {
                std::cmp::Ordering::Greater=>return None,
                std::cmp::Ordering::Equal if !(range.starts_with('[')&&range.ends_with(']'))=>return None,
                _=>{}
            }
        }
    } else if !version(range) {
        return None;
    }
    Some(dependencies::in_range(boot, range))
}

/// Initializr client metadata uses versionRange; older snapshots used compatibilityRange.
/// Both are accepted only when they agree, avoiding precedence-dependent requests.
pub(crate) fn declared_range(value: &serde_json::Value) -> Option<&str> {
    value["versionRange"].as_str().or_else(|| value["compatibilityRange"].as_str())
}
fn valid_range(value: &serde_json::Value) -> bool {
    for key in ["versionRange", "compatibilityRange"] {
        if value.get(key).is_some_and(|v| !v.is_null() && v.as_str().is_none_or(|s| s.len() > 256)) { return false; }
    }
    match (value["versionRange"].as_str(), value["compatibilityRange"].as_str()) {
        (Some(a),Some(b)) => a == b,
        _ => true,
    }
}

pub(crate) fn valid_metadata(metadata: &serde_json::Value) -> bool {
    let text = |object: &serde_json::Value, key: &str, limit: usize| match object.get(key) {
        None | Some(serde_json::Value::Null) => true,
        Some(serde_json::Value::String(value)) => value.len() <= limit,
        _ => false,
    };
    if !metadata.is_object() || (!metadata["bootVersion"].is_object() && !metadata["dependencies"].is_object()) {
        return false;
    }
    for key in ["bootVersion", "javaVersion", "type", "language", "packaging"] {
        if metadata.get(key).is_some_and(|node| !node.is_object())
            || metadata[key].get("values").is_some_and(|values| !values.is_array())
            || !text(&metadata[key], "default", 256)
        {
            return false;
        }
        if let Some(options) = metadata[key]["values"].as_array() {
            if options.len() > 512 {
                return false;
            }
            let mut ids = std::collections::BTreeSet::new();
            for option in options {
                if !valid_range(option) {return false;}
                let Some(id) = option["id"].as_str().filter(|id| !id.is_empty() && id.len() <= 256) else {
                    return false;
                };
                if key == "type" {
                    if option.get("tags").is_some_and(|tags| !tags.is_null() && !tags.is_object()) {
                        return false;
                    }
                    for tag in ["build", "format"] {
                        if !text(&option["tags"], tag, 256) {
                            return false;
                        }
                    }
                }
                if !ids.insert(id)
                    || !text(option, "name", 512)
                    || declared_range(option).is_some_and(|range| range.len() > 256)
                {
                    return false;
                }
            }
        }
    }
    if metadata.get("dependencies").is_some_and(|node| !node.is_object())
        || metadata["dependencies"].get("values").is_some_and(|values| !values.is_array())
    {
        return false;
    }
    let groups = metadata["dependencies"]["values"].as_array();
    if groups.is_some_and(|groups| {
            groups.len() > 128 || groups.iter().any(|group| !group.is_object()
                || !text(group, "name", 512)
                || group.get("values").is_some_and(|values| !values.is_array()))
        })
    {
        return false;
    }
    let mut ids = std::collections::BTreeSet::new();
    for dependency in groups.into_iter().flatten().flat_map(|group| group["values"].as_array().into_iter().flatten()) {
        if !valid_range(dependency) {return false;}
        let Some(id) = dependency["id"].as_str().filter(|id| !id.is_empty() && id.len() <= 256) else {
            return false;
        };
        if ids.len() >= 4096
            || !ids.insert(id)
            || !text(dependency, "name", 512)
            || !text(dependency, "description", 8192)
            || declared_range(dependency).is_some_and(|v| v.len() > 256)
        {
            return false;
        }
    }
    true
}

impl Launcher {
    pub(crate) fn load_metadata(&mut self, refresh: bool) {
        let _ = self.changed();
        if self.server != START {
            self.catalog_status = Some(format!("Catalogue access is limited to {START}; custom-server compatibility is unverified."));
            return;
        }
        if !refresh && self.metadata.is_some() {
            return;
        }
        let result = starters::get("/metadata/client")
            .map_err(|e| ui_kit::denied::map(e, "Refreshing the catalogue", "net:start.spring.io", "Spring Initializr Launcher"))
            .and_then(|metadata| {
            if !valid_metadata(&metadata) {
                return Err(Error::Invalid("Server metadata is invalid or exceeds catalogue limits; previous catalogue retained.".into()));
            }
            match self.persist(Some(metadata.clone())) {
                Ok(()) => {
                    self.catalog_status =
                        Some("Server metadata refreshed and saved. Only declared choice/range facts are validated.".into())
                }
                Err(Error::Denied(_)) => {
                    self.catalog_status = Some("Server metadata loaded for this session; storage is unavailable.".into())
                }
                Err(error) => return Err(error),
            }
            Ok(metadata)
        });
        match result {
            Ok(metadata) => {self.imports.invalidate();self.starter_edits.invalidate(); self.metadata = Some(metadata);},
            Err(error) => {
                self.catalog_status = Some(format!(
                    "Catalogue refresh failed: {error}. {}",
                    if self.metadata.is_some() {
                        "Using the previous saved catalogue."
                    } else {
                        "Compatibility is unverified; type values manually."
                    }
                ))
            }
        }
    }

    pub(crate) fn compatibility_problem(&mut self) -> Option<String> {
        let fields: std::collections::BTreeMap<_, _> = ["bootVersion", "javaVersion", "type", "language", "packaging", "dependencies"]
            .into_iter()
            .map(|key| (key, self.get(key)))
            .collect();
        let metadata = self.metadata.as_ref()?;
        let boot = fields["bootVersion"].as_str();
        let boot = if boot.is_empty() { metadata["bootVersion"]["default"].as_str().unwrap_or("") } else { boot };
        for key in ["bootVersion", "javaVersion", "type", "language", "packaging"] {
            let selected = if key == "bootVersion" { boot } else { fields[key].as_str() };
            if key == "bootVersion" && selected.is_empty() {
                continue;
            }
            let Some(options) = metadata[key]["values"].as_array().filter(|v| !v.is_empty()) else { continue };
            let Some(option) = options.iter().find(|option| option["id"].as_str() == Some(selected)) else {
                return Some(format!(
                    "{key} `{selected}` is not offered by the saved server metadata. Refresh or select an offered value."
                ));
            };
            if declared_range(option).is_some_and(|range| allows(boot, range) == Some(false)) {
                return Some(format!("{key} `{selected}` is outside the server's declared range for Spring Boot {boot}."));
            }
        }
        for id in fields["dependencies"].split([',', ' ']).filter(|id| !id.is_empty()) {
            let dependency = metadata["dependencies"]["values"]
                .as_array()
                .into_iter()
                .flatten()
                .flat_map(|group| group["values"].as_array().into_iter().flatten())
                .find(|d| d["id"].as_str() == Some(id));
            if dependency.and_then(|d| declared_range(d)).is_some_and(|range| allows(boot, range) == Some(false)) {
                return Some(format!("Dependency `{id}` is outside the server's declared range for Spring Boot {boot}."));
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn refreshing_the_catalogue_denied_names_permission() {
        let host = openidea_plugin::mock::MockHost::install().with_capabilities(&["project.read"]);
        let mut launcher = Launcher::default();
        launcher.bind(&host.invocation()).unwrap();
        launcher.load_metadata(true);
        let status = launcher.catalog_status.unwrap();
        assert!(status.contains("needs the net:start.spring.io permission"), "{status}");
        assert!(status.contains("Settings › Plugins › Spring Initializr Launcher"), "{status}");
    }
    #[test]
    fn current_version_ranges_are_validated_and_conflicting_legacy_aliases_are_rejected() {
        use serde_json::json;
        let mut metadata = json!({"bootVersion":{"default":"4.1.2","values":[{"id":"4.1.2"}]},"dependencies":{"values":[{"name":"Developer Tools","values":[{"id":"modulith","versionRange":"[4.0.0,4.3.0-M1)"}]}]}});
        assert!(valid_metadata(&metadata));
        let dependency = &mut metadata["dependencies"]["values"][0]["values"][0];
        assert_eq!(declared_range(dependency), Some("[4.0.0,4.3.0-M1)"));
        dependency["compatibilityRange"] = json!("[4.0.0,4.3.0-M1)");
        assert!(valid_metadata(&metadata));
        metadata["dependencies"]["values"][0]["values"][0]["compatibilityRange"] = json!("[1.0.0,2.0.0)");
        assert!(!valid_metadata(&metadata));
        metadata["dependencies"]["values"][0]["values"][0]["versionRange"] = json!(false);
        assert!(!valid_metadata(&metadata));
    }
    #[test]
    fn ranges_observe_boundaries_and_unknown_qualifiers() {
        assert_eq!(allows("3.5.0", "[3.5.0,4.0.0)"), Some(true));
        assert_eq!(allows("4.0.0", "[3.5.0,4.0.0)"), Some(false));
        assert_eq!(allows("4.0.0-RC1", "[4.0.0-M1,4.0.0)"), Some(true));
        assert_eq!(allows("3.5.nope", "3.5.0"), None);
        assert_eq!(allows("4.0.0", "banana"), None);
    }
}
