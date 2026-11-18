//! Pending changesets from an explicit, reviewed `liquibase status --verbose`. Never runs on events.
use crate::*;
use openidea_plugin::ProcessOutput;

pub(crate) const COMMAND: &str = "show-pending";
const NOTES: &str = "liquibase-status";
const EFFECT: &str = "Status lists the changesets not yet applied to the database. It connects with the credentials your Liquibase configuration supplies and changes nothing.";

/// A changeset as `status --verbose` lists it: `path::id::author`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Unrun {
    path: String,
    id: String,
    author: String,
}

/// The unrun changesets, or `None` for output this parser does not recognise.
fn parse(stdout: &str) -> Option<Vec<Unrun>> {
    let mut lines = stdout.lines().map(str::trim);
    while let Some(line) = lines.next() {
        if line.ends_with(" is up to date") {
            return Some(Vec::new());
        }
        let Some((count, _)) = line.split_once(" not been applied to ") else { continue };
        let expected: usize = count.split_whitespace().next()?.parse().ok()?;
        let mut unrun = Vec::new();
        for line in lines.by_ref() {
            let (Some((path, rest)), Some((_, author))) = (line.split_once("::"), line.rsplit_once("::")) else { break };
            let id = rest.rsplit_once("::").map_or("", |(id, _)| id);
            if path.is_empty() || id.is_empty() {
                break;
            }
            unrun.push(Unrun { path: path.into(), id: id.into(), author: author.into() });
        }
        // A short list means the CLI printed something else in between: refuse rather than guess.
        return (unrun.len() == expected).then_some(unrun);
    }
    None
}

/// No raw output: CLI errors can echo the connection URL and credentials.
fn failure(output: &ProcessOutput) -> String {
    let text = format!("{}\n{}", output.stdout, output.stderr).to_lowercase();
    let code = output.status.map_or("stopped".to_owned(), |code| format!("exit {code}"));
    let hint = if text.contains("changelog") && (text.contains("not found") || text.contains("does not exist")) {
        "Liquibase could not find the changelog: check changeLogFile and searchPath in liquibase.properties."
    } else if ["connect", "unknownhost", "timed out", "authentication", "access denied"].iter().any(|word| text.contains(word)) {
        "Liquibase could not reach the database: check that it is running and that the url and credentials Liquibase uses are right."
    } else {
        "Run the Liquibase: status run configuration to see the full output."
    };
    format!("Liquibase status failed ({code}). {hint}")
}

pub(crate) struct Checked {
    root: String,
    properties: Option<String>,
    target: String,
    at: Instant,
    total: usize,
    /// Changelog file → unrun (id, author).
    pending: BTreeMap<String, BTreeSet<(String, String)>>,
    /// Files the run covered for certain: their other changesets are applied.
    covered: BTreeSet<String>,
    /// Every changelog file the result was matched against; a change to one makes it stale.
    files: BTreeSet<String>,
}

#[derive(Default)]
pub(crate) enum State {
    #[default]
    None,
    Checked(Checked),
    Failed { root: String, properties: Option<String>, message: String },
}

/// The unique project changelog whose root-relative path ends with Liquibase's logical path.
fn locate(files: &BTreeSet<String>, root: &str, logical: &str) -> Option<String> {
    let logical = logical.trim_start_matches("classpath:").trim_start_matches('/');
    let suffix = format!("/{logical}");
    let mut hits = files.iter().filter(|file| {
        let relative = root_relative(file, root);
        relative == logical || relative.ends_with(&suffix)
    });
    let hit = hits.next()?;
    hits.next().is_none().then(|| hit.clone())
}

fn ago(at: Instant) -> String {
    match at.elapsed().as_secs() {
        0..60 => "just now".into(),
        seconds @ 60..3600 => format!("{} min ago", seconds / 60),
        seconds => format!("{} h ago", seconds / 3600),
    }
}

impl Liquibase {
    pub(crate) fn show_pending(&mut self, ctx: &Invocation) -> Result<(), Error> {
        let args = vec!["status".to_owned(), "--verbose".to_owned()];
        let Some(reviewed) = execution_review::review(ctx, args.clone(), "Check pending changesets in {db}?", EFFECT)? else { return Ok(()) };
        let project = ctx.project().ok_or(Error::Stale)?;
        self.drop_status();
        // ponytail: blocking run inside the 60 s task budget; move to host::start + Poll if real databases need longer.
        let task = host::begin_task("Liquibase status", false);
        let output = project.run_process("liquibase", &args, None);
        drop(task);
        let unrun = match output {
            Err(Error::Denied(_)) => Err(ui_kit::denied::message("Running Liquibase", "process:liquibase", TITLE)),
            Err(_) => Err("Could not start liquibase. Install the Liquibase CLI and put it on the PATH the IDE uses, then run Show Pending Changesets… again.".to_owned()),
            Ok(output) if output.status != Some(0) => Err(failure(&output)),
            Ok(output) => parse(&output.stdout).ok_or_else(|| "Liquibase status finished, but its output was not recognised; no changeset states are shown. Run the Liquibase: status run configuration to read it.".to_owned()),
        };
        self.status = match unrun {
            Ok(unrun) => State::Checked(self.check(&project, reviewed, unrun)),
            Err(message) => {
                host::notify(NotificationLevel::Warning, TITLE, &message);
                State::Failed { root: reviewed.root, properties: reviewed.properties, message }
            }
        };
        if ui_kit::web::reveal_view("changesets", include_str!("ui_config.json")).is_err() {
            refresh_browser();
        }
        Ok(())
    }

    fn check(&mut self, project: &Project, reviewed: execution_review::Reviewed, unrun: Vec<Unrun>) -> Checked {
        let root = reviewed.root;
        let mut walk = Walk::new(project, &root, self.max_files.unwrap_or(MAX_FILES));
        walk.discover();
        let files: BTreeSet<String> = walk.found.keys().cloned().collect();
        let mut pending: BTreeMap<String, BTreeSet<(String, String)>> = BTreeMap::new();
        for set in &unrun {
            if let Some(file) = locate(&files, &root, &set.path) {
                pending.entry(file).or_default().insert((set.id.clone(), set.author.clone()));
            }
        }
        let mut covered = BTreeSet::new();
        if let Some(master) = reviewed.changelog.as_deref().and_then(|changelog| locate(&files, &root, changelog)) {
            walk.file(&master, None, &mut Vec::new());
            if !walk.partial {
                covered = walk.visited.clone();
            }
        }
        let mut notes = BTreeMap::new();
        for file in &covered {
            if walk.found.get(file).is_some_and(|log| !log.1.change_sets.is_empty()) {
                notes.insert(file.clone(), "up to date".to_owned());
            }
        }
        for (file, sets) in &pending {
            notes.insert(file.clone(), format!("{} pending", sets.len()));
        }
        let target = reviewed.target.unwrap_or_else(|| "the configured database".into());
        let tooltip = format!("From Liquibase status on {target}; run Show Pending Changesets… again after deploying");
        for (file, note) in notes.iter().take(2_000) {
            if host::set_file_note(file, NOTES, note, Some(&tooltip)).is_err() {
                break;
            }
        }
        Checked { root, properties: reviewed.properties, target, at: Instant::now(), total: unrun.len(), pending, covered, files }
    }

    fn drop_status(&mut self) {
        if matches!(std::mem::take(&mut self.status), State::Checked(_)) {
            let _ = host::clear_file_decorations(NOTES);
        }
    }

    /// A changed changelog or liquibase.properties, or a created or removed file (includeAll), makes the result stale.
    pub(crate) fn status_touched(&mut self, path: &str, created_or_removed: bool) {
        let stale = match &self.status {
            State::None => false,
            State::Checked(Checked { root, files, .. }) => {
                files.contains(path) || path.ends_with("/liquibase.properties") || (created_or_removed && path.starts_with(&format!("{root}/")))
            }
            State::Failed { .. } => path.ends_with("/liquibase.properties"),
        };
        if stale {
            self.drop_status();
            refresh_browser();
        }
    }

    /// The status line for `root`'s result; drops a result whose liquibase.properties changed.
    pub(crate) fn status_line(&mut self, project: &Project, root: &str) -> Option<String> {
        let (owner, properties) = match &self.status {
            State::None => return None,
            State::Checked(checked) => (&checked.root, &checked.properties),
            State::Failed { root, properties, .. } => (root, properties),
        };
        if owner != root {
            return None;
        }
        if execution_review::properties(project).ok().as_ref() != Some(properties) {
            self.drop_status();
            return None;
        }
        Some(match &self.status {
            State::Checked(checked) => {
                let plural = if checked.total == 1 { "" } else { "s" };
                format!("{} pending changeset{plural} on {} · checked {}", checked.total, checked.target, ago(checked.at))
            }
            State::Failed { message, .. } => message.clone(),
            State::None => return None,
        })
    }

    pub(crate) fn mark_status(&self, root: &str, items: &mut [TreeItem], sets: &BTreeMap<String, (String, String, String)>) {
        let State::Checked(checked) = &self.status else { return };
        if checked.root != root {
            return;
        }
        for item in items.iter_mut() {
            let Some((file, id, author)) = sets.get(&item.key) else { continue };
            let state = if checked.pending.get(file).is_some_and(|sets| sets.contains(&(id.clone(), author.clone()))) {
                "Pending"
            } else if checked.covered.contains(file) {
                "Applied"
            } else {
                "State unknown"
            };
            item.detail = Some(match &item.detail {
                Some(detail) => format!("{state} · {detail}"),
                None => state.to_owned(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;

    const PROPERTIES: &str = "url=jdbc:postgresql://db.example:5432/app?password=TOKEN_439\nchangeLogFile=db/master.xml\nusername=private\npassword=TOKEN_439\n";
    const PENDING: &str = "1 changeset has not been applied to private@jdbc:postgresql://db.example:5432/app\n     db/changes/002.yaml::2::cy\nLiquibase command 'status' was executed successfully.\n";

    fn output(status: i32, stdout: &str, stderr: &str) -> ProcessOutput {
        ProcessOutput { status: Some(status), stdout: stdout.into(), stderr: stderr.into() }
    }

    fn project() -> MockHost {
        let host = MockHost::install().with_capabilities(&["project.read", "process:liquibase"]);
        host.add_file("liquibase.properties", PROPERTIES);
        host.add_file("db/master.xml", "<databaseChangeLog>\n    <changeSet id=\"1\" author=\"ann\"/>\n    <include file=\"changes/002.yaml\" relativeToChangelogFile=\"true\"/>\n</databaseChangeLog>\n");
        host.add_file("db/changes/002.yaml", "databaseChangeLog:\n  - changeSet:\n      id: 2\n      author: cy\n");
        host.add_file("other/loose.xml", "<databaseChangeLog>\n    <changeSet id=\"9\" author=\"zed\"/>\n</databaseChangeLog>\n");
        host
    }

    fn checked(host: &MockHost, stdout: &'static str) -> Liquibase {
        host.on_process("liquibase", move |_, _| Ok(output(0, stdout, "")));
        let mut plugin = Liquibase::default();
        host.answer_next_prompt(Some(&[]));
        plugin.run_command(COMMAND, &host.invocation(), None).unwrap();
        plugin
    }

    fn notes(host: &MockHost) -> Vec<(String, String)> {
        host.file_notes().into_iter().map(|((_, path), (note, _))| (path, note)).collect()
    }

    fn details(host: &MockHost, plugin: &mut Liquibase) -> (Vec<(String, String)>, String) {
        let view = plugin.render("changesets", None, &host.invocation()).unwrap();
        let Body::Tree(items) = view.body else { panic!("tree") };
        (items.into_iter().filter_map(|item| Some((item.label, item.detail?))).collect(), view.status.unwrap_or_default())
    }

    #[test]
    fn parses_unrun_changesets_up_to_date_and_refuses_unrecognised_output() {
        assert_eq!(parse(PENDING).unwrap(), [Unrun { path: "db/changes/002.yaml".into(), id: "2".into(), author: "cy".into() }]);
        let ids = parse("2 changesets have not been applied to x@y\n  a.sql::create::table::Ann Lee\n  b.yaml::7::bob\n").unwrap();
        assert_eq!((ids[0].id.as_str(), ids[0].author.as_str(), ids[1].path.as_str()), ("create::table", "Ann Lee", "b.yaml"));
        assert_eq!(parse("Starting Liquibase\nx@jdbc:h2:mem:db is up to date\n").unwrap(), []);
        assert!(parse("2 changesets have not been applied to x@y\n  a.sql::1::ann\nWARNING\n").is_none());
        assert!(parse("Unexpected error running Liquibase").is_none());
    }

    #[test]
    fn nothing_runs_without_confirmation() {
        let host = project();
        host.on_process("liquibase", |_, _| Ok(output(0, PENDING, "")));
        let mut plugin = Liquibase::default();
        host.answer_next_prompt(None);
        plugin.run_command(COMMAND, &host.invocation(), None).unwrap();
        host.answer_next_prompt(None);
        plugin.handle("changesets", ViewEvent::Button("browser-status".into()), &host.invocation()).unwrap();
        plugin.render("changesets", None, &host.invocation()).unwrap();
        plugin.handle_events_until(&[openidea_plugin::Event::FileSaved("/project/db/master.xml".into())], Instant::now() + EVENT_BUDGET);
        assert!(host.process_runs().is_empty() && host.terminals().is_empty() && host.file_notes().is_empty());
    }

    #[test]
    fn confirmed_run_marks_pending_applied_and_unknown_and_notes_files() {
        let host = project();
        let mut plugin = checked(&host, PENDING);
        let runs = host.process_runs();
        assert_eq!((runs[0].args.as_slice(), runs[0].cwd.as_deref()), (["status".to_owned(), "--verbose".to_owned()].as_slice(), Some("/project")));
        assert_eq!(notes(&host), [("/project/db/changes/002.yaml".to_owned(), "1 pending".to_owned()), ("/project/db/master.xml".to_owned(), "up to date".to_owned())]);
        let (rows, status) = details(&host, &mut plugin);
        assert!(rows.contains(&("1 (ann)".into(), "Applied".into())), "{rows:?}");
        assert!(rows.contains(&("2 (cy)".into(), "Pending".into())), "{rows:?}");
        assert!(rows.contains(&("9 (zed)".into(), "State unknown".into())), "{rows:?}");
        assert!(status.starts_with("1 pending changeset on jdbc:postgresql://db.example:5432/app · checked just now"), "{status}");
        for secret in ["TOKEN_439", "private", "password"] {
            assert!(!status.contains(secret) && host.notifications().iter().all(|n| !n.body.contains(secret)), "{secret}");
        }
    }

    #[test]
    fn unresolved_changelog_leaves_non_pending_changesets_unknown() {
        let host = project();
        host.add_file("liquibase.properties", "changeLogFile=${CHANGELOG}\n");
        let mut plugin = checked(&host, PENDING);
        assert_eq!(notes(&host), [("/project/db/changes/002.yaml".to_owned(), "1 pending".to_owned())]);
        let (rows, status) = details(&host, &mut plugin);
        assert!(rows.contains(&("1 (ann)".into(), "State unknown".into())), "{rows:?}");
        assert!(status.contains("on the configured database"), "{status}");
    }

    #[test]
    fn saved_changelog_or_changed_properties_make_the_result_stale() {
        let host = project();
        let mut plugin = checked(&host, PENDING);
        plugin.handle_events_until(&[openidea_plugin::Event::FileSaved("/project/db/changes/002.yaml".into())], Instant::now() + EVENT_BUDGET);
        assert!(host.file_notes().is_empty());
        assert!(!details(&host, &mut plugin).1.contains("pending changeset"));

        let mut plugin = checked(&host, PENDING);
        host.add_file("liquibase.properties", "url=jdbc:postgresql://other/app\nchangeLogFile=db/master.xml\n");
        assert!(!details(&host, &mut plugin).1.contains("pending changeset"));
        assert!(host.file_notes().is_empty());
        assert_eq!(host.process_runs().len(), 2, "never re-runs on its own");
    }

    #[test]
    fn failed_or_denied_runs_explain_clear_old_notes_and_never_echo_output() {
        let host = project();
        let mut plugin = checked(&host, PENDING);
        assert!(!host.file_notes().is_empty());
        host.on_process("liquibase", |_, _| Ok(output(1, "", "Connection refused: jdbc:postgresql://db.example/app?password=TOKEN_439")));
        host.answer_next_prompt(Some(&[]));
        plugin.run_command(COMMAND, &host.invocation(), None).unwrap();
        assert!(host.file_notes().is_empty());
        let message = host.notifications().last().unwrap().body.clone();
        assert!(message.contains("could not reach the database") && !message.contains("TOKEN_439"), "{message}");
        assert!(details(&host, &mut plugin).1.starts_with("Liquibase status failed (exit 1)"));

        let denied = MockHost::install().with_capabilities(&["project.read"]);
        denied.add_file("liquibase.properties", PROPERTIES);
        let mut plugin = Liquibase::default();
        denied.answer_next_prompt(Some(&[]));
        plugin.run_command(COMMAND, &denied.invocation(), None).unwrap();
        assert!(denied.notifications().last().unwrap().body.contains("process:liquibase"));
        assert!(denied.file_notes().is_empty());
    }
}
