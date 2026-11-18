//! Reviewed API1.4 Run Targets. Distinct root-relative and experimental mapped project-root modes.
use openidea_plugin::host::storage;
use openidea_plugin::{Error, FileKind, FileStat, Invocation, PickItem, PluginRunTarget, Project, host};
use serde::{
    Deserialize,
    de::{self, MapAccess, SeqAccess, Visitor},
};
use serde_json::Value;
use std::{collections::BTreeSet, fmt};

pub(crate) const ID: &str = "devcontainer-root-relative";
pub(crate) const MAPPED_ID: &str = "devcontainer-mapped-project-root";
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode { RootRelative, MappedProjectRoot }
impl Mode {
    fn id(self) -> &'static str { match self { Self::RootRelative => ID, Self::MappedProjectRoot => MAPPED_ID } }
}
const MAX_OUTPUT: usize = 128 * 1024;
fn invalid(message: &str) -> Error {
    Error::Invalid(message.into())
}
fn absolute(path: &str) -> bool {
    path.starts_with('/')
        && path.len() > 1
        && path.len() <= 4096
        && !path.contains(['\\', '\0', '\n', '\r'])
        && !path.chars().any(char::is_control)
        && !path
            .split('/')
            .skip(1)
            .any(|p| p.is_empty() || p == "." || p == "..")
}
fn full_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn text<'a>(value: &'a Value, field: &str) -> Result<&'a str, Error> {
    value[field]
        .as_str()
        .ok_or_else(|| invalid("Docker returned container details in an unexpected format."))
}
// Docker output is not trusted to select an owner through last-key-wins JSON semantics.
struct Strict(Value);
impl<'de> Deserialize<'de> for Strict {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Strict;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("JSON without duplicate keys")
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Strict, E> {
                Ok(Strict(v.into()))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Strict, E> {
                Ok(Strict(v.into()))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Strict, E> {
                Ok(Strict(v.into()))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Strict, E> {
                serde_json::Number::from_f64(v)
                    .map(|n| Strict(n.into()))
                    .ok_or_else(|| E::custom("invalid number"))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Strict, E> {
                Ok(Strict(v.into()))
            }
            fn visit_string<E: de::Error>(self, v: String) -> Result<Strict, E> {
                Ok(Strict(v.into()))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Strict, E> {
                Ok(Strict(Value::Null))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Strict, A::Error> {
                let mut out = vec![];
                while let Some(Strict(v)) = a.next_element()? {
                    out.push(v)
                }
                Ok(Strict(out.into()))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<Strict, A::Error> {
                let mut out = serde_json::Map::new();
                while let Some(k) = a.next_key::<String>()? {
                    if out.contains_key(&k) {
                        return Err(de::Error::custom("duplicate key"));
                    }
                    let Strict(v) = a.next_value()?;
                    out.insert(k, v);
                }
                Ok(Strict(out.into()))
            }
        }
        d.deserialize_any(V)
    }
}
fn json(input: &str) -> Result<Value, Error> {
    if input.len() > MAX_OUTPUT {
        return Err(invalid("Docker returned more than 128 KiB of container details."));
    }
    serde_json::from_str::<Strict>(input)
        .map(|v| v.0)
        .map_err(|_| invalid("Docker returned container details that are not valid JSON."))
}
fn run(project: &Project, args: &[String]) -> Result<String, Error> {
    let out = project.run_process("docker", args, None)?;
    if out.status != Some(0) {
        return Err(invalid(
            "Docker could not read the container, so nothing was registered. Check that Docker is running, then try again.",
        ));
    }
    if out.stdout.len() > MAX_OUTPUT {
        return Err(invalid("Docker returned more than 128 KiB of container details."));
    }
    Ok(out.stdout)
}
fn endpoint(project: &Project) -> Result<String, Error> {
    let name = run(project, &["context".into(), "show".into()])?;
    let name = name.trim();
    if name.is_empty()
        || name.len() > 128
        || name.starts_with('-')
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
    {
        return Err(invalid("Unsupported Docker context name."));
    }
    let value = json(&run(
        project,
        &["context".into(), "inspect".into(), name.into()],
    )?)?;
    let list = value
        .as_array()
        .filter(|a| a.len() == 1)
        .ok_or_else(|| invalid("Docker did not report exactly one current context. Check docker context ls, then try again."))?;
    if list[0]["Name"].as_str() != Some(name) {
        return Err(invalid("The current Docker context changed while checking. Try again."));
    }
    let endpoint = text(&list[0]["Endpoints"]["docker"], "Host")?;
    if !endpoint.strip_prefix("unix://").is_some_and(absolute) {
        return Err(invalid(
            "Run targets need a local Docker socket (unix://). Switch to a local context with docker context use, then try again.",
        ));
    }
    Ok(endpoint.into())
}
fn prefixed(endpoint: &str, rest: &[&str]) -> Vec<String> {
    [
        vec!["--host".into(), endpoint.into()],
        rest.iter().map(|s| s.to_string()).collect(),
    ]
    .concat()
}
fn candidates(project: &Project, root: &str, endpoint: &str) -> Result<Vec<String>, Error> {
    let out = run(
        project,
        &prefixed(
            endpoint,
            &[
                "ps",
                "--no-trunc",
                "--filter",
                &format!("label=devcontainer.local_folder={root}"),
                "--format",
                "{{json .}}",
            ],
        ),
    )?;
    let mut ids = BTreeSet::new();
    for line in out.lines().filter(|line| !line.trim().is_empty()) {
        let value = json(line)?;
        let id = text(&value, "ID")?;
        if !full_id(id) || !ids.insert(id.to_owned()) || ids.len() > 256 {
            return Err(invalid(
                "Docker listed unexpected container IDs, or more than 256 containers. Refresh and try again.",
            ));
        }
    }
    Ok(ids.into_iter().collect())
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Identity {
    id: String,
    mount: String,
    user: String,
    started: String,
}
fn identity(input: &str, root: &str, config: &str, id: &str) -> Result<Identity, Error> {
    let value = json(input)?;
    let list = value
        .as_array()
        .filter(|a| a.len() == 1)
        .ok_or_else(|| invalid("Exactly one inspected container is required."))?;
    let c = &list[0];
    if !full_id(id)
        || c["Id"].as_str() != Some(id)
        || c["State"]["Running"] != true
        || c["State"]["Paused"] != false
        || c["State"]["Restarting"] != false
        || c["State"]["Dead"] != false
    {
        return Err(invalid(
            "The selected container is not running, or is paused or restarting. Start it, then try again.",
        ));
    }
    if c["Config"]["Labels"]["devcontainer.local_folder"].as_str() != Some(root)
        || c["Config"]["Labels"]["devcontainer.config_file"].as_str() != Some(config)
    {
        return Err(invalid(
            "This container was started for another project folder or configuration. Start the container from this project, then try again.",
        ));
    }
    let mounts = c["Mounts"]
        .as_array()
        .ok_or_else(|| invalid("Container mounts are unknown."))?;
    let matches: Vec<_> = mounts
        .iter()
        .filter(|m| m["Type"] == "bind" && m["Source"].as_str() == Some(root) && m["RW"] == true)
        .collect();
    if matches.len() != 1 {
        return Err(invalid(
            "The container must mount the project folder exactly once, read-write.",
        ));
    }
    let mount = text(matches[0], "Destination")?;
    if !absolute(mount) {
        return Err(invalid(
            "The project folder is mounted at an unsupported path in the container.",
        ));
    }
    for other in mounts {
        if std::ptr::eq(other, matches[0]) {
            continue;
        }
        let destination = text(other, "Destination")?;
        if destination.starts_with(&format!("{mount}/")) && absolute(destination) {
            continue;
        }
        if !absolute(destination) || destination == mount || mount.starts_with(&format!("{destination}/")) {
            return Err(Error::Invalid(format!(
                "The mount at {destination} hides the project folder {mount}. Remove that mount or move it below the project folder."
            )));
        }
    }
    let user = text(&c["Config"], "User")?;
    let started = text(&c["State"], "StartedAt")?;
    if user.len() > 128
        || user.chars().any(char::is_control)
        || started.is_empty()
        || started.len() > 128
        || started.chars().any(char::is_control)
    {
        return Err(invalid(
            "The container user or start time could not be read.",
        ));
    }
    Ok(Identity {
        id: id.into(),
        mount: mount.into(),
        user: user.into(),
        started: started.into(),
    })
}
fn inspect(
    project: &Project,
    root: &str,
    config: &str,
    endpoint: &str,
    id: &str,
) -> Result<Identity, Error> {
    identity(
        &run(project, &prefixed(endpoint, &["container", "inspect", id]))?,
        root,
        config,
        id,
    )
}
pub(crate) const REVIEW: &str = "run-target-review";
const EXPIRY: std::time::Duration = std::time::Duration::from_secs(120);
struct Prepared {
    plan: Plan,
    nonce: String,
    at: std::time::Instant,
}
#[derive(Default)]
pub(crate) struct State {
    sequence: u64,
    prepared: Option<Prepared>,
    status: Option<(String, &'static str)>,
}
impl State {
    pub(crate) fn clear(&mut self) {
        self.prepared = None;
        self.status = None;
    }
    pub(crate) fn clear_for(&mut self, root: &str) {
        if self.prepared.as_ref().is_some_and(|p| p.plan.root == root)
            || self.status.as_ref().is_some_and(|(owner, _)| owner == root)
        {
            self.clear();
            host::refresh_view(REVIEW);
        }
    }
    fn begin(&mut self) -> Result<String, Error> {
        self.clear();
        self.sequence = self.sequence.checked_add(1).ok_or(Error::Stale)?;
        // Instance-reclaim/reload must not recycle a still-visible old button key.
        // This is an opaque UI ticket, not an external cryptographic capability.
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| Error::Stale)?
            .as_nanos();
        Ok(format!("{timestamp:x}-{:x}", self.sequence))
    }
    pub(crate) fn view(&self, ctx: &Invocation) -> openidea_plugin::views::View {
        use openidea_plugin::views::{Body, Button, Field, FieldValue, View};
        let root = ctx.project().and_then(|p| p.root().ok());
        if let Some(prepared) = &self.prepared {
            if root.as_deref() != Some(prepared.plan.root.as_str()) {
                return View::message(
                    "This registration belongs to another project. Nothing was changed.",
                );
            }
            if prepared.at.elapsed() >= EXPIRY {
                return View::message(
                    "This registration expired. Prepare it again; nothing was changed.",
                );
            }
            return View::new(Body::Form(vec![Field {
                key: "review".into(), label: "Registration to review".into(),
                value: FieldValue::readonly_text(&prepared.plan.preview(), true),
            }])).status("Not registered yet. Registering checks the project, configuration and container again.")
                .button(Button::new(&format!("register:{}", prepared.nonce), "Register"));
        }
        if let Some((owner, message)) = &self.status {
            if root.as_deref() == Some(owner.as_str()) {
                return View::message(*message);
            }
            return View::message("Target status belongs to another project.");
        }
        View::message("Nothing to review. Run Dev Container: Register Container as Run Target first.")
    }
    pub(crate) fn commit(&mut self, ctx: &Invocation, key: &str) -> Result<(), Error> {
        let nonce = key.strip_prefix("register:").ok_or(Error::Stale)?;
        let prepared = self.prepared.as_ref().ok_or(Error::Stale)?;
        // Foreign, replayed and expired controls never withdraw a prior slot.
        if prepared.nonce != nonce
            || prepared.at.elapsed() >= EXPIRY
            || ctx.project().ok_or(Error::Stale)?.root()? != prepared.plan.root
        {
            return Err(Error::Stale);
        }
        let prepared = self.prepared.take().ok_or(Error::Stale)?;
        self.status = None; // Consume the correct fresh-event ticket before bounded validation.
        let mut withdrawn = false;
        let result = (|| {
            let project = prepared.plan.validate(ctx)?;
            if prepared.at.elapsed() >= EXPIRY {
                return Err(Error::Stale);
            }
            // This event has no second modal. The API does not atomically bind its
            // project snapshot through inspection/registration; no stronger claim.
            remember(&prepared.plan)?;
            host::unregister_run_target(&project, prepared.plan.mode.id());
            withdrawn = true;
            host::register_run_target(&project, &prepared.plan.target())
        })();
        let message = match &result {
            Ok(()) => match prepared.plan.mode {
                Mode::RootRelative => "Registered the Run target. Select it in Run on to use it; no container command has run.",
                Mode::MappedProjectRoot => "Registered project-root tooling. Select it in the project's Language server targets to use it. Nothing was selected for you and no container command has run.",
            }
            Err(Error::Invalid(message)) if message == STORAGE_DENIED => {
                "Not registered: run target registration needs the storage permission. Allow it in Settings › Plugins › Dev Container CLI."
            }
            Err(_) if withdrawn => {
                "Registration failed after the previous target was removed. Prepare it again."
            }
            Err(_) => {
                "Not registered: the container or configuration changed. The previous target is unchanged."
            }
        };
        self.status = Some((prepared.plan.root, message));
        host::refresh_view(REVIEW); // Never reveal/steal focus after the commit event.
        result
    }
}

struct Plan {
    mode: Mode,
    root: String,
    config: String,
    source: String,
    stat: FileStat,
    endpoint: String,
    identity: Identity,
}
impl Plan {
    fn target(&self) -> PluginRunTarget {
        PluginRunTarget {
            id: self.mode.id().into(),
            name: format!("Dev Container ({}, {})", match self.mode { Mode::RootRelative => "Run target", Mode::MappedProjectRoot => "project-root tooling" }, &self.identity.id[..12]),
            program: "docker".into(),
            args: prefixed(
                &self.endpoint,
                &match self.mode {
                    Mode::RootRelative => vec!["exec", "--workdir", &self.identity.mount, &self.identity.id],
                    Mode::MappedProjectRoot => vec!["exec", "-i", "--workdir", &self.identity.mount, &self.identity.id],
                },
            ),
            workdir_args: vec![],
            container_root: match self.mode { Mode::RootRelative => None, Mode::MappedProjectRoot => Some(self.identity.mount.clone()) },
            commands: vec![],
        }
    }
    fn preview(&self) -> String {
        let (heading, ending) = match self.mode {
            Mode::RootRelative => (
                "Register this container as a Run target.",
                "Run and Test commands will run inside the container from the project bind folder, with project-relative arguments and no shell. Select the target in Run on to use it.",
            ),
            Mode::MappedProjectRoot => (
                "Register this container as project-root tooling.",
                "Language servers and tools you select this target for will run inside the container from the project bind folder, with project paths mapped. Select the target in the project's Language server targets to use it.",
            ),
        };
        format!(
            "{heading}\n\nProject: {}\nConfiguration: {}\nDocker endpoint: {}\nContainer ID: {}\nProject bind: {} -> {} (read-write)\nContainer user: {}\nStarted at: {}\nCommand prefix:\n{}\n\n{ending}\n\nNothing runs now. If the container or its configuration changes, remove the target and register it again.",
            serde_json::to_string(&self.root).unwrap(),
            serde_json::to_string(&self.config).unwrap(),
            serde_json::to_string(&self.endpoint).unwrap(),
            self.identity.id,
            serde_json::to_string(&self.root).unwrap(),
            serde_json::to_string(&self.identity.mount).unwrap(),
            serde_json::to_string(&self.identity.user).unwrap(),
            self.identity.started,
            serde_json::to_string_pretty(&self.target().args).unwrap()
        )
    }
    fn validate(&self, ctx: &Invocation) -> Result<Project, Error> {
        let project = ctx.project().ok_or(Error::Stale)?;
        if project.root()? != self.root
            || project.stat(&self.config)? != self.stat
            || project.read_file(&self.config)? != self.source
        {
            return Err(Error::Stale);
        }
        if inspect(
            &project,
            &self.root,
            &self.config,
            &self.endpoint,
            &self.identity.id,
        )? != self.identity
        {
            return Err(Error::Stale);
        }
        Ok(project)
    }
}
fn registration_key(root: &str, target_id: &str) -> String {
    format!("run-target:{target_id}:{root}")
}
const STORAGE_DENIED: &str = "Run target registration needs the storage permission. Allow it in Settings › Plugins › Dev Container CLI.";
fn remember(plan: &Plan) -> Result<(), Error> {
    let value = serde_json::json!({"id": plan.identity.id, "endpoint": plan.endpoint, "config": plan.config}).to_string();
    match storage::set(&registration_key(&plan.root, plan.mode.id()), &value) {
        Err(Error::Denied(_)) => {
            host::notify(openidea_plugin::NotificationLevel::Warning, "Dev Container", STORAGE_DENIED);
            Err(Error::Invalid(STORAGE_DENIED.into()))
        }
        other => other,
    }
}
fn stored_registration(saved: &str) -> Option<(String, String, String)> {
    let value: Value = serde_json::from_str(saved).ok()?;
    let id = value["id"].as_str()?;
    let endpoint = value["endpoint"].as_str()?;
    let config = value["config"].as_str()?;
    let valid = full_id(id)
        && endpoint.strip_prefix("unix://").is_some_and(absolute)
        && absolute(config)
        && !endpoint.starts_with('-')
        && !config.starts_with('-');
    valid.then(|| (id.into(), endpoint.into(), config.into()))
}
pub(crate) fn validate_running(root: &str, target_id: &str) -> Result<(), String> {
    let refuse = |why: &str| format!("{why} Start the container, then register the target again.");
    let Some(project) = host::projects()
        .unwrap_or_default()
        .into_iter()
        .find(|p| p.root().is_ok_and(|r| r == root))
    else {
        return Err(refuse("The project is not open."));
    };
    let saved = match storage::get(&registration_key(root, target_id)) {
        Ok(Some(saved)) => saved,
        Ok(None) => return Err("Register the target again.".into()),
        Err(Error::Denied(_)) => return Err(STORAGE_DENIED.into()),
        Err(_) => return Err(refuse("The registered container could not be read.")),
    };
    let Some((id, endpoint, config)) = stored_registration(&saved) else {
        return Err("Register the target again.".into());
    };
    inspect(&project, root, &config, &endpoint, &id)
        .map(|_| ())
        .map_err(|_| refuse("The registered container is not running, or Docker is not reachable."))
}
pub(crate) fn prepare(state: &mut State, ctx: &Invocation) -> Result<Option<String>, Error> {
    prepare_mode(state, ctx, Mode::RootRelative)
}
pub(crate) fn prepare_mapped(state: &mut State, ctx: &Invocation) -> Result<Option<String>, Error> {
    prepare_mode(state, ctx, Mode::MappedProjectRoot)
}
fn prepare_mode(state: &mut State, ctx: &Invocation, mode: Mode) -> Result<Option<String>, Error> {
    let nonce = state.begin()?;
    let project = ctx
        .project()
        .ok_or_else(|| invalid("Open a project before registering a target."))?;
    let root = project.root()?;
    if !absolute(&root) {
        return Err(invalid(
            "Run targets need a project on a local Unix path (macOS or Linux).",
        ));
    }
    let Some(relative) = crate::configs::choose(&project, false)? else {
        return Ok(None);
    };
    let config = format!("{root}/{relative}");
    let stat = project.stat(&config)?;
    if stat.kind != FileKind::File || stat.size > crate::MAX_BYTES as u64 {
        return Err(invalid("The configuration must be a project file up to 256 KiB."));
    }
    let source = project.read_file(&config)?;
    if source.len() > crate::MAX_BYTES {
        return Err(invalid("Configuration snapshot exceeds 256 KiB."));
    }
    let endpoint = endpoint(&project)?;
    let ids = candidates(&project, &root, &endpoint)?;
    if ids.is_empty() {
        return Err(invalid(
            "No running dev container for this project. Start it with Dev Container: Start Container, then try again.",
        ));
    }
    let choices: Vec<_> = ids
        .iter()
        .map(|id| PickItem {
            key: id.clone(),
            label: format!("Container {}", &id[..12]),
            description: Some("Its labels and project folder mount are checked next.".into()),
        })
        .collect();
    let Some(id) = host::pick(
        match mode { Mode::RootRelative => "Select the container for the Run target", Mode::MappedProjectRoot => "Select the container for project-root tooling" },
        &choices,
    )?
    else {
        return Ok(None);
    };
    if !ids.contains(&id) {
        return Err(Error::Stale);
    }
    let identity = inspect(&project, &root, &config, &endpoint, &id)?;
    let plan = Plan {
        mode,
        root,
        config,
        source,
        stat,
        endpoint,
        identity,
    };
    let at = std::time::Instant::now();
    let preview = plan.preview();
    if preview.len() > 16 * 1024 {
        return Err(invalid(
            "The registration details exceed the 16 KiB confirmation limit.",
        ));
    }
    if !host::confirm(
        match mode { Mode::RootRelative => "Review Run target registration", Mode::MappedProjectRoot => "Review project-root tooling registration" },
        &preview,
    )? {
        return Ok(None);
    }
    // Modal answers produce an immutable draft only. ctx.project() is a snapshot;
    // it is deliberately not used as a post-modal active-project freshness claim.
    state.prepared = Some(Prepared { plan, nonce, at });
    if let Err(error) = host::reveal_view(REVIEW) {
        state.clear();
        return Err(error);
    }
    host::refresh_view(REVIEW);
    Ok(Some(
        "Nothing is registered yet. Review it and choose Register.".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{
        Plugin, ProcessOutput,
        mock::MockHost,
        views::{Body, ViewEvent, Views},
    };
    const CID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    fn fixture() -> Value {
        serde_json::json!([{"Id":CID,"State":{"Running":true,"Paused":false,"Restarting":false,"Dead":false,"StartedAt":"2026-11-09T00:00:00Z"},"Config":{"User":"1000:1000","Labels":{"devcontainer.local_folder":"/work/app","devcontainer.config_file":"/work/app/.devcontainer/devcontainer.json"},"Env":["DO_NOT_DISPLAY=synthetic"]},"Mounts":[{"Type":"bind","Source":"/work/app","Destination":"/workspace/app","RW":true}]}])
    }
    fn ok(text: &str) -> Result<ProcessOutput, Error> {
        Ok(ProcessOutput {
            status: Some(0),
            stdout: text.into(),
            stderr: String::new(),
        })
    }
    fn setup() -> MockHost {
        let host =
            MockHost::install().with_capabilities(&["project.read", "process:docker", "storage"]);
        host.set_project_root(Some("/work/app"));
        host.add_file(".devcontainer/devcontainer.json", "{}");
        host.on_process("docker",|args,_|match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice(){
            ["context","show"]=>ok("desktop-linux\n"),
            ["context","inspect","desktop-linux"]=>ok(r#"[{"Name":"desktop-linux","Endpoints":{"docker":{"Host":"unix:///var/run/docker.sock"}}}]"#),
            ["--host","unix:///var/run/docker.sock","ps","--no-trunc","--filter","label=devcontainer.local_folder=/work/app","--format","{{json .}}"]=>ok(&serde_json::json!({"ID":CID}).to_string()),
            ["--host","unix:///var/run/docker.sock","container","inspect",CID]=>ok(&fixture().to_string()),
            _=>panic!("unexpected/mutating Docker argv {args:?}"),
        });
        host
    }
    fn plan(host: &MockHost) -> Plan {
        let p = host.invocation().project().unwrap();
        Plan {
            mode: Mode::RootRelative,
            root: "/work/app".into(),
            config: "/work/app/.devcontainer/devcontainer.json".into(),
            source: "{}".into(),
            stat: p.stat(".devcontainer/devcontainer.json").unwrap(),
            endpoint: "unix:///var/run/docker.sock".into(),
            identity: identity(
                &fixture().to_string(),
                "/work/app",
                "/work/app/.devcontainer/devcontainer.json",
                CID,
            )
            .unwrap(),
        }
    }
    fn mapped_draft(host: &MockHost, main: &mut crate::Main) -> String {
        host.answer_next_prompt(Some(&[CID]));
        host.answer_next_prompt(Some(&["yes"]));
        main.run_command("register-mapped-tooling-target", &host.invocation(), None).unwrap();
        format!("register:{}", main.run_target.prepared.as_ref().unwrap().nonce)
    }
    const RUNNING_ROW: &str = r#"{"ID":"CID","Names":"app","State":"running"}"#;
    fn registered(host: &MockHost) {
        remember(&plan(host)).unwrap();
    }
    #[test]
    fn validate_accepts_the_registered_running_container() {
        let host = setup();
        registered(&host);
        validate_running("/work/app", ID).unwrap();
        assert_eq!(host.process_runs().len(), 1);
    }
    #[test]
    fn validate_refuses_stale_container_id() {
        let host = setup();
        registered(&host);
        host.on_process("docker", |_, _| Ok(ProcessOutput { status: Some(1), stdout: String::new(), stderr: "No such container".into() }));
        assert!(validate_running("/work/app", ID).is_err());
    }
    #[test]
    fn validate_refuses_when_docker_down() {
        let host = setup();
        registered(&host);
        host.on_process("docker", |_, _| Err(Error::Invalid("cannot connect to the Docker daemon".into())));
        assert!(validate_running("/work/app", ID).is_err());
    }
    #[test]
    fn validate_refuses_without_a_registration() {
        let host = setup();
        assert!(validate_running("/work/app", ID).is_err());
        assert!(host.process_runs().is_empty());
    }
    #[test]
    fn denied_storage_fails_registration_and_validation_clearly() {
        let host = setup();
        let mut main = crate::Main::default();
        let key = draft(&host, &mut main);
        let host = host.with_capabilities(&["project.read", "process:docker"]);
        assert!(main.handle(REVIEW, ViewEvent::Button(key), &host.invocation()).is_err());
        assert!(main.run_target.status.as_ref().unwrap().1.contains("needs the storage permission"));
        assert_eq!(STORAGE_DENIED, ui_kit::denied::message("Run target registration", "storage", "Dev Container CLI"));
        assert!(host.run_targets().is_empty());
        assert!(host.notifications().iter().any(|n| n.body == STORAGE_DENIED));
        assert!(validate_running("/work/app", ID).unwrap_err() == STORAGE_DENIED);
    }
    #[test]
    fn validate_refuses_malformed_stored_registration() {
        let host = setup();
        for value in [
            "{\"id\":\"-x\",\"endpoint\":\"unix:///var/run/docker.sock\",\"config\":\"/work/app/c.json\"}".to_string(),
            format!("{{\"id\":\"{CID}\",\"endpoint\":\"unix:///a\\n--x\",\"config\":\"/work/app/c.json\"}}"),
            "legacy".to_string(),
        ] {
            storage::set(&registration_key("/work/app", ID), &value).unwrap();
            assert_eq!(validate_running("/work/app", ID).unwrap_err(), "Register the target again.");
        }
        assert!(host.process_runs().is_empty());
    }
    #[test]
    fn commit_remembers_the_registered_container() {
        let host = setup();
        let mut main = crate::Main::default();
        let key = draft(&host, &mut main);
        main.handle(REVIEW, ViewEvent::Button(key), &host.invocation()).unwrap();
        validate_running("/work/app", ID).unwrap();
    }
    fn register_both(host: &MockHost) {
        let project = host.invocation().project().unwrap();
        let mut p = plan(host);
        host::register_run_target(&project, &p.target()).unwrap();
        p.mode = Mode::MappedProjectRoot;
        host::register_run_target(&project, &p.target()).unwrap();
    }
    #[test]
    fn mapped_public_fresh_review_preserves_default_and_registers_exact_stdin_mapping_fields() {
        let host = setup();
        let default = plan(&host).target();
        host::register_run_target(&host.invocation().project().unwrap(), &default).unwrap();
        let mut main = crate::Main::default();
        let key = mapped_draft(&host, &mut main);
        assert_eq!(host.run_targets().len(), 1, "modal prepares only");
        let preview = main.run_target.prepared.as_ref().unwrap().plan.preview();
        assert!(preview.contains("Language server targets") && !preview.contains("eb72"));
        assert!(preview.contains("project bind folder"));
        assert!(preview.len() < 16384 && !preview.contains("DO_NOT_DISPLAY"));
        main.render(REVIEW, None, &host.invocation()).unwrap();
        assert_eq!(host.process_runs().len(), 4, "render is inspection-free");
        main.handle(REVIEW, ViewEvent::Button(key.clone()), &host.invocation()).unwrap();
        let targets = host.run_targets();
        assert_eq!(targets.get(&("/work/app".into(), ID.into())), Some(&default));
        let mapped = &targets[&("/work/app".into(), MAPPED_ID.into())];
        assert_eq!(mapped.args, ["--host", "unix:///var/run/docker.sock", "exec", "-i", "--workdir", "/workspace/app", CID]);
        assert_eq!(mapped.container_root.as_deref(), Some("/workspace/app"));
        assert!(mapped.workdir_args.is_empty() && mapped.commands.is_empty());
        assert_eq!(host.process_runs().len(), 5, "only bounded reads, no exec");
        assert!(main.handle(REVIEW, ViewEvent::Button(key), &host.invocation()).is_err());
        assert_eq!(host.run_targets(), targets, "replay must preserve both slots");
        assert!(host.terminals().is_empty());
    }
    #[test]
    fn mapped_cancelled_picker_or_review_preserves_both_registered_modes() {
        for picker in [false, true] {
            let host = setup(); register_both(&host); let previous = host.run_targets();
            host.answer_next_prompt(if picker {Some(&[CID])} else {None});
            if picker {host.answer_next_prompt(None);}
            let mut main = crate::Main::default();
            main.run_command("register-mapped-tooling-target", &host.invocation(), None).unwrap();
            assert_eq!(host.run_targets(), previous);
            assert!(main.run_target.prepared.is_none());
        }
    }
    #[test]
    fn mapped_foreign_expired_or_wrong_nonce_events_leave_both_slots_and_no_reinspection() {
        for case in 0..3 {
            let host = setup(); register_both(&host); let previous = host.run_targets();
            let mut main = crate::Main::default(); let mut key = mapped_draft(&host, &mut main);
            match case {
                0 => host.set_project_root(Some("/foreign")),
                1 => main.run_target.prepared.as_mut().unwrap().at = std::time::Instant::now() - EXPIRY,
                _ => key.push_str("-forged"),
            }
            assert!(main.handle(REVIEW, ViewEvent::Button(key), &host.invocation()).is_err());
            assert_eq!(host.run_targets(), previous);
            assert_eq!(host.process_runs().len(), 4);
        }
    }
    #[test]
    fn mapped_fresh_commit_rejects_changed_config_container_or_permission_without_withdrawal() {
        for case in 0..3 {
            let host = setup(); register_both(&host); let previous = host.run_targets();
            let mut main = crate::Main::default(); let key = mapped_draft(&host, &mut main);
            let host = match case {
                0 => {host.add_file(".devcontainer/devcontainer.json", "{changed:true}");host},
                1 => {host.on_process("docker", |args,_| {assert_eq!(args[2],"container");let mut f=fixture();f[0]["State"]["StartedAt"]="restarted".into();ok(&f.to_string())});host},
                _ => host.with_capabilities(&["project.read", "storage"]),
            };
            assert!(main.handle(REVIEW, ViewEvent::Button(key), &host.invocation()).is_err());
            assert_eq!(host.run_targets(), previous);
            assert!(main.run_target.prepared.is_none());
        }
    }
    #[test]
    fn mapped_remove_and_default_remove_are_independent_and_project_owned() {
        let host=setup();register_both(&host);
        host.set_project_root(Some("/foreign"));
        let mut main=crate::Main::default();
        main.run_command("unregister-mapped-tooling-target",&host.invocation(),None).unwrap();
        assert_eq!(host.run_targets().len(),2);
        host.set_project_root(Some("/work/app"));
        main.run_command("unregister-mapped-tooling-target",&host.invocation(),None).unwrap();
        assert!(host.run_targets().contains_key(&("/work/app".into(),ID.into())));
        assert!(!host.run_targets().contains_key(&("/work/app".into(),MAPPED_ID.into())));
        register_both(&host);
        main.run_command("unregister-run-target",&host.invocation(),None).unwrap();
        assert!(host.run_targets().contains_key(&("/work/app".into(),MAPPED_ID.into())));
        assert_eq!(host.process_runs().len(),0);
    }
    #[test]
    fn container_stop_withdraws_both_mode_slots_after_stopping() {
        let host=setup();register_both(&host);
        host.on_process("docker", |args,_| {
            match args.first().map(String::as_str) { Some("ps") => ok(&RUNNING_ROW.replace("CID", CID)), Some("stop") => ok(CID), _ => panic!("unexpected {args:?}") }
        });
        host.answer_next_prompt(Some(&[]));
        crate::Main::default().run_command("stop",&host.invocation(),None).unwrap();
        assert!(host.run_targets().is_empty());
    }
    #[test]
    fn mapped_destination_spaces_are_literal_and_options_always_precede_fixed_full_id() {
        let host=setup();let mut p=plan(&host);p.mode=Mode::MappedProjectRoot;
        p.identity.mount="/workspace/project name;literal".into();
        let target=p.target();assert_eq!(target.args[5],"/workspace/project name;literal");
        assert_eq!(target.args.last().unwrap(),CID);
        assert_eq!(target.container_root.as_deref(),Some(target.args[5].as_str()));
        assert!(target.workdir_args.is_empty());
    }
    #[test]
    fn register_exact_project_owned_prefix_without_running_container_command() {
        let host = setup();
        host.answer_next_prompt(Some(&[CID]));
        host.answer_next_prompt(Some(&["yes"]));
        let mut main = crate::Main::default();
        let prepared = main
            .run_command("register-run-target", &host.invocation(), None)
            .unwrap()
            .unwrap();
        assert!(prepared.contains("Nothing is registered yet"));
        assert!(host.run_targets().is_empty());
        assert_eq!(host.process_runs().len(), 4);
        let key = format!(
            "register:{}",
            main.run_target.prepared.as_ref().unwrap().nonce
        );
        main.render(REVIEW, None, &host.invocation()).unwrap();
        assert_eq!(
            host.process_runs().len(),
            4,
            "review rendering must not inspect Docker"
        );
        main.handle(REVIEW, ViewEvent::Button(key.clone()), &host.invocation())
            .unwrap();
        assert!(
            main.handle(REVIEW, ViewEvent::Button(key), &host.invocation())
                .is_err()
        );
        assert!(
            main.run_target
                .status
                .as_ref()
                .unwrap()
                .1
                .starts_with("Registered")
        );
        let targets = host.run_targets();
        let target = targets.get(&("/work/app".into(), ID.into())).unwrap();
        assert_eq!(target.program, "docker");
        assert_eq!(
            target.args,
            [
                "--host",
                "unix:///var/run/docker.sock",
                "exec",
                "--workdir",
                "/workspace/app",
                CID
            ]
        );
        assert!(
            target.container_root.is_none()
                && target.workdir_args.is_empty()
                && target.commands.is_empty()
        );
        assert_eq!(host.process_runs().len(), 5);
        assert!(
            host.process_runs()
                .iter()
                .all(|r| r.cwd.as_deref() == Some("/work/app"))
        );
        assert!(host.terminals().is_empty());
    }
    #[test]
    fn cancelled_picker_or_preview_registers_nothing() {
        for approve_picker in [false, true] {
            let host = setup();
            host.answer_next_prompt(if approve_picker { Some(&[CID]) } else { None });
            host.answer_next_prompt(None);
            assert!(
                prepare(&mut State::default(), &host.invocation())
                    .unwrap()
                    .is_none()
            );
            assert!(host.run_targets().is_empty());
            assert!(host.terminals().is_empty());
        }
    }
    #[test]
    fn forged_picker_id_is_not_inspected_or_registered() {
        let host = setup();
        host.answer_next_prompt(Some(&["bbbb"]));
        assert!(prepare(&mut State::default(), &host.invocation()).is_err());
        assert!(host.run_targets().is_empty());
    }
    #[test]
    fn scope_labels_mount_and_liveness_must_all_be_exact() {
        for kind in 0..14 {
            let mut f = fixture();
            let c = &mut f[0];
            match kind {
                0 => c["Id"] = "bbbb".into(),
                1 => c["State"]["Running"] = false.into(),
                2 => c["State"]["Paused"] = true.into(),
                3 => c["State"]["Restarting"] = true.into(),
                4 => c["State"]["Dead"] = true.into(),
                5 => c["Config"]["Labels"]["devcontainer.local_folder"] = "/work/other".into(),
                6 => {
                    c["Config"]["Labels"]["devcontainer.config_file"] =
                        "/work/app/.devcontainer/other.json".into()
                }
                7 => c["Mounts"][0]["Source"] = "/work/app-child".into(),
                8 => c["Mounts"][0]["RW"] = false.into(),
                9 => c["Mounts"][0]["Type"] = "volume".into(),
                10 => c["Mounts"][0]["Destination"] = "/workspace/../app".into(),
                11 => c["Mounts"] = serde_json::json!([]),
                12 => c["State"]
                    .as_object_mut()
                    .unwrap()
                    .remove("Paused")
                    .map(|_| ())
                    .unwrap(),
                _ => c["Config"]["User"] = "user\nspoof".into(),
            }
            assert!(
                identity(
                    &f.to_string(),
                    "/work/app",
                    "/work/app/.devcontainer/devcontainer.json",
                    CID
                )
                .is_err(),
                "case{kind}"
            );
        }
    }
    #[test]
    fn nested_overlapping_and_ambiguous_mounts_refuse() {
        for destination in ["/workspace", "/workspace/app"] {
            let mut f = fixture();
            f[0]["Mounts"]
                .as_array_mut()
                .unwrap()
                .push(serde_json::json!({"Type":"volume","Destination":destination}));
            assert!(
                identity(
                    &f.to_string(),
                    "/work/app",
                    "/work/app/.devcontainer/devcontainer.json",
                    CID
                )
                .is_err()
            );
        }
        let mut f = fixture();
        let m = f[0]["Mounts"][0].clone();
        f[0]["Mounts"].as_array_mut().unwrap().push(m);
        assert!(
            identity(
                &f.to_string(),
                "/work/app",
                "/work/app/.devcontainer/devcontainer.json",
                CID
            )
            .is_err()
        );
    }
    #[test]
    fn nested_node_modules_mount_allowed() {
        let mut f = fixture();
        f[0]["Mounts"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"Type":"volume","Destination":"/workspace/app/node_modules"}));
        let identity = identity(&f.to_string(), "/work/app", "/work/app/.devcontainer/devcontainer.json", CID).unwrap();
        assert_eq!(identity.mount, "/workspace/app");
        f[0]["Mounts"].as_array_mut().unwrap().push(serde_json::json!({"Type":"volume","Destination":"/workspace"}));
        let error = super::identity(&f.to_string(), "/work/app", "/work/app/.devcontainer/devcontainer.json", CID).unwrap_err();
        assert!(error.to_string().contains("/workspace"));
    }
    #[test]
    fn strict_json_rejects_duplicates_trailing_text_and_oversize() {
        for input in [r#"{"x":1,"x":2}"#, r#"{"a":{"x":1,"x":2}}"#, "[] trailing"] {
            assert!(json(input).is_err());
        }
        assert!(json(&" ".repeat(MAX_OUTPUT + 1)).is_err());
        assert!(full_id(CID));
        assert!(!full_id(&CID[..12]));
        assert!(!absolute("/"));
        assert!(!absolute("/a//b"));
        assert!(!absolute("C:\\work"));
    }
    #[test]
    fn remote_or_mismatched_context_endpoint_refuses() {
        for endpoint_value in [
            "tcp://127.0.0.1:2375",
            "ssh://host",
            "unix://relative",
            "unix:///tmp/../docker.sock",
            "npipe:////./pipe/docker_engine",
        ] {
            let host = setup();
            let value = endpoint_value.to_string();
            host.on_process("docker",move|args,_| if args==["context","show"]{ok("desktop-linux")}else{ok(&serde_json::json!([{"Name":"desktop-linux","Endpoints":{"docker":{"Host":value}}}]).to_string())});
            assert!(endpoint(&host.invocation().project().unwrap()).is_err());
        }
    }
    #[test]
    fn project_config_or_current_unsaved_text_change_invalidates_plan() {
        let host = setup();
        let p = plan(&host);
        host.set_project_root(Some("/another"));
        assert!(p.validate(&host.invocation()).is_err());
        host.set_project_root(Some("/work/app"));
        host.open_editor(
            "/work/app/.devcontainer/devcontainer.json",
            "jsonc",
            "{\"image\":\"changed\"}",
            &[],
        );
        assert!(p.validate(&host.invocation()).is_err());
        assert!(host.run_targets().is_empty());
    }
    #[test]
    fn changed_container_restart_snapshot_refuses() {
        let host = setup();
        let p = plan(&host);
        host.on_process("docker", |_, _| {
            let mut f = fixture();
            f[0]["State"]["StartedAt"] = "later".into();
            ok(&f.to_string())
        });
        assert!(p.validate(&host.invocation()).is_err());
    }
    #[test]
    fn preview_is_exact_and_does_not_expose_environment() {
        let host = setup();
        let p = plan(&host);
        let preview = p.preview();
        assert!(
            preview.contains(CID)
                && preview.contains("unix:///var/run/docker.sock")
                && preview.contains("--workdir")
                && preview.contains("project bind folder")
        );
        assert!(!preview.contains("DO_NOT_DISPLAY"));
        assert!(preview.len() < 16 * 1024);
    }
    #[test]
    fn denial_or_anonymous_project_never_registers() {
        let host = setup().with_capabilities(&["project.read"]);
        assert!(prepare(&mut State::default(), &host.invocation()).is_err());
        assert!(host.run_targets().is_empty());
        host.set_project_root(None);
        assert!(prepare(&mut State::default(), &host.invocation()).is_err());
    }
    #[test]
    fn unregister_is_project_scoped_and_needs_no_cli() {
        let host = setup();
        let p = plan(&host);
        host::register_run_target(&host.invocation().project().unwrap(), &p.target()).unwrap();
        host.set_project_root(Some("/other"));
        crate::Main::default()
            .run_command("unregister-run-target", &host.invocation(), None)
            .unwrap();
        assert_eq!(host.run_targets().len(), 1);
        host.set_project_root(Some("/work/app"));
        crate::Main::default()
            .run_command("unregister-run-target", &host.invocation(), None)
            .unwrap();
        assert!(host.run_targets().is_empty());
        assert!(host.process_runs().is_empty());
    }
    #[test]
    fn controlled_start_rebuild_and_stop_withdraw_prior_registration() {
        for action in ["up", "rebuild", "stop"] {
            let host = setup().with_capabilities(&[
                "project.read",
                "process:docker",
                "process:devcontainer",
                "storage",
            ]);
            let p = plan(&host);
            host::register_run_target(&host.invocation().project().unwrap(), &p.target()).unwrap();
            host.answer_next_prompt(Some(&["yes"]));
            if action == "stop" {
                host.on_process("docker", |args, _| {
                    if args[0] == "ps" {
                        ok(&RUNNING_ROW.replace("CID", CID))
                    } else if args[0] == "stop" {
                        ok(CID)
                    } else {
                        panic!("unexpected {args:?}")
                    }
                });
            }
            let mut main = crate::Main::default();
            main.run_target.prepared = Some(Prepared {
                plan: plan(&host),
                nonce: "controlled".into(),
                at: std::time::Instant::now(),
            });
            main.run_command(action, &host.invocation(), None).unwrap();
            assert!(host.run_targets().is_empty(), "{action}");
            assert!(
                main.run_target.prepared.is_none(),
                "{action} must discard prepared metadata too"
            );
        }
    }
    #[test]
    fn failed_fresh_commit_reinspection_preserves_old_slot_without_exec() {
        let host = setup();
        let p = plan(&host);
        host::register_run_target(&host.invocation().project().unwrap(), &p.target()).unwrap();
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let seen = calls.clone();
        host.on_process("docker",move|args,_| {
            match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
                ["context","show"]=>ok("desktop-linux"),
                ["context","inspect","desktop-linux"]=>ok(r#"[{"Name":"desktop-linux","Endpoints":{"docker":{"Host":"unix:///var/run/docker.sock"}}}]"#),
                ["--host","unix:///var/run/docker.sock","ps","--no-trunc","--filter","label=devcontainer.local_folder=/work/app","--format","{{json .}}"]=>ok(&serde_json::json!({"ID":CID}).to_string()),
                ["--host","unix:///var/run/docker.sock","container","inspect",CID]=>{let n=seen.get();seen.set(n+1);let mut f=fixture();if n>0 {f[0]["State"]["Running"]=false.into();}ok(&f.to_string())},
                _=>panic!("unexpected/exec {args:?}"),
            }
        });
        host.answer_next_prompt(Some(&[CID]));
        host.answer_next_prompt(Some(&["yes"]));
        let mut main = crate::Main::default();
        main.run_command("register-run-target", &host.invocation(), None)
            .unwrap();
        let key = format!(
            "register:{}",
            main.run_target.prepared.as_ref().unwrap().nonce
        );
        assert!(
            main.handle(REVIEW, ViewEvent::Button(key), &host.invocation())
                .is_err()
        );
        assert_eq!(host.run_targets().len(), 1);
        assert!(main.run_target.prepared.is_none());
        assert_eq!(calls.get(), 2);
        assert!(host.terminals().is_empty());
    }
    fn draft(host: &MockHost, main: &mut crate::Main) -> String {
        host.answer_next_prompt(Some(&[CID]));
        host.answer_next_prompt(Some(&["yes"]));
        let status = main
            .run_command("register-run-target", &host.invocation(), None)
            .unwrap()
            .unwrap();
        assert!(status.starts_with("Nothing is registered yet"));
        format!(
            "register:{}",
            main.run_target.prepared.as_ref().unwrap().nonce
        )
    }
    fn previous_slot(host: &MockHost) -> PluginRunTarget {
        let mut old = plan(host).target();
        old.name = "previous-slot".into();
        host::register_run_target(&host.invocation().project().unwrap(), &old).unwrap();
        old
    }
    #[test]
    fn foreign_expired_and_wrong_project_controls_never_withdraw_a_previous_slot() {
        for kind in 0..4 {
            let host = setup();
            let old = previous_slot(&host);
            let mut main = crate::Main::default();
            let key = draft(&host, &mut main);
            assert_eq!(
                host.run_targets().get(&("/work/app".into(), ID.into())),
                Some(&old)
            );
            let reads = host.process_runs().len();
            let event = match kind {
                0 => ViewEvent::Button("register:foreign".into()),
                1 => ViewEvent::Field("review".into(), "forged target".into()),
                2 => {
                    host.add_project("/work/app");
                    host.set_project_root(Some("/another"));
                    ViewEvent::Button(key.clone())
                }
                _ => {
                    main.run_target.prepared.as_mut().unwrap().at =
                        std::time::Instant::now() - std::time::Duration::from_secs(121);
                    ViewEvent::Button(key.clone())
                }
            };
            assert!(main.handle(REVIEW, event, &host.invocation()).is_err());
            assert_eq!(host.process_runs().len(), reads);
            assert_eq!(
                host.run_targets().get(&("/work/app".into(), ID.into())),
                Some(&old)
            );
            assert!(main.run_target.prepared.is_some());
            let view = main.render(REVIEW, None, &host.invocation()).unwrap();
            if kind >= 2 {
                assert!(matches!(view.body, Body::Message(_)) && view.toolbar.is_empty());
            }
            if kind == 2 {
                host.set_project_root(Some("/work/app"));
                host.answer_next_prompt(None); // Register must not ask another modal.
                main.handle(REVIEW, ViewEvent::Button(key), &host.invocation())
                    .unwrap();
                assert_ne!(
                    host.run_targets().get(&("/work/app".into(), ID.into())),
                    Some(&old)
                );
            }
        }
    }
    #[test]
    fn new_preparation_cancel_and_activation_reset_ticket_without_mutating_old_target() {
        let host = setup();
        let old = previous_slot(&host);
        let mut main = crate::Main::default();
        let first = draft(&host, &mut main);
        let second = draft(&host, &mut main);
        assert_ne!(first, second);
        assert!(
            main.handle(REVIEW, ViewEvent::Button(first), &host.invocation())
                .is_err()
        );
        assert_eq!(
            main.run_target.prepared.as_ref().unwrap().nonce,
            second.strip_prefix("register:").unwrap()
        );
        host.answer_next_prompt(None);
        assert_eq!(
            main.run_command("register-run-target", &host.invocation(), None)
                .unwrap(),
            None
        );
        assert!(main.run_target.prepared.is_none());
        assert!(
            main.handle(REVIEW, ViewEvent::Button(second), &host.invocation())
                .is_err()
        );
        let key = draft(&host, &mut main);
        main.activate(&openidea_plugin::Context::default()).unwrap();
        assert!(main.run_target.prepared.is_none() && main.run_target.status.is_none());
        assert!(
            main.handle(REVIEW, ViewEvent::Button(key), &host.invocation())
                .is_err()
        );
        assert_eq!(
            host.run_targets().get(&("/work/app".into(), ID.into())),
            Some(&old)
        );
        assert!(host.terminals().is_empty());
    }
    #[test]
    fn fresh_commit_rechecks_changed_or_unsaved_configuration_before_any_withdrawal() {
        for unsaved in [false, true] {
            let host = setup();
            let old = previous_slot(&host);
            let mut main = crate::Main::default();
            let key = draft(&host, &mut main);
            let reads = host.process_runs().len();
            if unsaved {
                host.open_editor(
                    "/work/app/.devcontainer/devcontainer.json",
                    "jsonc",
                    "{\"image\":\"changed\"}",
                    &[],
                );
            } else {
                host.add_file(".devcontainer/devcontainer.json", "[]");
            }
            assert!(
                main.handle(REVIEW, ViewEvent::Button(key.clone()), &host.invocation())
                    .is_err()
            );
            assert_eq!(
                host.process_runs().len(),
                reads,
                "stale config must fail before Docker reinspection"
            );
            assert_eq!(
                host.run_targets().get(&("/work/app".into(), ID.into())),
                Some(&old)
            );
            assert!(
                main.run_target.prepared.is_none(),
                "a valid-scope attempt consumes the ticket"
            );
            assert!(
                main.handle(REVIEW, ViewEvent::Button(key), &host.invocation())
                    .is_err()
            );
            match main.render(REVIEW, None, &host.invocation()).unwrap().body {
                Body::Message(message) => assert!(message.starts_with("Not registered")),
                _ => panic!("failed validation status must not claim Registered"),
            }
        }
    }
}
