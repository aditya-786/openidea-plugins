//! Current-source issue references backed exclusively by validated session search pages.
use crate::{model::{Task,SourceReference}, search::Search};
use openidea_plugin::hover::Hover;
use openidea_plugin::providers::Document;
use openidea_plugin::text::{offset, position};
use openidea_plugin::{Error, Invocation, Position, Range, host};
fn invalid(message: &str) -> Error {
    Error::Invalid(message.into())
}
fn token(doc: &Document, at: Position) -> Option<(&str, Range)> {
    if doc.text.len() > 512 * 1024 {
        return None;
    }
    let at = offset(&doc.text, at)?;
    let allowed = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'#' | b'/');
    let bytes = doc.text.as_bytes();
    let mut start = at;
    while start > 0 && allowed(bytes[start - 1]) {
        start -= 1;
    }
    let mut end = at;
    while end < bytes.len() && allowed(bytes[end]) {
        end += 1;
    }
    if start == end || end - start > 128 || !doc.text.is_char_boundary(start) || !doc.text.is_char_boundary(end) {
        return None;
    }
    // A known ID embedded in a Unicode identifier is not a separate reference.
    if doc.text[..start].chars().next_back().is_some_and(char::is_alphanumeric)
        || doc.text[end..].chars().next().is_some_and(char::is_alphanumeric)
    {
        return None;
    }
    Some((&doc.text[start..end], Range::new(position(&doc.text, start), position(&doc.text, end))))
}
/// The search parser validates server/target membership; also prove that the
/// browser page and source spelling identify the exact same API issue.
fn closed_identity(task: &Task) -> bool {
    let Some(api) = task.api.as_deref() else {
        return false;
    };
    let Some(web) = task.url.as_deref() else {
        return false;
    };
    match task.tracker.as_deref() {
        Some("fogbugz") => crate::fogbugz::identity(task),
        Some("redmine"|"mantis"|"generic") => crate::self_hosted::identity(task),
        Some("github") | Some("forgejo") => {
            let github = task.tracker.as_deref() == Some("github");
            let prefix = if github { "https://api.github.com/repos/" } else { "https://codeberg.org/api/v1/repos/" };
            let Some(rest) = api.strip_prefix(prefix) else {
                return false;
            };
            let parts: Vec<_> = rest.split('/').collect();
            if parts.len() != 4
                || parts[2] != "issues"
                || parts[3].is_empty()
                || !parts[3].bytes().all(|b| b.is_ascii_digit())
                || parts[..2]
                    .iter()
                    .any(|s| s.is_empty() || !s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.')))
            {
                return false;
            }
            let expected = if github { format!("{}-{}", parts[1], parts[3]) } else { format!("#{}", parts[3]) };
            let id_ok = task.id == expected || (github && task.id == format!("{}/{}-{}", parts[0], parts[1], parts[3])) || (!github && task.id == format!("{}/{}#{}", parts[0], parts[1], parts[3]));
            id_ok
                && web
                    == format!(
                        "https://{}/{}/{}/issues/{}",
                        if github { "github.com" } else { "codeberg.org" },
                        parts[0],
                        parts[1],
                        parts[3]
                    )
        }
        Some("gitlab")=> {
            let Some(SourceReference::GitLab{project_path,project_id,iid})=&task.source_reference else{return false;};
            *project_id>0&&*iid>0&&(task.id==format!("#{iid}")||task.id==format!("{project_path}#{iid}"))
                && api==format!("https://gitlab.com/api/v4/projects/{project_id}/issues/{iid}")
                && web==format!("https://gitlab.com/{project_path}/-/issues/{iid}")
        }
        Some("trello")=> {
            let Some(SourceReference::Trello{short_link})=&task.source_reference else{return false;};
            task.id.len()==24&&task.id.bytes().all(|b|b.is_ascii_hexdigit())&&short_link.len()==8&&short_link.bytes().all(|b|b.is_ascii_alphanumeric())
                &&api==format!("https://api.trello.com/1/cards/{}",task.id)&&web==format!("https://trello.com/c/{short_link}")
        }
        Some("lighthouse") => {
            let Some((server,path))=api.strip_prefix("https://").and_then(|s|s.split_once('/')) else{return false;};
            let parts:Vec<_>=path.split('/').collect();
            if parts.len()!=4||parts[0]!="projects"||parts[2]!="tickets"||!server.ends_with(".lighthouseapp.com") {return false;}
            let (Some(project),Some(number))=(parts[1].parse::<u64>().ok().filter(|n|*n>0),parts[3].strip_suffix(".xml").and_then(|n|n.parse::<u64>().ok()).filter(|n|*n>0)) else{return false;};
            task.id==crate::trackers::lighthouse_id(server,project,number)&&api==format!("https://{server}/projects/{project}/tickets/{number}.xml")&&web==format!("https://{server}/projects/{project}/tickets/{number}")
        }
        Some("jira") | Some("youtrack") => {
            if task.id.is_empty() || !task.id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')) {
                return false;
            }
            let Some((server, _)) = api.strip_prefix("https://").and_then(|s| s.split_once('/')) else {
                return false;
            };
            let (api_path, web_path) =
                if task.tracker.as_deref() == Some("jira") { ("rest/api/3/issue", "browse") } else { ("api/issues", "issue") };
            api == format!("https://{server}/{api_path}/{}", task.id) && web == format!("https://{server}/{web_path}/{}", task.id)
        }
        _ => false,
    }
}
fn selected(search: &Search, root: &str, doc: &Document, at: Position) -> Option<(Task, String, Range)> {
    let (id, range) = token(doc, at)?;
    search.known(root).into_iter().find(|(task, _)| task.id == id && closed_identity(task)).map(|(task, scope)| (task, scope, range))
}
fn escape(text: &str) -> String {
    let mut result = String::new();
    for ch in text.chars().take(2048) {
        if ch.is_control() {
            result.push(' ');
            continue;
        }
        if matches!(ch, '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '#' | '!' | '(' | ')' | '|') {
            result.push('\\');
        }
        result.push(ch);
    }
    result
}
pub fn hover(search: &Search, doc: &Document, at: Position) -> Option<Hover> {
    let root = host::project_of(&doc.path)?.root().ok()?;
    let (task, _, range) = selected(search, &root, doc, at)?;
    let description = if task.description.is_empty() { String::new() } else { format!("\n\n{}{}", escape(&task.description), if task.description.chars().count() > 2048 { "… (open task description for more)" } else { "" }) };
    Some(Hover::new(format!("**{}** — {}{}\n\nTracker: {} · {}\n\nCached explicit search result (up to 120 seconds old); status is not live. Use Tasks: Open/Activate Cached Issue at Caret for reviewed actions.",escape(&task.id),escape(&task.summary),description,escape(task.tracker.as_deref()?),escape(task.url.as_deref()?))).range(range))
}
fn same_source(ctx: &Invocation, root: &str, doc: &Document) -> Result<(), Error> {
    if ctx.project().and_then(|p| p.root().ok()).as_deref() != Some(root)
        || host::project_of(&doc.path).and_then(|p| p.root().ok()).as_deref() != Some(root)
    {
        return Err(invalid("The document's owning project changed."));
    }
    let editor = ctx.editor().ok_or("No editor is open.")?;
    let current = editor.document()?;
    if editor.path() != doc.path || current.version != doc.version || current.text != doc.text {
        return Err(invalid("The source changed. Select the issue again."));
    }
    Ok(())
}
fn matches(existing: &Task, issue: &Task) -> bool {
    existing.id == issue.id && existing.tracker == issue.tracker && existing.api == issue.api && existing.url == issue.url
}
fn validate_existing(tasks: &crate::model::TaskList, issue: &Task) -> Result<(), Error> {
    if tasks.tasks.iter().chain(&tasks.issues).filter(|task| task.id == issue.id).any(|task| !matches(task, issue)) {
        return Err(invalid("This ID belongs to a different existing task/server."));
    }
    Ok(())
}
/// Single-source authority retained while contextual branch options are reviewed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Review {
    pub issue: Task,
    root: String,
    doc: Document,
    signature: String,
    pub branch_format: String,
}
impl Review {
    pub(crate) fn validate(&self, search: &Search, ctx: &Invocation) -> Result<(), Error> {
        same_source(ctx, &self.root, &self.doc)?;
        let project = ctx.project().ok_or(Error::Stale)?;
        search.validate_known(&project, ctx, &self.issue, &self.signature)?;
        let tasks = crate::load(&self.root)?;
        validate_existing(&tasks, &self.issue)?;
        if tasks.find(&self.issue.id).is_some() || crate::setting_or("branchNameFormat", "{id}") != self.branch_format {
            return Err(invalid("The local task or branch-name configuration changed. Review the issue again."));
        }
        Ok(())
    }
}
pub(crate) fn prepare(search: &Search, ctx: &Invocation) -> Result<Review, Error> {
    let root = ctx.project().ok_or("Open the owning project first.")?.root()?;
    let editor = ctx.editor().ok_or("No editor is open.")?;
    let current = editor.document()?;
    let doc = Document { path: editor.path(), language: editor.language(), version: current.version, text: current.text };
    let selections = editor.selections()?;
    let [selection] = selections.as_slice() else { return Err(invalid("Use one caret or select one exact cached issue ID.")); };
    let (issue, signature, range) = selected(search, &root, &doc, selection.start)
        .ok_or("No unambiguous cached issue at the caret. Run Tasks: Search Issues first.")?;
    if selection.start != selection.end && *selection != range {
        return Err(invalid("Select exactly one issue ID."));
    }
    let review = Review { issue, root, doc, signature, branch_format: crate::setting_or("branchNameFormat", "{id}") };
    review.validate(search, ctx)?;
    Ok(review)
}

pub fn run(search: &Search, ctx: &Invocation, activate: bool) -> Result<(), Error> {
    let project = ctx.project().ok_or("Open the owning project first.")?;
    let root = project.root()?;
    if root.len() > 4096 || root.chars().any(char::is_control) {
        return Err(invalid("The project identity cannot be represented safely in a review."));
    }
    let editor = ctx.editor().ok_or("No editor is open.")?;
    let source = editor.document()?;
    let doc = Document { path: editor.path(), language: editor.language(), version: source.version, text: source.text };
    let selections = editor.selections()?;
    let [selection] = selections.as_slice() else {
        return Err(invalid("Use one caret or select one exact cached issue ID."));
    };
    let (issue, signature, range) = selected(search, &root, &doc, selection.start)
        .ok_or("No unambiguous cached issue at the caret. Run Tasks: Search Issues first.")?;
    if selection.start != selection.end && *selection != range {
        return Err(invalid("Select exactly one issue ID."));
    }
    same_source(ctx, &root, &doc)?;
    search.validate_known(&project, ctx, &issue, &signature)?;
    if !activate {
        return host::open_url(issue.url.as_deref().ok_or("The issue has no page.")?);
    }
    validate_existing(&crate::load(&root)?, &issue)?;
    let review = format!(
        "{}\nTime tracking switches to this issue. No branch is created, and the issue on {} is not changed.",
        issue.summary,
        crate::trackers::tracker_name(issue.tracker.as_deref().unwrap_or_default())
    );
    if !ui_kit::prompts::confirm_exact(&format!("Start a task for {}?", issue.id), &review)? {
        return Ok(());
    }
    same_source(ctx, &root, &doc)?;
    search.validate_known(&project, ctx, &issue, &signature)?;
    crate::update(&root, |tasks, now| {
        validate_existing(tasks, &issue)?;
        if tasks.find_any(&issue.id).is_none() {
            tasks.issues.push(issue.clone());
        }
        tasks.activate(&issue.id, now)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Tasks;
    use openidea_plugin::hover::HoverProvider;
    use openidea_plugin::mock::MockHost;
    use openidea_plugin::{HttpResponse, Plugin};
    fn setup() -> (MockHost, Search) {
        let host =
            MockHost::install().with_capabilities(&["project.read", "editor.read", "storage", "vcs.read", "secrets", "net:api.github.com"]);
        host.set_secret("github-token", "secret");
        host.set_vcs_remotes(&[("origin", "git@github.com:acme/tool.git")]);
        host.on_fetch(|_|Ok(HttpResponse{status:200,headers:vec![],body:serde_json::json!({"items":[{"number":1,"title":"Fix <script> [link](url)","body":"Description <img> [unsafe](javascript:evil)","html_url":"https://github.com/acme/tool/issues/1","url":"https://api.github.com/repos/acme/tool/issues/1","repository_url":"https://api.github.com/repos/acme/tool"}]}).to_string().into_bytes()}));
        host.answer_next_prompt(Some(&["fix"]));
        host.answer_next_prompt(Some(&["0"]));
        let mut search = Search::default();
        assert_eq!(search.pick(&host.invocation().project().unwrap(), &host.invocation()).unwrap().unwrap().id, "tool-1");
        (host, search)
    }
    fn doc(text: &str) -> Document {
        Document { path: "/project/a.txt".into(), language: "plaintext".into(), text: text.into(), version: 1 }
    }
    fn editor(host: &MockHost, text: &str) {
        host.open_editor("/project/a.txt", "plaintext", text, &[(0, 2, 0, 2)]);
    }
    #[test]
    fn hover_is_cached_escaped_and_exact_utf16_without_requests_or_processes() {
        let (host, search) = setup();
        let mut plugin = Tasks { issue_search: search, ..Default::default() };
        let d = doc("😀 tool-1.");
        let before = host.requests().len();
        let h = plugin.hover(&d, Position::new(0, 6)).unwrap();
        assert_eq!(h.range, Some(Range::new(Position::new(0, 3), Position::new(0, 9))));
        assert!(h.markdown.contains("\\<script\\>") && h.markdown.contains("\\[link\\]\\(url\\)"));
        assert!(h.markdown.contains("Description \\<img\\> \\[unsafe\\]\\(javascript:evil\\)"));
        assert!(h.markdown.contains("Cached explicit search"));
        assert_eq!(host.requests().len(), before);
        assert!(host.process_runs().is_empty());
        let mut other = d.clone();
        other.path = "/another/a.txt".into();
        assert!(plugin.hover(&other, Position::new(0, 6)).is_none());
        assert!(plugin.hover(&doc("étool-1"), Position::new(0, 4)).is_none());
        assert!(plugin.hover(&doc("tool-10"), Position::new(0, 2)).is_none());
        assert!(plugin.hover(&doc(&"x".repeat(512 * 1024 + 1)), Position::new(0, 2)).is_none());
    }
    #[test]
    fn explicit_open_uses_validated_cached_url_and_no_new_fetch() {
        let (host, search) = setup();
        editor(&host, "tool-1");
        let before = host.requests().len();
        run(&search, &host.invocation(), false).unwrap();
        assert_eq!(host.opened_urls(), ["https://github.com/acme/tool/issues/1"]);
        assert_eq!(host.requests().len(), before);
        assert!(host.process_runs().is_empty());
    }
    #[test]
    fn activate_is_confirmed_local_only_and_cancel_preserves_task() {
        let (host, search) = setup();
        editor(&host, "tool-1");
        host.answer_next_prompt(None);
        run(&search, &host.invocation(), true).unwrap();
        assert_eq!(crate::load("/project").unwrap().active, crate::model::DEFAULT_TASK_ID);
        host.answer_next_prompt(Some(&["ok"]));
        run(&search, &host.invocation(), true).unwrap();
        assert_eq!(crate::load("/project").unwrap().active, "tool-1");
        assert!(host.process_runs().is_empty() && host.opened_urls().is_empty());
        assert_eq!(host.requests().len(), 1);
    }
    #[test]
    fn changed_configuration_and_existing_foreign_identity_cannot_activate() {
        let (host, search) = setup();
        editor(&host, "tool-1");
        host.set_setting("tasks.githubQuery", "\"different\"");
        // Actual setting namespace is read through trackers::setting; changing remotes is authoritative too.
        host.set_vcs_remotes(&[("origin", "git@github.com:other/repo.git")]);
        assert!(run(&search, &host.invocation(), true).is_err());
        assert_eq!(crate::load("/project").unwrap().active, crate::model::DEFAULT_TASK_ID);
        host.set_vcs_remotes(&[("origin", "git@github.com:acme/tool.git")]);
        host.set_setting("tasks.githubQuery", "\"\"");
        crate::update("/project", |tasks, _| {
            tasks.issues.push(Task::issue(
                "jira",
                "tool-1",
                "Other",
                "https://other.atlassian.net/browse/tool-1",
                "https://other.atlassian.net/rest/api/3/issue/tool-1",
            ));
            Ok(())
        })
        .unwrap();
        host.answer_next_prompt(Some(&["ok"]));
        assert!(run(&search, &host.invocation(), true).is_err());
        assert_eq!(crate::load("/project").unwrap().active, crate::model::DEFAULT_TASK_ID);
    }
    #[test]
    fn unknown_selection_denial_and_changed_source_fail_without_actions() {
        let (host, search) = setup();
        editor(&host, "unknown-1");
        assert!(run(&search, &host.invocation(), false).is_err());
        editor(&host, "tool-1");
        let original = doc("tool-1");
        host.open_editor("/project/a.txt", "plaintext", "tool-2", &[]);
        assert!(same_source(&host.invocation(), "/project", &original).is_err());
        let host = host.with_capabilities(&["project.read", "storage"]);
        assert!(run(&search, &host.invocation(), false).is_err());
        assert!(host.opened_urls().is_empty() && host.process_runs().is_empty());
    }
    #[test]
    fn dispatch_uses_context_actions_without_legacy_git_workflow() {
        let (host, search) = setup();
        editor(&host, "tool-1");
        let mut plugin = Tasks { git: true, issue_search: search, ..Default::default() };
        host.answer_next_prompt(Some(&["ok"]));
        plugin.run_command("activate-issue-at-caret", &host.invocation(), None).unwrap();
        assert_eq!(crate::load("/project").unwrap().active, "tool-1");
        assert!(host.process_runs().is_empty());
    }
    #[test]
    fn contextual_branch_prepare_and_cancel_are_side_effect_free() {
        let (host, search) = setup();
        editor(&host, "tool-1");
        let mut plugin = Tasks { git: true, issue_search: search, ..Default::default() };
        let before = crate::state::raw("/project").unwrap();
        plugin.run_command("review-issue-branch-at-caret", &host.invocation(), None).unwrap();
        assert!(matches!(plugin.form, Some(crate::Form::CachedOpen { .. })));
        assert_eq!(crate::state::raw("/project").unwrap(), before);
        assert!(host.process_runs().is_empty());
        plugin.button("cancel", &host.invocation()).unwrap();
        assert!(plugin.form.is_none());
        assert_eq!(crate::state::raw("/project").unwrap(), before);
    }
    #[test]
    fn contextual_branch_submit_revalidates_source_and_configuration_before_state_or_git() {
        let (host, search) = setup();
        editor(&host, "tool-1");
        let mut plugin = Tasks { git: true, issue_search: search, ..Default::default() };
        plugin.run_command("review-issue-branch-at-caret", &host.invocation(), None).unwrap();
        let before = crate::state::raw("/project").unwrap();
        host.type_in_editor(" changed");
        assert!(plugin.submit(&host.invocation().project().unwrap(), &host.invocation()).is_err());
        assert_eq!(crate::state::raw("/project").unwrap(), before);
        editor(&host, "tool-1");
        host.set_setting("branchNameFormat", "\"different-{id}\"");
        assert!(plugin.submit(&host.invocation().project().unwrap(), &host.invocation()).is_err());
        assert_eq!(crate::state::raw("/project").unwrap(), before);
        assert!(host.process_runs().is_empty());
    }
    #[test]
    fn contextual_branch_ticket_refuses_foreign_root_and_changed_server() {
        let (host, search) = setup();
        editor(&host, "tool-1");
        let review = prepare(&search, &host.invocation()).unwrap();
        let before = crate::state::raw("/project").unwrap();
        host.set_vcs_remotes(&[("origin", "git@github.com:other/repo.git")]);
        assert!(review.validate(&search, &host.invocation()).is_err());
        host.set_vcs_remotes(&[("origin", "git@github.com:acme/tool.git")]);
        host.set_project_root(Some("/other"));
        assert!(review.validate(&search, &host.invocation()).is_err());
        assert_eq!(crate::state::raw("/project").unwrap(), before);
        assert!(host.process_runs().is_empty());
    }
    #[test]
    fn contextual_branch_requires_opt_in_git_and_a_not_yet_local_identity() {
        let (host, search) = setup();
        editor(&host, "tool-1");
        let mut plugin = Tasks { issue_search: search, ..Default::default() };
        assert!(plugin.run_command("review-issue-branch-at-caret", &host.invocation(), None).is_err());
        assert!(plugin.form.is_none());
        plugin.git = true;
        let review = prepare(&plugin.issue_search, &host.invocation()).unwrap();
        crate::update("/project", |tasks, now| {
            tasks.issues.push(review.issue.clone());
            tasks.activate(&review.issue.id, now)
        }).unwrap();
        let before = crate::state::raw("/project").unwrap();
        assert!(review.validate(&plugin.issue_search, &host.invocation()).is_err());
        assert_eq!(crate::state::raw("/project").unwrap(), before);
        assert!(host.process_runs().is_empty());
    }
    #[test]
    fn contextual_branch_final_review_uses_only_existing_mock_git_workflow() {
        let (host, search) = setup();
        let host = host.with_capabilities(&["project.read", "editor.read", "storage", "vcs.read", "secrets", "net:api.github.com", "process:git"]);
        host.on_process("git", |args, _| {
            let command = args.join(" ");
            Ok(openidea_plugin::ProcessOutput {
                status: Some(if command.starts_with("rev-parse --verify") { 1 } else { 0 }),
                stdout: if command == "rev-parse --abbrev-ref HEAD" { "main\n".into() } else { String::new() },
                stderr: String::new(),
            })
        });
        editor(&host, "tool-1");
        let mut plugin = Tasks { git: true, issue_search: search, ..Default::default() };
        plugin.run_command("review-issue-branch-at-caret", &host.invocation(), None).unwrap();
        assert!(host.process_runs().is_empty());
        plugin.submit(&host.invocation().project().unwrap(), &host.invocation()).unwrap();
        assert!(host.process_runs().iter().any(|r| r.args == ["switch", "-c", "tool-1"]));
        assert_eq!(crate::load("/project").unwrap().active, "tool-1");
        assert_eq!(host.requests().len(), 1);
    }
    #[test]
    fn browser_identity_must_match_api_and_source_id_exactly() {
        let good = Task::issue(
            "github",
            "tool-1",
            "One",
            "https://github.com/acme/tool/issues/1",
            "https://api.github.com/repos/acme/tool/issues/1",
        );
        assert!(closed_identity(&good));
        for url in
            ["https://github.com/other/tool/issues/1", "https://github.com/acme/tool/issues/2", "https://github.com/acme/tool/issues/1?x=1"]
        {
            let mut wrong = good.clone();
            wrong.url = Some(url.into());
            assert!(!closed_identity(&wrong));
        }
        let mut wrong = good.clone();
        wrong.id = "other-1".into();
        assert!(!closed_identity(&wrong));
        for (tracker, id, url, api) in [
            ("youtrack", "APP-1", "https://acme.youtrack.cloud/issue/APP-1", "https://acme.youtrack.cloud/api/issues/APP-1"),
            ("jira", "APP-1", "https://acme.atlassian.net/browse/APP-1", "https://acme.atlassian.net/rest/api/3/issue/APP-1"),
            ("forgejo", "#1", "https://codeberg.org/acme/tool/issues/1", "https://codeberg.org/api/v1/repos/acme/tool/issues/1"),
        ] {
            assert!(closed_identity(&Task::issue(tracker, id, "One", url, api)));
        }
        assert!(!closed_identity(&Task::issue(
            "gitlab",
            "#1",
            "One",
            "https://gitlab.com/acme/tool/-/issues/1",
            "https://gitlab.com/api/v4/projects/123/issues/1"
        )));
    }
}

#[cfg(test)]
mod retained_owner_tests {
    use super::*;
    use crate::Tasks;
    use openidea_plugin::{HttpResponse,Plugin,hover::HoverProvider,mock::MockHost};
    use serde_json::{Value,json};
    fn capture(tracker:&str,body:Value)->(MockHost,Tasks,Option<Task>) {
        let host=MockHost::install().with_capabilities(&["project.read","editor.read","storage","vcs.read","secrets","net:gitlab.com","net:api.trello.com"]);
        if tracker=="gitlab" {host.set_secret("gitlab-token","fixture");host.set_setting("gitlabProject",r#""acme/tool""#);}else{host.set_secret("trello-key","fixture");host.set_secret("trello-token","fixture");}
        host.on_fetch(move |_|Ok(HttpResponse{status:200,headers:vec![],body:body.to_string().into_bytes()}));host.answer_next_prompt(Some(&["bug"]));host.answer_next_prompt(Some(&["0"]));let mut search=Search::default();let issue=search.pick(&host.invocation().project().unwrap(),&host.invocation()).ok().flatten();(host,Tasks{issue_search:search,..Default::default()},issue)
    }
    fn gitlab()->Value {json!([{"iid":7,"project_id":123,"title":"Scoped issue","web_url":"https://gitlab.com/acme/tool/-/issues/7","references":{"full":"acme/tool#7"},"_links":{"self":"https://gitlab.com/api/v4/projects/123/issues/7"}}])}
    fn trello()->Value {json!({"cards":[{"id":"5abbe4b7ddc1b351ef961414","name":"Card","url":"https://trello.com/c/Abcd1234/1-fixture","shortLink":"Abcd1234"}]})}
    fn doc(text:&str)->Document {Document{path:"/project/source.txt".into(),language:"plaintext".into(),text:text.into(),version:1}}
    #[test]
    fn gitlab_numeric_api_owner_is_retained_from_selected_repository_response() {
        let (host,mut plugin,issue)=capture("gitlab",gitlab());let issue=issue.unwrap();assert!(closed_identity(&issue));assert!(issue.source_reference.is_some());
        let source="😀 #7";let before=host.requests().len();let h=plugin.hover(&doc(source),Position::new(0,4)).unwrap();assert_eq!(h.range,Some(Range::new(Position::new(0,3),Position::new(0,5))));assert!(h.markdown.contains("Scoped issue"));
        host.open_editor("/project/source.txt","plaintext",source,&[(0,4,0,4)]);plugin.run_command("open-issue-at-caret",&host.invocation(),None).unwrap();assert_eq!(host.opened_urls(),["https://gitlab.com/acme/tool/-/issues/7"]);assert_eq!(host.requests().len(),before);assert!(host.process_runs().is_empty());
        let restored:Task=serde_json::from_str(&serde_json::to_string(&issue).unwrap()).unwrap();assert!(restored.source_reference.is_none());assert!(!closed_identity(&restored),"serialized tasks cannot mint search-cache provenance");
    }
    #[test]
    fn trello_card_id_and_short_link_use_validated_canonical_browser_identity() {
        let (host,mut plugin,issue)=capture("trello",trello());let issue=issue.unwrap();assert_eq!(issue.url.as_deref(),Some("https://trello.com/c/Abcd1234"));assert!(closed_identity(&issue));let source="5abbe4b7ddc1b351ef961414";let before=host.requests().len();assert!(plugin.hover(&doc(source),Position::new(0,2)).is_some());
        host.open_editor("/project/source.txt","plaintext",source,&[(0,2,0,2)]);host.answer_next_prompt(Some(&[]));plugin.run_command("activate-issue-at-caret",&host.invocation(),None).unwrap();assert_eq!(crate::load("/project").unwrap().active,source);assert_eq!(host.requests().len(),before);assert!(host.process_runs().is_empty());
    }
    #[test]
    fn missing_or_crossed_response_owners_remain_quiet() {
        for body in [ {let mut v=gitlab();v[0]["references"]=json!({});v}, {let mut v=gitlab();v[0]["project_id"]=json!(999);v}, {let mut v=gitlab();v[0]["references"]["full"]=json!("other/tool#7");v} ] {
            let (_host,mut plugin,_)=capture("gitlab",body);assert!(plugin.hover(&doc("#7"),Position::new(0,1)).is_none());
        }
        for body in [ {let mut v=trello();v["cards"][0]["shortLink"]=json!("Wrong123");v}, {let mut v=trello();v["cards"][0]["url"]=json!("https://trello.com/c/Abcd1234?secret=fixture");v}, {let mut v=trello();v["cards"][0]["shortLink"]=Value::Null;v} ] {
            let (_host,mut plugin,_)=capture("trello",body);assert!(plugin.hover(&doc("5abbe4b7ddc1b351ef961414"),Position::new(0,2)).is_none());
        }
    }
    #[test]
    fn retained_identity_cannot_move_to_another_api_or_browser_owner() {
        let (_host,_plugin,issue)=capture("gitlab",gitlab());let mut issue=issue.unwrap();issue.api=Some("https://gitlab.com/api/v4/projects/999/issues/7".into());assert!(!closed_identity(&issue));issue.api=Some("https://gitlab.com/api/v4/projects/123/issues/7".into());issue.url=Some("https://gitlab.com/other/tool/-/issues/7".into());assert!(!closed_identity(&issue));
    }
}
