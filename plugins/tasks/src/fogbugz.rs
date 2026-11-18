//! Explicit approved-process FogBugz XML transport; credentials stay in an
//! external private profile, never native plugin settings/storage or HTTP headers.
use crate::{model::Task, trackers::Scope};
use openidea_plugin::{Error, host};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::{Duration, Instant};
const HELPER: &str = include_str!("../adapters/fogbugz_bridge.py");
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub host: String,
    pub number: String,
    pub event_id: String,
    pub profile_id: String,
    pub can_close: bool,
}
fn decimal(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 18
        && !s.starts_with('0')
        && s.bytes().all(|b| b.is_ascii_digit())
        && s.parse::<u32>().is_ok_and(|n| n <= i32::MAX as u32)
}
fn host_valid(host: &str) -> bool {
    host.strip_suffix(".fogbugz.com").is_some_and(|t| {
        !t.is_empty()
            && t.len() <= 63
            && !t.starts_with('-')
            && !t.ends_with('-')
            && t.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    })
}
fn profile_id(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit())
}
fn configured() -> Result<(String, String), Error> {
    let server = host::setting_string("fogbugzHost").unwrap_or_default();
    let profile = host::setting_string("fogbugzProfile").unwrap_or_default();
    if !host_valid(&server)
        || !profile.starts_with('/')
        || profile.len() > 4096
        || profile.contains(['\\', '\0', '\n', '\r'])
        || profile.split('/').any(|p| matches!(p, "." | ".."))
    {
        return Err(
            "Configure exact FogBugz tenant host and absolute external private profile path first."
                .into(),
        );
    }
    Ok((server, profile))
}
pub fn configured_host() -> Option<String> {
    configured().ok().map(|(s, _)| s)
}
pub fn id(server: &str, number: &str) -> String {
    format!("FB-{}-{number}", server.trim_end_matches(".fogbugz.com"))
}
pub fn identity(task: &Task) -> bool {
    let Some(proof) = &task.fogbugz else {
        return false;
    };
    task.tracker.as_deref() == Some("fogbugz")
        && host_valid(&proof.host)
        && decimal(&proof.number)
        && decimal(&proof.event_id)
        && profile_id(&proof.profile_id)
        && task.id == id(&proof.host, &proof.number)
        && task.api.as_deref()
            == Some(format!("https://{}/api.asp?ixBug={}", proof.host, proof.number).as_str())
        && task.url.as_deref()
            == Some(format!("https://{}/default.asp?{}", proof.host, proof.number).as_str())
}
struct Private(String);
impl Drop for Private {
    fn drop(&mut self) {
        let _ = host::fs::write_file(&self.0, "");
    }
}
fn invoke(server: &str, profile: &str, request: Value) -> Result<Value, Error> {
    let directory = host::fs::plugin_dir()?;
    let helper = format!("{directory}/fogbugz_bridge.py");
    host::fs::write_file(&helper, HELPER)?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| Error::Stale)?
        .as_nanos();
    let input = Private(format!("{directory}/fogbugz-{nonce}.in"));
    let output = Private(format!("{directory}/fogbugz-{nonce}.out"));
    let text = request.to_string();
    if text.len() > 16384 {
        return Err("FogBugz request exceeds 16KiB.".into());
    }
    host::fs::write_file(&input.0, &text)?;
    let task = host::begin_task("FogBugz approved local credential bridge", true);
    let child = host::start(
        &host::Command::new("python3").args(["-I", "-S", &helper, profile, &input.0, &output.0]),
        None,
    )?;
    child.close_stdin()?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut bytes = 0;
    loop {
        if task.is_cancelled() || Instant::now() > deadline {
            let _ = child.kill();
            return Err(if task.is_cancelled() {
                Error::Cancelled
            } else {
                "FogBugz bridge timed out; check remote issue state before retrying a close.".into()
            });
        }
        let chunk = child.wait(100)?;
        bytes += chunk.stdout.len() + chunk.stderr.len();
        if bytes > 8192 {
            let _ = child.kill();
            return Err("FogBugz bridge exceeded its bounded process output.".into());
        }
        if let Some(status) = chunk.status {
            if status != 0 {
                return Err("FogBugz credential bridge failed; check private profile, approved Python 3 and network. No credential or remote error body is displayed.".into());
            }
            break;
        }
    }
    let stat = host::fs::stat(&output.0)?;
    if stat.kind != openidea_plugin::FileKind::File || stat.size > 1 << 20 {
        return Err("FogBugz bridge result exceeds 1MiB.".into());
    }
    let reply: Value = serde_json::from_str(&host::fs::read_file(&output.0)?)
        .map_err(|_| "Invalid FogBugz bridge JSON.")?;
    if reply["version"] != 1
        || reply["ok"] != true
        || reply["host"] != server
        || reply["operation"] != request["operation"]
        || !reply["profileId"].as_str().is_some_and(profile_id)
    {
        return Err("FogBugz bridge refused this request; check profile, issue freshness and account permissions. No remote error body is displayed.".into());
    }
    if configured()? != (server.into(), profile.into()) {
        return Err(Error::Stale);
    }
    Ok(reply)
}
fn parse(reply: &Value, server: &str) -> Result<Vec<Task>, Error> {
    let profile = reply["profileId"]
        .as_str()
        .filter(|s| profile_id(s))
        .ok_or("Missing FogBugz profile identity.")?;
    let items = reply["issues"]
        .as_array()
        .filter(|a| a.len() <= 50)
        .ok_or("FogBugz issue list exceeds 50.")?;
    let mut seen = std::collections::BTreeSet::new();
    let mut tasks = Vec::new();
    for item in items {
        let number = item["number"]
            .as_str()
            .filter(|s| decimal(s))
            .ok_or("Invalid FogBugz issue number.")?;
        let event = item["eventId"]
            .as_str()
            .filter(|s| decimal(s))
            .ok_or("Invalid FogBugz event identity.")?;
        let summary = item["summary"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 4096 && !s.chars().any(char::is_control))
            .ok_or("Invalid FogBugz summary.")?;
        let description = item["description"]
            .as_str()
            .filter(|s| s.len() <= 32768 && !s.contains('\0'))
            .ok_or("Invalid FogBugz description.")?;
        let closed = item["closed"].as_bool().ok_or("Invalid FogBugz state.")?;
        let can_close = item["canClose"]
            .as_bool()
            .ok_or("Invalid FogBugz permissions.")?;
        let web = format!("https://{server}/default.asp?{number}");
        if item["url"] != web || !seen.insert(number) {
            return Err("Foreign or duplicate FogBugz issue identity.".into());
        }
        let mut task = Task::issue(
            "fogbugz",
            &id(server, number),
            summary,
            &web,
            &format!("https://{server}/api.asp?ixBug={number}"),
        );
        task.description = description.into();
        task.closed = closed;
        task.fogbugz = Some(Identity {
            host: server.into(),
            number: number.into(),
            event_id: event.into(),
            profile_id: profile.into(),
            can_close,
        });
        if !identity(&task) {
            return Err(Error::Stale);
        }
        tasks.push(task);
    }
    Ok(tasks)
}
pub fn search(scope: &Scope) -> Result<Vec<Task>, Error> {
    let query = scope
        .query
        .filter(|q| !q.trim().is_empty() && q.len() <= 4096 && !q.chars().any(char::is_control))
        .ok_or("FogBugz bridge runs only for an explicit bounded search.")?;
    if scope.page != 1 {
        return Err(
            "FogBugz XML search has no supported offset; refine the query for more matches.".into(),
        );
    }
    let (server, profile) = configured()?;
    if !ui_kit::prompts::confirm_exact(
        &format!("Search FogBugz on {server}?"),
        &format!(
            "Query: {}\nPython 3 reads your token from {} and sends the search to {server}. The token stays in that file.\nShows up to 50 results.",
            serde_json::to_string(query).unwrap_or_default(),
            profile
        ),
    )? {
        return Err(Error::Cancelled);
    }
    let reply = invoke(
        &server,
        &profile,
        json!({"version":1,"operation":"search","expectedHost":server,"query":query,"page":1}),
    )?;
    if reply["page"] != 1 || !reply["hasMore"].is_boolean() {
        return Err("Invalid FogBugz search page.".into());
    }
    if reply["hasMore"] == true {
        host::notify(
            openidea_plugin::NotificationLevel::Info,
            "FogBugz search",
            "The server has more matches than the 50-result window; refine the query.",
        );
    }
    parse(&reply, &server)
}
pub fn close(task: &Task) -> Result<(), Error> {
    if !identity(task) {
        return Err("FogBugz task identity is invalid; search again.".into());
    }
    let proof = task.fogbugz.as_ref().ok_or(Error::Stale)?;
    let (server, profile) = configured()?;
    if server != proof.host || !proof.can_close || task.closed {
        return Err("FogBugz tenant, state or close permission changed; search again.".into());
    }
    if !ui_kit::prompts::confirm_exact(
        &format!("Close FogBugz issue {}?", task.id),
        &format!(
            "{}\nCloses it on {server}. If someone changed it since your last search, nothing is closed; search again.\nYour local task and Git branches are not changed.",
            serde_json::to_string(&task.summary).unwrap_or_default()
        ),
    )? {
        return Err(Error::Cancelled);
    }
    let reply = invoke(
        &server,
        &profile,
        json!({"version":1,"operation":"close","expectedHost":server,"expectedId":proof.number,"expectedEventId":proof.event_id,"expectedProfileId":proof.profile_id}),
    ).map_err(|error| match error { Error::Denied(_) => error, _ => "FogBugz close was not confirmed; check the remote issue before retrying. No credential or remote error body is displayed.".into() })?;
    let tasks = parse(&reply, &server).map_err(|_| {
        Error::Invalid(
            "FogBugz close response could not be verified; check the remote issue before retrying."
                .into(),
        )
    })?;
    if tasks.len() != 1
        || tasks[0].id != task.id
        || !tasks[0].closed
        || tasks[0]
            .fogbugz
            .as_ref()
            .is_none_or(|p| p.profile_id != proof.profile_id)
    {
        return Err(
            "FogBugz close could not be verified; check the remote issue before retrying.".into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trackers;
    use openidea_plugin::mock::MockHost;
    fn reply() -> Value {
        json!({"version":1,"ok":true,"host":"acme.fogbugz.com","operation":"search","profileId":"a".repeat(64),"page":1,"hasMore":false,"issues":[{"number":"17","eventId":"123","summary":"Fix parser","description":"Plain\ntext","url":"https://acme.fogbugz.com/default.asp?17","closed":false,"canClose":true}]})
    }
    #[test]
    fn sanitized_bridge_records_bind_tenant_event_profile_and_persist_without_credentials() {
        let tasks = parse(&reply(), "acme.fogbugz.com").unwrap();
        let task = &tasks[0];
        assert!(identity(task));
        assert_eq!(task.id, "FB-acme-17");
        assert!(trackers::can_update_state(task));
        let serialized = serde_json::to_string(task).unwrap();
        assert!(!serialized.contains("token"));
        let restored: Task = serde_json::from_str(&serialized).unwrap();
        assert_eq!(&restored, task);
        let mut foreign = task.clone();
        foreign.url = Some("https://other.fogbugz.com/default.asp?17".into());
        assert!(!identity(&foreign));
        let mut foreign = task.clone();
        foreign.fogbugz.as_mut().unwrap().event_id = "00".into();
        assert!(!identity(&foreign));
        let mut changed = reply();
        changed["issues"][0]["url"] = json!("https://acme.fogbugz.com/default.asp?18");
        assert!(parse(&changed, "acme.fogbugz.com").is_err());
        let mut changed = reply();
        let duplicate = changed["issues"][0].clone();
        changed["issues"].as_array_mut().unwrap().push(duplicate);
        assert!(parse(&changed, "acme.fogbugz.com").is_err());
    }
    #[test]
    fn bridge_target_is_explicit_and_periodic_refresh_never_launches_python() {
        let h = MockHost::install().with_capabilities(&["storage", "project.read"]);
        h.set_setting("fogbugzHost", &json!("acme.fogbugz.com").to_string());
        h.set_setting(
            "fogbugzProfile",
            &json!("/profiles/fogbugz.json").to_string(),
        );
        let repos = std::collections::BTreeMap::new();
        let scope = Scope {
            remotes: &[],
            query: None,
            page: 1,
            repositories: &repos,
        };
        let targets = trackers::search_targets(&scope).unwrap();
        let fog = targets.iter().find(|t| t.tracker == "fogbugz").unwrap();
        assert!(!fog.paginated());
        assert!(search(&scope).is_err());
        assert_eq!(trackers::fetch_all(&scope).trackers, 0);
        assert!(h.process_runs().is_empty());
        let signature = trackers::search_signature(&scope).unwrap();
        h.set_setting("fogbugzProfile", &json!("/profiles/other.json").to_string());
        assert_ne!(signature, trackers::search_signature(&scope).unwrap());
        h.set_setting(
            "fogbugzHost",
            &json!("acme.fogbugz.com.evil.test").to_string(),
        );
        assert!(configured_host().is_none());
    }
    #[test]
    fn cancelled_search_and_foreign_or_cancelled_close_do_not_start_processes() {
        let h = MockHost::install().with_capabilities(&[]);
        h.set_setting("fogbugzHost", &json!("acme.fogbugz.com").to_string());
        h.set_setting(
            "fogbugzProfile",
            &json!("/profiles/fogbugz.json").to_string(),
        );
        let repos = std::collections::BTreeMap::new();
        let scope = Scope {
            remotes: &[],
            query: Some("assignedTo:me"),
            page: 1,
            repositories: &repos,
        };
        h.answer_next_prompt(None);
        assert!(matches!(search(&scope), Err(Error::Cancelled)));
        let task = parse(&reply(), "acme.fogbugz.com").unwrap().remove(0);
        h.answer_next_prompt(None);
        assert!(matches!(close(&task), Err(Error::Cancelled)));
        h.set_setting("fogbugzHost", &json!("other.fogbugz.com").to_string());
        assert!(close(&task).is_err());
        assert!(h.process_runs().is_empty());
        assert!(h.requests().is_empty());
    }
}
