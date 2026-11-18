//! Runs the built component under the actual v0.1.39 runtime and default
//! fuel/memory limits, with a 50 ms deadline on each editing provider.
//! Unexpected host callbacks panic: editing must need neither processes nor
//! network, storage, project file reads, or a desktop window.
#![allow(unused_variables)]
use ide_core::CancellationToken;
use ide_extension::ledger::Subject;
use ide_extension::Capability;
use ide_plugin::*;
use std::{
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};
struct ProbeHost;
impl HostApi for ProbeHost {
    fn notify(
        &self,
        client: Option<&str>,
        level: NotificationLevel,
        title: String,
        body: String,
        actions: Vec<ide_plugin::NotificationAction>,
    ) -> Result<(), HostError> {
        panic!("Unexpected host callback: notify")
    }
    fn status_item(&self, id: &str, item: Option<StatusItem>) {
        panic!("Unexpected host callback: status_item")
    }
    fn projects(&self) -> Vec<std::path::PathBuf> {
        vec![]
    }
    fn buffer_text(&self, path: &Path) -> Option<String> {
        panic!("Unexpected host callback: buffer_text")
    }
    fn document(&self, path: &Path) -> Option<(u64, String)> {
        panic!("Unexpected host callback: document")
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
        panic!("Unexpected host callback: storage_get")
    }
    fn storage_set(&self, key: &str, value: Option<String>) -> Result<(), HostError> {
        panic!("Unexpected host callback: storage_set")
    }
    fn storage_keys(&self, prefix: &str) -> Result<Vec<String>, HostError> {
        panic!("Unexpected host callback: storage_keys")
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
        panic!("Unexpected host callback: fetch")
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
        panic!("Unexpected host callback: run_process")
    }
    fn subscribe(&self, kind: EventKind) {
        panic!("Unexpected host callback: subscribe")
    }
    fn subscribe_paths(&self, kind: &str, globs: Vec<String>) -> Result<(), HostError> {
        panic!("Unexpected host callback: subscribe_paths")
    }
    fn unsubscribe(&self, kind: &str) {
        panic!("Unexpected host callback: unsubscribe")
    }
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
        panic!("Unexpected host callback: task_begin")
    }
    fn task_report(&self, id: u64, done: u32, total: Option<u32>, message: Option<String>) {
        panic!("Unexpected host callback: task_report")
    }
    fn task_cancelled(&self, id: u64) -> bool {
        panic!("Unexpected host callback: task_cancelled")
    }
    fn task_end(&self, id: u64) {
        panic!("Unexpected host callback: task_end")
    }
    fn refresh_view(&self, view: &str) {
        assert!(
            matches!(view, "ddev" | "setup"),
            "Unexpected view refresh: {view}"
        );
    }
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
        panic!("Unexpected host callback: open_terminal")
    }
    fn open_location(
        &self,
        client: Option<&str>,
        path: &Path,
        line: u32,
        column: u32,
    ) -> Result<(), HostError> {
        panic!("Unexpected host callback: open_location")
    }
    fn open_log_tab(
        &self,
        client: &str,
        key: String,
        title: String,
        log: ServiceLog,
    ) -> Result<(), HostError> {
        panic!("Unexpected host callback: open_log_tab")
    }
    fn locale(&self) -> String {
        panic!("Unexpected host callback: locale")
    }
    fn setting(&self, root: Option<&Path>, key: &str) -> Option<String> {
        panic!("Unexpected host callback: setting")
    }
    fn read_text_file(&self, real: &Path, max: usize) -> Result<String, HostError> {
        panic!("Unexpected host callback: read_text_file")
    }
    fn write_text_file(&self, real: &Path, text: &str) -> Result<(), HostError> {
        panic!("Unexpected host callback: write_text_file")
    }
    fn write_bytes_file(&self, real: &Path, bytes: &[u8]) -> Result<(), HostError> {
        panic!("Unexpected host callback: write_bytes_file")
    }
}
fn main() {
    let wasm = std::env::args()
        .nth(1)
        .expect("usage: ddev-wasm-smoke <dist/plugin.wasm>");
    let engine = PluginEngine::new().unwrap();
    let component = engine.compile(Path::new(&wasm)).unwrap();
    let mut runtime = Runtime::instantiate(
        &engine,
        &component,
        RuntimeSetup {
            plugin_id: "aritti.ddev".into(),
            version: "0.1.0".into(),
            granted: vec![Capability::ProjectRead],
            subject: Subject::Installed("aritti.ddev".into()),
            host: Arc::new(ProbeHost),
            limits: Limits::default(),
        },
    )
    .unwrap();
    let started = Instant::now();
    runtime.activate().expect("Wasm activation");
    println!("activation: {:?}", started.elapsed());
    let budget = Duration::from_millis(50);
    for (text, at, label) in [
        (
            "nodejs_ro",
            ide_core::Pos { line: 0, col: 9 },
            "nodejs_root",
        ),
        ("php_version: 8.", ide_core::Pos { line: 0, col: 15 }, "8.6"),
        ("php_version:", ide_core::Pos { line: 0, col: 12 }, "8.3"),
        (
            "database: {type:}",
            ide_core::Pos { line: 0, col: 16 },
            "postgres",
        ),
        (
            "name: café🦀\nphp_v",
            ide_core::Pos { line: 1, col: 5 },
            "php_version",
        ),
        ("database:\n  ty", ide_core::Pos { line: 1, col: 4 }, "type"),
        (
            "database:\n  type: postgres\n  version: ",
            ide_core::Pos { line: 2, col: 11 },
            "17",
        ),
        (
            "hooks:\n  post-start:\n    - ex",
            ide_core::Pos { line: 2, col: 8 },
            "exec",
        ),
    ] {
        let doc = ProviderDocument {
            path: "/work/.ddev/config.yaml".into(),
            language: "yaml".into(),
            version: 1,
            text: text.into(),
        };
        let start = Instant::now();
        let items = runtime
            .provide_completion(&doc, at, None, budget)
            .expect("Wasm completion");
        assert!(items.iter().any(|i| i.label == label), "missing {label}");
        println!("completion {label}: {:?}", start.elapsed());
    }
    for path in [
        r"C:\WORK\.DDEV\CONFIG.YAML",
        r"\\server\share\.DDEV\CONFIG.LOCAL.YAML",
    ] {
        let doc = ProviderDocument {
            path: path.into(),
            language: "yaml".into(),
            version: 1,
            text: "php_ver".into(),
        };
        assert!(
            runtime
                .provide_completion(&doc, ide_core::Pos { line: 0, col: 7 }, None, budget)
                .expect("Windows/UNC schema completion")
                .iter()
                .any(|item| item.label == "php_version")
        );
    }
    let doc = ProviderDocument {
        path: "/work/.ddev/config.yaml".into(),
        language: "yaml".into(),
        version: 1,
        text: "php_version: '99.0'\n".into(),
    };
    let start = Instant::now();
    assert!(
        !runtime
            .provide_diagnostics(&doc, budget)
            .expect("Wasm diagnostics")
            .is_empty()
    );
    println!("diagnostics: {:?}", start.elapsed());
    let start = Instant::now();
    assert!(
        runtime
            .provide_hover(&doc, ide_core::Pos { line: 0, col: 4 }, budget)
            .expect("Wasm hover")
            .is_some()
    );
    println!("hover: {:?}", start.elapsed());
    let many_duplicates = ProviderDocument {
        path: "/work/.ddev/config.yaml".into(),
        language: "yaml".into(),
        version: 2,
        text: "xdebug_enabled: nope\n".repeat(130),
    };
    let start = Instant::now();
    let diagnostics = runtime
        .provide_diagnostics(&many_duplicates, budget)
        .expect("bounded Wasm diagnostics");
    assert!(!diagnostics.is_empty() && diagnostics.len() <= 100);
    println!("bounded duplicate diagnostics: {:?}", start.elapsed());
    let global = ProviderDocument {
        path: "/home/user/.ddev/global_config.yaml".into(),
        language: "yaml".into(),
        version: 1,
        text: "hooks:\n  post-start:\n    - ex".into(),
    };
    let start = Instant::now();
    assert!(
        runtime
            .provide_completion(&global, ide_core::Pos { line: 2, col: 8 }, None, budget)
            .expect("global hook completion")
            .iter()
            .any(|item| item.label == "exec")
    );
    let global = ProviderDocument {
        text: "ssh_agent_upstream: ".into(),
        ..global
    };
    let values = runtime
        .provide_completion(&global, ide_core::Pos { line: 0, col: 20 }, None, budget)
        .expect("global SSH agent completion");
    assert!(values.iter().any(|item| item.label == "host"));
    assert!(values.iter().all(|item| !item.label.is_empty()));
    println!("global schema assistance: {:?}", start.elapsed());
}
