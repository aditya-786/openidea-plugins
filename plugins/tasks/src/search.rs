//! Explicit server search with bounded session-only pages. No automatic retries,
//! persistent issue cache or background calls are made from the picker.
use crate::{
    load,
    model::Task,
    trackers::{self, Scope, SearchTarget},
};
use openidea_plugin::{Error, Invocation, PickItem, Project, host};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

const MAX_PAGES: u32 = 5;
const MAX_STEPS: usize = 20;
const TTL: Duration = Duration::from_secs(120);
struct Cached {
    root: String,
    scope: String,
    target: SearchTarget,
    query: String,
    page: u32,
    at: Instant,
    issues: Vec<Task>,
    cursor:Option<String>,
    next:Option<String>,
}
#[derive(Default)]
pub struct Search {
    cache: Vec<Cached>,
}
fn invalid(message: &str) -> Error {
    Error::Invalid(message.into())
}
fn query() -> Result<Option<String>, Error> {
    let Some(query) = host::input("Search issues (1–256 bytes; Jira: prefix jql: for raw JQL)", None)? else {
        return Ok(None);
    };
    let query = query.trim();
    if query.is_empty() {
        return Ok(None);
    }
    if query.len() > 256 || query.chars().any(char::is_control) {
        return Err(invalid("Use a search query of 1–256 bytes without control characters."));
    }
    Ok(Some(query.into()))
}
fn contextual_query(target:&SearchTarget,current:&str)->Result<Option<String>,Error> {
    if !matches!(target.tracker,"redmine"|"mantis") {return Ok(Some(current.into()));}
    let title=if target.tracker=="redmine" {"Redmine filters (project_id, issue_id, assigned_to_id, status_id)"} else {"Mantis filters (required project_id, optional filter_id)"};
    let initial=if current.contains('=') {current.to_owned()} else if target.tracker=="redmine"&&current.bytes().all(|b|b.is_ascii_digit())&&!current.is_empty(){format!("issue_id={current}")} else if target.tracker=="redmine" {"assigned_to_id=me&status_id=open".into()}else{"project_id=1".into()};
    let Some(value)=host::input(title,Some(&initial))? else{return Ok(None);};
    if value.trim().is_empty(){return Ok(None);}if value.len()>256||value.chars().any(char::is_control){return Err(invalid("Use at most 256 bytes of documented tracker filter parameters without controls."));}Ok(Some(value))
}
fn choose(targets: &[SearchTarget]) -> Result<Option<usize>, Error> {
    if targets.len() == 1 {
        return Ok(Some(0));
    }
    let items: Vec<_> = targets.iter().enumerate().map(|(index, target)| PickItem::new(&index.to_string(), &target.label)).collect();
    let Some(key) = host::pick("Search issue server / repository", &items)? else {
        return Ok(None);
    };
    key.parse::<usize>().ok().filter(|index| *index < targets.len()).map(Some).ok_or_else(|| invalid("Unknown server selection."))
}
fn scope(project: &Project) -> Result<(Vec<(String, String)>, BTreeMap<String, Vec<String>>), Error> {
    Ok((project.vcs_remotes().unwrap_or_default(), load(&project.root()?)?.repositories))
}
fn ensure(project: &Project, ctx: &Invocation, root: &str, signature: &str) -> Result<(), Error> {
    if project.root()? != root
        || ctx.project().and_then(|project| project.root().ok()).as_deref() != Some(root)
        || !host::projects()?.iter().any(|project| project.root().ok().as_deref() == Some(root))
    {
        return Err(invalid("Project changed or is unavailable. Search again."));
    }
    let (remotes, repositories) = scope(project)?;
    if trackers::search_signature(&Scope { remotes: &remotes, repositories: &repositories, query: None, page: 1 })? != signature {
        return Err(invalid("Tracker/server configuration changed. Search again."));
    }
    Ok(())
}
impl Search {
    /// Already validated server-search facts only; never performs host IO.
    pub(crate) fn known(&self, root: &str) -> Vec<(Task, String)> {
        let mut known: BTreeMap<String, Option<(Task, String)>> = BTreeMap::new();
        for entry in self.cache.iter().filter(|e| e.root == root && e.at.elapsed() < TTL) {
            for issue in &entry.issues {
                let next = (issue.clone(), entry.scope.clone());
                known
                    .entry(issue.id.clone())
                    .and_modify(|previous| {
                        if previous.as_ref().is_some_and(|(task, scope)| {
                            task.tracker != issue.tracker || task.api != issue.api || task.url != issue.url || task.source_reference!=issue.source_reference || task.fogbugz!=issue.fogbugz || task.self_hosted!=issue.self_hosted || scope != &entry.scope
                        }) {
                            *previous = None;
                        }
                    })
                    .or_insert(Some(next));
            }
        }
        known.into_values().flatten().collect()
    }
    pub(crate) fn validate_known(&self, project: &Project, ctx: &Invocation, issue: &Task, signature: &str) -> Result<(), Error> {
        let root = project.root()?;
        ensure(project, ctx, &root, signature)?;
        if !self.known(&root).iter().any(|(current, scope)| current == issue && scope == signature) {
            return Err(invalid("Cached issue changed, expired or became ambiguous. Search again."));
        }
        Ok(())
    }
    pub fn pick(&mut self, project: &Project, ctx: &Invocation) -> Result<Option<Task>, Error> {
        // Authentication identity cannot be queried by plugins. Never reuse a
        // cached search across command invocations / account changes.
        self.cache.clear();
        let root = project.root()?;
        let Some(mut query) = query()? else {
            return Ok(None);
        };
        let (remotes, repositories) = scope(project)?;
        let scope = Scope {
            remotes: &remotes,
            repositories: &repositories,
            query: None,
            page: 1,
        };
        let targets = trackers::search_targets(&scope)?;
        if targets.is_empty() {
            return Err(invalid(
                "No supported tracker/server is configured and authorized. Check token, host and repository in Settings › Plugins › Tasks.",
            ));
        }
        let signature = trackers::search_signature(&scope)?;
        let Some(mut selected) = choose(&targets)? else {
            return Ok(None);
        };
        if matches!(targets[selected].tracker,"redmine"|"mantis"|"generic") {let Some(next)=contextual_query(&targets[selected],&query)? else{return Ok(None);};query=next;}
        let (mut page, mut force) = (1, false);
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_STEPS {
            ensure(project, ctx, &root, &signature)?;
            let target = &targets[selected];
            let cached = if force {
                None
            } else {
                self.cache
                    .iter()
                    .find(|entry| {
                        entry.root == root
                            && entry.scope == signature
                            && entry.target == *target
                            && entry.query == query
                            && entry.page == page
                            && entry.cursor == cursor
                            && entry.at.elapsed() < TTL
                    })
                    .map(|entry| trackers::SearchPage {
                        issues: entry.issues.clone(),
                        next: entry.next.clone(),
                    })
            };
            let was_cached = cached.is_some();
            let result = match cached {
                Some(issues) => Ok(issues),
                None => {
                    let _progress = host::begin_task("Searching issues", false);
                    trackers::search_page_cursor(
                        &Scope {
                            remotes: &remotes,
                            repositories: &repositories,
                            query: Some(&query),
                            page,
                        },
                        target,
                        page,
                        cursor.as_deref(),
                    )
                }
            };
            ensure(project, ctx, &root, &signature)?;
            let result = result.and_then(|reply| {
                if target.tracker == "jira"
                    && reply.next.as_ref().is_some_and(|next| {
                        self.cache.iter().any(|entry| {
                            entry.root == root
                                && entry.scope == signature
                                && entry.target == *target
                                && entry.query == query
                                && entry.page <= page
                                && entry.cursor.as_ref() == Some(next)
                        })
                    })
                {
                    return Err(invalid("Jira continuation cycle; start a new query."));
                }
                Ok(reply)
            });
            let (issues, next, error) = match result {
                Ok(reply) => {
                    let issues = reply.issues;
                    let next = reply.next;
                    if !was_cached {
                        self.cache.retain(|entry| {
                            !(entry.root == root
                                && entry.scope == signature
                                && entry.target == *target
                                && entry.query == query
                                && entry.page == page)
                        });
                        if self.cache.len() == 20 {
                            self.cache.remove(0);
                        }
                        self.cache.push(Cached {
                            root: root.clone(),
                            scope: signature.clone(),
                            target: target.clone(),
                            query: query.clone(),
                            page,
                            at: Instant::now(),
                            issues: issues.clone(),
                            cursor: cursor.clone(),
                            next: next.clone(),
                        });
                    }
                    (issues, next, None)
                }
                Err(error) => (
                    Vec::new(),
                    None,
                    Some(error.to_string().chars().take(300).collect::<String>()),
                ),
            };
            let has_next = if target.tracker == "jira" {
                next.is_some()
            } else {
                target.paginated() && issues.len() == 50
            };
            let mut items: Vec<PickItem> = issues
                .iter()
                .enumerate()
                .map(|(index, issue)| {
                    PickItem::new(&index.to_string(), &issue.label()).description(&target.label)
                })
                .collect();
            if let Some(error) = &error {
                items.push(PickItem::new("@retry", "Search failed — Retry").description(error));
            } else {
                if issues.is_empty() {
                    items.push(PickItem::new("@empty", "No matching issues on this page"));
                }
                if has_next && page < MAX_PAGES {
                    items.push(PickItem::new("@next", "Next page (more results may exist)"));
                }
                items.push(PickItem::new("@retry", "Retry this page from server"));
            }
            if page > 1 {
                items.push(PickItem::new("@prev", "Previous page (session cache)"));
            }
            items.push(PickItem::new("@query", "New search query"));
            if targets.len() > 1 {
                items.push(PickItem::new(
                    "@server",
                    "Choose another server / repository",
                ));
            }
            let limit = if !target.paginated() {
                " · first page only"
            } else if page == MAX_PAGES {
                " · five-page limit"
            } else {
                ""
            };
            let title = format!(
                "Go to Issue · {} · page {page}{}{}",
                target.label,
                if was_cached { " · cached" } else { "" },
                limit
            );
            let Some(key) = host::pick(&title, &items)? else {
                return Ok(None);
            };
            ensure(project, ctx, &root, &signature)?;
            match key.as_str() {
                "@retry" => {
                    force = true;
                    self.cache
                        .retain(|entry| !(entry.target == *target && entry.query == query));
                }
                "@next" if error.is_none() && has_next && page < MAX_PAGES => {
                    page += 1;
                    cursor = next;
                    force = false;
                }
                "@prev" if page > 1 => {
                    page -= 1;
                    cursor = self
                        .cache
                        .iter()
                        .find(|entry| {
                            entry.root == root
                                && entry.scope == signature
                                && entry.target == *target
                                && entry.query == query
                                && entry.page == page
                        })
                        .and_then(|entry| entry.cursor.clone());
                    if target.tracker == "jira" && page > 1 && cursor.is_none() {
                        page = 1;
                    }
                    force = false;
                }
                "@query" => {
                    let next=if matches!(target.tracker,"redmine"|"mantis"|"generic") {contextual_query(target,&query)?}else{query_input()?};
                    let Some(next) = next else {
                        return Ok(None);
                    };
                    query = next;
                    page = 1;
                    cursor = None;
                    force = false;
                }
                "@server" if targets.len() > 1 => {
                    let Some(next) = choose(&targets)? else {
                        return Ok(None);
                    };
                    selected = next;
                    if matches!(targets[selected].tracker,"redmine"|"mantis"|"generic") {let Some(next)=contextual_query(&targets[selected],&query)? else{return Ok(None);};query=next;}
                    page = 1;
                    cursor = None;
                    force = false;
                }
                "@empty" if issues.is_empty() && error.is_none() => return Ok(None),
                _ => {
                    let index = key
                        .parse::<usize>()
                        .ok()
                        .filter(|index| *index < issues.len())
                        .ok_or_else(|| invalid("Unknown issue/search action."))?;
                    self.validate_known(project,ctx,&issues[index],&signature)?;
                    return Ok(Some(issues[index].clone()));
                }
            }
        }
        Err(invalid(
            "Search interaction limit reached. Run Search Issues again.",
        ))
    }
}
fn query_input() -> Result<Option<String>, Error> {
    query()
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{HttpResponse, mock::MockHost};
    use serde_json::json;
    use std::{cell::Cell, rc::Rc};
    fn setup() -> MockHost {
        let host = MockHost::install().with_capabilities(&["project.read", "storage", "vcs.read", "secrets", "net:api.github.com"]);
        host.set_secret("github-token", "secret");
        host.set_vcs_remotes(&[("origin", "git@github.com:acme/tool.git")]);
        host
    }
    fn github(repo: &str, start: u64, count: u64) -> HttpResponse {
        let items:Vec<_>=(start..start+count).map(|n|json!({"number":n,"title":format!("Issue {n}"),"html_url":format!("https://github.com/{repo}/issues/{n}"),"url":format!("https://api.github.com/repos/{repo}/issues/{n}"),"repository_url":format!("https://api.github.com/repos/{repo}")})).collect();
        HttpResponse { status: 200, headers: vec![], body: json!({"items":items}).to_string().into_bytes() }
    }
    fn answer(host: &MockHost, values: &[&str]) {
        for value in values {
            host.answer_next_prompt(Some(&[*value]));
        }
    }
    #[test]
    fn pages_are_bounded_and_previous_uses_only_the_current_scoped_session_cache() {
        let host = setup();
        host.on_fetch(|request| Ok(if request.url.ends_with("page=2") { github("acme/tool", 51, 50) } else { github("acme/tool", 1, 50) }));
        answer(&host, &["crash", "@next", "@prev", "0"]);
        let mut search = Search::default();
        let project = host.invocation().project().unwrap();
        let issue = search.pick(&project, &host.invocation()).unwrap().unwrap();
        assert_eq!(issue.id, "tool-1");
        assert_eq!(host.requests().len(), 2);
        assert_eq!(search.cache.len(), 2);
        assert!(host.requests()[1].url.ends_with("page=2"));
        answer(&host, &["crash", "0"]);
        search.pick(&project, &host.invocation()).unwrap();
        assert_eq!(host.requests().len(), 3, "new invocation cannot reuse another authentication session");
    }
    #[test]
    fn server_repository_switch_never_reuses_another_targets_hits() {
        let host = setup();
        crate::update("/project", |tasks, _| {
            tasks.repositories.insert("github".into(), vec!["acme/one".into(), "acme/two".into()]);
            Ok(())
        })
        .unwrap();
        host.on_fetch(|request| Ok(github(if request.url.contains("acme%2Fone") { "acme/one" } else { "acme/two" }, 7, 1)));
        answer(&host, &["crash", "0", "@server", "1", "0"]);
        let issue = Search::default().pick(&host.invocation().project().unwrap(), &host.invocation()).unwrap().unwrap();
        assert_eq!(issue.api.as_deref(), Some("https://api.github.com/repos/acme/two/issues/7"));
        assert_eq!(host.requests().len(), 2);
    }
    #[test]
    fn retry_is_explicit_errors_arent_cached_and_cancellation_does_not_activate() {
        let host = setup();
        let count = Rc::new(Cell::new(0));
        let calls = count.clone();
        host.on_fetch(move |_| {
            calls.set(calls.get() + 1);
            Ok(if calls.get() == 1 {
                HttpResponse { status: 429, headers: vec![], body: b"Rate limited".to_vec() }
            } else {
                github("acme/tool", 1, 1)
            })
        });
        answer(&host, &["crash", "@retry", "0"]);
        let mut search = Search::default();
        assert!(search.pick(&host.invocation().project().unwrap(), &host.invocation()).unwrap().is_some());
        assert_eq!(count.get(), 2);
        assert_eq!(search.cache.len(), 1);
        assert_eq!(load("/project").unwrap().active, crate::model::DEFAULT_TASK_ID);
        answer(&host, &["another"]);
        assert!(search.pick(&host.invocation().project().unwrap(), &host.invocation()).unwrap().is_none());
        assert_eq!(load("/project").unwrap().active, crate::model::DEFAULT_TASK_ID);
    }
    #[test]
    fn invalid_choices_and_foreign_server_results_cannot_be_selected_or_cached() {
        let host = setup();
        host.on_fetch(|_| Ok(github("foreign/repository", 1, 1)));
        answer(&host, &["crash"]);
        let mut search = Search::default();
        assert!(search.pick(&host.invocation().project().unwrap(), &host.invocation()).unwrap().is_none());
        assert!(search.cache.is_empty());
        host.on_fetch(|_| Ok(github("acme/tool", 1, 1)));
        answer(&host, &["crash", "999"]);
        assert!(search.pick(&host.invocation().project().unwrap(), &host.invocation()).is_err());
        answer(&host, &[&"x".repeat(257)]);
        let before = host.requests().len();
        assert!(search.pick(&host.invocation().project().unwrap(), &host.invocation()).is_err());
        assert_eq!(host.requests().len(), before);
    }
    #[test]
    fn configuration_and_project_changes_invalidate_scope_before_acceptance() {
        let host = setup();
        let ctx = host.invocation();
        let project = ctx.project().unwrap();
        let (remotes, repositories) = scope(&project).unwrap();
        let key = trackers::search_signature(&Scope { remotes: &remotes, repositories: &repositories, query: None, page: 1 }).unwrap();
        assert!(ensure(&project, &ctx, "/project", &key).is_ok());
        host.set_setting("githubAssignedOnly", "true");
        assert!(ensure(&project, &ctx, "/project", &key).is_err());
        host.set_setting("githubAssignedOnly", "null");
        host.set_project_root(Some("/other"));
        assert!(ensure(&project, &ctx, "/project", &key).is_err());
    }
    #[test]
    fn known_ids_are_root_scoped_expiring_and_ambiguous_servers_are_quiet() {
        let (_host, mut search) = (setup(), Search::default());
        let issue = Task::issue(
            "github",
            "tool-1",
            "One",
            "https://github.com/acme/tool/issues/1",
            "https://api.github.com/repos/acme/tool/issues/1",
        );
        let cached = |root: &str, issue: Task, age: Duration| Cached {
            root: root.into(),
            scope: "scope".into(),
            target: SearchTarget {
                tracker: "github",
                server: "api.github.com".into(),
                repository: Some("acme/tool".into()),
                label: "GitHub".into(),
            },
            query: "x".into(),
            page: 1,
            at: Instant::now() - age,
            issues: vec![issue],
            cursor:None,next:None,
        };
        search.cache.push(cached("/a", issue.clone(), Duration::ZERO));
        assert_eq!(search.known("/a").len(), 1);
        assert!(search.known("/b").is_empty());
        search.cache.push(cached("/a", issue.clone(), Duration::ZERO));
        assert_eq!(search.known("/a").len(), 1);
        let mut other = issue.clone();
        other.api = Some("https://api.github.com/repos/other/tool/issues/1".into());
        search.cache.push(cached("/a", other, Duration::ZERO));
        assert!(search.known("/a").is_empty());
        search.cache.clear();
        search.cache.push(cached("/a", issue, TTL));
        assert!(search.known("/a").is_empty());
    }
}

#[cfg(test)]
mod jira_cursor_tests {
    use super::*;
    use openidea_plugin::{HttpResponse,mock::MockHost};
    use serde_json::json;
    fn setup()->MockHost {let h=MockHost::install().with_capabilities(&["project.read","storage","vcs.read","secrets","net:acme.atlassian.net"]);h.set_secret("jira-token","fixture");h.set_setting("jiraHost",r#""acme.atlassian.net""#);h}
    fn reply(id:u32,next:Option<&str>)->HttpResponse {
        let mut value=json!({"issues":[{"key":format!("APP-{id}"),"fields":{"summary":format!("Issue{id}")}}],"isLast":next.is_none()});
        if let Some(next)=next {value["nextPageToken"]=json!(next);}
        HttpResponse{status:200,headers:vec![],body:value.to_string().into_bytes()}
    }
    fn answers(h:&MockHost,values:&[&str]) {for v in values {h.answer_next_prompt(Some(&[*v]));}}
    #[test]
    fn jira_short_pages_continue_by_opaque_cursor_and_previous_is_cached() {
        let h=setup();h.on_fetch(|r| {assert!(r.url.starts_with("https://acme.atlassian.net/rest/api/3/search/jql?"));Ok(if r.url.contains("nextPageToken=") {assert!(r.url.contains("nextPageToken=next%2B%2F%3D%3F"));reply(2,None)}else{reply(1,Some("next+/=?"))})});
        answers(&h,&["crash","@next","@prev","@next","0"]);let mut s=Search::default();let issue=s.pick(&h.invocation().project().unwrap(),&h.invocation()).unwrap().unwrap();assert_eq!(issue.id,"APP-2");assert_eq!(h.requests().len(),2);assert_eq!(s.cache.len(),2);assert_eq!(s.cache[1].cursor.as_deref(),Some("next+/=?"));assert!(s.cache[1].next.is_none());
    }
    #[test]
    fn jira_new_query_drops_cursor_and_response_cycles_are_not_cached() {
        let h=setup();h.on_fetch(|r|Ok(if r.url.contains("nextPageToken=B") {reply(3,Some("A"))}else if r.url.contains("nextPageToken=A") {reply(2,Some("B"))}else{reply(1,Some("A"))}));
        answers(&h,&["crash","@next","@query","different","@next","@next"]);let mut s=Search::default();assert!(s.pick(&h.invocation().project().unwrap(),&h.invocation()).unwrap().is_none());
        let requests=h.requests();assert_eq!(requests.len(),5);assert!(!requests[2].url.contains("nextPageToken"));assert!(requests[2].url.contains("different"));assert!(!s.cache.iter().any(|c|c.page==3));
    }
    #[test]
    fn jira_retry_invalidates_descendant_pages_and_never_activates_a_task() {
        let h=setup();h.on_fetch(|r|Ok(if r.url.contains("nextPageToken="){reply(2,None)}else{reply(1,Some("A"))}));
        answers(&h,&["crash","@next","@prev","@retry"]);let mut s=Search::default();assert!(s.pick(&h.invocation().project().unwrap(),&h.invocation()).unwrap().is_none());assert_eq!(h.requests().len(),3);assert_eq!(s.cache.len(),1);assert_eq!(s.cache[0].page,1);assert_eq!(load("/project").unwrap().active,crate::model::DEFAULT_TASK_ID);
    }
}

#[cfg(test)]
mod trello_page_tests {
    use super::*;
    use openidea_plugin::{HttpResponse,mock::MockHost};
    use serde_json::json;
    #[test]
    fn trello_search_uses_zero_based_pages_and_previous_is_session_cached() {
        let h=MockHost::install().with_capabilities(&["project.read","storage","vcs.read","secrets","net:api.trello.com"]);h.set_secret("trello-key","fixture-key");h.set_secret("trello-token","fixture-token");
        h.on_fetch(|r|{assert!(r.url.starts_with("https://api.trello.com/1/search?query=crash"));assert!(r.url.contains("cards_limit=50"));let start=if r.url.contains("cards_page=1"){51}else{assert!(r.url.contains("cards_page=0"));1};let cards:Vec<_>=(start..start+50).map(|n|json!({"id":format!("card{n}"),"name":format!("Card{n}"),"url":format!("https://trello.com/c/card{n}")})).collect();Ok(HttpResponse{status:200,headers:vec![],body:json!({"cards":cards}).to_string().into_bytes()})});
        for a in ["crash","@next","@prev","0"] {h.answer_next_prompt(Some(&[a]));}
        let mut s=Search::default();assert_eq!(s.pick(&h.invocation().project().unwrap(),&h.invocation()).unwrap().unwrap().id,"card1");assert_eq!(h.requests().len(),2);assert_eq!(s.cache.len(),2);
    }
}
