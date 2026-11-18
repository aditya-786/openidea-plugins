//! Explicit instance-owned LIST→WATCH; positive observations, never complete inventory.
use crate::{ALL_NAMESPACES, config, forward};
use openidea_plugin::host::{Child, Chunk, Command};
use openidea_plugin::views::{Body, Button, Field, FieldValue, View, ViewEvent};
use openidea_plugin::{Error, Invocation, PickItem, host};
use serde_json::Value;
use std::collections::BTreeMap;
use std::hash::BuildHasher;
use std::time::Instant;
pub(crate) const VIEW: &str = "resource-watch";
const MAX_EVENT: usize = 64 << 10;
const MAX_PENDING: usize = 128 << 10;
const MAX_POLLS: usize = 256;
const MAX_RESOURCES: usize = 128;
const KINDS: &[(&str, &str, &str)] = &[
    ("pods", "Pod", "v1"),
    ("services", "Service", "v1"),
    ("deployments", "Deployment", "apps/v1"),
];
type Choices = (Option<String>, Option<String>, Option<String>);
fn choices(context: &Option<String>, namespace: &Option<String>) -> Choices {
    (context.clone(), namespace.clone(), config::kubeconfig())
}
fn capture(ctx: &Invocation, context: Option<&str>, namespace: Option<&str>) -> Result<forward::Scope, Error> {
    let (project, _) = forward::owner(Some(ctx))?;
    forward::Scope::capture_for_project(context, namespace, &project)
}
#[derive(Clone)]
struct Spec {
    root: String,
    kind: usize,
    scope: forward::Scope,
    choices: Choices,
}
impl Spec {
    fn args(&self) -> Vec<String> {
        self.scope.args(&[
            "get",
            KINDS[self.kind].0,
            "--watch",
            "--output-watch-events",
            "-o",
            "json",
            "--request-timeout=10m",
        ])
    }
    fn label(&self) -> String {
        format!("{} · {}", self.scope.label(), KINDS[self.kind].0)
    }
}
struct History {
    label: String,
    names: Vec<String>,
    since: Instant,
}
struct Session {
    spec: Spec,
    child: Option<Child>,
    pending: String,
    names: BTreeMap<String, String>,
    history: Option<History>,
    status: String,
    events: u64,
    skipped: u64,
    skipping: bool,
    at_line_start: bool,
    started: Instant,
    exit: Option<i32>,
    stderr_seen: bool,
}
impl Session {
    /// Drops the current event and everything up to the next line that starts a new one.
    fn skip_event(&mut self, at_line_start: bool) {
        self.skipped = self.skipped.saturating_add(1);
        self.skipping = true;
        self.at_line_start = at_line_start;
        self.drop_until_event_start();
    }
    fn drop_until_event_start(&mut self) {
        let mut at_line_start = self.at_line_start;
        loop {
            if at_line_start && self.pending.starts_with('{') {
                self.skipping = false;
                return;
            }
            let Some(end_of_line) = self.pending.find('\n') else {
                self.at_line_start = at_line_start && self.pending.is_empty();
                self.pending.clear();
                return;
            };
            self.pending.drain(..=end_of_line);
            at_line_start = true;
        }
    }
    fn retire(&mut self, status: &str) {
        self.child = None;
        self.pending.clear();
        self.status = status.to_owned();
    }
    fn apply(&mut self, value: &Value) -> Result<(), &'static str> {
        let event = value
            .get("type")
            .and_then(Value::as_str)
            .ok_or("Watching stopped because kubectl sent an unexpected message.")?;
        if event == "ERROR" {
            return Err(if value["object"]["code"] == 410 {
                "Watching stopped because the cluster no longer has the history it needs (410). Restart to list again."
            } else {
                "Watching stopped because the cluster reported an error. Restart to try again."
            });
        }
        let object = &value["object"];
        let (_, kind, api) = KINDS[self.spec.kind];
        if object["kind"] != kind || object["apiVersion"] != api {
            return Err("Watching stopped because an object had an unexpected kind.");
        }
        let version = object["metadata"]["resourceVersion"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control));
        if version.is_none() {
            return Err("Watching stopped because an object had no usable resource version.");
        }
        if event == "BOOKMARK" {
            self.events = self.events.saturating_add(1);
            return Ok(());
        }
        if !["ADDED", "MODIFIED", "DELETED"].contains(&event) {
            return Err("Watching stopped because of an unknown event type.");
        }
        let name = object["metadata"]["name"]
            .as_str()
            .filter(|n| crate::is_dns_subdomain(n))
            .ok_or("Watching stopped because an object had an invalid name.")?;
        let uid = object["metadata"]["uid"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 128 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
            .ok_or("Watching stopped because an object had an invalid UID.")?;
        if object["metadata"]["namespace"].as_str() != self.spec.args().iter().find_map(|arg| arg.strip_prefix("--namespace=")) {
            return Err("Watching stopped because an object was in another namespace.");
        }
        if event == "DELETED" {
            if self.names.get(name).is_some_and(|known| known == uid) {
                self.names.remove(name);
            }
        } else {
            if !self.names.contains_key(name) && self.names.len() >= MAX_RESOURCES {
                return Err("Watching stopped after 128 names; the names seen so far are kept.");
            }
            self.names.insert(name.into(), uid.into());
        }
        self.events = self.events.saturating_add(1);
        Ok(())
    }
    fn ingest(&mut self, chunk: Chunk) -> Result<bool, &'static str> {
        self.stderr_seen |= !chunk.stderr.is_empty();
        self.pending.push_str(&chunk.stdout);
        if self.skipping {
            self.drop_until_event_start();
        }
        if self.pending.len() > MAX_PENDING {
            self.pending.clear();
            self.skip_event(false);
        }
        while !self.skipping {
            let trimmed = self.pending.trim_start();
            if trimmed.len() != self.pending.len() {
                self.pending = trimmed.to_owned();
            }
            if self.pending.is_empty() {
                break;
            }
            if !stream_depth(&self.pending) {
                self.skip_event(false);
                continue;
            }
            let mut stream = serde_json::Deserializer::from_str(&self.pending).into_iter::<Value>();
            match stream.next() {
                Some(Ok(value)) => {
                    let used = stream.byte_offset();
                    drop(stream);
                    // Reuse bounded duplicate/alias/tag/depth/node rejection; JSON
                    // syntax was established above. Never display raw object data.
                    let acceptable = used <= MAX_EVENT && crate::kubeconfig_import::bounded_structure(&self.pending[..used]).is_ok();
                    self.pending.drain(..used);
                    if !acceptable {
                        self.skipped = self.skipped.saturating_add(1);
                        continue;
                    }
                    self.apply(&value)?;
                }
                Some(Err(error)) if error.is_eof() => break,
                Some(Err(_)) => self.skip_event(false),
                None => break,
            }
        }
        if !self.skipping && self.pending.len() > MAX_EVENT {
            self.pending.clear();
            self.skip_event(false);
        }
        if let Some(exit) = chunk.status {
            self.exit = Some(exit);
            self.status = format!("The process exited with status {exit}; reading its remaining output.");
        }
        if self.exit.is_some() && chunk.stdout.is_empty() && chunk.stderr.is_empty() {
            self.status = if self.pending.is_empty() {
                format!(
                    "The process exited with status {}. The names seen are not a full list.",
                    self.exit.unwrap()
                )
            } else {
                "The process exited in the middle of an event. The names seen are not a full list.".into()
            };
            self.child = None;
            self.pending.clear();
            return Ok(true);
        }
        Ok(false)
    }
    /// Reads until the child has no more output, so a busy namespace never overflows the host's buffer.
    fn poll(&mut self) {
        for _ in 0..MAX_POLLS {
            let Some(child) = self.child.as_ref() else { return };
            let chunk = match child.poll(16 << 10) {
                Ok(chunk) => chunk,
                Err(_) => {
                    self.retire("Watching stopped because the process could not be read.");
                    return;
                }
            };
            let empty = chunk.stdout.is_empty() && chunk.stderr.is_empty();
            match self.ingest(chunk) {
                Ok(true) => return,
                Err(message) => {
                    self.retire(message);
                    return;
                }
                _ => {}
            }
            if empty {
                return;
            }
        }
    }
}
// Bound recursive serde_json allocation before parsing, including incomplete
// events. Braces inside escaped strings do not change structural depth.
fn stream_depth(text: &str) -> bool {
    let mut stack = Vec::new();
    let mut quoted = false;
    let mut escaped = false;
    for byte in text.bytes() {
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
        } else {
            match byte {
                b'"' => quoted = true,
                b'{' | b'[' => {
                    stack.push(byte);
                    if stack.len() > 16 {
                        return false;
                    }
                }
                b'}' if stack.pop() != Some(b'{') => return false,
                b']' if stack.pop() != Some(b'[') => return false,
                _ => {}
            }
        }
    }
    true
}
#[derive(Default)]
pub(crate) struct Watch {
    session: Option<Session>,
    token: String,
    serial: u64,
}
impl Watch {
    fn rotate(&mut self) {
        self.serial = self.serial.wrapping_add(1);
        let r = std::collections::hash_map::RandomState::new();
        self.token = format!(
            "{:016x}{:016x}",
            r.hash_one((self.serial, "watch")),
            r.hash_one((self.serial, "controls"))
        );
    }
    pub(crate) fn clear(&mut self) {
        self.session = None;
        self.rotate();
    }
    pub(crate) fn settings_changed(&mut self, context: &Option<String>, namespace: &Option<String>) {
        let current = choices(context, namespace);
        let mut changed = false;
        if let Some(session) = &mut self.session {
            if session.spec.choices != current && session.child.is_some() {
                session.retire("Watching stopped because the context, namespace or kubeconfig changed.");
                changed = true;
            }
        }
        if changed {
            self.rotate();
            crate::ui_group::refresh_view(VIEW);
        }
    }
    pub(crate) fn closed_roots(&mut self) {
        if self.session.is_none() {
            return;
        }
        let Ok(projects) = host::projects() else {
            self.clear();
            crate::ui_group::refresh_view(VIEW);
            return;
        };
        let open = |root: &str| projects.iter().any(|p| p.root().ok().as_deref() == Some(root));
        let mut changed = false;
        if self.session.as_ref().is_some_and(|s| !open(&s.spec.root)) {
            self.session = None;
            changed = true;
        }
        if changed {
            self.rotate();
            crate::ui_group::refresh_view(VIEW);
        }
    }
    pub(crate) fn prepare(
        &mut self,
        ctx: &Invocation,
        context: &Option<String>,
        namespace: &Option<String>,
        restart: bool,
    ) -> Result<(), Error> {
        self.closed_roots();
        let (_, root) = forward::owner(Some(ctx))?;
        if namespace.as_deref() == Some(ALL_NAMESPACES) {
            return Err("Select one namespace before watching.".into());
        }
        if self.session.as_ref().is_some_and(|s| s.child.is_some() && s.spec.root != root) {
            return Err("An owned watch is running in another project; stop it from that project first.".into());
        }
        self.settings_changed(context, namespace);
        self.rotate();
        let kind = if restart {
            self.session.as_ref().filter(|s| s.spec.root == root).ok_or(Error::Stale)?.spec.kind
        } else {
            let items: Vec<_> = KINDS
                .iter()
                .enumerate()
                .map(|(i, (name, _, _))| PickItem::new(&i.to_string(), name))
                .collect();
            host::pick("Watch resources in the selected namespace", &items)?
                .ok_or(Error::Cancelled)?
                .parse::<usize>()
                .ok()
                .filter(|i| *i < KINDS.len())
                .ok_or(Error::Stale)?
        };
        let scope = capture(ctx, context.as_deref(), namespace.as_deref())?;
        let spec = Spec {
            root,
            kind,
            scope,
            choices: choices(context, namespace),
        };
        let preview = format!(
            "Watch {} in {}?\n\nProject: {}\nCommand: kubectl {:?}\n\nThe watch only reads from the cluster. It lists the names it sees (up to {MAX_RESOURCES}) and keeps running until you stop it.",
            KINDS[spec.kind].0,
            spec.scope.label(),
            spec.root,
            spec.args()
        );
        if !ui_kit::prompts::confirm_exact("Start resource watch?", &preview)? {
            crate::ui_group::refresh_view(VIEW);
            return Ok(());
        }
        let result = self.launch(ctx, context, namespace, spec);
        self.rotate();
        crate::ui_group::refresh_view(VIEW);
        result?;
        crate::ui_group::reveal_view(VIEW)
    }
    fn launch(&mut self, ctx: &Invocation, context: &Option<String>, namespace: &Option<String>, spec: Spec) -> Result<(), Error> {
        let (project, root) = forward::owner(Some(ctx))?;
        if root != spec.root
            || choices(context, namespace) != spec.choices
            || capture(ctx, context.as_deref(), namespace.as_deref())? != spec.scope
            || !spec.scope.configuration_current()
            || self.session.as_ref().is_some_and(|s| s.child.is_some() && s.spec.root != root)
        {
            return Err(Error::Stale);
        }
        let child = host::start(&Command::new("kubectl").args(&spec.args()), Some(&project))?;
        child.close_stdin()?;
        let history = self.session.as_ref().filter(|s| s.spec.root == root).and_then(|s| {
            if s.names.is_empty() {
                s.history.as_ref().map(|h| History {
                    label: h.label.clone(),
                    names: h.names.clone(),
                    since: h.since,
                })
            } else {
                Some(History {
                    label: format!("{}; previous watch", s.spec.label()),
                    names: s.names.keys().cloned().collect(),
                    since: s.started,
                })
            }
        });
        self.session = Some(Session {
            spec,
            child: Some(child),
            pending: String::new(),
            names: BTreeMap::new(),
            history,
            status: "Watching. Use Refresh Watch Status to read new changes.".into(),
            events: 0,
            skipped: 0,
            skipping: false,
            at_line_start: true,
            started: Instant::now(),
            exit: None,
            stderr_seen: false,
        });
        Ok(())
    }
    pub(crate) fn view(&mut self, ctx: &Invocation, context: &Option<String>, namespace: &Option<String>) -> Result<View, Error> {
        self.closed_roots();
        let Ok((_, root)) = forward::owner(Some(ctx)) else {
            return Ok(View::message(
                "Open the owning project with project.read to inspect its resource watch.",
            ));
        };
        if !self.session.as_ref().is_some_and(|s| s.spec.root == root) {
            return Ok(View::message(
                "No resource watch for this project. Run Kube Explorer: Start Resource Watch… explicitly.",
            ));
        }
        self.settings_changed(context, namespace);
        if let Some(s) = self.session.as_mut().filter(|s| s.spec.root == root) {
            s.poll();
        }
        let key = |name: &str| format!("{name}@{}", self.token);
        let mut fields = vec![];
        let mut view = View::new(Body::Message(String::new()));
        if let Some(s) = self.session.as_ref().filter(|s| s.spec.root == root) {
            let skipped = match s.skipped {
                0 => String::new(),
                count => format!("; {count} unreadable or oversized events skipped"),
            };
            fields.push(Field::new(
                "scope",
                "Watching",
                FieldValue::readonly_text(
                    &format!(
                        "{}\n{}\nrunning for {}s; {} events read{skipped}",
                        s.spec.root,
                        s.spec.label(),
                        s.started.elapsed().as_secs(),
                        s.events
                    ),
                    true,
                ),
            ));
            fields.push(Field::new(
                "status",
                "Status",
                FieldValue::readonly_text(
                    &format!(
                        "{}{}",
                        s.status,
                        if s.stderr_seen {
                            "\nThe command also wrote to stderr; it is not shown."
                        } else {
                            ""
                        }
                    ),
                    true,
                ),
            ));
            let names = s.names.keys().take(32).cloned().collect::<Vec<_>>().join("\n");
            fields.push(Field::new(
                "observed",
                "Names seen so far",
                FieldValue::readonly_text(
                    &format!(
                        "Showing up to 32 of {} names seen (up to {MAX_RESOURCES} are kept). A name that is not listed may still exist.\n{names}",
                        s.names.len()
                    ),
                    true,
                ),
            ));
            if let Some(h) = &s.history {
                fields.push(Field::new(
                    "history",
                    "Names from the previous watch",
                    FieldValue::readonly_text(
                        &format!(
                            "{} · started {}s before this watch\n{}",
                            h.label,
                            h.since.elapsed().as_secs(),
                            h.names.iter().take(16).cloned().collect::<Vec<_>>().join("\n")
                        ),
                        true,
                    ),
                ));
            }
            view = view
                .button(Button::new(&key("poll"), "Refresh Watch Status").enabled(s.child.is_some()))
                .button(Button::new(&key("stop"), "Stop Watch").enabled(s.child.is_some()))
                .button(Button::new(&key("restart"), "Restart…"));
        }
        view.body = Body::Form(fields);
        Ok(view.status("Read-only watch of the selected namespace"))
    }
    pub(crate) fn handle(
        &mut self,
        event: ViewEvent,
        ctx: &Invocation,
        context: &Option<String>,
        namespace: &Option<String>,
    ) -> Result<(), Error> {
        self.closed_roots();
        let ViewEvent::Button(id) = event else { return Err(Error::Stale) };
        let (action, token) = id.rsplit_once('@').ok_or(Error::Stale)?;
        let (_, root) = forward::owner(Some(ctx))?;
        if token != self.token || !self.session.as_ref().is_some_and(|s| s.spec.root == root) {
            return Err(Error::Stale);
        }
        self.settings_changed(context, namespace);
        if token != self.token {
            return Err(Error::Stale);
        }
        let result = (|| {
            match action {
                "poll" => {
                    let s = self.session.as_mut().filter(|s| s.spec.root == root).ok_or(Error::Stale)?;
                    if capture(ctx, context.as_deref(), namespace.as_deref()).ok().as_ref() != Some(&s.spec.scope) {
                        s.retire("Watching stopped because the cluster settings could not be checked again.");
                        return Err(Error::Stale);
                    }
                    s.poll();
                }
                "stop" => {
                    let s = self.session.as_mut().filter(|s| s.spec.root == root).ok_or(Error::Stale)?;
                    if let Some(child) = &s.child {
                        child.kill()?;
                    }
                    s.retire("Watch stopped.");
                }
                "restart" => return self.prepare(ctx, context, namespace, true),
                _ => return Err(Error::Stale),
            }
            Ok(())
        })();
        self.rotate();
        crate::ui_group::refresh_view(VIEW);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::KubeExplorer;
    use openidea_plugin::{Plugin, ProcessOutput, mock::MockHost, views::Views};
    use serde_json::json;
    fn setup() -> (MockHost, KubeExplorer) {
        let host = MockHost::install().with_capabilities(&["project.read", "process:kubectl", "storage"]);
        host.on_process("kubectl", |args,_| {
            assert!(args.iter().any(|a|a=="config"), "watch itself must use managed start, not synchronous run_process");
            Ok(ProcessOutput {status:Some(0),stdout:r#"{"contexts":[{"name":"dev","context":{"cluster":"local","namespace":"shop"}}],"clusters":[{"name":"local","cluster":{"server":"https://dev.invalid"}}]}"#.into(),stderr:String::new()})
        });
        (
            host,
            KubeExplorer {
                context: Some("dev".into()),
                namespace: Some("shop".into()),
                ..Default::default()
            },
        )
    }
    fn start(k: &mut KubeExplorer, h: &MockHost) -> Result<Option<String>, Error> {
        h.answer_next_prompt(Some(&["0"]));
        h.answer_next_prompt(Some(&[]));
        k.run_command("watch-resources", &h.invocation(), None)
    }
    fn press(k: &mut KubeExplorer, h: &MockHost, action: &str) -> Result<(), Error> {
        let id = format!("{action}@{}", k.watch.token);
        k.handle(VIEW, ViewEvent::Button(id), &h.invocation())
    }
    fn launch(k: &mut KubeExplorer, h: &MockHost) {
        start(k, h).unwrap();
    }
    fn event(kind: &str, name: &str, uid: &str) -> String {
        json!({"type":kind,"object":{"apiVersion":"v1","kind":"Pod","metadata":{"name":name,"uid":uid,"namespace":"shop","resourceVersion":"opaque/rv-1"},"spec":{"ignoredFloat":0.5,"sensitive":"must-not-display"}}}).to_string()
    }
    fn ingest(k: &mut KubeExplorer, output: &str) -> Result<bool, &'static str> {
        k.watch.session.as_mut().unwrap().ingest(Chunk {
            stdout: output.into(),
            ..Default::default()
        })
    }
    #[test]
    fn watch_starts_after_one_confirm_and_render_never_spawns() {
        let (h, mut k) = setup();
        launch(&mut k, &h);
        let session = k.watch.session.as_ref().unwrap();
        assert!(session.child.is_some());
        assert!(session.spec.args().ends_with(&[
            "get".into(),
            "pods".into(),
            "--watch".into(),
            "--output-watch-events".into(),
            "-o".into(),
            "json".into(),
            "--request-timeout=10m".into()
        ]));
        assert!(session.spec.args().contains(&"--namespace=shop".into()));
        assert!(!session.spec.args().iter().any(|a| a.starts_with("--resource-version")));
        assert!(session.child.as_ref().unwrap().write("not accepted").is_err(), "managed stdin is closed");
        let before = h.process_runs().len();
        k.render(VIEW, None, &h.invocation()).unwrap();
        assert_eq!(h.process_runs().len(), before);
    }
    #[test]
    fn declined_confirm_foreign_root_and_changed_cluster_identity_start_nothing() {
        let (h, mut k) = setup();
        h.answer_next_prompt(Some(&["0"]));
        h.answer_next_prompt(None);
        k.run_command("watch-resources", &h.invocation(), None).unwrap();
        assert!(k.watch.session.is_none());
        h.on_process("kubectl",|_,_|Ok(ProcessOutput{status:Some(0),stdout:r#"{"contexts":[{"name":"dev","context":{"cluster":"local","namespace":"shop"}}],"clusters":[{"name":"local","cluster":{"server":"https://changed.invalid"}}]}"#.into(),stderr:String::new()}));
        launch(&mut k, &h);
        let token = k.watch.token.clone();
        h.add_project("/project");
        h.set_project_root(Some("/other"));
        assert!(k.handle(VIEW, ViewEvent::Button(format!("poll@{token}")), &h.invocation()).is_err());
    }
    #[test]
    fn fragmented_concatenated_events_keep_only_metadata_and_uid_deletion_is_safe() {
        let (h, mut k) = setup();
        launch(&mut k, &h);
        let raw = event("ADDED", "web", "uid-a");
        let split = raw.len() / 2;
        ingest(&mut k, &raw[..split]).unwrap();
        assert!(k.watch.session.as_ref().unwrap().names.is_empty());
        ingest(&mut k, &format!("{}\n{}", &raw[split..], event("ADDED", "other", "uid-b"))).unwrap();
        assert_eq!(k.watch.session.as_ref().unwrap().names.len(), 2);
        ingest(&mut k, &event("ADDED", "web", "uid-new")).unwrap();
        ingest(&mut k, &event("DELETED", "web", "uid-a")).unwrap();
        assert_eq!(
            k.watch.session.as_ref().unwrap().names.get("web").map(String::as_str),
            Some("uid-new")
        );
        let view = k.render(VIEW, None, &h.invocation()).unwrap();
        let text = format!("{:?}", view.body);
        assert!(!text.contains("must-not-display"));
        assert!(text.contains("may still exist"));
        ingest(&mut k, &event("DELETED", "web", "uid-new")).unwrap();
        assert!(!k.watch.session.as_ref().unwrap().names.contains_key("web"));
    }
    #[test]
    fn namespace_kind_duplicate_and_caps_refuse_events_without_losing_accepted_names() {
        let (h, mut k) = setup();
        launch(&mut k, &h);
        ingest(&mut k, &event("ADDED", "good", "uid-a")).unwrap();
        for raw in [
            event("ADDED", "bad", "uid-b").replace("\"shop\"", "\"foreign\""),
            event("ADDED", "bad", "uid-b").replace("\"Pod\"", "\"Service\""),
        ] {
            k.watch.session.as_mut().unwrap().pending.clear();
            assert!(ingest(&mut k, &raw).is_err());
            assert!(k.watch.session.as_ref().unwrap().names.contains_key("good"));
        }
        let duplicate = event("ADDED", "bad", "uid-b").replace("\"type\":\"ADDED\"", "\"type\":\"ADDED\",\"type\":\"DELETED\"");
        assert!(ingest(&mut k, &duplicate).is_ok());
        assert_eq!(k.watch.session.as_ref().unwrap().skipped, 1);
        assert!(!k.watch.session.as_ref().unwrap().names.contains_key("bad"));
        k.watch.session.as_mut().unwrap().pending.clear();
        assert!(ingest(&mut k, &"x".repeat(MAX_PENDING + 1)).is_ok());
        assert_eq!(k.watch.session.as_ref().unwrap().skipped, 2);
        ingest(&mut k, "\n").unwrap();
        for n in 1..MAX_RESOURCES {
            ingest(&mut k, &event("ADDED", &format!("pod-{n}"), &format!("uid-{n}"))).unwrap();
        }
        assert!(ingest(&mut k, &event("ADDED", "overflow", "uid-overflow")).is_err());
        assert_eq!(k.watch.session.as_ref().unwrap().names.len(), MAX_RESOURCES);
    }
    #[test]
    fn bookmarks_410_and_exit_draining_never_claim_complete_inventory() {
        let (h, mut k) = setup();
        launch(&mut k, &h);
        ingest(&mut k, &event("ADDED", "good", "uid-a")).unwrap();
        ingest(
            &mut k,
            r#"{"type":"BOOKMARK","object":{"kind":"Pod","apiVersion":"v1","metadata":{"resourceVersion":"opaque/not-numeric"}}}"#,
        )
        .unwrap();
        assert!(
            ingest(
                &mut k,
                r#"{"type":"ERROR","object":{"kind":"Status","code":410,"message":"private-server-message"}}"#
            )
            .unwrap_err()
            .contains("410")
        );
        let s = k.watch.session.as_mut().unwrap();
        assert!(s.names.contains_key("good"));
        s.pending.clear();
        assert!(
            !s.ingest(Chunk {
                stdout: event("MODIFIED", "good", "uid-a"),
                status: Some(0),
                ..Default::default()
            })
            .unwrap()
        );
        assert!(s.child.is_some());
        assert!(
            s.ingest(Chunk {
                status: Some(0),
                ..Default::default()
            })
            .unwrap()
        );
        assert!(s.child.is_none());
        assert!(s.status.contains("not a full list"));
        assert!(!format!("{:?}", k.render(VIEW, None, &h.invocation()).unwrap().body).contains("private-server-message"));
    }
    #[test]
    fn stop_releases_only_owned_child_and_restart_keeps_separate_history() {
        let (h, mut k) = setup();
        launch(&mut k, &h);
        ingest(&mut k, &event("ADDED", "old", "uid-old")).unwrap();
        let spare = host::start(&Command::new("kubectl"), None).unwrap();
        press(&mut k, &h, "stop").unwrap();
        assert!(k.watch.session.as_ref().unwrap().child.is_none());
        assert!(spare.poll(4096).unwrap().status.is_none());
        h.answer_next_prompt(None);
        press(&mut k, &h, "restart").unwrap();
        assert!(k.watch.session.as_ref().unwrap().child.is_none());
        h.answer_next_prompt(Some(&[]));
        press(&mut k, &h, "restart").unwrap();
        let s = k.watch.session.as_ref().unwrap();
        assert!(s.child.is_some());
        assert!(s.names.is_empty());
        assert_eq!(s.history.as_ref().unwrap().names, ["old"]);
    }
    #[test]
    fn failed_replacement_keeps_previous_owned_watch_and_snapshots() {
        let (h, mut k) = setup();
        launch(&mut k, &h);
        ingest(&mut k, &event("ADDED", "old", "uid-old")).unwrap();
        let spare: Vec<_> = (0..7).map(|_| host::start(&Command::new("kubectl"), None).unwrap()).collect();
        h.answer_next_prompt(Some(&[]));
        assert!(press(&mut k, &h, "restart").is_err());
        let s = k.watch.session.as_ref().unwrap();
        assert!(s.child.is_some());
        assert!(s.names.contains_key("old"));
        assert!(spare[0].poll(4096).unwrap().status.is_none());
    }
    #[test]
    fn foreign_render_preserves_owned_watch_but_scope_change_and_deactivation_release_it() {
        let (h, mut k) = setup();
        launch(&mut k, &h);
        ingest(&mut k, &event("ADDED", "private-name", "uid-a")).unwrap();
        h.add_project("/project");
        h.set_project_root(Some("/other"));
        let before = h.process_runs().len();
        assert!(matches!(k.render(VIEW, None, &h.invocation()).unwrap().body, Body::Message(_)));
        assert!(k.watch.session.as_ref().unwrap().child.is_some());
        assert_eq!(h.process_runs().len(), before);
        h.set_project_root(Some("/project"));
        k.namespace = Some("new".into());
        k.render(VIEW, None, &h.invocation()).unwrap();
        assert!(k.watch.session.as_ref().unwrap().child.is_none());
        assert!(k.watch.session.as_ref().unwrap().names.contains_key("private-name"));
        k.deactivate();
        assert!(k.watch.session.is_none());
    }
    #[test]
    fn deep_or_node_heavy_json_is_skipped_before_observations_change() {
        let (h, mut k) = setup();
        launch(&mut k, &h);
        ingest(&mut k, &event("ADDED", "good", "uid-a")).unwrap();
        let deep = format!("{{\"type\":\"ADDED\",\"object\":{}0{}}}", "[".repeat(40), "]".repeat(40));
        assert!(ingest(&mut k, &format!("{deep}\n")).is_ok());
        assert_eq!(k.watch.session.as_ref().unwrap().skipped, 1);
        assert!(k.watch.session.as_ref().unwrap().names.contains_key("good"));
        k.watch.session.as_mut().unwrap().pending.clear();
        let many=json!({"type":"ADDED","object":{"apiVersion":"v1","kind":"Pod","metadata":{"name":"extra","uid":"uid-b","namespace":"shop","resourceVersion":"rv"},"ignored":vec![0;5000]}}).to_string();
        assert!(ingest(&mut k, &many).is_ok());
        assert_eq!(k.watch.session.as_ref().unwrap().skipped, 2);
        assert!(!k.watch.session.as_ref().unwrap().names.contains_key("extra"));
        assert!(stream_depth(r#"{"text":"escaped \" }[[["}"#));
    }
    #[test]
    fn all_namespaces_refuse_without_watch_start() {
        let (h, mut k) = setup();
        k.namespace = Some(ALL_NAMESPACES.into());
        assert!(k.run_command("watch-resources", &h.invocation(), None).is_err());
        assert!(h.process_runs().is_empty());
        assert!(k.watch.session.is_none());
    }
    #[test]
    fn local_scope_tool_errors_do_not_publish_raw_credential_text() {
        let (h, mut k) = setup();
        h.on_process("kubectl", |_, _| {
            Ok(ProcessOutput {
                status: Some(1),
                stdout: String::new(),
                stderr: "parser secret-token-value auth-url-value".into(),
            })
        });
        h.answer_next_prompt(Some(&["0"]));
        let error = k.run_command("watch-resources", &h.invocation(), None).unwrap_err().to_string();
        assert!(!error.contains("secret-token-value") && !error.contains("auth-url-value"));
        assert!(k.watch.session.is_none());
    }
    #[test]
    fn relative_kubeconfig_inspection_uses_the_same_project_cwd_as_watch_spawn() {
        let (h, mut k) = setup();
        h.set_setting("kubeconfig", "\"configs/relative-kubeconfig\"");
        launch(&mut k, &h);
        assert!(h.process_runs().iter().all(|r| r.cwd.as_deref() == Some("/project")));
        assert!(k.watch.session.as_ref().unwrap().spec.args().contains(&"--kubeconfig=configs/relative-kubeconfig".into()));
        assert_eq!(k.watch.session.as_ref().unwrap().spec.root, "/project");
    }
    #[test]
    fn closed_project_disposes_the_owned_child_and_metadata_without_polling() {
        let (h, mut k) = setup();
        launch(&mut k, &h);
        ingest(&mut k, &event("ADDED", "private-name", "uid-a")).unwrap();
        let before = h.process_runs().len();
        h.set_project_root(None);
        assert!(matches!(k.render(VIEW, None, &h.invocation()).unwrap().body, Body::Message(_)));
        assert!(k.watch.session.is_none());
        assert_eq!(h.process_runs().len(), before);
    }
    #[test]
    fn actual_mock_child_banner_is_skipped_and_revoked_scope_keeps_last_good() {
        let (h, mut k) = setup();
        launch(&mut k, &h);
        ingest(&mut k, &event("ADDED", "good", "uid-a")).unwrap();
        press(&mut k, &h, "poll").unwrap();
        let s = k.watch.session.as_ref().unwrap();
        assert!(s.child.is_some(), "a banner line no longer ends the watch");
        assert!(s.names.contains_key("good"));
        let h = h.with_capabilities(&["project.read", "storage"]);
        assert!(press(&mut k, &h, "poll").is_err());
        assert!(k.watch.session.as_ref().unwrap().child.is_none());
    }
    #[test]
    fn malformed_watch_line_resyncs() {
        let (h, mut k) = setup();
        launch(&mut k, &h);
        let input = format!("not json at all\n{}\n{{\"type\": broken}}\n{}\n", event("ADDED", "first", "uid-1"), event("ADDED", "second", "uid-2"));
        ingest(&mut k, &input).unwrap();
        let s = k.watch.session.as_ref().unwrap();
        assert!(s.child.is_some());
        assert_eq!(s.names.keys().collect::<Vec<_>>(), ["first", "second"]);
        assert_eq!(s.skipped, 2);
        ingest(&mut k, "{\"type\":\"ADD").unwrap();
        ingest(&mut k, &format!("garbage tail\n{}\n", event("ADDED", "third", "uid-3"))).unwrap();
        assert!(k.watch.session.as_ref().unwrap().names.contains_key("third"));
    }
    #[test]
    fn oversized_watch_event_skipped() {
        let (h, mut k) = setup();
        launch(&mut k, &h);
        let big = event("ADDED", "big", "uid-big").replace("must-not-display", &"x".repeat(MAX_EVENT));
        let input = format!("{big}\n{}\n", event("ADDED", "small", "uid-small"));
        for piece in input.as_bytes().chunks(16 << 10) {
            ingest(&mut k, std::str::from_utf8(piece).unwrap()).unwrap();
        }
        let s = k.watch.session.as_ref().unwrap();
        assert!(s.child.is_some());
        assert_eq!(s.names.keys().collect::<Vec<_>>(), ["small"]);
        assert_eq!(s.skipped, 1);
    }
}
