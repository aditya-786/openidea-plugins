//! Current Maven parent/sole imported Boot BOM proof and version-bound cached coordinates, never inferred BOMs.
mod gradle;
use super::*;
use openidea_plugin::file_actions::{FileAction, FileActions};
use openidea_plugin::providers::Document;
use std::collections::{BTreeMap, BTreeSet};
use std::hash::BuildHasher;
use std::time::{Duration, Instant};
const MAX_SOURCE: usize = 256 << 10;
#[derive(Clone, Debug)]
struct Node {
    name: String,
    parent: Option<usize>,
    text: String,
    close: usize,
    start: usize,
    empty_end: Option<usize>,
    attrs: Vec<(String, String)>,
}
#[derive(Clone, Debug)]
struct Proof {
    boot: String,
    management: &'static str,
    at: usize,
    end: usize,
    before: String,
    after: String,
    inner: String,
    existing: BTreeSet<(String, String)>,
    newline: &'static str,
    gradle: Option<bool>,
    refs: Vec<String>,
}
fn hunk(text: &str, from: usize, to: usize) -> String {
    let starts: Vec<usize> = std::iter::once(0).chain(text.match_indices('\n').map(|(i, _)| i + 1)).collect();
    let line = |offset: usize| starts.partition_point(|&start| start <= offset) - 1;
    let first = line(from).saturating_sub(3);
    let last = (line(to.saturating_sub(1).max(from)) + 3).min(starts.len() - 1);
    let end = starts.get(last + 1).copied().unwrap_or(text.len());
    format!("Lines {}–{}:\n{}", first + 1, last + 1, text[starts[first]..end].trim_end_matches(['\r', '\n']))
}
fn literal(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && value.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}
fn ga_version(value: &str) -> bool {
    let p: Vec<_> = value.split('.').collect();
    p.len() == 3 && p.iter().all(|p| !p.is_empty() && p.len() <= 4 && p.bytes().all(|b| b.is_ascii_digit()))
}
fn parse(text: &str) -> Option<Vec<Node>> {
    use quick_xml::{Reader, events::Event};
    if text.len() > MAX_SOURCE {
        return None;
    }
    let mut r = Reader::from_str(text);
    r.config_mut().check_comments = true;
    let mut nodes = Vec::<Node>::new();
    let mut stack = Vec::new();
    let mut declaration = false;
    loop {
        let start = r.buffer_position() as usize;
        match r.read_event() {
            Ok(Event::Start(e) | Event::Empty(e)) => {
                if nodes.len() >= 512 || stack.len() >= 32 {
                    return None;
                }
                let name = e.name().as_ref().to_owned();
                if name.contains(':') {
                    return None;
                }
                let mut attrs = Vec::new();
                for attr in e.attributes() {
                    let attr = attr.ok()?;
                    attrs.push((attr.key.as_ref().to_owned(), attr.value.as_ref().to_owned()));
                }
                let empty = text[..r.buffer_position() as usize].ends_with("/>");
                let index = nodes.len();
                nodes.push(Node {
                    name,
                    parent: stack.last().copied(),
                    text: String::new(),
                    close: if empty { start } else { 0 },
                    start,
                    empty_end: empty.then_some(r.buffer_position() as usize),
                    attrs,
                });
                if !empty {
                    stack.push(index);
                }
            }
            Ok(Event::Text(t)) => {
                if let Some(&i) = stack.last() {
                    nodes[i].text.push_str(t.as_ref());
                } else if !t.as_ref().trim().is_empty() {
                    return None;
                }
            }
            Ok(Event::End(_)) => {
                let i = stack.pop()?;
                nodes[i].close = start;
            }
            Ok(Event::Decl(decl)) => {
                if declaration || start != 0 || !nodes.is_empty() || !stack.is_empty() {
                    return None;
                }
                let declaration_tag = quick_xml::events::BytesStart::from_content(decl.as_ref(), 3);
                let mut order = 0;
                for attribute in declaration_tag.attributes() {
                    let attribute = attribute.ok()?;
                    let value = attribute.value.as_ref();
                    match attribute.key.as_ref() {
                        "version" if order == 0 && matches!(value, "1.0" | "1.1") => order = 1,
                        "encoding" if order == 1 && value.len() <= 64
                            && value.bytes().next().is_some_and(|b| b.is_ascii_alphabetic())
                            && value.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)) => order = 2,
                        "standalone" if matches!(order, 1 | 2) && matches!(value, "yes" | "no") => order = 3,
                        _ => return None,
                    }
                }
                if order == 0 {
                    return None;
                }
                declaration = true;
            }
            Ok(Event::PI(pi)) if pi.target().eq_ignore_ascii_case("xml") => return None,
            Ok(Event::Eof) => return stack.is_empty().then_some(nodes),
            Ok(Event::DocType(_) | Event::CData(_) | Event::GeneralRef(_)) | Err(_) => return None,
            _ => {}
        }
    }
}
fn only<'a>(nodes: &'a [Node], parent: usize, name: &str) -> Option<(usize, &'a Node)> {
    let hits: Vec<_> = nodes.iter().enumerate().filter(|(_, n)| n.parent == Some(parent) && n.name == name).collect();
    let [(i, n)] = hits.as_slice() else {
        return None;
    };
    Some((*i, *n))
}
fn value(nodes: &[Node], parent: usize, name: &str) -> Option<String> {
    let (i, n) = only(nodes, parent, name)?;
    if nodes.iter().any(|n| n.parent == Some(i)) || !n.attrs.is_empty() {
        return None;
    }
    Some(n.text.trim().to_owned())
}
fn proof(doc:&Document)->Option<Proof>{maven_proof(doc).or_else(||gradle::proof(doc))}
fn maven_proof(doc: &Document) -> Option<Proof> {
    if doc.language != "xml" || doc.path.rsplit('/').next() != Some("pom.xml") {
        return None;
    }
    let nodes = parse(&doc.text)?;
    if nodes.iter().filter(|n| n.parent.is_none()).count() != 1 || nodes.first()?.name != "project" {
        return None;
    }
    let mut root_names = BTreeSet::new();
    for n in nodes.iter().filter(|n| n.parent == Some(0)) {
        if !root_names.insert(&n.name)
            || !matches!(
                n.name.as_str(),
                "modelVersion"
                    | "parent"
                    | "groupId"
                    | "artifactId"
                    | "version"
                    | "packaging"
                    | "name"
                    | "description"
                    | "url"
                    | "licenses"
                    | "developers"
                    | "scm"
                    | "properties"
                    | "dependencies"
                    | "dependencyManagement"
                    | "build"
            )
        {
            return None;
        }
    }
    if !literal(&value(&nodes, 0, "artifactId")?) {
        return None;
    }
    if root_names.contains(&"packaging".to_owned()) && !matches!(value(&nodes, 0, "packaging")?.as_str(), "jar" | "war") {
        return None;
    }
    if !nodes[0].text.trim().is_empty() {
        return None;
    }
    if nodes[0].attrs.iter().any(|(key, _)| key == "xsi:schemaLocation")
        && !nodes[0].attrs.iter().any(|(key, value)| key == "xmlns:xsi" && value == "http://www.w3.org/2001/XMLSchema-instance")
    {
        return None;
    }
    for (i, n) in nodes.iter().enumerate() {
        for (key, value) in &n.attrs {
            if i == 0 && key == "xmlns" && value == "http://maven.apache.org/POM/4.0.0" {
                continue;
            }
            if i == 0
                && (key == "xmlns:xsi" && value == "http://www.w3.org/2001/XMLSchema-instance"
                    || key == "xsi:schemaLocation" && !value.contains(['<', '>', '&']))
            {
                continue;
            }
            return None;
        }
        if matches!(n.name.as_str(), "profiles" | "repositories" | "pluginRepositories" | "modules" | "extensions")
        {
            return None;
        }
    }
    if value(&nodes, 0, "modelVersion")?.as_str() != "4.0.0" {
        return None;
    }
    // Exactly one recognized management source, never inferred precedence.
    let parent=only(&nodes,0,"parent");let managed=only(&nodes,0,"dependencyManagement");
    if nodes.iter().any(|n|n.name=="dependencyManagement"&&n.parent!=Some(0)) {return None;}
    let (boot,management)=match (parent,managed) {
        (Some((parent,_)),None)=> {
            if nodes.iter().filter(|n|n.parent==Some(parent)).any(|n|!matches!(n.name.as_str(),"groupId"|"artifactId"|"version"|"relativePath")) {return None;}
            if value(&nodes,parent,"groupId")?!="org.springframework.boot"||value(&nodes,parent,"artifactId")?!="spring-boot-starter-parent"||value(&nodes,parent,"relativePath")?!="" {return None;}
            (value(&nodes,parent,"version")?,"the spring-boot-starter-parent POM")
        }
        (None,Some((managed,node)))=> {
            if !literal(&value(&nodes,0,"groupId")?)||!literal(&value(&nodes,0,"version")?)||!node.text.trim().is_empty() {return None;}
            let (dependencies,node)=only(&nodes,managed,"dependencies")?;
            if nodes.iter().filter(|n|n.parent==Some(managed)).count()!=1||!node.text.trim().is_empty() {return None;}
            let (dependency,node)=only(&nodes,dependencies,"dependency")?;
            if nodes.iter().filter(|n|n.parent==Some(dependencies)).count()!=1||!node.text.trim().is_empty() {return None;}
            let mut names=BTreeSet::new();
            if nodes.iter().filter(|n|n.parent==Some(dependency)).any(|n|!names.insert(&n.name)||!matches!(n.name.as_str(),"groupId"|"artifactId"|"version"|"type"|"scope")) {return None;}
            if value(&nodes,dependency,"groupId")?!="org.springframework.boot"||value(&nodes,dependency,"artifactId")?!="spring-boot-dependencies"||value(&nodes,dependency,"type")?!="pom"||value(&nodes,dependency,"scope")?!="import" {return None;}
            (value(&nodes,dependency,"version")?,"the imported spring-boot-dependencies BOM")
        }
        (None,None)=>(String::new(),"no Spring Boot version management"),
        _=>return None,
    };
    if !boot.is_empty()&&!ga_version(&boot) {
        return None;
    }
    if nodes.iter().filter(|n| n.parent == Some(0) && n.name == "properties").count() > 1 {
        return None;
    }
    for (i, n) in nodes.iter().enumerate().filter(|(_, n)| n.parent == Some(0) && n.name == "properties") {
        if !n.text.trim().is_empty() {
            return None;
        }
        let mut seen = BTreeSet::new();
        for (p, c) in nodes.iter().enumerate().filter(|(_, n)| n.parent == Some(i)) {
            if !seen.insert(&c.name)
                || !matches!(c.name.as_str(), "java.version" | "project.build.sourceEncoding")
                || nodes.iter().any(|n| n.parent == Some(p))
            {
                return None;
            }
        }
    }
    let newline = if doc.text.contains("\r\n") { "\r\n" } else { "\n" };
    let deps = only(&nodes, 0, "dependencies");
    let (at, end, before, after, inner) = if let Some((_, node)) = deps {
        if !node.text.trim().is_empty() {
            return None;
        }
        if let Some(end) = node.empty_end {
            let line = doc.text[..node.start].rfind('\n').map_or(0, |i| i + 1);
            let indent = &doc.text[line..node.start];
            if !indent.chars().all(|c| matches!(c, ' ' | '\t')) {
                return None;
            }
            (node.start, end, format!("<dependencies>{newline}"), format!("{indent}</dependencies>"), format!("{indent}    "))
        } else {
            if !doc.text[node.close..].starts_with("</dependencies>") {
                return None;
            }
            let line = doc.text[..node.close].rfind('\n').map_or(0, |i| i + 1);
            let indent = &doc.text[line..node.close];
            if !indent.chars().all(|c| matches!(c, ' ' | '\t')) {
                return None;
            }
            (line, line, String::new(), String::new(), format!("{indent}    "))
        }
    } else {
        // Create before build, otherwise before the closing project. Do not reorder existing bytes.
        let target = only(&nodes, 0, "build").map(|(_, n)| n.start).unwrap_or(nodes[0].close);
        let line = doc.text[..target].rfind('\n').map_or(0, |i| i + 1);
        if !doc.text[line..target].chars().all(|c| matches!(c, ' ' | '\t')) {
            return None;
        }
        let indent = nodes
            .iter()
            .filter(|n| n.parent == Some(0))
            .find_map(|n| {
                let start = doc.text[..n.start].rfind('\n')? + 1;
                let whitespace = &doc.text[start..n.start];
                (!whitespace.is_empty() && whitespace.chars().all(|c| matches!(c, ' ' | '\t'))).then_some(whitespace)
            })
            .unwrap_or("    ");
        (line, line, format!("{indent}<dependencies>{newline}"), format!("{indent}</dependencies>{newline}"), format!("{indent}    "))
    };
    let mut existing = BTreeSet::new();
    for (i, n) in nodes.iter().enumerate().filter(|(_, n)| deps.is_some_and(|(deps, _)| n.parent == Some(deps))) {
        if n.name != "dependency" {
            return None;
        }
        let group = value(&nodes, i, "groupId")?;
        let artifact = value(&nodes, i, "artifactId")?;
        if !literal(&group) || !literal(&artifact) || !existing.insert((group, artifact)) {
            return None;
        }
        if !n.text.trim().is_empty() {
            return None;
        }
        let mut child_names = BTreeSet::new();
        for (j, child) in nodes.iter().enumerate().filter(|(_, n)| n.parent == Some(i)) {
            if !child_names.insert(&child.name) || nodes.iter().any(|n| n.parent == Some(j)) || !child.attrs.is_empty() {
                return None;
            }
            if child.name == "version" && !literal(child.text.trim()) {
                return None;
            }
            if child.name == "scope" && !matches!(child.text.trim(), "compile" | "runtime" | "provided" | "test") {
                return None;
            }
        }
        if nodes.iter().filter(|n| n.parent == Some(i)).any(|n| !matches!(n.name.as_str(), "groupId" | "artifactId" | "version" | "scope"))
        {
            return None;
        }
    }
    Some(Proof { boot, management, at, end, before, after, inner, existing, newline,gradle:None,refs:Vec::new() })
}
fn aggregator(doc: &Document) -> Option<Vec<String>> {
    if doc.language != "xml" || doc.path.rsplit('/').next() != Some("pom.xml") {
        return None;
    }
    let nodes = parse(&doc.text)?;
    if nodes.iter().filter(|n| n.parent.is_none()).count() != 1 || nodes.first()?.name != "project" {
        return None;
    }
    if nodes.iter().enumerate().any(|(i, n)| n.attrs.iter().any(|(k, v)| i != 0 || k != "xmlns" || v != "http://maven.apache.org/POM/4.0.0")) {
        return None;
    }
    if value(&nodes, 0, "packaging")? != "pom" || value(&nodes, 0, "modelVersion")? != "4.0.0" {
        return None;
    }
    if nodes.iter().any(|n| matches!(n.name.as_str(), "profiles" | "repositories" | "pluginRepositories" | "extensions")) {
        return None;
    }
    let (modules, node) = only(&nodes, 0, "modules")?;
    if !node.text.trim().is_empty() {
        return None;
    }
    let mut names = Vec::new();
    for (i, n) in nodes.iter().enumerate().filter(|(_, n)| n.parent == Some(modules)) {
        let name = n.text.trim().to_owned();
        if n.name != "module" || nodes.iter().any(|c| c.parent == Some(i)) || !literal(&name) || names.contains(&name) {
            return None;
        }
        names.push(name);
    }
    (!names.is_empty() && names.len() <= 16).then_some(names)
}
/// Module poms of an aggregator that individually carry the existing Boot proof.
fn module_targets(doc: &Document) -> Vec<(Document, Proof)> {
    let (Some(names), Some(project)) = (aggregator(doc), host::project_of(&doc.path)) else {
        return Vec::new();
    };
    let dir = doc.path.rsplit_once('/').map_or("", |(d, _)| d);
    names
        .iter()
        .filter_map(|name| {
            let path = format!("{dir}/{name}/pom.xml");
            let text = project.read_file(&path).ok()?;
            let module = Document { path, language: "xml".into(), version: 0, text };
            let p = maven_proof(&module)?;
            Some((module, p))
        })
        .collect()
}
fn starter_target(doc: &Document, cache: &Cache) -> Result<(Document, Proof), Error> {
    let Some(module) = &cache.module else {
        return Ok((doc.clone(), proof(doc).ok_or(Error::Stale)?));
    };
    module_targets(doc).into_iter().find(|(m, _)| &m.path == module).ok_or(Error::Stale)
}
fn root(doc: &Document) -> Option<String> {
    let project = host::project_of(&doc.path)?;
    let root = project.root().ok()?;
    if !root.starts_with('/')
        || root == "/"
        || root.len() > 4096
        || root.contains("//")
        || root.chars().any(char::is_control)
        || doc.path.contains('\\')
        || doc.path.contains("//")
        || doc.path.len() > 4096
        || doc.path.chars().any(char::is_control)
        || doc.path.split('/').any(|p| matches!(p, "." | ".."))
        || !doc.path.starts_with(&format!("{root}/"))
    {
        return None;
    }
    Some(root)
}
fn fresh_source(doc: &Document, root: &str) -> Result<(), Error> {
    let project = host::project_of(&doc.path).ok_or(Error::Stale)?;
    if project.root()? != root
        || project.stat(&doc.path)?.kind != openidea_plugin::FileKind::File
        || project.read_file(&doc.path)? != doc.text
    {
        return Err(Error::Stale);
    }
    if doc.path.ends_with("build.gradle")||doc.path.ends_with("build.gradle.kts"){return gradle::fresh(doc,root);}
    let mut directory = doc.path.rsplit_once('/').map(|(p, _)| p).ok_or(Error::Stale)?;
    for _ in 0..32 {
        for config in ["maven.config", "jvm.config", "extensions.xml"] {
            if project.exists(&format!("{directory}/.mvn/{config}"))? {
                return Err("This project has .mvn/maven.config, jvm.config or extensions.xml, which can change the build in ways the plugin can't see. Add starters by hand.".into());
            }
        }
        if directory == root {
            return Ok(());
        }
        directory = directory.rsplit_once('/').map(|(p, _)| p).filter(|p| p.len() >= root.len()).ok_or(Error::Stale)?;
    }
    Err("The pom.xml is nested more than 32 folders deep; add starters by hand.".into())
}
#[derive(Clone)]
struct Starter {
    id: String,
    name: String,
    group: String,
    artifact: String,
    scope: String,
    version: Option<String>,
}
struct Cache {
    root: String,
    path: String,
    boot: String,
    module: Option<String>,
    at: Instant,
    metadata: serde_json::Value,
    starters: Vec<Starter>,
    selector: Option<String>,
}
struct Pending {
    doc: Document,
    root: String,
    boot: String,
}
/// Starters chosen and confirmed in the command, waiting for the one ⌥⏎ action that inserts them.
struct Staged {
    doc: Document,
    expected: Vec<(String, String)>,
    action: FileAction,
}
/// Writes the confirmed starters; `Some(reason)` when nothing was written and the ⌥⏎ action has to insert them.
fn write_starters(staged: &Staged) -> Result<Option<String>, Error> {
    for (path, text) in &staged.expected {
        if host::project_of(path).ok_or(Error::Stale)?.read_file(path)? != *text {
            return Err("The build file changed since the preview, so nothing was written. Run Add Spring Boot Starters again.".into());
        }
    }
    if let Some(open) = host::open_editors().unwrap_or_default().into_iter().find(|e| e.dirty && staged.expected.iter().any(|(p, _)| *p == e.path)) {
        return Ok(Some(format!("{} has unsaved changes, so it wasn't written. Save it and run Add Spring Boot Starters again.", open.path.rsplit('/').next().unwrap_or_default())));
    }
    // The version catalog goes first, so the build file never names an alias that isn't there.
    for (written, ((path, edits), (_, before))) in staged.action.edits.iter().zip(&staged.expected).rev().enumerate() {
        let mut after = before.clone();
        for edit in edits.iter().rev() {
            let from = openidea_plugin::text::offset(before, edit.range.start).ok_or(Error::Stale)?;
            let to = openidea_plugin::text::offset(before, edit.range.end).ok_or(Error::Stale)?;
            after.replace_range(from..to, &edit.text);
        }
        match host::project_of(path).ok_or(Error::Stale)?.write_file(path, &after) {
            Err(Error::Denied(_)) if written == 0 => {
                return Ok(Some(ui_kit::denied::message("Add Spring Boot Starters", "project.write", "Spring Initializr Launcher")));
            }
            result => result.map_err(|e| ui_kit::denied::map(e, "Add Spring Boot Starters", "project.write", "Spring Initializr Launcher"))?,
        }
    }
    Ok(None)
}
const STAGED: &str = "starters:staged";
#[derive(Default)]
pub(crate) struct State {
    cache: Option<Cache>,
    pending: BTreeMap<String, Pending>,
    staged: Option<Staged>,
}
impl State {
    pub(crate) fn invalidate(&mut self) {
        self.cache = None;
        self.pending.clear();
        self.staged = None;
    }
}
impl Launcher {
    /// Loads the starters, asks which to add, shows the change and writes it, or stages it for ⌥⏎
    /// when the build file can't be written (no project.write, unsaved changes).
    pub(crate) fn prepare_starters(&mut self, ctx: &Invocation) -> Result<Option<String>, Error> {
        let doc = self.cache_starters(ctx)?;
        let root = self.starter_edits.cache.as_ref().ok_or(Error::Stale)?.root.clone();
        let preview = self.starter_file_actions(&doc, Range::default()).pop().ok_or(Error::Stale)?;
        let action = self.resolve_starter_file_action(&doc, preview)?;
        let mut expected = Vec::new();
        for (path, _) in &action.edits {
            expected.push((path.clone(), host::project_of(path).ok_or(Error::Stale)?.read_file(path)?));
        }
        let file = doc.path.rsplit('/').next().unwrap_or_default().to_owned();
        let staged = Staged { doc, expected, action };
        let Some(reason) = write_starters(&staged)? else {
            let (path, edits) = &staged.action.edits[0];
            let _ = host::open_location(path, edits[0].range.start.line, 0);
            let shown = path.strip_prefix(&format!("{root}/")).unwrap_or(path);
            return Ok(Some(format!("{} to {shown}.", staged.action.title.replacen("Add", "Added", 1))));
        };
        let message = format!("{reason} Or press ⌥⏎ in {file} and choose {} to insert them. One Undo removes them again.", staged.action.title);
        self.starter_edits.staged = Some(staged);
        Ok(Some(message))
    }
    fn cache_starters(&mut self, ctx: &Invocation) -> Result<Document, Error> {
        self.starter_edits.invalidate();
        let editor = ctx.editor().ok_or("Open a pom.xml, build.gradle or build.gradle.kts in the editor first. If one is open, allow editor.read for Spring Initializr in Settings › Plugins.")?;
        let snapshot = editor.document().map_err(|error| match error {
            Error::Denied(_) => Error::Invalid("Allow editor.read for Spring Initializr in Settings › Plugins.".into()),
            other => other,
        })?;
        let doc = Document { path: editor.path(), language: editor.language(), version: snapshot.version, text: snapshot.text };
        let first = proof(&doc);
        if first.is_none() && aggregator(&doc).is_none() {
            return Err("This build file isn't supported. Use a Maven pom.xml with the Spring Boot parent or BOM, a Maven aggregator, or the project-root build.gradle(.kts); builds with dynamic scripts or custom repositories can't be edited.".into());
        }
        let root = root(&doc).ok_or(Error::Stale)?;
        if ctx.project().and_then(|p| p.root().ok()).as_deref() != Some(&root)
            || host::project_of(&doc.path).ok_or(Error::Stale)?.read_file(&doc.path)? != doc.text
        {
            return Err(Error::Stale);
        }
        fresh_source(&doc, &root)?;
        let _ = self.changed();
        if self.server != START {
            return Err("Add Spring Boot Starters works only with start.spring.io. Set Server URL to https://start.spring.io in the Spring Initializr tool window.".into());
        }
        let selector = host::storage::get(&session::locator(&self.owner, "selector"))?;
        if let Some(raw) = &selector {
            let value = crate::strict_json::parse(raw, session::MAX_SELECTOR).map_err(|_| Error::Stale)?;
            if raw.len() > session::MAX_SELECTOR || value["owner"].as_str() != Some(&self.owner) || value["server"].as_str() != Some(START) {
                return Err(Error::Stale);
            }
        }
        if session::selected_server(&self.owner) != self.server {
            return Err(Error::Stale);
        }
        let (p, module) = match first {
            Some(p) => (p, None),
            None => {
                let targets = module_targets(&doc);
                let items: Vec<_> = targets.iter().map(|(m, p)| PickItem::new(&m.path, &m.path).description(&format!("{} · {}", p.management, p.boot))).collect();
                if items.is_empty() {
                    return Err("None of this aggregator's modules has a supported pom.xml. Open a module's pom.xml and try again.".into());
                }
                let chosen = host::pick("Add starters to which module?", &items)?.ok_or(Error::Cancelled)?;
                let (module_doc, p) = targets.into_iter().find(|(m, _)| m.path == chosen).ok_or(Error::Stale)?;
                fresh_source(&module_doc, &root)?;
                (p, Some(module_doc.path))
            }
        };
        self.load_metadata(false);
        let metadata = self.metadata.as_ref().ok_or("Couldn't load the starter list from start.spring.io. Check the connection and run Add Spring Boot Starters again.")?.clone();
        let unmanaged = p.boot.is_empty();
        let boot = if unmanaged { metadata["bootVersion"]["default"].as_str().filter(|v| ga_version(v)).ok_or("start.spring.io didn't name a default Spring Boot version for this build without Spring Boot management.")?.to_owned() } else { p.boot.clone() };
        if !metadata["bootVersion"]["values"].as_array().is_some_and(|v| v.iter().any(|v| v["id"].as_str() == Some(&boot))) {
            return Err(format!("start.spring.io doesn't offer Spring Boot {boot}, the version this build uses. Update the Spring Boot version, then try again.").into());
        }
        let coords = starters::get(&format!("/dependencies?bootVersion={boot}"))?;
        if coords["bootVersion"].as_str() != Some(&boot) {
            return Err(format!("start.spring.io answered for a different Spring Boot version than {boot}. Try again later.").into());
        }
        let catalogue = dependencies::Catalogue::from_metadata(&metadata);
        let mut found = Vec::new();
        let mut artifacts = BTreeSet::new();
        for dep in catalogue.categories.iter().flat_map(|g| &g.dependencies) {
            if dep.range.as_ref().is_some_and(|range| compatibility::allows(&boot, range) != Some(true)) {
                continue;
            }
            let d = &coords["dependencies"][&dep.id];
            let Some(obj) = d.as_object() else {
                continue;
            };
            if obj.keys().any(|k| !matches!(k.as_str(), "groupId" | "artifactId" | "scope" | "version" | "bom" | "repository"))
                || !d["bom"].is_null()
                || !d["repository"].is_null()
            {
                continue;
            }
            let version = match &d["version"] {
                serde_json::Value::Null => None,
                serde_json::Value::String(v) if literal(v) => Some(v.clone()),
                _ => continue,
            };
            let Some(group) = d["groupId"].as_str().filter(|v| literal(v)) else {
                continue;
            };
            if version.is_some() != unmanaged || (!unmanaged && group != "org.springframework.boot") {
                continue;
            }
            let Some(artifact) = d["artifactId"].as_str().filter(|v| literal(v) && (unmanaged || v.starts_with("spring-boot-starter-"))) else {
                continue;
            };
            let scope = match &d["scope"] {
                serde_json::Value::Null => "compile",
                serde_json::Value::String(scope) => scope.as_str(),
                _ => continue,
            };
            if !matches!(scope, "compile" | "runtime" | "provided" | "test") || (p.gradle.is_some()&&scope=="provided") || !artifacts.insert(format!("{group}:{artifact}")) {
                continue;
            }
            found.push(Starter { id: dep.id.clone(), name: dep.name.clone(), group: group.into(), artifact: artifact.into(), scope: scope.into(), version });
        }
        if found.is_empty() {
            return Err(format!("start.spring.io lists no starters that can be added to this build for Spring Boot {boot}.").into());
        }
        if host::storage::get(&session::locator(&self.owner, "selector"))? != selector {
            return Err(Error::Stale);
        }
        self.starter_edits.cache =
            Some(Cache { root, path: doc.path.clone(), boot, module, at: Instant::now(), metadata, starters: found, selector });
        Ok(doc)
    }
}
impl Launcher {
    fn starter_file_actions(&mut self, doc: &Document, _selection: Range) -> Vec<FileAction> {
        let Some(root) = root(doc) else {
            return Vec::new();
        };
        if let Some(staged) = self.starter_edits.staged.as_ref().filter(|s| s.doc.path == doc.path && s.doc.version == doc.version && s.doc.text == doc.text) {
            return vec![FileAction { title: staged.action.title.clone(), kind: Some("source".into()), preferred: true, data: Some(STAGED.into()), ..FileAction::default() }];
        }
        let Some(cache) = &self.starter_edits.cache else {
            return Vec::new();
        };
        let Ok((_, p)) = starter_target(doc, cache) else {
            return Vec::new();
        };
        if self.owner != format!("project:{root}")
            || self.server != START
            || cache.root != root
            || cache.path != doc.path
            || !(p.boot.is_empty() || cache.boot == p.boot)
            || cache.at.elapsed() > Duration::from_secs(900)
        {
            return Vec::new();
        }
        if self.starter_edits.pending.len() >= 4 {
            self.starter_edits.pending.clear();
        }
        let random = std::collections::hash_map::RandomState::new();
        let token = format!("starters:{:016x}{:016x}", random.hash_one((doc.version, 0u8)), random.hash_one((doc.version, 1u8)));
        self.starter_edits.pending.insert(token.clone(), Pending { doc: doc.clone(), root, boot: cache.boot.clone() });
        vec![FileAction {
            title: "Preview Add Spring Boot Starters…".into(),
            kind: Some("source".into()),
            data: Some(token),
            ..FileAction::default()
        }]
    }
    fn resolve_starter_file_action(&mut self, doc: &Document, action: FileAction) -> Result<FileAction, Error> {
        let token = action.data.as_deref().ok_or(Error::Stale)?;
        if token == STAGED {
            let staged = self.starter_edits.staged.take().ok_or(Error::Stale)?;
            if staged.doc.path != doc.path || staged.doc.version != doc.version || staged.doc.text != doc.text {
                return Err(Error::Stale);
            }
            for (path, text) in &staged.expected {
                if host::project_of(path).ok_or(Error::Stale)?.read_file(path)? != *text {
                    return Err("The build file changed since the starters were chosen. Run Add Spring Boot Starters again.".into());
                }
            }
            return Ok(staged.action);
        }
        let pending = self.starter_edits.pending.get(token).ok_or(Error::Stale)?;
        if doc.path != pending.doc.path
            || doc.language != pending.doc.language
            || doc.version != pending.doc.version
            || doc.text != pending.doc.text
            || root(doc).as_deref() != Some(&pending.root)
            || self.owner != format!("project:{}", pending.root)
            || self.server != START
        {
            return Err(Error::Stale);
        }
        let cache = self.starter_edits.cache.as_ref().ok_or(Error::Stale)?;
        if cache.at.elapsed() > Duration::from_secs(900)
            || self.metadata.as_ref() != Some(&cache.metadata)
            || cache.boot != pending.boot
            || cache.path != doc.path
            || cache.root != pending.root
        {
            return Err(Error::Stale);
        }
        let (target, p) = starter_target(doc, cache)?;
        if !(p.boot.is_empty() || p.boot == cache.boot) {
            return Err(Error::Stale);
        }
        fresh_source(doc, &pending.root)?;
        let doc = &target;
        fresh_source(doc, &pending.root)?;
        if host::storage::get(&session::locator(&self.owner, "selector"))? != cache.selector {
            return Err(Error::Stale);
        }
        let catalog = if p.gradle.is_some() { gradle::catalog(&p, &pending.root)? } else { None };
        let mut existing = p.existing.clone();
        if let Some(c) = &catalog {
            existing.extend(c.used.iter().cloned());
        }
        let available: Vec<_> =
            cache.starters.iter().filter(|s| (p.gradle.is_none()||s.scope!="provided")&&!existing.contains(&(s.group.clone(), s.artifact.clone()))).collect();
        let items: Vec<_> = available
            .iter()
            .map(|s| {
                PickItem::new(&s.id, &s.name)
                    .description(&format!("{}:{} · {} · {}", s.group, s.artifact, s.scope, s.version.as_deref().map_or(format!("Boot management {}", cache.boot), |v| format!("explicit version {v}"))))
            })
            .collect();
        if items.is_empty() {
            return Err("Every starter start.spring.io offers for this build is already declared.".into());
        }
        let ids = host::pick_many("Choose Spring Boot starters to add", &items)?.ok_or(Error::Cancelled)?;
        if ids.is_empty() {
            return Err(Error::Cancelled);
        }
        if ids.len() > 8 || ids.iter().collect::<BTreeSet<_>>().len() != ids.len() {
            return Err("Choose at most eight starters at a time.".into());
        }
        let inner = &p.inner;
        let mut insertion = p.before.clone();
        let mut catalog_lines = Vec::new();
        for id in &ids {
            let s = available.iter().find(|s| s.id == *id).ok_or(Error::Stale)?;
            let n = p.newline;
            if let Some(kotlin)=p.gradle {
                let config=match s.scope.as_str(){"compile"=>"implementation","runtime"=>"runtimeOnly","test"=>"testImplementation",_=>return Err("Gradle does not infer provided or unknown scope mappings.".into())};
                let reference=if let Some(c)=&catalog {
                    let (line,accessor)=c.entry(&s.artifact).ok_or("A version catalog alias for this starter already names another module.")?;
                    if !line.is_empty(){catalog_lines.push(line);}
                    format!("libs.{accessor}")
                } else {
                    format!("{}org.springframework.boot:{}{}",if kotlin{"\""}else{"'"},s.artifact,if kotlin{"\""}else{"'"})
                };
                insertion.push_str(&if kotlin {format!("{inner}{config}({reference}){n}")}else{format!("{inner}{config} {reference}{n}")});
                continue;
            }
            insertion.push_str(&format!(
                "{inner}<dependency>{n}{inner}    <groupId>{}</groupId>{n}{inner}    <artifactId>{}</artifactId>{n}",
                s.group, s.artifact
            ));
            if let Some(version) = &s.version {
                insertion.push_str(&format!("{inner}    <version>{version}</version>{n}"));
            }
            if s.scope != "compile" {
                insertion.push_str(&format!("{inner}    <scope>{}</scope>{n}", s.scope));
            }
            insertion.push_str(&format!("{inner}</dependency>{n}"));
        }
        insertion.push_str(&p.after);
        let mut after = doc.text.clone();
        after.replace_range(p.at..p.end, &insertion);
        if proof(&Document { text: after.clone(), ..doc.clone() }).is_none() {
            return Err(Error::Stale);
        }
        let catalog_edit = catalog.as_ref().filter(|_| !catalog_lines.is_empty()).map(|c| {
            let (at, text) = c.insertion(&catalog_lines, p.newline);
            let mut resulting = c.text.clone();
            resulting.insert_str(at, &text);
            (c.path.clone(), at, text, resulting)
        });
        let mut preview = format!(
            "File: {}\nVersions come from {} (Spring Boot {}).\n{}\n\nChange, with 3 lines around it:\n{}",
            doc.path, p.management, cache.boot,
            if p.boot.is_empty() { "Each starter gets the version start.spring.io lists for this Spring Boot version. No repositories or BOMs are added." } else { "No repositories, BOMs or versions are added." },
            hunk(&after, p.at, p.at + insertion.len())
        );
        if let Some((path, at, text, resulting)) = &catalog_edit {
            preview.push_str(&format!("\n\nVersion catalog {path}, change:\n{}", hunk(resulting, *at, at + text.len())));
        }
        let title = format!("Add {} Spring Boot Starter{}", ids.len(), if ids.len() == 1 { "" } else { "s" });
        if !ui_kit::prompts::confirm_exact(&format!("{title} to {}?", doc.path.rsplit('/').next().unwrap_or_default()), &preview)? {
            return Err(Error::Cancelled);
        }
        fresh_source(doc, &pending.root)?;
        if let Some(c) = &catalog {
            if host::project_of(&c.path).ok_or(Error::Stale)?.read_file(&c.path)? != c.text {
                return Err(Error::Stale);
            }
        }
        if root(doc).as_deref() != Some(&pending.root)
            || self.server != START
            || cache.at.elapsed() > Duration::from_secs(900)
            || host::storage::get(&session::locator(&self.owner, "selector"))? != cache.selector
        {
            return Err(Error::Stale);
        }
        self.starter_edits.pending.remove(token);
        let at = position(&doc.text, p.at);
        let mut edits = vec![(doc.path.clone(), vec![TextEdit { range: Range::new(at, position(&doc.text, p.end)), text: insertion }])];
        if let Some((path, offset, text, _)) = catalog_edit {
            let c = catalog.as_ref().ok_or(Error::Stale)?;
            let at = position(&c.text, offset);
            edits.push((path, vec![TextEdit { range: Range::new(at, at), text }]));
        }
        Ok(FileAction {
            title,
            kind: Some("source".into()),
            edits,
            ..FileAction::default()
        })
    }
}

pub(crate) fn generated_pom(text: &str, group: &str, artifact: &str, boot: &str, java: &str) -> bool {
    let doc = Document { path: "/project/pom.xml".into(), language: "xml".into(), version: 0, text: text.into() };
    proof(&doc).is_some_and(|p| p.boot == boot && p.management == "the spring-boot-starter-parent POM")
        && parse(text)
            .is_some_and(|n| {
                value(&n, 0, "groupId").as_deref() == Some(group)
                    && value(&n, 0, "artifactId").as_deref() == Some(artifact)
                    && value(&n, 0, "packaging").as_deref().unwrap_or("jar") == "jar"
                    && only(&n, 0, "properties").and_then(|(index, _)| value(&n, index, "java.version")).as_deref() == Some(java)
            })
}
/// Exact declared starter echo only; never an effective dependency resolver.
pub(crate) fn generated_starters(text: &str, ids: &[&str], boot: &str, coords: &serde_json::Value) -> bool {
    if ids.len() > 64 || coords["bootVersion"].as_str() != Some(boot) {
        return false;
    }
    let Some(nodes) = parse(text) else { return false; };
    let Some((dependencies, _)) = only(&nodes, 0, "dependencies") else { return ids.is_empty(); };
    let mut seen = BTreeSet::new();
    for id in ids {
        let d = &coords["dependencies"][id];
        let Some(object) = d.as_object() else { return false; };
        if object.keys().any(|key| !matches!(key.as_str(), "groupId" | "artifactId" | "scope" | "version" | "bom" | "repository"))
            || !d["version"].is_null() || !d["bom"].is_null() || !d["repository"].is_null()
            || d["groupId"].as_str() != Some("org.springframework.boot") {
            return false;
        }
        let Some(artifact) = d["artifactId"].as_str().filter(|a| literal(a) && a.starts_with("spring-boot-starter-")) else { return false; };
        let scope = if d["scope"].is_null() { "compile" } else { d["scope"].as_str().unwrap_or("") };
        if !matches!(scope, "compile" | "runtime" | "provided" | "test") || !seen.insert(artifact) { return false; }
        let Some((index, _)) = nodes.iter().enumerate().find(|(index, node)| node.parent == Some(dependencies)
            && value(&nodes, *index, "groupId").as_deref() == Some("org.springframework.boot")
            && value(&nodes, *index, "artifactId").as_deref() == Some(artifact)) else { return false; };
        if only(&nodes, index, "version").is_some() || value(&nodes, index, "scope").as_deref().unwrap_or("compile") != scope { return false; }
    }
    true
}
impl FileActions for Launcher {
    fn file_actions(&mut self, doc: &Document, selection: Range) -> Vec<FileAction> {
        let mut actions = self.import_file_actions(doc);
        actions.extend(self.starter_file_actions(doc, selection));
        actions
    }
    fn resolve_file_action(&mut self, doc: &Document, action: FileAction) -> Result<FileAction, Error> {
        if action.data.as_deref().is_some_and(|s| s.starts_with("import:")) {
            self.resolve_import(doc, action)
        } else {
            self.resolve_starter_file_action(doc, action)
        }
    }
}

#[cfg(test)]
impl Launcher {
    /// What the command does before its dialogs: bind the project, then load the starters.
    fn bound_cache(&mut self, ctx: &Invocation) -> Result<Document, Error> {
        self.bind(ctx)?;
        self.cache_starters(ctx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{mock::MockHost, text::offset};
    const POM: &str = "<project xmlns=\"http://maven.apache.org/POM/4.0.0\">\n  <modelVersion>4.0.0</modelVersion>\n  <parent><groupId>org.springframework.boot</groupId><artifactId>spring-boot-starter-parent</artifactId><version>3.5.16</version><relativePath/></parent>\n  <groupId>com.example</groupId><artifactId>demo</artifactId><version>1.0.0</version>\n  <!-- 😀 keep -->\n  <properties><java.version>17</java.version></properties>\n  <dependencies>\n  </dependencies>\n</project>\n";
    pub(super) const META: &str = r#"{"bootVersion":{"values":[{"id":"3.5.16"},{"id":"4.0.0"}]},"dependencies":{"values":[{"name":"Web","values":[{"id":"web","name":"Spring Web","compatibilityRange":"[3.0.0,4.0.0)"},{"id":"data-jpa","name":"JPA"},{"id":"cloud","name":"Cloud"},{"id":"unknown-range","compatibilityRange":"mystery"}]}]}}"#;
    pub(super) const COORDS: &str = r#"{"bootVersion":"3.5.16","dependencies":{"web":{"groupId":"org.springframework.boot","artifactId":"spring-boot-starter-web"},"data-jpa":{"groupId":"org.springframework.boot","artifactId":"spring-boot-starter-data-jpa","scope":"runtime"},"cloud":{"groupId":"org.springframework.cloud","artifactId":"spring-cloud-starter","bom":"cloud"},"unknown-range":{"groupId":"org.springframework.boot","artifactId":"spring-boot-starter-unknown"}}}"#;
    fn setup(text: &str) -> (MockHost, Launcher, Document) {
        setup_with(text, &["storage", "editor.read", "project.read", "net:start.spring.io"])
    }
    const WRITABLE: [&str; 5] = ["storage", "editor.read", "project.read", "project.write", "net:start.spring.io"];
    fn setup_with(text: &str, capabilities: &[&str]) -> (MockHost, Launcher, Document) {
        let host = MockHost::install().with_capabilities(capabilities);
        host.add_file("pom.xml", text);
        host.open_editor("/project/pom.xml", "xml", text, &[]);
        host.on_fetch(|req| {
            Ok(openidea_plugin::HttpResponse {
                status: 200,
                body: if req.url.contains("metadata/client") { META.as_bytes().to_vec() } else { COORDS.as_bytes().to_vec() },
                ..Default::default()
            })
        });
        let raw = host.invocation().editor().unwrap().document().unwrap();
        let doc = Document { path: "/project/pom.xml".into(), language: "xml".into(), text: raw.text, version: raw.version };
        (host, Launcher::default(), doc)
    }
    fn ready(text: &str) -> (MockHost, Launcher, Document) {
        let (host, mut l, doc) = setup(text);
        l.bound_cache(&host.invocation()).unwrap();
        (host, l, doc)
    }
    fn action(l: &mut Launcher, d: &Document) -> FileAction {
        l.file_actions(d, Range::default()).remove(0)
    }
    #[test]
    fn advertised_literal_gradle_starters_path_is_supported() {
        let h=MockHost::install().with_capabilities(&["storage","editor.read","project.read","net:start.spring.io"]);
        let text="plugins {\n    id 'java'\n    id 'org.springframework.boot' version '3.5.16'\n    id 'io.spring.dependency-management' version '1.1.7'\n}\ngroup = 'com.example'\nversion = '1.0.0'\nrepositories { mavenCentral() }\ndependencies {\n}\n";
        h.add_file("build.gradle",text);h.open_editor("/project/build.gradle","groovy",text,&[]);
        h.on_fetch(|req|Ok(openidea_plugin::HttpResponse{status:200,body:if req.url.contains("metadata/client"){META.as_bytes().to_vec()}else{COORDS.as_bytes().to_vec()},..Default::default()}));
        assert!(Launcher::default().bound_cache(&h.invocation()).is_ok(),"literal Gradle should prepare advertised version-bound starters");
    }
    #[test]
    fn command_picks_and_confirms_then_one_alt_enter_action_inserts_without_more_dialogs() {
        let (host, mut l, d) = setup(POM);
        host.answer_next_prompt(Some(&["web"]));
        host.answer_next_prompt(Some(&["yes"]));
        let message = l.run_command("add-starters", &host.invocation(), None).unwrap().unwrap();
        assert!(message.contains("Add Spring Boot Starters needs the project.write permission. Allow it in Settings › Plugins › Spring Initializr Launcher."), "{message}");
        assert!(message.contains("⌥⏎ in pom.xml") && message.contains("Add 1 Spring Boot Starter"), "{message}");
        let actions = l.file_actions(&d, Range::default());
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].title, "Add 1 Spring Boot Starter");
        let resolved = l.resolve_file_action(&d, actions[0].clone()).unwrap();
        assert!(apply_one(&d, &resolved).contains("spring-boot-starter-web"));
        assert!(l.resolve_file_action(&d, actions[0].clone()).is_err());
        assert_eq!(host.editor_text(), POM);
    }
    fn read(path: &str) -> String {
        host::project_of(path).unwrap().read_file(path).unwrap()
    }
    #[test]
    fn with_project_write_the_command_writes_the_module_pom_after_one_confirm_and_opens_it() {
        let (host, mut l, _) = setup_with(AGG, &WRITABLE);
        host.add_file("app/pom.xml", POM);
        host.add_file("lib/pom.xml", &POM.replace("<relativePath/>", ""));
        host.answer_next_prompt(Some(&["/project/app/pom.xml"]));
        host.answer_next_prompt(Some(&["web"]));
        host.answer_next_prompt(Some(&["yes"]));
        let message = l.run_command("add-starters", &host.invocation(), None).unwrap().unwrap();
        assert_eq!(message, "Added 1 Spring Boot Starter to app/pom.xml.");
        assert!(read("/project/app/pom.xml").contains("<artifactId>spring-boot-starter-web</artifactId>"));
        assert_eq!(read("/project/pom.xml"), AGG);
        assert_eq!(host.opened_locations()[0].0, "/project/app/pom.xml");
        assert!(l.starter_edits.staged.is_none());
    }
    #[test]
    fn cancelling_the_confirm_writes_nothing() {
        let (host, mut l, _) = setup_with(AGG, &WRITABLE);
        host.add_file("app/pom.xml", POM);
        host.answer_next_prompt(Some(&["/project/app/pom.xml"]));
        host.answer_next_prompt(Some(&["web"]));
        host.answer_next_prompt(None);
        assert!(matches!(l.run_command("add-starters", &host.invocation(), None), Err(Error::Cancelled)));
        assert_eq!(read("/project/app/pom.xml"), POM);
        assert!(l.starter_edits.staged.is_none());
    }
    #[test]
    fn unsaved_build_file_is_not_written_and_waits_for_alt_enter() {
        let (host, mut l, _) = setup_with(POM, &WRITABLE);
        host.type_in_editor("");
        host.answer_next_prompt(Some(&["web"]));
        host.answer_next_prompt(Some(&["yes"]));
        let message = l.run_command("add-starters", &host.invocation(), None).unwrap().unwrap();
        assert!(message.starts_with("pom.xml has unsaved changes, so it wasn't written.") && message.contains("⌥⏎ in pom.xml"), "{message}");
        assert!(l.starter_edits.staged.is_some());
    }
    #[test]
    fn build_file_changed_since_the_preview_is_refused() {
        let (host, mut l, _) = setup(POM);
        host.answer_next_prompt(Some(&["web"]));
        host.answer_next_prompt(Some(&["yes"]));
        l.run_command("add-starters", &host.invocation(), None).unwrap();
        let staged = l.starter_edits.staged.take().unwrap();
        host.type_in_editor("<!-- edited -->");
        let error = write_starters(&staged).unwrap_err();
        assert!(error.to_string().contains("changed since the preview, so nothing was written"), "{error}");
    }
    #[test]
    fn staged_starters_refuse_when_the_build_file_changed_and_cancel_stages_nothing() {
        let (host, mut l, d) = setup(POM);
        host.answer_next_prompt(Some(&["web"]));
        host.answer_next_prompt(None);
        assert!(matches!(l.run_command("add-starters", &host.invocation(), None), Err(Error::Cancelled)));
        assert!(l.starter_edits.staged.is_none());
        host.answer_next_prompt(Some(&["web"]));
        host.answer_next_prompt(Some(&["yes"]));
        l.run_command("add-starters", &host.invocation(), None).unwrap();
        let staged = l.file_actions(&d, Range::default()).remove(0);
        host.type_in_editor("changed");
        assert!(l.resolve_file_action(&d, staged).is_err());
    }
    #[test]
    fn cached_picker_full_preview_prepares_atomic_utf16_crlf_edit_without_direct_write() {
        let text = POM.replace('\n', "\r\n");
        let (host, mut l, d) = ready(&text);
        let prepared = action(&mut l, &d);
        let replay = prepared.clone();
        let before = host.fetches();
        host.answer_next_prompt(Some(&["web", "data-jpa"]));
        host.answer_next_prompt(Some(&["yes"]));
        let resolved = l.resolve_file_action(&d, prepared).unwrap();
        assert!(resolved.operations.is_empty());
        assert_eq!(resolved.edits.len(), 1);
        let (path, edits) = &resolved.edits[0];
        assert_eq!(path, &d.path);
        assert_eq!(edits.len(), 1);
        let e = &edits[0];
        let from = offset(&d.text, e.range.start).unwrap();
        assert_eq!(from, offset(&d.text, e.range.end).unwrap());
        assert_eq!(from, d.text.find("  </dependencies>").unwrap());
        assert!(e.text.contains("spring-boot-starter-web</artifactId>\r\n"));
        assert!(e.text.contains("<scope>runtime</scope>"));
        assert!(!e.text.contains("<version>"));
        assert!(!e.text.contains("cloud"));
        assert!(!e.text.contains("unknown"));
        assert_eq!(host.editor_text(), text);
        assert_eq!(host.fetches(), before);
        assert!(host.process_runs().is_empty());
        assert!(l.resolve_file_action(&d, replay).is_err());
    }
    #[test]
    fn command_uses_literal_source_boot_not_generation_form_and_provider_stays_cold() {
        let (host, mut l, d) = setup(POM);
        assert!(l.file_actions(&d, Range::default()).is_empty());
        assert_eq!(host.fetches(), 0);
        l.bind(&host.invocation()).unwrap();
        l.set("bootVersion", "4.0.0");
        l.bound_cache(&host.invocation()).unwrap();
        assert!(host.requests().iter().any(|r| r.url.ends_with("/dependencies?bootVersion=3.5.16")));
        assert_eq!(host.editor_text(), POM);
        assert_eq!(l.starter_edits.cache.as_ref().unwrap().starters.len(), 2);
    }
    #[test]
    fn cancellation_unknown_duplicate_or_excess_selection_never_writes() {
        for mode in 0..5 {
            let (host, mut l, d) = ready(POM);
            let prepared = action(&mut l, &d);
            match mode {
                0 => host.answer_next_prompt(None),
                1 => host.answer_next_prompt(Some(&[])),
                2 => host.answer_next_prompt(Some(&["cloud"])),
                3 => host.answer_next_prompt(Some(&["web", "web"])),
                _ => {
                    host.answer_next_prompt(Some(&["web"]));
                    host.answer_next_prompt(None);
                }
            }
            assert!(l.resolve_file_action(&d, prepared).is_err(), "{mode}");
            assert_eq!(host.editor_text(), POM);
        }
    }
    #[test]
    fn stale_document_root_metadata_selector_and_cache_are_rejected() {
        for mode in 0..8 {
            let (host, mut l, mut d) = ready(POM);
            let prepared = action(&mut l, &d);
            match mode {
                0 => d.version += 1,
                1 => d.text.push(' '),
                2 => host.type_in_editor("changed"),
                3 => host.set_project_root(Some("/other")),
                4 => l.metadata = Some(serde_json::json!({})),
                5 => session::select_server(&l.owner, "https://other.example").unwrap(),
                6 => l.starter_edits.cache.as_mut().unwrap().at = Instant::now() - Duration::from_secs(901),
                _ => host.add_file(".mvn/maven.config", "-Pcustom"),
            };
            let before = host.editor_text();
            host.answer_next_prompt(Some(&["web"]));
            host.answer_next_prompt(Some(&["yes"]));
            assert!(l.resolve_file_action(&d, prepared).is_err(), "{mode}");
            assert_eq!(host.editor_text(), before);
        }
    }
    #[test]
    fn unknown_management_and_namespace_forms_refuse_before_network() {
        for text in [
            POM.replace("<relativePath/>", ""),
            POM.replace("<relativePath/>", "<relativePath>../parent.xml</relativePath>"),
            POM.replace("3.5.16", "${boot.version}"),
            POM.replace("3.5.16", "3.5.16-SNAPSHOT"),
            POM.replace("</project>", "<dependencyManagement/></project>"),
            POM.replace("</project>", "<profiles/></project>"),
            POM.replace("</project>", "<repositories/></project>"),
            POM.replace("http://maven.apache.org/POM/4.0.0", "urn:other"),
            POM.replace("<java.version>", "<spring-framework.version>"),
            POM.replace("</dependencies>", "</wrong>"),
            POM.replace("</project>", "<dependencies></dependencies></project>"),
            format!("<!DOCTYPE project>{POM}"),
        ] {
            let (host, mut l, _) = setup(&text);
            assert!(l.bound_cache(&host.invocation()).is_err(), "{text}");
            assert_eq!(host.fetches(), 0);
            assert_eq!(host.editor_text(), text);
        }
    }
    #[test]
    fn misplaced_or_duplicate_xml_declarations_refuse_before_metadata_fetch() {
        let declaration = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>";
        for text in [
            POM.replace("<modelVersion>", &format!("{declaration}<modelVersion>")),
            format!("{declaration}{declaration}{POM}"),
            format!("<!-- before -->{declaration}{POM}"),
            format!(" {declaration}{POM}"),
            format!("{POM}{declaration}"),
        ] {
            let (host, mut l, _) = setup(&text);
            let result = l.bound_cache(&host.invocation());
            assert!(!matches!(&result, Err(Error::NotFound(_))), "must reach the actual Add Starters command");
            assert!(result.is_err(), "{text}");
            assert!(host.requests().is_empty());
            assert!(host.process_runs().is_empty());
        }
    }
    #[test]
    fn malformed_comments_reserved_pi_and_unbound_schema_attribute_refuse() {
        for text in [
            format!("<!-- bad--comment -->{POM}"),
            format!("<?XML version=\"1.0\"?>{POM}"),
            POM.replace("<project ", "<project xsi:schemaLocation=\"urn:x source.xsd\" "),
        ] {
            let (host, mut l, _) = setup(&text);
            let result = l.bound_cache(&host.invocation());
            assert!(!matches!(&result, Err(Error::NotFound(_))), "must reach the actual Add Starters command");
            assert!(result.is_err(), "{text}");
            assert!(host.requests().is_empty());
        }
    }
    #[test]
    fn ambiguous_or_unsupported_xml_declaration_attributes_refuse() {
        for declaration in [
            "<?xml version='1.0' version='1.1'?>",
            "<?xml encoding='UTF-8' version='1.0'?>",
            "<?xml version='1.2'?>",
            "<?xml version='1.0' standalone='maybe'?>",
            "<?xml version='1.0' encoding='not an encoding name'?>",
            "<?xml version='1.0' unknown='value'?>",
        ] {
            let text = format!("{declaration}{POM}");
            let (host, mut l, _) = setup(&text);
            let result = l.bound_cache(&host.invocation());
            assert!(!matches!(&result, Err(Error::NotFound(_))), "must reach the actual Add Starters command");
            assert!(result.is_err(), "{declaration}");
            assert!(host.requests().is_empty());
        }
    }
    #[test]
    fn leading_declaration_m2e_and_bound_schema_attribute_preserve_preview() {
        let text = format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<?m2e ignore?>\n{}", POM.replace("<project ", "<project xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:schemaLocation=\"http://maven.apache.org/POM/4.0.0 pom.xsd\" "));
        let (host, mut l, d) = ready(&text);
        let action = action(&mut l, &d);
        host.answer_next_prompt(Some(&["web"]));
        host.answer_next_prompt(Some(&["yes"]));
        let result = l.resolve_file_action(&d, action).unwrap();
        let after = apply_one(&d, &result);
        assert!(after.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<?m2e ignore?>\n"));
        assert!(after.contains("xsi:schemaLocation=\"http://maven.apache.org/POM/4.0.0 pom.xsd\""));
        assert!(after.contains("spring-boot-starter-web"));
        assert!(result.operations.is_empty());
        assert_eq!(host.editor_text(), text);
        for declaration in ["<?xml version='1.0' standalone='yes'?>", "<?xml version='1.1' encoding='UTF-8' standalone='no'?>"] {
            assert!(proof(&Document { text: format!("{declaration}{POM}"), ..d.clone() }).is_some());
        }
    }
    #[test]
    fn existing_coordinate_collisions_and_competing_module_configuration_refuse() {
        let existing=POM.replace("  </dependencies>","    <dependency><groupId>org.springframework.boot</groupId><artifactId>spring-boot-starter-web</artifactId></dependency>\n  </dependencies>");
        let (host, mut l, d) = ready(&existing);
        let prepared = action(&mut l, &d);
        host.answer_next_prompt(Some(&["web"]));
        assert!(l.resolve_file_action(&d, prepared).is_err());
        for config in [".mvn/maven.config", ".mvn/jvm.config", ".mvn/extensions.xml"] {
            let (host, mut l, _) = setup(POM);
            host.add_file(config, "custom");
            assert!(l.bound_cache(&host.invocation()).is_err());
            assert_eq!(host.fetches(), 0);
        }
    }
    #[test]
    fn add_starters_on_40kib_pom() {
        let text = POM.replace("<!-- 😀 keep -->", &format!("<!--\n{}-->", "padding line of a long build file\n".repeat(1300)));
        assert!(text.len() > 40 << 10);
        let (host, mut l, d) = ready(&text);
        let prepared = action(&mut l, &d);
        host.answer_next_prompt(Some(&["web"]));
        host.answer_next_prompt(Some(&["yes"]));
        let resolved = l.resolve_file_action(&d, prepared).unwrap();
        assert!(resolved.edits[0].1[0].text.contains("spring-boot-starter-web"));
    }
    #[test]
    fn hunk_shows_only_the_changed_lines_with_three_lines_of_context() {
        let text: String = (1..=20).map(|n| format!("line{n}\n")).collect();
        let at = text.find("line10").unwrap();
        let shown = hunk(&text, at, at + "line10\n".len());
        assert_eq!(shown, "Lines 7–13:\nline7\nline8\nline9\nline10\nline11\nline12\nline13");
    }
    #[test]
    fn add_starters_editor_denied_message() {
        let host = MockHost::install().with_capabilities(&["storage", "project.read", "net:start.spring.io"]);
        host.add_file("pom.xml", POM);
        host.open_editor("/project/pom.xml", "xml", POM, &[]);
        let error = Launcher::default().bound_cache(&host.invocation()).unwrap_err();
        assert!(error.to_string().contains("editor.read"), "{error}");
    }
    #[test]
    fn matching_artifact_in_another_group_is_not_a_coordinate_collision() {
        let text=POM.replace("  </dependencies>","    <dependency><groupId>other</groupId><artifactId>spring-boot-starter-web</artifactId><version>1.0.0</version></dependency>\n  </dependencies>");
        let (host, mut l, d) = ready(&text);
        let prepared = action(&mut l, &d);
        host.answer_next_prompt(Some(&["web"]));
        host.answer_next_prompt(Some(&["yes"]));
        let resolved = l.resolve_file_action(&d, prepared).unwrap();
        assert!(resolved.edits[0].1[0].text.contains("org.springframework.boot"));
        assert_eq!(host.editor_text(), text);
    }
    #[test]
    fn oversized_source_and_changed_or_malformed_server_selection_refuse_before_fetch() {
        for mode in 0..3 {
            let text =
                if mode == 0 { POM.replace("<!-- 😀 keep -->", &format!("<!--{}-->", " ".repeat(MAX_SOURCE))) } else { POM.into() };
            let (host, mut l, _) = setup(&text);
            l.bind(&host.invocation()).unwrap();
            if mode == 1 {
                session::select_server(&l.owner, "https://other.example").unwrap();
            }
            if mode == 2 {
                host::storage::set(&session::locator(&l.owner, "selector"), "corrupt").unwrap();
            }
            assert!(l.bound_cache(&host.invocation()).is_err());
            assert_eq!(host.fetches(), 0);
            assert_eq!(host.editor_text(), text);
        }
    }
    #[test]
    fn wrong_version_coordinate_response_or_unsupported_declared_metadata_refuses() {
        for mode in 0..3 {
            let (host, mut l, _) = setup(POM);
            host.on_fetch(move |req| {
                Ok(openidea_plugin::HttpResponse {
                    status: 200,
                    body: if req.url.contains("metadata/client") {
                        if mode == 0 { META.replace("3.5.16", "3.4.0").into_bytes() } else { META.as_bytes().to_vec() }
                    } else if mode == 1 {
                        COORDS.replace("\"bootVersion\":\"3.5.16\"", "\"bootVersion\":\"4.0.0\"").into_bytes()
                    } else {
                        COORDS
                            .replace(
                                "\"artifactId\":\"spring-boot-starter-web\"",
                                "\"artifactId\":\"spring-boot-starter-web\",\"version\":\"1.2.3\"",
                            )
                            .replace("\"scope\":\"runtime\"", "\"scope\":\"annotationProcessor\"")
                            .into_bytes()
                    },
                    ..Default::default()
                })
            });
            assert!(l.bound_cache(&host.invocation()).is_err(), "{mode}");
            assert!(l.starter_edits.cache.is_none());
        }
    }
    fn apply_one(d: &Document, action: &FileAction) -> String {
        let edits = &action.edits[0].1;
        assert_eq!(edits.len(), 1);
        let from = offset(&d.text, edits[0].range.start).unwrap();
        let to = offset(&d.text, edits[0].range.end).unwrap();
        let mut after = d.text.clone();
        after.replace_range(from..to, &edits[0].text);
        after
    }
    #[test]
    fn creates_absent_or_expands_empty_dependencies_with_complete_atomic_preview() {
        for empty in ["", "  <dependencies/>", "  <dependencies />"] {
            for newline in ["\n", "\r\n"] {
                let text = POM.replace("  <dependencies>\n  </dependencies>", empty).replace('\n', newline);
                let (host, mut l, d) = ready(&text);
                let a = action(&mut l, &d);
                let requests = host.fetches();
                host.answer_next_prompt(Some(&["web", "data-jpa"]));
                host.answer_next_prompt(Some(&["yes"]));
                let resolved = l.resolve_file_action(&d, a).unwrap();
                assert!(resolved.operations.is_empty());
                let after = apply_one(&d, &resolved);
                let p = proof(&Document { text: after.clone(), ..d.clone() }).unwrap();
                assert!(p.existing.contains(&("org.springframework.boot".into(), "spring-boot-starter-web".into())));
                assert!(p.existing.contains(&("org.springframework.boot".into(), "spring-boot-starter-data-jpa".into())));
                assert!(after.contains("<!-- 😀 keep -->"));
                assert!(after.contains(&format!("<dependencies>{newline}")));
                assert_eq!(after.matches("<dependencies>").count(), 1);
                assert_eq!(host.editor_text(), text);
                assert_eq!(host.fetches(), requests);
                if newline == "\r\n" {
                    assert!(!after.replace("\r\n", "").contains('\n'));
                }
            }
        }
    }
    #[test]
    fn missing_block_is_created_before_existing_build_without_reformatting_it() {
        let build = "  <build>\n    <plugins><plugin><groupId>org.springframework.boot</groupId><artifactId>spring-boot-maven-plugin</artifactId></plugin></plugins>\n  </build>\n";
        let text = POM.replace("  <dependencies>\n  </dependencies>\n", build);
        let (host, mut l, d) = ready(&text);
        let a = action(&mut l, &d);
        host.answer_next_prompt(Some(&["web"]));
        host.answer_next_prompt(Some(&["yes"]));
        let resolved = l.resolve_file_action(&d, a).unwrap();
        let after = apply_one(&d, &resolved);
        assert!(after.find("</dependencies>").unwrap() < after.find("<build>").unwrap());
        assert!(after.contains(build));
        let edit = &resolved.edits[0].1[0];
        assert_eq!(edit.range.start, edit.range.end);
    }
    #[test]
    fn empty_block_replacement_is_source_owned_cancelable_and_replay_safe() {
        let text = POM.replace("  <dependencies>\n  </dependencies>", "  <dependencies/>");
        let (host, mut l, d) = ready(&text);
        let a = action(&mut l, &d);
        host.answer_next_prompt(Some(&["web"]));
        host.answer_next_prompt(None);
        assert!(matches!(l.resolve_file_action(&d, a.clone()), Err(Error::Cancelled)));
        assert_eq!(host.editor_text(), text);
        host.answer_next_prompt(Some(&["web"]));
        host.answer_next_prompt(Some(&["yes"]));
        let resolved = l.resolve_file_action(&d, a.clone()).unwrap();
        let edit = &resolved.edits[0].1[0];
        let start = offset(&d.text, edit.range.start).unwrap();
        let end = offset(&d.text, edit.range.end).unwrap();
        assert_eq!(&d.text[start..end], "<dependencies/>");
        assert!(l.resolve_file_action(&d, a).is_err());
    }
    #[test]
    fn new_container_forms_keep_management_and_source_ambiguity_guards() {
        let absent = POM.replace("  <dependencies>\n  </dependencies>", "");
        for extra in ["<dependencyManagement/>", "<repositories/>", "<profiles/>"] {
            let text = absent.replace("</project>", &format!("{extra}</project>"));
            let (host, mut l, _) = setup(&text);
            assert!(l.bound_cache(&host.invocation()).is_err());
            assert_eq!(host.fetches(), 0);
        }
        let inline = absent.replace("\n\n</project>", "</project>");
        assert!(proof(&Document { path: "/project/pom.xml".into(), language: "xml".into(), version: 1, text: inline }).is_none());
    }
    fn bom_pom()->String {
        let mut text=POM.to_owned();let from=text.find("<parent>").unwrap();let end=text.find("</parent>").unwrap()+"</parent>".len();
        text.replace_range(from..end,"<dependencyManagement><dependencies><dependency><groupId>org.springframework.boot</groupId><artifactId>spring-boot-dependencies</artifactId><version>3.5.16</version><type>pom</type><scope>import</scope></dependency></dependencies></dependencyManagement>");text
    }
    #[test]
    fn sole_literal_import_bom_reuses_metadata_and_returns_only_previewed_atomic_dependency_edit() {
        let text=bom_pom().replace('\n',"\r\n");let (host,mut l,d)=ready(&text);assert!(proof(&d).unwrap().management.contains("imported"));let fetches=host.fetches();let a=action(&mut l,&d);
        host.answer_next_prompt(Some(&["web"]));host.answer_next_prompt(Some(&[]));let resolved=l.resolve_file_action(&d,a.clone()).unwrap();let after=apply_one(&d,&resolved);
        assert!(after.contains("spring-boot-starter-web"));assert!(after.contains("<artifactId>spring-boot-dependencies</artifactId><version>3.5.16</version>"));assert!(!after.contains("starter-parent"));assert!(proof(&Document{text:after.clone(),..d.clone()}).is_some());
        assert_eq!(after.replace(&resolved.edits[0].1[0].text,""),text,"existing BOM/source bytes are untouched");assert_eq!(host.editor_text(),text,"host applies returned edit, resolver never writes");assert_eq!(host.fetches(),fetches);assert!(l.resolve_file_action(&d,a).is_err());
    }
    #[test]
    fn imported_bom_requires_one_unambiguous_static_management_source_before_network() {
        let bom=bom_pom();let parent=&POM[POM.find("<parent>").unwrap()..POM.find("</parent>").unwrap()+"</parent>".len()];
        for text in [
            bom.replace("<scope>import</scope>","<scope>compile</scope>"),bom.replace("<type>pom</type>",""),bom.replace("spring-boot-dependencies","other-bom"),bom.replace("3.5.16","${boot.version}"),bom.replace("3.5.16","3.5.16-SNAPSHOT"),
            bom.replace("<dependencyManagement>",&format!("{parent}<dependencyManagement>")),
            bom.replace("</dependencyManagement>","</dependencyManagement><dependencyManagement/>"),
            bom.replace("</scope></dependency>","</scope><optional>true</optional></dependency>"),
            bom.replace("<groupId>com.example</groupId>",""),bom.replace("<version>1.0.0</version>",""),
            bom.replace("</project>","<repositories/></project>"),bom.replace("</project>","<profiles/></project>"),
        ] {let (host,mut l,_)=setup(&text);assert!(l.bound_cache(&host.invocation()).is_err(),"{text}");assert_eq!(host.fetches(),0);assert_eq!(host.editor_text(),text);}
    }
    #[test]
    fn imported_bom_cancel_stale_version_and_existing_coordinate_keep_source_unchanged() {
        let text=bom_pom();let (host,mut l,d)=ready(&text);let a=action(&mut l,&d);assert!(matches!(l.resolve_file_action(&d,a),Err(Error::Cancelled)));
        let a=action(&mut l,&d);let changed=Document{text:d.text.replace("3.5.16","4.0.0"),version:d.version+1,..d.clone()};assert!(l.resolve_file_action(&changed,a).is_err());assert_eq!(host.editor_text(),text);
        let existing=text.replace("  </dependencies>","    <dependency><groupId>org.springframework.boot</groupId><artifactId>spring-boot-starter-web</artifactId></dependency>\n  </dependencies>");let (host,mut l,d)=ready(&existing);let a=action(&mut l,&d);host.answer_next_prompt(Some(&["web"]));assert!(l.resolve_file_action(&d,a).is_err());assert_eq!(host.editor_text(),existing);
    }

    const AGG: &str = "<project xmlns=\"http://maven.apache.org/POM/4.0.0\">\n  <modelVersion>4.0.0</modelVersion>\n  <groupId>com.example</groupId><artifactId>agg</artifactId><version>1.0.0</version>\n  <packaging>pom</packaging>\n  <modules><module>app</module><module>lib</module></modules>\n</project>\n";
    #[test]
    fn aggregator_picks_proven_module_and_edits_only_that_pom() {
        let (host, mut l, d) = setup(AGG);
        host.add_file("app/pom.xml", POM);
        host.add_file("lib/pom.xml", &POM.replace("<relativePath/>", ""));
        host.answer_next_prompt(Some(&["/project/app/pom.xml"]));
        l.bound_cache(&host.invocation()).unwrap();
        let a = action(&mut l, &d);
        host.answer_next_prompt(Some(&["web"]));
        host.answer_next_prompt(Some(&["yes"]));
        let resolved = l.resolve_file_action(&d, a).unwrap();
        assert_eq!(resolved.edits.len(), 1);
        assert_eq!(resolved.edits[0].0, "/project/app/pom.xml");
        let module = Document { path: "/project/app/pom.xml".into(), text: POM.into(), ..d.clone() };
        let after = apply_one(&module, &resolved);
        assert!(after.contains("spring-boot-starter-web"));
        assert!(proof(&Document { text: after, ..module }).is_some());
        let (host, mut l, _) = setup(AGG);
        host.add_file("app/pom.xml", &POM.replace("<relativePath/>", ""));
        assert!(l.bound_cache(&host.invocation()).is_err());
        assert_eq!(host.fetches(), 0);
    }
    #[test]
    fn unmanaged_pom_gets_explicit_version_only_from_dependency_metadata() {
        let text = POM.replace("<parent><groupId>org.springframework.boot</groupId><artifactId>spring-boot-starter-parent</artifactId><version>3.5.16</version><relativePath/></parent>\n", "");
        let (host, mut l, d) = setup(&text);
        host.on_fetch(|req| {
            Ok(openidea_plugin::HttpResponse {
                status: 200,
                body: if req.url.contains("metadata/client") {
                    META.replace("{\"bootVersion\":{", "{\"bootVersion\":{\"default\":\"3.5.16\",").into_bytes()
                } else {
                    COORDS.replace("\"artifactId\":\"spring-boot-starter-web\"", "\"artifactId\":\"spring-boot-starter-web\",\"version\":\"1.2.3\"").into_bytes()
                },
                ..Default::default()
            })
        });
        l.bound_cache(&host.invocation()).unwrap();
        assert_eq!(l.starter_edits.cache.as_ref().unwrap().starters.len(), 1);
        let a = action(&mut l, &d);
        host.answer_next_prompt(Some(&["web"]));
        host.answer_next_prompt(Some(&["yes"]));
        let resolved = l.resolve_file_action(&d, a).unwrap();
        let after = apply_one(&d, &resolved);
        assert!(after.contains("<version>1.2.3</version>"));
        let p = proof(&Document { text: after, ..d.clone() }).unwrap();
        assert!(p.existing.contains(&("org.springframework.boot".into(), "spring-boot-starter-web".into())));
        let (host, mut l, _) = setup(&text);
        assert!(l.bound_cache(&host.invocation()).is_err());
    }
    #[test]
    fn real_start_spring_io_responses_yield_expected_starters() {
        const REAL_META: &str = include_str!("fixtures/initializr-metadata.json");
        const REAL_COORDS: &str = include_str!("fixtures/initializr-dependencies.json");
        let parent = "<parent><groupId>org.springframework.boot</groupId><artifactId>spring-boot-starter-parent</artifactId><version>3.5.16</version><relativePath/></parent>\n";
        let managed = POM.replace("<version>3.5.16</version>", "<version>4.1.2</version>");
        let unmanaged = POM.replace(parent, "");
        let ids = |text: &str| {
            let (host, mut l, _) = setup(text);
            host.on_fetch(|req| {
                Ok(openidea_plugin::HttpResponse {
                    status: 200,
                    body: if req.url.contains("metadata/client") { REAL_META } else { REAL_COORDS }.as_bytes().to_vec(),
                    ..Default::default()
                })
            });
            l.bound_cache(&host.invocation()).unwrap();
            l.starter_edits.cache.as_ref().unwrap().starters.iter().map(|s| (s.id.clone(), s.version.clone())).collect::<Vec<_>>()
        };
        assert_eq!(ids(&managed), vec![("web".to_string(), None)]);
        assert_eq!(ids(&unmanaged), vec![("camel".to_string(), Some("4.22.0".to_string()))]);
    }
}
