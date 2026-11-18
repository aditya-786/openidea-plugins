//! Issue trackers. Each is used once the user has entered its token.

use std::collections::BTreeMap;

use openidea_plugin::{Error, HttpRequest, host};
use serde_json::{Value, json};

use crate::model::{Task,SourceReference};

const LIMIT: u32 = 50;

type Remotes = [(String, String)];

/// What a fetch looks at: `query` turns the listing into a search, `repositories` (by tracker id) override the settings.
pub struct Scope<'a> {
    pub remotes: &'a Remotes,
    pub query: Option<&'a str>,
    pub page: u32,
    pub repositories: &'a BTreeMap<String, Vec<String>>,
}

pub struct Tracker {
    pub id: &'static str,
    pub name: &'static str,
    secrets: &'static [&'static str],
    headers: &'static [(&'static str, &'static str)],
    fetch: fn(&Tracker, &Scope) -> Result<Vec<Task>, Error>,
}

const TRACKERS: [Tracker; 11] = [
    Tracker { id: "redmine", name: "Redmine (local credential bridge)", secrets: &[], headers: &[], fetch: |_,scope| crate::self_hosted::search(scope,"redmine") },
    Tracker { id: "mantis", name: "MantisBT (local credential bridge)", secrets: &[], headers: &[], fetch: |_,scope| crate::self_hosted::search(scope,"mantis") },
    Tracker { id: "generic", name: "Generic REST/regex (local credential bridge)", secrets: &[], headers: &[], fetch: |_,scope| crate::self_hosted::search(scope,"generic") },
    Tracker { id: "fogbugz", name: "FogBugz (local credential bridge)", secrets: &[], headers: &[], fetch: |_,scope| crate::fogbugz::search(scope) },
    Tracker { id: "lighthouse", name: "Lighthouse", secrets: &["lighthouse-token"], headers: &[("X-LighthouseToken", "{{secret:lighthouse-token}}")], fetch: lighthouse },
    Tracker {
        id: "github",
        name: "GitHub",
        secrets: &["github-token"],
        headers: &[
            ("Authorization", "Bearer {{secret:github-token}}"),
            ("Accept", "application/vnd.github+json"),
            ("User-Agent", "openidea-tasks"),
        ],
        fetch: github,
    },
    Tracker {
        id: "gitlab",
        name: "GitLab",
        secrets: &["gitlab-token"],
        headers: &[("PRIVATE-TOKEN", "{{secret:gitlab-token}}")],
        fetch: gitlab,
    },
    Tracker {
        id: "forgejo",
        name: "Codeberg",
        secrets: &["forgejo-token"],
        headers: &[("Authorization", "Bearer {{secret:forgejo-token}}"), ("Accept", "application/json")],
        fetch: forgejo,
    },
    Tracker {
        id: "youtrack",
        name: "YouTrack",
        secrets: &["youtrack-token"],
        headers: &[("Authorization", "Bearer {{secret:youtrack-token}}"), ("Accept", "application/json")],
        fetch: youtrack,
    },
    Tracker {
        id: "jira",
        name: "Jira",
        secrets: &["jira-token"],
        headers: &[("Authorization", "Basic {{secret:jira-token}}"), ("Accept", "application/json")],
        fetch: jira,
    },
    Tracker {
        id: "trello",
        name: "Trello",
        secrets: &["trello-key", "trello-token"],
        headers: &[("Authorization", "OAuth oauth_consumer_key=\"{{secret:trello-key}}\", oauth_token=\"{{secret:trello-token}}\"")],
        fetch: trello,
    },
];

#[derive(Debug, Default)]
pub struct Fetched {
    pub issues: Vec<Task>,
    pub errors: Vec<String>,
    /// Errors that are a denied permission.
    pub denied: usize,
    pub trackers: usize,
    pub failed_trackers: Vec<String>,
}

/// Open issues (or the search hits) of every configured tracker; one failing doesn't stop the others.
pub fn fetch_all(scope: &Scope) -> Fetched {
    let mut fetched = Fetched::default();

    for tracker in &TRACKERS {
        if !updates_automatically(tracker) {
            continue;
        }

        fetched.trackers += 1;

        match (tracker.fetch)(tracker, scope) {
            Ok(issues) => fetched.issues.extend(issues),
            Err(e) => {
                fetched.denied += usize::from(matches!(e, Error::Denied(_)));
                fetched.errors.push(format!("{}: {e}", tracker.name));
                fetched.failed_trackers.push(tracker.id.to_owned());
            }
        }
    }

    fetched
}

/// A fetched issue is the stored task's issue; GitHub tasks stored before ids
/// carried the owner (`repo-5`) still match their `owner/repo-5` issue.
pub fn same_issue(issue: &Task, task: &Task) -> bool {
    if issue.id == task.id {
        return true;
    }

    issue.tracker.as_deref() == Some("github")
        && issue.api == task.api
        && issue.id.split_once('/').is_some_and(|(_, legacy)| legacy == task.id)
}

/// Approved external credential bridges run only inside explicit Search Issues.
fn updates_automatically(tracker: &Tracker) -> bool {
    !matches!(tracker.id, "fogbugz" | "redmine" | "mantis" | "generic") && tracker.secrets.iter().all(|s| host::secret_is_set(s))
}

/// Whether some tracker has the credentials Update Issues needs.
pub fn any_configured() -> bool {
    TRACKERS.iter().any(updates_automatically)
}

pub fn is_repository_tracker(id: &str) -> bool {
    matches!(id, "github" | "gitlab" | "forgejo")
}

pub fn tracker_name(id: &str) -> &str {
    TRACKERS.iter().find(|t| t.id == id).map_or(id, |t| t.name)
}

pub fn name(task: &Task) -> &str {
    let id = task.tracker.as_deref().unwrap_or_default();

    TRACKERS.iter().find(|t| t.id == id).map_or(id, |t| t.name)
}

/// The Close Task form offers "Update issue state" for these.
pub fn can_update_state(task: &Task) -> bool {
    if matches!(task.tracker.as_deref(),Some("redmine"|"mantis"|"generic")) {return crate::self_hosted::identity(task) && task.self_hosted.as_ref().is_some_and(|p|p.can_close) && !task.closed;}
    (task.tracker.as_deref()==Some("fogbugz") && crate::fogbugz::identity(task) && task.fogbugz.as_ref().is_some_and(|p| p.can_close) && !task.closed) || matches!(task.tracker.as_deref(), Some("github" | "gitlab" | "forgejo" | "youtrack" | "jira" | "lighthouse"))
}

pub fn can_post_time(task: &Task) -> bool {
    matches!(task.tracker.as_deref(), Some("youtrack" | "jira"))
}

/// Sets the task state with the tracker's resolved state.
pub fn close_issue(task: &Task) -> Result<(), Error> {
    if matches!(task.tracker.as_deref(),Some("redmine"|"mantis"|"generic")) {return crate::self_hosted::close(task);}
    if task.tracker.as_deref()==Some("fogbugz") { return crate::fogbugz::close(task); }
    let (tracker, api) = issue_api(task)?;

    match tracker.id {
        "lighthouse" => {
            let site=site("lighthouseHost","Lighthouse")?;
            let project=setting("lighthouseProject").and_then(|s|s.parse::<u64>().ok()).filter(|p|*p>0).ok_or("Configure Lighthouse project ID.")?;
            let number=api.strip_prefix(&format!("https://{site}/projects/{project}/tickets/")).and_then(|s|s.strip_suffix(".xml")).and_then(|s|s.parse::<u64>().ok()).filter(|n|*n>0).ok_or("Invalid Lighthouse ticket number.")?;
            if !cloud_host(&site,".lighthouseapp.com") || task.id!=lighthouse_id(&site,project,number) || api!=format!("https://{site}/projects/{project}/tickets/{number}.xml") || task.url.as_deref()!=Some(format!("https://{site}/projects/{project}/tickets/{number}").as_str()) {return Err(Error::Stale);}
            lighthouse_request(tracker,"PUT",api,Some("<ticket><state>resolved</state></ticket>")).map(drop)
        }
        "github" | "forgejo" => send(tracker, "PATCH", api, Some(json!({ "state": "closed" }))).map(drop),
        "gitlab" => send(tracker, "PUT", &format!("{api}?state_event=close"), None).map(drop),
        "youtrack" => {
            let state = setting("youtrackCloseState").unwrap_or_else(|| "Fixed".into());
            let commands = format!("{}/api/commands", origin(api));
            let command = json!({ "query": format!("state {state}"), "issues": [{ "idReadable": task.id }] });

            send(tracker, "POST", &commands, Some(command)).map(drop)
        }
        "jira" => {
            let transitions = send(tracker, "GET", &format!("{api}/transitions"), None)?;
            let done = items(&transitions["transitions"], |t| {
                (t["to"]["statusCategory"]["key"] == "done").then(|| t["id"].as_str().map(str::to_owned)).flatten()
            });
            let id = done.first().ok_or("None of this issue's transitions leads to a done status.")?;

            send(tracker, "POST", &format!("{api}/transitions"), Some(json!({ "transition": { "id": id } }))).map(drop)
        }
        _ => Err(format!("{} issues can't be closed from the IDE.", tracker.name).into()),
    }
}

/// YouTrackRepository.updateTimeSpent / JiraRestApi.updateTimeSpend.
pub fn post_time(task: &Task, seconds: u64) -> Result<(), Error> {
    let (tracker, api) = issue_api(task)?;

    match tracker.id {
        "youtrack" => {
            let item = json!({ "usesMarkdown": true, "text": "", "duration": { "minutes": seconds.div_ceil(60) } });
            send(tracker, "POST", &format!("{api}/timeTracking/workItems"), Some(item)).map(drop)
        }
        "jira" => send(tracker, "POST", &format!("{api}/worklog"), Some(json!({ "timeSpentSeconds": seconds.max(60) }))).map(drop),
        _ => Err(format!("Posting work time to {} isn't supported.", tracker.name).into()),
    }
}

fn issue_api(task: &Task) -> Result<(&'static Tracker, &str), Error> {
    let tracker = TRACKERS.iter().find(|t| Some(t.id) == task.tracker.as_deref()).ok_or("The task isn't a tracker issue.")?;
    let api = task.api.as_deref().ok_or("The issue has no API address. Update issues and try again.")?;

    let expected = match tracker.id {
        "youtrack" => Some(format!("https://{}", site("youtrackHost", "YouTrack")?)),
        "jira" => Some(format!("https://{}", site("jiraHost", "Jira")?)),
        _ => expected_origin(tracker),
    };

    if expected.as_deref() != Some(origin(api)) {
        return Err(format!("The issue's API address isn't on the configured {} server. Update issues and try again.", tracker.name).into());
    }

    Ok((tracker, api))
}

/// Where a tracker's credentials may be sent; `None` when the tracker has no such server.
pub fn expected_origin(tracker: &Tracker) -> Option<String> {
    match tracker.id {
        "github" => Some("https://api.github.com".into()),
        "gitlab" => Some("https://gitlab.com".into()),
        "forgejo" => Some("https://codeberg.org".into()),
        "trello" => Some("https://api.trello.com".into()),
        "youtrack" => site("youtrackHost", "YouTrack").ok().map(|s| format!("https://{s}")),
        "jira" => site("jiraHost", "Jira").ok().map(|s| format!("https://{s}")),
        "lighthouse" => site("lighthouseHost", "Lighthouse").ok().map(|s| format!("https://{s}")),
        _ => None,
    }
}

/// `owner/name`: exactly two segments of `[A-Za-z0-9._-]+`, neither `.` nor `..`.
pub fn valid_repo(s: &str) -> bool {
    let parts: Vec<&str> = s.split('/').collect();

    parts.len() == 2
        && parts.iter().all(|p| !p.is_empty() && !matches!(*p, "." | "..") && p.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)))
}

/// The repositories added in the Tasks view, else the setting, else the remote's.
fn repositories(scope: &Scope, tracker: &str, setting_key: &str, remote_host: &str) -> Vec<String> {
    let added = scope.repositories.get(tracker).cloned().unwrap_or_default();

    if !added.is_empty() {
        return added;
    }

    setting(setting_key).or_else(|| remote_path(scope.remotes, remote_host)).into_iter().collect()
}

// Closed tracker hits are opt-in for explicit search only; periodic listings stay open-only.
fn include_closed(scope: &Scope) -> bool {
    scope.query.is_some() && host::setting_bool("searchClosedIssues") == Some(true)
}

fn github(tracker: &Tracker, scope: &Scope) -> Result<Vec<Task>, Error> {
    let repos = repositories(scope, "github", "githubRepository", "github.com");

    if repos.is_empty() {
        return Err("No github.com remote. Set the repository in Settings › Plugins › Tasks.".into());
    }

    if let Some(bad) = repos.iter().find(|r| !valid_repo(r)) {
        return Err(format!("\"{bad}\" isn't a GitHub repository. Use owner/name.").into());
    }

    let mut query = format!("{} is:issue{}", repos.iter().map(|r| format!("repo:{r}")).collect::<Vec<_>>().join(" "),
        if include_closed(scope) { "" } else { " is:open" });

    if host::setting_bool("githubAssignedOnly") == Some(true) {
        query.push_str(" assignee:@me");
    }

    if let Some(extra) = setting("githubQuery") {
        query = format!("{query} {extra}");
    }

    if let Some(text) = scope.query {
        query = format!("{query} {text}");
    }

    let url = format!("https://api.github.com/search/issues?q={}&sort=updated&per_page={LIMIT}&page={}", encode(&query), scope.page);
    let several = repos.len() > 1;

    Ok(issue_items(&send(tracker, "GET", &url, None)?["items"], |issue| {
        let mut segments = issue["repository_url"].as_str().map(|u| u.rsplit('/')).into_iter().flatten();
        let repo_name = segments.next();
        let owner = segments.next();
        let name = match (several, owner, repo_name) {
            (true, Some(owner), Some(repo)) => format!("{owner}/{repo}"),
            (_, _, Some(repo)) => repo.to_owned(),
            _ => repos[0].rsplit('/').next().unwrap_or(&repos[0]).to_owned(),
        };

        Some(Task::issue(
            "github",
            &format!("{name}-{}", issue["number"].as_u64()?),
            issue["title"].as_str()?,
            issue["html_url"].as_str()?,
            issue["url"].as_str()?,
        ))
    }))
}

fn gitlab(tracker: &Tracker, scope: &Scope) -> Result<Vec<Task>, Error> {
    let projects = repositories(scope, "gitlab", "gitlabProject", "gitlab.com");
    let search = scope.query.map(|q| format!("&search={}", encode(q))).unwrap_or_default();
    let bases = match projects.is_empty() {
        true => vec!["https://gitlab.com/api/v4/issues".to_owned()],
        false => projects.iter().map(|p| format!("https://gitlab.com/api/v4/projects/{}/issues", encode(p))).collect(),
    };
    let single = projects.len() == 1;
    let mut found = Vec::new();

    for base in bases {
        let state = if include_closed(scope) { "all" } else { "opened" };
        let url = format!("{base}?state={state}&order_by=updated_at&per_page={LIMIT}{search}&page={}", scope.page);

        found.extend(issue_items(&send(tracker, "GET", &url, None)?, |issue| {
            let id = match single {
                true => format!("#{}", issue["iid"].as_u64()?),
                false => issue["references"]["full"].as_str()?.to_owned(),
            };

            let mut task=Task::issue("gitlab", &id, issue["title"].as_str()?, issue["web_url"].as_str()?, issue["_links"]["self"].as_str()?);
            if let (Some(full),Some(project_id),Some(iid))=(issue["references"]["full"].as_str(),issue["project_id"].as_u64(),issue["iid"].as_u64()) {
                if let Some((path,suffix))=full.rsplit_once('#') {
                    if path.len()<=100 && path.split('/').count()>=2 && path.split('/').all(|p|!p.is_empty()&&!matches!(p,"."|"..")&&p.bytes().all(|b|b.is_ascii_alphanumeric()||b"_-.".contains(&b))) && suffix==iid.to_string() && project_id>0 && iid>0
                        && task.api.as_deref()==Some(format!("https://gitlab.com/api/v4/projects/{project_id}/issues/{iid}").as_str())
                        && task.url.as_deref()==Some(format!("https://gitlab.com/{path}/-/issues/{iid}").as_str()) {
                        task.source_reference=Some(SourceReference::GitLab{project_path:path.into(),project_id,iid});
                    }
                }
            }
            Some(task)
        }));
    }

    Ok(found)
}

fn forgejo(tracker: &Tracker, scope: &Scope) -> Result<Vec<Task>, Error> {
    let repos = repositories(scope, "forgejo", "forgejoRepository", "codeberg.org");

    if repos.is_empty() {
        return Err("No codeberg.org remote. Set the repository in Settings › Plugins › Tasks.".into());
    }

    if let Some(bad) = repos.iter().find(|r| !valid_repo(r)) {
        return Err(format!("\"{bad}\" isn't a Codeberg repository. Use owner/name.").into());
    }

    let search = scope.query.map(|q| format!("&q={}", encode(q))).unwrap_or_default();
    let mut found = Vec::new();

    for repo in &repos {
        let state = if include_closed(scope) { "all" } else { "open" };
        let url = format!(
            "https://codeberg.org/api/v1/repos/{repo}/issues?type=issues&state={state}&sort=updated&limit={LIMIT}{search}&page={}",
            scope.page
        );
        let prefix = if repos.len() > 1 { repo.as_str() } else { "" };

        found.extend(issue_items(&send(tracker, "GET", &url, None)?, |issue| {
            Some(Task::issue(
                "forgejo",
                &format!("{prefix}#{}", issue["number"].as_u64()?),
                issue["title"].as_str()?,
                issue["html_url"].as_str()?,
                issue["url"].as_str()?,
            ))
        }));
    }

    Ok(found)
}

fn youtrack(tracker: &Tracker, scope: &Scope) -> Result<Vec<Task>, Error> {
    let site = site("youtrackHost", "YouTrack")?;
    let query = scope.query.map(str::to_owned).or_else(|| setting("youtrackQuery")).unwrap_or_default();
    let url = format!(
        "https://{site}/api/issues?query={}&fields=idReadable,summary,description&$top={LIMIT}&$skip={}",
        encode(&query),
        (scope.page.saturating_sub(1)) * LIMIT
    );

    Ok(issue_items(&send(tracker, "GET", &url, None)?, |issue| {
        let id = issue["idReadable"].as_str()?;

        Some(Task::issue(
            "youtrack",
            id,
            issue["summary"].as_str()?,
            &format!("https://{site}/issue/{}", encode(id)),
            &format!("https://{site}/api/issues/{}", encode(id)),
        ))
    }))
}

/// Plain text becomes a quoted `text ~` search; `jql: <query>` is sent as
/// owned JQL after a local shape check only (the server still decides validity).
pub fn jira_search_jql(input: &str) -> Result<String, Error> {
    let input = input.trim();
    let raw = input
        .get(..4)
        .filter(|prefix| prefix.eq_ignore_ascii_case("jql:"))
        .map(|_| input[4..].trim());
    let Some(jql) = raw else {
        return Ok(format!(
            "text ~ \"{}\" order by updated DESC",
            input.replace('\\', "\\\\").replace('"', "\\\"")
        ));
    };
    let refuse = |message: &str| Err(Error::Invalid(format!("Invalid JQL: {message}")));
    if jql.is_empty() {
        return refuse("the query after `jql:` is empty.");
    }
    if jql.len() > 256 {
        return refuse("use at most 256 bytes.");
    }
    if jql.chars().any(char::is_control) {
        return refuse("control characters are not allowed.");
    }
    let (mut quote, mut escaped, mut depth) = (None::<char>, false, 0i32);
    let mut unquoted = String::new();
    for c in jql.chars() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            unquoted.push('q');
            continue;
        }
        match c {
            '"' | '\'' => {
                quote = Some(c);
                unquoted.push('q');
                continue;
            }
            '(' => depth += 1,
            ')' => depth -= 1,
            _ => {}
        }
        if depth < 0 {
            return refuse("unmatched `)`.");
        }
        unquoted.push(c);
    }
    if quote.is_some() {
        return refuse("unterminated quoted string.");
    }
    if depth != 0 {
        return refuse("unmatched `(`.");
    }
    let lower = unquoted.to_ascii_lowercase();
    let words: Vec<&str> = lower.split_whitespace().collect();
    let Some(order) = words.windows(2).position(|w| w == ["order", "by"]) else {
        return match words.last() {
            Some(&("and" | "or" | "not")) => refuse("query ends with a dangling operator."),
            _ => Ok(jql.into()),
        };
    };
    if order == 0 {
        return Ok(jql.into());
    }
    if words[order + 2..].windows(2).any(|w| w == ["order", "by"]) {
        return refuse("ORDER BY may appear only once.");
    }
    if matches!(words[order - 1], "and" | "or" | "not") {
        return refuse("ORDER BY follows a dangling operator.");
    }
    let clause = words[order + 2..].join(" ");
    if clause.is_empty() {
        return refuse("ORDER BY needs at least one field.");
    }
    for field in clause.split(',') {
        let parts: Vec<&str> = field.split_whitespace().collect();
        let shaped = matches!(parts.as_slice(), [_] | [_, "asc" | "desc"]);
        if !shaped || parts[0].contains(['(', ')']) {
            return refuse("ORDER BY must be `field [ASC|DESC], ...` and come last.");
        }
    }
    Ok(jql.into())
}

fn jira(tracker: &Tracker, scope: &Scope) -> Result<Vec<Task>, Error> {
    Ok(jira_page(tracker, scope, None)?.issues)
}
fn jira_page(tracker: &Tracker, scope: &Scope, cursor: Option<&str>) -> Result<SearchPage, Error> {
    if cursor.is_some_and(|c| c.is_empty() || c.len() > 4096 || c.chars().any(char::is_control)) {
        return Err("Invalid Jira continuation token.".into());
    }
    let site = site("jiraHost", "Jira")?;
    let jql = match scope.query {
        Some(text) => jira_search_jql(text)?,
        None => setting("jiraQuery").unwrap_or_default(),
    };
    let mut url = format!(
        "https://{site}/rest/api/3/search/jql?jql={}&maxResults={LIMIT}&fields=summary,description",
        encode(&jql)
    );

    if let Some(cursor) = cursor {
        url.push_str(&format!("&nextPageToken={}", encode(cursor)));
    }
    let response = send(tracker, "GET", &url, None).map_err(|error|match error {Error::Denied(_)=>Error::Denied("Jira search permission denied.".into()),_=>Error::Failed("Jira search failed. Retry the page or start a new query.".into())})?;
    let raw = response["issues"]
        .as_array()
        .ok_or("Jira search response lacks an issues array.")?;
    if raw.len() > LIMIT as usize {
        return Err("Jira returned more than the requested page limit.".into());
    }
    let next = match response.get("nextPageToken") {
        None | Some(Value::Null) => None,
        Some(Value::String(token))
            if !token.is_empty() && token.len() <= 4096 && !token.chars().any(char::is_control) =>
        {
            Some(token.clone())
        }
        _ => return Err("Invalid Jira continuation token.".into()),
    };
    if next.as_deref().is_some_and(|n| Some(n) == cursor) {
        return Err("Jira repeated its continuation token.".into());
    }
    if response.get("isLast").is_some_and(|v| !v.is_boolean())
        || (response["isLast"] == true && next.is_some())
        || (response["isLast"] == false && next.is_none())
    {
        return Err("Inconsistent Jira pagination metadata.".into());
    }
    let issues = issue_items(&response["issues"], |issue| {
        let key = issue["key"].as_str()?;

        Some(Task::issue(
            "jira",
            key,
            issue["fields"]["summary"].as_str()?,
            &format!("https://{site}/browse/{}", encode(key)),
            &format!("https://{site}/rest/api/3/issue/{}", encode(key)),
        ))
    });
    if issues.len() != raw.len() {
        return Err("Jira returned an incomplete issue record.".into());
    }
    Ok(SearchPage { issues, next })
}

/// The cards the user is a member of (TrelloRepository without a board), or the cards matching a search.
fn trello(tracker: &Tracker, scope: &Scope) -> Result<Vec<Task>, Error> {
    let (url, cards_at) = match scope.query {
        Some(text) => (
            format!("https://api.trello.com/1/search?query={}&modelTypes=cards&cards_limit={LIMIT}&cards_page={}&card_fields=name,url,shortLink,desc", encode(text),scope.page.saturating_sub(1)),
            "cards",
        ),
        None => ("https://api.trello.com/1/members/me/cards?filter=visible&fields=name,url,shortLink,desc".to_owned(), ""),
    };
    let response = send(tracker, "GET", &url, None)?;
    let cards = if cards_at.is_empty() { &response } else { &response[cards_at] };

    let raw=cards.as_array().ok_or("Trello response lacks a cards array.")?;
    if scope.query.is_some() && raw.len()>LIMIT as usize {return Err("Trello returned more than the requested page limit.".into());}
    let parsed=issue_items(cards, |card| {
        let id = card["id"].as_str()?;

        let mut task=Task::issue("trello",id,card["name"].as_str()?,card["url"].as_str()?,&format!("https://api.trello.com/1/cards/{id}"));
        if let Some(link)=card["shortLink"].as_str().filter(|s|s.len()==8&&s.bytes().all(|b|b.is_ascii_alphanumeric())) {
            let canonical=format!("https://trello.com/c/{link}");
            if id.len()==24 && id.bytes().all(|b|b.is_ascii_hexdigit()) && task.url.as_deref().is_some_and(|u|u==canonical||u.strip_prefix(&format!("{canonical}/")).is_some_and(|tail|!tail.is_empty()&&!tail.contains(['?','#','\\'])&&!tail.chars().any(char::is_control))) {
                task.url=Some(canonical);task.source_reference=Some(SourceReference::Trello{short_link:link.into()});
            }
        }
        Some(task)
    });
    if parsed.len()!=raw.len() {return Err("Trello returned an incomplete card record.".into());}
    Ok(parsed)
}

fn send(tracker: &Tracker, method: &str, url: &str, body: Option<Value>) -> Result<Value, Error> {
    let mut request = HttpRequest { method: method.into(), url: url.into(), ..HttpRequest::default() };

    for (name, value) in tracker.headers {
        request = request.header(name, value);
    }

    if let Some(body) = body {
        request = request.header("Content-Type", "application/json");
        request.body = Some(body.to_string().into_bytes());
    }

    let response = host::fetch(&request)?;

    if response.body.len() > 1 << 20 {
        return Err("Tracker response exceeds the 1 MiB limit.".into());
    }
    if response.status >= 400 {
        return Err(format!("HTTP {}: {}", response.status, response.text().chars().take(200).collect::<String>()).into());
    }

    if response.body.is_empty() {
        return Ok(Value::Null);
    }

    serde_json::from_slice(&response.body).map_err(|e| format!("Unexpected response: {e}.").into())
}

// Issue descriptions are displayed only as plaintext; never execute tracker HTML/Markdown.
fn issue_items(list: &Value, item: impl Fn(&Value) -> Option<Task>) -> Vec<Task> {
    items(list, |raw| {
        let mut task = item(raw)?;
        let description = match task.tracker.as_deref() {
            Some("github" | "forgejo") => &raw["body"],
            Some("gitlab" | "youtrack") => &raw["description"],
            Some("jira") => &raw["fields"]["description"],
            Some("trello") => &raw["desc"],
            _ => &Value::Null,
        };
        task.description = description_text(description);
        Some(task)
    })
}
fn description_text(value: &Value) -> String {
    const LIMIT: usize = 32 * 1024 - 64;
    fn append(out: &mut String, text: &str) {
        for c in text.chars().filter(|c| *c == '\n' || *c == '\t' || !c.is_control()) {
            if out.len() + c.len_utf8() > LIMIT { break; }
            out.push(c);
        }
    }
    fn visit(value: &Value, out: &mut String, depth: usize, budget: &mut usize) {
        if depth > 32 { *budget = 0; return; }
        if *budget == 0 || out.len() >= LIMIT { return; }
        *budget -= 1;
        match value["type"].as_str() {
            Some("text") => if let Some(text) = value["text"].as_str() { append(out, text); },
            Some("hardBreak") => append(out, "\n"),
            _ => {},
        }
        if let Some(children) = value["content"].as_array() {
            for child in children { visit(child, out, depth+1, budget); }
        }
        if matches!(value["type"].as_str(), Some("paragraph" | "heading" | "listItem" | "codeBlock")) && !out.ends_with('\n') { append(out, "\n"); }
    }
    let mut out = String::new();
    let mut budget = 4096;
    if let Some(text) = value.as_str() { append(&mut out, text); }
    else if value["type"] == "doc" { visit(value, &mut out, 0, &mut budget); }
    if out.len() >= LIMIT || budget == 0 || value.as_str().is_some_and(|s| s.len() > LIMIT) { out.push_str("\n[Description truncated]"); }
    out.trim().to_owned()
}

fn items<T>(list: &Value, item: impl Fn(&Value) -> Option<T>) -> Vec<T> {
    list.as_array().into_iter().flatten().filter_map(item).collect()
}

fn setting(key: &str) -> Option<String> {
    host::setting_string(key).map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
}

/// A host setting, without the `https://` or path users paste along.
fn site(key: &str, tracker: &str) -> Result<String, Error> {
    let value = setting(key).ok_or_else(|| format!("Set the {tracker} host in Settings › Plugins › Tasks."))?;
    let value = value.split_once("://").map_or(value.as_str(), |(_, rest)| rest);

    let host = value.split('/').next().unwrap_or_default().to_ascii_lowercase();
    let allowed = match key {
        "youtrackHost" => cloud_host(&host, ".youtrack.cloud") || self_hosted_host(&host),
        "jiraHost" => cloud_host(&host, ".atlassian.net"),
        _ => true,
    };

    if !allowed {
        let label = if key == "jiraHost" { "Jira Cloud host" } else { "YouTrack host" };

        return Err(format!("The \"{label}\" setting isn't a valid {tracker} host. Fix it in Settings › Plugins › Tasks.").into());
    }

    Ok(host)
}

/// `https://host` of an API address.
fn origin(url: &str) -> &str {
    let host_end = url.find("://").map_or(0, |i| i + 3);

    url[host_end..].find('/').map_or(url, |i| &url[..host_end + i])
}

/// `owner/name` of the first remote on `host`, `origin` first.
fn remote_path(remotes: &Remotes, host: &str) -> Option<String> {
    let mut found: Vec<_> = remotes.iter().filter_map(|(name, url)| Some((name != "origin", path_on(url, host)?))).collect();
    found.sort();

    found.into_iter().next().map(|(_, path)| path)
}

fn path_on(url: &str, host: &str) -> Option<String> {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let rest = rest.rsplit_once('@').map_or(rest, |(_, rest)| rest);
    let path = rest.strip_prefix(host)?.strip_prefix([':', '/'])?;
    let path = path.trim_end_matches('/').trim_end_matches(".git");

    path.contains('/').then(|| path.to_owned())
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// A concrete server/repository selection. Search never fans a page out to
/// every repository or follows a server-supplied pagination URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchTarget {
    pub tracker: &'static str,
    pub server: String,
    pub repository: Option<String>,
    pub label: String,
}
impl SearchTarget {
    pub fn paginated(&self) -> bool {
        matches!(
            self.tracker,
            "github" | "gitlab" | "forgejo" | "youtrack" | "jira" | "trello" | "lighthouse" | "redmine" | "mantis" | "generic"
        )
    }
}
pub fn search_targets(scope: &Scope) -> Result<Vec<SearchTarget>, Error> {
    let mut targets = Vec::new();
    let bridge_profiles=crate::self_hosted::profiles()?;
    for tracker in &TRACKERS {
        if matches!(tracker.id,"redmine"|"mantis"|"generic") {
            for profile in bridge_profiles.iter().filter(|p|p.family==tracker.id) {
                targets.push(SearchTarget {tracker:tracker.id,server:crate::self_hosted::server(&profile.base_url).ok_or(Error::Stale)?.into(),repository:Some(profile.base_url.clone()),label:format!("{} · {}",tracker.name,profile.base_url)});
                if targets.len()>16 {return Err("Issue search supports at most 16 server/profile selections.".into());}
            }
            continue;
        }
        if !tracker.secrets.iter().all(|secret| host::secret_is_set(secret)) {
            continue;
        }
        let (server, repos) = match tracker.id {
            "fogbugz" => { let Some(server)=crate::fogbugz::configured_host() else {continue;}; (server,Vec::new()) },
            "github" => ("api.github.com".into(), repositories(scope, "github", "githubRepository", "github.com")),
            "gitlab" => ("gitlab.com".into(), repositories(scope, "gitlab", "gitlabProject", "gitlab.com")),
            "forgejo" => ("codeberg.org".into(), repositories(scope, "forgejo", "forgejoRepository", "codeberg.org")),
            "youtrack" => {
                let Ok(server) = site("youtrackHost", "YouTrack") else {
                    continue;
                };
                (server, Vec::new())
            }
            "jira" => {
                let Ok(server) = site("jiraHost", "Jira") else {
                    continue;
                };
                (server, Vec::new())
            }
            "lighthouse" => {
                let Ok(server)=site("lighthouseHost","Lighthouse") else {continue;};
                let Ok(project)=lighthouse_project(scope) else {continue;};
                (server,vec![project.to_string()])
            }
            "trello" => ("api.trello.com".into(), Vec::new()),
            _ => continue,
        };
        let allowed = match tracker.id {
            "youtrack" => cloud_host(&server, ".youtrack.cloud") || self_hosted_host(&server),
            "jira" => cloud_host(&server, ".atlassian.net"),
            "lighthouse" => cloud_host(&server,".lighthouseapp.com"),
            _ => true,
        };
        if !allowed {
            continue;
        }
        let repos: Vec<Option<String>> = if repos.is_empty() { vec![None] } else { repos.into_iter().map(Some).collect() };
        for repository in repos {
            if let Some(repo) = &repository {
                if repo.len() > 256
                    || repo.split('/').any(|part| {
                        part.is_empty()
                            || part == "."
                            || part == ".."
                            || !part.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
                    })
                {
                    return Err("Invalid tracker repository path.".into());
                }
                if matches!(tracker.id, "github" | "forgejo") && repo.split('/').count() != 2 {
                    return Err("GitHub/Codeberg repositories use owner/name.".into());
                }
            }
            let label =
                format!("{} · {}{}", tracker.name, server, repository.as_ref().map(|repo| format!(" / {repo}")).unwrap_or_default());
            targets.push(SearchTarget { tracker: tracker.id, server: server.clone(), repository, label });
            if targets.len() > 16 {
                return Err("Issue search supports at most 16 configured server/repository selections.".into());
            }
        }
    }
    Ok(targets)
}
/// A bare DNS name (no userinfo, port or path). Reaching it still needs the user's `net:ask` approval.
fn self_hosted_host(host: &str) -> bool {
    host.len() <= 253
        && host.contains('.')
        && host.split('.').all(|l| !l.is_empty() && !l.starts_with('-') && !l.ends_with('-') && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
}
fn cloud_host(host: &str, suffix: &str) -> bool {
    host.strip_suffix(suffix)
        .is_some_and(|tenant| !tenant.is_empty() && tenant.len() <= 128 && tenant.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
}
/// Exact relevant configuration, without retaining authentication values or
/// credential-bearing remote URLs. Presence of a secret is not its identity.
pub fn search_signature(scope: &Scope) -> Result<String, Error> {
    let targets = search_targets(scope)?;
    let settings = ["githubQuery", "youtrackQuery", "jiraQuery", "fogbugzProfile"].map(|key| setting(key).unwrap_or_default());
    if settings.iter().any(|value| value.len() > 4096) {
        return Err("Tracker query setting exceeds 4096 bytes.".into());
    }
    let identities: Vec<_> = targets.iter().map(|target| (target.tracker, &target.server, &target.repository)).collect();
    Ok(json!([identities, settings, host::setting_bool("githubAssignedOnly"), host::setting_bool("searchClosedIssues"), crate::self_hosted::profiles()?]).to_string())
}
#[derive(Clone)]
pub struct SearchPage {
    pub issues: Vec<Task>,
    pub next: Option<String>,
}
#[cfg(test)]
pub fn search_page(scope: &Scope, target: &SearchTarget, page: u32) -> Result<Vec<Task>, Error> {
    search_page_cursor(scope, target, page, None).map(|p| p.issues)
}
pub fn search_page_cursor(
    scope: &Scope,
    target: &SearchTarget,
    page: u32,
    cursor: Option<&str>,
) -> Result<SearchPage, Error> {
    if !(1..=5).contains(&page) || (!target.paginated() && page != 1) {
        return Err("Unsupported search page.".into());
    }
    if (target.tracker == "jira" && (page == 1) != cursor.is_none())
        || (target.tracker != "jira" && cursor.is_some())
    {
        return Err("Search continuation does not match its page.".into());
    }
    if target.tracker=="trello" && scope.query.is_none() {return Err("Trello paging requires an explicit search query.".into());}
    if !search_targets(scope)?.contains(target) {
        return Err("Tracker/server configuration changed. Search again.".into());
    }
    let tracker = TRACKERS
        .iter()
        .find(|tracker| tracker.id == target.tracker)
        .ok_or("Unknown tracker")?;
    let mut repos = scope.repositories.clone();
    if let Some(repo) = &target.repository {
        repos.insert(target.tracker.into(), vec![repo.clone()]);
    }
    let selected = Scope {
        remotes: scope.remotes,
        query: scope.query,
        repositories: &repos,
        page,
    };
    let fetched = if target.tracker == "jira" {
        jira_page(tracker, &selected, cursor)?
    } else {
        SearchPage {
            issues: (tracker.fetch)(tracker, &selected)?,
            next: None,
        }
    };
    let issues = fetched.issues;
    let mut seen = std::collections::BTreeSet::new();
    let mut safe = Vec::new();
    for issue in issues {
        let api = issue.api.as_deref().unwrap_or_default();
        let web = issue.url.as_deref().unwrap_or_default();
        let web_host = match target.tracker {
            "github" => "github.com",
            "trello" => "trello.com",
            _ => target.server.as_str(),
        };
        if let Some(SourceReference::GitLab{project_path,project_id,..})=&issue.source_reference {
            if target.repository.as_ref().is_some_and(|repo|repo!=project_path&&repo!=&project_id.to_string()) {return Err("GitLab reference is outside the selected repository.".into());}
        }
        if issue.id.is_empty()
            || issue.id.len() > 128
            || issue.summary.len() > 4096
            || api.len() > 2048
            || web.len() > 2048
            || !api.starts_with(&format!("https://{}/", target.server))
            || !web.starts_with(&format!("https://{web_host}/"))
            || [issue.id.as_str(), issue.summary.as_str(), api, web]
                .iter()
                .any(|value| value.chars().any(char::is_control))
        {
            return Err("Tracker returned an invalid or foreign-server issue. Search results were not cached.".into());
        }
        if target.tracker == "github" {
            if let Some(repo) = &target.repository {
                if !api.to_ascii_lowercase().starts_with(&format!(
                    "https://api.github.com/repos/{}/issues/",
                    repo.to_ascii_lowercase()
                )) {
                    return Err("GitHub returned an issue outside the selected repository.".into());
                }
            }
        }
        let path = api
            .strip_prefix(&format!("https://{}/", target.server))
            .unwrap_or_default();
        let numeric_tail = |prefix: &str| {
            path.strip_prefix(prefix)
                .is_some_and(|tail| !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()))
        };
        let endpoint = match target.tracker {
            "redmine"|"mantis"|"generic" => crate::self_hosted::identity(&issue) && issue.self_hosted.as_ref().is_some_and(|p|p.family==target.tracker&&Some(&p.base_url)==target.repository.as_ref()&&crate::self_hosted::server(&p.base_url)==Some(target.server.as_str())),
            "fogbugz" => crate::fogbugz::identity(&issue) && issue.fogbugz.as_ref().is_some_and(|p| p.host==target.server),
            "github" => path.rsplit_once("/issues/").is_some_and(|(base, tail)| {
                base.starts_with("repos/")
                    && !tail.is_empty()
                    && tail.bytes().all(|b| b.is_ascii_digit())
            }),
            "gitlab" => path.rsplit_once("/issues/").is_some_and(|(base, tail)| {
                base.starts_with("api/v4/projects/")
                    && !tail.is_empty()
                    && tail.bytes().all(|b| b.is_ascii_digit())
            }),
            "forgejo" => target
                .repository
                .as_ref()
                .is_some_and(|repo| numeric_tail(&format!("api/v1/repos/{repo}/issues/"))),
            "youtrack" => {
                path == format!("api/issues/{}", issue.id)
                    && issue
                        .id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
            }
            "jira" => {
                path == format!("rest/api/3/issue/{}", issue.id)
                    && issue
                        .id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
            }
            "lighthouse" => target.repository.as_ref().and_then(|project|project.parse::<u64>().ok()).is_some_and(|project| {
                path.strip_prefix(&format!("projects/{project}/tickets/")).and_then(|s|s.strip_suffix(".xml")).and_then(|s|s.parse::<u64>().ok()).filter(|n|*n>0).is_some_and(|number|issue.id==lighthouse_id(&target.server,project,number)&&path==format!("projects/{project}/tickets/{number}.xml")&&web==format!("https://{}/projects/{project}/tickets/{number}",target.server))
            }),
            "trello" => {
                path == format!("1/cards/{}", issue.id)
                    && issue
                        .id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
            }
            _ => false,
        };
        if !endpoint {
            return Err("Tracker returned an unsupported issue API endpoint. Search results were not cached.".into());
        }
        if seen.insert(api.to_string()) {
            safe.push(issue);
        }
        if safe.len() == 50 {
            break;
        }
    }
    Ok(SearchPage {
        issues: safe,
        next: fetched.next,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::HttpResponse;
    use openidea_plugin::mock::MockHost;

    fn respond(host: &MockHost, body: &'static str) {
        host.on_fetch(move |_| Ok(HttpResponse { status: 200, headers: vec![], body: body.into() }));
    }

    #[test]
    fn finds_the_project_of_ssh_and_https_remotes() {
        let remotes = vec![
            ("fork".to_owned(), "https://github.com/me/tool.git".to_owned()),
            ("origin".to_owned(), "git@github.com:acme/tool.git".to_owned()),
            ("lab".to_owned(), "ssh://git@gitlab.com/group/sub/proj".to_owned()),
        ];

        assert_eq!(remote_path(&remotes, "github.com").as_deref(), Some("acme/tool"));
        assert_eq!(remote_path(&remotes, "gitlab.com").as_deref(), Some("group/sub/proj"));
        assert_eq!(remote_path(&remotes, "bitbucket.org"), None);
    }

    #[test]
    fn encodes_query_values() {
        assert_eq!(encode("repo:a/b is:open"), "repo%3Aa%2Fb%20is%3Aopen");
        assert_eq!(origin("https://acme.youtrack.cloud/api/issues/X-1"), "https://acme.youtrack.cloud");
    }

    #[test]
    fn closes_a_jira_issue_through_its_done_transition() {
        let host = MockHost::install().with_capabilities(&["secrets", "net:acme.atlassian.net"]);
        host.set_secret("jira-token", "Zm9v");
        host.set_setting("jiraHost", "\"acme.atlassian.net\"");
        respond(
            &host,
            r#"{"transitions":[{"id":"11","to":{"statusCategory":{"key":"indeterminate"}}},{"id":"31","to":{"statusCategory":{"key":"done"}}}]}"#,
        );
        let task = Task::issue(
            "jira",
            "PROJ-7",
            "Crash",
            "https://acme.atlassian.net/browse/PROJ-7",
            "https://acme.atlassian.net/rest/api/3/issue/PROJ-7",
        );

        close_issue(&task).unwrap();

        let requests = host.requests();
        assert_eq!(requests[1].method, "POST");
        assert_eq!(requests[1].url, "https://acme.atlassian.net/rest/api/3/issue/PROJ-7/transitions");
        assert_eq!(requests[1].body.as_deref(), Some(br#"{"transition":{"id":"31"}}"#.as_slice()));
        assert!(requests[1].headers.contains(&("Authorization".into(), "Basic Zm9v".into())));
    }

    fn no_repositories() -> BTreeMap<String, Vec<String>> {
        BTreeMap::new()
    }

    #[test]
    fn update_issues_refuses_non_atlassian_jira_host() {
        let host = MockHost::install().with_capabilities(&["secrets", "net:*"]);
        host.set_secret("jira-token", "Zm9v");
        host.set_setting("jiraHost", "\"evil.test\"");
        respond(&host, r#"{"issues":[]}"#);

        let fetched = fetch_all(&Scope { remotes: &[], query: None, page: 1, repositories: &no_repositories() });

        assert!(host.requests().is_empty());
        assert!(fetched.errors[0].contains("Jira Cloud host"), "{:?}", fetched.errors);
    }

    #[test]
    fn close_issue_refuses_foreign_api_origin() {
        let host = MockHost::install().with_capabilities(&["secrets", "net:*"]);
        host.set_secret("github-token", "t");
        respond(&host, "{}");
        let task = Task::issue("github", "r-1", "Bug", "https://github.com/a/r/issues/1", "https://evil.test/repos/a/r/issues/1");

        assert!(close_issue(&task).is_err());
        assert!(host.requests().is_empty());
    }

    #[test]
    fn forgejo_repo_must_be_owner_slash_name() {
        let host = MockHost::install().with_capabilities(&["secrets", "net:*"]);
        host.set_secret("forgejo-token", "t");
        host.set_setting("forgejoRepository", "\"a/b/../../x\"");
        respond(&host, "[]");

        let fetched = fetch_all(&Scope { remotes: &[], query: None, page: 1, repositories: &no_repositories() });

        assert!(host.requests().is_empty());
        assert_eq!(fetched.errors.len(), 1);
        assert!(valid_repo("acme/tool.rs"));
        assert!(!valid_repo("a/b/c") && !valid_repo("../x") && !valid_repo("a/b?x"));
    }

    #[test]
    fn github_repo_must_be_owner_slash_name() {
        let host = MockHost::install().with_capabilities(&["secrets", "net:*"]);
        host.set_secret("github-token", "t");
        host.set_setting("githubRepository", "\"a/b is:private\"");
        respond(&host, r#"{"items":[]}"#);

        let fetched = fetch_all(&Scope { remotes: &[], query: None, page: 1, repositories: &no_repositories() });

        assert!(host.requests().is_empty());
        assert_eq!(fetched.errors.len(), 1);
    }

    #[test]
    fn jira_key_encoded_in_path() {
        let host = MockHost::install().with_capabilities(&["secrets", "net:*"]);
        host.set_secret("jira-token", "Zm9v");
        host.set_setting("jiraHost", "\"acme.atlassian.net\"");
        respond(&host, r#"{"issues":[{"key":"A-1/../x","fields":{"summary":"s"}}]}"#);

        let fetched = fetch_all(&Scope { remotes: &[], query: None, page: 1, repositories: &no_repositories() });

        assert_eq!(fetched.issues[0].api.as_deref(), Some("https://acme.atlassian.net/rest/api/3/issue/A-1%2F..%2Fx"));
        assert_eq!(fetched.issues[0].url.as_deref(), Some("https://acme.atlassian.net/browse/A-1%2F..%2Fx"));
    }

    #[test]
    fn posts_youtrack_work_items_in_minutes() {
        let host = MockHost::install().with_capabilities(&["secrets", "net:acme.youtrack.cloud"]);
        host.set_secret("youtrack-token", "perm:x");
        host.set_setting("youtrackHost", "\"acme.youtrack.cloud\"");
        respond(&host, "");
        let task =
            Task::issue("youtrack", "X-1", "Crash", "https://acme.youtrack.cloud/issue/X-1", "https://acme.youtrack.cloud/api/issues/X-1");

        post_time(&task, 3601).unwrap();

        let request = &host.requests()[0];
        assert_eq!(request.url, "https://acme.youtrack.cloud/api/issues/X-1/timeTracking/workItems");
        let body: Value = serde_json::from_slice(request.body.as_deref().unwrap()).unwrap();
        assert_eq!(body["duration"]["minutes"], 61);
    }

    #[test]
    fn two_repos_same_name_distinct_ids() {
        let host = MockHost::install().with_capabilities(&["secrets", "net:api.github.com"]);
        host.set_secret("github-token", "t");
        host.on_fetch(|_| {
            let body = r#"{"items":[
                {"number":5,"title":"A","html_url":"https://github.com/alice/app/issues/5","url":"https://api.github.com/repos/alice/app/issues/5","repository_url":"https://api.github.com/repos/alice/app"},
                {"number":5,"title":"B","html_url":"https://github.com/bob/app/issues/5","url":"https://api.github.com/repos/bob/app/issues/5","repository_url":"https://api.github.com/repos/bob/app"}]}"#;
            Ok(HttpResponse { status: 200, headers: vec![], body: body.into() })
        });
        let repositories = BTreeMap::from([("github".to_owned(), vec!["alice/app".to_owned(), "bob/app".to_owned()])]);

        let fetched = fetch_all(&Scope { remotes: &[], query: None, page: 1, repositories: &repositories });

        let ids: Vec<_> = fetched.issues.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["alice/app-5", "bob/app-5"]);
    }

    #[test]
    fn tasks_stored_with_the_old_github_id_still_match() {
        let api = "https://api.github.com/repos/alice/app/issues/5";
        let issue = Task::issue("github", "alice/app-5", "A", "https://github.com/alice/app/issues/5", api);
        let old = Task::issue("github", "app-5", "A", "https://github.com/alice/app/issues/5", api);
        let other = Task::issue("github", "app-5", "B", "https://github.com/bob/app/issues/5", "https://api.github.com/repos/bob/app/issues/5");

        assert!(same_issue(&issue, &old));
        assert!(!same_issue(&issue, &other));
    }

    #[test]
    fn reads_trello_cards_with_both_secrets_in_one_header() {
        let host = MockHost::install().with_capabilities(&["secrets", "net:api.trello.com"]);
        host.set_secret("trello-key", "k");
        host.set_secret("trello-token", "t");
        respond(&host, r#"[{"id":"5f3a","name":"Ship it","url":"https://trello.com/c/abc"}]"#);

        let fetched = fetch_all(&Scope { remotes: &[], query: None, page: 1, repositories: &BTreeMap::new() });

        assert_eq!(fetched.trackers, 1);
        assert_eq!(fetched.issues[0].label(), "5f3a: Ship it");
        let auth = &host.requests()[0].headers[0].1;
        assert_eq!(auth, r#"OAuth oauth_consumer_key="k", oauth_token="t""#);
    }
    #[test]
    fn search_pagination_and_cloud_host_restrictions_are_explicit() {
        let host = MockHost::install().with_capabilities(&["secrets", "net:*"]);
        host.set_secret("youtrack-token", "token");
        host.set_setting("youtrackHost", "\"tenant.youtrack.cloud\"");
        respond(&host, "[]");
        let repositories = BTreeMap::new();
        let scope = Scope { remotes: &[], repositories: &repositories, query: Some("bug"), page: 2 };
        let target = search_targets(&scope).unwrap().remove(0);
        search_page(&scope, &target, 2).unwrap();
        assert!(host.requests()[0].url.contains("$skip=50"));
        assert!(search_page(&scope, &target, 6).is_err());
        host.set_setting("youtrackHost", "\"user@evil.example\"");
        assert!(search_targets(&scope).unwrap().is_empty());
        host.set_setting("youtrackHost", "\"evil.example:8443\"");
        assert!(search_targets(&scope).unwrap().is_empty());
        assert!(!cloud_host("user@tenant.youtrack.cloud", ".youtrack.cloud"));
    }
    #[test]
    fn self_hosted_youtrack_needs_the_hosts_net_ask_approval() {
        let host = MockHost::install().with_capabilities(&["secrets", "net:ask"]);
        host.set_secret("youtrack-token", "token");
        host.set_setting("youtrackHost", "\"youtrack.corp.example\"");
        let repositories = BTreeMap::new();
        let scope = Scope { remotes: &[], repositories: &repositories, query: Some("bug"), page: 1 };
        let target = search_targets(&scope).unwrap().remove(0);
        assert_eq!(target.server, "youtrack.corp.example");
        assert!(matches!(search_page(&scope, &target, 1), Err(Error::Denied(c)) if c == "net:youtrack.corp.example"));
    }
    #[test]
    fn page_parameters_are_tracker_specific_and_cursor_trackers_refuse_more() {
        let host = MockHost::install().with_capabilities(&["secrets", "net:*"]);
        for secret in ["github-token", "gitlab-token", "forgejo-token", "jira-token", "trello-key", "trello-token"] {
            host.set_secret(secret, "token");
        }
        host.set_setting("jiraHost", "\"tenant.atlassian.net\"");
        respond(&host, "[]");
        let repositories = BTreeMap::from([
            ("github".into(), vec!["acme/tool".into()]),
            ("gitlab".into(), vec!["group/sub/tool".into()]),
            ("forgejo".into(), vec!["acme/tool".into()]),
        ]);
        let scope = Scope { remotes: &[], repositories: &repositories, query: Some("bug & fix"), page: 2 };
        let targets = search_targets(&scope).unwrap();
        for target in &targets {
            if target.tracker=="trello" {continue;} // Dedicated search-page shape is exercised below.
            if target.paginated() && !matches!(target.tracker,"jira"|"trello") {
                search_page(&scope, target, 2).unwrap();
            } else {
                assert!(search_page(&scope, target, 2).is_err());
            }
        }
        let requests = host.requests();
        assert_eq!(requests.len(), 3);
        assert!(requests.iter().all(|request| request.url.contains("page=2")));
        assert!(requests.iter().any(|request| request.url.contains("projects/group%2Fsub%2Ftool/issues")));
        assert!(requests.iter().all(|request| request.url.contains("bug%20%26%20fix")));
        assert!(targets.iter().find(|target| target.tracker == "jira").is_some_and(|target| target.paginated()));
        assert!(targets.iter().find(|target| target.tracker == "trello").is_some_and(|target| target.paginated()));
    }
}

#[cfg(test)]
mod jira_cursor_contract_tests {
    use super::*;
    use openidea_plugin::{HttpResponse,mock::MockHost};
    #[test]
    fn jira_requires_valid_bounded_nonrepeated_metadata_and_explicit_page_cursor() {
        let h=MockHost::install().with_capabilities(&["secrets","net:acme.atlassian.net"]);h.set_secret("jira-token","fixture");h.set_setting("jiraHost",r#""acme.atlassian.net""#);let repos=BTreeMap::new();let scope=Scope{remotes:&[],query:Some("bug"),page:1,repositories:&repos};let t=search_targets(&scope).unwrap().remove(0);
        assert!(search_page_cursor(&scope,&t,2,None).is_err());assert!(h.requests().is_empty());
        for value in [json!({"issues":[],"nextPageToken":17}),json!({"issues":[],"nextPageToken":""}),json!({"issues":[],"nextPageToken":"a\nb"}),json!({"issues":[],"isLast":false}),json!({"issues":[],"isLast":true,"nextPageToken":"A"}),json!({"issues":[],"nextPageToken":"A"}),json!({"issues":[{"key":"APP-1"}]})] {
            h.on_fetch(move |_|Ok(HttpResponse{status:200,headers:vec![],body:value.to_string().into_bytes()}));assert!(search_page_cursor(&scope,&t,2,Some("A")).is_err());
        }
        let before=h.requests().len();assert!(search_page_cursor(&scope,&t,2,Some(&"x".repeat(4097))).is_err());assert_eq!(h.requests().len(),before);
    }
}

#[cfg(test)]
mod trello_page_contract_tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    #[test]
    fn trello_member_listing_cannot_be_mistaken_for_a_search_page() {
        let h=MockHost::install().with_capabilities(&["secrets","net:api.trello.com"]);h.set_secret("trello-key","key");h.set_secret("trello-token","token");let repositories=BTreeMap::new();let scope=Scope{remotes:&[],repositories:&repositories,query:None,page:1};let target=search_targets(&scope).unwrap().remove(0);
        assert!(search_page_cursor(&scope,&target,1,None).is_err());assert!(search_page_cursor(&scope,&target,6,None).is_err());assert!(h.requests().is_empty());
        let query=Scope{query:Some("bug"),..scope};
        for body in [serde_json::json!({}),serde_json::json!({"cards":[{"id":"broken"}]})] {
            h.on_fetch(move |_|Ok(openidea_plugin::HttpResponse{status:200,headers:vec![],body:body.to_string().into_bytes()}));assert!(search_page_cursor(&query,&target,1,None).is_err());
        }
    }
}

#[cfg(test)]
mod closed_search_tests {
    use super::*;
    use openidea_plugin::{mock::MockHost, HttpResponse};
    #[test]
    fn closed_issue_option_changes_explicit_queries_but_never_refresh() {
        let host = MockHost::install().with_capabilities(&["secrets", "net:*"]);
        for token in ["github-token", "gitlab-token", "forgejo-token"] { host.set_secret(token, "fixture"); }
        host.on_fetch(|_| Ok(HttpResponse { status: 200, headers: vec![], body: b"[]".to_vec() }));
        let repositories = BTreeMap::from([
            ("github".into(), vec!["acme/tool".into()]),
            ("gitlab".into(), vec!["acme/tool".into()]),
            ("forgejo".into(), vec!["acme/tool".into()]),
        ]);
        let scope = Scope { remotes: &[], query: Some("bug"), page: 1, repositories: &repositories };
        let signature = search_signature(&scope).unwrap();
        host.set_setting("searchClosedIssues", "true");
        assert_ne!(signature, search_signature(&scope).unwrap(), "cached pages must not cross closed-filter changes");
        for target in search_targets(&scope).unwrap() { search_page(&scope, &target, 1).unwrap(); }
        let requests = host.requests();
        assert!(requests.iter().any(|r| r.url.contains("gitlab.com") && r.url.contains("state=all")));
        assert!(requests.iter().any(|r| r.url.contains("codeberg.org") && r.url.contains("state=all")));
        assert!(requests.iter().any(|r| r.url.contains("api.github.com") && !r.url.contains("is%3Aopen")));
        let before = requests.len();
        let listing = Scope { query: None, ..scope };
        fetch_all(&listing);
        let requests = host.requests();
        let refresh = &requests[before..];
        assert!(refresh.iter().any(|r| r.url.contains("gitlab.com") && r.url.contains("state=opened")));
        assert!(refresh.iter().any(|r| r.url.contains("codeberg.org") && r.url.contains("state=open")));
        assert!(refresh.iter().any(|r| r.url.contains("api.github.com") && r.url.contains("is%3Aopen")));
    }
}

#[cfg(test)]
mod issue_description_tests {
    use super::*;
    use openidea_plugin::{mock::MockHost, HttpResponse};
    #[test]
    fn tracker_search_retains_bounded_plaintext_description() {
        let h = MockHost::install().with_capabilities(&["secrets", "net:api.github.com"]);
        h.set_secret("github-token", "fixture");
        h.set_setting("githubRepository", "\"acme/tool\"");
        h.on_fetch(|_| Ok(HttpResponse { status: 200, headers: vec![], body: br#"{"items":[{"number":1,"title":"Bug","body":"Trace\n at example.App.main(App.java:7)","html_url":"https://github.com/acme/tool/issues/1","url":"https://api.github.com/repos/acme/tool/issues/1"}]}"#.to_vec() }));
        let issues = fetch_all(&Scope { remotes: &[], query: None, page: 1, repositories: &BTreeMap::new() });
        assert_eq!(issues.issues[0].description, "Trace\n at example.App.main(App.java:7)");
    }
    #[test]
    fn jira_document_text_is_flattened_without_link_markup_or_execution() {
        let doc = json!({"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"First"},{"type":"hardBreak"},{"type":"text","text":"line","marks":[{"type":"link","attrs":{"href":"javascript:ignored"}}]}]},{"type":"paragraph","content":[{"type":"text","text":"Next"}]}]});
        assert_eq!(description_text(&doc), "First\nline\nNext");
        let text = description_text(&Value::String("é".repeat(40_000)));
        assert!(text.len() <= 32 * 1024 && text.ends_with("[Description truncated]"));
        assert_eq!(description_text(&Value::String("safe\0text".into())), "safetext");
    }
}

pub(crate) fn lighthouse_id(site: &str, project: u64, number: u64) -> String {
    format!("LH-{}-{project}-{number}",site.split('.').next().unwrap_or_default())
}
fn lighthouse_project(scope: &Scope) -> Result<u64, Error> {
    let projects = repositories(scope, "lighthouse", "lighthouseProject", "lighthouseapp.com");
    let [project] = projects.as_slice() else { return Err("Configure one numeric Lighthouse project ID.".into()); };
    project.parse::<u64>().ok().filter(|id|*id>0 && id.to_string()==*project)
        .ok_or_else(|| "Use a positive numeric Lighthouse project ID.".into())
}
fn lighthouse_request(tracker: &Tracker, method: &str, url: &str, body: Option<&str>) -> Result<String, Error> {
    let mut request=HttpRequest{method:method.into(),url:url.into(),..HttpRequest::default()};
    for (name,value) in tracker.headers { request=request.header(name,value); }
    request=request.header("Accept","application/xml");
    if let Some(body)=body { request=request.header("Content-Type","application/xml");request.body=Some(body.as_bytes().to_vec()); }
    let response=host::fetch(&request)?;
    if response.body.len()>1<<20 {return Err("Lighthouse response exceeds 1 MiB.".into());}
    if !(200..300).contains(&response.status) {return Err(format!("Lighthouse HTTP {}. Check token, account and project.",response.status).into());}
    String::from_utf8(response.body).map_err(|_| "Lighthouse returned non-UTF8 XML.".into())
}
fn lighthouse(tracker: &Tracker, scope: &Scope) -> Result<Vec<Task>, Error> {
    let site=site("lighthouseHost","Lighthouse")?;
    if !cloud_host(&site,".lighthouseapp.com") {return Err("Use a Lighthouse Cloud tenant host.".into());}
    let project=lighthouse_project(scope)?;
    let query=format!("{}sort:updated {}",if include_closed(scope){""}else{"state:open "},scope.query.unwrap_or_default());
    let url=format!("https://{site}/projects/{project}/tickets.xml?q={}&limit={LIMIT}&page={}",encode(&query),scope.page);
    parse_lighthouse(&lighthouse_request(tracker,"GET",&url,None)?,&site,project)
}
fn parse_lighthouse(xml: &str, site: &str, project: u64) -> Result<Vec<Task>, Error> {
    use quick_xml::{Reader, events::Event};
    if xml.len()>1<<20 {return Err("Lighthouse XML exceeds 1 MiB.".into());}
    let mut reader=Reader::from_str(xml);
    let mut stack:Vec<String>=Vec::new();
    let mut fields:BTreeMap<String,String>=BTreeMap::new();
    let mut out=Vec::new();
    let mut root=false;
    loop {
        match reader.read_event().map_err(|_|"Malformed Lighthouse XML.")? {
            Event::Start(e) => {
                let name=e.name().as_ref().to_owned();
                if stack.is_empty() {if root||name!="tickets" {return Err("Lighthouse response is not a tickets list.".into());}root=true;}
                if stack.len()>=16 {return Err("Lighthouse XML nesting exceeds its limit.".into());}
                if stack.len()==1&&name=="ticket" {fields.clear();}
                if stack.len()==2&&stack[1]=="ticket"&&matches!(name.as_str(),"number"|"project-id"|"title"|"body") {
                    if fields.insert(name.clone(),String::new()).is_some() {return Err("Lighthouse returned duplicate ticket fields.".into());}
                }
                stack.push(name);
            }
            Event::End(e) => {
                let name=e.name().as_ref().to_owned();
                if stack.last().map(String::as_str)!=Some(name.as_str()) {return Err("Mismatched Lighthouse XML.".into());}
                if stack.len()==2&&name=="ticket" {
                    if out.len()>=LIMIT as usize {return Err("Lighthouse exceeded the requested page limit.".into());}
                    let number=fields.get("number").and_then(|n|n.parse::<u64>().ok()).filter(|n|*n>0).ok_or("Lighthouse ticket has no valid number.")?;
                    if fields.get("project-id").and_then(|n|n.parse::<u64>().ok())!=Some(project) {return Err("Lighthouse ticket belongs to another project.".into());}
                    let title=fields.get("title").filter(|t|!t.is_empty()&&t.len()<=4096&&!t.chars().any(char::is_control)).ok_or("Lighthouse ticket has an invalid title.")?;
                    let mut task=Task::issue("lighthouse",&lighthouse_id(site,project,number),title,&format!("https://{site}/projects/{project}/tickets/{number}"),&format!("https://{site}/projects/{project}/tickets/{number}.xml"));
                    task.description=description_text(&Value::String(fields.get("body").cloned().unwrap_or_default()));out.push(task);
                }
                stack.pop();
            }
            Event::Text(text) => {
                if stack.is_empty() && !text.as_ref().trim().is_empty() {return Err("Text outside Lighthouse XML root.".into());}
                if stack.len()==3&&stack[1]=="ticket" {
                    if let Some(field)=fields.get_mut(&stack[2]) {
                        let text=text.xml10_content();
                        field.push_str(&quick_xml::escape::unescape(&text).map_err(|_|"Invalid Lighthouse entity.")?);
                    }
                }
            }
            Event::GeneralRef(reference) => {
                let encoded=format!("&{};",reference.as_ref());
                let decoded=quick_xml::escape::unescape(&encoded).map_err(|_|"Invalid Lighthouse entity reference.")?;
                if stack.len()==3&&stack[1]=="ticket" {if let Some(field)=fields.get_mut(&stack[2]) {field.push_str(&decoded);}}
            }
            Event::CData(text) => if stack.len()==3&&stack[1]=="ticket" {if let Some(field)=fields.get_mut(&stack[2]) {field.push_str(&text.xml10_content());}},
            Event::DocType(_)=>return Err("Lighthouse XML DTDs are unsupported.".into()),
            Event::Eof=>break,
            Event::Empty(e) => {if stack.is_empty() {if root||e.name().as_ref()!="tickets" {return Err("Lighthouse response is not a tickets list.".into());}root=true;} else if stack.len()==1&&e.name().as_ref()=="ticket" {return Err("Lighthouse returned an empty ticket record.".into());}},
            _=>{},
        }
    }
    if !root||!stack.is_empty() {return Err("Incomplete Lighthouse XML.".into());}
    Ok(out)
}

#[cfg(test)]
mod lighthouse_tests {
    use super::*;
    use openidea_plugin::{mock::MockHost,HttpResponse};
    const XML:&str=r#"<tickets><ticket><number type="integer">7</number><project-id type="integer">42</project-id><title>Fix &amp; test</title><body><![CDATA[Trace <raw>]]></body><versions><ticket><title>old version</title></ticket></versions></ticket></tickets>"#;
    #[test]
    fn cloud_search_uses_header_secret_scoped_project_and_bounded_pagination() {
        let h=MockHost::install().with_capabilities(&["secrets","net:acme.lighthouseapp.com"]);
        h.set_secret("lighthouse-token","fixture");h.set_setting("lighthouseHost","\"acme.lighthouseapp.com\"");h.set_setting("lighthouseProject","\"42\"");
        h.on_fetch(|_|Ok(HttpResponse{status:200,headers:vec![],body:XML.as_bytes().to_vec()}));
        let repositories=BTreeMap::new();let scope=Scope{remotes:&[],repositories:&repositories,query:Some("bug & fix"),page:1};
        let target=search_targets(&scope).unwrap().remove(0);
        let results=search_page(&scope,&target,2).unwrap();
        assert_eq!(results[0].summary,"Fix & test");assert_eq!(results[0].description,"Trace <raw>");
        assert_eq!(results[0].api.as_deref(),Some("https://acme.lighthouseapp.com/projects/42/tickets/7.xml"));
        let request=&h.requests()[0];assert!(request.url.contains("page=2")&&request.url.contains("limit=50")&&request.url.contains("bug%20%26%20fix"));
        assert!(request.headers.iter().any(|(key,value)|key=="X-LighthouseToken"&&value=="fixture"));
        assert!(!request.url.contains("fixture"));
        h.set_setting("lighthouseHost","\"acme.lighthouseapp.com.evil.example\"");
        assert!(search_targets(&scope).unwrap().is_empty());
    }
    #[test]
    fn incomplete_or_foreign_xml_never_becomes_a_partial_ticket_page() {
        assert!(parse_lighthouse(&XML.replace("<project-id type=\"integer\">42","<project-id type=\"integer\">43"),"acme.lighthouseapp.com",42).is_err());
        for xml in ["<!DOCTYPE tickets><tickets/>","<tickets><ticket/></tickets>","<tickets><ticket><number>7</number></ticket></tickets>","<tickets>","<error>denied</error>"] {
            assert!(parse_lighthouse(xml,"acme.lighthouseapp.com",42).is_err(),"{xml}");
        }
        assert!(parse_lighthouse("<tickets/>","acme.lighthouseapp.com",42).unwrap().is_empty());
    }
    #[test]
    fn resolve_uses_exact_current_tenant_and_ticket_identity() {
        let h=MockHost::install().with_capabilities(&["secrets","net:acme.lighthouseapp.com"]);
        h.set_secret("lighthouse-token","fixture");h.set_setting("lighthouseHost","\"acme.lighthouseapp.com\"");h.set_setting("lighthouseProject","\"42\"");
        h.on_fetch(|_|Ok(HttpResponse{status:200,headers:vec![],body:Vec::new()}));
        let task=Task::issue("lighthouse","LH-acme-42-7","Issue","https://acme.lighthouseapp.com/projects/42/tickets/7","https://acme.lighthouseapp.com/projects/42/tickets/7.xml");
        close_issue(&task).unwrap();assert_eq!(h.requests()[0].method,"PUT");
        assert_eq!(h.requests()[0].body.as_deref(),Some(b"<ticket><state>resolved</state></ticket>".as_slice()));
        let before=h.requests().len();h.set_setting("lighthouseProject","\"43\"");
        assert!(close_issue(&task).is_err());assert_eq!(h.requests().len(),before);
    }
}

#[cfg(test)]
mod jql_tests {
    use super::jira_search_jql;

    #[test]
    fn plain_text_stays_a_quoted_text_search() {
        assert_eq!(jira_search_jql(r#"a"b"#).unwrap(), r#"text ~ "a\"b" order by updated DESC"#);
    }

    #[test]
    fn owned_jql_passes_through_when_well_shaped() {
        for ok in [
            "JQL: project = APP AND status in (\"In Progress\", Done) order by updated DESC, key",
            "jql:summary ~ \"order by (\" ORDER BY \"Story Points\" asc",
            "jql:order by created",
        ] {
            assert_eq!(jira_search_jql(ok).unwrap(), ok[4..].trim(), "{ok}");
        }
    }

    #[test]
    fn malformed_jql_is_refused_locally() {
        for bad in [
            "jql:",
            "jql:project = \"A",
            "jql:(project = A",
            "jql:project = A)",
            "jql:project = A and",
            "jql:project = A order by",
            "jql:project = A order by key sideways",
            "jql:project = A order by key, ",
            "jql:project = A order by key order by id",
            "jql:project = A and order by key",
            "jql:project = A\u{7}",
            &format!("jql:{}", "a".repeat(257)),
        ] {
            let error = jira_search_jql(bad).unwrap_err().to_string();
            assert!(error.contains("Invalid JQL"), "{bad}: {error}");
        }
    }
}
