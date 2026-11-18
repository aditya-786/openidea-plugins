//! DDEV integration for OpenIDEA.
mod configuration;
mod database;
mod execution;
mod model;
mod runtime;
mod services;
mod support;
mod version;

use model::{Description, text};
use openidea_plugin::details::ProcessLog;
use openidea_plugin::{
    Error, HttpRequest, Invocation, NotificationLevel, PickItem, Plugin, Project, host,
    register_plugin,
};

const VIEW: &str = "ddev";
const PROJECT: &str = "project";
const CONFIG: &str = ".ddev/config.yaml";

#[derive(Default)]
pub struct Main {
    snapshots: std::collections::BTreeMap<String, Description>,
    selected: Option<String>,
    live_selections: std::collections::BTreeMap<String, Option<String>>,
    current_root: Option<String>,
    refreshed_at: std::collections::BTreeMap<String, u64>,
    failure: std::collections::BTreeMap<String, String>,
    reported_failures: std::collections::BTreeSet<String>,
    pending_configs: std::collections::BTreeMap<String, support::PendingConfig>,
    setup_drafts: std::collections::BTreeMap<String, support::SetupDraft>,
    cli_available: Option<(String, bool)>,
    diagnostics: Option<(String, String)>,
    run_targets: std::collections::BTreeMap<String, openidea_plugin::PluginRunTarget>,
    connected_databases: std::collections::BTreeSet<String>,
    database_states: std::collections::BTreeMap<String, database::Applied>,
    #[cfg(test)]
    terminal_launches: Vec<host::Command>,
}

impl Main {
    fn invalidate(&mut self) {
        self.snapshots.clear();
        self.failure.clear();
        self.cli_available = None;
        host::refresh_view(VIEW);
    }

    fn sync_selection(&mut self, project: &Project) -> Result<(), Error> {
        let root = project.root()?;
        if self.current_root.as_deref() != Some(&root) {
            if let Some(previous) = &self.current_root {
                self.live_selections
                    .insert(previous.clone(), self.selected.clone());
            }
            // Session selection does not depend on the optional persistence
            // grant; storage is only the fallback after idle reclaim.
            self.selected = self.live_selections.get(&root).cloned().unwrap_or_else(|| {
                host::storage::get(&format!("selection:{root}"))
                    .ok()
                    .flatten()
            });
            self.current_root = Some(root);
        }
        Ok(())
    }

    fn description(&mut self, project: &Project) -> Result<&Description, Error> {
        self.load(project, true, &runtime::Prefs::load(project))
    }

    fn cached_description(&mut self, project: &Project) -> Result<&Description, Error> {
        self.load(project, false, &runtime::Prefs::load(project))
    }

    fn is_stale_with(&self, project: &Project, prefs: &runtime::Prefs) -> bool {
        let Ok(root) = project.root() else {
            return false;
        };
        prefs.enabled("watchDdev")
            && runtime::now().saturating_sub(self.refreshed_at.get(&root).copied().unwrap_or(0))
                >= 60
    }

    fn load(
        &mut self,
        project: &Project,
        refresh: bool,
        prefs: &runtime::Prefs,
    ) -> Result<&Description, Error> {
        self.sync_selection(project)?;
        self.check_target_preferences(project, prefs);
        let root = project.root()?;
        // Configuration can disappear while watching is disabled or before the
        // next filesystem event. Never return a cached runtime for that case.
        if !project.exists(CONFIG)? {
            self.sync_target(project, None);
            self.snapshots.remove(&root);
            self.failure.remove(&root);
            self.cli_available = None;
            return Err(
                "No .ddev/config.yaml in the project root. Run DDEV: Configure Project first."
                    .into(),
            );
        }
        let expired = refresh && self.is_stale_with(project, prefs);
        if !expired
            && let Some(message) = self.failure.get(&root)
        {
            return Err(message.clone().into());
        }
        if expired || !self.snapshots.contains_key(&root) {
            self.snapshots.remove(&root);
            let desc = match run_json(project, "describe")
                .and_then(|output| Description::parse(&output))
            {
                Ok(desc) => {
                    self.failure.remove(&root);
                    desc
                }
                Err(error) => {
                    self.sync_target(project, None);
                    self.refreshed_at.insert(root.clone(), runtime::now());
                    self.report_failure(project, &error);
                    self.failure.insert(root, error.to_string());
                    return Err(error);
                }
            };
            self.clear_failure_notice(project);
            self.offer_tour(project);
            self.refreshed_at.insert(root.clone(), runtime::now());
            self.snapshots.insert(root.clone(), desc);
        }
        let desc = self.snapshots[&root].clone();
        self.sync_target_with(project, Some(&desc), prefs);
        let desc = &self.snapshots[&root];
        // A windowless scheduled refresh can replace the snapshot without
        // entering the interactive load branch. Validate selection on reuse too.
        if self
            .selected
            .as_deref()
            .is_some_and(|key| key != PROJECT && desc.service(key).is_none())
        {
            self.selected = None;
        }
        runtime::publish(Some(desc), true);
        Ok(desc)
    }

    fn fresh_description(&self, project: &Project) -> Option<Description> {
        let root = project.root().ok()?;
        let age = runtime::now().saturating_sub(*self.refreshed_at.get(&root)?);
        (age < 60).then(|| self.snapshots.get(&root).cloned())?
    }

    // Upstream permits Start when DDEV is installed and configured even when
    // describe fails (for example before the first successful project start).
    fn can_start(&mut self, project: &Project) -> bool {
        self.can_start_with(project, true, &runtime::Prefs::load(project))
    }

    fn can_start_with(&mut self, project: &Project, refresh: bool, prefs: &runtime::Prefs) -> bool {
        if let Ok(desc) = self.load(project, refresh, prefs) {
            return runtime::allowed(desc, "start");
        }
        if !project.exists(CONFIG).unwrap_or(false) {
            return false;
        }
        self.cli_is_available(project)
    }

    fn cli_is_available(&mut self, project: &Project) -> bool {
        let Ok(root) = project.root() else {
            return false;
        };
        if let Some((cached_root, available)) = &self.cli_available
            && cached_root == &root
        {
            return *available;
        }
        let available = run_ddev(project, &["--version".into()])
            .is_ok_and(|out| out.status == Some(0) && installed_version(&out.stdout).is_ok());
        self.cli_available = Some((root, available));
        available
    }

    fn publish_unavailable(&mut self, project: &Project, configured: bool) {
        runtime::publish(None, configured);
        let can_start = configured && self.can_start(project);
        let _ = host::set_context("can-start", can_start);
        let _ = host::set_context("available", can_start);
        let _ = host::set_context("can-restart-stop", can_start);
    }

    fn target(&mut self, project: &Project, arg: Option<&str>) -> Result<String, Error> {
        // Loading a new project's snapshot clears any selection from the previous project.
        self.cached_description(project)?;
        let key = arg
            .map(str::to_owned)
            .or_else(|| self.selected.clone())
            .unwrap_or(PROJECT.into());
        let desc = self.cached_description(project)?;
        if key != PROJECT && desc.service(&key).is_none() {
            return Err(Error::NotFound(format!("DDEV service {key}")));
        }
        Ok(key)
    }

    fn log(&mut self, project: &Project, key: &str) -> Result<ProcessLog, Error> {
        let desc = self.cached_description(project)?;
        if !model::safe_name(&desc.name) {
            return Err("DDEV returned an invalid project name for log streaming.".into());
        }
        let name = if key == PROJECT {
            "web"
        } else {
            let service = desc.service(key).ok_or("Unknown DDEV service.")?;
            if !service.logs {
                return Err("This link has no separate container log.".into());
            }
            &service.name
        };
        let service_arg = format!("--service={name}");
        Ok(ProcessLog::new(
            "ddev",
            &["logs", &desc.name, "--follow", "--tail=200", &service_arg],
        )
        .toggle("timestamps", "Timestamps", &["--time"]))
    }

    fn open_service(&mut self, project: &Project, arg: Option<&str>) -> Result<(), Error> {
        let desc = self.description(project)?;
        if !runtime::allowed(desc, "open-service") {
            return Err("Start the DDEV project before opening its service URLs.".into());
        }
        if let Some(key) = arg {
            let link = if key == PROJECT {
                desc.url.as_ref()
            } else {
                desc.service(key)
                    .ok_or("Unknown DDEV service.")?
                    .url
                    .as_ref()
            }
            .ok_or("This DDEV service has no browser URL.")?;
            return host::open_url(link);
        }
        let mut links = vec![];
        if let Some(link) = &desc.url {
            links.push((PROJECT.to_owned(), "Project".to_owned(), link.clone()));
        }
        links.extend(desc.services.iter().filter_map(|s| {
            Some((
                format!("service:{}", s.name),
                s.name.clone(),
                s.url.clone()?,
            ))
        }));
        if links.is_empty() {
            return Err(
                "DDEV reported no service URLs. Start the project and refresh status.".into(),
            );
        }
        let items: Vec<_> = links
            .iter()
            .map(|(key, name, link)| PickItem::new(key, name).description(link))
            .collect();
        if let Some(key) = host::pick("Open DDEV service", &items)? {
            let (_, _, link) = links
                .iter()
                .find(|(id, _, _)| id == &key)
                .ok_or("Unknown DDEV service.")?;
            host::open_url(link)?;
        }
        Ok(())
    }
}

impl Plugin for Main {
    fn activate(&mut self, _ctx: &openidea_plugin::Context) -> Result<(), Error> {
        configuration::initialize();
        host::refresh_view(VIEW);
        support::refresh_setup();
        for kind in [
            openidea_plugin::EventKind::FileSaved,
            openidea_plugin::EventKind::EditorFocused,
        ] {
            let _ = host::subscribe(kind);
        }
        let _ = host::subscribe_paths(
            "files-changed",
            &[
                ".ddev",
                ".openidea",
                ".ddev/config*.yaml",
                ".ddev/docker-compose*.yaml",
                ".openidea/settings.toml",
            ],
        );
        // Startup has no invoking window: only initialize global context when
        // exactly one project is open. Window renders supply their own keys.
        let projects = host::projects().unwrap_or_default();
        if projects.len() == 1 && projects[0].exists(CONFIG).unwrap_or(false) {
            let _task = host::begin_task("Initializing DDEV", false);
            if let Ok(desc) = self.description(&projects[0]).cloned() {
                self.integrations(&projects[0], &desc, false);
            }
            runtime::automatic_updates(&projects[0]);
        } else if projects.len() > 1 {
            // The original initializes each project. With no invoking window,
            // use the quiet path to avoid broadcasting one project's contexts.
            let _task = host::begin_task("Initializing DDEV projects", false);
            for project in projects {
                if project.exists(CONFIG).unwrap_or(false) {
                    self.refresh_background(&project);
                    runtime::automatic_updates(&project);
                }
            }
        }
        Ok(())
    }

    fn handle_events(&mut self, events: &[openidea_plugin::Event], ctx: &Invocation) {
        self.on_events(events, ctx);
    }

    fn run_command(
        &mut self,
        id: &str,
        ctx: &Invocation,
        arg: Option<&str>,
    ) -> Result<Option<String>, Error> {
        if let Some(expected_root) = arg.and_then(|arg| arg.strip_prefix("project:")) {
            let origin = ctx
                .project()
                .ok_or("Open the originating project's window to use this DDEV action.")?;
            if origin.root()? != expected_root {
                return Err(format!("This DDEV action belongs to {expected_root}. Run it from that project's window.").into());
            }
        }
        // Documentation/release links and revealing Services do not need
        // a configured (or even open) project, like the original help actions.
        match id {
            "open-guide" => {
                host::open_url(support::GUIDE)?;
                return Ok(None);
            }
            "open-releases" => {
                host::open_url("https://github.com/ddev/ddev/releases/latest")?;
                return Ok(None);
            }
            "open-docs" => {
                host::open_url("https://docs.ddev.com/en/stable/")?;
                return Ok(None);
            }
            "show-services" => {
                host::reveal_view(VIEW)?;
                return Ok(None);
            }
            _ => {}
        }
        let project = ctx.project().ok_or("Open a project to use DDEV.")?;
        let runtime_command = matches!(
            id,
            "start"
                | "restart"
                | "stop"
                | "share"
                | "shell"
                | "composer"
                | "php"
                | "node"
                | "open-service"
                | "logs"
                | "setup-database"
                | "copy-db-password"
                | "delete"
                | "poweroff"
        );
        let _task = runtime_command.then(|| host::begin_task("Checking DDEV project state", false));
        if runtime_command {
            // Commands must guard against the current CLI result, even if the
            // Services tree still holds a recent snapshot or watching is off.
            self.invalidate();
        }
        match id {
            "refresh" => {
                self.follow_config(&project);
                self.invalidate();
                match self.description(&project).cloned() {
                    Ok(desc) => self.integrations(&project, &desc, true),
                    Err(error) => {
                        self.publish_unavailable(&project, project.exists(CONFIG).unwrap_or(false));
                        return Err(error);
                    }
                }
            }
            "actions" => self.actions(ctx)?,
            "setup" => {
                host::reveal_view(support::SETUP)?;
            }
            "diagnose" => {
                self.diagnose(&project)?;
                host::reveal_view(support::SETUP)?;
            }
            "disable-update-checks" => {
                support::save_preferences(
                    &project,
                    &[("checkForUpdates", toml_edit::value(false))],
                )?;
                self.setup_drafts.remove(&project.root()?);
                support::refresh_setup();
                host::notify(
                    NotificationLevel::Info,
                    "DDEV updates",
                    "Automatic update checks disabled for this project. The explicit check remains available.",
                );
            }
            "setup-database" => {
                let desc = self.description(&project)?.clone();
                self.configure_database(&project, &desc, true)?;
                let native = project.data_sources().is_ok();
                if native {
                    database::connect(&project)?;
                    self.connected_databases.insert(project.root()?);
                }
                let message = if native {
                    "Data source configured with a secure credential. Database connection and introspection requested.".into()
                } else {
                    format!(
                        "Connection saved without the password; enter it in Database. {}",
                        ui_kit::denied::message("Saving the password and connecting automatically", "database.sources", "DDEV Integration")
                    )
                };
                host::notify(NotificationLevel::Info, "DDEV database", &message);
            }
            "copy-db-password" => {
                let db = database::connection(self.description(&project)?)?;
                let password = db
                    .get("password")
                    .and_then(serde_json::Value::as_str)
                    .ok_or("DDEV reported no database password.")?;
                host::copy_to_clipboard(password)?;
                host::notify(
                    NotificationLevel::Info,
                    "DDEV database",
                    "Database password copied.",
                );
            }
            "composer" | "php" | "node" => {
                if !runtime::allowed(self.description(&project)?, id) {
                    return Err("Start the DDEV project before running container tools.".into());
                }
                let Some(input) = host::input(
                    &format!("ddev {id} arguments (quoted arguments are supported)"),
                    None,
                )?
                else {
                    return Ok(None);
                };
                let args = shell_words::split(&input).map_err(|e| Error::from(e.to_string()))?;
                let mut command = if id == "composer" {
                    vec!["composer".into()]
                } else {
                    // DDEV selects RawCmd only when the --raw flag is explicitly
                    // set; otherwise its shell reconstruction can expand user
                    // dollars/backticks and drop empty arguments.
                    vec!["exec".into(), "--raw".into(), "--".into(), id.into()]
                };
                command.extend(args);
                self.terminal(&project, id, &command)?;
            }
            "open-config" => {
                project.read_file(CONFIG)?;
                host::open_location(
                    &format!("{}/{CONFIG}", project.root()?.trim_end_matches('/')),
                    0,
                    0,
                )?;
            }
            "open-service" => self.open_service(&project, arg)?,
            "logs" => {
                let key = self.target(&project, arg)?;
                let log = self.log(&project, &key)?;
                let name = self.description(&project)?.name.clone();
                // Native log editor identity is per plugin/window/key, without
                // project identity. Keep streams separate after project switches.
                let tab_key = format!(
                    "{}:logs:{}",
                    database::source_id(&project)?,
                    support::fingerprint(&key)
                );
                host::open_log_tab(&tab_key, &format!("DDEV {name} {key} logs"), &log)?;
            }
            "check-updates" => check_updates(&project, true)?,
            "start" | "restart" | "stop" | "poweroff" | "delete" | "share" | "config" | "shell" => {
                if id != "config" {
                    project.read_file(CONFIG)?;
                }
                if matches!(id, "delete" | "poweroff") && !self.cli_is_available(&project) {
                    return Err("DDEV is unavailable. Check its installation and execution permission, then retry.".into());
                }
                if id == "start" && !self.can_start(&project) {
                    return Err("DDEV: Start requires an installed DDEV and a configured project that is not running or starting. Refresh status after external changes.".into());
                }
                if matches!(id, "restart" | "stop") {
                    let allowed = match self.description(&project) {
                        Ok(desc) => runtime::allowed(desc, id),
                        Err(_) => self.cli_is_available(&project),
                    };
                    if !allowed {
                        return Err(format!("DDEV: {} needs a running project. Start it first, or Refresh Status if it was started outside the IDE.", command_name(id)).into());
                    }
                }
                if matches!(id, "share" | "shell")
                    && !runtime::allowed(self.description(&project)?, id)
                {
                    return Err(format!("DDEV: {} can't run while the project is {}. Start it first, or Refresh Status if it was started outside the IDE.", command_name(id), self.description(&project)?.status).into());
                }
                let confirmation = match id {
                    "delete" => {
                        let name = self.description(&project).ok().map(|desc| desc.name.clone());
                        Some(("Delete DDEV project", delete_question(name.as_deref(), &project.root()?)))
                    }
                    "poweroff" => Some((
                        "Power off DDEV",
                        "Stop all DDEV projects and the shared router on this machine?".into(),
                    )),
                    "share" => Some((
                        "Share DDEV project",
                        format!(
                            "Share {} on a public URL? Anyone with the link can reach the site until you stop sharing.",
                            self.description(&project).map_or_else(|_| "this project".to_owned(), |d| d.name.clone())
                        ),
                    )),
                    "config" => Some((
                        "Configure DDEV project",
                        "Run ddev config --auto in this project? It can create or update .ddev/config.yaml.".into(),
                    )),
                    _ => None,
                };
                if let Some((title, body)) = confirmation
                    && !host::confirm(title, &body)?
                {
                    return Ok(None);
                }
                let args = match id {
                    "config" => {
                        let mut args = vec!["config".into(), "--auto".into()];
                        if let Some(version) = config_php_version(&project)? {
                            args.extend(["--php-version".into(), version]);
                        }
                        args
                    }
                    "shell" => {
                        let key = self.target(&project, arg)?;
                        let service = if key == PROJECT {
                            "web".to_owned()
                        } else {
                            let s = self
                                .description(&project)?
                                .service(&key)
                                .ok_or("Unknown DDEV service.")?;
                            if !s.logs {
                                return Err("This link has no separate container shell.".into());
                            }
                            s.name.clone()
                        };
                        vec!["ssh".into(), format!("--service={service}")]
                    }
                    _ => vec![id.into()],
                };
                let followup = if id == "config" {
                    Some(support::PendingConfig::prepare(&project, &args, self)?)
                } else {
                    None
                };
                let operation = self.terminal(&project, id, &args)?;
                if let Some(pending) = followup {
                    pending.remember(self, &project, operation);
                }
                self.invalidate();
            }
            _ => return Err(Error::NotFound(format!("command {id}"))),
        }
        Ok(None)
    }
}

fn run_json(project: &Project, command: &str) -> Result<String, Error> {
    let output = run_ddev(project, &[command.into(), "--json-output".into()])?;
    if output.status != Some(0) {
        // JSON-enabled DDEV writes structured error records, too.
        let error = model::error_message(&output.stderr)
            .or_else(|| model::error_message(&output.stdout))
            .unwrap_or_else(|| "DDEV returned no error message.".into());
        return Err(format!(
            "ddev {command} failed (exit {:?}): {}",
            output.status,
            error.trim()
        )
        .into());
    }
    Ok(output.stdout)
}

/// The command title after "DDEV: ", for errors that name the command.
fn command_name(id: &str) -> &str {
    match id {
        "restart" => "Restart Project",
        "stop" => "Stop Project",
        "share" => "Share Project",
        "shell" => "Open Shell",
        other => other,
    }
}

fn delete_question(name: Option<&str>, root: &str) -> String {
    let project = match name {
        Some(name) => format!("{name} ({root})"),
        None => root.to_owned(),
    };
    format!("Delete DDEV project {project}? DDEV will delete its containers and database.")
}

fn run_ddev(project: &Project, args: &[String]) -> Result<openidea_plugin::ProcessOutput, Error> {
    host::run_command(&execution::command(args), Some(project), None)
}

fn config_php_version(project: &Project) -> Result<Option<String>, Error> {
    let requested = support::read_php_preference(project);
    let requested = requested.trim();
    if !requested.is_empty() {
        return normalize_php_version(requested, false)
            .map(Some)
            .ok_or_else(|| "PHP version must be major.minor, for example 8.3.".into());
    }
    // The SDK exposes plugin settings only. Respect an explicit project PHP
    // language level before composer.json's platform version, without reading
    // or modifying global IDE settings outside the project.
    if let Ok(settings) = project.read_file(".openidea/settings.toml")
        && let Ok(doc) = settings.parse::<toml_edit::DocumentMut>()
        && let Some(version) = doc
            .get("languages")
            .and_then(|l| l.get("php"))
            .and_then(|p| p.get("languageLevel"))
            .and_then(|v| v.as_str())
        && let Some(version) = normalize_php_version(version, false)
    {
        return Ok(Some(version));
    }
    if let Ok(composer) = project.read_file("composer.json")
        && let Ok(doc) = serde_json::from_str::<serde_json::Value>(&composer)
        && let Some(version) = doc.pointer("/config/platform/php").and_then(|v| v.as_str())
    {
        return Ok(normalize_php_version(version, true));
    }
    Ok(None)
}

fn normalize_php_version(version: &str, patch: bool) -> Option<String> {
    let parts: Vec<_> = version.trim().split('.').collect();
    if !(parts.len() == 2 || patch && parts.len() == 3)
        || parts
            .iter()
            .any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    Some(format!("{}.{}", parts[0], parts[1]))
}

fn check_updates(project: &Project, explicit: bool) -> Result<(), Error> {
    let _task = host::begin_task("Checking DDEV version", false);
    // --version works without Docker; `version --json-output` can exit nonzero
    // after printing a valid version if the Docker daemon is stopped.
    let output = run_ddev(project, &["--version".into()])?;
    if output.status != Some(0) {
        return Err(format!(
            "ddev --version failed: {}",
            model::error_message(&output.stderr)
                .unwrap_or_else(|| "DDEV returned no error message.".into())
        )
        .into());
    }
    let installed = installed_version(&output.stdout)?;
    let mut request = HttpRequest::get("https://api.github.com/repos/ddev/ddev/releases/latest")
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "openidea-ddev");
    request.timeout_ms = Some(10_000);
    let response = host::fetch(&request)?;
    if response.status != 200 {
        return Err(format!("GitHub release check failed: HTTP {}", response.status).into());
    }
    let release: serde_json::Value =
        serde_json::from_slice(&response.body).map_err(|e| Error::from(e.to_string()))?;
    let latest = text(&release, "tag_name");
    let newer = version::Version::parse(latest)?.newer_than(&installed);
    let body = if newer {
        format!(
            "DDEV {latest} is available (installed: v{installed}). See https://github.com/ddev/ddev/releases/latest"
        )
    } else {
        format!("DDEV v{installed} is up to date with the latest stable release ({latest}).")
    };
    let body = format!("{body}\n\nProject: {}", project.root()?);
    if newer {
        let mut actions = vec![
            openidea_plugin::NotificationAction::new("Release notes", "open-releases"),
            openidea_plugin::NotificationAction::new("Installation guide", "open-guide"),
        ];
        actions.push(
            openidea_plugin::NotificationAction::new(
                "Disable automatic checks",
                "disable-update-checks",
            )
            .argument(format!("project:{}", project.root()?)),
        );
        host::notify_with_actions(NotificationLevel::Info, "DDEV updates", &body, &actions)?;
    } else if explicit {
        host::notify(NotificationLevel::Info, "DDEV updates", &body);
    }
    Ok(())
}

fn installed_version(stdout: &str) -> Result<version::Version, Error> {
    for line in stdout.lines() {
        let record = serde_json::from_str::<serde_json::Value>(line).ok();
        let message = record.as_ref().map_or(line, |v| text(v, "msg")).trim();
        if let Some(version) = message.strip_prefix("ddev version ") {
            return version::Version::parse(version);
        }
    }
    Err("DDEV returned no recognizable version.".into())
}

impl openidea_plugin::RunTargetValidation for Main {
    fn validate(
        &mut self,
        project: &str,
        _target_id: &str,
        _launch: &openidea_plugin::RunTargetLaunch,
    ) -> Result<(), String> {
        let Some(project) = host::projects()
            .unwrap_or_default()
            .into_iter()
            .find(|p| p.root().is_ok_and(|root| root == project))
        else {
            return Ok(());
        };
        let desc = match self.fresh_description(&project) {
            Some(desc) => desc,
            None => {
                let Ok(desc) =
                    run_json(&project, "describe").and_then(|o| Description::parse(&o))
                else {
                    return Ok(());
                };
                desc
            }
        };
        if desc.status == "running" {
            return Ok(());
        }
        Err(format!(
            "DDEV project \"{}\" is {}. Run DDEV: Start first.",
            desc.name, desc.status
        ))
    }
}

register_plugin!(Main, views, details, providers, diagnostics, hover, run_target_validation);

#[cfg(test)]
mod tests;

#[cfg(test)]
mod live_tests;
