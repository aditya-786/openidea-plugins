//! Project-owned directive declarations, cached per project root and keyed on the
//! stats of the files they come from. No virtual PHP text is attached to physical ranges.
use openidea_plugin::{FileStat, Project, host};
use serde_json::Value;
use std::{
    cell::RefCell,
    collections::{BTreeSet, HashMap},
    time::{Duration, Instant},
};
fn clean(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains(['\\', '\0', '\n', '\r'])
        && path
            .split('/')
            .all(|p| !p.is_empty() && !matches!(p, "." | ".."))
}
fn normalize(base: &str, path: &str) -> Option<String> {
    if path.starts_with('/') || path.contains('\\') {
        return None;
    }
    let mut p: Vec<_> = base.split('/').filter(|p| !p.is_empty()).collect();
    for s in path.split('/') {
        match s {
            "" | "." => {}
            ".." => {
                p.pop()?;
            }
            s => p.push(s),
        }
    }
    let path = p.join("/");
    clean(&path).then_some(path)
}
struct Reader {
    project: Project,
    root: String,
    files: usize,
    bytes: usize,
    started: Instant,
}
impl Reader {
    fn read(&mut self, path: &str, limit: usize) -> Option<String> {
        if !clean(path)
            || self.files >= 64
            || self.bytes > 2 * 1024 * 1024
            || self.started.elapsed() > Duration::from_millis(250)
        {
            return None;
        }
        self.files += 1;
        let full = format!("{}/{path}", self.root);
        if host::project_of(&full)
            .and_then(|p| p.root().ok())
            .as_deref()
            != Some(self.root.as_str())
            || self.project.stat(&full).ok()?.size > limit as u64
        {
            return None;
        }
        let text = self.project.read_file(&full).ok()?;
        self.bytes += text.len();
        (text.len() <= limit && self.bytes <= 2 * 1024 * 1024).then_some(text)
    }
}
fn names(value: &Value, out: &mut BTreeSet<String>) {
    for key in ["directives", "ifDirectives", "scriptDirectives"] {
        for entry in value
            .pointer(&format!("/blade/{key}"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if entry.get("prefix").is_some()
                || entry.get("suffix").is_some()
                || entry.get("transformers").is_some()
            {
                continue;
            }
            if let Some(name) = entry
                .get("name")
                .and_then(Value::as_str)
                .filter(|s| valid(s))
            {
                out.insert(name.into());
            }
        }
    }
}
fn valid(name: &str) -> bool {
    !name.is_empty()
        && name.len() < 128
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}
const WATCHED: [&str; 4] = [
    "ide.json",
    ".ide.json",
    ".idea/ide.json",
    "vendor/composer/installed.json",
];

type Stamp = [Option<FileStat>; 4];

thread_local! {
    static CACHE: RefCell<HashMap<String, (Stamp, BTreeSet<String>)>> = RefCell::new(HashMap::new());
}

pub(super) fn for_path(path: &str) -> BTreeSet<String> {
    let Some(project) = host::project_of(path) else {
        return BTreeSet::new();
    };
    let Ok(root) = project.root() else {
        return BTreeSet::new();
    };
    let stamp: Stamp = WATCHED.map(|file| project.stat(&format!("{root}/{file}")).ok());
    let cached = CACHE.with(|cache| {
        cache
            .borrow()
            .get(&root)
            .filter(|(known, _)| *known == stamp)
            .map(|(_, names)| names.clone())
    });
    if let Some(names) = cached {
        return names;
    }
    let names = scan(project, root.clone());
    CACHE.with(|cache| cache.borrow_mut().insert(root, (stamp, names.clone())));
    names
}

fn scan(project: Project, root: String) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut read = Reader {
        project,
        root,
        files: 0,
        bytes: 0,
        started: Instant::now(),
    };
    let mut disabled = BTreeSet::new();
    for file in ["ide.json", ".ide.json", ".idea/ide.json"] {
        if let Some(source) = read.read(file, 256 * 1024) {
            if let Ok(value) = serde_json::from_str::<Value>(&source) {
                names(&value, &mut out);
                disabled.extend(
                    value
                        .pointer("/packages/disabledPackages")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .map(str::to_owned),
                );
            }
        }
    }
    let Some(source) = read.read("vendor/composer/installed.json", 2 * 1024 * 1024) else {
        return out;
    };
    let Ok(value) = serde_json::from_str::<Value>(&source) else {
        return out;
    };
    let Some(packages) = value
        .get("packages")
        .and_then(Value::as_array)
        .or_else(|| value.as_array())
    else {
        return out;
    };
    if packages.len() > 512 {
        return out;
    }
    for package in packages {
        if package
            .get("name")
            .and_then(Value::as_str)
            .is_some_and(|n| disabled.contains(n))
        {
            continue;
        }
        let Some(base) = package
            .get("install-path")
            .and_then(Value::as_str)
            .and_then(|p| normalize("vendor/composer", p))
        else {
            continue;
        };
        let Some(roots) = package
            .pointer("/autoload/psr-4")
            .and_then(Value::as_object)
        else {
            continue;
        };
        for provider in package
            .pointer("/extra/laravel/providers")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            let mut sources = Vec::new();
            for (prefix, dirs) in roots {
                let Some(local) = provider.strip_prefix(prefix) else {
                    continue;
                };
                for dir in dirs.as_str().into_iter().chain(
                    dirs.as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str),
                ) {
                    let Some(root) = normalize(&base, dir) else {
                        continue;
                    };
                    let Some(path) = normalize(&root, &format!("{}.php", local.replace('\\', "/")))
                    else {
                        continue;
                    };
                    if let Some(source) = read.read(&path, 128 * 1024) {
                        if declared_class(&source, provider) {
                            sources.push(source);
                        }
                    }
                }
            }
            if sources.len() == 1 {
                out.extend(provider_names(&sources[0]));
            }
        }
    }
    out
}
fn mask(source: &str) -> Option<String> {
    let bytes = source.as_bytes();
    let mut out = bytes.to_vec();
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        if matches!(bytes[i], b'\'' | b'"') {
            let q = bytes[i];
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\\' {
                    i += 2;
                    continue;
                }
                if bytes[i] == q {
                    break;
                }
                i += 1;
            }
            if i >= bytes.len() {
                return None;
            }
            i += 1;
        } else if bytes[i..].starts_with(b"//") || bytes[i] == b'#' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else if bytes[i..].starts_with(b"/*") {
            i += 2;
            while i + 1 < bytes.len() && !bytes[i..].starts_with(b"*/") {
                i += 1;
            }
            if i + 1 >= bytes.len() {
                return None;
            }
            i += 2;
        } else if bytes[i..].starts_with(b"<<<") {
            return None;
        } else {
            i += 1;
            continue;
        }
        for b in &mut out[start..i] {
            if !matches!(*b, b'\n' | b'\r') {
                *b = b' ';
            }
        }
    }
    String::from_utf8(out).ok()
}
fn declared_class(source: &str, expected: &str) -> bool {
    let Some(code) = mask(source) else {
        return false;
    };
    let declarations = |keyword: &str| {
        code.match_indices(keyword)
            .filter_map(|(at, _)| {
                if at > 0
                    && (crate::is_identifier(code.as_bytes()[at - 1])
                        || code.as_bytes()[at - 1] == b':')
                {
                    return None;
                }
                let rest = &code[at + keyword.len()..];
                if !rest.starts_with(char::is_whitespace) {
                    return None;
                }
                let name = rest
                    .trim_start()
                    .split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '\\')))
                    .next()?;
                (!name.is_empty()).then_some(name)
            })
            .collect::<Vec<_>>()
    };
    let namespaces = declarations("namespace");
    let classes = declarations("class");
    namespaces.len() == 1
        && classes.len() == 1
        && format!("{}\\{}", namespaces[0], classes[0]) == expected
}

fn provider_names(source: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let Some(code) = mask(source) else {
        return out;
    }; // Only an exact ordinary facade import or explicit FQ spelling is accepted.
    let imported = code
        .split(';')
        .filter(|s| s.trim().starts_with("use "))
        .filter(|s| s.contains("Blade"))
        .collect::<Vec<_>>();
    let alias = if imported.len() == 1 {
        let raw = imported[0].trim().strip_prefix("use ").unwrap().trim();
        if raw == "Illuminate\\Support\\Facades\\Blade" {
            Some("Blade")
        } else {
            raw.strip_prefix("Illuminate\\Support\\Facades\\Blade as ")
                .filter(|s| valid(s))
        }
    } else {
        None
    };
    for method in ["directive", "if"] {
        let mut spellings = vec![format!("\\Illuminate\\Support\\Facades\\Blade::{method}")];
        if let Some(alias) = alias {
            spellings.push(format!("{alias}::{method}"));
        }
        for needle in spellings {
            for (at, _) in code.match_indices(&needle) {
                if at > 0
                    && (crate::is_identifier(code.as_bytes()[at - 1])
                        || code.as_bytes()[at - 1] == b'\\')
                {
                    continue;
                }
                let tail = source[at + needle.len()..].trim_start();
                let Some(tail) = tail.strip_prefix('(').map(str::trim_start) else {
                    continue;
                };
                let Some(q @ ('\'' | '"')) = tail.chars().next() else {
                    continue;
                };
                let Some(end) = tail[1..].find(q).map(|n| n + 1) else {
                    continue;
                };
                let name = &tail[1..end];
                if valid(name) && tail[end + 1..].trim_start().starts_with(',') {
                    out.insert(name.into());
                }
            }
        }
    }
    out
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn provider_identity_and_literals_are_proven() {
        let source = "<?php namespace Acme\\UI; use Illuminate\\Support\\Facades\\Blade; class Provider {function boot(){Blade::directive('markdown_2',fn()=>1);}}";
        assert!(declared_class(source, "Acme\\UI\\Provider"));
        assert!(provider_names(source).contains("markdown_2"));
        assert!(
            provider_names(&source.replace("Illuminate\\Support\\Facades\\Blade", "App\\Blade"))
                .is_empty()
        );
        assert!(
            provider_names(
                "<?php // \\Illuminate\\Support\\Facades\\Blade::directive('fake',fn()=>1);"
            )
            .is_empty()
        );
    }
}
#[cfg(test)]
mod installed_tests {
    use super::*;
    use openidea_plugin::{
        injections::{Document, Injections},
        mock::MockHost,
    };
    #[test]
    #[ignore = "requires disposable installed Composer package fixture"]
    fn installed_vendor_registration_drives_current_physical_php_injection() {
        let folder = std::env::var("OPENIDEA_LARAVEL_PACKAGE_FIXTURE").expect("fixture path");
        let base = std::path::Path::new(&folder);
        let host = MockHost::install().with_capabilities(&["project.read"]);
        let installed =
            std::fs::read_to_string(base.join("vendor/composer/installed.json")).unwrap();
        host.add_file("vendor/composer/installed.json", &installed);
        let value: Value = serde_json::from_str(&installed).unwrap();
        for p in value["packages"].as_array().unwrap() {
            let Some(root) = p["install-path"]
                .as_str()
                .and_then(|p| normalize("vendor/composer", p))
            else {
                continue;
            };
            let Some(roots) = p.pointer("/autoload/psr-4").and_then(Value::as_object) else {
                continue;
            };
            for provider in p
                .pointer("/extra/laravel/providers")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                for (prefix, dir) in roots {
                    let (Some(local), Some(dir)) = (provider.strip_prefix(prefix), dir.as_str())
                    else {
                        continue;
                    };
                    let path = normalize(&root, &format!("{dir}/{}.php", local.replace('\\', "/")))
                        .unwrap();
                    if let Ok(source) = std::fs::read_to_string(base.join(&path)) {
                        host.add_file(&path, &source);
                    }
                }
            }
        }
        let mut plugin = crate::Blade;
        let source = "@nativeError('email', $color)";
        let doc = Document {
            path: "/project/a.blade.php".into(),
            language: "blade".into(),
            version: 1,
            text: source.into(),
        };
        let found = plugin.injections(&doc);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].language, "php");
        host.add_project("/project/vendor/nativephp");
        assert!(plugin.injections(&doc).is_empty());
    }
}
