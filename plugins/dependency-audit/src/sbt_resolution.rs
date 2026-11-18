//! Reviewed sbt2.0.12 effective UpdateReport adapter; never used by editor providers.
use crate::*;
use openidea_plugin::host::{Child, Command};
use openidea_plugin::views::{Field, FieldValue};
use std::collections::{BTreeMap, BTreeSet};
pub(crate) const VIEW: &str = "sbt-resolution";
const EXPORT: &str = include_str!("sbt-export.command");
const VERSION: &str = "2.0.12";
struct Run {
    root: String,
    before: BTreeMap<String, String>,
    output: String,
    child: Option<Child>,
    status: Option<i32>,
}
#[derive(Default)]
pub(crate) struct State {
    run: Option<Run>,
    message: String,
    log: String,
}
pub(crate) struct Evidence {
    root: String,
    before: BTreeMap<String, String>,
    deps: Vec<Dep>,
    provenance: HashMap<String, String>,
    modules: usize,
}
fn saved(root: &str) -> Result<(), Error> {
    if host::open_editors()?
        .iter()
        .any(|e| e.dirty && e.path.starts_with(&format!("{root}/")))
    {
        return Err("Save project buffers before resolving/importing sbt's saved build.".into());
    }
    Ok(())
}
fn snapshot(p: &Project, root: &str) -> Result<BTreeMap<String, String>, Error> {
    let mut paths = BTreeSet::new();
    for glob in [
        "**/*.sbt",
        "**/project/**/*.scala",
        "**/project/build.properties",
        "**/.sbtopts",
        "**/.jvmopts",
    ] {
        let found = p.find_files(glob, 501)?;
        if found.len() >= 501 {
            return Err("sbt build-source discovery bound reached.".into());
        }
        paths.extend(
            found
                .into_iter()
                .filter(|s| !s.contains("/target/") && !s.contains("/.openidea/")),
        );
    }
    for relative in [
        "build.sbt",
        "project/build.properties",
        ".sbtopts",
        ".jvmopts",
    ] {
        let path = format!("{root}/{relative}");
        if p.exists(&path)? {
            paths.insert(path);
        }
    }
    let mut before = BTreeMap::new();
    let mut bytes = 0;
    for path in paths {
        if !owned_source_path(root, &path) {
            return Err(Error::Stale);
        }
        let text = p.read_file(&path)?;
        bytes += text.len();
        if bytes > 4 << 20 || before.len() >= 500 {
            return Err("sbt sources exceed 500 files or 4 MiB.".into());
        }
        before.insert(path, text);
    }
    Ok(before)
}
fn current(r: &Run, ctx: &Invocation) -> Result<Project, Error> {
    let p = ctx.project().ok_or(Error::Stale)?;
    if p.root()? != r.root {
        return Err(Error::Stale);
    }
    saved(&r.root)?;
    if snapshot(&p, &r.root)? != r.before {
        return Err("sbt build inputs changed; rerun before auditing.".into());
    }
    Ok(p)
}
fn declared_version(text: &str) -> bool {
    let values: Vec<_> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#') && !l.starts_with('!'))
        .filter_map(|l| {
            l.strip_prefix("sbt.version")
                .and_then(|v| v.trim_start().strip_prefix('='))
        })
        .collect();
    values.len() == 1 && values[0].trim() == VERSION
}
fn decode(s: &str) -> Result<String, Error> {
    if s.len() > 8192 || s.len() % 2 != 0 {
        return Err("Unsupported sbt export field".into());
    }
    let bytes = s
        .as_bytes()
        .chunks_exact(2)
        .map(|b| {
            let h = (b[0] as char).to_digit(16).ok_or("Invalid hex")?;
            let l = (b[1] as char).to_digit(16).ok_or("Invalid hex")?;
            Ok((h * 16 + l) as u8)
        })
        .collect::<Result<Vec<_>, Error>>()?;
    let s = String::from_utf8(bytes).map_err(|_| Error::Invalid("Non-UTF8 sbt export".into()))?;
    if s.chars().any(char::is_control) {
        return Err("Control-bearing sbt export field".into());
    }
    Ok(s)
}
fn parse(root: &str, text: &str) -> Result<Evidence, Error> {
    if text.len() > 4 << 20 {
        return Err("sbt export exceeds 4MiB.".into());
    }
    let mut lines = text.lines();
    let header: Vec<_> = lines
        .next()
        .ok_or("Missing sbt header")?
        .split('\t')
        .collect();
    if header.len() != 4
        || header[..3] != ["OPENIDEA-SBT", "1", VERSION]
        || decode(header[3])? != root
    {
        return Err("Foreign or unsupported sbt export header.".into());
    }
    let mut modules = BTreeMap::new();
    let mut records = Vec::new();
    let mut internal = HashSet::new();
    let mut seen = HashSet::new();
    let mut footer = None;
    for line in lines {
        if footer.is_some() {
            return Err("Trailing data after sbt completion marker.".into());
        }
        let fields: Vec<_> = line.split('\t').collect();
        match fields.as_slice() {
            ["M", id, path, group, name, version] => {
                let identity = (decode(group)?, decode(name)?, decode(version)?);
                if [&identity.0, &identity.1, &identity.2].iter().any(|s| {
                    s.is_empty()
                        || s.len() > 512
                        || s.contains([':', '/', '\\', ' ', '$', '[', ']'])
                }) {
                    return Err("Unsupported sbt project identity".into());
                }
                internal.insert(identity);
                let id = decode(id)?;
                let relative = decode(path)?;
                if id.is_empty()
                    || relative.starts_with(['/', '\\'])
                    || relative.split(['/', '\\']).any(|x| matches!(x, "." | ".."))
                    || relative.contains(':')
                    || modules.len() >= 500
                    || modules.insert(id, relative).is_some()
                {
                    return Err("Invalid/duplicate/foreign sbt module".into());
                }
            }
            ["D", module, configuration, group, name, version] => {
                let tuple = (
                    decode(module)?,
                    decode(configuration)?,
                    decode(group)?,
                    decode(name)?,
                    decode(version)?,
                );
                if tuple.0.is_empty()
                    || tuple.1.is_empty()
                    || [&tuple.2, &tuple.3, &tuple.4].iter().any(|s| {
                        s.is_empty()
                            || s.len() > 512
                            || s.contains([':', '/', '\\', ' ', '$', '[', ']'])
                    })
                    || !seen.insert(tuple.clone())
                    || records.len() >= 10000
                {
                    return Err("Unsupported or duplicate sbt coordinate.".into());
                }
                records.push(tuple);
            }
            ["END", m, d] => {
                footer = Some((
                    m.parse::<usize>()
                        .map_err(|_| Error::Invalid("Invalid sbt module count".into()))?,
                    d.parse::<usize>()
                        .map_err(|_| Error::Invalid("Invalid sbt dependency count".into()))?,
                ));
            }
            _ => return Err("Malformed/truncated sbt export; no partial audit.".into()),
        }
    }
    if modules.is_empty() || footer != Some((modules.len(), records.len())) {
        return Err("Incomplete sbt export completion proof.".into());
    }
    let mut deps = Vec::new();
    let mut provenance: HashMap<String, String> = HashMap::new();
    let path = format!("{root}/build.sbt");
    for (module, configuration, group, name, version) in records {
        let relative = modules
            .get(&module)
            .ok_or("sbt dependency belongs to unknown module")?;
        if internal.contains(&(group.clone(), name.clone(), version.clone())) {
            continue;
        }
        let package = format!("{group}:{name}");
        let key = format!("{package} {version}");
        let text = format!(
            "Resolved sbt 2.0.12 update; module {module} ({}); configuration {configuration}. Root build.sbt location, not an exact declaration span.",
            if relative.is_empty() {
                "project root"
            } else {
                relative
            }
        );
        if let Some(old) = provenance.get_mut(&key) {
            if old.len() + text.len() > 16384 {
                return Err("sbt provenance exceeds bound".into());
            }
            old.push_str("; ");
            old.push_str(&text);
        } else {
            provenance.insert(key, text);
            deps.push(Dep {
                path: path.clone(),
                ecosystem: manifest::Ecosystem::Maven,
                name: package,
                version,
                at: (0, 0),
                version_at: None,
            });
        }
    }
    Ok(Evidence {
        root: root.into(),
        before: BTreeMap::new(),
        deps,
        provenance,
        modules: modules.len(),
    })
}
impl State {
    pub(crate) fn start(&mut self, ctx: &Invocation) -> Result<(), Error> {
        if self.run.as_ref().is_some_and(|r| r.child.is_some()) {
            return Err("Complete or cancel the active sbt resolver first.".into());
        }
        let p = ctx.project().ok_or("Open an sbt project first")?;
        let root = p.root()?;
        if root.chars().any(char::is_control) {
            return Err(Error::Stale);
        }
        if !p.exists(&format!("{root}/build.sbt"))?
            || !declared_version(&p.read_file(&format!("{root}/project/build.properties"))?)
        {
            return Err("This profile requires root build.sbt and project/build.properties declaring exactly sbt.version=2.0.12.".into());
        }
        saved(&root)?;
        let before = snapshot(&p, &root)?;
        if !host::confirm(
            "Resolve sbt dependencies",
            &format!(
                "Project: {root}\nApproved official sbt 2.0.12 runner/runtime, Java 17+\n\nAfter this review the runner version is probed, then a managed --server --batch session loads this executable Scala build and plugins. It may execute arbitrary configured tasks, contact configured repositories and update normal sbt/build caches. Project/global JVM/sbt options and external build inputs are not fully captured by the bounded snapshot. No project wrapper is invoked and no build definition is modified.\n\nAn owned temporary command resolves every owned project ref's update report and exports non-evicted modules across its reported configurations. External project bases refuse export. Poll/cancel the owned child, then explicitly Audit resolved dependencies to send coordinates to OSV. Failed/incomplete/stale exports retain the previous report."
            ),
        )? {
            return Ok(());
        }
        let check = Run {
            root: root.clone(),
            before: before.clone(),
            output: String::new(),
            child: None,
            status: None,
        };
        current(&check, ctx)?;
        let version = p
            .run_process("sbt", &["--script-version".into()], None)
            .map_err(|e| ui_kit::denied::map(e, "sbt dependency resolution", "process:sbt", "Dependency Audit"))?;
        let found = version.stdout.trim();
        if version.status != Some(0) || found != VERSION {
            return Err(Error::Invalid(if version.status == Some(0) && !found.is_empty() && found.len() <= 64 {
                format!("Found sbt {found}; needs sbt {VERSION}. Install that version and try again.")
            } else {
                format!("Couldn't read the sbt version; needs sbt {VERSION}.")
            }));
        }
        current(&check, ctx)?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Error::Stale)?
            .as_nanos();
        let dir = format!("{root}/.openidea/sbt-resolve-{stamp:x}");
        if p.exists(&dir)? {
            return Err(Error::Stale);
        }
        p.write_file(
            &format!("{dir}/review.txt"),
            "sbt2.0.12 owned update export",
        )?;
        let output = format!("{dir}/resolved.tsv");
        let args = vec![
            "--server".into(),
            "--batch".into(),
            "--no-colors".into(),
            format!("-Dopenidea.audit.root={root}"),
            format!("-Dopenidea.audit.output={output}"),
            EXPORT.into(),
            "openideaDependencyAuditExport".into(),
        ];
        current(&check, ctx)?;
        let child = host::start(&Command::new("sbt").args(&args), Some(&p))?;
        self.run = Some(Run {
            root,
            before,
            output,
            child: Some(child),
            status: None,
        });
        self.message = "Resolving all owned project refs. Poll for output/completion.".into();
        self.log.clear();
        crate::reveal(VIEW)?;
        crate::refresh(VIEW);
        Ok(())
    }
    pub(crate) fn poll(&mut self, ctx: &Invocation) -> Result<(), Error> {
        let r = self.run.as_mut().ok_or("Start an sbt resolver first")?;
        current(r, ctx)?;
        let Some(child) = r.child.as_ref() else {
            return Ok(());
        };
        let chunk = child.poll(65536)?;
        self.log.push_str(&chunk.stdout);
        self.log.push_str(&chunk.stderr);
        if self.log.len() > 32768 {
            let mut at = self.log.len() - 32768;
            while !self.log.is_char_boundary(at) {
                at += 1
            }
            self.log = self.log[at..].into()
        }
        if let Some(status) = chunk.status {
            r.status = Some(status);
            r.child = None;
            self.message = if status == 0 {
                "sbt completed. Audit resolved dependencies explicitly.".into()
            } else {
                format!("sbt failed ({status}); previous report retained.")
            }
        }
        crate::refresh(VIEW);
        Ok(())
    }
    pub(crate) fn cancel(&mut self, ctx: &Invocation) -> Result<(), Error> {
        let r = self.run.as_mut().ok_or("No sbt resolver")?;
        if ctx.project().and_then(|p| p.root().ok()).as_deref() != Some(&r.root) {
            return Err(Error::Stale);
        }
        if let Some(child) = r.child.as_ref() {
            child.kill()?
        }
        r.child = None;
        r.status = Some(-1);
        self.message = "sbt cancelled; previous report retained.".into();
        crate::refresh(VIEW);
        Ok(())
    }
    pub(crate) fn evidence(&self, ctx: &Invocation) -> Result<Evidence, Error> {
        let r = self
            .run
            .as_ref()
            .filter(|r| r.child.is_none() && r.status == Some(0))
            .ok_or("Wait for successful completed sbt resolution")?;
        let p = current(r, ctx)?;
        let mut e = parse(&r.root, &p.read_file(&r.output)?)?;
        e.before = r.before.clone();
        Ok(e)
    }
    pub(crate) fn render(&self, ctx: &Invocation) -> View {
        let Some(r) = self.run.as_ref() else {
            return View::message("Run Resolve sbt Dependencies to review a local build export.");
        };
        if ctx.project().and_then(|p| p.root().ok()).as_deref() != Some(&r.root) {
            return View::message(
                "Return to the owning project to inspect/cancel its sbt resolver.",
            );
        }
        View::new(Body::Form(vec![
            Field::new(
                "sbt-status",
                "Status",
                FieldValue::readonly_text(
                    &format!(
                        "{}\nOfficial sbt 2.0.12; all project modules and configurations\n{}",
                        self.message, r.output
                    ),
                    true,
                ),
            ),
            Field::new(
                "sbt-log",
                "Recent bounded output",
                FieldValue::readonly_text(&self.log, true),
            ),
        ]))
        .button(
            Button::new("sbt-poll", "Poll sbt")
                .icon("refresh")
                .enabled(r.child.is_some()),
        )
        .button(
            Button::new("sbt-cancel", "Cancel sbt")
                .icon("stop")
                .enabled(r.child.is_some()),
        )
        .button(
            Button::new("sbt-audit", "Audit Resolved Dependencies")
                .icon("check")
                .enabled(r.status == Some(0) && r.child.is_none()),
        )
    }
}
pub(crate) fn audit_evidence(project: &Project, e: Evidence) -> Result<(), Error> {
    saved(&e.root)?;
    if project.root()? != e.root || snapshot(project, &e.root)? != e.before {
        return Err(Error::Stale);
    }
    let findings = audit(&e.deps, true, now(), Instant::now() + CALL_BUDGET)?;
    let mut rows: Vec<Row> = findings
        .iter()
        .map(|f| Row {
            path: f.dep.path.clone(),
            line: 0,
            name: f.dep.name.clone(),
            version: f.dep.version.clone(),
            fix: f.fix(),
            advisories: f.advisories.iter().map(|a| a.id.clone()).collect(),
            ecosystem: Some("Maven".into()),
            sources: vec![Declaration {
                path: f.dep.path.clone(),
                line: 0,
                evidence: e
                    .provenance
                    .get(&format!("{} {}", f.dep.name, f.dep.version))
                    .cloned()
                    .unwrap_or_default(),
            }],
            details: f.advisories.clone(),
        })
        .collect();
    binding::annotate(&mut rows, &e.before.iter().map(|(k, v)| (k.clone(), v.clone())).collect());
    let report = Report {
        rows,
        scanned_at: Some(now()),
        files_scanned: Some(e.before.len()),
        files_skipped: 0,
        dependencies_checked: Some(e.deps.len()),
        checked_files: e.before.keys().cloned().collect(),
        discovery_limited: false,
        ..Report::default()
    };
    if project.root()? != e.root || snapshot(project, &e.root)? != e.before {
        return Err(Error::Stale);
    }
    saved(&e.root)?;
    persist_report(project, &report)?;
    refresh_report();
    crate::notify_project(
        project,
        NotificationLevel::Info,
        "sbt dependency audit",
        &format!(
            "Audited {} effective sbt coordinates from {} modules.",
            e.deps.len(),
            e.modules
        ),
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn official_effective_multi_module_export_preserves_property_transitives_and_configurations() {
        let h = openidea_plugin::mock::MockHost::install();
        h.add_file("/project/build.sbt", "fixture");
        let e = parse(
            "/project",
            include_str!("../tests/fixtures/sbt-resolved-2.0.12.tsv"),
        )
        .unwrap();
        assert_eq!(e.modules, 3);
        for (name, version) in [
            ("com.google.guava:guava", "32.1.3-jre"),
            ("com.google.guava:failureaccess", "1.0.1"),
            ("org.junit.jupiter:junit-jupiter-api", "5.10.2"),
        ] {
            assert!(
                e.deps
                    .iter()
                    .any(|d| d.name == name && d.version == version)
            );
        }
        assert!(
            e.deps
                .iter()
                .all(|d| !d.name.starts_with("fixture:") && d.version_at.is_none())
        );
        assert!(
            e.provenance
                .values()
                .any(|s| s.contains("configuration test"))
        );
    }
    #[test]
    fn completion_owner_counts_duplicate_and_path_proofs_are_required() {
        let h = openidea_plugin::mock::MockHost::install();
        h.add_file("/project/build.sbt", "fixture");
        let good = include_str!("../tests/fixtures/sbt-resolved-2.0.12.tsv");
        assert!(parse("/other", good).is_err());
        assert!(parse("/project", &good.replace("END\t3", "END\t4")).is_err());
        assert!(
            parse(
                "/project",
                &good
                    .lines()
                    .filter(|l| !l.starts_with("END"))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
            .is_err()
        );
        assert!(parse("/project", &format!("{good}M\t726f6f74\t\n")).is_err());
        assert!(
            parse(
                "/project",
                &good.replacen("M\t726f6f74\t", "M\t726f6f74\t2e2e2f65787465726e616c", 1)
            )
            .is_err()
        );
    }
    #[test]
    fn exact_declared_and_approved_runner_version_before_managed_export() {
        assert!(declared_version("# build\nsbt.version = 2.0.12\n"));
        assert!(!declared_version("sbt.version=2.0.12\nsbt.version=2.0.12"));
        assert!(!declared_version("sbt.version=1.12.0"));
        use openidea_plugin::{ProcessOutput, mock::MockHost};
        let h = MockHost::install().with_capabilities(&[
            "project.read",
            "project.write",
            "editor.read",
            "process:sbt",
        ]);
        h.add_file("/project/build.sbt", "fixture");
        h.add_file("/project/project/build.properties", "sbt.version=2.0.12");
        h.on_process("sbt", |args, _| {
            assert_eq!(args, &["--script-version"]);
            Ok(ProcessOutput {
                status: Some(0),
                stdout: "2.0.12\n".into(),
                stderr: String::new(),
            })
        });
        h.answer_next_prompt(Some(&["ok"]));
        let mut state = State::default();
        state.start(&h.invocation()).unwrap();
        let output = state.run.as_ref().unwrap().output.clone();
        h.add_file(
            &output,
            include_str!("../tests/fixtures/sbt-resolved-2.0.12.tsv"),
        );
        state
            .run
            .as_ref()
            .unwrap()
            .child
            .as_ref()
            .unwrap()
            .wait(1)
            .unwrap();
        state.poll(&h.invocation()).unwrap();
        assert_eq!(state.evidence(&h.invocation()).unwrap().modules, 3);
        h.add_file("/project/build.sbt", "drift");
        assert!(state.evidence(&h.invocation()).is_err());
    }
    #[test]
    fn a_wrong_sbt_runner_says_what_was_found() {
        use openidea_plugin::{ProcessOutput, mock::MockHost};
        let h = MockHost::install().with_capabilities(&["project.read", "project.write", "editor.read", "process:sbt"]);
        h.add_file("/project/build.sbt", "fixture");
        h.add_file("/project/project/build.properties", "sbt.version=2.0.12");
        h.on_process("sbt", |_, _| Ok(ProcessOutput { status: Some(0), stdout: "1.10.0\n".into(), stderr: String::new() }));
        h.answer_next_prompt(Some(&["ok"]));
        let error = State::default().start(&h.invocation()).unwrap_err().to_string();
        assert!(error.contains("Found sbt 1.10.0; needs sbt 2.0.12"), "{error}");
    }
    #[test]
    fn review_cancellation_never_invokes_sbt() {
        let h = openidea_plugin::mock::MockHost::install();
        h.add_file("/project/build.sbt", "fixture");
        h.add_file("/project/project/build.properties", "sbt.version=2.0.12");
        h.answer_next_prompt(None);
        let mut state = State::default();
        state.start(&h.invocation()).unwrap();
        assert!(h.process_runs().is_empty() && state.run.is_none());
    }
    #[test]
    fn resolved_sbt_coordinates_enter_osv_report_and_failures_keep_prior_report() {
        use openidea_plugin::{HttpResponse, mock::MockHost};
        let h = MockHost::install().with_capabilities(&[
            "project.read",
            "editor.read",
            "storage",
            "net:api.osv.dev",
        ]);
        h.add_file("/project/build.sbt", "fixture");
        h.add_file("/project/project/build.properties", "sbt.version=2.0.12");
        let p = host::projects().unwrap().remove(0);
        // Keep the real update graph; fixture OSV responder covers its package coordinates.
        h.on_fetch(crate::tests::osv);
        let mut e = parse(
            "/project",
            include_str!("../tests/fixtures/sbt-resolved-2.0.12.tsv"),
        )
        .unwrap();
        e.before = snapshot(&p, "/project").unwrap();
        audit_evidence(&p, e).unwrap();
        let original = host::storage::get(&rows_key(&p).unwrap()).unwrap();
        h.on_fetch(|_| {
            Ok(HttpResponse {
                status: 503,
                headers: vec![],
                body: vec![],
            })
        });
        let mut e = parse(
            "/project",
            include_str!("../tests/fixtures/sbt-resolved-2.0.12.tsv"),
        )
        .unwrap();
        e.before = snapshot(&p, "/project").unwrap();
        // New unique coordinate prevents the previous query cache from satisfying a failed rescan.
        e.deps[0].version = "987.654.321".into();
        assert!(audit_evidence(&p, e).is_err());
        assert_eq!(
            host::storage::get(&rows_key(&p).unwrap()).unwrap(),
            original
        );
    }
}
