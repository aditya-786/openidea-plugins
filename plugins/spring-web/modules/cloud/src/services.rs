//! Conservative, source-declared service identities. No active-profile,
//! environment, discovery-server or effective-classpath resolution is attempted.
use super::{Index, Key, StreamConfig, java, literal};
use std::collections::{BTreeMap, BTreeSet};

fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 255 && name.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
}

pub(super) fn client_name(annotation: &java::Annotation) -> Option<String> {
    for key in ["name", "value", "url"] {
        if annotation.args.iter().filter(|a| a.name == key).count() > 1 {
            return None;
        }
    }
    // A URL bypasses load balancing: its name is not the destination service.
    if !literal(annotation, &["url"])?.is_empty() {
        return None;
    }
    let name = literal(annotation, &["name"])?;
    let value = literal(annotation, &["value"])?;
    if !name.is_empty() && !value.is_empty() && name != value {
        return None;
    }
    let name = if name.is_empty() { value } else { name };
    valid_name(&name).then_some(name)
}

fn source_module(path: &str) -> Option<String> {
    let path = path.replace('\\', "/");
    let (module, source) = path.rsplit_once("/src/main/")?;
    (source.starts_with("java/") || source.starts_with("kotlin/")).then(|| module.to_owned())
}

fn config_module(path: &str) -> Option<(String, String)> {
    let path = path.replace('\\', "/");
    let (module, resource) = path.rsplit_once("/src/main/resources/")?;
    let file = resource.strip_prefix("config/").unwrap_or(resource);
    (!file.contains('/')).then(|| (module.to_owned(), file.to_owned()))
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Config {
    base: Option<String>,
    names: BTreeSet<String>,
    unresolved: bool,
}

impl Config {
    pub(super) fn parse(path: &str, text: &str, keys: &[Key]) -> Self {
        let Some((_, file)) = config_module(path) else { return Self::default() };
        if text.len() > 128 * 1024 {
            return Self { unresolved: true, ..Self::default() };
        }
        let names: Vec<_> = keys.iter().filter(|k| k.key == "spring.application.name").collect();
        // The small configuration parser is not a YAML evaluator or a Spring
        // property-source resolver. Withhold narrowing for unsupported overlays.
        let unresolved = names.len() > 1
            || keys.iter().any(|k| matches!(k.key.as_str(), "spring" | "spring.application" | "spring.config") && !k.value.is_empty())
            || keys.iter().any(|k| {
                matches!(
                    k.key.as_str(),
                    "spring.config.import" | "spring.config.location" | "spring.config.additional-location" | "spring.config.name"
                )
            })
            || text.lines().any(|l| {
                let l = l.trim();
                matches!(l, "---" | "..." | "#---" | "!---") || l.starts_with("--- ") || l.contains("<<:")
            });
        let mut result = Self { unresolved, ..Self::default() };
        for key in &names {
            if valid_name(&key.value) {
                result.names.insert(key.value.clone());
            } else {
                result.unresolved = true;
            }
        }
        let base = matches!(file.as_str(), "application.properties" | "application.yml" | "application.yaml");
        let conditional = keys.iter().any(|k| k.key == "spring.profiles" || k.key.starts_with("spring.config.activate."));
        if base && !conditional && !result.unresolved && names.len() == 1 {
            result.base = Some(names[0].value.clone());
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incomplete_config_inventory_cannot_exclude_a_service() {
        let mut index = Index::new(super::super::CONFIG_GLOBS, super::super::config_file);
        index.update("/project/admin/src/main/resources/application.properties", "spring.application.name=admin\n");
        let client =
            super::super::facts_in("Client.java", "@FeignClient(\"users\") interface Client { @GetMapping(\"/users\") String get(); }")
                .clients
                .remove(0);
        assert!(Identities::new(&index).allows(&client, "/project/admin/src/main/java/Controller.java"));
    }

    #[test]
    fn flow_style_spring_config_keeps_the_identity_unresolved() {
        let keys = |text: &str| super::super::config::keys("/p/users/src/main/resources/application.yaml", text);
        let text = "spring:\n  application:\n    name: users\n  config: {import: optional:other.yaml}\n";
        assert!(Config::parse("/p/users/src/main/resources/application.yaml", text, &keys(text)).unresolved);
    }

    #[test]
    fn module_identity_handles_windows_paths_and_never_parent_prefixes() {
        assert_eq!(source_module(r"C:\project\users\src\main\kotlin\Controller.kt").as_deref(), Some("C:/project/users"));
        assert_eq!(config_module(r"C:\project\users\src\main\resources\config\application.yaml").unwrap().0, "C:/project/users");
        assert!(source_module("/project/users/src/test/java/Controller.java").is_none());
        assert!(config_module("/project/users/src/main/resources/nested/application.yaml").is_none());
    }
}

#[derive(Default)]
pub(super) struct Identities(BTreeMap<String, String>);

impl Identities {
    pub(super) fn new(config: &Index<StreamConfig>) -> Self {
        if !config.fresh() {
            return Self::default();
        }
        let mut modules: BTreeMap<String, Config> = BTreeMap::new();
        for (path, facts) in config.iter() {
            let Some((module, _)) = config_module(path) else { continue };
            let group = modules.entry(module).or_default();
            group.unresolved |= facts.service.unresolved;
            group.names.extend(facts.service.names.iter().cloned());
            if let Some(base) = &facts.service.base {
                group.base = Some(base.clone());
            }
        }
        Self(
            modules
                .into_iter()
                .filter_map(|(module, config)| {
                    let name = config.base?;
                    (!config.unresolved && config.names.len() == 1).then_some((module, name))
                })
                .collect(),
        )
    }

    pub(super) fn allows(&self, client: &super::Endpoint, server_path: &str) -> bool {
        let Some(client_name) = &client.service else { return true };
        let Some(module) = source_module(server_path) else { return true };
        self.0.get(&module).is_none_or(|server_name| server_name == client_name)
    }
}
