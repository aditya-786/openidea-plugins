//! Explicit finite CLI handles. Rendering never starts or polls a process.
use openidea_plugin::details::{Pane, Tab};
use openidea_plugin::host::{Child, Chunk, Command};
use openidea_plugin::views::{Body, Button, ListItem, View, ViewEvent};
use openidea_plugin::{Error, Invocation, Project, host};
use std::hash::{BuildHasher, Hasher};

pub(crate) const VIEW: &str = "managed-cli";
const MAX_OUTPUT: usize = 64 * 1024;
const POLL_BYTES: u32 = 16 * 1024;
const MAX_POLLS: usize = 4;

#[derive(Default)]
pub(crate) struct Operations {
    session: Option<Session>,
    failed: Option<(String, String)>,
}

struct Session {
    key: String,
    root: String,
    config: String,
    args: Vec<String>,
    child: Option<Child>,
    output: String,
    truncated: bool,
    status: String,
    exit: Option<i32>,
}

fn owner(ctx: &Invocation) -> Result<(Project, String), Error> {
    let project = ctx.project().ok_or_else(|| Error::NotFound("project".into()))?;
    let root = project.root()?;
    if root.is_empty() {
        return Err(Error::Stale);
    }
    Ok((project, root))
}

fn row(message: &str) -> View {
    View::new(Body::List(vec![ListItem::new("managed-status", "Background run").detail(message)]))
        .button(Button::new("refresh-managed", "Refresh Status").icon("refresh"))
}

fn append(session: &mut Session, label: &str, value: &str) {
    if value.is_empty() { return; }
    session.output.push_str(label);
    session.output.push_str(value);
    if !value.ends_with('\n') { session.output.push('\n'); }
    if session.output.len() > MAX_OUTPUT {
        let mut cut = session.output.len() - MAX_OUTPUT;
        while !session.output.is_char_boundary(cut) { cut += 1; }
        session.output.drain(..cut);
        session.truncated = true;
    }
}

impl Operations {
    pub(crate) fn idle(&self) -> Result<(), Error> {
        if self.session.as_ref().is_some_and(|s| s.child.is_some()) {
            return Err("A background command is still running. Open Dev Container: Show Background Runs, then press Refresh Status or Cancel Command before starting another.".into());
        }
        Ok(())
    }

    pub(crate) fn start(&mut self, project: &Project, config: String, args: Vec<String>) -> Result<(), Error> {
        self.idle()?;
        let root = project.root()?;
        let child = match host::start(&Command::new("devcontainer").args(&args), Some(project)) {
            Ok(child) => child,
            Err(error) => {
                self.failed = Some((root, format!("Last start failed: {}", error.to_string().chars().take(1024).collect::<String>())));
                host::refresh_view(VIEW);
                return Err(error);
            }
        };
        if let Err(error) = child.close_stdin() {
            let _ = child.kill();
            self.failed = Some((root, "Could not prepare the CLI, so the run was stopped. The container may be partly built.".into()));
            host::refresh_view(VIEW);
            return Err(error);
        }
        let mut random = std::collections::hash_map::RandomState::new().build_hasher();
        random.write(root.as_bytes());
        let key = format!("operation-{:016x}", random.finish());
        self.session = Some(Session {
            key, root, config, args, child: Some(child), output: String::new(), truncated: false,
            status: "Started. Refresh Status shows progress.".into(), exit: None,
        });
        self.failed = None;
        host::refresh_view(VIEW);
        Ok(())
    }

    /// A failed inventory is unknown, not evidence that a project closed.
    fn closed_roots(&mut self) {
        let Some(session) = self.session.as_mut().filter(|s| s.child.is_some()) else { return; };
        let roots = host::projects().and_then(|projects| projects.into_iter().map(|p| p.root()).collect::<Result<Vec<_>, _>>());
        match roots {
            Err(_) => session.status = "Project list unavailable; the run is kept and its outcome is unknown.".into(),
            Ok(roots) if !roots.contains(&session.root) => {
                if let Some(child) = session.child.take() { let _ = child.kill(); }
                session.status = "The project closed, so the run was cancelled. The container may be partly built; cancelling does not undo changes.".into();
            }
            _ => {}
        }
    }

    fn current<'a>(&'a mut self, ctx: &Invocation, key: &str) -> Result<&'a mut Session, Error> {
        let (_, root) = owner(ctx)?;
        self.session.as_mut().filter(|s| s.root == root && s.key == key).ok_or(Error::Stale)
    }

    fn poll(session: &mut Session) -> Result<(), Error> {
        Self::poll_with(session, |child, max| child.poll(max))
    }

    fn poll_with(session: &mut Session, mut poll: impl FnMut(&Child, u32) -> Result<Chunk, Error>) -> Result<(), Error> {
        if session.child.is_none() { return Ok(()); }
        for _ in 0..MAX_POLLS {
            let result = poll(session.child.as_ref().unwrap(), POLL_BYTES);
            let Chunk { stdout, stderr, status } = match result {
                Ok(chunk) => chunk,
                Err(error) => {
                    session.status = "Status unavailable; the run and its output so far are kept. Refresh Status to retry.".into();
                    return Err(error);
                }
            };
            append(session, "[stdout]\n", &stdout);
            append(session, "[stderr]\n", &stderr);
            if let Some(code) = status { session.exit = Some(code); }
            if stdout.is_empty() && stderr.is_empty() {
                if let Some(code) = session.exit {
                    session.child.take();
                    session.status = format!("The CLI exited with status {code}. Its output is kept; the container is not tracked after this.");
                } else {
                    session.status = "Still running. Refresh Status shows progress.".into();
                }
                return Ok(());
            }
        }
        session.status = session.exit.map_or_else(
            || "Still running, with more output waiting. Refresh Status reads it.".into(),
            |code| format!("CLI exited with status {code}; output is still draining. Refresh Status or press Cancel Command."),
        );
        Ok(())
    }

    pub(crate) fn render(&self, ctx: &Invocation) -> View {
        let (_, root) = match owner(ctx) {
            Ok(owner) => owner,
            Err(_) => return row("Open the project and allow project.read to see its background run."),
        };
        let failure = self.failed.as_ref().filter(|(owner, _)| owner == &root).map(|(_, error)| error.as_str());
        let Some(session) = self.session.as_ref().filter(|s| s.root == root) else {
            return row(failure.unwrap_or("No background command for this project. Run Dev Container: Start Container in Background… or Dev Container: Rebuild Container in Background…. The terminal commands still work."));
        };
        let mut view = View::new(Body::List(vec![ListItem::new(&session.key, "Background run")
            .detail(&format!("{} · {}", session.config, session.status))]))
            .button(Button::new("refresh-managed", "Refresh Status").icon("refresh"))
            .button(Button::new(&format!("cancel-{}", session.key), "Cancel Command").enabled(session.child.is_some()))
            .status("Refresh to see progress · cancelling does not undo a started container or build");
        if let Some(error) = failure { view = view.status(error); }
        view
    }

    pub(crate) fn detail(&self, key: &str, ctx: &Invocation) -> Result<Vec<Tab>, Error> {
        let (_, root) = owner(ctx)?;
        let Some(session) = self.session.as_ref().filter(|s| s.key == key && s.root == root) else { return Ok(Vec::new()); };
        Ok(vec![
            Tab::new("target", "Target / State", Pane::Text(format!(
                "Project: {}\nConfiguration: {}\nProgram: devcontainer\nArguments: {}\n\n{}\n\nThe CLI reads the configuration and may run the build and lifecycle commands it references. Closing it does not prove the container or build stopped. The handle is released after about 300 seconds idle.", session.root, session.config, serde_json::to_string(&session.args).unwrap_or_default(), session.status))),
            Tab::new("output", "Captured Output", Pane::Text(format!("{}{}", if session.truncated { "[Earlier output truncated; the latest 64 KiB is kept]\n" } else { "" }, if session.output.is_empty() { "No output yet. Refresh Status reads the latest output." } else { &session.output }))),
        ])
    }

    pub(crate) fn handle(&mut self, event: ViewEvent, ctx: &Invocation) -> Result<(), Error> {
        match event {
            ViewEvent::Button(button) if button == "refresh-managed" => {
                self.closed_roots();
                if let Ok((_, root)) = owner(ctx) {
                    if let Some(session) = self.session.as_mut().filter(|s| s.root == root) {
                        let result = Self::poll(session);
                        host::refresh_view(VIEW);
                        result?;
                    }
                }
            }
            ViewEvent::Button(button) if button.starts_with("cancel-") => {
                let session = self.current(ctx, &button[7..])?;
                let Some(child) = session.child.as_ref() else { return Err(Error::Stale); };
                if let Err(error) = child.kill() {
                    // The user explicitly requested handle closure. Releasing
                    // this owned resource is distinct from inferring an exit.
                    session.child.take();
                    session.status = "Could not confirm the cancellation; the run was released and its output kept. Its outcome is unknown.".into();
                    host::refresh_view(VIEW);
                    return Err(error);
                }
                let _ = Self::poll(session);
                session.child.take();
                session.status = "Cancelled. The container may be partly built; cancelling does not undo changes.".into();
            }
            ViewEvent::Select(key) | ViewEvent::Activate(key) if key != "managed-status" => { self.current(ctx, &key)?; }
            _ => {}
        }
        host::refresh_view(VIEW);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Main;
    use openidea_plugin::{Plugin, details::Details, mock::MockHost, views::Views};

    fn setup() -> (MockHost, Main) {
        let host = MockHost::install().with_capabilities(&["project.read", "process:devcontainer"]);
        host.add_file(".devcontainer/devcontainer.json", "{}");
        (host, Main::default())
    }
    fn start(host: &MockHost, plugin: &mut Main, id: &str) {
        host.answer_next_prompt(Some(&[]));
        plugin.run_command(id, &host.invocation(), None).unwrap();
    }
    fn refresh(host: &MockHost, plugin: &mut Main) {
        plugin.handle(VIEW, ViewEvent::Button("refresh-managed".into()), &host.invocation()).unwrap();
    }

    #[test]
    fn public_commands_review_cancel_start_poll_exit_and_preserve_terminal_route() {
        let (host, mut plugin) = setup();
        host.answer_next_prompt(None);
        plugin.run_command("up-managed", &host.invocation(), None).unwrap();
        assert!(plugin.operations.session.is_none());
        start(&host, &mut plugin, "rebuild-managed");
        let session = plugin.operations.session.as_ref().unwrap();
        assert_eq!(session.root, "/project");
        assert_eq!(session.args, ["up", "--workspace-folder", "/project", "--config", ".devcontainer/devcontainer.json", "--remove-existing-container"]);
        assert!(session.child.as_ref().unwrap().write("unwanted stdin").is_err(), "CLI stdin is closed");
        assert!(host.terminals().is_empty());
        refresh(&host, &mut plugin);
        let session = plugin.operations.session.as_ref().unwrap();
        assert!(session.output.contains("started: devcontainer") && session.status.contains("running"));
        // MockHost's positive wait completes its synthetic child; production
        // uses nonblocking poll only, never this completion shortcut.
        session.child.as_ref().unwrap().wait(1).unwrap();
        refresh(&host, &mut plugin);
        let session = plugin.operations.session.as_ref().unwrap();
        assert!(session.child.is_none() && session.status.contains("status 0") && session.output.contains("started:"));
        assert!(session.status.contains("not tracked after this"));
        start(&host, &mut plugin, "up");
        assert_eq!(host.terminals().len(), 1, "legacy terminal command remains separate");
    }

    #[test]
    fn live_handle_busy_foreign_or_stale_controls_cannot_cancel_another_operation() {
        let (host, mut plugin) = setup();
        start(&host, &mut plugin, "up-managed");
        refresh(&host, &mut plugin);
        let first = plugin.operations.session.as_ref().unwrap().key.clone();
        assert!(plugin.run_command("up-managed", &host.invocation(), None).is_err());
        host.set_project_root(Some("/other"));
        let view = plugin.render(VIEW, None, &host.invocation()).unwrap();
        let Body::List(rows) = view.body else { panic!("Services requires list rows") };
        assert_eq!(rows[0].key, "managed-status");
        assert!(plugin.detail(VIEW, &first, &host.invocation()).unwrap().is_empty());
        assert!(plugin.handle(VIEW, ViewEvent::Button(format!("cancel-{first}")), &host.invocation()).is_err());
        assert_eq!(plugin.operations.session.as_ref().unwrap().child.as_ref().unwrap().wait(0).unwrap().status, None);
        host.set_project_root(Some("/project"));
        plugin.handle(VIEW, ViewEvent::Button(format!("cancel-{first}")), &host.invocation()).unwrap();
        let session = plugin.operations.session.as_ref().unwrap();
        assert!(session.child.is_none() && session.status.contains("does not undo") && session.output.contains("started:"));
        start(&host, &mut plugin, "up-managed");
        assert_ne!(plugin.operations.session.as_ref().unwrap().key, first);
        assert!(plugin.handle(VIEW, ViewEvent::Button(format!("cancel-{first}")), &host.invocation()).is_err());
        assert!(plugin.operations.session.as_ref().unwrap().child.is_some());
    }

    #[test]
    fn membership_failure_retains_handle_but_successful_closed_owner_inventory_cleans_up() {
        let (host, mut plugin) = setup();
        start(&host, &mut plugin, "up-managed");
        refresh(&host, &mut plugin);
        let host = host.with_capabilities(&[]);
        refresh(&host, &mut plugin);
        let session = plugin.operations.session.as_ref().unwrap();
        assert!(session.child.is_some() && session.status.contains("Project list unavailable"));
        assert!(session.output.contains("started:"));
        let host = host.with_capabilities(&["project.read", "process:devcontainer"]);
        refresh(&host, &mut plugin);
        assert!(plugin.operations.session.as_ref().unwrap().status.contains("running"));
        host.set_project_root(None);
        refresh(&host, &mut plugin);
        let session = plugin.operations.session.as_ref().unwrap();
        assert!(session.child.is_none() && session.status.contains("project closed") && session.output.contains("started:"));
        let key = session.key.clone();
        assert!(plugin.detail(VIEW, &key, &host.invocation()).is_err());
    }

    #[test]
    fn failed_new_spawn_retains_previous_accepted_output_and_surfaces_failure() {
        let (host, mut plugin) = setup();
        start(&host, &mut plugin, "up-managed");
        refresh(&host, &mut plugin);
        plugin.operations.session.as_ref().unwrap().child.as_ref().unwrap().wait(1).unwrap();
        refresh(&host, &mut plugin);
        let previous = plugin.operations.session.as_ref().unwrap().output.clone();
        let host = host.with_capabilities(&["project.read"]);
        host.answer_next_prompt(Some(&[]));
        assert!(matches!(plugin.run_command("up-managed", &host.invocation(), None), Err(Error::Invalid(m)) if m.contains("process:devcontainer permission")));
        assert_eq!(plugin.operations.session.as_ref().unwrap().output, previous);
        assert!(plugin.operations.failed.as_ref().unwrap().1.contains("Last start failed"));
        assert!(host.refreshed_views().iter().any(|view| view == VIEW));
        assert!(host.terminals().is_empty());
    }

    #[test]
    fn rendering_and_details_only_use_captured_state_and_bound_utf8_output() {
        let (host, mut plugin) = setup();
        start(&host, &mut plugin, "up-managed");
        let key = plugin.operations.session.as_ref().unwrap().key.clone();
        let tabs = plugin.detail(VIEW, &key, &host.invocation()).unwrap();
        assert!(matches!(&tabs[1].body, Pane::Text(text) if text.contains("No output yet")));
        plugin.render(VIEW, None, &host.invocation()).unwrap();
        assert!(plugin.operations.session.as_ref().unwrap().output.is_empty(), "render does not poll");
        let session = plugin.operations.session.as_mut().unwrap();
        append(session, "[stdout]\n", &"😀".repeat(MAX_OUTPUT));
        append(session, "[stderr]\n", "last useful error");
        assert!(session.truncated && session.output.len() <= MAX_OUTPUT && session.output.ends_with("last useful error\n"));
        let tabs = plugin.detail(VIEW, &key, &host.invocation()).unwrap();
        assert!(matches!(&tabs[1].body, Pane::Text(text) if text.contains("truncated") && text.len() < MAX_OUTPUT + 128));
        assert!(plugin.detail(VIEW, "forged", &host.invocation()).unwrap().is_empty());
    }

    #[test]
    fn reviewed_plan_rechecks_source_and_current_project_before_managed_spawn() {
        let (host, mut plugin) = setup();
        let project = host.invocation().project().unwrap();
        let plan = crate::launch::Plan::prepare(&project, "up", ".devcontainer/devcontainer.json").unwrap();
        host.open_editor("/project/.devcontainer/devcontainer.json", "jsonc", "[]", &[]);
        assert!(plan.launch_managed(&host.invocation(), &mut plugin.operations).is_err());
        assert!(plugin.operations.session.is_none());
        host.open_editor("/project/.devcontainer/devcontainer.json", "jsonc", "{}", &[]);
        let plan = crate::launch::Plan::prepare(&project, "up", ".devcontainer/devcontainer.json").unwrap();
        host.add_project("/project/.devcontainer");
        assert!(plan.launch_managed(&host.invocation(), &mut plugin.operations).is_err());
        assert!(plugin.operations.session.is_none() && host.terminals().is_empty());
    }

    #[test]
    fn oversized_concrete_preview_refuses_before_confirming_or_starting() {
        let (host, mut plugin) = setup();
        host.set_project_root(Some(&format!("/{}", "😀".repeat(6000))));
        host.answer_next_prompt(Some(&[]));
        assert!(matches!(plugin.run_command("up-managed", &host.invocation(), None), Err(Error::Invalid(_))));
        assert!(plugin.operations.session.is_none());
        assert!(host::confirm("Pending", "The oversized preview did not consume the answer").unwrap());
    }

    #[test]
    fn temporary_poll_error_keeps_owned_child_and_previous_output_for_retry() {
        let (host, mut plugin) = setup();
        start(&host, &mut plugin, "up-managed");
        refresh(&host, &mut plugin);
        let session = plugin.operations.session.as_mut().unwrap();
        let previous = session.output.clone();
        assert!(matches!(Operations::poll_with(session, |_, _| Err(Error::Denied("status".into()))), Err(Error::Denied(_))));
        assert!(session.child.is_some() && session.output == previous && session.status.contains("Status unavailable"));
        assert_eq!(session.child.as_ref().unwrap().wait(0).unwrap().status, None);
        refresh(&host, &mut plugin);
        assert!(plugin.operations.session.as_ref().unwrap().status.contains("running"));
    }

    #[test]
    fn exited_output_drains_in_bounded_rounds_before_releasing_handle() {
        let (host, mut plugin) = setup();
        start(&host, &mut plugin, "up-managed");
        let session = plugin.operations.session.as_mut().unwrap();
        let mut reads = 0;
        Operations::poll_with(session, |_, max| {
            assert_eq!(max, POLL_BYTES);
            reads += 1;
            Ok(Chunk { stdout: "x".repeat(max as usize), stderr: "é".repeat(max as usize / 2), status: Some(17) })
        }).unwrap();
        assert_eq!(reads, MAX_POLLS);
        assert!(session.child.is_some() && session.exit == Some(17) && session.status.contains("still draining"));
        assert!(session.truncated && session.output.len() <= MAX_OUTPUT);
        Operations::poll_with(session, |_, _| Ok(Chunk { status: Some(17), ..Chunk::default() })).unwrap();
        assert!(session.child.is_none() && session.status.contains("status 17"));
    }
}
