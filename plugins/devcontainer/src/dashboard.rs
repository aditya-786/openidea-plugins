//! The Dev Containers view: the containers of the project, found by the label the CLI puts on them.

use openidea_plugin::views::{Body, Button, ListItem, View, ViewEvent, Viewport, Views};
use openidea_plugin::{Error, Invocation, NotificationAction, NotificationLevel, Project, host};

use crate::{LOGIN_SHELL, Main, RUN_TARGET_REMOVED};
use openidea_plugin::Plugin;

pub struct Container {
    pub id: String,
    pub name: String,
    pub image: String,
    pub status: String,
    pub running: bool,
}

/// The lines of `docker ps -a --format '{{json .}}'`; lines that are not JSON objects are skipped.
pub fn parse_containers(stdout: &str) -> Vec<Container> {
    let text = |value: &serde_json::Value, key: &str| value.get(key).and_then(|v| v.as_str()).unwrap_or_default().to_owned();

    stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter_map(|value| {
            let id = text(&value, "ID");
            let status = text(&value, "Status");
            let running = match text(&value, "State").as_str() {
                "" => status.starts_with("Up"),
                state => state == "running",
            };
            (!id.is_empty()).then(|| Container { name: text(&value, "Names"), image: text(&value, "Image"), status, running, id })
        })
        .collect()
}

pub(crate) fn list(project: &Project, root: &str) -> Result<Vec<Container>, Error> {
    let args = ["ps", "-a", "--filter", &format!("label=devcontainer.local_folder={root}"), "--format", "{{json .}}"].map(str::to_owned);
    let output = project.run_process("docker", &args, None)?;
    if output.status != Some(0) {
        return Err(format!("docker ps failed: {}", output.stderr.trim()).into());
    }

    Ok(parse_containers(&output.stdout))
}

const STATUS_KEY: &str = "devcontainer:status";
fn status_view(message: &str) -> View {
    View::new(Body::List(vec![ListItem::new(STATUS_KEY, "Dev Containers").icon("docker-devcontainer").detail(message)]))
}

impl Views for Main {
    fn render(&mut self, view: &str, _viewport: Option<Viewport>, ctx: &Invocation) -> Result<View, Error> {
        (|| -> Result<View, Error> {
            if view == crate::run_target::REVIEW { return Ok(self.run_target.view(ctx)); }
            if view == crate::operations::VIEW { return Ok(self.operations.render(ctx)); }
            let project = match self.bind_project(ctx) {
                Ok(project) => project,
                Err(Error::NotFound(_)) => return Ok(status_view("Open a project to see its dev containers.").button(Button::new("refresh", "Refresh").icon("refresh"))),
                Err(error) => return Ok(status_view(&format!("Cannot identify the project: {error}")).button(Button::new("refresh", "Refresh").icon("refresh"))),
            };
            let containers = match list(&project, &project.root()?) {
                Ok(containers) => containers,
                Err(error) => {
                    self.selected = None;
                    self.running.clear();
                    return Ok(status_view(&match error {
                        Error::Denied(_) => ui_kit::denied::message("Listing containers", "process:docker", "Dev Container CLI"),
                        error => format!("Cannot list containers: {error}"),
                    }).button(Button::new("refresh", "Refresh").icon("refresh")));
                }
            };
            if self.selected.as_ref().is_some_and(|id| !containers.iter().any(|c| c.id == *id)) {
                self.selected = None;
            }
            let selected = self.selected.clone();
            let empty = containers.is_empty();
            let toolbar = |view: View, running_ids: &std::collections::BTreeSet<String>| {
                let selected = selected.as_ref();
                let running = selected.is_some_and(|id| running_ids.contains(id));
                view.button(Button::new("refresh", "Refresh").icon("refresh"))
                    .button(Button::new("up", "Create and Start Container…").icon("plus").enabled(empty))
                    .button(Button::new("shell", "Open Shell").icon("terminal").enabled(running))
                    .button(Button::new("stop", "Stop").icon("stop").enabled(running))
                    .button(Button::new("start", "Start").icon("run").enabled(selected.is_some() && !running))
                    .button(Button::new("rebuild", "Rebuild Container…").icon("rerun"))
                    .button(Button::new("configuration", "Choose Configuration").icon("settings"))
            };
            if empty {
                self.running.clear();
                let configured = !host::setting_string("configFile").unwrap_or_default().trim().is_empty()
                    || crate::configs::discover(&project).map_or(true, |found| !found.is_empty());
                let message = match configured {
                    true => "No dev container for this project yet. Create and Start Container… builds one from the configuration.",
                    false => "This project has no devcontainer.json. Create one with New › Dev Container Configuration, then Create and Start Container….",
                };
                return Ok(toolbar(status_view(message), &self.running));
            }

            let running: std::collections::BTreeSet<String> = containers.iter().filter(|c| c.running).map(|c| c.id.clone()).collect();
            let items = containers
                .iter()
                .map(|c| ListItem::new(&c.id, &c.name).icon(if c.running { "docker-container-running" } else { "docker-container-stopped" }).detail(&format!("{} - {}", c.image, c.status)))
                .collect();

            let status = format!("{} running, {} stopped", running.len(), containers.len() - running.len());
            let view = toolbar(View::new(Body::List(items)), &running).status(&status);
            self.running = running;

            Ok(view)
        })().map(ui_kit::actions::available)
    }

    fn handle(&mut self, view: &str, event: ViewEvent, ctx: &Invocation) -> Result<(), Error> {
        self.handle_event(view, event, ctx).map_err(crate::explain_denial)
    }
}

impl Main {
    fn handle_event(&mut self, view: &str, event: ViewEvent, ctx: &Invocation) -> Result<(), Error> {
        if view == crate::run_target::REVIEW {
            return match event {
                ViewEvent::Button(key) => self.run_target.commit(ctx, &key),
                _ => Err(Error::Invalid("Target review is read-only.".into())),
            };
        }
        if view == crate::operations::VIEW { return self.operations.handle(event, ctx); }
        if matches!(&event, ViewEvent::Select(key) | ViewEvent::Activate(key) if key == STATUS_KEY) {
            self.selected = None;
            return Ok(());
        }
        if matches!(&event, ViewEvent::Button(button) if button == "refresh") {
            // A status row remains refreshable even without a project/grant.
            let _ = self.bind_project(ctx);
            host::refresh_view(view);
            return Ok(());
        }
        self.bind_project(ctx)?;
        match event {
            ViewEvent::Select(id) => {
                self.container(ctx, &id)?;
                self.selected = Some(id);
                host::refresh_view(view);
            }
            ViewEvent::Activate(id) => {
                let (_, container) = self.container(ctx, &id)?;
                if container.running {
                    open_shell(&container.id)?;
                } else {
                    self.start(view, ctx, container.id)?;
                }
            }
            ViewEvent::Button(button) if button == "refresh" => host::refresh_view(view),
            ViewEvent::Button(button) if matches!(button.as_str(), "up" | "rebuild") => {
                self.run_command(&button, ctx, None)?;
                host::refresh_view(view);
            }
            ViewEvent::Button(button) if button == "configuration" => {
                let project = self.bind_project(ctx)?;
                if let Some(path) = crate::configs::choose(&project, true)? {
                    self.run_target.clear_for(&project.root()?);
                    host::unregister_run_target(&project, crate::run_target::ID);
                    host::unregister_run_target(&project, crate::run_target::MAPPED_ID);
                    host::notify(NotificationLevel::Info, "Dev Container configuration", &format!("Using {path} for this project."));
                }
            }
            ViewEvent::Button(button) if button == "shell" => {
                if let Some(id) = self.selected.clone() {
                    self.shell_into(ctx, &id)?;
                }
            }
            ViewEvent::Button(button) if button == "start" => {
                if let Some(id) = self.selected.clone() {
                    self.start(view, ctx, id)?;
                }
            }
            ViewEvent::Button(button) if button == "stop" => {
                let Some(id) = self.selected.clone() else {
                    return Ok(());
                };
                let (project, container) = self.container(ctx, &id)?;
                if !container.running {
                    return Err("This container is already stopped. Refresh Services.".into());
                }
                let body = "Runs docker stop on this container. Unsaved work in a container shell is lost.\nAny Dev Container run target for this project is removed too.";
                if !host::confirm(&format!("Stop {}?", container.name), body)? {
                    return Ok(());
                }
                let stopped = project.run_process("docker", &["stop".to_owned(), container.id.clone()], None)?;
                if stopped.status != Some(0) {
                    return Err(format!("docker stop failed: {}", stopped.stderr.trim()).into());
                }
                self.run_target.clear_for(&project.root()?);
                host::unregister_run_target(&project, crate::run_target::ID);
                host::unregister_run_target(&project, crate::run_target::MAPPED_ID);
                self.selected = None;
                host::notify(NotificationLevel::Info, "Dev Container", &format!("Stopped container {}. {RUN_TARGET_REMOVED}", container.name));
                host::refresh_view(view);
            }
            _ => {}
        }
        Ok(())
    }
}

impl Main {
    fn bind_project(&mut self, ctx: &Invocation) -> Result<Project, Error> {
        let identity = ctx.project().ok_or_else(|| Error::NotFound("project".into())).and_then(|p| p.root().map(|root| (p, root)));
        match identity {
            Ok((project, root)) => {
                if self.selection_project.as_deref() != Some(&root) {
                    self.selected = None;
                    self.running.clear();
                    self.selection_project = Some(root);
                }
                Ok(project)
            }
            Err(error) => {
                self.selected = None;
                self.running.clear();
                self.selection_project = None;
                Err(error)
            }
        }
    }

    fn container(&mut self, ctx: &Invocation, id: &str) -> Result<(Project, Container), Error> {
        let project = self.bind_project(ctx)?;
        if id.is_empty() || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            self.selected = None;
            self.running.clear();
            return Err(Error::Invalid("Invalid Docker container ID.".into()));
        }
        let found = list(&project, &project.root()?).and_then(|containers| containers.into_iter().find(|c| c.id == id).ok_or(Error::Stale));
        match found {
            Ok(container) => Ok((project, container)),
            Err(error) => {
                self.selected = None;
                self.running.clear();
                Err(error)
            }
        }
    }

    fn start(&mut self, view: &str, ctx: &Invocation, id: String) -> Result<(), Error> {
        let (project, container) = self.container(ctx, &id)?;
        if container.running {
            return Err("This container is already running. Refresh Services.".into());
        }
        self.run_target.clear_for(&project.root()?);
        host::unregister_run_target(&project, crate::run_target::ID);
        host::unregister_run_target(&project, crate::run_target::MAPPED_ID);
        let started = project.run_process("docker", &["start".to_owned(), container.id], None)?;
        if started.status != Some(0) {
            return Err(format!("docker start failed: {}", started.stderr.trim()).into());
        }
        let open_shell = NotificationAction::new("Open Shell", "shell").argument(id);
        if host::notify_with_actions(NotificationLevel::Info, "Dev Container", "Started the container.", &[open_shell]).is_err() {
            host::notify(NotificationLevel::Info, "Dev Container", "Started the container.");
        }
        host::refresh_view(view);
        Ok(())
    }

    /// Opens a shell in one of this project's running containers, rechecked by id.
    pub(crate) fn shell_into(&mut self, ctx: &Invocation, id: &str) -> Result<(), Error> {
        let (_, container) = self.container(ctx, id)?;
        if !container.running {
            return Err("This container is stopped. Refresh Services before opening a shell.".into());
        }
        open_shell(&container.id)
    }
}

/// One terminal per container: opening it again focuses the running shell.
fn open_shell(id: &str) -> Result<(), Error> {
    let command = host::Command::new("docker").args(["exec", "-it", id, "sh", "-c", LOGIN_SHELL]);

    host::open_terminal_session(&format!("docker-shell-{id}"), "Dev Container: Shell", &command).map(drop)
}

impl openidea_plugin::details::Details for Main {
    fn detail(&mut self, view: &str, key: &str, ctx: &Invocation) -> Result<Vec<openidea_plugin::details::Tab>, Error> {
        use openidea_plugin::details::{Pane, ProcessLog, Tab};
        if view == crate::operations::VIEW { return self.operations.detail(key, ctx); }
        if view != "containers" {
            return Ok(Vec::new());
        }
        if key == STATUS_KEY {
            return Ok(vec![Tab::new("status", "Dev Containers", Pane::Message(
                "This row shows a status message, not a container. Open a project and allow process:docker in Settings › Plugins › Dev Container CLI, then Refresh. If the list is empty, use Dev Container: Start Container or choose a configuration.".into(),
            ))]);
        }
        let project = self.bind_project(ctx)?;
        let containers = list(&project, &project.root()?).map_err(crate::explain_denial)?;
        let Some(container) = containers.iter().find(|c| c.id == key) else {
            return Ok(vec![Tab::new(
                "container",
                "Container",
                Pane::Message("This container is no longer available. Refresh Services.".into()),
            )]);
        };
        let mut tabs = vec![Tab::new(
            "container",
            "Container",
            Pane::Text(format!("{}\nID: {}\nImage: {}\nStatus: {}", container.name, container.id, container.image, container.status)),
        )];
        tabs.push(Tab::new(
            "logs",
            "Logs",
            ProcessLog::new("docker", &["logs", "--follow", "--tail=1000", &container.id]).toggle(
                "timestamps",
                "Timestamps",
                &["--timestamps"],
            ),
        ));
        Ok(tabs)
    }
}

#[cfg(test)]
mod services_tests {
    use super::*;
    use openidea_plugin::details::{Details, Pane};
    use openidea_plugin::mock::MockHost;
    #[test]
    fn selected_container_has_metadata_and_managed_logs_but_missing_ids_do_not() {
        let host = MockHost::install().with_capabilities(&["project.read", "process:docker"]);
        host.on_process("docker", |_, _| {
            Ok(openidea_plugin::ProcessOutput {
                status: Some(0),
                stdout: r#"{"ID":"abc123","Names":"app","Image":"node:22","Status":"Up"}"#.into(),
                stderr: String::new(),
            })
        });
        let mut plugin = Main::default();
        let tabs = plugin.detail("containers", "abc123", &host.invocation()).unwrap();
        let Pane::Text(text) = &tabs[0].body else { panic!("metadata") };
        assert!(text.contains("node:22"));
        let Pane::Log(log) = &tabs[1].body else { panic!("a managed log") };
        assert_eq!(log.args, ["logs", "--follow", "--tail=1000", "abc123"]);
        let missing = plugin.detail("containers", "other", &host.invocation()).unwrap();
        assert!(matches!(missing[0].body, Pane::Message(_)));
    }
    fn mutable_host() -> (MockHost, std::rc::Rc<std::cell::Cell<u8>>) {
        let host = MockHost::install().with_capabilities(&["project.read", "process:docker"]);
        let mode = std::rc::Rc::new(std::cell::Cell::new(0));
        let state = mode.clone();
        host.on_process("docker", move |args, _| {
            let stdout = if args[0] != "ps" {
                ""
            } else if args.iter().any(|a| a == "label=devcontainer.local_folder=/second") || state.get() == 1 {
                ""
            } else if state.get() == 2 {
                r#"{"ID":"abc123","Names":"app","State":"exited"}"#
            } else {
                r#"{"ID":"abc123","Names":"app","State":"running"}"#
            };
            Ok(openidea_plugin::ProcessOutput { status: Some(0), stdout: stdout.into(), stderr: String::new() })
        });
        (host, mode)
    }

    fn no_mutations(host: &MockHost) {
        assert!(host.terminals().is_empty());
        assert!(host.process_runs().iter().all(|r| r.args[0] == "ps"));
    }

    #[test]
    fn empty_inventory_offers_start_and_rebuild() {
        let (host, mode) = mutable_host();
        mode.set(1);
        let view = Main::default().render("containers", None, &host.invocation()).unwrap();
        let ids: Vec<_> = view.toolbar.iter().map(|b| b.id.as_str()).collect();
        assert_eq!(ids, ["refresh", "up", "rebuild", "configuration"]);
    }

    fn status_detail(view: &View) -> String {
        let Body::List(rows) = &view.body else { panic!("a status row") };
        rows[0].detail.clone().unwrap_or_default()
    }

    #[test]
    fn empty_inventory_without_configuration_points_to_the_new_menu_template() {
        let (host, mode) = mutable_host();
        mode.set(1);
        let view = Main::default().render("containers", None, &host.invocation()).unwrap();
        assert!(status_detail(&view).contains("New › Dev Container Configuration"));
        host.add_file(".devcontainer/devcontainer.json", "{}");
        let view = Main::default().render("containers", None, &host.invocation()).unwrap();
        assert!(status_detail(&view).starts_with("No dev container for this project yet."));
    }

    #[test]
    fn declining_the_stop_confirm_keeps_the_container_running() {
        let (host, _) = mutable_host();
        let mut plugin = Main::default();
        plugin.handle("containers", ViewEvent::Select("abc123".into()), &host.invocation()).unwrap();
        plugin.handle("containers", ViewEvent::Button("stop".into()), &host.invocation()).unwrap();
        no_mutations(&host);
        assert!(host.notifications().is_empty());
    }

    #[test]
    fn stopping_names_the_container_and_the_removed_run_target() {
        let (host, _) = mutable_host();
        let mut plugin = Main::default();
        plugin.handle("containers", ViewEvent::Select("abc123".into()), &host.invocation()).unwrap();
        host.answer_next_prompt(Some(&[]));
        plugin.handle("containers", ViewEvent::Button("stop".into()), &host.invocation()).unwrap();
        assert_eq!(host.notifications()[0].body, format!("Stopped container app. {}", crate::RUN_TARGET_REMOVED));
    }

    #[test]
    fn switching_projects_clears_selection_before_toolbar_actions() {
        for action in ["shell", "stop", "start"] {
            let (host, _) = mutable_host();
            let mut plugin = Main::default();
            plugin.handle("containers", ViewEvent::Select("abc123".into()), &host.invocation()).unwrap();
            host.set_project_root(Some("/second"));
            plugin.handle("containers", ViewEvent::Button(action.into()), &host.invocation()).unwrap();
            assert!(plugin.selected.is_none());
            no_mutations(&host);
            assert!(plugin.handle("containers", ViewEvent::Activate("abc123".into()), &host.invocation()).is_err());
            no_mutations(&host);
        }
    }

    #[test]
    fn deleted_or_unlisted_containers_cannot_be_stopped_or_opened() {
        for action in ["shell", "stop", "start"] {
            let (host, mode) = mutable_host();
            let mut plugin = Main::default();
            plugin.handle("containers", ViewEvent::Select("abc123".into()), &host.invocation()).unwrap();
            mode.set(1);
            assert!(plugin.handle("containers", ViewEvent::Button(action.into()), &host.invocation()).is_err());
            assert!(plugin.selected.is_none());
            no_mutations(&host);
        }
    }

    #[test]
    fn current_state_not_cached_state_controls_shell_and_activation() {
        let (host, mode) = mutable_host();
        let mut plugin = Main::default();
        plugin.handle("containers", ViewEvent::Select("abc123".into()), &host.invocation()).unwrap();
        mode.set(2);
        assert!(plugin.handle("containers", ViewEvent::Button("shell".into()), &host.invocation()).is_err());
        no_mutations(&host);
        plugin.handle("containers", ViewEvent::Activate("abc123".into()), &host.invocation()).unwrap();
        assert!(host.process_runs().iter().any(|r| r.args == ["start", "abc123"]));
        assert!(host.terminals().is_empty());
    }

    #[test]
    fn missing_or_denied_project_identity_clears_selection() {
        for denied in [false, true] {
            let (host, _) = mutable_host();
            let mut plugin = Main::default();
            plugin.handle("containers", ViewEvent::Select("abc123".into()), &host.invocation()).unwrap();
            let host = if denied {
                host.with_capabilities(&["process:docker"])
            } else {
                host.set_project_root(None);
                host
            };
            assert!(plugin.handle("containers", ViewEvent::Button("stop".into()), &host.invocation()).is_err());
            assert!(plugin.selected.is_none());
            no_mutations(&host);
        }
    }

    #[test]
    fn option_shaped_container_ids_are_rejected_without_execution() {
        let (host, _) = mutable_host();
        let mut plugin = Main::default();
        assert!(plugin.handle("containers", ViewEvent::Activate("--help".into()), &host.invocation()).is_err());
        no_mutations(&host);
    }
}

#[cfg(test)]
mod status_regressions {
    use super::*;
    use openidea_plugin::{mock::MockHost, details::{Details, Pane}};
    #[test]
    fn project_free_status_is_visible_and_cannot_start_a_container() {
        let host = MockHost::install();
        host.set_project_root(None);
        let mut plugin = Main::default();
        let view = plugin.render("containers", None, &host.invocation()).unwrap();
        assert!(matches!(view.body, Body::List(ref rows) if rows.len()==1 && rows[0].key==STATUS_KEY));
        plugin.handle("containers", ViewEvent::Select(STATUS_KEY.into()), &host.invocation()).unwrap();
        plugin.handle("containers", ViewEvent::Activate(STATUS_KEY.into()), &host.invocation()).unwrap();
        let tabs = plugin.detail("containers", STATUS_KEY, &host.invocation()).unwrap();
        assert!(matches!(&tabs[0].body, Pane::Message(text) if text.contains("not a container")));
        assert!(plugin.selected.is_none());
        assert!(host.process_runs().is_empty());
        assert!(host.terminals().is_empty());
    }
}
