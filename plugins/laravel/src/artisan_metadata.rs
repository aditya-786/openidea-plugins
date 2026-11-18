//! Explicit PHP execution only. Cached runtime facts never become static
//! declarations, and reading a picker never runs project bootstrap code.
use openidea_plugin::{Error, Invocation, NotificationLevel, Project, host};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::time::Instant;
const MAX_OUTPUT: usize = 4 * 1024 * 1024;
const MAX_CACHE: usize = 5 * 1024 * 1024;
const ARGS: [&str; 5] = [
    "artisan",
    "list",
    "--format=json",
    "--no-interaction",
    "--no-ansi",
];
pub(super) fn hash(text: &str) -> String {
    format!(
        "{:016x}",
        text.bytes()
            .fold(0xcbf29ce484222325u64, |h, b| (h ^ u64::from(b))
                .wrapping_mul(0x100000001b3))
    )
}
fn key(root: &str) -> String {
    format!("artisan-metadata-v2:{}", hash(root))
}
pub(super) fn identity(project: &Project) -> Result<Value, Error> {
    let root = project.root()?;
    if root.is_empty() || root.len() > 4096 || root.chars().any(char::is_control) {
        return Err(Error::Invalid(
            "Project identity is unavailable or too large.".into(),
        ));
    }
    let mut files = serde_json::Map::new();
    for (name, required, limit) in [
        ("artisan", true, 256 * 1024),
        ("composer.json", false, 512 * 1024),
        ("composer.lock", false, 2 * 1024 * 1024),
    ] {
        match project.read_file(&format!("{root}/{name}")) {
            Ok(text) if text.len() <= limit => {
                files.insert(name.into(), json!(hash(&text)));
            }
            Ok(_) => {
                return Err(Error::Invalid(format!(
                    "{name} exceeds the discovery identity bound."
                )));
            }
            Err(Error::NotFound(_)) if !required => {
                files.insert(name.into(), Value::Null);
            }
            Err(Error::NotFound(_)) => {
                return Err(Error::Invalid(
                    "The selected project has no Artisan entrypoint.".into(),
                ));
            }
            Err(error) => return Err(error),
        }
    }
    Ok(json!({"root":root,"runner":"php","argv":ARGS,"files":files}))
}
fn text(value: &Value, key: &str, max: usize) -> Result<String, Error> {
    let value = value[key].as_str().unwrap_or_default();
    if value.len() > max
        || value
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err(Error::Invalid(format!(
            "Artisan {key} exceeds text limits."
        )));
    }
    Ok(value.to_owned())
}
fn name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_:-.".contains(&b))
        && !value.starts_with('-')
}
pub(super) fn parse(raw: &str) -> Result<Value, Error> {
    if raw.len() > MAX_OUTPUT {
        return Err(Error::Invalid(
            "Artisan JSON output exceeds 4 MiB; previous snapshot retained.".into(),
        ));
    }
    let value = crate::strict_json::parse_with_budget(raw, MAX_OUTPUT, 200_000).map_err(|_| {
        Error::Invalid(
            "Artisan did not return bounded duplicate-free JSON; previous snapshot retained."
                .into(),
        )
    })?;
    let commands = value["commands"]
        .as_array()
        .filter(|v| !v.is_empty() && v.len() <= 512)
        .ok_or("Artisan JSON must have 1..512 commands.")?;
    let mut seen = BTreeSet::new();
    let mut normalized = Vec::new();
    for command in commands {
        let id = text(command, "name", 128)?;
        if !name(&id) || !seen.insert(id.clone()) {
            return Err(Error::Invalid(
                "Artisan command names are invalid or duplicated.".into(),
            ));
        }
        if !command["definition"].is_object() {
            return Err(Error::Invalid(
                "Artisan JSON has no supported argument/option definition object.".into(),
            ));
        }
        let mut definitions = serde_json::Map::new();
        for kind in ["arguments", "options"] {
            let source = &command["definition"][kind];
            let items: Vec<&Value> = match source {
                Value::Object(o) => o.values().collect(),
                Value::Array(a) => a.iter().collect(),
                Value::Null => {
                    return Err(Error::Invalid(
                        "Artisan JSON argument/option definitions are missing.".into(),
                    ));
                }
                _ => {
                    return Err(Error::Invalid(
                        "Artisan argument/option definitions must be objects or arrays.".into(),
                    ));
                }
            };
            if items.len() > 128 {
                return Err(Error::Invalid("Too many Artisan arguments/options.".into()));
            }
            let mut names = BTreeSet::new();
            let mut entries = Vec::new();
            for item in items {
                let id = text(item, "name", 128)?;
                if !name(id.trim_start_matches('-')) || !names.insert(id.clone()) {
                    return Err(Error::Invalid(
                        "Artisan argument/option names are missing or duplicated.".into(),
                    ));
                }
                let mut entry = json!({"name":id,"description":text(item,"description",2048)?});
                for flag in [
                    "is_required",
                    "is_array",
                    "accept_value",
                    "is_value_required",
                    "is_multiple",
                ] {
                    if let Some(value) = item.get(flag) {
                        if !value.is_boolean() {
                            return Err(Error::Invalid(
                                "Artisan argument flags must be boolean.".into(),
                            ));
                        }
                        entry[flag] = value.clone();
                    }
                }
                if let Some(default) = item.get("default") {
                    fn scalar(v: &Value) -> bool {
                        v.is_null()
                            || v.is_boolean()
                            || v.is_number()
                            || v.as_str()
                                .is_some_and(|s| s.len() <= 4096 && !s.contains('\0'))
                    }
                    if !(scalar(default)
                        || default
                            .as_array()
                            .is_some_and(|a| a.len() <= 128 && a.iter().all(scalar)))
                    {
                        return Err(Error::Invalid(
                            "Unsupported or oversized Artisan default.".into(),
                        ));
                    }
                    entry["default"] = default.clone();
                }
                entries.push(entry);
            }
            definitions.insert(kind.into(), Value::Array(entries));
        }
        normalized.push(json!({"name":id,"description":text(command,"description",4096)?,"definition":definitions}));
    }
    Ok(Value::Array(normalized))
}
fn load(root: &str) -> Option<Value> {
    let raw = host::storage::get(&key(root)).ok().flatten()?;
    if raw.len() > MAX_CACHE {
        return None;
    }
    let value = crate::strict_json::parse_with_budget(&raw, MAX_CACHE, 200_000).ok()?;
    (value["capturedAt"].as_u64().is_some()
        && value["schema"] == 2
        && value["identity"]["root"] == root
        && value["identity"]["runner"] == "php"
        && value["identity"]["argv"] == json!(ARGS))
    .then_some(value)
}
fn store(identity: &Value, commands: Value) -> Result<(), Error> {
    let normalized = json!({"commands":&commands}).to_string();
    let _ = parse(&normalized)?;
    let root = identity["root"].as_str().ok_or("No project identity.")?;
    if let Some(raw) = host::storage::get(&key(root))? {
        if raw.len() > MAX_CACHE || load(root).is_none() {
            return Err(Error::Invalid(
                crate::ui_text::text(
                    "native.sourceArtisan.f3da11e3ec",
                    "Existing Artisan cache is unreadable or has another owner; it was retained.",
                )
                .into(),
            ));
        }
    }
    let captured = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let raw = json!({"schema":2,"identity":identity,"capturedAt":captured,"commands":commands})
        .to_string();
    if raw.len() > MAX_CACHE {
        return Err(Error::Invalid(
            "Artisan cache exceeds 5 MiB; previous snapshot retained.".into(),
        ));
    }
    host::storage::set(&key(root), &raw)
}
pub(super) fn snapshot(project: &Project) -> Option<Value> {
    let current = identity(project).ok()?;
    let mut value = load(current["root"].as_str()?)?;
    if value["identity"] != current {
        return None;
    }
    value["commands"] = parse(&json!({"commands":value["commands"]}).to_string()).ok()?;
    Some(value)
}

pub fn cached(project: &Project) -> Option<Vec<(String, String)>> {
    let identity = identity(project).ok()?;
    let value = load(identity["root"].as_str()?)?;
    if value["identity"] != identity {
        return None;
    }
    // Revalidate the stored normalized command evidence with the same parser.
    let commands = parse(&json!({"commands":value["commands"]}).to_string()).ok()?;
    Some(
        commands
            .as_array()?
            .iter()
            .map(|command| {
                let arguments = command["definition"]["arguments"].as_array().map_or(0, Vec::len);
                let options = command["definition"]["options"].as_array().map_or(0, Vec::len);
                (
                    command["name"].as_str().unwrap_or_default().to_owned(),
                    crate::ui_text::format("native.sourceArtisan.c926f6358e","{description} — {arguments} arguments / {options} options; captured Unix {captured} (may be stale)",&[("description",&(command["description"].as_str().unwrap_or_default()).to_string()),("arguments",&(arguments).to_string()),("options",&(options).to_string()),("captured",&(value["capturedAt"]).to_string())]),
                )
            })
            .collect(),
    )
}
pub(super) fn collect(
    next: impl FnMut(u32) -> Result<host::Chunk, Error>,
    cancelled: impl Fn() -> bool,
) -> Result<String, Error> {
    collect_limited(MAX_OUTPUT, 256, next, cancelled)
}
pub(super) fn collect_limited(
    limit: usize,
    polls: usize,
    mut next: impl FnMut(u32) -> Result<host::Chunk, Error>,
    cancelled: impl Fn() -> bool,
) -> Result<String, Error> {
    let started = Instant::now();
    let mut stdout = String::new();
    let mut stderr = String::new();
    for _ in 0..polls {
        if cancelled() {
            return Err(Error::Cancelled);
        }
        if started.elapsed().as_millis() >= 4000 {
            return Err(Error::Failed(
                "Artisan metadata discovery exceeded four seconds; previous snapshot retained."
                    .into(),
            ));
        }
        let remaining = 4000u128.saturating_sub(started.elapsed().as_millis());
        let chunk = next(remaining.min(250) as u32)?;
        if started.elapsed().as_millis() >= 4000 {
            return Err(Error::Failed("Artisan metadata exceeded its four-second polling budget; previous snapshot retained.".into()));
        }
        if stdout.len() + stderr.len() + chunk.stdout.len() + chunk.stderr.len() > limit {
            return Err(Error::Invalid(format!(
                "Artisan output exceeds {} KiB; previous snapshot retained.",
                limit / 1024
            )));
        }
        stdout.push_str(&chunk.stdout);
        stderr.push_str(&chunk.stderr);
        if cancelled() {
            return Err(Error::Cancelled);
        }
        if let Some(status) = chunk.status {
            if status != 0 {
                return Err(Error::Failed(format!(
                    "Artisan list exited {status}; previous snapshot retained. {}",
                    stderr.chars().take(1024).collect::<String>()
                )));
            }
            // Exit status does not mean the host's buffered pipes have drained.
            if chunk.stdout.is_empty() && chunk.stderr.is_empty() {
                return Ok(stdout);
            }
        }
    }
    Err(Error::Failed("Artisan metadata discovery exhausted its four-second polling budget; previous snapshot retained.".into()))
}
fn execute(project: &Project, args: &[String]) -> Result<String, Error> {
    let task = host::begin_task(
        crate::ui_text::text(
            "native.sourceArtisan.0de52b5ae0",
            "Discover Artisan metadata (executes project PHP)",
        ),
        true,
    );
    let child = crate::artisan_runner::start_background(crate::artisan_runner::Runner::Php, args, project)?;
    child.close_stdin()?;
    let result = collect(|timeout| child.wait(timeout), || task.is_cancelled());
    if result.is_err() {
        let _ = child.kill();
    }
    result
}
fn refresh_with(
    ctx: &Invocation,
    execute: impl FnOnce(&Project, &[String]) -> Result<String, Error>,
) -> Result<Option<String>, Error> {
    let project = ctx.project().ok_or(crate::ui_text::text(
        "native.sourceArtisan.fc5901cf3a",
        "Open a project first; anonymous Artisan execution is not supported.",
    ))?;
    let before = identity(&project)?;
    let root = before["root"].as_str().unwrap();
    if host::storage::get(&key(root))?.is_some() && load(root).is_none() {
        return Err(Error::Invalid(
            crate::ui_text::text(
                "native.sourceArtisan.f3da11e3ec",
                "Existing Artisan cache is unreadable or has another owner; it was retained.",
            )
            .into(),
        ));
    }
    let preview = crate::ui_text::format(
        "native.sourceArtisan.23a45eb837",
        "This executes project-controlled PHP bootstrap code and may have side effects. It is not static file reading.\n\nProject / working directory: {root}\nRunner: php (resolved from host PATH, process:php grant required)\nArgv: {argv}\n\nOnly command metadata is requested. No Docker/remote runner is used. Output is limited to 4 MiB and polling to four seconds. Refresh is explicit; cancellation/failure retains the previous saved snapshot.",
        &[
            ("root", &(root).to_string()),
            ("argv", &(json!(ARGS)).to_string()),
        ],
    );
    if !ui_kit::prompts::confirm_exact(
        crate::ui_text::text(
            "native.sourceArtisan.7bca1e06da",
            "Execute project PHP to discover Artisan commands?",
        ),
        &preview,
    )? {
        return Ok(None);
    }
    if ctx.project().and_then(|p| p.root().ok()).as_deref() != Some(root)
        || identity(&project)? != before
    {
        return Err(Error::Stale);
    }
    let args = ARGS.map(str::to_owned);
    let output = execute(&project, &args)?;
    if ctx.project().and_then(|p| p.root().ok()).as_deref() != Some(root)
        || identity(&project)? != before
    {
        return Err(Error::Stale);
    }
    let commands = parse(&output)?;
    let count = commands.as_array().map_or(0, Vec::len);
    store(&before, commands)?;
    host::notify(
        NotificationLevel::Info,
        "Laravel",
        &crate::ui_text::format(
            "native.sourceArtisan.215fb7956c",
            "Saved executed Artisan runtime metadata for {count} commands. It is a snapshot, not bundled static metadata.",
            &[("count", &(count).to_string())],
        ),
    );
    Ok(None)
}
pub fn refresh(ctx: &Invocation) -> Result<Option<String>, Error> {
    refresh_ui_inner(ctx).map_err(ui_error)
}
fn refresh_ui_inner(ctx: &Invocation) -> Result<Option<String>, Error> {
    refresh_with(ctx, execute)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    fn setup() -> MockHost {
        let host = MockHost::install();
        host.add_file("artisan", "<?php require 'bootstrap/app.php';");
        host.add_file("composer.json", "{}");
        host
    }
    fn sample() -> String {
        json!({"commands":[{"name":"app:custom","description":"Project command","definition":{"arguments":{"name":{"name":"name","is_required":true,"description":"A name"}},"options":{"force":{"name":"--force","accept_value":false}}}}]}).to_string()
    }
    fn seed(host: &MockHost) {
        host.answer_next_prompt(Some(&["ok"]));
        refresh_with(&host.invocation(), |project, args| {
            assert_eq!(project.root().unwrap(), "/project");
            assert_eq!(args, ARGS.map(str::to_owned));
            Ok(sample())
        })
        .unwrap();
    }
    #[test]
    fn approved_snapshot_restores_without_execution_and_keeps_argument_option_facts() {
        let host = setup();
        seed(&host);
        let project = host.invocation().project().unwrap();
        let items = cached(&project).unwrap();
        assert_eq!(items[0].0, "app:custom");
        assert!(items[0].1.contains("1 arguments / 1 options"));
        assert!(
            load("/project").unwrap()["commands"][0]["definition"]["arguments"][0]["is_required"]
                .as_bool()
                .unwrap()
        );
        assert!(host.process_runs().is_empty());
    }
    #[test]
    fn cancel_and_absent_project_or_storage_do_not_execute_php() {
        let host = setup();
        let ran = std::cell::Cell::new(false);
        refresh_with(&host.invocation(), |_, _| {
            ran.set(true);
            Ok(sample())
        })
        .unwrap();
        assert!(!ran.get());
        host.set_project_root(None);
        assert!(refresh_with(&host.invocation(), |_, _| panic!("must not run")).is_err());
        let host = MockHost::install().with_capabilities(&["project.read"]);
        host.add_file("artisan", "<?php");
        assert!(matches!(
            refresh_with(&host.invocation(), |_, _| panic!("must not run")),
            Err(Error::Denied(_))
        ));
    }
    #[test]
    fn failed_or_stale_refresh_retains_actual_previous_snapshot() {
        let host = setup();
        seed(&host);
        let before = host.storage()[&key("/project")].clone();
        for output in [
            Err(Error::Failed("runtime bootstrap failed".into())),
            Ok("not JSON".into()),
            Ok("x".repeat(MAX_OUTPUT + 1)),
        ] {
            host.answer_next_prompt(Some(&["ok"]));
            assert!(refresh_with(&host.invocation(), |_, _| output).is_err());
            assert_eq!(host.storage()[&key("/project")], before);
        }
        host.answer_next_prompt(Some(&["ok"]));
        assert_eq!(
            refresh_with(&host.invocation(), |_, _| {
                host.add_file("artisan", "<?php changed");
                Ok(sample())
            }),
            Err(Error::Stale)
        );
        assert_eq!(host.storage()[&key("/project")], before);
        assert!(cached(&host.invocation().project().unwrap()).is_none());
    }
    #[test]
    fn project_and_runner_owner_mismatches_do_not_reuse_or_replace_cache() {
        let host = setup();
        seed(&host);
        let before = host.storage()[&key("/project")].clone();
        host.set_project_root(Some("/other"));
        host.add_file("/other/artisan", "<?php");
        assert!(cached(&host.invocation().project().unwrap()).is_none());
        host::storage::set(&key("/other"), &before).unwrap();
        host.answer_next_prompt(Some(&["ok"]));
        assert!(
            refresh_with(&host.invocation(), |_, _| panic!(
                "wrong-owner cache must not execute"
            ))
            .is_err()
        );
        assert_eq!(host.storage()[&key("/other")], before);
    }
    #[test]
    fn parser_rejects_duplicates_malformed_flags_and_unbounded_metadata() {
        assert!(
            parse(&json!({"commands":[{"name":"app:test"},{"name":"app:test"}]}).to_string())
                .is_err()
        );
        assert!(parse(&json!({"commands":[{"name":"--danger"}]}).to_string()).is_err());
        let raw = sample().replace("true", "\"true\"");
        assert!(parse(&raw).is_err());
        let raw = json!({"commands":vec![json!({"name":"app:test"});513]}).to_string();
        assert!(parse(&raw).is_err());
    }
    #[test]
    fn bounded_collection_handles_split_output_exit_failure_cancellation_and_timeout() {
        let mut chunks = vec![
            host::Chunk {
                status: Some(0),
                ..Default::default()
            },
            host::Chunk {
                stdout: "b".into(),
                status: Some(0),
                ..Default::default()
            },
            host::Chunk {
                stdout: "a".into(),
                ..Default::default()
            },
        ];
        assert_eq!(
            collect(|_| Ok(chunks.pop().unwrap()), || false).unwrap(),
            "ab"
        );
        assert!(matches!(
            collect(|_| panic!("cancel before poll"), || true),
            Err(Error::Cancelled)
        ));
        assert!(
            collect(
                |_| Ok(host::Chunk {
                    status: Some(2),
                    stderr: "failed".into(),
                    ..Default::default()
                }),
                || false
            )
            .unwrap_err()
            .to_string()
            .contains("exited 2")
        );
        assert!(
            collect(|_| Ok(host::Chunk::default()), || false)
                .unwrap_err()
                .to_string()
                .contains("budget")
        );
        assert!(
            collect(
                |_| Ok(host::Chunk {
                    stdout: "x".repeat(MAX_OUTPUT + 1),
                    ..Default::default()
                }),
                || false
            )
            .is_err()
        );
    }
    #[test]
    fn denied_php_grant_is_authoritative_after_concrete_approval() {
        let host = MockHost::install().with_capabilities(&["project.read", "storage"]);
        host.add_file("artisan", "<?php");
        host.answer_next_prompt(Some(&["ok"]));
        assert!(matches!(refresh(&host.invocation()), Err(Error::Invalid(message)) if message.contains("process:php permission")));
        assert!(host.storage().get(&key("/project")).is_none());
    }
}

#[cfg(test)]
mod workflow_tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    #[test]
    fn cached_runtime_command_is_available_without_a_second_discovery_execution() {
        let host =
            MockHost::install().with_capabilities(&["project.read", "storage", "process:php"]);
        host.add_file("artisan", "<?php");
        let raw = json!({"commands":[{"name":"app:runtime","description":"Runtime only","definition":{"arguments":{},"options":{}}}]})
            .to_string();
        host.answer_next_prompt(Some(&["ok"]));
        refresh_with(&host.invocation(), |_, _| Ok(raw)).unwrap();
        host.answer_next_prompt(Some(&["app:runtime"]));
        host.answer_next_prompt(Some(&["--verbose"]));
        let state = crate::generate::Terminals::default();
        crate::generate::run_artisan(&state, &host.invocation()).unwrap();
        assert_eq!(
            state.commands()[0],
            vec!["artisan", "app:runtime", "--verbose"]
        );
        assert!(host.finish_terminal(&state.keys()[0]).is_some());
        assert!(host.process_runs().is_empty());
    }
    #[test]
    fn denied_storage_and_oversized_cache_do_not_replace_previous_state() {
        let host = MockHost::install();
        host.add_file("artisan", "<?php");
        let identity = identity(&host.invocation().project().unwrap()).unwrap();
        let valid = parse(
            &json!({"commands":[{"name":"about","definition":{"arguments":{},"options":{}}}]})
                .to_string(),
        )
        .unwrap();
        store(&identity, valid).unwrap();
        let before = host.storage()[&key("/project")].clone();
        let invalid = Value::Array(vec![
            json!({"name":"about","description":"x".repeat(MAX_CACHE),"definition":{"arguments":{},"options":{}}}),
        ]);
        assert!(store(&identity, invalid).is_err());
        assert_eq!(host.storage()[&key("/project")], before);
        assert!(parse(&json!({"commands":[{"name":"about"}]}).to_string()).is_err());
    }
}

#[cfg(test)]
mod workspace_metadata_tests {
    use super::*;
    #[test]
    fn retains_argument_order_defaults_and_repeated_flags() {
        let raw = r#"{"commands":[{"name":"app:ordered","definition":{"arguments":{"z":{"name":"z","is_required":true,"default":null},"a":{"name":"a","is_array":true,"default":["first","second"]}},"options":{"label":{"name":"--label","accept_value":true,"default":"two words"}}}}]}"#;
        let commands = parse(raw).unwrap();
        assert_eq!(commands[0]["definition"]["arguments"][0]["name"], "z");
        assert_eq!(
            commands[0]["definition"]["arguments"][1]["default"],
            json!(["first", "second"])
        );
        assert_eq!(
            commands[0]["definition"]["options"][0]["default"],
            "two words"
        );
    }
}

#[cfg(test)]
mod large_catalog_tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    #[test]
    fn large_command_catalog_survives_collection_normalization_and_cache_reload() {
        let host = MockHost::install();
        host.add_file("artisan", "<?php");
        let raw=json!({"commands":(0..270).map(|i|json!({"name":format!("app:command-{i}"),"description":"D".repeat(2500),"definition":{"arguments":{},"options":{}}})).collect::<Vec<_>>()}).to_string();
        assert!(raw.len() > 512 * 1024);
        let mut chunks = raw
            .as_bytes()
            .chunks(4096)
            .map(|c| std::str::from_utf8(c).unwrap().to_owned());
        let collected = collect(
            |_| {
                Ok(match chunks.next() {
                    Some(stdout) => host::Chunk {
                        stdout,
                        ..Default::default()
                    },
                    None => host::Chunk {
                        status: Some(0),
                        ..Default::default()
                    },
                })
            },
            || false,
        )
        .unwrap();
        let project = host.invocation().project().unwrap();
        store(&identity(&project).unwrap(), parse(&collected).unwrap()).unwrap();
        assert_eq!(cached(&project).unwrap().len(), 270);
        assert_eq!(
            snapshot(&project).unwrap()["commands"]
                .as_array()
                .unwrap()
                .len(),
            270
        );
    }
    #[test]
    #[ignore = "requires LARAVEL_ARTISAN_METADATA_FIXTURE pointing to captured Artisan list JSON"]
    fn captured_application_catalog_roundtrips() {
        let host = MockHost::install();
        host.add_file("artisan", "<?php");
        let raw =
            std::fs::read_to_string(std::env::var("LARAVEL_ARTISAN_METADATA_FIXTURE").unwrap())
                .unwrap();
        let expected = serde_json::from_str::<Value>(&raw).unwrap()["commands"]
            .as_array()
            .unwrap()
            .len();
        let project = host.invocation().project().unwrap();
        store(&identity(&project).unwrap(), parse(&raw).unwrap()).unwrap();
        assert_eq!(cached(&project).unwrap().len(), expected);
        println!(
            "accepted {} bytes / {expected} commands, persisted and reloaded",
            raw.len()
        );
    }
}

// Exact authored failures are localized only at native UI delivery boundaries.
fn ui_error(error: Error) -> Error {
    let translate = |message: String| -> String {
        match message.as_str() {
            "Project identity is unavailable or too large." => {
                crate::ui_text::t("runtime.error.ef3816319395", &message)
            }
            "The selected project has no Artisan entrypoint." => {
                crate::ui_text::t("runtime.error.d15c4899c80e", &message)
            }
            "Artisan JSON output exceeds 4 MiB; previous snapshot retained." => {
                crate::ui_text::t("runtime.error.df3051410e9c", &message)
            }
            "Artisan did not return bounded duplicate-free JSON; previous snapshot retained." => {
                crate::ui_text::t("runtime.error.288d11deeb9f", &message)
            }
            "Artisan JSON must have 1..512 commands." => {
                crate::ui_text::t("runtime.error.0b268e807e9a", &message)
            }
            "Artisan command names are invalid or duplicated." => {
                crate::ui_text::t("runtime.error.c3d0ecd71b48", &message)
            }
            "Artisan JSON has no supported argument/option definition object." => {
                crate::ui_text::t("runtime.error.8ac8c9bf6d28", &message)
            }
            "Artisan JSON argument/option definitions are missing." => {
                crate::ui_text::t("runtime.error.f2c83718369c", &message)
            }
            "Artisan argument/option definitions must be objects or arrays." => {
                crate::ui_text::t("runtime.error.9808d4acb93b", &message)
            }
            "Too many Artisan arguments/options." => {
                crate::ui_text::t("runtime.error.05aaf0767912", &message)
            }
            "Artisan argument/option names are missing or duplicated." => {
                crate::ui_text::t("runtime.error.f7c1998ef913", &message)
            }
            "Artisan argument flags must be boolean." => {
                crate::ui_text::t("runtime.error.756be86edf66", &message)
            }
            "Unsupported or oversized Artisan default." => {
                crate::ui_text::t("runtime.error.3e03c02ef829", &message)
            }
            "No project identity." => crate::ui_text::t("runtime.error.d1a7af06fbee", &message),
            "Artisan cache exceeds 5 MiB; previous snapshot retained." => {
                crate::ui_text::t("runtime.error.50ef3dcacf12", &message)
            }
            "Artisan metadata discovery exceeded four seconds; previous snapshot retained." => {
                crate::ui_text::t("runtime.error.6bf8246398ac", &message)
            }
            "Artisan metadata exceeded its four-second polling budget; previous snapshot retained." => {
                crate::ui_text::t("runtime.error.6662b5e52e5e", &message)
            }
            "Artisan metadata discovery exhausted its four-second polling budget; previous snapshot retained." => {
                crate::ui_text::t("runtime.error.7e2e24289455", &message)
            }
            _ => crate::ui_text::owned_error(&message, UI_ERROR_TEMPLATES).unwrap_or(message),
        }
    };
    match error {
        Error::Invalid(message) => Error::Invalid(translate(message)),
        Error::Failed(message) => Error::Failed(translate(message)),
        other => other,
    }
}

// Explicit formats emitted by this module; only UI delivery uses locale state.
const UI_ERROR_TEMPLATES: &[(&str, &str)] = &[
    (
        "final.artisan_metadata.identityBound",
        "{name} exceeds the discovery identity bound.",
    ),
    (
        "final.artisan_metadata.textBound",
        "Artisan {key} exceeds text limits.",
    ),
    (
        "final.artisan_metadata.outputBound",
        "Artisan output exceeds {limit} KiB; previous snapshot retained.",
    ),
    (
        "final.artisan_metadata.processFailed",
        "Artisan list exited {status}; previous snapshot retained. {output}",
    ),
];
