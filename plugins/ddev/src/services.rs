use crate::{Main, PROJECT, VIEW, model, runtime};
use openidea_plugin::details::{Details, Pane, Tab};
use openidea_plugin::views::{Body, Button, TreeItem, View, ViewEvent, Viewport, Views};
use openidea_plugin::{Error, Invocation, Plugin, host};

const NO_PROJECT: &str = "status:no-project";
const NOT_CONFIGURED: &str = "status:not-configured";
const UNAVAILABLE: &str = "status:unavailable";
/// A short reason for the unavailable row, from the describe failure.
fn unavailable_reason(error: &str) -> &'static str {
    let error = error.to_ascii_lowercase();
    if error.contains("docker") {
        "Container runtime not running"
    } else if error.contains("not granted") {
        "DDEV permission needed"
    } else if error.contains("not found") || error.contains("no such file") {
        "DDEV not installed"
    } else {
        "DDEV unavailable"
    }
}
fn is_status(key: &str) -> bool {
    matches!(key, NO_PROJECT | NOT_CONFIGURED | UNAVAILABLE)
}

impl Views for Main {
    fn render(
        &mut self,
        view: &str,
        _viewport: Option<Viewport>,
        ctx: &Invocation,
    ) -> Result<View, Error> {
        (|| -> Result<View, Error> {
            if view == crate::support::SETUP {
                if ctx.project().is_none() { return Ok(View::message("Open a project for DDEV setup.")); }
                return self.render_setup(ctx);
            }
            if view != VIEW {
                return Err(Error::NotFound(view.into()));
            }
            let Some(project) = ctx.project() else {
                // Services renders only tree/list bodies, including unavailable states.
                return Ok(View::new(Body::Tree(vec![TreeItem::new(NO_PROJECT, "Open a project to use DDEV")
                    .icon("info").detail("Choose a folder containing .ddev/config.yaml")]))
                    .status("No project is open"));
            };
            let configured = match project.exists(crate::CONFIG) {
                Ok(configured) => configured,
                Err(error) => return Ok(View::new(Body::Tree(vec![TreeItem::new(UNAVAILABLE, "DDEV")
                    .icon("info").detail("Project access unavailable")]))
                    .status(&format!("Cannot read DDEV configuration: {error}"))),
            };
            let cached = project
                .root()
                .is_ok_and(|root| self.snapshots.contains_key(&root));
            let prefs = runtime::Prefs::load(&project);
            let refreshing = cached && self.is_stale_with(&project, &prefs);
            let _task = (!cached).then(|| host::begin_task("Reading DDEV status", false));
            let result = self.load(&project, false, &prefs).cloned();
            let can_start = self.can_start_with(&project, false, &prefs);
            let last_known = project.root().ok().and_then(|root| {
                self.snapshots.get(&root)?;
                Some(runtime::now().saturating_sub(*self.refreshed_at.get(&root)?))
            });
            let selected = self.selected.as_deref().unwrap_or(PROJECT);
            let desc = result.as_ref().ok();
            let state = match (&result, configured) {
                (Ok(d), _) => d.status.as_str(),
                (Err(error), true) => unavailable_reason(&error.to_string()),
                (Err(_), false) => "Not configured",
            };
            let name = desc.map_or("DDEV", |d| &d.name);
            if desc.is_none() {
                runtime::publish(None, configured);
                let _ = host::set_context("can-start", can_start);
                let _ = host::set_context("available", can_start);
                let _ = host::set_context("can-restart-stop", can_start);
            }
            // Inherit the provider SVG; row icon paths are not resolved by Services.
            let mut items = vec![if desc.is_some() {
                TreeItem::new(PROJECT, name).detail(state).expandable(true)
            } else {
                TreeItem::new(if configured { UNAVAILABLE } else { NOT_CONFIGURED }, name)
                    .icon("info").detail(state)
            }];
            if let Some(desc) = desc {
                for service in &desc.services {
                    // Keep runtime state visible beside URL-bearing services too.
                    let status = model::text(&service.info, "status");
                    let status = if status.is_empty() { state } else { status };
                    let detail = service
                        .url
                        .as_ref()
                        .map_or_else(|| status.into(), |url| format!("{status} — {url}"));
                    items.push(
                        TreeItem::new(&format!("service:{}", service.name), &service.name)
                            .parent(PROJECT)
                            .icon(if service.name == "db" {
                                "database"
                            } else if !service.logs {
                                "external-link"
                            } else {
                                "run"
                            })
                            .detail(&detail),
                    );
                }
            }
            let service = desc.and_then(|d| d.service(selected));
            let root = selected == PROJECT;
            let has_url = desc.is_some_and(|d| {
                if root {
                    d.url.is_some()
                } else {
                    service.is_some_and(|s| s.url.is_some())
                }
            });
            let running = desc.is_some_and(|d| d.status == "running");
            let container = root || service.is_some_and(|s| s.logs);
            let status = match &result {
                Ok(d) if refreshing => format!("{} — {} (refreshing)", d.name, d.status),
                Ok(d) => format!("{} — {}", d.name, d.status),
                Err(e) => match last_known {
                    Some(age) => format!("DDEV not reachable: {e} (last known status {age} s ago)"),
                    None => format!("{state}: {e}. Configuration and Help remain available."),
                },
            };
            Ok(View::new(Body::Tree(items))
                .button(Button::new("refresh", "Refresh Status").icon("refresh"))
                .button(Button::new("start", "Start Project").icon("run").enabled(can_start))
                .button(Button::new("restart", "Restart Project").icon("rerun")
                    .enabled(running || desc.is_none() && can_start))
                .button(
                    Button::new("stop", "Stop Project")
                        .icon("stop")
                        .enabled(running || desc.is_none() && can_start),
                )
                .button(Button::new("shell", "Open Shell").icon("terminal").enabled(running && container))
                .button(Button::new("logs", "Open Logs").icon("list").enabled(desc.is_some() && container))
                .button(Button::new("open-service", "Open in Browser").icon("external-link").enabled(running && has_url))
                .button(Button::new("actions", "More Actions…").icon("more"))
                .button(Button::new("diagnose", "Help and Diagnostics").icon("help").enabled(desc.is_none() && configured))
                .button(
                    Button::new(
                        if configured { "open-config" } else { "config" },
                        if configured { "Open Configuration" } else { "Configure Project…" },
                    )
                    .icon("settings"),
                )
                .status(&status))
        })().map(ui_kit::actions::available)
    }

    fn handle(&mut self, view: &str, event: ViewEvent, ctx: &Invocation) -> Result<(), Error> {
        if ui_kit::web::handle(self, view, &event, ctx, include_str!("ui_config.json"))? { return Ok(()); }
        if view == crate::support::SETUP {
            return self.handle_setup(event, ctx);
        }
        if view != VIEW {
            return Err(Error::NotFound(view.into()));
        }
        if matches!(&event, ViewEvent::Select(key) | ViewEvent::Activate(key) if is_status(key)) {
            return Ok(());
        }
        if let Some(project) = ctx.project() {
            // Button events can precede a render after a project switch.
            self.sync_selection(&project)?;
        }
        match event {
            ViewEvent::Select(key) => {
                let project = ctx.project().ok_or("Open a project to use DDEV.")?;
                let cached = project
                    .root()
                    .is_ok_and(|root| self.snapshots.contains_key(&root));
                let _task = (!cached).then(|| host::begin_task("Reading DDEV status", false));
                if key != PROJECT {
                    self.target(&project, Some(&key))?;
                }
                self.selected = Some(key.clone());
                let _ = host::storage::set(&format!("selection:{}", project.root()?), &key);
            }
            ViewEvent::Activate(key) => {
                let project = ctx.project().ok_or("Open a project to use DDEV.")?;
                let cached = project
                    .root()
                    .is_ok_and(|root| self.snapshots.contains_key(&root));
                let _task = (!cached).then(|| host::begin_task("Reading DDEV status", false));
                let Ok(desc) = self.cached_description(&project) else {
                    return Ok(());
                };
                let url = if key == PROJECT {
                    desc.url.as_ref()
                } else {
                    desc.service(&key).and_then(|s| s.url.as_ref())
                };
                if url.is_some() && runtime::allowed(desc, "open-service") {
                    self.run_command("open-service", ctx, Some(&key))?;
                }
            }
            ViewEvent::Button(id) => {
                let key = if matches!(id.as_str(), "refresh" | "start" | "restart" | "stop" | "actions" | "open-config" | "config") {
                    PROJECT.into()
                } else {
                    self.selected.clone().unwrap_or(PROJECT.into())
                };
                self.run_command(&id, ctx, Some(&key))?;
            }
            _ => {}
        }
        Ok(())
    }
}

impl Details for Main {
    fn detail(&mut self, view: &str, key: &str, ctx: &Invocation) -> Result<Vec<Tab>, Error> {
        if view != VIEW {
            return Err(Error::NotFound(view.into()));
        }
        if is_status(key) {
            let message = match key {
                NO_PROJECT => "Open a project containing .ddev/config.yaml to inspect its services and use DDEV controls.",
                NOT_CONFIGURED => "This project has no DDEV configuration. Use Configure Project to review and create it. No runtime command runs when selecting this status.",
                _ => "DDEV status is unavailable. Check that DDEV is installed, Docker is running and the process:ddev permission is allowed in Settings › Plugins › DDEV Integration, then Refresh Status. Configuration and More Actions still work.",
            };
            return Ok(vec![Tab::new("overview", "Overview", Pane::Message(message.into()))]);
        }
        let project = ctx.project().ok_or("Open a project to use DDEV.")?;
        let cached = project
            .root()
            .is_ok_and(|root| self.snapshots.contains_key(&root));
        let _task = (!cached).then(|| host::begin_task("Reading DDEV status", false));
        let result = self.cached_description(&project).cloned();
        if key != PROJECT {
            let desc = result.as_ref().map_err(|e| Error::from(e.to_string()))?;
            if desc.service(key).is_none() {
                return Err(Error::NotFound(format!("DDEV service {key}")));
            }
        }
        self.selected = Some(key.to_owned());
        let _ = host::storage::set(&format!("selection:{}", project.root()?), key);
        let mut tabs = vec![];
        match result {
            Ok(desc) => {
                let overview = if key == PROJECT {
                    let mut lines = vec![format!("Project: {}", desc.name), format!("Status: {}", desc.status), format!("Folder: {}", project.root()?)];
                    if let Some(url) = &desc.url { lines.push(format!("Website: {url}")); }
                    for (label, field) in [("PHP", "php_version"), ("Node.js", "nodejs_version"), ("Web server", "webserver_type"), ("Docroot", "docroot")] {
                        let value = model::text(&desc.raw, field);
                        if !value.is_empty() { lines.push(format!("{label}: {value}")); }
                    }
                    lines.push(format!("Services: {}", desc.services.len()));
                    lines.push("\nSelect a service for its browser URL, shell or logs.\nMore Actions…: project controls, Database setup and container tools.\nGuided Setup: preferences and execution target.".into());
                    lines.join("\n")
                } else {
                    let service = desc.service(key).unwrap();
                    format!("Service: {}\nStatus: {}\nBrowser: {}\n\n{}", service.name, model::text(&service.info, "status"), service.url.as_deref().unwrap_or("No browser URL"), if service.logs { "Shell and Logs operate on this container." } else { "This is a browser link; it has no separate container shell or logs." })
                };
                tabs.push(Tab::new("overview", "Overview", Pane::Text(overview)));
                let (info, logs) = if key == PROJECT {(model::pretty(&desc.raw),true)} else {
                    let service = desc.service(key).unwrap(); (model::pretty(&service.info),service.logs)
                };
                tabs.push(Tab::new("inspect","Inspect",Pane::Text(info)));
                if logs { tabs.push(Tab::new("logs","Logs",self.log(&project,key)?)); }
                if (key == PROJECT || key == "service:db")
                    && let Ok(summary) = crate::database::summary(&desc)
                {
                    tabs.push(Tab::new("database", "Database", Pane::Text(summary)));
                }
            }
            Err(e) => tabs.push(Tab::new("overview","Overview",Pane::Text(format!("DDEV runtime is unavailable: {e}\n\n{} Also check Docker. Configuration can still be read and edited.", ui_kit::denied::message("Reading DDEV runtime details", "process:ddev", "DDEV Integration"))))),
        }
        if key == PROJECT {
            let config = match project.read_file(crate::CONFIG) {
                Ok(text) => Pane::Text(text),
                Err(Error::NotFound(_)) => {
                    Pane::Message("No .ddev/config.yaml. Run DDEV: Configure Project.".into())
                }
                Err(e) => Pane::Message(format!("Cannot read configuration: {e}")),
            };
            tabs.push(Tab::new("configuration", "Configuration", config));
            tabs.push(Tab::new("help", "Help", Pane::Text(runtime::HELP.into())));
        }
        Ok(tabs)
    }
}

#[cfg(test)]
mod reason_tests {
    use super::unavailable_reason;

    #[test]
    fn unavailable_reason_names_the_likely_cause() {
        assert_eq!(unavailable_reason("permission denied: process:ddev is not granted"), "DDEV permission needed");
        assert_eq!(unavailable_reason("ddev: command not found"), "DDEV not installed");
        assert_eq!(unavailable_reason("ddev describe failed (exit Some(1)): Could not connect to a Docker provider"), "Container runtime not running");
        assert_eq!(unavailable_reason("ddev describe failed (exit Some(1)): boom"), "DDEV unavailable");
    }
}
