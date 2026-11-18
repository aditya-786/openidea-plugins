//! Optional owned, paged snapshot. No filesystem offset/tail API is assumed.
use super::*;
use openidea_plugin::views::{Body, Button, Table};
use std::time::{Duration, Instant};
pub const VIEW: &str = "log-snapshot";
const MAX_BYTES: usize = 256 << 10;
const MAX_LINES: usize = 8192;
const MAX_EVENTS: usize = 2048;
const PAGE: u32 = 100;
struct Row {
    line: u32,
    level: Option<Severity>,
    cells: Vec<String>,
}
/// The level facet: section buttons, as the host has no pressed state for toolbar buttons.
const LEVELS: [(&str, Severity, &str); 5] = [
    ("error", Severity::Error, "Errors"),
    ("warning", Severity::Warning, "Warnings"),
    ("info", Severity::Info, "Info"),
    ("debug", Severity::Debug, "Debug"),
    ("trace", Severity::Trace, "Trace"),
];
pub(crate) fn level_filter(key: &str) -> Option<Severity> {
    LEVELS.iter().find(|(id, ..)| *id == key).map(|(_, level, _)| *level)
}
struct Snapshot {
    root: String,
    path: String,
    text: String,
    version: u64,
    format: String,
    rows: Vec<Row>,
    total: usize,
    at: Instant,
}
#[derive(Default)]
pub(crate) struct Viewer {
    snapshot: Option<Snapshot>,
    visible: std::ops::Range<u32>,
    page: u32,
    pub(crate) level: Option<Severity>,
}
/// The snapshot's rows of the chosen level; activation and paging count these.
fn shown(snapshot: &Snapshot, level: Option<Severity>) -> Vec<&Row> {
    snapshot.rows.iter().filter(|row| level.is_none_or(|level| row.level == Some(level))).collect()
}
fn owner(ctx: &Invocation, path: &str) -> Result<(String, Project), Error> {
    let project = ctx.project().ok_or(Error::Stale)?;
    let root = project.root()?;
    if !path.starts_with('/')
        || path.contains('\\')
        || path.split('/').any(|p| p == "." || p == "..")
        || spring_text_root(path).as_deref() != Some(root.as_str())
    {
        return Err(Error::Stale);
    }
    Ok((root, project))
}
fn spring_text_root(path: &str) -> Option<String> {
    host::project_of(path)?.root().ok()
}
fn cell(text: &str, limit: usize) -> String {
    text.chars().take(limit).map(|c| if c.is_control() && !matches!(c, '\t') { ' ' } else { c }).collect()
}
fn too_large(bytes: u64) -> Error {
    format!("This log is {} KiB; snapshots are limited to 256 KiB and 8192 lines. Use the log editor for larger files.", bytes.div_ceil(1024)).into()
}
fn build(doc: &Document, root: String, config: &Config, mut cancelled: impl FnMut() -> bool) -> Result<Snapshot, Error> {
    if doc.text.len() > MAX_BYTES || doc.text.lines().take(MAX_LINES + 1).count() > MAX_LINES {
        return Err(too_large(doc.text.len() as u64));
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut checkpoint = || -> Result<(), Error> {
        if cancelled() { return Err(Error::Cancelled); }
        if Instant::now() >= deadline { return Err("Snapshot parse budget exceeded; previous snapshot retained.".into()); }
        Ok(())
    };
    checkpoint()?;
    let format = config.detect_checked(&doc.text, &mut checkpoint)?;
    checkpoint()?;
    let events = log::events_checked(format, &doc.text, &mut checkpoint)?;
    let total = events.len();
    let mut rows = Vec::new();
    for event in events.into_iter().take(MAX_EVENTS) {
        checkpoint()?;
        let parsed = Parsed::new(format, event.text);
        let level = parsed.level(format);
        let severity = match level {
            Some(Severity::Error) => "Error",
            Some(Severity::Warning) => "Warning",
            Some(Severity::Info) => "Info",
            Some(Severity::Debug) => "Debug",
            Some(Severity::Trace) => "Trace",
            None => "Unclassified",
        };
        let text = format.filter(|f| !f.level_from_message)
            .and_then(|f| f.severity_column)
            .and_then(|column| parsed.values().get(column).copied())
            .map(|value| cell(value.trim(), 128)).filter(|value| !value.is_empty())
            .unwrap_or_else(|| severity.to_owned());
        rows.push(Row {
            line: event.lines.start,
            level,
            cells: vec![
                (event.lines.start + 1).to_string(),
                text,
                cell(parsed.time(format).unwrap_or(""), 128),
                cell(parsed.category(format).unwrap_or(""), 128),
                cell(parsed.message().lines().next().unwrap_or(""), 512),
            ],
        });
    }
    checkpoint()?;
    Ok(Snapshot {
        root,
        path: doc.path.clone(),
        text: doc.text.clone(),
        version: doc.version,
        format: format.map_or("Plain text", |f| f.name.as_str()).into(),
        rows,
        total,
        at: Instant::now(),
    })
}
impl Ideolog {
    pub(crate) fn capture_log(&mut self, ctx: &Invocation) -> Result<(), Error> {
        let editor = ctx.editor().ok_or("Focus a Log editor, then Capture Active Log Snapshot.")?;
        if !matches!(editor.language().as_str(), "log" | "ideolog") {
            return Err("Focus a Log editor before capturing.".into());
        }
        let path = editor.path();
        let (root, project) = owner(ctx, &path)?;
        let stat = project.stat(&path)?;
        if stat.kind != openidea_plugin::FileKind::File {
            return Err("Only log files saved in the project can be captured.".into());
        }
        if stat.size > MAX_BYTES as u64 {
            return Err(too_large(stat.size));
        }
        let task = host::begin_task("Capture bounded log snapshot", true);
        if task.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let buffer = editor.document()?;
        let doc = Document { path: path.clone(), language: editor.language(), text: buffer.text, version: buffer.version };
        let config = self.config();
        let snapshot = build(&doc, root.clone(), &config, || task.is_cancelled())?;
        let latest = editor.document()?;
        if task.is_cancelled() {
            return Err(Error::Cancelled);
        }
        if editor.path() != doc.path || latest.version != doc.version || latest.text != doc.text || owner(ctx, &path)?.0 != root {
            return Err(Error::Stale);
        }
        self.viewer.snapshot = Some(snapshot);
        self.viewer.visible = 0..0;
        self.viewer.page = 0;
        ui_kit::web::reveal_view(VIEW, include_str!("ui_config.json"))?;
        ui_kit::web::refresh_view(VIEW, include_str!("ui_config.json"));
        Ok(())
    }
    pub(crate) fn render_log(&mut self, viewport: Option<Viewport>, ctx: &Invocation) -> Result<View, Error> {
        let Some(snapshot) = &self.viewer.snapshot else {
            return Ok(View::message("No snapshot. Focus a Log editor and run Ideolog: Capture Active Log Snapshot. Capture is explicit; this view does not read or tail files.").button(Button::new("capture-log","Capture Active Log")));
        };
        if owner(ctx, &snapshot.path).ok().is_none_or(|(root, _)| root != snapshot.root) {
            return Ok(View::message(
                "This snapshot belongs to another project; return to its owning project or capture that project's log.",
            ));
        }
        let custom = ui_kit::web::is_rendering();
        let shown = shown(snapshot, self.viewer.level);
        let viewport = if custom {
            let last_page = (shown.len() as u32).saturating_sub(1) / PAGE;
            self.viewer.page = self.viewer.page.min(last_page);
            Some(Viewport { first_row: self.viewer.page * PAGE, rows: PAGE })
        } else { viewport.map(|v| Viewport { first_row: v.first_row, rows: v.rows.clamp(1, PAGE) }) };
        let range = Viewport::range(viewport, shown.len() as u32, 50);
        let rows = shown[range.start as usize..range.end as usize].iter().map(|r| r.cells.clone()).collect();
        let filtered = match self.viewer.level {
            Some(level) => format!("{} {} of ", shown.len(), LEVELS.iter().find(|(_, l, _)| *l == level).map_or("", |(_, _, label)| label).to_lowercase()),
            None => String::new(),
        };
        let mut view = View::new(Body::Table(Table{columns:vec!["Source line".into(),"Level".into(),"Timestamp".into(),"Category".into(),"Message (first line)".into()],total_rows:shown.len()as u32,first_row:range.start,rows,editable:false}));
        let mut sections = vec![("all", None, "All", snapshot.rows.len())];
        for (id, level, label) in LEVELS {
            let count = snapshot.rows.iter().filter(|row| row.level == Some(level)).count();
            if count > 0 || self.viewer.level == Some(level) {
                sections.push((id, Some(level), label, count));
            }
        }
        for (id, level, label, count) in sections {
            let marker = if level == self.viewer.level { "\u{25cf} " } else { "" };
            view = view.button(Button::new(&format!("section:{id}"), &format!("{marker}{label} ({count})")));
        }
        view = view.button(Button::new("capture-log","Capture Active Log")).button(Button::new("discard-log","Discard Snapshot"))
            .status(&format!("{} · version {} · {} bytes · {} · {filtered}{} of {} events · {}s old · snapshot only, no tail; messages limited to 512 characters; activate a row to open its log source",snapshot.path,snapshot.version,snapshot.text.len(),snapshot.format,snapshot.rows.len(),snapshot.total,snapshot.at.elapsed().as_secs()));
        if custom {
            view = view.button(Button::new("snapshot-previous", "Previous Events").icon("arrow-left").enabled(range.start > 0))
                .button(Button::new("snapshot-next", "Next Events").icon("arrow-right").enabled(range.end < shown.len() as u32));
            view.status = Some(format!("{} · Events {}–{} of {}", view.status.as_deref().unwrap_or_default(), if range.is_empty() {0} else {range.start + 1}, range.end, shown.len()));
        }
        self.viewer.visible = range;
        Ok(view)
    }
    pub(crate) fn handle_log(&mut self, event: ViewEvent, ctx: &Invocation) -> Result<(), Error> {
        if let ViewEvent::Button(ref key) = event {
            match key.as_str() {
                "capture-log" => return self.capture_log(ctx),
                section if section.starts_with("section:") => {
                    let snapshot = self.viewer.snapshot.as_ref().ok_or(Error::Stale)?;
                    if owner(ctx, &snapshot.path)?.0 != snapshot.root { return Err(Error::Stale); }
                    self.viewer.level = level_filter(&section["section:".len()..]);
                    self.viewer.page = 0;
                    self.viewer.visible = 0..0;
                    ui_kit::web::refresh_view(VIEW, include_str!("ui_config.json"));
                    return Ok(());
                }
                "snapshot-previous" | "snapshot-next" => {
                    let snapshot = self.viewer.snapshot.as_ref().ok_or(Error::Stale)?;
                    if owner(ctx, &snapshot.path)?.0 != snapshot.root { return Err(Error::Stale); }
                    let last_page = (shown(snapshot, self.viewer.level).len() as u32).saturating_sub(1) / PAGE;
                    self.viewer.page = if key == "snapshot-next" { self.viewer.page.saturating_add(1).min(last_page) }
                        else { self.viewer.page.saturating_sub(1) };
                    self.viewer.visible = 0..0;
                    ui_kit::web::refresh_view(VIEW, include_str!("ui_config.json"));
                    return Ok(());
                }
                "discard-log" => {
                    if let Some(s) = &self.viewer.snapshot {
                        if owner(ctx, &s.path)?.0 != s.root {
                            return Err(Error::Stale);
                        }
                    }
                    self.viewer = Viewer::default();
                    ui_kit::web::refresh_view(VIEW, include_str!("ui_config.json"));
                    return Ok(());
                }
                _ => {}
            }
        }
        if let Some(row) = event.table_activate() {
            let snapshot = self.viewer.snapshot.as_ref().ok_or(Error::Stale)?;
            if !self.viewer.visible.contains(&row) {
                return Err(Error::Stale);
            }
            let (_, project) = owner(ctx, &snapshot.path)?;
            let current = project.stat(&snapshot.path)?;
            if current.kind != openidea_plugin::FileKind::File
                || current.size > MAX_BYTES as u64
                || project.read_file(&snapshot.path)? != snapshot.text
            {
                return Err("Snapshot source changed; capture again before following a row.".into());
            }
            let line = shown(snapshot, self.viewer.level).get(row as usize).ok_or(Error::Stale)?.line;
            if owner(ctx, &snapshot.path)?.0 != snapshot.root {
                return Err(Error::Stale);
            }
            host::open_location(&snapshot.path, line, 0)?;
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    fn setup(text: &str) -> (MockHost, Ideolog) {
        let host = MockHost::install().with_capabilities(&["project.read", "editor.read", "storage"]);
        host.add_file("app.log", text);
        host.open_editor("/project/app.log", "log", text, &[]);
        (host, Ideolog::default())
    }
    fn activate(row: u32) -> ViewEvent {
        ViewEvent::Other { kind: "table-activate".into(), data: format!("{{\"row\":{row}}}") }
    }
    #[test]
    fn custom_group_snapshot_paging_preserves_absolute_row_navigation_and_hidden_tab_guards() {
        use serde_json::{json,Value};
        fn request(plugin:&mut Ideolog,host:&MockHost,mut value:Value)->Value {
            value["channel"]=json!("ui-kit");value["sequence"]=json!(1);value["project"]=json!("/project");
            plugin.handle("patterns",ViewEvent::Message(value.to_string()),&host.invocation()).unwrap();
            serde_json::from_str(&host.view_messages().last().unwrap().1).unwrap()
        }
        let text="entry\n".repeat(250);
        let (host,mut plugin)=setup(&text);plugin.capture_log(&host.invocation()).unwrap();
        assert_eq!(host.revealed_views(),["patterns"]);
        let first=request(&mut plugin,&host,json!({"op":"render"}));
        assert_eq!(first["model"]["activeView"],VIEW);assert_eq!(first["model"]["body"]["firstRow"],0);
        let next=request(&mut plugin,&host,json!({"op":"event","targetView":VIEW,"event":{"kind":"button","key":"snapshot-next"}}));
        assert_eq!(next["model"]["body"]["firstRow"],100);
        let opened=request(&mut plugin,&host,json!({"op":"event","targetView":VIEW,"event":{"kind":"table-activate","row":100}}));
        assert!(opened.get("error").is_none());assert_eq!(host.opened_locations(),[("/project/app.log".into(),100,0)]);
        let old=request(&mut plugin,&host,json!({"op":"event","targetView":VIEW,"event":{"kind":"table-activate","row":0}}));
        assert!(old.get("error").is_some());
        let last=request(&mut plugin,&host,json!({"op":"event","targetView":VIEW,"event":{"kind":"button","key":"snapshot-next"}}));
        assert_eq!(last["model"]["body"]["firstRow"],200);assert_eq!(last["model"]["body"]["rows"].as_array().unwrap().len(),50);
        request(&mut plugin,&host,json!({"op":"select-view","targetView":VIEW,"target":"patterns"}));
        let stale=request(&mut plugin,&host,json!({"op":"event","targetView":VIEW,"event":{"kind":"button","key":"discard-log"}}));
        assert!(stale.get("error").is_some());assert!(plugin.viewer.snapshot.is_some());
        let restored=request(&mut plugin,&host,json!({"op":"select-view","targetView":"patterns","target":VIEW}));
        assert_eq!(restored["model"]["body"]["firstRow"],200);
        assert!(host.process_runs().is_empty()&&host.requests().is_empty());
    }

    #[test]
    fn snapshot_retains_info_debug_and_custom_level_text() {
        let text = "2026-11-09|INFO|main|one\n2026-11-09|DEBUG|main|two\n2026-11-09|CUSTOM|main|three\n";
        let (host, mut plugin) = setup(text);
        plugin.capture_log(&host.invocation()).unwrap();
        let snapshot = plugin.viewer.snapshot.as_ref().unwrap();
        assert_eq!(snapshot.rows.iter().map(|row| row.cells[1].as_str()).collect::<Vec<_>>(), ["INFO", "DEBUG", "CUSTOM"]);
    }
    #[test]
    fn explicit_capture_reuses_parser_pages_and_links_multiline_events_to_exact_log_line() {
        let text = "2026-11-08|ERROR|main|😀 boom\r\n    at A.java:12\r\n2026-11-09|WARN|main|later\r\n";
        let (host, mut plugin) = setup(text);
        let ctx = host.invocation();
        assert!(matches!(plugin.render_log(None, &ctx).unwrap().body, Body::Message(_)));
        assert!(host.tasks().is_empty());
        plugin.capture_log(&ctx).unwrap();
        let view = plugin.render_log(Some(Viewport { first_row: 1, rows: 1 }), &ctx).unwrap();
        let Body::Table(table) = view.body else { panic!("table") };
        assert_eq!(table.first_row, 1);
        assert_eq!(table.total_rows, 2);
        assert_eq!(table.rows.len(), 1);
        assert!(table.rows[0][4].contains("later"));
        assert!(plugin.handle_log(activate(0), &ctx).is_err(), "off-page requests rejected");
        plugin.handle_log(activate(1), &ctx).unwrap();
        assert_eq!(host.opened_locations(), [("/project/app.log".into(), 2, 0)]);
        plugin.render_log(None, &ctx).unwrap();
        let snapshot = plugin.viewer.snapshot.as_ref().unwrap();
        assert!(snapshot.rows[0].cells[4].contains("😀"));
        assert_eq!(snapshot.rows[0].line, 0);
    }
    #[test]
    fn scope_changed_deleted_or_dirty_source_never_opens_stale_link() {
        for change in ["project", "source", "deleted"] {
            let (host, mut plugin) = setup("a\nb\n");
            let ctx = host.invocation();
            plugin.capture_log(&ctx).unwrap();
            plugin.render_log(None, &ctx).unwrap();
            match change {
                "project" => host.set_project_root(Some("/other")),
                "source" => {
                    host.add_file("app.log", "new source");
                    host.open_editor("/project/app.log", "log", "new source", &[]);
                }
                _ => plugin.viewer.snapshot.as_mut().unwrap().path = "/project/deleted.log".into(),
            };
            assert!(plugin.handle_log(activate(0), &host.invocation()).is_err(), "{change}");
            assert!(host.opened_locations().is_empty());
        }
    }
    #[test]
    fn snapshot_checks_cancellation_between_physical_continuation_lines() {
        let text=format!("first event\n{}","    continuation 😀\r\n".repeat(299));
        let doc=Document {path:"/project/app.log".into(),language:"log".into(),version:1,text};
        let mut checks=0;
        let snapshot=build(&doc,"/project".into(),&Config::default(),||{ checks+=1; false }).unwrap();
        assert_eq!(snapshot.rows.len(),1);
        assert!(checks>=300,"physical lines need cancellation checkpoints, got {checks}");
    }
    #[test]
    fn checked_grouping_and_detection_abort_without_partial_snapshot() {
        let text=format!("first event\n{}","    continuation\r\n".repeat(299));
        let mut checks=0;
        let grouped=log::events_checked(None,&text,|| { checks+=1; Ok::<(),Error>(()) }).unwrap();
        assert_eq!(checks,301); assert_eq!(grouped.len(),1); assert_eq!(grouped[0].lines,0..300);
        let mut checks=0;
        let stopped=log::events_checked(None,&text,|| { checks+=1; if checks==100 {Err(Error::Cancelled)} else {Ok(())} });
        assert!(matches!(stopped,Err(Error::Cancelled))); assert_eq!(checks,100);
        let (host,mut plugin)=setup("kept"); plugin.capture_log(&host.invocation()).unwrap();
        let doc=Document {path:"/project/app.log".into(),language:"log".into(),version:2,text};
        let mut checks=0;
        let stopped=build(&doc,"/project".into(),&Config::default(),|| { checks+=1; checks==100 });
        assert!(matches!(stopped,Err(Error::Cancelled))); assert_eq!(checks,100);
        assert_eq!(plugin.viewer.snapshot.as_ref().unwrap().text,"kept");
    }
    #[test]
    fn cancelling_bounded_parser_retains_prior_snapshot_and_no_timer() {
        let (host, mut plugin) = setup("one\ntwo");
        plugin.capture_log(&host.invocation()).unwrap();
        let before = plugin.viewer.snapshot.as_ref().unwrap().text.clone();
        let doc = Document { path: "/project/app.log".into(), language: "log".into(), version: 2, text: "next".into() };
        let mut calls = 0;
        assert!(matches!(
            build(&doc, "/project".into(), &Config::default(), || {
                calls += 1;
                calls > 1
            }),
            Err(Error::Cancelled)
        ));
        assert_eq!(plugin.viewer.snapshot.as_ref().unwrap().text, before);
        assert!(host.requests().is_empty());
    }
    #[test]
    fn large_source_rows_display_limits_and_paging_cap_are_explicit() {
        let (_host, mut plugin) = setup(&"a\n".repeat(3000));
        plugin.capture_log(&_host.invocation()).unwrap();
        let snapshot = plugin.viewer.snapshot.as_ref().unwrap();
        assert_eq!(snapshot.rows.len(), MAX_EVENTS);
        assert_eq!(snapshot.total, 3000);
        let view = plugin.render_log(Some(Viewport { first_row: 0, rows: u32::MAX }), &_host.invocation()).unwrap();
        let Body::Table(table) = view.body else { panic!("table") };
        assert_eq!(table.rows.len(), PAGE as usize);
        let doc = Document { path: "/project/app.log".into(), language: "log".into(), version: 1, text: "x".repeat(MAX_BYTES + 1) };
        let Err(Error::Failed(message)) = build(&doc, "/project".into(), &Config::default(), || false) else { panic!("oversized snapshot") };
        assert!(message.starts_with("This log is 257 KiB; snapshots are limited to 256 KiB"), "{message}");
        let doc = Document { text: "x\n".repeat(MAX_LINES + 1), ..doc };
        assert!(build(&doc, "/project".into(), &Config::default(), || false).is_err());
    }
    #[test]
    fn capture_failures_keep_last_good_and_discard_requires_owning_project() {
        let (host, mut p) = setup("kept");
        p.capture_log(&host.invocation()).unwrap();
        host.add_file("app.log", &"x".repeat(MAX_BYTES + 1));
        assert!(p.capture_log(&host.invocation()).is_err());
        assert_eq!(p.viewer.snapshot.as_ref().unwrap().text, "kept");
        host.set_project_root(Some("/other"));
        assert!(p.handle_log(ViewEvent::Button("discard-log".into()), &host.invocation()).is_err());
        assert!(matches!(p.render_log(None, &host.invocation()).unwrap().body, Body::Message(_)));
        host.set_project_root(Some("/project"));
        p.handle_log(ViewEvent::Button("discard-log".into()), &host.invocation()).unwrap();
        assert!(p.viewer.snapshot.is_none());
    }
}
