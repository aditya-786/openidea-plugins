//! Shared-kit setup/recovery UI and file-driven configuration follow-up.
use crate::{CONFIG, Main, database, model, runtime};
use openidea_plugin::views::{Body, Button, Field, FieldValue, View, ViewEvent};
use openidea_plugin::{
    Error, Invocation, NotificationAction, NotificationLevel, Plugin, Project, host,
};
use toml_edit::{DocumentMut, Item, Table, value};

pub const SETUP: &str = "setup";

pub(crate) fn refresh_setup() {
    host::refresh_view(SETUP);
    let _ = host::post_view_message(SETUP, r#"{"channel":"ui-kit-refresh"}"#);
}

pub const GUIDE: &str = "https://docs.ddev.com/en/stable/users/install/";

pub fn key(project: &Project, kind: &str) -> Result<String, Error> {
    Ok(format!("{kind}:{}", database::source_id(project)?))
}
pub fn fingerprint(text: &str) -> String {
    format!(
        "{:016x}",
        text.bytes()
            .fold(0xcbf29ce484222325_u64, |h, b| (h ^ u64::from(b))
                .wrapping_mul(0x100000001b3))
    )
}

pub fn valid_container_root(path: &str) -> bool {
    path.starts_with('/')
        && path.len() <= 4096
        && !path.chars().any(char::is_control)
        && !path.split('/').any(|part| part == "..")
}

#[derive(Clone)]
pub struct PendingConfig {
    root: String,
    operation: String,
    terminal: String,
    requested: u64,
}
impl PendingConfig {
    pub fn prepare(project: &Project, args: &[String], plugin: &Main) -> Result<Self, Error> {
        let root = project.root()?;
        let operation = crate::execution::terminal_key(project, "config", args)?;
        if let Some(pending) = plugin
            .pending_configs
            .get(&root)
            .cloned()
            .or_else(|| Self::restore(project))
        {
            if pending.operation == operation {
                return Ok(pending);
            }
            if runtime::now().saturating_sub(pending.requested) <= 1800 {
                return Err("DDEV Configure is already in progress for this project. Wait for its terminal to finish or close it before changing configuration arguments.".into());
            }
        }
        Ok(Self {
            root,
            operation,
            terminal: String::new(),
            requested: runtime::now(),
        })
    }
    pub fn remember(mut self, plugin: &mut Main, project: &Project, terminal: String) {
        // A reusable session key can receive a fresh operation after exit.
        // Keep the deadline when merely focusing the same live operation, but
        // start a new deadline for its replacement (also after idle reclaim).
        if self.terminal != terminal {
            self.requested = runtime::now();
        }
        self.terminal = terminal;
        if let Ok(key) = key(project, "pending-config") {
            let json = serde_json::json!({"requested":self.requested,"operation":self.operation,"terminal":self.terminal});
            let _ = host::storage::set(&key, &json.to_string());
        }
        plugin.pending_configs.insert(self.root.clone(), self);
    }
    fn restore(project: &Project) -> Option<Self> {
        let text = host::storage::get(&key(project, "pending-config").ok()?).ok()??;
        let json: serde_json::Value = serde_json::from_str(&text).ok()?;
        Some(Self {
            root: project.root().ok()?,
            operation: json.get("operation")?.as_str()?.into(),
            terminal: json.get("terminal")?.as_str()?.into(),
            requested: json.get("requested")?.as_u64()?,
        })
    }
}

#[derive(Clone)]
pub struct SetupDraft {
    watch: bool,
    updates: bool,
    database: bool,
    php: String,
    target: bool,
    container_root: String,
}

fn settings(project: &Project) -> Result<(String, DocumentMut), Error> {
    let old = match project.read_file(".openidea/settings.toml") {
        Ok(text) => text,
        Err(Error::NotFound(_)) => String::new(),
        Err(error) => return Err(error),
    };
    let doc = old
        .parse::<DocumentMut>()
        .map_err(|e| Error::from(format!("Project settings were not changed: {e}")))?;
    Ok((old, doc))
}

/// Preserve unrelated settings; edits are applied only after the form's Save.
pub fn save_preferences(project: &Project, values: &[(&str, Item)]) -> Result<(), Error> {
    let (old, mut doc) = settings(project)?;
    if doc.get("plugins").is_none() {
        doc["plugins"] = Item::Table(Table::new());
    }
    let inline = doc["plugins"].is_inline_table();
    let plugins = doc["plugins"]
        .as_table_like_mut()
        .ok_or("The plugins settings must be a table; left unchanged.")?;
    if !plugins.contains_key("aritti.ddev") {
        let own = if inline {
            Item::Value(toml_edit::InlineTable::new().into())
        } else {
            Item::Table(Table::new())
        };
        plugins.insert("aritti.ddev", own);
    }
    let own = plugins
        .get_mut("aritti.ddev")
        .and_then(Item::as_table_like_mut)
        .ok_or("DDEV settings must be a table; left unchanged.")?;
    for (name, val) in values {
        if !matches!(
            *name,
            "watchDdev"
                | "checkForUpdates"
                | "autoConfigureDataSource"
                | "phpVersion"
                | "autoConfigureRunTarget"
                | "containerRoot"
        ) {
            return Err("Unknown DDEV preference.".into());
        }
        own.insert(name, val.clone());
    }
    let updated = doc.to_string();
    if updated != old {
        project.write_file(".openidea/settings.toml", &updated)?;
    }
    Ok(())
}

pub fn read_php_preference(project: &Project) -> String {
    read_string(project, "phpVersion", "")
}

pub fn read_string(project: &Project, key: &str, default: &str) -> String {
    settings(project)
        .ok()
        .and_then(|(_, doc)| {
            doc.get("plugins")?
                .get("aritti.ddev")?
                .get(key)?
                .as_str()
                .map(str::to_owned)
        })
        .unwrap_or_else(|| host::setting_string(key).unwrap_or_else(|| default.into()))
}

impl Main {
    pub(crate) fn finish_config(
        &mut self,
        project: &Project,
        operation: &str,
        terminal: &str,
        success: bool,
    ) {
        let Ok(root) = project.root() else { return };
        let pending = self
            .pending_configs
            .get(&root)
            .cloned()
            .or_else(|| PendingConfig::restore(project));
        let Some(pending) = pending else { return };
        if pending.operation != operation || pending.terminal != terminal {
            return;
        }
        self.pending_configs.remove(&root);
        if let Ok(key) = key(project, "pending-config") {
            let _ = host::storage::delete(&key);
        }
        if success
            && runtime::now().saturating_sub(pending.requested) <= 1800
            && project.exists(CONFIG).unwrap_or(false)
        {
            let _ = host::open_location(&format!("{}/{CONFIG}", root.trim_end_matches('/')), 0, 0);
        }
    }

    pub(crate) fn report_failure(&mut self, project: &Project, error: &Error) {
        let Ok(root) = project.root() else { return };
        let storage_key = key(project, "failure-notice").ok();
        // Upstream latches a command failure until a successful read. Changes
        // in error wording (socket paths, timeouts, etc.) are still one outage.
        // Any old persisted signature also counts as an existing notice.
        let seen = self.reported_failures.contains(&root)
            || storage_key
                .as_ref()
                .and_then(|k| host::storage::get(k).ok().flatten())
                .is_some();
        if seen {
            return;
        }
        let body = format!(
            "Project: {root}\n{error}\n\nCheck DDEV's installation, process permission and container runtime, then retry."
        );
        if host::notify_with_actions(
            NotificationLevel::Warning,
            "DDEV unavailable",
            &body,
            &[
                NotificationAction::new("Retry", "refresh").argument(format!("project:{root}")),
                NotificationAction::new("Setup", "setup").argument(format!("project:{root}")),
                NotificationAction::new("Installation guide", "open-guide"),
            ],
        )
        .is_ok()
        {
            self.reported_failures.insert(root);
            if let Some(k) = storage_key {
                let _ = host::storage::set(&k, "reported");
            }
        }
    }
    pub(crate) fn clear_failure_notice(&mut self, project: &Project) {
        if let Ok(root) = project.root() {
            self.reported_failures.remove(&root);
        }
        if let Ok(k) = key(project, "failure-notice") {
            let _ = host::storage::delete(&k);
        }
    }
    pub(crate) fn offer_tour(&mut self, project: &Project) {
        let Ok(k) = key(project, "setup-tour") else {
            return;
        };
        let Ok(seen) = host::storage::get(&k) else {
            return;
        };
        if seen.is_some() {
            return;
        }
        let Ok(root) = project.root() else { return };
        if host::notify_with_actions(NotificationLevel::Info,"DDEV project detected",
            "Services > DDEV contains project controls, container shells, browser links and logs. The status-bar item opens project actions.",
            &[NotificationAction::new("Show Services","show-services").argument(format!("project:{root}")),NotificationAction::new("Guided setup","setup").argument(format!("project:{root}"))]).is_ok() {
            let _=host::storage::set(&k,"shown");
        }
    }
    pub(crate) fn follow_config(&mut self, project: &Project) {
        let Ok(root) = project.root() else { return };
        let pending = self
            .pending_configs
            .get(&root)
            .cloned()
            .or_else(|| PendingConfig::restore(project));
        let Some(pending) = pending else { return };
        // Completion is delivered by the owned terminal even across reclaim.
        // A file event alone cannot prove `ddev config` succeeded.
        if runtime::now().saturating_sub(pending.requested) <= 1800 {
            return;
        }
        self.pending_configs.remove(&root);
        if let Ok(k) = key(project, "pending-config") {
            let _ = host::storage::delete(&k);
        }
    }
    pub(crate) fn render_setup(&mut self, ctx: &Invocation) -> Result<View, Error> {
        let project = ctx.project().ok_or("Open a project for DDEV setup.")?;
        let root = project.root()?;
        let draft = self
            .setup_drafts
            .entry(root.clone())
            .or_insert_with(|| SetupDraft {
                watch: runtime::enabled_for(&project, "watchDdev"),
                updates: runtime::enabled_for(&project, "checkForUpdates"),
                database: runtime::enabled_for(&project, "autoConfigureDataSource"),
                php: read_php_preference(&project),
                target: runtime::enabled_for(&project, "autoConfigureRunTarget"),
                container_root: read_string(&project, "containerRoot", "/var/www/html"),
            });
        let configured = project.exists(CONFIG)?;
        let diagnostics = self
            .diagnostics
            .as_ref()
            .filter(|(r, _)| r == &root)
            .map_or(
                "Use Check Environment to read the installed DDEV and Docker versions.",
                |(_, s)| s.as_str(),
            );
        let mut fields = vec![
            Field::new(
                "project",
                "Project",
                FieldValue::readonly_text(&root, false),
            ),
            Field::new(
                "steps",
                "Setup",
                FieldValue::readonly_text(
                    "1. Install DDEV and start a supported container runtime.\n2. Allow DDEV execution in the plugin permissions.\n3. Configure this project, then start it from Services > DDEV.\n4. Select a service for its shell, browser URL, inspect information or logs.",
                    true,
                ),
            ),
            Field::new(
                "config-status",
                "Configuration",
                FieldValue::readonly_text(
                    if configured {
                        ".ddev/config.yaml exists"
                    } else {
                        "Not configured. Configure Project creates .ddev/config.yaml."
                    },
                    false,
                ),
            ),
            Field::new(
                "diagnostics",
                "Environment",
                FieldValue::readonly_text(diagnostics, true),
            ),
            Field::new(
                "binary-help",
                "DDEV executable",
                FieldValue::readonly_text(
                    "Settings › Plugins › DDEV Integration › process:ddev lets you choose and approve the executable globally or for this project. All DDEV commands, logs and execution targets use that host-approved choice.",
                    true,
                ),
            ),
            Field::new(
                "watchDdev",
                "Refresh DDEV status every minute",
                FieldValue::Checkbox(draft.watch),
            ),
            Field::new(
                "checkForUpdates",
                "Check for DDEV updates automatically",
                FieldValue::Checkbox(draft.updates),
            ),
            Field::new(
                "autoConfigureDataSource",
                "Keep a Database connection for the DDEV database",
                FieldValue::Checkbox(draft.database),
            ),
            Field::new(
                "phpVersion",
                "PHP version for Configure Project (optional)",
                FieldValue::Text(draft.php.clone()),
            ),
            Field::new(
                "autoConfigureRunTarget",
                "Offer DDEV for PHP, Node.js, Composer and project runs",
                FieldValue::Checkbox(draft.target),
            ),
            Field::new(
                "containerRoot",
                "Project path inside the web container",
                FieldValue::Text(draft.container_root.clone()),
            ),
            Field::new(
                "execution-help",
                "Run and debug",
                FieldValue::readonly_text(
                    "Accept the IDE's DDEV execution-target prompt, or choose DDEV in Run/Debug settings. Project paths map into the web container. Enable Xdebug in DDEV for PHP debugging. Target registration does not change your selected interpreter or target.",
                    true,
                ),
            ),
        ];
        // The setup checklist and configuration status come first, then the
        // preferences, then the longer help text. Field ids preserve draft and Save behavior.
        let order = [
            "project",
            "steps",
            "config-status",
            "watchDdev",
            "checkForUpdates",
            "autoConfigureDataSource",
            "autoConfigureRunTarget",
            "containerRoot",
            "phpVersion",
            "execution-help",
            "diagnostics",
            "binary-help",
        ];
        fields.sort_by_key(|field| {
            order
                .iter()
                .position(|key| *key == field.key)
                .unwrap_or(usize::MAX)
        });
        Ok(View::new(Body::Form(fields))
            .button(Button::new("diagnose", "Check Environment").icon("refresh"))
            .button(Button::new("config", "Configure Project").icon("settings"))
            .button(Button::new("show-services", "Show Services").icon("open-in-tool-window"))
            .button(Button::new("save-settings", "Save Preferences").icon("check"))
            .button(Button::new("reset-settings", "Reset Changes").icon("undo"))
            .button(Button::new("open-docs", "DDEV Documentation").icon("help"))
            .status("Preferences apply to this project after Save. Saving requires the optional project.write permission."))
    }
    pub(crate) fn handle_setup(&mut self, event: ViewEvent, ctx: &Invocation) -> Result<(), Error> {
        self.render_setup(ctx)?;
        let root = ctx.project().unwrap().root()?;
        match event {
            ViewEvent::Field(name, val) => {
                let draft = self.setup_drafts.get_mut(&root).unwrap();
                match name.as_str() {
                    "watchDdev"
                    | "checkForUpdates"
                    | "autoConfigureDataSource"
                    | "autoConfigureRunTarget" => {
                        let value = match val.as_str() {
                            "true" => true,
                            "false" => false,
                            _ => return Err("Invalid checkbox value.".into()),
                        };
                        match name.as_str() {
                            "watchDdev" => draft.watch = value,
                            "checkForUpdates" => draft.updates = value,
                            "autoConfigureDataSource" => draft.database = value,
                            _ => draft.target = value,
                        };
                    }
                    "phpVersion" => draft.php = val,
                    "containerRoot" => draft.container_root = val,
                    _ => return Err("Unknown setup field.".into()),
                }
            }
            ViewEvent::Button(id) if id == "reset-settings" => {
                self.setup_drafts.remove(&root);
            }
            ViewEvent::Button(id) if id == "save-settings" => {
                let project = ctx.project().unwrap();
                let draft = self.setup_drafts.get(&root).unwrap();
                if !draft.php.trim().is_empty()
                    && crate::normalize_php_version(draft.php.trim(), false).is_none()
                {
                    return Err("PHP version must be major.minor, for example 8.3.".into());
                }
                let container_root = draft.container_root.trim();
                if !valid_container_root(container_root) {
                    return Err("Container project path must be absolute without parent traversal, for example /var/www/html.".into());
                }
                save_preferences(
                    &project,
                    &[
                        ("watchDdev", value(draft.watch)),
                        ("checkForUpdates", value(draft.updates)),
                        ("autoConfigureDataSource", value(draft.database)),
                        ("phpVersion", value(draft.php.trim())),
                        ("autoConfigureRunTarget", value(draft.target)),
                        ("containerRoot", value(container_root)),
                    ],
                )?;
                self.invalidate();
                // Apply withdrawal even if DDEV is unavailable. A stale target
                // must not survive an explicit disable or mapping change.
                self.sync_target(&project, None);
                // Like the original settings apply/reinitialize path, apply
                // enabled integrations now, even with periodic watching off.
                // Optional runtime denial must not prevent saving preferences.
                if project.exists(CONFIG).unwrap_or(false) {
                    let _task = host::begin_task("Applying DDEV preferences", false);
                    self.refresh_background(&project);
                }
                host::notify(
                    NotificationLevel::Info,
                    "DDEV preferences",
                    "Project preferences saved.",
                );
            }
            ViewEvent::Button(id) => {
                self.run_command(&id, ctx, None)?;
            }
            _ => {}
        }
        refresh_setup();
        Ok(())
    }
    pub(crate) fn diagnose(&mut self, project: &Project) -> Result<(), Error> {
        let _task = host::begin_task("Checking the DDEV environment", false);
        let result = crate::run_ddev(project, &["version".into(), "--json-output".into()]);
        let text = match result {
            Ok(output) => {
                let mut lines = vec![format!("DDEV version command exit: {:?}", output.status)];
                if let Ok(raw) = model::raw_output(&output.stdout) {
                    for name in [
                        "DDEV version",
                        "docker",
                        "docker-compose",
                        "docker-platform",
                        "docker-api",
                        "architecture",
                        "os",
                    ] {
                        let value = model::text(&raw, name);
                        if !value.is_empty() {
                            lines.push(format!("{name}: {value}"));
                        }
                    }
                }
                if output.status != Some(0) {
                    lines.push("The environment check failed. Start the container runtime and check DDEV's installation guide.".into());
                }
                lines.join("\n")
            }
            Err(error) => format!(
                "Cannot run the DDEV environment check: {error}\n{} Also verify that DDEV is installed.",
                ui_kit::denied::message("The DDEV environment check", "process:ddev", "DDEV Integration")
            ),
        };
        self.diagnostics = Some((project.root()?, text));
        refresh_setup();
        Ok(())
    }
}
