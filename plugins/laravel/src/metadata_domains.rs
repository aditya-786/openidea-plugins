//! Owned, bounded inventories for declarative completion domains. No PHP executes.
use super::{Metadata,evidence};
use crate::index::{AddName, Index, Names};
use openidea_plugin::{Position, host::Project};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

const FILES: u32 = 1024;
const BYTES: usize = 128 * 1024;
const TOTAL: usize = 4 * 1024 * 1024;
const NAMES: usize = 8192;

pub(super) fn supported(category: &str) -> bool {
    matches!(
        category,
        "routeAction"
            | "moduleName"
            | "directoryFiles"
            | "cssClassName"
            | "cssClassNames"
            | "bladeIcon"
    )
}
pub(super) fn validate(category: &str, options: &Value) -> Vec<String> {
    let mut errors = Vec::new();
    if category != "directoryFiles" {
        return errors;
    }
    let one = options.get("directory");
    let many = options.get("directories");
    if one.is_some() == many.is_some() {
        errors.push("directoryFiles requires exactly one of directory or directories".into());
    }
    if one.is_some_and(|v| !v.is_string() || v.as_str() == Some("")) {
        errors.push("directory must be a nonempty string".into());
    }
    if many.is_some_and(|v| {
        v.as_array().is_none_or(|a| {
            a.is_empty()
                || a.len() > 64
                || a.iter().any(|v| !v.is_string() || v.as_str() == Some(""))
        })
    }) {
        errors.push("directories must contain 1–64 nonempty strings".into());
    }
    if options.get("recursive").is_some_and(|v| !v.is_boolean()) {
        errors.push("recursive must be boolean".into());
    }
    if options.get("suffixToClear").is_some_and(|v| !v.is_string()) {
        errors.push("suffixToClear must be a string".into());
    }
    errors
}
fn key(category: &str, source: &str, options: &Value) -> String {
    format!("{category}\0{source}\0{options}")
}
pub(super) fn inventory(
    index: &Index,
    category: &str,
    options: &Value,
    metadata_source: &str,
    _source: &str,
    _at: usize,
    _receiver: Option<&str>,
) -> Names {
    if category == "routeAction" {
        return crate::actions(index);
    }
    index
        .metadata
        .domain_names
        .get(&key(category, metadata_source, options))
        .cloned()
        .unwrap_or_default()
}
fn add(names: &mut Names, name: &str, path: &str, line: u32, width: usize) {
    add_at(names, name, path, Position::new(line, 0), width);
}
fn add_at(names: &mut Names, name: &str, path: &str, at: Position, width: usize) {
    if names.len() >= NAMES || name.is_empty() {
        return;
    }
    if let Some(existing) = names.get_mut(name) {
        if existing.path != path {
            existing.path.clear();
            existing.width = 0;
            existing.detail = Some("Ambiguous project sources".into());
        }
    } else {
        names.add(
            name,
            Some("Project metadata inventory".into()),
            path,
            at,
            width,
        );
    }
}
struct Scan<'a> {
    project: &'a Project,
    root: String,
    total: usize,
    queries: usize,
    listed: usize,
    inputs: BTreeSet<String>,
    source_evidence: evidence::SourceEvidence,
    source_evidence_incomplete: bool,
    source_queries: BTreeMap<(String,u32),Option<Vec<String>>>,
    roots: BTreeSet<String>,
    issues: Vec<String>,
}
impl Scan<'_> {
    fn files(&mut self, glob: &str) -> Vec<String> {
        if self.queries >= 64 || self.listed >= 4096 {
            if self.queries == 64 {
                self.issues.push("metadata inventories exceed aggregate enumeration budget; remaining scans withheld".into());
            }
            self.queries += 1;
            return Vec::new();
        }
        self.queries += 1;
        let result = self.project.find_files(glob, FILES + 1);
        let captured = result.as_ref().ok().map(|paths| {
            let mut owned:Vec<_>=paths.iter().filter(|path|crate::owns_current_path(self.project,path)).cloned().collect();owned.sort();owned.dedup();owned
        });
        if captured.is_none() { self.source_evidence_incomplete = true; }
        evidence::merge(&mut self.source_queries,&mut self.source_evidence_incomplete,[((glob.into(),FILES+1),captured)]);
        match result {
            Ok(files)
                if files.len() <= FILES as usize
                    && self.listed.saturating_add(files.len()) <= 4096 =>
            {
                self.listed += files.len();
                files
                    .into_iter()
                    .filter(|p| crate::owns_current_path(self.project, p))
                    .collect()
            }
            Ok(_) => {
                self.issues.push(format!(
                    "metadata inventory {glob}: exceeds {FILES} files; withheld"
                ));
                Vec::new()
            }
            Err(_) => {
                self.issues.push(format!(
                    "metadata inventory {glob}: file enumeration unavailable"
                ));
                Vec::new()
            }
        }
    }
    fn read(&mut self, path: &str) -> Option<String> {
        self.read_limit(path, BYTES)
    }
    fn read_limit(&mut self, path: &str, limit: usize) -> Option<String> {
        if !crate::owns_current_path(self.project, path) {
            self.source_evidence_incomplete = true;
            return None;
        }
        let relative = path.strip_prefix(&format!("{}/", self.root))?;
        self.inputs.insert(relative.into());
        let remaining = TOTAL.saturating_sub(self.total);
        let content = match evidence::read(self.project,&self.root,relative,limit.min(remaining),&mut self.source_evidence,&mut self.source_evidence_incomplete) {
            Some(content)=>content,
            None=>{ if self.project.stat(relative).is_ok_and(|stat|stat.size>limit.min(remaining) as u64) { self.issues.push(format!("metadata inventory {relative}: content budget exceeded")); } return None; }
        };
        self.total += content.len();
        Some(content)
    }
}
pub(super) fn cache(project: &Project, metadata: &mut Metadata) {
    let Ok(root) = project.root() else { return };
    let mut scan = Scan {
        project,
        root: root.trim_end_matches('/').into(),
        total: 0,
        queries: 0,
        listed: 0,
        inputs: BTreeSet::new(),
        source_evidence: BTreeMap::new(),
        source_evidence_incomplete: false,
        source_queries: BTreeMap::new(),
        roots: BTreeSet::new(),
        issues: Vec::new(),
    };
    let options = metadata.domain_options.clone();
    let mut css = None;
    let mut modules = None;
    let mut icons = None;
    let mut stored = 0usize;
    for (source, category, options) in options {
        if metadata.domain_names.len() >= 128 || stored >= 32768 {
            scan.issues.push(
                "metadata inventories exceed aggregate cache budget; remaining rules withheld"
                    .into(),
            );
            break;
        }
        if !validate(&category, &options).is_empty() {
            continue;
        }
        let names = match category.as_str() {
            "directoryFiles" => directory(&mut scan, &source, &options),
            "cssClassName" | "cssClassNames" => {
                css.get_or_insert_with(|| css_inventory(&mut scan)).clone()
            }
            "moduleName" => modules
                .get_or_insert_with(|| module_inventory(&mut scan))
                .clone(),
            "bladeIcon" => icons
                .get_or_insert_with(|| icon_inventory(&mut scan))
                .clone(),
            _ => continue,
        };
        if names.len() >= NAMES {
            scan.issues.push(format!(
                "{source}: {category} reached {NAMES}-name cap; inventory may be incomplete"
            ));
        }
        if stored.saturating_add(names.len()) > 32768 {
            scan.issues.push(format!(
                "{source}: {category} exceeds aggregate cache budget; withheld"
            ));
            continue;
        }
        stored += names.len();
        metadata
            .domain_names
            .insert(key(&category, &source, &options), names);
    }
    metadata.domain_roots.extend(scan.roots);
    evidence::merge(&mut metadata.source_evidence,&mut metadata.source_evidence_incomplete,scan.source_evidence);
    metadata.source_evidence_incomplete |= scan.source_evidence_incomplete;
    evidence::merge(&mut metadata.source_queries,&mut metadata.source_evidence_incomplete,scan.source_queries);
    metadata.inputs.extend(scan.inputs);
    metadata.issues.extend(scan.issues);
}
fn directory(scan: &mut Scan<'_>, source: &str, options: &Value) -> Names {
    let mut names = Names::new();
    let directories: Vec<&str> = options["directory"]
        .as_str()
        .map(|d| vec![d])
        .unwrap_or_else(|| {
            options["directories"]
                .as_array()
                .map(|a| a.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default()
        });
    let recursive = options["recursive"].as_bool().unwrap_or(true);
    let suffix = options["suffixToClear"].as_str();
    for directory in directories {
        let resolved = if directory == "/" || (directory == "." && !source.contains('/')) {
            Some(String::new())
        } else {
            super::resolve_path(source, directory)
        };
        let Some(relative) = resolved else {
            scan.issues.push(format!(
                "{source}: directoryFiles path escapes project or is unsupported: {directory}"
            ));
            continue;
        };
        scan.roots.insert(relative.clone());
        if suffix.is_none() {
            empty_directories(scan, &relative, recursive, &mut names);
        }
        let glob = if relative.is_empty() {
            "**/*".into()
        } else {
            format!("{relative}/**/*")
        };
        for path in scan.files(&glob) {
            let base = if relative.is_empty() {
                format!("{}/", scan.root)
            } else {
                format!("{}/{relative}/", scan.root)
            };
            let Some(name) = path.strip_prefix(&base) else {
                continue;
            };
            let parts: Vec<_> = name.split('/').collect();
            if suffix.is_none() {
                for length in 1..parts.len() {
                    if !recursive && length > 1 {
                        break;
                    }
                    let dir = parts[..length].join("/");
                    add(&mut names, &dir, &format!("{}{dir}", base), 0, 0);
                }
            }
            if !recursive && parts.len() > 1 {
                continue;
            }
            let name = if let Some(suffix) = suffix {
                let Some(name) = name.strip_suffix(suffix) else {
                    continue;
                };
                name
            } else {
                name
            };
            add(&mut names, name, &path, 0, 0);
        }
    }
    names
}
/// Directory enumeration has its own optional scoped filesystem grant. A denied
/// grant leaves the Project.find_files inventory usable, with an honest partial
/// diagnostic. Symlinks and nested projects are never traversed.
fn empty_directories(scan: &mut Scan<'_>, relative: &str, recursive: bool, names: &mut Names) {
    // Granted directory listings include empty directories, which find_files cannot replay.
    // Until directory-entry evidence is represented, do not certify this snapshot reusable.
    scan.source_evidence_incomplete = true;
    directory_walk(
        scan,
        relative,
        recursive,
        names,
        &mut openidea_plugin::host::fs::read_dir,
    );
}
fn directory_walk(
    scan: &mut Scan<'_>,
    relative: &str,
    recursive: bool,
    names: &mut Names,
    read: &mut impl FnMut(
        &str,
    )
        -> Result<Vec<openidea_plugin::host::fs::DirEntry>, openidea_plugin::Error>,
) {
    use openidea_plugin::FileKind;
    let base = if relative.is_empty() {
        scan.root.clone()
    } else {
        format!("{}/{relative}", scan.root)
    };
    let mut pending = vec![(base.clone(), String::new(), 0usize)];
    let mut visited = BTreeSet::new();
    while let Some((path, prefix, depth)) = pending.pop() {
        if !visited.insert(path.clone()) || !crate::owns_current_path(scan.project, &path) {
            continue;
        }
        if !scan
            .project
            .stat(&path)
            .is_ok_and(|s| s.kind == FileKind::Directory)
        {
            scan.issues.push(format!("directoryFiles {relative}: directory identity changed or symlink; inventory is partial"));
            continue;
        }
        if scan.queries >= 64 || scan.listed >= 4096 || names.len() >= NAMES {
            scan.issues.push(format!(
                "directoryFiles {relative}: directory walk budget reached; inventory is partial"
            ));
            return;
        }
        scan.queries += 1;
        let entries = match read(&path) {
            Ok(v) => v,
            Err(openidea_plugin::Error::Denied(_)) => {
                scan.issues.push(format!("directoryFiles {relative}: scoped filesystem read grant unavailable; file-derived fallback omits empty directories"));
                return;
            }
            Err(_) => {
                scan.issues.push(format!(
                    "directoryFiles {relative}: directory listing unavailable; inventory is partial"
                ));
                continue;
            }
        };
        if entries.len() >= 4096 || scan.listed.saturating_add(entries.len()) > 4096 {
            scan.issues.push(format!(
                "directoryFiles {relative}: directory entry budget reached; inventory is partial"
            ));
            return;
        }
        scan.listed += entries.len();
        for entry in entries {
            if entry.kind != FileKind::Directory
                || entry.name.is_empty()
                || entry.name == "."
                || entry.name == ".."
                || entry.name.contains(['/', '\\', '\0', '\n', '\r'])
            {
                continue;
            }
            let child = format!("{path}/{}", entry.name);
            if !crate::owns_current_path(scan.project, &child) {
                continue;
            }
            let name = if prefix.is_empty() {
                entry.name
            } else {
                format!("{prefix}/{}", entry.name)
            };
            add(names, &name, &child, 0, 0);
            if recursive {
                if depth >= 31 {
                    scan.issues.push(format!(
                        "directoryFiles {relative}: depth limit reached; inventory is partial"
                    ));
                } else {
                    pending.push((child, name, depth + 1));
                }
            }
        }
    }
}

/// Called once after package indexing with only Composer-declared, PSR-4/class-
/// proven provider sources. Inventory callbacks never read provider files.
pub(super) fn cache_package_icons(
    project: &Project,
    metadata: &mut Metadata,
    sources: &BTreeMap<String, String>,
) {
    if !metadata
        .domain_options
        .iter()
        .any(|(_, category, _)| category == "bladeIcon")
    {
        return;
    }
    let Ok(root) = project.root() else { return };
    let mut scan = Scan {
        project,
        root: root.trim_end_matches('/').into(),
        total: 0,
        queries: 0,
        listed: 0,
        inputs: BTreeSet::new(),
        source_evidence: BTreeMap::new(),
        source_evidence_incomplete: false,
        source_queries: BTreeMap::new(),
        roots: BTreeSet::new(),
        issues: vec![],
    };
    let mut names = Names::new();
    let mut calls = 0usize;
    let mut source_bytes = 0usize;
    let mut attempts = 0usize;
    if sources.len() > 128 {
        scan.issues.push(
            "Blade icon provider source count exceeds 128; remaining providers withheld".into(),
        );
    }
    for (path, source) in sources.iter().take(128) {
        source_bytes = source_bytes.saturating_add(source.len());
        if source_bytes > TOTAL {
            scan.issues.push("Blade icon provider sources exceed aggregate 4MiB budget; remaining providers withheld".into());
            break;
        }
        let full = format!("{}/{path}", scan.root);
        if source.len() > BYTES
            || !crate::owns_current_path(project, &full)
            || !project.stat(path).is_ok_and(|s| s.size <= BYTES as u64)
            || project.read_file(path).ok().as_deref() != Some(source)
        {
            continue;
        }
        evidence::record(&mut scan.source_evidence,path,source,&mut scan.source_evidence_incomplete);
        scan.inputs.insert(path.clone());
        let Some(code) = crate::package_facts::code(source) else {
            continue;
        };
        for (at, _) in code.match_indices("add") {
            if !code[..at].trim_end().ends_with("->") {
                continue;
            }
            let tail = code[at + 3..].trim_start();
            if !tail.starts_with('(') {
                continue;
            }
            let open = code.len() - tail.len();
            attempts += 1;
            if attempts > 128 {
                scan.issues.push("Blade icon provider call candidates exceed128; remaining registrations withheld".into());
                break;
            }
            if !crate::eloquent_members::metadata_inference::call_identity(source, open)
                .is_some_and(|call| {
                    call.class.as_deref() == Some("BladeUI\\Icons\\Factory")
                        && call.method.eq_ignore_ascii_case("add")
                })
            {
                continue;
            }
            calls += 1;
            if calls > 128 {
                scan.issues.push(
                    "Blade icon provider registrations exceed128 calls; remaining sets withheld"
                        .into(),
                );
                break;
            }
            let args = crate::php::arguments(source, open);
            if args.len() != 2 {
                continue;
            }
            let Some((set, _)) = crate::php::literal(source, args[0].clone()) else {
                continue;
            };
            let arrow = code[..at].trim_end().len().checked_sub(2);
            let Some(arrow) = arrow else { continue };
            let receiver_end = code[..arrow].trim_end().len();
            let receiver_start = code[..receiver_end].rfind('$').unwrap_or(receiver_end);
            match merged_provider_options(&mut scan,source,&code,path,args[1].clone(),receiver_start){Some((prefix,paths))=>collect_icons(&mut scan,&mut names,set,&prefix,&paths),None=>scan.issues.push(format!("{path}: Blade icon set {set} has dynamic, disk-backed or unproven options; withheld"))}
        }
        if calls > 128 || attempts > 128 {
            break;
        }
    }
    let mut stored: usize = metadata.domain_names.values().map(Names::len).sum();
    for (source, category, options) in &metadata.domain_options {
        if category == "bladeIcon" {
            let Some(cached) = metadata
                .domain_names
                .get_mut(&key(category, source, options))
            else {
                continue;
            };
            for (name, entry) in &names {
                if let Some(existing) = cached.get_mut(name) {
                    if existing.path != entry.path {
                        existing.path.clear();
                        existing.width = 0;
                        existing.detail = Some("Ambiguous icon registrations".into());
                    }
                } else if cached.len() < NAMES && stored < 32768 {
                    cached.insert(name.clone(), entry.clone());
                    stored += 1;
                }
            }
        }
    }
    evidence::merge(&mut metadata.source_evidence,&mut metadata.source_evidence_incomplete,scan.source_evidence);
    metadata.source_evidence_incomplete |= scan.source_evidence_incomplete;
    evidence::merge(&mut metadata.source_queries,&mut metadata.source_evidence_incomplete,scan.source_queries);
    metadata.inputs.extend(scan.inputs);
    metadata.domain_roots.extend(scan.roots);
    metadata.issues.extend(scan.issues);
}
fn literal_path(origin: &str, expression: &str, root: &str) -> Option<String> {
    let expression = expression.trim();
    if let Some((path, _)) = crate::php::literal(expression, 0..expression.len()) {
        if let Some(relative) = path.strip_prefix(&format!("{root}/")) {
            return super::resolve_path("ide.json", relative);
        }
        if path.starts_with('/') || path.contains(['$', '\\']) {
            return None;
        }
        return super::resolve_path("ide.json", path);
    }
    let tail = expression
        .strip_prefix("__DIR__")?
        .trim_start()
        .strip_prefix('.')?
        .trim_start();
    let (suffix, _) = crate::php::literal(tail, 0..tail.len())?;
    if !suffix.starts_with('/') || suffix.contains(['$', '\\']) {
        return None;
    }
    let directory = origin.rsplit_once('/')?.0;
    super::resolve_path("ide.json", &format!("{directory}{suffix}"))
}

type IconOptions = BTreeMap<String, (String, String)>;
fn option_map(source: &str, open: usize, origin: &str) -> Option<IconOptions> {
    let mut map = BTreeMap::new();
    for item in crate::php::items(source, open) {
        let (key, _) = crate::php::literal(source, item.key?)?;
        if map
            .insert(
                key.into(),
                (source[item.value].trim().into(), origin.into()),
            )
            .is_some()
        {
            return None;
        }
    }
    Some(map)
}
fn resolved_icon_options(map: &IconOptions, root: &str) -> Option<(String, Vec<String>)> {
    if let Some((disk, _)) = map.get("disk") {
        if !crate::php::literal(disk, 0..disk.len()).is_some_and(|(v, _)| v.is_empty()) {
            return None;
        }
    }
    let (prefix, _) = map.get("prefix")?;
    let (prefix, _) = crate::php::literal(prefix, 0..prefix.len())?;
    if prefix.is_empty()
        || !prefix
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    {
        return None;
    }
    let (path, origin) = map.get("paths").or_else(|| map.get("path"))?;
    let paths = if let Some(open) = crate::php::array_open(path, 0..path.len()) {
        crate::php::items(path, open)
            .into_iter()
            .map(|i| literal_path(origin, &path[i.value], root))
            .collect::<Option<Vec<_>>>()?
    } else {
        vec![literal_path(origin, path, root)?]
    };
    if paths.is_empty() || paths.len() > 64 {
        return None;
    }
    Some((prefix.into(), paths))
}
fn provider_icon_options(
    source: &str,
    open: usize,
    origin: &str,
    root: &str,
) -> Option<(String, Vec<String>)> {
    resolved_icon_options(&option_map(source, open, origin)?, root)
}
fn returned_options(source: &str, origin: &str) -> Option<IconOptions> {
    let code = crate::package_facts::code(source)?;
    let mut rest = code.strip_prefix("<?php")?.trim_start();
    if rest.starts_with("declare") {
        let open = code.len() - rest.len() + 7;
        let close = crate::php::closing(source, open)?;
        rest = code[close + 1..]
            .trim_start()
            .strip_prefix(';')?
            .trim_start();
    }
    let body = rest.strip_prefix("return")?;
    if body
        .as_bytes()
        .first()
        .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
    {
        return None;
    }
    let start = code.len() - body.len();
    let open = crate::php::array_open(source, start..source.len())?;
    let close = crate::php::closing(source, open)?;
    if !matches!(code[close + 1..].trim(), ";" | "; ?>") {
        return None;
    }
    option_map(source, open, origin)
}
fn member_open(source: &str, start: usize, method: &str) -> Option<usize> {
    let rest = source
        .get(start..)?
        .trim_start()
        .strip_prefix("->")?
        .trim_start();
    let name = rest.get(..method.len())?;
    if !name.eq_ignore_ascii_case(method) {
        return None;
    }
    let rest = rest[method.len()..].trim_start();
    rest.starts_with('(').then_some(source.len() - rest.len())
}
fn config_get_key(source: &str, range: std::ops::Range<usize>) -> Option<String> {
    let expression = source[range.clone()].trim();
    let first = expression.strip_prefix('$')?;
    let count = first
        .bytes()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == b'_')
        .count();
    if count == 0 {
        return None;
    }
    let start = range.start + source[range.clone()].find(expression)?;
    let make = member_open(source, start + 1 + count, "make")?;
    if !crate::eloquent_members::metadata_inference::call_identity(source, make).is_some_and(
        |call| {
            matches!(
                call.class.as_deref(),
                Some(
                    "Illuminate\\Contracts\\Container\\Container"
                        | "Illuminate\\Container\\Container"
                )
            ) && call.method.eq_ignore_ascii_case("make")
        },
    ) {
        return None;
    }
    let args = crate::php::arguments(source, make);
    if args.len() != 1
        || !crate::php::literal(source, args[0].clone()).is_some_and(|(v, _)| v == "config")
    {
        return None;
    }
    let close = crate::php::closing(source, make)?;
    let get = member_open(source, close + 1, "get")?;
    let args = crate::php::arguments(source, get);
    if args.is_empty() || args.len() > 2 {
        return None;
    }
    if args.len() == 2 {
        let open = crate::php::array_open(source, args[1].clone())?;
        if !crate::php::items(source, open).is_empty() {
            return None;
        }
    }
    let (key, _) = crate::php::literal(source, args[0].clone())?;
    let close = crate::php::closing(source, get)?;
    if !source.get(close + 1..range.end)?.trim().is_empty()
        || key.is_empty()
        || !key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    {
        return None;
    }
    Some(key.into())
}
fn previous_config_assignment(
    source: &str,
    code: &str,
    receiver_start: usize,
    variable: &str,
) -> Option<std::ops::Range<usize>> {
    if !variable.starts_with('$')
        || !variable[1..]
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_')
    {
        return None;
    }
    let before = code[..receiver_start].trim_end();
    let end = before.len().checked_sub(1)?;
    if code.as_bytes()[end] != b';' {
        return None;
    }
    let begin = code[..end].rfind([';', '{', '}']).map_or(0, |n| n + 1);
    let assignment = &code[begin..end];
    let equal = assignment.find('=')?;
    if assignment[..equal].trim() != variable
        || assignment[equal + 1..].trim_start().starts_with(['=', '>'])
    {
        return None;
    }
    let range = begin + equal + 1..end;
    (!source[range.clone()].trim().is_empty()).then_some(range)
}
fn merged_provider_options(
    scan: &mut Scan<'_>,
    source: &str,
    code: &str,
    origin: &str,
    range: std::ops::Range<usize>,
    receiver_start: usize,
) -> Option<(String, Vec<String>)> {
    if let Some(open) = crate::php::array_open(source, range.clone()) {
        return provider_icon_options(source, open, origin, &scan.root);
    }
    let expression = source[range.clone()].trim();
    let leading = source[range.clone()].find(expression)?;
    let rest = expression.strip_prefix("array_merge")?.trim_start();
    if !rest.starts_with('(') {
        return None;
    }
    let open = range.start + leading + expression.len() - rest.len();
    if !crate::eloquent_members::metadata_inference::global_helper(source, open, "array_merge") {
        return None;
    }
    let close = crate::php::closing(source, open)?;
    if !source.get(close + 1..range.end)?.trim().is_empty() {
        return None;
    }
    let args = crate::php::arguments(source, open);
    if args.len() < 2 || args.len() > 4 {
        return None;
    }
    let mut map = IconOptions::new();
    for arg in args {
        if let Some(open) = crate::php::array_open(source, arg.clone()) {
            map.extend(option_map(source, open, origin)?);
            continue;
        }
        let variable = source[arg.clone()].trim();
        let assignment = previous_config_assignment(source, code, receiver_start, variable)?;
        let key = config_get_key(source, assignment)?;
        // Only literal mergeConfigFrom roots of this declared provider can add
        // package defaults. The published app config wins, exactly as Laravel's
        // shallow top-level merge does; dynamic config invalidates this set.
        let mut config = IconOptions::new();
        let mut defaults = 0;
        let mut merge_candidates = 0;
        for (at, _) in code.match_indices("mergeConfigFrom") {
            merge_candidates += 1;
            if merge_candidates > 16 {
                return None;
            }
            if !code[..at].trim_end().ends_with("->") {
                continue;
            }
            let rest = code[at + 15..].trim_start();
            if !rest.starts_with('(') {
                continue;
            }
            let call = code.len() - rest.len();
            let Some(identity) =
                crate::eloquent_members::metadata_inference::call_identity(source, call)
            else {
                continue;
            };
            let provider = crate::eloquent_members::declaration(source)?;
            if identity.class.as_deref() != Some(provider.as_str()) {
                continue;
            }
            let args = crate::php::arguments(source, call);
            if args.len() != 2
                || !crate::php::literal(source, args[1].clone()).is_some_and(|(v, _)| v == key)
            {
                continue;
            }
            defaults += 1;
            if defaults > 1 {
                return None;
            }
            let path = literal_path(origin, &source[args[0].clone()], &scan.root)?;
            let full = format!("{}/{path}", scan.root);
            let content = scan.read(&full)?;
            config.extend(returned_options(&content, &path)?);
        }
        let app = format!("config/{key}.php");
        scan.inputs.insert(app.clone());
        if scan.project.exists(&app).ok()? {
            let content = scan.read(&format!("{}/{app}", scan.root))?;
            config.extend(returned_options(&content, &app)?);
        }
        if config.is_empty() {
            return None;
        }
        map.extend(config);
    }
    resolved_icon_options(&map, &scan.root)
}

fn collect_icons(
    scan: &mut Scan<'_>,
    names: &mut Names,
    set: &str,
    prefix: &str,
    paths: &[String],
) {
    for relative in paths {
        scan.roots.insert(relative.clone());
        for path in scan.files(&format!("{relative}/**/*.svg")) {
            let base = format!("{}/{relative}/", scan.root);
            let Some(name) = path
                .strip_prefix(&base)
                .and_then(|p| p.strip_suffix(".svg"))
            else {
                continue;
            };
            let name = name.replace('/', ".");
            add(names, &format!("{prefix}-{name}"), &path, 0, 0);
            if set == "default" {
                add(names, &name, &path, 0, 0);
            }
        }
    }
}

fn module_inventory(scan: &mut Scan<'_>) -> Names {
    let mut names = Names::new();
    for path in scan.files("**/module.json") {
        let Some(text) = scan.read(&path) else {
            continue;
        };
        let Ok(json) = crate::strict_json::parse(&text, BYTES) else {
            continue;
        };
        if let Some(name) = json["name"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 256 && !s.contains(['\n', '\0', '/', '\\']))
        {
            add(&mut names, name, &path, 0, 0);
        }
    }
    names
}
// Only selector preambles are inspected. Strings, comments, URLs and declaration
// values cannot contribute false class names. Escaped punctuation supports built CSS.
fn css_tokens(text: &str) -> Vec<(String, usize, usize)> {
    let mut clean = text.as_bytes().to_vec();
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' {
            i = (i + 2).min(b.len());
            continue;
        }
        if b[i..].starts_with(b"/*") {
            let start = i;
            i += 2;
            while i + 1 < b.len() && !b[i..].starts_with(b"*/") {
                i += 1;
            }
            i = (i + 2).min(b.len());
            for c in &mut clean[start..i] {
                if *c != b'\n' {
                    *c = b' ';
                }
            }
            continue;
        }
        if matches!(b[i], b'\'' | b'"') {
            let start = i;
            let quote = b[i];
            i += 1;
            while i < b.len() {
                if b[i] == b'\\' {
                    i = (i + 2).min(b.len());
                    continue;
                }
                if b[i] == quote {
                    i += 1;
                    break;
                }
                i += 1;
            }
            for c in &mut clean[start..i] {
                if *c != b'\n' {
                    *c = b' ';
                }
            }
            continue;
        }
        i += 1;
    }
    let mut result = Vec::new();
    let (mut i, mut boundary) = (0, 0);
    while i < clean.len() {
        if clean[i] == b'\\' {
            i = (i + 2).min(clean.len());
            continue;
        }
        if clean[i] == b'{' {
            let p = &clean[boundary..i];
            if p.iter().copied().find(|c| !c.is_ascii_whitespace()) != Some(b'@') {
                let mut j = 0;
                while j < p.len() {
                    if p[j] != b'.' {
                        j += 1;
                        continue;
                    }
                    j += 1;
                    let start = j;
                    if let Some((name, end)) = css_identifier(p, j) {
                        j = end;
                        if !name.is_empty() {
                            result.push((name, boundary + start, boundary + end));
                        }
                    }
                }
            }
            boundary = i + 1;
        } else if matches!(clean[i], b';' | b'}') {
            boundary = i + 1;
        }
        i += 1;
    }
    result
}
// CSS Syntax3 ident-sequence / escaped-code-point algorithms. Offsets stay in
// UTF-8 bytes; escapes permit a leading digit without changing its displayed name.
fn css_identifier(bytes: &[u8], start: usize) -> Option<(String, usize)> {
    fn name_start(c: char) -> bool {
        c.is_ascii_alphabetic() || c == '_' || c == '\0' || c as u32 >= 0x80
    }
    fn valid_escape(b: &[u8], i: usize) -> bool {
        b.get(i) == Some(&b'\\')
            && b.get(i + 1)
                .is_some_and(|c| !matches!(c, b'\n' | b'\r' | b'\x0c'))
    }
    fn character(b: &[u8], i: usize) -> Option<char> {
        let first = *b.get(i)?;
        let width = match first {
            0..=0x7f => 1,
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => return None,
        };
        std::str::from_utf8(b.get(i..i + width)?)
            .ok()?
            .chars()
            .next()
    }
    let first = character(bytes, start)?;
    let begins = if first == '-' {
        character(bytes, start + 1).is_some_and(|c| name_start(c) || c == '-')
            || valid_escape(bytes, start + 1)
    } else {
        name_start(first) || valid_escape(bytes, start)
    };
    if !begins {
        return None;
    }
    let mut i = start;
    let mut name = String::new();
    while i < bytes.len() {
        let c = character(bytes, i)?;
        if name_start(c) || c.is_ascii_digit() || c == '-' {
            name.push(if c == '\0' {
                char::REPLACEMENT_CHARACTER
            } else {
                c
            });
            i += c.len_utf8();
            continue;
        }
        if !valid_escape(bytes, i) {
            break;
        }
        i += 1;
        if bytes[i].is_ascii_hexdigit() {
            let begin = i;
            while i < bytes.len() && i - begin < 6 && bytes[i].is_ascii_hexdigit() {
                i += 1;
            }
            let scalar =
                u32::from_str_radix(std::str::from_utf8(&bytes[begin..i]).ok()?, 16).ok()?;
            name.push(if scalar == 0 {
                char::REPLACEMENT_CHARACTER
            } else {
                char::from_u32(scalar).unwrap_or(char::REPLACEMENT_CHARACTER)
            });
            if bytes
                .get(i)
                .is_some_and(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b'\x0c'))
            {
                let cr = bytes[i] == b'\r';
                i += 1;
                if cr && bytes.get(i) == Some(&b'\n') {
                    i += 1;
                }
            }
        } else {
            let c = character(bytes, i)?;
            name.push(if c == '\0' {
                char::REPLACEMENT_CHARACTER
            } else {
                c
            });
            i += c.len_utf8();
        }
    }
    Some((name, i))
}

#[cfg(test)]
fn css_classes(text: &str) -> Vec<(String, u32)> {
    css_tokens(text)
        .into_iter()
        .map(|(name, start, _)| (name, openidea_plugin::text::position(text, start).line))
        .collect()
}
// Selectors arrive in source order. Advancing only the newly encountered text
// keeps minified single-line CSS linear while preserving UTF-16 editor columns.
#[derive(Default)]
struct CssPositionCursor { offset:usize, line:u32, character:u32 }
impl CssPositionCursor {
    fn at(&mut self,text:&str,offset:usize)->Position {
        debug_assert!(offset>=self.offset);
        for ch in text[self.offset..offset].chars() {
            if ch=='\n' {self.line+=1;self.character=0;} else {self.character+=ch.len_utf16() as u32;}
        }
        self.offset=offset;
        Position::new(self.line,self.character)
    }
}
fn css_inventory(scan: &mut Scan<'_>) -> Names {
    let mut names = Names::new();
    for path in scan.files("**/*.css") {
        if let Some(text) = scan.read_limit(&path, 2 * 1024 * 1024) {
            let mut position=CssPositionCursor::default();
            for (name, start, end) in css_tokens(&text) {
                add_at(
                    &mut names,
                    &name,
                    &path,
                    position.at(&text, start),
                    text[start..end].encode_utf16().count(),
                );
            }
        }
    }
    names
}

fn literal_config_sets(
    text: &str,
    origin: &str,
    root: &str,
) -> Option<Vec<(String, Option<(String, Vec<String>)>)>> {
    let options = returned_options(text, origin)?;
    let (sets, _) = options.get("sets")?;
    let open = crate::php::array_open(sets, 0..sets.len())?;
    let mut result = Vec::new();
    for item in crate::php::items(sets, open) {
        let (name, _) = crate::php::literal(sets, item.key?)?;
        let resolved = crate::php::array_open(sets, item.value)
            .and_then(|open| provider_icon_options(sets, open, origin, root));
        result.push((name.into(), resolved));
    }
    Some(result)
}
#[cfg(test)]
fn icon_sets(text: &str) -> Vec<(String, String, Vec<String>)> {
    literal_config_sets(text, "config/blade-icons.php", "/project")
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(name, options)| options.map(|(prefix, paths)| (name, prefix, paths)))
        .collect()
}
fn icon_inventory(scan: &mut Scan<'_>) -> Names {
    let mut names = Names::new();
    let origin = "config/blade-icons.php";
    let config = format!("{}/{origin}", scan.root);
    scan.inputs.insert(origin.into());
    let Some(text) = scan.read(&config) else {
        return names;
    };
    let Some(sets) = literal_config_sets(&text, origin, &scan.root) else {
        scan.issues.push("config/blade-icons.php: icon sets are not a proven literal returned map; inventory is partial".into());
        return names;
    };
    for (set, options) in sets {
        match options{Some((prefix,paths))=>collect_icons(scan,&mut names,&set,&prefix,&paths),None=>scan.issues.push(format!("config/blade-icons.php: icon set {set} has dynamic, disk-backed or invalid options; withheld"))}
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn validates_directory_contract() {
        assert!(
            validate(
                "directoryFiles",
                &json!({"directory":"assets","recursive":false,"suffixToClear":".php"})
            )
            .is_empty()
        );
        assert!(
            !validate(
                "directoryFiles",
                &json!({"directory":"a","directories":["b"]})
            )
            .is_empty()
        );
        assert!(!validate("directoryFiles", &json!({"directories":[1]})).is_empty());
    }
    #[test]
    fn css_monotonic_positions_match_editor_utf16_after_astral_and_crlf_text() {
        let text="/* 🚀 */ .first{} .café:hover{}\r\n/* 漢字😀 */ .\\31 23{}\n.last{}";
        let mut cursor=CssPositionCursor::default();
        for (_,start,_) in css_tokens(text){assert_eq!(cursor.at(text,start),openidea_plugin::text::position(text,start));}
        let large=(0..8192).map(|i|format!(".class{i}{{}} ")).collect::<String>();
        let mut cursor=CssPositionCursor::default();let mut last=0;
        for (_,start,_)in css_tokens(&large){let position=cursor.at(&large,start);assert_eq!(position,Position::new(0,start as u32));assert!(start>=last);last=start;}
    }
    #[test]
    fn css_skips_values_comments_strings() {
        let names = css_classes(
            "/* .bad{} */ .one, .hover\\:two:hover { content:'.fake{}'; background:url(a.b); } @media screen {.three {color:red}} ",
        );
        assert_eq!(
            names.into_iter().map(|x| x.0).collect::<Vec<_>>(),
            vec!["one", "hover:two", "three"]
        );
    }
    #[test]
    fn literal_icon_sets_only() {
        let sets = icon_sets(
            "<?php return ['sets'=>['default'=>['prefix'=>'icon','paths'=>['resources/svg','more']], 'remote'=>['prefix'=>'other','path'=>'svg','disk'=>'s3'], 'dynamic'=>['prefix'=>'dyn','path'=>base_path('svg')]]];",
        );
        assert_eq!(
            sets,
            vec![(
                "default".into(),
                "icon".into(),
                vec!["resources/svg".into(), "more".into()]
            )]
        );
    }
    #[test]
    fn cache_identity_includes_source_and_options() {
        assert_ne!(
            key("directoryFiles", "ide.json", &json!({"directory":"a"})),
            key("directoryFiles", "pkg/ide.json", &json!({"directory":"a"}))
        );
    }
    #[test]
    fn caches_owned_relative_directories_css_modules_icons_and_invalidates_roots() {
        let host = openidea_plugin::mock::MockHost::install();
        host.add_file("vendor/pkg/assets/sub/a.php", "<?php");
        host.add_file("vendor/pkg/assets/b.txt", "b");
        host.add_file("resources/app.css", ".good {color:red}");
        host.add_file("Modules/Blog/module.json", r#"{"name":"Blog"}"#);
        host.add_file(
            "config/blade-icons.php",
            "<?php return ['sets'=>['default'=>['prefix'=>'icon','path'=>'resources/svg']]];",
        );
        host.add_file("resources/svg/nav/arrow.svg", "<svg/>");
        let project = openidea_plugin::host::projects().unwrap().remove(0);
        let mut metadata = Metadata::default();
        let filtered = json!({"directory":"assets","suffixToClear":".php"});
        let direct = json!({"directory":"assets","recursive":false});
        metadata.domain_options = vec![
            (
                "vendor/pkg/ide.json".into(),
                "directoryFiles".into(),
                filtered.clone(),
            ),
            (
                "vendor/pkg/ide.json".into(),
                "directoryFiles".into(),
                direct.clone(),
            ),
            ("ide.json".into(), "cssClassName".into(), json!({})),
            ("ide.json".into(), "moduleName".into(), json!({})),
            ("ide.json".into(), "bladeIcon".into(), json!({})),
        ];
        cache(&project, &mut metadata);
        assert!(
            metadata.domain_names[&key("directoryFiles", "vendor/pkg/ide.json", &filtered)]
                .contains_key("sub/a")
        );
        let names = &metadata.domain_names[&key("directoryFiles", "vendor/pkg/ide.json", &direct)];
        assert!(names.contains_key("sub"));
        assert!(names.contains_key("b.txt"));
        assert!(!names.contains_key("sub/a.php"));
        assert!(
            metadata.domain_names[&key("cssClassName", "ide.json", &json!({}))]
                .contains_key("good")
        );
        assert!(
            metadata.domain_names[&key("moduleName", "ide.json", &json!({}))].contains_key("Blog")
        );
        assert!(
            metadata.domain_names[&key("bladeIcon", "ide.json", &json!({}))]
                .contains_key("icon-nav.arrow")
        );
        assert!(metadata.affects("vendor/pkg/assets/new.php"));
        assert!(metadata.affects("resources/svg/new.svg"));
    }
    #[test]
    fn refuses_directory_escape_and_duplicate_json_module_names() {
        let host = openidea_plugin::mock::MockHost::install();
        host.add_file(
            "Modules/Bad/module.json",
            r#"{"name":"Bad","name":"Spoof"}"#,
        );
        let project = openidea_plugin::host::projects().unwrap().remove(0);
        let mut scan = Scan {
            project: &project,
            root: project.root().unwrap(),
            total: 0,
            queries: 0,
            listed: 0,
            inputs: BTreeSet::new(),
        source_evidence: BTreeMap::new(),
        source_evidence_incomplete: false,
        source_queries: BTreeMap::new(),
            roots: BTreeSet::new(),
            issues: vec![],
        };
        assert!(directory(&mut scan, "ide.json", &json!({"directory":"../outside"})).is_empty());
        assert!(!scan.issues.is_empty());
        assert!(module_inventory(&mut scan).is_empty());
    }
    #[test]
    fn public_completion_uses_cache_and_replaces_only_current_css_token() {
        let host = openidea_plugin::mock::MockHost::install();
        host.add_file("ide.json",r#"{"completions":[{"complete":"cssClassNames","condition":[{"functionNames":["styles"]}]}]}"#);
        host.add_file("resources/app.css", ".alpha{} .beta{}");
        let project = openidea_plugin::host::projects().unwrap().remove(0);
        let index = Index::build(&project);
        let source = "<?php styles('alpha be tail');";
        let at = source.find("be tail").unwrap() + 2;
        let items = super::super::completion(&index, source, at);
        let beta = items
            .iter()
            .find(|i| i.label == "beta")
            .expect("metadata domain reaches completion provider");
        let edit = beta.replace.as_ref().expect("token replacement");
        assert_eq!(beta.insert_text, "beta");
        assert_eq!(
            edit.start.character as usize,
            source.find("be tail").unwrap()
        );
        assert_eq!(
            edit.end.character as usize,
            source.find("be tail").unwrap() + 2
        );
        // Inventory lookup consumes the index cache and the rule's original options.
        assert!(
            inventory(
                &index,
                "cssClassNames",
                &Value::Null,
                "ide.json",
                source,
                at,
                None
            )
            .contains_key("beta")
        );
    }
    #[test]
    fn nested_projects_are_excluded_and_conflicting_sources_have_no_target() {
        let host = openidea_plugin::mock::MockHost::install();
        host.add_file("assets/a.txt", "a");
        host.add_file("other/a.txt", "b");
        host.add_project("/project/assets/private");
        host.add_file("assets/private/secret.txt", "secret");
        let project = openidea_plugin::host::projects()
            .unwrap()
            .into_iter()
            .find(|p| p.root().ok().as_deref() == Some("/project"))
            .unwrap();
        let mut scan = Scan {
            project: &project,
            root: project.root().unwrap(),
            total: 0,
            queries: 0,
            listed: 0,
            inputs: BTreeSet::new(),
        source_evidence: BTreeMap::new(),
        source_evidence_incomplete: false,
        source_queries: BTreeMap::new(),
            roots: BTreeSet::new(),
            issues: vec![],
        };
        let names = directory(
            &mut scan,
            "ide.json",
            &json!({"directories":["assets","other"]}),
        );
        assert!(!names.contains_key("private/secret.txt"));
        assert_eq!(names["a.txt"].path, "");
    }
    #[test]
    fn aggregate_enumeration_budget_stops_further_host_queries() {
        let host = openidea_plugin::mock::MockHost::install();
        host.add_file("assets/a.txt", "a");
        let project = openidea_plugin::host::projects().unwrap().remove(0);
        let mut scan = Scan {
            project: &project,
            root: project.root().unwrap(),
            total: 0,
            queries: 64,
            listed: 0,
            inputs: BTreeSet::new(),
        source_evidence: BTreeMap::new(),
        source_evidence_incomplete: false,
        source_queries: BTreeMap::new(),
            roots: BTreeSet::new(),
            issues: vec![],
        };
        assert!(scan.files("assets/**/*").is_empty());
        assert_eq!(scan.queries, 65);
        assert_eq!(scan.issues.len(), 1);
    }
    #[test]
    fn directory_walk_includes_empty_folders_and_respects_recursion_ownership_and_symlinks() {
        use openidea_plugin::{FileKind, host::fs::DirEntry};
        let host = openidea_plugin::mock::MockHost::install();
        host.add_dir("assets");
        host.add_dir("assets/empty");
        host.add_dir("assets/deep");
        host.add_dir("assets/deep/leaf");
        host.add_project("/project/assets/foreign");
        let project = openidea_plugin::host::projects()
            .unwrap()
            .into_iter()
            .find(|p| p.root().ok().as_deref() == Some("/project"))
            .unwrap();
        let mut scan = Scan {
            project: &project,
            root: project.root().unwrap(),
            total: 0,
            queries: 0,
            listed: 0,
            inputs: BTreeSet::new(),
        source_evidence: BTreeMap::new(),
        source_evidence_incomplete: false,
        source_queries: BTreeMap::new(),
            roots: BTreeSet::new(),
            issues: vec![],
        };
        let mut calls = Vec::new();
        let mut reader = |path: &str| {
            calls.push(path.to_string());
            Ok(match path {
                "/project/assets" => vec![
                    DirEntry {
                        name: "empty".into(),
                        kind: FileKind::Directory,
                    },
                    DirEntry {
                        name: "deep".into(),
                        kind: FileKind::Directory,
                    },
                    DirEntry {
                        name: "foreign".into(),
                        kind: FileKind::Directory,
                    },
                    DirEntry {
                        name: "link".into(),
                        kind: FileKind::Symlink,
                    },
                    DirEntry {
                        name: "../escape".into(),
                        kind: FileKind::Directory,
                    },
                ],
                "/project/assets/deep" => vec![DirEntry {
                    name: "leaf".into(),
                    kind: FileKind::Directory,
                }],
                _ => vec![],
            })
        };
        let mut names = Names::new();
        directory_walk(&mut scan, "assets", true, &mut names, &mut reader);
        assert!(names.contains_key("empty"));
        assert!(names.contains_key("deep/leaf"));
        assert!(!names.contains_key("foreign"));
        assert!(!names.contains_key("link"));
        assert!(
            !calls
                .iter()
                .any(|p| p.contains("foreign") || p.contains("escape"))
        );
        let mut flat = Names::new();
        directory_walk(&mut scan, "assets", false, &mut flat, &mut |_| {
            Ok(vec![DirEntry {
                name: "empty".into(),
                kind: FileKind::Directory,
            }])
        });
        assert!(flat.contains_key("empty"));
        assert!(!flat.contains_key("deep/leaf"));
    }
    #[test]
    fn missing_directory_grant_retains_file_fallback_and_explains_empty_directory_limit() {
        let host = openidea_plugin::mock::MockHost::install().with_capabilities(&["project.read"]);
        host.add_file("assets/file.txt", "a");
        host.add_dir("assets");
        let project = openidea_plugin::host::projects().unwrap().remove(0);
        let mut scan = Scan {
            project: &project,
            root: project.root().unwrap(),
            total: 0,
            queries: 0,
            listed: 0,
            inputs: BTreeSet::new(),
        source_evidence: BTreeMap::new(),
        source_evidence_incomplete: false,
        source_queries: BTreeMap::new(),
            roots: BTreeSet::new(),
            issues: vec![],
        };
        let names = directory(&mut scan, "ide.json", &json!({"directory":"assets"}));
        assert!(names.contains_key("file.txt"));
        assert!(scan.issues.iter().any(|i| i.contains("grant unavailable")));
    }
    #[test]
    fn css_identifiers_decode_hex_unicode_and_escaped_leading_digits() {
        let css = ".café, .\u{1f680}, .\\31 23, .\\000063af\\e9, .hover\\:bg, .--custom, .-\\32 x {color:red} .123bad, .-1bad {}";
        let names: Vec<_> = css_classes(css).into_iter().map(|v| v.0).collect();
        assert_eq!(
            names,
            vec!["café", "🚀", "123", "café", "hover:bg", "--custom", "-2x"]
        );
        assert_eq!(css_identifier(b"\\0 x", 0).unwrap().0, "�x");
        assert_eq!(css_identifier(b"\\110000x", 0).unwrap().0, "�x");
        assert_eq!(css_identifier(b"\\d800 x", 0).unwrap().0, "�x");
        assert!(css_identifier(b"\\\ninvalid", 0).is_none());
    }
    fn icon_metadata() -> Metadata {
        let mut metadata = Metadata::default();
        metadata
            .domain_options
            .push(("ide.json".into(), "bladeIcon".into(), Value::Null));
        metadata
            .domain_names
            .insert(key("bladeIcon", "ide.json", &Value::Null), Names::new());
        metadata
    }
    #[test]
    fn declared_typed_provider_icon_sets_use_owned_magic_paths_and_invalidate_roots() {
        let host = openidea_plugin::mock::MockHost::install();
        let provider = r#"<?php namespace Vendor\Icons; use BladeUI\Icons\Factory; class Provider extends \Illuminate\Support\ServiceProvider {function boot(Factory $factory){$factory->add('vendor',['prefix'=>'v','path'=>__DIR__.'/../resources/svg']);}}"#;
        let path = "vendor/acme/icons/src/Provider.php";
        host.add_file(path, provider);
        host.add_file("vendor/acme/icons/resources/svg/nested/check.svg", "<svg/>");
        let project = openidea_plugin::host::projects().unwrap().remove(0);
        let mut metadata = icon_metadata();
        cache_package_icons(
            &project,
            &mut metadata,
            &BTreeMap::from([(path.into(), provider.into())]),
        );
        let names = &metadata.domain_names[&key("bladeIcon", "ide.json", &Value::Null)];
        assert!(
            names.contains_key("v-nested.check"),
            "{:?}",
            metadata.issues
        );
        assert!(metadata.affects("vendor/acme/icons/resources/svg/new.svg"));
        let old = metadata.domain_names.clone();
        cache_package_icons(
            &project,
            &mut metadata,
            &BTreeMap::from([(
                path.into(),
                provider.replace("prefix'=>'v", "prefix'=>'spoof"),
            )]),
        );
        assert_eq!(
            metadata.domain_names.keys().collect::<Vec<_>>(),
            old.keys().collect::<Vec<_>>()
        );
        assert!(
            !metadata.domain_names[&key("bladeIcon", "ide.json", &Value::Null)]
                .contains_key("spoof-nested.check")
        );
    }
    #[test]
    fn heroicons_style_registered_config_merge_honors_published_literal_overrides() {
        let host = openidea_plugin::mock::MockHost::install();
        let provider = r#"<?php declare(strict_types=1); namespace Vendor\Icons; use BladeUI\Icons\Factory; use Illuminate\Contracts\Container\Container; class Provider extends \Illuminate\Support\ServiceProvider {function register(){ $this->mergeConfigFrom(__DIR__.'/../config/icons.php','icons'); $this->callAfterResolving(Factory::class,function(Factory $factory,Container $container){$config=$container->make('config')->get('icons',[]);$factory->add('icons',array_merge(['path'=>__DIR__.'/../resources/svg'],$config));});}}"#;
        let path = "vendor/acme/icons/src/Provider.php";
        host.add_file(path, provider);
        host.add_file(
            "vendor/acme/icons/config/icons.php",
            "<?php return ['prefix'=>'pkg'];",
        );
        host.add_file("config/icons.php", "<?php return ['prefix'=>'custom'];");
        host.add_file("vendor/acme/icons/resources/svg/check.svg", "<svg/>");
        let project = openidea_plugin::host::projects().unwrap().remove(0);
        let mut metadata = icon_metadata();
        cache_package_icons(
            &project,
            &mut metadata,
            &BTreeMap::from([(path.into(), provider.into())]),
        );
        let names = &metadata.domain_names[&key("bladeIcon", "ide.json", &Value::Null)];
        assert!(names.contains_key("custom-check"), "{:?}", metadata.issues);
        assert!(!names.contains_key("pkg-check"));
        assert!(metadata.affects("config/icons.php"));
        assert!(metadata.affects("vendor/acme/icons/config/icons.php"));
    }
    #[test]
    fn wrong_factory_dynamic_disks_foreign_paths_and_config_reassignment_are_withheld() {
        let host = openidea_plugin::mock::MockHost::install();
        let project = openidea_plugin::host::projects().unwrap().remove(0);
        let cases = [
            r#"<?php namespace Vendor; class Provider {function boot(\Other\Factory $factory){$factory->add('x',['prefix'=>'x','path'=>'resources/svg']);}}"#,
            r#"<?php namespace Vendor; class Provider {function boot(\BladeUI\Icons\Factory $factory){$factory->add('x',['prefix'=>'x','path'=>'resources/svg','disk'=>'s3']);}}"#,
            r#"<?php namespace Vendor; class Provider {function boot(\BladeUI\Icons\Factory $factory){$factory->add('x',['prefix'=>'x','path'=>'/outside']);}}"#,
        ];
        host.add_file("resources/svg/a.svg", "<svg/>");
        for (case, source) in cases.iter().enumerate() {
            let path = format!("vendor/pkg/Provider{case}.php");
            host.add_file(&path, source);
            let mut metadata = icon_metadata();
            cache_package_icons(
                &project,
                &mut metadata,
                &BTreeMap::from([(path, (*source).into())]),
            );
            assert!(metadata.domain_names[&key("bladeIcon", "ide.json", &Value::Null)].is_empty());
        }
        let source = "<?php function register(){ $config=other(); $factory->add('x', $config); }";
        let code = crate::package_facts::code(source).unwrap();
        let receiver = source.find("$factory").unwrap();
        assert!(previous_config_assignment(source, &code, receiver, "$config").is_some());
        let range = previous_config_assignment(source, &code, receiver, "$config").unwrap();
        assert!(config_get_key(source, range).is_none());
    }
    #[test]
    fn composer_declared_icon_provider_reaches_public_metadata_completion() {
        let host = openidea_plugin::mock::MockHost::install();
        host.add_file("ide.json",r#"{"completions":[{"complete":"bladeIcon","condition":[{"functionNames":["svg_icon"]}]}]}"#);
        host.add_file("vendor/composer/installed.json",r#"{"packages":[{"name":"acme/icons","install-path":"../acme/icons","autoload":{"psr-4":{"Vendor\\Icons\\":"src/"}},"extra":{"laravel":{"providers":["Vendor\\Icons\\Provider"]}}}]}"#);
        host.add_file("vendor/acme/icons/src/Provider.php",r#"<?php namespace Vendor\Icons; use BladeUI\Icons\Factory; class Provider extends \Illuminate\Support\ServiceProvider {function boot(Factory $factory){$factory->add('vendor',['prefix'=>'v','path'=>__DIR__.'/../resources/svg']);}}"#);
        host.add_file("vendor/acme/icons/resources/svg/check.svg", "<svg/>");
        host.add_file("vendor/acme/icons/src/Unregistered.php",r#"<?php namespace Vendor\Icons; use BladeUI\Icons\Factory; class Unregistered extends \Illuminate\Support\ServiceProvider {function boot(Factory $factory){$factory->add('evil',['prefix'=>'evil','path'=>__DIR__.'/../resources/svg']);}}"#);
        let project = openidea_plugin::host::projects().unwrap().remove(0);
        let index = Index::build(&project);
        let source = "<?php svg_icon('v-ch');";
        let items = super::super::completion(&index, source, source.find("v-ch").unwrap() + 4);
        assert!(
            items.iter().any(|i| i.label == "v-check"),
            "issues={:?}, providers={:?}",
            index.metadata.issues,
            index.packages.provider_files
        );
        assert!(
            !index.metadata.domain_names[&key("bladeIcon", "ide.json", &Value::Null)]
                .contains_key("evil-check")
        );
    }
    #[test]
    fn unicode_and_escaped_css_names_reach_php_completion_with_literal_encoding() {
        let host = openidea_plugin::mock::MockHost::install();
        host.add_file("ide.json",r#"{"completions":[{"complete":"cssClassName","condition":[{"functionNames":["style"]}]}]}"#);
        host.add_file(
            "resources/app.css",
            r#".caf\e9, .dollar\$, .quote\'name, .brace\{name {}"#,
        );
        let project = openidea_plugin::host::projects().unwrap().remove(0);
        let index = Index::build(&project);
        for (source, label, insert) in [
            ("<?php style('caf');", "café", "café"),
            ("<?php style(\"dollar\");", "dollar$", "dollar\\$"),
            ("<?php style('quote');", "quote'name", "quote\\'name"),
            ("<?php style('brace');", "brace{name", "brace{name"),
        ] {
            let at = source
                .rfind(if source.contains("\"dollar") {
                    "\""
                } else {
                    "'"
                })
                .unwrap();
            let items = super::super::completion(&index, source, at);
            let item = items.iter().find(|i| i.label == label).unwrap_or_else(|| {
                panic!(
                    "{source}: {:?}",
                    items.iter().map(|i| &i.label).collect::<Vec<_>>()
                )
            });
            assert_eq!(item.insert_text, insert);
        }
    }
    #[test]
    fn css_navigation_retains_multiline_identifier_offsets_and_escape_width() {
        let host = openidea_plugin::mock::MockHost::install();
        host.add_file(
            "resources/app.css",
            "/* heading */\n.caf\\e9,\n .🚀 {color:red}",
        );
        let project = openidea_plugin::host::projects().unwrap().remove(0);
        let mut scan = Scan {
            project: &project,
            root: project.root().unwrap(),
            total: 0,
            queries: 0,
            listed: 0,
            inputs: BTreeSet::new(),
        source_evidence: BTreeMap::new(),
        source_evidence_incomplete: false,
        source_queries: BTreeMap::new(),
            roots: BTreeSet::new(),
            issues: vec![],
        };
        let names = css_inventory(&mut scan);
        assert_eq!(names["café"].at, Position::new(1, 1));
        assert_eq!(names["café"].width, 6);
        assert_eq!(names["🚀"].at, Position::new(2, 2));
        assert_eq!(names["🚀"].width, 2);
    }
    #[test]
    fn config_merge_rejects_shadowed_builtins_wrong_containers_and_dynamic_app_overrides() {
        let host = openidea_plugin::mock::MockHost::install();
        let path = "vendor/acme/icons/src/Provider.php";
        let original = r#"<?php namespace Vendor\Icons; use BladeUI\Icons\Factory; use Illuminate\Contracts\Container\Container; class Provider extends \Illuminate\Support\ServiceProvider {function register(){ $this->mergeConfigFrom(__DIR__.'/../config/icons.php','icons');$this->callAfterResolving(Factory::class,function(Factory $factory,Container $container){$config=$container->make('config')->get('icons',[]);$factory->add('icons',array_merge(['path'=>__DIR__.'/../resources/svg'],$config));});}}"#;
        host.add_file(
            "vendor/acme/icons/config/icons.php",
            "<?php return ['prefix'=>'pkg'];",
        );
        host.add_file("vendor/acme/icons/resources/svg/check.svg", "<svg/>");
        let project = openidea_plugin::host::projects().unwrap().remove(0);
        for source in [
            original.replace("use BladeUI", "use function Evil\\array_merge; use BladeUI"),
            original.replace(
                "Illuminate\\Contracts\\Container\\Container",
                "Evil\\Container",
            ),
            original.replace("$factory->add", "$config = unknown(); $factory->add"),
        ] {
            host.add_file(path, &source);
            let mut metadata = icon_metadata();
            cache_package_icons(
                &project,
                &mut metadata,
                &BTreeMap::from([(path.into(), source)]),
            );
            assert!(metadata.domain_names[&key("bladeIcon", "ide.json", &Value::Null)].is_empty());
            assert!(
                metadata
                    .issues
                    .iter()
                    .any(|i| i.contains("unproven options"))
            );
        }
        host.add_file(path, original);
        host.add_file("config/icons.php", "<?php return dynamic_config();");
        let mut metadata = icon_metadata();
        cache_package_icons(
            &project,
            &mut metadata,
            &BTreeMap::from([(path.into(), original.into())]),
        );
        assert!(metadata.domain_names[&key("bladeIcon", "ide.json", &Value::Null)].is_empty());
    }
}
