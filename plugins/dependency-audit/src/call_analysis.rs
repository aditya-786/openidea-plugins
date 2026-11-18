//! Optional, reviewed OSV-Scanner call facts. Never invoked by editor providers.
use crate::*;
use openidea_plugin::host::{Child, Command};
use openidea_plugin::views::{Field, FieldValue};
use std::collections::{BTreeMap, BTreeSet};
pub(crate) const VIEW: &str = "call-analysis";
#[derive(Clone, Copy, Serialize, Deserialize)]
enum Language {
    Go,
    Rust,
    RustMacho,
}
impl Language {
    fn program(self) -> &'static str {
        match self {
            Self::RustMacho => "osv-scanner-macho",
            _ => "osv-scanner",
        }
    }
    fn version(self) -> &'static str {
        match self {
            Self::RustMacho => "2.7.1-openidea-macho.1",
            _ => "2.7.1",
        }
    }
    fn profile(self) -> &'static str {
        match self {
            Self::Go => "Official Go/govulncheck",
            Self::Rust => "Official Rust ELF (experimental; native Darwin failed)",
            Self::RustMacho => {
                "Source-built OpenIDEA Mach-O/dSYM patch (experimental, not unchanged official binary)"
            }
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::Go => "go",
            Self::Rust | Self::RustMacho => "rust",
        }
    }
    fn other(self) -> &'static str {
        match self {
            Self::Go => "rust",
            Self::Rust | Self::RustMacho => "go",
        }
    }
    fn lock(self) -> &'static str {
        match self {
            Self::Go => "go.mod",
            Self::Rust | Self::RustMacho => "Cargo.lock",
        }
    }
    fn ecosystem(self) -> &'static str {
        match self {
            Self::Go => "Go",
            Self::Rust | Self::RustMacho => "crates.io",
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
struct Fact {
    source: String,
    package: String,
    version: String,
    advisory: String,
    status: String,
}
#[derive(Clone, Serialize, Deserialize)]
struct Evidence {
    root: String,
    language: Language,
    at: u64,
    before: BTreeMap<String, String>,
    facts: Vec<Fact>,
    packages: usize,
}
struct Run {
    root: String,
    language: Language,
    before: BTreeMap<String, String>,
    output: String,
    child: Option<Child>,
    status: Option<i32>,
    failed: bool,
    stdout_tail: String,
    stderr_tail: String,
}
#[derive(Default)]
pub(crate) struct State {
    run: Option<Run>,
    evidence: Option<Evidence>,
    message: String,
    log: String,
}
fn snapshot(project: &Project, root: &str) -> Result<BTreeMap<String, String>, Error> {
    let mut paths = BTreeSet::new();
    for glob in [
        "**/*.go",
        "**/go.mod",
        "**/go.sum",
        "**/go.work",
        "**/go.work.sum",
        "**/*.rs",
        "**/Cargo.toml",
        "**/Cargo.lock",
        "**/.cargo/config",
        "**/.cargo/config.toml",
        "**/rust-toolchain",
        "**/rust-toolchain.toml",
        "**/osv-scanner.toml",
    ] {
        let found = project.find_files(glob, 501)?;
        if found.len() >= 501 {
            return Err(
                "Call-analysis source discovery reached its bound; no partial snapshot admitted."
                    .into(),
            );
        }
        paths.extend(
            found
                .into_iter()
                .filter(|p| !p.contains("/target/") && !p.contains("/.openidea/")),
        );
    }
    // Required root lock/manifests can be ignored by normal file discovery.
    for relative in [
        "Cargo.toml",
        "Cargo.lock",
        "go.mod",
        "go.sum",
        "go.work",
        "go.work.sum",
        "rust-toolchain",
        "rust-toolchain.toml",
        ".cargo/config",
        ".cargo/config.toml",
        "osv-scanner.toml",
    ] {
        let path = format!("{root}/{relative}");
        if project.exists(&path)? {
            paths.insert(path);
        }
    }
    let mut result = BTreeMap::new();
    let mut bytes = 0;
    for path in paths {
        if !owned_source_path(root, &path) {
            return Err(Error::Stale);
        }
        let source = project.read_file(&path)?;
        bytes += source.len();
        if result.len() >= 500 || bytes > 512 << 10 {
            return Err("Call analysis supports at most 500 source files and 512 KiB.".into());
        }
        result.insert(path, source);
    }
    Ok(result)
}
fn saved(root: &str) -> Result<(), Error> {
    if host::open_editors()?
        .iter()
        .any(|e| e.dirty && e.path.starts_with(&format!("{root}/")))
    {
        return Err(
            "Save project buffers first: the scanner and compilers read saved sources.".into(),
        );
    }
    Ok(())
}
fn current(run: &Run, ctx: &Invocation) -> Result<Project, Error> {
    let p = ctx.project().ok_or(Error::Stale)?;
    if p.root()? != run.root {
        return Err(Error::Stale);
    }
    saved(&run.root)?;
    if snapshot(&p, &run.root)? != run.before {
        return Err("Call-analysis sources changed; rerun before accepting results.".into());
    }
    Ok(p)
}
fn version_matches(s: &str, language: Language) -> bool {
    s.len() < 32768
        && s.lines()
            .any(|l| l.trim() == format!("osv-scanner version: {}", language.version()))
}
fn parse(root: &str, language: Language, text: &str) -> Result<(Vec<Fact>, usize), Error> {
    if text.len() > 4 << 20 {
        return Err("Scanner JSON exceeds 4MiB.".into());
    }
    let v: serde_json::Value = serde_json::from_str(text).map_err(|_| {
        Error::Invalid("Malformed/truncated scanner JSON; previous evidence retained.".into())
    })?;
    let sources = v["results"]
        .as_array()
        .ok_or("Scanner result list is unavailable; no clean result inferred.")?;
    if sources.len() > 500 {
        return Err("Scanner source bound exceeds 500; previous evidence retained.".into());
    }
    let mut facts = Vec::new();
    let mut count = 0;
    for source in sources {
        let path = source["source"]["path"]
            .as_str()
            .ok_or("Missing scanner source identity")?;
        if !owned_source_path(root, path) {
            return Err("Scanner returned a foreign source; previous evidence retained.".into());
        }
        for package in source["packages"]
            .as_array()
            .ok_or("Malformed scanner packages")?
        {
            if package["package"]["ecosystem"].as_str() != Some(language.ecosystem()) {
                continue;
            }
            count += 1;
            if count > 10000 {
                return Err("Scanner package bound exceeded.".into());
            }
            let name = package["package"]["name"]
                .as_str()
                .ok_or("Missing scanner package name")?;
            let version = package["package"]["version"]
                .as_str()
                .ok_or("Missing scanner version")?;
            if [name, version, path]
                .iter()
                .any(|s| s.len() > 4096 || s.chars().any(char::is_control))
            {
                return Err("Unsupported scanner text field".into());
            }
            if let Some(groups) = package.get("groups") {
                for group in groups.as_array().ok_or("Malformed scanner groups")? {
                    let analyses = group
                        .get("experimental_analysis")
                        .and_then(|x| x.as_object());
                    for id in group["ids"]
                        .as_array()
                        .ok_or("Missing scanner advisory IDs")?
                    {
                        let id = id.as_str().ok_or("Malformed scanner advisory ID")?;
                        if id.is_empty() || id.len() > 256 || id.chars().any(char::is_control) {
                            return Err("Unsupported scanner advisory ID".into());
                        }
                        // Analysis maps are keyed by advisory ID. Never infer uncalled
                        // from absent, partial, or another advisory's analysis.
                        let status = match analyses
                            .and_then(|m| m.get(id))
                            .and_then(|x| x.get("called"))
                            .and_then(|x| x.as_bool())
                        {
                            Some(true) => "Called (scanner evidence)",
                            Some(false) => "Uncalled (scanner evidence; not a safety guarantee)",
                            None => "Unknown (no advisory call evidence)",
                        };
                        facts.push(Fact {
                            source: path.into(),
                            package: name.into(),
                            version: version.into(),
                            advisory: id.into(),
                            status: status.into(),
                        });
                        if facts.len() > 10000 {
                            return Err("Scanner advisory bound exceeded.".into());
                        }
                    }
                }
            }
        }
    }
    if count == 0 {
        return Err(
            "No packages in the selected language were exported; call status is Unknown.".into(),
        );
    }
    Ok((facts, count))
}
fn failure(tail: &mut String, chunk: &str) -> bool {
    let text = format!("{tail}{}", chunk.to_ascii_lowercase());
    let failed = [
        "failed to run code analysis",
        "failed to analyse",
        "error running call analysis",
    ]
    .iter()
    .any(|pattern| text.contains(pattern));
    let mut at = text.len().saturating_sub(128);
    while !text.is_char_boundary(at) {
        at += 1
    }
    *tail = text[at..].into();
    failed
}
fn valid_owner(e: &Evidence, root: &str) -> bool {
    if e.root != root || e.before.len() > 500 || e.facts.len() > 10000 {
        return false;
    }
    let paths = e
        .before
        .keys()
        .chain(e.facts.iter().map(|f| &f.source))
        .collect::<BTreeSet<_>>();
    paths.len() <= 512 && paths.iter().all(|p| owned_source_path(root, p))
}

impl State {
    pub(crate) fn start(&mut self, ctx: &Invocation) -> Result<(), Error> {
        if self.run.as_ref().is_some_and(|r| r.child.is_some()) {
            return Err("Complete or cancel the current call analysis first.".into());
        }
        let p = ctx.project().ok_or("Open a local project first.")?;
        let root = p.root()?;
        let Some(choice) = host::pick(
            "Call-analysis profile",
            &[
                openidea_plugin::PickItem::new("go", "Go (govulncheck)"),
                openidea_plugin::PickItem::new("rust", "Rust official ELF (experimental)"),
                openidea_plugin::PickItem::new(
                    "rust-macho",
                    "Rust source-built Mach-O/dSYM adapter (experimental)",
                ),
            ],
        )?
        else {
            return Ok(());
        };
        let language = match choice.as_str() {
            "go" => Language::Go,
            "rust" => Language::Rust,
            "rust-macho" => Language::RustMacho,
            _ => return Err(Error::Stale),
        };
        if !p.exists(&format!("{root}/{}", language.lock()))? {
            return Err(format!(
                "This root profile requires {} in the project root.",
                language.lock()
            )
            .into());
        }
        saved(&root)?;
        let before = snapshot(&p, &root)?;
        if !host::confirm(
            "Review vulnerable-call analysis",
            &format!(
                "Project: {root}\nProgram: {}\nVersion: {}\nProfile: {}\nLanguage: {} only\n\nThe version probe and scan execute approved local programs. The scanner may contact OSV and configured dependency services; names/versions and Go vulnerability query information may leave this computer. Go/Rust toolchains read saved sources and may fetch dependencies. Rust executes build scripts, compiles code and remains experimental; unchanged official Mach-O analysis failed during Darwin qualification. The separately named source-built patch reads Mach-O and adjacent dSYM debug data; it remains experimental. Project configuration and external dependency/compiler inputs are not fully captured by the bounded source snapshot. Other languages and missing advisory call facts remain Unknown. Results never establish safety or original Mend equivalence.\n\nThe managed child can be polled/cancelled; complete valid results replace only this tool's dated evidence. Existing vulnerability reports remain untouched.",
                language.program(),
                language.version(),
                language.profile(),
                language.name()
            ),
        )? {
            return Ok(());
        }
        let probe = Run {
            root: root.clone(),
            language,
            before: before.clone(),
            output: String::new(),
            child: None,
            status: None,
            failed: false,
            stdout_tail: String::new(),
            stderr_tail: String::new(),
        };
        current(&probe, ctx)?;
        let version = p
            .run_process(language.program(), &["--version".into()], None)
            .map_err(|e| ui_kit::denied::map(e, "Vulnerable call analysis", &format!("process:{}", language.program()), "Dependency Audit"))?;
        if version.status != Some(0) || !version_matches(&version.stdout, language) {
            let found = version
                .stdout
                .lines()
                .find_map(|l| l.trim().strip_prefix("osv-scanner version: "))
                .filter(|found| found.len() <= 64);
            let needed = language.version();
            return Err(Error::Invalid(match found {
                Some(found) => format!("Found {} {found}; needs {needed}. Install that version and try again.", language.program()),
                None => format!("Couldn't read the {} version; needs {needed}.", language.program()),
            }));
        }
        current(&probe, ctx)?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Error::Stale)?
            .as_nanos();
        let dir = format!("{root}/.openidea/call-analysis-{stamp:x}");
        if p.exists(&dir)? {
            return Err(Error::Stale);
        }
        p.write_file(
            &format!("{dir}/review.txt"),
            &format!(
                "{} {} {}",
                language.program(),
                language.version(),
                language.profile()
            ),
        )?;
        let output = format!("{dir}/result.json");
        let args = vec![
            "scan".into(),
            "source".into(),
            format!("--call-analysis={}", language.name()),
            format!("--no-call-analysis={}", language.other()),
            "--format=json".into(),
            "--all-vulns".into(),
            "--all-packages".into(),
            format!("--output-file={output}"),
            root.clone(),
        ];
        current(&probe, ctx)?;
        let child = host::start(&Command::new(language.program()).args(&args), Some(&p))?;
        self.run = Some(Run {
            root,
            language,
            before,
            output,
            child: Some(child),
            status: None,
            failed: false,
            stdout_tail: String::new(),
            stderr_tail: String::new(),
        });
        self.message="Analysis running. Poll for completion; previous evidence retained until valid success.".into();
        self.log.clear();
        crate::reveal(VIEW)?;
        crate::refresh(VIEW);
        Ok(())
    }
    pub(crate) fn poll(&mut self, ctx: &Invocation) -> Result<(), Error> {
        let run = self.run.as_mut().ok_or("Start call analysis first.")?;
        let p = current(run, ctx)?;
        let Some(child) = &run.child else {
            return Ok(());
        };
        let chunk = child.poll(65536)?;
        run.failed |= failure(&mut run.stdout_tail, &chunk.stdout)
            | failure(&mut run.stderr_tail, &chunk.stderr);
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
            run.status = Some(status);
            run.child = None;
            self.message = format!(
                "Scanner completed ({status}); validating output. Previous evidence retained until validation succeeds."
            );
            if (status == 0 || status == 1) && !run.failed {
                let text = p.read_file(&run.output)?;
                let (facts, packages) = parse(&run.root, run.language, &text)?;
                if !facts.iter().any(|f| !f.status.starts_with("Unknown")) {
                    self.message="No explicit advisory call facts were returned. Call status is Unknown; previous dated evidence retained.".into();
                    crate::refresh(VIEW);
                    return Ok(());
                }
                current(run, ctx)?;
                let evidence = Evidence {
                    root: run.root.clone(),
                    language: run.language,
                    at: now(),
                    before: run.before.clone(),
                    facts,
                    packages,
                };
                let json =
                    serde_json::to_string(&evidence).map_err(|e| Error::Invalid(e.to_string()))?;
                if json.len() > 512 << 10 {
                    return Err("Call evidence exceeds 512KiB local storage budget; previous evidence retained.".into());
                }
                host::storage::set(&format!("call-analysis:{}", run.root), &json)?;
                self.evidence = Some(evidence);
                self.message =
                    "Completed dated scanner evidence. Unreported call facts remain Unknown."
                        .into();
            } else {
                self.message = format!(
                    "Scanner failed ({status}); previous evidence retained. Partial JSON is not accepted."
                )
            }
        }
        crate::refresh(VIEW);
        Ok(())
    }
    pub(crate) fn cancel(&mut self, ctx: &Invocation) -> Result<(), Error> {
        let r = self.run.as_mut().ok_or("No scan to cancel")?;
        if ctx.project().and_then(|p| p.root().ok()).as_deref() != Some(&r.root) {
            return Err(Error::Stale);
        }
        if let Some(child) = r.child.as_ref() {
            child.kill()?
        }
        r.child = None;
        r.status = Some(-1);
        self.message = "Cancelled; previous evidence retained.".into();
        crate::refresh(VIEW);
        Ok(())
    }
    pub(crate) fn render(&self, ctx: &Invocation) -> View {
        let Some(p) = ctx.project() else {
            return View::message("Open the owning project to inspect call evidence.");
        };
        let Ok(root) = p.root() else {
            return View::message("Project unavailable");
        };
        let stored = host::storage::get(&format!("call-analysis:{root}"))
            .ok()
            .flatten()
            .filter(|s| s.len() <= 512 << 10)
            .and_then(|s| serde_json::from_str::<Evidence>(&s).ok())
            .filter(|e| valid_owner(e, &root));
        let evidence = self
            .evidence
            .as_ref()
            .filter(|e| valid_owner(e, &root))
            .or(stored.as_ref());
        let mut text = String::from(
            "Version-qualified official/source-built profiles. Go/Rust only; all other languages Unknown. No reported vulnerability does not establish absence or safety.\n",
        );
        if let Some(e) = evidence {
            let stale =
                saved(&root).is_err() || snapshot(&p, &root).ok().as_ref() != Some(&e.before);
            text.push_str(&format!(
                "\nObserved Unix UTC seconds: {}; language {}; packages {}. {}\nProfile: {} {} — {}\n",
                e.at,
                e.language.name(),
                e.packages,
                if stale {
                    "STALE: saved sources/buffers differ"
                } else {
                    "Captured source snapshot unchanged"
                }, e.language.program(), e.language.version(), e.language.profile()
            ));
            for f in e.facts.iter().take(200) {
                text.push_str(&format!(
                    "\n{} {} | {} | {}\nSource: {}\n",
                    f.package, f.version, f.advisory, f.status, f.source
                ));
            }
            if e.facts.len() > 200 {
                text.push_str("Display capped at200 advisory facts; full validated evidence retained locally.\n")
            }
            if e.facts.is_empty() {
                text.push_str(
                    "No advisory call facts were reported; call status remains Unknown.\n",
                )
            }
        } else {
            text.push_str("No completed evidence for this project.")
        }
        let owner = self.run.as_ref().is_some_and(|r| r.root == root);
        let running = owner && self.run.as_ref().is_some_and(|r| r.child.is_some());
        View::new(Body::Form(vec![
            Field::new(
                "calls-status",
                "Status",
                FieldValue::readonly_text(
                    if owner {
                        &self.message
                    } else {
                        "No active scan in this project"
                    },
                    true,
                ),
            ),
            Field::new(
                "calls-evidence",
                "Dated call evidence",
                FieldValue::readonly_text(&text, true),
            ),
            Field::new(
                "calls-log",
                "Recent output (bounded)",
                FieldValue::readonly_text(if owner { &self.log } else { "" }, true),
            ),
        ]))
        .button(
            Button::new("calls-poll", "Poll Call Analysis")
                .icon("refresh")
                .enabled(running),
        )
        .button(
            Button::new("calls-cancel", "Cancel Call Analysis")
                .icon("stop")
                .enabled(running),
        )
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_version_only() {
        assert!(version_matches(
            "osv-scanner version: 2.7.1\nosv-scalibr version: 0.5.3",
            Language::Go
        ));
        assert!(!version_matches(
            "osv-scanner version: 2.7.10",
            Language::Go
        ));
    }
    #[test]
    fn source_built_profile_cannot_be_confused_with_official_binary() {
        assert_eq!(Language::RustMacho.program(), "osv-scanner-macho");
        assert!(!version_matches(
            "osv-scanner version: 2.7.1",
            Language::RustMacho
        ));
        assert!(!version_matches(
            "osv-scanner version: 2.7.1-openidea-macho.1",
            Language::Rust
        ));
        assert!(version_matches(
            "osv-scanner version: 2.7.1-openidea-macho.1",
            Language::RustMacho
        ));
        assert!(
            Language::RustMacho
                .profile()
                .contains("not unchanged official")
        );
    }
    #[test]
    fn oversized_source_lists_are_refused_before_unbounded_owner_queries() {
        let sources =
            vec![serde_json::json!({"source":{"path":"/project/Cargo.lock"},"packages":[]}); 501];
        assert!(
            parse(
                "/project",
                Language::RustMacho,
                &serde_json::json!({"results":sources}).to_string()
            )
            .is_err()
        );
    }
    #[test]
    fn absent_analysis_is_unknown_and_foreign_sources_rejected() {
        let h = openidea_plugin::mock::MockHost::install();
        h.add_file("/project/go.mod", "module fixture");
        let t = r#"{"results":[{"source":{"path":"/project/go.mod"},"packages":[{"package":{"name":"example.org/lib","version":"1","ecosystem":"Go"},"groups":[{"ids":["GO-1","GO-2"],"experimental_analysis":{"GO-1":{"called":false}}}]}]}]}"#;
        let (facts, _) = parse("/project", Language::Go, t).unwrap();
        assert!(facts[0].status.starts_with("Uncalled"));
        assert!(facts[1].status.starts_with("Unknown"));
        assert!(parse("/else", Language::Go, t).is_err());
        assert!(parse("/project", Language::Go, "{}").is_err());
    }
    #[test]
    fn cancellation_review_never_probes_or_starts() {
        let h = openidea_plugin::mock::MockHost::install();
        h.add_file("/project/go.mod", "module fixture");
        h.answer_next_prompt(Some(&["go"]));
        h.answer_next_prompt(None);
        let mut s = State::default();
        s.start(&h.invocation()).unwrap();
        assert!(h.process_runs().is_empty());
        assert!(s.run.is_none());
    }
    #[test]
    fn managed_completion_persists_only_current_owned_evidence_and_failed_scan_preserves_previous()
    {
        use openidea_plugin::{ProcessOutput, mock::MockHost};
        let h = MockHost::install().with_capabilities(&[
            "project.read",
            "project.write",
            "editor.read",
            "storage",
            "process:osv-scanner",
        ]);
        h.add_file("/project/go.mod", "module fixture");
        h.on_process("osv-scanner", |_, _| {
            Ok(ProcessOutput {
                status: Some(0),
                stdout: "osv-scanner version: 2.7.1".into(),
                stderr: String::new(),
            })
        });
        h.answer_next_prompt(Some(&["go"]));
        h.answer_next_prompt(Some(&["ok"]));
        let mut s = State::default();
        s.start(&h.invocation()).unwrap();
        let output = s.run.as_ref().unwrap().output.clone();
        h.add_file(
            &output,
            include_str!("../tests/fixtures/osv-scanner-2.7.1-go-called.json"),
        );
        s.run
            .as_ref()
            .unwrap()
            .child
            .as_ref()
            .unwrap()
            .wait(1)
            .unwrap();
        s.poll(&h.invocation()).unwrap();
        assert!(s.evidence.as_ref().unwrap().packages > 0);
        let previous = host::storage::get("call-analysis:/project").unwrap();
        h.answer_next_prompt(Some(&["go"]));
        h.answer_next_prompt(Some(&["ok"]));
        s.start(&h.invocation()).unwrap();
        s.run.as_mut().unwrap().failed = true;
        s.run
            .as_ref()
            .unwrap()
            .child
            .as_ref()
            .unwrap()
            .wait(1)
            .unwrap();
        s.poll(&h.invocation()).unwrap();
        assert_eq!(
            host::storage::get("call-analysis:/project").unwrap(),
            previous
        );
        h.add_file("/project/go.mod", "changed");
        h.answer_next_prompt(Some(&["go"]));
        h.answer_next_prompt(Some(&["ok"]));
        s.start(&h.invocation()).unwrap();
        h.add_file("/project/go.mod", "changed again");
        assert!(s.poll(&h.invocation()).is_err());
        assert_eq!(
            host::storage::get("call-analysis:/project").unwrap(),
            previous
        );
    }
    #[test]
    fn real_macho_rust_exports_preserve_explicit_calls_and_unknown_advisories() {
        let h = openidea_plugin::mock::MockHost::install();
        h.add_file("/project/Cargo.lock", "version=4");
        for (text, called) in [
            (
                include_str!("../tests/fixtures/osv-scanner-macho-rust-called.json"),
                true,
            ),
            (
                include_str!("../tests/fixtures/osv-scanner-macho-rust-uncalled.json"),
                false,
            ),
        ] {
            let (facts, _) = parse("/project", Language::RustMacho, text).unwrap();
            let grow = facts
                .iter()
                .find(|f| f.advisory == "RUSTSEC-2019-0009")
                .unwrap();
            assert!(
                grow.status
                    .starts_with(if called { "Called" } else { "Uncalled" })
            );
            assert!(
                facts
                    .iter()
                    .find(|f| f.advisory == "RUSTSEC-2018-0018")
                    .unwrap()
                    .status
                    .starts_with("Unknown")
            );
            assert!(
                facts
                    .iter()
                    .find(|f| f.advisory == "GHSA-mm7v-vpv8-xfc3")
                    .unwrap()
                    .status
                    .starts_with("Unknown"),
                "alias must not inherit another identifier's call fact"
            );
        }
    }
    #[test]
    fn patched_rust_process_is_separate_and_source_drift_refuses_results() {
        use openidea_plugin::{ProcessOutput, mock::MockHost};
        let h = MockHost::install().with_capabilities(&[
            "project.read",
            "project.write",
            "editor.read",
            "storage",
            "process:osv-scanner-macho",
        ]);
        h.add_file("/project/Cargo.lock", "version = 4");
        h.add_file("/project/src/main.rs", "fn main() {}");
        h.on_process("osv-scanner-macho", |_, _| {
            Ok(ProcessOutput {
                status: Some(0),
                stdout: "osv-scanner version: 2.7.1-openidea-macho.1".into(),
                stderr: String::new(),
            })
        });
        h.answer_next_prompt(Some(&["rust-macho"]));
        h.answer_next_prompt(Some(&["ok"]));
        let mut state = State::default();
        state.start(&h.invocation()).unwrap();
        let run = state.run.as_ref().unwrap();
        assert_eq!(run.language.program(), "osv-scanner-macho");
        run.child.as_ref().unwrap().wait(1).unwrap();
        h.add_file(
            "/project/src/main.rs",
            "fn main() { println!(\"changed\"); }",
        );
        assert!(state.poll(&h.invocation()).is_err());
        assert!(
            host::storage::get("call-analysis:/project")
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn foreign_persisted_evidence_is_not_rendered() {
        let h = openidea_plugin::mock::MockHost::install();
        h.add_file("/project/go.mod", "module fixture");
        let evidence = Evidence {
            root: "/foreign".into(),
            language: Language::Go,
            at: 1,
            before: BTreeMap::new(),
            facts: vec![Fact {
                source: "/foreign/go.mod".into(),
                package: "SECRET_FOREIGN".into(),
                version: "1".into(),
                advisory: "GO-1".into(),
                status: "Called".into(),
            }],
            packages: 1,
        };
        host::storage::set(
            "call-analysis:/project",
            &serde_json::to_string(&evidence).unwrap(),
        )
        .unwrap();
        let view = State::default().render(&h.invocation());
        assert!(!format!("{view:?}").contains("SECRET_FOREIGN"));
    }

    #[test]
    fn real_official_go_exports_distinguish_called_uncalled_and_unknown_alias() {
        let h = openidea_plugin::mock::MockHost::install();
        h.add_file("/project/go.mod", "module fixture");
        for (source, expected) in [
            (
                include_str!("../tests/fixtures/osv-scanner-2.7.1-go-called.json"),
                "Called",
            ),
            (
                include_str!("../tests/fixtures/osv-scanner-2.7.1-go-uncalled.json"),
                "Uncalled",
            ),
        ] {
            let (facts, _) = parse("/project", Language::Go, source).unwrap();
            assert!(
                facts
                    .iter()
                    .any(|f| f.advisory == "GO-2022-1059" && f.status.starts_with(expected))
            );
            assert!(facts.iter().any(|f|f.advisory=="GHSA-69ch-w2m2-3vjp"&&f.status.starts_with("Unknown")));
        }
    }

    #[test]
    fn split_failure_phrase_is_detected_with_independent_stream_tails() {
        let mut out = String::new();
        let mut err = String::new();
        assert!(!failure(&mut err, "Failed to run code ana"));
        assert!(!failure(&mut out, "lysis"));
        assert!(failure(&mut err, "lysis (govulncheck)"));
    }
}
