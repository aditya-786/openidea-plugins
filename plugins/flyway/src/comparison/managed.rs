//! Managed comparison core: snapshot, diff and diffText stages run offline (schema model and
//! snapshot files only) with the installed Flyway CLI that `offline_profile` detects. A
//! user-entered version is never a runtime permit.
use super::*;
use openidea_plugin::host::{Child, Chunk, Command};
const CAP: usize = 1 << 20;
pub(super) const BLOCKED: &str = "Install the Flyway CLI so flyway runs from this project, then prepare the comparison again.";
/// The installed CLI the stages run, and the project configuration it reads from the working directory.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Profile {
    pub edition: String,
    pub version: String,
    pub config: Option<String>,
    pub locations: Vec<String>,
}
/// Detects `flyway --version` through `process:flyway`, the same program the stages launch.
pub(super) fn offline_profile(project: &Project) -> Result<Profile, Error> {
    let _task = host::begin_task("Checking the Flyway CLI", false);
    let failed = |reason: &str| Error::from(format!("Flyway CLI check failed: {reason}").as_str());
    let output = match project.run_process("flyway", &["--version".to_owned()], None) {
        Ok(output) => output,
        Err(Error::Denied(_)) => return Err("Allow running flyway for Flyway Migrations in Settings › Plugins to start a comparison.".into()),
        Err(Error::NotFound(_)) => return Err(BLOCKED.into()),
        // The real host may report a missing program as Invalid.
        Err(Error::Invalid(reason)) if ["no such file", "not found"].iter().any(|missing| reason.to_ascii_lowercase().contains(missing)) => {
            return Err(BLOCKED.into());
        }
        Err(Error::Invalid(reason) | Error::Failed(reason)) => return Err(failed(&reason)),
        Err(error) => return Err(failed(&error.to_string())),
    };
    if output.status != Some(0) {
        let code = output.status.map_or("was stopped".to_owned(), |code| format!("exited with {code}"));
        return Err(failed(&format!("flyway --version {code}.")));
    }
    let banner = output.stdout.lines().find_map(|line| line.trim().strip_prefix("Flyway ")?.split_once(" Edition "));
    let unrecognised = || failed("flyway --version did not print an edition and version.");
    let Some((edition, rest)) = banner else { return Err(unrecognised()) };
    let version = rest.split_whitespace().next().filter(|version| version.len() <= 64).ok_or_else(unrecognised)?;
    if edition.is_empty() || edition.len() > 64 || edition.chars().chain(version.chars()).any(char::is_control) {
        return Err(unrecognised());
    }
    let config = config::CONFIG_FILES.iter().find(|file| project.exists(file).unwrap_or(false)).map(|file| (*file).to_owned());
    Ok(Profile { edition: edition.into(), version: version.into(), config, locations: config::load(project).locations })
}
#[derive(Clone)]
pub(super) struct Plan {
    root: String,
    model: String,
    target: String,
    source: String,
    artifact: String,
    inputs: Vec<(String, String)>,
}
impl Plan {
    pub fn capture(project: &Project, d: &Draft) -> Result<Self, Error> {
        for p in [&d.model, &d.source, &d.artifact] {
            path(p)?;
        }
        if d.target != "empty" {
            path(&d.target)?;
        }
        let paths = [&d.model, &d.source, &d.artifact, &d.report];
        if paths
            .iter()
            .enumerate()
            .any(|(i, p)| paths[..i].contains(p))
            || (d.target != "empty" && paths.contains(&&d.target))
        {
            return Err("Use distinct comparison paths.".into());
        }
        if [&d.source, &d.artifact].iter().any(|output| {
            output.starts_with(&format!("{}/", d.model))
                || d.model.starts_with(&format!("{output}/"))
        }) {
            return Err("Comparison outputs cannot overlap the schema model input tree.".into());
        }
        if stat(project, &d.model)?.kind != openidea_plugin::FileKind::Directory {
            return Err("Schema model must be an owned directory.".into());
        }
        vacant_output(project, &d.source)?;
        vacant_output(project, &d.artifact)?;
        let root = project.root()?;
        let mut plan = Self {
            root,
            model: d.model.clone(),
            target: d.target.clone(),
            source: d.source.clone(),
            artifact: d.artifact.clone(),
            inputs: Vec::new(),
        };
        plan.inputs = plan.input_snapshot(project)?;
        Ok(plan)
    }
    fn input_snapshot(&self, project: &Project) -> Result<Vec<(String, String)>, Error> {
        if project.root()? != self.root {
            return Err(Error::Stale);
        }
        let mut paths = project.find_files(&format!("{}/**", self.model), 65)?;
        if paths.is_empty() || paths.len() > 64 {
            return Err("Use a nonempty schema model with at most 64 text files.".into());
        }
        paths.sort();
        paths.dedup();
        let prefix = format!("{}/{}/", self.root.trim_end_matches('/'), self.model);
        let mut out = Vec::new();
        let mut bytes = 0;
        for absolute in paths {
            if !absolute.starts_with(&prefix) {
                return Err(Error::Stale);
            }
            let relative = absolute
                .strip_prefix(&format!("{}/", self.root.trim_end_matches('/')))
                .ok_or(Error::Stale)?;
            let text = bounded_read(project, relative)?;
            bytes += text.len();
            if bytes > CAP {
                return Err("Schema model exceeds the 1 MiB input proof budget.".into());
            }
            out.push((relative.into(), text));
        }
        if self.target != "empty" {
            out.push((self.target.clone(), bounded_read(project, &self.target)?));
        }
        Ok(out)
    }
    fn fresh(&self, project: &Project) -> Result<(), Error> {
        if self.input_snapshot(project)? != self.inputs {
            return Err(Error::Stale);
        }
        Ok(())
    }
    pub fn revalidate_prepare(&self, project: &Project) -> Result<(), Error> {
        self.fresh(project)?;
        vacant_output(project, &self.source)?;
        vacant_output(project, &self.artifact)
    }
    fn affected(&self, changed: &str) -> bool {
        [&self.model, &self.target]
            .iter()
            .filter(|p| p.as_str() != "empty")
            .any(|p| {
                let absolute = format!("{}/{}", self.root.trim_end_matches('/'), p);
                !changed.is_empty()
                    && (changed == absolute
                        || changed.starts_with(&format!("{absolute}/"))
                        || absolute.starts_with(&format!("{}/", changed.trim_end_matches('/'))))
            })
    }
    fn args(&self, step: usize) -> Vec<String> {
        let mut args = match step {
            0 => vec![
                "snapshot".into(),
                "-snapshot.source=schemaModel".into(),
                format!("-schemaModelLocation={}", self.model),
                format!("-snapshot.filename={}", self.source),
            ],
            1 => vec![
                "diff".into(),
                format!("-diff.source=snapshot:{}", self.source),
                format!(
                    "-diff.target={}",
                    if self.target == "empty" {
                        "empty".into()
                    } else {
                        format!("snapshot:{}", self.target)
                    }
                ),
                format!("-diff.artifactFilename={}", self.artifact),
            ],
            _ => vec![
                "diffText".into(),
                format!("-diffText.artifactFilename={}", self.artifact),
            ],
        };
        args.push("-outputType=json".into());
        args.push(format!("-workingDirectory={}", self.root));
        args
    }
    pub fn review(&self) -> String {
        format!(
            "Project: {}\nSchema model: {} text files\nPreparing checks the installed Flyway CLI with flyway --version.\nStages, each started by you and checked before the next:\n{}\nThe stages read the schema model and snapshot files only and never connect to a database. Cancelling keeps partial snapshot and diff files. Select definitions from the new report; earlier change IDs are not reused.",
            self.root,
            self.inputs.len(),
            serde_json::to_string_pretty(&[self.args(0), self.args(1), self.args(2)])
                .unwrap_or_default(),
        )
    }
}
fn bounded_read(project: &Project, path: &str) -> Result<String, Error> {
    let st = stat(project, path)?;
    if st.kind != openidea_plugin::FileKind::File || st.size == 0 || st.size > CAP as u64 {
        return Err(
            "Comparison input/output must be a nonempty owned text file up to 1 MiB.".into(),
        );
    }
    let text = read(project, path)?;
    if text.len() > CAP {
        return Err("Comparison text exceeds 1 MiB.".into());
    }
    Ok(text)
}
#[derive(Default)]
pub(super) struct Core {
    plan: Option<Plan>,
    profile: Option<Profile>,
    child: Option<Child>,
    step: usize,
    ready: bool,
    raw: String,
    outputs: Vec<(String, String)>,
    pub report: Option<String>,
    pub status: String,
}
impl Core {
    pub fn running(&self) -> bool {
        self.child.is_some()
    }
    pub fn prepared(&self) -> bool {
        self.plan.is_some()
    }
    /// Start is offered once an installed CLI profile exists and no stage has run yet.
    pub fn startable(&self) -> bool {
        self.profile.is_some() && self.step == 0 && !self.running()
    }
    pub fn continuable(&self) -> bool {
        self.ready && !self.running()
    }
    pub fn prepare(&mut self, plan: Plan, profile: Result<Profile, Error>) -> Result<(), Error> {
        if self.running() {
            return Err("Stop the running comparison first.".into());
        }
        let profile = profile.and_then(|profile| {
            if profile.edition.eq_ignore_ascii_case("Community") {
                return Err(format!("Ready check: Flyway {} Edition {} cannot run snapshot or diff; Flyway Teams or Enterprise is required.", profile.edition, profile.version).into());
            }
            Ok(profile)
        });
        let status = match &profile {
            Ok(profile) => format!(
                "Ready with Flyway {} Edition {} (configuration: {}; locations: {}). Start runs the snapshot stage.",
                profile.edition,
                profile.version,
                profile.config.as_deref().unwrap_or("none"),
                profile.locations.join(", ")
            ),
            Err(error) => error.to_string(),
        };
        *self = Self {
            plan: Some(plan),
            profile: profile.ok(),
            status,
            ..Self::default()
        };
        Ok(())
    }
    pub fn start(&mut self, project: &Project) -> Result<(), Error> {
        self.plan
            .as_ref()
            .ok_or("Prepare the managed comparison first.")?
            .fresh(project)?;
        // Only a CLI detected at prepare time admits a launch; reported fields never do.
        if self.profile.is_none() {
            return Err(BLOCKED.into());
        }
        self.launch_admitted(project)
    }
    fn fresh(&self, project: &Project) -> Result<(), Error> {
        let plan = self.plan.as_ref().ok_or(Error::Stale)?;
        plan.fresh(project)?;
        for (path, text) in &self.outputs {
            if bounded_read(project, path)? != *text {
                return Err(Error::Stale);
            }
        }
        Ok(())
    }
    // Called after `start` admitted the detected profile; tests drive it directly.
    fn launch_admitted(&mut self, project: &Project) -> Result<(), Error> {
        if self.running() {
            return Err("A comparison is already running.".into());
        }
        if self.step >= 3 {
            return Err("Comparison already finished.".into());
        }
        self.fresh(project)?;
        let plan = self.plan.as_ref().unwrap();
        if self.step == 0 {
            vacant_output(project, &plan.source)?;
            vacant_output(project, &plan.artifact)?;
        }
        if self.step == 1 {
            vacant_output(project, &plan.artifact)?;
        }
        let child = host::start(
            &Command::new("flyway").args(plan.args(self.step)),
            Some(project),
        )?;
        self.child = Some(child);
        self.ready = false;
        self.raw.clear();
        if self.child.as_ref().unwrap().close_stdin().is_err() {
            self.status = "Stdin close failed; cancel the retained owned handle.".into();
            return Err("Comparison stdin closure failed.".into());
        }
        self.status = format!(
            "Stage {} running; Check explicitly. Output is private until validated.",
            self.step + 1
        );
        Ok(())
    }
    pub fn next(&mut self, project: &Project) -> Result<(), Error> {
        if !self.ready {
            return Err("Check the completed stage before continuing.".into());
        }
        self.start(project)
    }
    pub fn check(&mut self, project: &Project) -> Result<(), Error> {
        if let Err(error) = self.fresh(project) {
            let _ = self.cancel();
            return Err(error);
        }
        for _ in 0..4 {
            let chunk = match self
                .child
                .as_ref()
                .ok_or("No comparison child is running.")?
                .poll(4096)
            {
                Ok(chunk) => chunk,
                Err(error) => {
                    self.status = "Polling failed; owned handle retained, outcome unknown.".into();
                    return Err(error);
                }
            };
            match self.ingest(project, chunk) {
                Ok(true) => break,
                Ok(false) => {}
                Err(error) => {
                    if self.status.contains("running;") {
                        self.status =
                            "Stage result rejected; no comparison report accepted.".into();
                    }
                    return Err(error);
                }
            }
        }
        Ok(())
    }
    fn ingest(&mut self, project: &Project, chunk: Chunk) -> Result<bool, Error> {
        if self.raw.len() + chunk.stdout.len() + chunk.stderr.len() > CAP {
            self.cancel()?;
            self.status = "Comparison output cap reached; no result accepted.".into();
            return Err("Comparison output exceeds 1 MiB.".into());
        }
        // Neither stderr nor arbitrary JSON/logs are displayed; they may contain
        // ambient secrets. Stage definitions are the only eventual preview data.
        if !chunk.stderr.is_empty() {
            self.cancel()?;
            self.status = "Comparison stderr received; no result accepted.".into();
            return Err("Comparison reported stderr; inspect the native CLI separately.".into());
        }
        self.raw.push_str(&chunk.stdout);
        let Some(status) = chunk.status else {
            return Ok(false);
        };
        if !chunk.stdout.is_empty() {
            return Ok(false);
        }
        self.child.take();
        if status != 0 {
            self.status = format!(
                "Stage {} failed with exit {status}; partial outputs retained, no rollback claimed.",
                self.step + 1
            );
            return Err("Comparison stage failed.".into());
        }
        self.fresh(project)?;
        let plan = self.plan.as_ref().unwrap();
        if self.step < 2 {
            let path = if self.step == 0 {
                &plan.source
            } else {
                &plan.artifact
            };
            let text = bounded_read(project, path)?;
            self.outputs.push((path.clone(), text));
        } else {
            parse(&self.raw)?;
            self.report = Some(self.raw.clone());
        }
        self.step += 1;
        self.ready = self.step < 3;
        self.status = if self.ready {
            format!(
                "Stage {} completed. Continue runs the next stage.",
                self.step
            )
        } else {
            "Completed diffText parsed; select raw definitions explicitly. No migration/deployment SQL inferred.".into()
        };
        Ok(true)
    }
    pub fn cancel(&mut self) -> Result<(), Error> {
        if let Some(child) = &self.child {
            if let Err(error) = child.kill() {
                self.status = "Cancellation failed; owned handle retained, outcome unknown.".into();
                return Err(error);
            }
            self.child.take();
        }
        self.ready = false;
        self.raw.clear();
        self.report = None;
        self.status =
            "Owned comparison cancelled; partial files retained, no rollback/cleanup claimed."
                .into();
        Ok(())
    }
    pub fn invalidate(&mut self, changed: &[&str]) -> bool {
        if !self
            .plan
            .as_ref()
            .is_some_and(|p| changed.iter().any(|path| p.affected(path)))
        {
            return false;
        }
        if self.cancel().is_ok() {
            self.status =
                "Comparison input changed; reprepare before execution or selection.".into();
        }
        true
    }
    pub fn membership(&mut self) {
        if !self.running() {
            return;
        }
        let root = self.plan.as_ref().map(|p| p.root.as_str()).unwrap_or("");
        let roots = host::projects().and_then(|projects| {
            projects
                .iter()
                .map(Project::root)
                .collect::<Result<Vec<_>, _>>()
        });
        match roots {
            Ok(roots) if !roots.iter().any(|r| r == root) => {
                if self.cancel().is_ok() {
                    self.status = "Owning project closed; partial files retained.".into();
                }
            }
            Err(_) => {
                self.status =
                    "Project membership unavailable; retained owned child, outcome unknown.".into()
            }
            _ => {}
        }
    }
    pub fn report_fresh(&self, project: &Project, source: &str) -> Result<(), Error> {
        self.fresh(project)?;
        if self.report.as_deref() != Some(source) {
            return Err(Error::Stale);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    fn setup() -> (MockHost, Review) {
        let host =
            MockHost::install().with_capabilities(&["project.read", "events", "process:flyway", "storage"]);
        host.add_dir("schema-model");
        host.add_dir("review");
        host.add_file("schema-model/a.sql", "CREATE TABLE a(id int);");
        let mut review = Review::default();
        review.begin(&host.invocation()).unwrap();
        (host, review)
    }
    fn core(host: &MockHost, review: &Review) -> Core {
        let project = host.invocation().project().unwrap();
        let plan = Plan::capture(&project, review.draft.as_ref().unwrap()).unwrap();
        let mut core = Core::default();
        core.prepare(plan, Err(BLOCKED.into())).unwrap();
        core
    }
    fn done(core: &mut Core, project: &Project, stdout: &str) -> Result<bool, Error> {
        if !stdout.is_empty() {
            assert!(!core.ingest(
                project,
                Chunk {
                    stdout: stdout.into(),
                    status: Some(0),
                    ..Chunk::default()
                }
            )?);
        }
        core.ingest(
            project,
            Chunk {
                status: Some(0),
                ..Chunk::default()
            },
        )
    }
    #[test]
    fn real_registered_readiness_cannot_promote_self_reported_profile_to_spawn() {
        let (host, _review) = setup();
        let mut plugin = Flyway::default();
        plugin
            .run_command("review-comparison", &host.invocation(), None)
            .unwrap();
        let token = plugin.comparison.draft.as_ref().unwrap().token.clone();
        for (key, value) in [("version", "13.12.1"), ("edition", "Enterprise")] {
            openidea_plugin::views::Views::handle(
                &mut plugin,
                VIEW,
                ViewEvent::Field(format!("{key}@{token}"), value.into()),
                &host.invocation(),
            )
            .unwrap();
        }
        host.answer_next_prompt(Some(&["yes"]));
        openidea_plugin::views::Views::handle(
            &mut plugin,
            VIEW,
            ViewEvent::Button(format!("prepare-managed@{token}")),
            &host.invocation(),
        )
        .unwrap();
        assert_eq!(plugin.comparison.draft.as_ref().unwrap().managed.status, BLOCKED);
        for key in ["start-managed", "continue-managed"] {
            assert!(
                openidea_plugin::views::Views::handle(
                    &mut plugin,
                    VIEW,
                    ViewEvent::Button(format!("{key}@{token}")),
                    &host.invocation()
                )
                .is_err()
            );
        }
        assert!(!plugin.comparison.draft.as_ref().unwrap().managed.running());
        assert!(host.process_runs().iter().all(|run| run.args == ["--version"]) && host.requests().is_empty());
    }
    /// Profile contract `Core::start` relies on: the stages run `flyway <stage> … -outputType=json
    /// -workingDirectory=<root>` through `process:flyway`, reading only the schema model and snapshot
    /// files (no database URL), so the profile must prove that this exact `flyway` program is installed
    /// (its `--version` banner gives edition and version) and record the project configuration the CLI
    /// reads from the working directory (config file and migration locations) for the review.
    #[test]
    fn offline_profile_from_installed_cli() {
        let (host, _review) = setup();
        host.add_file("flyway.toml", "[flyway]\nlocations = [\"filesystem:sql\"]\n");
        host.on_process("flyway", |args, _| {
            assert_eq!(args, ["--version"]);
            Ok(openidea_plugin::ProcessOutput { status: Some(0), stdout: "\nFlyway Community Edition 10.x by Redgate\n".into(), stderr: String::new() })
        });
        let project = host.invocation().project().unwrap();
        let profile = offline_profile(&project).unwrap();
        assert_eq!(profile.edition, "Community");
        assert_eq!(profile.version, "10.x");
        assert_eq!(profile.config.as_deref(), Some("flyway.toml"));
        assert_eq!(profile.locations, ["filesystem:sql"]);
        assert!(!host.tasks().is_empty(), "the JVM start-up runs inside a task");
        host.on_process("flyway", |_, _| Err(Error::NotFound("flyway".into())));
        assert_eq!(offline_profile(&project).unwrap_err().to_string(), BLOCKED);
        host.on_process("flyway", |_, _| Err(Error::Invalid("flyway: No such file or directory".into())));
        assert_eq!(offline_profile(&project).unwrap_err().to_string(), BLOCKED, "the real host may report a missing program as Invalid");
        host.on_process("flyway", |_, _| Err(Error::Failed("spawn timed out".into())));
        assert_eq!(offline_profile(&project).unwrap_err().to_string(), "Flyway CLI check failed: spawn timed out");
        host.on_process("flyway", |_, _| Ok(openidea_plugin::ProcessOutput { status: Some(1), stdout: "Flyway Community Edition 10.x by Redgate\n".into(), stderr: "ERROR: license".into() }));
        assert_eq!(offline_profile(&project).unwrap_err().to_string(), "Flyway CLI check failed: flyway --version exited with 1.");
        host.on_process("flyway", |_, _| Ok(openidea_plugin::ProcessOutput { status: Some(0), stdout: "Picked up JAVA_TOOL_OPTIONS\nversion 10\n".into(), stderr: String::new() }));
        assert_eq!(offline_profile(&project).unwrap_err().to_string(), "Flyway CLI check failed: flyway --version did not print an edition and version.");
        let denied = host.with_capabilities(&["project.read", "events", "storage"]);
        assert!(offline_profile(&denied.invocation().project().unwrap()).unwrap_err().to_string().contains("Allow running flyway for Flyway Migrations in Settings › Plugins"));
    }
    #[test]
    fn start_enabled_with_profile() {
        let (host, _review) = setup();
        let mut plugin = Flyway::default();
        let start_button = |plugin: &mut Flyway| {
            let view = openidea_plugin::views::Views::render(plugin, VIEW, None, &host.invocation()).unwrap();
            view.toolbar.into_iter().find(|button| button.id.starts_with("start-managed@"))
        };
        let prepare = |plugin: &mut Flyway| {
            plugin.run_command("review-comparison", &host.invocation(), None).unwrap();
            let token = plugin.comparison.draft.as_ref().unwrap().token.clone();
            let runs = host.process_runs().len();
            host.answer_next_prompt(None);
            openidea_plugin::views::Views::handle(plugin, VIEW, ViewEvent::Button(format!("prepare-managed@{token}")), &host.invocation()).unwrap();
            assert_eq!(host.process_runs().len(), runs, "a declined review never runs flyway --version");
            host.answer_next_prompt(Some(&[]));
            openidea_plugin::views::Views::handle(plugin, VIEW, ViewEvent::Button(format!("prepare-managed@{token}")), &host.invocation()).unwrap();
            token
        };
        let token = prepare(&mut plugin);
        assert!(start_button(&mut plugin).is_none(), "no installed CLI, no Start");
        assert_eq!(plugin.comparison.draft.as_ref().unwrap().managed.status, BLOCKED);
        assert!(openidea_plugin::views::Views::handle(&mut plugin, VIEW, ViewEvent::Button(format!("start-managed@{token}")), &host.invocation()).is_err());
        host.on_process("flyway", |_, _| Ok(openidea_plugin::ProcessOutput { status: Some(0), stdout: "Flyway Enterprise Edition 11.3.0 by Redgate\n".into(), stderr: String::new() }));
        host.on_process("flyway", |_, _| Ok(openidea_plugin::ProcessOutput { status: Some(0), stdout: "Flyway Community Edition 11.3.0 by Redgate\n".into(), stderr: String::new() }));
        prepare(&mut plugin);
        assert!(start_button(&mut plugin).is_none(), "Community cannot snapshot or diff");
        assert!(plugin.comparison.draft.as_ref().unwrap().managed.status.contains("Teams or Enterprise is required"));
        host.on_process("flyway", |_, _| Ok(openidea_plugin::ProcessOutput { status: Some(0), stdout: "Flyway Enterprise Edition 11.3.0 by Redgate\n".into(), stderr: String::new() }));
        let token = prepare(&mut plugin);
        assert!(start_button(&mut plugin).is_some_and(|button| button.enabled));
        openidea_plugin::views::Views::handle(&mut plugin, VIEW, ViewEvent::Button(format!("start-managed@{token}")), &host.invocation()).unwrap();
        assert!(plugin.comparison.draft.as_ref().unwrap().managed.running());
        assert!(host.process_runs().iter().all(|run| run.args == ["--version"]), "stages run as an owned child, never a blocking process");
    }
    #[test]
    fn mock_admitted_stages_require_explicit_continuation_and_pair_the_new_report() {
        let (host, review) = setup();
        let project = host.invocation().project().unwrap();
        let mut core = core(&host, &review);
        assert!(
            !core
                .plan
                .as_ref()
                .unwrap()
                .args(2)
                .iter()
                .any(|arg| arg.starts_with("-diffText.changes="))
        );
        core.launch_admitted(&project).unwrap();
        assert!(core.running());
        assert!(core.launch_admitted(&project).is_err());
        host.add_file("review/source.snapshot.json", "{\"snapshot\":true}");
        assert!(done(&mut core, &project, "{}").unwrap());
        assert!(!core.running());
        assert!(core.ready);
        assert!(core.report.is_none());
        core.launch_admitted(&project).unwrap();
        host.add_file("review/changes.diff", "artifact fixture");
        assert!(done(&mut core, &project, "{}").unwrap());
        core.launch_admitted(&project).unwrap();
        let report = r#"{"differences":[]}"#;
        assert!(done(&mut core, &project, report).unwrap());
        assert_eq!(core.report.as_deref(), Some(report));
        core.report_fresh(&project, report).unwrap();
        host.add_file("review/changes.diff", "changed artifact");
        assert!(core.report_fresh(&project, report).is_err());
        assert!(core.launch_admitted(&project).is_err());
        assert!(host.requests().is_empty());
    }
    #[test]
    fn produced_report_uses_existing_explicit_picker_without_old_artifact_ids() {
        let (host, mut review) = setup();
        let project = host.invocation().project().unwrap();
        let mut c = core(&host, &review);
        c.launch_admitted(&project).unwrap();
        host.add_file("review/source.snapshot.json", "snapshot fixture");
        done(&mut c, &project, "{}").unwrap();
        c.launch_admitted(&project).unwrap();
        host.add_file("review/changes.diff", "artifact fixture");
        done(&mut c, &project, "{}").unwrap();
        c.launch_admitted(&project).unwrap();
        let report = r#"{"differences":[{"id":"new-1","differenceType":"Add","objectType":"Table","from":{"schema":"app","name":"a","definition":"CREATE TABLE app.a(id int);"},"to":null}]}"#;
        done(&mut c, &project, report).unwrap();
        let d = review.draft.as_mut().unwrap();
        d.managed = c;
        d.selected = vec!["old-artifact-id".into()];
        accept_managed_report(d).unwrap();
        assert!(d.selected.is_empty());
        let token = d.token.clone();
        host.answer_next_prompt(Some(&["new-1"]));
        review
            .handle(
                ViewEvent::Button(format!("select@{token}")),
                &host.invocation(),
            )
            .unwrap();
        assert_eq!(review.draft.as_ref().unwrap().selected, vec!["new-1"]);
        let Body::Form(fields) = review.view(&host.invocation()).unwrap().body else {
            panic!("form")
        };
        assert!(fields.iter().any(|f| f.key == "sql-new-1-SOURCE"));
        assert!(
            review
                .handle(
                    ViewEvent::Button(format!("plan@{token}")),
                    &host.invocation()
                )
                .is_err()
        );
        assert!(host.requests().is_empty());
    }
    #[test]
    fn stale_model_foreign_root_and_output_collision_refuse_start() {
        let (host, review) = setup();
        let project = host.invocation().project().unwrap();
        let mut a = core(&host, &review);
        host.add_file("schema-model/a.sql", "changed");
        assert!(a.launch_admitted(&project).is_err());
        let mut b = core(&host, &review);
        host.add_file("review/source.snapshot.json", "collision");
        assert!(b.launch_admitted(&project).is_err());
        let foreign = MockHost::install();
        foreign.set_project_root(Some("/foreign"));
        let mut roots = Core::default();
        roots.plan = a.plan.take();
        assert!(
            roots
                .launch_admitted(&foreign.invocation().project().unwrap())
                .is_err()
        );
    }
    #[test]
    fn nonzero_stderr_output_caps_and_malformed_report_never_publish() {
        for case in 0..4 {
            let (host, review) = setup();
            let project = host.invocation().project().unwrap();
            let mut c = core(&host, &review);
            c.launch_admitted(&project).unwrap();
            let result = match case {
                0 => c.ingest(
                    &project,
                    Chunk {
                        status: Some(2),
                        ..Chunk::default()
                    },
                ),
                1 => c.ingest(
                    &project,
                    Chunk {
                        stderr: "fabricated secret".into(),
                        ..Chunk::default()
                    },
                ),
                2 => c.ingest(
                    &project,
                    Chunk {
                        stdout: "x".repeat(CAP + 1),
                        ..Chunk::default()
                    },
                ),
                _ => {
                    c.step = 2;
                    c.raw = "{bad".into();
                    c.ingest(
                        &project,
                        Chunk {
                            status: Some(0),
                            ..Chunk::default()
                        },
                    )
                }
            };
            assert!(result.is_err());
            assert!(c.report.is_none());
            assert!(!c.running());
            assert!(!c.status.contains("fabricated secret"));
        }
    }
    #[test]
    fn cancellation_input_events_and_scope_checks_keep_one_owned_handle() {
        let (host, mut review) = setup();
        let project = host.invocation().project().unwrap();
        let plan = Plan::capture(&project, review.draft.as_ref().unwrap()).unwrap();
        review
            .draft
            .as_mut()
            .unwrap()
            .managed
            .prepare(plan, Err(BLOCKED.into()))
            .unwrap();
        review
            .draft
            .as_mut()
            .unwrap()
            .managed
            .launch_admitted(&project)
            .unwrap();
        assert!(review.begin(&host.invocation()).is_err());
        let token = review.draft.as_ref().unwrap().token.clone();
        assert!(
            review
                .handle(
                    ViewEvent::Field(format!("model@{token}"), "changed".into()),
                    &host.invocation()
                )
                .is_err()
        );
        review.invalidate(&[Event::FileSaved("/project/schema-model/a.sql".into())]);
        assert!(!review.draft.as_ref().unwrap().managed.running());
        assert!(review.draft.as_ref().unwrap().managed.report.is_none());
    }
}

#[cfg(test)]
mod owner_tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    #[test]
    fn nested_project_model_cannot_be_captured_by_outer_adapter() {
        let host = MockHost::install();
        host.add_dir("schema-model");
        host.add_dir("review");
        host.add_file("schema-model/a.sql", "CREATE TABLE a(id int);");
        let mut review = Review::default();
        review.begin(&host.invocation()).unwrap();
        host.add_project("/project/schema-model");
        assert!(
            Plan::capture(
                &host.invocation().project().unwrap(),
                review.draft.as_ref().unwrap()
            )
            .is_err()
        );
    }
    #[test]
    fn output_cannot_modify_the_model_tree_it_is_meant_to_snapshot() {
        let host = MockHost::install();
        host.add_dir("schema-model");
        host.add_dir("review");
        host.add_file("schema-model/a.sql", "CREATE TABLE a(id int);");
        let mut review = Review::default();
        review.begin(&host.invocation()).unwrap();
        review.draft.as_mut().unwrap().source = "schema-model/source.snapshot.json".into();
        assert!(
            Plan::capture(
                &host.invocation().project().unwrap(),
                review.draft.as_ref().unwrap()
            )
            .is_err()
        );
    }
    #[test]
    fn exact_input_bytes_are_revalidated_after_plan_review() {
        let host = MockHost::install();
        host.add_dir("schema-model");
        host.add_dir("review");
        host.add_file("schema-model/a.sql", "CREATE TABLE a(id int);");
        let mut review = Review::default();
        review.begin(&host.invocation()).unwrap();
        let project = host.invocation().project().unwrap();
        let plan = Plan::capture(&project, review.draft.as_ref().unwrap()).unwrap();
        host.add_file("schema-model/a.sql", "CREATE TABLE b(id int);");
        assert!(plan.revalidate_prepare(&project).is_err());
    }
}
