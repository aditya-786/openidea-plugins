//! Literal Gradle build-file coordinates resolved against the Gradle module cache
//! (`modules-2/files-2.1/<group>/<artifact>/<version>/<sha1>/<file>`). No Gradle execution.
use super::{Dependency, Error, invalid, literal, versions};
use openidea_plugin::{FileKind, host};
use std::collections::BTreeSet;

pub(super) const CACHE: &str = "~/.gradle/caches/modules-2/files-2.1";
const MAX_HASH_DIRS: usize = 16;
const MAX_BOM: u64 = 1024 * 1024;

fn official(group: &str) -> bool {
    matches!(group, "org.springframework.boot" | "org.springframework.cloud")
}

fn quoted(s: &str) -> Option<(&str, &str)> {
    let quote = s.chars().next().filter(|c| matches!(c, '"' | '\''))?;
    let rest = &s[1..];
    let end = rest.find(quote)?;
    Some((&rest[..end], &rest[end + 1..]))
}

fn scope(configuration: &str) -> Option<&'static str> {
    Some(match configuration {
        "implementation" | "api" | "compileOnly" | "annotationProcessor" => "compile",
        "runtimeOnly" | "developmentOnly" => "runtime",
        "testImplementation" | "testRuntimeOnly" | "testCompileOnly" => "test",
        _ => return None,
    })
}

/// A coordinate string must be `group:artifact[:version]` with literal parts: no ranges,
/// dynamic versions, classifiers, extensions or interpolation.
fn coordinate(text: &str, scope: &str) -> Option<(Dependency, bool)> {
    let parts: Vec<_> = text.split(':').collect();
    let (group, artifact, version) = match parts.as_slice() {
        [g, a] => (*g, *a, None),
        [g, a, v] => (*g, *a, Some(*v)),
        _ => return None,
    };
    if !literal(group)
        || !literal(artifact)
        || group.split('.').any(str::is_empty)
        || version.is_some_and(|v| !literal(v) || v.starts_with("latest.") || v.to_ascii_uppercase().ends_with("SNAPSHOT"))
    {
        return None;
    }
    let explicit = version.is_some();
    Some((
        Dependency {
            group: group.into(),
            artifact: artifact.into(),
            version: version.unwrap_or_default().into(),
            scope: scope.into(),
        },
        explicit,
    ))
}

pub(super) struct Declared {
    pub deps: Vec<Dependency>,
    pub boms: Vec<Dependency>,
}

pub(super) fn declared(text: &str) -> Declared {
    let mut deps = Vec::new();
    let mut boms: Vec<Dependency> = Vec::new();
    let mut seen = BTreeSet::new();
    for line in text.lines() {
        let line = line.split("//").next().unwrap_or("").trim();
        if line.contains("org.springframework.boot") && !line.contains("org.springframework.boot:") {
            let after = line.split(" version ").nth(1).map(str::trim_start);
            if let Some((v, _)) = after.and_then(quoted).filter(|(v, _)| literal(v)) {
                boms.push(bom("org.springframework.boot", "spring-boot-dependencies", v));
            }
        }
        for marker in ["platform(", "enforcedPlatform("] {
            let Some((_, rest)) = line.split_once(marker) else {
                continue;
            };
            if let Some((text, _)) = quoted(rest.trim_start()) {
                if let Some((d, true)) = coordinate(text, "managed catalog") {
                    if official(&d.group) {
                        boms.push(d);
                    }
                }
            }
        }
        let name_end = line
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(line.len());
        let Some(scope) = scope(&line[..name_end]) else {
            continue;
        };
        let rest = line[name_end..].trim_start();
        let rest = rest.strip_prefix('(').unwrap_or(rest).trim_start();
        let Some((text, tail)) = quoted(rest) else {
            continue;
        };
        if !matches!(tail.trim(), "" | ")") {
            continue;
        }
        if let Some((d, explicit)) = coordinate(text, scope) {
            if seen.insert((d.group.clone(), d.artifact.clone(), explicit)) {
                deps.push(d);
            }
        }
    }
    Declared { deps, boms }
}

fn bom(group: &str, artifact: &str, version: &str) -> Dependency {
    Dependency {
        group: group.into(),
        artifact: artifact.into(),
        version: version.into(),
        scope: "managed catalog; dependency presence unverified".into(),
    }
}

/// Finds `<artifact>-<version><ext>` below exactly one checksum directory.
pub(super) fn file(cache: &str, d: &Dependency, ext: &str) -> Result<Option<String>, Error> {
    if ![&d.group, &d.artifact, &d.version].into_iter().all(|s| literal(s)) {
        return Err(invalid("Unsafe Gradle cache coordinate."));
    }
    let base = format!("{cache}/{}/{}/{}", d.group, d.artifact, d.version);
    let name = format!("{}-{}{ext}", d.artifact, d.version);
    let Some(hashes) = super::versions::present(host::fs::read_dir(&base))? else {
        return Ok(None);
    };
    let mut found = None;
    for hash in hashes
        .iter()
        .filter(|e| e.kind == FileKind::Directory && literal(&e.name))
        .take(MAX_HASH_DIRS)
    {
        let dir = format!("{base}/{}", hash.name);
        let Ok(entries) = host::fs::read_dir(&dir) else {
            continue;
        };
        if entries
            .iter()
            .any(|e| e.kind == FileKind::File && e.name == name)
            && found.replace(format!("{dir}/{name}")).is_some()
        {
            return Err(invalid("Ambiguous Gradle cache entries for one coordinate."));
        }
    }
    Ok(found)
}

/// Declared dependencies with explicit or Boot-BOM-managed versions, plus the BOM POMs read.
pub(super) fn resolve(
    text: &str,
    cache: &str,
) -> Result<(Vec<Dependency>, Vec<(String, String)>), Error> {
    use sha2::{Digest, Sha256};
    let declared = declared(text);
    let mut managed: Vec<Dependency> = Vec::new();
    let mut sources = Vec::new();
    let mut visited = BTreeSet::new();
    for b in declared.boms.iter().take(8) {
        if !visited.insert(b.gav()) {
            continue;
        }
        let Some(path) = file(cache, b, ".pom")? else {
            continue;
        };
        let stat = host::fs::stat(&path)?;
        if stat.kind != FileKind::File || stat.size > MAX_BOM {
            return Err(invalid("Cached BOM POM is not a bounded regular file."));
        }
        let raw = host::fs::read_bytes(&path)?;
        let pom = std::str::from_utf8(&raw).map_err(|_| invalid("Cached POM is not UTF8."))?;
        managed.extend(versions::managed(pom)?.into_iter().filter(|d| official(&d.group)));
        sources.push((path, format!("{:x}", Sha256::digest(&raw))));
    }
    let mut out: Vec<Dependency> = Vec::new();
    for mut d in declared.deps {
        if d.version.is_empty() {
            let Some(m) = managed
                .iter()
                .find(|m| m.group == d.group && m.artifact == d.artifact)
            else {
                continue;
            };
            d.version = m.version.clone();
        }
        if !out.iter().any(|o| o.group == d.group && o.artifact == d.artifact) {
            out.push(d);
        }
    }
    for m in managed {
        if !out.iter().any(|o| o.group == m.group && o.artifact == m.artifact) {
            out.push(m);
        }
    }
    Ok((out, sources))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_ranges_dynamic_versions_classifiers_and_interpolation() {
        let d = declared(
            r#"
            implementation("org.a:ok:1.0")
            implementation 'org.a:managed'
            api "org.a:range:[1.0,2.0)"
            api "org.a:dyn:1.+"
            api "org.a:latest:latest.release"
            api "org.a:snap:1.0-SNAPSHOT"
            api "org.a:cls:1.0:sources"
            api "org.a:ext:1.0@aar"
            api "org.a:interp:${v}"
            api "../x:evil:1.0"
            api("org.a:paren:2.0") // trailing
            "#,
        );
        let names: Vec<_> = d.deps.iter().map(|d| d.artifact.as_str()).collect();
        assert_eq!(names, ["ok", "managed", "paren"]);
    }

    #[test]
    fn reads_boot_plugin_and_platform_boms() {
        let d = declared(
            "plugins { id 'org.springframework.boot' version '3.3.12' }\ndependencies { implementation platform(\"org.springframework.cloud:spring-cloud-dependencies:2023.0.1\") }",
        );
        let gavs: Vec<_> = d.boms.iter().map(Dependency::gav).collect();
        assert_eq!(
            gavs,
            [
                "org.springframework.boot:spring-boot-dependencies:3.3.12",
                "org.springframework.cloud:spring-cloud-dependencies:2023.0.1"
            ]
        );
    }
}
