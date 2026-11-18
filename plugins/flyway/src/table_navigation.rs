//! Source-only table navigation, deliberately separate from Rename identities.
use openidea_plugin::{Location, Position, Project, Range, host, providers::{Document, LineMarker}, text::position};
use std::{collections::{BTreeMap, BTreeSet}, time::{Duration, Instant}};
#[derive(Clone)]
struct Token { word: String, text: String, start: usize, end: usize, quote: Option<char> }
/// Identifier part as written: `quote` is the opening delimiter (`"`, `` ` `` or `[`) when delimited.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Ident { text: String, quote: Option<char> }
impl Ident {
    fn plain(text: impl Into<String>) -> Self { Ident { text: text.into(), quote: None } }
    /// JPA name as written in `@Table`: plain, `"x"`, `` `x` `` or `[x]`; delimiters are Hibernate's quoting syntax.
    fn parse_mapping(value: &str) -> Option<Self> {
        let first = value.chars().next()?;
        let close = match first { '"' => '"', '`' => '`', '[' => ']', _ => return identifier(value).then(|| Ident::plain(value)) };
        let inner = value.strip_prefix(first)?.strip_suffix(close)?;
        if inner.is_empty() || inner.contains(['"', '`', '[', ']', '\n']) { return None; }
        Some(Ident { text: inner.into(), quote: Some(first) })
    }
}
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Dialect { Postgres, H2, Oracle, MySql, SqlServer }
/// Canonical lookup key; none when resolution would depend on an unknown dialect or on server settings.
/// Unquoted names fold per dialect (lower when unknown, which only has to be consistent on both sides);
/// quoted names stay exact, except SQL Server's case-insensitive collation.
fn key(parts: &[Ident], dialect: Option<Dialect>) -> Option<String> {
    let mut out = Vec::new();
    for part in parts {
        out.push(match (part.quote, dialect) {
            (None, Some(Dialect::H2 | Dialect::Oracle)) => part.text.to_ascii_uppercase(),
            (None, _) => part.text.to_ascii_lowercase(),
            (Some(_), None) => return None,
            (Some('"'), Some(Dialect::Postgres | Dialect::H2 | Dialect::Oracle)) => part.text.clone(),
            (Some('"' | '['), Some(Dialect::SqlServer)) => part.text.to_ascii_lowercase(),
            (Some('`'), Some(Dialect::MySql)) => part.text.clone(),
            _ => return None,
        });
    }
    Some(out.join("."))
}
fn dialect_of(text: &str) -> Result<Vec<Dialect>, ()> {
    let lower = text.to_ascii_lowercase();
    let mut found = Vec::new();
    for (marker, dialect) in [("jdbc:postgresql:", Dialect::Postgres), ("org.postgresql.driver", Dialect::Postgres), ("jdbc:h2:", Dialect::H2), ("org.h2.driver", Dialect::H2),
        ("jdbc:oracle:", Dialect::Oracle), ("oracle.jdbc.", Dialect::Oracle), ("jdbc:mysql:", Dialect::MySql), ("jdbc:mariadb:", Dialect::MySql), ("com.mysql.", Dialect::MySql), ("org.mariadb.jdbc", Dialect::MySql),
        ("jdbc:sqlserver:", Dialect::SqlServer), ("com.microsoft.sqlserver", Dialect::SqlServer)] {
        if lower.contains(marker) && !found.contains(&dialect) { found.push(dialect); }
    }
    if found.contains(&Dialect::H2) && ["database_to_upper", "database_to_lower", "case_insensitive_identifiers", "mode="].iter().any(|flag| lower.contains(flag)) { return Err(()); }
    Ok(found)
}
/// Dialect only from explicit datasource URL/driver text in Spring or Flyway config; conflicting or absent evidence is none.
fn detect_dialect(project: &Project) -> Option<Dialect> {
    let mut all: Vec<Dialect> = Vec::new();
    for glob in ["**/application*.properties", "**/application*.yml", "**/application*.yaml", "**/flyway.conf", "**/flyway.toml"] {
        let files = project.find_files(glob, 33).ok()?;
        if files.len() > 32 { return None; }
        for path in files {
            let found = dialect_of(&project.read_file(&path).ok()?).ok()?;
            for dialect in found { if !all.contains(&dialect) { all.push(dialect); } }
        }
    }
    if all.len() == 1 { all.pop() } else { None }
}
fn tokens(text: &str) -> Option<Vec<Token>> {
    if text.len() > 128 << 10 { return None; }
    let bytes = text.as_bytes(); let mut at = 0; let mut out = Vec::new();
    while at < bytes.len() {
        if out.len() >= 16384 { return None; }
        if bytes[at].is_ascii_whitespace() { at += 1; continue; }
        if text[at..].starts_with("--") { at += text[at..].find('\n').unwrap_or(text.len()-at); continue; }
        if text[at..].starts_with("/*") {
            let end = text[at+2..].find("*/")? + at + 2;
            if text[at+2..end].contains("/*") { return None; }
            at = end + 2; continue;
        }
        if bytes[at] == b'\'' {
            at += 1;
            loop {
                if at >= bytes.len() { return None; }
                if bytes[at] == b'\'' {
                    at += 1;
                    if bytes.get(at) == Some(&b'\'') { at += 1; } else { break; }
                } else if bytes[at] == b'\\' { return None; } else { at += 1; }
            }
            out.push(Token { word: "<literal>".into(), text: String::new(), start: at, end: at, quote: None }); continue;
        }
        let close = match bytes[at] { b'"' => Some('"'), b'`' => Some('`'), b'[' => Some(']'), _ => None };
        if let Some(close) = close {
            let (start, open) = (at, bytes[at] as char);
            at += 1;
            let mut name = String::new();
            loop {
                let found = text[at..].find(close)?;
                name.push_str(&text[at..at + found]);
                at += found + 1;
                if text[at..].starts_with(close) { name.push(close); at += 1; } else { break; }
            }
            if name.is_empty() || name.contains('\n') { return None; }
            out.push(Token { word: "<quoted>".into(), text: name, start, end: at, quote: Some(open) }); continue;
        }
        if bytes[at] == b'$' { return None; }
        let start = at;
        if bytes[at].is_ascii_alphabetic() || bytes[at] == b'_' {
            at += 1;
            while bytes.get(at).is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_') { at += 1; }
            out.push(Token { word: text[start..at].to_ascii_lowercase(), text: text[start..at].into(), start, end: at, quote: None });
        } else {
            let ch = text[at..].chars().next()?; at += ch.len_utf8();
            out.push(Token { word: ch.to_string(), text: String::new(), start, end: at, quote: None });
        }
    }
    Some(out)
}
fn identifier(word: &str) -> bool { word.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') && word.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_') }
#[derive(Clone)]
struct Table { parts: Vec<Ident>, range: Range, definition: bool }
fn name_part(token: &Token) -> bool { token.quote.is_some() || identifier(&token.word) }
fn tables(text: &str) -> Option<Vec<Table>> {
    let tokens = tokens(text)?; let mut out = Vec::new();
    for (i, token) in tokens.iter().enumerate() {
        let previous = i.checked_sub(1).and_then(|j| tokens.get(j)).filter(|t| t.quote.is_none()).map(|t| t.word.as_str());
        if token.quote.is_some() { continue; }
        let definition = token.word == "table" && previous == Some("create");
        let reference = (matches!(token.word.as_str(), "from" | "join" | "references") || token.word == "update" && previous != Some("on"))
            || token.word == "into" && previous == Some("insert")
            || token.word == "table" && matches!(previous, Some("alter" | "drop" | "truncate"));
        if !definition && !reference { continue; }
        let mut next = i + 1;
        if tokens.get(next).is_some_and(|t| t.word == "if") {
            next += 1;
            if tokens.get(next).is_some_and(|t| t.word == "not") { next += 1; }
            if !tokens.get(next).is_some_and(|t| t.word == "exists") { continue; }
            next += 1;
        }
        let Some(first) = tokens.get(next).filter(|t| name_part(t)) else { continue; };
        let mut parts = vec![Ident { text: first.text.clone(), quote: first.quote }]; let mut end = first.end;
        while tokens.get(next + 1).is_some_and(|t| t.word == ".") {
            let Some(part) = tokens.get(next + 2).filter(|t| name_part(t)) else { return None; };
            parts.push(Ident { text: part.text.clone(), quote: part.quote }); end = part.end; next += 2;
        }
        out.push(Table { parts, range: Range::new(position(text, first.start), position(text, end)), definition });
    }
    Some(out)
}
fn persistence_annotation(file: &spring_text::java::JavaFile, ann: &spring_text::java::Annotation, text: &str) -> bool {
    let written = &text[ann.at + 1..ann.name_at + ann.name.len()];
    if written.contains('.') { return written == format!("jakarta.persistence.{}", ann.name) || written == format!("javax.persistence.{}", ann.name); }
    let exact: Vec<_> = file.imports.iter().filter(|import| !import.is_static && import.path.ends_with(&format!(".{}", ann.name))).collect();
    if !exact.is_empty() { return exact.len() == 1 && [format!("jakarta.persistence.{}", ann.name), format!("javax.persistence.{}", ann.name)].contains(&exact[0].path); }
    file.imports.iter().filter(|import| !import.is_static && import.path.ends_with(".*")).count() == 1
        && file.imports.iter().any(|import| !import.is_static && matches!(import.path.as_str(), "jakarta.persistence.*" | "javax.persistence.*"))
        && !file.types.iter().any(|ty| ty.name == ann.name)
}
type Mapped = (Vec<Ident>, Range, String);
fn parse_source(text: &str, kotlin: bool) -> spring_text::java::JavaFile {
    if kotlin { spring_text::kotlin::parse(text) } else { spring_text::java::parse(text) }
}
/// Binary class name (`pkg.Outer$Inner`), the form a JPA XML `class` attribute uses.
fn class_name(file: &spring_text::java::JavaFile, index: usize) -> String {
    let mut chain = vec![file.types[index].name.as_str()]; let mut at = index;
    while let Some(outer) = file.types[at].outer { chain.insert(0, &file.types[outer].name); at = outer; }
    let name = chain.join("$");
    if file.package.is_empty() { name } else { format!("{}.{name}", file.package) }
}
fn entity_tables_in(text: &str, kotlin: bool) -> Vec<Mapped> {
    if text.len() > 128 << 10 { return vec![]; }
    let file = parse_source(text, kotlin); let mut out = Vec::new();
    // Constants are resolved from this file only; cross-file names stay quiet.
    let mut constants = spring_text::constants::Constants::default();
    constants.add(&file, text);
    // Kotlin `const val` lives in a companion, keyed `Companion.NAME`; two companions in one file would collide.
    let companion_unique = file.types.iter().filter(|ty| ty.name == "Companion").count() <= 1;
    for (index, ty) in file.types.iter().enumerate() {
        if ty.kind != "class" || !ty.annotation("Entity").is_some_and(|ann| persistence_annotation(&file, ann, text)) { continue; }
        let Some(table) = ty.annotation("Table").filter(|ann| persistence_annotation(&file, ann, text)) else { continue; };
        let literal = |key: &str| table.arg(key).and_then(|arg| {
            let written = arg.text.trim();
            if let [only] = arg.strings.as_slice() {
                if written.starts_with('"') && written.ends_with('"') && written.len() == only.end - only.at + 2 { return Some(only.value.clone()); }
            }
            constants.resolve(&arg.text, Some(&ty.name)).or_else(|| (kotlin && companion_unique).then(|| constants.resolve(arg.text.trim().strip_prefix(&format!("{}.", ty.name)).unwrap_or(&arg.text), Some("Companion"))).flatten())
        });
        let Some(name) = literal("name").and_then(|value| Ident::parse_mapping(&value)) else { continue; };
        let mut parts = vec![name];
        if table.arg("schema").is_some() {
            let Some(schema) = literal("schema").and_then(|value| Ident::parse_mapping(&value)) else { continue; };
            parts.insert(0, schema);
        }
        out.push((parts, Range::new(position(text, ty.at), position(text, ty.at + ty.name.len())), class_name(&file, index)));
    }
    out
}
#[cfg(test)]
fn entity_tables(text: &str) -> Vec<Mapped> { entity_tables_in(text, false) }
/// Spring Boot's default physical naming for a plain `CamelCase` class name; none when the
/// conversion is ambiguous (digits, acronyms, non-ASCII).
fn snake_case(name: &str) -> Option<String> {
    let bytes = name.as_bytes();
    if !bytes.first()?.is_ascii_uppercase() || !bytes.last()?.is_ascii_lowercase() || !bytes.iter().all(u8::is_ascii_alphabetic)
        || bytes.windows(2).any(|pair| pair[0].is_ascii_uppercase() && pair[1].is_ascii_uppercase()) { return None; }
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        if i > 0 && c.is_ascii_uppercase() { out.push('_'); }
        out.push(c.to_ascii_lowercase());
    }
    Some(out)
}
/// Top-level `@Entity` classes without `@Table`/`@Entity` arguments/superclass: the table is the
/// implicitly named one. Only valid under Spring Boot's default naming strategies (see `implicit_naming_default`).
fn implicit_entity_tables_in(text: &str, kotlin: bool) -> Vec<Mapped> {
    if text.len() > 128 << 10 { return vec![]; }
    let file = parse_source(text, kotlin);
    (0..file.types.len()).filter_map(|index| {
        let ty = &file.types[index];
        // Kotlin supertypes are not split into class and interface, so any supertype is refused.
        let plain = ty.kind == "class" && ty.outer.is_none() && ty.extends.is_empty() && (!kotlin || ty.implements.is_empty())
            && !ty.has(&["Table", "SecondaryTable", "Inheritance", "MappedSuperclass", "Embeddable"]) && !ty.modifiers.iter().any(|m| m == "abstract");
        let entity = ty.annotation("Entity").is_some_and(|ann| ann.args.is_empty() && persistence_annotation(&file, ann, text));
        (plain && entity).then(|| Some((vec![Ident::plain(snake_case(&ty.name)?)], Range::new(position(text, ty.at), position(text, ty.at + ty.name.len())), class_name(&file, index)))).flatten()
    }).collect()
}
#[cfg(test)]
fn implicit_entity_tables(text: &str) -> Vec<Mapped> { implicit_entity_tables_in(text, false) }
/// True only with Spring Boot declared at the root and no naming/quoting override anywhere visible.
fn implicit_naming_default(project: &Project, root: &str) -> bool {
    let boot = ["pom.xml", "build.gradle", "build.gradle.kts"].iter()
        .any(|name| project.read_file(&format!("{root}/{name}")).is_ok_and(|text| text.contains("spring-boot")));
    if !boot { return false; }
    for glob in ["**/application*.properties", "**/application*.yml", "**/application*.yaml", "**/persistence.xml", "**/hibernate.properties", "**/hibernate.cfg.xml"] {
        let Ok(files) = project.find_files(glob, 33) else { return false; };
        if files.len() > 32 { return false; }
        for path in files {
            if path.ends_with("persistence.xml") || path.ends_with("hibernate.properties") || path.ends_with("hibernate.cfg.xml") { return false; }
            let Ok(text) = project.read_file(&path) else { return false; };
            let lower = text.to_ascii_lowercase();
            if lower.contains("naming") || lower.contains("quoted") { return false; }
        }
    }
    true
}
/// JPA XML mappings that replace annotation metadata: per class (binary name) the literal table, or `None` when the
/// XML takes over but the table is not literal and unambiguous. `annotations_unreliable` means annotation-derived
/// tables cannot be trusted (complete-metadata mode, default schema/catalog, or a mapping set we cannot enumerate).
#[derive(Default)]
struct XmlMappings { entities: BTreeMap<String, Option<(Vec<Ident>, String, Range)>>, annotations_unreliable: bool }
fn xml_mappings(project: &Project, root: &str) -> XmlMappings {
    let unprovable = || XmlMappings { entities: BTreeMap::new(), annotations_unreliable: true };
    let under_root = |path: &String| path.starts_with(&format!("{root}/"));
    let (Ok(persistence), Ok(defaults)) = (project.find_files("**/persistence.xml", 9), project.find_files("**/META-INF/orm.xml", 9)) else { return unprovable(); };
    if persistence.len() > 8 || defaults.len() > 8 { return unprovable(); }
    let mut files: BTreeSet<String> = defaults.into_iter().filter(under_root).collect(); let mut units = 0;
    for path in persistence.into_iter().filter(under_root) {
        let Ok(text) = project.read_file(&path) else { return unprovable(); };
        let elements = spring_text::xml::elements(&text);
        units += elements.iter().filter(|e| e.local() == "persistence-unit").count();
        for element in elements.iter().filter(|e| e.local() == "mapping-file") {
            let name = text[element.tag_end..].split('<').next().unwrap_or("").trim().trim_start_matches('/');
            if name.is_empty() || name.contains("..") || name.contains(['*', '?', '[', '{', '\\', ' ']) { return unprovable(); }
            let Ok(found) = project.find_files(&format!("**/{name}"), 9) else { return unprovable(); };
            let found: Vec<_> = found.into_iter().filter(|f| under_root(f) && f.ends_with(&format!("/{name}")) && module(f) == module(&path)).collect();
            let [only] = found.as_slice() else { return unprovable(); };
            files.insert(only.clone());
        }
    }
    if units > 1 { return unprovable(); }
    let mut out = XmlMappings::default();
    for path in files {
        let Ok(text) = project.read_file(&path) else { return unprovable(); };
        if text.len() > 256 << 10 { return unprovable(); }
        let elements = spring_text::xml::elements(&text);
        let Some(root_index) = elements.iter().position(|e| e.parent.is_none() && e.local() == "entity-mappings") else { return unprovable(); };
        let has = |name: &str| elements.iter().any(|e| e.local() == name);
        if has("delimited-identifiers") { return unprovable(); }
        let defaults = has("schema") || has("catalog");
        out.annotations_unreliable |= defaults || has("xml-mapping-metadata-complete");
        let package = elements.iter().find(|e| e.parent == Some(root_index) && e.local() == "package")
            .map(|e| text[e.tag_end..].split('<').next().unwrap_or("").trim().to_string()).unwrap_or_default();
        for (index, entity) in elements.iter().enumerate().filter(|(_, e)| e.parent == Some(root_index) && e.local() == "entity") {
            let Some(class) = entity.attr("class") else { continue; };
            let written = class.value.trim();
            let fqn = if package.is_empty() || written.contains('.') { written.to_string() } else { format!("{package}.{written}") };
            let literal = elements.iter().find(|e| e.parent == Some(index) && e.local() == "table").and_then(|table| {
                if table.attr("catalog").is_some() { return None; }
                let mut parts = vec![Ident::parse_mapping(table.value("name")?)?];
                match table.value("schema") {
                    Some(schema) => parts.insert(0, Ident::parse_mapping(schema)?),
                    None if defaults => return None,
                    None => {}
                }
                Some((parts, path.clone(), Range::new(position(&text, class.value_at), position(&text, class.value_end))))
            });
            let overrides = elements.iter().any(|e| e.parent == Some(index) && e.local() == "table") || entity.value("metadata-complete") == Some("true");
            if !overrides { continue; }
            let ambiguous = out.entities.contains_key(&fqn);
            out.entities.insert(fqn, if ambiguous { None } else { literal });
        }
    }
    out
}
fn module(path: &str) -> Option<&str> { path.split_once("/src/").map(|(prefix, _)| prefix) }
struct Owner { inputs: Vec<String>, files: BTreeSet<String>, dialect: Option<Dialect>, definitions: BTreeMap<String, Vec<Location>>, at: Instant }
const TTL: Duration = Duration::from_secs(120);
/// Files whose change can alter tables or entity mappings: sources, mapping XML and datasource/build config.
const MAPPING_FILES: &[&str] = &[".java", ".kt", ".xml", ".properties", ".yml", ".yaml", ".conf", ".toml", ".gradle", ".kts"];
#[derive(Default)]
pub(crate) struct State { owners: BTreeMap<String, Owner> }
impl State {
    /// Rebuilds only when the migration list changed, the cache expired or an event invalidated it.
    pub fn warm(&mut self, project: &Project, files: &[String]) {
        let Ok(root) = project.root() else { return; };
        if let Some(owner) = self.owners.get_mut(&root).filter(|owner| owner.inputs == files && owner.at.elapsed() < TTL) {
            owner.at = Instant::now();
            return;
        }
        self.owners.remove(&root);
        let at = Instant::now(); let dialect = detect_dialect(project); let mut definitions: BTreeMap<String, Vec<Location>> = BTreeMap::new(); let mut bytes = 0;
        let mut accepted = BTreeSet::new();
        for path in files {
            if !path.ends_with(".sql") || !path.starts_with(&format!("{root}/")) { continue; }
            // ponytail: time and byte budget keep a partial index (see coverage); index incrementally if large projects need all of it
            if at.elapsed() > Duration::from_millis(200) { break; }
            if host::project_of(path).and_then(|p| p.root().ok()).as_deref() != Some(&root) { continue; }
            let Ok(text) = project.read_file(path) else { continue; }; bytes += text.len();
            if bytes > 2 << 20 { break; }
            let Some(tables) = tables(&text) else { continue; };
            accepted.insert(path.clone());
            for table in tables.into_iter().filter(|table| table.definition) {
                let Some(name) = key(&table.parts, dialect) else { continue; };
                definitions.entry(name).or_default().push(Location { path: path.clone(), range: table.range });
            }
        }
        // A source association is admitted only for a literal (or same-file constant) JPA @Entity/@Table model, or a
        // plainly named @Entity under unmodified Spring Boot naming; no classpath inference.
        let sources = project.find_files("**/*.java", 257).and_then(|mut java| { java.extend(project.find_files("**/*.kt", 257)?); java.retain(|path| path.ends_with(".java") || path.ends_with(".kt")); java.sort(); java.dedup(); Ok(java) });
        if let Ok(java) = sources {
            if java.len() <= 256 {
                let implicit_default = implicit_naming_default(project, &root);
                let xml = xml_mappings(project, &root);
                let mut custom_naming = false;
                let mut explicit: Vec<(Vec<Ident>, String, Range, String)> = Vec::new();
                let mut implicit: Vec<(Vec<Ident>, String, Range, String)> = Vec::new();
                for path in java {
                    if at.elapsed() > Duration::from_millis(400) { break; }
                    if !path.starts_with(&format!("{root}/")) || host::project_of(&path).and_then(|p| p.root().ok()).as_deref() != Some(&root) { continue; }
                    let Ok(text) = project.read_file(&path) else { continue; }; bytes += text.len();
                    if bytes > 4 << 20 { break; }
                    let kotlin = path.ends_with(".kt");
                    custom_naming |= text.contains("NamingStrategy");
                    explicit.extend(entity_tables_in(&text, kotlin).into_iter().map(|(name, range, class)| (name, path.clone(), range, class)));
                    if implicit_default { implicit.extend(implicit_entity_tables_in(&text, kotlin).into_iter().map(|(name, range, class)| (name, path.clone(), range, class))); }
                }
                let admitted = !xml.annotations_unreliable;
                let in_time = at.elapsed() <= Duration::from_millis(400);
                let annotated = explicit.into_iter().filter(|_| admitted && in_time)
                    .chain(implicit.into_iter().filter(|_| admitted && in_time && implicit_default && !custom_naming));
                for (parts, path, range, class) in annotated {
                    if xml.entities.contains_key(&class) { continue; }
                    let Some(name) = key(&parts, dialect) else { continue; };
                    definitions.entry(name).or_default().push(Location { path, range });
                }
                for (parts, path, range) in xml.entities.into_values().flatten() {
                    let Some(name) = key(&parts, dialect) else { continue; };
                    definitions.entry(name).or_default().push(Location { path, range });
                }
            }
        }
        if self.owners.len() >= 8 { self.owners.clear(); }
        self.owners.insert(root, Owner { inputs: files.to_vec(), files: accepted, dialect, definitions, at });
    }
    /// Indexed and listed SQL migrations of a warm root.
    pub fn coverage(&self, root: &str) -> Option<(usize, usize)> {
        self.owners.get(root).map(|owner| (owner.files.len(), owner.inputs.iter().filter(|path| path.ends_with(".sql")).count()))
    }
    /// Drops a root only for a listed migration (or its parent) or a mapping file; build copies stay cached.
    pub fn invalidate(&mut self, path: &str) {
        let parent = format!("{}/", path.trim_end_matches('/'));
        self.owners.retain(|root, owner| {
            let inside = path == root || path.starts_with(&format!("{root}/"));
            let relevant = owner.inputs.iter().any(|input| input == path || input.starts_with(&parent)) || MAPPING_FILES.iter().any(|ext| path.ends_with(ext));
            !(inside && relevant)
        });
    }
    fn owner(&self, doc: &Document) -> Option<&Owner> {
        if doc.language != "sql" { return None; }
        let root = host::project_of(&doc.path).and_then(|p| p.root().ok())?;
        self.owners.get(&root).filter(|owner| owner.at.elapsed() < TTL && owner.files.contains(&doc.path))
    }
    /// The `CREATE TABLE` migrations and mapped entities of `table`; nothing when more than 32 claim it.
    fn targets(owner: &Owner, path: &str, table: &Table) -> Vec<Location> {
        key(&table.parts, owner.dialect).and_then(|name| owner.definitions.get(&name)).filter(|locations| locations.len() <= 32).map(|locations| locations.iter()
            .filter(|location| location.path.ends_with(".sql") || module(path).is_none() || module(path) == module(&location.path))
            .cloned().collect()).unwrap_or_default()
    }
    pub fn definition(&self, doc: &Document, at: Position) -> Vec<Location> {
        let Some(owner) = self.owner(doc) else { return vec![]; };
        let Some(tables) = tables(&doc.text) else { return vec![]; };
        let Some(table) = tables.iter().find(|table| !table.definition && table.range.start <= at && at < table.range.end) else { return vec![]; };
        Self::targets(owner, &doc.path, table)
    }
    /// Gutter markers on table references: the same targets as Go to Definition, merged per line.
    pub fn markers(&self, doc: &Document) -> Vec<LineMarker> {
        let Some(owner) = self.owner(doc) else { return vec![]; };
        let Some(tables) = tables(&doc.text) else { return vec![]; };
        let mut lines: BTreeMap<u32, Vec<Location>> = BTreeMap::new();
        for table in tables.iter().filter(|table| !table.definition) {
            let targets = lines.entry(table.range.start.line).or_default();
            for target in Self::targets(owner, &doc.path, table) {
                if !targets.contains(&target) { targets.push(target); }
            }
        }
        lines.into_iter().flat_map(|(line, targets)| crate::line_markers(line, crate::TABLE_MARKER, "Table's CREATE TABLE migrations and mapped entities", targets)).collect()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_tables_ignore_comments_literals_and_preserve_schema_and_utf16() {
        let text = "-- CREATE TABLE ghost(x int);\nSELECT 'CREATE TABLE fake(x int)'; CREATE TABLE IF NOT EXISTS public.users(id int); UPDATE public.users SET id=1;";
        let found = tables(text).unwrap();
        assert_eq!(found.len(), 2);
        assert!(found[0].definition && !found[1].definition);
        assert_eq!(key(&found[0].parts, None).unwrap(), "public.users");
        assert_eq!(key(&found[1].parts, None).unwrap(), "public.users");
        assert!(tables("select $$ CREATE TABLE hidden(x int) $$;").is_none());
        assert!(tables("CREATE TABLE \"Users\"(id int)").is_some());
        assert!(tables("/* unfinished").is_none());
    }
    #[test]
    fn provider_navigates_owned_migrations_but_never_publishes_rename_or_guesses_schema() {
        let host = openidea_plugin::mock::MockHost::install();
        let create = "/project/sql/V1__init.sql";
        let change = "/project/sql/V2__update.sql";
        host.add_file("sql/V1__init.sql", "CREATE TABLE public.users(id int);");
        host.add_file("sql/V2__update.sql", "UPDATE public.users SET id=2;");
        let project = host.invocation().project().unwrap();
        let mut state = State::default();
        state.warm(&project, &[create.into(), change.into()]);
        let doc = Document { path: change.into(), language: "sql".into(), version: 1, text: "UPDATE public.users SET id=2;".into() };
        let locations = state.definition(&doc, Position::new(0, 10));
        assert_eq!(locations.len(), 1);
        assert_eq!(locations[0].path, create);
        assert_eq!(locations[0].range, Range::new(Position::new(0, 13), Position::new(0, 25)));
        let unqualified = Document { text: "UPDATE users SET id=2;".into(), ..doc.clone() };
        assert!(state.definition(&unqualified, Position::new(0, 8)).is_empty());
        let declaration = Document { path: create.into(), text: "CREATE TABLE public.users(id int);".into(), ..doc.clone() };
        assert!(state.definition(&declaration, Position::new(0, 16)).is_empty());
        let foreign = Document { path: "/other/sql/V2__update.sql".into(), ..doc.clone() };
        assert!(state.definition(&foreign, Position::new(0, 10)).is_empty());
        state.invalidate(create);
        assert!(state.definition(&doc, Position::new(0, 10)).is_empty());
    }

    #[test]
    fn reference_lines_get_one_table_marker_per_create_table_and_entity() {
        let host = openidea_plugin::mock::MockHost::install();
        host.add_file("sql/V1__init.sql", "CREATE TABLE users(id int);\nCREATE TABLE orders(id int);");
        host.add_file("sql/V2__query.sql", "SELECT * FROM users JOIN orders ON 1=1;\nUPDATE users SET id=1; UPDATE users SET id=2;\nCREATE TABLE audit(id int);");
        host.add_file("src/main/java/User.java", "import jakarta.persistence.Entity; import jakarta.persistence.Table; @Entity @Table(name=\"users\") class User {}");
        let files = vec!["/project/sql/V1__init.sql".to_string(), "/project/sql/V2__query.sql".to_string()];
        let doc = Document { path: files[1].clone(), language: "sql".into(), version: 1, text: "SELECT * FROM users JOIN orders ON 1=1;\nUPDATE users SET id=1; UPDATE users SET id=2;\nCREATE TABLE audit(id int);".into() };
        assert!(State::default().markers(&doc).is_empty(), "a cold cache gives no marker");
        let mut state = State::default();
        state.warm(&host.invocation().project().unwrap(), &files);
        let markers = state.markers(&doc);
        assert!(markers.iter().all(|m| m.icon.as_deref() == Some(crate::TABLE_MARKER) && m.line < 2));
        let line = |n: u32| markers.iter().filter(|m| m.line == n).map(|m| m.target.clone().unwrap()).collect::<Vec<_>>();
        let first = line(0);
        assert_eq!(first.iter().map(|l| (l.path.as_str(), l.range.start)).collect::<Vec<_>>(), [
            ("/project/sql/V1__init.sql", Position::new(0, 13)),
            ("/project/src/main/java/User.java", Position::new(0, 104)),
            ("/project/sql/V1__init.sql", Position::new(1, 13)),
        ]);
        assert!(first.iter().all(|l| l.range.start < l.range.end));
        assert_eq!(line(1).len(), 2, "the same table twice on a line lists its targets once");
        assert_eq!(first[..2], state.definition(&doc, Position::new(0, 15))[..]);
    }

    #[test]
    fn table_navigation_with_200_migrations() {
        let host = openidea_plugin::mock::MockHost::install();
        let mut files = vec!["/project/sql/V0__unreadable.sql".to_string()];
        host.add_file("sql/V1__init.sql", "CREATE TABLE users(id int);");
        files.push("/project/sql/V1__init.sql".into());
        for n in 2..=200 {
            host.add_file(&format!("sql/V{n}__touch.sql"), "UPDATE users SET id=1;");
            files.push(format!("/project/sql/V{n}__touch.sql"));
        }
        files.sort();
        let mut state = State::default();
        state.warm(&host.invocation().project().unwrap(), &files);
        let doc = Document { path: "/project/sql/V200__touch.sql".into(), language: "sql".into(), version: 1, text: "UPDATE users SET id=1;".into() };
        let locations = state.definition(&doc, Position::new(0, 8));
        assert_eq!(locations.len(), 1);
        assert_eq!(locations[0].path, "/project/sql/V1__init.sql");
        assert_eq!(state.coverage("/project"), Some((200, 201)));
    }

    #[test]
    fn unrelated_changes_keep_table_navigation() {
        let host = openidea_plugin::mock::MockHost::install();
        host.add_file("sql/V1__init.sql", "CREATE TABLE users(id int);");
        host.add_file("sql/V2__touch.sql", "UPDATE users SET id=1;");
        let files = vec!["/project/sql/V1__init.sql".to_string(), "/project/sql/V2__touch.sql".to_string()];
        let mut state = State::default();
        state.warm(&host.invocation().project().unwrap(), &files);
        let doc = Document { path: files[1].clone(), language: "sql".into(), version: 1, text: "UPDATE users SET id=1;".into() };
        for unrelated in ["/project/target/classes/sql/V1__init.sql", "/project/README.md", "/other/sql/V1__init.sql"] {
            state.invalidate(unrelated);
            assert_eq!(state.definition(&doc, Position::new(0, 8)).len(), 1, "{unrelated}");
        }
        state.invalidate("/project/src/main/java/User.java");
        assert!(state.definition(&doc, Position::new(0, 8)).is_empty());
        state.warm(&host.invocation().project().unwrap(), &files);
        state.invalidate("/project/sql");
        assert!(state.definition(&doc, Position::new(0, 8)).is_empty(), "a removed parent directory drops its migrations");
    }

    #[test]
    fn entity_association_requires_qualified_literal_jpa_mapping() {
        let source = "import jakarta.persistence.Entity; import jakarta.persistence.Table; @Entity @Table(name=\"users\", schema=\"public\") class User {}";
        let found = entity_tables(source);
        assert_eq!(found.len(), 1);
        assert_eq!(key(&found[0].0, None).unwrap(), "public.users");
        assert!(entity_tables(&source.replace("jakarta.persistence.Table", "com.example.Table")).is_empty());
        assert!(entity_tables(&source.replace("name=\"users\"", "name=TABLE_NAME")).is_empty());
        assert!(entity_tables(&source.replace("@Table(name=\"users\", schema=\"public\")", "")).is_empty());
    }

    #[test]
    fn same_file_constants_resolve_table_and_schema_but_foreign_ones_stay_quiet() {
        let head = "import jakarta.persistence.*; ";
        let ok = format!("{head}@Entity @Table(name = User.TABLE, schema = SCHEMA) class User {{ static final String TABLE = \"users\"; public static final String SCHEMA = \"public\"; }}");
        assert_eq!(key(&entity_tables(&ok)[0].0, None).unwrap(), "public.users");
        assert!(entity_tables(&format!("{head}@Entity @Table(name = Other.TABLE) class User {{}}")).is_empty());
        assert!(entity_tables(&format!("{head}@Entity @Table(name = T) class User {{ static final String T = prefix(); }}")).is_empty());
        assert!(entity_tables(&format!("{head}@Entity @Table(name = T) class User {{ String T = \"users\"; }}")).is_empty());
    }

    #[test]
    fn implicit_snake_case_names_are_plain_camel_case_only() {
        assert_eq!(snake_case("OrderLine").as_deref(), Some("order_line"));
        assert_eq!(snake_case("User").as_deref(), Some("user"));
        for ambiguous in ["HTMLPage", "Order2", "UserID", "user", "Ünder"] { assert!(snake_case(ambiguous).is_none(), "{ambiguous}"); }
        let head = "import jakarta.persistence.*; ";
        assert_eq!(key(&implicit_entity_tables(&format!("{head}@Entity class OrderLine {{}}"))[0].0, None).unwrap(), "order_line");
        for skipped in ["@Entity(name = \"X\") class OrderLine {}", "@Entity @Table(name = \"a\") class OrderLine {}", "@Entity class OrderLine extends Base {}", "@Entity abstract class OrderLine {}", "@Entity @Inheritance class OrderLine {}", "class OrderLine {}"] {
            assert!(implicit_entity_tables(&format!("{head}{skipped}")).is_empty(), "{skipped}");
        }
        assert!(implicit_entity_tables("@Entity class OrderLine {}").is_empty());
    }

    #[test]
    fn implicit_names_require_spring_boot_and_no_naming_override() {
        let entity = "import jakarta.persistence.*; @Entity class OrderLine {}";
        let run = |setup: &dyn Fn(&openidea_plugin::mock::MockHost)| {
            let host = openidea_plugin::mock::MockHost::install();
            host.add_file("src/main/resources/db/migration/V1__a.sql", "CREATE TABLE order_line(id int);");
            host.add_file("src/main/resources/db/migration/V2__b.sql", "UPDATE order_line SET id=1;");
            host.add_file("src/main/java/OrderLine.java", entity);
            setup(&host);
            let mut state = State::default();
            let sql = ["/project/src/main/resources/db/migration/V1__a.sql".to_string(), "/project/src/main/resources/db/migration/V2__b.sql".to_string()];
            state.warm(&host.invocation().project().unwrap(), &sql);
            let doc = Document { path: sql[1].clone(), language: "sql".into(), version: 1, text: "UPDATE order_line SET id=1;".into() };
            state.definition(&doc, Position::new(0, 10)).len()
        };
        assert_eq!(run(&|h| h.add_file("pom.xml", "<artifactId>spring-boot-starter-data-jpa</artifactId>")), 2);
        assert_eq!(run(&|_| {}), 1);
        assert_eq!(run(&|h| { h.add_file("pom.xml", "spring-boot"); h.add_file("src/main/resources/application.properties", "spring.jpa.hibernate.naming.physical-strategy=x"); }), 1);
        assert_eq!(run(&|h| { h.add_file("pom.xml", "spring-boot"); h.add_file("src/main/java/Cfg.java", "class Cfg implements PhysicalNamingStrategy {}"); }), 1);
    }

    #[test]
    fn literal_entity_targets_stay_in_the_sql_source_module() {
        let host = openidea_plugin::mock::MockHost::install();
        host.add_file("a/src/main/resources/db/migration/V1__init.sql", "CREATE TABLE public.users(id int);");
        host.add_file("a/src/main/resources/db/migration/V2__update.sql", "UPDATE public.users SET id=2;");
        let entity = "import jakarta.persistence.*; @Entity @Table(name=\"users\", schema=\"public\") class User {}";
        host.add_file("a/src/main/java/User.java", entity);
        host.add_file("b/src/main/java/User.java", entity);
        let mut state = State::default();
        state.warm(&host.invocation().project().unwrap(), &["/project/a/src/main/resources/db/migration/V1__init.sql".into(), "/project/a/src/main/resources/db/migration/V2__update.sql".into()]);
        let doc = Document { path: "/project/a/src/main/resources/db/migration/V2__update.sql".into(), language: "sql".into(), version: 1, text: "UPDATE public.users SET id=2;".into() };
        let locations = state.definition(&doc, Position::new(0, 10));
        assert_eq!(locations.len(), 2);
        assert!(locations.iter().any(|location| location.path == "/project/a/src/main/java/User.java"));
        assert!(!locations.iter().any(|location| location.path.contains("/b/")));
        assert!(entity_tables(&entity.replace("import jakarta.persistence.*;", "import jakarta.persistence.*; import custom.*;")).is_empty());
    }

    fn ids(sql: &str) -> Vec<Ident> { tables(sql).unwrap().remove(0).parts }

    #[test]
    fn quoted_identifiers_tokenize_with_escapes_in_every_dialect_style() {
        assert_eq!(ids("CREATE TABLE \"a\"\"b\".\"C\"(x int)"), vec![Ident { text: "a\"b".into(), quote: Some('"') }, Ident { text: "C".into(), quote: Some('"') }]);
        assert_eq!(ids("CREATE TABLE `a``b`(x int)")[0], Ident { text: "a`b".into(), quote: Some('`') });
        assert_eq!(ids("CREATE TABLE [dbo].[a]]b](x int)")[1], Ident { text: "a]b".into(), quote: Some('[') });
        assert_eq!(tables("SELECT 1 FROM \"from\"; CREATE TABLE \"table\"(x int)").unwrap().len(), 2);
        for broken in ["CREATE TABLE \"x(id int)", "CREATE TABLE `x", "CREATE TABLE [x", "CREATE TABLE \"\"(id int)"] { assert!(tables(broken).is_none(), "{broken}"); }
    }

    #[test]
    fn identifier_keys_fold_unquoted_per_dialect_and_keep_quoted_exact() {
        let plain = |t: &str| Ident::plain(t);
        let quoted = |t: &str, q: char| Ident { text: t.into(), quote: Some(q) };
        assert_eq!(key(&[plain("Users")], Some(Dialect::Postgres)).unwrap(), "users");
        assert_eq!(key(&[plain("Users")], Some(Dialect::H2)).unwrap(), "USERS");
        assert_eq!(key(&[plain("Users")], Some(Dialect::Oracle)).unwrap(), "USERS");
        assert_eq!(key(&[quoted("Users", '"')], Some(Dialect::Postgres)).unwrap(), "Users");
        assert_eq!(key(&[quoted("USERS", '"')], Some(Dialect::H2)), key(&[plain("users")], Some(Dialect::H2)));
        assert_ne!(key(&[quoted("Users", '"')], Some(Dialect::Postgres)), key(&[plain("Users")], Some(Dialect::Postgres)));
        assert_eq!(key(&[quoted("Users", '[')], Some(Dialect::SqlServer)).unwrap(), "users");
        assert_eq!(key(&[quoted("Users", '`')], Some(Dialect::MySql)).unwrap(), "Users");
        assert!(key(&[quoted("Users", '"')], None).is_none());
        assert!(key(&[quoted("Users", '`')], Some(Dialect::Postgres)).is_none());
        assert!(key(&[quoted("Users", '[')], Some(Dialect::Postgres)).is_none());
        assert!(key(&[quoted("Users", '"')], Some(Dialect::MySql)).is_none());
        assert_eq!(key(&[plain("Users")], None).unwrap(), "users");
    }

    #[test]
    fn dialect_needs_explicit_unambiguous_url_or_driver_evidence() {
        let run = |files: &[(&str, &str)]| {
            let host = openidea_plugin::mock::MockHost::install();
            for (path, text) in files { host.add_file(path, text); }
            detect_dialect(&host.invocation().project().unwrap())
        };
        assert_eq!(run(&[("src/main/resources/application.properties", "spring.datasource.url=jdbc:postgresql://db/app")]), Some(Dialect::Postgres));
        assert_eq!(run(&[("flyway.conf", "flyway.url=jdbc:mariadb://db/app")]), Some(Dialect::MySql));
        assert_eq!(run(&[("src/main/resources/application.yml", "driver-class-name: org.h2.Driver")]), Some(Dialect::H2));
        assert_eq!(run(&[("application.properties", "spring.datasource.url=${DB_URL}")]), None);
        assert_eq!(run(&[("application.properties", "jdbc:postgresql://x"), ("flyway.conf", "flyway.url=jdbc:h2:mem:x")]), None);
        assert_eq!(run(&[("application.properties", "spring.datasource.url=jdbc:h2:mem:x;MODE=PostgreSQL")]), None);
    }

    #[test]
    fn quoted_entity_names_match_quoted_sql_only_with_explicit_dialect() {
        let entity = "import jakarta.persistence.*; @Entity @Table(name = \"\\\"Users\\\"\", schema = \"[Sales]\") class User {}";
        let parts = &entity_tables(entity)[0].0;
        assert_eq!(parts[1], Ident { text: "Users".into(), quote: Some('"') });
        assert_eq!(parts[0].quote, Some('['));
        assert!(entity_tables(&entity.replace("\\\"Users\\\"", "\\\"Us\\\"ers\\\"")).is_empty());
        let run = |url: Option<&str>, sql: &str| {
            let host = openidea_plugin::mock::MockHost::install();
            let java = "import jakarta.persistence.*; @Entity @Table(name = \"\\\"Users\\\"\") class User {}";
            host.add_file("src/main/resources/db/migration/V1__a.sql", "CREATE TABLE \"Users\"(id int);");
            host.add_file("src/main/resources/db/migration/V2__b.sql", sql);
            host.add_file("src/main/java/User.java", java);
            if let Some(url) = url { host.add_file("src/main/resources/application.properties", url); }
            let files = ["/project/src/main/resources/db/migration/V1__a.sql".to_string(), "/project/src/main/resources/db/migration/V2__b.sql".to_string()];
            let mut state = State::default();
            state.warm(&host.invocation().project().unwrap(), &files);
            let doc = Document { path: files[1].clone(), language: "sql".into(), version: 1, text: sql.into() };
            state.definition(&doc, Position::new(0, 10)).len()
        };
        let pg = Some("spring.datasource.url=jdbc:postgresql://db/app");
        assert_eq!(run(pg, "UPDATE \"Users\" SET id=1;"), 2);
        assert_eq!(run(pg, "UPDATE users SET id=1;"), 0);
        assert_eq!(run(pg, "UPDATE \"users\" SET id=1;"), 0);
        assert_eq!(run(None, "UPDATE \"Users\" SET id=1;"), 0);
        assert_eq!(run(Some("spring.datasource.url=jdbc:h2:mem:t"), "UPDATE \"Users\" SET id=1;"), 2);
    }


    #[test]
    fn kotlin_entities_resolve_named_args_constants_schema_and_implicit_names() {
        let head = "package app\nimport jakarta.persistence.*\n";
        let k = |body: &str| entity_tables_in(&format!("{head}{body}"), true);
        let found = k("@Entity @Table(name = \"users\", schema = \"public\") data class User(@Id val id: Long)");
        assert_eq!(key(&found[0].0, None).unwrap(), "public.users");
        assert_eq!(found[0].2, "app.User");
        let constant = k("@Entity @Table(name = User.TABLE) class User { companion object { const val TABLE = \"users\" } }");
        assert_eq!(key(&constant[0].0, None).unwrap(), "users");
        assert!(k("@Entity @Table(name = \"$prefix\") class User").is_empty());
        assert!(k("@Entity @Table(name = Other.TABLE) class User").is_empty());
        assert!(entity_tables_in("@Entity @Table(name = \"a\") class User", true).is_empty());
        let implicit = |body: &str| implicit_entity_tables_in(&format!("{head}{body}"), true);
        assert_eq!(key(&implicit("@Entity class OrderLine(@Id val id: Long)")[0].0, None).unwrap(), "order_line");
        for skipped in ["@Entity class OrderLine : Base()", "@Entity @Table(name = \"a\") class OrderLine", "@Entity(name = \"X\") class OrderLine", "@Entity class HTMLPage"] {
            assert!(implicit(skipped).is_empty(), "{skipped}");
        }
    }

    fn xml_state(files: &[(&str, &str)], java: &str) -> Vec<String> {
        let host = openidea_plugin::mock::MockHost::install();
        host.add_file("src/main/resources/db/migration/V1__a.sql", "CREATE TABLE xml_users(id int); CREATE TABLE ann_users(id int);");
        host.add_file("src/main/resources/db/migration/V2__b.sql", "SELECT * FROM xml_users; SELECT * FROM ann_users;");
        host.add_file("src/main/java/app/User.java", java);
        for (path, text) in files { host.add_file(path, text); }
        let mut state = State::default();
        let sql = ["/project/src/main/resources/db/migration/V1__a.sql".to_string(), "/project/src/main/resources/db/migration/V2__b.sql".to_string()];
        state.warm(&host.invocation().project().unwrap(), &sql);
        let doc = Document { path: sql[1].clone(), language: "sql".into(), version: 1, text: "SELECT * FROM xml_users; SELECT * FROM ann_users;".into() };
        let mut out: Vec<String> = state.definition(&doc, Position::new(0, 16)).into_iter().chain(state.definition(&doc, Position::new(0, 41))).map(|l| l.path.rsplit('/').next().unwrap().to_string()).filter(|name| !name.ends_with(".sql")).collect();
        out.sort();
        out
    }

    const ANNOTATED: &str = "package app; import jakarta.persistence.*; @Entity @Table(name = \"ann_users\") class User {}";
    const ORM: &str = "<entity-mappings><package>app</package><entity class=\"User\"><table name=\"xml_users\"/></entity></entity-mappings>";

    #[test]
    fn xml_mapping_overrides_the_annotation_table_of_the_same_class() {
        assert_eq!(xml_state(&[], ANNOTATED), ["User.java"]);
        let both = xml_state(&[("src/main/resources/META-INF/orm.xml", ORM)], ANNOTATED);
        assert_eq!(both, ["orm.xml"]);
        let listed = xml_state(&[("src/main/resources/META-INF/persistence.xml", "<persistence><persistence-unit name=\"p\"><mapping-file>mappings/x.xml</mapping-file></persistence-unit></persistence>"),
            ("src/main/resources/mappings/x.xml", &ORM.replace("<package>app</package>", "").replace("\"User\"", "\"app.User\"").replace("<table name=\"xml_users\"/>", "<table name=\"xml_users\" schema=\"\"/>"))], ANNOTATED);
        assert_eq!(listed, [] as [&str; 0]);
    }

    #[test]
    fn xml_mapping_semantics_that_change_meaning_are_refused() {
        let unlisted = [("src/main/resources/META-INF/persistence.xml", "<persistence><persistence-unit name=\"p\"><mapping-file>missing.xml</mapping-file></persistence-unit></persistence>")];
        assert_eq!(xml_state(&unlisted, ANNOTATED), [] as [&str; 0]);
        let two_units = [("src/main/resources/META-INF/persistence.xml", "<persistence><persistence-unit name=\"a\"/><persistence-unit name=\"b\"/></persistence>")];
        assert_eq!(xml_state(&two_units, ANNOTATED), [] as [&str; 0]);
        let complete = ORM.replace("<package>", "<persistence-unit-metadata><xml-mapping-metadata-complete/></persistence-unit-metadata><package>");
        assert_eq!(xml_state(&[("src/main/resources/META-INF/orm.xml", &complete)], ANNOTATED), ["orm.xml"]);
        let delimited = ORM.replace("<package>", "<persistence-unit-metadata><persistence-unit-defaults><delimited-identifiers/></persistence-unit-defaults></persistence-unit-metadata><package>");
        assert_eq!(xml_state(&[("src/main/resources/META-INF/orm.xml", &delimited)], ANNOTATED), [] as [&str; 0]);
        let default_schema = ORM.replace("<package>", "<schema>s</schema><package>");
        assert_eq!(xml_state(&[("src/main/resources/META-INF/orm.xml", &default_schema)], ANNOTATED), [] as [&str; 0]);
        let catalog = ORM.replace("name=\"xml_users\"", "name=\"xml_users\" catalog=\"c\"");
        assert_eq!(xml_state(&[("src/main/resources/META-INF/orm.xml", &catalog)], ANNOTATED), [] as [&str; 0]);
        let duplicate = ORM.replace("</entity-mappings>", "<entity class=\"app.User\"><table name=\"other\"/></entity></entity-mappings>");
        assert_eq!(xml_state(&[("src/main/resources/META-INF/orm.xml", &duplicate)], ANNOTATED), [] as [&str; 0]);
        let metadata_complete = "<entity-mappings><entity class=\"app.User\" metadata-complete=\"true\"/></entity-mappings>";
        assert_eq!(xml_state(&[("src/main/resources/META-INF/orm.xml", metadata_complete)], ANNOTATED), [] as [&str; 0]);
    }
}
