//! Full, owner-bound snapshots. Reader/writer share one pre-write limit.
use crate::{DEFAULT_URL, Snapshot, check_url};
use openidea_plugin::{Error, Project, host};
use serde::{Deserialize, Serialize};

pub(crate) const MAX_BYTES: usize = 8 << 20;
const SCHEMA: u8 = 5;
/// Schema 1 lacks `health_details` and schema 2 stores configprops values unmasked and no handler descriptors, schemas before 4 store env values unmasked, and schema 4 masked fewer secret names: all migrate by reading (defaults, then masking) and are rewritten masked.
const OLDEST_READABLE_SCHEMA: u8 = 1;
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct Scope {
    pub root: String,
    pub url: String,
    pub auth: String,
}
impl Scope {
    pub fn current(project: &Project) -> Result<Self, Error> {
        let root = project.root()?;
        if !root.starts_with('/')
            || root.len() > 4096
            || root.chars().any(char::is_control)
            || root.split('/').any(|p| matches!(p, "." | ".."))
        {
            return Err(Error::Invalid("A verified project identity is required.".into()));
        }
        let url = check_url(&host::setting_string("actuatorUrl").unwrap_or_else(|| DEFAULT_URL.into()))?;
        let auth = host::setting_string("actuatorAuth").unwrap_or_else(|| "none".into());
        if !matches!(auth.as_str(), "none" | "bearer" | "basic") {
            return Err("Choose a supported Actuator authentication mode.".into());
        }
        Ok(Self { root, url, auth })
    }
    pub fn key(&self) -> String {
        let identity = serde_json::to_string(self).expect("scope");
        let hash = identity.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100000001b3));
        format!("runtime-v1:{hash:016x}")
    }
}
#[derive(Serialize, Deserialize)]
struct Envelope {
    schema: u8,
    scope: Scope,
    snapshot: Snapshot,
}
/// The snapshot and whether it came from an older schema (so its stored bytes need rewriting).
fn decode(value: &str, scope: &Scope) -> Result<(Snapshot, bool), Error> {
    if value.len() > MAX_BYTES {
        return Err("The saved runtime snapshot exceeds the 8 MiB read limit; use Clear to discard it.".into());
    }
    let stored: Envelope = serde_json::from_str(value)
        .map_err(|_| Error::Invalid("The saved runtime snapshot is unreadable; use Clear to discard it.".into()))?;
    if !(OLDEST_READABLE_SCHEMA..=SCHEMA).contains(&stored.schema) || stored.scope != *scope || stored.snapshot.url != scope.url {
        return Err("The saved runtime snapshot belongs to a different scope or schema; it was not reused.".into());
    }
    let mut snapshot = stored.snapshot;
    if stored.schema < 4 {
        for source in &mut snapshot.sources {
            crate::snapshot::mask_props(&mut source.props);
        }
    }
    if stored.schema < 3 {
        for bean in &mut snapshot.configprops {
            crate::snapshot::mask_props(&mut bean.props);
        }
    }
    if stored.schema < 5 {
        for props in snapshot.sources.iter_mut().map(|s| &mut s.props).chain(snapshot.configprops.iter_mut().map(|b| &mut b.props)) {
            crate::snapshot::mask_props(props);
        }
    }
    Ok((snapshot, stored.schema < SCHEMA))
}
pub(crate) fn read(scope: &Scope) -> Result<Option<Snapshot>, Error> {
    let Some(value) = host::storage::get(&scope.key())? else {
        return Ok(None);
    };
    let (snapshot, migrated) = decode(&value, scope)?;
    if migrated {
        // Older schemas kept secrets unmasked on disk; a failed rewrite still returns the masked copy.
        let _ = write(scope, &snapshot);
    }
    Ok(Some(snapshot))
}
pub(crate) fn write(scope: &Scope, snapshot: &Snapshot) -> Result<(), Error> {
    if snapshot.url != scope.url {
        return Err(Error::Stale);
    }
    let json = serde_json::to_string(&Envelope { schema: SCHEMA, scope: scope.clone(), snapshot: snapshot.clone() })
        .map_err(|e| Error::Failed(e.to_string()))?;
    if json.len() > MAX_BYTES {
        return Err("New runtime data exceeds the 8 MiB snapshot limit; previous snapshot retained.".into());
    }
    // A hash is only a storage locator, never proof of ownership.
    if let Some(existing) = host::storage::get(&scope.key())? {
        decode(&existing, scope)?;
    }
    host::storage::set(&scope.key(), &json)
}
#[derive(Serialize, Deserialize)]
struct Failure {
    scope: Scope,
    at: u64,
    message: String,
}
fn failure_key(scope: &Scope) -> String {
    format!("{}:failure", scope.key())
}
const MAX_RECENT: usize = 10;
fn recent_key(scope: &Scope) -> String {
    format!("{}:recent-requests", scope.key())
}
/// Most-recent-first "METHOD /path" choices; never URLs, headers, bodies or responses.
pub(crate) fn recent_requests(scope: &Scope) -> Vec<(String, String)> {
    let stored = host::storage::get(&recent_key(scope)).ok().flatten().unwrap_or_default();
    let entries: Vec<(String, String)> = serde_json::from_str(&stored).unwrap_or_default();
    entries.into_iter().filter(|(m, p)| m.len() <= 16 && p.len() <= 512).take(MAX_RECENT).collect()
}
pub(crate) fn remember_request(scope: &Scope, method: &str, path: &str) {
    if method.len() > 16 || path.len() > 512 {
        return;
    }
    let mut entries = recent_requests(scope);
    entries.retain(|(m, p)| (m.as_str(), p.as_str()) != (method, path));
    entries.insert(0, (method.to_owned(), path.to_owned()));
    entries.truncate(MAX_RECENT);
    if let Ok(json) = serde_json::to_string(&entries) {
        let _ = host::storage::set(&recent_key(scope), &json);
    }
}
pub(crate) fn failure(scope: &Scope) -> Option<String> {
    let value = host::storage::get(&failure_key(scope)).ok()??;
    if value.len() > 4096 {
        return None;
    }
    let record: Failure = serde_json::from_str(&value).ok()?;
    (record.scope == *scope).then(|| format!("{} (Unix {})", record.message, record.at))
}
pub(crate) fn failed(scope: &Scope, message: &str) {
    let end = message.char_indices().map(|(i, _)| i).take_while(|i| *i <= 1600).last().unwrap_or(0);
    let message = if message.len() > 1600 { format!("{}…", &message[..end]) } else { message.to_owned() };
    let record = Failure { scope: scope.clone(), at: crate::now(), message };
    if let Ok(Some(existing)) = host::storage::get(&failure_key(scope)) {
        if existing.len() > 4096 || serde_json::from_str::<Failure>(&existing).is_ok_and(|r| r.scope != *scope) {
            return;
        }
    }
    if let Ok(json) = serde_json::to_string(&record) {
        if json.len() <= 4096 {
            let _ = host::storage::set(&failure_key(scope), &json);
        }
    }
}
pub(crate) fn success(scope: &Scope) {
    if let Ok(Some(value)) = host::storage::get(&failure_key(scope)) {
        if value.len() > 4096 || serde_json::from_str::<Failure>(&value).is_ok_and(|r| r.scope != *scope) {
            return;
        }
    }
    let _ = host::storage::delete(&failure_key(scope));
}
pub(crate) fn clear(scope: &Scope) -> Result<(), Error> {
    if let Some(value) = host::storage::get(&scope.key())? {
        // An unreadable record can be explicitly discarded; never erase a known
        // different owner merely because its hash happened to collide.
        if value.len() <= MAX_BYTES {
            if let Ok(record) = serde_json::from_str::<Envelope>(&value) {
                if record.scope != *scope {
                    return Err(Error::Stale);
                }
            }
        }
    }
    host::storage::delete(&scope.key())?;
    success(scope);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Bean, Logger, Source};
    use openidea_plugin::mock::MockHost;
    fn scope() -> Scope {
        Scope { root: "/project".into(), url: DEFAULT_URL.into(), auth: "none".into() }
    }
    fn snapshot() -> Snapshot {
        Snapshot {
            url: DEFAULT_URL.into(),
            fetched_at: 42,
            beans: vec![Bean { name: "demo".into(), ty: "com.acme.Demo".into(), ..Bean::default() }],
            loggers: vec![Logger { name: "ROOT".into(), configured: "INFO".into(), effective: "INFO".into() }],
            ..Snapshot::default()
        }
    }
    #[test]
    fn full_snapshots_above_old_limit_survive_instance_reclaim_without_trimming() {
        let host = MockHost::install();
        let mut snap = snapshot();
        snap.sources.push(Source { name: "runtime".into(), props: vec![("large".into(), "é".repeat(600_000))] });
        write(&scope(), &snap).unwrap();
        let restored = read(&scope()).unwrap().unwrap();
        assert_eq!(restored, snap);
        assert!(!restored.trimmed);
        assert_eq!(restored.loggers.len(), 1);
        assert!(host.storage()[&scope().key()].len() > 900 * 1024);
        assert!(host.requests().is_empty());
    }
    #[test]
    fn exact_envelope_boundary_is_readable_and_overflow_preserves_real_previous_bytes() {
        let host = MockHost::install();
        let s = scope();
        let previous = snapshot();
        write(&s, &previous).unwrap();
        let bytes = host.storage()[&s.key()].clone();
        let mut snap = snapshot();
        snap.sources.push(Source { name: "runtime".into(), props: vec![("large".into(), String::new())] });
        let empty = serde_json::to_string(&Envelope { schema: SCHEMA, scope: s.clone(), snapshot: snap.clone() }).unwrap().len();
        snap.sources[0].props[0].1 = "x".repeat(MAX_BYTES - empty + 1);
        assert!(write(&s, &snap).is_err());
        assert_eq!(host.storage()[&s.key()], bytes);
        assert_eq!(read(&s).unwrap(), Some(previous));
        snap.sources[0].props[0].1.pop();
        write(&s, &snap).unwrap();
        assert_eq!(host.storage()[&s.key()].len(), MAX_BYTES);
        assert_eq!(read(&s).unwrap().unwrap(), snap);
    }
    #[test]
    fn schema_one_cache_is_migrated_without_details_and_rewritten_as_current() {
        let host = MockHost::install();
        let s = scope();
        let mut value = serde_json::to_value(Envelope { schema: 1, scope: s.clone(), snapshot: snapshot() }).unwrap();
        value["snapshot"].as_object_mut().unwrap().remove("health_details");
        host::storage::set(&s.key(), &value.to_string()).unwrap();
        let restored = read(&s).unwrap().unwrap();
        assert!(restored.health_details.is_empty());
        write(&s, &restored).unwrap();
        assert!(host.storage()[&s.key()].starts_with(r#"{"schema":5"#));
    }
    #[test]
    fn schema_two_cache_masks_configprops_values_when_read() {
        let _host = MockHost::install();
        let s = scope();
        let mut snap = snapshot();
        snap.configprops.push(crate::snapshot::PropsBean { prefix: "app".into(), ty: "com.acme.App".into(), props: vec![("app.password".into(), "hunter2".into()), ("app.name".into(), "demo".into())] });
        host::storage::set(&s.key(), &serde_json::to_string(&Envelope { schema: 2, scope: s.clone(), snapshot: snap }).unwrap()).unwrap();
        let restored = read(&s).unwrap().unwrap();
        assert_eq!(restored.configprops[0].props, vec![("app.password".to_owned(), crate::snapshot::MASK.to_owned()), ("app.name".to_owned(), "demo".to_owned())]);
    }
    #[test]
    fn older_schema_cache_is_rewritten_masked_on_read() {
        let host = MockHost::install();
        let s = scope();
        for schema in [3, 4] {
            let mut snap = snapshot();
            snap.sources.push(Source { name: "env".into(), props: vec![("APIKEY".into(), "hunter2".into()), ("app.name".into(), "demo".into()),
                ("spring.datasource.url".into(), "jdbc:postgresql://u:p/ss?w@db/x".into()),
                ("sun.java.command".into(), "app.jar --x.password=hunter3 --keep=1".into())] });
            host::storage::set(&s.key(), &serde_json::to_string(&Envelope { schema, scope: s.clone(), snapshot: snap }).unwrap()).unwrap();
            let restored = read(&s).unwrap().unwrap();
            assert_eq!(restored.sources[0].props[0].1, crate::snapshot::MASK);
            let stored = &host.storage()[&s.key()];
            assert!(!stored.contains("hunter2") && !stored.contains("hunter3") && !stored.contains("p/ss"), "schema {schema} cache kept a raw secret");
            assert!(restored.sources[0].props[3].1.contains("--keep=1"));
            assert!(stored.starts_with(r#"{"schema":5"#));
        }
    }
    #[test]
    fn owner_url_auth_and_schema_are_checked_independently_of_hash_locator() {
        let host = MockHost::install();
        let s = scope();
        let snap = snapshot();
        for wrong in [
            Scope { root: "/other".into(), ..s.clone() },
            Scope { url: "http://localhost:9090/actuator".into(), ..s.clone() },
            Scope { auth: "bearer".into(), ..s.clone() },
        ] {
            let value = serde_json::to_string(&Envelope { schema: SCHEMA, scope: wrong.clone(), snapshot: snap.clone() }).unwrap();
            host::storage::set(&s.key(), &value).unwrap();
            assert!(read(&s).is_err());
            assert!(write(&s, &snap).is_err());
            assert!(clear(&s).is_err());
            assert_eq!(host.storage()[&s.key()], value);
        }
        host::storage::set(&s.key(), "broken").unwrap();
        assert!(read(&s).is_err());
        assert!(write(&s, &snap).is_err());
        clear(&s).unwrap();
        assert!(read(&s).unwrap().is_none());
        let value = serde_json::to_string(&Envelope { schema: 9, scope: s.clone(), snapshot: snap }).unwrap();
        host::storage::set(&s.key(), &value).unwrap();
        assert!(read(&s).is_err());
        let host = host.with_capabilities(&[]);
        assert!(read(&s).is_err());
        assert_eq!(host.storage()[&s.key()], value);
    }
}
