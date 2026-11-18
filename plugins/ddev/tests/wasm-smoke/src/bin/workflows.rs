//! Actual-Wasm command/view workflows with a fixture-backed host; no external process/network/desktop UI.
#![allow(unused_variables)]
use ide_core::CancellationToken;
use ide_extension::ledger::Subject;
use ide_extension::Capability;
use ide_plugin::*;
use std::{path::Path, sync::Arc, time::Duration};
struct WorkflowHost {
    root: std::path::PathBuf,
    status: std::sync::Mutex<String>,
    storage: std::sync::Mutex<std::collections::BTreeMap<String, String>>,
    terminals: std::sync::Mutex<Vec<(String, Vec<String>)>>,
    logs: std::sync::Mutex<Vec<ServiceLog>>,
    urls: std::sync::Mutex<Vec<String>>,
    clipboard: std::sync::Mutex<Option<String>>,
    contexts: std::sync::Mutex<std::collections::BTreeMap<String, String>>,
    process_reads: std::sync::atomic::AtomicUsize,
    sources: std::sync::Mutex<std::collections::BTreeMap<String, PluginDataSource>>,
    credentials: std::sync::Mutex<std::collections::BTreeSet<String>>,
    connections: std::sync::atomic::AtomicUsize,
    database_writes: std::sync::atomic::AtomicUsize,
    credential_writes: std::sync::atomic::AtomicUsize,
    connected: std::sync::atomic::AtomicBool,
    fail_credential_once: std::sync::atomic::AtomicBool,
    fail_describe: std::sync::atomic::AtomicBool,
    targets: std::sync::Mutex<std::collections::BTreeMap<String, PluginRunTarget>>,
    terminal_keys: std::sync::Mutex<std::collections::BTreeMap<String, String>>,
    terminal_counter: std::sync::atomic::AtomicUsize,
    window_status: std::sync::Mutex<std::collections::BTreeMap<String, String>>,
    locations: std::sync::Mutex<Vec<std::path::PathBuf>>,
}
impl WorkflowHost {
    fn new(root: std::path::PathBuf) -> Self {
        Self {
            root,
            status: std::sync::Mutex::new("running".into()),
            storage: Default::default(),
            terminals: Default::default(),
            logs: Default::default(),
            urls: Default::default(),
            clipboard: Default::default(),
            contexts: Default::default(),
            process_reads: Default::default(),
            sources: Default::default(),
            credentials: Default::default(),
            connections: Default::default(),
            database_writes: Default::default(),
            credential_writes: Default::default(),
            connected: Default::default(),
            fail_credential_once: Default::default(),
            fail_describe: Default::default(),
            targets: Default::default(),
            terminal_keys: Default::default(),
            terminal_counter: Default::default(),
            window_status: Default::default(),
            locations: Default::default(),
        }
    }
}
impl HostApi for WorkflowHost {
    fn database_upsert(&self, root: &Path, source: PluginDataSource) -> Result<(), HostError> {
        assert_eq!(root, self.root);
        assert_eq!(source.port, Some(32850));
        assert!(
            !serde_json::to_string(&source)
                .unwrap()
                .contains("fixture-only-secret")
        );
        self.database_writes
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.connected
            .store(false, std::sync::atomic::Ordering::SeqCst);
        self.sources
            .lock()
            .unwrap()
            .insert(source.id.clone(), source);
        Ok(())
    }
    fn database_sources(&self, root: &Path) -> Result<Vec<String>, HostError> {
        assert_eq!(root, self.root);
        Ok(self.sources.lock().unwrap().keys().cloned().collect())
    }
    fn database_credential(
        &self,
        root: &Path,
        id: &str,
        password: String,
    ) -> Result<(), HostError> {
        assert_eq!(root, self.root);
        assert!(self.sources.lock().unwrap().contains_key(id));
        assert_eq!(password, "fixture-only-secret");
        if self
            .fail_credential_once
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(HostError::Failed(
                "fixture credential-store write failed".into(),
            ));
        }
        self.credential_writes
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.credentials.lock().unwrap().insert(id.into());
        Ok(())
    }
    fn database_connect(
        &self,
        root: &Path,
        client: &str,
        id: &str,
        cancel: &CancellationToken,
        timeout: Duration,
    ) -> Result<(), HostError> {
        assert_eq!(root, self.root);
        assert_eq!(client, "fixture-window");
        assert!(self.credentials.lock().unwrap().contains(id));
        self.connected
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.connections
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
    fn register_run_target(&self, root: &Path, target: PluginRunTarget) -> Result<(), HostError> {
        assert_eq!(root, self.root);
        assert_eq!(target.args, ["exec", "--raw"]);
        assert_eq!(target.workdir_args, ["--dir={cwd}", "--"]);
        self.targets
            .lock()
            .unwrap()
            .insert(target.id.clone(), target);
        Ok(())
    }
    fn unregister_run_target(&self, root: &Path, id: &str) {
        self.targets.lock().unwrap().remove(id);
    }
    fn window_status_item(
        &self,
        client: &str,
        id: &str,
        item: Option<StatusItem>,
    ) -> Result<(), HostError> {
        assert_eq!(client, "fixture-window");
        if let Some(item) = item {
            self.window_status
                .lock()
                .unwrap()
                .insert(id.into(), item.text);
        } else {
            self.window_status.lock().unwrap().remove(id);
        }
        Ok(())
    }
    fn open_terminal_session(
        &self,
        client: &str,
        key: String,
        title: String,
        cmd: ProcessCommand,
        cancel: &CancellationToken,
        timeout: Duration,
    ) -> Result<String, HostError> {
        assert_eq!(client, "fixture-window");
        assert_eq!(cmd.program, "ddev");
        assert_eq!(cmd.approval_root.as_deref(), Some(self.root.as_path()));
        assert_eq!(cmd.env, [("DDEV_NONINTERACTIVE".into(), "true".into())]);
        self.terminals.lock().unwrap().push((cmd.program, cmd.args));
        let mut keys = self.terminal_keys.lock().unwrap();
        let next = format!(
            "fixture-operation-{}",
            self.terminal_counter
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        );
        Ok(keys.entry(key).or_insert(next).clone())
    }

    fn set_context(
        &self,
        client: Option<&str>,
        key: &str,
        value: Option<String>,
    ) -> Result<(), HostError> {
        let mut keys = self.contexts.lock().unwrap();
        if let Some(value) = value {
            keys.insert(key.into(), value);
        } else {
            keys.remove(key);
        }
        Ok(())
    }

    fn notify(
        &self,
        client: Option<&str>,
        level: NotificationLevel,
        title: String,
        body: String,
        actions: Vec<ide_plugin::NotificationAction>,
    ) -> Result<(), HostError> {
        Ok(())
    }
    fn status_item(&self, id: &str, item: Option<StatusItem>) {}
    fn projects(&self) -> Vec<std::path::PathBuf> {
        vec![self.root.clone()]
    }
    fn buffer_text(&self, path: &Path) -> Option<String> {
        None
    }
    fn document(&self, path: &Path) -> Option<(u64, String)> {
        None
    }
    fn apply_edits(
        &self,
        client: Option<&str>,
        path: &Path,
        version: u64,
        edits: Vec<ide_core::TextEdit>,
    ) -> Result<u64, HostError> {
        panic!("Unexpected host callback: apply_edits")
    }
    fn storage_get(&self, key: &str) -> Result<Option<String>, HostError> {
        Ok(self.storage.lock().unwrap().get(key).cloned())
    }
    fn storage_set(&self, key: &str, value: Option<String>) -> Result<(), HostError> {
        let mut data = self.storage.lock().unwrap();
        if let Some(value) = value {
            data.insert(key.into(), value);
        } else {
            data.remove(key);
        }
        Ok(())
    }
    fn storage_keys(&self, prefix: &str) -> Result<Vec<String>, HostError> {
        Ok(self
            .storage
            .lock()
            .unwrap()
            .keys()
            .filter(|key| key.starts_with(prefix))
            .cloned()
            .collect())
    }
    fn vcs_status(&self, root: &Path) -> Result<Option<RepositoryStatus>, HostError> {
        panic!("Unexpected host callback: vcs_status")
    }
    fn vcs_fetch(&self, root: &Path, cancel: &CancellationToken) -> Result<(), HostError> {
        panic!("Unexpected host callback: vcs_fetch")
    }
    fn find_files(&self, root: &Path, glob: &str, limit: usize) -> Result<Vec<String>, HostError> {
        panic!("Unexpected host callback: find_files")
    }
    fn publish_diagnostics(
        &self,
        path: &Path,
        items: Vec<ide_core::Diagnostic>,
    ) -> Result<(), HostError> {
        panic!("Unexpected host callback: publish_diagnostics")
    }
    fn fetch(
        &self,
        request: HttpRequest,
        cancel: &CancellationToken,
    ) -> Result<HttpResponse, HostError> {
        assert_eq!(
            request.url,
            "https://api.github.com/repos/ddev/ddev/releases/latest"
        );
        Ok(HttpResponse {
            status: 200,
            headers: vec![],
            body: br#"{"tag_name":"v1.25.3"}"#.to_vec(),
        })
    }
    fn secret_is_set(&self, name: &str) -> bool {
        panic!("Unexpected host callback: secret_is_set")
    }
    fn run_process(
        &self,
        cmd: ProcessCommand,
        cwd: Option<&Path>,
        stdin: Option<String>,
        cancel: &CancellationToken,
        timeout: Duration,
    ) -> Result<ProcessOutput, HostError> {
        self.process_reads
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(cmd.program, "ddev");
        assert_eq!(cwd, Some(self.root.as_path()));
        if cmd.args[0] == "describe" && self.fail_describe.load(std::sync::atomic::Ordering::SeqCst)
        {
            return Ok(ProcessOutput {
                status: Some(1),
                stdout: String::new(),
                stderr: "fixture describe unavailable".into(),
            });
        }
        let stdout = match cmd.args[0].as_str() {
            "--version" => "ddev version v1.25.3\n".into(),
            "version" => include_str!("../../../fixtures/ddev_version.json").into(),
            "describe" => {
                let mut data: serde_json::Value =
                    serde_json::from_str(include_str!("../../../fixtures/ddev_describe.json"))
                        .unwrap();
                data["raw"]["name"] = serde_json::json!("demo");
                data["raw"]["status"] = serde_json::json!(*self.status.lock().unwrap());
                data["raw"]["services"] = serde_json::json!({"web":{"status":"running","https_url":"https://demo.ddev.site"},"db":{"status":"running"}});
                data["raw"]["dbinfo"] = serde_json::json!({"database_type":"mariadb","database_version":"10.11","published_port":32850,"dbname":"db","username":"db","password":"fixture-only-secret"});
                data.to_string()
            }
            _ => panic!("unexpected process {:?}", cmd.args),
        };
        Ok(ProcessOutput {
            status: Some(0),
            stdout,
            stderr: String::new(),
        })
    }
    fn subscribe(&self, kind: EventKind) {}
    fn subscribe_paths(&self, kind: &str, globs: Vec<String>) -> Result<(), HostError> {
        Ok(())
    }
    fn unsubscribe(&self, kind: &str) {}
    fn start_process(
        &self,
        cmd: ProcessCommand,
        cwd: Option<&Path>,
        client: Option<&str>,
    ) -> Result<u64, HostError> {
        panic!("Unexpected host callback: start_process")
    }
    fn poll_process(&self, id: u64, max: usize) -> Result<ProcessChunk, HostError> {
        panic!("Unexpected host callback: poll_process")
    }
    fn write_process(
        &self,
        id: u64,
        text: &str,
        cancel: &CancellationToken,
        timeout: Duration,
    ) -> Result<(), HostError> {
        panic!("Unexpected host callback: write_process")
    }
    fn close_process_stdin(&self, id: u64) -> Result<(), HostError> {
        panic!("Unexpected host callback: close_process_stdin")
    }
    fn kill_process(&self, id: u64) -> Result<(), HostError> {
        panic!("Unexpected host callback: kill_process")
    }
    fn release_process(&self, id: u64) {
        panic!("Unexpected host callback: release_process")
    }
    fn wait_process(
        &self,
        id: u64,
        max: usize,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> Result<ProcessChunk, HostError> {
        panic!("Unexpected host callback: wait_process")
    }
    fn task_begin(
        &self,
        client: Option<&str>,
        title: String,
        cancellable: bool,
        cancel: CancellationToken,
    ) -> u64 {
        1
    }
    fn task_report(&self, id: u64, done: u32, total: Option<u32>, message: Option<String>) {}
    fn task_cancelled(&self, id: u64) -> bool {
        false
    }
    fn task_end(&self, id: u64) {}
    fn refresh_view(&self, view: &str) {}
    fn post_view_message(&self, view: &str, message: String) -> Result<(), HostError> {
        assert!(
            matches!(view, "ddev" | "setup"),
            "Unexpected view message: {view}"
        );
        Ok(())
    }
    fn open_terminal(
        &self,
        client: Option<&str>,
        title: String,
        program: &str,
        args: Vec<String>,
        cwd: Option<&Path>,
    ) -> Result<(), HostError> {
        assert_eq!(cwd, Some(self.root.as_path()));
        self.terminals.lock().unwrap().push((program.into(), args));
        Ok(())
    }
    fn open_location(
        &self,
        client: Option<&str>,
        path: &Path,
        line: u32,
        column: u32,
    ) -> Result<(), HostError> {
        assert!(path.starts_with(&self.root));
        self.locations.lock().unwrap().push(path.to_path_buf());
        Ok(())
    }
    fn open_log_tab(
        &self,
        client: &str,
        key: String,
        title: String,
        log: ServiceLog,
    ) -> Result<(), HostError> {
        self.logs.lock().unwrap().push(log);
        Ok(())
    }
    fn locale(&self) -> String {
        "en".into()
    }
    fn setting(&self, root: Option<&Path>, key: &str) -> Option<String> {
        if key == "checkForUpdates" {
            Some("false".into())
        } else {
            None
        }
    }
    fn read_text_file(&self, real: &Path, max: usize) -> Result<String, HostError> {
        assert!(real.starts_with(&self.root));
        let data = std::fs::read_to_string(real).map_err(|e| HostError::NotFound(e.to_string()))?;
        assert!(data.len() <= max);
        Ok(data)
    }
    fn write_text_file(&self, real: &Path, text: &str) -> Result<(), HostError> {
        assert!(real.starts_with(&self.root));
        std::fs::write(real, text).map_err(|e| HostError::Failed(e.to_string()))
    }
    fn write_bytes_file(&self, real: &Path, bytes: &[u8]) -> Result<(), HostError> {
        assert!(real.starts_with(&self.root));
        std::fs::write(real, bytes).map_err(|e| HostError::Failed(e.to_string()))
    }
    fn ask(
        &self,
        client: Option<&str>,
        prompt: Prompt,
        cancel: &CancellationToken,
    ) -> Result<Option<Vec<String>>, HostError> {
        Ok(match prompt {
            Prompt::Confirm { .. } => Some(vec!["ok".into()]),
            Prompt::Input { .. } => Some(vec!["--version".into()]),
            _ => None,
        })
    }
    fn copy_to_clipboard(&self, client: Option<&str>, text: String) -> Result<(), HostError> {
        *self.clipboard.lock().unwrap() = Some(text);
        Ok(())
    }
    fn open_url(&self, client: Option<&str>, url: String) -> Result<(), HostError> {
        self.urls.lock().unwrap().push(url);
        Ok(())
    }
    fn reveal_view(&self, client: Option<&str>, view: &str) -> Result<(), HostError> {
        Ok(())
    }
}
fn main() {
    let wasm = std::env::args()
        .nth(1)
        .expect("usage: workflows <dist/plugin.wasm>");
    let directory = std::env::temp_dir().join(format!(
        "ddev-wasm-workflows-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(directory.join(".ddev")).unwrap();
    std::fs::create_dir_all(directory.join(".openidea")).unwrap();
    let root = std::fs::canonicalize(&directory).unwrap();
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(root.clone());
    std::fs::write(root.join(".ddev/config.yaml"), "name: demo\ntype: php\n").unwrap();
    let host = Arc::new(WorkflowHost::new(root.clone()));
    let engine = PluginEngine::new().unwrap();
    let component = engine.compile(Path::new(&wasm)).unwrap();
    let grants = vec![
        Capability::ProjectRead,
        Capability::ProjectWrite,
        Capability::DatabaseSources,
        Capability::Process("ddev".into()),
        Capability::Storage,
        Capability::Events,
        Capability::Clipboard,
        Capability::Net("api.github.com".into()),
    ];
    let mut runtime = Runtime::instantiate(
        &engine,
        &component,
        RuntimeSetup {
            plugin_id: "aritti.ddev".into(),
            version: "0.1.0".into(),
            granted: grants,
            subject: Subject::Installed("aritti.ddev".into()),
            host: host.clone(),
            limits: Limits::default(),
        },
    )
    .unwrap();
    let invocation = Invocation {
        client: Some("fixture-window".into()),
        workspace_root: Some(root.clone()),
        ..Default::default()
    };
    let mut exercised = std::collections::BTreeSet::new();
    runtime.activate().unwrap();
    let services = runtime
        .render_view("ddev", None, invocation.clone())
        .unwrap();
    assert!(
        matches!(services.body,ViewBody::Tree(ref rows) if rows.iter().any(|row| row.key=="service:db"))
    );
    let details = runtime
        .service_detail("ddev", "project", invocation.clone())
        .unwrap();
    for tab in ["inspect", "logs", "database", "configuration", "help"] {
        assert!(details.iter().any(|t| t.id == tab), "missing {tab}");
    }
    runtime
        .view_event(
            "ddev",
            ViewEvent::Select {
                key: "service:db".into(),
            },
            invocation.clone(),
        )
        .unwrap();
    runtime
        .view_event(
            "ddev",
            ViewEvent::Button { id: "shell".into() },
            invocation.clone(),
        )
        .unwrap();
    assert_eq!(
        host.terminals.lock().unwrap().last().unwrap().1,
        ["ssh", "--service=db"]
    );
    for (id, arg) in [
        ("shell", Some("service:db")),
        ("logs", Some("project")),
        ("open-service", Some("service:web")),
        ("open-config", None),
        ("setup-database", None),
        ("copy-db-password", None),
        ("diagnose", None),
        ("check-updates", None),
        ("help", None),
        ("setup", None),
        ("show-services", None),
        ("open-guide", None),
        ("open-releases", None),
        ("open-docs", None),
    ] {
        command(&mut runtime, id, arg, invocation.clone(), &mut exercised);
    }
    assert!(
        host.logs
            .lock()
            .unwrap()
            .iter()
            .any(|log| log.args[1] == "demo")
    );
    assert!(
        host.urls
            .lock()
            .unwrap()
            .iter()
            .any(|url| url.starts_with("https://"))
    );
    assert_eq!(
        host.clipboard.lock().unwrap().as_deref(),
        Some("fixture-only-secret")
    );
    assert!(!root.join(".openidea/data-sources.toml").exists());
    assert_eq!(host.sources.lock().unwrap().len(), 1);
    assert_eq!(host.credentials.lock().unwrap().len(), 1);
    assert!(host.connections.load(std::sync::atomic::Ordering::SeqCst) > 0);
    assert_eq!(host.targets.lock().unwrap().len(), 1);
    let writes = host
        .database_writes
        .load(std::sync::atomic::Ordering::SeqCst);
    let credentials = host
        .credential_writes
        .load(std::sync::atomic::Ordering::SeqCst);
    for _ in 0..2 {
        command(
            &mut runtime,
            "refresh",
            None,
            invocation.clone(),
            &mut exercised,
        );
    }
    runtime
        .handle_events(vec![WorkbenchEvent::Scheduled], Invocation::default())
        .unwrap();
    assert_eq!(
        host.database_writes
            .load(std::sync::atomic::Ordering::SeqCst),
        writes,
        "unchanged refresh must not upsert/disconnect the native source"
    );
    assert_eq!(
        host.credential_writes
            .load(std::sync::atomic::Ordering::SeqCst),
        credentials,
        "unchanged refresh must not rewrite the secure credential"
    );
    assert!(host.connected.load(std::sync::atomic::Ordering::SeqCst));
    // A failed explicit reapply has already disconnected the native source.
    // Its cache must be cleared so a later refresh retries and reconnects.
    host.fail_credential_once
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(
        runtime
            .run_command("setup-database", None, invocation.clone())
            .is_err()
    );
    assert!(!host.connected.load(std::sync::atomic::Ordering::SeqCst));
    command(
        &mut runtime,
        "refresh",
        None,
        invocation.clone(),
        &mut exercised,
    );
    assert!(host.connected.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(
        host.database_writes
            .load(std::sync::atomic::Ordering::SeqCst),
        writes + 2
    );
    assert_eq!(
        host.credential_writes
            .load(std::sync::atomic::Ordering::SeqCst),
        credentials + 1
    );
    assert!(
        !root.join(".openidea/data-sources.toml").exists(),
        "secure-store failure must not switch to a file fallback"
    );

    // Simulate managed metadata carried by a moved project. Reuse its owned
    // identity instead of creating a second source from the new path seed.
    let mut moved = host
        .sources
        .lock()
        .unwrap()
        .values()
        .next()
        .unwrap()
        .clone();
    moved.id = "aritti.ddev.1234567890abcdef".into();
    host.sources.lock().unwrap().clear();
    host.sources.lock().unwrap().insert(moved.id.clone(), moved);
    command(
        &mut runtime,
        "refresh",
        None,
        invocation.clone(),
        &mut exercised,
    );
    assert_eq!(
        host.sources
            .lock()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        ["aritti.ddev.1234567890abcdef"]
    );
    assert!(host.connected.load(std::sync::atomic::Ordering::SeqCst));
    // Check argv/path construction through the real host target implementation.
    let target = host.targets.lock().unwrap().get("ddev").unwrap().clone();
    if let Some(live_root) = std::env::var_os("DDEV_LIVE_PROJECT") {
        live_target_probe(Path::new(&live_root), &target);
    }
    for tool in ["php", "node", "composer"] {
        let config: ide_run::RunConfig = serde_json::from_value(serde_json::json!({"id":"probe","name":"Probe","kind":"shell","command":format!("/usr/bin/{tool}"),"args":[root.join("public/index.php").to_string_lossy(),"a b", "$(literal)", ""],"cwd":"public","env":{},"detected":false,"isTest":false})).unwrap();
        let wrapped = ide_remote::plugin_target::prepare(
            &root,
            &target,
            Path::new("/approved/ddev"),
            &config,
        )
        .unwrap();
        assert_eq!(wrapped.command, "/approved/ddev");
        let argv = [tool, "/var/www/html/public/index.php", "a b", "$(literal)", ""];
        // The composer override runs `ddev composer` without the exec prefix or workdir args.
        let expected: Vec<&str> = match tool {
            "composer" => argv.to_vec(),
            _ => ["exec", "--raw", "--dir=/var/www/html/public", "--"].into_iter().chain(argv).collect(),
        };
        assert_eq!(wrapped.args, expected);
    }
    for id in ["php", "node", "composer"] {
        command(&mut runtime, id, None, invocation.clone(), &mut exercised);
        let calls = host.terminals.lock().unwrap();
        let args = &calls.last().unwrap().1;
        if id != "composer" {
            assert_eq!(&args[..4], ["exec", "--raw", "--", id]);
        }
    }
    for id in ["restart", "stop", "share", "poweroff", "delete", "config"] {
        command(&mut runtime, id, None, invocation.clone(), &mut exercised);
    }
    // Deliver the real API 1.4 extension through the compiled event export.
    let (key, id) = host
        .terminal_keys
        .lock()
        .unwrap()
        .iter()
        .find(|(key, _)| key.contains(":config:"))
        .map(|(key, id)| (key.clone(), id.clone()))
        .unwrap();
    let locations = host.locations.lock().unwrap().len();
    // The old process has ended, but its exit may still be queued. Launching
    // an identical operation gets a new id despite reusing the session key.
    host.terminal_keys.lock().unwrap().remove(&key);
    command(
        &mut runtime,
        "config",
        None,
        invocation.clone(),
        &mut exercised,
    );
    let current_id = host
        .terminal_keys
        .lock()
        .unwrap()
        .get(&key)
        .unwrap()
        .clone();
    assert_ne!(current_id, id);
    runtime
        .handle_events(
            vec![WorkbenchEvent::TerminalExited(TerminalExited {
                id: id.clone(),
                key: key.clone(),
                status: Some(0),
                closed: false,
            })],
            invocation.clone(),
        )
        .unwrap();
    assert_eq!(
        host.locations.lock().unwrap().len(),
        locations,
        "old exit must not consume the new pending configuration"
    );
    let id = current_id;

    runtime
        .handle_events(
            vec![WorkbenchEvent::TerminalExited(TerminalExited {
                id,
                key,
                status: Some(0),
                closed: false,
            })],
            invocation.clone(),
        )
        .unwrap();
    assert_eq!(host.locations.lock().unwrap().len(), locations + 1);
    assert_eq!(
        host.window_status
            .lock()
            .unwrap()
            .get("ddev-status")
            .map(String::as_str),
        Some("DDEV: running")
    );
    runtime
        .render_view("setup", None, invocation.clone())
        .unwrap();
    runtime
        .view_event(
            "setup",
            ViewEvent::Field {
                key: "phpVersion".into(),
                value: "8.3".into(),
            },
            invocation.clone(),
        )
        .unwrap();
    runtime
        .view_event(
            "setup",
            ViewEvent::Button {
                id: "save-settings".into(),
            },
            invocation.clone(),
        )
        .unwrap();
    assert!(
        std::fs::read_to_string(root.join(".openidea/settings.toml"))
            .unwrap()
            .contains("8.3")
    );
    runtime
        .view_event(
            "setup",
            ViewEvent::Button {
                id: "reset-settings".into(),
            },
            invocation.clone(),
        )
        .unwrap();
    command(
        &mut runtime,
        "disable-update-checks",
        None,
        invocation.clone(),
        &mut exercised,
    );
    *host.status.lock().unwrap() = "stopped".into();
    command(
        &mut runtime,
        "refresh",
        None,
        invocation.clone(),
        &mut exercised,
    );
    command(
        &mut runtime,
        "start",
        None,
        invocation.clone(),
        &mut exercised,
    );
    assert!(
        runtime
            .run_command("stop", None, invocation.clone())
            .is_err()
    );
    command(
        &mut runtime,
        "actions",
        None,
        invocation.clone(),
        &mut exercised,
    ); // picker cancellation
    runtime
        .handle_events(
            vec![WorkbenchEvent::FileSaved(
                root.join(".ddev/config.local.yaml")
                    .to_string_lossy()
                    .into(),
            )],
            invocation.clone(),
        )
        .unwrap();
    assert_eq!(
        host.contexts
            .lock()
            .unwrap()
            .get("running")
            .map(String::as_str),
        Some("false")
    );
    *host.status.lock().unwrap() = "running".into();
    runtime
        .handle_events(vec![WorkbenchEvent::Scheduled], Invocation::default())
        .unwrap();
    let reads = host.process_reads.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        host.contexts
            .lock()
            .unwrap()
            .get("running")
            .map(String::as_str),
        Some("false")
    );
    runtime
        .handle_events(
            vec![WorkbenchEvent::EditorFocused(Some(
                root.join("index.php").to_string_lossy().into(),
            ))],
            invocation.clone(),
        )
        .unwrap();
    assert_eq!(
        host.contexts
            .lock()
            .unwrap()
            .get("running")
            .map(String::as_str),
        Some("true")
    );
    assert_eq!(
        host.process_reads.load(std::sync::atomic::Ordering::SeqCst),
        reads,
        "focus must reuse the background snapshot"
    );
    let mut restricted = Runtime::instantiate(
        &engine,
        &component,
        RuntimeSetup {
            plugin_id: "aritti.ddev".into(),
            version: "0.1.0".into(),
            granted: vec![Capability::ProjectRead],
            subject: Subject::Installed("aritti.ddev".into()),
            host: host.clone(),
            limits: Limits::default(),
        },
    )
    .unwrap();
    restricted.activate().unwrap();
    let safe = restricted
        .service_detail("ddev", "project", invocation.clone())
        .unwrap();
    assert!(safe.iter().any(|tab| tab.id == "configuration"));
    assert!(matches!(
        restricted
            .render_view("setup", None, invocation.clone())
            .unwrap()
            .body,
        ViewBody::Form(_)
    ));
    assert!(
        restricted
            .run_command("start", None, invocation.clone())
            .is_err()
    );
    restricted
        .run_command("open-config", None, invocation.clone())
        .unwrap();
    assert_eq!(
        exercised.len(),
        27,
        "all current manifest commands must be exercised"
    );
    host.fail_describe
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(
        runtime
            .run_command("refresh", None, invocation.clone())
            .is_err()
    );
    let recovery = runtime
        .render_view("ddev", None, invocation.clone())
        .unwrap();
    assert!(
        recovery
            .toolbar
            .iter()
            .any(|button| button.id == "stop" && button.enabled)
    );
    assert_eq!(
        host.contexts
            .lock()
            .unwrap()
            .get("can-restart-stop")
            .map(String::as_str),
        Some("true")
    );
    for action in ["restart", "stop"] {
        command(
            &mut runtime,
            action,
            None,
            invocation.clone(),
            &mut exercised,
        );
        assert_eq!(host.terminals.lock().unwrap().last().unwrap().1, [action]);
    }
    host.fail_describe
        .store(false, std::sync::atomic::Ordering::SeqCst);
    std::fs::remove_file(root.join(".ddev/config.yaml")).unwrap();
    runtime
        .handle_events(
            vec![WorkbenchEvent::FileSaved(
                root.join(".ddev/config.yaml").to_string_lossy().into(),
            )],
            invocation.clone(),
        )
        .unwrap();
    assert!(
        host.targets.lock().unwrap().is_empty(),
        "remove execution target when configuration disappears"
    );
    println!(
        "Wasm workflows passed: activation, Services/detail, selection/shell/logs/URLs, 27 commands, settings, state guards, events and denied-runtime fallback"
    );
}

fn command(
    runtime: &mut Runtime,
    id: &str,
    arg: Option<&str>,
    invocation: Invocation,
    exercised: &mut std::collections::BTreeSet<String>,
) {
    runtime
        .run_command(id, arg, invocation)
        .unwrap_or_else(|error| panic!("command {id}: {error:?}"));
    exercised.insert(id.to_owned());
}

// Opt-in only: use the target registered by the compiled component and the
// real host's argv/path wrapper against the disposable live-smoke project.
fn live_target_probe(root: &Path, target: &PluginRunTarget) {
    let root = std::fs::canonicalize(root).unwrap();
    assert!(
        root.file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("ddev-live-parity-")
    );
    let directory = root.join("app/sub dir");
    assert!(directory.is_dir());
    for (tool, filename, source) in [
        (
            "php",
            "probe.php",
            "<?php echo json_encode(['cwd'=>getcwd(),'args'=>array_slice($argv,1)]);",
        ),
        (
            "node",
            "probe.js",
            "console.log(JSON.stringify({cwd:process.cwd(),args:process.argv.slice(2)}));",
        ),
    ] {
        let file = directory.join(filename);
        std::fs::write(&file, source).unwrap();
        let config: ide_run::RunConfig = serde_json::from_value(serde_json::json!({
            "id":"live-probe", "name":"Live probe", "kind":"shell", "command":tool,
            "args":[file.to_string_lossy(),"a b","$HOME;`literal`",""],
            "cwd":"app/sub dir", "env":{}, "detected":false, "isTest":false
        }))
        .unwrap();
        let wrapped =
            ide_remote::plugin_target::prepare(&root, target, Path::new("ddev"), &config).unwrap();
        let result = std::process::Command::new(&wrapped.command)
            .args(&wrapped.args)
            .env("DDEV_NONINTERACTIVE", "true")
            .current_dir(wrapped.cwd.as_ref().unwrap())
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let output: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(output["cwd"], "/var/www/html/app/sub dir");
        assert_eq!(
            output["args"],
            serde_json::json!(["a b", "$HOME;`literal`", ""])
        );
    }
    println!(
        "Live PHP/Node target passed: compiled-Wasm registration, actual host wrapper, mapped cwd/path and literal argv"
    );
}
