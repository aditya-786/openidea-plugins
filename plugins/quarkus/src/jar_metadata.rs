//! Private copy of Boot's scoped Maven selection/retention workflow; Quarkus model parser below.
//! Explicit literal root-POM Maven-cache metadata snapshots, never classpath discovery.
use openidea_plugin::{Error, FileKind, Invocation, PickItem, host};
use sha2::{Digest, Sha256};
use spring_text::{
    config::canonical,
    metadata::{Catalog, Meta},
    xml,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};
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
fn pom_namespace<R>(
    reader: &quick_xml::reader::NsReader<R>,
    element: &quick_xml::events::BytesStart<'_>,
) -> Result<String, Error> {
    use quick_xml::name::ResolveResult;
    if !crate::qute::namespace_valid(reader, element) {
        return Err(invalid("Invalid POM namespace binding."));
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
    if text.len() > 256 * 1024 || text.contains("<!") {
        return Err(invalid(
            "The root pom.xml is over 256 KiB or contains comments, DTDs or entities, which this command can't read.",
        ));
    }
    let mut reader = quick_xml::reader::NsReader::from_str(text);
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
            if !literal(value) || fields.insert(child.local(), value).is_some() {
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
        let (Some(group), Some(artifact), Some(version)) = (
            fields.get("groupId"),
            fields.get("artifactId"),
            fields.get("version"),
        ) else {
            continue;
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
            version: (*version).into(),
            scope: scope.into(),
        };
        if !keys.insert(format!("{}:{}", group, artifact)) {
            return Err(invalid("The root pom.xml lists the same dependency twice. Remove the duplicate and try again."));
        }
        result.push(dep);
        if result.len() > 128 {
            return Err(invalid("Direct dependency limit exceeded."));
        }
    }
    Ok(result)
}
struct Snapshot {
    at: Instant,
    pom: String,
    pom_digest: String,
    jar: String,
    hash: String,
    dep: Dependency,
    meta: Meta,
    entries: Vec<String>,
}
#[derive(Default)]
pub struct State {
    snapshots: BTreeMap<String, Snapshot>,
}
impl State {
    pub fn invalidate(&mut self, path: &str) {
        self.snapshots.retain(|_, s| s.pom != path);
    }
    pub fn catalog(&self, path: &str) -> Catalog {
        let mut c = Catalog::default();
        if let Some(root) = host::project_of(path).and_then(|p| p.root().ok()) {
            if let Some(s) = self
                .snapshots
                .get(&root)
                .filter(|s| s.at.elapsed() < Duration::from_secs(600))
            {
                c.add(&s.meta);
            }
        }
        c
    }
    pub fn provenance(&self, path: &str) -> Option<String> {
        let root = host::project_of(path)?.root().ok()?;
        let s = self
            .snapshots
            .get(&root)
            .filter(|s| s.at.elapsed() < Duration::from_secs(600))?;
        Some(format!(
            "Loaded from Maven dependency {} ({} scope), {} · SHA-256 {} · files {}. Read from the dependency declared in pom.xml; the resolved classpath isn't checked.",
            s.dep.gav(),
            s.dep.scope,
            s.jar,
            s.hash,
            s.entries.join(", ")
        ))
    }
    pub fn import(&mut self, ctx: &Invocation) -> Result<Option<String>, Error> {
        self.import_from(ctx, CACHE)
    }
    fn import_from(&mut self, ctx: &Invocation, cache: &str) -> Result<Option<String>, Error> {
        let result = self.import_at(ctx, cache);
        if matches!(result, Err(Error::Denied(_))) {
            host::notify(
                openidea_plugin::NotificationLevel::Warning,
                "Quarkus JAR metadata",
                &ui_kit::denied::message("Reading Maven JAR metadata", "fs.read:~/.m2/repository", "Quarkus Tools"),
            );
            return Ok(None);
        }
        if result.is_err()
            && ctx
                .project()
                .and_then(|p| p.root().ok())
                .is_some_and(|root| {
                    self.snapshots
                        .get(&root)
                        .is_some_and(|s| s.at.elapsed() < Duration::from_secs(600))
                })
        {
            host::notify(
                openidea_plugin::NotificationLevel::Warning,
                "Quarkus JAR metadata",
                "Read failed; the previously loaded config keys are still used.",
            );
        }
        result
    }
    fn import_at(&mut self, ctx: &Invocation, cache: &str) -> Result<Option<String>, Error> {
        let project = ctx
            .project()
            .ok_or("Open the Maven project first.")?;
        let root = project.root()?;
        let pom = format!("{root}/pom.xml");
        if root.len() > 4096 || root.chars().any(char::is_control) {
            return Err(invalid("The project path is too long or contains control characters."));
        }
        let source = project.read_file(&pom)?;
        let pom_digest = format!("{:x}", Sha256::digest(source.as_bytes()));
        if self
            .snapshots
            .get(&root)
            .is_some_and(|snapshot| snapshot.pom_digest != pom_digest)
        {
            self.snapshots.remove(&root);
        }
        let dependencies = dependencies(&source)?;
        if dependencies.is_empty() {
            return Err(invalid(
                "The root pom.xml lists no JAR dependency with written-out coordinates. Dependencies managed by a BOM or using ${…} properties aren't listed.",
            ));
        }
        let choices: Vec<_> = dependencies
            .iter()
            .enumerate()
            .map(|(i, d)| PickItem::new(&i.to_string(), &format!("{} ({})", d.gav(), d.scope)))
            .collect();
        let Some(key) = host::pick("Read metadata from one declared Maven dependency", &choices)?
        else {
            return Ok(None);
        };
        let selected = key
            .parse::<usize>()
            .ok()
            .and_then(|i| dependencies.get(i))
            .ok_or("Unknown dependency selection.")?
            .clone();
        let jar = selected.path(cache);
        let grant_probe = host::fs::stat(&jar)?;
        if grant_probe.kind != FileKind::File
            || grant_probe.size > super::jar_archive::MAX_ARCHIVE as u64
        {
            return Err(invalid("Selected JAR must be a file of at most 8 MiB."));
        }
        let review = format!(
            "Read local dependency JAR metadata?\n\nProject: {root}\nPOM: {pom}\nDependency: {}\nScope: {}\nJAR: {jar}\n\nReads only META-INF/quarkus-config-doc/quarkus-config-model.json and META-INF/quarkus-config-doc/quarkus-config-javadoc.json from this JAR. Nothing is extracted, run, built or downloaded. Keys come from the dependency declared in pom.xml, not from the resolved classpath. Replaces the keys loaded earlier for this project only if reading succeeds.",
            selected.gav(),
            selected.scope
        );
        if !ui_kit::prompts::confirm_exact("Read selected Maven JAR metadata", &review)? {
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
                return Err(invalid("Project/POM changed. Select the dependency again."));
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
        self.snapshots
            .retain(|_, snapshot| snapshot.at.elapsed() < Duration::from_secs(600));
        if !self.snapshots.contains_key(&root) && self.snapshots.len() >= 8 {
            return Err(invalid(
                "Config keys from JARs are already loaded for 8 projects. Try again in a few minutes: loaded keys expire after 10 minutes.",
            ));
        }
        self.snapshots.insert(
            root,
            Snapshot {
                at: Instant::now(),
                pom,
                pom_digest,
                jar,
                hash: format!("{:x}", Sha256::digest(&bytes)),
                dep: selected,
                meta,
                entries,
            },
        );
        host::notify(
            openidea_plugin::NotificationLevel::Info,
            "Quarkus JAR metadata",
            &format!(
                "Loaded {count} config keys from the selected Maven dependency."
            ),
        );
        Ok(None)
    }
}
fn text(value: Option<&serde_json::Value>, limit: usize) -> Result<String, Error> {
    let Some(value) = value else {
        return Ok(String::new());
    };
    let value = match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Null => String::new(),
        _ => return Err(invalid("Unsupported structured configuration value.")),
    };
    if value.len() > limit || value.chars().any(char::is_control) {
        return Err(invalid("Oversized or unsupported metadata text."));
    }
    Ok(value)
}
fn parse_entries(entries: &[(String, String)]) -> Result<Meta, Error> {
    use serde_json::Value;
    use spring_text::metadata::{Deprecation, Prop};
    const MODEL: &str = "META-INF/quarkus-config-doc/quarkus-config-model.json";
    const DOCS: &str = "META-INF/quarkus-config-doc/quarkus-config-javadoc.json";
    if entries.is_empty()
        || entries.len() > 2
        || entries
            .iter()
            .any(|(name, text)| !matches!(name.as_str(), MODEL | DOCS) || text.len() > 512 * 1024)
        || entries.iter().map(|(_, text)| text.len()).sum::<usize>() > 768 * 1024
    {
        return Err(invalid("The JAR's config metadata is too large (over 768 KiB) or has unexpected files."));
    }
    let model = entries
        .iter()
        .find(|(name, _)| name == MODEL)
        .ok_or("No supported Quarkus config model in the selected JAR.")?;
    let model: Value = serde_json::from_str(&model.1)
        .map_err(|_| invalid("Invalid Quarkus configuration model JSON."))?;
    let roots = model
        .get("configRoots")
        .and_then(Value::as_array)
        .ok_or("Unsupported Quarkus config model layout.")?;
    if roots.len() > 128 {
        return Err(invalid("Configuration root limit exceeded."));
    }
    let docs: Value = if let Some((_, docs)) = entries.iter().find(|(name, _)| name == DOCS) {
        serde_json::from_str(docs).map_err(|_| invalid("Invalid Quarkus Javadoc JSON."))?
    } else {
        Value::Null
    };
    if !docs.is_null() && docs.get("elements").and_then(Value::as_object).is_none() {
        return Err(invalid("Unsupported Quarkus Javadoc layout."));
    }
    let mut stack = vec![];
    for root in roots {
        let items = root
            .get("items")
            .and_then(Value::as_array)
            .ok_or("Unsupported config root items.")?;
        stack.extend(items.iter().map(|item| (item, 0usize)));
    }
    let mut visited = 0;
    let mut names = BTreeSet::new();
    let mut result = Meta::default();
    while let Some((item, depth)) = stack.pop() {
        visited += 1;
        if visited > 4096 || depth > 16 {
            return Err(invalid(
                "Configuration model nesting/record limit exceeded.",
            ));
        }
        let class = item
            .get("@class")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let property = class == "ConfigProperty" || class.ends_with(".ConfigProperty");
        let section = class == "ConfigSection" || class.ends_with(".ConfigSection");
        if section {
            if let Some(items) = item.get("items").and_then(Value::as_array) {
                stack.extend(items.iter().map(|item| (item, depth + 1)));
            }
            continue;
        }
        if !property {
            continue;
        } // Unknown model revisions/class lists are not configuration keys.
        let name = item
            .get("path")
            .and_then(|v| v.get("property"))
            .and_then(Value::as_str)
            .ok_or("A property in the JAR's config model has no name.")?;
        if item.get("map").is_some_and(|value| !value.is_boolean()) {
            return Err(invalid("Unsupported map flag."));
        }
        if item.get("map").and_then(Value::as_bool) == Some(true)
            || name.contains(['\"', '{', '}', '[', ']'])
        {
            continue;
        }
        if name.len() > 256
            || canonical(name).is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
        {
            return Err(invalid("Unsupported property spelling."));
        }
        if !names.insert(canonical(name)) {
            return Err(invalid(
                "Ambiguous duplicate canonical configuration properties.",
            ));
        }
        let ty = text(
            item.get("typeDescription").or_else(|| item.get("type")),
            256,
        )?;
        let ty = if ty
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'$'))
        {
            ty.rsplit('.').next().unwrap_or(&ty).to_string()
        } else {
            ty
        };
        let default = text(item.get("defaultValue"), 1024)?;
        let deprecated = match item.get("deprecation") {
            None | Some(Value::Null) => None,
            Some(d) if d.is_object() => Some(Deprecation {
                removed: false,
                replacement: text(d.get("replacement"), 256)?,
            }),
            _ => return Err(invalid("Unsupported deprecation record.")),
        };
        let source_type = text(item.get("sourceType"), 256)?;
        let source_element = text(item.get("sourceElementName"), 128)?;
        let doc = docs
            .get("elements")
            .and_then(|e| e.get(format!("{source_type}.{source_element}")))
            .and_then(|e| e.get("description"));
        let description = match doc {
            None => String::new(),
            Some(Value::String(s)) if s.len() <= 4096 => {
                s.split_whitespace().collect::<Vec<_>>().join(" ")
            }
            _ => return Err(invalid("Unsupported Javadoc description.")),
        };
        if description.chars().any(char::is_control) {
            return Err(invalid("Unsupported Javadoc control characters."));
        }
        let mut escaped = String::new();
        for c in description.chars().take(512) {
            if matches!(c, '\\' | '[' | ']' | '!' | '(' | ')' | '*' | '_' | '|') {
                escaped.push('\\');
            }
            escaped.push(c);
        }
        let description = escaped;
        // Raw documentation is retained only as bounded text; provider markdown escapes it.
        result.props.push(Prop {
            name: name.into(),
            ty,
            default,
            deprecated,
            description,
        });
        if let Some(values) = item.get("enumAcceptedValues").and_then(|e| e.get("values")) {
            let values = values.as_object().ok_or("Unsupported enum hint layout.")?;
            if values.len() > 128 {
                return Err(invalid("Enum hint limit exceeded."));
            }
            let mut accepted = vec![];
            for value in values.values() {
                if let Some(value) = value.get("configValue") {
                    accepted.push(text(Some(value), 1024)?);
                }
            }
            if !accepted.is_empty() {
                result.hints.push((name.into(), accepted));
            }
        }
    }
    if result.props.is_empty() {
        return Err(invalid(
            "The JAR's config model lists no supported config properties.",
        ));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::hover::HoverProvider;
    use openidea_plugin::mock::MockHost;
    use openidea_plugin::providers::Document;
    const MODEL: &str = "META-INF/quarkus-config-doc/quarkus-config-model.json";
    const DOCS: &str = "META-INF/quarkus-config-doc/quarkus-config-javadoc.json";
    const POM: &str = "<project xmlns='http://maven.apache.org/POM/4.0.0'><dependencies><dependency><groupId>io.quarkus</groupId><artifactId>custom-extension</artifactId><version>1.2.3</version></dependency></dependencies></project>";
    const JSON: &str = r#"{"configRoots":[{"items":[{"@class":"io.quarkus.annotation.processor.documentation.config.model.ConfigProperty","path":{"property":"custom.extension.enabled"},"type":"java.lang.Boolean","defaultValue":true,"sourceType":"org.example.Config","sourceElementName":"enabled","enumAcceptedValues":{"values":{"ON":{"configValue":"enabled"},"OFF":{"configValue":"disabled"}}}}]}]}"#;
    const PATH: &str = "/m2/io/quarkus/custom-extension/1.2.3/custom-extension-1.2.3.jar";
    fn setup() -> MockHost {
        let h = MockHost::install().with_capabilities(&[
            "project.read",
            "events",
            "fs.write:/m2",
            "fs.read:/m2",
        ]);
        h.add_file("pom.xml", POM);
        h.add_external_file(PATH, "fixture");
        host::fs::write_bytes(
            PATH,
            &super::super::jar_archive::fixture(&[(MODEL, JSON.as_bytes(), 8)]),
        )
        .unwrap();
        h
    }
    fn answers(h: &MockHost, yes: bool) {
        h.answer_next_prompt(Some(&["0"]));
        h.answer_next_prompt(if yes { Some(&["ok"]) } else { None });
    }
    #[test]
    fn actual_model_fields_produce_keys_and_static_hints_not_extension_or_class_lists() {
        let meta = parse_entries(&[(MODEL.into(), JSON.into())]).unwrap();
        assert_eq!(meta.props[0].name, "custom.extension.enabled");
        assert_eq!(meta.props[0].ty, "Boolean");
        assert_eq!(meta.props[0].default, "true");
        assert_eq!(meta.hints[0].1, ["disabled", "enabled"]);
        for text in [
            r#"{"deployment-artifact":"io.quarkus:deployment:1"}"#,
            r#"{"configRoots":["org.example.Root"]}"#,
            r#"{"configRoots":[{"items":[{"@class":"UnknownFutureType","name":"invented.key"}]}]}"#,
        ] {
            assert!(parse_entries(&[(MODEL.into(), text.into())]).is_err());
        }
        assert!(
            super::super::jar_archive::metadata(&super::super::jar_archive::fixture(&[
                (
                    "META-INF/quarkus-extension.properties",
                    b"deployment-artifact=io.quarkus:deployment:1",
                    0
                ),
                ("META-INF/quarkus-config-roots.list", b"org.example.Root", 0)
            ]))
            .is_err()
        );
    }
    #[test]
    fn source_doc_association_is_bounded_literal_text_not_markup_execution() {
        let docs = r#"{"elements":{"org.example.Config.enabled":{"description":"First line\n![image](https://example.invalid/image)"}}}"#;
        let meta =
            parse_entries(&[(MODEL.into(), JSON.into()), (DOCS.into(), docs.into())]).unwrap();
        assert!(
            meta.props[0].description.contains("First line ")
                && meta.props[0].description.contains("\\!\\[image\\]")
        );
        let other = r#"{"elements":{"other.Class.enabled":{"description":"Wrong owner"}}}"#;
        assert!(
            parse_entries(&[(MODEL.into(), JSON.into()), (DOCS.into(), other.into())])
                .unwrap()
                .props[0]
                .description
                .is_empty()
        );
    }
    #[test]
    fn dynamic_named_paths_maps_duplicate_owners_and_unknown_layouts_do_not_create_keys() {
        for json in [
            JSON.replace(
                "custom.extension.enabled",
                "custom.extension.\\\"named\\\".enabled",
            ),
            JSON.replace("\"type\"", "\"map\":true,\"type\""),
        ] {
            assert!(parse_entries(&[(MODEL.into(), json)]).is_err());
        }
        let mut value: serde_json::Value = serde_json::from_str(JSON).unwrap();
        let first = value["configRoots"][0]["items"][0].clone();
        value["configRoots"][0]["items"]
            .as_array_mut()
            .unwrap()
            .push(first);
        assert!(parse_entries(&[(MODEL.into(), value.to_string())]).is_err());
        assert!(parse_entries(&[(DOCS.into(), "{\"elements\":{}}".into())]).is_err());
        assert!(parse_entries(&[(MODEL.into(), "x".repeat(512 * 1024 + 1))]).is_err());
    }
    #[test]
    fn explicit_import_has_root_owned_hover_provenance_without_network_or_processes() {
        let h = setup();
        answers(&h, true);
        let mut state = State::default();
        state.import_at(&h.invocation(), "/m2").unwrap();
        let catalog = state.catalog("/project/application.properties");
        assert!(
            spring_text::metadata::Lookup(vec![&catalog])
                .prop("custom.extension.enabled")
                .is_some()
        );
        assert!(
            state
                .provenance("/another/application.properties")
                .is_none()
        );
        let mut plugin = crate::QuarkusTools {
            jars: state,
            ..Default::default()
        };
        let doc = Document {
            path: "/project/application.properties".into(),
            language: "properties".into(),
            text: "custom.extension.enabled=true".into(),
            version: 1,
        };
        plugin.warm_live(
            &doc.path,
            Some(&doc.text),
            Instant::now() + Duration::from_secs(1),
        );
        let hover = plugin
            .hover(&doc, openidea_plugin::Position::new(0, 3))
            .unwrap();
        assert!(
            hover.markdown.contains("SHA-256")
                && hover.markdown.contains("resolved classpath isn't checked")
        );
        assert!(h.requests().is_empty() && h.process_runs().is_empty());
    }
    #[test]
    fn hover_labels_winning_source_catalog_and_packaged_versions_not_runtime_values() {
        let h = setup();
        answers(&h, true);
        let mut state = State::default();
        state.import_at(&h.invocation(), "/m2").unwrap();
        let mut plugin = crate::QuarkusTools { jars: state, ..Default::default() };
        let doc = |text: &str| Document { path: "/project/application.properties".into(), language: "properties".into(), text: text.into(), version: 1 };
        let selected = doc("custom.extension.enabled=configured-sensitive");
        plugin.warm_live(&selected.path, Some(&selected.text), Instant::now()+Duration::from_secs(1));
        let selected_hover = plugin.hover(&selected, openidea_plugin::Position::new(0,3)).unwrap().markdown;
        assert!(selected_hover.contains("Metadata origin: selected dependency JAR snapshot"));
        assert!(selected_hover.contains("SHA-256") && selected_hover.contains("io.quarkus:custom-extension:1.2.3"));
        assert!(!selected_hover.contains("configured-sensitive"));
        let bundled = doc("quarkus.http.port=4321");
        let bundled_hover = plugin.hover(&bundled, openidea_plugin::Position::new(0,3)).unwrap().markdown;
        assert!(bundled_hover.contains("Metadata origin: bundled fallback snapshot") && bundled_hover.contains("Quarkus 3.40.1"));
        assert!(bundled_hover.contains("Quarkus 3.40.1 configuration metadata") && bundled_hover.contains("not an inferred project dependency version"));
        assert!(!bundled_hover.contains("4321"));
        let local_source = "package org.example;\nimport org.eclipse.microprofile.config.inject.ConfigProperty;\nclass Config { @ConfigProperty(name=\"custom.extension.enabled\",defaultValue=\"false\") boolean enabled; }";
        h.add_file("src/main/java/org/example/Config.java", local_source);
        plugin.warm_live("/project/src/main/java/org/example/Config.java", Some(local_source), Instant::now()+Duration::from_secs(1));
        let local_hover = plugin.hover(&selected, openidea_plugin::Position::new(0,3)).unwrap().markdown;
        assert!(local_hover.contains("Metadata origin: project-source current/cached declarations") && local_hover.contains("Declared default: `false`"));
        assert!(!local_hover.contains("Selected Maven") && !local_hover.contains("configured-sensitive"));
        assert!(h.requests().is_empty() && h.process_runs().is_empty());
    }
    #[test]
    fn cancellation_denial_corrupt_replacement_and_expiry_preserve_scope() {
        let h = setup();
        let mut state = State::default();
        answers(&h, true);
        state.import_at(&h.invocation(), "/m2").unwrap();
        let previous = state.provenance("/project/application.properties").unwrap();
        answers(&h, false);
        state.import_at(&h.invocation(), "/m2").unwrap();
        assert_eq!(
            state.provenance("/project/application.properties").unwrap(),
            previous
        );
        host::fs::write_bytes(PATH, b"bad archive").unwrap();
        answers(&h, true);
        assert!(state.import_at(&h.invocation(), "/m2").is_err());
        assert_eq!(
            state.provenance("/project/application.properties").unwrap(),
            previous
        );
        let h = h.with_capabilities(&["project.read"]);
        h.answer_next_prompt(Some(&["0"]));
        assert!(state.import_at(&h.invocation(), "/m2").is_err());
        state.invalidate("/other/pom.xml");
        assert!(
            state
                .provenance("/project/application.properties")
                .is_some()
        );
        state.snapshots.get_mut("/project").unwrap().at = Instant::now() - Duration::from_secs(600);
        assert!(
            state
                .provenance("/project/application.properties")
                .is_none()
        );
    }
    #[test]
    fn root_pom_selection_is_literal_and_duplicate_or_unowned_sources_are_refused() {
        assert_eq!(
            dependencies(POM).unwrap()[0].path(CACHE),
            "~/.m2/repository/io/quarkus/custom-extension/1.2.3/custom-extension-1.2.3.jar"
        );
        assert!(
            dependencies(&POM.replace("1.2.3", "${quarkus.version}"))
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
        assert!(
            dependencies(&POM.replace("1.2.3", "../escape"))
                .unwrap()
                .is_empty()
        );
        assert!(dependencies(&POM.replace("</project>", "")).is_err());
    }
    #[test]
    fn literal_sections_work_but_unknown_and_excessive_models_do_not_expand_scope() {
        let mut record: serde_json::Value = serde_json::from_str::<serde_json::Value>(JSON)
            .unwrap()["configRoots"][0]["items"][0]
            .clone();
        let model = |record: serde_json::Value| {
            serde_json::json!({"configRoots":[{"items":[record]}]}).to_string()
        };
        let section =
            serde_json::json!({"@class":"io.quarkus.ConfigSection","items":[record.clone()]});
        assert_eq!(
            parse_entries(&[(MODEL.into(), model(section))])
                .unwrap()
                .props
                .len(),
            1
        );
        for _ in 0..18 {
            record = serde_json::json!({"@class":"io.quarkus.ConfigSection","items":[record]});
        }
        assert!(parse_entries(&[(MODEL.into(), model(record))]).is_err());
        let mut record: serde_json::Value = serde_json::from_str::<serde_json::Value>(JSON)
            .unwrap()["configRoots"][0]["items"][0]
            .clone();
        record["type"] = serde_json::json!("java.util.Optional<java.lang.Boolean>");
        assert_eq!(
            parse_entries(&[(MODEL.into(), model(record))])
                .unwrap()
                .props[0]
                .ty,
            "java.util.Optional<java.lang.Boolean>"
        );
    }
    #[test]
    fn explicit_observed_pom_changes_do_not_reuse_last_good_from_another_source_scope() {
        let h = setup();
        let mut state = State::default();
        answers(&h, true);
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
    fn malformed_reserved_or_unbound_pom_cannot_authorize_metadata_import() {
        for binding in [
            "xmlns:xml=\"urn:foreign\"",
            "xmlns:xmlns=\"urn:foreign\"",
            "xmlns:a=\"http://www.w3.org/XML/1998/namespace\"",
            "bad:flag=\"x\"",
        ] {
            let host = setup();
            host.add_file(
                "pom.xml",
                &POM.replacen("<project ", &format!("<project {binding} "), 1),
            );
            answers(&host, true);
            let mut state = State::default();
            assert!(
                state.import_at(&host.invocation(), "/m2").is_err(),
                "{binding}"
            );
            assert!(state.snapshots.is_empty());
        }
    }

    #[test]
    fn legal_namespace_prefixes_and_xml_attributes_keep_literal_dependency_contract() {
        let legal = POM.replacen(
            "<project ",
            "<project xmlns:xml=\"http://www.w3.org/XML/1998/namespace\" xml:lang=\"en\" ",
            1,
        );
        assert_eq!(dependencies(&legal).unwrap().len(), 1);
        let prefixed = "<p:project xmlns:p=\"http://maven.apache.org/POM/4.0.0\"><p:dependencies><p:dependency><p:groupId>io.quarkus</p:groupId><p:artifactId>custom-extension</p:artifactId><p:version>1.2.3</p:version></p:dependency></p:dependencies></p:project>";
        assert_eq!(dependencies(prefixed).unwrap().len(), 1);
        let mixed = POM.replace("<dependencies>", "<dependencies xmlns=\"\">");
        assert!(dependencies(&mixed).is_err());
    }
    #[test]
    fn m2_denied_message() {
        let h = setup().with_capabilities(&["project.read"]);
        h.answer_next_prompt(Some(&["0"]));
        let mut state = State::default();
        assert!(state.import_from(&h.invocation(), "/m2").unwrap().is_none());
        assert!(h.notifications().iter().any(|n| n.body == "Reading Maven JAR metadata needs the fs.read:~/.m2/repository permission. Allow it in Settings › Plugins › Quarkus Tools."));
    }
}
