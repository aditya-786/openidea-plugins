//! Dev Container CLI: drives the project's development container with the
//! `devcontainer` CLI (https://containers.dev).

mod checks;
mod configs;
mod compose;
mod compose_semantics;
mod feature_checks;
mod create;
mod dashboard;
mod dockerfile;
mod features;
mod feature_options;
mod launch;
mod operations;
mod run_target;

use std::path::Path;

use openidea_plugin::providers::{CompletionItem, CompletionKind, Document, Providers};
use openidea_plugin::text::{offset, position};
use openidea_plugin::{Diagnostic, Error, Invocation, NotificationLevel, Plugin, Position, Project, Range, host, register_plugin};

use checks::{Context, FEATURES_MALFORMED, UNKNOWN_PROPERTY};

const MAX_BYTES: usize = 256 * 1024;

const LOGIN_SHELL: &str = "command -v bash >/dev/null && exec bash -l; exec sh -l";
/// Stopping a container also unregisters the project's run targets.
const RUN_TARGET_REMOVED: &str = "Any Dev Container run target for this project was removed.";

#[derive(Default)]
pub struct Main {
    run_target: run_target::State,
    /// Container id picked in the Dev Containers view.
    selected: Option<String>,
    selection_project: Option<String>,
    running: std::collections::BTreeSet<String>,
    operations: operations::Operations,
}

impl Plugin for Main {
    fn activate(&mut self, _ctx: &openidea_plugin::Context) -> Result<(), Error> {
        self.run_target.clear();
        Ok(())
    }
    fn run_command(&mut self, id: &str, ctx: &Invocation, arg: Option<&str>) -> Result<Option<String>, Error> {
        self.command(id, ctx, arg).map_err(explain_denial)
    }
}

/// Process denials become the standard "allow this permission" message.
pub(crate) fn explain_denial(error: Error) -> Error {
    let Error::Denied(capability) = &error else { return error };
    let feature = match capability.as_str() {
        "process:devcontainer" => "Running the devcontainer CLI",
        "process:docker" => "Listing, starting and stopping containers",
        "process:git" => "Cloning a repository",
        _ => return error,
    };
    Error::Invalid(ui_kit::denied::message(feature, capability, "Dev Container CLI"))
}

impl Main {
    fn command(&mut self, id: &str, ctx: &Invocation, arg: Option<&str>) -> Result<Option<String>, Error> {
        if id == "managed-output" {
            host::reveal_view(operations::VIEW)?;
            return Ok(None);
        }
        if id == "start-cloned" {
            return start_cloned(arg);
        }
        let project = ctx.project().ok_or("No project is open.")?;
        let root = project.root()?;

        if id == "register-run-target" { return run_target::prepare(&mut self.run_target, ctx); }
        if id == "register-mapped-tooling-target" { return run_target::prepare_mapped(&mut self.run_target, ctx); }
        if matches!(id, "unregister-run-target" | "unregister-mapped-tooling-target") {
            self.run_target.clear_for(&root);
            host::unregister_run_target(&project, if id == "unregister-run-target" { run_target::ID } else { run_target::MAPPED_ID });
            return Ok(None);
        }

        if matches!(id, "up-managed" | "rebuild-managed") {
            self.operations.idle()?;
            let Some(config) = configs::choose(&project, false)? else { return Ok(None); };
            let plan = launch::Plan::prepare(&project, if id == "up-managed" { "up" } else { "rebuild" }, &config)?;
            if !ui_kit::prompts::confirm_exact(plan.title(), &format!("Runs the Dev Container CLI in the background. Refresh Status reads its progress and Cancel stops it.\nCancelling does not undo a container or build that already started.\nOutput keeps the latest 64 KiB.\n\nDetails\n{}\n\nThe CLI client closes after about 300 seconds idle or when the plugin stops.", plan.preview()))? {
                return Ok(None);
            }
            plan.launch_managed(ctx, &mut self.operations)?;
            self.run_target.clear_for(&root);
            host::reveal_view(operations::VIEW)?;
            return Ok(None);
        }

        if id == "choose-configuration" {
            if let Some(path) = configs::choose(&project, true)? {
                self.run_target.clear_for(&root);
                host::unregister_run_target(&project, run_target::ID);
                host::unregister_run_target(&project, run_target::MAPPED_ID);
                host::notify(NotificationLevel::Info, "Dev Container configuration", &format!("Using {path} for this project."));
                return Ok(None);
            }
            return Ok(None);
        }

        if id == "stop" {
            return stop(&mut self.run_target, &project, &root);
        }

        if id == "info" {
            return info(&project, &root);
        }

        if id == "refresh-features" {
            return features::refresh();
        }

        if id == "clone" {
            return clone(&root);
        }

        if !matches!(id, "up" | "rebuild" | "shell") {
            return Err(Error::NotFound(format!("command {id}")));
        }
        // Open Shell on the Started notification names the container it started.
        if id == "shell" && let Some(container) = arg {
            self.shell_into(ctx, container)?;
            return Ok(None);
        }
        let Some(config) = configs::choose(&project, false)? else {
            return Ok(None);
        };
        if matches!(id, "up" | "rebuild") {
            let plan = launch::Plan::prepare(&project, id, &config)?;
            if !ui_kit::prompts::confirm_exact(plan.title(), &plan.preview())? {
                return Ok(None);
            }
            self.run_target.clear_for(&root);
            plan.launch(ctx)?;
        } else {
            let command = host::Command::new("devcontainer").args(["exec", "--workspace-folder", &root, "--config", &config, "sh", "-c", LOGIN_SHELL]);
            host::open_terminal_session(&shell_key(&root, &config), "Dev Container: Open Shell in Container", &command)?;
        }

        Ok(None)
    }
}

/// One shell terminal per project folder and configuration (at most 128 bytes).
fn shell_key(root: &str, config: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (root, config).hash(&mut hasher);
    format!("shell-{:016x}", hasher.finish())
}

fn is_devcontainer_file(path: &str) -> bool {
    let path = path.replace('\\', "/");

    matches!(path.rsplit('/').next(), Some("devcontainer.json" | ".devcontainer.json"))
}

impl openidea_plugin::diagnostics::DiagnosticsProvider for Main {
    fn diagnostics(&mut self, doc: &openidea_plugin::providers::Document) -> Vec<Diagnostic> {
        if !is_devcontainer_file(&doc.path) {
            return Vec::new();
        }
        let path = doc.path.as_str();

        let text = doc.text.as_str();
        let findings = match text.len() > MAX_BYTES {
            true => vec![],
            false => enabled_findings(path, text),
        };
        let diagnostics: Vec<Diagnostic> = findings
            .iter()
            .map(|f| Diagnostic {
                code: Some(f.code.into()),
                ..Diagnostic::new(Range::new(position(text, f.start), position(text, f.end)), f.severity, &f.message)
            })
            .collect();

        diagnostics
    }
}

fn enabled_findings(path: &str, text: &str) -> Vec<checks::Finding> {
    let properties = host::setting_bool("checkProperties").unwrap_or(true);
    let features = host::setting_bool("checkFeatures").unwrap_or(true);

    if !checks::in_valid_location(path) {
        let hint = checks::folder_structure(path).filter(|_| host::setting_bool("checkFolderStructure").unwrap_or(true));

        let token = checks::first_token(text);

        return hint.map(|hint| checks::Finding { start: token.start, end: token.end, ..hint }).into_iter().collect();
    }

    let mut findings = checks::check(text);
    findings.extend(compose::reference_findings(path, text));
    findings.extend(feature_checks::local_feature_findings(path, text));

    findings
        .into_iter()
        .filter(|f| match f.code {
            UNKNOWN_PROPERTY => properties,
            checks::INVALID_PROPERTY_VALUE | checks::COMPOSE_REFERENCE => host::setting_bool("checkPropertyValues").unwrap_or(true),
            FEATURES_MALFORMED => features,
            _ => true,
        })
        .collect()
}

fn info(project: &Project, root: &str) -> Result<Option<String>, Error> {
    let Some(config) = configs::choose(project, false)? else {
        return Ok(None);
    };
    let args = vec!["read-configuration".to_owned(), "--workspace-folder".to_owned(), root.to_owned(), "--config".to_owned(), config];
    let _task = host::begin_task("Reading the Dev Container configuration", false);
    let output = project.run_process("devcontainer", &args, None)?;

    if output.status != Some(0) {
        return Err(format!("devcontainer read-configuration failed: {}", output.stderr.trim()).into());
    }

    let summary = summarise(&output.stdout).ok_or("devcontainer read-configuration printed no JSON configuration.")?;
    host::notify(NotificationLevel::Info, "Dev Container", &summary);

    Ok(None)
}

/// The CLI may print log noise before its JSON, so the first parseable object wins.
fn summarise(stdout: &str) -> Option<String> {
    let value = stdout
        .match_indices('{')
        .find_map(|(start, _)| serde_json::Deserializer::from_str(&stdout[start..]).into_iter::<serde_json::Value>().next()?.ok())?;
    let config = value.get("configuration")?;
    let mut lines = vec![];

    if let Some(image) = config.get("image").and_then(|v| v.as_str()) {
        lines.push(format!("Image: {image}"));
    }

    let compose = match config.get("dockerComposeFile") {
        Some(serde_json::Value::String(file)) => Some(file.clone()),
        Some(serde_json::Value::Array(files)) => Some(files.iter().filter_map(|f| f.as_str()).collect::<Vec<_>>().join(", ")),
        _ => None,
    };
    if let Some(files) = compose {
        lines.push(format!("Compose file: {files}"));
    }

    if let Some(user) = config.get("remoteUser").and_then(|v| v.as_str()) {
        lines.push(format!("Remote user: {user}"));
    }

    let ports: Vec<String> = config
        .get("forwardPorts")
        .and_then(|v| v.as_array())
        .map(|ports| ports.iter().map(|p| p.as_str().map(str::to_owned).unwrap_or_else(|| p.to_string())).collect())
        .unwrap_or_default();
    lines.push(match ports.is_empty() {
        true => "Forwarded ports: none".to_owned(),
        false => format!("Forwarded ports: {}", ports.join(", ")),
    });

    let features = config.get("features").and_then(|v| v.as_object()).map_or(0, |f| f.len());
    lines.push(format!("Features: {features}"));

    let has_post_create = config.get("postCreateCommand").is_some_and(|v| !v.is_null() && v.as_str() != Some(""));
    lines.push(format!("postCreateCommand: {}", if has_post_create { "yes" } else { "no" }));

    Some(lines.join("\n"))
}

fn repository_name(url: &str) -> &str {
    let name = url.trim_end_matches('/').rsplit(['/', ':']).next().unwrap_or("");

    name.strip_suffix(".git").unwrap_or(name)
}

fn clone(root: &str) -> Result<Option<String>, Error> {
    let Some(url) = host::input("Repository URL to clone", None)? else {
        return Ok(None);
    };
    let url = url.trim();
    let name = repository_name(url);
    if url.starts_with('-') || url.contains(char::is_whitespace) || name.is_empty() {
        host::notify(NotificationLevel::Warning, "Dev Container", &format!("\"{url}\" is not a repository URL."));
        return Ok(None);
    }

    let parent = Path::new(root).parent().map_or(root.to_owned(), |p| p.to_string_lossy().into_owned());
    let suggested = format!("{}/{name}", parent.trim_end_matches('/'));
    let Some(target) = host::input("Clone into (a new folder)", Some(&suggested))? else {
        return Ok(None);
    };
    let target = target.trim();
    if target.is_empty() {
        return Ok(None);
    }

    let args = ["clone".to_owned(), "--".to_owned(), url.to_owned(), target.to_owned()];
    host::open_terminal("Dev Container: Clone Repository", "git", &args)?;
    let start = openidea_plugin::NotificationAction::new("Start Container", "start-cloned").argument(target);
    host::notify_with_actions(NotificationLevel::Info, "Dev Container", "Cloning in a terminal. When it finishes, start the container for the new folder.", &[start])?;

    Ok(None)
}

fn start_cloned(target: Option<&str>) -> Result<Option<String>, Error> {
    let picked;
    let target = match target.map(str::trim).filter(|t| !t.is_empty()) {
        Some(target) => target,
        None => {
            let Some(folder) = host::pick_directory("Choose the cloned folder to start", None)? else {
                return Ok(None);
            };
            picked = folder;
            picked.as_str()
        }
    };
    if target.starts_with('-') {
        return Err("Choose a cloned folder to start.".into());
    }
    let args = ["up".to_owned(), "--workspace-folder".to_owned(), target.to_owned()];
    let body = format!("Run devcontainer with arguments:\n{}", serde_json::to_string_pretty(&args).unwrap_or_default());
    if !ui_kit::prompts::confirm_exact("Start cloned Dev Container", &body)? {
        return Ok(None);
    }
    host::open_terminal("Dev Container: Start Cloned Container", "devcontainer", &args)?;

    Ok(None)
}

/// The CLI has no stop command; its containers carry the workspace folder as a label.
fn stop(run_target: &mut run_target::State, project: &Project, root: &str) -> Result<Option<String>, Error> {
    let running: Vec<dashboard::Container> = dashboard::list(project, root)?.into_iter().filter(|c| c.running).collect();

    if running.is_empty() {
        host::notify(NotificationLevel::Info, "Dev Container", "No running container for this project.");
        return Ok(None);
    }

    let names = running.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join(", ");
    let body = format!("Runs docker stop on {names}. Unsaved work in a container shell is lost.\nAny Dev Container run target for this project is removed too.");
    if !host::confirm(&format!("Stop {names}?"), &body)? {
        return Ok(None);
    }

    let ids: Vec<String> = running.into_iter().map(|c| c.id).collect();
    let stopped = project.run_process("docker", &[vec!["stop".to_owned()], ids].concat(), None)?;

    if stopped.status != Some(0) {
        return Err(format!("docker stop failed: {}", stopped.stderr.trim()).into());
    }

    run_target.clear_for(root);
    host::unregister_run_target(project, run_target::ID);
    host::unregister_run_target(project, run_target::MAPPED_ID);
    host::notify(NotificationLevel::Info, "Dev Container", &format!("Stopped {names}. {RUN_TARGET_REMOVED}"));

    Ok(None)
}

impl Providers for Main {
    fn line_markers(&mut self, doc: &Document) -> Vec<openidea_plugin::providers::LineMarker> {
        if !is_devcontainer_file(&doc.path) || doc.text.len() > MAX_BYTES {
            return vec![];
        }
        compose::file_markers(&doc.path, &doc.text)
    }

    fn completion(&mut self, doc: &Document, at: Position, _trigger: Option<&str>) -> Vec<CompletionItem> {
        if !is_devcontainer_file(&doc.path) || doc.text.len() > MAX_BYTES || !host::setting_bool("completeProperties").unwrap_or(true) {
            return vec![];
        }

        let Some(offset) = offset(&doc.text, at) else {
            return vec![];
        };
        if let Some(key) = checks::feature_key_at(&doc.text, offset) {
            return feature_items(&doc.text, key);
        }
        if let Some((reference, context, present)) = checks::feature_option_context(&doc.text, offset) {
            let options = feature_options::cached(&reference);
            let span = checks::word_range(&doc.text, offset);
            let replace = Range::new(position(&doc.text, span.start), position(&doc.text, span.end));
            return match context {
                Context::Key => options.into_iter().filter(|(key,_)| !present.contains(key)).map(|(key, option)| {
                    let has_value = doc.text[span.end..].trim_start().starts_with(':');
                    let mut item = CompletionItem::new(&key, CompletionKind::Property);
                    item.insert_text = if has_value {format!("\"{key}\"")} else {format!("\"{key}\": $0")};
                    item.snippet = !has_value;
                    item.replace = Some(replace);
                    item.detail = Some(format!("Cached feature option (unverified): {}", option["description"].as_str().unwrap_or("")));
                    item
                }).collect(),
                Context::Value { key } => options.get(&key).into_iter().flat_map(|option| {
                    if option["type"] == "boolean" {vec![serde_json::Value::Bool(true),serde_json::Value::Bool(false)]}
                    else {option.get("enum").or_else(||option.get("proposals")).and_then(serde_json::Value::as_array).cloned().unwrap_or_default()}
                }).filter_map(|value| {
                    let insert = serde_json::to_string(&value).ok()?;
                    let label = value.as_str().map(str::to_owned).unwrap_or_else(||insert.clone());
                    let mut item = CompletionItem::new(&label, CompletionKind::EnumMember);
                    item.insert_text = insert; item.replace = Some(replace);
                    item.detail = Some("Cached feature option (unverified)".into());
                    Some(item)
                }).collect(),
            };
        }
        if let Some((keys, choices)) = checks::nested_choices(&doc.text, offset) {
            let span = checks::word_range(&doc.text, offset);
            let replace = Range::new(position(&doc.text, span.start), position(&doc.text, span.end));
            return choices.into_iter().map(|(value, quoted)| {
                let mut item = CompletionItem::new(value, if keys { CompletionKind::Property } else { CompletionKind::EnumMember });
                item.insert_text = if keys && doc.text[span.end..].trim_start().starts_with(':') { format!("\"{value}\"") } else if keys { format!("\"{value}\": $0") } else if quoted { format!("\"{value}\"") } else { value.to_owned() };
                item.snippet = keys && !doc.text[span.end..].trim_start().starts_with(':');
                item.replace = Some(replace);
                item
            }).collect();
        }
        let Some(context) = checks::context_at(&doc.text, offset) else {
            return vec![];
        };
        let replace = checks::word_range(&doc.text, offset);
        let replace = Range::new(position(&doc.text, replace.start), position(&doc.text, replace.end));

        let mut items = match context {
            Context::Key => checks::missing_properties(&doc.text, offset)
                .into_iter()
                .map(|name| {
                    let mut item = CompletionItem::new(name, CompletionKind::Property);
                    item.insert_text = format!("\"{name}\": $0");
                    item.snippet = true;
                    item
                })
                .collect(),
            Context::Value { key } => checks::value_choices(&key)
                .iter()
                .map(|(value, quoted)| {
                    let mut item = CompletionItem::new(value, CompletionKind::EnumMember);
                    if *quoted {
                        item.insert_text = format!("\"{value}\"");
                    }
                    item
                })
                .collect::<Vec<_>>(),
        };

        for item in &mut items {
            item.replace = Some(replace);
        }

        items
    }
}

fn feature_items(text: &str, key: std::ops::Range<usize>) -> Vec<CompletionItem> {
    let has_value = text[key.end..].trim_start().starts_with(':');
    let replace = Range::new(position(text, key.start), position(text, key.end));

    features::cached()
        .into_iter()
        .map(|(reference, name)| {
            let mut item = CompletionItem::new(&reference, CompletionKind::Module);
            item.detail = Some(name).filter(|n| !n.is_empty());
            item.insert_text = if has_value { format!("\"{reference}\"") } else { format!("\"{reference}\": {{$0}}") };
            item.snippet = !has_value;
            item.filter_text = Some(format!("\"{reference}\""));
            item.replace = Some(replace);
            item
        })
        .collect()
}

impl openidea_plugin::RunTargetValidation for Main {
    fn validate(&mut self, project: &str, target_id: &str, _launch: &openidea_plugin::RunTargetLaunch) -> Result<(), String> {
        run_target::validate_running(project, target_id)
    }
}

register_plugin!(Main, providers, views, diagnostics, details, file_actions, run_target_validation);

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::Event;
    use openidea_plugin::ProcessOutput;
    use openidea_plugin::mock::MockHost;
    use openidea_plugin::views::{Body, ViewEvent, Views};

    fn host() -> MockHost {
        let host = MockHost::install().with_capabilities(&["project.read", "process:devcontainer", "process:docker"]);
        host.set_project_root(Some("/work/app"));
        host
    }

    fn ok(stdout: &str) -> Result<ProcessOutput, Error> {
        Ok(ProcessOutput { status: Some(0), stdout: stdout.into(), stderr: String::new() })
    }

    #[test]
    fn cancelling_configuration_choice_starts_no_terminal_or_process() {
        let host = host();
        host.add_file(".devcontainer/a/devcontainer.json", "{}");
        host.add_file(".devcontainer/b/devcontainer.json", "{}");
        host.answer_next_prompt(None);
        Main::default().run_command("up", &host.invocation(), None).unwrap();
        assert!(host.terminals().is_empty() && host.process_runs().is_empty());
    }

    #[test]
    fn cancelling_start_preview_preserves_remembered_configuration_without_launch() {
        let host = host().with_capabilities(&["project.read", "process:devcontainer", "storage"]);
        host.add_file(".devcontainer/a/devcontainer.json", "{}");
        host.add_file(".devcontainer/b/devcontainer.json", "{}");
        host.answer_next_prompt(Some(&[".devcontainer/b/devcontainer.json"]));
        host.answer_next_prompt(None);
        Main::default().run_command("up", &host.invocation(), None).unwrap();
        assert!(host.terminals().is_empty() && host.process_runs().is_empty());
        host.answer_next_prompt(Some(&[]));
        Main::default().run_command("up", &host.invocation(), None).unwrap();
        assert_eq!(host.terminals()[0].args[4], ".devcontainer/b/devcontainer.json");
    }

    #[test]
    fn chooser_command_persists_for_later_cli_commands_and_new_instances() {
        let host = host().with_capabilities(&["project.read", "process:devcontainer", "storage"]);
        host.add_file(".devcontainer/a/devcontainer.json", "{}");
        host.add_file(".devcontainer/b/devcontainer.json", "{}");
        host.answer_next_prompt(Some(&[".devcontainer/b/devcontainer.json"]));
        Main::default().run_command("choose-configuration", &host.invocation(), None).unwrap();
        assert!(host.terminals().is_empty() && host.process_runs().is_empty());
        assert!(host.notifications().iter().any(|n| n.body.contains(".devcontainer/b/devcontainer.json")));
        host.answer_next_prompt(Some(&[]));
        Main::default().run_command("up", &host.invocation(), None).unwrap();
        assert_eq!(host.terminals()[0].args[4], ".devcontainer/b/devcontainer.json");
    }

    #[test]
    fn up_uses_the_default_config() {
        let host = host();
        host.add_file(".devcontainer/devcontainer.json", "{}");
        host.answer_next_prompt(Some(&[]));

        Main::default().run_command("up", &host.invocation(), None).unwrap();

        let terminal = &host.terminals()[0];
        assert_eq!(terminal.title, "Dev Container: Start Container");
        assert_eq!(terminal.program, "devcontainer");
        assert_eq!(terminal.args, ["up", "--workspace-folder", "/work/app", "--config", ".devcontainer/devcontainer.json"]);
    }

    #[test]
    fn rebuild_falls_back_to_the_root_config() {
        let host = host();
        host.add_file(".devcontainer.json", "{}");
        host.answer_next_prompt(Some(&[""]));

        Main::default().run_command("rebuild", &host.invocation(), None).unwrap();

        assert_eq!(
            host.terminals()[0].args,
            ["up", "--workspace-folder", "/work/app", "--config", ".devcontainer.json", "--remove-existing-container"]
        );
    }

    #[test]
    fn declining_the_rebuild_confirm_starts_nothing() {
        let host = host();
        host.add_file(".devcontainer.json", "{}");
        host.answer_next_prompt(None);

        Main::default().run_command("rebuild", &host.invocation(), None).unwrap();

        assert!(host.terminals().is_empty());
    }

    #[test]
    fn several_default_configs_let_the_user_pick() {
        let host = host();
        host.add_file(".devcontainer/devcontainer.json", "{}");
        host.add_file(".devcontainer.json", "{}");
        host.answer_next_prompt(Some(&[".devcontainer.json"]));
        host.answer_next_prompt(Some(&[]));

        Main::default().run_command("up", &host.invocation(), None).unwrap();

        assert_eq!(host.terminals()[0].args[4], ".devcontainer.json");
    }

    #[test]
    fn shell_uses_the_configured_file() {
        let host = host();
        host.set_setting("configFile", "\".devcontainer/python/devcontainer.json\"");
        host.add_file(".devcontainer/python/devcontainer.json", "{}");

        Main::default().run_command("shell", &host.invocation(), None).unwrap();
        Main::default().run_command("shell", &host.invocation(), None).unwrap();

        assert!(host.terminals().is_empty());
        let key = shell_key("/work/app", ".devcontainer/python/devcontainer.json");
        assert!(host.finish_terminal(&key).is_some());
        assert!(host.finish_terminal(&key).is_none(), "the second Open Shell focused the first terminal");
    }

    #[test]
    fn up_without_config_fails() {
        let host = host();

        let error = Main::default().run_command("up", &host.invocation(), None).unwrap_err();

        assert!(error.to_string().starts_with("No .devcontainer/devcontainer.json"));
        assert!(host.terminals().is_empty());
    }

    fn stop_host() -> MockHost {
        let host = host();
        host.on_process("docker", |args, _| match args[0].as_str() {
            "ps" => {
                assert_eq!(args[3], "label=devcontainer.local_folder=/work/app");
                ok("{\"ID\":\"abc\",\"Names\":\"web\",\"State\":\"running\"}\n{\"ID\":\"def\",\"Names\":\"db\",\"State\":\"running\"}\n{\"ID\":\"0ff\",\"Names\":\"old\",\"State\":\"exited\"}\n")
            }
            "stop" => {
                assert_eq!(args, ["stop", "abc", "def"]);
                ok("")
            }
            other => panic!("unexpected docker {other}"),
        });
        host
    }

    #[test]
    fn stop_names_the_running_containers_and_stops_them_after_confirming() {
        let host = stop_host();
        host.answer_next_prompt(Some(&[]));

        Main::default().run_command("stop", &host.invocation(), None).unwrap();

        assert!(host.process_runs().iter().any(|r| r.args[0] == "stop"));
        assert_eq!(host.notifications()[0].body, format!("Stopped web, db. {RUN_TARGET_REMOVED}"));
    }

    #[test]
    fn declining_stop_stops_nothing_and_keeps_run_targets() {
        let host = stop_host();
        let project = host.invocation().project().unwrap();
        let target = openidea_plugin::PluginRunTarget { id: run_target::ID.into(), name: "t".into(), program: "docker".into(), args: vec![], workdir_args: vec![], container_root: None, commands: vec![] };
        host::register_run_target(&project, &target).unwrap();

        Main::default().run_command("stop", &host.invocation(), None).unwrap();

        assert!(host.process_runs().iter().all(|r| r.args[0] == "ps"));
        assert_eq!(host.run_targets().len(), 1);
    }

    #[test]
    fn stop_without_running_container_keeps_run_targets() {
        let host = host();
        host.on_process("docker", |_, _| ok(""));
        let project = host.invocation().project().unwrap();
        let target = openidea_plugin::PluginRunTarget { id: run_target::ID.into(), name: "t".into(), program: "docker".into(), args: vec![], workdir_args: vec![], container_root: None, commands: vec![] };
        host::register_run_target(&project, &target).unwrap();

        Main::default().run_command("stop", &host.invocation(), None).unwrap();

        assert_eq!(host.notifications()[0].body, "No running container for this project.");
        assert_eq!(host.run_targets().len(), 1);
    }

    #[test]
    fn clone_opens_terminal() {
        let host = host().with_capabilities(&["project.read", "process:git"]);

        host.answer_next_prompt(Some(&["https://example.com/acme/tool.git"]));
        host.answer_next_prompt(Some(&["/work/tool"]));
        Main::default().run_command("clone", &host.invocation(), None).unwrap();

        let terminals = host.terminals();
        assert_eq!(terminals[0].program, "git");
        assert_eq!(terminals[0].args, ["clone", "--", "https://example.com/acme/tool.git", "/work/tool"]);
        assert!(host.process_runs().is_empty());

        let action = &host.notifications_with_actions()[0].actions[0];
        assert_eq!((action.command.as_str(), action.argument.as_deref()), ("start-cloned", Some("/work/tool")));
    }

    #[test]
    fn start_cloned_confirms_then_opens_the_container_terminal() {
        let host = host().with_capabilities(&["project.read", "process:devcontainer"]);

        host.answer_next_prompt(Some(&["yes"]));
        Main::default().run_command("start-cloned", &host.invocation(), Some("/work/tool")).unwrap();
        assert_eq!(host.terminals()[0].program, "devcontainer");
        assert_eq!(host.terminals()[0].args, ["up", "--workspace-folder", "/work/tool"]);

        let before = host.terminals().len();
        assert!(Main::default().run_command("start-cloned", &host.invocation(), Some("--x")).is_err());
        assert_eq!(host.terminals().len(), before);
    }

    #[test]
    fn start_cloned_without_argument_asks_for_a_folder() {
        let host = host().with_capabilities(&["project.read", "process:devcontainer"]);

        host.answer_next_prompt(Some(&["/work/picked"]));
        host.answer_next_prompt(Some(&["yes"]));
        Main::default().run_command("start-cloned", &host.invocation(), None).unwrap();
        assert_eq!(host.terminals()[0].args, ["up", "--workspace-folder", "/work/picked"]);

        host.answer_next_prompt(None);
        let before = host.terminals().len();
        Main::default().run_command("start-cloned", &host.invocation(), None).unwrap();
        assert_eq!(host.terminals().len(), before);
    }

    #[test]
    fn clone_stops_on_dismissal_or_bad_url() {
        let host = host().with_capabilities(&["project.read", "process:git"]);

        host.answer_next_prompt(None);
        Main::default().run_command("clone", &host.invocation(), None).unwrap();
        host.answer_next_prompt(Some(&["--upload-pack=x"]));
        Main::default().run_command("clone", &host.invocation(), None).unwrap();
        host.answer_next_prompt(Some(&["https://example.com/acme/tool.git"]));
        host.answer_next_prompt(None);
        Main::default().run_command("clone", &host.invocation(), None).unwrap();

        assert!(host.process_runs().is_empty() && host.terminals().is_empty());
        assert_eq!(host.notifications().len(), 1);
    }

    #[test]
    fn stop_without_docker_is_denied() {
        let host = host().with_capabilities(&["project.read", "process:devcontainer"]);

        let error = Main::default().run_command("stop", &host.invocation(), None).unwrap_err();

        assert!(matches!(error, Error::Invalid(m) if m == "Listing, starting and stopping containers needs the process:docker permission. Allow it in Settings › Plugins › Dev Container CLI."));
    }

    fn strip_line_comments(text: &str) -> String {
        text.lines().filter(|line| !line.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n")
    }

    fn open(host: &MockHost, path: &str, text: &str) {
        host.add_file(path, text);
        Main::default().handle_events(&[Event::FileOpened(format!("/work/app/{path}"))], &host.invocation());
        host.open_editor(&format!("/work/app/{path}"), "jsonc", text, &[]);
        host.refresh_diagnostics(&mut Main::default());
    }

    fn published(host: &MockHost, path: &str) -> Vec<Diagnostic> {
        host.provider_diagnostics().get(&format!("/work/app/{path}")).cloned().unwrap_or_default()
    }

    fn codes(diagnostics: &[Diagnostic]) -> Vec<&str> {
        diagnostics.iter().map(|d| d.code.as_deref().unwrap()).collect()
    }

    const FILE: &str = ".devcontainer/devcontainer.json";

    #[test]
    fn unknown_top_level_property_warns() {
        let host = host();
        open(&host, FILE, "{\n  \"image\": \"x\",\n  \"postCreateCommnad\": \"ls\"\n}\n");

        let found = published(&host, FILE);

        assert_eq!(codes(&found), ["unknown-property"]);
        assert_eq!(found[0].range, Range::new(openidea_plugin::Position::new(2, 2), openidea_plugin::Position::new(2, 21)));
        assert!(found[0].message.contains("postCreateCommand"));
    }

    #[test]
    fn unknown_property_is_quiet_for_vendor_and_unrelated_names() {
        let host = host();
        open(
            &host,
            FILE,
            "{\"$schema\": \"s\", \"x-vendor\": 1, \"completelyDifferent\": 2, \"image\": \"i\", \"customizations\": {\"vscode\": {\"extensons\": []}}}",
        );

        assert!(published(&host, FILE).is_empty());
    }

    #[test]
    fn known_property_types_enum_values_and_nested_port_values_are_checked() {
        let host=host();
        open(&host,FILE,r#"{"image":3,"shutdownAction":"explode","portsAttributes":{"3000":{"protocol":"ftp","requireLocalPort":"yes"}}}"#);
        assert_eq!(codes(&published(&host,FILE)),["invalid-property-value";4]);
        host.set_setting("checkPropertyValues","false");
        open(&host,FILE,r#"{"image":3,"shutdownAction":"explode"}"#);
        assert!(published(&host,FILE).is_empty());
        host.set_setting("checkPropertyValues","true");
        open(&host,FILE,"{ /* JSONC */ \"image\":\"ubuntu\",\"shutdownAction\":\"none\",\"otherPortsAttributes\":{\"protocol\":\"http\",\"requireLocalPort\":true,},}");
        assert!(published(&host,FILE).is_empty());
    }

    #[test]
    fn host_requirements_follow_the_schema_types() {
        let host=host();
        open(&host,FILE,r#"{"hostRequirements":{"cpus":0,"memory":"8 gb","storage":"32gb","gpu":{"cores":1,"x":2}}}"#);
        assert_eq!(codes(&published(&host,FILE)),["invalid-property-value";3]);
        open(&host,FILE,r#"{"hostRequirements":{"cpus":4,"memory":"8gb","storage":"100","gpu":"optional","other":1}}"#);
        assert!(published(&host,FILE).is_empty());
        open(&host,FILE,r#"{"hostRequirements":{"gpu":{"cores":2,"memory":"4gb"}}}"#);
        assert!(published(&host,FILE).is_empty());
    }

    #[test]
    fn compose_file_and_service_references_are_checked_against_local_files() {
        let host=host();
        host.add_file(".devcontainer/docker-compose.yml","services:\n  app:\n    image: x\n");
        open(&host,FILE,r#"{"dockerComposeFile":"docker-compose.yml","service":"web"}"#);
        assert_eq!(codes(&published(&host,FILE)),["compose-reference"]);
        open(&host,FILE,r#"{"dockerComposeFile":["docker-compose.yml","../missing.yml"],"service":"app"}"#);
        assert_eq!(codes(&published(&host,FILE)),["compose-reference"]);
        open(&host,FILE,r#"{"dockerComposeFile":"docker-compose.yml","service":"app"}"#);
        assert!(published(&host,FILE).is_empty());
        open(&host,FILE,r#"{"dockerComposeFile":"${localEnv:F}.yml","service":"${localEnv:S}"}"#);
        assert!(published(&host,FILE).is_empty());
        open(&host,FILE,r#"{"dockerComposeFile":"../../outside.yml","service":"nope"}"#);
        assert!(published(&host,FILE).is_empty());
        host.set_setting("checkPropertyValues","false");
        open(&host,FILE,r#"{"dockerComposeFile":"nope.yml"}"#);
        assert!(published(&host,FILE).is_empty());
    }

    #[test]
    fn features_must_be_an_object() {
        let host = host();
        open(&host, FILE, "{\"features\": [\"a\"]}");

        assert_eq!(codes(&published(&host, FILE)), ["features-malformed"]);
    }

    #[test]
    fn feature_id_with_whitespace_warns() {
        let host = host();
        open(&host, FILE, "{\"features\": {\"ghcr.io/x/y node:1\": {}, \"\": {}, \"ok:1\": \"latest\", \"b\": true, \"c\": 3}}");

        assert_eq!(codes(&published(&host, FILE)), ["features-malformed"; 3]);
    }

    #[test]
    fn jsonc_comments_and_trailing_commas_are_read() {
        let host = host();
        open(&host, FILE, "{\n // \"fake\": 1,\n /* \"imagee\": */ \"features\": { \"a b\": {}, /* c */ },\n \"imge\": \"x\", // end\n}");

        let found = published(&host, FILE);

        assert_eq!(codes(&found), ["features-malformed", "unknown-property"]);
        assert_eq!(found[1].range.start.line, 3);
    }

    #[test]
    fn ranges_use_utf16_columns_with_multibyte_text() {
        let host = host();
        open(&host, FILE, "{\"name\": \"é😀\", \"imge\": 1}");

        let found = published(&host, FILE);

        assert_eq!(found[0].range.start.character, 16);
        assert_eq!(found[0].range.end.character, 22);
    }

    #[test]
    fn crlf_files_report_correct_lines() {
        let host = host();
        open(&host, FILE, "{\r\n  \"image\": \"x\",\r\n  \"imge\": 1\r\n}\r\n");

        let found = published(&host, FILE);

        assert_eq!(found[0].range.start.line, 2);
        assert_eq!(found[0].range.start.character, 2);

        open(&host, "devcontainer.json", "\r\n// x\r\n{}");
        assert_eq!(published(&host, "devcontainer.json")[0].range.start.line, 2);
    }

    #[test]
    fn fixed_file_publishes_empty_list() {
        let host = host();
        open(&host, ".devcontainer.json", "{\"imge\": 1}");
        assert_eq!(host.provider_diagnostics().len(), 1);

        open(&host, ".devcontainer.json", "{\"image\": \"x\"}");
        assert!(host.provider_diagnostics().is_empty());
    }

    #[test]
    fn escaped_key_recognised() {
        let host = host();
        open(&host, ".devcontainer.json", "{\"\\u0069mage\": \"x\"}");
        assert!(host.provider_diagnostics().is_empty());
    }

    #[test]
    fn manifest_activates_on_views_only() {
        let manifest = include_str!("../plugin.toml");

        for activation in ["on-view:containers", "on-view:run-target-review"] {
            assert!(manifest.contains(&format!("\"{activation}\"")));
        }
        assert!(!manifest.contains("on-event:") && !manifest.contains("\"events\""));
    }

    #[test]
    fn utf8_bom_is_skipped() {
        let host = host();
        open(&host, FILE, "\u{feff}{\"imge\": 1}");

        assert_eq!(codes(&published(&host, FILE)), ["unknown-property"]);
    }

    #[test]
    fn oversized_file_publishes_empty_list() {
        let host = host();
        open(&host, FILE, &format!("{{\"imge\": \"{}\"}}", "x".repeat(MAX_BYTES)));

        assert!(host.provider_diagnostics().is_empty());
    }

    #[test]
    fn non_devcontainer_json_is_ignored() {
        let host = host();
        open(&host, "package.json", "{\"imge\": 1}");

        assert!(host.provider_diagnostics().is_empty());
    }

    #[test]
    fn misplaced_config_gets_only_the_structure_hint() {
        let host = host();
        open(&host, "other/devcontainer.json", "{\"imge\": 1}");

        assert_eq!(codes(&published(&host, "other/devcontainer.json")), ["folder-structure"]);
    }

    #[test]
    fn devcontainer_json_outside_a_devcontainer_folder_gets_a_hint() {
        let host = host();
        open(&host, "devcontainer.json", "// c\n{}");

        let found = published(&host, "devcontainer.json");

        assert_eq!(codes(&found), ["folder-structure"]);
        assert_eq!(found[0].severity, openidea_plugin::Severity::Info);
        assert_eq!(found[0].range, Range::new(openidea_plugin::Position::new(1, 0), openidea_plugin::Position::new(1, 1)));
    }

    #[test]
    fn config_two_levels_below_devcontainer_gets_a_hint() {
        let host = host();
        open(&host, ".devcontainer/a/b/devcontainer.json", "{}");

        assert_eq!(codes(&published(&host, ".devcontainer/a/b/devcontainer.json")), ["folder-structure"]);
    }

    #[test]
    fn config_one_folder_below_devcontainer_is_quiet() {
        let host = host();
        open(&host, ".devcontainer/python/devcontainer.json", "{}");

        assert!(host.provider_diagnostics().is_empty());
    }

    #[test]
    fn root_config_files_are_quiet() {
        let host = host();
        open(&host, ".devcontainer.json", "{}");
        open(&host, "sub/.devcontainer.json", "{}");

        assert!(host.provider_diagnostics().is_empty());
    }

    #[test]
    fn windows_separators_are_normalised() {
        assert!(checks::folder_structure("C:\\work\\.devcontainer\\devcontainer.json").is_none());
        assert!(checks::folder_structure("C:\\work\\devcontainer.json").is_some());
    }

    #[test]
    fn folder_structure_is_quiet_for_library_or_dynamic_name() {
        assert!(checks::folder_structure("/work/app/devcontainer.json.bak").is_none());
        assert!(checks::folder_structure("/work/app/my-devcontainer.json").is_none());
        assert!(!is_devcontainer_file("/work/app/my-devcontainer.json"));
    }

    #[test]
    fn moving_the_file_clears_the_hint() {
        let host = host();
        open(&host, "devcontainer.json", "{}");
        assert_eq!(host.provider_diagnostics().len(), 1);

        open(&host, ".devcontainer/devcontainer.json", "{}");
        assert!(published(&host, ".devcontainer/devcontainer.json").is_empty());
    }

    #[test]
    fn folder_structure_can_be_switched_off() {
        let host = host();
        host.set_setting("checkFolderStructure", "false");
        open(&host, "devcontainer.json", "{}");

        assert!(host.provider_diagnostics().is_empty());
    }

    #[test]
    fn structure_hint_ignores_the_bom() {
        let host = host();
        open(&host, "devcontainer.json", "\u{feff}{}");

        assert_eq!(published(&host, "devcontainer.json")[0].range.start.line, 0);
    }

    #[test]
    fn nested_folder_config_is_checked() {
        let host = host();
        open(&host, ".devcontainer/python/devcontainer.json", "{\"imge\": 1}");

        assert_eq!(host.provider_diagnostics().len(), 1);
    }

    #[test]
    fn each_group_can_be_switched_off() {
        let host = host();
        host.set_setting("checkProperties", "false");
        open(&host, FILE, "{\"imge\": 1, \"features\": 3}");
        assert_eq!(codes(&published(&host, FILE)), ["features-malformed"]);

        host.set_setting("checkFeatures", "false");
        open(&host, FILE, "{\"imge\": 1, \"features\": 3}");
        assert!(host.provider_diagnostics().is_empty());
    }

    #[test]
    fn current_document_diagnostics_work_without_file_events() {
        let host = MockHost::install().with_capabilities(&["project.read"]);
        let mut plugin = Main::default();
        assert!(plugin.activate(&host.context("aritti.devcontainer")).is_ok());
        let doc = openidea_plugin::providers::Document {
            path: "/p/.devcontainer/devcontainer.json".into(),
            language: "jsonc".into(),
            version: 7,
            text: "{\"imge\": 1}".into(),
        };
        assert!(!openidea_plugin::diagnostics::DiagnosticsProvider::diagnostics(&mut plugin, &doc).is_empty());
        let fixed = openidea_plugin::providers::Document { text: String::new(), version: 8, ..doc };
        assert!(openidea_plugin::diagnostics::DiagnosticsProvider::diagnostics(&mut plugin, &fixed).is_empty());
    }

    #[test]
    fn all_templates_registered() {
        let manifest = include_str!("../plugin.toml");
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("templates");
        let mut registered = 0;

        for entry in std::fs::read_dir(&dir).unwrap() {
            let name = entry.unwrap().file_name().into_string().unwrap();
            let path = format!("path = \"templates/{name}\"");
            assert!(manifest.contains(&path), "{name} is not registered");
            registered += 1;
        }

        assert_eq!(manifest.matches("[[contributes.fileTemplates]]").count(), registered);

        for line in manifest.lines().filter(|l| l.starts_with("path = \"templates/")) {
            let relative = line.trim_start_matches("path = \"").trim_end_matches('"');
            let text = std::fs::read_to_string(dir.parent().unwrap().join(relative)).unwrap();
            let json: serde_json::Value = serde_json::from_str(&strip_line_comments(&text)).unwrap_or_else(|e| panic!("{relative}: {e}"));
            assert!(json.get("$schema").is_some(), "{relative} lacks $schema");
            assert!(json.get("image").is_some() || json.get("dockerComposeFile").is_some(), "{relative} lacks image or dockerComposeFile");
        }

        assert!(registered >= 7);
    }
    const CONFIGURATION: &str = r#"npm notice hello
{"configuration": {"image": "mcr.microsoft.com/devcontainers/base:ubuntu", "remoteUser": "vscode",
 "forwardPorts": [3000, 8080], "features": {"a": {}, "b": {}}, "postCreateCommand": "npm i"}, "workspace": {}}"#;

    fn info_host(result: Result<ProcessOutput, Error>) -> MockHost {
        let host = host();
        host.add_file(".devcontainer/devcontainer.json", "{}");
        host.on_process("devcontainer", move |args, _| {
            assert_eq!(args, ["read-configuration", "--workspace-folder", "/work/app", "--config", ".devcontainer/devcontainer.json"]);
            result.clone()
        });
        host
    }

    #[test]
    fn read_configuration_json_is_summarised_in_notification() {
        let host = info_host(ok(CONFIGURATION));

        Main::default().run_command("info", &host.invocation(), None).unwrap();

        let body = &host.notifications()[0].body;
        assert!(body.contains("Image: mcr.microsoft.com/devcontainers/base:ubuntu"), "{body}");
        assert!(body.contains("Remote user: vscode"));
        assert!(body.contains("Forwarded ports: 3000, 8080"));
        assert!(body.contains("Features: 2"));
        assert!(body.contains("postCreateCommand: yes"));
    }

    #[test]
    fn read_configuration_compose_file_is_summarised() {
        let host = info_host(ok(r#"{"configuration": {"dockerComposeFile": ["docker-compose.yml"], "service": "app"}}"#));

        Main::default().run_command("info", &host.invocation(), None).unwrap();

        let body = &host.notifications()[0].body;
        assert!(body.contains("Compose file: docker-compose.yml"), "{body}");
        assert!(body.contains("postCreateCommand: no"));
    }

    #[test]
    fn read_configuration_quiet_failures() {
        for result in [
            ok("not json"),
            Ok(ProcessOutput { status: Some(1), stdout: String::new(), stderr: "boom".into() }),
            Err(Error::NotFound("devcontainer".into())),
        ] {
            let host = info_host(result);

            assert!(Main::default().run_command("info", &host.invocation(), None).is_err());
            assert!(host.notifications().is_empty());
        }
    }

    #[test]
    fn info_without_cli_capability_is_denied() {
        let host = info_host(ok("{}")).with_capabilities(&["project.read"]);

        let error = Main::default().run_command("info", &host.invocation(), None).unwrap_err();

        assert!(matches!(error, Error::Invalid(m) if m.starts_with("Running the devcontainer CLI needs the process:devcontainer permission.")));
    }

    fn complete_in(path: &str, marked: &str) -> Vec<openidea_plugin::providers::CompletionItem> {
        let text = marked.replacen('|', "", 1);
        let caret = marked.find('|').unwrap();
        let doc = openidea_plugin::providers::Document { path: path.into(), language: "jsonc".into(), version: 1, text: text.clone() };

        Main::default().completion(&doc, position(&text, caret), None)
    }

    fn complete(marked: &str) -> Vec<openidea_plugin::providers::CompletionItem> {
        complete_in("/work/app/.devcontainer/devcontainer.json", marked)
    }

    fn labels(items: &[openidea_plugin::providers::CompletionItem]) -> Vec<&str> {
        items.iter().map(|i| i.label.as_str()).collect()
    }

    #[test]
    fn suggests_missing_top_level_properties() {
        let _host = host();

        let items = complete("{\n  \"image\": \"x\",\n  |\n}");

        assert!(labels(&items).contains(&"remoteUser"));
        assert!(!labels(&items).contains(&"image"));
        let item = items.iter().find(|i| i.label == "remoteUser").unwrap();
        assert_eq!(item.insert_text, "\"remoteUser\": $0");
        assert!(item.snippet);
    }

    #[test]
    fn suggests_enum_values_for_shutdown_action() {
        let _host = host();

        let items = complete("{\"shutdownAction\": \"|\"}");

        assert_eq!(labels(&items), ["none", "stopContainer", "stopCompose"]);
        assert_eq!(items[0].insert_text, "\"none\"");
        assert!(!items[0].snippet);
    }

    #[test]
    fn suggests_booleans_for_privileged() {
        let _host = host();

        assert_eq!(labels(&complete("{\"privileged\": |}")), ["true", "false"]);
        assert_eq!(labels(&complete("{\"privileged\": tr|,\n\"image\": 1}")), ["true", "false"]);
    }

    #[test]
    fn cached_feature_option_completion_is_bound_to_the_current_feature_index() {
        let _host = MockHost::install().with_capabilities(&["storage"]);
        let index = "fixture index";
        host::storage::set("feature-index",index).unwrap();
        host::storage::set(feature_options::CACHE_KEY,&serde_json::json!({"index":index,"features":{
            "ghcr.io/example/node:1":{"enabled":{"type":"boolean"},"version":{"type":"string","enum":["stable","next"]}}
        }}).to_string()).unwrap();
        assert_eq!(labels(&complete(r#"{"features":{"ghcr.io/example/node:1":{ |}}}"#)), ["enabled","version"]);
        assert_eq!(labels(&complete(r#"{"features":{"ghcr.io/example/node:1":{"enabled": |}}}"#)), ["true","false"]);
        let items=complete(r#"{"features":{"ghcr.io/example/node:1":{"version":"|"}}}"#);
        assert_eq!(labels(&items), ["stable","next"]);
        assert_eq!(items[0].insert_text,"\"stable\"");
        assert!(complete(r#"{"features":{"other":{ |}}}"#).is_empty());
        assert!(complete(r#"{"features":{"ghcr.io/example/node:1":{/* | */}}}"#).is_empty());
        host::storage::set("feature-index","different index").unwrap();
        assert!(complete(r#"{"features":{"ghcr.io/example/node:1":{ |}}}"#).is_empty());
    }

    #[test]
    fn nested_build_and_host_requirements_complete_without_root_properties() {
        let _host = host();
        assert_eq!(labels(&complete(r#"{"build":{"dockerfile":"Dockerfile", |}}"#)), ["context", "args", "target", "cacheFrom", "options"]);
        assert_eq!(labels(&complete(r#"{"hostRequirements":{ |}}"#)), ["cpus", "memory", "storage", "gpu"]);
        assert!(complete(r#"{"build":{"args":{"|"}}}"#).is_empty());
        assert!(complete(r#"{"build":{/* comment | */}}"#).is_empty());
        assert!(labels(&complete(r#"{"build":{ "con|"}}"#)).contains(&"context"));
        assert!(!complete(r#"{"build":{ |"#).is_empty());
        let existing = complete(r#"{"build":{"con|text":".."}}"#);
        let item = existing.iter().find(|i| i.label == "context").unwrap();
        assert_eq!(item.insert_text, "\"context\"");
        assert!(!item.snippet);
    }

    #[test]
    fn port_attributes_complete_keys_enums_and_booleans_in_jsonc() {
        let _host = host();
        let items = complete(r#"{"portsAttributes":{"3000":{"label":"Web", |}}}"#);
        assert_eq!(labels(&items), ["protocol", "onAutoForward", "requireLocalPort", "elevateIfNeeded"]);
        assert_eq!(labels(&complete(r#"{"portsAttributes":{"db:5432":{"protocol":"|"}}}"#)), ["http", "https"]);
        assert_eq!(labels(&complete(r#"{"otherPortsAttributes":{"requireLocalPort": |}}"#)), ["true", "false"]);
        let items = complete("{\"name\":\"🐳\",\"otherPortsAttributes\":{ /* JSONC */ \"protocol\":\"h|\",}}");
        assert_eq!(items[0].insert_text, "\"http\"");
        let span = items[0].replace.unwrap();
        assert_eq!(span.end.character - span.start.character, 3);
        assert!(complete(r#"{"portsAttributes":{"|"}}"#).is_empty());
        assert!(complete(r#"{"customizations":{"protocol":"|"}}"#).is_empty());
    }

    #[test]
    fn completion_is_quiet_inside_nested_objects_and_arrays() {
        let _host = host();

        for marked in [
            "{\"features\": {|}}",
            "{\"customizations\": {\"vscode\": {\"x\": |}}}",
            "{\"forwardPorts\": [|]}",
            "{\"forwardPorts\": [3000, \"|\"]}",
            "{\"image\": \"x\"}|",
            "|{\"image\": \"x\"}",
        ] {
            assert!(complete(marked).is_empty(), "{marked}");
        }
    }

    #[test]
    fn completion_is_quiet_inside_comments_and_for_other_files() {
        let _host = host();

        assert!(complete("{\n  // rem|\n}").is_empty());
        assert!(complete("{\n  /* rem| */\n}").is_empty());
        assert!(complete("{\"privileged\": true, // x|\n}").is_empty());
        assert!(!complete("{\n  // c\n  |\n}").is_empty());

        for path in ["/work/app/package.json", "/work/app/tsconfig.json", "/work/app/.vscode/settings.json"] {
            assert!(complete_in(path, "{|}").is_empty(), "{path}");
        }
    }

    #[test]
    fn completion_works_in_misplaced_files() {
        let _host = host();

        assert!(!complete_in("/work/app/devcontainer.json", "{|}").is_empty());
        assert!(!complete_in("/work/app/.devcontainer.json", "{|}").is_empty());
    }

    #[test]
    fn completion_survives_an_unterminated_string_at_the_caret() {
        let _host = host();

        let items = complete("{\"image\": \"x\",\n  \"remo|");
        assert!(labels(&items).contains(&"remoteUser"));
        let replace = items[0].replace.clone().unwrap();
        assert_eq!((replace.start.line, replace.start.character, replace.end.character), (1, 2, 7));

        assert_eq!(labels(&complete("{\"waitFor\": \"|")).len(), 6);
    }

    #[test]
    fn completion_reads_jsonc_comments_and_trailing_commas() {
        let _host = host();

        let items = complete("{\n  // \"name\": 1,\n  /* \"init\": */ \"image\": \"x\", // end\n  |\n}");

        assert!(labels(&items).contains(&"name"));
        assert!(labels(&items).contains(&"init"));
        assert!(!labels(&items).contains(&"image"));
        assert_eq!(labels(&complete("{\"init\": |, }")), ["true", "false"]);
    }

    #[test]
    fn feature_keys_are_found_after_comments() {
        for text in [
            "{\"features\": { // first\n  \"ghcr.io/a/b:1\": {},\n  /* next */ \"x\": {} }}",
            "{\"features\": {\n  \"a\": {}, // next\n  \"x\": {} }}",
        ] {
            let offset = text.find("\"x\"").unwrap() + 2;

            assert!(checks::feature_key_at(text, offset).is_some(), "{text}");
        }
    }

    #[test]
    fn completion_ranges_use_utf16_columns_with_multibyte_text() {
        let _host = host();

        let items = complete("{\"name\": \"é😀\", \"pri|\": 1}");

        let replace = items.iter().find(|i| i.label == "privileged").unwrap().replace.clone().unwrap();
        assert_eq!((replace.start.character, replace.end.character), (16, 21));
    }

    #[test]
    fn completion_can_be_switched_off() {
        let host = host();
        host.set_setting("completeProperties", "false");

        assert!(complete("{|}").is_empty());
    }

    #[test]
    fn completion_is_fast_on_a_256_kib_file() {
        let _host = host();

        let oversized = format!("{{\"name\": \"{}\", |}}", "x".repeat(MAX_BYTES));
        assert!(complete(&oversized).is_empty());

        let near_limit = format!("{{\"name\": \"{}\", |}}", "x".repeat(MAX_BYTES - 100));
        assert!(!complete(&near_limit).is_empty());
    }

    #[test]
    fn completion_at_the_end_of_unfinished_text_does_not_panic() {
        let _host = host();

        for marked in ["{\n|", "{ |", "{\"a\": 1,\n|", "{\"a\": |", "{\"a\":\n|"] {
            let _ = complete(marked);
        }

        assert!(!complete("{\n|").is_empty());
        assert!(!complete("{ |").is_empty());
        assert!(!complete("{\"image\": 1,\n|").is_empty());
    }

    #[test]
    fn structure_hint_ranges_use_utf16_columns_with_multibyte_first_character() {
        let host = host();
        open(&host, "devcontainer.json", "/* é😀 */ 日{}");

        let found = published(&host, "devcontainer.json");

        assert_eq!(found[0].range.start.character, 10);
        assert_eq!(found[0].range.end.character, 11);

        open(&host, "other/devcontainer.json", "é{}");
        assert_eq!(published(&host, "other/devcontainer.json")[0].range.end.character, 1);
    }

    #[test]
    fn manifest_registers_the_completion_and_line_marker_provider() {
        let manifest = include_str!("../plugin.toml");

        assert!(manifest.contains("kinds = [\"completion\", \"code-actions\", \"line-markers\"]"));
        assert!(manifest.contains("languages = [\"json\", \"jsonc\"]"));
    }

    const INDEX: &str = r#"{"collections": [{"features": [
        {"id": "ghcr.io/devcontainers/features/node", "name": "Node.js", "majorVersion": "1"},
        {"id": "ghcr.io/x/features/plain", "name": "Plain"},
        {"name": "No id"}
    ]}]}"#;

    fn feature_labels(marked: &str) -> Vec<String> {
        complete(marked).into_iter().map(|i| i.label).collect()
    }

    #[test]
    fn refresh_stores_the_index_and_completion_reads_it() {
        let host = MockHost::install().with_capabilities(&["project.read", "storage", "net:containers.dev"]);
        host.on_fetch(|r| {
            assert_eq!(r.url, features::INDEX_URL);
            Ok(openidea_plugin::HttpResponse { status: 200, headers: vec![], body: INDEX.as_bytes().to_vec() })
        });
        Main::default().run_command("refresh-features", &host.invocation(), None).unwrap();

        let items = complete("{\n  \"features\": {\n    \"ghcr|\n  }\n}");
        assert_eq!(
            items.iter().map(|i| i.label.as_str()).collect::<Vec<_>>(),
            ["ghcr.io/devcontainers/features/node:1", "ghcr.io/x/features/plain"]
        );
        assert_eq!(items[0].insert_text, "\"ghcr.io/devcontainers/features/node:1\": {$0}");
        assert_eq!(items[0].detail.as_deref(), Some("Node.js"));

        let after_comma = "{\"features\": {\"a/b\": {}, \"|\": {}}}";
        assert_eq!(feature_labels(after_comma).len(), 2);
        let existing = complete("{\"features\": {\"gh|cr\": {}}}");
        assert_eq!(existing[0].insert_text, "\"ghcr.io/devcontainers/features/node:1\"");
    }

    #[test]
    fn feature_ids_are_quiet_outside_feature_keys() {
        let _host = MockHost::install().with_capabilities(&["project.read", "storage"]);
        host::storage::set("feature-index", "ghcr.io/a/b:1\tB\n").unwrap();
        assert_eq!(feature_labels("{\"features\": {\"|\": {}}}"), ["ghcr.io/a/b:1"]);
        assert!(feature_labels("{\"features\": {\"ghcr.io/a/b:1\": {\"version\": \"|\"}}}").is_empty());
        assert!(feature_labels("{\"features\": {\"ghcr.io/a/b:1\": {\"x\": 1, \"|\": 2}}}").is_empty());
        assert!(feature_labels("{\"image\": \"|\"}").is_empty());
        assert!(feature_labels("{\"features\": [\"|\"]}").is_empty());
    }

    #[test]
    fn empty_cache_and_denied_storage_offer_nothing() {
        let _host = MockHost::install().with_capabilities(&["project.read"]);
        assert!(feature_labels("{\"features\": {\"|\": {}}}").is_empty());
    }

    #[test]
    fn refresh_reports_a_bad_status_and_an_odd_index() {
        let host = MockHost::install().with_capabilities(&["project.read", "storage", "net:containers.dev"]);
        host.on_fetch(|_| Ok(openidea_plugin::HttpResponse { status: 503, headers: vec![], body: vec![] }));
        assert!(Main::default().run_command("refresh-features", &host.invocation(), None).is_err());
        host.on_fetch(|_| Ok(openidea_plugin::HttpResponse { status: 200, headers: vec![], body: b"[]".to_vec() }));
        assert!(Main::default().run_command("refresh-features", &host.invocation(), None).is_err());
        assert!(host.storage().is_empty());
    }

    fn docker_host(output: &'static str) -> MockHost {
        let host = host();
        host.on_process("docker", move |args, _| match args[0].as_str() {
            "ps" => {
                assert_eq!(args[1..4], ["-a", "--filter", "label=devcontainer.local_folder=/work/app"]);
                ok(output)
            }
            _ => ok(""),
        });
        host
    }

    const PS: &str =
        "{\"ID\":\"abc123\",\"Image\":\"node:20\",\"Names\":\"happy_fox\",\"Status\":\"Up 2 minutes\"}\nnot json\n{\"Names\":\"no id\"}\n";

    #[test]
    fn parses_docker_ps_json_lines() {
        let containers = dashboard::parse_containers(PS);
        assert_eq!(containers.len(), 1);
        assert_eq!(
            (containers[0].id.as_str(), containers[0].name.as_str(), containers[0].image.as_str()),
            ("abc123", "happy_fox", "node:20")
        );
    }

    #[test]
    fn view_lists_containers_and_acts_on_the_selection() {
        let host = docker_host(PS);
        let mut plugin = Main::default();
        let view = plugin.render("containers", None, &host.invocation()).unwrap();
        let Body::List(items) = view.body else { panic!("a list") };
        assert_eq!(items[0].key, "abc123");
        assert_eq!(items[0].detail.as_deref(), Some("node:20 - Up 2 minutes"));
        assert_eq!(view.toolbar.iter().map(|b| b.id.as_str()).collect::<Vec<_>>(), ["refresh", "rebuild", "configuration"]);

        plugin.handle("containers", ViewEvent::Select("abc123".into()), &host.invocation()).unwrap();
        assert!(plugin.render("containers", None, &host.invocation()).unwrap().toolbar.iter().any(|b| b.id == "shell"));
        plugin.handle("containers", ViewEvent::Button("shell".into()), &host.invocation()).unwrap();
        assert!(host.finish_terminal("docker-shell-abc123").is_some());

        host.answer_next_prompt(Some(&[]));
        plugin.handle("containers", ViewEvent::Button("stop".into()), &host.invocation()).unwrap();
        assert!(host.process_runs().iter().any(|run| run.args == ["stop", "abc123"]));
        assert!(plugin.selected.is_none());
    }

    const PS_ALL: &str = "{\"ID\":\"abc123\",\"Image\":\"node:20\",\"Names\":\"up\",\"State\":\"running\",\"Status\":\"Up 2 minutes\"}\n{\"ID\":\"def456\",\"Image\":\"node:18\",\"Names\":\"old\",\"State\":\"exited\",\"Status\":\"Exited (0) 3 days ago\"}\n";

    #[test]
    fn view_lists_stopped_containers_and_starts_them() {
        let host = docker_host(PS_ALL);
        let mut plugin = Main::default();
        let view = plugin.render("containers", None, &host.invocation()).unwrap();
        let Body::List(items) = &view.body else { panic!("a list") };
        assert_eq!(items.len(), 2);
        assert_eq!(items[1].detail.as_deref(), Some("node:18 - Exited (0) 3 days ago"));
        assert_eq!(view.status.as_deref(), Some("1 running, 1 stopped"));

        plugin.handle("containers", ViewEvent::Select("def456".into()), &host.invocation()).unwrap();
        let toolbar = plugin.render("containers", None, &host.invocation()).unwrap().toolbar;
        assert_eq!(toolbar.iter().map(|b| b.id.as_str()).collect::<Vec<_>>(), ["refresh", "start", "rebuild", "configuration"]);

        plugin.handle("containers", ViewEvent::Button("start".into()), &host.invocation()).unwrap();
        assert!(host.process_runs().iter().any(|run| run.args == ["start", "def456"]));
        let started = &host.notifications_with_actions()[0];
        assert_eq!(started.actions, [openidea_plugin::NotificationAction::new("Open Shell", "shell").argument("def456")]);

        plugin.handle("containers", ViewEvent::Activate("abc123".into()), &host.invocation()).unwrap();
        assert!(host.finish_terminal("docker-shell-abc123").is_some());
        plugin.run_command("shell", &host.invocation(), Some("abc123")).unwrap();
        assert!(host.finish_terminal("docker-shell-abc123").is_some());
        assert!(plugin.run_command("shell", &host.invocation(), Some("def456")).is_err(), "the mock still lists def456 as stopped");
    }

    #[test]
    fn view_explains_empty_denied_and_failed_listings() {
        let host = docker_host("");
        host.add_file(".devcontainer/devcontainer.json", "{}");
        let message = |host: &MockHost| match Main::default().render("containers", None, &host.invocation()).unwrap().body {
            Body::List(rows) if rows.len() == 1 && rows[0].key == "devcontainer:status" => rows[0].detail.clone().unwrap(),
            _ => panic!("a status row visible in Services"),
        };
        assert!(message(&host).starts_with("No dev container"));

        let denied = MockHost::install().with_capabilities(&["project.read"]);
        assert!(message(&denied).contains("docker permission"));

        let failing = host_failing();
        assert!(message(&failing).starts_with("Cannot list containers"));
    }

    fn host_failing() -> MockHost {
        let host = host();
        host.on_process("docker", |_, _| Ok(ProcessOutput { status: Some(1), stdout: String::new(), stderr: "daemon down".into() }));
        host
    }

    #[test]
    fn stop_button_without_a_selection_does_nothing() {
        let host = docker_host(PS);
        Main::default().handle("containers", ViewEvent::Button("stop".into()), &host.invocation()).unwrap();
        assert!(host.process_runs().is_empty());
    }
}
