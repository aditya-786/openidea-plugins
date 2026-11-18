//! Dependency Audit for OpenIDEA: checks dependencies against OSV.dev.

mod manifest;
mod pyproject;
mod workspaces;
mod npm_graph;
mod build_resolution;
mod call_analysis;
mod imports;
mod sbt_resolution;
mod live;
mod commit;
mod osv;
mod resources;
mod actions;
mod editor_actions;
mod scoped;
mod binding;
mod workspace_chain;

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use openidea_plugin::host;
use openidea_plugin::diagnostics::DiagnosticsProvider;
use openidea_plugin::providers::{CodeAction, Document, Providers};
use openidea_plugin::views::{Body, Button, TreeItem, View, ViewEvent, Viewport, Views};
use openidea_plugin::{
    Diagnostic, Error, Event, Invocation, LogLevel, NotificationLevel, Plugin, Project, Range, Severity, TextEdit, register_plugin,
};
use resources::{evidence, report_status, resource_body};
use serde::{Deserialize, Serialize};
use spring_text::index::project_of;
use spring_text::range;
use spring_text::tree::Expansion;

use manifest::{Dep, GLOBS, Kind, Note, bumped, kind, lockfile, parse};
use osv::{Finding, audit, cached_advisory, compare, ignored_advisories, is_ignored, level};

type Display = HashMap<String, Option<(String, Option<String>)>>;

const REPORT_VIEW: &str = "vulnerabilities";
/// No report/model payload crosses frames; each page re-renders its own Invocation.
fn refresh_report() {
    host::refresh_view(REPORT_VIEW);
    let _ = host::post_view_message(REPORT_VIEW, r#"{"channel":"ui-kit-refresh"}"#);
}
const UI_CONFIG: &str = include_str!("ui_config.json");
pub(crate) fn reveal(view: &str) -> Result<(), Error> {
    ui_kit::web::reveal_view(view, UI_CONFIG)
}
pub(crate) fn refresh(view: &str) {
    ui_kit::web::refresh_view(view, UI_CONFIG);
}
fn owned_source_path(root: &str, path: &str) -> bool {
    !root.is_empty() && path.starts_with(&format!("{}/",root.trim_end_matches('/')))
        && !path.split('/').any(|part| matches!(part,"."|".."))
        && project_of(path).and_then(|p|p.root().ok()).as_deref()==Some(root)
}
const MAX_REPORT_OWNER_PATHS: usize = 512;
fn validate_report_sources(report: &Report, mut owned: impl FnMut(&str)->bool) -> Result<(),Error> {
    let deadline=Instant::now()+Duration::from_millis(50);
    let mut paths=HashSet::new();
    for row in &report.rows {
        for path in std::iter::once(row.path.as_str()).chain(row.sources.iter().map(|s|s.path.as_str())) {
            if paths.insert(path) && paths.len()>MAX_REPORT_OWNER_PATHS { return Err(Error::Invalid("Saved report exceeds 512 unique evidence paths; owner proof unavailable.".into())); }
        }
        if Instant::now()>=deadline { return Err(Error::Stale); }
    }
    for path in paths {
        if Instant::now()>=deadline || !owned(path) { return Err(Error::Stale); }
    }
    Ok(())
}
fn owned_lock(project: &Project, path: &str) -> Option<String> {
    let root=project.root().ok()?;
    let lock=lockfile(path)?;
    owned_source_path(&root,&lock).then(||project.read_file(&lock).ok()).flatten()
}


#[derive(Default)]
pub struct DependencyAudit {
    expansion: Expansion,
    display_owner: Option<String>,
    npm_graph: Option<npm_graph::Snapshot>,
    npm_error: Option<String>,
    build_resolution:build_resolution::State,
    call_analysis:call_analysis::State,
    sbt_resolution:sbt_resolution::State,
    targets: HashMap<String, Target>,
    display: Display,
    live: live::Live,
    auto_resolve_reported: HashSet<String>,
}

enum Target {
    Location(String, u32),
    Url(String),
}

#[derive(Serialize, Deserialize)]
struct Row {
    path: String,
    line: u32,
    name: String,
    version: String,
    fix: Option<String>,
    advisories: Vec<String>,
    #[serde(default)]
    ecosystem: Option<String>,
    #[serde(default)]
    sources: Vec<Declaration>,
    #[serde(default)]
    details: Vec<osv::Advisory>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Declaration {
    path: String,
    line: u32,
    evidence: String,
}

const MAX_COORDINATE_ROWS: usize = 500;

#[derive(Default, Serialize, Deserialize)]
struct Report {
    rows: Vec<Row>,
    #[serde(default)]
    scanned_at: Option<u64>,
    #[serde(default)]
    files_scanned: Option<usize>,
    #[serde(default)]
    files_skipped: usize,
    #[serde(default)]
    dependencies_checked: Option<usize>,
    #[serde(default)]
    checked_files: Vec<String>,
    #[serde(default)]
    discovery_limited: bool,
    #[serde(default)]
    coordinates: Vec<[String; 3]>,
    #[serde(default)]
    skipped_paths: Vec<String>,
    /// `[label, name, ecosystem, spec, path]` for local workspace packages and marker-guarded requirements.
    #[serde(default)]
    notes: Vec<[String; 5]>,
}

#[derive(Serialize, Deserialize)]
struct ScanFailure {
    at: u64,
    message: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum StoredReport {
    Current(Report),
    Legacy(Vec<Row>),
}

const CALL_BUDGET: Duration = Duration::from_secs(45);
const COMMAND_BUDGET: Duration = Duration::from_secs(55);
const EVENT_BUDGET: Duration = Duration::from_secs(10);
const EVENT_COORDINATES: usize = 1000;
const MIN_RESOLVE_TIME: Duration = Duration::from_secs(10);

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or_default()
}

fn read_deps(project: &Project, path: &str) -> Result<(String, Vec<Dep>), Error> {
    if !owned_source_path(&project.root()?,path) { return Err(Error::Stale); }
    let text = project.read_file(path)?;
    let lock = owned_lock(project,path);
    let deps = parse(path, &text, lock.as_deref());

    Ok((text, deps))
}

fn diagnostic(finding: &Finding, text: &str) -> Diagnostic {
    Diagnostic {
        range: range(text, finding.dep.at.0, finding.dep.at.1),
        severity: finding.severity(),
        message: finding.message(),
        code: finding.advisories.first().map(|a| a.id.clone()),
        source: Some("OSV".into()),
        unnecessary: false,
        deprecated: false,
        related: Vec::new(),
    }
}

fn check(path: &str, deadline: Instant, uncached: &mut usize, live: &mut live::Live) -> Result<(), Error> {
    let project = project_of(path).ok_or_else(|| Error::NotFound(format!("project of {path}")))?;
    let (text, deps) = read_deps(&project, path)?;
    let _task = host::begin_task("Checking dependencies with OSV.dev", false);
    osv::audit_within(&deps, true, now(), deadline, uncached)?;
    live.remember(&project.root()?, path, &text, &deps);
    Ok(())
}

fn rows_key(project: &Project) -> Result<String, Error> {
    let root=project.root()?;
    if root.is_empty() { return Err(Error::Stale); }
    Ok(format!("scan:{root}"))
}

const MAX_REPORT_BYTES: usize = 8 << 20;

/// The reader and writer share one bound. Never replace last-good evidence
/// with a serialized report that this version cannot read back.
fn persist_report(project: &Project, report: &Report) -> Result<(), Error> {
    let stored = serde_json::to_string(report).map_err(|e| Error::Failed(e.to_string()))?;
    if stored.len() > MAX_REPORT_BYTES {
        return Err(Error::Invalid("The new report exceeds the 8 MiB saved-report limit; previous report retained.".into()));
    }
    host::storage::set(&rows_key(project)?, &stored)?;
    present_findings(project);
    Ok(())
}

/// One project's tree-note and editor decoration set: clearing a set reaches every project.
fn findings_set(root: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    root.hash(&mut hasher);
    format!("osv-{:016x}", hasher.finish())
}

/// Replaces this project's project-tree notes ("2 vulnerabilities (1 high or critical)") on files
/// with findings and the inline advisory text on their declaration lines. Reads the stored
/// report, so ignores apply; nothing is shown without one.
fn present_findings(project: &Project) {
    let Ok(root) = project.root() else { return };
    let set = findings_set(&root);
    let _ = host::clear_file_decorations(&set);
    let _ = host::clear_decorations(&set);
    let Ok(Some(report)) = stored_report(project) else { return };
    let mut counts: std::collections::BTreeMap<&str, (usize, usize)> = std::collections::BTreeMap::new();
    let mut lines: std::collections::BTreeMap<&str, std::collections::BTreeMap<u32, (Vec<String>, Vec<String>)>> = std::collections::BTreeMap::new();
    for row in &report.rows {
        let details: Vec<Option<osv::Advisory>> =
            row.advisories.iter().map(|id| row.details.iter().find(|a| a.id == *id).cloned().or_else(|| cached_advisory(id))).collect();
        let high = details.iter().filter(|a| level(a.as_ref().and_then(|a| a.severity.as_deref())) == Severity::Error).count();
        let count = counts.entry(&row.path).or_default();
        count.0 += row.advisories.len();
        count.1 += high;
        let Some(first) = row.advisories.first() else { continue };
        let shown = details[0].as_ref().and_then(|a| a.aliases.iter().find(|alias| alias.starts_with("CVE-"))).unwrap_or(first);
        let more = if row.advisories.len() > 1 { format!(" +{}", row.advisories.len() - 1) } else { String::new() };
        let fix = row.fix.as_ref().map_or_else(|| "no fix".to_owned(), |fix| format!("fixed in {fix}"));
        let text = format!("{shown}{more} · {fix}");
        let hover = format!("{} {}: {}", row.name, row.version, row.advisories.join(", "));
        // Exact spans only: manifest declarations from the scan and declarations bound by name.
        let declared = row.sources.iter().filter(|s| {
            s.evidence.contains(binding::DECLARED) || s.evidence.starts_with("Manifest declaration") && !s.evidence.contains("not bound")
        });
        for source in declared {
            // A transitive finding sits on its direct dependency's line: name the vulnerable package.
            let text = if source.evidence.contains("transitive") || source.evidence.contains("workspace") {
                format!("{} {}: {text}", row.name, row.version)
            } else {
                text.clone()
            };
            let (texts, hovers) = lines.entry(&source.path).or_default().entry(source.line).or_default();
            if !texts.contains(&text) {
                texts.push(text);
                hovers.push(hover.clone());
            }
        }
    }
    for (path, (total, high)) in counts.iter().take(500) {
        let noun = if *total == 1 { "vulnerability" } else { "vulnerabilities" };
        let note = if *high > 0 { format!("{total} {noun} ({high} high or critical)") } else { format!("{total} {noun}") };
        if host::set_file_note(path, &set, &note, Some("From the last Dependency Audit scan")).is_err() {
            break;
        }
    }
    for (path, by_line) in lines.iter().take(200) {
        let Ok(text) = project.read_file(path) else { continue };
        let source: Vec<&str> = text.lines().collect();
        let items: Vec<_> = by_line
            .iter()
            .take(1000)
            .filter_map(|(line, (texts, hovers))| {
                let end = openidea_plugin::Position::new(*line, source.get(*line as usize)?.encode_utf16().count() as u32);
                Some(openidea_plugin::Decoration {
                    range: Range::new(end, end),
                    style: "warning".into(),
                    hover: Some(hovers.join("\n")),
                    after: Some(format!("  {}", texts.join("; "))),
                })
            })
            .collect();
        let _ = host::set_decorations(path, &set, &items);
    }
}

fn stored_report(project: &Project) -> Result<Option<Report>, Error> {
    let root=project.root()?;
    if root.is_empty() { return Err(Error::Stale); }
    let stored = host::storage::get(&rows_key(project)?)?;
    let report = stored.filter(|s| s.len() <= MAX_REPORT_BYTES).and_then(|json| serde_json::from_str::<StoredReport>(&json).ok());
    let ignored = ignored_advisories();
    let scopes = scoped::load(&project.root()?);
    report.map(|report| {
        let mut report = match report {
            StoredReport::Current(report) => report,
            StoredReport::Legacy(rows) => Report { rows, ..Report::default() },
        };
        validate_report_sources(&report,|path|owned_source_path(&root,path))?;
        report.rows = report
            .rows
            .into_iter()
            .filter_map(|mut row| {
                let aliases: HashMap<_, _> = row.advisories.iter().map(|id| (id.clone(), scoped::aliases(&row,id))).collect();
                let count = row.advisories.len();
                let eco = resources::ecosystem(&row).to_owned();
                row.advisories.retain(|id| {
                    !scoped::matches(&scopes,&eco,&row.name,&row.version,id,aliases.get(id).map(Vec::as_slice).unwrap_or_default())
                    && !is_ignored(
                        &ignored,
                        id,
                        aliases.get(id).map(Vec::as_slice).unwrap_or_default(),
                    )
                });
                if count != row.advisories.len() {
                    let key = format!("{}:{}",eco,if eco=="PyPI" {manifest::normalise_pypi(&row.name)} else {row.name.clone()});
                    row.fix = row.advisories.iter().filter_map(|id| row.details.iter().find(|a|a.id==*id).cloned().or_else(||cached_advisory(id)))
                        .filter_map(|a|osv::nearest_fix(&row.version,a.fixed.get(&key)?)).max_by(|a,b|compare(a,b));
                }
                if row.advisories.iter().any(|id| row.details.iter().find(|a|a.id==*id).cloned()
                    .unwrap_or_else(||osv::Advisory{id:id.clone(),..Default::default()}).known_malicious()) { row.fix=None; }
                (!row.advisories.is_empty()).then_some(row)
            })
            .collect();
        Ok(report)
    }).transpose()
}

fn stored_rows(project: &Project) -> Result<Option<Vec<Row>>, Error> {
    Ok(stored_report(project)?.map(|r| r.rows))
}

fn failure_key(project: &Project) -> Result<String, Error> {
    Ok(format!("{}:failure", rows_key(project)?))
}
fn last_failure(project: &Project) -> Option<ScanFailure> {
    host::storage::get(&failure_key(project).ok()?).ok().flatten().and_then(|json| serde_json::from_str(&json).ok())
}

fn scan(project: &Project, live: &mut live::Live, deadline: Instant) -> Result<String, Error> {
    let result = scan_report(project, live, deadline);
    if let Ok(key) = failure_key(project) {
        match &result {
            Ok((_, true)) => {
                let _ = host::storage::delete(&key);
            }
            Ok((_, false)) => {
                let failure =
                    ScanFailure { at: now(), message: "The new scan result could not be stored; previous report retained.".into() };
                let _ = host::storage::set(&key, &serde_json::to_string(&failure).unwrap_or_default());
            }
            Err(error) => {
                let failure = ScanFailure { at: now(), message: error.to_string() };
                let _ = host::storage::set(&key, &serde_json::to_string(&failure).unwrap_or_default());
            }
        }
    }
    refresh_report();
    result.map(|(body, _)| body)
}

fn scan_report(project: &Project, live: &mut live::Live, deadline: Instant) -> Result<(String, bool), Error> {
    live.clear();
    let root = project.root()?;
    let task = host::begin_task("Scanning dependencies", true);
    let mut files: Vec<String> = Vec::new();
    let mut discovery_limited = false;
    for glob in GLOBS {
        let found = project.find_files(glob, 200)?;
        discovery_limited |= found.len() >= 200;
        for path in found {
            if kind(&path).is_some() && !files.contains(&path) {
                files.push(path);
            }
        }
    }

    let mut texts: HashMap<String, String> = HashMap::new();
    let mut file_deps: Vec<(String, Vec<Dep>)> = Vec::new();
    let mut deps: Vec<Dep> = Vec::new();
    let mut seen = HashSet::new();
    let mut skipped = 0;
    let mut skipped_paths: Vec<String> = Vec::new();
    for path in &files {
        if task.is_cancelled() {
            return Err(Error::Cancelled);
        }

        let (text, found) = match read_deps(project, path) {
            Ok(read) => read,
            Err(e) => {
                host::log(LogLevel::Warn, &format!("Scan skipped {path}: {e}"));
                skipped += 1;
                if skipped_paths.len() < MAX_COORDINATE_ROWS {
                    skipped_paths.push(path.clone());
                }
                continue;
            }
        };
        live.remember(&root, path, &text, &found);
        for dep in &found {
            if seen.insert(dep_key(dep)) {
                deps.push(dep.clone());
            }
        }
        texts.insert(path.clone(), text);
        file_deps.push((path.clone(), found));
    }

    let package_jsons: HashMap<String, String> =
        texts.iter().filter(|(p, _)| kind(p) == Some(Kind::PackageJson)).map(|(p, t)| (p.clone(), t.clone())).collect();
    let mut pnpm_workspaces: HashMap<String, String> = HashMap::new();
    for path in package_jsons.keys() {
        let dir = &path[..path.rfind('/').map_or(0, |i| i + 1)];
        if !pnpm_workspaces.contains_key(dir)
            && let Ok(text) = project.read_file(&format!("{dir}pnpm-workspace.yaml"))
        {
            pnpm_workspaces.insert(dir.to_owned(), text);
        }
    }
    let members = workspaces::members(&package_jsons, &pnpm_workspaces);
    let mut notes: Vec<Note> = Vec::new();
    for (path, found) in &mut file_deps {
        let local: HashSet<String> = members
            .iter()
            .filter(|(root, _)| path.starts_with(root.as_str()))
            .flat_map(|(_, names)| names.iter().cloned())
            .collect();
        let file_notes = manifest::notes(path, &texts[path], &local);
        found.retain(|dep| !file_notes.iter().any(|n| n.label == manifest::LOCAL && n.name == dep.name && dep.ecosystem == n.ecosystem));
        notes.extend(file_notes);
    }
    deps.clear();
    seen.clear();
    for dep in file_deps.iter().flat_map(|(_, found)| found) {
        if seen.insert(dep_key(dep)) {
            deps.push(dep.clone());
        }
    }

    if !files.is_empty() && texts.is_empty() {
        return Err(Error::Failed("No discovered dependency file could be read; previous report retained.".into()));
    }
    let findings = audit(&deps, true, now(), deadline)?;

    let mut rows: Vec<Row> = findings
        .iter()
        .map(|f| Row {
            path: f.dep.path.clone(),
            line: texts.get(&f.dep.path).map(|text| range(text, f.dep.at.0, f.dep.at.1).start.line).unwrap_or_default(),
            name: f.dep.name.clone(),
            version: f.dep.version.clone(),
            fix: f.fix(),
            advisories: f.advisories.iter().map(|a| a.id.clone()).collect(),
            ecosystem: Some(f.dep.ecosystem.osv().into()),
            sources: file_deps
                .iter()
                .flat_map(|(path, deps)| {
                    deps.iter().filter(|dep| dep_key(dep) == dep_key(&f.dep)).map(|dep| Declaration {
                        path: path.clone(),
                        line: range(&texts[path], dep.at.0, dep.at.1).start.line,
                        evidence: evidence(path).into(),
                    })
                })
                .collect(),
            details: f.advisories.clone(),
        })
        .collect();
    binding::annotate(&mut rows, &texts);
    imports::scan(project, &mut rows, deadline);
    let report = Report {
        rows,
        scanned_at: Some(now()),
        files_scanned: Some(texts.len()),
        files_skipped: skipped,
        dependencies_checked: Some(deps.len()),
        checked_files: texts.keys().cloned().collect(),
        discovery_limited,
        coordinates: deps
            .iter()
            .take(MAX_COORDINATE_ROWS)
            .map(|d| [d.ecosystem.osv().to_owned(), d.ecosystem.osv_name(&d.name), d.version.clone()])
            .collect(),
        skipped_paths,
        notes: notes
            .iter()
            .take(MAX_COORDINATE_ROWS)
            .map(|n| [n.label.to_owned(), n.name.clone(), n.ecosystem.osv().to_owned(), n.spec.clone(), n.path.clone()])
            .collect(),
    };
    let visible = scoped::visible_count(&root,&report);
    let kept = persist_report(project, &report);
    if let Err(e) = &kept {
        // A failed replacement must leave the last successfully stored report intact.
        host::log(LogLevel::Warn, &format!("Scan results not stored: {e}"));
    }
    let mut body = match visible == 0 {
        true if files.is_empty() => no_dependency_files(),
        true => format!("No known vulnerabilities in {} dependencies ({} files).", deps.len(), texts.len()),
        false => format!(
            "Vulnerable dependencies: {} of {} ({} files). See the Dependency Audit window.",
            visible,
            deps.len(),
            texts.len()
        ),
    };
    let scoped_count = scoped::load(&root).len();
    if scoped_count > 0 { body.push_str(&format!(" ({scoped_count} ignored findings in this project are hidden.)")); }
    if kept.is_err() {
        body.push_str(" (New results could not be stored; any previous report remains in the Dependency Audit window.)");
    }

    if discovery_limited {
        body.push_str(" (Partial report: dependency-file discovery reached its per-glob limit.)");
    }
    if skipped > 0 {
        body.push_str(&format!(" (Partial report: {skipped} dependency files could not be read.)"));
    }
    Ok((body, kept.is_ok()))
}

fn no_dependency_files() -> String {
    let names: Vec<&str> = GLOBS.iter().map(|glob| glob.trim_start_matches("**/")).collect();
    format!("No supported dependency files found. Supported: {}.", names.join(", "))
}

fn dep_key(dep: &Dep) -> (manifest::Ecosystem, String, String) {
    (dep.ecosystem, dep.name.clone(), dep.version.clone())
}

fn tree_of(root: &str, rows: &[Row], mut display: Option<&mut Display>) -> (Vec<TreeItem>, HashMap<String, Target>) {
    let root = format!("{root}/");
    let mut items: Vec<TreeItem> = Vec::new();
    let mut targets = HashMap::new();
    for row in rows {
        if !targets.contains_key(&row.path) {
            let label = row.path.strip_prefix(&root).unwrap_or(&row.path);
            items.push(TreeItem::new(&row.path, label).icon("file"));
            targets.insert(row.path.clone(), Target::Location(row.path.clone(), 0));
        }

        let key = format!("{}|{} {}", row.path, row.name, row.version);
        let detail = row.fix.as_ref().map_or("no fix".to_owned(), |fix| format!("fix: {fix}"));
        items.push(TreeItem::new(&key, &format!("{} {}", row.name, row.version)).parent(&row.path).icon("package").detail(&detail));
        targets.insert(key.clone(), Target::Location(row.path.clone(), row.line));

        for id in &row.advisories {
            let advisory_key = format!("{key}|{id}");
            let item = TreeItem::new(&advisory_key, id).parent(&key);
            let shown = display.as_deref_mut().map(|cache| {
                cache
                    .entry(id.clone())
                    .or_insert_with(|| {
                        row.details.iter().find(|a| a.id == *id).cloned().or_else(|| cached_advisory(id)).map(|a| (a.summary, a.severity))
                    })
                    .clone()
            });
            items.push(match shown {
                None => item,
                Some(Some((summary, severity))) => {
                    let icon = match level(severity.as_deref()) {
                        Severity::Error => "error",
                        Severity::Warning => "warning",
                        _ => "info",
                    };
                    item.icon(icon).detail(&summary)
                }
                Some(None) => item.icon("warning"),
            });
            targets.insert(advisory_key, Target::Url(format!("https://osv.dev/vulnerability/{id}")));
        }
    }

    (items, targets)
}

pub(crate) fn notify_project(project: &Project, level: NotificationLevel, title: &str, body: &str) {
    if project.notify(level, title, body, &[]).is_err() {
        host::notify(level, title, body);
    }
}

fn scan_and_notify(project: &Project, live: &mut live::Live, deadline: Instant) -> Result<(), Error> {
    let body = scan(project, live, deadline)?;
    notify_project(project, NotificationLevel::Info, "Dependency Audit", &body);

    Ok(())
}

fn scan_button(label: &str) -> Button {
    Button::new("scan", label).icon("refresh")
}

fn is_single_constraint(spec: &str) -> bool {
    let version = spec.trim_start_matches(['^', '~', '=', '>', '<', 'v']);

    version.starts_with(|c: char| c.is_ascii_digit()) && !version.contains([' ', '|', ',', '*'])
}

fn overlaps(dep: &Dep, text: &str, selection: &Range) -> bool {
    let underlined = range(text, dep.at.0, dep.at.1);

    underlined.end.line >= selection.start.line && underlined.start.line <= selection.end.line
}

fn upgrade_actions(findings: &[Finding], doc: &Document) -> Vec<CodeAction> {
    // Deps sharing a `${property}` share one range: one action, the highest fix.
    let mut upgrades: Vec<((usize, usize), String, String)> = Vec::new();
    let declared = parse(&doc.path, &doc.text, None);

    for finding in findings {
        let Some(at) = finding.dep.version_at else {
            continue;
        };
        let Some(fix) = finding.fix() else {
            continue;
        };
        if !doc.text.get(at.0..at.1).is_some_and(is_single_constraint) {
            continue;
        }
        if declared.iter().any(|other| other.version_at == Some(at) && dep_key(other) != dep_key(&finding.dep)) {
            continue;
        }

        match upgrades.iter_mut().find(|(range_at, ..)| *range_at == at) {
            Some((_, best, _)) => {
                if compare(&fix, best) == Ordering::Greater {
                    *best = fix;
                }
            }
            None => {
                let short = finding.dep.name.rsplit(':').next().unwrap_or(&finding.dep.name);
                upgrades.push((at, fix, short.to_owned()));
            }
        }
    }

    upgrades
        .into_iter()
        .map(|((from, to), fix, short)| CodeAction {
            title: format!("Upgrade {short} to {fix}"),
            kind: Some("quickfix".into()),
            preferred: true,
            edits: vec![(doc.path.clone(), vec![TextEdit { range: range(&doc.text, from, to), text: bumped(&doc.text[from..to], &fix) }])],
            data: None,
        })
        .collect()
}

impl DependencyAudit {
    fn clear_presentation(&mut self) { self.targets.clear(); self.display.clear(); }
    fn invalidate_report(&mut self) { self.clear_presentation(); refresh_report(); }
}

impl Plugin for DependencyAudit {
    fn run_command(&mut self, id: &str, ctx: &Invocation, arg: Option<&str>) -> Result<Option<String>, Error> {
        let picked;
        let arg = match arg {
            None if matches!(id, "preview-upgrade" | "open-declaration" | "open-advisory" | "ignore-finding") => {
                let Some(key) = actions::pick_row(ctx)? else { return Ok(None); };
                picked = key;
                Some(picked.as_str())
            }
            arg => arg,
        };
        let selected;
        let arg = if matches!(id, "open-advisory" | "ignore-finding") && arg.is_some_and(|key|key.starts_with("dep:")) {
            let Some(key) = actions::choose_advisory(ctx, arg.unwrap(), id == "ignore-finding")? else { return Ok(None); };
            selected = key;Some(selected.as_str())
        } else { arg };
        if matches!(id,"ignore-finding" | "review-ignores" | "export-ignores" | "import-ignores") { return self.scoped_command(id,ctx,arg); }
        if matches!(id,"preview-upgrade" | "open-advisory" | "open-declaration") { return self.resource_command(id,ctx,arg); }
        if id == "resolve-sbt-dependencies"{self.sbt_resolution.start(ctx)?;return Ok(None);}
        if id == "analyze-vulnerable-calls"{self.call_analysis.start(ctx)?;return Ok(None);}
        if id == "resolve-build-dependencies"{self.build_resolution.resolve(ctx,true,true)?;return Ok(None);}
        if id == "inspect-npm-graph" { self.npm_graph=Some(npm_graph::Snapshot::capture(ctx)?);reveal(npm_graph::VIEW)?;return Ok(None); }
        if id != "scan" {
            return Err(Error::NotFound(format!("command {id}")));
        }

        let deadline = Instant::now() + COMMAND_BUDGET;
        let project = ctx.project().ok_or("Open a project first.")?;
        self.clear_presentation();
        scan_and_notify(&project, &mut self.live, deadline)?;
        self.auto_resolve(ctx, deadline);

        Ok(None)
    }

    fn activate(&mut self, _ctx: &openidea_plugin::Context) -> Result<(), Error> {
        let _ = host::subscribe_paths("files-changed", GLOBS);
        let _ = host::subscribe(openidea_plugin::EventKind::EditorFocused);
        Ok(())
    }

    fn handle_events(&mut self, events: &[Event], ctx: &Invocation) {
        let cleared=events.iter().any(|event| matches!(event, Event::FileSaved(_) | Event::FilesChanged(_)));
        let dependency_files_changed=events.iter().any(|event| match event {
            Event::FileSaved(path) => kind(path).is_some(),
            Event::FilesChanged(changed) => changed.created.iter().chain(&changed.modified).chain(&changed.removed).any(|path| kind(path).is_some()),
            _ => false,
        });
        let owner=ctx.project().and_then(|p|p.root().ok()).filter(|root|!root.is_empty());
        let owner_changed=events.iter().any(|event|matches!(event,Event::EditorFocused(_))) && owner!=self.display_owner;
        if (cleared || owner_changed) && self.npm_graph.is_some() {refresh(npm_graph::VIEW);}
        if cleared || owner_changed {
            if dependency_files_changed || owner_changed {
                self.live.clear();
            }
            self.display_owner=owner;
            self.invalidate_report();
        }
        if host::setting_bool("autoCheck") == Some(false) {
            return;
        }

        let mut paths: Vec<&String> = Vec::new();
        for event in events {
            let (Event::FileOpened(path) | Event::FileSaved(path)) = event else {
                continue;
            };
            if kind(path).is_some() && !paths.contains(&path) {
                paths.push(path);
            }
        }

        let deadline = Instant::now() + EVENT_BUDGET;
        let mut uncached = EVENT_COORDINATES;
        let mut refreshed_cache=false;
        for path in paths {
            if let Err(e) = check(path, deadline, &mut uncached, &mut self.live) {
                host::log(LogLevel::Warn, &format!("OSV check of {path}: {e}"));
                if refreshed_cache && !cleared && !owner_changed { self.invalidate_report(); }
                return;
            }
            refreshed_cache=true;
        }
        if refreshed_cache && !cleared && !owner_changed { self.invalidate_report(); }
    }
}

fn editor_findings(doc:&Document,selection:Range)->Vec<Finding> {
        if doc.text.len()>512<<10 || !matches!(kind(&doc.path),Some(Kind::Pom|Kind::Gradle|Kind::PackageJson|Kind::ComposerJson|Kind::VersionCatalog|Kind::Requirements)) {
            return Vec::new();
        }

        let Some(project) = project_of(&doc.path) else {
            return Vec::new();
        };
        let lock = owned_lock(&project,&doc.path);
        let all = parse(&doc.path, &doc.text, lock.as_deref());
        let selected_ranges: Vec<(usize, usize)> =
            all.iter().filter(|dep| overlaps(dep, &doc.text, &selection)).filter_map(|dep| dep.version_at).collect();
        let deps: Vec<Dep> = all.into_iter().filter(|dep| overlaps(dep,&doc.text,&selection) || dep.version_at.is_some_and(|at| selected_ranges.contains(&at))).collect();
        if deps.is_empty() {
            return Vec::new();
        }

        let Ok(findings) = audit(&deps, false, now(), Instant::now()) else {
            return Vec::new();
        };

        let scopes = scoped::load(&project.root().unwrap_or_default());
        let findings:Vec<_> = findings.into_iter().filter_map(|mut f|scoped::filter(&scopes,&mut f).then_some(f)).collect();
        findings
}
impl Providers for DependencyAudit {
    fn code_actions(&mut self,doc:&Document,selection:Range)->Vec<CodeAction> {editor_actions::offer(doc,selection,&editor_findings(doc,selection))}
    fn resolve_code_action(&mut self,doc:&Document,action:CodeAction)->Result<CodeAction,Error> {editor_actions::resolve(self,doc,action)}
}

impl DependencyAudit {
    fn rebuild_target(&mut self, key: &str, ctx: &Invocation) -> Result<(), Error> {
        self.targets.clear();
        let Some(project) = ctx.project() else {
            return Ok(());
        };
        let Some(rows) = stored_rows(&project)? else {
            return Ok(());
        };

        let (kind,key) = if let Some(key) = key.strip_prefix("dep:") { ("dep",key) } else if let Some(key) = key.strip_prefix("advisory:") { ("advisory",key) } else { ("",key) };
        let requested = if kind.is_empty() { key.to_owned() } else { format!("{kind}:{key}") };
        for row in rows {
            let base = format!("{}|{} {}", row.path, row.name, row.version);
            let target = if key == row.path {
                Some(Target::Location(row.path.clone(), 0))
            } else if key == base && kind != "advisory" {
                Some(Target::Location(row.path.clone(), row.line))
            } else {
                row.advisories
                    .iter()
                    .find(|id| kind != "dep" && osv::is_safe_id(id) && key == format!("{base}|{id}"))
                    .map(|id| Target::Url(format!("https://osv.dev/vulnerability/{id}")))
            };
            if let Some(target) = target {
                self.targets.insert(requested.clone(), target);
                break;
            }
        }

        Ok(())
    }
}

impl Views for DependencyAudit {
    fn render(&mut self, view: &str, _viewport: Option<Viewport>, ctx: &Invocation) -> Result<View, Error> {
        if view==sbt_resolution::VIEW{return Ok(self.sbt_resolution.render(ctx));}
        if view==call_analysis::VIEW{return Ok(self.call_analysis.render(ctx));}
        if view==build_resolution::VIEW{return Ok(self.build_resolution.render(ctx));}
        if view==npm_graph::VIEW {
            let inspect=Button::new("inspect-npm-graph","Inspect package-lock.json").icon("refresh");
            return Ok(self.npm_graph.as_ref().map(|s|s.render(ctx)).unwrap_or_else(||View::message(self.npm_error.as_deref().unwrap_or("Inspect the npm lockfile graph of package-lock.json (focused editor, else the nearest one in the project)."))).button(inspect));
        }
        (|| -> Result<View, Error> {
            self.targets.clear();
            let Some(project) = ctx.project() else {
                self.clear_presentation();self.display_owner=None;
                return Ok(View::message("Open a project first."));
            };
            let Some(root)=project.root().ok().filter(|root|!root.is_empty()) else {
                self.clear_presentation();self.display_owner=None;
                return Ok(View::message("Can't read this project's folder. Reopen the project, then run Dependency Audit: Scan Project."));
            };
            if self.display_owner.as_deref()!=Some(root.as_str()) { self.clear_presentation();self.display_owner=Some(root.clone()); }
            let failure = last_failure(&project);
            let report=match stored_report(&project) {
                Ok(report)=>report,
                Err(_)=> { self.clear_presentation(); return Ok(View::message("Can't read the saved report. Run Dependency Audit: Scan Project.")); }
            };
            let Some(report) = report else {
                self.clear_presentation();
                let message = match &failure {
                    Some(failure) => format!("The last scan failed: {}. Run Dependency Audit: Scan Project to retry.", failure.message),
                    None => "Run Dependency Audit: Scan Project to check this project's dependencies against OSV.dev.".into(),
                };
                return Ok(View::message(&message).button(scan_button("Scan")).button(Button::new("review-ignores","Review Ignored Findings").icon("list")));
            };
            let status = report_status(&report, failure.as_ref());
            if report.rows.is_empty() {
                self.clear_presentation();
                let message = if report.files_scanned == Some(0) { no_dependency_files() } else { "The last successful scan found no known vulnerabilities (after configured ignores).".into() };
                return Ok(View::message(&message)
                    .button(scan_button("Rescan")).button(Button::new("review-ignores","Review Ignored Findings").icon("list"))
                    .status(&status));
            }
            if ui_kit::web::is_rendering() {
                self.targets.clear();
                return Ok(View::new(resource_body(&root, &report, failure.as_ref())).button(scan_button("Rescan")).button(Button::new("review-ignores","Review Ignored Findings").icon("list")).status(&status));
            }
            let (mut items, targets) = tree_of(&root, &report.rows, Some(&mut self.display));
            self.targets = targets;
            self.expansion.apply(&mut items);
            Ok(View::new(Body::Tree(items)).button(scan_button("Rescan")).button(Button::new("review-ignores","Review Ignored Findings").icon("list")).status(&status))
        })().map(ui_kit::actions::available)
    }

    fn handle(&mut self, view: &str, event: ViewEvent, ctx: &Invocation) -> Result<(), Error> {
        if view==sbt_resolution::VIEW{match event{ViewEvent::Button(key)if key=="sbt-poll"=>return self.sbt_resolution.poll(ctx),ViewEvent::Button(key)if key=="sbt-cancel"=>return self.sbt_resolution.cancel(ctx),ViewEvent::Button(key)if key=="sbt-audit"=>{let e=self.sbt_resolution.evidence(ctx)?;let p=ctx.project().ok_or(Error::Stale)?;return sbt_resolution::audit_evidence(&p,e);},_=>return Ok(())}}
        if view==call_analysis::VIEW{match event{ViewEvent::Button(key)if key=="calls-poll"=>return self.call_analysis.poll(ctx),ViewEvent::Button(key)if key=="calls-cancel"=>return self.call_analysis.cancel(ctx),_=>return Ok(())}}
        if view==build_resolution::VIEW{match event{ViewEvent::Button(key)if key=="resolve-build"=>return self.build_resolution.start(ctx,true),ViewEvent::Message(raw)=>return self.resolution_tick(&raw,ctx),ViewEvent::Button(key)if key=="resolution-cancel"=>return self.build_resolution.cancel(ctx),ViewEvent::Button(key)if key=="resolution-audit"=>{let evidence=self.build_resolution.evidence(ctx)?;let project=ctx.project().ok_or(Error::Stale)?;return build_resolution::audit_evidence(&project,evidence);},_=>return Ok(())}}
        if view==npm_graph::VIEW {
            if matches!(&event,ViewEvent::Button(key) if key=="inspect-npm-graph") {
                match npm_graph::Snapshot::capture(ctx) {
                    Ok(snapshot)=>{self.npm_graph=Some(snapshot);self.npm_error=None;}
                    Err(error)=>{self.npm_graph=None;self.npm_error=Some(error.to_string());}
                }
                refresh(npm_graph::VIEW);
                return Ok(());
            }
            if let Some(snapshot)=self.npm_graph.as_mut(){return snapshot.handle(event,ctx);}
            return Ok(());
        }
        if ui_kit::web::handle(self, view, &event, ctx, include_str!("ui_config.json"))? {
            return Ok(());
        }

        if self.expansion.handle(&event) {
            refresh(view);

            return Ok(());
        }

        match event {
            ViewEvent::Button(id) if id == "review-ignores" => {
                if let Some(message)=self.scoped_command("review-ignores",ctx,None)? {host::notify(NotificationLevel::Info,"Dependency Audit",&message);}
                Ok(())
            }
            ViewEvent::Button(id) if id == "scan" => {
                let deadline = Instant::now() + COMMAND_BUDGET;
                let project = ctx.project().ok_or("Open a project first.")?;
                self.clear_presentation();
                scan_and_notify(&project, &mut self.live, deadline)?;
                self.auto_resolve(ctx, deadline);
                Ok(())
            }
            ViewEvent::Activate(key) => {
                self.rebuild_target(&key, ctx)?;
                match self.targets.get(&key) {
                    Some(Target::Location(path, line)) => host::open_location(path, *line, 0),
                    Some(Target::Url(url)) => host::open_url(url),
                    None => Ok(()),
                }
            }
            _ => Ok(()),
        }
    }
}

impl DiagnosticsProvider for DependencyAudit {
    fn diagnostics(&mut self, doc: &Document) -> Vec<Diagnostic> {
        let Some(root) = project_of(&doc.path).and_then(|p| p.root().ok()).filter(|r| !r.is_empty()) else { return Vec::new() };
        self.live.diagnostics(&root, doc)
    }
}

register_plugin!(DependencyAudit, providers, views, diagnostics, commit_checks);

#[cfg(test)]
mod tests;

impl DependencyAudit {
    fn auto_resolve(&mut self, ctx: &Invocation, deadline: Instant) {
        let run_tool = host::setting_bool("autoResolveBuilds") == Some(true)
            && deadline.saturating_duration_since(Instant::now()) >= MIN_RESOLVE_TIME;
        let Err(error) = self.build_resolution.resolve(ctx, run_tool, false) else { return };
        // NotFound: no Maven or Gradle build in this project, so there was nothing to resolve.
        if matches!(error, Error::Stale | Error::Cancelled | Error::NotFound(_)) {
            return;
        }
        let root = ctx.project().and_then(|project| project.root().ok()).unwrap_or_default();
        if !self.auto_resolve_reported.insert(root) {
            return;
        }
        let reason = match error {
            Error::Invalid(message) | Error::Failed(message) | Error::NotFound(message) => message,
            other => other.to_string(),
        };
        host::notify(
            NotificationLevel::Warning,
            "Dependency Audit",
            &format!("Automatic build resolution didn't run: {reason} Run Dependency Audit: Resolve Effective Build Dependencies… to retry."),
        );
    }
    fn resolution_tick(&mut self, raw: &str, ctx: &Invocation) -> Result<(), Error> {
        let request: serde_json::Value = serde_json::from_str(raw).map_err(|_| Error::Stale)?;
        if request["op"] != "resolution-tick" || !self.build_resolution.running() {
            return Err(Error::Stale);
        }
        let result = self.build_resolution.poll(ctx);
        let reply = serde_json::json!({"_backgroundPoll": request["_backgroundPoll"]});
        ui_kit::web::post_view_message(build_resolution::VIEW, &reply.to_string(), UI_CONFIG)?;
        result?;
        if self.build_resolution.succeeded() {
            let evidence = self.build_resolution.evidence(ctx)?;
            let project = ctx.project().ok_or(Error::Stale)?;
            build_resolution::audit_evidence(&project, evidence)?;
        }
        Ok(())
    }
}
