//! Project-scoped configuration discovery/choice. Commands may ask; editor
//! providers never enumerate configurations or display a picker.
use openidea_plugin::host::{self, storage};
use openidea_plugin::{Error, NotificationLevel, PickItem, Project};

const DEFAULTS: [&str; 2] = [".devcontainer/devcontainer.json", ".devcontainer.json"];
const MAX_CONFIGS: usize = 256;

fn key(root: &str) -> String {
    format!("devcontainer.config-choice:{}", serde_json::to_string(root).unwrap_or_default())
}

pub(crate) fn relative(root: &str, path: &str) -> Result<String, Error> {
    let path = path.trim().replace('\\', "/");
    let root = root.replace('\\', "/");
    let prefix = format!("{}/", root.trim_end_matches('/'));
    let path = path.strip_prefix(&prefix).unwrap_or(&path);
    if path.is_empty() || path.starts_with('/') || path.as_bytes().get(1) == Some(&b':') || path.split('/').any(|p| p == "..") {
        return Err(Error::Invalid("The configuration must be a file inside the current project.".into()));
    }
    let parts: Vec<_> = path.split('/').filter(|p| !p.is_empty() && *p != ".").collect();
    if parts.is_empty() {
        return Err(Error::Invalid("The configuration path is empty.".into()));
    }
    Ok(parts.join("/"))
}

fn existing(project: &Project, path: &str) -> Result<bool, Error> {
    if !owned(project, path)? {
        return Ok(false);
    }
    let path = format!("{}/{}", project.root()?.trim_end_matches('/'), path);
    match project.read_file(&path) {
        Ok(_) => Ok(true),
        Err(Error::NotFound(_)) => Ok(false),
        Err(error) => Err(error),
    }
}

/// Lexical confinement under a Project is not proof of its current ownership:
/// another open project can own a nested configuration directory.
fn owned(project: &Project, path: &str) -> Result<bool, Error> {
    let root = project.root()?;
    let relative = relative(&root, path)?;
    let absolute = format!("{}/{}", root.trim_end_matches(['/', '\\']), relative);
    Ok(host::project_of(&absolute).and_then(|owner| owner.root().ok()).as_deref() == Some(root.as_str()))
}

pub(crate) fn ensure_owned(project: &Project, path: &str) -> Result<(), Error> {
    if !owned(project, path)? {
        return Err(Error::Invalid("This configuration belongs to another open project nested in this one. Choose it from that project's window.".into()));
    }
    Ok(())
}

pub(crate) fn discover(project: &Project) -> Result<Vec<String>, Error> {
    let root = project.root()?;
    let mut found = Vec::new();
    // Explicit probes preserve support for ignored default configs. API 1.3
    // find_files visits hidden directories; normal ignore rules still apply.
    for path in DEFAULTS {
        if existing(project, path)? {
            found.push(path.to_owned());
        }
    }
    let nested = project.find_files(".devcontainer/*/devcontainer.json", (MAX_CONFIGS + 1) as u32)?;
    if nested.len() > MAX_CONFIGS {
        return Err(Error::Invalid(
            "More than 256 Dev Container configurations were found. Set an explicit Configuration file in plugin settings.".into(),
        ));
    }
    for path in nested {
        let path = relative(&root, &path)?;
        if path.starts_with(".devcontainer/") && path.split('/').count() == 3 && existing(project, &path)? {
            found.push(path);
        }
    }
    found.sort_by(|a, b| {
        let rank = |s: &str| DEFAULTS.iter().position(|d| *d == s).unwrap_or(DEFAULTS.len());
        rank(a).cmp(&rank(b)).then_with(|| a.cmp(b))
    });
    found.dedup();
    if found.len() > MAX_CONFIGS {
        return Err(Error::Invalid(
            "More than 256 Dev Container configurations were found. Set an explicit Configuration file in plugin settings.".into(),
        ));
    }
    Ok(found)
}

fn remember(root: &str, path: &str) {
    if let Err(error) = storage::set(&key(root), path) {
        if !matches!(error, Error::Denied(_)) {
            host::notify(
                NotificationLevel::Warning,
                "Dev Container configuration",
                &format!("Using {path}, but could not remember the choice: {error}"),
            );
        }
    }
}

/// None is cancellation, never permission to silently choose the first config.
pub(crate) fn choose(project: &Project, force_pick: bool) -> Result<Option<String>, Error> {
    let root = project.root()?;
    let configured = host::setting_string("configFile").unwrap_or_default();
    if !configured.trim().is_empty() {
        if force_pick {
            return Err(Error::Invalid(
                "Configuration file is overridden in plugin settings. Clear that setting to choose and remember a project configuration."
                    .into(),
            ));
        }
        let path = relative(&root, &configured)?;
        return if existing(project, &path)? {
            Ok(Some(path))
        } else {
            Err(Error::NotFound(format!("Configured Dev Container file {path} was not found in this project.")))
        };
    }
    if !force_pick {
        if let Ok(Some(saved)) = storage::get(&key(&root)) {
            if let Ok(path) = relative(&root, &saved) {
                if existing(project, &path)? {
                    return Ok(Some(path));
                }
            }
            let _ = storage::delete(&key(&root));
        }
    }
    let found = discover(project)?;
    if found.is_empty() {
        return Err("No .devcontainer/devcontainer.json, .devcontainer.json or .devcontainer/<name>/devcontainer.json found in this project. Create a configuration or set Configuration file in plugin settings.".into());
    }
    let chosen = if found.len() == 1 && !force_pick {
        Some(found[0].clone())
    } else {
        let items: Vec<_> = found.iter().map(|p| PickItem::new(p, p)).collect();
        host::pick("Dev Container configuration", &items)?
    };
    let Some(path) = chosen else {
        return Ok(None);
    };
    if !found.contains(&path) || !existing(project, &path)? {
        return Err(Error::Stale);
    }
    remember(&root, &path);
    Ok(Some(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    fn project(host: &MockHost) -> Project {
        host.invocation().project().unwrap()
    }

    #[test]
    fn hidden_nested_configs_are_discovered_without_unrelated_or_deep_files() {
        let host = MockHost::install();
        for name in [
            ".devcontainer/devcontainer.json",
            ".devcontainer.json",
            ".devcontainer/z/devcontainer.json",
            ".devcontainer/a/devcontainer.json",
            "other/devcontainer.json",
            ".devcontainer/a/deep/devcontainer.json",
        ] {
            host.add_file(name, "{}");
        }
        assert_eq!(
            discover(&project(&host)).unwrap(),
            [
                ".devcontainer/devcontainer.json",
                ".devcontainer.json",
                ".devcontainer/a/devcontainer.json",
                ".devcontainer/z/devcontainer.json"
            ]
        );
    }

    #[test]
    fn remembered_choice_survives_new_instances_and_does_not_cross_projects() {
        let host = MockHost::install();
        host.add_file("/project/.devcontainer/a/devcontainer.json", "{}");
        host.add_file("/project/.devcontainer/b/devcontainer.json", "{}");
        host.add_file("/second/.devcontainer/c/devcontainer.json", "{}");
        host.answer_next_prompt(Some(&[".devcontainer/b/devcontainer.json"]));
        assert_eq!(choose(&project(&host), false).unwrap().as_deref(), Some(".devcontainer/b/devcontainer.json"));
        assert_eq!(choose(&project(&host), false).unwrap().as_deref(), Some(".devcontainer/b/devcontainer.json"));
        host.set_project_root(Some("/second"));
        assert_eq!(choose(&project(&host), false).unwrap().as_deref(), Some(".devcontainer/c/devcontainer.json"));
        host.set_project_root(Some("/project"));
        assert_eq!(choose(&project(&host), false).unwrap().as_deref(), Some(".devcontainer/b/devcontainer.json"));
    }

    #[test]
    fn stale_or_escaping_persisted_choices_are_ignored_and_replaced() {
        let host = MockHost::install();
        host.add_file(".devcontainer/devcontainer.json", "{}");
        for saved in [".devcontainer/gone/devcontainer.json", "../other/devcontainer.json", "/other/devcontainer.json"] {
            storage::set(&key("/project"), saved).unwrap();
            assert_eq!(choose(&project(&host), false).unwrap().as_deref(), Some(".devcontainer/devcontainer.json"));
            assert_eq!(storage::get(&key("/project")).unwrap().as_deref(), Some(".devcontainer/devcontainer.json"));
        }
    }

    #[test]
    fn cancel_preserves_previous_selection_and_starts_nothing() {
        let host = MockHost::install();
        host.add_file(".devcontainer/a/devcontainer.json", "{}");
        host.add_file(".devcontainer/b/devcontainer.json", "{}");
        storage::set(&key("/project"), ".devcontainer/b/devcontainer.json").unwrap();
        host.answer_next_prompt(None);
        assert_eq!(choose(&project(&host), true).unwrap(), None);
        assert_eq!(storage::get(&key("/project")).unwrap().as_deref(), Some(".devcontainer/b/devcontainer.json"));
        assert!(host.process_runs().is_empty() && host.terminals().is_empty());
    }

    #[test]
    fn configured_override_is_validated_and_missing_does_not_fall_back() {
        let host = MockHost::install();
        host.add_file(".devcontainer/devcontainer.json", "{}");
        for path in ["../other/devcontainer.json", "/outside/devcontainer.json", ".devcontainer/gone.json"] {
            host.set_setting("configFile", &serde_json::to_string(path).unwrap());
            assert!(choose(&project(&host), false).is_err());
        }
        host.set_setting("configFile", "\"/project/.devcontainer/devcontainer.json\"");
        assert_eq!(choose(&project(&host), false).unwrap().as_deref(), Some(".devcontainer/devcontainer.json"));
        assert!(choose(&project(&host), true).is_err());
    }

    #[test]
    fn optional_storage_denial_keeps_selection_usable_but_read_denial_fails() {
        let host = MockHost::install().with_capabilities(&["project.read"]);
        host.add_file(".devcontainer/a/devcontainer.json", "{}");
        host.add_file(".devcontainer/b/devcontainer.json", "{}");
        host.answer_next_prompt(Some(&[".devcontainer/b/devcontainer.json"]));
        assert_eq!(choose(&project(&host), false).unwrap().as_deref(), Some(".devcontainer/b/devcontainer.json"));
        let host = host.with_capabilities(&[]);
        assert!(matches!(choose(&project(&host), false), Err(Error::Denied(_))));
    }
    #[test]
    fn discovery_cap_requires_an_explicit_choice_instead_of_silently_truncating() {
        let host = MockHost::install();
        for n in 0..257 {
            host.add_file(&format!(".devcontainer/c{n}/devcontainer.json"), "{}");
        }
        assert!(discover(&project(&host)).unwrap_err().to_string().contains("256"));
        assert!(host.terminals().is_empty());
    }

    #[test]
    fn discovery_and_saved_or_explicit_choices_exclude_nested_project_configurations() {
        let host = MockHost::install();
        host.add_file("/project/.devcontainer/devcontainer.json", "{}");
        host.add_file("/project/.devcontainer/team/devcontainer.json", "{}");
        storage::set(&key("/project"), ".devcontainer/team/devcontainer.json").unwrap();
        host.add_project("/project/.devcontainer/team");
        assert_eq!(discover(&project(&host)).unwrap(), [".devcontainer/devcontainer.json"]);
        assert_eq!(choose(&project(&host), false).unwrap().as_deref(), Some(".devcontainer/devcontainer.json"));
        host.set_setting("configFile", "\".devcontainer/team/devcontainer.json\"");
        assert!(choose(&project(&host), false).is_err());
        assert!(host.terminals().is_empty() && host.process_runs().is_empty());
    }
}
