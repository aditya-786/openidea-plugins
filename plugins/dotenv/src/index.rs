//! What a project declares and reads: keys of `.env` files, Dockerfiles and
//! docker-compose files, and where code (or `${KEY}`) uses them. The original
//! plugin keeps this in IntelliJ's file index; here a project walk builds it
//! and saved files update it.

use crate::{code, docker, ownership, parse};
use openidea_plugin::{Error, Location, Project, Range, text};
use std::collections::{BTreeSet, HashSet};
use std::time::Instant;

/// Env files read in each folder that looks like a project. They are read
/// directly, because the walk may skip gitignored files and `.env` usually is.
pub const ENV_FILES: &[&str] = &[
    ".env",
    ".env.local",
    ".env.example",
    ".env.dist",
    ".env.sample",
    ".env.template",
    ".env.defaults",
    ".env.dev",
    ".env.development",
    ".env.development.local",
    ".env.test",
    ".env.test.local",
    ".env.testing",
    ".env.staging",
    ".env.prod",
    ".env.production",
    ".env.production.local",
    ".env.ci",
    ".env.docker",
    ".flaskenv",
];

/// Files whose folder gets [`ENV_FILES`] looked up, besides the root.
const MARKERS: &[&str] = &[
    "package.json",
    "composer.json",
    "pyproject.toml",
    "requirements.txt",
    "manage.py",
    "go.mod",
    "Gemfile",
    "Cargo.toml",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "artisan",
    "Dockerfile",
    "docker-compose.yml",
    "docker-compose.yaml",
    "compose.yml",
    "compose.yaml",
];

pub(crate) const CODE: &[&str] = &[
    "php", "py", "js", "jsx", "mjs", "cjs", "ts", "tsx", "mts", "cts", "vue", "svelte", "go", "rb", "rake", "rs", "java", "kt", "kts",
    "scala", "groovy", "cs",
];

/// Paths the walk returns at most.
const MAX_FILES: u32 = 50_000;
pub(crate) const MAX_SOURCE_BYTES: usize = 256 << 10;
const MAX_FACTS: usize = 50_000;

#[derive(Clone, Debug)]
pub struct Declaration {
    pub key: String,
    pub value: String,
    pub location: Location,
    /// From `.env`, a Dockerfile or a compose file; `.env.example` and the
    /// like only count for a key when no primary file has it.
    pub primary: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Usage {
    pub key: String,
    pub location: Location,
}

#[derive(Default)]
pub struct Index {
    /// The project was walked once; events only update single files after that.
    pub walked: bool,
    /// The listing a cut walk continues from; `None` once walked or not yet listed.
    walk: Option<Walk>,
    held_complete: bool,
    /// The walk saw everything: no cut-off, unreadable file or unresolved reference.
    pub complete: bool,
    /// Files whose failed read, size or unresolved reference keeps `complete` false; re-reading them can restore it.
    blockers: BTreeSet<String>,
    /// Incompleteness no single file's re-read can clear (fact budget, deadline).
    hard_incomplete: bool,
    pub declarations: Vec<Declaration>,
    pub usages: Vec<Usage>,
    /// Folders of indexed Compose files: the project directory whose `.env` Compose interpolates from.
    pub compose_folders: std::collections::HashSet<String>,
    /// Files some Compose `env_file:` names; their keys reach containers, not interpolation.
    pub env_file_paths: std::collections::HashSet<String>,
    pub loaders: Vec<EnvLoader>,
    pub arg_links: Vec<ArgLink>,
    /// (compose file, Dockerfile) pairs for every literal build target, even a missing Dockerfile.
    pub build_targets: Vec<(String, String)>,
}

/// A Compose service whose `env_file:` names `env_path`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvLoader {
    pub env_path: String,
    pub service: String,
    pub compose_path: String,
    pub line: usize,
}

/// A Dockerfile `ARG` that a service's `build.args` feeds from `${env_key}`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArgLink {
    pub env_key: String,
    pub arg: String,
    pub service: String,
    pub stage: Option<String>,
    pub based_on: Option<String>,
    pub dockerfile_path: String,
    pub dockerfile_line: usize,
    pub compose_path: String,
    pub compose_line: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Source {
    Env,
    Dockerfile,
    Compose,
    Code,
}

pub fn folder_of(path: &str) -> &str {
    path.rsplit_once(['/', '\\']).map_or("", |(folder, _)| folder)
}

/// The value as completion and hover show it: keys that look like secrets are masked.
pub fn shown_value<'a>(key: &str, value: &'a str) -> &'a str {
    let key = key.to_ascii_uppercase();
    match !value.is_empty() && ["PASS", "SECRET", "TOKEN", "KEY", "CREDENTIAL"].iter().any(|word| key.contains(word)) {
        true => "••••",
        false => value,
    }
}

pub fn is_secret_like(key: &str, value: &str) -> bool {
    !value.is_empty() && shown_value(key, value) != value
}

pub fn is_compose_file(path: &str) -> bool {
    source(path) == Some(Source::Compose)
}

pub fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

thread_local! {
    /// File names other plugins contributed to `aritti.dotenv.sources`: (name, declares).
    static EXTRA_SOURCES: std::cell::RefCell<Vec<(String, bool)>> = const { std::cell::RefCell::new(Vec::new()) };
}

pub fn set_extra_sources(sources: Vec<(String, bool)>) {
    EXTRA_SOURCES.with(|extra| *extra.borrow_mut() = sources);
}

fn extra_source(name: &str) -> Option<bool> {
    EXTRA_SOURCES.with(|extra| extra.borrow().iter().find(|(n, _)| n == name).map(|(_, declares)| *declares))
}

pub fn is_env_file(path: &str) -> bool {
    let name = file_name(path);
    extra_source(name) == Some(true)
        || name == ".env" || name == ".flaskenv" || name.starts_with(".env.") || name.ends_with(".env")
}

fn source(path: &str) -> Option<Source> {
    let name = file_name(path);
    if is_env_file(path) {
        return Some(Source::Env);
    }
    if extra_source(name) == Some(false) {
        return Some(Source::Code);
    }
    if name == "Dockerfile" || name.starts_with("Dockerfile.") || name.to_ascii_lowercase().ends_with(".dockerfile") {
        return Some(Source::Dockerfile);
    }
    if matches!(name, "docker-compose.yml" | "docker-compose.yaml" | "compose.yml" | "compose.yaml") {
        return Some(Source::Compose);
    }
    let code = name.starts_with("phpunit.xml") || name.rsplit_once('.').is_some_and(|(_, ext)| CODE.contains(&ext));
    code.then_some(Source::Code)
}

pub fn range(text: &str, bytes: std::ops::Range<usize>) -> Range {
    Range::new(text::position(text, bytes.start), text::position(text, bytes.end))
}

pub fn env_declarations(path: &str, text: &str) -> Vec<Declaration> {
    let primary = file_name(path) == ".env";
    let tokens = parse::lex(text);
    parse::properties(text, &tokens)
        .into_iter()
        .map(|p| Declaration {
            key: p.name(text),
            value: p.short_value(),
            location: Location { path: path.to_owned(), range: range(text, p.key.clone()) },
            primary,
        })
        .collect()
}

/// `${KEY}` references of a `.env` file.
pub fn nested_usages(path: &str, text: &str) -> Vec<Usage> {
    parse::lex(text)
        .into_iter()
        .filter(|t| t.kind == parse::Kind::NestedKey)
        .map(|t| Usage { key: text[t.range.clone()].to_owned(), location: Location { path: path.to_owned(), range: range(text, t.range) } })
        .collect()
}

fn code_usages(path: &str, text: &str) -> Vec<Usage> {
    let mut usages = Vec::new();
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let body = line.trim_end_matches(['\n', '\r']);
        // Minified code: one huge line, no `getenv` worth a quadratic scan.
        if body.len() <= 2000 {
            for key in code::keys_in_line(body) {
                usages.push(Usage {
                    key: body[key.clone()].to_owned(),
                    location: Location { path: path.to_owned(), range: range(text, offset + key.start..offset + key.end) },
                });
            }
        }
        offset += line.len();
    }
    usages
}

fn docker_declarations(path: &str, text: &str, entries: Vec<docker::Entry>) -> Vec<Declaration> {
    entries
        .into_iter()
        .map(|(key, value, r)| Declaration {
            key,
            value,
            location: Location { path: path.to_owned(), range: range(text, r) },
            primary: true,
        })
        .collect()
}

fn normalise(path: &str) -> Option<Vec<&str>> {
    let mut parts = Vec::new();
    for part in path.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    Some(parts)
}

/// `file` (absolute, or relative to `folder`) with `.` and `..` resolved, or
/// `None` when it ends up outside `root`.
fn confined_path(root: &str, folder: &str, file: &str) -> Option<String> {
    let joined = if file.starts_with('/') { file.to_owned() } else { format!("{folder}/{file}") };
    let root = normalise(root)?;
    let parts = normalise(&joined)?;
    if parts.len() <= root.len() || parts[..root.len()] != root[..] {
        return None;
    }
    Some(format!("/{}", parts.join("/")))
}

impl Index {
    fn add(&mut self, path: &str, text: &str) {
        if text.len() > MAX_SOURCE_BYTES {
            self.block(path);
            return;
        }
        match source(path) {
            Some(Source::Env) => {
                self.declarations.extend(env_declarations(path, text));
                self.usages.extend(nested_usages(path, text));
            }
            Some(Source::Dockerfile) => self.declarations.extend(docker_declarations(path, text, docker::dockerfile(text))),
            Some(Source::Compose) => {
                self.declarations.extend(docker_declarations(path, text, docker::compose(text)));
                self.usages.extend(docker::compose_interpolations(text).into_iter().map(|i| Usage {
                    key: i.name,
                    location: Location { path: path.to_owned(), range: range(text, i.range) },
                }));
                self.compose_folders.insert(folder_of(path).to_owned());
            }
            Some(Source::Code) => self.usages.extend(code_usages(path, text)),
            None => {}
        }
        self.bound_facts();
    }

    fn bound_facts(&mut self) {
        if self.declarations.len() > MAX_FACTS || self.usages.len() > MAX_FACTS {
            self.complete = false;
            self.hard_incomplete = true;
            self.declarations.truncate(MAX_FACTS);
            self.usages.truncate(MAX_FACTS);
        }
    }

    /// Reads the files a compose file names in `env_file:` and declares their
    /// keys, unless the index already has that file. Missing or unreadable
    /// explicitly referenced files leave discovery incomplete.
    pub fn add_compose_env_files(&mut self, project: &Project, root: &str, compose_path: &str, text: &str) {
        self.compose_env_files_before(project, root, compose_path, text, Instant::now() + std::time::Duration::from_secs(1));
        if !self.complete {
            self.held_complete = false;
        }
    }

    /// Something could not be read: a walk in progress must not finish complete.
    pub fn mark_incomplete(&mut self, path: &str) {
        self.held_complete = false;
        self.block(path);
    }

    fn block(&mut self, path: &str) {
        self.complete = false;
        self.blockers.insert(path.to_owned());
    }

    /// Drops every fact under the folder `prefix`, which was removed.
    pub fn forget_under(&mut self, prefix: &str) {
        let under = format!("{}/", prefix.trim_end_matches(['/', '\\']));
        self.declarations.retain(|d| !d.location.path.starts_with(&under));
        self.usages.retain(|u| !u.location.path.starts_with(&under));
        self.loaders.retain(|l| !l.compose_path.starts_with(&under));
        self.arg_links.retain(|l| !l.compose_path.starts_with(&under));
        self.build_targets.retain(|(compose, _)| !compose.starts_with(&under));
        self.compose_folders.retain(|f| !format!("{f}/").starts_with(&under));
        self.env_file_paths.retain(|f| !f.starts_with(&under));
        self.blockers.retain(|f| !f.starts_with(&under));
        self.restore_completeness();
    }

    fn restore_completeness(&mut self) {
        if self.walked && self.blockers.is_empty() && !self.hard_incomplete {
            self.complete = true;
        }
    }

    fn compose_env_files_before(&mut self, project: &Project, root: &str, compose_path: &str, text: &str, deadline: Instant) {
        if source(compose_path) != Some(Source::Compose) {
            return;
        }
        // Resolve env_file aliases/merges with explicit parser budgets. The
        // completeness bit also checks that every expanded environment key has
        // a physical declaration covered by the source-range model.
        let folder = compose_path.rsplit_once(['/', '\\']).map_or("", |(folder, _)| folder);
        self.add_ownership(project, root, compose_path, text);
        let (files, graph_complete, optional) = docker::compose_env_file_graph(text);
        if !graph_complete { self.block(compose_path); }
        if files.len() > 128 {
            self.block(compose_path);
        }
        for file in files.into_iter().take(128) {
            if Instant::now() >= deadline {
                self.block(compose_path);
                break;
            }
            if file.contains('$') || file.len() > 4096 {
                self.block(compose_path);
                continue;
            }
            let Some(path) = confined_path(root, folder, &file) else {
                self.block(compose_path);
                continue;
            };
            self.env_file_paths.insert(path.clone());
            if self.declarations.iter().any(|d| d.location.path == path) {
                continue;
            }
            match project.read_file(&path) {
                Ok(env) if env.len() <= MAX_SOURCE_BYTES => {
                    self.declarations.extend(env_declarations(&path, &env).into_iter().map(|d| Declaration { primary: true, ..d }))
                }
                Err(openidea_plugin::Error::NotFound(_)) if optional.contains(&file) => {},
                _ => self.block(&path),
            }
            self.bound_facts();
        }
    }

    fn add_ownership(&mut self, project: &Project, root: &str, compose_path: &str, text: &str) {
        let folder = folder_of(compose_path);
        let mut seen_usages: Option<HashSet<(String, String, Range)>> = None;
        for service in ownership::compose_services(text) {
            for (file, line) in &service.env_files {
                let Some(env_path) = confined_path(root, folder, file).filter(|_| !file.contains('$')) else { continue };
                let loader = EnvLoader { env_path, service: service.name.clone(), compose_path: compose_path.to_owned(), line: *line };
                if !self.loaders.contains(&loader) {
                    self.loaders.push(loader);
                }
            }
            if service.build_args.is_empty() {
                continue;
            }
            let context = service.context.as_deref().unwrap_or(".");
            let dockerfile = service.dockerfile.as_deref().unwrap_or("Dockerfile");
            if [context, dockerfile].iter().any(|p| p.contains('$') || p.contains("://") || p.starts_with("git@")) {
                continue;
            }
            let relative = if dockerfile.starts_with('/') { dockerfile.to_owned() } else { format!("{context}/{dockerfile}") };
            let Some(dockerfile_path) = confined_path(root, folder, &relative) else { continue };
            self.build_targets.push((compose_path.to_owned(), dockerfile_path.clone()));
            let Ok(dockerfile_text) = project.read_file(&dockerfile_path) else { continue };
            if dockerfile_text.len() > MAX_SOURCE_BYTES {
                continue;
            }
            let args = ownership::dockerfile_args(&dockerfile_text);
            for (arg, env_key, compose_line) in &service.build_args {
                let reimported = args.iter().any(|a| &a.name == arg && a.stage.is_some() && a.reimports_global);
                for declared in args.iter().filter(|a| &a.name == arg && (a.stage.is_some() || reimported)) {
                    let spans = std::iter::once(&declared.range).chain(&declared.uses);
                    for (i, span) in spans.enumerate() {
                        let location = Location { path: dockerfile_path.clone(), range: range(&dockerfile_text, span.clone()) };
                        if i == 0 {
                            let link = ArgLink {
                                env_key: env_key.clone(),
                                arg: arg.clone(),
                                service: service.name.clone(),
                                stage: declared.stage.clone(),
                                based_on: declared.based_on.clone(),
                                dockerfile_path: dockerfile_path.clone(),
                                dockerfile_line: location.range.start.line as usize + 1,
                                compose_path: compose_path.to_owned(),
                                compose_line: *compose_line,
                            };
                            if !self.arg_links.contains(&link) {
                                self.arg_links.push(link);
                            }
                        }
                        let seen = seen_usages.get_or_insert_with(|| self.usages.iter().map(|u| (u.location.path.clone(), u.key.clone(), u.location.range)).collect());
                        if seen.insert((location.path.clone(), env_key.clone(), location.range)) {
                            self.usages.push(Usage { key: env_key.clone(), location });
                        }
                    }
                }
            }
        }
        self.bound_facts();
    }

    /// Replaces what `path` declares and uses with `text` (`None`: it is gone).
    pub fn update(&mut self, path: &str, text: Option<&str>) {
        self.declarations.retain(|d| d.location.path != path);
        self.usages.retain(|u| u.location.path != path);
        self.loaders.retain(|l| l.compose_path != path);
        self.arg_links.retain(|l| l.compose_path != path);
        self.build_targets.retain(|(compose, _)| compose != path);
        self.blockers.remove(path);
        if let Some(text) = text {
            self.add(path, text);
        }
        self.restore_completeness();
    }

    /// Compose files whose build `dockerfile` is `path`.
    pub fn composes_building(&self, path: &str) -> Vec<String> {
        let mut composes: Vec<String> = self.build_targets.iter().filter(|(_, d)| d == path).map(|(c, _)| c.clone()).collect();
        composes.dedup();
        composes
    }

    pub fn indexes(path: &str) -> bool {
        source(path).is_some()
    }

    /// The env files at the root only: quick enough for an editor request.
    pub fn quick(project: &Project, root: &str) -> Self {
        let mut index = Self::default();
        for name in ENV_FILES {
            let path = format!("{}/{name}", root.trim_end_matches(['/', '\\']));
            if let Ok(text) = project.read_file(&path) {
                index.add(&path, &text);
            }
        }
        index
    }

    /// Every file that declares or reads keys, until `deadline`. A walk cut by
    /// the deadline keeps its place; [`Index::resume`] continues it.
    pub fn full(project: &Project, root: &str, deadline: Instant) -> Self {
        let mut index = Self::begin(project, root, deadline);
        index.resume(project, root, deadline);
        index
    }

    /// Lists the project once. Reading the listed files is [`Index::resume`]'s job.
    pub fn begin(project: &Project, root: &str, deadline: Instant) -> Self {
        if Instant::now() >= deadline {
            return Self::default();
        }
        let mut files = match project.find_files("**/*", MAX_FILES + 1) {
            Ok(files) => files,
            Err(_) => return Self::default(),
        };
        let discovery_complete = files.len() <= MAX_FILES as usize;
        files.truncate(MAX_FILES as usize);
        let discovered: BTreeSet<_> = files.iter().cloned().collect();
        let mut folders = BTreeSet::from([root.trim_end_matches(['/', '\\']).to_owned()]);
        for file in &files {
            if MARKERS.contains(&file_name(file)) {
                if let Some((folder, _)) = file.rsplit_once(['/', '\\']) {
                    folders.insert(folder.to_owned());
                }
            }
        }
        // Known root/marker env filenames are optional probes; disappearance of
        // a file actually discovered by the walk is an incomplete/racing scan.
        let mut paths: Vec<(Source, String)> = folders
            .iter()
            .flat_map(|folder| ENV_FILES.iter().map(move |name| (Source::Env, format!("{folder}/{name}"))))
            .chain(files.into_iter().filter_map(|f| Some((source(&f)?, f))))
            .collect();
        paths.sort();
        paths.dedup();
        Self {
            complete: discovery_complete,
            hard_incomplete: !discovery_complete,
            walk: Some(Walk { paths: paths.into_iter().map(|(_, path)| path).collect(), next: 0, discovered, touched: HashSet::new() }),
            ..Self::default()
        }
    }

    /// Reads the listed files from the cursor until `deadline`. Finishing the list sets `walked`.
    pub fn resume(&mut self, project: &Project, root: &str, deadline: Instant) {
        self.resume_limited(project, root, deadline, usize::MAX);
    }

    fn resume_limited(&mut self, project: &Project, root: &str, deadline: Instant, max_reads: usize) {
        let Some(mut walk) = self.walk.take() else { return };
        self.complete = self.complete || self.held_complete;
        let mut reads = 0;
        while walk.next < walk.paths.len() {
            let path = walk.paths[walk.next].clone();
            if walk.touched.contains(&path) {
                walk.next += 1;
                continue;
            }
            if reads >= max_reads || Instant::now() >= deadline {
                self.held_complete = self.complete;
                self.complete = false;
                self.walk = Some(walk);
                return;
            }
            reads += 1;
            walk.next += 1;
            match project.read_file(&path) {
                Ok(text) => {
                    self.add(&path, &text);
                    if text.len() <= MAX_SOURCE_BYTES {
                        self.compose_env_files_before(project, root, &path, &text, deadline);
                    }
                }
                Err(Error::NotFound(_)) if !walk.discovered.contains(&path) => {}
                Err(_) => self.block(&path),
            }
            if self.declarations.len() >= MAX_FACTS || self.usages.len() >= MAX_FACTS {
                self.complete = false;
                self.hard_incomplete = true;
                break;
            }
        }
        self.walked = true;
    }

    /// Marks `path` as already brought up to date, so a walk in progress doesn't add it twice.
    pub fn touched(&mut self, path: &str) {
        if let Some(walk) = &mut self.walk {
            walk.touched.insert(path.to_owned());
        }
    }

    pub fn walk_in_progress(&self) -> bool {
        self.walk.is_some()
    }
}

/// The listing of a walk and how far reading it got.
struct Walk {
    paths: Vec<String>,
    next: usize,
    discovered: BTreeSet<String>,
    touched: HashSet<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::Position;
    use openidea_plugin::mock::MockHost;
    use std::time::Duration;

    #[test]
    fn indexes_the_project() {
        let host = MockHost::install();
        host.set_project_root(Some("/p"));
        host.add_file(".env", "A=1\nB=\"${A}\"\n");
        host.add_file("api/composer.json", "{}");
        host.add_file("api/.env.example", "C=3\n");
        host.add_file("Dockerfile", "ENV D=4\n");
        host.add_file(
            "deploy/docker-compose.yml",
            "services:\n  x:\n    env_file: [\"./conf/vars\", missing.env]\n    environment:\n      - E=5\n",
        );
        host.add_file("deploy/conf/vars", "F=6\n");
        host.add_file("api/index.php", "<?php\necho getenv('A') . $_ENV['C'];\n");
        host.add_file("README.md", "getenv('NOPE')");
        let project = host.invocation().project().unwrap();

        let index = Index::full(&project, "/p", Instant::now() + Duration::from_secs(5));
        let mut keys: Vec<_> = index.declarations.iter().map(|d| (d.key.as_str(), d.primary)).collect();
        keys.sort();
        assert_eq!(keys, [("A", true), ("B", true), ("C", false), ("D", true), ("E", true), ("F", true)]);
        let usages: Vec<_> = index.usages.iter().map(|u| (u.key.as_str(), file_name(&u.location.path))).collect();
        assert_eq!(usages, [("A", ".env"), ("A", "index.php"), ("C", "index.php")]);
        assert_eq!(index.usages[1].location.range, Range::new(Position::new(1, 13), Position::new(1, 14)));

        let quick = Index::quick(&project, "/p");
        assert_eq!(quick.declarations.len(), 2, "only the root's .env");
    }

    #[test]
    fn completeness_requires_discovery_budget_and_readable_supported_sources() {
        let host = MockHost::install();
        host.set_project_root(Some("/p"));
        let project = host.invocation().project().unwrap();
        assert!(
            Index::full(&project, "/p", Instant::now() + Duration::from_secs(5)).complete,
            "missing optional conventional files are normal"
        );
        assert!(!Index::full(&project, "/p", Instant::now() - Duration::from_secs(1)).complete);
        host.add_file(".env", &"#".repeat(MAX_SOURCE_BYTES + 1));
        assert!(!Index::full(&project, "/p", Instant::now() + Duration::from_secs(5)).complete);
        let _denied = host.with_capabilities(&[]);
        assert!(!Index::full(&project, "/p", Instant::now() + Duration::from_secs(5)).complete);
    }

    #[test]
    fn ignore_files_do_not_imply_incompleteness() {
        let host = MockHost::install();
        host.set_project_root(Some("/p"));
        host.add_file(".env", "A=1\n");
        host.add_file(".gitignore", "*.env\n");
        let project = host.invocation().project().unwrap();
        let index = Index::full(&project, "/p", Instant::now() + Duration::from_secs(5));
        assert!(index.complete && index.walked);
        assert!(index.declarations.iter().any(|d| d.key == "A"));
    }

    #[test]
    fn expired_deadline_leaves_the_walk_unfinished() {
        let host = MockHost::install();
        host.set_project_root(Some("/p"));
        host.add_file(".env", "A=1\n");
        let project = host.invocation().project().unwrap();
        assert!(!Index::full(&project, "/p", Instant::now() - Duration::from_secs(1)).walked);
        assert!(Index::full(&project, "/p", Instant::now() + Duration::from_secs(5)).walked);
    }

    #[test]
    fn a_cut_walk_resumes_from_its_cursor_without_rereading() {
        let host = MockHost::install();
        host.set_project_root(Some("/p"));
        for n in 0..10 {
            host.add_file(&format!("f{n}.py"), &format!("os.getenv('K{n}')\n"));
        }
        let project = host.invocation().project().unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut index = Index::begin(&project, "/p", deadline);
        let total = index.walk.as_ref().unwrap().paths.len();
        let part = 4;
        let mut batches = 0;
        let mut cursor = 0;
        while !index.walked {
            index.resume_limited(&project, "/p", deadline, part);
            batches += 1;
            let reached = index.walk.as_ref().map_or(total, |w| w.next);
            assert_eq!(reached, (cursor + part).min(total), "each batch reads exactly its share");
            cursor = reached;
            assert!(!index.complete || index.walked, "a partial index never claims completeness");
        }
        assert_eq!(batches, total.div_ceil(part));
        assert_eq!(index.usages.len(), 10, "no file read twice");
        assert!(index.complete && !index.walk_in_progress());
    }

    #[test]
    fn completeness_returns_once_the_blocking_file_is_fixed() {
        let host = MockHost::install();
        host.set_project_root(Some("/p"));
        host.add_file(".env", &format!("A={}\n", "x".repeat(MAX_SOURCE_BYTES)));
        let project = host.invocation().project().unwrap();
        let mut index = Index::full(&project, "/p", Instant::now() + Duration::from_secs(5));
        assert!(index.walked && !index.complete);
        index.update("/p/.env", Some("A=1\n"));
        assert!(index.complete);
    }

    #[test]
    fn an_incomplete_mark_during_a_cut_walk_survives_the_resume() {
        let host = MockHost::install();
        host.set_project_root(Some("/p"));
        for n in 0..6 {
            host.add_file(&format!("f{n}.py"), "os.getenv('K')\n");
        }
        let project = host.invocation().project().unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut index = Index::begin(&project, "/p", deadline);
        index.resume_limited(&project, "/p", deadline, 2);
        index.mark_incomplete("/p/f0.py");
        index.resume(&project, "/p", deadline);
        assert!(index.walked && !index.complete);
    }

    #[test]
    fn truncated_inventory_never_proves_absence() {
        let host = MockHost::install();
        host.set_project_root(Some("/p"));
        for n in 0..=MAX_FILES {
            host.add_file(&format!("unrelated/{n}.txt"), "");
        }
        let project = host.invocation().project().unwrap();
        let mut index = Index::full(&project, "/p", Instant::now() + Duration::from_secs(10));
        assert!(!index.complete);
        index.update("/p/.env", Some("A=1\n"));
        assert!(!index.complete);
        index.forget_under("/p/unrelated");
        assert!(!index.complete);
    }

    #[test]
    fn unresolved_compose_inputs_leave_discovery_unknown() {
        for name in ["missing.env", "${CONFIG}", "../../outside.env", "*shared"] {
            let index = compose_index(name);
            assert!(!index.complete, "{name}");
            assert!(index.declarations.iter().any(|d| d.key == "A"), "retain known declarations");
        }
        let index = compose_index("../.env");
        assert!(index.complete, "resolvable explicitly named files are complete");
    }

    #[test]
    fn env_file_paths_are_confined_to_the_root() {
        assert_eq!(confined_path("/p", "/p/deploy", "../.env").as_deref(), Some("/p/.env"));
        assert_eq!(confined_path("/p/", "/p/deploy", "./a/../x.env").as_deref(), Some("/p/deploy/x.env"));
        assert_eq!(confined_path("/p", "/p/deploy", "/p/y.env").as_deref(), Some("/p/y.env"));
        assert_eq!(confined_path("/p", "/p/deploy", "../../outside.env"), None);
        assert_eq!(confined_path("/p", "/p/deploy", "/etc/passwd"), None);
        assert_eq!(confined_path("/p", "/p/deploy", "../../../x"), None);
    }

    fn compose_index(env_file: &str) -> Index {
        let host = MockHost::install();
        host.set_project_root(Some("/p"));
        host.add_file(".env", "A=1\n");
        host.add_file("/outside.env", "OUT=1\n");
        host.add_file("deploy/docker-compose.yml", &format!("services:\n  x:\n    env_file: {env_file}\n"));
        let project = host.invocation().project().unwrap();
        Index::full(&project, "/p", Instant::now() + Duration::from_secs(5))
    }

    #[test]
    fn env_file_parent_dir_path_is_normalised_and_deduped() {
        let index = compose_index("../.env");
        let found: Vec<_> = index.declarations.iter().map(|d| (d.key.as_str(), d.location.path.as_str())).collect();
        assert_eq!(found, [("A", "/p/.env")]);
    }

    #[test]
    fn env_file_escaping_the_root_is_ignored() {
        for path in ["../../outside.env", "/outside.env", "/p/../outside.env"] {
            let index = compose_index(path);
            assert!(index.declarations.iter().all(|d| d.key != "OUT"), "{path}");
        }
    }
}

#[cfg(test)]
mod optional_input_tests {
    use super::*;
    use std::time::Duration;
    use openidea_plugin::mock::MockHost;
    #[test]
    fn resolved_alias_and_missing_optional_files_are_complete_source_discovery() {
        let h=MockHost::install();h.set_project_root(Some("/p"));h.add_file(".env","A=1\n");
        h.add_file("compose.yml","x-file: &file {path: missing.env, required: false}\nx-service: &service {env_file: [*file]}\nservices: {app: {<<: *service}}\n");
        let project=h.invocation().project().unwrap();let index=Index::full(&project,"/p",Instant::now()+Duration::from_secs(5));
        assert!(index.complete);assert!(index.declarations.iter().any(|d|d.key=="A"));
    }

    #[test]
    fn arg_links_cover_reimported_globals_and_stage_uses() {
        let host = MockHost::install();
        host.set_project_root(Some("/p"));
        host.add_file(".env", "V=1\n");
        host.add_file("compose.yaml", "services:\n  w:\n    build:\n      args:\n        V: ${V}\n");
        host.add_file("Dockerfile", "ARG V=1\nFROM php AS a\nFROM a AS b\nARG V\nRUN echo $V\n");
        let project = host.invocation().project().unwrap();
        let index = Index::full(&project, "/p", Instant::now() + Duration::from_secs(5));
        let links: Vec<_> = index.arg_links.iter().map(|l| (l.stage.clone(), l.based_on.clone(), l.dockerfile_line)).collect();
        assert_eq!(links, [(None, None, 1), (Some("b".into()), Some("a".into()), 4)]);
        let lines: Vec<_> = index.usages.iter().filter(|u| u.location.path.ends_with("Dockerfile")).map(|u| u.location.range.start.line).collect();
        assert_eq!(lines, [0, 3, 4]);
    }
}
