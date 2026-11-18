//! Dependencies declared by the project manifests and lockfiles.

use std::collections::{HashMap, HashSet};

use serde_json::Value;
use spring_text::xml::{Element, elements};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Ecosystem {
    Maven,
    Npm,
    Packagist,
    CratesIo,
    PyPI,
}

impl Ecosystem {
    pub fn osv_name(self, name: &str) -> String {
        match self {
            Ecosystem::PyPI => normalise_pypi(name),
            _ => name.to_owned(),
        }
    }

    pub fn osv(self) -> &'static str {
        match self {
            Ecosystem::Maven => "Maven",
            Ecosystem::Npm => "npm",
            Ecosystem::Packagist => "Packagist",
            Ecosystem::CratesIo => "crates.io",
            Ecosystem::PyPI => "PyPI",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Pom,
    Gradle,
    PackageJson,
    PackageLock,
    ComposerJson,
    ComposerLock,
    VersionCatalog,
    YarnLock,
    PnpmLock,
    CargoLock,
    PoetryLock,
    UvLock,
    PipfileLock,
    Requirements,
    Pylock,
    Pyproject,
}

pub const GLOBS: &[&str] = &[
    "**/pom.xml",
    "**/build.gradle",
    "**/build.gradle.kts",
    "**/package.json",
    "**/composer.json",
    "**/package-lock.json",
    "**/composer.lock",
    "**/libs.versions.toml",
    "**/yarn.lock",
    "**/pnpm-lock.yaml",
    "**/Cargo.lock",
    "**/poetry.lock",
    "**/uv.lock",
    "**/Pipfile.lock",
    "**/requirements.txt",
    "**/pylock.toml",
    "**/pylock.*.toml",
    "**/pyproject.toml",
];

/// PyPI names compare case-insensitively with runs of `-`, `_` and `.` equal (PEP 503).
pub fn normalise_pypi(name: &str) -> String {
    let mut normalised = String::new();
    let mut separator = false;

    for c in name.chars() {
        match "-_.".contains(c) {
            true => separator = true,
            false => {
                if separator {
                    normalised.push('-');
                    separator = false;
                }
                normalised.push(c.to_ascii_lowercase());
            }
        }
    }

    normalised
}

pub const MARKER: &str = "Marker not evaluated";
pub const LOCAL: &str = "Local workspace package";
pub const PRIVATE: &str = "Private, not queried";

/// A declaration shown in the report beside the audited coordinates: why it was not queried, or a caveat on a queried one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    pub path: String,
    pub ecosystem: Ecosystem,
    pub name: String,
    pub spec: String,
    pub label: &'static str,
}

/// Local packages (`workspace:`/`link:` specs, or names of known workspace members) and marker-guarded requirements of one file.
pub fn notes(path: &str, text: &str, members: &HashSet<String>) -> Vec<Note> {
    match kind(path) {
        Some(Kind::Pyproject) => crate::pyproject::parse(path, text).1,
        Some(Kind::PackageJson) => {
            let Ok(root) = serde_json::from_str::<Value>(text) else { return Vec::new() };
            let mut notes = Vec::new();
            for section in ["dependencies", "devDependencies", "optionalDependencies"] {
                let Some(entries) = root.get(section).and_then(Value::as_object) else { continue };
                for (name, spec) in entries {
                    let Some(spec) = spec.as_str() else { continue };
                    if spec.starts_with("workspace:") || spec.starts_with("link:") || members.contains(name) {
                        notes.push(Note { path: path.to_owned(), ecosystem: Ecosystem::Npm, name: name.clone(), spec: spec.to_owned(), label: LOCAL });
                    }
                }
            }

            notes
        }
        Some(Kind::PackageLock) => private_notes(package_lock(path, text).1),
        Some(Kind::YarnLock) => private_notes(yarn_lock(path, text).1),
        Some(Kind::ComposerLock) => private_notes(composer_lock(path, text).1),
        Some(Kind::CargoLock) => private_notes(toml_lock(path, text, Ecosystem::CratesIo).1),
        Some(Kind::PoetryLock) => private_notes(toml_lock(path, text, Ecosystem::PyPI).1),
        Some(Kind::PnpmLock) => private_notes(pnpm_lock(path, text).1),
        _ => Vec::new(),
    }
}

fn private_notes(skipped: Vec<Dep>) -> Vec<Note> {
    skipped.into_iter().map(|dep| Note { path: dep.path, ecosystem: dep.ecosystem, name: dep.name, spec: dep.version, label: PRIVATE }).collect()
}

/// Registries whose packages are public identities for OSV; anything else is a private mirror and its names stay local.
const NPM_HOSTS: &[&str] = &["registry.npmjs.org", "registry.yarnpkg.com"];
const COMPOSER_HOSTS: &[&str] = &["api.github.com", "codeload.github.com", "gitlab.com", "bitbucket.org", "repo.packagist.org"];

fn url_host(url: &str) -> Option<&str> {
    let rest = url.split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit('@').next()?;

    host.split(':').next()
}

/// A resolved URL on a host outside `allowed`; paths and absent values are not hosted anywhere.
fn private_url(url: Option<&str>, allowed: &[&str]) -> bool {
    let Some(url) = url.filter(|url| url.contains("://")) else {
        return false;
    };

    !url_host(url).is_some_and(|host| allowed.iter().any(|a| host.eq_ignore_ascii_case(a)))
}

fn private_npm(entry: &Value) -> bool {
    private_url(entry.get("resolved").and_then(Value::as_str), NPM_HOSTS)
}

fn private_composer(entry: &Value) -> bool {
    entry.get("dist").and_then(|dist| dist.get("url")).and_then(Value::as_str).is_some_and(|url| private_url(Some(url), COMPOSER_HOSTS) || !url.contains("://"))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dep {
    pub path: String,
    pub ecosystem: Ecosystem,
    pub name: String,
    pub version: String,
    /// Bytes the diagnostic underlines.
    pub at: (usize, usize),
    /// Bytes the quick fix replaces; `None` in lockfiles.
    pub version_at: Option<(usize, usize)>,
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

pub fn kind(path: &str) -> Option<Kind> {
    match file_name(path) {
        "pom.xml" => Some(Kind::Pom),
        "build.gradle" | "build.gradle.kts" => Some(Kind::Gradle),
        "package.json" => Some(Kind::PackageJson),
        "package-lock.json" => Some(Kind::PackageLock),
        "composer.json" => Some(Kind::ComposerJson),
        "composer.lock" => Some(Kind::ComposerLock),
        "libs.versions.toml" => Some(Kind::VersionCatalog),
        "yarn.lock" => Some(Kind::YarnLock),
        "pnpm-lock.yaml" => Some(Kind::PnpmLock),
        "Cargo.lock" => Some(Kind::CargoLock),
        "poetry.lock" => Some(Kind::PoetryLock),
        "uv.lock" => Some(Kind::UvLock),
        "Pipfile.lock" => Some(Kind::PipfileLock),
        "requirements.txt" => Some(Kind::Requirements),
        "pyproject.toml" => Some(Kind::Pyproject),
        name if name == "pylock.toml" || (name.starts_with("pylock.") && name.ends_with(".toml")) => Some(Kind::Pylock),
        _ => None,
    }
}

pub fn lockfile(path: &str) -> Option<String> {
    let sibling = match kind(path)? {
        Kind::PackageJson => "package-lock.json",
        Kind::ComposerJson => "composer.lock",
        _ => return None,
    };

    Some(format!("{}{sibling}", &path[..path.len() - file_name(path).len()]))
}

pub fn parse(path: &str, text: &str, lock: Option<&str>) -> Vec<Dep> {
    match kind(path) {
        Some(Kind::Pom) => pom(path, text),
        Some(Kind::Gradle) => gradle(path, text),
        Some(Kind::PackageJson) => package_json(path, text, lock),
        Some(Kind::PackageLock) => package_lock(path, text).0,
        Some(Kind::ComposerJson) => composer_json(path, text, lock),
        Some(Kind::ComposerLock) => composer_lock(path, text).0,
        Some(Kind::VersionCatalog) => version_catalog(path, text),
        Some(Kind::YarnLock) => yarn_lock(path, text).0,
        Some(Kind::PnpmLock) => pnpm_lock(path, text).0,
        Some(Kind::CargoLock) => toml_lock(path, text, Ecosystem::CratesIo).0,
        Some(Kind::PoetryLock) => toml_lock(path, text, Ecosystem::PyPI).0,
        Some(Kind::UvLock) => uv_lock(path, text),
        Some(Kind::PipfileLock) => pipfile_lock(path, text),
        Some(Kind::Requirements) => requirements(path, text),
        Some(Kind::Pylock) => pylock(path, text),
        Some(Kind::Pyproject) => crate::pyproject::parse(path, text).0,
        None => Vec::new(),
    }
}

/// Trimmed text of a closed element and its byte range.
fn inner_text<'a>(text: &'a str, el: &Element) -> Option<(&'a str, (usize, usize))> {
    let body = text.get(el.tag_end..el.end)?;
    if !body.ends_with('>') {
        return None;
    }
    let close = body.rfind("</")?;
    let raw = &body[..close];
    let trimmed = raw.trim();
    let from = el.tag_end + (raw.len() - raw.trim_start().len());

    Some((trimmed, (from, from + trimmed.len())))
}

fn pom_dependency_site(els: &[Element], index: usize) -> bool {
    let mut names=Vec::new();let mut parent=els[index].parent;
    while let Some(i)=parent {
        if names.len()>=16 {return false;}
        names.push(els[i].name.as_str());parent=els[i].parent;
    }
    if names.ends_with(&["profile","profiles","project"]) {names.truncate(names.len()-3);names.push("project");}
    matches!(names.as_slice(),
        ["dependencies","project"] |
        ["dependencies","dependencyManagement","project"] |
        ["dependencies","plugin","plugins","build","project"] |
        ["dependencies","plugin","plugins","pluginManagement","build","project"])
}

fn pom(path: &str, text: &str) -> Vec<Dep> {
    let els = elements(text);
    // The shared tolerant scanner recovers unfinished/mismatched XML for editor
    // features. Recovered containers cannot establish an editable POM version.
    if els.iter().any(|e| {
        let Some(start)=text.get(e.start..e.tag_end) else {return true;};
        if start.ends_with("/>") {return false;}
        let Some(body)=text.get(e.tag_end..e.end) else {return true;};
        let Some(close)=body.rfind("</").and_then(|at|body.get(at+2..)).and_then(|s|s.strip_suffix('>')) else {return true;};
        close.trim()!=e.name
    }) {return Vec::new();}
    let roots:Vec<_>=els.iter().enumerate().filter(|(_,e)|e.parent.is_none()).collect();
    let [(root, project)]=roots.as_slice() else {return Vec::new();};
    if project.name!="project" {return Vec::new();}
    let root_ns=project.attr("xmlns").map_or("",|a|a.value.as_str());
    if !matches!(root_ns,""|"http://maven.apache.org/POM/4.0.0") {return Vec::new();}
    let mut namespaces:Vec<&str>=Vec::with_capacity(els.len());
    let mut owned:Vec<bool>=Vec::with_capacity(els.len());
    for e in &els {
        let ns=e.attr("xmlns").map(|a|a.value.as_str()).unwrap_or_else(||e.parent.map_or("",|p|namespaces[p]));
        namespaces.push(ns);
        owned.push(!e.name.contains(':')&&ns==root_ns&&e.parent.is_none_or(|p|owned[p]));
    }
    let properties: Vec<(&str, &str, (usize, usize), bool)> = els
        .iter()
        .enumerate()
        .filter(|(i,e)| owned[*i] && e.parent.is_some_and(|p| els[p].name == "properties"))
        .map(|(_,e)|e)
        .filter_map(|e| inner_text(text, e).map(|(value, range)| (e.name.as_str(), value, range, e.parent.is_some_and(|p|els[p].parent==Some(*root)))))
        .collect();

    let mut children: HashMap<usize, [Option<usize>; 3]> = HashMap::new();
    let mut ambiguous=HashSet::new();
    for (child, el) in els.iter().enumerate() {
        if !owned[child] {continue;}
        let Some(parent) = el.parent else {
            continue;
        };
        let Some(slot) = ["groupId", "artifactId", "version"].iter().position(|n| *n == el.name) else {
            continue;
        };

        if children.entry(parent).or_default()[slot].replace(child).is_some() {ambiguous.insert(parent);}
    }

    let mut deps = Vec::new();
    for (index, dependency) in els.iter().enumerate() {
        if !owned[index] || dependency.name != "dependency" || ambiguous.contains(&index) || !pom_dependency_site(&els,index) {
            continue;
        }
        let read = |slot: usize| {
            let child = children.get(&index)?[slot]?;

            inner_text(text, &els[child])
        };
        let (Some((group, _)), Some((artifact, at)), Some((declared, declared_at))) =
            (read(0), read(1), read(2))
        else {
            continue;
        };

        let (version, version_at) = match declared.strip_prefix("${").and_then(|d| d.strip_suffix('}')) {
            Some(key) => {
                let matches:Vec<_>=properties.iter().filter(|(name,_,_,_)|*name==key).collect();
                let [(_,value,range,true)]=matches.as_slice() else {continue;};
                (*value,*range)
            },
            None => (declared, declared_at),
        };
        if !version.starts_with(|c: char| c.is_ascii_digit()) {
            continue;
        }

        deps.push(Dep {
            path: path.to_owned(),
            ecosystem: Ecosystem::Maven,
            name: format!("{group}:{artifact}"),
            version: version.to_owned(),
            at,
            version_at: Some(version_at),
        });
    }

    deps
}

fn gradle(path: &str, text: &str) -> Vec<Dep> {
    let is_name = |part: &str| !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c));
    let configurations = ["implementation", "api", "compile", "compileOnly", "compileOnlyApi", "runtime", "runtimeOnly", "testCompile", "testImplementation", "testCompileOnly", "testRuntime", "testRuntimeOnly", "annotationProcessor", "testAnnotationProcessor", "kapt", "ksp", "classpath", "platform", "enforcedPlatform"];
    let bytes=text.as_bytes();
    let mut deps=Vec::new();let mut i=0;
    while i<bytes.len() {
        if bytes[i..].starts_with(b"//") { while i<bytes.len() && bytes[i]!=b'\n' {i+=1;} continue; }
        if bytes[i..].starts_with(b"/*") {
            i+=2;let mut depth=1usize;
            while i<bytes.len()&&depth>0 {
                if bytes[i..].starts_with(b"/*") {depth+=1;if depth>64 {return Vec::new();}i+=2;}
                else if bytes[i..].starts_with(b"*/") {depth-=1;i+=2;}
                else {i+=1;}
            }
            continue;
        }
        // Groovy slashy/regex forms need a language parser; withhold rather
        // than interpreting quote-like content inside them as declarations.
        // A `/` after anything but an operator is division.
        if bytes[i]==b'/' {
            let previous=text[..i].trim_end().bytes().next_back();
            if previous.is_none_or(|b|b"=(,~+-*%&|!<>?:^{;".contains(&b)) {return Vec::new();}
            i+=1;continue;
        }
        if !matches!(bytes[i], b'\''|b'"') {i+=1;continue;}
        let quote=bytes[i];
        if bytes.get(i..i+3).is_some_and(|s|s.iter().all(|b|*b==quote)) {
            i+=3;
            while i<bytes.len()&&!bytes.get(i..i+3).is_some_and(|s|s.iter().all(|b|*b==quote)) {i+=1;}
            i=(i+3).min(bytes.len());continue;
        }
        let open=i;i+=1;let from=i;let mut escaped=false;
        while i<bytes.len()&&bytes[i]!=quote&&bytes[i]!=b'\n' {
            if bytes[i]==b'\\' {escaped=true;i=(i+2).min(bytes.len());} else {i+=1;}
        }
        if i>=bytes.len()||bytes[i]!=quote {continue;}
        let to=i;i+=1;
        if escaped {continue;}
        let prefix=text[..open].trim_end().trim_end_matches('(').trim_end();
        let name_start=prefix.char_indices().rev().find(|(_,c)|!c.is_ascii_alphanumeric()&&*c!='_').map_or(0,|(p,c)|p+c.len_utf8());
        if !configurations.contains(&&prefix[name_start..]) {continue;}
        // A literal prefix of concatenation/coalescing is not a version declaration.
        if text[i..].trim_start().starts_with(['+', '.', '?', '$']) {continue;}
        let parts:Vec<_>=text[from..to].split(':').collect();
        let [group,artifact,version]=parts[..] else {continue;};
        if !is_name(group)||!is_name(artifact)||!version.starts_with(|c:char|c.is_ascii_digit())||version.contains('$') {continue;}
        deps.push(Dep {path:path.into(),ecosystem:Ecosystem::Maven,name:format!("{group}:{artifact}"),version:version.into(),at:(from,to),version_at:Some((to-version.len(),to))});
    }
    deps
}

/// The spec's leading operator (`^`, `~`, `>=`, `v`) followed by `fix`.
pub fn bumped(spec: &str, fix: &str) -> String {
    let operator = spec.len() - spec.trim_start_matches(|c: char| !c.is_ascii_digit()).len();

    format!("{}{fix}", &spec[..operator])
}

fn is_exact(spec: &str) -> bool {
    let (release, tag) = spec.split_at(spec.find(['-', '+']).unwrap_or(spec.len()));

    release.starts_with(|c: char| c.is_ascii_digit())
        && release.chars().all(|c| c.is_ascii_digit() || c == '.')
        && tag.chars().all(|c| c.is_ascii_alphanumeric() || ".-+".contains(c))
}

/// Offset just after the `:` that follows `pos`, skipping whitespace.
fn after_colon(text: &str, pos: usize) -> Option<usize> {
    let rest = text.get(pos..)?.trim_start().strip_prefix(':')?;

    Some(text.len() - rest.trim_start().len())
}

/// Byte ranges (key contents, value contents) of the first `"key"\s*:\s*"value"`.
fn find_entry(text: &str, key: &str, value: &str) -> Option<((usize, usize), (usize, usize))> {
    let needle = format!("\"{key}\"");
    let mut from = 0;
    while let Some(found) = text[from..].find(&needle) {
        let key_start = from + found;
        from = key_start + needle.len();

        let Some(value_start) = after_colon(text, from).filter(|&at| text[at..].starts_with('"')).map(|at| at + 1) else {
            continue;
        };
        let rest = &text[value_start..];
        if rest.starts_with(value) && rest[value.len()..].starts_with('"') {
            return Some(((key_start + 1, key_start + 1 + key.len()), (value_start, value_start + value.len())));
        }
    }

    None
}

/// Offset of the `"key":` that opens a section; entries are searched from there so the same name in another section cannot match.
fn section_start(text: &str, key: &str) -> Option<usize> {
    let needle = format!("\"{key}\"");
    let mut from = 0;
    while let Some(found) = text[from..].find(&needle) {
        let at = from + found;
        from = at + needle.len();
        if after_colon(text, from).is_some() {
            return Some(at);
        }
    }

    None
}

fn shift(range: (usize, usize), base: usize) -> (usize, usize) {
    (base + range.0, base + range.1)
}

/// `(name, spec, section offset)` of every string entry in the object `section`.
fn section_entries<'a>(root: &'a Value, text: &str, section: &str) -> Vec<(&'a str, &'a str, usize)> {
    let (Some(entries), Some(base)) = (root.get(section).and_then(Value::as_object), section_start(text, section)) else {
        return Vec::new();
    };

    entries.iter().filter_map(|(name, spec)| Some((name.as_str(), spec.as_str()?, base))).collect()
}

fn package_json(path: &str, text: &str, lock: Option<&str>) -> Vec<Dep> {
    let Ok(root) = serde_json::from_str::<Value>(text) else {
        return Vec::new();
    };
    let lock: Option<Value> = lock.and_then(|l| serde_json::from_str(l).ok());

    let mut deps = Vec::new();
    for section in ["dependencies", "devDependencies"] {
        for (name, spec, base) in section_entries(&root, text, section) {
            if let Some(target) = spec.strip_prefix("npm:") {
                deps.extend(npm_alias(path, text, base, name, spec, target));

                continue;
            }
            if spec.contains(':') {
                continue;
            }
            let local = lock.as_ref().is_some_and(|l| {
                l.get("packages").and_then(|p| p.get(format!("node_modules/{name}"))).is_some_and(|entry| entry.get("link").is_some() || private_npm(entry))
                    || (l.get("lockfileVersion").and_then(Value::as_u64) == Some(1) && l.get("dependencies").and_then(|d| d.get(name)).is_some_and(private_npm))
            });
            if local {
                continue;
            }
            let locked = lock
                .as_ref()
                .and_then(|l| {
                    l.get("packages").and_then(|p| p.get(format!("node_modules/{name}")))
                        .or_else(|| (l.get("lockfileVersion").and_then(Value::as_u64) == Some(1)).then(|| l.get("dependencies")?.get(name)).flatten())
                        .and_then(|entry| entry.get("version")?.as_str())
                });
            let version = match locked {
                Some(version) => version,
                None if is_exact(spec) => spec,
                None => continue,
            };
            let Some((at, version_at)) = find_entry(&text[base..], name, spec) else {
                continue;
            };

            deps.push(Dep {
                path: path.to_owned(),
                ecosystem: Ecosystem::Npm,
                name: name.to_owned(),
                version: version.to_owned(),
                at: shift(at, base),
                version_at: Some(shift(version_at, base)),
            });
        }
    }

    deps
}

/// `"alias": "npm:<package>@<exact version>"` names the real registry package; ranges, tags and bare names stay unknown.
/// The editor fix is withheld because it would rewrite the alias spec.
fn npm_alias(path: &str, text: &str, base: usize, alias: &str, spec: &str, target: &str) -> Option<Dep> {
    let split = target.rfind('@').filter(|&i| i > 0)?;
    let (package, version) = (&target[..split], &target[split + 1..]);
    let scoped = package.strip_prefix('@').unwrap_or(package);
    let valid = !scoped.is_empty()
        && scoped.matches('/').count() == usize::from(package.starts_with('@'))
        && scoped.chars().all(|c| c.is_ascii_alphanumeric() || "-_.~/".contains(c));
    if !valid || !is_exact(version) {
        return None;
    }
    let (at, _) = find_entry(&text[base..], alias, spec)?;

    Some(Dep {
        path: path.to_owned(),
        ecosystem: Ecosystem::Npm,
        name: package.to_owned(),
        version: version.to_owned(),
        at: shift(at, base),
        version_at: None,
    })
}

/// Offset of every object key after `from`, first occurrence wins; one pass keeps thousands of lock entries linear.
fn key_offsets(text: &str, from: usize) -> HashMap<&str, usize> {
    let bytes = text.as_bytes();
    let mut keys = HashMap::new();
    let mut i = from;
    while i < bytes.len() {
        if bytes[i] != b'"' {
            i += 1;
            continue;
        }
        let start = i + 1;
        i = start;
        while i < bytes.len() && bytes[i] != b'"' {
            i += if bytes[i] == b'\\' { 2 } else { 1 };
        }
        let Some(content) = text.get(start..i).filter(|_| i < bytes.len()) else {
            break;
        };
        i += 1;
        if after_colon(text, i).is_some() {
            keys.entry(content).or_insert(start);
        }
    }

    keys
}

/// Audited coordinates and those skipped because the lockfile resolved them from a private host.
fn package_lock(path: &str, text: &str) -> (Vec<Dep>, Vec<Dep>) {
    let Ok(root) = serde_json::from_str::<Value>(text) else {
        return Default::default();
    };
    if root.get("lockfileVersion").and_then(Value::as_u64) == Some(1) {
        return package_lock_v1(path, text);
    }
    let (Some(packages), Some(base)) = (root.get("packages").and_then(Value::as_object), section_start(text, "packages")) else {
        return Default::default();
    };
    let offsets = key_offsets(text, base);

    let mut deps = Vec::new();
    let mut private = Vec::new();
    for (key, entry) in packages {
        let Some(installed) = key.rsplit("node_modules/").next().filter(|_| key.contains("node_modules/")) else {
            continue;
        };
        if entry.get("link").is_some() {
            continue;
        }
        let (Some(version), Some(&start)) = (entry.get("version").and_then(Value::as_str), offsets.get(key.as_str())) else {
            continue;
        };

        let dep = Dep {
            path: path.to_owned(),
            ecosystem: Ecosystem::Npm,
            name: entry.get("name").and_then(Value::as_str).unwrap_or(installed).to_owned(),
            version: version.to_owned(),
            at: (start, start + key.len()),
            version_at: None,
        };
        match private_npm(entry) {
            true => private.push(dep),
            false => deps.push(dep),
        }
    }

    (deps, private)
}

/// Object members with exact physical source ranges. The document is parsed
/// before this runs; serde's stream offsets keep repeated nested package names
/// distinct and handle quoted/escaped strings without text-search guesses.
pub(crate) fn object_members(text: &str, start: usize) -> Option<Vec<(String, (usize, usize), usize)>> {
    let mut at = start;
    while text.as_bytes().get(at).is_some_and(u8::is_ascii_whitespace) { at += 1; }
    if text.as_bytes().get(at) != Some(&b'{') { return None; }
    at += 1;
    let mut entries = Vec::new();
    let mut names = HashSet::new();
    loop {
        while text.as_bytes().get(at).is_some_and(u8::is_ascii_whitespace) { at += 1; }
        if text.as_bytes().get(at) == Some(&b'}') { return Some(entries); }
        let key_start = at;
        let mut key = serde_json::Deserializer::from_str(text.get(at..)?).into_iter::<String>();
        let name = key.next()?.ok()?; if !names.insert(name.clone()) { return None; } at += key.byte_offset(); let key_end = at;
        at = after_colon(text, at)?;
        let value_at = at;
        let mut value = serde_json::Deserializer::from_str(text.get(at..)?).into_iter::<Value>();
        value.next()?.ok()?; at += value.byte_offset();
        entries.push((name, (key_start + 1, key_end - 1), value_at));
        if entries.len() > 10_000 { return None; }
        while text.as_bytes().get(at).is_some_and(u8::is_ascii_whitespace) { at += 1; }
        match text.as_bytes().get(at) { Some(b',') => at += 1, Some(b'}') => return Some(entries), _ => return None }
    }
}

fn package_lock_v1(path: &str, text: &str) -> (Vec<Dep>, Vec<Dep>) {
    fn walk(path: &str, text: &str, at: usize, depth: usize, deps: &mut Vec<Dep>, private: &mut Vec<Dep>) -> Option<()> {
        if depth > 32 { return None; }
        for (name, span, start) in object_members(text, at)? {
            let members = object_members(text, start)?;
            let version = members.iter().find(|(name,_,_)| name == "version").and_then(|(_,_,at)| {
                serde_json::Deserializer::from_str(&text[*at..]).into_iter::<String>().next()?.ok()
            });
            let resolved = members.iter().find(|(name,_,_)| name == "resolved").and_then(|(_,_,at)| {
                serde_json::Deserializer::from_str(&text[*at..]).into_iter::<String>().next()?.ok()
            });
            if let Some(version) = version.filter(|version| is_version(version)) {
                let dep = Dep { path:path.into(), ecosystem:Ecosystem::Npm, name, version, at:span, version_at:None };
                match private_url(resolved.as_deref(), NPM_HOSTS) {
                    true => private.push(dep),
                    false => deps.push(dep),
                }
                if deps.len() + private.len() > 10_000 { return None; }
            }
            if let Some((_,_,at)) = members.iter().find(|(name,_,_)| name == "dependencies") {
                walk(path, text, *at, depth + 1, deps, private)?;
            }
        }
        Some(())
    }
    let Some(members) = object_members(text, 0) else { return Default::default(); };
    let Some((_,_,at)) = members.iter().find(|(name,_,_)| name == "dependencies") else { return Default::default(); };
    let (mut deps, mut private) = (Vec::new(), Vec::new());
    if walk(path,text,*at,0,&mut deps,&mut private).is_none() { return Default::default(); }
    (deps, private)
}

fn pipfile_lock(path: &str, text: &str) -> Vec<Dep> {
    let Ok(root) = serde_json::from_str::<Value>(text) else {return Vec::new();};
    let Some(sources) = root.pointer("/_meta/sources").and_then(Value::as_array) else {return Vec::new();};
    let pypi_sources: HashSet<_> = sources.iter().filter(|source| source.get("verify_ssl").and_then(Value::as_bool) == Some(true)
        && matches!(source.get("url").and_then(Value::as_str), Some("https://pypi.org/simple"|"https://pypi.org/simple/")))
        .filter_map(|source|source.get("name").and_then(Value::as_str)).collect();
    if pypi_sources.is_empty() {return Vec::new();}
    let Some(sections) = object_members(text,0) else {return Vec::new();};
    let mut deps=Vec::new();
    for (section,_,at) in sections.into_iter().filter(|(name,_,_)|matches!(name.as_str(),"default"|"develop")) {
        let Some(packages)=object_members(text,at) else {return Vec::new();};
        for (name,span,_) in packages {
            let Some(entry)=root.get(&section).and_then(|section|section.get(&name)).and_then(Value::as_object) else {continue;};
            if ["git","path","file","editable"].iter().any(|key|entry.contains_key(*key)) {continue;}
            let registry=match entry.get("index").and_then(Value::as_str) {Some(index)=>pypi_sources.contains(index),None=>sources.len()==1&&pypi_sources.len()==1};
            let Some(version)=entry.get("version").and_then(Value::as_str).and_then(|v|v.strip_prefix("==")).filter(|v|is_version(v)) else {continue;};
            if registry {deps.push(Dep{path:path.into(),ecosystem:Ecosystem::PyPI,name,version:version.into(),at:span,version_at:None});}
            if deps.len()>10_000 {return Vec::new();}
        }
    }
    deps
}

fn locked_composer_version(entry: &Value) -> Option<&str> {
    let version = entry.get("version")?.as_str()?;
    let version = version.strip_prefix('v').unwrap_or(version);

    version.starts_with(|c: char| c.is_ascii_digit()).then_some(version)
}

fn composer_json(path: &str, text: &str, lock: Option<&str>) -> Vec<Dep> {
    let Ok(root) = serde_json::from_str::<Value>(text) else {
        return Vec::new();
    };
    let lock: Option<Value> = lock.and_then(|l| serde_json::from_str(l).ok());
    let locked_entry = |name: &str| {
        let lock = lock.as_ref()?;
        ["packages", "packages-dev"]
            .into_iter()
            .filter_map(|section| lock.get(section)?.as_array())
            .flatten()
            .find(|entry| entry.get("name").and_then(Value::as_str) == Some(name))
    };

    let mut deps = Vec::new();
    for section in ["require", "require-dev"] {
        for (name, spec, base) in section_entries(&root, text, section) {
            if !name.contains('/') {
                continue;
            }
            let entry = locked_entry(name);
            if entry.is_some_and(private_composer) {
                continue;
            }
            let version = match entry.and_then(locked_composer_version) {
                Some(version) => version,
                None if is_exact(spec) => spec,
                None => continue,
            };
            let Some((at, version_at)) = find_entry(&text[base..], name, spec) else {
                continue;
            };

            deps.push(Dep {
                path: path.to_owned(),
                ecosystem: Ecosystem::Packagist,
                name: name.to_owned(),
                version: version.to_owned(),
                at: shift(at, base),
                version_at: Some(shift(version_at, base)),
            });
        }
    }

    deps
}

fn composer_lock(path: &str, text: &str) -> (Vec<Dep>, Vec<Dep>) {
    let Ok(root) = serde_json::from_str::<Value>(text) else {
        return Default::default();
    };

    let mut deps = Vec::new();
    let mut private = Vec::new();
    for section in ["packages", "packages-dev"] {
        let (Some(entries), Some(mut cursor)) = (root.get(section).and_then(Value::as_array), section_start(text, section)) else {
            continue;
        };
        for entry in entries {
            let (Some(name), Some(version)) = (entry.get("name").and_then(Value::as_str), locked_composer_version(entry)) else {
                continue;
            };
            // Entries are in document order, so resuming after the previous hit keeps this linear.
            let Some((_, name_at)) = find_entry(&text[cursor..], "name", name) else {
                continue;
            };
            let at = shift(name_at, cursor);
            cursor = at.1;

            let dep = Dep {
                path: path.to_owned(),
                ecosystem: Ecosystem::Packagist,
                name: name.to_owned(),
                version: version.to_owned(),
                at,
                version_at: None,
            };
            match private_composer(entry) {
                true => private.push(dep),
                false => deps.push(dep),
            }
        }
    }

    (deps, private)
}

/// `(offset, line without its line break)` for every line.
fn lines(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut offset = 0;

    text.split_inclusive('\n').map(move |raw| {
        let start = offset;
        offset += raw.len();

        (start, raw.trim_end_matches(['\n', '\r']))
    })
}

fn offset_in(text: &str, part: &str) -> usize {
    part.as_ptr() as usize - text.as_ptr() as usize
}

fn lock_dep(path: &str, ecosystem: Ecosystem, name: &str, version: &str, at: usize) -> Dep {
    Dep {
        path: path.to_owned(),
        ecosystem,
        name: name.to_owned(),
        version: version.to_owned(),
        at: (at, at + name.len()),
        version_at: None,
    }
}

fn is_version(version: &str) -> bool {
    version.starts_with(|c: char| c.is_ascii_digit())
}

/// Contents of the leading `"..."` or `'...'` of `text` and the offset of the contents within it.
fn quoted(text: &str) -> Option<(&str, usize)> {
    let quote = text.chars().next().filter(|c| *c == '"' || *c == '\'')?;
    let rest = &text[1..];

    Some((&rest[..rest.find(quote)?], 1))
}

/// Contents and absolute offset of the string in `key = "value"` found in `line`.
fn toml_string<'a>(line: &'a str, line_at: usize, key: &str) -> Option<(&'a str, usize)> {
    let mut from = 0;
    while let Some(found) = line[from..].find(key) {
        let start = from + found;
        from = start + key.len();

        let boundary = line[..start].chars().next_back().is_none_or(|c| !(c.is_ascii_alphanumeric() || "_-".contains(c)));
        let Some(rest) = line[from..].trim_start().strip_prefix('=').filter(|_| boundary) else {
            continue;
        };
        let rest = rest.trim_start();
        let (value, inside) = quoted(rest)?;

        return Some((value, line_at + line.len() - rest.len() + inside));
    }

    None
}

/// `toml_string` for a key at the very start of the line, so rows of multi-line arrays and inline tables never match.
fn toml_key<'a>(line: &'a str, line_at: usize, key: &str) -> Option<(&'a str, usize)> {
    line.starts_with(key).then(|| toml_string(line, line_at, key)).flatten()
}

fn version_catalog(path: &str, text: &str) -> Vec<Dep> {
    let mut versions: HashMap<&str, &str> = HashMap::new();
    let mut section = "";
    let mut libraries: Vec<(usize, &str)> = Vec::new();
    for (at, line) in lines(text) {
        let trimmed = line.trim();
        if let Some(header) = trimmed.strip_prefix('[') {
            section = header.trim_end_matches(']').trim();

            continue;
        }
        match section {
            "versions" => {
                if let Some((key, value)) = trimmed.split_once('=')
                    && let Some((version, _)) = quoted(value.trim_start())
                {
                    versions.insert(key.trim(), version);
                }
            }
            "libraries" if !trimmed.starts_with('#') => libraries.push((at, line)),
            _ => {}
        }
    }

    let mut deps = Vec::new();
    for (at, line) in libraries {
        let Some((_, value)) = line.split_once('=') else {
            continue;
        };
        let value_at = at + line.len() - value.trim_start().len();
        let entry = |key: &str| toml_string(line, at, key);

        let (name, name_at, literal) = match (entry("module"), entry("group"), entry("name")) {
            (Some((module, module_at)), ..) => (module.to_owned(), module_at, None),
            (None, Some((group, group_at)), Some((artifact, _))) => (format!("{group}:{artifact}"), group_at, None),
            _ => match quoted(value.trim_start()) {
                Some((coordinates, inside)) => {
                    let parts: Vec<&str> = coordinates.split(':').collect();
                    let [group, artifact, version] = parts[..] else {
                        continue;
                    };

                    (format!("{group}:{artifact}"), value_at + inside, Some(version))
                }
                None => continue,
            },
        };
        let version = literal
            .or_else(|| entry("version").map(|(version, _)| version))
            .or_else(|| entry("version.ref").or_else(|| entry("ref")).and_then(|(key, _)| versions.get(key).copied()));
        let Some(version) = version.filter(|v| is_version(v)) else {
            continue;
        };

        let mut dep = lock_dep(path, Ecosystem::Maven, &name, version, name_at);
        let version_at = offset_in(text, version);
        dep.version_at = Some((version_at, version_at + version.len()));
        dep.at.1 = name_at + name.len().max(1);
        deps.push(dep);
    }

    deps
}

fn yarn_lock(path: &str, text: &str) -> (Vec<Dep>, Vec<Dep>) {
    let mut deps = Vec::new();
    let mut private = Vec::new();
    let mut header: Option<(&str, usize)> = None;
    let mut pending: Option<Dep> = None;
    for (at, line) in lines(text) {
        if !line.starts_with([' ', '#']) && line.ends_with(':') {
            deps.extend(pending.take());
            header = line.trim_end_matches(':').split(',').next().and_then(|first| {
                let first = first.trim();
                let (spec, inside) = quoted(first).unwrap_or((first.trim_start_matches('"'), usize::from(first.starts_with('"'))));
                let split = spec.char_indices().skip(1).find(|&(_, c)| c == '@')?.0;
                let (name, range) = (&spec[..split], &spec[split + 1..]);
                let Some(alias) = range.strip_prefix("npm:") else {
                    return (!range.contains(':')).then_some((name, at + inside));
                };
                let Some(real) = alias.rfind('@').filter(|&i| i > 0).map(|i| &alias[..i]) else {
                    return Some((name, at + inside));
                };

                Some((real, at + inside + spec.len() - alias.len()))
            });

            continue;
        }

        if let Some(resolved) = line.strip_prefix("  resolved") {
            let url = resolved.trim_start_matches(':').trim().trim_matches('"');
            if private_url(Some(url), NPM_HOSTS) {
                private.extend(pending.take());
            }

            continue;
        }
        let Some(rest) = line.strip_prefix("  version") else {
            continue;
        };
        let Some((name, name_at)) = header else {
            continue;
        };
        let version = rest.trim_start_matches(':').trim().trim_matches('"');
        if is_version(version) {
            deps.extend(pending.replace(lock_dep(path, Ecosystem::Npm, name, version, name_at)));
        }
    }
    deps.extend(pending);

    (deps, private)
}

/// `packages:` entries of `pnpm-lock.yaml`, with those whose `resolution.tarball` is on a private host skipped.
fn pnpm_lock(path: &str, text: &str) -> (Vec<Dep>, Vec<Dep>) {
    let (mut deps, mut private) = (Vec::new(), Vec::new());
    let mut pending: Option<(Dep, bool)> = None;
    let mut flush = |pending: &mut Option<(Dep, bool)>| match pending.take() {
        Some((dep, true)) => private.push(dep),
        Some((dep, false)) => deps.push(dep),
        None => {}
    };
    let mut in_packages = false;
    for (at, line) in lines(text) {
        if !line.starts_with([' ', '#']) && !line.is_empty() {
            flush(&mut pending);
            in_packages = line.trim_end() == "packages:";

            continue;
        }
        if !in_packages {
            continue;
        }
        if line.starts_with("   ") {
            if let Some((_, url)) = line.split_once("tarball:") {
                let url = url.split([',', '}']).next().unwrap_or_default().trim().trim_matches(['\'', '"']);
                if let Some((_, hidden)) = pending.as_mut() {
                    *hidden |= private_url(Some(url), NPM_HOSTS);
                }
            }

            continue;
        }
        let Some(key) = line.strip_prefix("  ").and_then(|k| k.strip_suffix(':')) else {
            continue;
        };
        flush(&mut pending);
        let (key, inside) = quoted(key).unwrap_or((key, 0));
        let slash = usize::from(key.starts_with('/'));
        let key = key.split('(').next().unwrap_or_default();
        let (name, version) = match key.rsplit_once('/').filter(|(name, version)| slash == 1 && name.len() > 1 && is_version(version)) {
            Some((name, version)) => (&name[1..], version.split('_').next().unwrap_or_default()),
            None => match key[slash..].rfind('@').filter(|&i| i > 0) {
                Some(split) => (&key[slash..slash + split], &key[slash + split + 1..]),
                None => continue,
            },
        };
        if is_version(version) {
            pending = Some((lock_dep(path, Ecosystem::Npm, name, version, at + 2 + inside + slash), false));
        }
    }
    flush(&mut pending);

    (deps, private)
}

/// `[[package]]` tables of `Cargo.lock` and `poetry.lock`, with the packages skipped as private.
/// Cargo members and path or git packages have no registry source; poetry packages with a `[package.source]` table come from outside PyPI.
fn toml_lock(path: &str, text: &str, ecosystem: Ecosystem) -> (Vec<Dep>, Vec<Dep>) {
    #[derive(Default)]
    struct Record<'a> {
        name: &'a str,
        at: usize,
        version: &'a str,
        public: bool,
        private: bool,
        source_table: bool,
    }

    let cargo = ecosystem == Ecosystem::CratesIo;
    let mut deps = Vec::new();
    let mut private = Vec::new();
    let mut current: Option<Record> = None;
    let mut keys = false;
    let mut flush = |current: &mut Option<Record>| {
        let Some(record) = current.take().filter(|r| !r.name.is_empty() && is_version(r.version)) else {
            return;
        };
        let dep = lock_dep(path, ecosystem, record.name, record.version, record.at);
        match (record.private || record.source_table, record.public || !cargo) {
            (true, _) => private.push(dep),
            (false, true) => deps.push(dep),
            (false, false) => {}
        }
    };
    for (at, line) in lines(text) {
        if line.starts_with('[') {
            let header = line.trim();
            keys = false;
            if header == "[[package]]" {
                flush(&mut current);
                current = Some(Record::default());
                keys = true;
            } else if header == "[package.source]" && !cargo {
                current.iter_mut().for_each(|r| r.source_table = true);
            } else if !header.starts_with("[package.") {
                flush(&mut current);
            }

            continue;
        }
        let Some(package) = current.as_mut().filter(|_| keys) else {
            continue;
        };
        if let Some((name, name_at)) = toml_key(line, at, "name") {
            (package.name, package.at) = (name, name_at);
        } else if let Some((version, _)) = toml_key(line, at, "version") {
            package.version = version;
        } else if let Some((source, _)) = toml_key(line, at, "source") {
            package.public = matches!(source, CRATES_IO_GIT | CRATES_IO_SPARSE);
            package.private = !package.public && (source.starts_with("registry+") || source.starts_with("sparse+"));
        }
    }
    flush(&mut current);

    (deps, private)
}

const CRATES_IO_GIT: &str = "registry+https://github.com/rust-lang/crates.io-index";
const CRATES_IO_SPARSE: &str = "sparse+https://index.crates.io/";

/// uv's resolved package records. Only canonical PyPI registry identities are
/// admitted: path, editable, Git and URL distributions are not OSV PyPI identities.
/// This intentionally does not evaluate markers, extras or dependency reachability.
fn uv_lock(path: &str, text: &str) -> Vec<Dep> {
    let mut deps = Vec::new();
    let mut package: Option<(&str, usize, &str, bool)> = None;
    let flush = |package: &mut Option<(&str, usize, &str, bool)>, deps: &mut Vec<Dep>| {
        if let Some((name, at, version, registry)) = package.take()
            && registry && !name.is_empty() && is_version(version)
        {
            deps.push(lock_dep(path, Ecosystem::PyPI, name, version, at));
        }
    };
    for (at, line) in lines(text) {
        let line_trimmed = line.trim();
        if line_trimmed.starts_with('[') {
            flush(&mut package, &mut deps);
            if line_trimmed == "[[package]]" { package = Some(("", 0, "", false)); }
            continue;
        }
        let Some(record) = package.as_mut() else { continue; };
        if let Some((name, start)) = toml_key(line, at, "name") {
            (record.0, record.1) = (name, start);
        } else if let Some((version, _)) = toml_key(line, at, "version") {
            record.2 = version;
        } else if let Some((key, value)) = line_trimmed.split_once('=')
            && key.trim() == "source"
        {
            // uv emits a one-line inline table with one source-kind field.
            // Additional fields, escapes and dynamic/non-registry forms stay unknown.
            record.3 = value.trim().strip_prefix('{').and_then(|v| v.strip_suffix('}'))
                .and_then(|v| v.trim().split_once('='))
                .is_some_and(|(key, value)| key.trim() == "registry"
                    && matches!(value.trim(), "\"https://pypi.org/simple\"" | "\"https://pypi.org/simple/\""));
        }
    }
    flush(&mut package, &mut deps);
    deps
}

/// PEP 751 `[[packages]]`: only an explicit pypi.org `index` with a `version` is a PyPI identity.
/// VCS, directory and archive packages, other indexes and missing versions stay unknown; markers are not evaluated.
fn pylock(path: &str, text: &str) -> Vec<Dep> {
    let mut deps = Vec::new();
    let mut package: Option<(&str, usize, &str, bool)> = None;
    let flush = |package: &mut Option<(&str, usize, &str, bool)>, deps: &mut Vec<Dep>| {
        if let Some((name, at, version, registry)) = package.take()
            && registry && !name.is_empty() && is_version(version)
        {
            deps.push(lock_dep(path, Ecosystem::PyPI, name, version, at));
        }
    };
    let mut keys = false;
    for (at, line) in lines(text) {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            let header = trimmed.trim_matches(['[', ']']);
            keys = false;
            if trimmed == "[[packages]]" {
                flush(&mut package, &mut deps);
                package = Some(("", 0, "", false));
                keys = true;
            } else if matches!(header, "packages.vcs" | "packages.directory" | "packages.archive") {
                if let Some(record) = package.as_mut() {
                    record.3 = false;
                    record.0 = "";
                }
            } else if !header.starts_with("packages.") {
                flush(&mut package, &mut deps);
            }
            continue;
        }
        let Some(record) = package.as_mut().filter(|_| keys) else { continue; };
        if let Some((name, start)) = toml_string(line, at, "name") {
            (record.0, record.1) = (name, start);
        } else if let Some((version, _)) = toml_string(line, at, "version") {
            record.2 = version;
        } else if let Some((key, value)) = trimmed.split_once('=')
            && key.trim() == "index"
        {
            record.3 = matches!(value.trim(), "\"https://pypi.org/simple\"" | "\"https://pypi.org/simple/\"");
        }
    }
    flush(&mut package, &mut deps);

    deps
}

fn requirements(path: &str, text: &str) -> Vec<Dep> {
    let mut deps = Vec::new();
    for (at, line) in lines(text) {
        let spec = line.split(['#', ';']).next().unwrap_or_default();
        let Some((name, version)) = spec.split_once("==") else {
            continue;
        };
        let name = name.trim();
        let name = name.split('[').next().unwrap_or(name);
        let version = version.split_whitespace().next().unwrap_or_default();
        let is_name = !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c));
        if is_name && is_version(version) && !version.contains('*') {
            let mut dep = lock_dep(path, Ecosystem::PyPI, name, version, at + line.len() - line.trim_start().len());
            let version_at = offset_in(text, version);
            dep.version_at = Some((version_at, version_at + version.len()));
            deps.push(dep);
        }
    }

    deps
}

#[cfg(test)]
mod tests {
    use super::*;

    const POM: &str = r#"<project>
  <properties><jackson.version>2.9.10</jackson.version></properties>
  <dependencies>
    <dependency><groupId>com.fasterxml.jackson.core</groupId><artifactId>jackson-databind</artifactId><version>${jackson.version}</version></dependency>
    <dependency><groupId>org.yaml</groupId><artifactId>snakeyaml</artifactId><version> 1.33 </version></dependency>
    <dependency><groupId>org.springframework.boot</groupId><artifactId>spring-boot-starter-web</artifactId></dependency>
    <dependency><groupId>x</groupId><artifactId>y</artifactId><version>${missing}</version></dependency>
    <dependency><groupId>x</groupId><artifactId>z</artifactId><version>[1.0,2.0)</version></dependency>
  </dependencies>
  <build><plugins><plugin><groupId>org.apache.maven.plugins</groupId><artifactId>maven-jar-plugin</artifactId><version>3.0.0</version></plugin></plugins></build>
</project>"#;

    fn found(deps: &[Dep]) -> Vec<(&str, &str)> {
        deps.iter().map(|d| (d.name.as_str(), d.version.as_str())).collect()
    }

    #[test]
    fn pom_reads_versions_and_properties() {
        let deps = parse("/p/pom.xml", POM, None);

        assert_eq!(found(&deps), [("com.fasterxml.jackson.core:jackson-databind", "2.9.10"), ("org.yaml:snakeyaml", "1.33")]);
        let canonical=POM.replacen("<project>","<project xmlns=\"http://maven.apache.org/POM/4.0.0\">",1);
        let canonical_deps=parse("/p/pom.xml",&canonical,None);
        assert_eq!(found(&canonical_deps),found(&deps));
        assert_eq!(deps[0].ecosystem, Ecosystem::Maven);
        assert_eq!(deps[0].path, "/p/pom.xml");
        let (from, to) = deps[0].version_at.unwrap();
        assert_eq!(&POM[from..to], "2.9.10");
        let (from, to) = deps[1].version_at.unwrap();
        assert_eq!(&POM[from..to], "1.33");
        assert_eq!(&POM[deps[1].at.0..deps[1].at.1], "snakeyaml");
    }

    #[test]
    fn pom_model_dependency_sites_retain_root_profile_and_plugin_source_declarations() {
        let dependency="<dependencies><dependency><groupId>x</groupId><artifactId>a</artifactId><version>1.0</version></dependency></dependencies>";
        for body in [dependency.into(),format!("<dependencyManagement>{dependency}</dependencyManagement>"),format!("<build><plugins><plugin>{dependency}</plugin></plugins></build>"),format!("<build><pluginManagement><plugins><plugin>{dependency}</plugin></plugins></pluginManagement></build>")] {
            for body in [body.clone(),format!("<profiles><profile>{body}</profile></profiles>")] {
                let source=format!("<project>{body}</project>");
                assert_eq!(found(&parse("/p/pom.xml",&source,None)),[("x:a","1.0")],"{source}");
            }
        }
    }

    #[test]
    fn kinds_and_lockfiles_by_file_name() {
        assert_eq!(kind("/p/api/build.gradle.kts"), Some(Kind::Gradle));
        assert_eq!(kind("/p/composer.lock"), Some(Kind::ComposerLock));
        assert_eq!(kind("/p/foopom.xml"), None);
        assert_eq!(lockfile("/p/web/package.json").as_deref(), Some("/p/web/package-lock.json"));
        assert_eq!(lockfile("/p/pom.xml"), None);
    }

    #[test]
    fn pom_never_panics_on_half_typed_text() {
        let text = format!("{POM}é");
        for end in (0..=text.len()).filter(|&i| text.is_char_boundary(i)) {
            parse("/p/pom.xml", &text[..end], None);
        }
    }

    const GRADLE: &str = "plugins {\n    id 'org.springframework.boot' version '3.1.0'\n}\ndependencies {\n    implementation 'org.yaml:snakeyaml:1.33'\n    implementation(\"com.google.guava:guava:31.0-jre\")\n    implementation \"org.x:y:$v\"\n    implementation 'org.springframework.boot:spring-boot-starter'\n}\n";

    #[test]
    fn gradle_reads_string_coordinates() {
        let deps = parse("/p/build.gradle.kts", GRADLE, None);

        assert_eq!(found(&deps), [("org.yaml:snakeyaml", "1.33"), ("com.google.guava:guava", "31.0-jre")]);
        let (from, to) = deps[1].version_at.unwrap();
        assert_eq!(&GRADLE[from..to], "31.0-jre");
        assert_eq!(&GRADLE[deps[0].at.0..deps[0].at.1], "org.yaml:snakeyaml:1.33");
    }

    #[test]
    fn gradle_coordinates_in_comments_examples_and_computed_literals_are_not_declarations() {
        let text = "/* implementation 'x:a:1' /* nested */ implementation 'x:b:1' */\n// implementation 'x:c:1'\nprintln 'x:d:1'\nimplementation 'x:e:1' + suffix\nimplementation 'x:actual:1'\n";
        assert_eq!(found(&parse("/p/build.gradle",text,None)),[("x:actual","1")]);
        assert!(parse("/p/build.gradle","def regex = /implementation 'x:a:1'/",None).is_empty());
        assert!(parse("/p/build.gradle","def example = \"\"\"implementation 'x:a:1'\"\"\"",None).is_empty());
    }

    #[test]
    fn gradle_never_panics_on_half_typed_text() {
        let text = format!("{GRADLE}implementation 'é:é:1' \"é");
        for end in (0..=text.len()).filter(|&i| text.is_char_boundary(i)) {
            parse("/p/build.gradle", &text[..end], None);
        }
    }

    const PACKAGE: &str = r#"{"dependencies": {"lodash": "^4.17.15", "vue": "3.2.0", "local": "file:../x"},
 "devDependencies": {"@babel/traverse": "~7.22.0"}}"#;
    const PACKAGE_LOCK: &str = r#"{"lockfileVersion": 3, "packages": {"": {"name": "app"},
 "node_modules/lodash": {"version": "4.17.15"}, "node_modules/@babel/traverse": {"version": "7.22.5"},
 "node_modules/foo/node_modules/minimist": {"version": "1.2.0"}, "node_modules/linked": {"link": true}}}"#;
    const COMPOSER: &str = r#"{"require": {"php": ">=8.1", "ext-json": "*", "symfony/http-kernel": "^5.4", "guzzlehttp/guzzle": "7.4.0"},
 "require-dev": {"phpunit/phpunit": "^9.5"}}"#;
    const COMPOSER_LOCK: &str = r#"{"packages": [{"name": "symfony/http-kernel", "version": "v5.4.20"}, {"name": "guzzlehttp/guzzle", "version": "7.4.0"}],
 "packages-dev": [{"name": "phpunit/phpunit", "version": "dev-main"}]}"#;

    fn sorted(deps: &[Dep]) -> Vec<(&str, &str)> {
        let mut all = found(deps);
        all.sort();
        all
    }

    fn named<'a>(deps: &'a [Dep], name: &str) -> &'a Dep {
        deps.iter().find(|d| d.name == name).unwrap()
    }

    #[test]
    fn package_json_takes_locked_versions() {
        let deps = parse("/p/package.json", PACKAGE, Some(PACKAGE_LOCK));

        assert_eq!(sorted(&deps), [("@babel/traverse", "7.22.5"), ("lodash", "4.17.15"), ("vue", "3.2.0")]);
        let lodash = named(&deps, "lodash");
        assert_eq!(lodash.ecosystem, Ecosystem::Npm);
        assert_eq!(&PACKAGE[lodash.at.0..lodash.at.1], "lodash");
        let (from, to) = lodash.version_at.unwrap();
        assert_eq!(&PACKAGE[from..to], "^4.17.15");
    }

    #[test]
    fn package_json_without_lockfile_keeps_exact_versions_only() {
        assert_eq!(found(&parse("/p/package.json", PACKAGE, None)), [("vue", "3.2.0")]);
    }

    #[test]
    fn package_lock_lists_every_installed_package() {
        let deps = parse("/p/package-lock.json", PACKAGE_LOCK, None);

        assert_eq!(sorted(&deps), [("@babel/traverse", "7.22.5"), ("lodash", "4.17.15"), ("minimist", "1.2.0")]);
        assert!(deps.iter().all(|d| d.version_at.is_none()));
        let lodash = named(&deps, "lodash");
        assert_eq!(&PACKAGE_LOCK[lodash.at.0..lodash.at.1], "node_modules/lodash");
    }

    #[test]
    fn npm_v1_nested_versions_have_distinct_source_locations_and_manifest_resolution() {
        let lock = r#"{"lockfileVersion":1,"dependencies":{"lodash":{"version":"4.17.21","dependencies":{"lodash":{"version":"3.10.1"},"@scope/pkg":{"version":"1.2.3"}}},"local":{"version":"file:../local"}}}"#;
        let deps = parse("/p/package-lock.json",lock,None);
        assert_eq!(sorted(&deps), [("@scope/pkg","1.2.3"),("lodash","3.10.1"),("lodash","4.17.21")]);
        let lodash:Vec<_> = deps.iter().filter(|dep| dep.name == "lodash").collect();
        assert_ne!(lodash[0].at,lodash[1].at);
        for dep in deps { assert_eq!(&lock[dep.at.0..dep.at.1],dep.name);assert!(dep.version_at.is_none()); }
        let package = r#"{"dependencies":{"lodash":"^4.17.0"}}"#;
        let direct = parse("/p/package.json",package,Some(lock));assert_eq!(found(&direct),[("lodash","4.17.21")]);
    }

    #[test]
    fn pipfile_lock_uses_resolved_pypi_versions_and_physical_section_spans() {
        let text=r#"{"_meta":{"sources":[{"name":"pypi","url":"https://pypi.org/simple","verify_ssl":true},{"name":"private","url":"https://private.example/simple","verify_ssl":true}]},"default":{"requests":{"version":"==2.31.0","index":"pypi"},"local":{"version":"==1.0","path":"."},"private":{"version":"==1.0","index":"private"}},"develop":{"requests":{"version":"==2.32.0","index":"pypi"},"unknown":{"version":"==1.0"}}}"#;
        let deps=parse("/p/Pipfile.lock",text,None);assert_eq!(sorted(&deps),[("requests","2.31.0"),("requests","2.32.0")]);assert_ne!(deps[0].at,deps[1].at);
        for dep in deps {assert_eq!(&text[dep.at.0..dep.at.1],"requests");assert_eq!(dep.ecosystem,Ecosystem::PyPI);assert!(dep.version_at.is_none());}
        assert!(GLOBS.contains(&"**/Pipfile.lock"));
    }
    #[test]
    fn uv_lock_admits_only_pypi_registry_packages_with_exact_spans() {
        let text = r#"version = 1
[[package]]
name = "Requests"
version = "2.31.0"
source = { registry = "https://pypi.org/simple" }
[package.metadata]
name = "not-a-package"
version = "99.0"
[[package]]
name = "local"
version = "1.0"
source = { editable = "." }
[[package]]
name = "private"
version = "2.0"
source = { registry = "https://private.example/simple" }
[[package]]
name = "urllib3"
version = "2.0.0"
source = { registry = "https://pypi.org/simple/" }
"#;
        let deps = parse("/p/uv.lock", text, None);
        assert_eq!(sorted(&deps), [("Requests", "2.31.0"), ("urllib3", "2.0.0")]);
        assert_eq!(deps[0].ecosystem, Ecosystem::PyPI);
        for dep in deps { assert_eq!(&text[dep.at.0..dep.at.1], dep.name); assert!(dep.version_at.is_none()); }
        assert!(GLOBS.contains(&"**/uv.lock"));
    }

    #[test]
    fn composer_json_takes_locked_versions_without_v_prefix() {
        let deps = parse("/p/composer.json", COMPOSER, Some(COMPOSER_LOCK));

        assert_eq!(sorted(&deps), [("guzzlehttp/guzzle", "7.4.0"), ("symfony/http-kernel", "5.4.20")]);
        assert_eq!(deps[0].ecosystem, Ecosystem::Packagist);
        assert_eq!(found(&parse("/p/composer.json", COMPOSER, None)), [("guzzlehttp/guzzle", "7.4.0")]);
    }

    #[test]
    fn composer_lock_lists_every_package() {
        let deps = parse("/p/composer.lock", COMPOSER_LOCK, None);

        assert_eq!(sorted(&deps), [("guzzlehttp/guzzle", "7.4.0"), ("symfony/http-kernel", "5.4.20")]);
        let kernel = named(&deps, "symfony/http-kernel");
        assert_eq!(&COMPOSER_LOCK[kernel.at.0..kernel.at.1], "symfony/http-kernel");
    }

    #[test]
    fn bumped_keeps_the_constraint_operator() {
        assert_eq!(bumped("^4.17.15", "4.17.21"), "^4.17.21");
        assert_eq!(bumped("2.12.3", "2.12.7.1"), "2.12.7.1");
    }

    #[test]
    fn offsets_point_into_the_section_they_came_from() {
        let text = r#"{"dependencies": {"lodash": "4.0.0"}, "devDependencies": {"lodash": "4.1.0"}}"#;
        let deps = parse("/p/package.json", text, None);

        assert_eq!(deps.len(), 2);
        for dep in &deps {
            let (from, to) = dep.version_at.unwrap();
            assert_eq!(&text[from..to], dep.version);
            assert_eq!(text[..from].matches("lodash").count(), if dep.version == "4.0.0" { 1 } else { 2 });
        }
    }

    #[test]
    fn nested_lock_keys_resolve_to_their_own_entry() {
        let text = r#"{"packages": {"node_modules/a/node_modules/x": {"version": "1.0.0"}, "node_modules/x": {"version": "2.0.0"}}}"#;
        let deps = parse("/p/package-lock.json", text, None);

        assert_eq!(deps.len(), 2);
        let nested = deps.iter().find(|d| d.version == "1.0.0").unwrap();
        assert_eq!(&text[nested.at.0..nested.at.1], "node_modules/a/node_modules/x");
        let top = deps.iter().find(|d| d.version == "2.0.0").unwrap();
        assert_eq!(&text[top.at.0..top.at.1], "node_modules/x");
    }

    #[test]
    fn json_never_panics_on_half_typed_text() {
        for (path, text) in [
            ("/p/package.json", PACKAGE),
            ("/p/package-lock.json", PACKAGE_LOCK),
            ("/p/composer.json", COMPOSER),
            ("/p/composer.lock", COMPOSER_LOCK),
        ] {
            let text = format!("{text}é\"é");
            for end in (0..=text.len()).filter(|&i| text.is_char_boundary(i)) {
                parse(path, &text[..end], Some(COMPOSER_LOCK));
            }
        }
    }

    #[test]
    fn reads_libs_versions_toml_aliases() {
        let text = "[versions]\njackson = \"2.12.3\"\n[libraries]\na = { module = \"com.fasterxml.jackson.core:jackson-databind\", version.ref = \"jackson\" }\nb = { group = \"org.yaml\", name = \"snakeyaml\", version = \"1.33\" }\nc = \"com.google.guava:guava:31.0-jre\"\nd = { module = \"x:managed\" }\n# old = { module = \"x:old\", version = \"1.0.0\" }\n  #g = \"x:older:2.0.0\"\ne = { module = \"x:dangling\", version = { ref = \"nope\" } }\nf = { module = \"x:ref\", version = { ref = \"jackson\" } }\n[plugins]\np = { id = \"x\", version = \"1.0\" }\n";
        let deps = parse("/p/gradle/libs.versions.toml", text, None);

        assert_eq!(
            found(&deps),
            [
                ("com.fasterxml.jackson.core:jackson-databind", "2.12.3"),
                ("org.yaml:snakeyaml", "1.33"),
                ("com.google.guava:guava", "31.0-jre"),
                ("x:ref", "2.12.3")
            ]
        );
        assert_eq!(deps[0].ecosystem, Ecosystem::Maven);
        assert_eq!(&text[deps[0].at.0..deps[0].at.1], "com.fasterxml.jackson.core:jackson-databind");
        assert_eq!(&text[deps[2].at.0..deps[2].at.1], "com.google.guava:guava");
    }

    #[test]
    fn reads_yarn_lock_v1_and_berry() {
        let v1 = "# yarn lockfile v1\n\n\"@babel/core@^7.0.0\", \"@babel/core@^7.1.0\":\n  version \"7.1.2\"\n  resolved \"x\"\n\nlodash@^4.17.15:\n  version \"4.17.15\"\n\nlocal@file:../x:\n  version \"1.0.0\"\n";
        let deps = parse("/p/yarn.lock", v1, None);
        assert_eq!(found(&deps), [("@babel/core", "7.1.2"), ("lodash", "4.17.15")]);
        assert_eq!(&v1[deps[1].at.0..deps[1].at.1], "lodash");
        assert_eq!(&v1[deps[0].at.0..deps[0].at.1], "@babel/core");

        let berry = "__metadata:\n  version: 6\n\n\"lodash@npm:^4.17.15\":\n  version: 4.17.21\n  resolution: \"lodash@npm:4.17.21\"\n\n\"app@workspace:.\":\n  version: 0.0.0-use.local\n";
        let deps = parse("/p/yarn.lock", berry, None);
        assert_eq!(found(&deps), [("lodash", "4.17.21")]);
        assert_eq!(&berry[deps[0].at.0..deps[0].at.1], "lodash");
    }

    #[test]
    fn real_world_lockfile_excerpts() {
        let berry = "__metadata:\n  version: 10\n  cacheKey: 10\n\n\"@babel/core@npm:^7.11.6, @babel/core@npm:^7.12.3\":\n  version: 7.26.10\n  resolution: \"@babel/core@npm:7.26.10\"\n\n\"react-helmet-async@npm:@slorber/react-helmet-async@*, react-helmet-async@npm:@slorber/react-helmet-async@1.3.0\":\n  version: 1.3.0\n  resolution: \"@slorber/react-helmet-async@npm:1.3.0\"\n\n\"@yarnpkg/builder@workspace:^, @yarnpkg/builder@workspace:packages/yarnpkg-builder\":\n  version: 0.0.0-use.local\n\n\"node-gyp@npm:latest\":\n  version: 11.1.0\n";
        let deps = parse("/p/yarn.lock", berry, None);
        assert_eq!(found(&deps), [("@babel/core", "7.26.10"), ("@slorber/react-helmet-async", "1.3.0"), ("node-gyp", "11.1.0")]);
        for dep in &deps {
            assert_eq!(&berry[dep.at.0..dep.at.1], dep.name);
        }

        let pnpm5 = "lockfileVersion: 5.4\n\npackages:\n\n  /@babel/core/7.0.0_react@18.0.0:\n    dev: true\n\n  /lodash/4.17.21:\n    dev: false\n";
        assert_eq!(found(&parse("/p/pnpm-lock.yaml", pnpm5, None)), [("@babel/core", "7.0.0"), ("lodash", "4.17.21")]);

        let pnpm9 = "lockfileVersion: '9.0'\n\nimporters:\n\n  .:\n    dependencies:\n      react:\n        specifier: 18.0.0\n        version: 18.0.0\n\npackages:\n\n  '@conventional-changelog/git-client@3.1.2':\n    resolution: {integrity: x}\n\nsnapshots:\n\n  '@conventional-changelog/git-client@3.1.2(conventional-commits-filter@6.0.1)':\n    dependencies:\n      a: 1.0.0\n";
        assert_eq!(found(&parse("/p/pnpm-lock.yaml", pnpm9, None)), [("@conventional-changelog/git-client", "3.1.2")]);

        let npm = r#"{"name":"npm","lockfileVersion":3,"packages":{"":{"name":"npm","workspaces":["workspaces/*"]},"node_modules/@npmcli/arborist":{"resolved":"workspaces/arborist","link":true},"node_modules/str":{"name":"string-width","version":"4.2.3"},"workspaces/arborist":{"name":"@npmcli/arborist","version":"9.0.0"},"workspaces/arborist/node_modules/x":{"version":"1.0.0","peer":true}}}"#;
        assert_eq!(found(&parse("/p/package-lock.json", npm, None)), [("string-width", "4.2.3"), ("x", "1.0.0")]);
    }

    #[test]
    fn reads_pnpm_lock() {
        let text = "lockfileVersion: '6.0'\n\nimporters:\n  .:\n    dependencies:\n      lodash:\n        version: 4.17.15\n\npackages:\n\n  /lodash@4.17.15:\n    resolution: {integrity: x}\n\n  /@babel/traverse@7.22.5(supports-color@5.5.0):\n    dev: true\n\n  '@scope/pkg@1.0.0':\n    dev: false\n\n  vue@3.2.0:\n    dev: false\n\nsnapshots:\n  other@9.9.9:\n    dev: false\n";
        let deps = parse("/p/pnpm-lock.yaml", text, None);

        assert_eq!(found(&deps), [("lodash", "4.17.15"), ("@babel/traverse", "7.22.5"), ("@scope/pkg", "1.0.0"), ("vue", "3.2.0")]);
        for dep in &deps {
            assert_eq!(&text[dep.at.0..dep.at.1], dep.name);
        }
    }

    #[test]
    fn pnpm_lock_skips_packages_resolved_from_a_private_tarball_host() {
        let text = "lockfileVersion: '9.0'\n\npackages:\n\n  internal@1.0.0:\n    resolution: {integrity: x, tarball: https://npm.acme.corp/internal/-/internal-1.0.0.tgz}\n\n  other@2.0.0:\n    resolution:\n      integrity: y\n      tarball: 'https://registry.npmjs.org/other/-/other-2.0.0.tgz'\n\n  secret@3.0.0:\n    resolution:\n      tarball: https://user:pw@nexus.local/secret.tgz\n";

        assert_eq!(found(&parse("/p/pnpm-lock.yaml", text, None)), [("other", "2.0.0")]);
        let notes = notes("/p/pnpm-lock.yaml", text, &HashSet::new());
        let hidden: Vec<_> = notes.iter().map(|n| (n.name.as_str(), n.label)).collect();
        assert_eq!(hidden, [("internal", PRIVATE), ("secret", PRIVATE)]);
    }

    #[test]
    fn reads_cargo_lock() {
        let text = "version = 3\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\n\n[[package]]\nname = \"serde\"\nversion = \"1.0.1\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n\n[[package]]\nname = \"local\"\nversion = \"0.2.0\"\nsource = \"path+file:///x\"\n";
        let deps = parse("/p/Cargo.lock", text, None);

        assert_eq!(found(&deps), [("serde", "1.0.1")]);
        assert_eq!(deps[0].ecosystem, Ecosystem::CratesIo);
        assert_eq!(&text[deps[0].at.0..deps[0].at.1], "serde");
    }

    fn private_names(path: &str, text: &str) -> Vec<(String, String)> {
        let mut names: Vec<_> = notes(path, text, &HashSet::new()).into_iter().filter(|n| n.label == PRIVATE).map(|n| (n.name, n.spec)).collect();
        names.sort();
        names
    }

    #[test]
    fn uv_lock_dependency_rows_do_not_rename_package() {
        let deps = parse("/p/uv.lock", include_str!("../tests/fixtures/uv.lock"), None);

        assert_eq!(found(&deps), [("certifi", "2024.8.30"), ("requests", "2.32.3"), ("urllib3", "2.2.3")]);
    }

    #[test]
    fn poetry_inline_dependency_keeps_version() {
        let deps = parse("/p/poetry.lock", include_str!("../tests/fixtures/poetry.lock"), None);

        assert!(found(&deps).contains(&("click", "8.1.7")));
        assert!(found(&deps).contains(&("colorama", "0.4.6")));
    }

    #[test]
    fn poetry_package_with_source_table_skipped() {
        let deps = parse("/p/poetry.lock", include_str!("../tests/fixtures/poetry.lock"), None);

        assert_eq!(found(&deps), [("click", "8.1.7"), ("colorama", "0.4.6")]);
        assert_eq!(private_names("/p/poetry.lock", include_str!("../tests/fixtures/poetry.lock")), [("internal-lib".to_owned(), "1.2.0".to_owned())]);
    }

    #[test]
    fn cargo_alt_registry_not_queried() {
        let text = include_str!("../tests/fixtures/Cargo.lock");

        assert_eq!(found(&parse("/p/Cargo.lock", text, None)), [("serde", "1.0.210"), ("tokio", "1.40.0")]);
        assert_eq!(private_names("/p/Cargo.lock", text), [("internal-crate".to_owned(), "0.3.1".to_owned())]);
    }

    #[test]
    fn npm_private_resolved_not_queried() {
        let lock = r#"{"lockfileVersion":3,"packages":{"":{},"node_modules/lodash":{"version":"4.17.21","resolved":"https://registry.npmjs.org/lodash/-/lodash-4.17.21.tgz"},"node_modules/secret":{"version":"1.0.0","resolved":"https://npm.corp.example/secret/-/secret-1.0.0.tgz"}}}"#;
        assert_eq!(found(&parse("/p/package-lock.json", lock, None)), [("lodash", "4.17.21")]);
        assert_eq!(private_names("/p/package-lock.json", lock), [("secret".to_owned(), "1.0.0".to_owned())]);

        let manifest = r#"{"dependencies":{"secret":"^1.0.0","lodash":"^4.0.0"}}"#;
        assert_eq!(found(&parse("/p/package.json", manifest, Some(lock))), [("lodash", "4.17.21")]);

        let yarn = "lodash@^4.17.15:\n  version \"4.17.15\"\n  resolved \"https://registry.yarnpkg.com/lodash/-/lodash-4.17.15.tgz#abc\"\n\nsecret@^1.0.0:\n  version \"1.0.0\"\n  resolved \"https://npm.corp.example/secret/-/secret-1.0.0.tgz#abc\"\n";
        assert_eq!(found(&parse("/p/yarn.lock", yarn, None)), [("lodash", "4.17.15")]);
        assert_eq!(private_names("/p/yarn.lock", yarn), [("secret".to_owned(), "1.0.0".to_owned())]);
    }

    #[test]
    fn npm_v1_private_resolved_not_queried() {
        let lock = r#"{"lockfileVersion":1,"dependencies":{"lodash":{"version":"4.17.21","resolved":"https://registry.npmjs.org/lodash/-/lodash-4.17.21.tgz"},"secret":{"version":"1.0.0","resolved":"https://npm.corp.example/secret/-/secret-1.0.0.tgz","dependencies":{"inner":{"version":"2.0.0","resolved":"https://registry.npmjs.org/inner/-/inner-2.0.0.tgz"}}}}}"#;
        assert_eq!(found(&parse("/p/package-lock.json", lock, None)), [("lodash", "4.17.21"), ("inner", "2.0.0")]);
        assert_eq!(private_names("/p/package-lock.json", lock), [("secret".to_owned(), "1.0.0".to_owned())]);

        let manifest = r#"{"dependencies":{"secret":"1.0.0","lodash":"^4.0.0"}}"#;
        assert_eq!(found(&parse("/p/package.json", manifest, Some(lock))), [("lodash", "4.17.21")]);
    }

    #[test]
    fn composer_private_dist_listed_not_queried() {
        let lock = r#"{"packages":[{"name":"guzzlehttp/guzzle","version":"7.4.0","dist":{"type":"zip","url":"https://api.github.com/repos/guzzle/guzzle/zipball/abc"}},{"name":"acme/internal","version":"1.0.0","dist":{"type":"zip","url":"https://packages.acme.example/dist/internal.zip"}}],"packages-dev":[]}"#;
        assert_eq!(found(&parse("/p/composer.lock", lock, None)), [("guzzlehttp/guzzle", "7.4.0")]);
        assert_eq!(private_names("/p/composer.lock", lock), [("acme/internal".to_owned(), "1.0.0".to_owned())]);

        let manifest = r#"{"require":{"acme/internal":"^1.0","guzzlehttp/guzzle":"^7.0"}}"#;
        assert_eq!(found(&parse("/p/composer.json", manifest, Some(lock))), [("guzzlehttp/guzzle", "7.4.0")]);
    }

    #[test]
    fn yarn_alias_range_points_at_real_name() {
        let text = "\"string-width-cjs@npm:string-width@^4.2.0\":\n  version: 4.2.3\n";
        let deps = parse("/p/yarn.lock", text, None);

        assert_eq!(found(&deps), [("string-width", "4.2.3")]);
        assert_eq!(&text[deps[0].at.0..deps[0].at.1], "string-width");
        assert!(deps[0].at.0 > text.find("@npm:").unwrap());
    }

    #[test]
    fn reads_pypi_pins_and_poetry_lock() {
        let reqs = "# deps\nDjango==3.2.0  # pinned\nrequests[security]==2.0.0 ; python_version > '3'\nflask>=1.0\n-r other.txt\n";
        let deps = parse("/p/requirements.txt", reqs, None);
        assert_eq!(found(&deps), [("Django", "3.2.0"), ("requests", "2.0.0")]);
        assert_eq!(&reqs[deps[0].at.0..deps[0].at.1], "Django");
        assert_eq!(deps[0].ecosystem, Ecosystem::PyPI);

        let poetry = "[[package]]\nname = \"django\"\nversion = \"3.2.0\"\n\n[metadata]\nlock-version = \"2.0\"\n";
        assert_eq!(found(&parse("/p/poetry.lock", poetry, None)), [("django", "3.2.0")]);
    }

    #[test]
    fn pylock_admits_only_pypi_index_packages_with_versions() {
        let text = r#"lock-version = "1.0"
[[packages]]
name = "Requests"
version = "2.31.0"
index = "https://pypi.org/simple"
marker = "python_version > '3'"
[[packages.wheels]]
name = "requests-2.31.0-py3-none-any.whl"
url = "https://files.pythonhosted.org/x.whl"
[[packages]]
name = "gitpkg"
version = "1.0"
index = "https://pypi.org/simple"
[packages.vcs]
type = "git"
url = "https://example.com/x.git"
[[packages]]
name = "private"
version = "1.0"
index = "https://private.example/simple"
[[packages]]
name = "noindex"
version = "1.0"
[[packages]]
name = "noversion"
index = "https://pypi.org/simple"
[[packages]]
name = "urllib3"
version = "2.0.0"
index = "https://pypi.org/simple/"
[tool.x]
name = "tool"
"#;
        for path in ["/p/pylock.toml", "/p/pylock.dev.toml"] {
            let deps = parse(path, text, None);
            assert_eq!(sorted(&deps), [("Requests", "2.31.0"), ("urllib3", "2.0.0")]);
            for dep in deps { assert_eq!(&text[dep.at.0..dep.at.1], dep.name); assert_eq!(dep.ecosystem, Ecosystem::PyPI); }
        }
        assert!(GLOBS.contains(&"**/pylock.toml") && GLOBS.contains(&"**/pylock.*.toml"));
        assert!(parse("/p/pylock.toml.bak", text, None).is_empty());
    }

    #[test]
    fn workspace_and_link_specs_are_local_notes_never_deps() {
        let text = r#"{"dependencies":{"a":"workspace:^","b":"link:../b","c":"1.0.0","sib":"2.0.0"}}"#;
        let deps = parse("/p/package.json", text, None);
        assert_eq!(found(&deps), [("c", "1.0.0"), ("sib", "2.0.0")]);
        let members = HashSet::from(["sib".to_owned()]);
        let mut names: Vec<_> = notes("/p/package.json", text, &members).into_iter().map(|n| (n.name, n.label)).collect();
        names.sort();
        assert_eq!(names, [("a".into(), LOCAL), ("b".into(), LOCAL), ("sib".into(), LOCAL)]);
    }

    #[test]
    fn lock_link_entries_are_local_and_workspace_nested_installs_are_audited() {
        let manifest = r#"{"dependencies":{"b":"1.0.0"}}"#;
        let lock = r#"{"lockfileVersion":3,"packages":{"":{},"node_modules/b":{"resolved":"packages/b","link":true},"packages/b":{"version":"1.0.0"},"packages/b/node_modules/x":{"version":"3.0.0"}}}"#;
        assert!(parse("/p/package.json", manifest, Some(lock)).is_empty());
        assert_eq!(found(&parse("/p/package-lock.json", lock, None)), [("x", "3.0.0")]);
    }

    #[test]
    fn pyproject_is_a_parsed_kind() {
        assert_eq!(kind("/p/pyproject.toml"), Some(Kind::Pyproject));
        assert!(GLOBS.contains(&"**/pyproject.toml"));
        let deps = parse("/p/pyproject.toml", "[project]\ndependencies = [\"a==1.0\"]\n", None);
        assert_eq!(found(&deps), [("a", "1.0")]);
    }

    #[test]
    fn package_json_npm_alias_audits_the_real_package_only_when_exact() {
        let text = r#"{"dependencies":{"old":"npm:lodash@4.17.15","scoped":"npm:@babel/core@7.0.0","range":"npm:lodash@^4.0.0","bare":"npm:lodash","w":"workspace:*","l":"link:../x","bad":"npm:a/b/c@1.0.0"}}"#;
        let deps = parse("/p/package.json", text, None);
        assert_eq!(sorted(&deps), [("@babel/core", "7.0.0"), ("lodash", "4.17.15")]);
        for dep in &deps { assert!(dep.version_at.is_none()); assert_eq!(dep.ecosystem, Ecosystem::Npm); }
        assert_eq!(&text[deps[0].at.0..deps[0].at.1], "old");
    }

    #[test]
    fn new_formats_never_panic_on_half_typed_text() {
        for (path, text) in [
            ("/p/libs.versions.toml", "[versions]\nj = \"é\"\n[libraries]\na = { module = \"é:é\", version.ref = \"j\" }\nb = \"é:é:1\"\n"),
            ("/p/yarn.lock", "\"é@npm:^1\", \"é@^1\":\n  version: 1.0.0\n"),
            ("/p/pnpm-lock.yaml", "packages:\n  '/é@1.0.0(é@1)':\n    dev: true\n"),
            ("/p/Cargo.lock", "[[package]]\nname = \"é\"\nversion = \"1.0.0\"\nsource = \"registry+é\"\n"),
            ("/p/requirements.txt", "é[é]==1.0 ; é # é\n"),
            ("/p/pylock.toml", "[[packages]]\nname = \"é\"\nversion = \"1\"\nindex = \"é\"\n[packages.vcs]\n"),
            ("/p/package.json", "{\"dependencies\":{\"é\":\"npm:é@1.0.0\"}}"),
        ] {
            let text = format!("{text}é\"é'");
            for end in (0..=text.len()).filter(|&i| text.is_char_boundary(i)) {
                parse(path, &text[..end], None);
            }
        }
    }
}
