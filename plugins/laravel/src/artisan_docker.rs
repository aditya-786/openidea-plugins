//! Opt-in local bind-mounted container metadata; never feeds the host PHP runner.
use crate::{artisan_metadata as metadata, hover::canonical};
use openidea_plugin::{Error, Invocation, NotificationLevel, PickItem, Project, host};
use serde_json::{Value, json};

const MAX_CACHE: usize = 5 * 1024 * 1024;
const MAX_OUTPUT: usize = 4 * 1024 * 1024;
const DDEV_WORKDIR: &str = "/var/www/html";
const FORMAT: &str = r#"{"Id":{{json .Id}},"Image":{{json .Image}},"Running":{{json .State.Running}},"Paused":{{json .State.Paused}},"Restarting":{{json .State.Restarting}},"StartedAt":{{json .State.StartedAt}},"Mounts":{{json .Mounts}},"DdevRoot":{{json (index .Config.Labels "com.ddev.approot")}}}"#;
#[path = "docker_discovery.rs"]
mod discovery;
pub(super) use discovery::{clear_discovered_target, discover};

const ARTISAN: [&str; 5] = [
    "artisan",
    "list",
    "--format=json",
    "--no-interaction",
    "--no-ansi",
];

#[derive(Clone, Debug, PartialEq, Eq)]
struct Target {
    socket: String,
    container: String,
    workdir: String,
}
impl Target {
    fn validate(socket: &str, container: &str, workdir: &str) -> Result<Self, Error> {
        let socket_path = socket.strip_prefix("unix://").unwrap_or_default();
        if socket.len() > 1024
            || !canonical(socket_path)
            || socket.chars().any(char::is_control)
            || container.len() != 64
            || !container
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || workdir.len() > 1024
            || !canonical(workdir)
            || workdir.chars().any(char::is_control)
        {
            return Err(Error::Invalid("Configure a local unix:///absolute/socket, full lowercase64hex container ID and normalized absolute container project directory in Laravel Docker metadata settings.".into()));
        }
        Ok(Self {
            socket: socket.into(),
            container: container.into(),
            workdir: workdir.into(),
        })
    }
    fn settings() -> Result<Self, Error> {
        Self::validate(
            &host::setting_string("artisanDockerSocket")
                .unwrap_or_else(|| "unix:///var/run/docker.sock".into()),
            &host::setting_string("artisanDockerContainer").unwrap_or_default(),
            &host::setting_string("artisanDockerWorkdir").unwrap_or_default(),
        )
    }
    fn for_project(project: &Project) -> Result<Self, Error> {
        discovery::selected(project)?.map_or_else(Self::settings, Ok)
    }
    fn from_identity(identity: &Value) -> Result<Self, Error> {
        Self::validate(
            identity["socket"].as_str().unwrap_or_default(),
            identity["container"].as_str().unwrap_or_default(),
            identity["workdir"].as_str().unwrap_or_default(),
        )
    }
    fn inspect(&self) -> Vec<String> {
        [
            "--host",
            &self.socket,
            "inspect",
            "--type",
            "container",
            "--format",
            FORMAT,
            &self.container,
        ]
        .map(str::to_owned)
        .to_vec()
    }
    fn exec(&self) -> Vec<String> {
        [
            "--host",
            &self.socket,
            "exec",
            "--workdir",
            &self.workdir,
            &self.container,
            "php",
        ]
        .into_iter()
        .chain(ARTISAN)
        .map(str::to_owned)
        .collect()
    }
}
fn key(root: &str) -> String {
    format!("artisan-docker-metadata-v1:{}", metadata::hash(root))
}
fn identity(project: &Project) -> Result<Value, Error> {
    let mut identity = metadata::identity(project)?;
    let root = identity["root"].as_str().ok_or(Error::Stale)?;
    if !canonical(root)
        || !host::projects()?
            .iter()
            .any(|p| p.root().is_ok_and(|r| r == root))
    {
        return Err(Error::Stale);
    }
    let target = Target::for_project(project)?;
    identity["runner"] = json!("docker-local-bind");
    identity["socket"] = json!(target.socket);
    identity["container"] = json!(target.container);
    identity["workdir"] = json!(target.workdir);
    identity["argv"] = json!(target.exec());
    Ok(identity)
}
fn inspect(raw: &str, identity: &Value) -> Result<Value, Error> {
    let target = Target::from_identity(identity)?;
    let value = crate::strict_json::parse(raw, MAX_OUTPUT)?;
    let root = identity["root"].as_str().ok_or(Error::Stale)?;
    if !canonical(root)
        || value["Id"] != target.container
        || value["Running"] != true
        || value["Paused"] != false
        || value["Restarting"] != false
    {
        return Err(Error::Invalid(
            "Docker target is not the exact configured running, unpaused container.".into(),
        ));
    }
    let started = value["StartedAt"]
        .as_str()
        .filter(|v| !v.is_empty() && v.len() <= 128 && !v.chars().any(char::is_control))
        .ok_or("Docker start identity is unavailable.")?;
    let image = value["Image"]
        .as_str()
        .filter(|v| {
            v.strip_prefix("sha256:").is_some_and(|digest| {
                digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit())
            })
        })
        .ok_or("Docker image identity is unavailable.")?;
    let mounts = value["Mounts"]
        .as_array()
        .filter(|v| v.len() <= 128)
        .ok_or("Docker mount inventory is unavailable or too large.")?;
    // Docker applies the most-specific covering mount. A parent-volume mount
    // does not shadow a deeper project bind; compatible nested binds preserve
    // the same host/project mapping and are safe to accept.
    let mut covering: Vec<(&str, &str, &str)> = Vec::new();
    let mut descendants: Vec<(&str, &str, &str)> = Vec::new();
    for mount in mounts {
        let destination = mount["Destination"]
            .as_str()
            .filter(|p| discovery::absolute(p) && p.len() <= 4096)
            .ok_or("Docker mount destination is not normalized.")?;
        let source = mount["Source"]
            .as_str()
            .filter(|s| s.len() <= 4096 && !s.chars().any(char::is_control))
            .ok_or("Docker mount source is unavailable.")?;
        let kind = mount["Type"]
            .as_str()
            .ok_or("Docker mount type is unavailable.")?;
        if destination == target.workdir
            || destination == "/"
            || crate::under(&target.workdir, destination)
        {
            covering.push((destination, source, kind));
        } else if crate::under(destination, &target.workdir) {
            descendants.push((destination, source, kind));
        }
    }
    covering.sort_by_key(|(destination, _, _)| std::cmp::Reverse(destination.len()));
    let Some(&(destination, source, kind)) = covering.first() else {
        return Err(Error::Invalid(
            "Docker target has no verifiable bind mapping for this project directory.".into(),
        ));
    };
    let bind_proof = kind == "bind"
        && discovery::absolute(source)
        && discovery::map_path(&target.workdir, destination, source).as_deref() == Some(root);
    let ddev_volume = kind == "volume"
        && target.workdir == DDEV_WORKDIR
        && value["DdevRoot"].as_str() == Some(root);
    if covering
        .get(1)
        .is_some_and(|(other, _, _)| *other == destination)
        || !(bind_proof || ddev_volume)
    {
        return Err(Error::Invalid("Docker project directory must map exactly to this project through its most-specific bind mount, or be a DDEV project (com.ddev.approot) on its synced project volume.".into()));
    }
    let mut destinations = std::collections::BTreeSet::new();
    for (destination, source, kind) in descendants {
        if !destinations.insert(destination)
            || kind != "bind"
            || !discovery::absolute(source)
            || discovery::map_path(destination, &target.workdir, root).as_deref() != Some(source)
        {
            return Err(Error::Invalid(
                "Docker project directory has an unverified or foreign shadowing mount.".into(),
            ));
        }
    }
    // Docker lists mounts in a different order on each inspect, so the proof sorts them.
    // Persist only identity and mount proof. Environment/secrets are never requested by FORMAT.
    let mut sorted_mounts = mounts.clone();
    sorted_mounts.sort_by_key(|mount| {
        (
            mount["Destination"].as_str().unwrap_or_default().to_owned(),
            mount["Source"].as_str().unwrap_or_default().to_owned(),
            mount["Type"].as_str().unwrap_or_default().to_owned(),
        )
    });
    let mut proof = json!({"Id":target.container,"Image":image,"Running":true,"Paused":false,"Restarting":false,"StartedAt":started,"Mounts":sorted_mounts});
    if ddev_volume {
        proof["DdevRoot"] = json!(root);
    }
    Ok(proof)
}
fn load(root: &str) -> Result<Option<Value>, Error> {
    let Some(raw) = host::storage::get(&key(root))? else {
        return Ok(None);
    };
    let value = crate::strict_json::parse(&raw, MAX_CACHE)?;
    if value["schema"] != 1
        || value["identity"]["root"] != root
        || value["identity"]["runner"] != "docker-local-bind"
        || value["capturedAt"].as_u64().is_none()
    {
        return Err(Error::Invalid(
            crate::ui_text::text("native.artisan_docker.59070c8145", "Existing Docker metadata cache has an unsupported or foreign owner; retained.").into(),
        ));
    }
    let target = Target::from_identity(&value["identity"])?;
    if value["identity"]["argv"] != json!(target.exec()) {
        return Err(Error::Invalid(
            crate::ui_text::text("native.artisan_docker.68f8f959be", "Docker cache argv owner is invalid; retained.").into(),
        ));
    }
    inspect(&value["proof"].to_string(), &value["identity"])?;
    metadata::parse(&json!({"commands":value["commands"]}).to_string())?;
    Ok(Some(value))
}
fn run(project: &Project, args: &[String]) -> Result<String, Error> {
    let task = host::begin_task(crate::ui_text::text("native.artisan_docker.e4cbafe8e6", "Inspect/discover Docker Artisan metadata"), true);
    let child = host::start(&host::Command::new("docker").args(args), Some(project)).map_err(|error| ui_kit::denied::map(error, "Running Artisan in Docker", "process:docker", "Laravel"))?;
    if let Err(error) = child.close_stdin() {
        let _ = child.kill();
        return Err(error);
    }
    let result = metadata::collect_limited(
        MAX_OUTPUT,
        16,
        |timeout| child.wait(timeout),
        || task.is_cancelled(),
    );
    if result.is_err() {
        let _ = child.kill();
    }
    result
}
fn stale_reason(reason: &str) -> Error {
    host::notify(
        NotificationLevel::Warning,
        "Laravel",
        &crate::ui_text::format("native.artisan_docker.70ad98e84b", "Docker metadata is out of date: {reason}.", &[("reason", reason)]),
    );
    Error::Stale
}
fn refresh_with(
    ctx: &Invocation,
    mut run: impl FnMut(&Project, &[String]) -> Result<String, Error>,
) -> Result<Option<String>, Error> {
    let project = ctx
        .project()
        .ok_or(crate::ui_text::text("native.artisan_docker.7b35695466", "Open the owning Laravel project first."))?;
    let before = identity(&project)?;
    let root = before["root"].as_str().ok_or(Error::Stale)?;
    let baseline = host::storage::get(&key(root))?;
    let _ = load(root)?; // Also requires storage before any runtime execution.
    let target = Target::from_identity(&before)?;
    let preview = crate::ui_text::format("native.dockerRoute.329d8b1654","This executes project/container-controlled PHP bootstrap code, even though only list metadata is requested. It can change project files, databases or container state.\n\nHost project / CLI working directory: {root}\nRunner: docker resolved from host PATH (process:docker required)\nPinned local endpoint: {socket}\nFull container ID: {container}\nContainer PHP working directory: {workdir}\n\nInspect argv (before and after execution): {inspectArgv}\nMetadata exec argv: {execArgv}\n\nInspection must prove one running unpaused container with an a verified project bind mapping and no foreign shadowing project mounts. No shell, container creation/start or application command selection is used. Each process is limited to 4MiB combined output and four seconds of polling. Killing the Docker client cannot guarantee stopping an already started container PHP process. The previous accepted cache survives failure/cancellation. Local PHP metadata stays separate. Docker workspace runs use this exact target and their own captured command definitions.",&[("root",&(root).to_string()),("socket",&(target.socket).to_string()),("container",&(target.container).to_string()),("workdir",&(target.workdir).to_string()),("inspectArgv",&(json!(target.inspect())).to_string()),("execArgv",&(json!(target.exec())).to_string())]);
    if !ui_kit::prompts::confirm_exact(
        crate::ui_text::text("native.dockerRoute.250e27e4d9","Execute container PHP to discover Docker Artisan metadata?"),
        &preview,
    )? {
        return Ok(None);
    }
    let current = || -> Result<bool, Error> {
        Ok(
            ctx.project().and_then(|p| p.root().ok()).as_deref() == Some(root)
                && identity(&project)? == before
                && host::storage::get(&key(root))? == baseline,
        )
    };
    if !current()? {
        return Err(stale_reason(crate::ui_text::text("native.artisan_docker.6ebc5c3664", "the project, Docker target or saved metadata changed before the refresh started")));
    }
    let proof = inspect(&run(&project, &target.inspect())?, &before)?;
    if !current()? {
        return Err(stale_reason(crate::ui_text::text("native.artisan_docker.5c2c4d8dac", "the project, Docker target or saved metadata changed while the container was inspected")));
    }
    let output = run(&project, &target.exec())?;
    let commands = metadata::parse(&output)?;
    if !current()? {
        return Err(stale_reason(crate::ui_text::text("native.artisan_docker.90b1c1210d", "the project, Docker target or saved metadata changed while php artisan list ran")));
    }
    let after = inspect(&run(&project, &target.inspect())?, &before)?;
    if proof != after {
        return Err(stale_reason(crate::ui_text::text("native.artisan_docker.b491b31cd1", "the container was restarted or its image or mounts changed while php artisan list ran")));
    }
    if !current()? {
        return Err(stale_reason(crate::ui_text::text("native.artisan_docker.5d348134a9", "the project, Docker target or saved metadata changed after php artisan list ran")));
    }
    let _ = load(root)?;
    let captured = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let value = json!({"schema":1,"identity":before,"capturedAt":captured,"proof":proof,"commands":commands});
    let raw = value.to_string();
    if raw.len() > MAX_CACHE {
        return Err(Error::Invalid(
            crate::ui_text::text("native.artisan_docker.6ee56051a8", "Docker metadata envelope exceeds 5MiB; previous snapshot retained.").into(),
        ));
    }
    host::storage::set(&key(root), &raw)?;
    host::notify(
        NotificationLevel::Info,
        "Laravel",
        crate::ui_text::text("native.artisan_docker.4d0b76ba3b", "Saved executed Docker Artisan metadata. Select Docker in Artisan to configure and run these commands. Local PHP metadata remains separate."),
    );
    Ok(None)
}
/// Current selected/configured target label; no Docker invocation or metadata refresh.
pub(super) fn target_label(project:&Project)->Result<Option<String>,Error>{target_label_inner(project).map_err(ui_error)}
fn target_label_inner(project: &Project) -> Result<Option<String>, Error> {
    let target = if let Some(target) = discovery::selected(project)? {
        target
    } else {
        let container = host::setting_string("artisanDockerContainer").unwrap_or_default();
        let workdir = host::setting_string("artisanDockerWorkdir").unwrap_or_default();
        if container.is_empty() && workdir.is_empty() {
            return Ok(None);
        }
        Target::settings()?
    };
    Ok(Some(format!(
        "{}… · {}",
        &target.container[..12],
        target.workdir
    )))
}

/// Current cached Docker definitions; reading never inspects or executes a container.
pub(super) fn snapshot(project:&Project)->Result<Value,Error>{snapshot_inner(project).map_err(ui_error)}
fn snapshot_inner(project: &Project) -> Result<Value, Error> {
    let owner = identity(project)?;
    load(owner["root"].as_str().ok_or(Error::Stale)?)?
        .filter(|value| value["identity"] == owner)
        .ok_or_else(|| {
            Error::Invalid(
                crate::ui_text::text("native.artisan_docker.c5db28ea55", "Refresh Docker commands for this project and configured target first.").into(),
            )
        })
}
pub(super) fn argv(
    project: &Project,
    artisan: &[String],
    terminal: bool,
) -> Result<Vec<String>, Error> {
    let snapshot = snapshot(project)?;
    let target = Target::from_identity(&snapshot["identity"])?;
    let mut args = vec!["--host".into(), target.socket, "exec".into()];
    if terminal {
        args.push("-it".into());
    }
    args.extend([
        "--workdir".into(),
        target.workdir,
        target.container,
        "php".into(),
    ]);
    args.extend_from_slice(artisan);
    Ok(args)
}
/// Proves the configured target is still the same running container and bind mount.
pub(super) fn verify(project: &Project) -> Result<(), Error> {
    verify_with(project, run).map_err(ui_error)
}
fn verify_with(
    project: &Project,
    mut run: impl FnMut(&Project, &[String]) -> Result<String, Error>,
) -> Result<(), Error> {
    let before = snapshot(project)?;
    let target = Target::from_identity(&before["identity"])?;
    let proof = inspect(&run(project, &target.inspect())?, &before["identity"])?;
    let saved = inspect(&before["proof"].to_string(), &before["identity"])?;
    if proof != saved {
        return Err(stale_reason(crate::ui_text::text("native.artisan_docker.d6e64708d2", "the container's start time, image or mounts differ from the saved Docker metadata; refresh the Docker commands again")));
    }
    if snapshot(project)? != before {
        return Err(stale_reason(crate::ui_text::text("native.artisan_docker.d92d98511b", "the saved Docker metadata changed while the container was verified")));
    }
    Ok(())
}

pub fn refresh(ctx: &Invocation) -> Result<Option<String>, Error> {
    refresh_with(ctx, run).map_err(ui_error)
}
pub fn browse(ctx:&Invocation)->Result<Option<String>,Error>{browse_inner(ctx).map_err(ui_error)}
fn browse_inner(ctx: &Invocation) -> Result<Option<String>, Error> {
    let project = ctx
        .project()
        .ok_or(crate::ui_text::text("native.artisan_docker.7b35695466", "Open the owning Laravel project first."))?;
    let owner = identity(&project)?;
    let value = load(owner["root"].as_str().ok_or(Error::Stale)?)?.filter(|v| v["identity"] == owner)
        .ok_or(crate::ui_text::text("native.artisan_docker.noSavedCommands", "No Artisan commands are saved for this Docker container yet. Run Laravel: Refresh Docker Artisan Commands first."))?;
    let commands = metadata::parse(&json!({"commands":value["commands"]}).to_string())?;
    let commands = commands.as_array().ok_or(crate::ui_text::text("native.artisan_docker.92654a4c9d", "Unsupported cached commands."))?;
    let choices: Vec<PickItem> = commands
        .iter()
        .map(|command| {
            let name = command["name"].as_str().unwrap_or_default();
            let description: String = command["description"]
                .as_str()
                .unwrap_or_default()
                .chars()
                .take(512)
                .collect();
            PickItem::new(name, name).description(&crate::ui_text::format("native.dockerRoute.c524db5420","Executed Docker snapshot; captured Unix {captured} (may be stale). {description}",&[("captured",&(value["capturedAt"]).to_string()),("description",&(description).to_string())]))
        })
        .collect();
    let Some(selected) = host::pick(crate::ui_text::text("native.artisan_docker.50816ef566", "Cached Docker Artisan commands — read-only"), &choices)?
    else {
        return Ok(None);
    };
    let command = commands
        .iter()
        .find(|c| c["name"] == selected)
        .ok_or(crate::ui_text::text("native.artisan_docker.be26dfdf4e", "Selected Docker command is stale or unknown."))?;
    if ctx.project().and_then(|p| p.root().ok()).as_deref() != owner["root"].as_str()
        || identity(&project)? != owner
    {
        return Err(Error::Stale);
    }
    let mut details = Vec::new();
    for kind in ["arguments", "options"] {
        for (i, item) in command["definition"][kind]
            .as_array()
            .ok_or(crate::ui_text::text("native.artisan_docker.be9f5ce189", "Unsupported cached definition."))?
            .iter()
            .enumerate()
        {
            let name = item["name"].as_str().unwrap_or_default();
            let flags: Vec<String> = [
                ("is_required", crate::ui_text::text("native.dockerRoute.1a77d41622","required")),
                ("is_array", crate::ui_text::text("native.dockerRoute.19edc12107","array")),
                ("accept_value", crate::ui_text::text("native.dockerRoute.13907317bf","accepts value")),
                ("is_value_required", crate::ui_text::text("native.dockerRoute.e91b1464a8","value required")),
                ("is_multiple", crate::ui_text::text("native.dockerRoute.88fc77e78f","multiple")),
            ]
            .into_iter()
            .filter_map(|(key, label)| {
                item[key]
                    .as_bool()
                    .map(|value| format!("{label}: {}", if value { crate::ui_text::text("native.dockerRoute.fb360f9c09","yes") } else { crate::ui_text::text("native.dockerRoute.fd12863535","no") }))
            })
            .collect();
            let description: String = item["description"]
                .as_str()
                .unwrap_or_default()
                .chars()
                .take(512)
                .collect();
            details.push(
                PickItem::new(&format!("{kind}:{i}"), &crate::ui_text::format("native.dockerRoute.b6f3838a93","{kind}: {name}",&[("kind",&(if kind=="arguments"{crate::ui_text::text("native.dockerRoute.536f158f6d","arguments")}else{crate::ui_text::text("native.dockerRoute.513f8de925","options")}).to_string()),("name",&(name).to_string())])).description(
                    &format!(
                        "{description}; {}",
                        if flags.is_empty() {
                            crate::ui_text::text("native.dockerRoute.25ab82bb0e","No captured argument flags").into()
                        } else {
                            flags.join("; ")
                        }
                    ),
                ),
            );
        }
    }
    if details.is_empty() {
        details.push(PickItem::new(
            "none",
            crate::ui_text::text("native.artisan_docker.da564b5fc4", "No declared arguments or options in this captured snapshot"),
        ));
    }
    let _ = host::pick(
        crate::ui_text::text("native.artisan_docker.b25995fb3c", "Cached declarations — read-only; no command will execute"),
        &details,
    )?;
    Ok(None)
}


// Exact authored messages are localized only after validation reaches a UI caller.
pub(super) fn ui_error(error:Error)->Error {
    fn message(message:String)->String{match message.as_str(){
        "Configure a local unix:///absolute/socket, full lowercase64hex container ID and normalized absolute container project directory in Laravel Docker metadata settings." => crate::ui_text::t("native.artisan_docker.2233a75456",&message),
        "Docker discovery exceeds 128 running containers; configure a target manually." => crate::ui_text::t("native.dockerRoute.dc0179d8a4",&message),
        "Docker discovery output exceeds 4 MiB." => crate::ui_text::t("native.dockerRoute.0074eb8175",&message),
        "Docker discovery returned an invalid or duplicate full container ID." => crate::ui_text::t("native.dockerRoute.c77c646d14",&message),
        "Docker image identity is unavailable." => crate::ui_text::t("native.artisan_docker.6c40883c1a",&message),
        "Docker inspection output exceeds 4 MiB." => crate::ui_text::t("native.dockerRoute.f8e9810cb0",&message),
        "Docker mount destination is not normalized." => crate::ui_text::t("native.artisan_docker.69dba2d9a4",&message),
        "Docker mount inventory is unavailable or too large." => crate::ui_text::t("native.artisan_docker.e7ed054e46",&message),
        "Docker mount source is unavailable." => crate::ui_text::t("native.artisan_docker.aacdd913eb",&message),
        "Docker mount type is unavailable." => crate::ui_text::t("native.artisan_docker.92c8419b6f",&message),
        "Docker project directory has an unverified or foreign shadowing mount." => crate::ui_text::t("native.artisan_docker.e7c08659a7",&message),
        "Docker project directory must map exactly to this project through its most-specific bind mount, or be a DDEV project (com.ddev.approot) on its synced project volume." => crate::ui_text::t("native.artisan_docker.f2508eb86c",&message),
        "Docker start identity is unavailable." => crate::ui_text::t("native.artisan_docker.a155ecb69d",&message),
        "Docker target has no verifiable bind mapping for this project directory." => crate::ui_text::t("native.artisan_docker.1d49b5d402",&message),
        "Docker target is not the exact configured running, unpaused container." => crate::ui_text::t("native.artisan_docker.b71ac18ab8",&message),
        "Saved Docker target has a foreign or unsupported owner." => crate::ui_text::t("native.dockerRoute.f21c24e157",&message),
        _=>message,
    }}
    match error{Error::Invalid(value)=>Error::Invalid(message(value)),Error::Failed(value)=>Error::Failed(message(value)),_=>error}
}
#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    const ID: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
    fn setup() -> MockHost {
        let host = MockHost::install();
        host.add_file("artisan", "<?php require 'bootstrap/app.php';");
        host.add_file("composer.json", "{}");
        host.set_setting("artisanDockerContainer", &json!(ID).to_string());
        host.set_setting("artisanDockerWorkdir", "\"/workspace\"");
        host
    }
    fn proof() -> Value {
        json!({"Id":ID,"Image":format!("sha256:{}", "a".repeat(64)),"Running":true,"Paused":false,"Restarting":false,"StartedAt":"2026-11-09T12:00:00Z","Mounts":[{"Type":"bind","Source":"/project","Destination":"/workspace"}]})
    }
    #[test]
    fn snapshot_saved_with_unsorted_mounts_still_verifies() {
        let host = setup();
        let project = host::projects().unwrap().remove(0);
        let mut saved = proof();
        saved["Mounts"] = json!([
            {"Type":"volume","Source":"/var/lib/docker/volumes/z/_data","Destination":"/zcache"},
            {"Type":"bind","Source":"/project","Destination":"/workspace"}
        ]);
        let commands = metadata::parse(&sample()).unwrap();
        let envelope = json!({"schema":1,"identity":identity(&project).unwrap(),"capturedAt":1,"proof":saved.clone(),"commands":commands});
        host::storage::set(&key("/project"), &envelope.to_string()).unwrap();
        let mut reordered = saved;
        reordered["Mounts"].as_array_mut().unwrap().reverse();
        assert_eq!(verify_with(&project, |_, _| Ok(reordered.to_string())), Ok(()));
    }
    #[test]
    fn mount_order_from_docker_does_not_change_the_proof() {
        let _host = setup();
        let identity = json!({"root":"/project","socket":"unix:///var/run/docker.sock","container":ID,"workdir":"/workspace"});
        let mut first = proof();
        first["Mounts"] = json!([
            {"Type":"volume","Source":"/var/lib/docker/volumes/cache/_data","Destination":"/cache"},
            {"Type":"bind","Source":"/project","Destination":"/workspace"}
        ]);
        let mut second = first.clone();
        second["Mounts"].as_array_mut().unwrap().reverse();
        assert_eq!(
            inspect(&first.to_string(), &identity).unwrap(),
            inspect(&second.to_string(), &identity).unwrap()
        );
    }
    fn sample() -> String {
        json!({"commands":[{"name":"app:container","description":"Container command","definition":{"arguments":{"input":{"name":"input","description":"Input name","is_required":true}},"options":{"force":{"name":"--force","description":"Force","accept_value":false}}}}]}).to_string()
    }
    fn seed(host: &MockHost) {
        host.answer_next_prompt(Some(&["ok"]));
        let mut calls = 0;
        refresh_with(&host.invocation(), |project, args| {
            assert_eq!(project.root().unwrap(), "/project");
            let target = Target::settings().unwrap();
            let expected = if calls == 1 {
                target.exec()
            } else {
                target.inspect()
            };
            assert_eq!(args, expected);
            assert_eq!(&args[..2], &["--host", "unix:///var/run/docker.sock"]);
            assert!(!args.iter().any(|a| {
                [
                    "sh",
                    "-c",
                    "--context",
                    "-i",
                    "-t",
                    "--detach",
                    "--privileged",
                    "run",
                    "start",
                ]
                .contains(&a.as_str())
            }));
            calls += 1;
            Ok(if calls == 2 {
                sample()
            } else {
                proof().to_string()
            })
        })
        .unwrap();
        assert_eq!(calls, 3);
    }
    #[test]
    fn docker_workspace_previews_exact_argv_and_rechecks_container_identity() {
        let host = setup();
        seed(&host);
        let project = host.invocation().project().unwrap();
        let mut state = crate::artisan_workspace::State::default();
        let terminals = crate::generate::Terminals::default();
        let reply = state
            .handle(
                &host.invocation(),
                r#"{"op":"runner","runner":"docker"}"#,
                &terminals,
            )
            .unwrap();
        assert_eq!(reply["runner"], "docker");
        assert_eq!(reply["commands"][0]["name"], "app:container");
        let preview=state.handle(&host.invocation(),&json!({"op":"preview","command":"app:container","token":reply["token"],"values":{"input":"a b; $(echo no)","--force":true},"revision":1}).to_string(),&terminals).unwrap();
        let args = preview["argv"].as_array().unwrap();
        assert_eq!(args[0], "--host");
        assert_eq!(args[2], "exec");
        assert_eq!(args.last().unwrap(), "a b; $(echo no)");
        assert!(!args.iter().any(|arg| arg == "sh"));
        verify_with(&project, |_, args| {
            assert_eq!(args, Target::settings().unwrap().inspect());
            Ok(proof().to_string())
        })
        .unwrap();
        assert_eq!(
            verify_with(&project, |_, _| {
                let mut restarted = proof();
                restarted["StartedAt"] = json!("changed");
                Ok(restarted.to_string())
            }),
            Err(Error::Stale)
        );
        let terminal = argv(&project, &["artisan".into(), "app:container".into()], true).unwrap();
        assert_eq!(terminal[3], "-it");
        assert!(
            state
                .open_generator(&host.invocation(), Some("make:model"))
                .is_err(),
            "Docker selection must never fall back to a local generator"
        );
        assert!(host.process_runs().is_empty());
    }
    #[test]
    fn approved_exact_local_bind_runner_caches_separately_and_browses_without_execution() {
        let host = setup();
        let project = host.invocation().project().unwrap();
        let php_owner = metadata::identity(&project).unwrap();
        let php_key = format!("artisan-metadata-v2:{}", metadata::hash("/project"));
        let php = json!({"schema":2,"identity":php_owner,"capturedAt":12,"commands":metadata::parse(&sample()).unwrap()}).to_string();
        host::storage::set(&php_key, &php).unwrap();
        seed(&host);
        assert_eq!(host.storage()[&php_key], php);
        assert_eq!(
            metadata::cached(&project).unwrap()[0].0,
            "app:container",
            "PHP cache remains its independently seeded snapshot"
        );
        let cache = load("/project").unwrap().unwrap();
        assert_eq!(cache["identity"]["runner"], "docker-local-bind");
        assert_eq!(cache["proof"]["Id"], ID);
        assert!(cache["proof"].get("Env").is_none());
        host.answer_next_prompt(Some(&["app:container"]));
        host.answer_next_prompt(Some(&["options:0"]));
        browse(&host.invocation()).unwrap();
        assert!(
            host.process_runs().is_empty()
                && host.terminals().is_empty()
                && host.requests().is_empty()
        );
    }
    #[test]
    fn missing_configuration_cancel_denied_storage_and_project_never_execute() {
        let host = setup();
        refresh_with(&host.invocation(), |_, _| {
            panic!("cancelled review must not inspect or exec")
        })
        .unwrap();
        host.set_setting("artisanDockerContainer", "\"short-id\"");
        assert!(
            refresh_with(&host.invocation(), |_, _| panic!(
                "invalid settings must not run"
            ))
            .is_err()
        );
        host.set_project_root(None);
        assert!(
            refresh_with(&host.invocation(), |_, _| panic!("no project must not run")).is_err()
        );
        let host = setup().with_capabilities(&["project.read"]);
        assert!(matches!(
            refresh_with(&host.invocation(), |_, _| panic!("storage denied")),
            Err(Error::Denied(_))
        ));
        assert!(host.process_runs().is_empty());
    }
    #[test]
    fn target_validation_rejects_remote_shell_relative_and_short_identity_inputs() {
        let _ui_host=openidea_plugin::mock::MockHost::install();
        for (socket, id, dir) in [
            ("tcp://daemon:2375", ID, "/workspace"),
            ("ssh://remote", ID, "/workspace"),
            ("unix:///var/../run/docker.sock", ID, "/workspace"),
            ("unix:///var/run/docker.sock", "123abc", "/workspace"),
            ("unix:///var/run/docker.sock", ID, "/workspace/../other"),
            ("unix:///var/run/docker.sock", ID, "relative"),
            ("unix:///var/run/docker.sock", ID, "/workspace\nother"),
        ] {
            assert!(
                Target::validate(socket, id, dir).is_err(),
                "{socket} {id} {dir}"
            );
        }
    }
    #[test]
    fn fresh_inspect_rejects_wrong_stopped_paused_or_shadowed_target_before_php() {
        for change in 0..9 {
            let host = setup();
            let mut candidate = proof();
            match change {
                0 => candidate["Id"] = json!("b".repeat(64)),
                1 => candidate["Running"] = json!(false),
                2 => candidate["Paused"] = json!(true),
                3 => candidate["Restarting"] = json!(true),
                4 => candidate["Mounts"][0]["Source"] = json!("/other"),
                5 => candidate["Mounts"][0]["Type"] = json!("volume"),
                6 => candidate["Mounts"].as_array_mut().unwrap().push(json!({"Type":"volume","Source":"/var/lib/volumes/x","Destination":"/workspace/vendor"})),
                7 => candidate["Mounts"].as_array_mut().unwrap().push(json!({"Type":"bind","Source":"/project","Destination":"/workspace"})),
                _ => candidate["Mounts"] = json!([]),
            }
            host.answer_next_prompt(Some(&["ok"]));
            let mut calls = 0;
            assert!(
                refresh_with(&host.invocation(), |_, args| {
                    calls += 1;
                    assert_eq!(args, Target::settings().unwrap().inspect());
                    Ok(candidate.to_string())
                })
                .is_err()
            );
            assert_eq!(calls, 1, "{change}");
            assert!(host.storage().is_empty());
        }
    }
    #[test]
    fn source_or_configuration_change_after_inspect_prevents_exec() {
        for source in [true, false] {
            let host = setup();
            host.answer_next_prompt(Some(&["ok"]));
            let mut calls = 0;
            assert_eq!(
                refresh_with(&host.invocation(), |_, _| {
                    calls += 1;
                    if source {
                        host.add_file("artisan", "<?php changed");
                    } else {
                        host.set_setting("artisanDockerWorkdir", "\"/changed\"");
                    }
                    Ok(proof().to_string())
                }),
                Err(Error::Stale)
            );
            assert_eq!(calls, 1);
            assert!(host.storage().is_empty());
        }
    }
    #[test]
    fn failed_cancelled_malformed_or_restarted_capture_retains_previous_cache() {
        for mode in 0..6 {
            let host = setup();
            seed(&host);
            let before = host.storage()[&key("/project")].clone();
            host.answer_next_prompt(Some(&["ok"]));
            let mut calls = 0;
            assert!(
                refresh_with(&host.invocation(), |_, _| {
                    calls += 1;
                    if calls == 2 {
                        return match mode {
                            0 => Err(Error::Cancelled),
                            1 => Err(Error::Failed("container execution failed".into())),
                            2 => Ok("not JSON".into()),
                            3 => Ok("x".repeat(MAX_OUTPUT + 1)),
                            _ => Ok(sample()),
                        };
                    }
                    let mut value = proof();
                    if calls == 3 {
                        if mode == 4 {
                            value["StartedAt"] = json!("2026-11-09T13:00:00Z");
                        } else {
                            host.add_file("composer.json", "{\"changed\":true}");
                        }
                    }
                    Ok(value.to_string())
                })
                .is_err()
            );
            assert_eq!(host.storage()[&key("/project")], before, "{mode}");
        }
    }
    #[test]
    fn malformed_or_foreign_envelopes_are_retained_and_block_runtime_work() {
        for raw in ["not JSON".to_owned(), "x".repeat(MAX_CACHE+1), json!({"schema":1,"identity":{"root":"/foreign","runner":"docker-local-bind"},"capturedAt":1}).to_string()] {
            let host = setup();
            host::storage::set(&key("/project"), &raw).unwrap();
            host.answer_next_prompt(Some(&["ok"]));
            assert!(refresh_with(&host.invocation(), |_, _| panic!("invalid cache must not run Docker")).is_err());
            assert_eq!(host.storage()[&key("/project")],raw);
        }
    }
    #[test]
    fn cached_browse_refuses_changed_target_source_and_foreign_project_without_runtime_calls() {
        for mode in 0..4 {
            let host = setup();
            seed(&host);
            match mode {
                0 => host.set_setting("artisanDockerSocket", "\"unix:///another/docker.sock\""),
                1 => host.set_setting("artisanDockerContainer", &json!("b".repeat(64)).to_string()),
                2 => host.add_file("composer.json", "{\"require\":{}}"),
                _ => {
                    host.set_project_root(Some("/other"));
                    host.add_file("/other/artisan", "<?php");
                }
            }
            assert!(browse(&host.invocation()).is_err());
            assert!(host.process_runs().is_empty() && host.terminals().is_empty());
        }
    }
    #[test]
    fn explicit_docker_grant_is_required_after_review_and_duplicate_json_is_refused() {
        let host = setup().with_capabilities(&["project.read", "storage"]);
        host.answer_next_prompt(Some(&["ok"]));
        assert!(matches!(refresh(&host.invocation()), Err(Error::Invalid(message)) if message.contains("process:docker permission")));
        assert!(host.storage().is_empty());
        let owner = identity(&host.invocation().project().unwrap()).unwrap();
        let bad = proof()
            .to_string()
            .replacen("{", &format!("{{\"Id\":\"{ID}\","), 1);
        assert!(inspect(&bad, &owner).is_err());
        let bad = sample().replace(
            "\"name\":\"app:container\"",
            "\"name\":\"other\",\"name\":\"app:container\"",
        );
        assert!(metadata::parse(&bad).is_err());
    }

    #[test]
    fn overlapping_refresh_cannot_replace_a_newer_saved_snapshot() {
        let host = setup();
        seed(&host);
        let mut newer = load("/project").unwrap().unwrap();
        newer["capturedAt"] = json!(999);
        let newer = newer.to_string();
        host.answer_next_prompt(Some(&["ok"]));
        let mut calls = 0;
        assert_eq!(
            refresh_with(&host.invocation(), |_, _| {
                calls += 1;
                if calls == 2 {
                    host::storage::set(&key("/project"), &newer).unwrap();
                    Ok(sample())
                } else {
                    Ok(proof().to_string())
                }
            }),
            Err(Error::Stale)
        );
        assert_eq!(calls, 2);
        assert_eq!(host.storage()[&key("/project")], newer);
    }
}
