//! Opt-in background Git observations. Managed output cannot prove inventory completeness.
use openidea_plugin::{Error, FileKind, Invocation, PickItem, Project, host};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};
const MAX_OUTPUT: usize = 512 * 1024;
const MAX_PATHS: usize = 2048;
const TTL: Duration = Duration::from_secs(120);
const DEADLINE: Duration = Duration::from_secs(4);
#[derive(Clone, PartialEq, Eq)]
struct Scope {
    root: String,
    branch: Option<String>,
    commit: Option<String>,
    head: String,
    index: Option<openidea_plugin::FileStat>,
    ignore: String,
    exclude: String,
}
fn invalid(s: &str) -> Error {
    Error::Invalid(s.into())
}
fn text(project: &Project, path: &str, max: usize) -> Result<String, Error> {
    match project.read_file(path) {
        Ok(s) if s.len() <= max => Ok(s),
        Err(Error::NotFound(_)) => Ok(String::new()),
        Err(e) => Err(e),
        _ => Err(invalid("Git inventory source exceeds its byte bound.")),
    }
}
fn valid_root(root: &str) -> bool {
    root.starts_with('/') && root != "/" && !root.ends_with('/') && !(root.contains("//") || root.contains('\\'))
        && root.len() <= 512 && !root.chars().any(char::is_control)
        && !root.split('/').any(|s| matches!(s,"."|".."))
}
fn scope(project: &Project) -> Result<Scope, Error> {
    let root = project.root()?;
    if !valid_root(&root) {
        return Err(invalid("Git inventory supports a concrete Unix project/repository root of at most 512 bytes."));
    }
    let repository = project.vcs_status()?.ok_or("No repository status is available.")?;
    if repository.root != root || project.stat(&format!("{root}/.git"))?.kind != FileKind::Directory {
        return Err(invalid(
            "Select the exact root of one normal .git-directory repository; worktrees/submodules/subdirectory projects are unsupported.",
        ));
    }
    let index = match project.stat(&format!("{root}/.git/index")) {
        Ok(s) if s.kind == FileKind::File => Some(s),
        Err(Error::NotFound(_)) => None,
        Err(e) => return Err(e),
        _ => return Err(invalid("The Git index is not a regular file.")),
    };
    Ok(Scope {
        head: text(project, &format!("{root}/.git/HEAD"), 4096)?,
        ignore: text(project, &format!("{root}/.gitignore"), 128 * 1024)?,
        exclude: text(project, &format!("{root}/.git/info/exclude"), 64 * 1024)?,
        root,
        branch: repository.branch,
        commit: repository.commit,
        index,
    })
}
fn command(root: &str) -> host::Command {
    let dir = format!("{root}/.git");
    let mut command = host::Command::new("git")
        .args(["--no-pager", "--no-optional-locks", "-C", root])
        .arg(&format!("--git-dir={dir}"))
        .arg(&format!("--work-tree={root}"))
        .args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.untrackedCache=false",
            "-c",
            "core.excludesFile=/dev/null",
            "ls-files",
            "--others",
            "--ignored",
            "--exclude-standard",
            "--full-name",
            "-z",
            "--",
        ]);
    for (key, value) in [
        ("GIT_DIR", dir.clone()),
        ("GIT_WORK_TREE", root.into()),
        ("GIT_COMMON_DIR", dir.clone()),
        ("GIT_INDEX_FILE", format!("{dir}/index")),
        ("GIT_OBJECT_DIRECTORY", format!("{dir}/objects")),
        ("GIT_ALTERNATE_OBJECT_DIRECTORIES", String::new()),
        ("GIT_CONFIG_NOSYSTEM", "1".into()),
        ("GIT_CONFIG_GLOBAL", "/dev/null".into()),
        ("GIT_CONFIG_COUNT", "0".into()),
        ("GIT_CONFIG_PARAMETERS", String::new()),
        ("GIT_OPTIONAL_LOCKS", "0".into()),
        ("GIT_TERMINAL_PROMPT", "0".into()),
    ] {
        command = command.env(key, &value);
    }
    for key in [
        "GIT_TRACE",
        "GIT_TRACE_SETUP",
        "GIT_TRACE_PERFORMANCE",
        "GIT_TRACE_PACK_ACCESS",
        "GIT_TRACE_PACKET",
        "GIT_TRACE_CURL",
        "GIT_TRACE2",
        "GIT_TRACE2_EVENT",
        "GIT_TRACE2_PERF",
        "GIT_TRACE_REFS",
        "GIT_TRACE_SHALLOW",
        "GIT_TRACE_WORKING_TREE_ENCODING",
        "GIT_TRACE_FSMONITOR",
    ] {
        command = command.env(key, "0");
    }
    command
}
trait Process {
    fn wait(&self, _ms: u32) -> Result<host::Chunk, Error> {
        self.poll()
    }
    fn poll(&self) -> Result<host::Chunk, Error>;
    fn close(&self) -> Result<(), Error>;
    fn kill(&self);
}
impl Process for host::Child {
    fn wait(&self, ms: u32) -> Result<host::Chunk, Error> {
        self.wait(ms)
    }
    fn poll(&self) -> Result<host::Chunk, Error> {
        self.poll(64 * 1024)
    }
    fn close(&self) -> Result<(), Error> {
        self.close_stdin()
    }
    fn kill(&self) {
        let _ = self.kill();
    }
}
struct Job {
    scope: Scope,
    child: Box<dyn Process>,
    task: host::Task,
    at: Instant,
    bytes: usize,
    paths: BTreeSet<String>,
    exited: Option<i32>,
    empty: u8,
}
struct Snapshot {
    // Only successful-exit Partial output carries capture proof. Never EOF/completeness proof.
    scope: Option<Scope>,
    at: Instant,
    paths: Vec<String>,
    state: &'static str,
    reason: String,
}
#[derive(Default)]
pub struct State {
    job: Option<Job>,
    snapshots: BTreeMap<String, Snapshot>,
}
fn owner(ctx: &Invocation) -> Result<(Project, String), Error> {
    let project = ctx.project().ok_or("Open the owning project first.")?;
    let root = project.root()?;
    if !valid_root(&root) || !host::projects()?.iter().any(|p|p.root().is_ok_and(|open|open==root)) { return Err(Error::Stale); }
    Ok((project, root))
}
pub(crate) struct PositiveSamples {
    pub root: String,
    pub paths: Vec<String>,
    pub observed: usize,
    scope: Scope,
    at: Instant,
}
impl PositiveSamples {
    pub fn validate(&self, project: &Project) -> Result<(), Error> {
        if self.at.elapsed() >= TTL || scope(project)? != self.scope { return Err(Error::Stale); }
        Ok(())
    }
}
impl State {
    pub(crate) fn positive_samples(&self, ctx: &Invocation) -> Result<PositiveSamples, Error> {
        let (project, root) = owner(ctx)?;
        if self.job.as_ref().is_some_and(|job| job.scope.root == root) { return Err(invalid("Wait for Ignore: Scan Git for Ignored Files… to finish first.")); }
        let snapshot = self.snapshots.get(&root).ok_or_else(|| invalid("No Git scan yet. Run Ignore: Scan Git for Ignored Files… first."))?;
        if snapshot.state != "Partial" || snapshot.at.elapsed() >= TTL { return Err(Error::Stale); }
        let proof = snapshot.scope.as_ref().ok_or_else(|| invalid("The last Git scan stopped early, so its files can't be marked. Run Ignore: Scan Git for Ignored Files… again."))?;
        let result = PositiveSamples { root, paths: snapshot.paths.iter().take(crate::file_hints::MAX_FILES).cloned().collect(), observed: snapshot.paths.len(), scope: proof.clone(), at: snapshot.at };
        result.validate(&project)?;
        Ok(result)
    }

    pub fn start(&mut self, ctx: &Invocation) -> Result<Option<String>, Error> {
        let result =
            self.start_with(ctx, |cmd, project| host::start(cmd, Some(project)).map(|child| Box::new(child) as Box<dyn Process>))?;
        self.collect();
        Ok(result)
    }
    fn collect(&mut self) {
        // No unbounded spin: each native first read waits at most 100 ms; at
        // most 40 rounds, with the wall-clock deadline checked every round.
        for _ in 0..40 {
            if self.job.is_none() {
                return;
            }
            self.poll(true);
        }
        if let Some(job) = self.job.take() {
            job.child.kill();
            self.bounded_samples(&job, "Collection poll budget reached; incomplete positive observations only.");
        }
    }
    fn start_with(
        &mut self,
        ctx: &Invocation,
        spawn: impl FnOnce(&host::Command, &Project) -> Result<Box<dyn Process>, Error>,
    ) -> Result<Option<String>, Error> {
        if self.job.is_some() {
            return Err(invalid("A Git inventory is already running. Cancel it in its owning project first."));
        }
        let (project, root) = owner(ctx)?;
        let current = scope(&project)?;
        if !self.snapshots.contains_key(&root) && self.snapshots.len() >= 8 {
            return Err(invalid("Eight project inventories are retained. Clear an unneeded project's cache first."));
        }
        let cmd = command(&root);
        let preview = format!(
            "Start read-only background Git observations?\n\nProject/repository: {root}\nHEAD: {}\nCWD: {root}\nProgram: git\nExact argv: {}\nAdded environment (inherited collisions refuse launch): {}\n\nGit reads on-disk repository ignore files; unsaved editor rules are not applied. Global/system excludes and trace targets are disabled. No hooks/fsmonitor, shell, aliases, helpers, writes or network are requested. Four-second cooperative deadline; 512 KiB output/2048 paths. Output transport can lose/truncate data, so results are ALWAYS PARTIAL and cannot justify unused/absence warnings. No automatic rescan follows completion.",
            current.commit.as_deref().unwrap_or("(unborn/detached unknown)"),
            serde_json::to_string(&cmd.args).unwrap_or_default(),
            serde_json::to_string(&cmd.env).unwrap_or_default()
        );
        if !ui_kit::prompts::confirm_exact("Enumerate Git ignored-file observations", &preview)? {
            return Ok(None);
        }
        if ctx.project().and_then(|p| p.root().ok()).as_deref() != Some(&root)
            || scope(&project)? != current
            || !host::projects()?.iter().any(|p| p.root().ok().as_deref() == Some(&root))
        {
            return Err(Error::Stale);
        }
        let started = Instant::now();
        let child = match spawn(&cmd, &project) {
            Ok(child) => child,
            Err(error) => {
                self.failed(&root, &format!("Launch refused: {error}"));
                return Err(error);
            }
        };
        if let Err(error) = child.close() {
            child.kill();
            self.failed(&root, "Cannot close child stdin; previous samples retained.");
            return Err(error);
        }
        self.job = Some(Job {
            scope: current,
            child,
            task: host::begin_task("Observing Git ignored files (partial only)", true),
            at: started,
            bytes: 0,
            paths: BTreeSet::new(),
            exited: None,
            empty: 0,
        });
        crate::refresh_templates();
        Ok(None)
    }
    fn failed(&mut self, root: &str, reason: &str) {
        self.retain_failure(root, reason);
        crate::refresh_templates();
    }
    fn retain_failure(&mut self, root: &str, reason: &str) {
        let entry = self.snapshots.entry(root.into()).or_insert(Snapshot {
            scope: None,
            at: Instant::now(),
            paths: vec![],
            state: "Partial",
            reason: String::new(),
        });
        entry.state = if entry.paths.is_empty() { "Partial" } else { "Stale" };
        entry.reason = reason.chars().take(512).collect();
    }
    fn bounded_samples(&mut self, job: &Job, reason: &str) {
        if self.snapshots.get(&job.scope.root).is_some_and(|s| !s.paths.is_empty()) {
            self.failed(&job.scope.root, reason);
        } else {
            self.snapshots.insert(
                job.scope.root.clone(),
                Snapshot {
                    scope: None,
                    at: Instant::now(),
                    paths: job.paths.iter().take(MAX_PATHS).cloned().collect(),
                    state: "Partial",
                    reason: reason.into(),
                },
            );
            crate::refresh_templates();
        }
    }
    #[cfg(test)]
    pub fn cancel(&mut self, ctx: &Invocation) -> Result<Option<String>, Error> {
        let (_, root) = owner(ctx)?;
        if self.job.as_ref().is_some_and(|j| j.scope.root == root) {
            let job = self.job.take().unwrap();
            job.child.kill();
            self.failed(&root, "Cancelled; previous observations retained. No absence conclusions.");
        } else {
            return Err(invalid("This project has no running Git inventory."));
        }
        Ok(None)
    }
    pub fn clear(&mut self, ctx: &Invocation) -> Result<Option<String>, Error> {
        let (_, root) = owner(ctx)?;
        if self.job.as_ref().is_some_and(|j| j.scope.root == root) {
            return Err(invalid("Cancel this project's running inventory before clearing it."));
        }
        self.snapshots.remove(&root);
        crate::refresh_templates();
        Ok(None)
    }
    pub fn invalidate(&mut self, path: &str) {
        let root = self.job.as_ref().filter(|j| owned(&j.scope.root, path)).map(|j| j.scope.root.clone());
        let mut changed = false;
        if let Some(root) = root {
            if let Some(job) = self.job.take() { job.child.kill(); }
            self.retain_failure(&root, "Repository/file event changed the source scope while running.");
            changed = true;
        }
        for (root, s) in &mut self.snapshots {
            if (owned(root, path) || root == path)
                && (s.state != "Stale" || s.reason != "Repository/files changed since these observations.") {
                s.state = "Stale";
                s.reason = "Repository/files changed since these observations.".into();
                changed = true;
            }
        }
        if changed { crate::refresh_templates(); }
    }
    #[cfg(test)]
    pub fn tick(&mut self) {
        self.poll(false);
    }
    fn poll(&mut self, wait: bool) {
        let Some(mut job) = self.job.take() else {
            return;
        };
        let root = job.scope.root.clone();
        let project = host::projects().ok().and_then(|p| p.into_iter().find(|p| p.root().ok().as_deref() == Some(&root)));
        let valid = project.as_ref().is_some_and(|p| scope(p).is_ok_and(|s| s == job.scope));
        if !valid || job.task.is_cancelled() || job.at.elapsed() >= DEADLINE {
            job.child.kill();
            self.failed(
                &root,
                if !valid {
                    "Project/repository/ignore source changed or became unavailable."
                } else if job.task.is_cancelled() {
                    "Cancelled; previous observations retained."
                } else {
                    "Cooperative deadline reached; previous observations retained."
                },
            );
            return;
        }
        for round in 0..4 {
            let chunk = match if wait && round == 0 { job.child.wait(100) } else { job.child.poll() } {
                Ok(c) => c,
                Err(_) => {
                    job.child.kill();
                    self.failed(&root, "Managed output failed; previous observations retained.");
                    return;
                }
            };
            job.bytes = job.bytes.saturating_add(chunk.stdout.len()).saturating_add(chunk.stderr.len());
            if job.bytes > MAX_OUTPUT || !chunk.stderr.is_empty() || chunk.stdout.contains('\u{fffd}') {
                job.child.kill();
                self.bounded_samples(&job, "Output/encoding bound or stderr encountered; intact earlier samples only, never totals.");
                return;
            }
            if let Err(error) = observe(&chunk.stdout, &mut job.paths) {
                job.child.kill();
                self.bounded_samples(&job, &error.to_string());
                return;
            }
            if let Some(status) = chunk.status {
                job.exited = Some(status);
            }
            if job.exited.is_some() && chunk.stdout.is_empty() {
                job.empty += 1;
            } else {
                job.empty = 0;
            }
            if job.empty >= 2 {
                if !project.as_ref().is_some_and(|p| scope(p).is_ok_and(|s| s == job.scope)) {
                    job.child.kill();
                    self.failed(&root, "Source scope changed before collection finished.");
                    return;
                }
                if job.exited == Some(0) {
                    self.snapshots.insert(root.clone(),Snapshot{scope:Some(job.scope.clone()),at:Instant::now(),paths:job.paths.into_iter().collect(),state:"Partial",reason:"Git exited 0; stream EOF/truncation cannot be proved. Intact interior records only; no totals/absence conclusions.".into()});
                    crate::refresh_templates();
                } else {
                    self.failed(&root, "Git did not exit successfully; previous observations retained.");
                }
                return;
            }
            if chunk.stdout.is_empty() {
                break;
            }
        }
        self.job = Some(job);
    }
    pub fn summary(&self, ctx: &Invocation) -> String {
        let Some(root) = ctx.project().and_then(|p| p.root().ok()) else {
            return "Git observations: project unavailable".into();
        };
        if self.job.as_ref().is_some_and(|j| j.scope.root == root) {
            return if self.job.as_ref().is_some_and(|j| j.at.elapsed() >= DEADLINE) {
                "Git observations: Running cleanup pending (deadline elapsed; incomplete)".into()
            } else {
                "Git observations: Running (partial-only; review shows any previous samples; native progress cancellation available)".into()
            };
        }
        let Some(s) = self.snapshots.get(&root) else {
            return "Git observations: Not scanned. Run Ignore: Scan Git for Ignored Files…".into();
        };
        format!(
            "Git observations: {} · {} observed samples · {}",
            if s.at.elapsed() >= TTL { "Stale" } else { s.state },
            s.paths.len(),
            s.reason
        )
    }
    pub fn review(&mut self, ctx: &Invocation) -> Result<Option<String>, Error> {
        let (project, root) = owner(ctx)?;
        // Explicit command only: rendering/summary never reads VCS or source files.
        let drifted = self.snapshots.get(&root).filter(|s|s.state!="Stale").and_then(|s|s.scope.as_ref())
            .is_some_and(|captured| !scope(&project).is_ok_and(|current| &current==captured));
        if drifted {
            self.retain_failure(&root,"Captured repository/HEAD/index/root-ignore/exclude scope changed or became unavailable; samples are historical.");
            crate::refresh_templates();
        }
        let summary = self.summary(ctx);
        let snapshot = self.snapshots.get(&root);
        if snapshot.is_none() && !self.job.as_ref().is_some_and(|job| job.scope.root == root) {
            return Err(invalid("No Git scan for this project yet. Run Ignore: Scan Git for Ignored Files… first."));
        }
        let paths = snapshot.map(|s| s.paths.as_slice()).unwrap_or(&[]);
        let mut rows = vec![PickItem::new("status", &summary)];
        if let Some(snapshot) = snapshot {
            let provenance = match snapshot.scope.as_ref() {
                Some(captured) => format!("Historical capture: HEAD {}; snapshot {}. Root-ignore/exclude/index proof only; no full recursive-source or current-policy proof.", captured.commit.as_deref().unwrap_or("unknown").chars().filter(|c|!c.is_control()).take(80).collect::<String>(),snapshot.state),
                None => "The scan stopped early; these files can be listed but not marked.".into(),
            };
            rows.push(PickItem::new("provenance",&provenance));
        }
        if let Some(editor) = ctx.editor().filter(|e| e.path() == format!("{root}/.gitignore")) {
            let doc = editor.document()?;
            if doc.text.len() <= 128 * 1024 {
                for (line, rule) in doc
                    .text
                    .lines()
                    .enumerate()
                    .filter_map(|(n, s)| crate::matcher::Rule::parse(s).map(|r| (n, r)))
                    .filter(|(_, r)| !r.negated && r.is_literal() && r.pattern.len() <= 256)
                    .take(128)
                {
                    let count = paths.iter().filter(|p| rule.matches(p, false)).count();
                    rows.push(PickItem::new(
                        &format!("rule:{line}"),
                        &format!("Line {}: {} — {count} observed lexical matches (0 is unknown)", line + 1, rule.pattern),
                    ));
                }
            }
        }
        rows.extend(paths.iter().take(256).enumerate().map(|(i, p)| PickItem::new(&format!("path:{i}"), p)));
        let _ = host::pick("Read-only partial Git observations (not totals or unused-rule proof)", &rows)?;
        Ok(None)
    }
}
fn owned(root: &str, path: &str) -> bool {
    path == root || path.strip_prefix(root.trim_end_matches('/')).is_some_and(|rest| rest.starts_with('/'))
}
fn observe(chunk: &str, paths: &mut BTreeSet<String>) -> Result<(), Error> {
    // Proc silently drops a buffer prefix. Never join fragments across polls;
    // after the first NUL, each terminated interior field is an intact path.
    let Some(first) = chunk.find('\0') else {
        return Ok(());
    };
    let rest = &chunk[first + 1..];
    for part in rest.split_inclusive('\0').filter_map(|part| part.strip_suffix('\0')) {
        if part.is_empty() {
            continue;
        }
        if part.len() > 1024
            || part.starts_with('/')
            || part.contains('\\')
            || part.chars().any(char::is_control)
            || part.split('/').any(|p| p.is_empty() || matches!(p, "." | ".."))
        {
            return Err(invalid("Unsupported Git path record; observations remain partial."));
        }
        paths.insert(part.into());
        if paths.len() > MAX_PATHS {
            return Err(invalid("Observed path count exceeded 2048."));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    use openidea_plugin::{Event, Plugin, RepositoryStatus};
    use std::{cell::RefCell, collections::VecDeque, rc::Rc};
    struct Fake {
        chunks: RefCell<VecDeque<host::Chunk>>,
        closed: Rc<RefCell<u32>>,
        killed: Rc<RefCell<u32>>,
    }
    impl Process for Fake {
        fn poll(&self) -> Result<host::Chunk, Error> {
            Ok(self.chunks.borrow_mut().pop_front().unwrap_or(host::Chunk { status: Some(0), ..Default::default() }))
        }
        fn close(&self) -> Result<(), Error> {
            *self.closed.borrow_mut() += 1;
            Ok(())
        }
        fn kill(&self) {
            *self.killed.borrow_mut() += 1;
        }
    }
    fn setup() -> MockHost {
        let h = MockHost::install().with_capabilities(&[
            "project.read",
            "vcs.read",
            "process:git",
            "editor.read",
            "storage",
            "events",
            "editor.write",
        ]);
        h.add_dir(".git");
        h.add_file(".git/HEAD", "ref: refs/heads/main\n");
        h.add_file(".gitignore", "target/\n");
        h.set_vcs_status(Some(RepositoryStatus {
            root: "/project".into(),
            branch: Some("main".into()),
            commit: Some("abcdef".into()),
            ..Default::default()
        }));
        h
    }
    fn start(s: &mut State, h: &MockHost, chunks: Vec<host::Chunk>) -> (Rc<RefCell<u32>>, Rc<RefCell<u32>>) {
        h.answer_next_prompt(Some(&["ok"]));
        let close = Rc::new(RefCell::new(0));
        let kill = Rc::new(RefCell::new(0));
        let c = close.clone();
        let k = kill.clone();
        s.start_with(&h.invocation(), |cmd, project| {
            assert_eq!(project.root().unwrap(), "/project");
            assert_eq!(cmd.program, "git");
            assert!(cmd.args.windows(2).any(|a| a == ["-C", "/project"]));
            assert!(cmd.args.contains(&"--git-dir=/project/.git".into()));
            assert!(cmd.env.contains(&("GIT_INDEX_FILE".into(), "/project/.git/index".into())));
            Ok(Box::new(Fake { chunks: RefCell::new(chunks.into()), closed: c, killed: k }))
        })
        .unwrap();
        (close, kill)
    }
    #[test]
    fn exact_readonly_root_command_refuses_inherited_scope_values_by_sdk_contract() {
        let c = command("/project");
        assert!(!c.args.iter().any(|a| a.starts_with('!') || a == "-p"));
        assert!(c.args.contains(&"core.fsmonitor=false".into()));
        assert!(c.args.contains(&"--no-optional-locks".into()));
        assert_eq!(c.env.iter().find(|(k, _)| k == "GIT_WORK_TREE").unwrap().1, "/project");
        assert!(c.env.contains(&("GIT_CONFIG_COUNT".into(), "0".into())));
        assert!(c.env.contains(&("GIT_TRACE2_EVENT".into(), "0".into())));
    }
    #[test]
    fn background_results_are_partial_and_no_negative_coverage_claims_exist() {
        let h = setup();
        let mut s = State::default();
        let (closed, killed) = start(
            &mut s,
            &h,
            vec![host::Chunk { stdout: "first\0target/a.class\0target/b.class\0tail".into(), status: Some(0), ..Default::default() }],
        );
        assert_eq!(*closed.borrow(), 1);
        assert!(s.summary(&h.invocation()).contains("Running"));
        s.tick();
        s.tick();
        assert!(s.job.is_none());
        assert_eq!(*killed.borrow(), 0);
        assert_eq!(s.snapshots["/project"].paths, ["target/a.class", "target/b.class"]);
        assert!(s.summary(&h.invocation()).contains("Partial"));
        assert!(!s.summary(&h.invocation()).contains("Complete"));
        h.open_editor("/project/.gitignore", "ignore", "target/\nunknown/\n", &[]);
        h.answer_next_prompt(None);
        s.review(&h.invocation()).unwrap();
        assert!(h.diagnostics().is_empty());
    }
    #[test]
    fn chunks_never_stitch_synthetic_paths_and_solo_output_makes_no_absence_statement() {
        let mut paths = BTreeSet::new();
        observe("lostprefix\0real/file\0split", &mut paths).unwrap();
        observe("suffix\0another/file\0", &mut paths).unwrap();
        assert_eq!(paths.into_iter().collect::<Vec<_>>(), ["another/file", "real/file"]);
        let mut paths = BTreeSet::new();
        observe("sole/file\0", &mut paths).unwrap();
        assert!(paths.is_empty());
        assert!(observe("x\0../escape\0", &mut paths).is_err());
        assert!(observe("x\0/absolute\0", &mut paths).is_err());
    }
    #[test]
    fn timeout_cancel_and_scope_changes_kill_child_and_keep_last_observations_stale() {
        let h = setup();
        let mut s = State::default();
        s.snapshots.insert(
            "/project".into(),
            Snapshot { scope: None, at: Instant::now(), paths: vec!["target/old".into()], state: "Partial", reason: "previous".into() },
        );
        let (_, killed) = start(&mut s, &h, vec![]);
        s.job.as_mut().unwrap().at = Instant::now() - DEADLINE;
        s.tick();
        assert_eq!(*killed.borrow(), 1);
        assert!(s.summary(&h.invocation()).contains("Stale"));
        assert_eq!(s.snapshots["/project"].paths, ["target/old"]);
        let (_, killed) = start(&mut s, &h, vec![]);
        h.cancel_tasks();
        s.tick();
        assert_eq!(*killed.borrow(), 1);
        let (_, killed) = start(&mut s, &h, vec![]);
        h.add_file(".gitignore", "changed/\n");
        s.tick();
        assert_eq!(*killed.borrow(), 1);
    }
    #[test]
    fn project_switch_denial_and_failed_launch_never_use_anonymous_or_other_root_state() {
        let h = setup();
        let mut s = State::default();
        let (_, killed) = start(&mut s, &h, vec![]);
        h.set_project_root(Some("/other"));
        assert!(s.cancel(&h.invocation()).is_err());
        assert_eq!(*killed.borrow(), 0);
        assert!(s.summary(&h.invocation()).contains("Not scanned"));
        s.tick();
        assert_eq!(*killed.borrow(), 1);
        h.set_project_root(Some("/project"));
        let h = h.with_capabilities(&["project.read", "vcs.read"]);
        h.answer_next_prompt(Some(&["ok"]));
        assert!(matches!(s.start(&h.invocation()), Err(Error::Denied(_))));
        assert!(s.job.is_none());
        h.set_project_root(None);
        assert!(s.start(&h.invocation()).is_err());
    }
    #[test]
    fn output_count_and_encoding_caps_never_become_complete() {
        let h = setup();
        let mut s = State::default();
        let stdout = format!("drop\0{}", (0..MAX_PATHS + 1).map(|i| format!("target/{i}\0")).collect::<String>());
        let (_, kill) = start(&mut s, &h, vec![host::Chunk { stdout, status: Some(0), ..Default::default() }]);
        s.tick();
        assert_eq!(*kill.borrow(), 1);
        assert_eq!(s.snapshots["/project"].paths.len(), MAX_PATHS);
        assert_eq!(s.snapshots["/project"].state, "Partial");
        let (_, kill) = start(&mut s, &h, vec![host::Chunk { stdout: "\u{fffd}".into(), status: Some(0), ..Default::default() }]);
        s.tick();
        assert_eq!(*kill.borrow(), 1);
        assert_eq!(s.snapshots["/project"].state, "Stale");
    }
    #[test]
    fn render_and_provider_do_not_spawn_or_poll_and_project_events_invalidate_only_owner() {
        let h = setup();
        let mut p = crate::IgnorePlugin::default();
        let (_, kill) = start(&mut p.inventory, &h, vec![]);
        use openidea_plugin::providers::{Document, Providers};
        use openidea_plugin::views::Views;
        p.render("templates", None, &h.invocation()).unwrap();
        p.code_actions(
            &Document { path: "/project/.gitignore".into(), language: "ignore".into(), text: "target/\n".into(), version: 1 },
            openidea_plugin::Range::new(openidea_plugin::Position::new(0, 0), openidea_plugin::Position::new(0, 0)),
        );
        assert!(p.inventory.job.is_some());
        assert_eq!(*kill.borrow(), 0);
        p.handle_events(&[Event::VcsChanged("/elsewhere".into())], &h.invocation());
        assert_eq!(*kill.borrow(), 0);
        p.handle_events(&[Event::VcsChanged("/project".into())], &h.invocation());
        assert_eq!(*kill.borrow(), 1);
    }
    #[test]
    fn no_scheduler_work_is_claimed_and_collection_keeps_its_explicit_owner() {
        let h = setup();
        h.add_project("/project");
        let mut p = crate::IgnorePlugin::default();
        start(&mut p.inventory, &h, vec![host::Chunk { stdout: "drop\0target/a\0".into(), status: Some(0), ..Default::default() }]);
        h.set_project_root(None);
        p.handle_events(&[Event::Scheduled], &h.invocation());
        p.handle_events(&[Event::Scheduled], &h.invocation());
        assert!(p.inventory.job.is_some());
        p.inventory.collect();
        assert!(p.inventory.job.is_none());
        assert_eq!(p.inventory.snapshots["/project"].paths, ["target/a"]);
        assert!(p.inventory.summary(&h.invocation()).contains("project unavailable"));
    }
    #[test]
    fn cancel_before_launch_and_expired_samples_do_not_silently_restart() {
        let h = setup();
        let mut s = State::default();
        h.answer_next_prompt(None);
        s.start_with(&h.invocation(), |_, _| panic!("cancel must not spawn")).unwrap();
        assert!(s.job.is_none());
        s.snapshots.insert(
            "/project".into(),
            Snapshot { scope: None, at: Instant::now() - TTL, paths: vec!["target/old".into()], state: "Partial", reason: "older observation".into() },
        );
        assert!(s.summary(&h.invocation()).contains("Stale"));
        s.tick();
        assert!(s.job.is_none());
    }
    #[test]
    fn native_progress_cancel_and_stalled_collection_have_bounded_reads_and_owned_cleanup() {
        struct Stall {
            reads: Rc<RefCell<u32>>,
            kills: Rc<RefCell<u32>>,
        }
        impl Process for Stall {
            fn poll(&self) -> Result<host::Chunk, Error> {
                *self.reads.borrow_mut() += 1;
                Ok(host::Chunk::default())
            }
            fn close(&self) -> Result<(), Error> {
                Ok(())
            }
            fn kill(&self) {
                *self.kills.borrow_mut() += 1;
            }
        }
        let h = setup();
        let mut state = State::default();
        let (_, kill) = start(&mut state, &h, vec![]);
        h.cancel_tasks();
        state.collect();
        assert_eq!(*kill.borrow(), 1);
        assert!(state.job.is_none());
        let reads = Rc::new(RefCell::new(0));
        let kills = Rc::new(RefCell::new(0));
        let r = reads.clone();
        let k = kills.clone();
        h.answer_next_prompt(Some(&["ok"]));
        state.start_with(&h.invocation(), |_, _| Ok(Box::new(Stall { reads: r, kills: k }))).unwrap();
        state.collect();
        assert!(*reads.borrow() <= 40);
        assert_eq!(*kills.borrow(), 1);
        assert!(state.job.is_none());
        assert!(state.summary(&h.invocation()).contains("Partial"));
    }
    #[test]
    fn pinned_sdk_managed_child_runs_only_under_mock_and_cannot_create_completeness() {
        let h = setup();
        h.answer_next_prompt(Some(&["ok"]));
        let mut state = State::default();
        state.start(&h.invocation()).unwrap();
        assert!(state.job.is_none());
        assert_eq!(state.snapshots["/project"].state, "Partial");
        assert!(state.snapshots["/project"].paths.is_empty());
        assert!(h.requests().is_empty() && h.process_runs().is_empty());
        let h = h.with_capabilities(&["project.read", "process:git"]);
        assert!(matches!(State::default().start(&h.invocation()), Err(Error::Denied(_))));
    }
    fn refresh_messages(h: &MockHost) -> Vec<(String,String)> {
        h.view_messages().into_iter().filter(|(_,message)|message==r#"{"channel":"ui-kit-refresh"}"#).collect()
    }
    #[test]
    fn partial_completion_and_public_clear_notify_without_render_collection() {
        use openidea_plugin::views::Views;
        let h=setup();let mut state=State::default();
        start(&mut state,&h,vec![host::Chunk{stdout:"drop\0target/a\0".into(),status:Some(0),..Default::default()}]);
        assert_eq!(refresh_messages(&h).len(),1);state.tick();state.tick();
        assert_eq!(refresh_messages(&h).len(),2);assert_eq!(state.snapshots["/project"].state,"Partial");
        let mut plugin=crate::IgnorePlugin{inventory:state,..Default::default()};
        plugin.run_command("git-inventory-clear",&h.invocation(),None).unwrap();assert_eq!(refresh_messages(&h).len(),3);
        assert!(plugin.inventory.summary(&h.invocation()).contains("Not scanned"));
        plugin.render("templates",None,&h.invocation()).unwrap();assert_eq!(refresh_messages(&h).len(),3);
        assert!(h.process_runs().is_empty()&&h.requests().is_empty());
        assert!(refresh_messages(&h).iter().all(|(view,message)|view=="templates"&&message==r#"{"channel":"ui-kit-refresh"}"#));
    }
    #[test]
    fn launch_failure_notifies_retained_stale_positive_samples() {
        let h=setup();let mut state=State::default();
        start(&mut state,&h,vec![host::Chunk{stdout:"drop\0target/a\0".into(),status:Some(0),..Default::default()}]);state.tick();state.tick();
        let before=refresh_messages(&h).len();h.answer_next_prompt(Some(&["ok"]));
        assert!(state.start_with(&h.invocation(),|_,_|Err(invalid("deterministic launch refusal"))).is_err());
        assert_eq!(refresh_messages(&h).len(),before+1);assert_eq!(state.snapshots["/project"].paths,["target/a"]);assert_eq!(state.snapshots["/project"].state,"Stale");
    }
    #[test]
    fn existing_own_root_vcs_retirement_emits_one_invalidation_and_repeats_are_quiet() {
        let h=setup();let mut plugin=crate::IgnorePlugin::default();
        let (_,kill)=start(&mut plugin.inventory,&h,vec![]);let before=refresh_messages(&h).len();
        plugin.handle_events(&[Event::VcsChanged("/elsewhere".into())],&h.invocation());assert_eq!(*kill.borrow(),0);assert_eq!(refresh_messages(&h).len(),before);
        plugin.handle_events(&[Event::VcsChanged("/project".into())],&h.invocation());assert_eq!(*kill.borrow(),1);assert_eq!(refresh_messages(&h).len(),before+1);
        plugin.handle_events(&[Event::VcsChanged("/project".into()),Event::FileSaved("/project/.gitignore".into())],&h.invocation());assert_eq!(refresh_messages(&h).len(),before+1);
        assert!(plugin.inventory.summary(&h.invocation()).contains("Stale"));
    }
    #[test]
    fn clearing_another_current_project_never_drops_original_samples_and_refusal_is_quiet() {
        let h=setup();let mut state=State::default();
        start(&mut state,&h,vec![host::Chunk{stdout:"drop\0target/a\0".into(),status:Some(0),..Default::default()}]);state.tick();state.tick();
        h.set_project_root(Some("/other"));state.clear(&h.invocation()).unwrap();assert_eq!(state.snapshots["/project"].paths,["target/a"]);
        let before=refresh_messages(&h).len();h.set_project_root(None);assert!(state.clear(&h.invocation()).is_err());assert_eq!(refresh_messages(&h).len(),before);
        h.set_project_root(Some("/project"));start(&mut state,&h,vec![]);let before=refresh_messages(&h).len();assert!(state.clear(&h.invocation()).is_err());assert_eq!(refresh_messages(&h).len(),before);
    }

    #[test]
    fn explicit_review_revalidates_successful_capture_after_missed_source_events() {
        for mode in 0..5 {
            let h=setup();let mut state=State::default();
            start(&mut state,&h,vec![host::Chunk{stdout:"lost\0target/old\0".into(),status:Some(0),..Default::default()}]);
            state.tick();state.tick();assert!(state.summary(&h.invocation()).contains("Partial"));
            match mode {
                0=>h.add_file(".gitignore","other/\n"),
                1=>h.add_file(".git/HEAD","ref: refs/heads/other\n"),
                2=>h.add_file(".git/index","new index metadata"),
                3=>h.set_vcs_status(Some(RepositoryStatus{root:"/project".into(),branch:Some("other".into()),commit:Some("def456".into()),..Default::default()})),
                _=>h.add_file(".git/info/exclude","private/\n"),
            }
            // No file/VCS event was delivered; cached rendering remains source-I/O free.
            assert!(state.summary(&h.invocation()).contains("Partial"));
            let before=refresh_messages(&h).len();h.answer_next_prompt(None);state.review(&h.invocation()).unwrap();
            assert!(state.summary(&h.invocation()).contains("Stale"),"mode {mode}");
            assert_eq!(state.snapshots["/project"].paths,["target/old"]);
            assert_eq!(refresh_messages(&h).len(),before+1);
            assert!(h.process_runs().is_empty()&&h.requests().is_empty()&&h.diagnostics().is_empty());
        }
    }
    #[test]
    fn unavailable_review_proof_keeps_historical_samples_and_repeat_invalidation_is_quiet() {
        let h=setup();let mut state=State::default();
        start(&mut state,&h,vec![host::Chunk{stdout:"lost\0target/old\0".into(),status:Some(0),..Default::default()}]);state.tick();state.tick();
        let h=h.with_capabilities(&["project.read","editor.read","storage","events"]);
        assert!(state.summary(&h.invocation()).contains("Partial"));
        let before=refresh_messages(&h).len();state.review(&h.invocation()).unwrap();
        assert_eq!(state.snapshots["/project"].state,"Stale");
        assert_eq!(state.snapshots["/project"].paths,["target/old"]);
        assert_eq!(refresh_messages(&h).len(),before+1);state.review(&h.invocation()).unwrap();
        assert_eq!(refresh_messages(&h).len(),before+1);
        h.set_project_root(Some("/other"));assert!(state.review(&h.invocation()).is_err());
        assert_eq!(state.snapshots["/project"].paths,["target/old"]);
        assert!(h.process_runs().is_empty()&&h.requests().is_empty());
    }
    #[test]
    fn explicit_inventory_commands_refuse_non_normalized_root_identities() {
        let h=setup();let mut state=State::default();
        for root in ["/","/project/","/project//nested","/project/../other","/project\\other"] {
            h.set_project_root(Some(root));
            assert!(state.clear(&h.invocation()).is_err(),"{root}");
        }
        assert!(h.process_runs().is_empty()&&h.requests().is_empty());
    }

    fn hints_plugin(h: &MockHost, names: &[&str]) -> crate::IgnorePlugin {
        for name in names { h.add_file(name, "owned"); }
        let mut inventory=State::default();
        let output=format!("discard\0{}",names.iter().map(|p|format!("{p}\0")).collect::<String>());
        start(&mut inventory,h,vec![host::Chunk{stdout:output,status:Some(0),..Default::default()}]);
        inventory.tick(); inventory.tick();
        assert!(inventory.snapshots["/project"].scope.is_some());
        let mut plugin=crate::IgnorePlugin{inventory,..Default::default()};
        plugin.activate(&openidea_plugin::Context::default()).unwrap();
        plugin
    }
    #[test]
    fn full_plugin_hints_publish_only_successful_partial_positive_facts_without_a_process() {
        let h=setup();let mut p=hints_plugin(&h,&["target/a.class","target/b.class"]);
        let status=p.run_command("publish-git-sample-hints",&h.invocation(),None).unwrap().unwrap();
        assert!(status.contains("2 historical positive") && status.contains("partial"));
        let hints=h.file_decorations();assert_eq!(hints.len(),2);
        assert!(hints.keys().all(|(_,path)|path.starts_with("/project/target/")));
        for value in hints.values(){assert_eq!(value.style,"muted");assert_eq!(value.badge.as_deref(),Some("I"));let tip=value.tooltip.as_ref().unwrap();assert!(tip.contains("last scan")&&tip.contains("may be stale")&&tip.contains("not the current Git status"));}
        assert!(h.process_runs().is_empty());
        p.run_command("clear-git-sample-hints",&h.invocation(),None).unwrap();assert!(h.file_decorations().is_empty());
    }
    #[test]
    fn stale_unproven_running_or_failed_inventory_never_publishes_and_refresh_withdraws() {
        for kind in 0..5 {
            let h=setup();let mut p=hints_plugin(&h,&["target/a"]);
            p.run_command("publish-git-sample-hints",&h.invocation(),None).unwrap();
            match kind {
                0 => p.inventory.snapshots.get_mut("/project").unwrap().at-=TTL,
                1 => p.inventory.snapshots.get_mut("/project").unwrap().scope=None,
                2 => p.inventory.invalidate("/project/anything"),
                3 => p.inventory.failed("/project","failed later scan"),
                _ => { start(&mut p.inventory,&h,vec![]); },
            }
            assert!(p.run_command("publish-git-sample-hints",&h.invocation(),None).is_err());
            assert!(h.file_decorations().is_empty());
        }
    }
    #[test]
    fn revalidated_scope_changes_missing_paths_and_other_project_owners_are_quiet() {
        for kind in 0..5 {
            let h=setup();let mut p=hints_plugin(&h,&["target/a"]);
            match kind {
                0 => h.add_file(".gitignore","different/"),
                1 => h.add_file(".git/HEAD","ref: refs/heads/other"),
                2 => h.add_project("/project/target"),
                3 => h.set_project_root(Some("/other")),
                _ => {p.inventory.snapshots.get_mut("/project").unwrap().paths=vec!["missing".into()];},
            }
            let result=p.run_command("publish-git-sample-hints",&h.invocation(),None);
            if kind<4 {assert!(result.is_err());} else {assert!(result.unwrap().unwrap().starts_with("0 historical"));}
            assert!(h.file_decorations().is_empty());assert!(h.process_runs().is_empty());
        }
    }
    #[test]
    fn bounded_file_publication_and_changed_event_cleanup_preserve_other_plugin_features() {
        let h=setup();let names:Vec<_>=(0..40).map(|i|format!("target/f{i:02}")).collect();let refs:Vec<_>=names.iter().map(String::as_str).collect();let mut p=hints_plugin(&h,&refs);
        p.run_command("publish-git-sample-hints",&h.invocation(),None).unwrap();assert_eq!(h.file_decorations().len(),16);
        p.handle_events(&[Event::FilesChanged(Default::default())],&h.invocation());assert!(h.file_decorations().is_empty());
        p.run_command("publish-git-sample-hints",&h.invocation(),None).unwrap();
        p.deactivate();assert!(h.file_decorations().is_empty());
        assert!(p.run_command("publish-git-sample-hints",&h.invocation(),None).is_err());
        p.activate(&openidea_plugin::Context::default()).unwrap();
        p.run_command("publish-git-sample-hints",&h.invocation(),None).unwrap();
        h.set_project_root(Some("/other"));p.handle_events(&[Event::EditorFocused(None)],&h.invocation());assert!(h.file_decorations().is_empty());
        assert!(h.process_runs().is_empty());
    }
    #[test]
    fn preferences_settings_activation_and_unavailable_events_withdraw_hints() {
        use openidea_plugin::views::{Views,ViewEvent};
        let h=setup();let mut p=hints_plugin(&h,&["target/a"]);
        p.run_command("publish-git-sample-hints",&h.invocation(),None).unwrap();
        let _=p.handle(crate::preferences::VIEW,ViewEvent::Button("preferences".into()),&h.invocation());
        assert!(h.file_decorations().is_empty());
        p.run_command("publish-git-sample-hints",&h.invocation(),None).unwrap();
        p.handle_events(&[Event::Other{kind:"settings-changed".into(),data:"{}".into()}],&h.invocation());
        assert!(h.file_decorations().is_empty());
        p.run_command("publish-git-sample-hints",&h.invocation(),None).unwrap();
        p.activate(&openidea_plugin::Context::default()).unwrap();assert!(h.file_decorations().is_empty());
        p.file_hints.configure(false);
        assert!(p.run_command("publish-git-sample-hints",&h.invocation(),None).is_err());
        assert!(h.file_decorations().is_empty() && h.process_runs().is_empty());
    }

    #[test]
    fn denied_vcs_or_absent_snapshot_clear_before_any_positive_publication() {
        let h=setup();let mut p=hints_plugin(&h,&["target/a"]);
        p.run_command("publish-git-sample-hints",&h.invocation(),None).unwrap();
        let h=h.with_capabilities(&["project.read","events"]);
        assert!(p.run_command("publish-git-sample-hints",&h.invocation(),None).is_err());
        assert!(h.file_decorations().is_empty());
        p.inventory.snapshots.clear();
        assert!(p.run_command("publish-git-sample-hints",&h.invocation(),None).is_err());
        assert!(h.file_decorations().is_empty() && h.process_runs().is_empty());
    }

}
