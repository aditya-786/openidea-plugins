//! Owned live-tail alternative. JVM helper follows one confined UTF8 file;
//! visible-panel messages only drain bounded output, never bootstrap an app.
use super::*;
use openidea_plugin::views::{Body, Button};
use std::collections::VecDeque;
use std::time::{Duration, Instant};
pub const VIEW: &str = "live-tail";
const SOURCE: &str = include_str!("../adapters/IdeologTail.java");
const MAX_LINES: usize = 512;
const EXPIRED: &str = "Stopped after 5 minutes. Click Start to follow again";
struct Lease {
    child: host::Child,
    root: String,
    path: String,
    expires: Instant,
    next: Instant,
    pending: String,
    generation: u64,
    offset: Option<u64>,
    bytes: Vec<u8>,
    ready: bool,
}
#[derive(Default)]
pub(crate) struct LiveTail {
    lease: Option<Lease>,
    root: String,
    path: String,
    rows: VecDeque<String>,
    bytes: usize,
    status: String,
}
fn hex_decode(value: &str) -> Result<Vec<u8>, Error> {
    if value.len() > 8192 || value.len() % 2 != 0 {
        return Err(Error::Stale);
    }
    (0..value.len())
        .step_by(2)
        .map(|at| {
            u8::from_str_radix(value.get(at..at + 2).ok_or(Error::Stale)?, 16)
                .map_err(|_| Error::Stale)
        })
        .collect()
}
fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}
impl LiveTail {
    pub(crate) fn source(&self) -> Option<(String, String)> {
        let lease = self.lease.as_ref()?;
        Some((lease.path.clone(), lease.root.clone()))
    }
    fn stop(&mut self, message: &str) {
        if let Some(lease) = self.lease.take() {
            let _ = lease.child.kill();
        }
        self.status = message.into();
    }
    fn consume(&mut self, line: &str) -> Result<(), Error> {
        let fields: Vec<_> = line.split('\t').collect();
        let lease = self.lease.as_mut().ok_or(Error::Stale)?;
        match fields.as_slice() {
            ["TAIL1", "READY", root, path] => {
                if lease.ready
                    || String::from_utf8(hex_decode(root)?).ok().as_deref() != Some(&lease.root)
                    || String::from_utf8(hex_decode(path)?).ok().as_deref() != Some(&lease.path)
                {
                    return Err(Error::Stale);
                }
                lease.ready = true;
                self.rows.clear();
                self.bytes = 0;
                self.status = format!("Following {} (stops in 5 min)", file_name(&lease.path));
            }
            ["TAIL1", "WAIT", "missing"] => {
                self.status = format!("Waiting for {} to come back after log rotation", file_name(&lease.path));
            }
            ["TAIL1", "RESET", generation] => {
                let generation = generation.parse::<u64>().map_err(|_| Error::Stale)?;
                if !lease.ready || generation != lease.generation + 1 {
                    return Err(Error::Stale);
                }
                lease.generation = generation;
                lease.offset = None;
                lease.bytes.clear();
                self.rows.clear();
                self.bytes = 0;
                self.status = format!("{} was rotated or emptied; showing its new lines", file_name(&lease.path));
            }
            ["TAIL1", "DATA", generation, offset, hex] => {
                let generation = generation.parse::<u64>().map_err(|_| Error::Stale)?;
                let at = offset.parse::<u64>().map_err(|_| Error::Stale)?;
                let bytes = hex_decode(hex)?;
                if !lease.ready
                    || generation != lease.generation
                    || lease.offset.is_some_and(|expected| expected != at)
                    || bytes.is_empty()
                {
                    return Err(Error::Stale);
                }
                lease.offset = Some(at.checked_add(bytes.len() as u64).ok_or(Error::Stale)?);
                lease.bytes.extend(bytes);
                while let Some(end) = lease.bytes.iter().position(|b| *b == b'\n') {
                    if end > 4096 {
                        return Err("Tail line exceeds 4096 bytes.".into());
                    }
                    let content = String::from_utf8(lease.bytes[..end].to_vec())
                        .map_err(|_| "Live tail reads UTF-8 text only.")?;
                    lease.bytes.drain(..=end);
                    let content = content.trim_end_matches('\r').to_owned();
                    self.bytes += content.len();
                    self.rows.push_back(content);
                    while self.rows.len() > MAX_LINES || self.bytes > 131072 {
                        if let Some(old) = self.rows.pop_front() {
                            self.bytes = self.bytes.saturating_sub(old.len());
                        }
                    }
                }
                if lease.bytes.len() > 4096 {
                    return Err("Tail line exceeds 4096 bytes.".into());
                }
            }
            ["TAIL1", "END", _] => {
                self.status = EXPIRED.into();
            }
            _ => return Err("Live tail got unexpected output from Java. Click Start to try again.".into()),
        }
        Ok(())
    }
}
impl Ideolog {
    fn verify_tail_source(&self, ctx: &Invocation) -> Result<(), Error> {
        let lease = self.tail.lease.as_ref().ok_or(Error::Stale)?;
        if ctx.project().and_then(|p| p.root().ok()).as_deref() != Some(&lease.root)
            || project_of(&lease.path)
                .and_then(|p| p.root().ok())
                .as_deref()
                != Some(&lease.root)
        {
            return Err(Error::Stale);
        }
        if host::open_editors()?
            .iter()
            .any(|editor| editor.path == lease.path && editor.dirty)
        {
            return Err("Live tail stopped: the log has unsaved edits. Save it, then click Start.".into());
        }
        let project = ctx.project().ok_or(Error::Stale)?;
        match project.stat(&lease.path) {
            Ok(stat) if stat.kind == openidea_plugin::FileKind::File => {}
            Err(Error::NotFound(_)) => {} // helper owns the bounded rotation grace
            _ => return Err(Error::Stale),
        }
        Ok(())
    }
    fn start_tail(
        &mut self,
        ctx: &Invocation,
    ) -> Result<(), Error> {
        if self.tail.lease.is_some() {
            return Err("Stop the current live tail before starting another.".into());
        }
        let editor = ctx
            .editor()
            .filter(|e| matches!(e.language().as_str(), "log" | "ideolog"))
            .ok_or("Focus a log file of this project, then click Start.")?;
        let path = editor.path();
        let project = project_of(&path).ok_or(Error::Stale)?;
        let root = project.root()?;
        if ctx.project().and_then(|p| p.root().ok()).as_deref() != Some(&root)
            || host::open_editors()?
                .iter()
                .any(|e| e.path == path && e.dirty)
        {
            return Err("Live tail follows saved log files of this project. Save the log, then click Start.".into());
        }
        if project.stat(&path)?.kind != openidea_plugin::FileKind::File {
            return Err(Error::Stale);
        }
        if !ui_kit::prompts::confirm_exact(
            &format!("Follow {}?", file_name(&path)),
            &format!(
                "Java 17 or newer shows the last 64 KiB of {path}, then its new lines for up to 5 minutes (or 1 MiB).\nIt only reads this file; nothing in your project runs.\nSwitching tabs or closing the panel stops it."
            ),
        )? {
            return Err(Error::Cancelled);
        }
        if project_of(&path).and_then(|p| p.root().ok()).as_deref() != Some(&root)
            || ctx.project().and_then(|p| p.root().ok()).as_deref() != Some(&root)
            || host::open_editors()?
                .iter()
                .any(|e| e.path == path && e.dirty)
        {
            return Err(Error::Stale);
        }
        let directory = host::fs::plugin_dir()?;
        let helper = format!("{}/IdeologTail.java", directory.trim_end_matches('/'));
        host::fs::write_file(&helper, SOURCE)?;
        let child = host::start(
            &host::Command::new("java")
                .args(["-Xmx64m", "--source", "17", &helper, &root, &path, "300000"]),
            None,
        )
        .map_err(|e| ui_kit::denied::map(e, "Live tail", "process:java", "Ideolog"))?;
        child.close_stdin()?;
        if self.tail.root != root || self.tail.path != path {
            self.tail.rows.clear();
            self.tail.bytes = 0;
        }
        self.tail.root = root.clone();
        self.tail.path = path.clone();
        self.tail.status = format!("Starting Java to follow {}…", file_name(&path));
        self.tail.lease = Some(Lease {
            child,
            root,
            path,
            expires: Instant::now() + Duration::from_secs(305),
            next: Instant::now(),
            pending: String::new(),
            generation: 0,
            offset: None,
            bytes: Vec::new(),
            ready: false,
        });
        Ok(())
    }
    fn poll_tail(&mut self, ctx: &Invocation) -> Result<(), Error> {
        if let Err(error) = self.verify_tail_source(ctx) {
            self.tail
                .stop("Stopped: the log has unsaved edits or left the project. Click Start to follow again");
            return Err(error);
        }
        let lease = self.tail.lease.as_mut().ok_or(Error::Stale)?;
        if Instant::now() > lease.expires {
            self.tail.stop(EXPIRED);
            return Ok(());
        }
        if Instant::now() < lease.next {
            return Ok(());
        }
        lease.next = Instant::now() + Duration::from_millis(500);
        let chunk = match lease.child.poll(32768) {
            Ok(chunk) => chunk,
            Err(error) => {
                self.tail
                    .stop("Java stopped unexpectedly. Click Start to try again");
                return Err(error);
            }
        };
        let failure = failure_text(&chunk.stderr);
        if !failure.is_empty() {
            self.tail.stop(&format!(
                "Java failed: {}. Check that Java 17 or newer is installed, then click Start",
                failure.chars().take(256).collect::<String>()
            ));
            return Ok(());
        }
        lease.pending.push_str(&chunk.stdout);
        if lease.pending.len() > 65536 {
            self.tail.stop("Stopped: the log wrote too much at once. Click Start to try again");
            return Err(Error::Stale);
        }
        let mut frames = Vec::new();
        while let Some(end) = lease.pending.find('\n') {
            frames.push(lease.pending[..end].to_owned());
            lease.pending.drain(..=end);
        }
        for frame in frames {
            if let Err(error) = self.tail.consume(&frame) {
                self.tail
                    .stop("Stopped: the log file changed unexpectedly. Click Start to follow again");
                return Err(error);
            }
        }
        if let Some(status) = chunk.status {
            self.tail.stop(if status == 0 {
                EXPIRED
            } else {
                "Java stopped with an error. Click Start to try again"
            });
        }
        Ok(())
    }
    pub(crate) fn render_tail(&self, ctx: &Invocation) -> View {
        let owned = ctx.project().and_then(|p| p.root().ok()).as_deref() == Some(&self.tail.root);
        let rows = self.tail.rows.iter().filter(|_| owned);
        let items: Vec<_> = rows
            .enumerate()
            .map(|(at, line)| serde_json::json!({"key": at.to_string(), "cells": [line]}))
            .collect();
        let active = owned && self.tail.lease.is_some();
        let poll = active.then(|| serde_json::json!({"intervalMs": 1000, "message": {"op": "tail-tick"}}));
        let status = if owned {
            format!("{} · {} lines (the last 512 are kept)", self.tail.status, items.len())
        } else {
            "Focus a log file of this project, then click Start. Needs Java 17 or newer.".into()
        };
        View::new(Body::Other {
            kind: "resources".into(),
            data: serde_json::json!({"columns": ["Line"], "items": items, "backgroundPoll": poll, "searchPlaceholder": "Filter lines…"}).to_string(),
        })
        .status(&status)
        .button(Button::new("start-tail", "Start").icon("run").enabled(!active))
        .button(Button::new("stop-tail", "Stop").icon("stop").enabled(active))
    }
    pub(crate) fn handle_tail(&mut self, event: ViewEvent, ctx: &Invocation) -> Result<(), Error> {
        match event {
            ViewEvent::Button(id) if id == "start-tail" => {
                let result = self.start_tail(ctx);
                ui_kit::web::refresh_view(VIEW, include_str!("ui_config.json"));
                result
            }
            ViewEvent::Button(id) if id == "stop-tail" => {
                self.stop_tail();
                ui_kit::web::refresh_view(VIEW, include_str!("ui_config.json"));
                Ok(())
            }
            ViewEvent::Message(raw) => self.tail_tick(&raw, ctx),
            _ => Ok(()),
        }
    }
    fn tail_tick(&mut self, raw: &str, ctx: &Invocation) -> Result<(), Error> {
        let request: serde_json::Value = serde_json::from_str(raw).map_err(|_| Error::Stale)?;
        if request["op"] != "tail-tick" || self.tail.lease.is_none() {
            return Err(Error::Stale);
        }
        let before = (self.tail.rows.len(), self.tail.bytes, self.tail.status.clone());
        let result = self.poll_tail(ctx);
        if let Err(error) = &result {
            self.tail.status = error.to_string();
        }
        let reply = serde_json::json!({"_backgroundPoll": request["_backgroundPoll"]});
        ui_kit::web::post_view_message(VIEW, &reply.to_string(), include_str!("ui_config.json"))?;
        let after = (self.tail.rows.len(), self.tail.bytes, self.tail.status.clone());
        if before != after || self.tail.lease.is_none() {
            ui_kit::web::refresh_view(VIEW, include_str!("ui_config.json"));
        }
        Ok(())
    }
    pub(crate) fn tail_lifecycle(&mut self, ctx: &Invocation) {
        if self.tail.lease.is_some() && self.verify_tail_source(ctx).is_err() {
            self.tail
                .stop("Stopped: the log file or project changed. Click Start to follow again");
        }
    }
    pub(crate) fn stop_tail(&mut self) {
        self.tail.stop("Stopped");
    }
}

/// The JVM announces `JAVA_TOOL_OPTIONS` on stderr; that is not a failure.
fn failure_text(stderr: &str) -> String {
    stderr
        .lines()
        .filter(|line| !line.starts_with("Picked up "))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    fn encoded(value: &str) -> String {
        value
            .as_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
    fn fixture(root: &str, path: &str) -> (MockHost, LiveTail) {
        let host =
            MockHost::install().with_capabilities(&["process:java", "project.read", "editor.read"]);
        host.set_project_root(Some(root));
        host.add_file(path, "initial\n");
        let child = host::start(&host::Command::new("java"), None).unwrap();
        let lease = Lease {
            child,
            root: root.into(),
            path: path.into(),
            expires: Instant::now() + Duration::from_secs(30),
            next: Instant::now(),
            pending: String::new(),
            generation: 0,
            offset: None,
            bytes: Vec::new(),
            ready: false,
        };
        (
            host,
            LiveTail {
                lease: Some(lease),
                root: root.into(),
                path: path.into(),
                ..Default::default()
            },
        )
    }
    #[test]
    fn java_tool_options_banner_keeps_tail() {
        let banner = "Picked up JAVA_TOOL_OPTIONS: -Xmx1g\nPicked up _JAVA_OPTIONS: -Dx=1\n";
        assert!(failure_text(banner).is_empty());
        assert_eq!(failure_text(&format!("{banner}Error: boom\n")), "Error: boom");
        assert_eq!(failure_text("Error: boom"), "Error: boom");
    }
    #[test]
    fn tail_protocol_is_ordered_utf8_bounded_and_generation_scoped() {
        let (_host, mut tail) = fixture("/project", "/project/app.log");
        tail.consume(&format!(
            "TAIL1\tREADY\t{}\t{}",
            encoded("/project"),
            encoded("/project/app.log")
        ))
        .unwrap();
        let text = "😀 initial\r\n";
        let bytes = text.as_bytes();
        let cut = 2;
        tail.consume(&format!(
            "TAIL1\tDATA\t0\t0\t{}",
            bytes[..cut]
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        ))
        .unwrap();
        assert!(tail.rows.is_empty());
        tail.consume(&format!(
            "TAIL1\tDATA\t0\t{cut}\t{}",
            bytes[cut..]
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        ))
        .unwrap();
        assert_eq!(tail.rows.back().unwrap(), "😀 initial");
        assert!(tail.consume("TAIL1\tDATA\t0\t999\t61").is_err());
        tail.consume("TAIL1\tRESET\t1").unwrap();
        assert!(tail.rows.is_empty());
        let mut offset = 0;
        for _ in 0..600 {
            let row = "line\r\n";
            tail.consume(&format!("TAIL1\tDATA\t1\t{offset}\t{}", encoded(row)))
                .unwrap();
            offset += row.len();
        }
        assert_eq!(tail.rows.len(), 512);
        assert_eq!(tail.bytes, 512 * 4);
        assert!(tail.consume("TAIL1\tRESET\t3").is_err());
    }
    #[test]
    fn ready_status_names_the_file_and_when_it_stops() {
        let (_host, mut tail) = fixture("/project", "/project/app.log");
        tail.consume(&format!("TAIL1\tREADY\t{}\t{}", encoded("/project"), encoded("/project/app.log"))).unwrap();
        assert_eq!(tail.status, "Following app.log (stops in 5 min)");
    }
    #[test]
    fn start_without_java_permission_names_the_permission() {
        let host = MockHost::install().with_capabilities(&["project.read", "editor.read"]);
        host.set_project_root(Some("/project"));
        host.add_file("/project/app.log", "line\n");
        host.open_editor("/project/app.log", "log", "line\n", &[]);
        host.answer_next_prompt(Some(&[]));
        let mut plugin = Ideolog::default();
        let error = plugin.start_tail(&host.invocation()).unwrap_err();
        assert!(matches!(error, Error::Invalid(ref m) if m == "Live tail needs the process:java permission. Allow it in Settings › Plugins › Ideolog."), "{error:?}");
        assert!(plugin.tail.lease.is_none());
    }
    #[test]
    fn source_invalidation_cancellation_and_stale_frames_stop_or_preserve_correct_lease() {
        let (host, tail) = fixture("/project", "/project/app.log");
        let mut plugin = Ideolog::default();
        plugin.tail = tail;
        let stale = serde_json::json!({"op": "unknown"}).to_string();
        assert!(plugin.tail_tick(&stale, &host.invocation()).is_err());
        assert!(plugin.tail.lease.is_some());
        plugin.tail.rows.push_back("last good".into());
        host.set_project_root(Some("/other"));
        plugin.tail_lifecycle(&host.invocation());
        assert!(plugin.tail.lease.is_none());
        assert_eq!(plugin.tail.rows.back().unwrap(), "last good");
        let (_host, tail) = fixture("/project", "/project/app.log");
        plugin.tail = tail;
        plugin.stop_tail();
        assert!(plugin.tail.lease.is_none());
    }
    #[cfg(all(not(target_arch = "wasm32"), unix))]
    #[test]
    fn actual_jvm_tail_follows_append_truncate_rotate_rejects_escape_and_cancels() {
        use std::{
            io::{BufRead, BufReader, Write},
            process::{Command, Stdio},
            sync::{
                atomic::{AtomicU64, Ordering},
                mpsc,
            },
        };
        static NEXT: AtomicU64 = AtomicU64::new(0);
        if Command::new("java").arg("-version").output().is_err() {
            eprintln!("optional JVM unavailable; runtime tail test skipped");
            return;
        }
        let dir = std::env::temp_dir().join(format!(
            "ideolog-tail-real-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&dir).unwrap();
        let dir = dir.canonicalize().unwrap();
        let helper = dir.join("IdeologTail.java");
        std::fs::write(&helper, SOURCE).unwrap();
        let path = dir.join("app.log");
        std::fs::write(&path, "initial INFO\n").unwrap();
        let root = dir.to_str().unwrap();
        let source = path.to_str().unwrap();
        let (_host, mut tail) = fixture(root, source);
        let launch = |duration: &str| {
            let mut child = Command::new("java")
                .args(["-Xmx64m", "--source", "17"])
                .arg(&helper)
                .arg(&dir)
                .arg(&path)
                .arg(duration)
                .current_dir(&dir)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .stdin(Stdio::null())
                .spawn()
                .unwrap();
            let (tx, rx) = mpsc::channel();
            let stdout = child.stdout.take().unwrap();
            std::thread::spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    if tx.send(line.unwrap()).is_err() {
                        break;
                    }
                }
            });
            (child, rx)
        };
        let (mut child, rx) = launch("12000");
        let consume_until = |tail: &mut LiveTail, rx: &mpsc::Receiver<String>, needle: &str| {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let line = rx
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    .expect("actual JVM tail output");
                tail.consume(&line).unwrap();
                if tail.rows.back().is_some_and(|row| row.contains(needle)) {
                    break;
                }
            }
        };
        consume_until(&mut tail, &rx, "initial INFO");
        writeln!(
            std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap(),
            "appended ERROR"
        )
        .unwrap();
        consume_until(&mut tail, &rx, "appended ERROR");
        std::fs::write(&path, "tiny WARN\n").unwrap();
        consume_until(&mut tail, &rx, "tiny WARN");
        assert_eq!(tail.rows.len(), 1);
        assert_eq!(tail.lease.as_ref().unwrap().generation, 1);
        let replacement = dir.join("rotated.log");
        std::fs::rename(&path, &replacement).unwrap();
        std::thread::sleep(Duration::from_millis(400));
        std::fs::write(&path, "rotated INFO\n").unwrap();
        consume_until(&mut tail, &rx, "rotated INFO");
        assert_eq!(tail.rows.len(), 1);
        assert_eq!(tail.lease.as_ref().unwrap().generation, 2);
        let outside = dir.with_file_name(format!("ideolog-tail-outside-{}", std::process::id()));
        std::fs::write(&outside, "OUTSIDE MUST NOT APPEAR\n").unwrap();
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&outside, &path).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() > deadline {
                child.kill().unwrap();
                panic!("source escape was not stopped");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let result = child.wait_with_output().unwrap();
        assert!(!result.status.success());
        assert!(!tail.rows.iter().any(|row| row.contains("OUTSIDE")));
        assert!(String::from_utf8_lossy(&result.stderr).contains("source changed owner"));
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, "restart INFO\n").unwrap();
        let (mut child, rx) = launch("12000");
        let first = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(first.starts_with("TAIL1\tREADY"));
        child.kill().unwrap();
        assert!(!child.wait().unwrap().success());
        let (mut child, rx) = launch("1000");
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut ended = false;
        while Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(line) if line.starts_with("TAIL1\tEND") => {
                    ended = true;
                    break;
                }
                Ok(_) => {}
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => break,
            }
        }
        if !ended {
            let _ = child.kill();
        }
        assert!(ended, "helper self-expires its runtime lease");
        assert!(child.wait().unwrap().success());
        let _ = std::fs::remove_file(outside);
        let _ = std::fs::remove_dir_all(&dir);
        eprintln!(
            "actual Java tail profile: append + truncate + delayed rotation + symlink escape rejection + cancellation + lease expiry passed"
        );
    }
}
