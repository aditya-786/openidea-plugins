//! Bounded source-only placeholder navigation. No host Rename graph or runtime config claim.
use crate::{
    Flyway,
    config::{self, Config},
    naming::{self, Name},
};
use openidea_plugin::{
    Location, Position, Project, Range, host,
    providers::{Document, LineMarker},
    text::position,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};
const MAX_TEXT: usize = 128 << 10;
const MAX_CONFIG: usize = 32 << 10;
const TTL: Duration = Duration::from_secs(120);
#[derive(Clone, Debug, PartialEq, Eq)]
struct Token {
    name: String,
    range: Range,
}
#[derive(Clone, PartialEq)]
struct Proof {
    path: String,
    config: Config,
    directories: Vec<String>,
    definitions: Vec<Token>,
}
struct Owner {
    proof: Proof,
    at: Instant,
    files: BTreeMap<String, (Instant, Vec<Token>)>,
}
#[derive(Default)]
pub(crate) struct State {
    owners: BTreeMap<String, Owner>,
}
fn inside(root: &str, path: &str) -> bool {
    path.starts_with(&format!("{}/", root.trim_end_matches('/')))
        && !path.split('/').any(|s| matches!(s, "." | ".."))
        && !path.contains('\\')
}
fn name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 128 && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}
fn token(text: &str, name: &str, start: usize) -> Token {
    Token { name: name.to_ascii_lowercase(), range: Range::new(position(text, start), position(text, start + name.len())) }
}
fn toml_value(value: &str) -> bool {
    if let Some(inner) = value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')) {
        return !inner.contains('\'') && !inner.chars().any(char::is_control);
    }
    match serde_json::from_str::<serde_json::Value>(value) {
        Ok(serde_json::Value::String(_) | serde_json::Value::Number(_) | serde_json::Value::Bool(_)) => true,
        Ok(serde_json::Value::Array(values)) => values.len() <= 16 && values.iter().all(serde_json::Value::is_string),
        _ => false,
    }
}
fn parsed(path: &str, text: &str, root: &str) -> Option<Proof> {
    if text.len() > MAX_CONFIG || text.contains('\0') || text.replace("\r\n", "").contains('\r') {
        return None;
    }
    let toml = path.ends_with(".toml");
    let mut table = "";
    let mut base = 0;
    let mut definitions = Vec::new();
    let mut keys = BTreeSet::new();
    let mut tables = BTreeSet::new();
    for raw in text.split_inclusive('\n') {
        let line = raw.trim_end_matches(['\r', '\n']);
        let clean = config::strip_comment(line);
        let body = clean.trim();
        let offset = base;
        base += raw.len();
        if body.is_empty() {
            continue;
        }
        if body.ends_with('\\') {
            return None;
        }
        if toml && body.starts_with('[') {
            table = body.strip_prefix('[')?.strip_suffix(']')?.trim();
            if !tables.insert(table) || !matches!(table, "flyway" | "flyway.placeholders") {
                return None;
            }
            continue;
        }
        let (written, value) = body.split_once('=')?;
        let written = written.trim();
        let value = value.trim();
        if value.is_empty() || (toml && !toml_value(value)) {
            return None;
        }
        let full = if toml { format!("{table}.{written}") } else { written.to_owned() };
        if !keys.insert(full.to_ascii_lowercase()) {
            return None;
        }
        let canonical = full.to_ascii_lowercase().replace('-', "");
        let default = match canonical.as_str() {
            "flyway.placeholderprefix" => Some("${"),
            "flyway.placeholdersuffix" => Some("}"),
            "flyway.placeholderreplacement" => Some("true"),
            _ => None,
        };
        if let Some(default) = default {
            if value.trim_matches(['\'', '"']) != default {
                return None;
            }
        }
        if canonical.contains("configfiles") || canonical.contains("environment") || canonical.contains("placeholderseparator") {
            return None;
        }
        let (spelling, from) = if toml && table == "flyway.placeholders" {
            let quoted = written.starts_with('"');
            let spelling = if quoted {
                written.strip_prefix('"')?.strip_suffix('"')?
            } else {
                if written.contains('.') {
                    return None;
                }
                written
            };
            (spelling, offset + line.find(written)? + usize::from(quoted))
        } else if !toml {
            let Some(spelling) = written.strip_prefix("flyway.placeholders.") else {
                continue;
            };
            (spelling, offset + line.find(written)? + "flyway.placeholders.".len())
        } else {
            continue;
        };
        if !name(spelling) {
            return None;
        }
        definitions.push(token(text, spelling, from));
        if definitions.len() > 512 {
            return None;
        }
    }
    let config = Config::from_settings(&config::settings(path, text), true);
    let naming = &config.naming;
    if naming.suffixes.len() > 16
        || [&naming.versioned, &naming.undo, &naming.repeatable, &naming.baseline, &naming.separator]
            .into_iter()
            .chain(naming.suffixes.iter())
            .any(|s| s.is_empty() || s.len() > 32 || !s.bytes().all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b)))
    {
        return None;
    }
    let mut directories = Vec::new();
    for location in &config.locations {
        let directory = if let Some(relative) = location.strip_prefix("filesystem:") {
            if relative.starts_with('/') { relative.to_owned() } else { format!("{root}/{relative}") }
        } else if let Some(relative) = location.strip_prefix("classpath:") {
            format!("{root}/src/main/resources/{relative}")
        } else {
            return None;
        };
        let directory = directory.trim_end_matches('/').to_owned();
        if !inside(root, &directory) || directory.contains(['*', '?', '{', '}', '$']) {
            return None;
        }
        directories.push(directory);
    }
    if directories.is_empty() || directories.len() > 16 {
        return None;
    }
    let mut unique = BTreeSet::new();
    if definitions.iter().any(|t| !unique.insert(&t.name)) {
        return None;
    }
    Some(Proof { path: path.into(), config, directories, definitions })
}
fn migration(proof: &Proof, path: &str) -> bool {
    proof.directories.iter().any(|d| path.starts_with(&format!("{d}/")))
        && path.ends_with(".sql")
        && matches!(naming::parse(path.rsplit('/').next().unwrap_or(""), &proof.config.naming), Name::Migration { .. })
}
fn sql(text: &str) -> Option<Vec<Token>> {
    if text.len() > MAX_TEXT {
        return None;
    }
    let b = text.as_bytes();
    let mut i = 0;
    let mut quote = None;
    let mut block = 0u8;
    let mut out = Vec::new();
    while i < b.len() {
        if block > 0 {
            if b[i..].starts_with(b"/*") {
                block = block.checked_add(1)?;
                if block > 16 {
                    return None;
                }
                i += 2;
            } else if b[i..].starts_with(b"*/") {
                block -= 1;
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        if quote.is_none() && b[i..].starts_with(b"--") {
            i += b[i..].iter().position(|c| *c == b'\n').unwrap_or(b.len() - i);
            continue;
        }
        if quote.is_none() && b[i..].starts_with(b"/*") {
            block = 1;
            i += 2;
            continue;
        }
        if b[i..].starts_with(b"${") {
            let end = i + 2 + b[i + 2..].iter().position(|c| *c == b'}')?;
            let spelling = std::str::from_utf8(&b[i + 2..end]).ok()?;
            if name(spelling) {
                out.push((spelling.to_ascii_lowercase(), i + 2));
                if out.len() > 512 {
                    return None;
                }
            }
            i = end + 1;
            continue;
        }
        if quote.is_none() && b[i] == b'$' {
            if let Some(end) = text[i + 1..].find('$').filter(|end| *end <= 64) {
                if text[i + 1..i + 1 + end].bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_') {
                    return None;
                }
            }
        }
        if let Some(q) = quote {
            if b[i] == b'\\' {
                i = (i + 2).min(b.len());
                continue;
            }
            if b[i] == q {
                if b.get(i + 1) == Some(&q) {
                    i += 2;
                    continue;
                }
                quote = None;
            }
        } else if matches!(b[i], b'\'' | b'"' | b'`') {
            quote = Some(b[i]);
        }
        i += 1;
    }
    if block != 0 || quote.is_some() {
        return None;
    }
    let mut cursor = 0;
    let mut line = 0u32;
    let mut column = 0u32;
    Some(
        out.into_iter()
            .map(|(name, start)| {
                for c in text[cursor..start].chars() {
                    if c == '\n' {
                        line += 1;
                        column = 0;
                    } else {
                        column += c.len_utf16() as u32;
                    }
                }
                let from = Position::new(line, column);
                column += name.len() as u32;
                cursor = start + name.len();
                Token { name, range: Range::new(from, Position::new(line, column)) }
            })
            .collect(),
    )
}
impl State {
    pub fn invalidate(&mut self, path: &str) {
        self.owners.retain(|root, _| !inside(root, path));
    }
    pub fn warm(&mut self, project: &Project, loaded: &Config) {
        let Ok(root) = project.root() else {
            return;
        };
        self.owners.retain(|_, o| o.at.elapsed() < TTL);
        let started = Instant::now();
        let result = (|| {
            let mut chosen = None;
            for file in config::CONFIG_FILES {
                if started.elapsed() > Duration::from_millis(200) {
                    return None;
                }
                let path = format!("{root}/{file}");
                if !project.exists(&path).ok()? {
                    continue;
                }
                if chosen.is_some() {
                    return None;
                }
                let text = project.read_file(&path).ok()?;
                chosen = Some(parsed(&path, &text, &root)?);
            }
            let proof = chosen?;
            // Existing Spring/environment overlays cannot silently compete with this owner.
            let spring = project.find_files("**/src/main/resources/application*", 101).ok()?;
            if spring.len() >= 100 {
                return None;
            }
            for path in spring {
                if started.elapsed() > Duration::from_millis(200) {
                    return None;
                }
                let text = project.read_file(&path).ok()?;
                if text.len() > MAX_TEXT || !config::settings(&path, &text).is_empty() {
                    return None;
                }
            }
            if started.elapsed() > Duration::from_millis(200) || &proof.config != loaded {
                return None;
            }
            Some(proof)
        })();
        let Some(proof) = result else {
            self.owners.remove(&root);
            return;
        };
        if self.owners.get(&root).is_some_and(|owner| owner.proof == proof) {
            self.owners.get_mut(&root).unwrap().at = Instant::now();
            return;
        }
        if self.owners.len() >= 8 {
            self.owners.clear();
        }
        self.owners.insert(root, Owner { proof, at: Instant::now(), files: BTreeMap::new() });
    }
    fn destinations(&mut self, doc: &Document, at: Position) -> Vec<Location> {
        let Some(root) = host::project_of(&doc.path).and_then(|p| p.root().ok()) else {
            return vec![];
        };
        if !inside(&root, &doc.path) {
            return vec![];
        }
        let Some(owner) = self.owners.get_mut(&root) else {
            return vec![];
        };
        if owner.at.elapsed() >= TTL {
            return vec![];
        }
        if doc.path == owner.proof.path {
            if parsed(&doc.path, &doc.text, &root).as_ref() != Some(&owner.proof) {
                self.owners.remove(&root);
                return vec![];
            }
            if !matches!(doc.language.as_str(), "ini" | "toml") {
                return vec![];
            }
            let Some(selected) = owner.proof.definitions.iter().find(|t| t.range.start <= at && at < t.range.end) else {
                return vec![];
            };
            let locations: Vec<_> = owner
                .files
                .iter()
                .filter(|(_, (at, _))| at.elapsed() < TTL)
                .flat_map(|(path, (_, tokens))| {
                    tokens.iter().filter(|t| t.name == selected.name).map(|t| Location { path: path.clone(), range: t.range })
                })
                .collect();
            return if locations.len() > 256 { vec![] } else { locations };
        }
        owner.files.retain(|_, (at, _)| at.elapsed() < TTL);
        owner.files.remove(&doc.path);
        if doc.language != "sql" || !migration(&owner.proof, &doc.path) {
            return vec![];
        }
        let Some(tokens) = sql(&doc.text) else {
            return vec![];
        };
        if owner.files.len() >= 128 || owner.files.values().map(|(_, tokens)| tokens.len()).sum::<usize>() + tokens.len() > 4096 {
            self.owners.remove(&root);
            return vec![];
        }
        let selected = tokens.iter().find(|t| t.range.start <= at && at < t.range.end).cloned();
        owner.files.insert(doc.path.clone(), (Instant::now(), tokens));
        let Some(selected) = selected else {
            return vec![];
        };
        owner
            .proof
            .definitions
            .iter()
            .filter(|t| t.name == selected.name)
            .map(|t| Location { path: owner.proof.path.clone(), range: t.range })
            .collect()
    }
}
impl Flyway {
    pub(crate) fn source_definition(&mut self, doc: &Document, at: Position) -> Vec<Location> {
        let placeholders = self.navigation.destinations(doc, at);
        if placeholders.is_empty() { self.table_navigation.definition(doc, at) } else { placeholders }
    }
    pub(crate) fn source_markers(&mut self, doc: &Document) -> Vec<LineMarker> {
        let _ = self.source_definition(doc, Position::new(0, 0));
        let mut markers = self.table_navigation.markers(doc);
        markers.extend(self.placeholder_markers(doc));
        markers
    }
    /// One marker per distinct placeholder of a line, each to its declaration in the cached configuration.
    fn placeholder_markers(&self, doc: &Document) -> Vec<LineMarker> {
        let Some(tokens) = sql(&doc.text).filter(|_| doc.language == "sql") else {
            return vec![];
        };
        let Some(root) = host::project_of(&doc.path).and_then(|p| p.root().ok()) else {
            return vec![];
        };
        let Some(owner) = self.navigation.owners.get(&root).filter(|owner| owner.at.elapsed() < TTL && migration(&owner.proof, &doc.path))
        else {
            return vec![];
        };
        if !owner.files.contains_key(&doc.path) {
            return vec![];
        }
        let mut lines: BTreeMap<u32, Vec<Location>> = BTreeMap::new();
        for t in tokens {
            let Some(definition) = owner.proof.definitions.iter().find(|d| d.name == t.name) else {
                continue;
            };
            let target = Location { path: owner.proof.path.clone(), range: definition.range };
            let targets = lines.entry(t.range.start.line).or_default();
            if !targets.contains(&target) {
                targets.push(target);
            }
        }
        const TOOLTIP: &str = "Declared Flyway placeholder in cached source configuration; runtime overrides are not resolved";
        lines.into_iter().flat_map(|(line, targets)| crate::line_markers(line, crate::PLACEHOLDER_MARKER, TOOLTIP, targets)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{Event, Plugin, mock::MockHost, providers::Providers};
    const SQL: &str = "/project/sql/V1__seed.sql";
    const CONF: &str = "flyway.placeholders.seed=42\n";
    fn doc(path: &str, text: &str) -> Document {
        Document {
            path: path.into(),
            language: if path.ends_with(".sql") {
                "sql"
            } else if path.ends_with(".toml") {
                "toml"
            } else {
                "ini"
            }
            .into(),
            version: 1,
            text: text.into(),
        }
    }
    fn at(d: &Document, needle: &str) -> Position {
        position(&d.text, d.text.find(needle).unwrap())
    }
    fn setup() -> (MockHost, Flyway) {
        let h = MockHost::install();
        h.add_file("flyway.conf", CONF);
        let mut p = Flyway::default();
        let project = host::project_of(SQL).unwrap();
        let config = config::load(&project);
        p.navigation.warm(&project, &config);
        (h, p)
    }
    #[test]
    fn literal_placeholder_navigation_preserves_prefix_case_and_utf16() {
        let (_h, mut p) = setup();
        let d = doc(SQL, "-- source\nselect '😀${SEED}';\n");
        let found = p.definition(&d, at(&d, "SEED"));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, "/project/flyway.conf");
        assert_eq!(found[0].range, Range::new(Position::new(0, 20), Position::new(0, 24)));
        let c = doc("/project/flyway.conf", CONF);
        let back = p.definition(&c, at(&c, "seed"));
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].path, SQL);
        assert_eq!(back[0].range, Range::new(Position::new(1, 12), Position::new(1, 16)));
        assert_eq!(p.line_markers(&d)[0].target.as_ref().unwrap().range, found[0].range);
    }
    #[test]
    fn every_placeholder_of_a_line_gets_its_own_marker() {
        let h = MockHost::install();
        h.add_file("flyway.conf", "flyway.placeholders.seed=42\nflyway.placeholders.owner=app\n");
        let mut p = Flyway::default();
        let d = doc(SQL, "select '${seed}', '${owner}', '${seed}', '${unknown}';\nselect 1;\n");
        assert!(p.line_markers(&d).is_empty(), "a cold cache gives no marker");
        let project = host::project_of(SQL).unwrap();
        p.navigation.warm(&project, &config::load(&project));
        let markers = p.line_markers(&d);
        let targets: Vec<_> = markers.iter().map(|m| (m.line, m.icon.as_deref(), m.target.as_ref().unwrap().range.start)).collect();
        assert_eq!(targets, [(0, Some(crate::PLACEHOLDER_MARKER), Position::new(0, 20)), (0, Some(crate::PLACEHOLDER_MARKER), Position::new(1, 20))]);
    }
    #[test]
    fn toml_literal_keys_have_exact_token_ranges_and_unknown_tables_are_quiet() {
        let h = MockHost::install();
        let text = "[flyway.placeholders]\r\n\"seed\" = \"42\"\r\n";
        h.add_file("flyway.toml", text);
        let mut p = Flyway::default();
        let project = host::project_of(SQL).unwrap();
        p.navigation.warm(&project, &config::load(&project));
        let d = doc(SQL, "select '${seed}';");
        let found = p.definition(&d, at(&d, "seed"));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].range, Range::new(Position::new(1, 1), Position::new(1, 5)));
        for text in [
            "[environments.dev.placeholders]\nseed=42\n",
            "[flyway.placeholders]\nseed=\"unterminated\n",
            "[flyway]\nlocations=[\"filesystem:sql\",,]\n",
            "[flyway.placeholders]\nseed=1\nSEED=2\n",
            "flyway.placeholderPrefix=@{\nflyway.placeholders.seed=1\n",
            "flyway.placeholderReplacement=false\nflyway.placeholders.seed=1\n",
        ] {
            assert!(
                parsed(if text.starts_with('[') { "/project/flyway.toml" } else { "/project/flyway.conf" }, text, "/project").is_none()
            );
        }
    }
    #[test]
    fn comments_malformed_and_overflowing_sql_never_become_partial_facts() {
        let text = "-- ${skip}\n/* ${also_skip} /* ${nested} */ */\nselect '${seed}', \"${SEED}\";";
        let found = sql(text).unwrap();
        assert_eq!(found.len(), 2);
        assert!(found.iter().all(|t| t.name == "seed"));
        for text in [
            "select '${seed".into(),
            "/* ${seed}".into(),
            "select $$-- ${seed} $$;".into(),
            "select '${seed}' ; 'unterminated".into(),
            "select '${seed}';\n".repeat(513),
            " ".repeat(MAX_TEXT + 1),
        ] {
            assert!(sql(&text).is_none());
        }
        assert!(sql("select '${ spaced }', '${flyway:timestamp}';").unwrap().is_empty());
    }
    #[test]
    fn cold_unknown_paths_other_roots_and_configuration_conflicts_do_not_guess() {
        let (h, mut p) = setup();
        let d = doc(SQL, "select '${seed}';");
        assert!(Flyway::default().definition(&d, at(&d, "seed")).is_empty());
        for path in ["/project/other/V1__seed.sql", "/project/sql/script.sql", "/project/sql/../V1__seed.sql", "/other/sql/V1__seed.sql"] {
            let d = doc(path, &d.text);
            assert!(p.definition(&d, at(&d, "seed")).is_empty());
        }
        h.add_file("flyway.toml", "[flyway.placeholders]\nseed=2\n");
        let project = host::project_of(SQL).unwrap();
        p.navigation.warm(&project, &config::load(&project));
        assert!(p.definition(&d, at(&d, "seed")).is_empty());
    }
    #[test]
    fn current_empty_rejected_and_changed_source_invalidates_previous_occurrences() {
        let (_h, mut p) = setup();
        let d = doc(SQL, "select '${seed}';");
        assert_eq!(p.definition(&d, at(&d, "seed")).len(), 1);
        let c = doc("/project/flyway.conf", CONF);
        assert_eq!(p.definition(&c, at(&c, "seed")).len(), 1);
        p.navigation.owners.get_mut("/project").unwrap().files.get_mut(SQL).unwrap().0 = Instant::now() - TTL - Duration::from_secs(1);
        assert!(p.definition(&c, at(&c, "seed")).is_empty());
        p.definition(&d, at(&d, "seed"));
        p.line_markers(&doc(SQL, "select 1;"));
        assert!(p.definition(&c, at(&c, "seed")).is_empty());
        p.definition(&d, at(&d, "seed"));
        p.line_markers(&doc(SQL, &"select '${seed}';\n".repeat(513)));
        assert!(p.definition(&c, at(&c, "seed")).is_empty());
        let changed = doc("/project/flyway.conf", &format!("# moved\n{CONF}"));
        assert!(p.definition(&changed, at(&changed, "seed")).is_empty());
        assert!(p.definition(&d, at(&d, "seed")).is_empty());
    }
    #[test]
    fn expiry_denial_and_delivered_delete_cannot_refresh_from_provider() {
        let (h, mut p) = setup();
        let d = doc(SQL, "select '${seed}';");
        assert_eq!(p.definition(&d, at(&d, "seed")).len(), 1);
        p.navigation.owners.get_mut("/project").unwrap().at = Instant::now() - TTL - Duration::from_secs(1);
        assert!(p.definition(&d, at(&d, "seed")).is_empty());
        let project = host::project_of(SQL).unwrap();
        p.navigation.warm(&project, &config::load(&project));
        assert_eq!(p.definition(&d, at(&d, "seed")).len(), 1);
        p.handle_events(
            &[Event::FilesChanged(openidea_plugin::FilesChanged {
                created: vec![],
                modified: vec![],
                removed: vec!["/project/flyway.conf".into()],
            })],
            &h.invocation(),
        );
        assert!(p.definition(&d, at(&d, "seed")).is_empty());
        let _h = h.with_capabilities(&[]);
        assert!(p.definition(&d, at(&d, "seed")).is_empty());
    }
    #[test]
    fn spring_overlay_and_dynamic_location_sources_are_unsupported() {
        let (h, mut p) = setup();
        h.add_file("src/main/resources/application.properties", "spring.flyway.placeholders.seed=other\n");
        let project = host::project_of(SQL).unwrap();
        p.navigation.warm(&project, &config::load(&project));
        assert!(p.navigation.owners.is_empty());
        for location in ["filesystem:../outside", "filesystem:sql/{vendor}", "filesystem:/outside/sql", "s3:bucket"] {
            assert!(parsed("/project/flyway.conf", &format!("flyway.locations={location}\n{CONF}"), "/project").is_none());
        }
    }
}
