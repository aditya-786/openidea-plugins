//! Bounded, owner-checked plugin state. Project-free forms are a distinct scope;
//! they never establish an owner for an existing-project edit.
use openidea_plugin::{Error, host};
use serde_json::{Value, json};
use std::collections::BTreeMap;
pub const MAX_STATE: usize = 2 * 1024 * 1024;
pub const MAX_FIELDS: usize = 32 * 1024;
pub const MAX_SELECTOR: usize = 8 * 1024;
pub const MAX_SERVER: usize = 2048;

pub fn locator(owner: &str, server: &str) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in owner.bytes().chain([0]).chain(server.bytes()) {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
    }
    format!("initializr-v1:{hash:016x}")
}

#[derive(Clone)]
pub(crate) struct Baseline { pub(crate) state: Option<String>, pub(crate) selector: Option<String> }
pub(crate) fn capture(owner:&str,server:&str)->Result<Baseline,Error> {
    let state=host::storage::get(&locator(owner,server))?;let selector=host::storage::get(&locator(owner,"selector"))?;
    if state.as_ref().is_some_and(|s|s.len()>MAX_STATE) || selector.as_ref().is_some_and(|s|s.len()>MAX_SELECTOR) {return Err(Error::Stale);}
    if let Some(raw)=selector.as_deref() {
        let value=crate::strict_json::parse(raw,MAX_SELECTOR)?;
        if value["owner"]!=owner || value["server"].as_str().is_none_or(|s|s.len()>MAX_SERVER) {return Err(Error::Invalid("Saved server selection is unreadable or foreign; draft retained.".into()));}
        if value["server"].as_str()!=Some(server) {return Err(Error::Invalid("Saved server selection changed before form load; draft retained. Reload saved form to continue.".into()));}
    } else if server!=crate::START {return Err(Error::Invalid("Saved server selection is missing for this scope; draft retained.".into()));}
    Ok(Baseline {state,selector})
}
pub(crate) fn decode(owner:&str,server:&str,raw:Option<&str>)->Option<(BTreeMap<String,String>,Option<Value>)> {
    let raw=raw?;if raw.len()>MAX_STATE {return None;}
    let value=crate::strict_json::parse(raw,MAX_STATE).ok()?;
    if value["schema"]!=1 || value["owner"]!=owner || value["server"]!=server {return None;}
    let fields:BTreeMap<String,String>=serde_json::from_value(value["fields"].clone()).ok()?;
    if serde_json::to_vec(&fields).ok()?.len()>MAX_FIELDS {return None;}
    Some((fields,value.get("metadata").filter(|v|crate::compatibility::valid_metadata(v)).cloned()))
}
#[cfg(test)]
pub fn load(owner: &str, server: &str) -> Option<(BTreeMap<String, String>, Option<Value>)> {
    decode(owner,server,host::storage::get(&locator(owner,server)).ok().flatten().as_deref())
}
pub(crate) fn save_checked(owner:&str,server:&str,fields:&BTreeMap<String,String>,metadata:Option<&Value>,baseline:&Baseline)->Result<String,Error> {
    let current=capture(owner,server)?;
    if current.state!=baseline.state || current.selector!=baseline.selector {return Err(Error::Invalid("Saved form or server selection changed in another window; draft retained. Reload saved form to continue.".into()));}
    save(owner,server,fields,metadata)?;
    Ok(json!({"schema":1,"owner":owner,"server":server,"fields":fields,"metadata":metadata}).to_string())
}

pub fn save(owner: &str, server: &str, fields: &BTreeMap<String, String>, metadata: Option<&Value>) -> Result<(), Error> {
    if serde_json::to_vec(fields).map_err(|e| Error::Failed(e.to_string()))?.len() > MAX_FIELDS {
        return Err(Error::Invalid("Form state exceeds 32 KiB; previous saved state retained.".into()));
    }
    let key = locator(owner, server);
    if let Some(raw) = host::storage::get(&key)? {
        if raw.len() > MAX_STATE {
            return Err(Error::Invalid("Saved form exceeds the state limit; previous state retained.".into()));
        }
        let value: Value =
            crate::strict_json::parse(&raw, MAX_STATE).map_err(|_| Error::Invalid("Saved form is ambiguous or unreadable; previous state retained.".into()))?;
        if value["schema"] != 1 || value["owner"] != owner || value["server"] != server {
            return Err(Error::Invalid("Saved form owner differs; previous state retained.".into()));
        }
    }
    let raw = json!({"schema":1,"owner":owner,"server":server,"fields":fields,"metadata":metadata}).to_string();
    if raw.len() > MAX_STATE {
        return Err(Error::Invalid("Catalogue state exceeds 2 MiB; previous saved state retained.".into()));
    }
    crate::strict_json::parse(&raw, MAX_STATE)?;
    host::storage::set(&key, &raw)
}

pub fn selected_server(owner: &str) -> String {
    let raw = host::storage::get(&locator(owner, "selector")).ok().flatten();
    raw.filter(|s| s.len() <= MAX_SELECTOR)
        .and_then(|s| crate::strict_json::parse(&s, MAX_SELECTOR).ok())
        .filter(|v| v["owner"] == owner)
        .and_then(|v| v["server"].as_str().filter(|server| server.len() <= MAX_SERVER).map(str::to_owned))
        .unwrap_or_else(|| crate::START.into())
}

pub fn select_server(owner: &str, server: &str) -> Result<(), Error> {
    if server.len() > MAX_SERVER {
        return Err(Error::Invalid("Server field exceeds 2048 bytes; previous selection retained.".into()));
    }
    let key = locator(owner, "selector");
    if host::storage::get(&key)?
        .is_some_and(|raw| crate::strict_json::parse(&raw, MAX_SELECTOR).ok().is_none_or(|v| v["owner"] != owner))
    {
        return Err(Error::Invalid("Saved selector is unreadable or foreign; previous selection retained.".into()));
    }
    let raw = json!({"owner":owner,"server":server}).to_string();
    if raw.len() > MAX_SELECTOR {
        return Err(Error::Invalid("Server selection exceeds 8 KiB; previous selection retained.".into()));
    }
    host::storage::set(&key, &raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Launcher, START};
    use openidea_plugin::{
        mock::MockHost,
        views::{ViewEvent, Views},
    };
    fn field(l: &mut Launcher, host: &MockHost, key: &str, value: &str) {
        l.handle("initializr", ViewEvent::Field(key.into(), value.into()), &host.invocation()).unwrap();
    }
    fn snapshot() -> Value {
        json!({"bootVersion":{"default":"3.5.16","values":[{"id":"3.5.16"},{"id":"4.0.0"}]},"javaVersion":{"values":[{"id":"17"},{"id":"21","name":"Java 21"}]},"type":{"values":[{"id":"maven-project"}]},"dependencies":{"values":[{"name":"Test","values":[{"id":"old","compatibilityRange":"[3.0.0,4.0.0)"}]}]}})
    }
    #[test]
    fn stale_same_scope_draft_never_overwrites_newer_saved_fields() {
        let h=MockHost::install();let mut first=Launcher::default();let mut second=Launcher::default();
        first.bind(&h.invocation()).unwrap();second.bind(&h.invocation()).unwrap();assert_eq!(first.get("artifactId"),"demo");assert_eq!(second.get("artifactId"),"demo");
        field(&mut first,&h,"artifactId","first-saved");let saved=h.storage();
        field(&mut second,&h,"artifactId","second-draft");assert_eq!(h.storage(),saved,"stale draft must not overwrite first window");assert_eq!(second.get("artifactId"),"second-draft");assert!(second.catalog_status.as_deref().unwrap().contains("changed"));
    }
    #[test]
    fn stale_selector_and_confirmed_reload_preserve_draft_until_explicit_recovery() {
        let h=MockHost::install();let mut l=Launcher::default();l.bind(&h.invocation()).unwrap();l.get("artifactId");
        select_server("project:/project","http://localhost:8080").unwrap();let saved=h.storage();
        field(&mut l,&h,"artifactId","unsaved");assert_eq!(h.storage(),saved);assert_eq!(l.get("artifactId"),"unsaved");
        h.answer_next_prompt(None);assert!(l.handle("initializr",ViewEvent::Button("reload-saved".into()),&h.invocation()).is_err());assert_eq!(l.get("artifactId"),"unsaved");assert_eq!(h.storage(),saved);
        h.answer_next_prompt(Some(&["yes"]));l.handle("initializr",ViewEvent::Button("reload-saved".into()),&h.invocation()).unwrap();assert_eq!(l.get("server"),"http://localhost:8080");assert_eq!(l.get("artifactId"),"demo");
        field(&mut l,&h,"artifactId","recovered");assert_eq!(load("project:/project","http://localhost:8080").unwrap().0["artifactId"],"recovered");
    }
    #[test]
    fn stale_metadata_refresh_retains_new_saved_catalogue_and_prior_session_facts() {
        let h=MockHost::install().with_capabilities(&["project.read","storage","net:start.spring.io"]);let mut first=Launcher::default();let mut second=Launcher::default();first.bind(&h.invocation()).unwrap();second.bind(&h.invocation()).unwrap();first.get("artifactId");second.get("artifactId");
        let good=snapshot();let body=good.to_string();h.on_fetch(move |_|Ok(openidea_plugin::HttpResponse {status:200,body:body.as_bytes().to_vec(),..Default::default()}));
        first.load_metadata(true);let saved=h.storage();assert_eq!(first.metadata,Some(good));
        second.load_metadata(true);assert_eq!(h.storage(),saved);assert!(second.metadata.is_none());assert!(second.catalog_status.as_deref().unwrap().contains("changed"));
    }
    #[test]
    fn unreadable_first_load_needs_reload_and_denied_reload_keeps_draft() {
        let h=MockHost::install().with_capabilities(&["project.read"]);let mut l=Launcher::default();l.bind(&h.invocation()).unwrap();l.set("artifactId","draft");
        let h=h.with_capabilities(&["project.read","storage"]);l.set("artifactId","still-draft");assert!(h.storage().is_empty());assert_eq!(l.get("artifactId"),"still-draft");
        let h=h.with_capabilities(&["project.read"]);h.answer_next_prompt(Some(&["yes"]));assert!(l.reload_saved().is_err());assert_eq!(l.get("artifactId"),"still-draft");
        let h=h.with_capabilities(&["project.read","storage"]);h.answer_next_prompt(Some(&["yes"]));l.reload_saved().unwrap();l.set("artifactId","saved");assert_eq!(load("project:/project",START).unwrap().0["artifactId"],"saved");
    }
    #[test]
    fn selector_change_before_first_form_load_cannot_admit_an_obsolete_scope() {
        let h=MockHost::install();let mut l=Launcher::default();l.bind(&h.invocation()).unwrap();
        select_server("project:/project","http://localhost:8080").unwrap();let saved=h.storage();
        field(&mut l,&h,"artifactId","old-scope-draft");assert_eq!(h.storage(),saved,"cold load must not accept a selector from another server");assert_eq!(l.get("artifactId"),"old-scope-draft");
    }
    #[test]
    fn conflict_draft_survives_project_switch_and_returns_with_old_baseline() {
        let h=MockHost::install();let mut l=Launcher::default();l.bind(&h.invocation()).unwrap();l.get("artifactId");
        save("project:/project",START,&BTreeMap::from([("artifactId".into(),"other-saved".into())]),None).unwrap();
        field(&mut l,&h,"artifactId","unsaved-local");let stored=h.storage();
        h.set_project_root(Some("/other"));l.bind(&h.invocation()).unwrap();h.set_project_root(Some("/project"));l.bind(&h.invocation()).unwrap();
        assert_eq!(l.get("artifactId"),"unsaved-local","switching projects must not drop conflict draft");assert_eq!(h.storage(),stored);
    }
    #[test]
    fn bounded_conflict_draft_capacity_refuses_scope_change_without_discarding_current() {
        let h=MockHost::install();let mut l=Launcher::default();
        for (root,draft) in [("/a","draft-a"),("/b","draft-b"),("/c","draft-c")] {
            h.set_project_root(Some(root));l.bind(&h.invocation()).unwrap();l.get("artifactId");
            save(&format!("project:{root}"),START,&BTreeMap::from([("artifactId".into(),"other-saved".into())]),None).unwrap();field(&mut l,&h,"artifactId",draft);
        }
        let stored=h.storage();h.set_project_root(Some("/d"));assert!(l.bind(&h.invocation()).is_err());assert_eq!(l.owner,"project:/c");assert_eq!(l.get("artifactId"),"draft-c");assert_eq!(h.storage(),stored);
        h.set_project_root(Some("/c"));l.bind(&h.invocation()).unwrap();h.answer_next_prompt(Some(&["yes"]));l.reload_saved().unwrap();h.set_project_root(Some("/d"));l.bind(&h.invocation()).unwrap();
        h.set_project_root(Some("/a"));l.bind(&h.invocation()).unwrap();assert_eq!(l.get("artifactId"),"draft-a");assert!(!l.save(),"restored original stale baseline must remain guarded");assert_eq!(h.storage(),stored);
    }
    #[test]
    fn project_server_and_project_free_forms_are_separate_and_restore() {
        let host = MockHost::install();
        let mut l = Launcher::default();
        field(&mut l, &host, "artifactId", "one");
        field(&mut l, &host, "server", "http://localhost:8080/");
        assert_eq!(l.get("artifactId"), "demo");
        field(&mut l, &host, "artifactId", "custom");
        host.set_project_root(Some("/other"));
        l.bind(&host.invocation()).unwrap();
        assert_eq!(l.get("artifactId"), "demo");
        field(&mut l, &host, "artifactId", "two");
        host.set_project_root(None);
        l.bind(&host.invocation()).unwrap();
        assert_eq!(l.get("artifactId"), "demo");
        field(&mut l, &host, "artifactId", "free");
        host.set_project_root(Some("/project"));
        let mut fresh = Launcher::default();
        fresh.bind(&host.invocation()).unwrap();
        assert_eq!(fresh.get("server"), "http://localhost:8080");
        assert_eq!(fresh.get("artifactId"), "custom");
        field(&mut fresh, &host, "server", START);
        assert_eq!(fresh.get("artifactId"), "one");
    }
    #[test]
    fn oversized_save_and_wrong_owner_preserve_previous_actual_state() {
        let _host = MockHost::install();
        let fields = BTreeMap::from([("artifactId".into(), "retained".into())]);
        save("project:/one", START, &fields, Some(&snapshot())).unwrap();
        let before = host::storage::get(&locator("project:/one", START)).unwrap().unwrap();
        let huge = json!({"bootVersion":{},"padding":"x".repeat(MAX_STATE)});
        assert!(save("project:/one", START, &fields, Some(&huge)).is_err());
        assert_eq!(host::storage::get(&locator("project:/one", START)).unwrap().unwrap(), before);
        assert_eq!(load("project:/one", START).unwrap().0["artifactId"], "retained");
        host::storage::set(&locator("project:/other", START), &before).unwrap();
        assert!(load("project:/other", START).is_none());
        assert!(save("project:/other", START, &fields, None).is_err());
    }
    #[test]
    fn cached_catalogue_restores_offline_and_failed_refresh_keeps_last_good() {
        let host = MockHost::install().with_capabilities(&["storage", "project.read", "net:start.spring.io"]);
        let metadata = snapshot();
        host.on_fetch(move |_| {
            Ok(openidea_plugin::HttpResponse { status: 200, body: metadata.to_string().into_bytes(), ..Default::default() })
        });
        let mut l = Launcher::default();
        l.bind(&host.invocation()).unwrap();
        l.load_metadata(true);
        assert!(l.metadata.is_some());
        let before = host.storage()[&locator("project:/project", START)].clone();
        host.on_fetch(|_| Err(Error::Failed("offline".into())));
        let mut fresh = Launcher::default();
        fresh.bind(&host.invocation()).unwrap();
        fresh.load_metadata(false);
        assert!(fresh.metadata.is_some());
        assert_eq!(host.requests().len(), 1);
        fresh.load_metadata(true);
        assert!(fresh.metadata.is_some());
        assert!(fresh.catalog_status.as_deref().unwrap().contains("previous saved catalogue"));
        assert_eq!(host.storage()[&locator("project:/project", START)], before);
    }
    #[test]
    fn server_choices_and_ranges_gate_generation_without_guessing_jdk_rules() {
        let host = MockHost::install();
        let mut l = Launcher::default();
        l.bind(&host.invocation()).unwrap();
        let _ = l.changed();
        l.metadata = Some(snapshot());
        field(&mut l, &host, "bootVersion", "4.0.0");
        field(&mut l, &host, "dependencies", "old");
        assert!(l.problem().unwrap().contains("Dependency `old`"));
        l.handle("initializr", ViewEvent::Button("generate".into()), &host.invocation()).unwrap();
        assert!(host.opened_urls().is_empty());
        field(&mut l, &host, "dependencies", "");
        assert!(l.problem().is_none(), "metadata without Java range does not prove Boot4 requires Java 21");
        field(&mut l, &host, "javaVersion", "Java 21");
        assert_eq!(l.get("javaVersion"), "21");
        field(&mut l, &host, "type", "gradle-project");
        assert!(l.problem().unwrap().contains("not offered"));
        field(&mut l, &host, "type", "maven-project");
        field(&mut l, &host, "javaVersion", "25");
        assert!(l.problem().unwrap().contains("not offered"));
    }
    #[test]
    fn explicit_option_ranges_are_enforced_and_unknown_ranges_stay_unknown() {
        let host = MockHost::install();
        let mut l = Launcher::default();
        l.bind(&host.invocation()).unwrap();
        let _ = l.changed();
        let mut metadata = snapshot();
        metadata["javaVersion"]["values"][0]["compatibilityRange"] = json!("[3.0.0,4.0.0)");
        l.metadata = Some(metadata);
        field(&mut l, &host, "bootVersion", "4.0.0");
        assert!(l.problem().unwrap().contains("declared range"));
        l.metadata.as_mut().unwrap()["javaVersion"]["values"][0]["compatibilityRange"] = json!("vendor-specific");
        assert!(l.problem().is_none());
    }
    #[test]
    fn project_free_cli_does_not_start_an_anonymous_mutation() {
        let host = MockHost::install().with_capabilities(&["storage", "process:spring"]);
        host.set_project_root(None);
        let mut l = Launcher::default();
        l.handle("initializr", ViewEvent::Button("cli".into()), &host.invocation()).unwrap();
        assert!(host.terminals().is_empty());
        assert_eq!(host.opened_urls().len(), 1);
    }
}

#[cfg(test)]
mod additional_tests {
    use super::*;
    use crate::Launcher;
    use openidea_plugin::mock::MockHost;
    #[test]
    fn duplicate_metadata_refresh_does_not_replace_saved_snapshot() {
        let host = MockHost::install().with_capabilities(&["storage", "project.read", "net:start.spring.io"]);
        let good = json!({"bootVersion":{"default":"3.5.16","values":[{"id":"3.5.16"}]}});
        let body = good.to_string();
        host.on_fetch(move |_| Ok(openidea_plugin::HttpResponse { status: 200, body: body.as_bytes().to_vec(), ..Default::default() }));
        let mut l = Launcher::default();
        l.bind(&host.invocation()).unwrap();
        l.load_metadata(true);
        let before = host.storage()[&locator("project:/project", crate::START)].clone();
        host.on_fetch(|_| {
            Ok(openidea_plugin::HttpResponse {
                status: 200,
                body: br#"{"bootVersion":{"values":[{"id":"x"},{"id":"x"}]}}"#.to_vec(),
                ..Default::default()
            })
        });
        l.load_metadata(true);
        assert_eq!(l.metadata.as_ref(), Some(&good));
        assert_eq!(host.storage()[&locator("project:/project", crate::START)], before);
        assert!(l.catalog_status.as_deref().unwrap().contains("previous saved catalogue"));
    }
    #[test]
    fn legacy_global_state_is_not_adopted_as_a_project_owner() {
        let host = MockHost::install();
        host::storage::set("form", r#"{"artifactId":"unknown-owner"}"#).unwrap();
        let mut l = Launcher::default();
        l.bind(&host.invocation()).unwrap();
        assert_eq!(l.get("artifactId"), "demo");
        assert!(host.storage().contains_key("form"));
        assert!(load("project:/other", crate::START).is_none());
    }
    #[test]
    fn duplicate_decoded_server_json_does_not_replace_catalogue() {
        let host = MockHost::install().with_capabilities(&["storage", "project.read", "net:start.spring.io"]);
        let good = json!({"bootVersion":{"default":"3.5.16","values":[{"id":"3.5.16"}]},"dependencies":{"values":[]}});
        let body = good.to_string();
        host.on_fetch(move |_| Ok(openidea_plugin::HttpResponse { status: 200, body: body.as_bytes().to_vec(), ..Default::default() }));
        let mut l = Launcher::default();
        l.bind(&host.invocation()).unwrap();
        l.load_metadata(true);
        let key = locator("project:/project", crate::START);
        let saved = host.storage()[&key].clone();
        host.on_fetch(|_| Ok(openidea_plugin::HttpResponse { status: 200,
            body: br#"{"bootVersion":{"default":"3.5.16","values":[{"id":"3.5.16"}]},"boot\u0056ersion":{"default":"4.0.0","values":[{"id":"4.0.0"}]}}"#.to_vec(), ..Default::default() }));
        l.load_metadata(true);
        assert_eq!(l.metadata.as_ref(), Some(&good));
        assert_eq!(host.storage()[&key], saved);
        assert!(l.catalog_status.as_deref().unwrap().contains("previous saved catalogue"));
    }
    #[test]
    fn wrong_typed_catalogue_containers_preserve_last_good_facts() {
        let host = MockHost::install().with_capabilities(&["storage", "project.read", "net:start.spring.io"]);
        let good = json!({"bootVersion":{"default":"3.5.16","values":[{"id":"3.5.16"}]},"dependencies":{"values":[]}});
        let body = good.to_string();
        host.on_fetch(move |_| Ok(openidea_plugin::HttpResponse { status: 200, body: body.as_bytes().to_vec(), ..Default::default() }));
        let mut l = Launcher::default();
        l.bind(&host.invocation()).unwrap();
        l.load_metadata(true);
        let key = locator("project:/project", crate::START);
        let saved = host.storage()[&key].clone();
        for bad in [
            json!({"bootVersion":{"values":"not an option array"}}),
            json!({"bootVersion":false,"dependencies":{"values":[]}}),
            json!({"bootVersion":{},"dependencies":{"values":[true]}}),
            json!({"bootVersion":{},"dependencies":{"values":[{"values":"not a dependency array"}]}}),
            json!({"bootVersion":{"default":false,"values":[]}}),
            json!({"bootVersion":{"values":[{"id":"3.5.16","name":false}]}}),
            json!({"bootVersion":{},"type":{"values":[{"id":"maven-project","tags":true}]}}),
            json!({"bootVersion":{},"type":{"values":[{"id":"maven-project","tags":{"build":false}}]}}),
            json!({"bootVersion":{},"type":{"values":[{"id":"maven-project","tags":{"format":"x".repeat(257)}}]}}),
        ] {
            let body = bad.to_string();
            host.on_fetch(move |_| Ok(openidea_plugin::HttpResponse { status: 200, body: body.as_bytes().to_vec(), ..Default::default() }));
            l.load_metadata(true);
            assert_eq!(l.metadata.as_ref(), Some(&good), "{bad}");
            assert_eq!(host.storage()[&key], saved);
        }
    }
    #[test]
    fn ambiguous_saved_envelopes_and_selectors_are_not_adopted_or_overwritten() {
        let host = MockHost::install();
        let owner = "project:/project";
        let key = locator(owner, crate::START);
        let raw = format!(r#"{{"schema":1,"owner":"foreign","owner":"{owner}","server":"{}","fields":{{"artifactId":"retained"}},"metadata":null}}"#, crate::START);
        host::storage::set(&key, &raw).unwrap();
        assert!(load(owner, crate::START).is_none());
        assert!(save(owner, crate::START, &BTreeMap::new(), None).is_err());
        assert_eq!(host.storage()[&key], raw);
        let selector = locator(owner, "selector");
        let raw = format!(r#"{{"owner":"foreign","owner":"{owner}","server":"http://localhost:8080"}}"#);
        host::storage::set(&selector, &raw).unwrap();
        assert_eq!(selected_server(owner), crate::START);
        assert!(select_server(owner, crate::START).is_err());
        assert_eq!(host.storage()[&selector], raw);
    }
}

#[cfg(test)]
mod default_version_tests {
    use super::*;
    use crate::Launcher;
    use openidea_plugin::mock::MockHost;
    #[test]
    fn saved_default_is_pinned_in_requests_and_cli_to_match_validation() {
        let host = MockHost::install();
        let mut l = Launcher::default();
        l.bind(&host.invocation()).unwrap();
        let _ = l.changed();
        l.metadata = Some(json!({"bootVersion":{"default":"3.5.16","values":[{"id":"3.5.16"}]}}));
        assert_eq!(l.get("bootVersion"), "");
        assert!(l.starter_url().contains("bootVersion=3.5.16"));
        assert!(l.cli_args().unwrap().iter().any(|arg| arg == "--boot-version=3.5.16"));
    }
    #[test]
    fn valid_long_owner_and_server_selector_round_trip() {
        let host = MockHost::install();
        let root = format!("/project/{}", "nested/".repeat(430)).trim_end_matches('/').to_owned();
        let owner = format!("project:{root}");
        let server = format!("https://example.test/{}", "s".repeat(MAX_SERVER - "https://example.test/".len()));
        assert!(root.len() < 4096 && server.len() == MAX_SERVER);
        host.set_project_root(Some(&root));
        select_server(&owner, &server).unwrap();
        assert_eq!(selected_server(&owner), server);
        let saved = host.storage()[&locator(&owner, "selector")].clone();
        assert!(select_server(&owner, &"x".repeat(MAX_SELECTOR)).is_err());
        assert_eq!(host.storage()[&locator(&owner, "selector")], saved);
        let mut fresh = Launcher::default();
        fresh.bind(&host.invocation()).unwrap();
        assert_eq!(fresh.get("server"), server);
    }
    #[test]
    fn failed_form_save_keeps_server_and_unsaved_draft_until_recovery() {
        let host = MockHost::install();
        let mut l = Launcher::default();
        l.bind(&host.invocation()).unwrap();
        select_server("project:/project", crate::START).unwrap();
        l.set("artifactId", "saved");
        let saved = host.storage();
        let host = host.with_capabilities(&["project.read"]);
        l.set("artifactId", "unsaved");
        l.set("server", "http://localhost:8080");
        assert_eq!(l.get("server"), crate::START);
        assert_eq!(l.get("artifactId"), "unsaved");
        assert!(l.catalog_status.as_deref().unwrap().contains("Server change refused"));
        assert_eq!(host.storage(), saved);
        let host = host.with_capabilities(&["storage", "project.read"]);
        l.set("server", "http://localhost:8080");
        assert_eq!(l.get("server"), "http://localhost:8080");
        l.set("server", crate::START);
        assert_eq!(l.get("artifactId"), "unsaved");
        assert!(host.requests().is_empty());
    }
    #[test]
    fn failed_selector_write_keeps_form_owner_and_visible_state() {
        let host = MockHost::install();
        let mut l = Launcher::default();
        l.bind(&host.invocation()).unwrap();
        l.set("artifactId", "retained");
        let key = locator("project:/project", "selector");
        let foreign = r#"{"owner":"foreign","server":"https://start.spring.io"}"#;
        host::storage::set(&key, foreign).unwrap();
        l.set("server", "http://localhost:8080");
        assert_eq!(l.get("server"), crate::START);
        assert_eq!(l.get("artifactId"), "retained");
        assert_eq!(host.storage()[&key], foreign);
        assert!(l.catalog_status.as_deref().unwrap().contains("Server change refused"));
        assert!(host.requests().is_empty());
    }
}
