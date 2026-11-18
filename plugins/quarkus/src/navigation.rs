//! Read-only configuration navigation; no synthetic or transformed Rename anchors.
use super::*;
use openidea_plugin::Location;
use spring_text::java as source;
pub(crate) const CONFIG_MARKER: &str = "icons/quarkus.svg";
#[derive(Clone)]
pub(crate) struct Target {
    pub key: String,
    pub range: Range,
}
pub(crate) fn literal_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 256
        && key.split('.').all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')))
}
pub(crate) fn config_targets(path: &str, text: &str) -> Vec<Target> {
    if !is_quarkus_config(path) || text.len() > live::MAX_TEXT {
        return vec![];
    }
    let keys = config::keys(path, text);
    if keys.len() > 512 {
        return vec![];
    }
    keys.iter()
        .filter(|k| !keys.iter().any(|child| child.key.len() > k.key.len() && child.key.starts_with(&format!("{}.", k.key))))
        .filter(|k| literal_key(&k.key) && k.at < k.end && k.end <= text.len())
        .map(|k| Target { key: k.key.clone(), range: range(text, k.at, k.end) })
        .collect()
}
pub(crate) fn java_targets(path: &str, text: &str) -> Vec<Target> {
    if !path.ends_with(".java") || text.len() > live::MAX_TEXT {
        return vec![];
    }
    let tokens = source::tokens(text);
    let mut stack = Vec::new();
    for t in &tokens {
        match t.s {
            "{" | "(" | "[" => {
                if stack.len() >= 128 {
                    return vec![];
                }
                stack.push(t.s);
            }
            "}" | ")" | "]" => {
                let expected = match t.s {
                    "}" => "{",
                    ")" => "(",
                    _ => "[",
                };
                if stack.pop() != Some(expected) {
                    return vec![];
                }
            }
            _ => {}
        }
    }
    if !stack.is_empty() {
        return vec![];
    }
    let file = source::parse(text);
    let qualified = "org.eclipse.microprofile.config.inject.ConfigProperty";
    if file.types.len() > 64
        || file.types.iter().any(|t| t.name == "ConfigProperty")
        || !file.imports.iter().any(|i| !i.is_static && i.path == qualified)
        || file.imports.iter().any(|i| !i.is_static && i.path.rsplit('.').next() == Some("ConfigProperty") && i.path != qualified)
    {
        return vec![];
    }
    // Only direct literal injection names. ConfigMapping method/group names are
    // derived, not source keys; their owner/path model is not established here.
    let mut targets = Vec::new();
    for t in &file.types {
        let generic =
            source::tokens(&text[t.at + t.name.len()..t.end]).iter().take_while(|t| t.s != "{").any(|t| t.s == "<" && t.at < t.end());
        if generic {
            continue;
        }
        for m in &t.members {
            if let Some(a) = m.annotation("ConfigProperty") {
                if let Some(target) = annotation_target(a, text) {
                    targets.push(target);
                }
            }
            for p in &m.params {
                if let Some(a) = p.annotation("ConfigProperty") {
                    if let Some(target) = annotation_target(a, text) {
                        targets.push(target);
                    }
                }
            }
        }
        for c in &t.components {
            if let Some(a) = c.annotation("ConfigProperty") {
                if let Some(target) = annotation_target(a, text) {
                    targets.push(target);
                }
            }
        }
    }
    if targets.len() > 512 { vec![] } else { targets }
}
fn annotation_target(a: &source::Annotation, text: &str) -> Option<Target> {
    let written: String = source::tokens(text.get(a.at + 1..a.name_at + a.name.len())?).iter().map(|t| t.s).collect();
    if written != "ConfigProperty" {
        return None;
    }
    let arg = a.arg("name")?;
    let [literal] = arg.strings.as_slice() else {
        return None;
    };
    if arg.text.trim() != format!("\"{}\"", literal.value)
        || literal.value.len() != literal.end - literal.at
        || !literal_key(&literal.value)
    {
        return None;
    }
    Some(Target { key: literal.value.clone(), range: range(text, literal.at, literal.end) })
}
/// Literal `prefix` of a `@ConfigMapping` owned by SmallRye: every key below it belongs to the mapping.
pub(crate) fn mapping_prefixes(path: &str, text: &str) -> Vec<Target> {
    if !path.ends_with(".java") || text.len() > live::MAX_TEXT || !text.contains("ConfigMapping") {
        return vec![];
    }
    let file = source::parse(text);
    let mut targets = Vec::new();
    for t in &file.types {
        let Some(a) = t.annotation("ConfigMapping").filter(|a| crate::java::annotation_owned(&file, text, a, "io.smallrye.config.ConfigMapping")) else {
            continue;
        };
        let Some(arg) = a.arg("prefix").or_else(|| a.arg("value")) else {
            continue;
        };
        let [literal] = arg.strings.as_slice() else {
            continue;
        };
        if arg.text.trim() == format!("\"{}\"", literal.value) && literal.value.len() == literal.end - literal.at && literal_key(&literal.value) {
            targets.push(Target { key: literal.value.clone(), range: range(text, literal.at, literal.end) });
        }
    }
    targets
}
const MAX_LINE_TARGETS: usize = 200;
/// Every exact target of the module's cached facts, `None` when they are incomplete, stale or over budget.
fn module_targets<T: Clone>(index: &spring_text::index::Index<T>, root: &str, module: &str, info: impl Fn(&T) -> (&[Target], bool)) -> Option<Vec<(String, Target)>> {
    if !index.fresh() || index.iter().take(513).count() > 512 || index.iter().any(|(_, i)| !info(i).1) {
        return None;
    }
    let mut found = Vec::new();
    let mut records = 0usize;
    for (path, i) in index.iter() {
        let (locations, _) = info(i);
        records += locations.len();
        if records > 4096 {
            return None;
        }
        if owned(root, path) && self::module(path).as_deref() == Some(module) {
            found.extend(locations.iter().map(|t| (path.to_owned(), t.clone())));
        }
    }
    Some(found)
}
impl QuarkusTools {
    /// The current document's literal keys and the other side's exact targets in the same module.
    fn source_links(&mut self, doc: &Document) -> Option<(bool, Vec<Target>, Vec<(String, Target)>)> {
        let java = doc.language == "java" && doc.path.ends_with(".java");
        let config = matches!(doc.language.as_str(), "properties" | "quarkus-properties" | "yaml") && is_quarkus_config(&doc.path);
        if !(java || config) || doc.path.split('/').any(|p| matches!(p, "." | "..")) {
            return None;
        }
        // Observe even unsupported/oversized current source before preflight:
        // a rejected new document must not leave its previous destination alive.
        let _ = self.sources.for_cached_document(&doc.path, &doc.text);
        let _ = self.configs.for_cached_document(&doc.path, &doc.text);
        let root = self.cached_module(doc)?;
        if !self.live.watching {
            return None;
        }
        let module = module(&doc.path)?;
        if java {
            let index = self.configs.for_cached_document(&doc.path, &doc.text)?;
            let targets = module_targets(index, &root, &module, |i| (&i.locations, i.complete))?;
            return Some((true, java_targets(&doc.path, &doc.text), targets));
        }
        let index = self.sources.for_cached_document(&doc.path, &doc.text)?;
        let targets = module_targets(index, &root, &module, |i| (&i.locations, i.complete))?;
        Some((false, config_targets(&doc.path, &doc.text), targets))
    }
    pub(crate) fn source_definition(&mut self, doc: &Document, at: Position) -> Vec<Location> {
        let Some((_, candidates, targets)) = self.source_links(doc) else {
            return vec![];
        };
        let Some(selected) = candidates.iter().find(|t| t.range.start <= at && at < t.range.end) else {
            return vec![];
        };
        targets.into_iter().filter(|(_, t)| t.key == selected.key).map(|(path, t)| Location { path: path.into(), range: t.range }).collect()
    }
    /// One marker per exact target on the key's line; the IDE merges a line's markers into one chooser.
    pub(crate) fn source_markers(&mut self, doc: &Document) -> Vec<LineMarker> {
        let Some((java, candidates, targets)) = self.source_links(doc) else {
            return vec![];
        };
        let tooltip = if java { "Config key declarations" } else { "Injection points" };
        let mut lines: BTreeMap<u32, Vec<Location>> = BTreeMap::new();
        for c in &candidates {
            let found = targets.iter().filter(|(_, t)| t.key == c.key);
            lines.entry(c.range.start.line).or_default().extend(found.map(|(path, t)| Location { path: path.into(), range: t.range }));
        }
        if java {
            for prefix in mapping_prefixes(&doc.path, &doc.text) {
                let below = format!("{}.", prefix.key);
                let found = targets.iter().filter(|(_, t)| t.key.starts_with(&below));
                lines.entry(prefix.range.start.line).or_default().extend(found.map(|(path, t)| Location { path: path.into(), range: t.range }));
            }
        }
        let mut markers = Vec::new();
        for (line, mut found) in lines {
            found.sort_by(|a, b| (&a.path, a.range.start).cmp(&(&b.path, b.range.start)));
            found.dedup();
            let tooltip = match found.len() {
                n if n > MAX_LINE_TARGETS => format!("{tooltip} (first {MAX_LINE_TARGETS} of {n})"),
                _ => tooltip.to_owned(),
            };
            for target in found.into_iter().take(MAX_LINE_TARGETS) {
                markers.push(LineMarker { line, icon: Some(CONFIG_MARKER.into()), tooltip: tooltip.clone(), target: Some(target) });
            }
        }
        markers
    }
}
fn owned(root: &str, path: &str) -> bool {
    crate::source_owned(root, path)
}

pub(crate) fn module(path: &str) -> Option<String> {
    ["/src/main/java/", "/src/main/resources/"].iter().find_map(|segment| path.split_once(segment).map(|(base, _)| base.to_owned()))
}
#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{Event, FilesChanged, mock::MockHost};
    const JAVA_PATH: &str = "/project/src/main/java/app/Greeting.java";
    const CONFIG_PATH: &str = "/project/src/main/resources/application.properties";
    const JAVA: &str = "package app;\nimport org.eclipse.microprofile.config.inject.ConfigProperty;\nclass Greeting {\n/* 😀 */ @ConfigProperty(name=\"greeting.message\") String message;\n}";
    fn doc(path: &str, text: &str) -> Document {
        Document {
            path: path.into(),
            language: if path.ends_with(".java") {
                "java"
            } else if path.ends_with(".yaml") {
                "yaml"
            } else {
                "properties"
            }
            .into(),
            version: 1,
            text: text.into(),
        }
    }
    fn at(d: &Document, needle: &str) -> Position {
        range_point(&d.text, d.text.find(needle).unwrap())
    }
    fn setup() -> (MockHost, QuarkusTools) {
        let h = MockHost::install();
        h.add_file("pom.xml", "<project><dependencies><dependency><groupId>io.quarkus</groupId><artifactId>quarkus-rest</artifactId></dependency></dependencies></project>");
        h.add_file(JAVA_PATH, JAVA);
        h.add_file(CONFIG_PATH, "greeting.message=hello\n");
        let mut p = QuarkusTools::default();
        p.activate_live();
        (h, p)
    }
    #[test]
    fn exact_source_navigation_is_bidirectional_with_utf16_tokens() {
        let (_h, mut p) = setup();
        let j = doc(JAVA_PATH, JAVA);
        let c = doc(CONFIG_PATH, "greeting.message=hello\n");
        let found = p.definition(&c, at(&c, "greeting.message"));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, JAVA_PATH);
        let byte = JAVA.find("greeting.message").unwrap();
        assert_eq!(found[0].range, range(JAVA, byte, byte + 16));
        let back = p.definition(&j, at(&j, "greeting.message"));
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].path, CONFIG_PATH);
        assert_eq!(back[0].range, range(&c.text, 0, 16));
    }
    #[test]
    fn yaml_leaf_navigation_preserves_composite_owner_without_parent_or_rename() {
        let (_h, mut p) = setup();
        let d = doc("/project/src/main/resources/application.yaml", "greeting:\n  message: hello\n");
        let targets = config_targets(&d.path, &d.text);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].key, "greeting.message");
        assert_eq!(targets[0].range.start, Position::new(1, 2));
        assert_eq!(p.definition(&d, Position::new(1, 2)).len(), 1);
        assert!(p.definition(&d, Position::new(0, 2)).is_empty());
    }
    #[test]
    fn stale_module_proof_suppresses_navigation_even_with_fresh_source_facts() {
        let (_h, mut p) = setup();
        let c = doc(CONFIG_PATH, "greeting.message=x\n");
        assert_eq!(p.definition(&c, at(&c, "greeting.message")).len(), 1);
        for proof in p.modules.values_mut() {
            proof.at = std::time::Instant::now() - live::MODULE_TTL - std::time::Duration::from_secs(1);
        }
        assert!(p.sources.for_cached_document(&c.path, &c.text).unwrap().fresh());
        assert!(p.definition(&c, at(&c, "greeting.message")).is_empty());
        assert!(p.modules.values().all(|proof| proof.at.elapsed() > live::MODULE_TTL));
    }
    #[test]
    fn cold_provider_does_not_discover_and_late_files_are_not_loaded() {
        let (h, _p) = setup();
        let mut p = QuarkusTools::default();
        let c = doc(CONFIG_PATH, "greeting.message=x\n");
        assert!(p.definition(&c, at(&c, "greeting.message")).is_empty());
        p.activate_live();
        assert_eq!(p.definition(&c, at(&c, "greeting.message")).len(), 1);
        h.add_file("/project/src/main/java/app/Late.java", &JAVA.replace("greeting.message", "late.key"));
        let c = doc(CONFIG_PATH, "late.key=x\n");
        assert!(p.definition(&c, at(&c, "late.key")).is_empty());
    }
    #[test]
    fn unsaved_current_java_source_replaces_cached_destination() {
        let (_h, mut p) = setup();
        let j = doc(JAVA_PATH, &JAVA.replace("greeting.message", "renamed.message"));
        p.definition(&j, at(&j, "renamed.message"));
        let old = doc(CONFIG_PATH, "greeting.message=x\n");
        assert!(p.definition(&old, at(&old, "greeting.message")).is_empty());
        let new = doc(CONFIG_PATH, "renamed.message=x\n");
        assert_eq!(p.definition(&new, at(&new, "renamed.message")).len(), 1);
    }
    #[test]
    fn observed_oversized_source_invalidates_previous_navigation_fact() {
        let (_h, mut p) = setup();
        let c = doc(CONFIG_PATH, "greeting.message=x\n");
        assert_eq!(p.definition(&c, at(&c, "greeting.message")).len(), 1);
        let oversized = doc(JAVA_PATH, &format!("{JAVA}{}", " ".repeat(live::MAX_TEXT)));
        assert!(p.definition(&oversized, Position::new(0, 0)).is_empty());
        assert!(p.definition(&c, at(&c, "greeting.message")).is_empty());
    }
    #[test]
    fn duplicate_targets_are_all_listed_and_foreign_modules_are_not_guessed() {
        let (h, mut p) = setup();
        h.add_file("src/main/java/app/Duplicate.java", &JAVA.replace("Greeting", "Duplicate"));
        p.live_events(&[Event::FileSaved("/project/src/main/java/app/Duplicate.java".into())]);
        let c = doc(CONFIG_PATH, "greeting.message=x\n");
        let found: Vec<_> = p.definition(&c, at(&c, "greeting.message")).into_iter().map(|l| l.path).collect();
        assert_eq!(found, ["/project/src/main/java/app/Duplicate.java", JAVA_PATH]);
        let foreign = doc("/project/child/src/main/resources/application.properties", "greeting.message=x\n");
        assert!(p.definition(&foreign, at(&foreign, "greeting.message")).is_empty());
    }
    fn targets(markers: &[LineMarker], line: u32) -> Vec<(String, Range)> {
        markers.iter().filter(|m| m.line == line).map(|m| {
            let t = m.target.clone().unwrap();
            (t.path, t.range)
        }).collect()
    }
    #[test]
    fn markers_link_exact_keys_both_ways() {
        let (_h, mut p) = setup();
        let java = p.line_markers(&doc(JAVA_PATH, JAVA));
        assert_eq!(targets(&java, 3), [(CONFIG_PATH.to_owned(), range("greeting.message=hello\n", 0, 16))]);
        assert_eq!(java.len(), 1);
        let config = p.line_markers(&doc(CONFIG_PATH, "other.key=1\ngreeting.message=hello\n"));
        let byte = JAVA.find("greeting.message").unwrap();
        assert_eq!(targets(&config, 1), [(JAVA_PATH.to_owned(), range(JAVA, byte, byte + 16))]);
        assert_eq!(config.len(), 1);
        assert!(config.iter().chain(&java).all(|m| m.icon.as_deref() == Some(CONFIG_MARKER) && m.target.as_ref().unwrap().range.start < m.target.as_ref().unwrap().range.end));
    }
    #[test]
    fn several_targets_give_several_same_icon_markers_on_one_line() {
        let (h, mut p) = setup();
        let dev = "/project/src/main/resources/application-dev.properties";
        h.add_file(dev, "greeting.message=dev\n");
        p.live_events(&[Event::FileSaved(dev.into())]);
        let markers = p.line_markers(&doc(JAVA_PATH, JAVA));
        assert_eq!(targets(&markers, 3).iter().map(|(path, _)| path.as_str()).collect::<Vec<_>>(), [dev, CONFIG_PATH]);
        assert!(markers.iter().all(|m| m.line == 3 && m.icon == markers[0].icon && m.tooltip == markers[0].tooltip));
    }
    #[test]
    fn config_mapping_prefix_lists_keys_below_it_only() {
        let (h, mut p) = setup();
        h.add_file(CONFIG_PATH, "server.host=a\nserver.port=1\nserverx.port=2\nserver=3\n");
        p.live_events(&[Event::FileSaved(CONFIG_PATH.into())]);
        let mapping = "package app;\nimport io.smallrye.config.ConfigMapping;\n@ConfigMapping(prefix = \"server\")\ninterface Server { String host(); }\n";
        let markers = p.line_markers(&doc("/project/src/main/java/app/Server.java", mapping));
        assert_eq!(targets(&markers, 2).iter().map(|(_, r)| r.start.line).collect::<Vec<_>>(), [0, 1]);
        assert_eq!(markers.len(), 2);
        let foreign = mapping.replace("io.smallrye.config.ConfigMapping", "other.ConfigMapping");
        assert!(p.line_markers(&doc("/project/src/main/java/app/Server.java", &foreign)).is_empty());
    }
    #[test]
    fn cold_cache_and_unmatched_keys_give_no_markers() {
        let _h = setup().0;
        let mut cold = QuarkusTools::default();
        assert!(cold.line_markers(&doc(JAVA_PATH, JAVA)).is_empty());
        assert!(cold.line_markers(&doc(CONFIG_PATH, "greeting.message=x\n")).is_empty());
        let (_h, mut p) = setup();
        assert!(p.line_markers(&doc(CONFIG_PATH, "unused.key=x\n%dev.greeting.message=y\n")).is_empty());
    }
    #[test]
    fn source_scope_denial_switch_removal_and_events_are_conservative() {
        let (h, mut p) = setup();
        let c = doc(CONFIG_PATH, "greeting.message=x\n");
        h.set_project_root(Some("/other"));
        assert!(p.definition(&c, at(&c, "greeting.message")).is_empty());
        h.set_project_root(Some("/project"));
        p.live_events(&[Event::FilesChanged(FilesChanged { created: vec![], modified: vec![], removed: vec![JAVA_PATH.into()] })]);
        assert!(p.definition(&c, at(&c, "greeting.message")).is_empty());
        let _h = h.with_capabilities(&[]);
        assert!(p.definition(&c, at(&c, "greeting.message")).is_empty());
    }
    #[test]
    fn only_literal_owned_configproperty_injection_targets_are_retained() {
        assert_eq!(java_targets(JAVA_PATH, JAVA).len(), 1);
        for text in [
            JAVA.replace("import org.eclipse.microprofile.config.inject.ConfigProperty;", ""),
            JAVA.replace("org.eclipse.microprofile.config.inject.ConfigProperty", "custom.ConfigProperty"),
            JAVA.replace("class Greeting", "class ConfigProperty {} class Greeting"),
            JAVA.replace("\"greeting.message\"", "\"greeting.\" + NAME"),
            JAVA.replace("\"greeting.message\"", "\"greeting.\\u006dessage\""),
            JAVA.replace("class Greeting", "class Greeting<T>"),
            JAVA.trim_end_matches('}').into(),
            format!("{JAVA}{}", " ".repeat(live::MAX_TEXT)),
        ] {
            assert!(java_targets(JAVA_PATH, &text).is_empty(), "{text}");
        }
        assert!(java_targets(JAVA_PATH, "@ConfigMapping(prefix=\"server\") interface S {String hostName();}").is_empty());
    }
    #[test]
    fn profiles_named_placeholders_and_qute_expressions_do_not_infer_owners() {
        let (_h, mut p) = setup();
        for text in ["%dev.greeting.message=x\n", "quarkus.datasource.\"named\".jdbc.url=x\n", "${greeting.message}=x\n"] {
            let d = doc(CONFIG_PATH, text);
            assert!(p.definition(&d, Position::new(0, 1)).is_empty());
        }
        let d = doc("/project/src/main/resources/templates/hello.html", "{@app.Greeting greeting} {greeting.message}");
        assert!(p.definition(&d, Position::new(0, 5)).is_empty());
    }
}
