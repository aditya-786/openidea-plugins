//! A log snapshot in the panel: one `kubectl logs --tail=N --timestamps` run,
//! filtered in the plugin by text or regular expression, case and start time,
//! with soft wrap, hideable time and source fields, a copy of the shown lines and a download
//! through the owning project API. Managed follow logs are a separate Services path.

use crate::forward::{Scope, Selection, owner};
use openidea_plugin::views::{Body, Button, View};
use openidea_plugin::{Error, Invocation, NotificationLevel, host};
use regex_lite::{Regex, RegexBuilder, escape};

use crate::{KubeExplorer, REQUEST_TIMEOUT, WORKLOADS, escape_html, file_safe};
use sha2::{Digest, Sha256};

const DEFAULT_TAIL: f64 = 1000.0;
const MAX_LOG_BYTES: usize = 2 << 20;
const MAX_SHOWN_LINES: usize = 5000;

struct Source {
    owner_root: String,
    scope: Scope,
    selection: Selection,
    captured: std::time::Instant,
}

pub struct LogView {
    source: Source,
    refresh_failed: bool,
    title: String,
    kind: String,
    row: String,
    lines: Vec<String>,
    truncated: bool,
    filter: String,
    regex: bool,
    match_case: bool,
    since: String,
    until: String,
    wrap: bool,
    show_time: bool,
    show_source: bool,
}

struct Shown<'a> {
    lines: Vec<&'a str>,
    matcher: Option<Regex>,
    problem: Option<String>,
}

fn split_source(line: &str) -> (&str, &str) {
    match line.strip_prefix('[').and_then(|rest| rest.find("] ")) {
        Some(close) => (&line[..close + 3], &line[close + 3..]),
        None => ("", line),
    }
}

/// Supported UTC date/time prefixes, deliberately narrower than arbitrary RFC3339.
fn valid_time_prefix(value: &str) -> bool {
    let len=value.len();
    if !value.is_ascii() || !matches!(len,10|13|16|19|21..=29) {return false;}
    let pattern=b"0000-00-00T00:00:00";
    for (index,byte) in value.bytes().take(19).enumerate() {
        if pattern[index]==b'0' {if !byte.is_ascii_digit(){return false;}}
        else if byte!=pattern[index] {return false;}
    }
    if len>19 && (value.as_bytes()[19]!=b'.' || !value.as_bytes()[20..].iter().all(u8::is_ascii_digit)) {return false;}
    let number=|from,to|value.get(from..to).and_then(|v|v.parse::<u32>().ok());
    let (Some(year),Some(month),Some(day))=(number(0,4),number(5,7),number(8,10)) else{return false;};
    let days=match month {1|3|5|7|8|10|12=>31,4|6|9|11=>30,2=>if year%400==0||year%4==0&&year%100!=0{29}else{28},_=>return false};
    year!=0 && day>=1 && day<=days
        && (len<13||number(11,13).is_some_and(|v|v<=23))
        && (len<16||number(14,16).is_some_and(|v|v<=59))
        && (len<19||number(17,19).is_some_and(|v|v<=59))
}
fn normalize_time_bound(value:&str)->Result<String,&'static str> {
    let value=value.trim();if value.is_empty(){return Ok(String::new());}
    if value.len()>30||!value.is_ascii(){return Err("Use a bounded UTC date/time prefix; non-ASCII input and timezone offsets are unsupported.");}
    let value=value.replace(' ',"T");
    let normalized=value.strip_suffix('Z').unwrap_or(&value);
    if value.ends_with('Z')&&normalized.len()<13||!valid_time_prefix(normalized) {
        return Err("Use a valid UTC prefix: YYYY-MM-DD or YYYY-MM-DDTHH[:MM[:SS[.fraction]]], optionally ending Z; offsets are unsupported.");
    }
    Ok(normalized.into())
}
fn time_bounds(since:&str,until:&str)->Result<(String,String),&'static str> {
    let since=normalize_time_bound(since)?;let until=normalize_time_bound(until)?;
    let overlap=since.len().min(until.len());
    if overlap>0&&since.as_bytes()[..overlap]>until.as_bytes()[..overlap] {return Err("Since time prefix is after Until; keep the prior accepted range.");}
    Ok((since,until))
}
fn timestamp(line: &str) -> Option<&str> {
    let token=split_source(line).1.split(' ').next()?;
    let utc=token.strip_suffix('Z')?;
    (utc.len()>=19&&valid_time_prefix(utc)).then_some(token)
}
fn comparable_timestamp(stamp:&str)->String {
    let stamp=stamp.strip_suffix('Z').unwrap_or(stamp);
    let fraction=stamp.get(19..).and_then(|s|s.strip_prefix('.')).unwrap_or("");
    // timestamp() admits only full ASCII UTC stamps and at most9 fraction digits.
    format!("{}.{fraction:0<9}",&stamp[..19])
}

fn cap(text: String) -> (String, bool) {
    if text.len() <= MAX_LOG_BYTES {
        return (text, false);
    }
    let mut from = text.len() - MAX_LOG_BYTES;
    while !text.is_char_boundary(from) {
        from += 1;
    }
    let from = text[from..].find('\n').map_or(from, |newline| from + newline + 1);
    (text[from..].to_owned(), true)
}

fn export_path(root: &str, value: &str) -> Result<String, Error> {
    let value = if value.starts_with('/') { value.strip_prefix(&format!("{}/", root.trim_end_matches('/'))).ok_or(Error::Stale)? } else { value };
    if value.is_empty() || value.len() > 1024 || value.chars().any(|c| c.is_control() || matches!(c, '\\' | '*' | '?' | '[' | ']' | '{' | '}'))
        || value.split('/').any(|part| part.is_empty() || matches!(part, "." | "..")) {
        return Err("Choose a concrete project path without traversal or wildcards.".into());
    }
    Ok(format!("{}/{value}", root.trim_end_matches('/')))
}
fn export_vacant(project: &openidea_plugin::Project, root: &str, path: &str) -> Result<(), Error> {
    if host::project_of(path).and_then(|p| p.root().ok()).as_deref() != Some(root) || project.root()? != root { return Err(Error::Stale); }
    if project.exists(path)? { return Err("Export destination already exists; choose a new file.".into()); }
    match project.read_file(path) {
        Ok(_) => return Err("Export destination has an open buffer; choose a new file.".into()),
        Err(Error::NotFound(_)) => {},
        Err(error) => return Err(error),
    }
    let (parent, _) = path.rsplit_once('/').ok_or(Error::Stale)?;
    if project.stat(parent)?.kind != openidea_plugin::FileKind::Directory { return Err("Export parent directory must already exist.".into()); }
    Ok(())
}

impl LogView {
    fn shown(&self) -> Shown<'_> {
        let mut problem = None;
        let matcher = match self.filter.is_empty() {
            true => None,
            false => {
                let pattern = if self.regex { self.filter.clone() } else { escape(&self.filter) };
                match RegexBuilder::new(&pattern).case_insensitive(!self.match_case).size_limit(1 << 20).build() {
                    Ok(matcher) => Some(matcher),
                    Err(error) => {
                        problem = Some(format!("invalid regular expression: {error}"));
                        None
                    }
                }
            }
        };
        let (since,until)=match time_bounds(&self.since,&self.until) {
            Ok(bounds)=>bounds,
            Err(message)=>return Shown {lines:Vec::new(),matcher,problem:Some(message.into())},
        };
        let mut in_range = since.is_empty();
        let mut lines = Vec::new();
        for line in &self.lines {
            if let Some(stamp) = timestamp(line) {
                let stamp=comparable_timestamp(stamp);
                let after = since.is_empty() || stamp.as_bytes()[..since.len()] >= *since.as_bytes();
                let before = until.is_empty() || stamp.as_bytes()[..until.len()] <= *until.as_bytes();
                in_range = after && before;
            }
            if in_range && matcher.as_ref().is_none_or(|m| m.is_match(line)) {
                lines.push(line.as_str());
            }
        }
        if lines.len() > MAX_SHOWN_LINES {
            lines.drain(..lines.len() - MAX_SHOWN_LINES);
        }
        Shown { lines, matcher, problem }
    }

    fn display(&self, line: &str) -> String {
        let (source, rest) = split_source(line);
        let rest = match (self.show_time, timestamp(line)) {
            (false, Some(stamp)) => rest[stamp.len()..].trim_start(),
            _ => rest,
        };
        match self.show_source {
            true => format!("{source}{rest}"),
            false => rest.to_owned(),
        }
    }

    fn displayed(&self, shown: &Shown) -> String {
        shown.lines.iter().map(|line| self.display(line)).collect::<Vec<_>>().join("\n")
    }

    fn page(&self, shown: &Shown) -> String {
        let body: Vec<String> = shown
            .lines
            .iter()
            .map(|raw| {
                let line = self.display(raw);
                let line = line.as_str();
                match &shown.matcher {
                None => escape_html(line),
                Some(matcher) => {
                    let mut out = String::new();
                    let mut at = 0;
                    for found in matcher.find_iter(line).filter(|m| !m.is_empty()) {
                        out.push_str(&escape_html(&line[at..found.start()]));
                        out.push_str(&format!("<mark>{}</mark>", escape_html(found.as_str())));
                        at = found.end();
                    }
                    out.push_str(&escape_html(&line[at..]));
                    out
                }
                }
            })
            .collect();
        let white_space = if self.wrap { "pre-wrap" } else { "pre" };
        format!(
            "<style>:root{{--bg:Canvas;--fg:CanvasText}}body{{margin:0;background:var(--openidea-bg,var(--bg));color:var(--openidea-fg,var(--fg))}}\
             pre{{margin:0;padding:8px 10px;font:12px/1.5 var(--openidea-font-mono,ui-monospace,Menlo,monospace);white-space:{white_space};word-break:break-all}}\
             mark{{background:color-mix(in srgb,var(--openidea-warning,#d6ae58) 55%,transparent);color:inherit}}</style><pre>{}</pre>\
             <script>window.openidea&&openidea.onMessage(function(m){{if(m==='end')window.scrollTo(0,document.documentElement.scrollHeight)}})</script>",
            body.join("\n")
        )
    }

    pub fn render(&self) -> View {
        let shown = self.shown();
        let mut status = format!(
            "{} · {} · Project {} · snapshot {}s old: {} of {} lines{}",
            self.title,
            self.source.scope.label(),
            self.source.owner_root,
            self.source.captured.elapsed().as_secs(),
            shown.lines.len(),
            self.lines.len(),
            if self.refresh_failed {
                "; refresh failed; previous matching snapshot retained"
            } else {
                ""
            }
        );
        if self.truncated {
            status.push_str(" (older output cut)");
        }
        if let Some(problem) = &shown.problem {
            status = format!("{status}; {problem}");
        }
        let toggle = |label: &str, on: bool| format!("{label}: {}", if on { "on" } else { "off" });
        View::new(Body::Html(self.page(&shown)))
            .status(&status)
            .button(Button::new("back", "Back").icon("arrow-left"))
            .button(Button::new("log-filter", "Filter…").icon("search"))
            .button(Button::new("log-since", "Since…").icon("history"))
            .button(Button::new("log-until", "Until…"))
            .button(Button::new("log-regex", &toggle("Regex", self.regex)))
            .button(Button::new("log-case", &toggle("Match case", self.match_case)))
            .button(Button::new("log-wrap", &toggle("Wrap", self.wrap)))
            .button(Button::new("log-time", &toggle("Time", self.show_time)))
            .button(Button::new("log-source", &toggle("Source", self.show_source)))
            .button(Button::new("log-end", "Scroll to end").icon("arrow-down"))
            .button(Button::new("log-clear", "Clear").icon("trash"))
            .button(Button::new("log-refresh", "Refresh").icon("refresh"))
            .button(Button::new("log-copy", "Copy").icon("copy"))
            .button(Button::new("log-download", "Download…").icon("download"))
    }
}

/// Inside the project only: a shared directory such as /tmp would make the file readable by every local user.
fn suggested_path(root: Option<&str>, file: &str) -> Option<String> {
    root.map(|root| format!("{}/{file}", root.trim_end_matches('/')))
}

impl KubeExplorer {
    pub(super) fn expire_log_context(&mut self, ctx: Option<&Invocation>) {
        let Some(view) = &self.log_view else { return };
        let current = owner(ctx).ok().map(|(_, root)| root);
        if current.as_deref() != Some(view.source.owner_root.as_str())
            || view.source.selection != self.forward_selection()
            || !view.source.scope.configuration_current()
        {
            self.log_view = None;
            crate::refresh_panel();
        }
    }

    fn download_log(&self, ctx: Option<&Invocation>) -> Result<(), Error> {
        let view = self.log_view.as_ref().ok_or(Error::Stale)?;
        let (project, root) = owner(ctx)?;
        if root != view.source.owner_root || view.source.selection != self.forward_selection() || !view.source.scope.configuration_current() {
            return Err(Error::Stale);
        }
        let shown = view.shown();
        if shown.problem.is_some() { return Err("Fix the log filter before exporting.".into()); }
        let file = format!("{}.log", file_safe(&format!("{}-{}", view.kind, view.row)));
        let suggestion = suggested_path(Some(&root), &file);
        let Some(path) = host::input("Export shown snapshot to a new file inside its owning project (existing/dirty files are refused)", suggestion.as_deref())? else { return Ok(()) };
        let path = export_path(&root, path.trim())?;
        export_vacant(&project, &root, &path)?;
        let mut text = view.displayed(&shown);
        text.push('\n');
        let digest = format!("{:x}", Sha256::digest(text.as_bytes()));
        let preview = format!(
            "Project: {root}\nDestination: {path}\nShown snapshot lines: {}\nExact UTF-8 bytes: {}\nSHA-256: {digest}\n\nExport the currently displayed filtered snapshot only? It may contain sensitive log data; the new project file uses normal project filesystem permissions, not private credential storage. No cluster request or shell writer runs. Existing files/open buffers are checked twice; the host rejects unsaved buffers. This project API has no exclusive-create protection against a final external filesystem race.",
            shown.lines.len(), text.len()
        );
        if !ui_kit::prompts::confirm_exact("Export shown log snapshot?", &preview)? { return Ok(()); }
        if owner(ctx)?.1 != root || view.source.selection != self.forward_selection() || !view.source.scope.configuration_current() {
            return Err(Error::Stale);
        }
        export_vacant(&project, &root, &path)?;
        project.write_file(&path, &text)?;
        let _ = host::open_location(&path, 0, 0);
        crate::config::notify_success("Logs saved", &format!("{} lines in {path}", shown.lines.len()));
        Ok(())
    }

    fn fetch_log_lines(
        &self,
        kind: &str,
        row: &str,
        scope: &Scope,
        project: &openidea_plugin::Project,
    ) -> Result<(Vec<String>, bool), Error> {
        let (_, name) = self.split_scope(kind, row)?;
        let tail = host::setting_number("logViewerLines")
            .unwrap_or(DEFAULT_TAIL)
            .clamp(10.0, 20000.0) as u32;
        let tail = format!("--tail={tail}");
        let target = format!("{kind}/{name}");
        let args: Vec<&str> = match kind {
            "pods" => vec![
                "logs",
                &name,
                "--timestamps",
                "--prefix",
                "--all-containers",
                &tail,
                REQUEST_TIMEOUT,
            ],
            _ => vec![
                "logs",
                &target,
                "--all-pods=true",
                "--all-containers",
                "--max-log-requests=20",
                "--timestamps",
                "--prefix",
                &tail,
                REQUEST_TIMEOUT,
            ],
        };
        let run = |args: &[&str]| {
            project.run_process(crate::KUBECTL, &scope.args(args), None).map_err(|error| crate::tool_error("Reading logs", self.context.as_deref(), error))
        };
        let mut out = run(&args)?;
        if out.status != Some(0) && out.stderr.contains("unknown flag") && args.contains(&"--all-pods=true") {
            let older_kubectl: Vec<&str> = args.iter().copied().filter(|arg| *arg != "--all-pods=true").collect();
            out = run(&older_kubectl)?;
        }
        if out.status!=Some(0) {return Err(Error::Failed(crate::failure_reason("Reading logs", self.context.as_deref(), &out.stderr)));}
        let (text, truncated) = cap(out.stdout);
        Ok((text.lines().map(str::to_owned).collect(), truncated))
    }

    pub fn open_log_view(
        &mut self,
        kind: &str,
        row: &str,
        ctx: Option<&Invocation>,
    ) -> Result<(), Error> {
        self.expire_log_context(ctx);
        if kind != "pods" && !WORKLOADS.contains(&kind) {
            return Err(format!("Logs do not apply to {kind}.").into());
        }
        let (project, owner_root) = owner(ctx)?;
        let (namespace, _) = self.split_scope(kind, row)?;
        let scope = Scope::capture_for_project(self.context.as_deref(), namespace.as_deref(), &project)?;
        let _task = host::begin_task(&format!("Reading logs of {kind}/{row}"), false);
        let (lines, truncated) = self.fetch_log_lines(kind, row, &scope, &project)?;
        self.ensure_panel_replace()?;
        let previous = self.log_view.take();
        self.begin_panel()?;
        self.log_view = Some(LogView {
            source: Source {
                owner_root,
                scope,
                selection: self.forward_selection(),
                captured: std::time::Instant::now(),
            },
            refresh_failed: false,
            title: format!("Logs {kind}/{row}"),
            kind: kind.to_owned(),
            row: row.to_owned(),
            lines,
            truncated,
            filter: previous
                .as_ref()
                .map(|v| v.filter.clone())
                .unwrap_or_default(),
            regex: previous.as_ref().is_some_and(|v| v.regex),
            match_case: previous.as_ref().is_some_and(|v| v.match_case),
            since: String::new(),
            until: String::new(),
            wrap: previous.as_ref().is_none_or(|v| v.wrap),
            show_time: previous.as_ref().is_none_or(|v| v.show_time),
            show_source: previous.as_ref().is_none_or(|v| v.show_source),
        });
        self.details = None;
        crate::refresh_views();
        Ok(())
    }

    pub fn log_button(&mut self, button: &str, ctx: Option<&Invocation>) -> Result<(), Error> {
        self.expire_log_context(ctx);
        if self.log_view.is_none() { return Err(Error::Stale); }
        let Some(view) = &mut self.log_view else { return Ok(()) };
        match button {
            "log-filter" => {
                let Some(text) = host::input("Show lines matching (empty: all)", Some(&view.filter))? else { return Ok(()) };
                view.filter = text.trim().to_owned();
            }
            "log-since" => {
                let Some(text) = host::input("Show lines from this UTC prefix (YYYY-MM-DD or YYYY-MM-DDTHH:MM[:SS[.fraction]], optional Z; empty clears)", Some(&view.since))?
                else {
                    return Ok(());
                };
                let candidate=text.trim().to_owned();
                time_bounds(&candidate,&view.until).map_err(|message|Error::Invalid(message.into()))?;
                view.since=candidate;
            }
            "log-until" => {
                let Some(text) = host::input("Show lines up to this UTC prefix (YYYY-MM-DD or YYYY-MM-DDTHH:MM[:SS[.fraction]], optional Z; empty clears)", Some(&view.until))? else {
                    return Ok(());
                };
                let candidate=text.trim().to_owned();
                time_bounds(&view.since,&candidate).map_err(|message|Error::Invalid(message.into()))?;
                view.until=candidate;
            }
            "log-time" => view.show_time = !view.show_time,
            "log-source" => view.show_source = !view.show_source,
            "log-end" => return crate::ui_group::post_view_message(crate::PANEL, "end"),
            "log-clear" => view.lines.clear(),
            "log-download" => return self.download_log(ctx),
            "log-regex" => view.regex = !view.regex,
            "log-case" => view.match_case = !view.match_case,
            "log-wrap" => view.wrap = !view.wrap,
            "log-copy" => {
                let shown = view.shown();
                let text = view.displayed(&shown);
                match host::copy_to_clipboard(&text) {
                    Ok(()) => crate::config::notify_success("Logs copied", &format!("{} lines", shown.lines.len())),
                    Err(Error::Denied(_)) => host::notify(
                        NotificationLevel::Info,
                        "Copy logs",
                        &ui_kit::denied::message("Copying the shown lines", "clipboard", "Kube Explorer"),
                    ),
                    Err(error) => return Err(error),
                }
                return Ok(());
            }
            "log-refresh" => {
                let (kind, row, scope, root) = (
                    view.kind.clone(),
                    view.row.clone(),
                    view.source.scope.clone(),
                    view.source.owner_root.clone(),
                );
                let (project,current_root)=owner(ctx)?;
                if current_root!=root {self.log_view=None;crate::refresh_panel();return Err(Error::Stale);}
                let (namespace, _) = self.split_scope(&kind, &row)?;
                if Scope::capture_for_project(self.context.as_deref(), namespace.as_deref(), &project)
                    .ok()
                    .as_ref()
                    != Some(&scope)
                {
                    self.log_view = None;
                    crate::refresh_panel();
                    return Err(Error::Stale);
                }
                let result = self.fetch_log_lines(&kind, &row, &scope, &project);
                if let Some(view) = &mut self.log_view {
                    match result {
                        Ok((lines, truncated)) => {
                            view.lines = lines;
                            view.truncated = truncated;
                            view.source.captured = std::time::Instant::now();
                            view.refresh_failed = false;
                        }
                        Err(error) => {
                            view.refresh_failed = true;
                            crate::refresh_panel();
                            return Err(error);
                        }
                    }
                }
                self.new_rich_callback();
            }
            _ => return Ok(()),
        }
        crate::refresh_views();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use openidea_plugin::ProcessOutput;
    use openidea_plugin::mock::MockHost;

    use crate::KUBECTL;

    use super::*;

    const LOG: &str = "2025-11-06T09:00:00.1Z start <ok>\n2025-11-06T10:00:00.2Z ERROR boom 42\ncontinued without stamp\n2025-11-06T11:00:00.3Z info Done\n2025-11-07T00:00:01Z error again\n";

    fn view(filter: &str) -> LogView {
        LogView {
            source: Source {
                owner_root: "/project".into(),
                scope: Scope::test_log_scope(),
                selection: (None, Some("shop".into()), None),
                captured: std::time::Instant::now(),
            },
            refresh_failed: false,
            title: "Logs pods/web".into(),
            kind: "pods".into(),
            row: "web".into(),
            lines: LOG.lines().map(str::to_owned).collect(),
            truncated: false,
            filter: filter.into(),
            regex: false,
            match_case: false,
            since: String::new(),
            until: String::new(),
            wrap: true,
            show_time: true,
            show_source: true,
        }
    }

    fn texts(view: &LogView) -> Vec<String> {
        view.shown().lines.iter().map(|l| l.to_string()).collect()
    }

    #[test]
    fn foreign_project_render_and_copy_discard_previous_content_without_io() {
        use openidea_plugin::views::{ViewEvent, Views};
        let (host, runs) = host(LOG);
        let mut kube = KubeExplorer::default();
        kube.open_log_view("pods", "web", Some(&host.invocation()))
            .unwrap();
        host::copy_to_clipboard("sentinel").unwrap();
        host.set_project_root(Some("/foreign-project"));
        assert!(
            kube.handle(
                crate::TREE,
                ViewEvent::Button(kube.test_control_key("log-copy")),
                &host.invocation()
            )
            .is_err()
        );
        assert!(kube.log_view.is_none());
        assert_eq!(host.clipboard().as_deref(), Some("sentinel"));
        let panel = kube
            .render("resource-details", None, &host.invocation())
            .unwrap();
        assert!(!matches!(panel.body, Body::Html(_)));
        assert_eq!(
            runs.borrow()
                .iter()
                .filter(|a| a.iter().any(|v| v == "logs"))
                .count(),
            1
        );
    }

    #[test]
    fn absent_owner_cannot_open_or_export_a_snapshot() {
        let (host, runs) = host(LOG);
        let mut kube = KubeExplorer::default();
        host.set_project_root(None);
        assert!(
            kube.open_log_view("pods", "web", Some(&host.invocation()))
                .is_err()
        );
        assert!(runs.borrow().is_empty());
        host.set_project_root(Some("/project"));
        kube.open_log_view("pods", "web", Some(&host.invocation()))
            .unwrap();
        host.set_project_root(None);
        assert!(
            kube.log_button("log-download", Some(&host.invocation()))
                .is_err()
        );
        assert!(kube.log_view.is_none());
        assert_eq!(runs.borrow().len(), 1);
    }

    #[test]
    fn namespace_switch_cannot_refresh_old_target_in_new_scope() {
        use openidea_plugin::views::{ViewEvent, Views};
        let (host, runs) = host(LOG);
        let mut kube = KubeExplorer {
            namespace: Some("shop".into()),
            ..Default::default()
        };
        kube.open_log_view("pods", "web", Some(&host.invocation()))
            .unwrap();
        kube.handle(
            crate::TREE,
            ViewEvent::Activate(kube.test_choice_key("ns/other",&host.invocation())),
            &host.invocation(),
        )
        .unwrap();
        assert!(
            kube.handle(
                crate::TREE,
                ViewEvent::Button(kube.test_control_key("log-refresh")),
                &host.invocation()
            )
            .is_err()
        );
        assert!(kube.log_view.is_none());
        assert_eq!(
            runs.borrow().len(),
            1,
            "no old target read through the new namespace"
        );
    }

    #[test]
    fn changed_mapping_discards_unconfirmed_snapshot_instead_of_retaining_on_failure() {
        let (host, _) = host(LOG);
        let mut kube = KubeExplorer::default();
        kube.open_log_view("pods", "web", Some(&host.invocation()))
            .unwrap();
        host.on_process(KUBECTL, |_, _| {
            Err(Error::Failed("cannot confirm mapping".into()))
        });
        assert!(matches!(
            kube.log_button("log-refresh", Some(&host.invocation())),
            Err(Error::Stale)
        ));
        assert!(kube.log_view.is_none());
    }

    #[test]
    fn same_alias_changed_server_refuses_before_fetching_new_logs() {
        let (host, _) = host(LOG);
        let mut kube = KubeExplorer {
            context: Some("dev".into()),
            ..Default::default()
        };
        kube.open_log_view("pods", "web", Some(&host.invocation()))
            .unwrap();
        let reads = std::rc::Rc::new(std::cell::Cell::new(0));
        let seen = reads.clone();
        host.on_process(KUBECTL,move |args,_| {
            if args.iter().any(|a|a=="logs") { seen.set(seen.get()+1); }
            Ok(ProcessOutput { status:Some(0),stdout:r#"{"contexts":[{"name":"dev","context":{"cluster":"local","namespace":"shop"}}],"clusters":[{"name":"local","cluster":{"server":"https://changed.invalid"}}]}"#.into(),stderr:String::new() })
        });
        assert!(matches!(
            kube.log_button("log-refresh", Some(&host.invocation())),
            Err(Error::Stale)
        ));
        assert!(kube.log_view.is_none());
        assert_eq!(reads.get(), 0);
    }

    #[test]
    fn revoked_project_read_clears_cache_before_copy() {
        let (host, _) = host(LOG);
        let mut kube = KubeExplorer::default();
        kube.open_log_view("pods", "web", Some(&host.invocation()))
            .unwrap();
        host::copy_to_clipboard("sentinel").unwrap();
        let host = host.with_capabilities(&["process:kubectl", "storage", "clipboard"]);
        assert!(
            kube.log_button("log-copy", Some(&host.invocation()))
                .is_err()
        );
        assert!(kube.log_view.is_none());
        assert_eq!(host.clipboard().as_deref(), Some("sentinel"));
    }

    #[test]
    fn failed_log_read_keeps_only_verified_same_owner_scope_snapshot() {
        let (host, _) = host(LOG);
        let mut kube = KubeExplorer {
            context: Some("dev".into()),
            namespace: Some("shop".into()),
            ..Default::default()
        };
        kube.open_log_view("pods", "web", Some(&host.invocation()))
            .unwrap();
        host.on_process(KUBECTL, |args, _| {
            if args.iter().any(|a|a=="view") { Ok(ProcessOutput { status:Some(0),stdout:r#"{"contexts":[{"name":"dev","context":{"cluster":"local","namespace":"shop"}}],"clusters":[{"name":"local","cluster":{"server":"https://dev.invalid"}}]}"#.into(),stderr:String::new() }) }
            else { Err(Error::Failed("synthetic log read failure".into())) }
        });
        assert!(
            kube.log_button("log-refresh", Some(&host.invocation()))
                .is_err()
        );
        let snapshot = kube.log_view.as_ref().unwrap();
        assert_eq!(snapshot.lines.join("\n"), LOG.trim_end());
        assert!(
            snapshot
                .render()
                .status
                .unwrap()
                .contains("previous matching snapshot retained")
        );
    }

    #[test]
    fn cached_render_reports_provenance_without_scope_discovery() {
        use openidea_plugin::views::Views;
        let (host, _) = host(LOG);
        let mut kube = KubeExplorer::default();
        kube.open_log_view("pods", "web", Some(&host.invocation()))
            .unwrap();
        let before = host.process_runs().len();
        let panel = kube
            .render("resource-details", None, &host.invocation())
            .unwrap();
        let status = panel.status.unwrap();
        for text in ["dev / shop", "/project", "snapshot", "pods/web"] {
            assert!(status.contains(text), "{status}");
        }
        assert_eq!(host.process_runs().len(), before);
    }

    #[test]
    fn all_namespace_row_reads_pinned_concrete_namespace_and_actor_stop_clears_cache() {
        use openidea_plugin::Plugin;
        let (host, runs) = host(LOG);
        let mut kube = KubeExplorer {
            namespace: Some(crate::ALL_NAMESPACES.into()),
            ..Default::default()
        };
        kube.open_log_view("pods", "team-a/web", Some(&host.invocation()))
            .unwrap();
        assert!(runs.borrow()[0].contains(&"--namespace=team-a".into()));
        assert!(!runs.borrow()[0].iter().any(|v| v == "--namespace=*"));
        assert!(
            kube.log_view
                .as_ref()
                .unwrap()
                .render()
                .status
                .unwrap()
                .contains("dev / team-a")
        );
        kube.deactivate();
        assert!(kube.log_view.is_none());
    }

    #[test]
    fn configuration_or_selected_resource_change_invalidates_cached_log_actions() {
        let (host, _) = host(LOG);
        let mut kube = KubeExplorer::default();
        kube.open_log_view("pods", "web", Some(&host.invocation()))
            .unwrap();
        host.set_setting("kubeconfig", "\"/other/config\"");
        assert!(
            kube.log_button("log-copy", Some(&host.invocation()))
                .is_err()
        );
        assert!(kube.log_view.is_none());
        host.set_setting("kubeconfig", "\"\"");
        kube.open_log_view("pods", "web", Some(&host.invocation()))
            .unwrap();
        kube.selected = Some(("pods".into(), "other".into()));
        assert!(
            kube.log_button("log-copy", Some(&host.invocation()))
                .is_err()
        );
        assert!(kube.log_view.is_none());
    }

    #[test]
    fn text_filter_ignores_case_until_match_case_is_on() {
        let mut v = view("error");
        assert_eq!(texts(&v).len(), 2);
        v.match_case = true;
        assert_eq!(texts(&v), ["2025-11-07T00:00:01Z error again"]);
    }

    #[test]
    fn plain_text_is_not_a_pattern_but_regex_mode_is() {
        let mut v = view("boom 4.");
        assert!(texts(&v).is_empty());
        v.regex = true;
        assert_eq!(texts(&v).len(), 1);
        v.filter = "(".into();
        assert_eq!(texts(&v).len(), 5);
        assert!(v.shown().problem.unwrap().contains("invalid regular expression"));
    }

    #[test]
    fn invalid_unicode_time_filter_never_panics_or_exposes_unfiltered_snapshot() {
        let _host=MockHost::install();let mut v=view("");v.since="😀".repeat(10);
        let shown=v.shown();assert!(shown.problem.is_some());assert!(shown.lines.is_empty());
    }
    #[test]
    fn utc_prefix_bounds_validate_calendar_precision_and_reject_unsupported_offsets() {
        for valid in ["", "2024-02-29", "2025-11-06 10", "2025-11-06T10:30", "2025-11-06T10:30:59.123456789Z", "2025-11-06T10Z"] {
            assert!(normalize_time_bound(valid).is_ok(),"{valid}");
        }
        for invalid in ["2025-02-29","2025-13-01","0000-01-01","2025-11-06T24","2025-11-06T10:61","2025-11-06T10:30:60","2025-11-06T10:30:00.1234567890Z","2025-11-06T10:30:00+01:00","2025-11-06T10:30:00.😀","2025-11-06Z"] {
            assert!(normalize_time_bound(invalid).is_err(),"{invalid}");
        }
        assert!(time_bounds("2025-11-07","2025-11-06").is_err());
        assert!(time_bounds("2025-11-06T20:30","2025-11-06").is_ok(),"Until date includes the whole day");
    }
    #[test]
    fn fractional_utc_comparison_pads_absent_fraction_and_preserves_prefix_precision() {
        let _host=MockHost::install();let mut v=view("");
        v.lines=["2025-11-06T10:00:00Z zero","2025-11-06T10:00:00.01Z hundredth","2025-11-06T10:00:00.1Z tenth","2025-11-06T10:00:00.19Z nineteen","2025-11-06T10:00:00.2Z fifth"].into_iter().map(str::to_owned).collect();
        v.since="2025-11-06T10:00:00.1".into();v.until=v.since.clone();
        assert_eq!(texts(&v),["2025-11-06T10:00:00.1Z tenth","2025-11-06T10:00:00.19Z nineteen"]);
        v.since="2025-11-06T10:00:00.000000001Z".into();v.until=String::new();assert_eq!(texts(&v).len(),4);
        v.since=String::new();v.until="2025-11-06T10:00:00.010".into();
        assert_eq!(texts(&v),["2025-11-06T10:00:00Z zero","2025-11-06T10:00:00.01Z hundredth"]);
    }
    #[test]
    fn malformed_unicode_timestamp_payload_is_retained_as_body_without_time_stripping() {
        let _host=MockHost::install();let mut v=view("");
        v.lines=["2025-11-06T10:00:00Z start","2025-11-06T😀 malformed timestamp-like payload","continuation 😀"].into_iter().map(str::to_owned).collect();
        v.since="2025-11-06T09:00".into();v.until="2025-11-06T10:00".into();
        assert_eq!(texts(&v).len(),3);v.show_time=false;
        assert_eq!(v.display(&v.lines[1]),v.lines[1]);assert_eq!(v.display(&v.lines[0]),"start");
    }
    #[test]
    fn invalid_or_reversed_public_time_input_preserves_last_accepted_filters_and_snapshot() {
        let (h,runs)=host(LOG);let mut kube=KubeExplorer{namespace:Some("shop".into()),..Default::default()};
        kube.open_log_view("pods","web",Some(&h.invocation())).unwrap();
        h.answer_next_prompt(Some(&["2025-11-06T10:00"]));kube.log_button("log-since",Some(&h.invocation())).unwrap();
        h.answer_next_prompt(Some(&["2025-11-06"]));kube.log_button("log-until",Some(&h.invocation())).unwrap();
        let previous=kube.log_view.as_ref().unwrap();let since=previous.since.clone();let until=previous.until.clone();let shown=previous.displayed(&previous.shown());let count=runs.borrow().len();
        for (button,input) in [("log-since","😀😀😀😀😀😀😀😀😀😀"),("log-until","2025-11-05"),("log-until","2025-11-06T10:00:00+01:00")] {
            h.answer_next_prompt(Some(&[input]));assert!(matches!(kube.log_button(button,Some(&h.invocation())),Err(Error::Invalid(_))));
            let view=kube.log_view.as_ref().unwrap();assert_eq!((&view.since,&view.until),(&since,&until));assert_eq!(view.displayed(&view.shown()),shown);
        }
        assert_eq!(runs.borrow().len(),count,"client-side filter never requeries logs");
        h.answer_next_prompt(None);kube.log_button("log-since",Some(&h.invocation())).unwrap();assert_eq!(kube.log_view.as_ref().unwrap().since,since);
    }
    #[test]
    fn since_keeps_lines_from_that_time_and_stampless_lines_follow_their_predecessor() {
        let mut v = view("");
        v.since = "2025-11-06 10:00".into();
        assert_eq!(texts(&v).len(), 4);
        assert!(texts(&v)[1].starts_with("continued"));
        v.since = "2025-11-07".into();
        assert_eq!(texts(&v), ["2025-11-07T00:00:01Z error again"]);
    }

    #[test]
    fn the_page_escapes_html_marks_matches_and_wraps_on_request() {
        let mut v = view("ok");
        let page = v.page(&v.shown());
        assert!(page.contains("start &lt;<mark>ok</mark>&gt;") && page.contains("pre-wrap"));
        v.wrap = false;
        assert!(v.page(&v.shown()).contains("white-space:pre;"));
    }

    #[test]
    fn output_beyond_the_cap_keeps_the_newest_whole_lines() {
        let line = "x".repeat(99) + "\n";
        let (kept, truncated) = cap(line.repeat(MAX_LOG_BYTES / 100 + 50));
        assert!(truncated && kept.len() <= MAX_LOG_BYTES && kept.ends_with('\n') && kept.lines().all(|l| l.len() == 99));
        assert!(!cap("short\n".into()).1);
    }

    fn host(output: &str) -> (MockHost, std::rc::Rc<std::cell::RefCell<Vec<Vec<String>>>>) {
        let host = MockHost::install().with_capabilities(&[
            "process:kubectl",
            "project.read",
            "storage",
            "clipboard",
        ]);
        let runs: std::rc::Rc<std::cell::RefCell<Vec<Vec<String>>>> = Default::default();
        let seen = runs.clone();
        let output = output.to_owned();
        host.on_process(KUBECTL, move |args, _| {
            if args.iter().any(|a| a == "current-context") { return Ok(ProcessOutput { status: Some(0), stdout: "dev".into(), stderr: String::new() }); }
            if args.iter().any(|a| a == "view") { return Ok(ProcessOutput { status: Some(0), stdout: r#"{"contexts":[{"name":"dev","context":{"cluster":"local","namespace":"shop"}}],"clusters":[{"name":"local","cluster":{"server":"https://dev.invalid"}}]}"#.into(), stderr: String::new() }); }
            seen.borrow_mut().push(args.to_vec());
            Ok(ProcessOutput { status: Some(0), stdout: output.clone(), stderr: String::new() })
        });
        (host, runs)
    }

    #[test]
    fn owned_log_capture_fetch_and_refresh_use_one_project_cwd_for_relative_configuration() {
        let (h,_)=host(LOG);h.set_setting("kubeconfig","\"configs/relative-kubeconfig\"");
        let mut kube=KubeExplorer{namespace:Some("shop".into()),..Default::default()};
        kube.open_log_view("pods","web",Some(&h.invocation())).unwrap();
        kube.log_button("log-refresh",Some(&h.invocation())).unwrap();
        assert!(h.process_runs().len()>=5);
        assert!(h.process_runs().iter().all(|r|r.cwd.as_deref()==Some("/project")),"{:?}",h.process_runs());
    }
    #[test]
    fn owned_log_read_errors_withhold_raw_tool_values_and_keep_verified_snapshot() {
        let (h,_)=host(LOG);let mut kube=KubeExplorer{context:Some("dev".into()),namespace:Some("shop".into()),..Default::default()};
        kube.open_log_view("pods","web",Some(&h.invocation())).unwrap();
        h.on_process(KUBECTL,|args,_| {
            if args.iter().any(|a|a=="view") {Ok(ProcessOutput{status:Some(0),stdout:r#"{"contexts":[{"name":"dev","context":{"cluster":"local","namespace":"shop"}}],"clusters":[{"name":"local","cluster":{"server":"https://dev.invalid"}}]}"#.into(),stderr:String::new()})}
            else {Ok(ProcessOutput{status:Some(1),stdout:"private-log-failure-body".into(),stderr:"private-credential-error-value".into()})}
        });
        let error=kube.log_button("log-refresh",Some(&h.invocation())).unwrap_err().to_string();
        assert!(!error.contains("private-log")&&!error.contains("private-credential"));
        let view=kube.log_view.as_ref().unwrap();assert_eq!(view.lines.join("\n"),LOG.trim_end());assert!(view.refresh_failed);
        assert!(h.process_runs().iter().all(|r|r.cwd.as_deref()==Some("/project")));
        let before=h.process_runs().len();h.set_project_root(None);
        assert!(kube.log_button("log-refresh",Some(&h.invocation())).is_err());assert!(kube.log_view.is_none());assert_eq!(h.process_runs().len(),before);
    }
    #[test]
    fn opening_reads_one_snapshot_and_the_buttons_filter_and_copy_it() {
        let (host, runs) = host(LOG);
        host.set_setting("logViewerLines", "50");
        let mut kube = KubeExplorer { namespace: Some("shop".into()), ..Default::default() };
        kube.open_log_view("pods", "web", Some(&host.invocation())).unwrap();
        assert_eq!(runs.borrow()[0], ["--context=dev", "--namespace=shop", "logs", "web", "--timestamps", "--prefix", "--all-containers", "--tail=50", REQUEST_TIMEOUT]);

        host.answer_next_prompt(Some(&["error"]));
        kube.log_button("log-filter", Some(&host.invocation())).unwrap();
        kube.log_button("log-copy", Some(&host.invocation())).unwrap();
        assert_eq!(host.clipboard().unwrap().lines().count(), 2);

        host.answer_next_prompt(None);
        kube.log_button("log-filter", Some(&host.invocation())).unwrap();
        assert_eq!(kube.log_view.as_ref().unwrap().filter, "error");

        kube.log_button("log-refresh", Some(&host.invocation())).unwrap();
        assert_eq!(runs.borrow().len(), 2);
        kube.button("back").unwrap();
        assert!(kube.log_view.is_none());
    }

    #[test]
    fn a_workload_reads_all_its_pods_and_copy_without_the_capability_only_notifies() {
        let (host, runs) = host(LOG);
        let mut kube = KubeExplorer { namespace: Some("shop".into()), ..Default::default() };
        kube.open_log_view("deployments", "web", Some(&host.invocation())).unwrap();
        assert!(runs.borrow()[0].contains(&"deployments/web".to_owned()) && runs.borrow()[0].contains(&"--all-pods=true".to_owned()));

        let denied = MockHost::install().with_capabilities(&["process:kubectl", "project.read", "storage"]);
        kube.log_button("log-copy", Some(&denied.invocation())).unwrap();
        assert_eq!(denied.notifications().len(), 1);
    }

    #[test]
    fn all_pods_flag_fallback() {
        let (host, runs) = host(LOG);
        host.on_process(KUBECTL, {
            let seen = runs.clone();
            move |args, _| {
                if args.iter().any(|a| a == "current-context") {
                    return Ok(ProcessOutput { status: Some(0), stdout: "dev".into(), stderr: String::new() });
                }
                if args.iter().any(|a| a == "view") {
                    return Ok(ProcessOutput { status: Some(0), stdout: r#"{"contexts":[{"name":"dev","context":{"cluster":"local","namespace":"shop"}}],"clusters":[{"name":"local","cluster":{"server":"https://dev.invalid"}}]}"#.into(), stderr: String::new() });
                }
                seen.borrow_mut().push(args.to_vec());
                if args.iter().any(|a| a == "--all-pods=true") {
                    return Ok(ProcessOutput { status: Some(1), stdout: String::new(), stderr: "error: unknown flag: --all-pods\n".into() });
                }
                Ok(ProcessOutput { status: Some(0), stdout: LOG.into(), stderr: String::new() })
            }
        });
        let mut kube = KubeExplorer { namespace: Some("shop".into()), ..Default::default() };
        kube.open_log_view("deployments", "web", Some(&host.invocation())).unwrap();
        assert_eq!(runs.borrow().len(), 2);
        assert!(!runs.borrow()[1].contains(&"--all-pods=true".to_owned()));
        assert_eq!(kube.log_view.as_ref().unwrap().lines.len(), 5);
    }

    #[test]
    fn other_kinds_and_bad_rows_run_nothing() {
        let (host, runs) = host(LOG);
        let mut kube = KubeExplorer::default();
        assert!(kube.open_log_view("services", "web", Some(&host.invocation())).is_err());
        kube.namespace = Some("*".into());
        assert!(kube.open_log_view("pods", "--bad", Some(&host.invocation())).is_err());
        assert!(runs.borrow().is_empty());
    }

    const PREFIXED: &str = "[pod/web/app] 2025-11-06T09:00:00Z one\n[pod/web/side] 2025-11-06T10:00:00Z two\n[pod/web/app] 2025-11-06T11:00:00Z three\n";

    fn prefixed() -> LogView {
        LogView { lines: PREFIXED.lines().map(str::to_owned).collect(), ..view("") }
    }

    #[test]
    fn until_bounds_the_range_and_the_fields_can_be_hidden() {
        let mut v = prefixed();
        v.since = "2025-11-06T09:30".into();
        v.until = "2025-11-06T10:30".into();
        assert_eq!(texts(&v), ["[pod/web/side] 2025-11-06T10:00:00Z two"]);
        v.show_time = false;
        assert_eq!(v.display(texts(&v)[0].as_str()), "[pod/web/side] two");
        v.show_source = false;
        assert_eq!(v.display(texts(&v)[0].as_str()), "two");
    }

    #[test]
    fn clear_scroll_and_field_buttons_change_the_view() {
        let (host, _) = host(PREFIXED);
        let mut kube = KubeExplorer::default();
        kube.open_log_view("pods", "web", Some(&host.invocation())).unwrap();
        kube.log_button("log-source", Some(&host.invocation())).unwrap();
        assert!(!kube.log_view.as_ref().unwrap().show_source);
        kube.log_button("log-end", Some(&host.invocation())).unwrap();
        let (physical,raw)=host.view_messages().last().unwrap().clone();
        assert_eq!(physical,crate::PANEL);
        let reply:serde_json::Value=serde_json::from_str(&raw).unwrap();
        assert_eq!(reply["targetView"],crate::PANEL);
        assert_eq!(reply["message"],"end");
        kube.log_button("log-clear", Some(&host.invocation())).unwrap();
        assert!(kube.log_view.as_ref().unwrap().lines.is_empty());
    }

    #[test]
    fn download_suggests_a_path_inside_the_project_and_never_in_tmp() {
        assert_eq!(suggested_path(Some("/work/app/"), "web.log").as_deref(), Some("/work/app/web.log"));
        assert_eq!(suggested_path(None, "web.log"), None);
    }

    #[test]
    fn download_writes_exact_shown_snapshot_through_project_api_without_tee() {
        let (host, _) = host(PREFIXED);
        let host = host.with_capabilities(&["process:kubectl", "project.read", "project.write", "storage"]);
        let mut kube = KubeExplorer::default();
        kube.open_log_view("pods", "web", Some(&host.invocation())).unwrap();
        kube.log_view.as_mut().unwrap().filter = "two".into();
        let before = host.process_runs().len();
        host.answer_next_prompt(Some(&["/project/web.log"]));
        host.answer_next_prompt(Some(&[]));
        kube.log_button("log-download", Some(&host.invocation())).unwrap();
        assert_eq!(host.invocation().project().unwrap().read_file("web.log").unwrap(), "[pod/web/side] 2025-11-06T10:00:00Z two\n");
        assert_eq!(host.process_runs().len(), before, "export must not invoke kubectl or tee");
    }
    #[test]
    fn log_export_cancel_preserves_disk_and_requires_project_write_only_at_commit() {
        let (host, _) = host(PREFIXED);
        let mut kube = KubeExplorer::default();
        kube.open_log_view("pods", "web", Some(&host.invocation())).unwrap();
        host.answer_next_prompt(Some(&["web.log"]));
        host.answer_next_prompt(None);
        kube.log_button("log-download", Some(&host.invocation())).unwrap();
        assert!(!host.invocation().project().unwrap().exists("web.log").unwrap());
        host.answer_next_prompt(Some(&["web.log"]));
        host.answer_next_prompt(Some(&[]));
        assert!(kube.log_button("log-download", Some(&host.invocation())).is_err());
        assert!(!host.invocation().project().unwrap().exists("web.log").unwrap());
    }
    #[test]
    fn log_export_refuses_outside_traversal_nested_and_existing_targets() {
        let (host, _) = host(PREFIXED);
        let host = host.with_capabilities(&["process:kubectl", "project.read", "project.write", "storage"]);
        host.add_project("/project/nested");
        host.add_file("keep.log", "keep");
        let mut kube = KubeExplorer::default();
        kube.open_log_view("pods", "web", Some(&host.invocation())).unwrap();
        let before = host.process_runs().len();
        for path in ["/tmp/web.log", "../web.log", "/project-sibling/web.log", "nested/web.log", "keep.log", "missing/web.log"] {
            host.answer_next_prompt(Some(&[path]));
            assert!(kube.log_button("log-download", Some(&host.invocation())).is_err(), "{path}");
        }
        assert_eq!(host.invocation().project().unwrap().read_file("keep.log").unwrap(), "keep");
        assert_eq!(host.process_runs().len(), before);
    }
    #[test]
    fn log_export_refuses_unsaved_new_target_and_invalid_filter() {
        let (host, _) = host(PREFIXED);
        let host = host.with_capabilities(&["process:kubectl", "project.read", "project.write", "editor.read", "storage"]);
        let mut kube = KubeExplorer::default();
        kube.open_log_view("pods", "web", Some(&host.invocation())).unwrap();
        host.open_editor("/project/dirty.log", "plaintext", "unwritten work", &[]);
        host.answer_next_prompt(Some(&["dirty.log"]));
        assert!(kube.log_button("log-download", Some(&host.invocation())).is_err());
        assert_eq!(host.invocation().editor().unwrap().document().unwrap().text, "unwritten work");
        let view = kube.log_view.as_mut().unwrap(); view.regex = true; view.filter = "[".into();
        assert!(kube.log_button("log-download", Some(&host.invocation())).is_err());
    }

    #[test]
    fn the_view_renders_as_html_with_the_filter_buttons() {
        let (host, _) = host(LOG);
        let mut kube = KubeExplorer::default();
        kube.open_log_view("pods", "web", Some(&host.invocation())).unwrap();
        let rendered = kube.render_tree().unwrap();
        assert!(matches!(rendered.body, Body::Html(_)));
        assert!(rendered.toolbar.iter().any(|b| b.id == kube.test_control_key("log-copy")));
    }
}
