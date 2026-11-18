//! A source-only literal Gradle adapter, not a Groovy/Kotlin/effective-model evaluator.
use super::*;
#[derive(Clone)]
struct Token {
    value: String,
    start: usize,
    end: usize,
    quote: Option<u8>,
}
fn lex(text: &str) -> Option<Vec<Token>> {
    if text.len() > MAX_SOURCE
        || text.contains('\r') && !text.replace("\r\n", "").chars().all(|c| c != '\r')
    {
        return None;
    }
    let b = text.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    while i < b.len() {
        if b[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if b[i..].starts_with(b"//") {
            i += 2;
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        let start = i;
        let (value, quote) = if matches!(b[i], b'\'' | b'"') {
            let q = b[i];
            i += 1;
            let a = i;
            while i < b.len() && b[i] != q {
                if matches!(b[i], b'\\' | b'$' | b'\r' | b'\n') || b[i] < 32 {
                    return None;
                }
                i += 1;
            }
            if i == b.len() {
                return None;
            }
            let value = text.get(a..i)?.to_owned();
            i += 1;
            (value, Some(q))
        } else if b[i].is_ascii_alphanumeric() || b[i] == b'_' {
            i += 1;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            (text[start..i].into(), None)
        } else if b"{}().=<>".contains(&b[i]) {
            i += 1;
            (text[start..i].into(), None)
        } else {
            return None;
        };
        out.push(Token {
            value,
            start,
            end: i,
            quote,
        });
        if out.len() > 2048 {
            return None;
        }
    }
    Some(out)
}
enum Dep {
    Coord(String),
    Platform(String),
    Ref(String),
}
struct Parser<'a> {
    text: &'a str,
    tokens: Vec<Token>,
    at: usize,
    kotlin: bool,
}
impl Parser<'_> {
    fn peek(&self, v: &str) -> bool {
        self.tokens
            .get(self.at)
            .is_some_and(|t| t.quote.is_none() && t.value == v)
    }
    fn take(&mut self, v: &str) -> Option<()> {
        if !self.peek(v) {
            return None;
        }
        self.at += 1;
        Some(())
    }
    fn string(&mut self) -> Option<String> {
        let t = self.tokens.get(self.at)?;
        if t.quote.is_none() || self.kotlin && t.quote != Some(b'"') {
            return None;
        }
        let v = t.value.clone();
        self.at += 1;
        Some(v)
    }
    fn call_string(&mut self) -> Option<String> {
        if self.peek("(") {
            self.take("(")?;
            let v = self.string()?;
            self.take(")")?;
            Some(v)
        } else if !self.kotlin {
            self.string()
        } else {
            None
        }
    }
    fn one_line(&self, from: usize) -> Option<()> {
        let a = self.tokens.get(from)?.start;
        let z = self.tokens.get(self.at.checked_sub(1)?)?.end;
        (!self.text[a..z].contains('\n')).then_some(())
    }
    fn dependency(&mut self) -> Option<Dep> {
        let paren = self.peek("(");
        if paren {
            self.take("(")?;
        } else if self.kotlin {
            return None;
        }
        let dep = if self.peek("platform") {
            self.take("platform")?;
            Dep::Platform(self.call_string()?)
        } else if self.peek("libs") {
            let mut accessor = String::from("libs");
            self.at += 1;
            while self.peek(".") {
                self.at += 1;
                let token = self.tokens.get(self.at)?;
                if token.quote.is_some() {
                    return None;
                }
                accessor.push('.');
                accessor.push_str(&token.value);
                self.at += 1;
            }
            Dep::Ref(accessor)
        } else {
            Dep::Coord(self.string()?)
        };
        if paren {
            self.take(")")?;
        }
        Some(dep)
    }
    fn exact(&mut self, values: &[&str]) -> Option<()> {
        for v in values {
            self.take(v)?;
        }
        Some(())
    }
}
pub(super) fn proof(doc: &Document) -> Option<Proof> {
    let kotlin = match (doc.path.rsplit('/').next()?, doc.language.as_str()) {
        ("build.gradle", "groovy") => false,
        ("build.gradle.kts", "kotlin") => true,
        _ => return None,
    };
    let mut p = Parser {
        text: &doc.text,
        tokens: lex(&doc.text)?,
        at: 0,
        kotlin,
    };
    p.exact(&["plugins", "{"])?;
    let mut plugins = BTreeSet::new();
    let mut boot = None;
    while !p.peek("}") {
        let from = p.at;
        let id = if kotlin && p.peek("java") {
            p.take("java")?;
            "java".into()
        } else {
            p.take("id")?;
            p.call_string()?
        };
        if !plugins.insert(id.clone()) {
            return None;
        }
        match id.as_str() {
            "java" => {}
            "org.springframework.boot" => {
                p.take("version")?;
                let v = p.string()?;
                if !ga_version(&v) {
                    return None;
                }
                boot = Some(v);
            }
            "io.spring.dependency-management" => {
                p.take("version")?;
                if p.string()? != "1.1.7" {
                    return None;
                }
            }
            _ => return None,
        };
        p.one_line(from)?;
    }
    p.take("}")?;
    if !plugins.contains("java") {
        return None;
    }
    let managed_by_plugin = plugins.contains("io.spring.dependency-management");
    let boot = boot?;
    let mut platform = None;
    let mut refs = Vec::new();
    let mut seen = BTreeSet::new();
    let mut existing = BTreeSet::new();
    let mut insert = None;
    let mut inner = None;
    while p.at < p.tokens.len() {
        let key = p.tokens.get(p.at)?.value.clone();
        if p.tokens[p.at].quote.is_some() || !seen.insert(key.clone()) {
            return None;
        }
        let from = p.at;
        p.at += 1;
        match key.as_str() {
            "group" | "version" | "description" => {
                p.take("=")?;
                let v = p.string()?;
                if key != "description" && !literal(&v) {
                    return None;
                }
                p.one_line(from)?;
            }
            "repositories" => {
                p.exact(&["{", "mavenCentral", "(", ")", "}"])?;
            }
            "java" => {
                p.exact(&[
                    "{",
                    "toolchain",
                    "{",
                    "languageVersion",
                    "=",
                    "JavaLanguageVersion",
                    ".",
                    "of",
                    "(",
                ])?;
                let token = p.tokens.get(p.at)?;
                if token.quote.is_some() {
                    return None;
                }
                let number = &token.value;
                if number.len() > 2
                    || !number.bytes().all(|b| b.is_ascii_digit())
                    || number.parse::<u32>().ok()? < 8
                {
                    return None;
                }
                p.at += 1;
                p.exact(&[")", "}", "}"])?;
            }
            "dependencies" => {
                p.take("{")?;
                while !p.peek("}") {
                    let from = p.at;
                    let config = p.tokens.get(p.at)?;
                    if config.quote.is_some()
                        || !matches!(
                            config.value.as_str(),
                            "implementation"
                                | "runtimeOnly"
                                | "compileOnly"
                                | "testImplementation"
                                | "testRuntimeOnly"
                                | "annotationProcessor"
                        )
                    {
                        return None;
                    }
                    let config_name = config.value.clone();
                    p.at += 1;
                    let dep = p.dependency()?;
                    p.one_line(from)?;
                    match dep {
                        Dep::Ref(accessor) => refs.push(accessor),
                        Dep::Platform(coord) => {
                            let parts = coord.split(':').collect::<Vec<_>>();
                            if config_name != "implementation"
                                || platform.is_some()
                                || parts.len() != 3
                                || parts[0] != "org.springframework.boot"
                                || parts[1] != "spring-boot-dependencies"
                                || !ga_version(parts[2])
                            {
                                return None;
                            }
                            platform = Some(parts[2].to_owned());
                        }
                        Dep::Coord(coord) => {
                            let parts = coord.split(':').collect::<Vec<_>>();
                            if !(parts.len() == 2 || parts.len() == 3)
                                || parts.iter().any(|v| !literal(v))
                                || !existing.insert((parts[0].into(), parts[1].into()))
                            {
                                return None;
                            }
                        }
                    }
                    let at = p.tokens[from].start;
                    let line = doc.text[..at].rfind('\n').map_or(0, |i| i + 1);
                    let indent = &doc.text[line..at];
                    if indent.is_empty() || !indent.chars().all(|c| matches!(c, ' ' | '\t')) {
                        return None;
                    }
                    if inner.is_none() {
                        inner = Some(indent.to_owned());
                    }
                }
                let close = p.tokens.get(p.at)?.start;
                let line = doc.text[..close].rfind('\n').map_or(0, |i| i + 1);
                let indent = &doc.text[line..close];
                if !indent.chars().all(|c| matches!(c, ' ' | '\t')) {
                    return None;
                }
                insert = Some(line);
                if inner.is_none() {
                    inner = Some(format!("{indent}    "));
                }
                p.take("}")?;
            }
            "tasks" => {
                p.take(".")?;
                if kotlin {
                    p.exact(&["withType", "<", "Test", ">"])?;
                } else {
                    p.exact(&["named", "("])?;
                    if p.string()? != "test" {
                        return None;
                    }
                    p.take(")")?;
                }
                p.exact(&["{", "useJUnitPlatform", "(", ")", "}"])?;
            }
            _ => return None,
        }
    }
    let at = insert?;
    let management = if managed_by_plugin {
        if platform.is_some() {
            return None;
        }
        "the Spring Boot and dependency-management plugins"
    } else {
        if platform.as_deref() != Some(&boot) {
            return None;
        }
        "the Spring Boot plugin and the spring-boot-dependencies platform"
    };
    Some(Proof {
        boot,
        management,
        refs,
        at,
        end: at,
        before: String::new(),
        after: String::new(),
        inner: inner?,
        existing,
        newline: if doc.text.contains("\r\n") {
            "\r\n"
        } else {
            "\n"
        },
        gradle: Some(kotlin),
    })
}
pub(super) fn fresh(doc: &Document, root: &str) -> Result<(), Error> {
    let project = host::project_of(&doc.path).ok_or(Error::Stale)?;
    let kotlin = doc.path.ends_with(".kts");
    let name = if kotlin {
        "build.gradle.kts"
    } else {
        "build.gradle"
    };
    if doc.path != format!("{root}/{name}") {
        return Err("Gradle adapter supports only the exact project-root build file.".into());
    }
    for name in [
        if kotlin {
            "build.gradle"
        } else {
            "build.gradle.kts"
        },
        "pom.xml",
        "gradle.properties",
        "buildSrc",
        "build-logic",
        "init.gradle",
        "init.gradle.kts",
        ".mvn/maven.config",
        ".mvn/extensions.xml",
    ] {
        if project.exists(&format!("{root}/{name}"))? {
            return Err("Competing build, version catalog, properties, init/build logic or Maven configuration is unsupported.".into());
        }
    }
    let p = proof(doc).ok_or(Error::Stale)?;
    catalog(&p, root)?;
    for (name, expected) in [
        ("settings.gradle", !kotlin),
        ("settings.gradle.kts", kotlin),
    ] {
        let path = format!("{root}/{name}");
        if !project.exists(&path)? {
            continue;
        }
        if !expected {
            return Err("Competing Gradle settings dialect is unsupported.".into());
        }
        let text = project.read_file(&path)?;
        if text.len() > 2048 {
            return Err(Error::Stale);
        }
        let mut p = Parser {
            text: &text,
            tokens: lex(&text).ok_or(Error::Stale)?,
            at: 0,
            kotlin,
        };
        p.exact(&["rootProject", ".", "name", "="])
            .ok_or(Error::Stale)?;
        let value = p.string().ok_or(Error::Stale)?;
        p.one_line(0).ok_or(Error::Stale)?;
        if !literal(&value) || p.at != p.tokens.len() {
            return Err("Only a literal rootProject.name settings file is supported; plugin/repository/module rules are not inferred.".into());
        }
    }
    Ok(())
}

pub(super) struct Catalog {
    pub text: String,
    pub path: String,
    pub used: BTreeSet<(String, String)>,
    accessors: BTreeMap<String, (String, String)>,
    insert: usize,
    has_libraries: bool,
}
fn accessor(alias: &str) -> String {
    alias.replace(['-', '_'], ".")
}
fn catalog_key(key: &str) -> bool {
    !key.is_empty() && key.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}
fn quoted(value: &str) -> Option<&str> {
    let v = value.trim().strip_prefix('"')?.strip_suffix('"')?;
    (!v.contains(['"', '\\', '$'])).then_some(v)
}
/// Literal `gradle/libs.versions.toml` only: versions/libraries/plugins tables of one-line entries.
pub(super) fn catalog(p: &Proof, root: &str) -> Result<Option<Catalog>, Error> {
    let project = host::project_of(&format!("{root}/build.gradle")).ok_or(Error::Stale)?;
    let path = format!("{root}/gradle/libs.versions.toml");
    if !project.exists(&path)? {
        if p.refs.is_empty() {
            return Ok(None);
        }
        return Err("Build references a version catalog that is not present.".into());
    }
    let text = project.read_file(&path)?;
    let refuse = || Error::from("Only a literal one-line versions/libraries/plugins libs.versions.toml is supported; no catalog model is inferred.");
    if text.len() > MAX_SOURCE || text.contains(['\r']) && !text.replace("\r\n", "").chars().all(|c| c != '\r') {
        return Err(refuse());
    }
    let mut section = "";
    let mut seen = BTreeSet::new();
    let mut accessors = BTreeMap::new();
    let mut insert = None;
    let mut offset = 0;
    for raw in text.split_inclusive('\n') {
        let line = raw.trim();
        let end = offset + raw.len();
        offset = end;
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            if !matches!(name, "versions" | "libraries" | "plugins") || !seen.insert(name.to_owned()) {
                return Err(refuse());
            }
            section = match name {
                "versions" => "versions",
                "libraries" => "libraries",
                _ => "plugins",
            };
            if section == "libraries" {
                insert = Some(end);
            }
            continue;
        }
        let (key, value) = line.split_once('=').ok_or_else(refuse)?;
        let key = key.trim();
        if section.is_empty() || !catalog_key(key) || value.contains(['\\', '$']) {
            return Err(refuse());
        }
        match section {
            "versions" => {
                quoted(value).ok_or_else(refuse)?;
            }
            "libraries" => {
                let body = value.trim().strip_prefix('{').and_then(|v| v.strip_suffix('}')).ok_or_else(refuse)?;
                let mut module = None;
                for field in body.split(',') {
                    let (k, v) = field.split_once('=').ok_or_else(refuse)?;
                    let v = quoted(v).ok_or_else(refuse)?;
                    match k.trim() {
                        "module" if module.is_none() => module = Some(v),
                        "version" | "version.ref" => {}
                        _ => return Err(refuse()),
                    }
                }
                let (g, a) = module.and_then(|m| m.split_once(':')).ok_or_else(refuse)?;
                if !literal(g) || !literal(a) || accessors.insert(accessor(key), (g.to_owned(), a.to_owned())).is_some() {
                    return Err(refuse());
                }
                insert = Some(end);
            }
            _ => {
                if !value.trim().starts_with('{') && quoted(value).is_none() {
                    return Err(refuse());
                }
            }
        }
    }
    let mut used = BTreeSet::new();
    for r in &p.refs {
        let found = r.strip_prefix("libs.").and_then(|a| accessors.get(a)).ok_or_else(refuse)?;
        used.insert(found.clone());
    }
    let has_libraries = insert.is_some();
    Ok(Some(Catalog { insert: insert.unwrap_or(text.len()), text, path, used, accessors, has_libraries }))
}
impl Catalog {
    /// Returns the catalog line and the `libs.` accessor for a starter, or None when the alias is taken by another module.
    pub(super) fn entry(&self, artifact: &str) -> Option<(String, String)> {
        let accessor = accessor(artifact);
        match self.accessors.get(&accessor) {
            Some((g, a)) if g == "org.springframework.boot" && a == artifact => Some((String::new(), accessor)),
            Some(_) => None,
            None => Some((format!("{artifact} = {{ module = \"org.springframework.boot:{artifact}\" }}"), accessor)),
        }
    }
    /// Edit offset and text for the added catalog lines.
    pub(super) fn insertion(&self, lines: &[String], newline: &str) -> (usize, String) {
        let mut out = String::new();
        if !self.text.is_empty() && !self.text[..self.insert].ends_with('\n') {
            out.push_str(newline);
        }
        if !self.has_libraries {
            if !self.text.is_empty() {
                out.push_str(newline);
            }
            out.push_str(&format!("[libraries]{newline}"));
        }
        for line in lines {
            out.push_str(line);
            out.push_str(newline);
        }
        (self.insert, out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::add_starters::tests::{COORDS, META};
    use openidea_plugin::{file_actions::FileActions, mock::MockHost, text::offset};
    const GROOVY: &str = "plugins {\n    id 'java'\n    id 'org.springframework.boot' version '3.5.16'\n    id 'io.spring.dependency-management' version '1.1.7'\n}\n// 😀 preserve exactly\ngroup = 'com.example'\nversion = '1.0.0'\njava {\n    toolchain {\n        languageVersion = JavaLanguageVersion.of(17)\n    }\n}\nrepositories { mavenCentral() }\ndependencies {\n    implementation 'org.springframework.boot:spring-boot-starter'\n}\ntasks.named('test') {\n    useJUnitPlatform()\n}\n";
    const KOTLIN: &str = "plugins {\n    java\n    id(\"org.springframework.boot\") version \"3.5.16\"\n    id(\"io.spring.dependency-management\") version \"1.1.7\"\n}\n// 😀 preserve exactly\ngroup = \"com.example\"\nversion = \"1.0.0\"\nrepositories { mavenCentral() }\ndependencies {\n    implementation(\"org.springframework.boot:spring-boot-starter\")\n}\ntasks.withType<Test> {\n    useJUnitPlatform()\n}\n";
    fn setup(text: &str, kotlin: bool) -> (MockHost, Launcher, Document) {
        let h = MockHost::install().with_capabilities(&[
            "storage",
            "project.read",
            "editor.read",
            "net:start.spring.io",
        ]);
        let name = if kotlin {
            "build.gradle.kts"
        } else {
            "build.gradle"
        };
        let path = format!("/project/{name}");
        let language = if kotlin { "kotlin" } else { "groovy" };
        h.add_file(name, text);
        h.open_editor(&path, language, text, &[]);
        h.on_fetch(|req| {
            Ok(openidea_plugin::HttpResponse {
                status: 200,
                body: if req.url.contains("metadata/client") {
                    META.as_bytes().to_vec()
                } else {
                    COORDS.as_bytes().to_vec()
                },
                ..Default::default()
            })
        });
        let version = h.invocation().editor().unwrap().document().unwrap().version;
        (
            h,
            Launcher::default(),
            Document {
                path,
                language: language.into(),
                text: text.into(),
                version,
            },
        )
    }
    fn ready(text: &str, kotlin: bool) -> (MockHost, Launcher, Document) {
        let (h, mut l, d) = setup(text, kotlin);
        l.bound_cache(&h.invocation())
            .unwrap();
        (h, l, d)
    }
    fn resolve(h: &MockHost, l: &mut Launcher, d: &Document, ids: &[&str]) -> FileAction {
        let a = l.file_actions(d, Range::default()).remove(0);
        h.answer_next_prompt(Some(ids));
        h.answer_next_prompt(Some(&["yes"]));
        l.resolve_file_action(d, a).unwrap()
    }
    fn after(d: &Document, a: &FileAction) -> String {
        assert!(a.operations.is_empty());
        assert_eq!(a.edits.len(), 1);
        assert_eq!(a.edits[0].0, d.path);
        assert_eq!(a.edits[0].1.len(), 1);
        let e = &a.edits[0].1[0];
        let from = offset(&d.text, e.range.start).unwrap();
        let to = offset(&d.text, e.range.end).unwrap();
        assert_eq!(from, to);
        let mut out = d.text.clone();
        out.replace_range(from..to, &e.text);
        out
    }
    #[test]
    fn groovy_and_kotlin_exact_preview_edits_preserve_utf16_crlf_and_scope() {
        for kotlin in [false, true] {
            let original = if kotlin { KOTLIN } else { GROOVY }.replace('\n', "\r\n");
            let (h, mut l, d) = ready(&original, kotlin);
            let fetches = h.fetches();
            let a = resolve(&h, &mut l, &d, &["web", "data-jpa"]);
            let insertion = if kotlin {
                "    implementation(\"org.springframework.boot:spring-boot-starter-web\")\r\n    runtimeOnly(\"org.springframework.boot:spring-boot-starter-data-jpa\")\r\n"
            } else {
                "    implementation 'org.springframework.boot:spring-boot-starter-web'\r\n    runtimeOnly 'org.springframework.boot:spring-boot-starter-data-jpa'\r\n"
            };
            let at = d.text.find("}\r\ntasks.").unwrap();
            let mut expected = d.text.clone();
            expected.insert_str(at, insertion);
            assert_eq!(after(&d, &a), expected);
            assert_eq!(h.editor_text(), original);
            assert_eq!(h.fetches(), fetches);
            assert!(h.process_runs().is_empty());
            assert!(h.opened_projects().is_empty());
        }
    }
    #[test]
    fn advertised_test_scope_maps_to_test_implementation_without_versions() {
        for kotlin in [false, true] {
            let text = if kotlin { KOTLIN } else { GROOVY };
            let (h, mut l, d) = setup(text, kotlin);
            h.on_fetch(|req| {
                Ok(openidea_plugin::HttpResponse {
                    status: 200,
                    body: if req.url.contains("metadata/client") {
                        META.as_bytes().to_vec()
                    } else {
                        COORDS
                            .replace("\"scope\":\"runtime\"", "\"scope\":\"test\"")
                            .into_bytes()
                    },
                    ..Default::default()
                })
            });
            l.bound_cache(&h.invocation())
                .unwrap();
            let a = resolve(&h, &mut l, &d, &["data-jpa"]);
            let new = after(&d, &a);
            assert!(new.contains(if kotlin {
                "testImplementation(\"org.springframework.boot:spring-boot-starter-data-jpa\")"
            } else {
                "testImplementation 'org.springframework.boot:spring-boot-starter-data-jpa'"
            }));
        }
    }
    #[test]
    fn cancelled_picker_and_full_preview_leave_gradle_bytes_unchanged() {
        for kotlin in [false, true] {
            for preview in [false, true] {
                let text = if kotlin { KOTLIN } else { GROOVY };
                let (h, mut l, d) = ready(text, kotlin);
                let a = l.file_actions(&d, Range::default()).remove(0);
                if preview {
                    h.answer_next_prompt(Some(&["web"]));
                }
                h.answer_next_prompt(None);
                assert!(l.resolve_file_action(&d, a).is_err());
                assert_eq!(h.editor_text(), text);
            }
        }
    }
    #[test]
    fn stale_source_language_owner_metadata_selector_and_settings_refuse() {
        for mode in 0..7 {
            let (h, mut l, mut d) = ready(GROOVY, false);
            let a = l.file_actions(&d, Range::default()).remove(0);
            match mode {
                0 => d.version += 1,
                1 => d.language = "plaintext".into(),
                2 => h.open_editor("/project/build.gradle", "groovy", "// changed", &[]),
                3 => l.owner = "project:/foreign".into(),
                4 => l.metadata = None,
                5 => {
                    host::storage::set(&session::locator(&l.owner, "selector"), "changed").unwrap();
                }
                _ => h.add_file("settings.gradle", "include 'module'"),
            };
            assert!(l.resolve_file_action(&d, a).is_err(), "{mode}");
            assert_eq!(
                h.editor_text(),
                if mode == 2 { "// changed" } else { GROOVY }
            );
        }
    }
    #[test]
    fn unsupported_dynamic_bom_repository_and_competing_sources_refuse_before_fetch() {
        let bad=[GROOVY.replace("version '3.5.16'","version bootVersion"),GROOVY.replace("mavenCentral()","mavenLocal()"),GROOVY.replace("implementation 'org.springframework.boot:spring-boot-starter'","implementation platform('org.springframework.boot:spring-boot-dependencies:3.5.16')"),format!("{GROOVY}apply from: 'other.gradle'\n"),GROOVY.replace("version '1.1.7'","version '1.0.0'"),GROOVY.replace("spring-boot-starter'","spring-boot-starter:$version'"),format!("{GROOVY}dependencyManagement {{ imports {{ mavenBom 'other:bom:1' }} }}\n")];
        for text in bad {
            let (h, mut l, d) = setup(&text, false);
            assert!(
                l.bound_cache(&h.invocation())
                    .is_err()
            );
            assert_eq!(h.fetches(), 0);
            assert!(l.file_actions(&d, Range::default()).is_empty());
        }
        for name in [
            "pom.xml",
            "build.gradle.kts",
            "gradle.properties",
            "buildSrc",
            "gradle/libs.versions.toml",
        ] {
            let (h, mut l, _) = setup(GROOVY, false);
            h.add_file(name, "fixture");
            assert!(
                l.bound_cache(&h.invocation())
                    .is_err(),
                "{name}"
            );
            assert_eq!(h.fetches(), 0);
        }
    }
    #[test]
    fn literal_settings_allowed_but_modules_custom_management_and_nested_source_refuse() {
        let (h, mut l, _) = setup(KOTLIN, true);
        h.add_file("settings.gradle.kts", "rootProject.name = \"demo\"\n");
        assert!(l.bound_cache(&h.invocation()).is_ok());
        let (h, mut l, _) = setup(GROOVY, false);
        h.add_file(
            "settings.gradle",
            "pluginManagement { repositories { mavenLocal() } }",
        );
        assert!(
            l.bound_cache(&h.invocation())
                .is_err()
        );
        assert_eq!(h.fetches(), 0);
        let (h, mut l, _) = setup(GROOVY, false);
        h.add_project("/project");
        h.set_project_root(Some("/other"));
        assert!(
            l.bound_cache(&h.invocation())
                .is_err()
        );
        assert_eq!(h.fetches(), 0);
        let (h, mut l, _) = setup(GROOVY, false);
        h.add_file("sub/build.gradle", GROOVY);
        h.open_editor("/project/sub/build.gradle", "groovy", GROOVY, &[]);
        assert!(
            l.bound_cache(&h.invocation())
                .is_err()
        );
        assert_eq!(h.fetches(), 0);
    }
    #[test]
    fn exact_coordinate_collisions_are_skipped_without_conflating_other_groups() {
        let existing = GROOVY.replace("spring-boot-starter'", "spring-boot-starter-web'");
        let (h, mut l, d) = ready(&existing, false);
        let a = l.file_actions(&d, Range::default()).remove(0);
        h.answer_next_prompt(Some(&["web"]));
        assert!(l.resolve_file_action(&d, a).is_err());
        assert_eq!(h.editor_text(), existing);
        let other = GROOVY.replace(
            "org.springframework.boot:spring-boot-starter'",
            "com.example:spring-boot-starter-web'",
        );
        let (h, mut l, d) = ready(&other, false);
        let a = resolve(&h, &mut l, &d, &["web"]);
        assert!(after(&d, &a).contains("org.springframework.boot:spring-boot-starter-web"));
    }
    #[test]
    fn metadata_bom_repository_version_and_provided_mapping_are_not_inferred() {
        for field in ["bom", "repository", "version", "scope"] {
            let (h, mut l, _d) = setup(GROOVY, false);
            let mut coords: serde_json::Value = serde_json::from_str(COORDS).unwrap();
            for dep in ["web", "data-jpa", "unknown-range"] {
                coords["dependencies"][dep][field] = if field == "scope" {
                    "provided".into()
                } else {
                    "unsupported".into()
                };
            }
            let body = coords.to_string();
            h.on_fetch(move |req| {
                Ok(openidea_plugin::HttpResponse {
                    status: 200,
                    body: if req.url.contains("metadata/client") {
                        META.as_bytes().to_vec()
                    } else {
                        body.as_bytes().to_vec()
                    },
                    ..Default::default()
                })
            });
            let prepared = l.bound_cache(&h.invocation());
            assert!(prepared.is_err());
            assert_eq!(h.editor_text(), GROOVY);
        }
    }
    fn apply(_h: &MockHost, d: &Document, a: &FileAction) -> (String, Option<String>) {
        let edit = |text: &str, e: &openidea_plugin::TextEdit| {
            let mut out = text.to_owned();
            out.replace_range(offset(text, e.range.start).unwrap()..offset(text, e.range.end).unwrap(), &e.text);
            out
        };
        let build = edit(&d.text, &a.edits[0].1[0]);
        let catalog = a.edits.get(1).map(|(path, edits)| {
            assert!(path.ends_with("/gradle/libs.versions.toml"));
            edit(&host::project_of(path).unwrap().read_file(path).unwrap(), &edits[0])
        });
        (build, catalog)
    }
    const CATALOG: &str = "[versions]\nspring = \"3.5.16\"\n\n[libraries]\nspring-boot-starter = { module = \"org.springframework.boot:spring-boot-starter\" }\n";
    #[test]
    fn platform_bom_without_dependency_management_plugin_is_supported_and_reparses() {
        for kotlin in [false, true] {
            let base = if kotlin { KOTLIN } else { GROOVY };
            let platform = if kotlin {
                "    implementation(platform(\"org.springframework.boot:spring-boot-dependencies:3.5.16\"))\n"
            } else {
                "    implementation platform('org.springframework.boot:spring-boot-dependencies:3.5.16')\n"
            };
            let dm = if kotlin { "    id(\"io.spring.dependency-management\") version \"1.1.7\"\n" } else { "    id 'io.spring.dependency-management' version '1.1.7'\n" };
            let text = base.replace(dm, "").replace("dependencies {\n", &format!("dependencies {{\n{platform}"));
            let (h, mut l, d) = ready(&text, kotlin);
            let a = resolve(&h, &mut l, &d, &["web"]);
            let (build, catalog) = apply(&h, &d, &a);
            assert!(catalog.is_none());
            assert!(proof(&Document { text: build.clone(), ..d.clone() }).is_some());
            assert!(build.contains("spring-boot-starter-web"));
            let mismatched = text.replace("dependencies:3.5.16", "dependencies:3.4.0");
            let (h, mut l, _) = setup(&mismatched, kotlin);
            assert!(l.bound_cache(&h.invocation()).is_err());
            assert_eq!(h.fetches(), 0);
            let unmanaged = base.replace(dm, "");
            assert!(proof(&Document { text: unmanaged, ..d.clone() }).is_none());
        }
    }
    #[test]
    fn literal_version_catalog_gets_entry_and_alias_reference_and_reparses() {
        for kotlin in [false, true] {
            let text = if kotlin { KOTLIN } else { GROOVY };
            let (h, mut l, d) = setup(text, kotlin);
            h.add_file("gradle/libs.versions.toml", CATALOG);
            l.bound_cache(&h.invocation()).unwrap();
            let a = resolve(&h, &mut l, &d, &["web"]);
            let (build, catalog) = apply(&h, &d, &a);
            let catalog = catalog.unwrap();
            assert!(catalog.ends_with("spring-boot-starter = { module = \"org.springframework.boot:spring-boot-starter\" }\nspring-boot-starter-web = { module = \"org.springframework.boot:spring-boot-starter-web\" }\n"));
            assert!(build.contains(if kotlin { "implementation(libs.spring.boot.starter.web)" } else { "implementation libs.spring.boot.starter.web" }));
            let reparsed = proof(&Document { text: build, ..d.clone() }).unwrap();
            assert_eq!(reparsed.refs, ["libs.spring.boot.starter.web"]);
            h.add_file("gradle/libs.versions.toml", &catalog);
            assert!(catalog_ok(&reparsed));
        }
    }
    fn catalog_ok(p: &Proof) -> bool {
        catalog(p, "/project").is_ok_and(|c| c.is_some_and(|c| c.used.contains(&("org.springframework.boot".into(), "spring-boot-starter-web".into()))))
    }
    #[test]
    fn catalog_refs_resolve_dedupe_and_unrecognised_catalogs_refuse() {
        let referenced = GROOVY.replace("'org.springframework.boot:spring-boot-starter'", "libs.spring.boot.starter");
        let (h, mut l, d) = setup(&referenced, false);
        h.add_file("gradle/libs.versions.toml", CATALOG);
        l.bound_cache(&h.invocation()).unwrap();
        let a = resolve(&h, &mut l, &d, &["web"]);
        assert!(apply(&h, &d, &a).0.contains("libs.spring.boot.starter.web"));
        for bad in ["[bundles]\nx = [\"a\"]\n", "[libraries]\nx = { module = \"a:b\", other = \"c\" }\n", "[libraries]\nx = { module = \"a:b\" \n"] {
            let (h, mut l, _) = setup(&referenced, false);
            h.add_file("gradle/libs.versions.toml", bad);
            assert!(l.bound_cache(&h.invocation()).is_err(), "{bad}");
            assert_eq!(h.fetches(), 0);
        }
        let (h, mut l, _) = setup(&referenced, false);
        assert!(l.bound_cache(&h.invocation()).is_err());
        assert_eq!(h.fetches(), 0);
        let _ = h;
    }
    #[test]
    fn catalog_without_libraries_table_gets_one() {
        let (h, mut l, d) = setup(GROOVY, false);
        h.add_file("gradle/libs.versions.toml", "[versions]\nspring = \"3.5.16\"");
        l.bound_cache(&h.invocation()).unwrap();
        let a = resolve(&h, &mut l, &d, &["web"]);
        assert_eq!(apply(&h, &d, &a).1.unwrap(), "[versions]\nspring = \"3.5.16\"\n\n[libraries]\nspring-boot-starter-web = { module = \"org.springframework.boot:spring-boot-starter-web\" }\n");
    }
    #[test]
    fn cold_provider_does_no_fetch_and_resolved_action_cannot_replay() {
        let (h, mut l, d) = setup(KOTLIN, true);
        assert!(l.file_actions(&d, Range::default()).is_empty());
        assert_eq!(h.fetches(), 0);
        l.bound_cache(&h.invocation())
            .unwrap();
        let a = l.file_actions(&d, Range::default()).remove(0);
        let replay = a.clone();
        h.answer_next_prompt(Some(&["web"]));
        h.answer_next_prompt(Some(&["yes"]));
        l.resolve_file_action(&d, a).unwrap();
        assert!(l.resolve_file_action(&d, replay).is_err());
    }
}
