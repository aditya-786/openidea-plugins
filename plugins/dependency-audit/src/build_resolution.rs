//! Explicit approved local-build adapters. Resolved facts enter dated command
//! reports; no editor provider invokes a package manager or network resolver.
use crate::*;
use openidea_plugin::host::{Child, Command};
use openidea_plugin::DependencyStatus;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
pub(crate) const VIEW: &str = "build-resolution";
const INIT: &str = include_str!("gradle-resolution.init.gradle");
const IDE_HINT: &str = " Reload the Maven project or open the Dependency Analyzer in the IDE for faster results without running the build tool.";
#[derive(Clone, Copy)]
enum Tool {
    Maven,
    Gradle,
}
impl Tool {
    fn program(self) -> &'static str {
        match self {
            Self::Maven => "mvn",
            Self::Gradle => "gradle",
        }
    }
    fn version(self) -> &'static str {
        match self {
            Self::Maven => "3.9.16",
            Self::Gradle => "9.9.1",
        }
    }
    fn output(self) -> &'static str {
        match self {
            Self::Maven => "maven.json",
            Self::Gradle => "gradle.jsonl",
        }
    }
}
struct Run {
    root: String,
    tool: Tool,
    configuration: String,
    output: String,
    before: BTreeMap<String, String>,
    child: Option<Child>,
    status: Option<i32>,
}
#[derive(Default)]
pub(crate) struct State {
    run: Option<Run>,
    message: String,
    log: String,
    ide_note: Option<String>,
}
pub(crate) struct Evidence {
    pub root: String,
    pub deps: Vec<Dep>,
    pub sources: HashMap<String, String>,
    pub files: Vec<String>,
    pub skipped: usize,
    before: BTreeMap<String, String>,
}
fn source_files(project: &Project, root: &str) -> Result<BTreeMap<String, String>, Error> {
    let mut paths = BTreeSet::new();
    for glob in [
        "**/pom.xml",
        "**/*.gradle",
        "**/*.gradle.kts",
        "**/gradle.properties",
        "**/libs.versions.toml",
    ] {
        let found = project.find_files(glob, 201)?;
        if found.len() >= 201 {
            return Err("Build source discovery reached its per-pattern bound; resolution review is incomplete.".into());
        }
        paths.extend(
            found
                .into_iter()
                .filter(|path| !path.contains("/.openidea/dependency-audit-resolve")),
        );
    }
    for relative in [
        ".mvn/maven.config",
        ".mvn/jvm.config",
        ".mvn/extensions.xml",
    ] {
        let path = format!("{root}/{relative}");
        if project.exists(&path)? {
            paths.insert(path);
        }
    }
    let mut before = BTreeMap::new();
    let mut bytes = 0usize;
    for path in paths {
        if !owned_source_path(root, &path) {
            return Err(Error::Stale);
        }
        let text = project.read_file(&path)?;
        bytes += text.len();
        if bytes > 4 << 20 || before.len() >= 500 {
            return Err("Build-source snapshot exceeds 500 files/4MiB.".into());
        }
        before.insert(path, text);
    }
    Ok(before)
}
fn approved(root: &str, tool: Tool) -> Result<bool, Error> {
    let key = format!("resolve-approved:{root}");
    let remembered = host::storage::get(&key).ok().flatten();
    if let Some(answer) = remembered {
        return Ok(answer == "yes");
    }
    let name = match tool {
        Tool::Maven => "Maven",
        Tool::Gradle => "Gradle",
    };
    let answer = host::confirm(
        "Resolve dependencies?",
        &format!("Run {name} to resolve dependencies? This executes this project's build scripts and plugins, and downloads from its repositories. Your answer is remembered for this project."),
    )?;
    let _ = host::storage::set(&key, if answer { "yes" } else { "no" });
    Ok(answer)
}
fn require_saved(root: &str) -> Result<(), Error> {
    if host::open_editors()?.iter().any(|editor| {
        editor.dirty
            && editor
                .path
                .starts_with(&format!("{}/", root.trim_end_matches('/')))
    }) {
        return Err("Save project editor changes before resolving/importing; the external build reads saved files.".into());
    }
    Ok(())
}
fn current(run: &Run, ctx: &Invocation) -> Result<Project, Error> {
    let project = ctx.project().ok_or(Error::Stale)?;
    if project.root()? != run.root {
        return Err(Error::Stale);
    }
    require_saved(&run.root)?;
    if source_files(&project, &run.root)? != run.before {
        return Err("Build sources changed during resolution; rerun before auditing.".into());
    }
    Ok(project)
}
fn version_matches(tool: Tool, output: &str) -> bool {
    let prefix = match tool {
        Tool::Maven => "Apache Maven ",
        Tool::Gradle => "Gradle ",
    };
    output.len() < 32 << 10
        && output.lines().any(|line| {
            line.trim()
                .strip_prefix(prefix)
                .and_then(|tail| tail.split_whitespace().next())
                == Some(tool.version())
        })
}
fn version_mismatch(tool: Tool, output: &str) -> Error {
    let (prefix, name) = match tool {
        Tool::Maven => ("Apache Maven ", "Maven"),
        Tool::Gradle => ("Gradle ", "Gradle"),
    };
    let found = output
        .lines()
        .find_map(|line| line.trim().strip_prefix(prefix)?.split_whitespace().next())
        .filter(|found| found.len() <= 64);
    let needed = tool.version();
    Error::Invalid(match found {
        Some(found) => format!("Found {name} {found}; needs {name} {needed}. Install that version and try again."),
        None => format!("Couldn't read the {name} version; needs {name} {needed}. Check that {} runs from the project folder.", tool.program()),
    })
}
fn clean(value: &str) -> bool {
    !value.is_empty() && value.len() <= 512 && !value.chars().any(char::is_control)
}
fn coordinate(group: &str, name: &str, version: &str) -> bool {
    clean(group)
        && clean(name)
        && clean(version)
        && !group.contains([':', '/', '\\'])
        && !name.contains([':', '/', '\\'])
        && !version.contains([' ', '[', ']', '(', ')', '$'])
}
fn add(
    deps: &mut Vec<Dep>,
    sources: &mut HashMap<String, String>,
    path: &str,
    group: &str,
    name: &str,
    version: &str,
    provenance: &str,
) -> Result<(), Error> {
    if !coordinate(group, name, version) {
        return Err(
            "Resolver returned an unsupported coordinate; no partial audit imported.".into(),
        );
    }
    let package = format!("{group}:{name}");
    let key = format!("{path}|{package} {version}");
    match sources.entry(key) {
        std::collections::hash_map::Entry::Vacant(entry) => {
            entry.insert(provenance.into());
            deps.push(Dep {
                path: path.into(),
                ecosystem: manifest::Ecosystem::Maven,
                name: package,
                version: version.into(),
                at: (0, 0),
                version_at: None,
            });
        }
        std::collections::hash_map::Entry::Occupied(mut entry) => {
            if !entry.get().contains(provenance) && entry.get().len() + provenance.len() < 8192 {
                entry.get_mut().push_str("; ");
                entry.get_mut().push_str(provenance);
            }
        }
    }
    if deps.len() > 10000 {
        return Err("Resolved dependency graph exceeds 10000 coordinates.".into());
    }
    Ok(())
}
fn parse_maven(root: &str, text: &str) -> Result<Evidence, Error> {
    if text.len() > 4 << 20 {
        return Err("Maven output exceeds 4MiB.".into());
    }
    let trees: Vec<Value> = serde_json::Deserializer::from_str(text)
        .into_iter::<Value>()
        .collect::<Result<_, _>>()
        .map_err(|_| Error::Invalid("Maven JSON trees are malformed or truncated.".into()))?;
    if trees.is_empty() || trees.len() > 500 {
        return Err("Maven exported no complete module trees, or more than 500 modules.".into());
    }
    let root_ids: HashSet<_> = trees
        .iter()
        .filter_map(|tree| {
            Some(format!(
                "{}:{}:{}",
                tree["groupId"].as_str()?,
                tree["artifactId"].as_str()?,
                tree["version"].as_str()?
            ))
        })
        .collect();
    if root_ids.len() != trees.len() {
        return Err("Maven module identities are missing or duplicated.".into());
    }
    fn walk(
        tree: &Value,
        depth: usize,
        module: &str,
        path: &str,
        internal: &HashSet<String>,
        deps: &mut Vec<Dep>,
        sources: &mut HashMap<String, String>,
    ) -> Result<(), Error> {
        if depth > 128 {
            return Err("Maven tree depth exceeds 128.".into());
        }
        let (group, name, version) = (
            tree["groupId"].as_str().ok_or("Missing Maven group")?,
            tree["artifactId"]
                .as_str()
                .ok_or("Missing Maven artifact")?,
            tree["version"].as_str().ok_or("Missing Maven version")?,
        );
        if !coordinate(group, name, version) {
            return Err("Unsupported Maven tree identity.".into());
        }
        if depth > 0 && !internal.contains(&format!("{group}:{name}:{version}")) {
            add(
                deps,
                sources,
                path,
                group,
                name,
                version,
                &format!(
                    "Resolved Maven 3.9.16 dependency:tree 3.8.1; module {module}; scope {}. Root POM location, not an exact declaration span.",
                    tree["scope"].as_str().unwrap_or("unspecified")
                ),
            )?;
        }
        if let Some(children) = tree.get("children") {
            for child in children.as_array().ok_or("Invalid Maven children")? {
                walk(child, depth + 1, module, path, internal, deps, sources)?;
            }
        }
        Ok(())
    }
    let path = format!("{root}/pom.xml");
    let mut deps = Vec::new();
    let mut sources = HashMap::new();
    for tree in trees {
        let module = format!(
            "{}:{}:{}",
            tree["groupId"].as_str().unwrap(),
            tree["artifactId"].as_str().unwrap(),
            tree["version"].as_str().unwrap()
        );
        walk(&tree, 0, &module, &path, &root_ids, &mut deps, &mut sources)?;
    }
    Ok(Evidence {
        root: root.into(),
        deps,
        sources,
        files: vec![path],
        skipped: 0,
        before: BTreeMap::new(),
    })
}
fn parse_gradle(root: &str, configuration: &str, text: &str) -> Result<Evidence, Error> {
    if text.len() > 4 << 20 {
        return Err("Gradle export exceeds 4MiB.".into());
    }
    let mut deps = Vec::new();
    let mut sources = HashMap::new();
    let mut files = BTreeSet::new();
    let mut skipped = 0;
    let mut modules = HashSet::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let record: Value = serde_json::from_str(line)
            .map_err(|_| Error::Invalid("Gradle export is malformed or truncated.".into()))?;
        let module = record["project"]
            .as_str()
            .filter(|s| s.starts_with(':') && clean(s))
            .ok_or("Missing Gradle project identity")?;
        if modules.len() >= 500
            || !modules.insert(module.to_owned())
            || record["schema"] != 1
            || record["buildRoot"].as_str() != Some(root)
            || record["configuration"].as_str() != Some(configuration)
        {
            return Err("Foreign, duplicate or unsupported Gradle export identity.".into());
        }
        let path = record["buildFile"]
            .as_str()
            .filter(|path| {
                path.starts_with(&format!("{root}/"))
                    && !path.split('/').any(|part| matches!(part, "." | ".."))
            })
            .ok_or("Foreign Gradle build file")?;
        files.insert(path.to_owned());
        if record["unavailable"] == true {
            skipped += 1;
            continue;
        }
        let rows = record["dependencies"]
            .as_array()
            .ok_or("Missing Gradle dependencies")?;
        for dep in rows {
            add(
                &mut deps,
                &mut sources,
                path,
                dep["group"].as_str().ok_or("Missing group")?,
                dep["name"].as_str().ok_or("Missing module")?,
                dep["version"].as_str().ok_or("Missing version")?,
                &format!(
                    "Resolved Gradle 9.9.1 {module} / {configuration}. Build-file location, not an exact declaration span."
                ),
            )?;
        }
    }
    if modules.is_empty() || modules.len() == skipped {
        return Err("No module exported a resolvable requested configuration.".into());
    }
    Ok(Evidence {
        root: root.into(),
        deps,
        sources,
        files: files.into_iter().collect(),
        skipped,
        before: BTreeMap::new(),
    })
}
fn gradle_file(before: &BTreeMap<String, String>, root: &str, project_path: &str) -> String {
    let dir = project_path.trim_matches(':').replace(':', "/");
    [format!("{root}/{dir}"), root.to_owned()]
        .iter()
        .flat_map(|base| ["build.gradle.kts", "build.gradle"].map(|file| format!("{}/{file}", base.trim_end_matches('/'))))
        .find(|path| before.contains_key(path))
        .unwrap_or_else(|| format!("{root}/build.gradle"))
}
/// The graph the IDE already resolved (last Maven reload, Gradle Dependency
/// Analyzer), read the way the mvn/gradle exports are: Maven keeps every scope,
/// Gradle keeps runtimeClasspath, reactor modules and omitted occurrences are
/// skipped. `Ok(None)` when it holds no such dependency yet.
fn ide_evidence(project: &Project, root: &str) -> Result<Option<Evidence>, Error> {
    let graph = project.build_dependencies(None)?;
    let before = source_files(project, root)?;
    let modules: HashSet<String> = graph
        .iter()
        .filter(|node| node.parent.is_none())
        .map(|node| format!("{}:{}", node.group, node.artifact))
        .collect();
    let mut deps = Vec::new();
    let mut sources = HashMap::new();
    for (index, node) in graph.iter().enumerate() {
        let mut ancestors = Vec::new();
        let mut at = index;
        while let Some(parent) = graph[at].parent {
            if parent as usize >= at {
                return Err(Error::Invalid("IDE dependency graph is out of order.".into()));
            }
            ancestors.push(at);
            at = parent as usize;
        }
        let Some(&direct) = ancestors.last() else {
            continue;
        };
        let module = &graph[at];
        let gradle = module.group.is_empty();
        if !matches!(node.status, DependencyStatus::Resolved | DependencyStatus::VersionManaged)
            || node.group.is_empty()
            || node.version.is_empty()
            || modules.contains(&format!("{}:{}", node.group, node.artifact))
            || (gradle && graph[direct].scope != "runtimeClasspath")
        {
            continue;
        }
        let via: Vec<String> = ancestors[1..]
            .iter()
            .rev()
            .take(6)
            .map(|&i| format!("{}:{}", graph[i].group, graph[i].artifact))
            .collect();
        let via = if via.is_empty() { String::new() } else { format!("; via {}", via.join(" > ")) };
        let (path, location, name) = if gradle {
            (gradle_file(&before, root, &module.artifact), "Build-file", module.artifact.clone())
        } else {
            (format!("{root}/pom.xml"), "Root POM", format!("{}:{}", module.group, module.artifact))
        };
        add(
            &mut deps,
            &mut sources,
            &path,
            &node.group,
            &node.artifact,
            &node.version,
            &format!(
                "Resolved by the IDE; module {name}; scope {}{via}. {location} location, not an exact declaration span.",
                node.scope
            ),
        )?;
    }
    if deps.is_empty() {
        return Ok(None);
    }
    Ok(Some(Evidence {
        root: root.into(),
        deps,
        sources,
        files: before.keys().cloned().collect(),
        skipped: 0,
        before,
    }))
}
impl State {
    /// Audit the IDE's resolved graph when it has one, else run mvn/gradle when
    /// `run_tool` allows it (after a scan: the setting; the explicit command: always).
    pub(crate) fn resolve(&mut self, ctx: &Invocation, run_tool: bool, focus: bool) -> Result<(), Error> {
        let project = ctx.project().ok_or(Error::Stale)?;
        let root = project.root()?;
        let nothing_resolved = match ide_evidence(&project, &root) {
            Ok(Some(evidence)) => {
                let count = evidence.deps.len();
                audit_evidence(&project, evidence)?;
                self.ide_note = Some(format!(
                    "Resolved by the IDE: {count} Maven/Gradle dependencies audited from its last Maven reload or Dependency Analyzer read. No build tool ran."
                ));
                crate::refresh(VIEW);
                if focus {
                    crate::reveal(VIEW)?;
                }
                return Ok(());
            }
            Ok(None) => true,
            Err(_) => false,
        };
        if !run_tool {
            return Ok(());
        }
        self.start(ctx, focus)?;
        if nothing_resolved {
            self.message.push_str(IDE_HINT);
            crate::refresh(VIEW);
        }
        Ok(())
    }
    pub(crate) fn start(&mut self, ctx: &Invocation, focus: bool) -> Result<(), Error> {
        if self.run.as_ref().is_some_and(|run| run.child.is_some()) {
            return Err("Cancel or complete the active resolver before starting another.".into());
        }
        let project = ctx.project().ok_or("Open a local project first.")?;
        let root = project.root()?;
        let tool = if project.exists(&format!("{root}/pom.xml"))? {
            Tool::Maven
        } else if project.exists(&format!("{root}/build.gradle"))? || project.exists(&format!("{root}/build.gradle.kts"))? {
            Tool::Gradle
        } else {
            return Err(Error::NotFound("No pom.xml or build.gradle(.kts) in the project root.".into()));
        };
        let configuration: String = match tool {
            Tool::Maven => "all declared scopes".into(),
            Tool::Gradle => "runtimeClasspath".into(),
        };
        if configuration.len() > 128
            || !configuration
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b' '))
        {
            return Err("Unsupported configuration name.".into());
        }
        if matches!(tool, Tool::Maven) && !project.exists(&format!("{root}/pom.xml"))? {
            return Err("The reviewed project root must contain pom.xml.".into());
        }
        require_saved(&root)?;
        let before = source_files(&project, &root)?;
        if before.is_empty() {
            return Err("No readable build sources found.".into());
        }
        if ctx.project().ok_or(Error::Stale)?.root()? != root
            || source_files(&project, &root)? != before
        {
            return Err(Error::Stale);
        }
        if focus {
            let _ = host::storage::set(&format!("resolve-approved:{root}"), "yes");
        } else if !approved(&root, tool)? {
            return Ok(());
        }
        let args = match tool {
            Tool::Maven => vec![
                "-B".into(),
                "-Dstyle.color=never".into(),
                "--version".into(),
            ],
            Tool::Gradle => vec!["--version".into()],
        };
        require_saved(&root)?;
        let version = project
            .run_process(tool.program(), &args, None)
            .map_err(|e| ui_kit::denied::map(e, "Build dependency resolution", &format!("process:{}", tool.program()), "Dependency Audit"))?;
        if version.status != Some(0) || !version_matches(tool, &version.stdout) {
            return Err(version_mismatch(tool, &version.stdout));
        }
        if ctx.project().ok_or(Error::Stale)?.root()? != root
            || source_files(&project, &root)? != before
        {
            return Err(Error::Stale);
        }
        let directory = format!("{root}/.openidea/dependency-audit-resolve");
        project.write_file(
            &format!("{directory}/review.txt"),
            &format!("{} {} / {configuration}", tool.program(), tool.version()),
        )?;
        let output = format!("{directory}/{}", tool.output());
        project.write_file(&output, "")?;
        let args = match tool {
            Tool::Maven => vec![
                "-B".into(),
                "-ntp".into(),
                "-Dstyle.color=never".into(),
                "org.apache.maven.plugins:maven-dependency-plugin:3.8.1:tree".into(),
                "-DoutputType=json".into(),
                "-DappendOutput=true".into(),
                format!("-DoutputFile={output}"),
            ],
            Tool::Gradle => {
                let script = format!("{directory}/export.gradle");
                project.write_file(&script, INIT)?;
                vec![
                    "--no-daemon".into(),
                    "--console=plain".into(),
                    "--init-script".into(),
                    script,
                    "openideaDependencyAuditExport".into(),
                    format!("-PopenideaAuditOutput={directory}"),
                    format!("-PopenideaAuditConfiguration={configuration}"),
                ]
            }
        };
        require_saved(&root)?;
        let child = host::start(&Command::new(tool.program()).args(&args), Some(&project))?;
        self.run = Some(Run {
            root,
            tool,
            configuration,
            output,
            before,
            child: Some(child),
            status: None,
        });
        self.message =
            "Resolver running; output updates automatically. Cancel kills the owned process."
                .into();
        self.log.clear();
        self.ide_note = None;
        if focus {
            crate::reveal(VIEW)?;
        }
        crate::refresh(VIEW);
        Ok(())
    }
    pub(crate) fn poll(&mut self, ctx: &Invocation) -> Result<(), Error> {
        let run = self.run.as_mut().ok_or("Start a resolver first.")?;
        let _ = current(run, ctx)?;
        let Some(child) = run.child.as_ref() else {
            return Ok(());
        };
        let chunk = child.poll(64 << 10)?;
        self.log.push_str(&chunk.stdout);
        self.log.push_str(&chunk.stderr);
        if self.log.len() > 32 << 10 {
            let mut at = self.log.len() - (32 << 10);
            while !self.log.is_char_boundary(at) {
                at += 1;
            }
            self.log = self.log[at..].into();
        }
        if let Some(status) = chunk.status {
            run.status = Some(status);
            run.child = None;
            self.message = if status == 0 {
                format!("Resolved by running {}; export completed.", run.tool.program())
            } else {
                format!("Resolver failed with status {status}; previous report retained.")
            };
        }
        crate::refresh(VIEW);
        Ok(())
    }
    pub(crate) fn cancel(&mut self, ctx: &Invocation) -> Result<(), Error> {
        let run = self.run.as_mut().ok_or("No resolver to cancel.")?;
        if ctx.project().and_then(|p| p.root().ok()).as_deref() != Some(run.root.as_str()) {
            return Err(Error::Stale);
        }
        if let Some(child) = run.child.take() {
            child.kill()?;
        }
        run.status = Some(-1);
        self.message = "Resolver cancelled; previous report retained.".into();
        crate::refresh(VIEW);
        Ok(())
    }
    pub(crate) fn evidence(&self, ctx: &Invocation) -> Result<Evidence, Error> {
        let run = self
            .run
            .as_ref()
            .filter(|run| run.status == Some(0) && run.child.is_none())
            .ok_or("Wait for a successful completed resolver first.")?;
        let project = current(run, ctx)?;
        let text = project.read_file(&run.output)?;
        let mut evidence = match run.tool {
            Tool::Maven => parse_maven(&run.root, &text),
            Tool::Gradle => parse_gradle(&run.root, &run.configuration, &text),
        }?;
        evidence.before = run.before.clone();
        evidence.files = run.before.keys().cloned().collect();
        for file in &evidence.files {
            if !owned_source_path(&run.root, file) {
                return Err(Error::Stale);
            }
        }
        Ok(evidence)
    }
    pub(crate) fn running(&self) -> bool {
        self.run.as_ref().is_some_and(|run| run.child.is_some())
    }
    pub(crate) fn succeeded(&self) -> bool {
        self.run.as_ref().is_some_and(|run| run.status == Some(0) && run.child.is_none())
    }
    pub(crate) fn render(&self, ctx: &Invocation) -> View {
        let resolve = Button::new("resolve-build", "Resolve Dependencies").icon("run").enabled(!self.running());
        if let Some(note) = self.ide_note.as_ref().filter(|_| !self.running()) {
            return View::message(note).button(resolve);
        }
        let Some(run) = &self.run else {
            return View::message(
                "Resolve the effective Maven or Gradle dependency versions of this project and audit them against OSV.",
            )
            .button(resolve);
        };
        if ctx.project().and_then(|p| p.root().ok()).as_deref() != Some(run.root.as_str()) {
            return View::message(
                "The active resolver belongs to another project; return there to poll or cancel.",
            );
        }
        if run.child.is_some() {
            let text = format!("{}\n{} {} / {}\n\n{}", self.message, run.tool.program(), run.tool.version(), run.configuration, self.log);
            return View::new(Body::Other {
                kind: "message".into(),
                data: serde_json::json!({"text": text, "backgroundPoll": {"intervalMs": 1000, "message": {"op": "resolution-tick"}}}).to_string(),
            })
            .button(resolve)
            .button(Button::new("resolution-cancel", "Cancel Resolver").icon("stop"));
        }
        View::new(Body::Form(vec![
            openidea_plugin::views::Field::new(
                "resolution-status",
                "Status",
                openidea_plugin::views::FieldValue::readonly_text(
                    &format!(
                        "{}\n{} {} / {}\nOutput: {}",
                        self.message,
                        run.tool.program(),
                        run.tool.version(),
                        run.configuration,
                        run.output
                    ),
                    true,
                ),
            ),
            openidea_plugin::views::Field::new(
                "resolution-output",
                "Recent output (bounded)",
                openidea_plugin::views::FieldValue::readonly_text(&self.log, true),
            ),
        ]))
        .button(resolve)
        .button(
            Button::new("resolution-audit", "Audit Resolved Dependencies")
                .icon("check")
                .enabled(run.status == Some(0) && run.child.is_none()),
        )
    }
}
pub(crate) fn audit_evidence(project: &Project, evidence: Evidence) -> Result<(), Error> {
    require_saved(&evidence.root)?;
    if project.root()? != evidence.root || source_files(project, &evidence.root)? != evidence.before
    {
        return Err(Error::Stale);
    }
    let findings = audit(&evidence.deps, true, now(), Instant::now() + CALL_BUDGET)?;
    let mut rows: Vec<Row> = findings
        .iter()
        .map(|finding| Row {
            path: finding.dep.path.clone(),
            line: 0,
            name: finding.dep.name.clone(),
            version: finding.dep.version.clone(),
            fix: finding.fix(),
            advisories: finding
                .advisories
                .iter()
                .map(|advisory| advisory.id.clone())
                .collect(),
            ecosystem: Some("Maven".into()),
            sources: evidence
                .deps
                .iter()
                .filter(|dep| dep.name == finding.dep.name && dep.version == finding.dep.version)
                .map(|dep| Declaration {
                    path: dep.path.clone(),
                    line: 0,
                    evidence: evidence
                        .sources
                        .get(&format!("{}|{} {}", dep.path, dep.name, dep.version))
                        .cloned()
                        .unwrap_or_else(|| "Resolved CLI observation".into()),
                })
                .collect(),
            details: finding.advisories.clone(),
        })
        .collect();
    binding::annotate(&mut rows, &evidence.before.iter().map(|(k, v)| (k.clone(), v.clone())).collect());
    let report = Report {
        rows,
        scanned_at: Some(now()),
        files_scanned: Some(evidence.files.len()),
        files_skipped: evidence.skipped,
        dependencies_checked: Some(
            evidence
                .deps
                .iter()
                .map(|dep| (&dep.name, &dep.version))
                .collect::<HashSet<_>>()
                .len(),
        ),
        checked_files: evidence.files,
        discovery_limited: false,
        ..Report::default()
    };
    if project.root()? != evidence.root || source_files(project, &evidence.root)? != evidence.before
    {
        return Err(Error::Stale);
    }
    require_saved(&evidence.root)?;
    persist_report(project, &report)?;
    refresh_report();
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn maven_reactor_graph_excludes_internal_modules_and_keeps_transitive_effective_versions() {
        let output = r#"{"groupId":"app","artifactId":"parent","version":"1","children":[{"groupId":"app","artifactId":"child","version":"1","children":[{"groupId":"org.example","artifactId":"library","version":"2.0","scope":"runtime"}]}]} {"groupId":"app","artifactId":"child","version":"1","children":[{"groupId":"org.example","artifactId":"library","version":"2.0","scope":"runtime"}]}"#;
        let evidence = parse_maven("/project", output).unwrap();
        assert_eq!(evidence.deps.len(), 1);
        assert_eq!(evidence.deps[0].name, "org.example:library");
        assert_eq!(evidence.deps[0].version, "2.0");
        assert!(evidence.deps[0].version_at.is_none());
        assert!(parse_maven("/project", "{}").is_err());
    }
    #[test]
    fn gradle_export_requires_exact_root_schema_configuration_and_module_ownership() {
        let text = r#"{"schema":1,"buildRoot":"/project","project":":app","buildFile":"/project/app/build.gradle","configuration":"runtimeClasspath","unavailable":false,"dependencies":[{"group":"org.example","name":"library","version":"2.0"}]}"#;
        let evidence = parse_gradle("/project", "runtimeClasspath", text).unwrap();
        assert_eq!(evidence.deps[0].name, "org.example:library");
        assert!(parse_gradle("/foreign", "runtimeClasspath", text).is_err());
        assert!(parse_gradle("/project", "testRuntimeClasspath", text).is_err());
        assert!(parse_gradle("/project", "runtimeClasspath", &format!("{text}\n{text}")).is_err());
    }
    #[test]
    fn official_tool_fixture_exports_include_bom_property_and_transitive_resolution() {
        let m = parse_maven(
            "/project",
            include_str!("../tests/fixtures/maven-resolved-3.9.16.json"),
        )
        .unwrap();
        let g = parse_gradle(
            "/project",
            "runtimeClasspath",
            include_str!("../tests/fixtures/gradle-resolved-9.9.1.jsonl"),
        )
        .unwrap();
        for evidence in [&m, &g] {
            assert!(
                evidence
                    .deps
                    .iter()
                    .any(|dep| dep.name == "com.google.guava:guava" && dep.version == "32.1.3-jre")
            );
            assert!(
                evidence
                    .deps
                    .iter()
                    .any(|dep| dep.name == "org.junit.jupiter:junit-jupiter-api"
                        && dep.version == "5.10.2")
            );
            assert!(
                evidence
                    .deps
                    .iter()
                    .any(|dep| dep.name == "com.google.guava:failureaccess"
                        && dep.version == "1.0.1")
            );
            assert!(
                evidence
                    .deps
                    .iter()
                    .all(|dep| !dep.name.starts_with("fixture:"))
            );
        }
        assert_eq!(g.skipped, 1);
    }
    #[test]
    fn reviewed_managed_maven_export_can_be_polled_and_imported_but_changed_sources_are_refused() {
        use openidea_plugin::{ProcessOutput, mock::MockHost};
        let h = MockHost::install().with_capabilities(&[
            "project.read",
            "project.write",
            "editor.read",
            "process:mvn",
        ]);
        h.add_file("/project/pom.xml", "<project/> ");
        h.on_process("mvn", |args, _| {
            Ok(ProcessOutput {
                status: Some(0),
                stdout: if args.iter().any(|arg| arg == "--version") {
                    "Apache Maven 3.9.16 (official)".into()
                } else {
                    "Export complete".into()
                },
                stderr: String::new(),
            })
        });
        let mut state = State::default();
        state.start(&h.invocation(), true).unwrap();
        assert!(state.run.as_ref().unwrap().child.is_some());
        let output = state.run.as_ref().unwrap().output.clone();
        h.add_file(
            output.strip_prefix("/project/").unwrap(),
            include_str!("../tests/fixtures/maven-resolved-3.9.16.json"),
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
        let evidence = state.evidence(&h.invocation()).unwrap();
        assert!(!evidence.deps.is_empty());
        assert!(h.requests().is_empty());
        h.add_file("/project/pom.xml", "changed");
        assert!(state.evidence(&h.invocation()).is_err());
    }
    #[test]
    fn completed_resolved_coordinates_use_existing_osv_report_pipeline_and_cache_only_editor_facts()
    {
        use openidea_plugin::{HttpResponse, mock::MockHost};
        let h = MockHost::install().with_capabilities(&[
            "project.read",
            "editor.read",
            "storage",
            "net:api.osv.dev",
        ]);
        h.add_file("/project/pom.xml", "<project/> ");
        h.on_fetch(crate::tests::osv);
        let mut evidence=parse_maven("/project",r#"{"groupId":"fixture","artifactId":"project","version":"1","children":[{"groupId":"com.fasterxml.jackson.core","artifactId":"jackson-databind","version":"2.12.3","scope":"runtime"}]}"#).unwrap();
        let project = host::projects().unwrap().remove(0);
        evidence.before = source_files(&project, "/project").unwrap();
        audit_evidence(&project, evidence).unwrap();
        let report = stored_report(&project).unwrap().unwrap();
        assert_eq!(report.rows.len(), 1);
        assert_eq!(
            report.rows[0].name,
            "com.fasterxml.jackson.core:jackson-databind"
        );
        assert!(
            report.rows[0].sources[0]
                .evidence
                .contains("Resolved Maven 3.9.16")
        );
        let original = host::storage::get(&rows_key(&project).unwrap()).unwrap();
        let mut evidence=parse_maven("/project",r#"{"groupId":"fixture","artifactId":"project","version":"1","children":[{"groupId":"org.example","artifactId":"offline","version":"1"}]}"#).unwrap();
        evidence.before = source_files(&project, "/project").unwrap();
        h.on_fetch(|_| {
            Ok(HttpResponse {
                status: 503,
                headers: vec![],
                body: vec![],
            })
        });
        assert!(audit_evidence(&project, evidence).is_err());
        assert_eq!(
            host::storage::get(&rows_key(&project).unwrap()).unwrap(),
            original
        );
    }
    #[test]
    fn dirty_build_inputs_cannot_launch_or_be_misrepresented_as_saved_cli_sources() {
        let h = openidea_plugin::mock::MockHost::install();
        h.add_file("/project/pom.xml", "<project/> ");
        h.open_editor("/project/pom.xml", "xml", "<project/> ", &[(0, 0, 0, 0)]);
        h.type_in_editor("<project>unsaved</project>");
        let mut state = State::default();
        assert!(state.start(&h.invocation(), true).is_err());
        assert!(state.run.is_none() && h.process_runs().is_empty());
    }
    fn node(group: &str, artifact: &str, version: &str, scope: &str, status: DependencyStatus, parent: Option<u32>) -> openidea_plugin::DependencyNode {
        openidea_plugin::DependencyNode { group: group.into(), artifact: artifact.into(), version: version.into(), scope: scope.into(), status, requested_version: None, parent }
    }
    fn ide_host(capabilities: &[&str]) -> openidea_plugin::mock::MockHost {
        use openidea_plugin::{ProcessOutput, mock::MockHost};
        let h = MockHost::install().with_capabilities(capabilities);
        h.add_file("/project/pom.xml", "<project/> ");
        h.on_fetch(crate::tests::osv);
        h.on_process("mvn", |_, _| Ok(ProcessOutput { status: Some(0), stdout: "Apache Maven 3.9.16 (official)".into(), stderr: String::new() }));
        h
    }
    const IDE_CAPABILITIES: &[&str] = &["project.read", "project.write", "editor.read", "storage", "net:api.osv.dev", "process:mvn", "build.read"];
    #[test]
    fn ide_graph_keeps_resolved_and_managed_versions_and_skips_omitted_modules_and_test_gradle_configurations() {
        use DependencyStatus::*;
        let h = ide_host(IDE_CAPABILITIES);
        h.add_file("/project/web/build.gradle.kts", "");
        h.set_build_dependencies(vec![
            node("app", "parent", "1", "", Resolved, None),
            node("app", "child", "1", "compile", Resolved, Some(0)),
            node("org.example", "kept", "1.0", "compile", Resolved, Some(1)),
            node("org.example", "managed", "3.0", "test", VersionManaged, Some(2)),
            node("org.example", "lost", "2.0", "compile", OmittedForConflict, Some(0)),
            node("org.example", "kept", "1.0", "compile", OmittedForDuplicate, Some(0)),
            node("", ":web", "", "", Resolved, None),
            node("org.yaml", "snakeyaml", "2.2", "runtimeClasspath", Resolved, Some(6)),
            node("org.junit", "junit", "4.13", "testRuntimeClasspath", Resolved, Some(6)),
            node("", ":core", "", "runtimeClasspath", Resolved, Some(6)),
            node("app", "child", "1", "", Resolved, None),
        ]);
        let project = h.invocation().project().unwrap();
        let evidence = ide_evidence(&project, "/project").unwrap().unwrap();
        let found: Vec<_> = evidence.deps.iter().map(|dep| (dep.path.as_str(), dep.name.as_str(), dep.version.as_str())).collect();
        assert_eq!(found, [
            ("/project/pom.xml", "org.example:kept", "1.0"),
            ("/project/pom.xml", "org.example:managed", "3.0"),
            ("/project/web/build.gradle.kts", "org.yaml:snakeyaml", "2.2"),
        ]);
        assert!(evidence.sources["/project/pom.xml|org.example:managed 3.0"].contains("Resolved by the IDE; module app:parent; scope test; via app:child > org.example:kept"));
        h.set_build_dependencies(vec![node("app", "parent", "1", "", Resolved, None)]);
        assert!(ide_evidence(&project, "/project").unwrap().is_none());
    }
    #[test]
    fn after_scan_audits_the_ide_graph_without_running_the_build_tool() {
        let h = ide_host(IDE_CAPABILITIES);
        h.set_build_dependencies(vec![
            node("fixture", "project", "1", "", DependencyStatus::Resolved, None),
            node("com.fasterxml.jackson.core", "jackson-databind", "2.12.3", "compile", DependencyStatus::Resolved, Some(0)),
        ]);
        let mut state = State::default();
        state.resolve(&h.invocation(), true, false).unwrap();
        assert!(h.process_runs().is_empty() && state.run.is_none());
        assert!(state.ide_note.as_ref().unwrap().starts_with("Resolved by the IDE"));
        let report = stored_report(&h.invocation().project().unwrap()).unwrap().unwrap();
        assert_eq!(report.rows[0].name, "com.fasterxml.jackson.core:jackson-databind");
        assert!(report.rows[0].sources[0].evidence.contains("Resolved by the IDE"));
    }
    #[test]
    fn after_scan_falls_back_to_the_build_tool_only_when_allowed_and_hints_when_the_ide_has_nothing() {
        let h = ide_host(IDE_CAPABILITIES);
        h.set_build_dependencies(vec![node("fixture", "project", "1", "", DependencyStatus::Resolved, None)]);
        let mut state = State::default();
        state.resolve(&h.invocation(), false, false).unwrap();
        assert!(h.process_runs().is_empty() && state.run.is_none());
        h.answer_next_prompt(Some(&[]));
        state.resolve(&h.invocation(), true, false).unwrap();
        assert!(state.running() && state.message.ends_with(IDE_HINT));
        let h = ide_host(&IDE_CAPABILITIES[..6]);
        let mut state = State::default();
        h.answer_next_prompt(Some(&[]));
        state.resolve(&h.invocation(), true, false).unwrap();
        assert!(state.running() && !state.message.contains(IDE_HINT));
    }
    #[test]
    fn resolve_command_prefers_the_ide_graph_and_runs_the_tool_only_without_one() {
        let h = ide_host(IDE_CAPABILITIES);
        h.set_setting("autoResolveBuilds", "false");
        h.set_build_dependencies(vec![
            node("fixture", "project", "1", "", DependencyStatus::Resolved, None),
            node("com.fasterxml.jackson.core", "jackson-databind", "2.12.3", "compile", DependencyStatus::Resolved, Some(0)),
        ]);
        let mut plugin = crate::DependencyAudit::default();
        plugin.run_command("resolve-build-dependencies", &h.invocation(), None).unwrap();
        assert!(h.process_runs().is_empty() && !plugin.build_resolution.running());
        assert!(plugin.build_resolution.ide_note.as_ref().unwrap().starts_with("Resolved by the IDE"));
        h.set_build_dependencies(vec![node("fixture", "project", "1", "", DependencyStatus::Resolved, None)]);
        plugin.run_command("resolve-build-dependencies", &h.invocation(), None).unwrap();
        assert!(plugin.build_resolution.running() && plugin.build_resolution.message.ends_with(IDE_HINT));
    }
    #[test]
    fn profiles_accept_only_recorded_official_tool_versions() {
        assert!(version_matches(
            Tool::Maven,
            "Apache Maven 3.9.16 (hash)\nJava version:26"
        ));
        assert!(version_matches(Tool::Gradle, "\nGradle 9.9.1\n"));
        assert!(!version_matches(Tool::Maven, "Apache Maven 4.0.0"));
        assert!(!version_matches(Tool::Gradle, "Gradle 9.0.0"));
    }
    #[test]
    fn a_wrong_tool_version_says_what_was_found() {
        let found = version_mismatch(Tool::Maven, "Apache Maven 3.8.1 (hash)").to_string();
        assert!(found.contains("Found Maven 3.8.1; needs Maven 3.9.16"), "{found}");
        let unreadable = version_mismatch(Tool::Gradle, "").to_string();
        assert!(unreadable.contains("needs Gradle 9.9.1"), "{unreadable}");
    }
    #[test]
    fn a_denied_build_tool_names_the_permission() {
        let h = openidea_plugin::mock::MockHost::install().with_capabilities(&["project.read", "project.write", "editor.read"]);
        h.add_file("/project/pom.xml", "<project/> ");
        h.answer_next_prompt(Some(&[]));
        let error = State::default().start(&h.invocation(), false).unwrap_err().to_string();
        assert!(error.contains("process:mvn permission"), "{error}");
    }
    fn resolver_host(extra: &[&str]) -> openidea_plugin::mock::MockHost {
        use openidea_plugin::{ProcessOutput, mock::MockHost};
        let mut caps = vec!["project.read", "project.write", "editor.read", "process:mvn"];
        caps.extend_from_slice(extra);
        let h = MockHost::install().with_capabilities(&caps);
        h.add_file("/project/pom.xml", "<project/> ");
        h.on_process("mvn", |args, _| {
            Ok(ProcessOutput {
                status: Some(0),
                stdout: if args.iter().any(|arg| arg == "--version") { "Apache Maven 3.9.16 (official)".into() } else { String::new() },
                stderr: String::new(),
            })
        });
        h
    }
    #[test]
    fn resolve_asks_once_per_project() {
        let h = resolver_host(&["storage"]);
        let mut state = State::default();
        state.start(&h.invocation(), false).unwrap();
        assert!(state.run.is_none() && h.process_runs().is_empty(), "declined means nothing runs");
        h.answer_next_prompt(Some(&[]));
        State::default().start(&h.invocation(), false).unwrap();
        assert!(h.process_runs().is_empty(), "a declined project is not asked again");

        let h = resolver_host(&["storage"]);
        h.answer_next_prompt(Some(&[]));
        State::default().start(&h.invocation(), false).unwrap();
        assert!(!h.process_runs().is_empty());
        let runs = h.process_runs().len();
        let mut second = State::default();
        second.start(&h.invocation(), false).unwrap();
        assert!(second.run.is_some() && h.process_runs().len() > runs, "an approved project is not asked again");
    }
    #[test]
    fn resolve_asks_every_time_when_storage_is_denied() {
        let h = resolver_host(&[]);
        h.answer_next_prompt(Some(&[]));
        State::default().start(&h.invocation(), false).unwrap();
        assert!(!h.process_runs().is_empty());
        let runs = h.process_runs().len();
        State::default().start(&h.invocation(), false).unwrap();
        assert_eq!(h.process_runs().len(), runs, "no remembered answer, so it asked and was declined");
    }
    #[test]
    fn explicit_resolve_does_not_ask_and_undoes_a_stored_no() {
        let h = resolver_host(&["storage"]);
        State::default().start(&h.invocation(), false).unwrap();
        assert!(h.process_runs().is_empty());
        let mut state = State::default();
        state.start(&h.invocation(), true).unwrap();
        assert!(state.run.is_some());
        let mut auto = State::default();
        auto.start(&h.invocation(), false).unwrap();
        assert!(auto.run.is_some(), "the explicit resolve approved the project");
    }
    #[test]
    fn resolve_dir_is_reused_and_never_accumulates() {
        let h = resolver_host(&["storage"]);
        let mut first = State::default();
        first.start(&h.invocation(), true).unwrap();
        let output = first.run.as_ref().unwrap().output.clone();
        assert!(output.starts_with("/project/.openidea/dependency-audit-resolve/"), "{output}");
        h.add_file(output.strip_prefix("/project/").unwrap(), "stale maven tree");
        h.add_file(".openidea/dependency-audit-resolve/export.gradle", "// owned");
        let mut second = State::default();
        second.start(&h.invocation(), true).unwrap();
        assert_eq!(second.run.as_ref().unwrap().output, output);
        let project = host::projects().unwrap().remove(0);
        assert_eq!(project.read_file(&output).unwrap(), "", "a previous run's output is cleared");
        assert!(source_files(&project, "/project").unwrap().keys().all(|path| !path.contains("dependency-audit-resolve")));
    }
}
