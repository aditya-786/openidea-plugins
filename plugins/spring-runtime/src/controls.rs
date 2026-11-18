//! Explicit instance-owned polling leases, never persistent timer authority.
use crate::*;
use openidea_plugin::views::{Field, FieldValue};
use std::time::{Duration, Instant};
pub const VIEW: &str = "runtime-controls";
const INTERVAL: Duration = Duration::from_secs(60);
const LEASE: Duration = Duration::from_secs(600);
pub(crate) const AUTO_REFRESH_OFF: &str = "Auto-refresh is off. Turn on Allow auto-refresh in Settings › Plugins › Spring Runtime Insight, then click Start.";
const CONTROLS_STATUS: &str = "Refresh reads the Actuator once. Start refreshes every minute while this tab is visible.";
struct Lease {
    scope: Scope,
    next: Instant,
    expires: Instant,
    remaining: u8,
    failures: u8,
}
#[derive(Default)]
pub(crate) struct Polling {
    lease: Option<Lease>,
    owner: Option<(String, String, u64)>,
    used: Vec<String>,
    requested: Option<Scope>,
}
impl Polling {
    pub fn clear(&mut self) {
        self.lease = None;
        self.requested = None;
    }
    pub fn active(&self, scope: &Scope) -> bool {
        self.lease.as_ref().is_some_and(|lease| &lease.scope == scope && lease.remaining > 0 && Instant::now() < lease.expires)
    }
    pub fn start(&mut self, scope: Scope) -> Result<(), Error> {
        if host::setting_bool("autoRefresh") != Some(true) {
            return Err(AUTO_REFRESH_OFF.into());
        }
        if self.lease.as_ref().is_some_and(|lease| Instant::now() >= lease.expires || lease.remaining == 0) {
            self.clear();
        }
        if self.lease.is_some() {
            return Err("Auto-refresh is already running for another project or Actuator URL. Stop it first.".into());
        }
        let remaining = number("pollAttempts", 5.0, 1.0, 10.0) as u8;
        let now = Instant::now();
        self.lease = Some(Lease { scope, next: now + INTERVAL, expires: now + LEASE, remaining, failures: 0 });
        Ok(())
    }
    pub fn stop(&mut self, root: &str) -> Result<bool, Error> {
        if self.lease.as_ref().is_some_and(|lease| lease.scope.root != root) {
            return Err(Error::Stale);
        }
        Ok(self.lease.take().is_some())
    }
    pub fn due(&mut self, scope: &Scope) -> Result<bool, &'static str> {
        let Some(lease) = &mut self.lease else {
            return Ok(false);
        };
        if &lease.scope != scope {
            self.clear();
            return Err("Auto-refresh stopped because the project, Actuator URL or authentication changed. Click Start to run it again.");
        }
        if Instant::now() >= lease.expires || lease.remaining == 0 {
            self.clear();
            return Err("Auto-refresh finished. Click Start to run it again.");
        }
        if Instant::now() < lease.next {
            return Ok(false);
        }
        lease.remaining -= 1;
        lease.next = Instant::now() + INTERVAL;
        Ok(true)
    }
    pub fn result(&mut self, scope: &Scope, result: &Result<String, Error>) -> Option<&'static str> {
        let Some(lease) = &mut self.lease else {
            return None;
        };
        if &lease.scope != scope {
            self.clear();
            return Some("Auto-refresh stopped because the project or Actuator URL changed.");
        }
        if matches!(result, Err(Error::Cancelled | Error::Stale)) {
            self.clear();
            return Some("Auto-refresh stopped because the refresh was cancelled. The previous snapshot is kept.");
        }
        if result.is_err() {
            lease.failures += 1;
        } else {
            lease.failures = 0;
        }
        if lease.failures >= 2 {
            self.clear();
            return Some("Auto-refresh stopped after two failed refreshes. The previous snapshot is kept.");
        }
        if lease.remaining == 0 {
            self.clear();
            return Some("Auto-refresh finished its refreshes. Click Start to run it again.");
        }
        None
    }
    pub fn status(&self, scope: &Scope) -> String {
        if host::setting_bool("autoRefresh") != Some(true) {
            return AUTO_REFRESH_OFF.into();
        }
        if self.lease.as_ref().is_some_and(|lease| &lease.scope == scope && Instant::now() >= lease.expires) {
            return "Auto-refresh ended after 10 minutes. Click Start to run it again.".into();
        }
        match &self.lease {
            Some(lease) if &lease.scope == scope => format!(
                "Auto-refresh on: {} refreshes left, next in {} s, ends in {} s, {} failed in a row. Keep this tab visible; hiding it stops auto-refresh.",
                lease.remaining,
                lease.next.saturating_duration_since(Instant::now()).as_secs(),
                lease.expires.saturating_duration_since(Instant::now()).as_secs(),
                lease.failures
            ),
            _ => "Auto-refresh is off for this project. Click Start to refresh every minute while this tab is visible.".into(),
        }
    }
    #[cfg(test)]
    pub(crate) fn due_now(&mut self) {
        if let Some(lease) = &mut self.lease {
            lease.next = Instant::now();
        }
    }
}
pub(crate) fn number(key: &str, default: f64, min: f64, max: f64) -> u32 {
    host::setting_number(key).filter(|value| value.is_finite()).unwrap_or(default).clamp(min, max) as u32
}
pub(crate) fn verify(project: &Project, scope: &Scope) -> Result<(), Error> {
    if Scope::current(project)? != *scope
        || !host::projects()?.iter().any(|project| project.root().ok().as_deref() == Some(scope.root.as_str()))
    {
        return Err(Error::Stale);
    }
    Ok(())
}
pub(crate) fn timeout() -> u32 {
    number("timeoutMs", 2000.0, 100.0, 30_000.0)
}
pub(crate) fn budget() -> u32 {
    number("refreshBudgetMs", 20_000.0, 1000.0, 60_000.0)
}
impl Runtime {
    #[cfg(test)]
    pub(crate) fn start_polling(&mut self, project: &Project) -> Result<String, Error> {
        let scope = Scope::current(project)?;
        self.load_scope(scope.clone());
        self.polling.start(scope.clone())?;
        let status = self.polling.status(&scope);
        self.poll_status = Some(status.clone());
        Ok(status)
    }
    /// Polling ticks come from the visible controls page, so the command asks that page to Start.
    pub(crate) fn request_poll_start(&mut self, ctx: &Invocation) -> Result<String, Error> {
        let project = ctx.project().ok_or("Open a project first.")?;
        if host::setting_bool("autoRefresh") != Some(true) {
            return Err(AUTO_REFRESH_OFF.into());
        }
        let scope = Scope::current(&project)?;
        verify(&project, &scope)?;
        if self.polling.active(&scope) {
            return Ok("Auto-refresh is already running for this project.".into());
        }
        self.polling.requested = Some(scope);
        crate::reveal_view(VIEW)?;
        // Not mounted yet: its first hello carries the request instead.
        let _ = crate::post_view_message(VIEW, r#"{"channel":"runtime-controls","op":"start-request"}"#);
        Ok("Auto-refresh started in Spring Runtime › Refresh Controls. It runs while that tab stays visible.".into())
    }
    pub(crate) fn stop_polling(&mut self, project: &Project) -> Result<String, Error> {
        let stopped = self.polling.stop(&project.root()?)?;
        let status = if stopped {
            "Auto-refresh stopped. To cancel a refresh in progress, use Cancel on the IDE task."
        } else {
            "Auto-refresh isn't running for this project. To cancel a refresh in progress, use Cancel on the IDE task."
        };
        self.poll_status = Some(status.into());
        invalidate_snapshot_views();
        Ok(status.into())
    }
    pub(crate) fn render_controls(&mut self, ctx: &Invocation) -> Result<View, Error> {
        let Some(project) = ctx.project() else {
            return Ok(View::message("Open a project to use the refresh controls."));
        };
        let scope = Scope::current(&project)?;
        self.load_scope(scope.clone());
        if ui_kit::web::is_rendering() {
            return Ok(View::new(Body::Html(include_str!("controls.html").into()))
                .status(CONTROLS_STATUS));
        }
        let text = format!("Current: {}\nLast control outcome: {}", self.polling.status(&scope), self.poll_status.as_deref().unwrap_or(""));
        Ok(View::new(Body::Form(vec![
            Field::new("control-project","Owning project",FieldValue::readonly_text(&scope.root,false)),
            Field::new("control-url","Actuator URL",FieldValue::readonly_text(&scope.url,false)),
            Field::new("control-auth","Authentication",FieldValue::readonly_text(&scope.auth,false)),
            Field::new("control-bounds","Request limits",FieldValue::readonly_text(&format!("{} ms per request, {} ms per refresh.",timeout(),budget()),true)),
            Field::new("control-poll","Auto-refresh",FieldValue::readonly_text(&text,true)),
            Field::new("control-provenance","About",FieldValue::readonly_text("Refresh runs as an IDE task; cancel it there. Stop Auto-Refresh only cancels the next refreshes. Requests go only to localhost or 127.0.0.1, but the IDE follows redirects, so the answering server isn't checked.",true)),
        ])).button(Button::new("refresh","Refresh Once").icon("refresh"))
        .button(Button::new("poll-start","Start Auto-Refresh").enabled(host::setting_bool("autoRefresh")==Some(true)&&!self.polling.active(&scope)))
        .button(Button::new("poll-stop","Stop Auto-Refresh").enabled(self.polling.lease.as_ref().is_some_and(|lease|lease.scope.root==scope.root)))
        .status(CONTROLS_STATUS))
    }
    pub(crate) fn controls_message(&mut self, raw: &str, ctx: &Invocation) -> Result<(), Error> {
        if raw.len() > 4096 {
            return Err(Error::Stale);
        }
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Request {
            channel: String,
            frame: String,
            nonce: String,
            sequence: u64,
            op: String,
            root: Option<String>,
            url: Option<String>,
            auth: Option<String>,
        }
        let request: Request = serde_json::from_str(raw).map_err(|_| Error::Stale)?;
        let hex = |s: &str| s.len() == 32 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        if request.channel != "runtime-controls" || !hex(&request.frame) || !hex(&request.nonce) || request.sequence == 0 {
            return Err(Error::Stale);
        }
        let mut autostart = false;
        let result = (|| -> Result<Scope, Error> {
            let project = ctx.project().ok_or(Error::Stale)?;
            let scope = Scope::current(&project)?;
            verify(&project, &scope)?;
            if request.op != "hello"
                && (request.root.as_deref() != Some(scope.root.as_str())
                    || request.url.as_deref() != Some(scope.url.as_str())
                    || request.auth.as_deref() != Some(scope.auth.as_str()))
            {
                if self.polling.owner.as_ref().is_some_and(|o| o.0 == request.frame && o.1 == request.nonce)
                    && self.polling.lease.as_ref().is_some_and(|l| l.scope != scope)
                {
                    self.polling.clear();
                }
                return Err(Error::Stale);
            }
            match request.op.as_str() {
                // No lease transfer or HTTP on render/reload; Start Auto-Refresh asks the page to Start.
                "hello" => autostart = self.polling.requested.as_ref() == Some(&scope),
                "start" => {
                    if self.polling.used.len() >= 64 || self.polling.used.contains(&request.nonce) {
                        return Err(Error::Stale);
                    }
                    self.load_scope(scope.clone());
                    self.polling.start(scope.clone())?;
                    self.polling.used.push(request.nonce.clone());
                    self.polling.owner = Some((request.frame.clone(), request.nonce.clone(), request.sequence));
                    self.polling.requested = None;
                }
                "tick" | "stop" => {
                    let owner = self.polling.owner.as_mut().ok_or(Error::Stale)?;
                    if owner.0 != request.frame || owner.1 != request.nonce || request.sequence <= owner.2 {
                        return Err(Error::Stale);
                    }
                    owner.2 = request.sequence;
                    if self.polling.lease.as_ref().is_some_and(|lease| lease.scope != scope) {
                        self.polling.clear();
                        return Err(Error::Stale);
                    }
                    if request.op == "stop" {
                        self.polling.stop(&scope.root)?;
                    } else {
                        self.view_poll(ctx);
                    }
                }
                "refresh" => {
                    let summary = self.refresh(&project)?;
                    self.poll_status = Some(summary);
                }
                _ => return Err(Error::Stale),
            }
            verify(&project, &scope)?;
            if matches!(request.op.as_str(), "start" | "stop") {
                self.poll_status = Some(self.polling.status(&scope));
                invalidate_snapshot_views();
            }
            Ok(scope)
        })();
        let answer = match result {
            Ok(scope) => serde_json::json!({"root":scope.root,"url":scope.url,"auth":scope.auth,
                "status":self.poll_status.as_deref().unwrap_or(""),"poll":self.polling.status(&scope),"autostart":autostart,
                "active":self.polling.active(&scope) && self.polling.owner.as_ref().is_some_and(|o|o.0==request.frame && o.1==request.nonce),
                "enabled":host::setting_bool("autoRefresh")==Some(true),
                "bounds":format!("At most {} ms per request and {} ms per refresh of the 9 endpoints.",timeout(),budget())}),
            Err(error) => {
                let message = match &error {
                    Error::Stale | Error::Cancelled => "The project or settings changed, so nothing ran. Reopen Refresh Controls and try again.".to_owned(),
                    _ if request.op == "refresh" => format!("Refresh failed: {} The previous snapshot is kept.", presentation(&error.to_string(), 600)),
                    _ => presentation(&error.to_string(), 600),
                };
                serde_json::json!({"error":message,"active":false})
            }
        };
        crate::post_view_message(VIEW, &serde_json::json!({"channel":"runtime-controls","frame":request.frame,"nonce":request.nonce,"sequence":request.sequence,"model":answer}).to_string())?;
        Ok(())
    }
    pub(crate) fn view_poll(&mut self, ctx: &Invocation) {
        if host::setting_bool("autoRefresh") != Some(true) {
            if self.polling.lease.is_some() {
                self.poll_status = Some("Auto-refresh stopped: Allow auto-refresh is off in settings.".into());
                self.polling.clear();
            }
            return;
        }
        let Some(project) = ctx.project() else {
            let message = "Auto-refresh skipped this refresh: no project is open.";
            if self.poll_status.as_deref() != Some(message) {
                host::log(LogLevel::Info, message);
            }
            self.poll_status = Some(message.into());

            return;
        };
        let scope = match Scope::current(&project) {
            Ok(scope) => scope,
            Err(_) => {
                self.polling.clear();
                self.poll_status = Some("Auto-refresh stopped: check Actuator base URL in Settings › Plugins › Spring Runtime Insight.".into());

                return;
            }
        };
        let mut attempt = std::mem::take(&mut self.polling);
        match attempt.due(&scope) {
            Ok(true) => {
                let result = self.refresh(&project);
                self.poll_status = Some(attempt.result(&scope, &result).map(str::to_owned).unwrap_or_else(|| attempt.status(&scope)));
                if host::setting_bool("autoRefresh") != Some(true) {
                    attempt.clear();
                }
                self.polling = attempt;
            }
            Err(message) => {
                self.polling = attempt;
                self.poll_status = Some(message.into());
                invalidate_snapshot_views();
            }
            Ok(false) => {
                self.polling = attempt;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{HttpResponse, mock::MockHost};
    const CAPS: &[&str] = &["project.read", "storage", "net:localhost", "net:127.0.0.1", "secrets"];
    fn response(req: &HttpRequest) -> Result<HttpResponse, Error> {
        let value = if req.url.ends_with("/beans") {
            serde_json::json!({"contexts":{"app":{"beans":{"a":{"type":"x.A","dependencies":[]}}}}})
        } else if req.url.ends_with("/env") {
            serde_json::json!({"propertySources":[]})
        } else if req.url.ends_with("/health") {
            serde_json::json!({"status":"UP"})
        } else if req.url.ends_with("/loggers") {
            serde_json::json!({"loggers":{}})
        } else if req.url.ends_with("/caches") {
            serde_json::json!({"cacheManagers":{}})
        } else if req.url.ends_with("/scheduledtasks") {
            serde_json::json!({"cron":[],"fixedDelay":[],"fixedRate":[]})
        } else {
            serde_json::json!({"contexts":{}})
        };
        Ok(HttpResponse { status: 200, headers: vec![], body: serde_json::to_vec(&value).unwrap() })
    }
    fn message(runtime: &mut Runtime, host: &MockHost, op: &str, frame: &str, nonce: &str, sequence: u64, root: Option<&str>) {
        runtime.controls_message(&serde_json::json!({"channel":"runtime-controls","op":op,"frame":frame,"nonce":nonce,"sequence":sequence,"root":root,"url":DEFAULT_URL,"auth":"none"}).to_string(), &host.invocation()).unwrap();
    }
    const FRAME: &str = "11111111111111111111111111111111";
    const NONCE: &str = "22222222222222222222222222222222";
    #[test]
    fn grouped_controls_keep_private_protocol_and_retire_a_lease_on_tab_departure() {
        let host = MockHost::install().with_capabilities(CAPS);
        host.set_setting("autoRefresh", "true");
        let mut runtime = Runtime::default();
        crate::reveal_view(VIEW).unwrap();
        let request = |runtime: &mut Runtime, target: &str, operation: Value| -> Value {
            let mut operation = operation;
            operation["channel"] = serde_json::json!("ui-kit");
            operation["sequence"] = serde_json::json!(1);
            operation["project"] = serde_json::json!("/project");
            operation["targetView"] = serde_json::json!(target);
            runtime.handle(crate::VIEW, ViewEvent::Message(operation.to_string()), &host.invocation()).unwrap();
            serde_json::from_str(&host.view_messages().last().unwrap().1).unwrap()
        };
        let render = request(&mut runtime, VIEW, serde_json::json!({"op":"render"}));
        assert!(render["model"]["body"]["html"].as_str().unwrap().contains("data-runtime-controls"));
        let start = serde_json::json!({"channel":"runtime-controls","op":"start","frame":FRAME,"nonce":NONCE,"sequence":1,"root":"/project","url":DEFAULT_URL,"auth":"none"});
        let started = request(&mut runtime, VIEW, serde_json::json!({"op":"opaque-message","message":start.to_string()}));
        assert_eq!(started["handled"], true);
        assert!(started.get("model").is_none(), "private response cannot remount its driver");
        assert!(runtime.polling.lease.is_some());
        let selected = request(&mut runtime, VIEW, serde_json::json!({"op":"select-view","target":crate::VIEW}));
        assert_eq!(selected["model"]["activeView"], crate::VIEW);
        assert!(runtime.polling.lease.is_none(), "tab navigation retires even a pending Start response's lease");
        let stale = request(&mut runtime, VIEW, serde_json::json!({"op":"opaque-message","message":start.to_string()}));
        assert!(stale.get("error").is_some());
        assert!(runtime.polling.lease.is_none());
        assert!(host.requests().is_empty());
    }

    #[test]
    fn private_start_stop_updates_mounted_runtime_status_without_fetch_or_hello_invalidation() {
        let host = MockHost::install().with_capabilities(CAPS);
        host.set_setting("autoRefresh", "true");
        let mut runtime = Runtime::default();
        message(&mut runtime, &host, "hello", FRAME, NONCE, 1, None);
        assert!(!host.view_messages().iter().any(|(_, raw)| raw.contains("ui-kit-refresh")));
        message(&mut runtime, &host, "start", FRAME, NONCE, 2, Some("/project"));
        assert!(runtime.poll_status.as_ref().unwrap().contains("Auto-refresh on"));
        message(&mut runtime, &host, "stop", FRAME, NONCE, 3, Some("/project"));
        assert!(runtime.poll_status.as_ref().unwrap().contains("Auto-refresh is off for this project"));
        let refreshes: Vec<_> = host.view_messages().into_iter().filter_map(|(view, raw)| {
            let value: Value = serde_json::from_str(&raw).unwrap();
            if value["channel"] != "ui-kit-refresh" { return None; }
            assert_eq!(view, crate::VIEW);
            value["targetView"].as_str().map(str::to_owned)
        }).collect();
        assert_eq!(refreshes.iter().map(String::as_str).collect::<Vec<_>>(), [crate::VIEW, graph::VIEW, "runtime-snapshot", crate::VIEW, graph::VIEW, "runtime-snapshot"]);
        assert!(host.requests().is_empty());
    }
    #[test]
    fn private_ticks_require_exact_frame_nonce_sequence_root_and_no_scheduled_fetch() {
        let host = MockHost::install().with_capabilities(CAPS);
        host.on_fetch(response);
        host.set_setting("autoRefresh", "true");
        let mut runtime = Runtime::default();
        message(&mut runtime, &host, "hello", FRAME, NONCE, 1, None);
        assert!(host.requests().is_empty());
        assert!(runtime.polling.lease.is_none());
        message(&mut runtime, &host, "start", FRAME, NONCE, 2, Some("/project"));
        assert!(runtime.polling.lease.is_some());
        runtime.polling.due_now();
        runtime.handle_events(&[Event::Scheduled], &host.invocation());
        assert!(host.requests().is_empty());
        for (frame, nonce, sequence, root) in
            [(NONCE, NONCE, 3, "/project"), (FRAME, FRAME, 3, "/project"), (FRAME, NONCE, 2, "/project"), (FRAME, NONCE, 3, "/other")]
        {
            message(&mut runtime, &host, "tick", frame, nonce, sequence, Some(root));
            assert!(host.requests().is_empty());
        }
        message(&mut runtime, &host, "tick", FRAME, NONCE, 3, Some("/project"));
        assert_eq!(host.requests().len(), ENDPOINTS.len());
        let refreshes: Vec<_> = host.view_messages().into_iter().filter_map(|(view, raw)| {
            let value: Value = serde_json::from_str(&raw).unwrap();
            if value["channel"] != "ui-kit-refresh" { return None; }
            assert_eq!(view, crate::VIEW);
            value["targetView"].as_str().map(str::to_owned)
        }).collect();
        assert_eq!(refreshes.iter().map(String::as_str).collect::<Vec<_>>(), [crate::VIEW, graph::VIEW, "runtime-snapshot", crate::VIEW, graph::VIEW, "runtime-snapshot"], "Start and successful private tick each invalidate the three snapshot tabs");
        runtime.polling.due_now();
        message(&mut runtime, &host, "tick", FRAME, NONCE, 3, Some("/project"));
        assert_eq!(host.requests().len(), ENDPOINTS.len());
        message(&mut runtime, &host, "stop", FRAME, NONCE, 4, Some("/project"));
        assert!(runtime.polling.lease.is_none());
        message(&mut runtime, &host, "start", FRAME, NONCE, 5, Some("/project"));
        assert!(runtime.polling.lease.is_none(), "consumed start cannot replay");
    }
    #[test]
    fn private_ticks_disarm_on_scope_or_settings_change_and_cancel() {
        for change in ["url", "auth", "disabled", "cancel"] {
            let host = MockHost::install().with_capabilities(CAPS);
            host.on_fetch(response);
            host.set_setting("autoRefresh", "true");
            let mut runtime = Runtime::default();
            message(&mut runtime, &host, "start", FRAME, NONCE, 1, Some("/project"));
            runtime.polling.due_now();
            match change {
                "url" => host.set_setting("actuatorUrl", "\"http://localhost:8081/actuator\""),
                "auth" => host.set_setting("actuatorAuth", "\"basic\""),
                "disabled" => host.set_setting("autoRefresh", "false"),
                _ => host.on_fetch(|_| Err(Error::Cancelled)),
            };
            message(&mut runtime, &host, "tick", FRAME, NONCE, 2, Some("/project"));
            assert!(runtime.polling.lease.is_none(), "{change}");
            if change != "cancel" {
                assert!(host.requests().is_empty(), "{change}");
            }
        }
    }
    #[test]
    fn malformed_private_frames_and_no_project_never_authorize_network() {
        let host = MockHost::install().with_capabilities(CAPS);
        host.set_setting("autoRefresh", "true");
        let mut runtime = Runtime::default();
        assert!(runtime.controls_message("{}", &host.invocation()).is_err());
        assert!(runtime.controls_message(&"x".repeat(4097), &host.invocation()).is_err());
        for frame in ["", "A1111111111111111111111111111111", "1111"] {
            let raw =
                serde_json::json!({"channel":"runtime-controls","op":"start","frame":frame,"nonce":NONCE,"sequence":1,"root":"/project"})
                    .to_string();
            assert!(runtime.controls_message(&raw, &host.invocation()).is_err());
        }
        host.set_project_root(None);
        message(&mut runtime, &host, "start", FRAME, NONCE, 1, Some("/project"));
        assert!(runtime.polling.lease.is_none());
        assert!(host.requests().is_empty());
    }
    #[test]
    fn native_task_cancel_and_scope_changes_stop_between_endpoint_boundaries() {
        let host = MockHost::install().with_capabilities(CAPS);
        host.on_fetch(response);
        let project = host.invocation().project().unwrap();
        let scope = Scope::current(&project).unwrap();
        let mut checks = 0;
        assert!(matches!(
            fetch_snapshot_guarded(&scope.url, &scope.auth, || {
                checks += 1;
                verify(&project, &scope)?;
                if checks == 2 {
                    host.cancel_tasks();
                }
                Ok(())
            }),
            Err(Error::Cancelled)
        ));
        assert_eq!(host.requests().len(), 1);
        assert!(host.tasks().iter().all(|task| task.ended));
        let host = MockHost::install().with_capabilities(CAPS);
        host.on_fetch(response);
        let project = host.invocation().project().unwrap();
        let scope = Scope::current(&project).unwrap();
        let mut checks = 0;
        assert!(matches!(
            fetch_snapshot_guarded(&scope.url, &scope.auth, || {
                checks += 1;
                if checks == 2 {
                    host.set_setting("actuatorAuth", "\"basic\"");
                }
                verify(&project, &scope)
            }),
            Err(Error::Stale)
        ));
        assert_eq!(host.requests().len(), 1);
    }
    #[test]
    fn global_setting_needs_explicit_owned_lease_and_early_ticks_do_not_fetch() {
        let host = MockHost::install().with_capabilities(CAPS);
        host.on_fetch(response);
        host.set_setting("autoRefresh", "true");
        let mut runtime = Runtime::default();
        runtime.view_poll(&host.invocation());
        assert!(host.requests().is_empty());
        runtime.start_polling(&host.invocation().project().unwrap()).unwrap();
        runtime.view_poll(&host.invocation());
        assert!(host.requests().is_empty());
        runtime.polling.due_now();
        runtime.view_poll(&host.invocation());
        assert_eq!(host.requests().len(), ENDPOINTS.len());
        assert!(runtime.snapshot.is_some());
        runtime.stop_polling(&host.invocation().project().unwrap()).unwrap();
        runtime.polling.due_now();
        runtime.view_poll(&host.invocation());
        assert_eq!(host.requests().len(), ENDPOINTS.len());
    }
    #[test]
    fn polling_is_bounded_by_attempts_time_failure_and_scope_with_no_rearm_on_render() {
        let host = MockHost::install();
        host.set_setting("autoRefresh", "true");
        host.set_setting("pollAttempts", "1");
        let scope = Scope::current(&host.invocation().project().unwrap()).unwrap();
        let mut polling = Polling::default();
        polling.start(scope.clone()).unwrap();
        polling.due_now();
        assert_eq!(polling.due(&scope), Ok(true));
        assert!(polling.result(&scope, &Ok("ok".into())).unwrap().contains("finished its refreshes"));
        assert!(polling.lease.is_none());
        polling.start(scope.clone()).unwrap();
        polling.lease.as_mut().unwrap().expires = Instant::now();
        assert!(polling.due(&scope).is_err());
        assert!(polling.lease.is_none());
        host.set_setting("pollAttempts", "5");
        polling.start(scope.clone()).unwrap();
        for _ in 0..2 {
            polling.due_now();
            assert_eq!(polling.due(&scope), Ok(true));
            polling.result(&scope, &Err(Error::Failed("failure".into())));
        }
        assert!(polling.lease.is_none());
        polling.start(scope.clone()).unwrap();
        let mut other = scope.clone();
        other.root = "/other".into();
        assert!(polling.stop("/other").is_err());
        assert!(polling.due(&other).is_err());
        assert!(polling.lease.is_none());
        let mut runtime = Runtime::default();
        runtime.render_controls(&host.invocation()).unwrap();
        assert!(runtime.polling.lease.is_none());
    }
    #[test]
    fn unsupported_or_empty_objects_cannot_replace_prior_usable_endpoints() {
        let host = MockHost::install().with_capabilities(CAPS);
        host.on_fetch(response);
        let project = host.invocation().project().unwrap();
        let mut runtime = Runtime::default();
        runtime.refresh(&project).unwrap();
        let old = runtime.snapshot.clone();
        let stored = host.storage();
        host.on_fetch(|req| {
            if req.url.ends_with("/beans") {
                Ok(HttpResponse { status: 200, headers: vec![], body: b"{}".to_vec() })
            } else {
                response(req)
            }
        });
        assert!(runtime.refresh(&project).is_err());
        assert_eq!(runtime.snapshot, old);
        let key = Scope::current(&project).unwrap().key();
        assert_eq!(host.storage()[&key], stored[&key]);
        for endpoint in ENDPOINTS {
            assert!(!snapshot::supported_shape(endpoint, &serde_json::json!({})));
            assert!(!snapshot::supported_shape(endpoint, &serde_json::json!([])));
        }
        for (endpoint, value) in [
            ("beans", serde_json::json!({"contexts":{}})),
            ("env", serde_json::json!({"propertySources":[]})),
            ("conditions", serde_json::json!({"contexts":{}})),
            ("mappings", serde_json::json!({"contexts":{}})),
            ("configprops", serde_json::json!({"contexts":{}})),
            ("scheduledtasks", serde_json::json!({"cron":[]})),
            ("loggers", serde_json::json!({"loggers":{}})),
            ("caches", serde_json::json!({"cacheManagers":{}})),
        ] {
            assert!(snapshot::supported_shape(endpoint, &value), "{endpoint}");
        }
        assert!(!snapshot::supported_shape("beans", &serde_json::json!({"contexts":{"app":{"beans":{"bad":null}}}})));
    }
    #[test]
    fn bounded_supported_urls_auth_and_request_timeouts_do_not_expose_secret_errors() {
        let host = MockHost::install().with_capabilities(CAPS);
        host.set_secret("actuator-token", "FAKE-CONNECTION-SECRET");
        host.set_setting("timeoutMs", "30000");
        host.set_setting("refreshBudgetMs", "1000");
        host.on_fetch(|req| {
            assert!(req.timeout_ms.unwrap() <= 1000);
            response(req)
        });
        for url in ["http://localhost:8080/actuator", "https://127.0.0.1:8443/manage"] {
            for auth in ["none", "bearer", "basic"] {
                host.set_setting("actuatorUrl", &serde_json::to_string(url).unwrap());
                host.set_setting("actuatorAuth", &serde_json::to_string(auth).unwrap());
                let mut runtime = Runtime::default();
                runtime.refresh(&host.invocation().project().unwrap()).unwrap();
                let shown = runtime.render_controls(&host.invocation()).unwrap();
                assert!(!format!("{:?}", shown).contains("FAKE-CONNECTION-SECRET"));
            }
        }
        for url in [
            "http://example.com/actuator",
            "http://localhost@evil/actuator",
            "http://localhost:0/actuator",
            "ftp://localhost/actuator",
            "http://localhost/actuator?token=bad",
        ] {
            assert!(check_url(url).is_err());
        }
        host.set_setting("actuatorAuth", "\"oauth\"");
        assert!(Scope::current(&host.invocation().project().unwrap()).is_err());
        host.set_setting("actuatorAuth", "\"none\"");
        host.on_fetch(|_| Err(Error::Failed("FAKE-CONNECTION-SECRET raw auth context".into())));
        let mut runtime = Runtime::default();
        assert!(runtime.refresh(&host.invocation().project().unwrap()).is_err());
        assert!(!runtime.error.unwrap().contains("FAKE-CONNECTION-SECRET"));
    }
    #[test]
    fn unowned_ticks_scope_changes_cancel_and_clear_preserve_the_last_good_snapshot() {
        let host = MockHost::install().with_capabilities(CAPS);
        host.on_fetch(response);
        host.set_setting("autoRefresh", "true");
        let project = host.invocation().project().unwrap();
        let mut runtime = Runtime::default();
        runtime.refresh(&project).unwrap();
        let old = runtime.snapshot.clone();
        runtime.start_polling(&project).unwrap();
        host.set_project_root(None);
        runtime.polling.due_now();
        runtime.view_poll(&host.invocation());
        assert_eq!(runtime.snapshot, old);
        let requests = host.requests().len();
        host.set_project_root(Some("/project"));
        host.set_setting("actuatorUrl", "\"http://localhost:8081/actuator\"");
        runtime.view_poll(&host.invocation());
        assert!(runtime.polling.lease.is_none());
        assert_eq!(host.requests().len(), requests);
        assert_eq!(runtime.snapshot, old);
        host.set_setting("actuatorUrl", "\"http://localhost:8080/actuator\"");
        runtime.start_polling(&project).unwrap();
        runtime.clear(&project).unwrap();
        assert!(runtime.polling.lease.is_none());
        assert!(runtime.snapshot.is_none());
    }
    #[test]
    fn auth_failures_and_unhandled_redirect_responses_do_not_replace_last_good_or_log_headers() {
        let host = MockHost::install().with_capabilities(CAPS);
        host.on_fetch(response);
        let project = host.invocation().project().unwrap();
        let mut runtime = Runtime::default();
        runtime.refresh(&project).unwrap();
        let previous = runtime.snapshot.clone();
        host.on_fetch(|_| Ok(HttpResponse { status: 401, headers: vec![], body: b"{\"password\":\"not-a-snapshot\"}".to_vec() }));
        let error = runtime.refresh(&project).unwrap_err().to_string();
        assert!(error.contains("needs authentication"));
        assert_eq!(runtime.snapshot, previous);
        host.on_fetch(|_| {
            Ok(HttpResponse { status: 302, headers: vec![("Location".into(), "https://secret:fake@remote.invalid/".into())], body: vec![] })
        });
        assert!(runtime.refresh(&project).is_err());
        assert_eq!(runtime.snapshot, previous);
        assert!(!runtime.error.as_deref().unwrap().contains("secret:fake"));
        assert!(host.logs().iter().all(|(_, message)| !message.contains("secret:fake")));
        // Mock direct3xx proves only plugin non-adoption/no manual follow, not host per-hop safety.
    }
    #[test]
    fn cancelled_scheduled_or_explicit_refresh_cannot_leave_authorized_future_polls() {
        let host = MockHost::install().with_capabilities(CAPS);
        host.set_setting("autoRefresh", "true");
        host.on_fetch(response);
        let project = host.invocation().project().unwrap();
        let mut runtime = Runtime::default();
        runtime.refresh(&project).unwrap();
        let previous = runtime.snapshot.clone();
        runtime.start_polling(&project).unwrap();
        runtime.polling.due_now();
        host.on_fetch(|_| Err(Error::Cancelled));
        runtime.view_poll(&host.invocation());
        assert!(runtime.polling.lease.is_none());
        assert_eq!(runtime.snapshot, previous);
        runtime.start_polling(&project).unwrap();
        assert!(matches!(runtime.refresh(&project), Err(Error::Cancelled)));
        assert!(runtime.polling.lease.is_none());
        assert_eq!(runtime.snapshot, previous);
    }
    fn last_control_error(host: &MockHost) -> String {
        host.view_messages()
            .iter()
            .rev()
            .filter_map(|(_, raw)| serde_json::from_str::<Value>(raw).ok())
            .filter_map(|outer| serde_json::from_str::<Value>(outer["message"].as_str()?).ok())
            .find(|value| value["channel"] == "runtime-controls")
            .and_then(|value| value["model"]["error"].as_str().map(str::to_owned))
            .unwrap()
    }
    fn last_control_model(host: &MockHost) -> Value {
        host.view_messages()
            .iter()
            .rev()
            .filter_map(|(_, raw)| serde_json::from_str::<Value>(raw).ok())
            .filter_map(|outer| serde_json::from_str::<Value>(outer["message"].as_str()?).ok())
            .find(|value| value["channel"] == "runtime-controls" && value.get("model").is_some())
            .map(|value| value["model"].clone())
            .unwrap()
    }
    #[test]
    fn start_auto_refresh_command_needs_the_setting_and_starts_the_controls_page() {
        let host = MockHost::install().with_capabilities(CAPS);
        let mut runtime = Runtime::default();
        let off = runtime.run_command("poll-start", &host.invocation(), None).unwrap_err().to_string();
        assert!(off.contains("Turn on Allow auto-refresh in Settings › Plugins › Spring Runtime Insight"), "{off}");
        assert!(host.revealed_views().is_empty());
        host.set_setting("autoRefresh", "true");
        let started = runtime.run_command("poll-start", &host.invocation(), None).unwrap().unwrap();
        assert!(started.contains("Auto-refresh started"), "{started}");
        assert!(host.view_messages().iter().any(|(_, raw)| raw.contains("start-request")));
        message(&mut runtime, &host, "hello", FRAME, NONCE, 1, None);
        assert_eq!(last_control_model(&host)["autostart"], true);
        message(&mut runtime, &host, "start", FRAME, NONCE, 2, Some("/project"));
        assert!(runtime.polling.lease.is_some());
        message(&mut runtime, &host, "hello", FRAME, NONCE, 3, None);
        assert_eq!(last_control_model(&host)["autostart"], false, "one command starts once");
        assert!(host.requests().is_empty());
    }
    #[test]
    fn refresh_failure_reason_reaches_the_controls_page() {
        let host = MockHost::install().with_capabilities(CAPS);
        host.on_fetch(|_| Err(Error::Failed("offline".into())));
        let mut runtime = Runtime::default();
        message(&mut runtime, &host, "refresh", FRAME, NONCE, 1, Some("/project"));
        let error = last_control_error(&host);
        let error = error.as_str();
        assert!(error.contains("Could not reach"), "{error}");
        message(&mut runtime, &host, "tick", FRAME, NONCE, 2, Some("/project"));
        assert!(last_control_error(&host).contains("nothing ran"));
    }
}
