//! Persisted setting compatibility is part of the native-page migration.
use super::*;
use openidea_plugin::mock::MockHost;
use serde_json::Value;
use std::collections::BTreeMap;
#[test]
fn native_pages_preserve_every_legacy_key_type_default_and_keep_runtime_values_out() {
    let legacy: Value = serde_json::from_str(include_str!("../tests/fixtures/settings.schema.json")).unwrap();
    let mut migrated = BTreeMap::new();
    for source in [
        include_str!("../settings-java.schema.json"),
        include_str!("../settings-configuration.schema.json"),
    ] {
        let page: Value = serde_json::from_str(source).unwrap();
        for (key, field) in page["properties"].as_object().unwrap() {
            assert!(
                migrated.insert(key.clone(), field.clone()).is_none(),
                "duplicate persisted key {key}"
            );
            assert!(!field["group"].as_str().unwrap().is_empty());
            assert!(!field["description"].as_str().unwrap().is_empty());
            assert_eq!(
                field["type"], "boolean",
                "no effective runtime values are static setting fields"
            );
        }
    }
    assert_eq!(
        migrated.len(),
        legacy["properties"].as_object().unwrap().len() + 1
    );
    let run = migrated.get("springBoot.nativeRunProfiles").expect("new opt-in native run setting");
    assert_eq!(run["type"], "boolean");
    assert_eq!(run["default"], false);
    for (key, field) in legacy["properties"].as_object().unwrap() {
        let migrated = migrated.get(key).expect("legacy key retained");
        assert_eq!(migrated["type"], field["type"], "{key}");
        assert_eq!(migrated["default"], field["default"], "{key}");
    }
}
#[test]
fn existing_boolean_overrides_still_drive_the_same_source_check_groups() {
    let host = MockHost::install();
    host.set_setting("springBoot.checkEventListeners", "false");
    host.set_setting("springBoot.checkConfigurationProperties", "false");
    let groups = checks::Groups::from_settings();
    assert!(!groups.event_listeners);
    assert!(!groups.configuration_properties);
    assert!(groups.test_constructors);
    assert!(groups.application_setup);
    host.set_setting("springBoot.checkEventListeners", "true");
    assert!(checks::Groups::from_settings().event_listeners);
}
#[test]
fn settings_metadata_action_requires_an_invoking_project_and_never_substitutes_runtime_values() {
    let host = MockHost::install().with_capabilities(&["project.read"]);
    host.set_project_root(None);
    let mut boot = BootTools::default();
    let error = boot
        .run_command("read-dependency-jar-metadata", &host.invocation(), None)
        .unwrap_err();
    assert!(error.to_string().contains("Open the Maven or Gradle project first"));
    assert!(host.requests().is_empty());
}
