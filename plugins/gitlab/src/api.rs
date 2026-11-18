//! gitlab.com REST (and one GraphQL mutation) for the parts of the original
//! plugin that OpenIDEA can host. The token never reaches the plugin: the IDE
//! fills `{{secret:token}}` in the `Authorization` header.

use openidea_plugin::host::storage;
use openidea_plugin::{Error, HttpRequest, LogLevel, host};
use std::cell::Cell;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::safe;

const DEFAULT_SERVER: &str = "https://gitlab.com";

/// The `server` setting: an HTTPS origin (optionally with a path prefix), else gitlab.com. A host other than
/// gitlab.com is approved on first use through `net:ask`.
// ponytail: an invalid value falls back to gitlab.com; plain-http instances need setting-derived net.http grants upstream.
pub fn server() -> String {
    let configured = host::setting_string("server").unwrap_or_default();
    let Ok(url) = url::Url::parse(configured.trim()) else {
        return DEFAULT_SERVER.to_owned();
    };
    let plain = url.scheme() == "https" && url.host_str().is_some() && url.username().is_empty() && url.password().is_none()
        && url.query().is_none() && url.fragment().is_none();
    match plain {
        true => url.as_str().trim_end_matches('/').to_owned(),
        false => DEFAULT_SERVER.to_owned(),
    }
}

pub fn server_host() -> String {
    url::Url::parse(&server()).ok().and_then(|u| u.host_str().map(str::to_owned)).unwrap_or_else(|| "gitlab.com".to_owned())
}

pub(crate) fn server_prefix() -> String {
    format!("{}/", server())
}
pub const TOKEN: &str = "token";
pub(crate) fn token() -> &'static str { if host::setting_string("accountSlot").as_deref()==Some("secondary") {"secondary-token"} else {TOKEN} }

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct User {
    pub id: u64,
    pub username: String,
    pub name: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Pipeline {
    pub id: u64,
    pub status: String,
    pub web_url: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Job {
    pub id: u64,
    pub name: String,
    pub stage: String,
    pub status: String,
    pub web_url: String,
    pub allow_failure: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Permissions {
    pub can_merge: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Milestone {
    pub id: u64,
    pub title: String,
}

#[derive(Deserialize)]
struct Label {
    name: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct DiffRefs {
    pub base_sha: String,
    pub head_sha: String,
    pub start_sha: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct MergeRequest {
    pub iid: u64,
    pub source_project_id: u64,
    pub target_project_id: u64,
    pub diff_refs: Option<DiffRefs>,
    pub title: String,
    pub description: Option<String>,
    pub state: String,
    pub draft: bool,
    pub source_branch: String,
    pub target_branch: String,
    pub author: User,
    pub reviewers: Vec<User>,
    pub assignees: Vec<User>,
    pub labels: Vec<String>,
    pub milestone: Option<Milestone>,
    pub web_url: String,
    pub sha: Option<String>,
    pub detailed_merge_status: Option<String>,
    pub head_pipeline: Option<Pipeline>,
    pub force_remove_source_branch: Option<bool>,
    pub squash: bool,
    pub user: Option<Permissions>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct RebaseState {
    pub rebase_in_progress: bool,
    pub merge_error: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Approver {
    pub user: User,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Approvals {
    pub approved_by: Vec<Approver>,
    pub approvals_left: u32,
    pub user_has_approved: bool,
    pub user_can_approve: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct NotePosition {
    pub new_path: Option<String>,
    pub new_line: Option<u32>,
    pub old_path: Option<String>,
    pub old_line: Option<u32>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Note {
    pub id: u64,
    pub body: String,
    pub author: User,
    pub created_at: String,
    pub system: bool,
    pub resolvable: bool,
    pub resolved: bool,
    pub position: Option<NotePosition>,
}

impl NotePosition {
    /// Repository-relative path and 1-based line; the new side wins for context lines.
    pub fn location(&self) -> Option<(&str, u32)> {
        match (&self.new_path, self.new_line, &self.old_path, self.old_line) {
            (Some(path), Some(line), _, _) => Some((path, line)),
            (_, _, Some(path), Some(line)) => Some((path, line)),
            _ => None,
        }
    }

    /// Whether this position is the diff line `old`/`new` of the file `old_path` → `new_path`.
    pub fn is_at(&self, old_path: &str, new_path: &str, old: Option<u32>, new: Option<u32>) -> bool {
        match (self.new_line, self.old_line) {
            (Some(line), _) => self.new_path.as_deref() == Some(new_path) && new == Some(line),
            (None, Some(line)) => self.old_path.as_deref() == Some(old_path) && old == Some(line) && new.is_none(),
            (None, None) => false,
        }
    }
}

impl Note {
    pub fn location(&self) -> Option<(&str, u32)> {
        self.position.as_ref()?.location()
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Discussion {
    pub id: String,
    pub notes: Vec<Note>,
}

impl Discussion {
    pub fn resolvable(&self) -> bool {
        self.notes.iter().any(|n| n.resolvable)
    }

    pub fn resolved(&self) -> bool {
        self.notes.iter().filter(|n| n.resolvable).all(|n| n.resolved)
    }

    /// GitLab's own events (state, label… changes) come as single system notes.
    pub fn is_system(&self) -> bool {
        self.notes.first().is_some_and(|n| n.system)
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Award {
    pub id: u64,
    pub name: String,
    pub user: User,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct DraftNote {
    pub id: u64,
    pub note: String,
    pub discussion_id: Option<String>,
    pub position: Option<NotePosition>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct FileDiff {
    pub old_path: String,
    pub new_path: String,
    pub diff: String,
    pub new_file: bool,
    pub deleted_file: bool,
    pub renamed_file: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Commit {
    pub id: String,
    pub short_id: String,
    pub title: String,
    pub author_name: String,
    pub parent_ids: Vec<String>,
}

/// A state, label or milestone change of a merge request.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TimelineEvent {
    pub created_at: String,
    pub user: String,
    pub text: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct ProjectInfo {
    pub id: u64,
    pub name: String,
    pub path: String,
    pub path_with_namespace: String,
    pub name_with_namespace: String,
    pub description: Option<String>,
    pub default_branch: Option<String>,
    pub web_url: String,
    pub http_url_to_repo: String,
    pub ssh_url_to_repo: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct NewMergeRequest {
    pub source_branch: String,
    pub target_branch: String,
    pub title: String,
    pub description: String,
    pub assignee_id: Option<u64>,
    pub reviewer_ids: Vec<u64>,
    pub labels: Vec<String>,
    pub squash: bool,
    pub remove_source_branch: bool,
}

#[derive(Deserialize)]
struct Branch {
    name: String,
}

/// The `position` of a new diff comment on line `old`/`new` (1-based; both for a context line).
pub fn diff_position(refs: &DiffRefs, old_path: &str, new_path: &str, old: Option<u32>, new: Option<u32>) -> Value {
    let mut position = json!({
        "position_type": "text",
        "base_sha": refs.base_sha,
        "start_sha": refs.start_sha,
        "head_sha": refs.head_sha,
        "old_path": old_path,
        "new_path": new_path,
    });
    if let Some(old) = old {
        position["old_line"] = json!(old);
    }
    if let Some(new) = new {
        position["new_line"] = json!(new);
    }
    position
}

/// Percent-encodes everything but RFC 3986 unreserved characters.
pub fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn encode_segments(revision: &str) -> String {
    revision.split('/').map(encode).collect::<Vec<_>>().join("/")
}

/// `group/sub/name` of a remote on the configured server (`https://`, `ssh://` or `git@host:path`).
pub fn project_path(remote: &str) -> Option<String> {
    let (authority, path) = match remote.split_once("://") {
        Some((_, rest)) => rest.split_once('/')?,
        None => remote.split_once(':')?,
    };
    let host = authority.rsplit('@').next()?.split(':').next()?;
    if !host.eq_ignore_ascii_case(&server_host()) {
        return None;
    }

    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, name) = path.rsplit_once('/')?;
    if owner.is_empty() || name.is_empty() {
        return None;
    }

    Some(path.to_owned())
}

/// The web page of `path` (repository-relative) at `revision`, with an
/// optional 0-based inclusive line range (`#L3-L5`).
pub fn blob_url(project: &str, revision: &str, path: &str, lines: Option<(u32, u32)>) -> String {
    let path: Vec<String> = path.split('/').map(encode).collect();
    let url = format!("{server}/{project}/-/blob/{}/{}", encode_segments(revision), path.join("/"), server = server());

    match lines {
        Some((first, last)) if first != last => format!("{url}#L{}-L{}", first + 1, last + 1),
        Some((first, _)) => format!("{url}#L{}", first + 1),
        None => url,
    }
}

pub fn tree_url(project: &str, revision: &str, path: &str) -> String {
    let path: Vec<String> = path.split('/').map(encode).collect();

    format!("{server}/{project}/-/tree/{}/{}", encode_segments(revision), path.join("/"), server = server())
}

pub fn new_merge_request_url(project: &str, source_branch: &str) -> String {
    format!("{server}/{project}/-/merge_requests/new?merge_request%5Bsource_branch%5D={}", encode(source_branch), server = server())
}

pub fn new_token_url() -> String {
    format!("{server}/-/user_settings/personal_access_tokens?name=OpenIDEA%20GitLab&scopes=api,read_user", server = server())
}

fn html_title(body: &str) -> Option<&str> {
    let start = body.find("<title>")? + "<title>".len();

    Some(&body[start..start + body[start..].find("</title>")?])
}

fn without_markup(text: &str) -> String {
    let mut plain = String::new();
    let mut in_tag = false;
    for c in text.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => plain.push(c),
            _ => {}
        }
    }

    plain.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// An HTML error page (a proxy's 502) is reduced to its title.
fn error_message(body: &str) -> String {
    let message = match serde_json::from_str::<Value>(body) {
        Ok(json) => match json.get("message").or_else(|| json.get("error")) {
            Some(Value::String(s)) => s.clone(),
            Some(other) => other.to_string(),
            None => json.to_string(),
        },
        Err(_) => html_title(body).unwrap_or(body).to_owned(),
    };

    without_markup(&message)
}

const TOKEN_ORIGIN_KEY: &str = "token-origin";

/// The IDE fills the placeholder; the plugin never sees the token. The token is for gitlab.com: any other server
/// must be confirmed once (and again whenever it changes), and only from a command or view event, never a provider.
pub(crate) fn authorization(server_origin: &str) -> Result<String, Error> {
    let bearer = format!("Bearer {{{{secret:{}}}}}", token());
    if server_origin == DEFAULT_SERVER || storage::get(TOKEN_ORIGIN_KEY)?.as_deref() == Some(server_origin) {
        return Ok(bearer);
    }

    let host_name = server_host();
    if !INTERACTIVE.get() {
        return Err(Error::Invalid(format!("Token not sent to {host_name}. Confirm it from a GitLab command or view first.")));
    }

    let body = format!("Your GitLab access token will be sent to {server_origin} with every request.");
    if !host::confirm(&format!("Send your GitLab token to {host_name}?"), &body)? {
        return Err(Error::Invalid(format!("Token not sent to {host_name}")));
    }

    storage::set(TOKEN_ORIGIN_KEY, server_origin)?;
    Ok(bearer)
}

/// Marks the commands and view events (never providers, renders or background events) that may ask the user.
pub(crate) struct Interactive { previous: bool }
impl Interactive {
    pub(crate) fn start() -> Self {
        Self { previous: INTERACTIVE.replace(true) }
    }
}
impl Drop for Interactive {
    fn drop(&mut self) {
        INTERACTIVE.set(self.previous);
    }
}

fn send(method: &str, url: String, body: Option<Value>) -> Result<Value, Error> {
    if CALLBACK_DEADLINE.get().is_some() && method!="GET" {return Err("Snapshot callbacks permit GET reads only.".into());}
    let mut request = HttpRequest {
        method: method.into(),
        url,
        headers: vec![("Authorization".into(), authorization(&server())?)],
        body: None,
        timeout_ms: Some(callback_timeout_ms(20_000)?),
    };

    if let Some(body) = body {
        request.headers.push(("Content-Type".into(), "application/json".into()));
        request.body = Some(body.to_string().into_bytes());
    }

    let response = host::fetch(&request)?;
    require_callback_read_budget()?;
    let text = response.text();

    match response.status {
        200..=299 if text.trim().is_empty() => Ok(Value::Null),
        200..=299 => serde_json::from_str(&text).map_err(|e| Error::Failed(format!("Unexpected GitLab response: {e}"))),
        401 => Err("GitLab rejected the access token. Check it in Settings › Plugins › GitLab.".into()),
        status => Err(Error::Failed(format!("GitLab returned {status}: {}", error_message(&text)).chars().take(200).collect())),
    }
}

fn parse<T: DeserializeOwned>(value: Value) -> Result<T, Error> {
    serde_json::from_value(value).map_err(|e| Error::Failed(format!("Unexpected GitLab response: {e}")))
}

fn rest(method: &str, path: &str, body: Option<Value>) -> Result<Value, Error> {
    send(method, format!("{server}/api/v4{path}", server = server()), body)
}

const MAX_PAGES: usize = 10;
/// For all the lists of one task, checked before every page; a page can take up to 20 s, so the task ends by 55 s, under its 60 s limit.
const PAGING_BUDGET_SECS: u64 = 35;

thread_local! {
    static DEADLINE: Cell<Option<u64>> = const { Cell::new(None) };
    static CALLBACK_DEADLINE: Cell<Option<std::time::Instant>> = const { Cell::new(None) };
    static INTERACTIVE: Cell<bool> = const { Cell::new(false) };
}

/// Content-tab actions expire after15s in the pinned host. Limit their remote
/// read portion to10s; this guard never broadens an already tighter scope.
pub(crate) struct CallbackReadBudget { previous:Option<std::time::Instant> }
impl CallbackReadBudget {
    pub(crate) fn start()->Self {
        let previous=CALLBACK_DEADLINE.get();let deadline=std::time::Instant::now()+std::time::Duration::from_secs(10);
        CALLBACK_DEADLINE.set(Some(previous.map_or(deadline,|old|old.min(deadline))));Self{previous}
    }
}
impl Drop for CallbackReadBudget {fn drop(&mut self){CALLBACK_DEADLINE.set(self.previous);}}
fn callback_timeout_ms(limit:u32)->Result<u32,Error> {
    let remaining=CALLBACK_DEADLINE.get().map(|d|d.saturating_duration_since(std::time::Instant::now()).as_millis().min(u128::from(limit)) as u32).unwrap_or(limit);
    if remaining==0 {return Err("Snapshot callback read budget exhausted; retry explicitly.".into());}Ok(remaining)
}
pub(crate) fn require_callback_read_budget()->Result<(),Error> {callback_timeout_ms(20_000).map(|_|())}
#[cfg(test)]
pub(crate) fn expire_callback_read_budget(){CALLBACK_DEADLINE.set(Some(std::time::Instant::now()));}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Starts the paging budget of the task (command, refresh, event) that is about to run.
pub fn begin_budget() {
    begin_budget_at(now_secs());
}

pub(crate) fn begin_budget_at(now: u64) {
    DEADLINE.set(Some(now + PAGING_BUDGET_SECS));
}

pub(crate) fn require_read_budget()->Result<(),Error> {
    if DEADLINE.get().is_some_and(|deadline|now_secs()>=deadline) {return Err("Snapshot request budget exhausted; retry explicitly.".into());}require_callback_read_budget()
}

/// Every page of a list (a short page is the last one), up to [`MAX_PAGES`] and the task's deadline.
fn paged<T: DeserializeOwned>(path: &str, per_page: usize) -> Result<Vec<T>, Error> {
    paged_until(path, per_page, now_secs, PAGING_BUDGET_SECS)
}

/// Without [`begin_budget`], the deadline is `budget_secs` from now.
pub(crate) fn paged_until<T: DeserializeOwned>(path: &str, per_page: usize, now: impl Fn() -> u64, budget_secs: u64) -> Result<Vec<T>, Error> {
    let separator = if path.contains('?') { '&' } else { '?' };
    let deadline = DEADLINE.get().unwrap_or_else(|| now() + budget_secs);
    let mut all = Vec::new();

    for page in 1..=MAX_PAGES {
        if now() >= deadline {
            host::log(LogLevel::Warn, &format!("{path}: out of time after {} pages; showing the first {} results.", page - 1, all.len()));
            break;
        }

        let items: Vec<T> = parse(rest("GET", &format!("{path}{separator}per_page={per_page}&page={page}"), None)?)?;
        let last = items.len() < per_page;
        all.extend(items);
        if last {
            break;
        }
    }

    Ok(all)
}

pub fn current_user() -> Result<User, Error> {
    parse(rest("GET", "/user", None)?)
}

pub const VISIBILITIES: [&str; 3] = ["private", "internal", "public"];

pub struct SnippetDraft {
    pub title: String,
    pub description: String,
    pub visibility: String,
    /// `(repository path, content)` of each file.
    pub files: Vec<(String, String)>,
    /// `group/name` for a project snippet, `None` for a personal one.
    pub project: Option<String>,
}

pub fn create_snippet(draft: &SnippetDraft) -> Result<String, Error> {
    if !VISIBILITIES.contains(&draft.visibility.as_str()) {
        return Err(format!("Unknown visibility {:?}.", draft.visibility).into());
    }

    let mut body = json!({
        "title": draft.title,
        "visibility": draft.visibility,
        "files": draft.files.iter().map(|(file_path, content)| json!({ "file_path": file_path, "content": content })).collect::<Vec<_>>(),
    });
    if !draft.description.is_empty() {
        body["description"] = json!(draft.description);
    }

    let path = match &draft.project {
        Some(project) => Project(project).path("/snippets"),
        None => "/snippets".to_owned(),
    };
    let snippet = rest("POST", &path, Some(body))?;

    snippet["web_url"].as_str().map(str::to_owned).ok_or_else(|| "GitLab returned no snippet URL.".into())
}

/// Projects the user is a member of, most recently active first.
pub fn member_projects() -> Result<Vec<ProjectInfo>, Error> {
    paged("/projects?membership=true&simple=true&order_by=last_activity_at", 100)
}

pub fn create_project(name: &str, visibility: &str) -> Result<ProjectInfo, Error> {
    if !VISIBILITIES.contains(&visibility) {
        return Err(format!("Unknown visibility {visibility:?}.").into());
    }

    let body = json!({ "name": name, "visibility": visibility });

    parse(rest("POST", "/projects", Some(body))?)
}

/// One GitLab project (`group/name`).
pub struct Project<'a>(pub &'a str);

impl Project<'_> {
    fn path(&self, rest: &str) -> String {
        format!("/projects/{}{rest}", encode(self.0))
    }

    fn mr(&self, iid: u64, rest: &str) -> String {
        self.path(&format!("/merge_requests/{iid}{rest}"))
    }

    /// `query` is the filter part of the query string (`state=opened&scope=all`).
    pub fn merge_requests(&self, query: &str) -> Result<Vec<MergeRequest>, Error> {
        paged(&self.path(&format!("/merge_requests?{query}")), 50)
    }

    pub fn merge_request(&self, iid: u64) -> Result<MergeRequest, Error> {
        parse(rest("GET", &self.mr(iid, ""), None)?)
    }

    /// Names a `git` ref can carry; anything else the server lists is dropped.
    pub fn branches(&self) -> Result<Vec<String>, Error> {
        let branches: Vec<Branch> = paged(&self.path("/repository/branches"), 100)?;

        Ok(branches.into_iter().map(|b| b.name).filter(|name| safe::ref_name(name).is_ok()).collect())
    }

    pub fn create_merge_request(&self, new: &NewMergeRequest) -> Result<MergeRequest, Error> {
        let mut body = json!({
            "source_branch": new.source_branch,
            "target_branch": new.target_branch,
            "title": new.title,
            "description": new.description,
            "reviewer_ids": new.reviewer_ids,
            "labels": new.labels.join(","),
            "squash": new.squash,
            "remove_source_branch": new.remove_source_branch,
        });
        if let Some(id) = new.assignee_id {
            body["assignee_id"] = json!(id);
        }

        parse(rest("POST", &self.path("/merge_requests"), Some(body))?)
    }

    pub fn approvals(&self, iid: u64) -> Result<Approvals, Error> {
        parse(rest("GET", &self.mr(iid, "/approvals"), None)?)
    }

    pub fn info(&self) -> Result<ProjectInfo, Error> {
        parse(rest("GET", &self.path(""), None)?)
    }

    /// Threads, system notes included ([`Discussion::is_system`]).
    pub fn discussions(&self, iid: u64) -> Result<Vec<Discussion>, Error> {
        let mut discussions: Vec<Discussion> = paged(&self.mr(iid, "/discussions"), 100)?;
        discussions.retain(|d| !d.notes.is_empty());

        Ok(discussions)
    }

    /// The cumulative changes, every page.
    /// A file's text at `sha`; empty when the file does not exist there (added or deleted by the change).
    pub fn raw_file(&self, path: &str, sha: &str) -> Result<String, Error> {
        const MAX_BYTES: usize = 2 * 1024 * 1024;
        let url = format!("{}/api/v4{}", server(), self.path(&format!("/repository/files/{}/raw?ref={}", encode(path), encode(safe::hex_sha(sha)?))));
        let request = HttpRequest {
            method: "GET".into(),
            url,
            headers: vec![("Authorization".into(), authorization(&server())?)],
            body: None,
            timeout_ms: Some(callback_timeout_ms(20_000)?),
        };
        let response = host::fetch(&request)?;
        require_callback_read_budget()?;

        match response.status {
            200..=299 if response.body.len() > MAX_BYTES => Err("The file is too large for the diff viewer. Open it on GitLab.".into()),
            200..=299 => Ok(response.text()),
            404 => Ok(String::new()),
            401 => Err("GitLab rejected the access token. Check it in Settings › Plugins › GitLab.".into()),
            status => Err(Error::Failed(format!("GitLab returned {status} for the file."))),
        }
    }

    pub fn diffs(&self, iid: u64) -> Result<Vec<FileDiff>, Error> {
        paged(&self.mr(iid, "/diffs"), 100)
    }

    /// Newest first, as GitLab lists them.
    pub fn mr_commits(&self, iid: u64) -> Result<Vec<Commit>, Error> {
        paged(&self.mr(iid, "/commits"), 100)
    }

    // ponytail: the newest 20 pipelines only; page further if older ones are ever needed.
    pub fn mr_pipelines(&self, iid: u64) -> Result<Vec<Pipeline>, Error> {
        parse(rest("GET", &self.mr(iid, "/pipelines?per_page=20"), None)?)
    }

    pub fn pipeline_jobs(&self, pipeline_id: u64) -> Result<Vec<Job>, Error> {
        paged(&self.path(&format!("/pipelines/{pipeline_id}/jobs")), 100)
    }

    /// Explicit caller only; plain text, no process, headers or raw errors exposed.
    pub fn job_trace(&self,job_id:u64)->Result<String,Error> {
        if job_id==0||DEADLINE.get().is_some_and(|d|now_secs()>=d) {return Err("Job trace request budget exhausted or invalid job.".into());}
        let timeout_ms=DEADLINE.get().map(|deadline|deadline.saturating_sub(now_secs()).saturating_mul(1000).min(20_000) as u32).unwrap_or(20_000);
        if timeout_ms==0 {return Err("Job trace request budget exhausted; retry explicitly.".into());}
        let timeout_ms=callback_timeout_ms(timeout_ms)?;
        let response=host::fetch(&HttpRequest{method:"GET".into(),url:format!("{}/api/v4{}",server(),self.path(&format!("/jobs/{job_id}/trace"))),headers:vec![("Authorization".into(),authorization(&server())?)],body:None,timeout_ms:Some(timeout_ms)}).map_err(|e|match e {Error::Denied(_)=>Error::Invalid(ui_kit::denied::message("Loading the job log",&format!("net:{}",server_host()),"GitLab")),_=>Error::Failed("Job trace request failed. Retry explicitly.".into())})?;
        require_read_budget()?;
        if response.status!=200 {return Err(Error::Failed(format!("Job trace unavailable (HTTP {}). Retry explicitly or open on GitLab.",response.status)));}
        if response.body.len()>1<<20 {return Err("Job trace exceeds the 1 MiB plugin acceptance limit; host transfer cap is 8MiB.".into());}
        String::from_utf8(response.body).map_err(|_|Error::from("Job trace is not UTF8 text. Open it on GitLab."))
    }

    pub fn retry_job(&self, job_id: u64) -> Result<(), Error> {
        rest("POST", &self.path(&format!("/jobs/{job_id}/retry")), None).map(drop)
    }

    pub fn retry_pipeline(&self, id: u64) -> Result<(), Error> {
        rest("POST", &self.path(&format!("/pipelines/{id}/retry")), None).map(drop)
    }

    pub fn cancel_pipeline(&self, id: u64) -> Result<(), Error> {
        rest("POST", &self.path(&format!("/pipelines/{id}/cancel")), None).map(drop)
    }

    pub fn commit_diff(&self, sha: &str) -> Result<Vec<FileDiff>, Error> {
        paged(&self.path(&format!("/repository/commits/{}/diff", encode(safe::hex_sha(sha)?))), 100)
    }

    pub fn draft_notes(&self, iid: u64) -> Result<Vec<DraftNote>, Error> {
        parse(rest("GET", &self.mr(iid, "/draft_notes"), None)?)
    }

    /// A pending review comment: on a diff line, as a reply, or on the merge request.
    pub fn create_draft(&self, iid: u64, note: &str, position: Option<Value>, reply_to: Option<&str>) -> Result<(), Error> {
        let mut body = json!({ "note": note });
        if let Some(position) = position {
            body["position"] = position;
        }

        if let Some(discussion) = reply_to {
            body["in_reply_to_discussion_id"] = json!(safe::discussion_id(discussion)?);
        }

        rest("POST", &self.mr(iid, "/draft_notes"), Some(body)).map(drop)
    }

    pub fn publish_drafts(&self, iid: u64) -> Result<(), Error> {
        rest("POST", &self.mr(iid, "/draft_notes/bulk_publish"), None).map(drop)
    }

    /// State, label and milestone changes, oldest first.
    pub fn events(&self, iid: u64) -> Result<Vec<TimelineEvent>, Error> {
        let mut events = Vec::new();

        for kind in ["state", "label", "milestone"] {
            let list: Vec<Value> = parse(rest("GET", &self.mr(iid, &format!("/resource_{kind}_events?per_page=100")), None)?)?;
            events.extend(list.iter().map(|e| timeline_event(kind, e)));
        }

        events.sort_by(|a, b| a.created_at.cmp(&b.created_at));

        Ok(events)
    }

    pub fn approve(&self, iid: u64) -> Result<(), Error> {
        rest("POST", &self.mr(iid, "/approve"), None).map(drop)
    }

    pub fn unapprove(&self, iid: u64) -> Result<(), Error> {
        rest("POST", &self.mr(iid, "/unapprove"), None).map(drop)
    }

    pub fn merge(&self, mr: &MergeRequest, remove_source_branch: bool, squash: bool) -> Result<(), Error> {
        let body = json!({
            "sha": mr.sha,
            "should_remove_source_branch": remove_source_branch,
            "squash": squash,
        });

        rest("PUT", &self.mr(mr.iid, "/merge"), Some(body)).map(drop)
    }

    pub fn rebase(&self, iid: u64) -> Result<(), Error> {
        rest("PUT", &self.mr(iid, "/rebase"), None).map(drop)
    }

    /// Where a rebase stands; GitLab reports a conflict as `merge_error` once the background rebase has failed.
    pub fn rebase_state(&self, iid: u64) -> Result<RebaseState, Error> {
        parse(rest("GET", &self.mr(iid, "?include_rebase_in_progress=true"), None)?)
    }

    /// `close` or `reopen`.
    pub fn set_state(&self, iid: u64, event: &str) -> Result<(), Error> {
        rest("PUT", &self.mr(iid, ""), Some(json!({ "state_event": event }))).map(drop)
    }

    pub fn labels(&self) -> Result<Vec<String>, Error> {
        let labels: Vec<Label> = paged(&self.path("/labels"), 100)?;

        Ok(labels.into_iter().map(|l| l.name).collect())
    }

    pub fn members(&self) -> Result<Vec<User>, Error> {
        paged(&self.path("/members/all"), 100)
    }

    pub fn milestones(&self) -> Result<Vec<Milestone>, Error> {
        paged(&self.path("/milestones?state=active"), 100)
    }

    /// `fields` is the JSON body of the update (`{"labels": "a,b"}`).
    pub fn update_merge_request(&self, iid: u64, fields: Value) -> Result<(), Error> {
        rest("PUT", &self.mr(iid, ""), Some(fields)).map(drop)
    }

    pub fn set_reviewers(&self, iid: u64, ids: &[u64]) -> Result<(), Error> {
        rest("PUT", &self.mr(iid, ""), Some(json!({ "reviewer_ids": ids }))).map(drop)
    }

    /// A new thread, on a diff line when `position` is set.
    pub fn discuss(&self, iid: u64, body: &str, position: Option<Value>) -> Result<(), Error> {
        let mut request = json!({ "body": body });
        if let Some(position) = position {
            request["position"] = position;
        }

        rest("POST", &self.mr(iid, "/discussions"), Some(request)).map(drop)
    }

    pub fn reply(&self, iid: u64, discussion: &str, body: &str) -> Result<(), Error> {
        rest("POST", &self.mr(iid, &format!("/discussions/{}/notes", encode(safe::discussion_id(discussion)?))), Some(json!({ "body": body }))).map(drop)
    }

    pub fn resolve(&self, iid: u64, discussion: &str, resolved: bool) -> Result<(), Error> {
        let path = self.mr(iid, &format!("/discussions/{}?resolved={resolved}", encode(safe::discussion_id(discussion)?)));

        rest("PUT", &path, None).map(drop)
    }

    pub fn edit_note(&self, iid: u64, note_id: u64, body: &str) -> Result<(), Error> {
        rest("PUT", &self.mr(iid, &format!("/notes/{note_id}")), Some(json!({ "body": body }))).map(drop)
    }

    pub fn delete_note(&self, iid: u64, note_id: u64) -> Result<(), Error> {
        rest("DELETE", &self.mr(iid, &format!("/notes/{note_id}")), None).map(drop)
    }

    /// The award emoji of a note, or of the merge request itself when `note_id` is `None`.
    fn awards_path(&self, iid: u64, note_id: Option<u64>) -> String {
        match note_id {
            Some(note) => self.mr(iid, &format!("/notes/{note}/award_emoji")),
            None => self.mr(iid, "/award_emoji"),
        }
    }

    pub fn awards(&self, iid: u64, note_id: Option<u64>) -> Result<Vec<Award>, Error> {
        paged(&self.awards_path(iid, note_id), 100)
    }

    pub fn add_award(&self, iid: u64, note_id: Option<u64>, name: &str) -> Result<(), Error> {
        let path = format!("{}?name={}", self.awards_path(iid, note_id), encode(safe::emoji(name)?));

        rest("POST", &path, None).map(drop)
    }

    pub fn remove_award(&self, iid: u64, note_id: Option<u64>, award_id: u64) -> Result<(), Error> {
        rest("DELETE", &format!("{}/{award_id}", self.awards_path(iid, note_id)), None).map(drop)
    }

    /// GitLab's documented re-review mutation; variables preserve repository
    /// and reviewer identity without interpolating user-controlled GraphQL.
    pub fn rerequest_review(&self, iid: u64, reviewer: u64) -> Result<(), Error> {
        if iid == 0 || reviewer == 0 {return Err("Invalid review request identity.".into());}
        let body = json!({"query":"mutation($projectPath: ID!, $iid: String!, $userId: UserID!) { mergeRequestReviewerRereview(input: {projectPath: $projectPath, iid: $iid, userId: $userId}) { errors } }",
            "variables":{"projectPath":self.0,"iid":iid.to_string(),"userId":format!("gid://gitlab/User/{reviewer}")}});
        let response=send("POST",format!("{}/api/graphql",server()),Some(body))?;
        if response["errors"].as_array().is_some_and(|errors|!errors.is_empty()) {return Err("GitLab rejected the re-review request.".into());}
        let errors=response["data"]["mergeRequestReviewerRereview"]["errors"].as_array().ok_or("GitLab did not acknowledge the re-review request.")?;
        if !errors.is_empty() {return Err("GitLab could not re-request review. Refresh reviewers or open the merge request on GitLab.".into());}
        Ok(())
    }

    /// REST has no draft flag; the original uses this same mutation.
    pub fn set_draft(&self, iid: u64, draft: bool) -> Result<(), Error> {
        let body = json!({
            "query": "mutation($projectPath: ID!, $iid: String!, $draft: Boolean!) { \
                mergeRequestSetDraft(input: {projectPath: $projectPath, iid: $iid, draft: $draft}) { errors } }",
            "variables": { "projectPath": self.0, "iid": iid.to_string(), "draft": draft },
        });
        let response = send("POST", format!("{}/api/graphql", server()), Some(body))?;

        let errors = response["errors"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|e| e["message"].as_str())
            .chain(response["data"]["mergeRequestSetDraft"]["errors"].as_array().into_iter().flatten().filter_map(Value::as_str))
            .collect::<Vec<_>>();
        if !errors.is_empty() {
            return Err(Error::Failed(errors.join("; ")));
        }

        Ok(())
    }
}

fn timeline_event(kind: &str, event: &Value) -> TimelineEvent {
    let action = event["action"].as_str().unwrap_or("");
    let text = match kind {
        "state" => event["state"].as_str().unwrap_or("changed").to_owned(),
        "label" => format!("{} label {}", if action == "remove" { "removed" } else { "added" }, event["label"]["name"].as_str().unwrap_or("?")),
        _ => {
            let verb = if action == "remove" { "removed milestone" } else { "set milestone" };
            format!("{verb} {}", event["milestone"]["title"].as_str().unwrap_or("?"))
        }
    };

    TimelineEvent {
        created_at: event["created_at"].as_str().unwrap_or("").to_owned(),
        user: event["user"]["username"].as_str().unwrap_or("").to_owned(),
        text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn callback_read_scope_clamps_rest_reads_refuses_writes_and_restores_previous_scope() {
        let h=openidea_plugin::mock::MockHost::install().with_capabilities(&["net:gitlab.com","secrets"]);h.set_secret(TOKEN,"offline-fixture");
        h.on_fetch(|r| {if CALLBACK_DEADLINE.get().is_some(){assert!(r.timeout_ms.is_some_and(|ms|ms>0&&ms<=10_000));}else{assert_eq!(r.timeout_ms,Some(20_000));}Ok(openidea_plugin::HttpResponse{status:200,headers:vec![],body:b"{}".to_vec()})});
        {
            let _scope=CallbackReadBudget::start();assert!(send("GET","https://gitlab.com/offline".to_owned(),None).is_ok());
            let count=h.requests().len();assert!(send("POST","https://gitlab.com/offline".to_owned(),None).is_err());assert_eq!(h.requests().len(),count);
            let short=std::time::Instant::now()+std::time::Duration::from_secs(1);CALLBACK_DEADLINE.set(Some(short));
            {let _nested=CallbackReadBudget::start();assert_eq!(CALLBACK_DEADLINE.get(),Some(short));assert!(callback_timeout_ms(20_000).unwrap()<=1000);}
            assert_eq!(CALLBACK_DEADLINE.get(),Some(short));
        }
        assert!(CALLBACK_DEADLINE.get().is_none());assert!(send("GET","https://gitlab.com/offline".to_owned(),None).is_ok());
    }
    #[test]
    fn callback_expiry_refuses_late_response_and_further_reads_without_fetching() {
        let h=openidea_plugin::mock::MockHost::install().with_capabilities(&["net:gitlab.com","secrets"]);h.set_secret(TOKEN,"offline-fixture");let _scope=CallbackReadBudget::start();
        h.on_fetch(|_| {expire_callback_read_budget();Ok(openidea_plugin::HttpResponse{status:200,headers:vec![],body:b"{}".to_vec()})});
        assert!(send("GET","https://gitlab.com/offline".to_owned(),None).is_err());let count=h.requests().len();assert!(send("GET","https://gitlab.com/offline".to_owned(),None).is_err());assert_eq!(h.requests().len(),count);
    }
    #[test]
    fn trace_request_timeout_never_exceeds_remaining_shared_read_budget() {
        let h=openidea_plugin::mock::MockHost::install().with_capabilities(&["net:gitlab.com","secrets"]);
        h.set_secret(TOKEN,"offline-fixture");
        DEADLINE.set(Some(now_secs()+2));
        h.on_fetch(|request| {assert!(request.timeout_ms.is_some_and(|ms|ms>0&&ms<=2000),"{:?}",request.timeout_ms);Ok(openidea_plugin::HttpResponse{status:200,headers:vec![],body:b"trace".to_vec()})});
        assert_eq!(Project("g/p").job_trace(1).unwrap(),"trace");
    }
    #[test]
    fn trace_response_arriving_after_budget_is_not_accepted() {
        let h=openidea_plugin::mock::MockHost::install().with_capabilities(&["net:gitlab.com","secrets"]);
        h.set_secret(TOKEN,"offline-fixture");
        begin_budget();
        h.on_fetch(|_| {DEADLINE.set(Some(0));Ok(openidea_plugin::HttpResponse{status:200,headers:vec![],body:b"late trace".to_vec()})});
        assert!(Project("g/p").job_trace(1).is_err());
        let count=h.requests().len();assert!(Project("g/p").job_trace(1).is_err());assert_eq!(h.requests().len(),count);
    }
    fn self_hosted(host: &openidea_plugin::mock::MockHost, origin: &str) {
        host.set_setting("server", &format!("{origin:?}"));
        host.on_fetch(|_| Ok(openidea_plugin::HttpResponse { status: 200, headers: vec![], body: b"{}".to_vec() }));
    }

    fn fresh_host() -> openidea_plugin::mock::MockHost {
        let host = openidea_plugin::mock::MockHost::install().with_capabilities(&["net:gitlab.com", "net:git.corp.test", "net:other.corp.test", "secrets", "storage"]);
        host.set_secret(TOKEN, "glpat-x");
        host
    }

    #[test]
    fn token_not_sent_to_new_server_without_confirm() {
        let host = fresh_host();
        self_hosted(&host, "https://git.corp.test");
        let _interactive = Interactive::start();

        let error = rest("GET", "/user", None).unwrap_err();

        assert!(error.to_string().contains("Token not sent to git.corp.test"), "{error}");
        assert!(host.requests().is_empty());
    }

    #[test]
    fn confirmed_server_remembered() {
        let host = fresh_host();
        self_hosted(&host, "https://git.corp.test");
        let _interactive = Interactive::start();
        host.answer_next_prompt(Some(&[]));

        rest("GET", "/user", None).unwrap();
        rest("GET", "/user", None).unwrap();

        assert_eq!(host.requests().len(), 2);
    }

    #[test]
    fn changed_server_asks_again() {
        let host = fresh_host();
        self_hosted(&host, "https://git.corp.test");
        let _interactive = Interactive::start();
        host.answer_next_prompt(Some(&[]));
        rest("GET", "/user", None).unwrap();

        self_hosted(&host, "https://other.corp.test");

        assert!(rest("GET", "/user", None).is_err());
        assert_eq!(host.requests().len(), 1);
    }

    #[test]
    fn provider_path_does_not_prompt_or_send() {
        let host = fresh_host();
        self_hosted(&host, "https://git.corp.test");
        host.answer_next_prompt(Some(&[]));

        assert!(rest("GET", "/user", None).is_err());
        assert!(host.requests().is_empty());

        let _interactive = Interactive::start();
        rest("GET", "/user", None).unwrap();
        assert_eq!(host.requests().len(), 1);
    }

    #[test]
    fn gitlab_com_token_needs_no_confirmation() {
        let host = fresh_host();
        host.on_fetch(|_| Ok(openidea_plugin::HttpResponse { status: 200, headers: vec![], body: b"{}".to_vec() }));

        rest("GET", "/user", None).unwrap();

        assert_eq!(host.requests().len(), 1);
    }

    #[test]
    fn discussions_drop_empty_notes() {
        let host = fresh_host();
        host.on_fetch(|_| Ok(openidea_plugin::HttpResponse { status: 200, headers: vec![], body: br#"[{"id":"a","notes":[]},{"id":"b","notes":[{"id":1}]}]"#.to_vec() }));

        let discussions = Project("g/p").discussions(1).unwrap();

        assert_eq!(discussions.len(), 1);
        assert_eq!(discussions[0].id, "b");
    }

    #[test]
    fn timeline_events() {
        let label = json!({"created_at": "t1", "user": {"username": "ann"}, "action": "add", "label": {"name": "bug"}});
        let state = json!({"created_at": "t2", "user": {"username": "bob"}, "state": "merged"});

        assert_eq!(timeline_event("label", &label).text, "added label bug");
        assert_eq!(timeline_event("state", &state), TimelineEvent { created_at: "t2".into(), user: "bob".into(), text: "merged".into() });
    }

    #[test]
    fn positions_match_diff_lines() {
        let context = NotePosition { new_path: Some("b".into()), new_line: Some(4), old_path: Some("a".into()), old_line: Some(3) };
        let removed = NotePosition { old_path: Some("a".into()), old_line: Some(3), ..NotePosition::default() };

        assert!(context.is_at("a", "b", Some(3), Some(4)));
        assert!(!context.is_at("a", "b", Some(3), None));
        assert!(removed.is_at("a", "b", Some(3), None));
        assert!(!removed.is_at("a", "b", Some(3), Some(4)));
    }

    #[test]
    fn project_paths_from_remotes() {
        let _host = openidea_plugin::mock::MockHost::install();
        assert_eq!(project_path("https://gitlab.com/group/sub/name.git").as_deref(), Some("group/sub/name"));
        assert_eq!(project_path("https://user:pw@gitlab.com/group/name").as_deref(), Some("group/name"));
        assert_eq!(project_path("git@gitlab.com:group/name.git").as_deref(), Some("group/name"));
        assert_eq!(project_path("ssh://git@gitlab.com:22/group/name.git").as_deref(), Some("group/name"));
        assert_eq!(project_path("git@github.com:group/name.git"), None);
        assert_eq!(project_path("https://gitlab.com/name.git"), None);
    }

    #[test]
    fn a_configured_https_server_replaces_gitlab_com() {
        let host = openidea_plugin::mock::MockHost::install();
        host.set_setting("server", "\"https://git.corp.test:8443/\"");
        assert_eq!(server(), "https://git.corp.test:8443");
        assert_eq!(server_host(), "git.corp.test");
        assert_eq!(project_path("git@git.corp.test:group/name.git").as_deref(), Some("group/name"));
        assert_eq!(project_path("https://gitlab.com/group/name.git"), None);
        assert_eq!(blob_url("g/p", "abc", "a.rs", None), "https://git.corp.test:8443/g/p/-/blob/abc/a.rs");
        assert!(crate::safe::clone_url("https://git.corp.test:8443/g/p.git").is_ok());
        assert!(crate::safe::clone_url("git@git.corp.test:g/p.git").is_ok());
        assert!(crate::safe::clone_url("https://gitlab.com/g/p.git").is_err());
    }

    #[test]
    fn an_invalid_or_plain_http_server_falls_back_to_gitlab_com() {
        let host = openidea_plugin::mock::MockHost::install();
        for value in ["", "not a url", "http://git.corp.test", "https://user:pw@git.corp.test", "https://git.corp.test/?x=1"] {
            host.set_setting("server", &format!("{value:?}"));
            assert_eq!(server(), "https://gitlab.com", "{value:?}");
        }
    }

    #[test]
    fn blob_urls() {
        let _host = openidea_plugin::mock::MockHost::install();
        let base = "https://gitlab.com/g/p/-/blob/abc/src/a%20b.rs";
        assert_eq!(blob_url("g/p", "abc", "src/a b.rs", None), base);
        assert_eq!(blob_url("g/p", "abc", "src/a b.rs", Some((2, 2))), format!("{base}#L3"));
        assert_eq!(blob_url("g/p", "abc", "src/a b.rs", Some((2, 4))), format!("{base}#L3-L5"));
    }

    #[test]
    fn blob_url_encodes_hash_in_branch() {
        let _host = openidea_plugin::mock::MockHost::install();
        assert_eq!(blob_url("g/p", "fix/a#b?c", "a.rs", None), "https://gitlab.com/g/p/-/blob/fix/a%23b%3Fc/a.rs");
        assert_eq!(tree_url("g/p", "fix/a#b", "src"), "https://gitlab.com/g/p/-/tree/fix/a%23b/src");
    }

    #[test]
    fn error_messages() {
        assert_eq!(error_message(r#"{"message":"405 Method Not Allowed"}"#), "405 Method Not Allowed");
        assert_eq!(error_message(r#"{"message":{"base":["nope"]}}"#), r#"{"base":["nope"]}"#);
        assert_eq!(error_message("plain"), "plain");
    }
}
