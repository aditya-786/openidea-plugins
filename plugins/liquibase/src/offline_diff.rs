//! Explicit offline-snapshot text report, not generated migrations or DB truth.
#[path = "offline_review.rs"]
mod review;
#[path = "offline_generated.rs"]
mod generated;
use super::*;
use openidea_plugin::host::{Child, Command};
use openidea_plugin::views::{Field, FieldValue, Table};
use sha2::{Digest, Sha256};
pub const VIEW: &str = "offline-diff";
const MAX_FILE: usize = 128 << 10;
const MAX_OUTPUT: usize = 64 << 10;
const TYPES: &[(&str, &str)] = &[("tables", "Table"), ("columns", "Column"), ("indexes", "Index"), ("views", "View")];
#[derive(Clone)]
struct Input {
    path: String,
    text: String,
    db: String,
}
#[derive(Clone)]
struct Plan {
    root: String,
    reference: Input,
    target: Input,
    staged: Vec<(String, String)>,
    kind: String,
    args: Vec<String>,
    generated: Option<generated::Request>,
}
struct Running {
    plan: Plan,
    child: Child,
    at: Instant,
    output: String,
    bytes: usize,
    status: Option<i32>,
}
#[derive(Clone, Debug)]
struct Item {
    kind: String,
    object: String,
    detail: String,
}
struct Report {
    root: String,
    source: String,
    items: Vec<Item>,
    at: Instant,
}
#[derive(Default)]
pub(crate) struct State {
    generation: u64,
    generated: Option<generated::Document>,
    generated_view: bool,
    review: Option<review::Selection>,
    running: Option<Running>,
    report: Option<Report>,
    selected: Option<usize>,
    status: String,
    visible: std::ops::Range<u32>,
    page: u32,
}
fn relative(root: &str, path: &str) -> Option<String> {
    let path = path.strip_prefix(&format!("{}/", root.trim_end_matches('/')))?;
    (!path.is_empty()
        && path.len() <= 192
        && path.bytes().all(|b| b.is_ascii_alphanumeric() || b"/_.-".contains(&b))
        && !path.split('/').any(|p| p.is_empty() || p == "." || p == ".."))
    .then(|| path.into())
}
fn read(project: &Project, path: &str, max: usize) -> Result<String, Error> {
    let stat = project.stat(path)?;
    if stat.kind != openidea_plugin::FileKind::File || stat.size > max as u64 {
        return Err(Error::Stale);
    }
    let text = project.read_file(path)?;
    if text.len() > max {
        return Err(Error::Stale);
    }
    Ok(text)
}
fn input(project: &Project, root: &str, path: &str) -> Result<Input, Error> {
    if host::project_of(path).and_then(|p| p.root().ok()).as_deref() != Some(root) {
        return Err(Error::Stale);
    }
    let path = relative(root, path).ok_or(Error::Stale)?;
    let text = read(project, &path, MAX_FILE)?;
    let value = crate::strict_json::parse(&text, MAX_FILE)?;
    let snapshot = value.get("snapshot").ok_or(Error::Stale)?;
    let db = snapshot
        .pointer("/database/shortName")
        .and_then(|v| v.as_str())
        .filter(|name| matches!(*name, "h2" | "postgresql" | "mysql" | "mariadb"))
        .ok_or(Error::Stale)?
        .to_owned();
    let objects = snapshot.get("objects").and_then(|v| v.as_object()).ok_or(Error::Stale)?;
    let mut count = 0usize;
    for (kind, objects) in objects {
        let name = kind.strip_prefix("liquibase.structure.core.").ok_or(Error::Stale)?;
        if !matches!(
            name,
            "Catalog"
                | "Column"
                | "ForeignKey"
                | "Index"
                | "PrimaryKey"
                | "Schema"
                | "Sequence"
                | "StoredProcedure"
                | "Table"
                | "UniqueConstraint"
                | "View"
        ) {
            return Err(Error::Stale);
        }
        let values = objects.as_array().ok_or(Error::Stale)?;
        count = count.saturating_add(values.len());
        if count > 4096 || values.iter().any(|v| !v.is_object()) {
            return Err(Error::Stale);
        }
    }

    Ok(Input { path, text, db })
}
fn stage_plan(root: &str, reference: &Input, target: &Input, kind: &str) -> Result<(String, Vec<(String, String)>), Error> {
    let dir = host::fs::plugin_dir()?;
    if dir.len() > 2048 || dir.contains(',') || dir.chars().any(char::is_control) {
        return Err(Error::Stale);
    }
    let mut hash = Sha256::new();
    for part in [root, reference.path.as_str(), reference.text.as_str(), target.path.as_str(), target.text.as_str(), kind] {
        hash.update((part.len() as u64).to_le_bytes());
        hash.update(part.as_bytes());
    }
    let id = format!("{:x}", hash.finalize());
    let scrub = |input: &Input| -> Result<String, Error> {
        let mut value: serde_json::Value = serde_json::from_str(&input.text).map_err(|_| Error::Stale)?;
        let database = value.pointer_mut("/snapshot/database").and_then(|v| v.as_object_mut()).ok_or(Error::Stale)?;
        database.retain(|key, _| matches!(key.as_str(), "shortName" | "productName" | "majorVersion" | "minorVersion" | "productVersion"));
        serde_json::to_string(&value).map_err(|_| Error::Stale)
    };
    let files = vec![
        (format!("{dir}/lb-diff-{id}-reference.json"), scrub(reference)?),
        (format!("{dir}/lb-diff-{id}-target.json"), scrub(target)?),
        (format!("{dir}/lb-diff-{id}-defaults.properties"), "# Empty defaults for explicitly reviewed offline diff.\n".into()),
    ];
    Ok((dir, files))
}
fn materialize(plan: &Plan) -> Result<(), Error> {
    let dir = host::fs::plugin_dir()?;
    let entries = host::fs::read_dir(&dir)?;
    // Native private stat may reject a missing canonical path as Invalid.
    // A complete bounded directory listing proves absence without interpreting errors.
    if entries.len() >= 4096 {
        return Err("Private staging directory listing reached the host limit.".into());
    }
    generated::vacant(plan)?;
    let existing = entries.iter().filter(|e| e.name.starts_with("lb-diff-")).count();
    let mut new = usize::from(plan.generated.is_some());
    for (path, _) in &plan.staged {
        let name = path.rsplit_once('/').ok_or(Error::Stale)?.1;
        match entries.iter().find(|entry| entry.name == name) {
            None => new += 1,
            Some(entry) if entry.kind == openidea_plugin::FileKind::File => {
                let stat = host::fs::stat(path)?;
                if stat.kind != openidea_plugin::FileKind::File || !host::fs::read_bytes(path)?.is_empty() {
                    return Err("Private staged input already exists and may be active. Stop its owning instance or clear stale staging files before retrying.".into());
                }
            }
            _ => return Err(Error::Stale),
        }
    }
    if existing + new > 96 {
        return Err("Private staging cap reached (96 known staging/output files; up to 32 report sets or 24 generated runs). Remove the plugin's empty lb-diff cache files or reinstall before creating more.".into());
    }
    for (path, text) in &plan.staged {
        if let Err(error) = host::fs::write_bytes(path, text.as_bytes()) {
            cleanup(plan);
            return Err(error);
        }
    }
    Ok(())
}
fn cleanup(plan: &Plan) {
    if let Some(request) = plan.generated.as_ref().filter(|request| request.started) {
        if host::fs::write_bytes(&request.output, b"").is_err() { host::log(openidea_plugin::LogLevel::Warn, "Generated private output cleanup could not complete; sensitive source may remain until manual cleanup/uninstall."); }
    }
    for (path, expected) in &plan.staged {
        match host::fs::read_bytes(path) {
            Ok(bytes) if bytes == expected.as_bytes() => {
                if host::fs::write_bytes(path, b"").is_err() {
                    host::log(openidea_plugin::LogLevel::Warn,
                        "Liquibase offline staging cleanup could not complete; owner-only copies may remain until manual cleanup/uninstall.");
                }
            }
            Ok(bytes) if bytes.is_empty() => {},
            Err(Error::NotFound(_)) => {},
            _ => host::log(openidea_plugin::LogLevel::Warn,
                "Liquibase staged input changed or is unreadable; cleanup retained the slot because exact source ownership is unavailable. Inspect private staging before retrying."),
        }
    }
}
fn staged_unchanged(plan: &Plan) -> bool {
    plan.staged.iter().all(|(path, text)| host::fs::read_bytes(path).ok().as_deref() == Some(text.as_bytes()))
}
fn verify(ctx: &Invocation, plan: &Plan) -> Result<Project, Error> {
    let project = ctx.project().ok_or(Error::Stale)?;
    if project.root()? != plan.root {
        return Err(Error::Stale);
    }
    for input in [&plan.reference, &plan.target] {
        let full = format!("{}/{}", plan.root, input.path);
        if host::project_of(&full).and_then(|p| p.root().ok()).as_deref() != Some(plan.root.as_str())
            || read(&project, &input.path, MAX_FILE)? != input.text
        {
            return Err(Error::Stale);
        }
    }
    Ok(project)
}
fn args(reference: &Input, target: &Input, kind: &str) -> Result<Vec<String>, Error> {
    if reference.path == target.path
        || reference.db != target.db
        || !TYPES.iter().any(|(name, _)| *name == kind) && kind != "tables,columns,indexes,views"
    {
        return Err(Error::Stale);
    }
    Ok(vec![
        "--defaults-file=liquibase-offline.properties".into(),
        "--search-path=.".into(),
        "--log-level=off".into(),
        "--show-banner=false".into(),
        "--output-file-encoding=UTF-8".into(),
        "diff".into(),
        format!("--reference-url=offline:{}?snapshot={}", reference.db, reference.path),
        format!("--url=offline:{}?snapshot={}", target.db, target.path),
        format!("--diff-types={kind}"),
    ])
}
// Command-specific environment keys take precedence over generic CLI URLs in
// Liquibase4.29.2. API1.4 refuses inherited-key collisions before spawning, so
// reserving the reviewed values also refuses an ambient command override.
fn offline_command(plan: &Plan) -> Result<Command, Error> {
    let prefix = if plan.generated.is_some() { "LIQUIBASE_COMMAND_DIFF_CHANGELOG" } else { "LIQUIBASE_COMMAND_DIFF" };
    let mut command = Command::new("liquibase").args(&plan.args);
    for (argument, name) in [("--url=", "URL"), ("--reference-url=", "REFERENCE_URL"), ("--diff-types=", "DIFF_TYPES")] {
        let value = plan.args.iter().find_map(|arg| arg.strip_prefix(argument)).ok_or(Error::Stale)?;
        command = command.env(&format!("{prefix}_{name}"), value);
    }
    if let Some(request) = &plan.generated {
        command = command.env(&format!("{prefix}_CHANGELOG_FILE"), &request.output)
            .env(&format!("{prefix}_AUTHOR"), &request.author);
    }
    Ok(command)
}

fn parse(output: &str, types: &str) -> Result<Vec<Item>, Error> {
    if output.len() > MAX_OUTPUT || output.chars().any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t')) {
        return Err(Error::Stale);
    }
    let requested: Vec<_> = TYPES.iter().filter(|(name, _)| types.split(',').any(|t| t == *name)).map(|(_, name)| *name).collect();
    let mut seen = BTreeSet::new();
    let mut items: Vec<Item> = Vec::new();
    let mut section: Option<String> = None;
    let mut empty = false;
    let mut section_items = 0;
    let mut schema = String::new();
    for line in output.lines() {
        let line = line.trim_end_matches('\r');
        if line == "Liquibase command 'diff' was executed successfully." {
            continue;
        }
        if ["Missing ", "Unexpected ", "Changed "].iter().any(|prefix| line.starts_with(prefix)) {
            if section.is_some() && !empty && section_items == 0 {
                return Err(Error::Stale);
            }
            let Some((title, rest)) = line.split_once(": ").or_else(|| line.strip_suffix(':').map(|t| (t, ""))) else {
                return Err(Error::Stale);
            };
            let expected =
                requested.iter().any(|ty| ["Missing", "Unexpected", "Changed"].iter().any(|kind| title == format!("{kind} {ty}(s)")));
            if !expected || !seen.insert(title.to_owned()) {
                return Err(Error::Stale);
            }
            if !rest.is_empty() && rest != "NONE" {
                return Err(Error::Stale);
            }
            section = Some(title.into());
            empty = rest == "NONE";
            section_items = 0;
            schema.clear();
            continue;
        }
        if let Some(section) = &section {
            if line.trim().is_empty() {
                continue;
            }
            if line.starts_with("  SCHEMA: ") {
                schema = line.trim().to_owned();
                continue;
            }
            if line.starts_with("     ") && !line.starts_with("          ") && !empty {
                if items.len() == 512 || line.len() > 1024 {
                    return Err(Error::Stale);
                }
                let name = line.trim();
                if name.is_empty() {
                    return Err(Error::Stale);
                }
                items.push(Item { kind: section.clone(), object: name.into(), detail: format!("{schema}\n{name}") });
                section_items += 1;
            } else if line.starts_with("          ") && !empty {
                let item = items.last_mut().ok_or(Error::Stale)?;
                if item.detail.len() + line.len() > 4096 {
                    return Err(Error::Stale);
                }
                item.detail.push('\n');
                item.detail.push_str(line.trim());
            } else {
                return Err(Error::Stale);
            }
        }
    }
    if seen.len() != requested.len() * 3 || section.is_some() && !empty && section_items == 0 {
        return Err(Error::Stale);
    }
    Ok(items)
}
fn sanitized(output: &str, plan: &Plan) -> String {
    let mut secrets = Vec::new();
    for input in [&plan.reference, &plan.target] {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&input.text) {
            for key in ["url", "user", "password", "username", "token", "secret"] {
                if let Some(value) = value.pointer(&format!("/snapshot/database/{key}")).and_then(|v| v.as_str()).filter(|s| !s.is_empty())
                {
                    secrets.push(value.to_owned());
                }
            }
        }
    }
    output
        .lines()
        .filter(|line| !line.starts_with("Reference Database:") && !line.starts_with("Comparison Database:"))
        .map(|line| {
            let mut line = line.to_owned();
            for secret in &secrets {
                line = line.replace(secret, "[redacted]");
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}
impl Liquibase {
    pub(crate) fn start_offline_diff(&mut self, ctx: &Invocation) -> Result<(), Error> { self.start_offline(ctx, false) }
    pub(crate) fn start_offline_generation(&mut self, ctx: &Invocation) -> Result<(), Error> { self.start_offline(ctx, true) }
    fn start_offline(&mut self, ctx: &Invocation, generating: bool) -> Result<(), Error> {
        if self.offline.running.is_some() {
            return Err("Stop or finish the existing instance-owned diff first.".into());
        }
        let project = ctx.project().ok_or("Open the owning project first.")?;
        let root = project.root()?;
        let paths = project.find_files("**/*.json", 129)?;
        if paths.len() > 128 {
            return Err("Choose a smaller project: offline snapshot discovery is limited to 128 JSON files.".into());
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut inputs = Vec::new();
        let mut bytes = 0;
        for path in paths {
            if Instant::now() >= deadline || bytes > 2 << 20 {
                return Err("Snapshot discovery is incomplete; narrow the project first.".into());
            }
            if let Ok(input) = input(&project, &root, &path) {
                bytes += input.text.len();
                inputs.push(input);
            }
        }
        if inputs.len() < 2 {
            return Err("Two supported same-database Liquibase JSON snapshots are required (h2/postgresql/mysql/mariadb); unsupported files are excluded.".into());
        }
        let choices: Vec<_> =
            inputs.iter().enumerate().map(|(i, s)| PickItem::new(&i.to_string(), &format!("{} ({})", s.path, s.db))).collect();
        let Some(reference) = host::pick("Reference snapshot (basis)", &choices)? else { return Ok(()) };
        let Some(target) = host::pick("Target snapshot (comparison)", &choices)? else { return Ok(()) };
        let reference = reference.parse::<usize>().ok().and_then(|i| inputs.get(i)).cloned().ok_or(Error::Stale)?;
        let target = target.parse::<usize>().ok().and_then(|i| inputs.get(i)).cloned().ok_or(Error::Stale)?;
        let choices: Vec<_> = TYPES
            .iter()
            .map(|(id, label)| PickItem::new(id, label))
            .chain(std::iter::once(PickItem::new("tables,columns,indexes,views", "Tables, columns, indexes and views")))
            .collect();
        let Some(kind) = host::pick("Compare object categories (not executable changes)", &choices)? else { return Ok(()) };
        args(&reference, &target, &kind)?;
        let (dir, staged, generated) = if generating {
            let suggestion = host::setting_string("author").filter(|a| generate::identity(a)).unwrap_or_else(|| "author".into());
            let Some(author) = host::input("Generated changelog author", Some(&suggestion))? else { return Ok(()); };
            let (dir, staged, request) = generated::prepare(&root, &reference, &target, &kind, author.trim())?;
            (dir, staged, Some(request))
        } else { let (dir, staged) = stage_plan(&root, &reference, &target, &kind)?; (dir, staged, None) };
        let mut staged_reference = reference.clone();
        staged_reference.path = staged[0].0.rsplit('/').next().unwrap().into();
        let mut staged_target = target.clone();
        staged_target.path = staged[1].0.rsplit('/').next().unwrap().into();
        let mut argv = args(&staged_reference, &staged_target, &kind)?;
        argv[0] = format!("--defaults-file={}", staged[2].0);
        argv[1] = format!("--search-path={dir}");
        if let Some(request) = &generated {
            argv[5] = "diff-changelog".into();
            argv.push(format!("--changelog-file={}", request.output));
            argv.push(format!("--author={}", request.author));
        }
        let mut plan = Plan { root, reference, target, staged, kind, args: argv, generated };
        verify(ctx, &plan)?;
        let sources = format!(
            "Reference: {} [editor-text SHA256 {:x}]\nTarget: {} [editor-text SHA256 {:x}]",
            plan.reference.path,
            Sha256::digest(plan.reference.text.as_bytes()),
            plan.target.path,
            Sha256::digest(plan.target.text.as_bytes())
        );
        let review = format!(
            "Working project: {:?}\n{}\nArgument contract: Liquibase4.29.2 text diff; installed CLI identity/version/edition are unverified.\nProgram: liquibase\nExact argv: {}\nOnly the selected local snapshot URLs are passed; no online URLs or credentials are accepted. No changelog/update or output-file flag is requested. Exact current buffers are copied into owner-only private staging, with snapshot connection identity fields removed and generated empty defaults. Still-matching staged inputs are overwritten empty on finish/Stop/failure; changed/unreadable slots are retained for owner review. Cleanup is a content check, not an atomic file-identity lock. Crash/reclaim can leave copies until cleanup/uninstall; 32 distinct staging identities maximum.\nCommand-specific URL and diff-type environment keys are reserved to reviewed values; inherited collisions fail before spawn. The approved CLI otherwise inherits environment/JVM/extensions. This is not a network/filesystem sandbox: ambient settings may affect behavior/output. Process permission authorizes that executable, not a verified database identity.\nOutput is bounded and partially redacted; stored snapshot credentials are not displayed. Inspect only; no DDL/changelog is generated. Instance-owned process may be lost after roughly five idle minutes. Status/Stop are explicit; no background timer.",
            plan.root,
            sources,
            serde_json::to_string(&plan.args).unwrap_or_default()
        );
        let review = if let Some(request) = &plan.generated {
            format!("Working project: {:?}\n{}\nArgument contract: Liquibase4.29.2 offline diff-changelog; installed identity/version/edition UNVERIFIED.\nProgram: liquibase\nExact argv: {}\nPrivate XML output: {}\nRequests generated source intended to bring target snapshot toward reference; project source is saved only by a later explicit FileAction. Exact reviewed buffers are privately staged with connection identity removed/empty defaults. Command-specific URL/diff-type/output/author environment keys are reserved to reviewed values; inherited collisions fail before spawn. The CLI may create auxiliary files; ambient environment/JVM/extensions are NOT sandboxed by process permission. Output is bounded and is not redacted/rewritten, so schema/default literals may be sensitive. Still-matching staged inputs and started-command output are overwritten empty on finish/Stop; changed/unreadable input slots are retained. Cleanup is not an atomic file-identity lock; crash/reclaim or auxiliary files can remain until manual cleanup.96 known staging/output files maximum.\nReview complete XML changesets after generation, then explicitly save an exact subset through a non-overwriting FileAction. Omitted dependencies, SQL correctness, full schema coverage and CLI output transport completeness are unverified. No update/rollback command is requested.", plan.root, sources, serde_json::to_string(&plan.args).unwrap_or_default(), request.output)
        } else { review };
        if !ui_kit::prompts::confirm_exact(if generating { "Generate this offline changelog candidate?" } else { "Run this offline snapshot diff?" }, &review)? {
            return Ok(());
        }
        let project = verify(ctx, &plan)?;
        let command = offline_command(&plan)?;
        materialize(&plan)?;
        if !staged_unchanged(&plan) || verify(ctx, &plan).is_err() || generated::vacant(&plan).is_err() {
            cleanup(&plan);
            return Err(Error::Stale);
        }
        let child = match host::start(&command, Some(&project)) {
            Ok(child) => child,
            Err(error) => {
                cleanup(&plan);
                return Err(error);
            }
        };
        // Before a successful start, unexpected output is not ours to clear.
        if let Some(request) = &mut plan.generated { request.started = true; }
        if child.close_stdin().is_err() {
            let _ = child.kill();
            cleanup(&plan);
            return Err(Error::Failed("Could not close offline diff input; process stopped.".into()));
        }
        if self.offline.report.as_ref().is_some_and(|report| report.root != plan.root) {
            self.offline.report = None;
            self.offline.selected = None;
            self.offline.review = None;
        }
        if generated::owner(&self.offline).is_some_and(|owner| owner != plan.root.as_str()) {
            // The current candidate is retained only in its owner scope; save validation checks exact source bytes.
            self.offline.generated_view = false;
        }
        self.offline.running = Some(Running { plan, child, at: Instant::now(), output: String::new(), bytes: 0, status: None });
        self.offline.status = "Offline diff started. Press Refresh Status to read its progress.".into();
        ui_kit::web::reveal_view(VIEW, include_str!("ui_config.json"))?;
        ui_kit::web::refresh_view(VIEW, include_str!("ui_config.json"));
        Ok(())
    }
    pub(crate) fn poll_offline_diff(&mut self, ctx: &Invocation) -> Result<(), Error> {
        let Some(mut run) = self.offline.running.take() else { return Ok(()) };
        if ctx.project().and_then(|p| p.root().ok()).as_deref() != Some(run.plan.root.as_str()) {
            self.offline.running = Some(run);
            return Err(Error::Stale);
        }
        if verify(ctx, &run.plan).is_err() || !staged_unchanged(&run.plan) || run.at.elapsed() > Duration::from_secs(120) {
            let _ = run.child.kill();
            cleanup(&run.plan);
            self.offline.status = "Diff stopped after source/scope change or expired polling lease; previous report retained.".into();
            ui_kit::web::refresh_view(VIEW, include_str!("ui_config.json"));
            return Err(Error::Stale);
        }
        for _ in 0..8 {
            let chunk = match run.child.poll(4096) {
                Ok(chunk) => chunk,
                Err(_) => {
                    let _ = run.child.kill();
                    cleanup(&run.plan);
                    self.offline.status = "Process output could not be read; previous report retained.".into();
                    ui_kit::web::refresh_view(VIEW, include_str!("ui_config.json"));
                    return Err(Error::Failed("Offline diff process read failed.".into()));
                }
            };
            let count = chunk.stdout.len() + chunk.stderr.len();
            run.bytes = run.bytes.saturating_add(count);
            if run.bytes > MAX_OUTPUT {
                let _ = run.child.kill();
                cleanup(&run.plan);
                self.offline.status = "Diff output cap exceeded; output discarded and previous report retained.".into();
                ui_kit::web::refresh_view(VIEW, include_str!("ui_config.json"));
                return Ok(());
            }
            run.output.push_str(&chunk.stdout);
            if let Some(status) = chunk.status {
                run.status = Some(status);
            }
            if count == 0 && run.status.is_some() {
                if verify(ctx, &run.plan).is_err() || !staged_unchanged(&run.plan) {
                    let _ = run.child.kill();
                    cleanup(&run.plan);
                    self.offline.status = "Source changed before completed output could be accepted; previous report retained.".into();
                    ui_kit::web::refresh_view(VIEW, include_str!("ui_config.json"));
                    return Err(Error::Stale);
                }
                let status = run.status.unwrap();
                if status == 0 && run.plan.generated.is_some() {
                    self.offline.status = if generated::capture(&mut self.offline, &run.plan).is_ok() {
                        "Observed generated XML captured. Choose complete changesets and save an exact preview through editor code actions. No update/rollback command or project output path was requested; version/schema/transport and omitted dependencies remain unverified."
                    } else { "Generated output was missing, unsupported, oversized or invalid; previous candidate retained. No no-differences conclusion is available." }.into();
                } else if status == 0 {
                    let safe = sanitized(&run.output, &run.plan);
                    match parse(&safe, &run.plan.kind) {
                        Ok(items) => {
                            let accepted = review::replace_report(&mut self.offline, Report {
                                root: run.plan.root.clone(),
                                source: format!("reference={} [editor-text SHA256 {:x}] -> target={} [editor-text SHA256 {:x}] · {} · snapshot coverage and output-drain completeness unverified · 4.29.2 text contract; installed CLI/version/edition unverified", run.plan.reference.path, Sha256::digest(run.plan.reference.text.as_bytes()), run.plan.target.path, Sha256::digest(run.plan.target.text.as_bytes()), run.plan.kind),
                                items, at: Instant::now(),
                            });
                            self.offline.status = if accepted.is_ok() {
                                "Observed offline CLI report rows captured; drain completeness is unverified. These are not deployable changes or runtime database truth. Known source credentials were redacted; other sensitive output may remain."
                            } else { "Report identity exhausted; previous report retained. Reload before accepting another report." }.into();
                        },
                        Err(_) => self.offline.status = "CLI output did not match the bounded supported text report; previous report retained. No differences/absence conclusion is available.".into(),
                    }
                } else {
                    self.offline.status =
                        format!("CLI exited with status {status}; raw diagnostics withheld and previous report retained.");
                }
                cleanup(&run.plan);
                ui_kit::web::refresh_view(VIEW, include_str!("ui_config.json"));
                return Ok(());
            }
        }
        self.offline.running = Some(run);
        self.offline.status = "The diff is still running. Press Refresh Status again, or Stop Diff.".into();
        ui_kit::web::refresh_view(VIEW, include_str!("ui_config.json"));
        Ok(())
    }
    pub(crate) fn render_offline_diff(&mut self, viewport: Option<Viewport>, ctx: &Invocation) -> Result<View, Error> {
        let Some(root) = ctx.project().and_then(|p| p.root().ok()) else {
            return Ok(View::message("Open the owning project before reviewing an offline snapshot diff."));
        };
        if self.offline.running.as_ref().is_some_and(|r| r.plan.root != root)
            || self.offline.report.as_ref().is_some_and(|r| r.root != root)
        {
            return Ok(View::message("Offline diff state belongs to another project; return to its owning project."));
        }
        let body = if let Some(body) = generated::body(&self.offline, &root) {
            self.offline.visible = 0..0;
            body
        } else if let Some(report) = &self.offline.report {
            if let Some(body) = review::body(&self.offline, report) {
                self.offline.visible = 0..0;
                body
            } else if let Some(item) = self.offline.selected.and_then(|i| report.items.get(i)) {
                Body::Form(vec![
                    Field::new("kind", "CLI difference", FieldValue::readonly_text(&item.kind, false)),
                    Field::new("object", "CLI object spelling", FieldValue::readonly_text(&item.object, false)),
                    Field::new(
                        "detail",
                        "Selected report observation (not executable changes)",
                        FieldValue::readonly_text(&item.detail, true),
                    ),
                ])
            } else {
                let viewport = if ui_kit::web::is_rendering() {
                    let last_page = (report.items.len() as u32).saturating_sub(1) / 100;
                    self.offline.page = self.offline.page.min(last_page);
                    Some(Viewport { first_row: self.offline.page * 100, rows: 100 })
                } else { viewport.map(|v| Viewport { first_row: v.first_row, rows: v.rows.clamp(1, 100) }) };
                let range = Viewport::range(viewport, report.items.len() as u32, 50);
                self.offline.visible = range.clone();
                Body::Table(Table {
                    columns: vec!["CLI category".into(), "Object (reported spelling)".into()],
                    rows: report.items[range.start as usize..range.end as usize]
                        .iter()
                        .map(|i| vec![i.kind.clone(), i.object.clone()])
                        .collect(),
                    first_row: range.start,
                    total_rows: report.items.len() as u32,
                    editable: false,
                })
            }
        } else {
            Body::Message("No offline diff yet. Press Run Offline Diff… and pick two Liquibase JSON snapshots to compare.".into())
        };
        let provenance = if self.offline.generated_view && generated::owner(&self.offline) == Some(root.as_str()) {
            "Observed generated XML candidate; exact snapshot provenance above; no database apply.".into()
        } else { self
            .offline
            .report
            .as_ref()
            .map(|r| format!("{} · {}s old · {} selectable observations", r.source, r.at.elapsed().as_secs(), r.items.len()))
            .unwrap_or_default() };
        let table = matches!(&body, Body::Table(_));
        let running = self.offline.running.is_some();
        let mut view = View::new(body).status(&format!("{} · {provenance}", self.offline.status));
        view = if running {
            view.button(Button::new("offline-poll", "Refresh Status").icon("refresh"))
                .button(Button::new("offline-stop", "Stop Diff").icon("stop"))
        } else {
            view.button(Button::new("offline-run", "Run Offline Diff…").icon("run"))
                .button(Button::new("offline-choose", "Choose Differences to Preview…").icon("filter").enabled(self.offline.report.as_ref().is_some_and(|r| !r.items.is_empty())))
                .button(Button::new("offline-more", "More Actions…").icon("more"))
        };
        if self.offline.selected.is_some() || self.offline.review.is_some() || self.offline.generated_view {
            view = view.button(Button::new("offline-back", "Back to Report").icon("arrow-left"));
        }
        if table && ui_kit::web::is_rendering() {
            let total = self.offline.report.as_ref().map_or(0, |report| report.items.len() as u32);
            let visible = &self.offline.visible;
            view = view.button(Button::new("offline-previous", "Previous Observations").icon("arrow-left").enabled(visible.start > 0))
                .button(Button::new("offline-next", "Next Observations").icon("arrow-right").enabled(visible.end < total));
            view.status = Some(format!("{} · Observations {}–{} of {total}", view.status.as_deref().unwrap_or_default(), if visible.is_empty() {0} else {visible.start + 1}, visible.end));
        }
        Ok(view)
    }
    /// The less frequent actions, behind one toolbar button.
    fn more_offline_actions(&mut self, ctx: &Invocation, root: &str) -> Result<(), Error> {
        let mut items = vec![PickItem::new("offline-generate", "Generate Changelog from Snapshots…")];
        if generated::owner(&self.offline) == Some(root) {
            items.push(PickItem::new("offline-organize-generated", "Organize Migrations…"));
            items.push(if self.offline.generated_view {
                PickItem::new("offline-select-generated", "Choose Generated Changesets…")
            } else {
                PickItem::new("offline-show-generated", "Show Generated Changelog")
            });
        }
        let Some(key) = host::pick("More offline diff actions", &items)? else { return Ok(()) };
        self.handle_offline_diff(ViewEvent::Button(key), ctx)
    }
    pub(crate) fn handle_offline_diff(&mut self, event: ViewEvent, ctx: &Invocation) -> Result<(), Error> {
        let current_root = ctx.project().and_then(|p| p.root().ok()).ok_or(Error::Stale)?;
        if self.offline.running.as_ref().is_some_and(|r| r.plan.root != current_root) {
            return Err(Error::Stale);
        }
        let starts = matches!(&event,ViewEvent::Button(key)if key=="offline-run" || key=="offline-generate");
        if !starts && self.offline.report.as_ref().is_some_and(|r| r.root != current_root) {
            return Err(Error::Stale);
        }
        if let ViewEvent::Button(key) = &event {
            match key.as_str() {
                "offline-previous" | "offline-next" => {
                    let report = self.offline.report.as_ref().ok_or(Error::Stale)?;
                    if report.root != current_root || self.offline.selected.is_some() || self.offline.review.is_some() || self.offline.generated_view { return Err(Error::Stale); }
                    let last_page = (report.items.len() as u32).saturating_sub(1) / 100;
                    self.offline.page = if key == "offline-next" { self.offline.page.saturating_add(1).min(last_page) }
                        else { self.offline.page.saturating_sub(1) };
                    self.offline.visible = 0..0;
                }
                "offline-run" => return self.start_offline_diff(ctx),
                "offline-generate" => return self.start_offline_generation(ctx),
                "offline-select-generated" => return self.choose_generated_changesets(ctx),
                "offline-organize-generated" => return self.organize_generated_changesets(ctx),
                "offline-show-generated" => {
                    if generated::owner(&self.offline) != Some(current_root.as_str()) { return Err(Error::Stale); }
                    self.offline.generated_view = true;
                },
                "offline-poll" => return self.poll_offline_diff(ctx),
                "offline-more" => return self.more_offline_actions(ctx, &current_root),
                "offline-choose" => return self.choose_offline_observations(ctx),
                "offline-stop" => {
                    if let Some(run) = &self.offline.running {
                        if ctx.project().and_then(|p| p.root().ok()).as_deref() != Some(run.plan.root.as_str()) {
                            return Err(Error::Stale);
                        }
                        run.child.kill()?;
                        cleanup(&run.plan);
                    }
                    self.offline.running = None;
                    self.offline.status = "Offline diff stopped; previous completed report retained.".into();
                }
                "offline-back" => {
                    if self
                        .offline
                        .report
                        .as_ref()
                        .is_some_and(|r| ctx.project().and_then(|p| p.root().ok()).as_deref() != Some(r.root.as_str()))
                    {
                        return Err(Error::Stale);
                    }
                    self.offline.selected = None;
                    self.offline.review = None;
                    self.offline.generated_view = false;
                }
                _ => {}
            }
        }
        if let Some(row) = event.table_activate() {
            let report = self.offline.report.as_ref().ok_or(Error::Stale)?;
            if ctx.project().and_then(|p| p.root().ok()).as_deref() != Some(report.root.as_str())
                || !self.offline.visible.contains(&row)
                || row as usize >= report.items.len()
            {
                return Err(Error::Stale);
            }
            self.offline.review = None;
            self.offline.selected = Some(row as usize);
        }
        ui_kit::web::refresh_view(VIEW, include_str!("ui_config.json"));
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    const REPORT: &str = "Reference Database: private-jdbc-url\nComparison Database: offline:h2\nProduct Name: EQUAL\nMissing Table(s): \n     CUSTOMER\nUnexpected Table(s): NONE\nChanged Table(s): \n     ACCOUNT\n          name changed\n";
    #[test]
    fn official_4292_h2_offline_output_and_snapshots_match_the_adapter_contract() {
        let host = MockHost::install();
        host.add_file("reference.json", include_str!("../tests/fixtures/liquibase-4.29.2/reference.json"));
        host.add_file("target.json", include_str!("../tests/fixtures/liquibase-4.29.2/target.json"));
        let project = host.invocation().project().unwrap();
        let reference = input(&project, "/project", "/project/reference.json").unwrap();
        let target = input(&project, "/project", "/project/target.json").unwrap();
        assert_eq!(reference.db, "h2");
        assert_eq!(target.db, "h2");
        let items = parse(include_str!("../tests/fixtures/liquibase-4.29.2/diff.txt"), "tables,columns,indexes,views").unwrap();
        assert!(items.iter().any(|item| item.kind == "Missing Table(s)" && item.object == "CUSTOMER"));
        assert!(items.iter().any(|item| item.kind == "Unexpected Table(s)" && item.object == "LEGACY"));
        assert!(items.iter().any(|item| item.kind == "Missing Column(s)" && item.object == "PUBLIC.CUSTOMER.NAME"));
        assert_eq!(items.len(), 7);
        let log = crate::generated_changelog::Changelog::parse(include_str!("../tests/fixtures/liquibase-4.29.2/generated.xml")).unwrap();
        assert_eq!(log.len(), 3);
        assert!(log.author_matches("parity"));
        let selected = log.subset(&BTreeSet::from([0, 1])).unwrap();
        assert!(selected.contains("CUSTOMER_NAME") && !selected.contains("dropTable"));
    }

    fn snapshot() -> String {
        r#"{"snapshot":{"database":{"shortName":"h2","url":"private-jdbc-url","user":"private-user-value","password":"private-password-value"},"objects":{}}}"#.into()
    }
    fn setup() -> (MockHost, Liquibase) {
        let host = MockHost::install().with_capabilities(&["project.read", "process:liquibase", "storage"]);
        host.add_file("ref.json", &snapshot());
        host.add_file("target.json", &snapshot());
        (host, Liquibase::default())
    }
    // SDK MockHost::start uses a built-in controllable Child, not on_process.
    fn finished(l: &mut Liquibase, output: &str, status: i32) {
        let run = l.offline.running.as_mut().unwrap();
        if status != 0 {
            run.child.kill().unwrap();
        }
        run.child.wait(1).unwrap();
        run.output = output.into();
        run.bytes = output.len();
        run.status = Some(status);
    }
    fn answers(host: &MockHost) {
        for answer in ["0", "1", "tables"] {
            host.answer_next_prompt(Some(&[answer]));
        }
        host.answer_next_prompt(Some(&[]));
    }
    #[test]
    fn custom_group_report_pages_keep_absolute_observation_identity_and_back_position() {
        use serde_json::{json,Value};
        fn request(plugin:&mut Liquibase,host:&MockHost,mut value:Value)->Value {
            value["channel"]=json!("ui-kit");value["sequence"]=json!(1);value["project"]=json!("/project");
            plugin.handle("changesets",ViewEvent::Message(value.to_string()),&host.invocation()).unwrap();
            serde_json::from_str(&host.view_messages().last().unwrap().1).unwrap()
        }
        let host=MockHost::install().with_capabilities(&["storage","project.read"]);let mut plugin=Liquibase::default();
        plugin.offline.report=Some(Report{root:"/project".into(),source:"captured fixture".into(),at:Instant::now(),
            items:(0..250).map(|i|Item{kind:"Table".into(),object:format!("table_{i}"),detail:format!("observation_{i}")}).collect()});
        ui_kit::web::reveal_view(VIEW,include_str!("ui_config.json")).unwrap();
        assert_eq!(host.revealed_views(),["changesets"]);
        let first=request(&mut plugin,&host,json!({"op":"render"}));assert_eq!(first["model"]["body"]["firstRow"],0);
        let next=request(&mut plugin,&host,json!({"op":"event","targetView":VIEW,"event":{"kind":"button","key":"offline-next"}}));
        assert_eq!(next["model"]["body"]["firstRow"],100);
        let old=request(&mut plugin,&host,json!({"op":"event","targetView":VIEW,"event":{"kind":"table-activate","row":0}}));assert!(old.get("error").is_some());
        let opened=request(&mut plugin,&host,json!({"op":"event","targetView":VIEW,"event":{"kind":"table-activate","row":100}}));
        assert!(opened.get("error").is_none());assert!(opened["model"]["body"].to_string().contains("table_100"));
        let back=request(&mut plugin,&host,json!({"op":"event","targetView":VIEW,"event":{"kind":"button","key":"offline-back"}}));
        assert_eq!(back["model"]["body"]["firstRow"],100);
        assert!(host.process_runs().is_empty()&&host.requests().is_empty());
    }

    #[test]
    fn foreign_packaged_browser_render_does_not_destroy_owner_cli_lease() {
        let (host,mut plugin)=setup();answers(&host);plugin.start_offline_diff(&host.invocation()).unwrap();
        let staged=plugin.offline.running.as_ref().unwrap().plan.staged.clone();
        host.set_project_root(Some("/other"));
        plugin.render("changesets",None,&host.invocation()).unwrap();
        plugin.render_offline_diff(None,&host.invocation()).unwrap();
        let run=plugin.offline.running.as_ref().expect("foreign rendering must preserve owned child");
        assert_eq!(run.child.poll(4096).unwrap().status,None);
        for (path,text) in &staged {assert_eq!(host::fs::read_bytes(path).unwrap(),text.as_bytes());}
        host.set_project_root(None);plugin.render("changesets",None,&host.invocation()).unwrap();
        assert!(plugin.offline.running.is_some());
    }

    #[test]
    fn duplicate_snapshot_keys_are_not_silently_reinterpreted_by_staging() {
        for text in [
            snapshot().replace("\"shortName\":\"h2\"", "\"shortName\":\"mysql\",\"shortName\":\"h2\""),
            snapshot().replace("\"shortName\":\"h2\"", "\"short\\u004Eame\":\"mysql\",\"shortName\":\"h2\""),
            snapshot().replace("\"objects\":{}", "\"objects\":{\"liquibase.structure.core.Table\":[]},\"objects\":{}"),
            snapshot().replace("\"objects\":{}", "\"objects\":{\"liquibase.structure.core.Table\":[{\"table\":{\"name\":\"FIRST\",\"name\":\"SECOND\"}}]}"),
        ] {
            let (host, plugin) = setup();
            host.add_file("ref.json", &text);
            let project = host.invocation().project().unwrap();
            assert!(input(&project, "/project", "/project/ref.json").is_err(), "duplicate keys must not become an accepted offline input");
            assert!(plugin.offline.running.is_none());
            assert!(host.process_runs().is_empty());
        }
    }
    #[test]
    fn explicit_cli_arguments_private_current_buffer_copies_and_selectable_output() {
        let (host, mut l) = setup();
        answers(&host);
        l.start_offline_diff(&host.invocation()).unwrap();
        assert!(l.offline.running.is_some());
        let run = l.offline.running.as_ref().unwrap();
        assert!(run.plan.args.iter().any(|arg| arg.starts_with("--reference-url=offline:h2?snapshot=lb-diff-")));
        assert!(!format!("{:?}", run.plan.args).contains("private-password"));
        let command = offline_command(&run.plan).unwrap();
        assert_eq!(command.env.len(), 3);
        assert!(command.env.iter().any(|(key, value)| key == "LIQUIBASE_COMMAND_DIFF_URL" && value.starts_with("offline:h2?snapshot=lb-diff-")));
        assert!(command.env.iter().any(|(key, value)| key == "LIQUIBASE_COMMAND_DIFF_REFERENCE_URL" && value.starts_with("offline:h2?snapshot=lb-diff-")));

        let staged = run.plan.staged.clone();
        for (path, text) in &staged {
            assert_eq!(host::fs::read_bytes(path).unwrap(), text.as_bytes());
            assert!(!text.contains("private-jdbc"));
            assert!(!text.contains("private-password"));
        }
        finished(&mut l, REPORT, 0);
        l.poll_offline_diff(&host.invocation()).unwrap();
        assert!(l.offline.running.is_none());
        let report = l.offline.report.as_ref().unwrap();
        assert_eq!(report.items.len(), 2);
        assert_eq!(report.items[1].object, "ACCOUNT");
        assert!(!report.source.contains("private-jdbc"));
        for (path, _) in staged {
            assert!(host::fs::read_bytes(&path).unwrap().is_empty());
        }
        let view = l.render_offline_diff(None, &host.invocation()).unwrap();
        assert!(matches!(view.body, Body::Table(_)));
        l.handle_offline_diff(ViewEvent::Other { kind: "table-activate".into(), data: "{\"row\":1}".into() }, &host.invocation()).unwrap();
        assert!(matches!(l.render_offline_diff(None, &host.invocation()).unwrap().body, Body::Form(_)));
    }
    #[test]
    fn cancel_at_every_prompt_has_no_process_or_staged_snapshot_write() {
        for cancelled in 0..4 {
            let (host, mut l) = setup();
            for answer in ["0", "1", "tables"].iter().take(cancelled.min(3)) {
                host.answer_next_prompt(Some(&[*answer]));
            }
            host.answer_next_prompt(None);
            l.start_offline_diff(&host.invocation()).unwrap();
            assert!(host.process_runs().is_empty());
            assert!(host::fs::read_dir(&host::fs::plugin_dir().unwrap()).unwrap().is_empty());
        }
    }
    #[test]
    fn changed_staged_input_is_not_truncated_after_ownership_check_rejects_it() {
        let (host, mut plugin) = setup();
        answers(&host);
        plugin.start_offline_diff(&host.invocation()).unwrap();
        let staged = plugin.offline.running.as_ref().unwrap().plan.staged.clone();
        host::fs::write_bytes(&staged[0].0, b"REPLACED-PRIVATE-SOURCE").unwrap();
        assert!(matches!(plugin.poll_offline_diff(&host.invocation()), Err(Error::Stale)));
        assert!(plugin.offline.running.is_none() && plugin.offline.report.is_none());
        assert_eq!(host::fs::read_bytes(&staged[0].0).unwrap(), b"REPLACED-PRIVATE-SOURCE");
        for (path, _) in staged.iter().skip(1) { assert!(host::fs::read_bytes(path).unwrap().is_empty()); }
        assert!(host.process_runs().is_empty() && host.requests().is_empty());
    }
    #[test]
    fn changed_sources_staging_or_scope_cannot_publish_new_report() {
        for change in ["source", "staging", "scope", "deadline"] {
            let (host, mut l) = setup();
            answers(&host);
            l.start_offline_diff(&host.invocation()).unwrap();
            let staged = l.offline.running.as_ref().unwrap().plan.staged.clone();
            match change {
                "source" => host.add_file("target.json", "{}"),
                "staging" => host::fs::write_bytes(&staged[0].0, b"changed").unwrap(),
                "scope" => host.set_project_root(Some("/other")),
                _ => l.offline.running.as_mut().unwrap().at = Instant::now() - Duration::from_secs(121),
            };
            assert!(l.poll_offline_diff(&host.invocation()).is_err(), "{change}");
            assert!(l.offline.report.is_none());
            if change == "scope" {
                assert!(l.offline.running.is_some(), "foreign root cannot kill owner child");
            } else {
                assert!(l.offline.running.is_none());
                for (index, (path, _)) in staged.into_iter().enumerate() {
                    let bytes = host::fs::read_bytes(&path).unwrap();
                    if change == "staging" && index == 0 { assert_eq!(bytes, b"changed"); }
                    else { assert!(bytes.is_empty()); }
                }
            }
        }
    }
    #[test]
    fn unsupported_exit_shape_and_output_caps_preserve_last_good() {
        for mode in ["exit", "shape", "cap"] {
            let (host, mut l) = setup();
            answers(&host);
            l.start_offline_diff(&host.invocation()).unwrap();
            finished(&mut l, REPORT, 0);
            l.poll_offline_diff(&host.invocation()).unwrap();
            let prior = l.offline.report.as_ref().unwrap().items.len();
            answers(&host);
            l.start_offline_diff(&host.invocation()).unwrap();
            let output = match mode {
                "shape" => "UNKNOWN FORMAT".into(),
                "cap" => "x".repeat(MAX_OUTPUT + 1),
                _ => "secret stderr".into(),
            };
            finished(&mut l, &output, if mode == "exit" { 1 } else { 0 });
            for _ in 0..4 {
                l.poll_offline_diff(&host.invocation()).unwrap();
            }
            assert!(l.offline.running.is_none());
            assert_eq!(l.offline.report.as_ref().unwrap().items.len(), prior);
            assert!(!l.offline.status.contains("private-password"));
        }
    }
    #[test]
    fn toolbar_shows_only_the_actions_of_the_current_state_with_icons() {
        let (host, mut l) = setup();
        let keys = |l: &mut Liquibase| l.render_offline_diff(None, &host.invocation()).unwrap().toolbar.iter().map(|b| (b.id.clone(), b.icon.is_some())).collect::<Vec<_>>();
        assert_eq!(keys(&mut l), [("offline-run".into(), true), ("offline-choose".into(), true), ("offline-more".into(), true)]);
        answers(&host);
        l.start_offline_diff(&host.invocation()).unwrap();
        assert_eq!(keys(&mut l), [("offline-poll".into(), true), ("offline-stop".into(), true)]);
        l.handle_offline_diff(ViewEvent::Button("offline-stop".into()), &host.invocation()).unwrap();
        host.answer_next_prompt(None);
        l.handle_offline_diff(ViewEvent::Button("offline-more".into()), &host.invocation()).unwrap();
        assert!(l.offline.running.is_none());
    }
    #[test]
    fn stopping_requires_owner_and_cleans_staging() {
        let (host, mut l) = setup();
        answers(&host);
        l.start_offline_diff(&host.invocation()).unwrap();
        let staged = l.offline.running.as_ref().unwrap().plan.staged.clone();
        host.set_project_root(Some("/other"));
        assert!(l.handle_offline_diff(ViewEvent::Button("offline-stop".into()), &host.invocation()).is_err());
        host.set_project_root(Some("/project"));
        l.handle_offline_diff(ViewEvent::Button("offline-stop".into()), &host.invocation()).unwrap();
        assert!(l.offline.running.is_none());
        for (path, _) in staged {
            assert!(host::fs::read_bytes(&path).unwrap().is_empty());
        }
    }
    #[test]
    fn parser_rejects_partial_duplicate_unknown_and_oversized_observations() {
        assert_eq!(parse(REPORT, "tables").unwrap().len(), 2);
        for output in [
            "Missing Table(s): NONE",
            &format!("{REPORT}Missing Table(s): NONE"),
            &REPORT.replace("Missing Table(s)", "Missing Trigger(s)"),
            &REPORT.replace("     CUSTOMER\n", ""),
            &"x".repeat(MAX_OUTPUT + 1),
        ] {
            assert!(parse(output, "tables").is_err());
        }
        assert!(parse(REPORT, "tables,indexes").is_err());
        assert!(parse(&REPORT.replace("NONE", "N\0ONE"), "tables").is_err());
    }
    #[test]
    fn path_and_kind_validation_unknown_snapshots_and_private_staging_cap() {
        let (host, mut l) = setup();
        let project = host.invocation().project().unwrap();
        for path in ["/project/../ref.json", "/project/name?x.json", "/other/ref.json"] {
            assert!(input(&project, "/project", path).is_err());
        }
        let reference = input(&project, "/project", "/project/ref.json").unwrap();
        let mut target = reference.clone();
        target.path = "target.json".into();
        assert!(args(&reference, &target, "triggers").is_err());
        target.db = "mysql".into();
        assert!(args(&reference, &target, "tables").is_err());
        let dir = host::fs::plugin_dir().unwrap();
        for i in 0..96 {
            host::fs::write_bytes(&format!("{dir}/lb-diff-existing-{i}"), b"").unwrap();
        }
        answers(&host);
        assert!(l.start_offline_diff(&host.invocation()).is_err());
        assert!(host.process_runs().is_empty());
    }
    #[test]
    fn render_and_selection_never_start_cli_or_expose_another_project() {
        let (host, mut l) = setup();
        l.render_offline_diff(None, &host.invocation()).unwrap();
        assert!(host.process_runs().is_empty());
        answers(&host);
        l.start_offline_diff(&host.invocation()).unwrap();
        finished(&mut l, REPORT, 0);
        l.poll_offline_diff(&host.invocation()).unwrap();
        let runs = host.process_runs().len();
        host.set_project_root(Some("/other"));
        assert!(matches!(l.render_offline_diff(None, &host.invocation()).unwrap().body, Body::Message(_)));
        assert!(
            l.handle_offline_diff(ViewEvent::Other { kind: "table-activate".into(), data: "{\"row\":0}".into() }, &host.invocation())
                .is_err()
        );
        assert_eq!(host.process_runs().len(), runs);
    }
    #[test]
    fn unsaved_snapshot_bytes_are_staged_instead_of_unreviewed_disk() {
        let (host, mut l) = setup();
        let unsaved = snapshot()
            .replace("\"objects\":{}", "\"objects\":{\"liquibase.structure.core.Table\":[{\"table\":{\"name\":\"editor-only-marker\"}}]}");
        host.open_editor("/project/ref.json", "json", &unsaved, &[]);
        answers(&host);
        l.start_offline_diff(&host.invocation()).unwrap();
        let plan = &l.offline.running.as_ref().unwrap().plan;
        assert!(plan.reference.text.contains("editor-only-marker"));
        assert!(plan.staged[0].1.contains("editor-only-marker"));
        assert!(!plan.staged[0].1.contains("private-password"));
        let (_, first) = stage_plan(&plan.root, &plan.reference, &plan.target, &plan.kind).unwrap();
        assert_eq!(first, plan.staged);
    }
    #[test]
    fn competing_staging_is_not_overwritten_and_exact_success_footer_is_supported() {
        let (host, mut first) = setup();
        answers(&host);
        first.start_offline_diff(&host.invocation()).unwrap();
        let staged = first.offline.running.as_ref().unwrap().plan.staged.clone();
        let mut other = Liquibase::default();
        answers(&host);
        assert!(other.start_offline_diff(&host.invocation()).is_err());
        for (path, text) in &staged {
            assert_eq!(host::fs::read_bytes(path).unwrap(), text.as_bytes());
        }
        assert_eq!(parse(&format!("{REPORT}Liquibase command 'diff' was executed successfully.\n"), "tables").unwrap().len(), 2);
    }
}
