use crate::{CONFIG, Main, PROJECT, check_updates, database, model::Description};
use openidea_plugin::{
    Error, Event, Invocation, LogLevel, PickItem, Plugin, Project, StatusItem, host,
};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
pub fn enabled(key: &str) -> bool {
    host::setting_bool(key).unwrap_or(true)
}

/// Project overrides parsed once per call. Windowless scheduled events cannot
/// ask the settings API for an arbitrary project, so the supported settings
/// file is read directly.
pub struct Prefs(Option<toml_edit::DocumentMut>);

impl Prefs {
    pub fn load(project: &Project) -> Self {
        Self(
            project
                .read_file(".openidea/settings.toml")
                .ok()
                .and_then(|text| text.parse().ok()),
        )
    }

    fn item(&self, key: &str) -> Option<&toml_edit::Item> {
        self.0.as_ref()?.get("plugins")?.get("aritti.ddev")?.get(key)
    }

    pub fn enabled(&self, key: &str) -> bool {
        self.item(key)
            .and_then(|v| v.as_bool())
            .unwrap_or_else(|| enabled(key))
    }

    pub fn string(&self, key: &str, default: &str) -> String {
        self.item(key)
            .and_then(|v| v.as_str())
            .map(str::to_owned)
            .unwrap_or_else(|| host::setting_string(key).unwrap_or_else(|| default.into()))
    }
}

pub fn enabled_for(project: &Project, key: &str) -> bool {
    Prefs::load(project).enabled(key)
}

pub fn update_key(project: &Project) -> Result<String, Error> {
    Ok(format!("update-check:{}", database::source_id(project)?))
}

pub fn allowed(desc: &Description, command: &str) -> bool {
    match command {
        "start" => !matches!(desc.status.as_str(), "running" | "starting"),
        "restart" | "stop" | "share" | "shell" | "composer" | "php" | "node" | "open-service" => {
            desc.status == "running"
        }
        "setup-database" | "copy-db-password" => database::connection(desc).is_ok(),
        _ => true,
    }
}

pub fn publish(desc: Option<&Description>, configured: bool) {
    let running = desc.is_some_and(|d| d.status == "running");
    let _ = host::set_context("known", true);
    let _ = host::set_context("configured", configured);
    let _ = host::set_context("available", desc.is_some());
    let _ = host::set_context(
        "has-database",
        desc.is_some_and(|d| d.raw.get("dbinfo").is_some_and(|db| db.is_object())),
    );
    let _ = host::set_context("running", running);
    let _ = host::set_context("can-restart-stop", running);
    let _ = host::set_context("can-start", desc.is_some_and(|d| allowed(d, "start")));
    if !configured && desc.is_none() {
        let _ = host::remove_window_status_item("ddev-status");
        return;
    }
    let label = desc.map_or_else(|| "DDEV: unavailable".into(), |d| format!("DDEV: {}", d.status));
    // API 1.4 overrides are window-scoped; never fall back to a global
    // project-specific value when invoked without a window.
    let _ = host::set_window_status_item(
        "ddev-status",
        &StatusItem {
            text: label,
            tooltip: Some("DDEV project actions".into()),
            command: Some("actions".into()),
        },
    );
}

pub fn automatic_updates(project: &Project) {
    if !enabled_for(project, "checkForUpdates") {
        return;
    }
    let Ok(key) = update_key(project) else { return };
    let Ok(last) = host::storage::get(&key) else {
        return;
    };
    let now = now();
    if last
        .and_then(|s| s.parse::<u64>().ok())
        .is_some_and(|last| now.saturating_sub(last) < 86400)
    {
        return;
    }
    // Requires granted storage; record attempts too, so offline/denied checks do
    // not run on each activation. No network is attempted without its grant.
    if host::storage::set(&key, &now.to_string()).is_err() {
        return;
    }
    if let Err(error) = check_updates(project, false)
        && !matches!(error, Error::Denied(_))
    {
        host::log(LogLevel::Warn, &format!("DDEV update check: {error}"));
    }
}

impl Main {
    pub(crate) fn integrations(
        &mut self,
        project: &Project,
        desc: &Description,
        interactive: bool,
    ) {
        let prefs = Prefs::load(project);
        self.sync_target_with(project, Some(desc), &prefs);
        if !prefs.enabled("autoConfigureDataSource")
            || desc.status != "running"
            || database::connection(desc).is_err()
        {
            if let Ok(root) = project.root() {
                self.connected_databases.remove(&root);
            }
            return;
        }
        match self.configure_database(project, desc, false) {
            Ok(_) => {
                if interactive
                    && let Ok(root) = project.root()
                    && !self.connected_databases.contains(&root)
                    && database::connect(project).is_ok()
                {
                    self.connected_databases.insert(root);
                }
            }
            Err(Error::Denied(_) | Error::Cancelled) => {}
            Err(error) => host::log(
                LogLevel::Warn,
                &format!("DDEV data source was not updated: {error}"),
            ),
        }
    }

    pub(crate) fn refresh_background(&mut self, project: &Project) {
        self.check_target_preferences(project, &Prefs::load(project));
        let result = crate::run_json(project, "describe").and_then(|o| Description::parse(&o));
        let desc = match result {
            Ok(desc) => desc,
            Err(error) => {
                self.sync_target(project, None);
                if let Ok(root) = project.root() {
                    self.failure.insert(root, error.to_string());
                }
                return;
            }
        };
        self.clear_failure_notice(project);
        self.integrations(project, &desc, false);
        if let Ok(root) = project.root() {
            self.failure.remove(&root);
            self.refreshed_at.insert(root.clone(), now());
            self.snapshots.insert(root, desc);
        }
    }

    pub(crate) fn on_events(&mut self, events: &[Event], ctx: &Invocation) {
        self.terminal_events(events, ctx);
        let scheduled = events.iter().any(|e| matches!(e, Event::Scheduled));
        if let Some(project) = ctx.project() {
            self.follow_config(&project);
        }
        let root = ctx.project().and_then(|p| p.root().ok());
        let affects = |path: &str| {
            root.as_deref()
                .is_some_and(|root| is_runtime_input(path, root))
        };
        let changed = events.iter().any(|e| match e {
            Event::FileSaved(path) => affects(path),
            Event::FilesChanged(batch) => batch
                .created
                .iter()
                .chain(&batch.modified)
                .chain(&batch.removed)
                .any(|path| affects(path)),
            Event::EditorFocused(_) => {
                ctx.project().and_then(|p| p.root().ok()).as_deref() != self.current_root.as_deref()
            }
            _ => false,
        });
        if scheduled {
            let projects = ctx
                .project()
                .map(|p| vec![p])
                .unwrap_or_else(|| host::projects().unwrap_or_default());
            for project in projects {
                if project.exists(CONFIG).unwrap_or(false) {
                    automatic_updates(&project);
                }
            }
        }
        let watching = ctx
            .project()
            .map(|p| enabled_for(&p, "watchDdev"))
            .unwrap_or_else(|| {
                host::projects()
                    .unwrap_or_default()
                    .iter()
                    .any(|p| enabled_for(p, "watchDdev"))
            });
        if !(changed || scheduled && watching) {
            if events
                .iter()
                .any(|event| matches!(event, Event::EditorFocused(_)))
                && let Some(project) = ctx.project()
            {
                // Scheduled reads cannot publish into a particular window.
                // Re-entering it republishes the cached state without forcing
                // another CLI read when that snapshot is still current.
                let _task = host::begin_task("Updating DDEV window state", false);
                match self.description(&project).cloned() {
                    Ok(desc) => self.integrations(&project, &desc, true),
                    Err(_) => {
                        self.publish_unavailable(&project, project.exists(CONFIG).unwrap_or(false))
                    }
                }
            }
            return;
        }
        let _task = host::begin_task("Refreshing DDEV status", false);
        self.invalidate();
        if let Some(project) = ctx.project() {
            if project.exists(CONFIG).unwrap_or(false) {
                if let Ok(desc) = self.description(&project).cloned() {
                    self.integrations(&project, &desc, true);
                } else {
                    self.publish_unavailable(&project, true);
                }
            } else {
                self.sync_target(&project, None);
                publish(None, false);
            }
        } else if scheduled {
            // The scheduler has no window context. Do not publish one project's
            // status/context keys globally into all the other windows.
            for project in host::projects().unwrap_or_default() {
                if !project.exists(CONFIG).unwrap_or(false) {
                    self.sync_target(&project, None);
                    continue;
                }
                if !enabled_for(&project, "watchDdev") {
                    continue;
                }
                self.refresh_background(&project);
            }
        }
    }

    pub(crate) fn actions(&mut self, ctx: &Invocation) -> Result<(), Error> {
        let project = ctx.project().ok_or("Open a project to use DDEV.")?;
        let desc = self.description(&project).ok().cloned();
        let configured = project.exists(CONFIG)?;
        let mut items = vec![
            PickItem::new("refresh", "Refresh status"),
            PickItem::new("setup", "Guided Setup"),
        ];
        if configured {
            items.push(PickItem::new("open-config", "Open configuration"));
        }
        if let Some(d) = &desc {
            if d.status == "running" {
                if let Some(url) = &d.url {
                    items.push(PickItem::new("open-url:project", "Open website").description(url));
                }
                for service in &d.services {
                    if let Some(url) = &service.url {
                        if service.name == "web" && Some(url) == d.url.as_ref() {
                            continue;
                        }
                        items.push(
                            PickItem::new(
                                &format!("open-url:service:{}", service.name),
                                &format!("Open {}", service.name),
                            )
                            .description(url),
                        );
                    }
                }
            }
            for (id, label) in [
                ("start", "Start project"),
                ("restart", "Restart project"),
                ("stop", "Stop project"),
                ("share", "Share project"),
                ("shell", "Open shell"),
                ("open-service", "Open service in browser"),
                ("logs", "Open logs"),
                ("composer", "Run Composer…"),
                ("php", "Run PHP…"),
                ("node", "Run Node.js…"),
                ("setup-database", "Configure database data source"),
                ("delete", "Delete project"),
                ("poweroff", "Power off all projects"),
            ] {
                if allowed(d, id) {
                    items.push(PickItem::new(id, label));
                }
            }
        }
        if desc.is_none() && configured && self.cli_is_available(&project) {
            // These original actions depend on installation/configuration, not
            // on a successful describe response (which may itself need repair).
            items.push(PickItem::new("start", "Start project"));
            items.push(PickItem::new("restart", "Restart project"));
            items.push(PickItem::new("stop", "Stop project"));
            items.push(PickItem::new("delete", "Delete project"));
            items.push(PickItem::new("poweroff", "Power off all projects"));
        }
        items.push(PickItem::new("config", "Configure project"));
        items.push(PickItem::new("check-updates", "Check for updates"));
        items.push(PickItem::new("diagnose", "Check environment"));
        items.push(PickItem::new("open-guide", "Installation guide"));
        let title = desc.as_ref().map_or("DDEV".into(), |d| {
            format!("DDEV: {} — {}", d.name, d.status)
        });
        if let Some(id) = host::pick(&title, &items)? {
            if !items.iter().any(|i| i.key == id) {
                return Err("Unknown DDEV action.".into());
            }
            if let Some(key) = id.strip_prefix("open-url:") {
                self.run_command("open-service", ctx, Some(key))?;
                return Ok(());
            }
            self.run_command(
                &id,
                ctx,
                if matches!(id.as_str(), "shell" | "logs") {
                    Some(PROJECT)
                } else {
                    None
                },
            )?;
        }
        Ok(())
    }
}

fn is_runtime_input(path: &str, root: &str) -> bool {
    let path = path.replace('\\', "/");
    let root = root.replace('\\', "/");
    let prefix = format!("{}/", root.trim_end_matches('/'));
    let windows = root.as_bytes().get(1) == Some(&b':')
        && root.as_bytes().first().is_some_and(u8::is_ascii_alphabetic);
    let relative = if let Some(relative) = path.strip_prefix(&prefix) {
        relative
    } else if windows
        && path
            .get(..prefix.len())
            .is_some_and(|p| p.eq_ignore_ascii_case(&prefix))
    {
        &path[prefix.len()..]
    } else if path.starts_with('/') || path.as_bytes().get(1) == Some(&b':') || path.contains("://")
    {
        return false;
    } else {
        path.as_str()
    };
    let relative = if windows {
        relative.to_ascii_lowercase()
    } else {
        relative.to_owned()
    };
    if matches!(
        relative.as_str(),
        ".ddev" | ".openidea" | ".openidea/settings.toml"
    ) {
        return true;
    }
    let Some((dir, file)) = relative.rsplit_once('/') else {
        return false;
    };
    dir == ".ddev"
        && ((file == "config.yaml" || file.starts_with("config.") && file.ends_with(".yaml"))
            || (file == "docker-compose.yaml"
                || file.starts_with("docker-compose.") && file.ends_with(".yaml")))
}

pub const HELP: &str = "DDEV Integration\n\nOpen the folder containing .ddev/config.yaml. Allow process:ddev in Settings › Plugins › DDEV Integration. Start Docker, then use DDEV: Start. The host's process permission page also chooses an approved executable globally or for this project.\n\nServices shows project status, containers, browser links, configuration and streaming logs. Overview summarizes a selection; Inspect redacts credential fields. Right-click for project actions. The status-bar picker includes direct service links.\n\nStatus refreshes every minute when enabled, on configuration changes and when a plugin terminal exits or closes. Explicit refresh remains available. Native terminals add DDEV_NONINTERACTIVE=true; an inherited variable with that name cannot be overwritten by the host. Configure opens its YAML after successful completion.\n\nAllow database.sources for native Database setup, secure credential storage and connection/introspection requests. Windowless startup only maintains the source. Without that permission, project.write can maintain a non-secret legacy connection file with manual password entry.\n\nThe DDEV project run target supports host-selected PHP/Node/Composer runs and tests with path mapping. Registration never changes the selected target; accept the host prompt or choose DDEV in Run/Debug/interpreter settings. Guided Setup controls registration and the container project path (/var/www/html by default). Install language servers in the container before explicitly selecting a target LSP. Enable/configure Xdebug in DDEV for supported PHP debugging.\n\nTerminal profile-menu/typed-command interception and JetBrains-specific SDK objects are not reproduced.\n\nDDEV installation: https://docs.ddev.com/en/stable/users/install/\nOriginal plugin: https://github.com/ddev/ddev-intellij-plugin\n";
