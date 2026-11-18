//! One standard "allow this permission" message for denied capabilities.
use openidea_plugin::{Error, NotificationLevel, host};
use std::sync::atomic::{AtomicBool, Ordering};

pub fn message(feature: &str, capability: &str, plugin: &str) -> String {
    format!(
        "{feature} needs the {capability} permission. Allow it in Settings › Plugins › {plugin}."
    )
}

pub fn map(err: Error, feature: &str, capability: &str, plugin: &str) -> Error {
    match err {
        Error::Denied(_) => Error::Invalid(message(feature, capability, plugin)),
        other => other,
    }
}

/// Like `map`, but rewrites only a denial of `capability` (prefix match); other denials pass through.
pub fn map_capability(err: Error, feature: &str, capability: &str, plugin: &str) -> Error {
    match err {
        Error::Denied(denied) if denied.starts_with(capability) => map(Error::Denied(denied), feature, capability, plugin),
        other => other,
    }
}

pub fn notify_once(flag: &AtomicBool, feature: &str, capability: &str, plugin: &str) {
    if flag.swap(true, Ordering::Relaxed) {
        return;
    }
    host::notify(NotificationLevel::Info, plugin, &message(feature, capability, plugin));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_names_capability_and_settings_page() {
        assert_eq!(
            message("Vulnerability lookup", "net:api.osv.dev", "Dependency Audit"),
            "Vulnerability lookup needs the net:api.osv.dev permission. Allow it in Settings › Plugins › Dependency Audit."
        );
    }

    #[test]
    fn map_rewrites_only_denied() {
        assert!(matches!(map(Error::Denied("x".into()), "F", "clipboard", "P"), Error::Invalid(m) if m.contains("clipboard")));
        assert!(matches!(map(Error::Stale, "F", "clipboard", "P"), Error::Stale));
    }

    #[test]
    fn map_capability_rewrites_only_the_named_capability() {
        let other = map_capability(Error::Denied("storage".into()), "F", "editor.read", "P");
        assert!(matches!(other, Error::Denied(c) if c == "storage"));
        let named = map_capability(Error::Denied("editor.read".into()), "F", "editor.read", "P");
        assert!(matches!(named, Error::Invalid(m) if m.contains("editor.read permission")));
    }

    #[test]
    fn notify_once_notifies_a_single_time() {
        let host = openidea_plugin::mock::MockHost::install();
        let flag = AtomicBool::new(false);
        notify_once(&flag, "Live checks", "events", "Spring Data Tools");
        notify_once(&flag, "Live checks", "events", "Spring Data Tools");
        assert_eq!(host.notifications().len(), 1);
    }
}
