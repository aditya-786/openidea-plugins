//! Explicit profile-backed Redmine/Mantis protocols on approved Python3.
use crate::{model::Task, trackers::Scope};
use openidea_plugin::{Error, host};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::{Duration, Instant};
const HELPER: &str = include_str!("../adapters/self_hosted_bridge.py");
const GENERIC_HELPER: &str = include_str!("../adapters/generic_bridge.py");
const GENERIC_REGEX: &str = include_str!("../adapters/GenericRegex.java");
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Profile {
    pub family: String,
    pub base_url: String,
    pub profile_path: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub family: String,
    pub base_url: String,
    pub number: String,
    pub project_id: String,
    pub revision: String,
    pub profile_id: String,
    pub can_close: bool,
    pub close_status_id: String,
    pub guard: String,
}
fn decimal(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('0')
        && value.len() <= 10
        && value.bytes().all(|b| b.is_ascii_digit())
        && value.parse::<u32>().is_ok_and(|v| v <= i32::MAX as u32)
}
fn digest(value: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
fn profile_id(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}
fn family(value: &str) -> bool {
    matches!(value, "redmine" | "mantis" | "generic")
}
fn generic_key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && !value.contains("..")
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}
fn number_ok(family: &str, value: &str) -> bool {
    if family == "generic" {
        return generic_key(value);
    }
    decimal(value)
}
fn project_ok(family: &str, value: &str) -> bool {
    if family == "generic" {
        return value.is_empty();
    }
    decimal(value)
}
fn status_ok(family: &str, value: &str) -> bool {
    if family == "generic" {
        return value.len() <= 128 && !value.chars().any(char::is_control);
    }
    decimal(value)
}
fn under_base(base: &str, url: &str) -> bool {
    url.strip_prefix(base)
        .is_some_and(|rest| rest.starts_with('/') && rest.len() <= 1024 && !rest.chars().any(char::is_control))
}
fn profile_path(value: &str) -> bool {
    value.starts_with('/')
        && value.len() <= 4096
        && !value.contains(['\\', '\0', '\n', '\r'])
        && value.split('/').all(|p| !matches!(p, "." | ".."))
}
pub fn server(base: &str) -> Option<&str> {
    let rest = base.strip_prefix("https://")?;
    if rest.is_empty() || rest.len() > 2048 || base.ends_with('/') {
        return None;
    }
    let (host, prefix) = rest.split_once('/').unwrap_or((rest, ""));
    if host.len() > 253
        || host.split('.').any(|p| {
            p.is_empty()
                || p.len() > 63
                || p.starts_with('-')
                || p.ends_with('-')
                || !p
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
        || !host.contains('.')
        || host.bytes().all(|b| b.is_ascii_digit() || b == b'.')
    {
        return None;
    }
    if !prefix.is_empty()
        && prefix.split('/').any(|p| {
            p.is_empty()
                || matches!(p, "." | "..")
                || !p
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-._~".contains(&b))
        })
    {
        return None;
    }
    Some(host)
}
pub fn profiles() -> Result<Vec<Profile>, Error> {
    let raw = host::setting("selfHostedProfiles").unwrap_or_else(|| "[]".into());
    if raw.len() > 65536 {
        return Err("Self-hosted tracker profile descriptors exceed 64KiB.".into());
    }
    let profiles:Vec<Profile>=serde_json::from_str(&raw).map_err(|_|"Self-hosted profile descriptors must be an array of family/baseUrl/profilePath objects.")?;
    let mut seen = std::collections::BTreeSet::new();
    if profiles.len() > 16
        || profiles.iter().any(|p| {
            !family(&p.family)
                || server(&p.base_url).is_none()
                || !profile_path(&p.profile_path)
                || !seen.insert((p.family.clone(), p.base_url.clone()))
        })
    {
        return Err("Use at most 16 unique Redmine/Mantis/Generic HTTPS DNS profile descriptors with normalized absolute credential paths.".into());
    }
    Ok(profiles)
}
fn configured(family: &str, base: &str) -> Result<Profile, Error> {
    profiles()?
        .into_iter()
        .find(|p| p.family == family && p.base_url == base)
        .ok_or("Self-hosted tracker profile changed or is unavailable; search again.".into())
}
pub fn id(family: &str, base: &str, number: &str) -> String {
    format!(
        "{}-{}-{number}",
        match family {
            "redmine" => "RM",
            "mantis" => "MB",
            _ => "GN",
        },
        &digest(base)[..16]
    )
}
fn urls(family: &str, base: &str, number: &str) -> (String, String) {
    if family == "redmine" {
        (
            format!("{base}/issues/{number}.json"),
            format!("{base}/issues/{number}"),
        )
    } else {
        (
            format!("{base}/api/rest/issues/{number}"),
            format!("{base}/view.php?id={number}"),
        )
    }
}
pub fn identity(task: &Task) -> bool {
    let Some(p) = &task.self_hosted else {
        return false;
    };
    if !family(&p.family)
        || task.tracker.as_deref() != Some(p.family.as_str())
        || server(&p.base_url).is_none()
        || !number_ok(&p.family, &p.number)
        || !project_ok(&p.family, &p.project_id)
        || !profile_id(&p.profile_id)
        || p.revision.is_empty()
        || p.revision.len() > 512
        || p.revision.chars().any(char::is_control)
        || !matches!(p.guard.as_str(), "etag" | "fresh-read" | "none")
        || p.can_close && (p.close_status_id.is_empty() || !status_ok(&p.family, &p.close_status_id) || p.guard == "none")
        || !p.close_status_id.is_empty() && !status_ok(&p.family, &p.close_status_id)
    {
        return false;
    }
    if p.family == "generic" {
        return task.id == id(&p.family, &p.base_url, &p.number)
            && task.api.as_deref().is_some_and(|u| under_base(&p.base_url, u))
            && task.url.as_deref().is_some_and(|u| under_base(&p.base_url, u));
    }
    let (api, web) = urls(&p.family, &p.base_url, &p.number);
    task.id == id(&p.family, &p.base_url, &p.number)
        && task.api.as_deref() == Some(api.as_str())
        && task.url.as_deref() == Some(web.as_str())
}
struct Private(String);
impl Drop for Private {
    fn drop(&mut self) {
        let _ = host::fs::write_file(&self.0, "");
    }
}
fn invoke(profile: &Profile, request: Value) -> Result<Value, Error> {
    let directory = host::fs::plugin_dir()?;
    host::fs::write_file(&format!("{directory}/self_hosted_bridge.py"), HELPER)?;
    let helper = if profile.family == "generic" {
        host::fs::write_file(&format!("{directory}/GenericRegex.java"), GENERIC_REGEX)?;
        let generic = format!("{directory}/generic_bridge.py");
        host::fs::write_file(&generic, GENERIC_HELPER)?;
        generic
    } else {
        format!("{directory}/self_hosted_bridge.py")
    };
    host::fs::write_file(
        &format!("{directory}/fogbugz_bridge.py"),
        include_str!("../adapters/fogbugz_bridge.py"),
    )?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| Error::Stale)?
        .as_nanos();
    let input = Private(format!("{directory}/self-hosted-{nonce}.in"));
    let output = Private(format!("{directory}/self-hosted-{nonce}.out"));
    let text = request.to_string();
    if text.len() > 16384 {
        return Err("Tracker bridge request exceeds 16KiB.".into());
    }
    host::fs::write_file(&input.0, &text)?;
    let task = host::begin_task("Self-hosted tracker credential bridge", true);
    let child = host::start(
        &host::Command::new("python3").args([
            "-I",
            "-S",
            &helper,
            &profile.profile_path,
            &input.0,
            &output.0,
        ]),
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
                "Tracker bridge timed out; check remote state before retrying a close.".into()
            });
        }
        let chunk = child.wait(100)?;
        bytes += chunk.stdout.len() + chunk.stderr.len();
        if bytes > 8192 {
            let _ = child.kill();
            return Err("Tracker bridge process output exceeds 8KiB.".into());
        }
        if let Some(status) = chunk.status {
            if status != 0 {
                return Err("Tracker credential bridge failed; check private profile and approved Python 3. No credential or remote body is displayed.".into());
            }
            break;
        }
    }
    let stat = host::fs::stat(&output.0)?;
    if stat.kind != openidea_plugin::FileKind::File || stat.size > 1 << 20 {
        return Err("Tracker bridge result exceeds 1MiB.".into());
    }
    let result: Value = serde_json::from_str(&host::fs::read_file(&output.0)?)
        .map_err(|_| "Invalid tracker bridge JSON.")?;
    if result["version"] != 1
        || result["ok"] != true
        || result["family"] != profile.family
        || result["baseUrl"] != profile.base_url
        || result["host"] != server(&profile.base_url).unwrap_or("")
        || !result["profileId"].as_str().is_some_and(profile_id)
        || result["operation"] != request["operation"]
    {
        return Err("Tracker bridge refused this request; check profile, source freshness and permissions. No remote error body is displayed.".into());
    }
    if configured(&profile.family, &profile.base_url)? != *profile {
        return Err(Error::Stale);
    }
    Ok(result)
}
fn parse(result: &Value, profile: &Profile) -> Result<Vec<Task>, Error> {
    let profile_hash = result["profileId"]
        .as_str()
        .filter(|s| profile_id(s))
        .ok_or("Missing tracker profile identity.")?;
    let items = result["issues"]
        .as_array()
        .filter(|a| a.len() <= 50)
        .ok_or("Tracker result exceeds 50 issues.")?;
    let mut seen = std::collections::BTreeSet::new();
    let mut tasks = Vec::new();
    for item in items {
        let field = |name| {
            item[name]
                .as_str()
                .ok_or_else(|| Error::Invalid("Missing tracker issue field.".into()))
        };
        let number = field("number")?;
        let project = field("projectId")?;
        let summary = field("summary")?;
        let description = field("description")?;
        let revision = field("revision")?;
        let close_status = field("closeStatusId")?;
        let guard = field("guard")?;
        let closed = item["closed"]
            .as_bool()
            .ok_or("Invalid tracker issue state.")?;
        let can_close = item["canClose"]
            .as_bool()
            .ok_or("Invalid tracker transition permissions.")?;
        let (api, web) = if profile.family == "generic" {
            (
                item["api"].as_str().unwrap_or_default().to_owned(),
                item["url"].as_str().unwrap_or_default().to_owned(),
            )
        } else {
            urls(&profile.family, &profile.base_url, number)
        };
        if !number_ok(&profile.family, number)
            || !project_ok(&profile.family, project)
            || summary.is_empty()
            || summary.len() > 4096
            || summary.chars().any(char::is_control)
            || description.len() > 32768
            || description.contains('\0')
            || item["url"] != web.as_str()
            || !seen.insert(number)
        {
            return Err("Invalid, foreign or duplicate tracker issue record.".into());
        }
        let mut task = Task::issue(
            &profile.family,
            &id(&profile.family, &profile.base_url, number),
            summary,
            &web,
            &api,
        );
        task.description = description.into();
        task.closed = closed;
        task.self_hosted = Some(Identity {
            family: profile.family.clone(),
            base_url: profile.base_url.clone(),
            number: number.into(),
            project_id: project.into(),
            revision: revision.into(),
            profile_id: profile_hash.into(),
            can_close,
            close_status_id: close_status.into(),
            guard: guard.into(),
        });
        if !identity(&task) {
            return Err("Invalid tracker issue proof.".into());
        }
        tasks.push(task);
    }
    Ok(tasks)
}
pub fn search(scope: &Scope, family: &str) -> Result<Vec<Task>, Error> {
    let selected = scope
        .repositories
        .get(family)
        .filter(|p| p.len() == 1)
        .and_then(|p| p.first())
        .ok_or("Select exactly one self-hosted tracker profile.")?;
    let profile = configured(family, selected)?;
    let query = scope
        .query
        .filter(|q| q.len() <= 4096 && !q.chars().any(char::is_control))
        .ok_or("Self-hosted bridges require an explicit bounded parameter search.")?;
    if !(1..=5).contains(&scope.page) {
        return Err("Search supports at most 5 bounded pages.".into());
    }
    if !ui_kit::prompts::confirm_exact(
        "Search this self-hosted tracker?",
        &format!(
            "Approved Python 3 reads the externally provisioned 0600 credential profile and calls this exact HTTPS service; redirects are refused.\nFamily: {family}\nBase URL: {}\nProfile: {}\nQuery parameters: {}\nPage: {} (at most 50 results)\nRedmine supports documented issue filter parameters. Mantis requires project_id and optional filter_id. Generic sends the query text as the profile-declared search parameter. No unsupported free-text search is invented.",
            profile.base_url,
            serde_json::to_string(&profile.profile_path).unwrap_or_default(),
            serde_json::to_string(query).unwrap_or_default(),
            scope.page
        ),
    )? {
        return Err(Error::Cancelled);
    }
    let result = invoke(
        &profile,
        json!({"version":1,"operation":"search","expectedFamily":family,"expectedBaseUrl":profile.base_url,"query":query,"page":scope.page}),
    )?;
    if result["page"] != scope.page || !result["hasMore"].is_boolean() {
        return Err("Invalid tracker search page.".into());
    }
    let parsed = parse(&result, &profile)?;
    if result["hasMore"] == true && parsed.len() < 50 {
        return Err("Tracker pagination reports more matches after a short page; refine the query instead of accepting an incomplete page.".into());
    }
    Ok(parsed)
}
pub fn close(task: &Task) -> Result<(), Error> {
    if !identity(task) || task.closed {
        return Err("Tracker issue identity/state is invalid; search again.".into());
    }
    let proof = task.self_hosted.as_ref().ok_or(Error::Stale)?;
    let profile = configured(&proof.family, &proof.base_url)?;
    if !proof.can_close {
        return Err("No reviewed allowed close transition is available; search again.".into());
    }
    if !ui_kit::prompts::confirm_exact(
        "Close this self-hosted issue?",
        &format!(
            "Service: {}\nIssue: {}\nSummary: {}\nProject: {}\nConfigured close status ID: {}\nConcurrency: {}\nThe helper re-fetches identity, profile, revision and allowed status before mutation, then verifies state. Redmine fresh-read guards cannot exclude another server update between the final read and PUT; Mantis uses the observed ETag. Local task/git options are handled separately.",
            proof.base_url,
            task.id,
            serde_json::to_string(&task.summary).unwrap_or_default(),
            proof.project_id,
            proof.close_status_id,
            proof.guard
        ),
    )? {
        return Err(Error::Cancelled);
    }
    let result=invoke(&profile,json!({"version":1,"operation":"close","expectedFamily":proof.family,"expectedBaseUrl":proof.base_url,"expectedId":proof.number,"expectedProjectId":proof.project_id,"expectedRevision":proof.revision,"expectedProfileId":proof.profile_id})).map_err(|error|match error{Error::Denied(_)=>error,_=>"Tracker close was not confirmed; check the remote issue before retrying. No credential or remote body is displayed.".into()})?;
    let parsed = parse(&result, &profile).map_err(|_| {
        Error::Invalid(
            "Tracker close response was not verified; check remote state before retrying.".into(),
        )
    })?;
    if parsed.len() != 1
        || parsed[0].id != task.id
        || !parsed[0].closed
        || parsed[0]
            .self_hosted
            .as_ref()
            .is_none_or(|p| p.profile_id != proof.profile_id || p.project_id != proof.project_id)
    {
        return Err(
            "Tracker close could not be verified; check remote state before retrying.".into(),
        );
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::trackers;
    use openidea_plugin::mock::MockHost;
    fn profile(family: &str) -> Profile {
        Profile {
            family: family.into(),
            base_url: "https://tracker.example/project".into(),
            profile_path: "/profiles/tracker.json".into(),
        }
    }
    fn result(family: &str) -> Value {
        let p = profile(family);
        let (_, web) = urls(family, &p.base_url, "17");
        json!({"profileId":"a".repeat(64),"issues":[{"number":"17","projectId":"1","summary":"Fix parser","description":"Plain\ntext","url":web,"closed":false,"revision":"revision-1","canClose":true,"closeStatusId":if family=="redmine"{"5"}else{"90"},"guard":if family=="redmine"{"fresh-read"}else{"etag"}}]})
    }
    fn configure(h: &MockHost, items: &[Profile]) {
        h.set_setting("selfHostedProfiles", &serde_json::to_string(items).unwrap());
    }
    #[test]
    fn distinct_profile_bases_have_stable_scoped_task_identities_and_nonsecret_proofs() {
        for family in ["redmine", "mantis"] {
            let p = profile(family);
            let tasks = parse(&result(family), &p).unwrap();
            assert!(identity(&tasks[0]));
            assert!(trackers::can_update_state(&tasks[0]));
            let text = serde_json::to_string(&tasks[0]).unwrap();
            assert!(!text.contains("apiKey"));
            let restored: Task = serde_json::from_str(&text).unwrap();
            assert_eq!(restored, tasks[0]);
            let mut foreign = restored;
            foreign.api = Some("https://other.example/issues/17.json".into());
            assert!(!identity(&foreign));
        }
        assert_ne!(
            id("redmine", "https://tracker.example/a", "17"),
            id("redmine", "https://tracker.example/b", "17")
        );
        assert_ne!(
            id("redmine", "https://tracker.example/a", "17"),
            id("mantis", "https://tracker.example/a", "17")
        );
    }
    #[test]
    fn multiple_exact_profiles_select_distinct_targets_and_never_run_automatically() {
        let h = MockHost::install().with_capabilities(&[]);
        let p = profile("redmine");
        let mut q = profile("mantis");
        q.base_url = "https://tracker.example/mantis".into();
        configure(&h, &[p.clone(), q]);
        let repos = std::collections::BTreeMap::new();
        let scope = Scope {
            remotes: &[],
            query: None,
            page: 1,
            repositories: &repos,
        };
        let targets = trackers::search_targets(&scope).unwrap();
        assert_eq!(targets.len(), 2);
        assert!(targets.iter().all(|t| t.paginated()));
        assert!(
            targets
                .iter()
                .any(|t| t.repository.as_deref() == Some(p.base_url.as_str()))
        );
        let signature = trackers::search_signature(&scope).unwrap();
        let mut changed = p;
        changed.profile_path = "/profiles/changed.json".into();
        configure(&h, &[changed]);
        assert_ne!(signature, trackers::search_signature(&scope).unwrap());
        assert_eq!(trackers::fetch_all(&scope).trackers, 0);
        assert!(h.process_runs().is_empty());
        assert!(h.requests().is_empty());
    }
    #[test]
    fn descriptors_and_reply_ownership_are_strict_before_task_creation() {
        let h = MockHost::install().with_capabilities(&[]);
        let p = profile("redmine");
        configure(&h, &[p.clone(), p.clone()]);
        assert!(profiles().is_err());
        for url in [
            "http://tracker.example",
            "https://user@tracker.example",
            "https://tracker.example:8443",
            "https://tracker.example/../other",
            "https://tracker.example/prefix%2Fother",
            "https://tracker.example/?token=x",
            "https://127.0.0.1",
        ] {
            assert!(server(url).is_none(), "{url}");
        }
        let mut reply = result("redmine");
        reply["issues"][0]["url"] = json!("https://tracker.example/project-foreign/issues/17");
        assert!(parse(&reply, &p).is_err());
        let mut reply = result("redmine");
        reply["issues"][0]["guard"] = json!("none");
        assert!(parse(&reply, &p).is_err());
        let mut reply = result("redmine");
        reply["issues"][0]["projectId"] = json!("0");
        assert!(parse(&reply, &p).is_err());
    }
    #[test]
    fn cancelled_search_or_close_and_changed_profile_never_start_transport() {
        let h = MockHost::install().with_capabilities(&[]);
        let p = profile("redmine");
        configure(&h, &[p.clone()]);
        let repos =
            std::collections::BTreeMap::from([("redmine".into(), vec![p.base_url.clone()])]);
        let scope = Scope {
            remotes: &[],
            query: Some("project_id=1"),
            page: 1,
            repositories: &repos,
        };
        h.answer_next_prompt(None);
        assert!(matches!(search(&scope, "redmine"), Err(Error::Cancelled)));
        let task = parse(&result("redmine"), &p).unwrap().remove(0);
        h.answer_next_prompt(None);
        assert!(matches!(close(&task), Err(Error::Cancelled)));
        configure(&h, &[]);
        assert!(close(&task).is_err());
        assert!(h.process_runs().is_empty());
    }
    #[test]
    fn generic_rows_keep_declared_urls_under_their_base_and_alphanumeric_keys() {
        let p = profile("generic");
        let reply = || json!({"profileId":"a".repeat(64),"issues":[{"number":"TASK-1","projectId":"","summary":"Example","description":"","url":"https://tracker.example/project/issues/TASK-1","api":"https://tracker.example/project/api/issues/TASK-1","closed":false,"revision":"b".repeat(64),"canClose":true,"closeStatusId":"DONE","guard":"etag"}]});
        let tasks = parse(&reply(), &p).unwrap();
        assert!(identity(&tasks[0]));
        assert!(trackers::can_update_state(&tasks[0]));
        assert_ne!(tasks[0].id, id("redmine", &p.base_url, "TASK-1"));
        for (field, value) in [
            ("url", "https://tracker.example/project-foreign/issues/TASK-1"),
            ("api", "https://other.example/project/api/issues/TASK-1"),
            ("number", "../x"),
            ("projectId", "1"),
        ] {
            let mut bad = reply();
            bad["issues"][0][field] = json!(value);
            assert!(parse(&bad, &p).is_err(), "{field}");
        }
    }
}
