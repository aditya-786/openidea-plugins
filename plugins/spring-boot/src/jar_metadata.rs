//! Explicit literal root-POM Maven-cache metadata snapshots, never classpath discovery.
use openidea_plugin::{Error, FileKind, Invocation, PickItem, host};
use sha2::{Digest, Sha256};
use spring_text::{
    config::canonical,
    metadata::{Catalog, Meta},
    xml,
};
use std::collections::{BTreeMap, BTreeSet};
#[path = "jar_versions.rs"]
mod versions;
#[path = "jar_gradle.rs"]
mod gradle;
const CACHE: &str = "~/.m2/repository";
#[derive(Clone, Debug, PartialEq, Eq)]
struct Dependency {
    group: String,
    artifact: String,
    version: String,
    scope: String,
}
impl Dependency {
    fn gav(&self) -> String {
        format!("{}:{}:{}", self.group, self.artifact, self.version)
    }
    fn path(&self, cache: &str) -> String {
        format!(
            "{}/{}/{}/{}/{}-{}.jar",
            cache,
            self.group.replace('.', "/"),
            self.artifact,
            self.version,
            self.artifact,
            self.version
        )
    }
}
fn invalid(message: &str) -> Error {
    Error::Invalid(message.into())
}
fn literal(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && !matches!(s, "." | "..")
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}
use spring_text::xml::namespace_valid;
fn pom_namespace<R>(
    reader: &quick_xml::reader::NsReader<R>,
    element: &quick_xml::events::BytesStart<'_>,
) -> Result<String, Error> {
    use quick_xml::name::ResolveResult;
    if !namespace_valid(reader, element) {
        return Err(invalid("Invalid POM namespace binding or qualified name."));
    }
    match reader.resolver().resolve_element(element.name()).0 {
        ResolveResult::Unbound => Ok(String::new()),
        ResolveResult::Bound(ns) if ns.as_ref() == "http://maven.apache.org/POM/4.0.0" => {
            Ok(ns.as_ref().to_owned())
        }
        _ => Err(invalid("Unbound or foreign POM element namespace.")),
    }
}
fn dependencies(text: &str) -> Result<Vec<Dependency>, Error> {
    if text.len() > 256 * 1024 {
        return Err(invalid(
            "Use a bounded literal root POM of at most 256 KiB.",
        ));
    }
    let mut reader = quick_xml::reader::NsReader::from_str(text);
    reader.config_mut().check_comments = true;
    let mut expected_namespace: Option<String> = None;
    let mut depth = 0usize;
    let mut roots = 0;
    loop {
        match reader.read_event() {
            Ok(quick_xml::events::Event::Start(e)) => {
                let namespace = pom_namespace(&reader, &e)?;
                if let Some(expected) = &expected_namespace {
                    if expected != &namespace {
                        return Err(invalid("Mixed POM element namespaces are unsupported."));
                    }
                } else {
                    expected_namespace = Some(namespace);
                }
                if depth == 0 {
                    roots += 1;
                }
                depth += 1;
                if depth > 32 {
                    return Err(invalid("POM nesting limit exceeded."));
                }
                for a in e.attributes().with_checks(true) {
                    a.map_err(|_| invalid("Invalid POM attributes."))?;
                }
            }
            Ok(quick_xml::events::Event::Empty(e)) => {
                let namespace = pom_namespace(&reader, &e)?;
                if let Some(expected) = &expected_namespace {
                    if expected != &namespace {
                        return Err(invalid("Mixed POM element namespaces are unsupported."));
                    }
                } else {
                    expected_namespace = Some(namespace);
                }
                if depth == 0 {
                    roots += 1;
                }
                for a in e.attributes().with_checks(true) {
                    a.map_err(|_| invalid("Invalid POM attributes."))?;
                }
            }
            Ok(quick_xml::events::Event::End(_)) => {
                depth = depth.checked_sub(1).ok_or("Invalid POM nesting.")?;
            }
            Ok(quick_xml::events::Event::DocType(_) | quick_xml::events::Event::CData(_)) => {
                return Err(invalid("DTD/CDATA POM forms are unsupported."));
            }
            Ok(quick_xml::events::Event::Text(text)) if depth == 0 => {
                if text
                    .as_ref()
                    .bytes()
                    .any(|b| !matches!(b, b' ' | b'\t' | b'\r' | b'\n'))
                {
                    return Err(invalid("Non-whitespace text outside the POM root."));
                }
            }
            Ok(quick_xml::events::Event::GeneralRef(_)) if depth == 0 => {
                return Err(invalid("Entity references outside the POM root."));
            }
            Ok(quick_xml::events::Event::Eof) => break,
            Ok(_) => {}
            Err(_) => return Err(invalid("The POM is not well-formed XML.")),
        }
    }
    if roots != 1 || depth != 0 {
        return Err(invalid("The POM must have one complete project root."));
    }
    let elements = xml::elements(text);
    if elements.iter().enumerate().any(|(i, _)| {
        xml::namespace(&elements, i).is_some_and(|n| n != "http://maven.apache.org/POM/4.0.0")
    }) {
        return Err(invalid("Foreign POM element namespaces are unsupported."));
    }
    if xml::root(&elements) != Some("project")
        || xml::namespace(&elements, 0).is_some_and(|n| n != "http://maven.apache.org/POM/4.0.0")
    {
        return Err(invalid("The root pom.xml is not a supported Maven POM."));
    }
    if elements.len() > 4096 {
        return Err(invalid("POM element limit exceeded."));
    }
    let properties = versions::properties(text)?;
    let managed = versions::managed(text)?;
    let mut result = vec![];
    let mut keys = BTreeSet::new();
    for (i, e) in elements
        .iter()
        .enumerate()
        .filter(|(_, e)| e.local() == "dependency")
    {
        let Some(parent) = e.parent else {
            continue;
        };
        if elements[parent].local() != "dependencies" || elements[parent].parent != Some(0) {
            continue;
        }
        let mut fields = BTreeMap::new();
        let mut supported = true;
        for child in elements.iter().filter(|child| child.parent == Some(i)) {
            if !matches!(
                child.local(),
                "groupId" | "artifactId" | "version" | "scope" | "type" | "optional" | "exclusions"
            ) {
                supported = false;
                break;
            }
            if child.local() == "exclusions" {
                continue;
            }
            let Some(close) = text
                .get(child.tag_end..child.end)
                .and_then(|s| s.rfind("</").map(|at| child.tag_end + at))
            else {
                supported = false;
                break;
            };
            let value = text[child.tag_end..close].trim();
            if (!literal(value)
                && !(child.local() == "version" && versions::resolve(value, &properties).is_some()))
                || fields.insert(child.local(), value).is_some()
            {
                supported = false;
                break;
            }
        }
        if !supported
            || fields.get("type").is_some_and(|v| *v != "jar")
            || fields.get("optional").is_some_and(|v| *v != "false")
        {
            continue;
        }
        let (Some(group), Some(artifact)) = (fields.get("groupId"), fields.get("artifactId"))
        else {
            continue;
        };
        let version = fields
            .get("version")
            .and_then(|v| versions::resolve(v, &properties))
            .or_else(|| {
                managed
                    .iter()
                    .find(|d| d.group == *group && d.artifact == *artifact)
                    .map(|d| d.version.clone())
            });
        let version = match (version, fields.contains_key("version")) {
            (Some(v), _) => v,
            (None, false) => String::new(),
            (None, true) => continue,
        };
        if group
            .split('.')
            .any(|p| p.is_empty() || p == "." || p == "..")
        {
            continue;
        }
        let scope = fields.get("scope").copied().unwrap_or("compile");
        if !matches!(scope, "compile" | "runtime" | "provided" | "test") {
            continue;
        }
        let dep = Dependency {
            group: (*group).into(),
            artifact: (*artifact).into(),
            version,
            scope: scope.into(),
        };
        if !keys.insert(format!("{}:{}", group, artifact)) {
            return Err(invalid("Duplicate direct dependency owners are ambiguous."));
        }
        result.push(dep);
        if result.len() > 128 {
            return Err(invalid("Direct dependency limit exceeded."));
        }
    }
    Ok(result)
}
struct Snapshot {
    pom: String,
    pom_digest: String,
    jar: String,
    hash: String,
    dep: Dependency,
    meta: Meta,
    entries: Vec<String>,
    version_sources: Vec<(String, String)>,
}
#[derive(Default)]
pub struct State {
    snapshots: BTreeMap<String, Vec<Snapshot>>,
    version_scopes: BTreeMap<String, (String, String, Vec<(String, String)>)>,
}
impl State {
    /// Drops what was read from `path` unless it still has the digest read
    /// (`None`: gone or unreadable).
    pub fn invalidate(&mut self, path: &str, digest: Option<&str>) {
        let same = |known: &str| digest == Some(known);
        self.version_scopes.retain(|_, (pom, pom_digest, catalogs)| {
            !(pom == path && !same(pom_digest))
                && !catalogs
                    .iter()
                    .any(|(catalog, known)| catalog == path && !same(known))
        });
        self.snapshots.retain(|_, snapshots| {
            !snapshots.iter().any(|s| {
                (s.pom == path && !same(&s.pom_digest))
                    || s.version_sources
                        .iter()
                        .any(|(source, known)| source == path && !same(known))
            })
        });
    }
    fn watches(&self, path: &str) -> bool {
        self.snapshots.values().flatten().any(|s| {
            s.pom == path || s.version_sources.iter().any(|(source, _)| source == path)
        }) || self
            .version_scopes
            .values()
            .any(|(pom, _, catalogs)| pom == path || catalogs.iter().any(|(c, _)| c == path))
    }
    pub fn source_changed(&mut self, path: &str, removed: bool) {
        if !self.watches(path) {
            return;
        }
        let digest = (!removed)
            .then(|| {
                let bytes = match host::project_of(path).and_then(|p| p.read_file(path).ok()) {
                    Some(text) => text.into_bytes(),
                    None => host::fs::read_bytes(path).ok()?,
                };
                Some(format!("{:x}", Sha256::digest(bytes)))
            })
            .flatten();
        self.invalidate(path, digest.as_deref());
    }
    pub fn catalog(&self, path: &str) -> Catalog {
        let mut c = Catalog::default();
        if let Some(root) = host::project_of(path).and_then(|p| p.root().ok()) {
            if let Some(snapshots) = self.snapshots.get(&root) {
                for snapshot in snapshots {
                    c.add(&snapshot.meta);
                }
            }
        }
        c
    }
    /// A selected version-qualified Spring artifact is not a complete runtime
    /// catalog. Never supplement it with authoritative bundled3.5 keys.
    pub fn version_scoped(&self, path: &str) -> bool {
        host::project_of(path)
            .and_then(|p| p.root().ok())
            .is_some_and(|root| self.version_scopes.contains_key(&root))
    }

    pub fn bundled(&self, path: &str) -> &'static Catalog {
        if self.version_scoped(path) {
            static EMPTY: std::sync::OnceLock<Catalog> = std::sync::OnceLock::new();
            EMPTY.get_or_init(Catalog::default)
        } else {
            super::meta::bundled()
        }
    }
    pub fn provenance(&self, path: &str) -> Option<String> {
        let root = host::project_of(path)?.root().ok()?;
        let snapshots = self.snapshots.get(&root)?;
        let descriptions=snapshots.iter().map(|s|format!(
            "Selected dependency snapshot: {} ({} scope), {} · SHA-256 {} · entries {}. Version catalog sources: {}. Declared source provenance; resolved/runtime classpath is unverified.",
            s.dep.gav(),s.dep.scope,s.jar,s.hash,s.entries.join(", "),s.version_sources.iter().map(|(path,digest)|format!("{path} SHA-256 {digest}")).collect::<Vec<_>>().join("; ")
        )).collect::<Vec<_>>();
        (!descriptions.is_empty()).then(|| descriptions.join("\n\n"))
    }
    pub fn import(&mut self, ctx: &Invocation) -> Result<Option<String>, Error> {
        let result = self.import_at(ctx, CACHE);
        if result.is_err()
            && ctx
                .project()
                .and_then(|p| p.root().ok())
                .is_some_and(|root| self.snapshots.contains_key(&root))
        {
            host::notify(
                openidea_plugin::NotificationLevel::Warning,
                "Spring Boot JAR metadata",
                "Read failed. The previous selected dependency snapshot remains available until its source changes; no new metadata was installed.",
            );
        }
        result
    }
    fn import_at(&mut self, ctx: &Invocation, cache: &str) -> Result<Option<String>, Error> {
        let project = ctx
            .project()
            .ok_or("Open the Maven or Gradle project first.")?;
        let root = project.root()?;
        let pom_path = format!("{root}/pom.xml");
        if root.len() > 4096 || root.chars().any(char::is_control) {
            return Err(invalid("Unreviewable project identity."));
        }
        let (pom, source, gradle_build) = match project.read_file(&pom_path) {
            Ok(source) => (pom_path, source, false),
            Err(error) => {
                let found = ["build.gradle.kts", "build.gradle"].iter().find_map(|name| {
                    let path = format!("{root}/{name}");
                    project.read_file(&path).ok().map(|text| (path, text))
                });
                let Some((path, text)) = found else {
                    return Err(error);
                };
                (path, text, true)
            }
        };
        let cache = if gradle_build && cache == CACHE {
            gradle::CACHE
        } else {
            cache
        };
        let pom_digest = format!("{:x}", Sha256::digest(source.as_bytes()));
        if self.snapshots.get(&root).is_some_and(|snapshots| {
            snapshots
                .iter()
                .any(|snapshot| snapshot.pom_digest != pom_digest)
        }) {
            self.snapshots.remove(&root);
        }
        if self
            .version_scopes
            .get(&root)
            .is_some_and(|(_, digest, _)| *digest != pom_digest)
        {
            self.version_scopes.remove(&root);
        }
        let (dependencies, catalog_sources) = if gradle_build {
            match host::fs::read_dir(cache) {
                Err(Error::Denied(_)) => return gradle_denied(),
                Err(e) => return Err(e),
                Ok(_) => {}
            }
            match gradle::resolve(&source, cache) {
                Err(Error::Denied(_)) => return gradle_denied(),
                other => other?,
            }
        } else {
            let mut dependencies = dependencies(&source)?;
            let (catalogs, catalog_sources) = versions::cache_catalog(&source, cache)?;
            for d in dependencies.iter_mut().filter(|d| d.version.is_empty()) {
                if let Some(c) = catalogs
                    .iter()
                    .find(|c| c.group == d.group && c.artifact == d.artifact)
                {
                    d.version = c.version.clone();
                }
            }
            for catalog in catalogs {
                if !dependencies
                    .iter()
                    .any(|d| d.group == catalog.group && d.artifact == catalog.artifact)
                {
                    dependencies.push(catalog);
                }
            }
            dependencies.retain(|d| {
                !d.version.is_empty() && !d.version.to_ascii_uppercase().ends_with("SNAPSHOT")
            });
            (dependencies, catalog_sources)
        };
        if dependencies.len() > 512 {
            return Err(invalid(
                "Declared metadata dependency catalog exceeds 512 candidates.",
            ));
        }
        if dependencies.is_empty() {
            return Err(invalid(
                "Found no dependency with a known version in the root build file (snapshot versions are skipped). Build the project once so its Spring Boot parent or BOM is in the local Maven or Gradle cache, then try again.",
            ));
        }
        let choices: Vec<_> = dependencies
            .iter()
            .enumerate()
            .map(|(i, d)| PickItem::new(&i.to_string(), &format!("{} ({})", d.gav(), d.scope)))
            .collect();
        let Some(key) = host::pick("Read metadata from one declared dependency", &choices)?
        else {
            return Ok(None);
        };
        let selected = key
            .parse::<usize>()
            .ok()
            .and_then(|i| dependencies.get(i))
            .ok_or("Unknown dependency selection.")?
            .clone();
        let jar = if gradle_build {
            match gradle::file(cache, &selected, ".jar") {
                Err(Error::Denied(_)) => return gradle_denied(),
                Err(e) => return Err(e),
                Ok(found) => found.ok_or("Selected JAR is not in the Gradle module cache.")?,
            }
        } else {
            selected.path(cache)
        };
        let grant_probe = host::fs::stat(&jar).map_err(|error| if gradle_build {
            ui_kit::denied::map(error, "Reading Gradle JAR metadata", "fs.read:~/.gradle/caches/modules-2/files-2.1", "Spring Boot Tools")
        } else {
            ui_kit::denied::map(error, "Reading Maven JAR metadata", "fs.read:~/.m2/repository", "Spring Boot Tools")
        })?;
        if grant_probe.kind != FileKind::File
            || grant_probe.size > super::jar_archive::MAX_ARCHIVE as u64
        {
            return Err(invalid("Selected JAR must be a file of at most 8 MiB."));
        }
        let review = format!(
            "Dependency: {} ({} scope), declared in {pom}\nJAR: {jar}\n\nReads only the Spring configuration metadata inside the JAR; nothing runs. Metadata read from other JARs is kept.",
            selected.gav(),
            selected.scope,
        );
        if !ui_kit::prompts::confirm_exact(&format!("Read Spring metadata from {}?", selected.gav()), &review)? {
            return Ok(None);
        }
        let ensure = || -> Result<(), Error> {
            if ctx.project().and_then(|p| p.root().ok()).as_deref() != Some(&root)
                || host::project_of(&pom)
                    .and_then(|p| p.root().ok())
                    .as_deref()
                    != Some(&root)
                || project.read_file(&pom)? != source
            {
                return Err(invalid("The build file changed. Run Read Metadata from Dependency JAR… again."));
            }
            for (path, digest) in &catalog_sources {
                let raw = host::fs::read_bytes(path)?;
                if format!("{:x}", Sha256::digest(raw)) != *digest {
                    return Err(invalid(
                        "Declared parent/BOM version catalog changed; read metadata again.",
                    ));
                }
            }
            Ok(())
        };
        ensure()?;
        let before = host::fs::stat(&jar)?;
        if before.kind != FileKind::File || before.size > super::jar_archive::MAX_ARCHIVE as u64 {
            return Err(invalid("Selected JAR must be a file of at most 8 MiB."));
        }
        let bytes = host::fs::read_bytes(&jar)?;
        let after = host::fs::stat(&jar)?;
        if before != after || bytes.len() as u64 != before.size {
            return Err(invalid("The selected JAR changed during reading."));
        }
        let entries = super::jar_archive::metadata(&bytes)?;
        let meta = parse_entries(&entries)?;
        ensure()?;
        let count = meta.props.len();
        let entries = entries.into_iter().map(|(name, _)| name).collect();
        self.snapshots.retain(|_, snapshots| {
            !snapshots.is_empty()
        });
        if !self.snapshots.contains_key(&root) && self.snapshots.len() >= 8 {
            return Err(invalid(
                "Eight project snapshots are already retained; reload before importing another project.",
            ));
        }
        if matches!(
            selected.group.as_str(),
            "org.springframework.boot" | "org.springframework.cloud"
        ) && !self.version_scopes.contains_key(&root)
            && self.version_scopes.len() >= 8
        {
            return Err(invalid(
                "Eight version source scopes are retained; reload before importing another project.",
            ));
        }
        let retained = self.snapshots.entry(root.clone()).or_default();
        let other = retained
            .iter()
            .filter(|snapshot| {
                snapshot.dep.group != selected.group || snapshot.dep.artifact != selected.artifact
            })
            .collect::<Vec<_>>();
        if other.len() >= 16
            || other.iter().map(|s| s.meta.props.len()).sum::<usize>() + meta.props.len() > 32768
        {
            return Err(invalid(
                "Selected artifact metadata snapshot budget exceeded.",
            ));
        }
        let incoming: BTreeMap<String, &spring_text::metadata::Prop> = meta
            .props
            .iter()
            .map(|prop| (canonical(&prop.name), prop))
            .collect();
        let incoming_hints: BTreeMap<String, &Vec<String>> = meta
            .hints
            .iter()
            .map(|(name, values)| (canonical(name), values))
            .collect();
        for snapshot in other {
            if snapshot
                .meta
                .props
                .iter()
                .any(|old| incoming.get(&canonical(&old.name)).is_some_and(|new| **new != *old))
                || snapshot.meta.hints.iter().any(|(old, values)| {
                    incoming_hints
                        .get(&canonical(old))
                        .is_some_and(|new| *new != values)
                })
            {
                return Err(invalid(
                    "Selected artifact metadata conflicts with a retained snapshot; no new facts installed.",
                ));
            }
        }
        retained.retain(|snapshot| {
            snapshot.dep.group != selected.group || snapshot.dep.artifact != selected.artifact
        });
        if matches!(
            selected.group.as_str(),
            "org.springframework.boot" | "org.springframework.cloud"
        ) {
            self.version_scopes.insert(
                root,
                (
                    pom.clone(),
                    pom_digest.clone(),
                    catalog_sources.clone(),
                ),
            );
        }
        retained.push(Snapshot {
            pom,
            pom_digest,
            jar,
            hash: format!("{:x}", Sha256::digest(&bytes)),
            dep: selected,
            meta,
            entries,
            version_sources: catalog_sources,
        });
        host::notify(
            openidea_plugin::NotificationLevel::Info,
            "Spring Boot JAR metadata",
            &format!(
                "Loaded {count} properties from {artifact}.", artifact = retained.last().map_or(String::new(), |s| s.dep.gav())
            ),
        );
        Ok(None)
    }
}
fn parse_entries(entries: &[(String, String)]) -> Result<Meta, Error> {
    if entries.is_empty()
        || entries.len() > 2
        || entries.iter().any(|(name, text)| {
            !matches!(
                name.as_str(),
                "META-INF/spring-configuration-metadata.json"
                    | "META-INF/additional-spring-configuration-metadata.json"
            ) || text.len() > 512 * 1024
        })
        || entries.iter().map(|(_, text)| text.len()).sum::<usize>() > 768 * 1024
    {
        return Err(invalid("Metadata document budget exceeded."));
    }
    let mut result = Meta::default();
    let mut names = BTreeSet::new();
    let mut hint_names = BTreeSet::new();
    for (_, text) in entries {
        let mut document_names = BTreeSet::new();
        let mut document_hints = BTreeSet::new();
        let v: serde_json::Value =
            serde_json::from_str(text).map_err(|_| invalid("Metadata is not valid JSON."))?;
        if !v.is_object()
            || v.get("properties").is_some_and(|p| !p.is_array())
            || v.get("hints").is_some_and(|p| !p.is_array())
        {
            return Err(invalid("Unsupported configuration metadata structure."));
        }
        if v.get("properties")
            .and_then(|v| v.as_array())
            .is_some_and(|props| {
                props.iter().any(|p| {
                    !p.is_object()
                        || p.get("name").and_then(|n| n.as_str()).is_none()
                        || ["type", "description"]
                            .iter()
                            .any(|key| p.get(key).is_some_and(|v| !v.is_string()))
                        || p.get("deprecation")
                            .is_some_and(|v| !v.is_object() && !v.is_null())
                        || p.get("deprecated").is_some_and(|v| !v.is_boolean())
                })
            })
        {
            return Err(invalid("Unsupported metadata property record."));
        }
        let m = super::meta::parse_json(text).ok_or("Invalid configuration metadata.")?;
        for p in m.props {
            if p.ty.len() > 256
                || p.default.len() > 1024
                || p.ty.chars().chain(p.default.chars()).any(char::is_control)
                || p.deprecated.as_ref().is_some_and(|d| {
                    d.replacement.len() > 256 || d.replacement.chars().any(char::is_control)
                })
                || p.name.len() > 256
                || canonical(&p.name).is_empty()
                || !p.name.bytes().all(|b| {
                    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'[' | b']')
                })
                || !document_names.insert(canonical(&p.name))
            {
                return Err(invalid("Ambiguous or unsupported metadata property names."));
            }
            if !names.insert(canonical(&p.name)) {
                if result
                    .props
                    .iter()
                    .any(|old| canonical(&old.name) == canonical(&p.name) && old == &p)
                {
                    continue;
                }
                return Err(invalid(
                    "The main/additional metadata documents disagree on a property.",
                ));
            }
            result.props.push(p);
        }
        for (name, values) in m.hints {
            if canonical(&name).is_empty()
                || !name.bytes().all(|b| {
                    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'[' | b']')
                })
                || name.len() > 256
                || values.len() > 128
                || values
                    .iter()
                    .any(|v| v.len() > 1024 || v.chars().any(char::is_control))
                || !document_hints.insert(canonical(&name))
            {
                return Err(invalid("Ambiguous or oversized metadata hints."));
            }
            if !hint_names.insert(canonical(&name)) {
                if result.hints.iter().any(|(old, old_values)| {
                    canonical(old) == canonical(&name) && old_values == &values
                }) {
                    continue;
                }
                return Err(invalid(
                    "The main/additional metadata documents disagree on a hint.",
                ));
            }
            result.hints.push((name, values));
        }
        if result.props.len() > 4096 || result.hints.len() > 1024 {
            return Err(invalid("Configuration metadata record limit exceeded."));
        }
    }
    if result.props.is_empty() {
        return Err(invalid(
            "The selected metadata contains no supported properties.",
        ));
    }
    Ok(result)
}

fn gradle_denied() -> Result<Option<String>, Error> {
    host::notify(
        openidea_plugin::NotificationLevel::Warning,
        "Spring Boot JAR metadata",
        &ui_kit::denied::message("Reading Gradle JAR metadata", "fs.read:~/.gradle/caches/modules-2/files-2.1", "Spring Boot Tools"),
    );
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::hover::HoverProvider;
    use openidea_plugin::mock::MockHost;
    use openidea_plugin::providers::Document;
    const POM: &str = "<project xmlns='http://maven.apache.org/POM/4.0.0'><modelVersion>4.0.0</modelVersion><dependencies><dependency><groupId>org.example</groupId><artifactId>config</artifactId><version>1.2.3</version></dependency></dependencies></project>";
    const JSON: &str = r#"{"properties":[{"name":"custom.jar.feature","type":"java.lang.Boolean","defaultValue":true,"description":"Selected archive fact."}],"hints":[{"name":"custom.jar.mode","values":[{"value":"safe"}]}]}"#;
    const PATH: &str = "/m2/org/example/config/1.2.3/config-1.2.3.jar";
    fn setup() -> MockHost {
        let h = MockHost::install().with_capabilities(&[
            "project.read",
            "fs.write:/m2",
            "fs.read:/m2",
            "editor.read",
        ]);
        h.add_file("pom.xml", POM);
        let bytes = super::super::jar_archive::fixture(&[(
            "META-INF/spring-configuration-metadata.json",
            JSON.as_bytes(),
            8,
        )]);
        h.add_external_file(PATH, "fixture placeholder");
        host::fs::write_bytes(PATH, &bytes).unwrap();
        h
    }
    fn answer(h: &MockHost, confirm: bool) {
        h.answer_next_prompt(Some(&["0"]));
        h.answer_next_prompt(if confirm { Some(&["ok"]) } else { None });
    }
    #[test]
    fn version_properties_and_direct_managed_versions_are_exact_not_maven_execution() {
        let source=POM.replace("<modelVersion>","<properties><config.version>1.2.3</config.version><alias>${config.version}</alias></properties><modelVersion>").replace("<version>1.2.3</version>","<version>${alias}</version>");
        assert_eq!(dependencies(&source).unwrap()[0].version, "1.2.3");
        let managed=POM.replace("<dependencies>","<dependencyManagement><dependencies>").replace("</dependencies>","</dependencies></dependencyManagement>").replace("</project>","<dependencies><dependency><groupId>org.example</groupId><artifactId>config</artifactId></dependency></dependencies></project>");
        assert_eq!(dependencies(&managed).unwrap()[0].version, "1.2.3");
        assert!(
            dependencies(&source.replace(
                "<config.version>1.2.3</config.version>",
                "<config.version>${alias}</config.version>"
            ))
            .unwrap()
            .is_empty()
        );
        assert!(
            dependencies(&source.replace(
                "<alias>${config.version}</alias>",
                "<alias>${config.version}</alias><alias>4</alias>"
            ))
            .is_err()
        );
    }
    #[test]
    fn declared_boot_parent_and_cloud_bom_versions_coexist_as_reviewed_local_snapshots() {
        let h = setup();
        let source = "<project><parent><groupId>org.springframework.boot</groupId><artifactId>spring-boot-starter-parent</artifactId><version>4.0.0</version></parent><dependencyManagement><dependencies><dependency><groupId>org.springframework.cloud</groupId><artifactId>spring-cloud-dependencies</artifactId><version>2025.0.0</version><type>pom</type><scope>import</scope></dependency></dependencies></dependencyManagement></project>";
        h.add_file("pom.xml", source);
        for (path, text) in [
            (
                "org/springframework/boot/spring-boot-starter-parent/4.0.0/spring-boot-starter-parent-4.0.0.pom",
                "<project><parent><groupId>org.springframework.boot</groupId><artifactId>spring-boot-dependencies</artifactId><version>4.0.0</version></parent><groupId>org.springframework.boot</groupId><artifactId>spring-boot-starter-parent</artifactId><version>4.0.0</version></project>",
            ),
            (
                "org/springframework/boot/spring-boot-dependencies/4.0.0/spring-boot-dependencies-4.0.0.pom",
                "<project><groupId>org.springframework.boot</groupId><artifactId>spring-boot-dependencies</artifactId><version>4.0.0</version><dependencyManagement><dependencies><dependency><groupId>org.springframework.boot</groupId><artifactId>spring-boot-autoconfigure</artifactId><version>${project.version}</version></dependency></dependencies></dependencyManagement></project>",
            ),
            (
                "org/springframework/cloud/spring-cloud-dependencies/2025.0.0/spring-cloud-dependencies-2025.0.0.pom",
                "<project><groupId>org.springframework.cloud</groupId><artifactId>spring-cloud-dependencies</artifactId><version>2025.0.0</version><properties><context.version>4.3.0</context.version></properties><dependencyManagement><dependencies><dependency><groupId>org.springframework.cloud</groupId><artifactId>spring-cloud-context</artifactId><version>${context.version}</version></dependency></dependencies></dependencyManagement></project>",
            ),
        ] {
            h.add_external_file(&format!("/m2/{path}"), text);
        }
        for (path, key) in [
            (
                "org/springframework/boot/spring-boot-autoconfigure/4.0.0/spring-boot-autoconfigure-4.0.0.jar",
                "boot.versioned.feature",
            ),
            (
                "org/springframework/cloud/spring-cloud-context/4.3.0/spring-cloud-context-4.3.0.jar",
                "cloud.versioned.feature",
            ),
        ] {
            let json =
                format!(r#"{{"properties":[{{"name":"{key}","type":"java.lang.Boolean"}}]}}"#);
            let bytes = super::super::jar_archive::fixture(&[(
                "META-INF/spring-configuration-metadata.json",
                json.as_bytes(),
                8,
            )]);
            h.add_external_file(&format!("/m2/{path}"), "");
            host::fs::write_bytes(&format!("/m2/{path}"), &bytes).unwrap();
        }
        let mut state = State::default();
        answer(&h, true);
        state.import_at(&h.invocation(), "/m2").unwrap();
        h.answer_next_prompt(Some(&["1"]));
        h.answer_next_prompt(Some(&["ok"]));
        state.import_at(&h.invocation(), "/m2").unwrap();
        let catalog = state.catalog("/project/application.properties");
        let lookup = spring_text::metadata::Lookup(vec![&catalog]);
        assert!(lookup.prop("boot.versioned.feature").is_some());
        assert!(lookup.prop("cloud.versioned.feature").is_some());
        assert!(state.version_scoped("/project/application.properties"));
        assert!(
            spring_text::metadata::Lookup(vec![state.bundled("/project/application.properties")])
                .prop("server.port")
                .is_none()
        );
        let provenance = state.provenance("/project/application.properties").unwrap();
        assert!(
            provenance.contains("spring-boot-autoconfigure:4.0.0")
                && provenance.contains("spring-cloud-context:4.3.0")
                && provenance.contains("dependency presence unverified")
        );
        assert!(h.process_runs().is_empty() && h.requests().is_empty());
        state.invalidate("/project/pom.xml", None);
        let cleared = state.catalog("/project/application.properties");
        assert!(
            spring_text::metadata::Lookup(vec![&cleared])
                .prop("boot.versioned.feature")
                .is_none()
        );
    }
    #[test]
    fn version_scoped_provider_lookup_drops_bundled_keys_and_withholds_incomplete_absence_findings()
    {
        use openidea_plugin::{Position, diagnostics::DiagnosticsProvider, providers::Providers};
        let h = setup();
        h.add_file(
            "pom.xml",
            &POM.replace("org.example", "org.springframework.boot")
                .replace(
                    "<artifactId>config</artifactId>",
                    "<artifactId>spring-boot</artifactId>",
                )
                .replace("1.2.3", "4.1.2"),
        );
        let path = "/m2/org/springframework/boot/spring-boot/4.1.2/spring-boot-4.1.2.jar";
        h.add_external_file(path, "");
        let bytes = super::super::jar_archive::fixture(&[(
            "META-INF/spring-configuration-metadata.json",
            JSON.as_bytes(),
            8,
        )]);
        host::fs::write_bytes(path, &bytes).unwrap();
        answer(&h, true);
        let mut state = State::default();
        state.import_at(&h.invocation(), "/m2").unwrap();
        assert!(state.version_scoped("/project/application.properties"));
        assert!(
            spring_text::metadata::Lookup(vec![state.bundled("/project/application.properties")])
                .prop("server.port")
                .is_none()
        );
        let mut tools = crate::BootTools {
            jars: state,
            ..Default::default()
        };
        let doc = |text: &str| Document {
            path: "/project/application.properties".into(),
            language: "properties".into(),
            version: 1,
            text: text.into(),
        };
        assert!(
            spring_text::metadata::Lookup(vec![super::super::meta::bundled()])
                .prop("server.port")
                .is_some()
        );
        assert!(
            tools
                .completion(&doc("server.po"), Position::new(0, 9), None)
                .iter()
                .all(|item| item.label != "server.port")
        );
        assert!(
            tools
                .hover(&doc("server.port=8080"), Position::new(0, 3))
                .is_none()
        );
        assert!(
            tools
                .completion(&doc("custom.jar.fea"), Position::new(0, 14), None)
                .iter()
                .any(|item| item.label == "custom.jar.feature")
        );
        assert!(
            tools
                .diagnostics(&doc("custom.jar.typo=true"))
                .iter()
                .all(|finding| finding.code.as_deref() != Some("unknown-property"))
        );
        assert!(h.process_runs().is_empty() && h.requests().is_empty());
    }
    #[test]
    #[ignore = "requires explicit SPRING_METADATA_REAL_FIXTURE with downloaded official POM/JAR fixture"]
    fn real_versioned_boot_cloud_metadata_fixture() {
        let directory = std::env::var("SPRING_METADATA_REAL_FIXTURE")
            .expect("set explicit downloaded fixture directory");
        let h =
            MockHost::install().with_capabilities(&["project.read", "fs.write:/m2", "fs.read:/m2"]);
        h.add_file(
            "pom.xml",
            &std::fs::read_to_string(format!("{directory}/project-pom.xml")).unwrap(),
        );
        let base = std::path::Path::new(&directory).join("m2");
        let mut queue = vec![base.clone()];
        let mut count = 0;
        while let Some(path) = queue.pop() {
            for entry in std::fs::read_dir(path).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    queue.push(path);
                } else {
                    count += 1;
                    assert!(count <= 80);
                    let relative = path.strip_prefix(&base).unwrap().to_str().unwrap();
                    let target = format!("/m2/{relative}");
                    h.add_external_file(&target, "");
                    host::fs::write_bytes(&target, &std::fs::read(path).unwrap()).unwrap();
                }
            }
        }
        let source = h
            .invocation()
            .project()
            .unwrap()
            .read_file("pom.xml")
            .unwrap();
        let (candidates, _) = versions::cache_catalog(&source, "/m2").unwrap();
        let mut state = State::default();
        for artifact in ["spring-boot", "spring-cloud-context"] {
            let index = candidates
                .iter()
                .position(|d| d.artifact == artifact)
                .unwrap()
                .to_string();
            h.answer_next_prompt(Some(&[&index]));
            h.answer_next_prompt(Some(&["ok"]));
            state.import_at(&h.invocation(), "/m2").unwrap();
        }
        let catalog = state.catalog("/project/application.properties");
        let lookup = spring_text::metadata::Lookup(vec![&catalog]);
        assert!(lookup.prop("spring.main.banner-mode").is_some());
        assert!(
            lookup
                .prop("spring.cloud.refresh.on-restart.enabled")
                .is_some()
        );
        assert_eq!(state.snapshots["/project"].len(), 2);
        for snapshot in &state.snapshots["/project"] {
            println!(
                "{} props={} SHA-256={} catalog_sources={}",
                snapshot.dep.gav(),
                snapshot.meta.props.len(),
                snapshot.hash,
                snapshot.version_sources.len()
            );
        }
        assert!(h.requests().is_empty() && h.process_runs().is_empty());
    }
    #[test]
    fn identical_generated_and_additional_records_deduplicate_but_conflicts_remain_errors() {
        let entries = vec![
            (
                "META-INF/spring-configuration-metadata.json".into(),
                JSON.into(),
            ),
            (
                "META-INF/additional-spring-configuration-metadata.json".into(),
                JSON.into(),
            ),
        ];
        let parsed = parse_entries(&entries).unwrap();
        assert_eq!(parsed.props.len(), 1);
        assert_eq!(parsed.hints.len(), 1);
        let mut conflict = entries;
        conflict[1].1 = JSON.replace("java.lang.Boolean", "java.lang.String");
        assert!(parse_entries(&conflict).is_err());
    }
    #[test]
    fn commented_pom_selects_only_live_literal_dependency() {
        let text = format!("<!-- <project><dependencies><dependency>fake</dependency></dependencies></project> -->\n{}\n<!-- trailing comment -->", POM.replace("<dependencies>", "<!-- dependency inventory --><dependencies><!-- <dependency><groupId>evil</groupId><artifactId>fake</artifactId><version>1</version></dependency> -->"));
        assert_eq!(dependencies(&text).unwrap(), dependencies(POM).unwrap());
        let h = setup();
        h.add_file("pom.xml", &text);
        answer(&h, true);
        let mut state = State::default();
        state.import_at(&h.invocation(), "/m2").unwrap();
        assert!(
            state
                .provenance("/project/application.properties")
                .unwrap()
                .contains("org.example:config:1.2.3")
        );
        assert!(h.requests().is_empty() && h.process_runs().is_empty());
    }
    #[test]
    fn malformed_or_coordinate_embedded_comments_do_not_authorize_archive() {
        for text in [
            format!("bad{POM}"),
            format!("{POM}bad"),
            format!("<!-- bad--comment -->{POM}"),
            format!("<!-- unterminated {POM}"),
            format!("<!DOCTYPE project SYSTEM 'https://example.invalid/pom.dtd'>{POM}"),
            POM.replace(
                "<groupId>org.example</groupId>",
                "<groupId><![CDATA[org.example]]></groupId>",
            ),
        ] {
            assert!(dependencies(&text).is_err(), "{text}");
        }
        for value in [
            "org.<!--comment-->example",
            "org.<?p x?>example",
            "org.&#101;xample",
        ] {
            assert!(
                dependencies(&POM.replace("org.example", value))
                    .unwrap()
                    .is_empty()
            );
        }
    }
    #[test]
    fn direct_root_dependencies_reject_unbound_properties_profiles_and_path_injection() {
        let d = dependencies(POM).unwrap();
        assert_eq!(d.len(), 1);
        assert_eq!(
            d[0].path(CACHE),
            "~/.m2/repository/org/example/config/1.2.3/config-1.2.3.jar"
        );
        for replacement in ["${version}", "../escape", "[1,2)", ".."] {
            assert!(
                dependencies(&POM.replace("1.2.3", replacement))
                    .unwrap()
                    .is_empty()
            );
        }
        assert!(
            dependencies(&POM.replace(
                "<version>1.2.3</version>",
                "<version>1.2.3</version><classifier>sources</classifier>"
            ))
            .unwrap()
            .is_empty()
        );
        assert!(
            dependencies(
                &POM.replace("<dependencies>", "<profiles><profile><dependencies>")
                    .replace("</dependencies>", "</dependencies></profile></profiles>")
            )
            .unwrap()
            .is_empty()
        );
        assert!(dependencies(&POM.replace("</project>","<dependencies><dependency><groupId>org.example</groupId><artifactId>config</artifactId><version>2</version></dependency></dependencies></project>")).is_err());
        assert!(dependencies(&POM.replace("</dependencies>", "")).is_err());
        assert!(
            dependencies(&POM.replace("http://maven.apache.org/POM/4.0.0", "urn:foreign")).is_err()
        );
    }
    #[test]
    fn explicit_import_is_root_owned_and_supplies_hover_provenance_without_requests() {
        let h = setup();
        answer(&h, true);
        let mut state = State::default();
        state.import_at(&h.invocation(), "/m2").unwrap();
        let catalog = state.catalog("/project/application.properties");
        assert!(
            spring_text::metadata::Lookup(vec![&catalog])
                .prop("custom.jar.feature")
                .is_some()
        );
        let other = state.catalog("/another/application.properties");
        assert!(
            spring_text::metadata::Lookup(vec![&other])
                .prop("custom.jar.feature")
                .is_none()
        );
        let mut plugin = crate::BootTools {
            jars: state,
            ..Default::default()
        };
        let doc = Document {
            path: "/project/application.properties".into(),
            language: "properties".into(),
            text: "custom.jar.feature=true".into(),
            version: 3,
        };
        let hover = plugin
            .hover(&doc, openidea_plugin::Position::new(0, 5))
            .unwrap();
        assert!(
            hover
                .markdown
                .contains("Selected dependency snapshot")
                && hover.markdown.contains("SHA-256")
                && hover.markdown.contains("classpath is unverified")
        );
        assert!(h.requests().is_empty() && h.process_runs().is_empty());
    }
    #[test]
    fn cancelled_denied_missing_and_failed_reimport_keep_the_previous_snapshot() {
        let h = setup();
        let mut state = State::default();
        answer(&h, true);
        state.import_at(&h.invocation(), "/m2").unwrap();
        let previous = state.provenance("/project/application.properties").unwrap();
        answer(&h, false);
        state.import_at(&h.invocation(), "/m2").unwrap();
        assert_eq!(
            state.provenance("/project/application.properties").unwrap(),
            previous
        );
        host::fs::write_bytes(PATH, b"invalid archive").unwrap();
        answer(&h, true);
        assert!(state.import_at(&h.invocation(), "/m2").is_err());
        assert_eq!(
            state.provenance("/project/application.properties").unwrap(),
            previous
        );
        let h = h.with_capabilities(&["project.read"]);
        h.answer_next_prompt(Some(&["0"]));
        assert!(matches!(
            state.import_at(&h.invocation(), "/m2"),
            Err(Error::Invalid(message)) if message == "Reading Maven JAR metadata needs the fs.read:~/.m2/repository permission. Allow it in Settings › Plugins › Spring Boot Tools."
        ));
        assert_eq!(
            state.provenance("/project/application.properties").unwrap(),
            previous
        );
        assert!(h.requests().is_empty() && h.process_runs().is_empty());
    }
    #[test]
    fn current_unsaved_pom_drives_selection_and_events_expiry_remove_facts() {
        let h = setup();
        h.open_editor(
            "/project/pom.xml",
            "xml",
            &POM.replace("<version>1.2.3</version>", "<version>${unsaved}</version>"),
            &[],
        );
        assert!(State::default().import_at(&h.invocation(), "/m2").is_err());
        h.open_editor("/project/pom.xml", "xml", POM, &[]);
        answer(&h, true);
        let mut state = State::default();
        state.import_at(&h.invocation(), "/m2").unwrap();
        assert!(
            state
                .provenance("/project/application.properties")
                .is_some()
        );
        answer(&h, true);
        state.import_at(&h.invocation(), "/m2").unwrap();
        state.invalidate("/other/pom.xml", None);
        assert!(
            state
                .provenance("/project/application.properties")
                .is_some()
        );
        state.invalidate("/project/pom.xml", None);
        assert!(
            state
                .provenance("/project/application.properties")
                .is_none()
        );
    }
    #[test]
    fn ambiguous_or_oversized_metadata_is_not_silently_merged() {
        let entry = |text: &str| {
            vec![(
                "META-INF/spring-configuration-metadata.json".into(),
                text.into(),
            )]
        };
        assert!(parse_entries(&entry(JSON)).is_ok());
        assert!(parse_entries(&entry("{bad")).is_err());
        assert!(parse_entries(&entry(r#"{"properties":{}}"#)).is_err());
        assert!(
            parse_entries(&entry(r#"{"properties":[{"name":"a-b"},{"name":"a_b"}]}"#)).is_err()
        );
        assert!(
            parse_entries(&entry(
                &JSON.replace("Selected archive fact.", &"x".repeat(512 * 1024))
            ))
            .is_err()
        );
        assert!(
            parse_entries(&entry(
                r#"{"properties":[{"name":"a","defaultValue":"line\nbreak"}]}"#
            ))
            .is_err()
        );
    }
    #[test]
    fn explicit_observed_pom_changes_do_not_reuse_last_good_from_another_source_scope() {
        let h = setup();
        let mut state = State::default();
        answer(&h, true);
        state.import_at(&h.invocation(), "/m2").unwrap();
        assert!(
            state
                .provenance("/project/application.properties")
                .is_some()
        );
        h.add_file("pom.xml", &POM.replace("1.2.3", "${new.version}"));
        assert!(state.import_at(&h.invocation(), "/m2").is_err());
        assert!(
            state
                .provenance("/project/application.properties")
                .is_none()
        );
    }
    #[test]
    fn malformed_namespace_source_cannot_authorize_selected_metadata_archive() {
        for binding in [
            "xmlns:xml=\"urn:foreign\"",
            "xmlns:xmlns=\"urn:foreign\"",
            "xmlns:a=\"http://www.w3.org/XML/1998/namespace\"",
            "bad:flag=\"x\"",
        ] {
            let h = setup();
            h.add_file(
                "pom.xml",
                &POM.replacen("<project ", &format!("<project {binding} "), 1),
            );
            answer(&h, true);
            let mut state = State::default();
            assert!(
                state.import_at(&h.invocation(), "/m2").is_err(),
                "{binding}"
            );
            assert!(state.snapshots.is_empty());
        }
    }

    #[test]
    fn coherent_maven_prefixes_and_legal_xml_attributes_preserve_source_contract() {
        let legal = POM.replacen(
            "<project ",
            "<project xmlns:xml=\"http://www.w3.org/XML/1998/namespace\" xml:lang=\"en\" ",
            1,
        );
        assert_eq!(dependencies(&legal).unwrap().len(), 1);
        let prefixed = "<p:project xmlns:p=\"http://maven.apache.org/POM/4.0.0\"><p:dependencies><p:dependency><p:groupId>org.example</p:groupId><p:artifactId>config</p:artifactId><p:version>1.2.3</p:version></p:dependency></p:dependencies></p:project>";
        assert_eq!(dependencies(prefixed).unwrap().len(), 1);
        for text in [
            POM.replace("<dependencies>", "<dependencies xmlns=\"\">"),
            POM.replace(
                "<project ",
                "<p:q:project xmlns:p=\"http://maven.apache.org/POM/4.0.0\" ",
            )
            .replace("</project>", "</p:q:project>"),
            POM.replace("<project ", "<project a:b:c=\"x\" "),
        ] {
            assert!(dependencies(&text).is_err());
        }
    }
    #[test]
    fn public_hover_reports_actual_selected_origin_and_preserves_precedence_without_runtime_values()
    {
        let h = setup();
        answer(&h, true);
        let mut state = State::default();
        state.import_at(&h.invocation(), "/m2").unwrap();
        let mut tools = crate::BootTools {
            jars: state,
            ..Default::default()
        };
        let doc = |text: &str| Document {
            path: "/project/application.properties".into(),
            language: "properties".into(),
            version: 1,
            text: text.into(),
        };
        let bundled = tools
            .hover(
                &doc("server.port=4321"),
                openidea_plugin::Position::new(0, 3),
            )
            .unwrap()
            .markdown;
        assert!(
            bundled.contains("Metadata origin: bundled fallback snapshot")
                && bundled.contains("Spring Boot 3.5.16")
                && bundled.contains("not an inferred project dependency version")
        );
        assert!(!bundled.contains("4321"));
        let selected = tools
            .hover(
                &doc("custom.jar.feature=configured-sensitive"),
                openidea_plugin::Position::new(0, 4),
            )
            .unwrap()
            .markdown;
        assert!(
            selected.contains("Metadata origin: selected dependency JAR snapshot")
                && selected.contains("org.example:config:1.2.3")
                && selected.contains("SHA-256")
        );
        assert!(!selected.contains("configured-sensitive"));
        h.add_file("src/main/resources/META-INF/spring-configuration-metadata.json",r#"{"properties":[{"name":"custom.jar.feature","type":"java.lang.String","defaultValue":"declared-local"}]}"#);
        let current = doc("custom.jar.feature=configured-sensitive");
        tools.warm_live(&current);
        let local = tools
            .hover(&current, openidea_plugin::Position::new(0, 4))
            .unwrap()
            .markdown;
        assert!(
            local.contains("Metadata origin: project-local current/cached declarations")
                && local.contains("declared-local")
        );
        assert!(
            !local.contains("Selected dependency snapshot")
                && !local.contains("configured-sensitive")
        );
        assert!(h.requests().is_empty() && h.process_runs().is_empty());
    }

    #[test]
    fn gradle_cache_resolves_explicit_and_boot_managed_versions_and_degrades_when_denied() {
        const CACHE: &str = "/gradle";
        let h = MockHost::install().with_capabilities(&[
            "project.read",
            "fs.write:/gradle",
            "fs.read:/gradle",
            "editor.read",
        ]);
        h.add_file(
            "build.gradle",
            "plugins { id 'org.springframework.boot' version '4.0.0' }\ndependencies {\n implementation 'org.example:config:1.2.3'\n implementation 'org.springframework.boot:spring-boot-autoconfigure'\n api 'org.example:ranged:[1.0,2.0)'\n}",
        );
        let bom = "<project><groupId>org.springframework.boot</groupId><artifactId>spring-boot-dependencies</artifactId><version>4.0.0</version><dependencyManagement><dependencies><dependency><groupId>org.springframework.boot</groupId><artifactId>spring-boot-autoconfigure</artifactId><version>${project.version}</version></dependency></dependencies></dependencyManagement></project>";
        let bom_path = format!("{CACHE}/org.springframework.boot/spring-boot-dependencies/4.0.0/aaaa/spring-boot-dependencies-4.0.0.pom");
        h.add_external_file(&bom_path, bom);
        let jar = |hash: &str, group: &str, artifact: &str, version: &str, key: &str| {
            let json = format!(r#"{{"properties":[{{"name":"{key}","type":"java.lang.Boolean"}}]}}"#);
            let bytes = super::super::jar_archive::fixture(&[(
                "META-INF/spring-configuration-metadata.json",
                json.as_bytes(),
                8,
            )]);
            let path = format!("{CACHE}/{group}/{artifact}/{version}/{hash}/{artifact}-{version}.jar");
            h.add_external_file(&path, "");
            host::fs::write_bytes(&path, &bytes).unwrap();
        };
        jar("bbbb", "org.example", "config", "1.2.3", "gradle.explicit.feature");
        jar("cccc", "org.springframework.boot", "spring-boot-autoconfigure", "4.0.0", "gradle.managed.feature");
        let mut state = State::default();
        for expected in ["gradle.explicit.feature", "gradle.managed.feature"] {
            let index = if expected.contains("explicit") { "0" } else { "1" };
            h.answer_next_prompt(Some(&[index]));
            h.answer_next_prompt(Some(&["ok"]));
            state.import_at(&h.invocation(), CACHE).unwrap();
            let catalog = state.catalog("/project/application.properties");
            let lookup = spring_text::metadata::Lookup(vec![&catalog]);
            assert!(lookup.prop(expected).is_some(), "{expected}");
        }
        assert!(state.version_scoped("/project/application.properties"));

        let denied = MockHost::install().with_capabilities(&["project.read", "editor.read"]);
        denied.add_file("build.gradle", "dependencies { implementation 'org.example:config:1.2.3' }");
        assert!(State::default().import_at(&denied.invocation(), CACHE).unwrap().is_none());
    }

    fn boot_bom(h: &MockHost, v: &str, managed: &str) {
        let (g, a) = ("org.springframework.boot", "spring-boot-dependencies");
        h.add_external_file(
            &format!("/m2/org/springframework/boot/{a}/{v}/{a}-{v}.pom"),
            &format!("<project><groupId>{g}</groupId><artifactId>{a}</artifactId><version>{v}</version><properties><x.version>{v}</x.version></properties><dependencyManagement><dependencies>{managed}</dependencies></dependencyManagement></project>"),
        );
    }
    fn autoconfigure_jar(h: &MockHost, v: &str) {
        let path = format!("/m2/org/springframework/boot/spring-boot-autoconfigure/{v}/spring-boot-autoconfigure-{v}.jar");
        let json = r#"{"properties":[{"name":"managed.versionless.feature","type":"java.lang.Boolean"}]}"#;
        let bytes = super::super::jar_archive::fixture(&[(
            "META-INF/spring-configuration-metadata.json",
            json.as_bytes(),
            8,
        )]);
        h.add_external_file(&path, "");
        host::fs::write_bytes(&path, &bytes).unwrap();
    }
    const MANAGED: &str = "<dependency><groupId>org.springframework.boot</groupId><artifactId>spring-boot-autoconfigure</artifactId><version>${x.version}</version></dependency>";
    const VERSIONLESS: &str = "<dependencies><dependency><groupId>org.springframework.boot</groupId><artifactId>spring-boot-autoconfigure</artifactId></dependency></dependencies>";

    #[test]
    fn versionless_boot_dependency_is_resolved_from_starter_parent_in_fake_m2() {
        let h = setup();
        h.add_file("pom.xml", &format!("<project><parent><groupId>org.springframework.boot</groupId><artifactId>spring-boot-starter-parent</artifactId><version>4.0.0</version></parent>{VERSIONLESS}</project>"));
        h.add_external_file("/m2/org/springframework/boot/spring-boot-starter-parent/4.0.0/spring-boot-starter-parent-4.0.0.pom", "<project><parent><groupId>org.springframework.boot</groupId><artifactId>spring-boot-dependencies</artifactId><version>4.0.0</version></parent><groupId>org.springframework.boot</groupId><artifactId>spring-boot-starter-parent</artifactId><version>4.0.0</version></project>");
        boot_bom(&h, "4.0.0", MANAGED);
        autoconfigure_jar(&h, "4.0.0");
        let mut state = State::default();
        answer(&h, true);
        state.import_at(&h.invocation(), "/m2").unwrap();
        let catalog = state.catalog("/project/application.properties");
        assert!(spring_text::metadata::Lookup(vec![&catalog]).prop("managed.versionless.feature").is_some());
    }

    #[test]
    fn versionless_boot_dependency_is_resolved_from_imported_bom_with_pom_properties() {
        let h = setup();
        h.add_file("pom.xml", &format!("<project><dependencyManagement><dependencies><dependency><groupId>org.springframework.boot</groupId><artifactId>spring-boot-dependencies</artifactId><version>${{boot.v}}</version><type>pom</type><scope>import</scope></dependency></dependencies></dependencyManagement>{VERSIONLESS}<properties><boot.v>4.0.1</boot.v></properties></project>"));
        boot_bom(&h, "4.0.1", MANAGED);
        autoconfigure_jar(&h, "4.0.1");
        let mut state = State::default();
        answer(&h, true);
        state.import_at(&h.invocation(), "/m2").unwrap();
        let catalog = state.catalog("/project/application.properties");
        assert!(spring_text::metadata::Lookup(vec![&catalog]).prop("managed.versionless.feature").is_some());
    }

    #[test]
    fn unresolved_snapshot_and_unmanaged_versionless_dependencies_are_dropped() {
        let h = setup();
        h.add_file("pom.xml", &format!("<project><dependencyManagement><dependencies><dependency><groupId>org.springframework.boot</groupId><artifactId>spring-boot-dependencies</artifactId><version>4.0.2-SNAPSHOT</version><type>pom</type><scope>import</scope></dependency></dependencies></dependencyManagement>{VERSIONLESS}</project>"));
        boot_bom(&h, "4.0.2-SNAPSHOT", MANAGED);
        let mut state = State::default();
        let error = state.import_at(&h.invocation(), "/m2").unwrap_err();
        assert!(format!("{error:?}").contains("Found no dependency with a known version"));
    }

    #[test]
    fn more_than_thirty_two_imported_boms_are_refused() {
        let h = setup();
        let imports: String = (0..33)
            .map(|i| format!("<dependency><groupId>org.springframework.boot</groupId><artifactId>spring-boot-dependencies</artifactId><version>4.0.{i}</version><type>pom</type><scope>import</scope></dependency>"))
            .collect();
        let pom = format!("<project><dependencyManagement><dependencies>{imports}</dependencies></dependencyManagement>{VERSIONLESS}</project>");
        for i in 0..33 {
            boot_bom(&h, &format!("4.0.{i}"), MANAGED);
        }
        let error = versions::cache_catalog(&pom, "/m2").unwrap_err();
        assert!(format!("{error:?}").contains("32 imported"));
    }

    #[test]
    fn uncached_imports_do_not_count_against_the_bound() {
        let h = setup();
        let imports: String = (0..33)
            .map(|i| format!("<dependency><groupId>org.springframework.boot</groupId><artifactId>spring-boot-dependencies</artifactId><version>4.0.{i}</version><type>pom</type><scope>import</scope></dependency>"))
            .collect();
        let pom = format!("<project><dependencyManagement><dependencies>{imports}</dependencies></dependencyManagement>{VERSIONLESS}</project>");
        h.add_external_file("/m2/placeholder", "");
        assert!(versions::cache_catalog(&pom, "/m2").is_ok());
    }

    #[test]
    fn cloud_release_train_imports_accepted() {
        let h = setup();
        let imports: String = (0..15)
            .map(|i| format!("<dependency><groupId>org.springframework.cloud</groupId><artifactId>spring-cloud-part{i}-dependencies</artifactId><version>4.3.0</version><type>pom</type><scope>import</scope></dependency>"))
            .collect();
        let train = format!("<project><groupId>org.springframework.cloud</groupId><artifactId>spring-cloud-dependencies</artifactId><version>2025.1.0</version><dependencyManagement><dependencies>{imports}</dependencies></dependencyManagement></project>");
        h.add_external_file("/m2/org/springframework/cloud/spring-cloud-dependencies/2025.1.0/spring-cloud-dependencies-2025.1.0.pom", &train);
        for i in 0..15 {
            h.add_external_file(
                &format!("/m2/org/springframework/cloud/spring-cloud-part{i}-dependencies/4.3.0/spring-cloud-part{i}-dependencies-4.3.0.pom"),
                &format!("<project><groupId>org.springframework.cloud</groupId><artifactId>spring-cloud-part{i}-dependencies</artifactId><version>4.3.0</version></project>"),
            );
        }
        let pom = "<project><dependencyManagement><dependencies><dependency><groupId>org.springframework.cloud</groupId><artifactId>spring-cloud-dependencies</artifactId><version>2025.1.0</version><type>pom</type><scope>import</scope></dependency></dependencies></dependencyManagement></project>";
        let (_, sources) = versions::cache_catalog(pom, "/m2").unwrap();
        assert_eq!(sources.len(), 16);
    }

    #[test]
    fn uncached_bom_skipped_on_invalid_no_such_file() {
        let missing = |text: &str| versions::present::<()>(Err(Error::Invalid(text.into())));
        assert_eq!(missing("/m2/x.pom: no such file").unwrap(), None);
        assert_eq!(missing("/m2/x: no such directory").unwrap(), None);
        assert!(missing("/m2/x.pom: is a directory").is_err());
        let h = setup();
        h.add_external_file("/m2/placeholder", "");
        let pom = "<project><dependencyManagement><dependencies><dependency><groupId>org.springframework.boot</groupId><artifactId>spring-boot-dependencies</artifactId><version>9.9.9</version><type>pom</type><scope>import</scope></dependency></dependencies></dependencyManagement></project>";
        let (_, sources) = versions::cache_catalog(pom, "/m2").unwrap();
        assert!(sources.is_empty());
    }

    #[test]
    fn uncached_gradle_coordinate_is_absent_on_invalid_no_such_directory() {
        let h = MockHost::install().with_capabilities(&["project.read", "fs.read:/gradle"]);
        h.add_external_file("/gradle/placeholder", "");
        let dep = Dependency {
            group: "org.example".into(),
            artifact: "gone".into(),
            version: "1.0".into(),
            scope: "compile".into(),
        };
        assert_eq!(gradle::file("/gradle", &dep, ".jar").unwrap(), None);
    }

    #[test]
    fn catalog_survives_past_600s() {
        let h = setup();
        answer(&h, true);
        let mut state = State::default();
        state.import_at(&h.invocation(), "/m2").unwrap();
        let path = "/project/application.properties";
        assert!(state.provenance(path).is_some());
        assert!(!spring_text::metadata::Lookup(vec![&state.catalog(path)]).all().next().is_none());
    }

    #[test]
    fn opening_pom_keeps_import() {
        let h = setup();
        answer(&h, true);
        let mut state = State::default();
        state.import_at(&h.invocation(), "/m2").unwrap();
        for _ in 0..2 {
            state.source_changed("/project/pom.xml", false);
        }
        assert!(state.provenance("/project/application.properties").is_some());
        h.add_file("pom.xml", &POM.replace("1.2.3", "1.2.4"));
        state.source_changed("/project/pom.xml", false);
        assert!(state.provenance("/project/application.properties").is_none());
    }

    #[test]
    fn gradle_cache_denied_message() {
        let denied = MockHost::install().with_capabilities(&["project.read", "editor.read"]);
        denied.add_file("build.gradle", "dependencies { implementation 'org.example:config:1.2.3' }");
        assert!(State::default().import_at(&denied.invocation(), "/gradle").unwrap().is_none());
        let notes = denied.notifications();
        assert!(notes.iter().any(|n| n.body == "Reading Gradle JAR metadata needs the fs.read:~/.gradle/caches/modules-2/files-2.1 permission. Allow it in Settings › Plugins › Spring Boot Tools."));
    }
}
