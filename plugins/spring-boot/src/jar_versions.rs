//! Bounded, local source-declared Maven version catalog. No Maven execution.
use super::{Dependency, Error, invalid, literal};
use openidea_plugin::{FileKind, host};
use sha2::{Digest, Sha256};
use spring_text::xml::{self, Element};
use std::collections::{BTreeMap, BTreeSet};
fn value<'a>(text: &'a str, elements: &[Element], parent: usize, name: &str) -> Option<&'a str> {
    let found = elements
        .iter()
        .filter(|e| e.parent == Some(parent) && e.local() == name)
        .collect::<Vec<_>>();
    let [e] = found.as_slice() else {
        return None;
    };
    let close = text.get(e.tag_end..e.end)?.rfind("</")? + e.tag_end;
    let value = text.get(e.tag_end..close)?.trim();
    (!value.contains(['<', '&'])).then_some(value)
}
pub(super) fn properties(text: &str) -> Result<BTreeMap<String, String>, Error> {
    let elements = xml::elements(text);
    let mut props = BTreeMap::new();
    for (i, e) in elements
        .iter()
        .enumerate()
        .filter(|(_, e)| e.local() == "properties" && e.parent == Some(0))
    {
        for child in elements.iter().filter(|e| e.parent == Some(i)) {
            let name = child.local();
            let Some(raw) = value(text, &elements, i, name) else {
                return Err(invalid(
                    "Duplicate or complex Maven version properties are unsupported.",
                ));
            };
            if props.insert(name.into(), raw.into()).is_some() {
                return Err(invalid("Duplicate Maven version properties are ambiguous."));
            }
        }
        let _ = e;
    }
    let parent = elements
        .iter()
        .position(|e| e.local() == "parent" && e.parent == Some(0));
    if let Some(version) = value(text, &elements, 0, "version")
        .or_else(|| parent.and_then(|i| value(text, &elements, i, "version")))
        .and_then(|v| resolve(v, &props))
    {
        props.insert("project.version".into(), version.clone());
        props.insert("pom.version".into(), version);
    }
    if props.len() > 256 {
        return Err(invalid("Maven property limit exceeded."));
    }
    Ok(props)
}
pub(super) fn resolve(raw: &str, props: &BTreeMap<String, String>) -> Option<String> {
    let mut result = raw;
    let mut seen = BTreeSet::new();
    for _ in 0..8 {
        if literal(result) {
            return Some(result.into());
        }
        let name = result.strip_prefix("${")?.strip_suffix('}')?;
        if !seen.insert(name) {
            return None;
        }
        result = props.get(name)?;
    }
    None
}
fn coordinate(
    text: &str,
    elements: &[Element],
    i: usize,
    props: &BTreeMap<String, String>,
    scope: &str,
) -> Option<Dependency> {
    let group = value(text, elements, i, "groupId")?;
    let artifact = value(text, elements, i, "artifactId")?;
    if !literal(group) || !literal(artifact) || group.split('.').any(str::is_empty) {
        return None;
    }
    let version = resolve(value(text, elements, i, "version")?, props)?;
    Some(Dependency {
        group: group.into(),
        artifact: artifact.into(),
        version,
        scope: scope.into(),
    })
}
fn official(d: &Dependency) -> bool {
    matches!(
        d.group.as_str(),
        "org.springframework.boot" | "org.springframework.cloud"
    )
}
pub(super) fn managed(text: &str) -> Result<Vec<Dependency>, Error> {
    let elements = xml::elements(text);
    let props = properties(text)?;
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for (i, e) in elements
        .iter()
        .enumerate()
        .filter(|(_, e)| e.local() == "dependency")
    {
        let Some(parent) = e.parent else {
            continue;
        };
        let Some(management) = elements[parent].parent else {
            continue;
        };
        if elements[parent].local() != "dependencies"
            || elements[management].local() != "dependencyManagement"
            || elements[management].parent != Some(0)
        {
            continue;
        }
        if value(text, &elements, i, "type").is_some_and(|v| v != "jar")
            || value(text, &elements, i, "classifier").is_some()
            || value(text, &elements, i, "scope") == Some("import")
        {
            continue;
        }
        if let Some(d) = coordinate(
            text,
            &elements,
            i,
            &props,
            "managed catalog; dependency presence unverified",
        ) {
            if !seen.insert((d.group.clone(), d.artifact.clone())) {
                return Err(invalid(
                    "Duplicate managed dependency coordinates are ambiguous.",
                ));
            }
            out.push(d);
        }
    }
    Ok(out)
}
fn references(text: &str) -> Result<Vec<(Dependency, bool)>, Error> {
    let elements = xml::elements(text);
    let props = properties(text)?;
    let mut out = Vec::new();
    for (i, e) in elements.iter().enumerate() {
        let parent = e.local() == "parent" && e.parent == Some(0);
        let imported = e.local() == "dependency"
            && value(text, &elements, i, "type") == Some("pom")
            && value(text, &elements, i, "scope") == Some("import")
            && e.parent.and_then(|i| elements[i].parent).is_some_and(|i| {
                elements[i].local() == "dependencyManagement" && elements[i].parent == Some(0)
            });
        if parent || imported {
            if let Some(d) =
                coordinate(text, &elements, i, &props, "declared parent/BOM").filter(official)
            {
                out.push((d, parent));
            }
        }
    }
    Ok(out)
}
/// `None` when the host says the path is not there (`NotFound`, or `Invalid("…: no such file")` outside the project).
pub(super) fn present<T>(found: Result<T, Error>) -> Result<Option<T>, Error> {
    match found {
        Ok(value) => Ok(Some(value)),
        Err(error) if spring_text::fs_missing(&error) => Ok(None),
        Err(error) => Err(error),
    }
}
const MAX_PARENT_DEPTH: usize = 4;
const MAX_IMPORTS: usize = 32;
pub(super) fn cache_catalog(
    source: &str,
    cache: &str,
) -> Result<(Vec<Dependency>, Vec<(String, String)>), Error> {
    let mut queue: Vec<(Dependency, bool, usize)> = references(source)?
        .into_iter()
        .map(|(d, parent)| (d, parent, 1))
        .collect();
    let mut imports = 0;
    let mut visited = BTreeSet::new();
    let mut catalogs = Vec::new();
    let mut snapshots = Vec::new();
    let mut bytes = 0;
    while let Some((dep, parent, depth)) = queue.pop() {
        if depth > MAX_PARENT_DEPTH {
            return Err(invalid("Parent/BOM chain deeper than 4; no snapshot loaded."));
        }
        if !visited.insert(dep.gav()) {
            continue;
        }
        if visited.len() > 64 {
            return Err(invalid(
                "Official parent/BOM catalog exceeds 64 files; no snapshot loaded.",
            ));
        }
        let path = dep.path(cache).trim_end_matches(".jar").to_owned() + ".pom";
        let Some(stat) = present(host::fs::stat(&path))? else {
            continue;
        };
        if !parent {
            imports += 1;
            if imports > MAX_IMPORTS {
                return Err(invalid("More than 32 imported BOMs; no snapshot loaded."));
            }
        }
        if stat.kind != FileKind::File || stat.size > 256 * 1024 {
            return Err(invalid(
                "Cached parent/BOM POM is not a bounded regular file.",
            ));
        }
        let raw = host::fs::read_bytes(&path)?;
        bytes += raw.len();
        if bytes > 2 * 1024 * 1024 {
            return Err(invalid("Official parent/BOM catalog exceeds 2MiB."));
        }
        if host::fs::stat(&path)? != stat || raw.len() as u64 != stat.size {
            return Err(invalid("Cached parent/BOM changed during reading."));
        }
        let text = std::str::from_utf8(&raw).map_err(|_| invalid("Cached POM is not UTF8."))?;
        super::dependencies(text)?; // Strict namespace/well-formedness validation.
        let elements = xml::elements(text);
        let props = properties(text)?;
        let artifact = value(text, &elements, 0, "artifactId");
        let version = value(text, &elements, 0, "version")
            .or_else(|| {
                elements
                    .iter()
                    .position(|e| e.local() == "parent" && e.parent == Some(0))
                    .and_then(|i| value(text, &elements, i, "version"))
            })
            .and_then(|v| resolve(v, &props));
        let group = value(text, &elements, 0, "groupId").or_else(|| {
            elements
                .iter()
                .position(|e| e.local() == "parent" && e.parent == Some(0))
                .and_then(|i| value(text, &elements, i, "groupId"))
        });
        if artifact != Some(dep.artifact.as_str())
            || group != Some(dep.group.as_str())
            || version.as_deref() != Some(dep.version.as_str())
        {
            return Err(invalid(&format!(
                "Cached POM coordinates do not match declared {} at {path}: group {group:?}, artifact {artifact:?}, version {version:?}.",
                dep.gav()
            )));
        }
        catalogs.extend(managed(text)?.into_iter().filter(official));
        for (d, parent) in references(text)? {
            queue.push((d, parent, depth + 1));
        }
        snapshots.push((path, format!("{:x}", Sha256::digest(raw))));
    }
    let mut entries = BTreeMap::<(String, String), Dependency>::new();
    for d in catalogs {
        let key = (d.group.clone(), d.artifact.clone());
        if let Some(previous) = entries.get(&key) {
            if previous.version != d.version {
                return Err(invalid(
                    "Official source catalogs disagree on a managed artifact version; choose an explicit dependency version.",
                ));
            }
        }
        entries.insert(key, d);
    }
    Ok((entries.into_values().collect(), snapshots))
}
