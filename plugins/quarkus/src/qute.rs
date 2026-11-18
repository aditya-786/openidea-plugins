//! Explicit source paths only; content generation and Qute type resolution remain LS-owned.
use super::*;
#[cfg(test)]
use openidea_plugin::file_actions::FileActions;
use openidea_plugin::file_actions::{FileAction, FileOperation};
use openidea_plugin::{FileKind, Project};
use spring_text::java as source;
use std::time::{Duration, Instant};
const MAX_TEXT: usize = 128 << 10;
#[derive(Clone)]
pub(super) struct Proof {
    path: String,
    text: String,
    at: Instant,
}
#[derive(Clone)]
struct Pending {
    root: String,
    module: String,
    doc: Document,
    template: Option<String>,
    field_at: Option<usize>,
}
#[derive(Default)]
pub(crate) struct State {
    pub(super) modules: BTreeMap<(String, String), Proof>,
    pending: BTreeMap<String, Pending>,
    inventories: BTreeMap<(String, String), Inventory>,
    removed_templates: BTreeSet<String>,
    navigation_events_lost: bool,
    next: u64,
}
struct Inventory {
    at: Instant,
    paths: BTreeSet<String>,
}
const NAV_TTL: Duration = Duration::from_secs(30);
const MAX_TEMPLATES: usize = 256;
fn absolute(path: &str) -> Option<String> {
    let path = path.replace('\\', "/");
    if path == "/"
        || path.contains("//")
        || path.len() == 3 && path.as_bytes().get(1) == Some(&b':')
    {
        return None;
    }
    let absolute = path.starts_with('/')
        || path.as_bytes().get(1) == Some(&b':')
            && path.as_bytes().get(2) == Some(&b'/')
            && path.as_bytes()[0].is_ascii_alphabetic();
    (absolute
        && path.len() <= 4096
        && !path.chars().any(char::is_control)
        && !path.split('/').any(|p| matches!(p, "." | "..")))
    .then(|| path.trim_end_matches('/').into())
}
fn inside(root: &str, path: &str) -> bool {
    path == root || path.starts_with(&format!("{root}/"))
}
fn module(root: &str, path: &str) -> Option<String> {
    let path = absolute(path)?;
    let module = if let Some(parent) = path.strip_suffix("/pom.xml") {
        parent
    } else if let Some((module, _)) = path.rsplit_once("/src/main/java/") {
        module
    } else if let Some((module, _)) = path.rsplit_once("/src/main/resources/templates/") {
        module
    } else if path == format!("{root}/.quarkus-cache") {
        root
    } else {
        return None;
    };
    inside(root, module).then(|| module.into())
}
fn template_path(path: &str) -> bool {
    if path.is_empty()
        || path.len() > 256
        || path.starts_with('/')
        || !path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/_-.".contains(&b))
        || ![".html", ".txt"]
            .iter()
            .any(|extension| path.ends_with(extension))
    {
        return false;
    }
    path.split('/').all(|part| {
        let stem = part.split('.').next().unwrap_or("").to_ascii_uppercase();
        !part.is_empty()
            && !part.starts_with('.')
            && !part.ends_with('.')
            && !matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            && !(stem.len() == 4
                && (stem.starts_with("COM") || stem.starts_with("LPT"))
                && matches!(stem.as_bytes()[3], b'1'..=b'9'))
    })
}
fn qute_tag_end(tail: &str) -> Option<usize> {
    let mut quote = None;
    let mut escape = false;
    for (at, character) in tail.char_indices().skip(1) {
        if let Some(delimiter) = quote {
            if escape { escape = false; }
            else if character == '\\' { escape = true; }
            else if character == delimiter { quote = None; }
        } else if matches!(character, '\'' | '"') { quote = Some(character); }
        else if character == '}' { return Some(at); }
    }
    None
}

/// The first unnamed include parameter is a literal template ID. `_id` is dynamic.
/// Return physical spans only; no transformed spelling is ever indexed for Rename.
fn include_literals(text: &str) -> Vec<(usize, usize, &str)> {
    if text.len() > MAX_TEXT { return vec![]; }
    let mut found = Vec::new();
    let mut cursor = 0;
    while let Some(next) = text[cursor..].find('{') {
        let start = cursor + next;
        let escaped = text[..start].bytes().rev().take_while(|b| *b == b'\\').count() % 2 != 0;
        if escaped { cursor = start + 1; continue; }
        let tail = &text[start..];
        if tail.starts_with("{!") || tail.starts_with("{|") {
            let end = if tail.starts_with("{!") { "!}" } else { "|}" };
            let Some(close) = tail.find(end) else { break; };
            cursor = start + close + end.len();
            continue;
        }
        let Some(end) = qute_tag_end(tail) else { break; };
        cursor = start + end + 1;
        let Some(body) = tail[..end].strip_prefix("{#include") else { continue; };
        if !body.starts_with(char::is_whitespace) { continue; }
        let body = body.trim();
        let Some(id) = body.split_whitespace().next() else { continue; };
        let id = id.strip_suffix('/').unwrap_or(id);
        if id.is_empty() || id.len() > 256 || id.starts_with('/')
            || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b"/_-.".contains(&b))
            || id.split('/').any(|part| part.is_empty() || part.starts_with('.') || part.ends_with('.'))
            || body.split_whitespace().any(|part| part == "_id" || part.starts_with("_id=")) { continue; }
        let at = start + "{#include".len() + tail["{#include".len()..end].len() - tail["{#include".len()..end].trim_start().len();
        found.push((at, at + id.len(), id));
    }
    found
}

fn injected_template(doc: &Document, at: Position) -> Option<String> {
    let offset = offset(&doc.text, at)?;
    if !balanced(&doc.text) { return None; }
    let file = source::parse(&doc.text);
    if file.types.iter().any(|ty| {
        let header = source::tokens(&doc.text[ty.at + ty.name.len()..ty.end]);
        let header: Vec<_> = header.iter().take_while(|token| token.s != "{").collect();
        header.iter().any(|t| t.s == "<") && header.iter().any(|t| matches!(t.s, "Template" | "Inject"))
    }) { return None; }
    for field in file.types.iter().flat_map(|ty| ty.fields()) {
        if field.modifiers.iter().any(|m| m == "static") { continue; }
        let Some(ty) = field.ty.as_ref() else { continue; };
        if ty.array || !ty.args.is_empty()
            || !(ty.text.trim() == "io.quarkus.qute.Template"
                || ty.text.trim() == "Template" && crate::java::source_type_owned(&file, "io.quarkus.qute.Template")) { continue; }
        let locations: Vec<_> = field.annotations.iter().filter(|a| a.name == "Location").collect();
        if !locations.is_empty() {
            let [location] = locations.as_slice() else { continue; };
            if !crate::java::annotation_owned(&file, &doc.text, location, "io.quarkus.qute.Location") { continue; }
            let Some(value) = location.arg("value") else { continue; };
            let [literal] = value.strings.as_slice() else { continue; };
            let id = literal.value.as_str();
            if source::tokens(&value.text).len() == 1 && literal.at <= offset && offset < literal.end
                && doc.text.get(literal.at..literal.end) == Some(id)
                && !id.is_empty() && id.len() <= 256 && !id.starts_with('/')
                && id.bytes().all(|b| b.is_ascii_alphanumeric() || b"/_-.".contains(&b))
                && id.split('/').all(|p| !p.is_empty() && !p.starts_with('.') && !p.ends_with('.')) {
                return Some(id.into());
            }
            continue;
        }
        if offset < field.at || offset >= field.at + field.name.len() { continue; }
        if !field.annotations.iter().any(|a| ["jakarta.inject.Inject", "javax.inject.Inject"].iter().any(|qualified| crate::java::annotation_owned(&file, &doc.text, a, qualified))) { continue; }
        if !field.name.is_empty() && field.name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            return Some(field.name.clone());
        }
    }
    None
}

fn defaulted_template_name(file: &source::JavaFile, annotation: Option<&source::Annotation>, name: &str) -> Option<String> {
    let Some(argument) = annotation.and_then(|a| a.arg("defaultName")) else { return Some(name.into()); };
    let expression: String = source::tokens(&argument.text).iter().map(|t| t.s).collect();
    let (owner, strategy) = expression.rsplit_once('.').unwrap_or(("", expression.as_str()));
    let qualified = "io.quarkus.qute.CheckedTemplate";
    let known = owner == qualified || owner == "CheckedTemplate" && crate::java::source_type_owned(file, qualified)
        || owner.is_empty() && !file.types.iter().flat_map(|t| &t.members).any(|m| m.name == strategy)
            && file.imports.iter().filter(|i| i.is_static && i.path.rsplit('.').next() == Some(strategy)).count() == 1
            && file.imports.iter().any(|i| i.is_static && i.path == format!("{qualified}.{strategy}"));
    if !known { return None; }
    match strategy {
        "ELEMENT_NAME" => Some(name.into()),
        "HYPHENATED_ELEMENT_NAME" => Some(crate::java::hyphenate(name)),
        "UNDERSCORED_ELEMENT_NAME" => Some(crate::java::hyphenate(name).replace('-', "_")),
        _ => None,
    }
}
fn checked_template(doc: &Document, at: Position) -> Option<String> {
    if !balanced(&doc.text) { return None; }
    let offset = offset(&doc.text, at)?;
    let file = source::parse(&doc.text);
    if file.types.iter().any(|ty| {
        let header = source::tokens(&doc.text[ty.at + ty.name.len()..ty.end]);
        let header: Vec<_> = header.iter().take_while(|token| token.s != "{").collect();
        header.iter().any(|t| t.s == "<") && header.iter().any(|t| matches!(t.s, "TemplateInstance" | "CheckedTemplate"))
    }) { return None; }
    let template_instance = |ty: &source::TypeRef| !ty.array && ty.args.is_empty()
        && (ty.text.trim() == "io.quarkus.qute.TemplateInstance"
            || ty.text.trim() == "TemplateInstance" && crate::java::source_type_owned(&file, "io.quarkus.qute.TemplateInstance"));
    for ty in &file.types {
        let annotations: Vec<_> = ty.annotations.iter().filter(|a| a.name == "CheckedTemplate").collect();
        if annotations.len() > 1 { continue; }
        let annotation = annotations.first().copied();
        if annotation.is_some_and(|a| !crate::java::annotation_owned(&file, &doc.text, a, "io.quarkus.qute.CheckedTemplate")) { continue; }
        let record = ty.kind == "record" && ty.implements.iter().any(template_instance);
        if annotation.is_none() && !record { continue; }
        let base = if let Some(base) = annotation.and_then(|a| a.arg("basePath")) {
            let [literal] = base.strings.as_slice() else { continue; };
            if source::tokens(&base.text).len() != 1 || doc.text.get(literal.at..literal.end) != Some(literal.value.as_str()) { continue; }
            literal.value.clone()
        } else if let Some(parent) = ty.outer {
            if !record && !ty.modifiers.iter().any(|m| m == "static") { continue; }
            file.types.get(parent)?.name.clone()
        } else { String::new() };
        if !base.is_empty() && (base.starts_with('/') || base.len() > 200 || !base.bytes().all(|b| b.is_ascii_alphanumeric() || b"/_-.".contains(&b)) || base.split('/').any(|p| p.is_empty() || p.starts_with('.') || p.ends_with('.'))) { continue; }
        let joined = |name: String| if base.is_empty() { name } else { format!("{base}/{name}") };
        if record && ty.at <= offset && offset < ty.at + ty.name.len() && !ty.name.contains('$') {
            return defaulted_template_name(&file, annotation, &ty.name).map(joined);
        }
        if annotation.is_none() { continue; }
        for method in ty.methods() {
            if !(method.at <= offset && offset < method.at + method.name.len())
                || !method.modifiers.iter().any(|m| m == "static") || !method.modifiers.iter().any(|m| m == "native")
                || method.body.is_some() || method.name.contains('$')
                || source::tokens(&doc.text[method.start..method.at]).iter().any(|token| token.s == "<")
                || ty.methods().filter(|m| m.name == method.name).count() != 1 { continue; }
            if !method.ty.as_ref().is_some_and(template_instance) { continue; }
            return defaulted_template_name(&file, annotation, &method.name).map(joined);
        }
    }
    None
}

const DEFAULT_SUFFIXES: [&str; 4] = ["qute.html", "qute.txt", "html", "txt"];
struct QuteConfig {
    suffixes: Vec<String>,
    excluded: bool,
}
/// Quarkus has no template-root property; literal `quarkus.qute.suffixes` is the provable custom lookup input.
/// `None` when the property is profiled, dynamic, YAML-only or outside what the html/txt inventory can prove.
fn qute_config(module: &str, path: &str) -> Option<QuteConfig> {
    let project = project_of(path)?;
    let resources = format!("{module}/src/main/resources");
    for yaml in ["application.yaml", "application.yml"] {
        if project.read_file(&format!("{resources}/{yaml}")).is_ok_and(|t| t.contains("suffixes") || t.contains("template-path-exclude")) {
            return None;
        }
    }
    let text = match project.read_file(&format!("{resources}/application.properties")) {
        Ok(text) if text.len() <= MAX_TEXT => text,
        Ok(_) => return None,
        Err(Error::NotFound(_)) | Err(Error::Invalid(_)) => String::new(),
        Err(_) => return None,
    };
    let mut suffixes = DEFAULT_SUFFIXES.map(String::from).to_vec();
    let mut excluded = false;
    let mut seen = false;
    for line in text.lines().map(str::trim).filter(|l| !l.starts_with(['#', '!'])) {
        let Some((key, value)) = line.split_once(['=', ':']) else { continue; };
        let key = key.trim();
        if key.contains("qute.template-path-exclude") { excluded = true; }
        if !key.contains("qute.suffixes") { continue; }
        let value = value.trim();
        if key != "quarkus.qute.suffixes" || seen || value.contains(['$', '{', '"', '\\']) { return None; }
        seen = true;
        suffixes = value.split(',').map(|v| v.trim().trim_start_matches('.').to_string()).collect();
        if suffixes.iter().any(|v| v.is_empty() || !(v.ends_with("html") || v.ends_with("txt")) || !v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.')) { return None; }
    }
    Some(QuteConfig { suffixes, excluded })
}
fn template_candidates<'a>(paths: &'a BTreeSet<String>, prefix: &str, id: &str, suffixes: &[String]) -> Vec<&'a String> {
    let base = format!("{prefix}{id}");
    paths.iter().filter(|p| **p == base || suffixes.iter().any(|s| **p == format!("{base}.{s}"))).collect()
}
/// Literal-only embedded template references: `(from, to, template id, variable/method/record name, declaration offset)`.
fn embedded_templates(doc: &Document) -> Vec<(usize, usize, String, String, usize)> {
    let file = source::parse(&doc.text);
    let mut found = Vec::new();
    for ty in &file.types {
        let mut probes = vec![(ty.at, ty.at + ty.name.len(), ty.name.clone(), ty.at)];
        for m in &ty.members {
            probes.push((m.at, m.at + m.name.len(), m.name.clone(), m.at));
            for l in m.annotations.iter().filter(|a| a.name == "Location").filter_map(|a| a.arg("value")).flat_map(|v| v.strings.iter()) {
                probes.push((l.at, l.end, m.name.clone(), m.at));
            }
        }
        for (from, to, name, decl) in probes {
            let at = openidea_plugin::text::position(&doc.text, from);
            if let Some(id) = injected_template(doc, at).or_else(|| checked_template(doc, at)) {
                if !found.iter().any(|f: &(usize, usize, String, String, usize)| f.0 == from) {
                    found.push((from, to, id, name, decl));
                }
            }
        }
    }
    found
}
fn data_literals(text: &str, pattern: &str, out: &mut BTreeSet<String>) {
    for (at, _) in text.match_indices(pattern) {
        let chain = &text[at..text.len().min(at + 2000)];
        let chain = &chain[..chain.find(';').unwrap_or(chain.len())];
        for (d, _) in chain.match_indices(".data(") {
            let rest = chain[d + 6..].trim_start();
            let Some(rest) = rest.strip_prefix('"') else { continue; };
            let Some(name) = rest.split('"').next() else { continue; };
            if !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') { out.insert(name.into()); }
        }
    }
}
/// Names provable for `{name}` in the template with `id`: checked-method parameters, record components and literal `.data("x", ..)` chains.
fn data_names(doc: &Document, id: &str, suffixes: &[String]) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    if doc.text.len() > MAX_TEXT || !balanced(&doc.text) { return names; }
    let file = source::parse(&doc.text);
    for (_, _, template, name, decl) in embedded_templates(doc) {
        if !(template == id || suffixes.iter().any(|s| format!("{template}.{s}") == id)) { continue; }
        for ty in &file.types {
            if ty.at == decl && ty.name == name {
                names.extend(ty.components.iter().map(|c| c.name.clone()));
                data_literals(&doc.text, &format!("new {name}("), &mut names);
            }
            for m in ty.members.iter().filter(|m| m.at == decl) {
                names.extend(m.params.iter().map(|p| p.name.clone()));
                data_literals(&doc.text, &format!("{}.{}(", ty.name, m.name), &mut names);
                data_literals(&doc.text, &format!("{}.", m.name), &mut names);
            }
        }
    }
    names
}
/// Literal `@TemplateData(namespace = "x")` / `@TemplateExtension(namespace = "x")` namespaces.
fn namespaces(text: &str) -> BTreeSet<String> {
    let file = source::parse(text);
    let mut found = BTreeSet::new();
    let annotated = file.types.iter().flat_map(|t| t.annotations.iter().chain(t.members.iter().flat_map(|m| m.annotations.iter())));
    for a in annotated.filter(|a| matches!(a.name.as_str(), "TemplateData" | "TemplateExtension")) {
        let Some(arg) = a.arg("namespace") else { continue; };
        let [lit] = arg.strings.as_slice() else { continue; };
        if source::tokens(&arg.text).len() == 1 && !lit.value.is_empty() && lit.value.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            found.insert(lit.value.clone());
        }
    }
    found
}

fn balanced(text: &str) -> bool {
    let mut stack = Vec::new();
    for t in source::tokens(text) {
        if t.s.starts_with('"') && (t.s.len() < 2 || !t.s.ends_with('"')) {
            return false;
        }
        match t.s {
            "(" => stack.push(")"),
            "[" => stack.push("]"),
            "{" => stack.push("}"),
            ")" | "]" | "}" => {
                if stack.pop() != Some(t.s) {
                    return false;
                }
            }
            _ => {}
        }
    }
    stack.is_empty()
}
fn named(file: &source::JavaFile, name: &str, qualified: &str) -> bool {
    !file.types.iter().any(|t| t.name == name)
        && file
            .imports
            .iter()
            .any(|i| !i.is_static && i.path == qualified)
        && !file
            .imports
            .iter()
            .any(|i| !i.is_static && i.path.rsplit('.').next() == Some(name) && i.path != qualified)
}
fn annotation(file: &source::JavaFile, text: &str, a: &source::Annotation) -> bool {
    let written: String = source::tokens(&text[a.at + 1..a.name_at + a.name.len()])
        .iter()
        .map(|t| t.s)
        .collect();
    written == "io.quarkus.qute.Location"
        || written == "Location" && named(file, "Location", "io.quarkus.qute.Location")
}
fn location(doc: &Document, selection: Range) -> Option<(usize, String)> {
    if doc.language != "java"
        || !doc.path.ends_with(".java")
        || doc.text.len() > MAX_TEXT
        || !balanced(&doc.text)
    {
        return None;
    }
    let from = offset(&doc.text, selection.start)?;
    let to = offset(&doc.text, selection.end)?;
    let file = source::parse(&doc.text);
    let shadow = file.types.iter().any(|ty| {
        let header = source::tokens(&doc.text[ty.at + ty.name.len()..ty.end]);
        let header: Vec<_> = header.iter().take_while(|token| token.s != "{").collect();
        header.iter().any(|t| t.s == "<")
            && header
                .iter()
                .any(|t| matches!(t.s, "Template" | "Location"))
    });
    if shadow
        || file.types.iter().any(|t| t.name == "io")
        || file
            .imports
            .iter()
            .any(|i| i.path.rsplit('.').next() == Some("io"))
    {
        return None;
    }
    for field in file.types.iter().flat_map(|t| t.fields()) {
        if field.modifiers.iter().any(|m| m == "static") || from < field.start || to > field.end {
            continue;
        }
        let ty = field.ty.as_ref()?;
        if ty.array
            || !ty.args.is_empty()
            || !(ty.text.trim() == "io.quarkus.qute.Template"
                || ty.text.trim() == "Template"
                    && named(&file, "Template", "io.quarkus.qute.Template"))
        {
            continue;
        }
        let annotations: Vec<_> = field
            .annotations
            .iter()
            .filter(|a| a.name == "Location")
            .collect();
        let [a] = annotations.as_slice() else {
            continue;
        };
        if !annotation(&file, &doc.text, a) {
            continue;
        }
        let value = a.arg("value")?;
        let [literal] = value.strings.as_slice() else {
            continue;
        };
        if source::tokens(&value.text).len() != 1
            || doc.text.get(literal.at..literal.end) != Some(literal.value.as_str())
            || !template_path(&literal.value)
        {
            continue;
        }
        return Some((field.at, literal.value.clone()));
    }
    None
}
/// Only direct, literal Maven dependencies. Profiles, dependencyManagement,
/// property interpolation and mixed XML namespaces do not establish this proof.
pub(super) fn namespace_valid<R>(
    reader: &quick_xml::reader::NsReader<R>,
    element: &quick_xml::events::BytesStart<'_>,
) -> bool {
    use quick_xml::name::ResolveResult;
    const XML: &str = "http://www.w3.org/XML/1998/namespace";
    const XMLNS: &str = "http://www.w3.org/2000/xmlns/";
    if element.name().as_ref().starts_with("xmlns:")
        || matches!(
            reader.resolver().resolve_element(element.name()).0,
            ResolveResult::Unknown(_)
        )
    {
        return false;
    }
    for attribute in element.attributes() {
        let Ok(attribute) = attribute else {
            return false;
        };
        let key = attribute.key.as_ref();
        if key == "xmlns" || key.starts_with("xmlns:") {
            let Ok(value) = attribute.normalized_value(quick_xml::XmlVersion::Implicit1_0) else {
                return false;
            };
            if key == "xmlns:xmlns"
                || key.starts_with("xmlns:") && value.is_empty()
                || key == "xmlns:xml" && value != XML
                || key != "xmlns:xml" && matches!(value.as_ref(), XML | XMLNS)
            {
                return false;
            }
        } else if matches!(
            reader.resolver().resolve_attribute(attribute.key).0,
            ResolveResult::Unknown(_)
        ) {
            return false;
        }
    }
    true
}
fn qute_pom(text: &str) -> bool {
    literal_maven_dependency(text, |artifact| matches!(artifact, "quarkus-qute" | "quarkus-rest-qute"))
}
pub(crate) fn literal_maven_dependency(text: &str, supported: impl Fn(&str) -> bool) -> bool {
    use quick_xml::{events::Event, reader::NsReader};
    if text.len() > MAX_TEXT {
        return false;
    }
    let mut reader = NsReader::from_str(text);
    reader.config_mut().trim_text(true);
    let mut stack: Vec<String> = Vec::new();
    let (mut group, mut artifact, mut found, mut closed, mut nodes) =
        (String::new(), String::new(), false, false, 0);
    let (mut groups, mut artifacts, mut unsupported) = (0, 0, false);
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                if !namespace_valid(&reader, &e) {
                    return false;
                }
                nodes += 1;
                if nodes > 2048 || stack.len() >= 64 {
                    return false;
                }
                let name = e.name().as_ref().to_owned();
                if name.contains(':') {
                    return false;
                }
                if stack.is_empty() && (name != "project" || closed) {
                    return false;
                }
                for attr in e.attributes() {
                    let Ok(attr) = attr else {
                        return false;
                    };
                    if attr.key.as_ref() == "xmlns"
                        && (!stack.is_empty()
                            || attr.value.as_ref() != "http://maven.apache.org/POM/4.0.0")
                    {
                        return false;
                    }
                }
                if stack
                    .iter()
                    .map(String::as_str)
                    .eq(["project", "dependencies", "dependency"])
                    && matches!(
                        name.as_str(),
                        "scope" | "type" | "classifier" | "systemPath"
                    )
                {
                    unsupported = true;
                }
                if stack.iter().map(String::as_str).eq([
                    "project",
                    "dependencies",
                    "dependency",
                    "groupId",
                ]) || stack.iter().map(String::as_str).eq([
                    "project",
                    "dependencies",
                    "dependency",
                    "artifactId",
                ]) {
                    unsupported = true;
                }
                stack.push(name);
                if stack.iter().map(String::as_str).eq([
                    "project",
                    "dependencies",
                    "dependency",
                    "groupId",
                ]) {
                    groups += 1;
                }
                if stack.iter().map(String::as_str).eq([
                    "project",
                    "dependencies",
                    "dependency",
                    "artifactId",
                ]) {
                    artifacts += 1;
                }
                if stack
                    .iter()
                    .map(String::as_str)
                    .eq(["project", "dependencies", "dependency"])
                {
                    group.clear();
                    artifact.clear();
                    groups = 0;
                    artifacts = 0;
                    unsupported = false;
                }
            }
            Ok(Event::Text(text)) => {
                let value = text.as_ref();
                if stack.iter().map(String::as_str).eq([
                    "project",
                    "dependencies",
                    "dependency",
                    "groupId",
                ]) {
                    group.push_str(&value);
                }
                if stack.iter().map(String::as_str).eq([
                    "project",
                    "dependencies",
                    "dependency",
                    "artifactId",
                ]) {
                    artifact.push_str(&value);
                }
            }
            Ok(Event::End(_)) => {
                if stack
                    .iter()
                    .map(String::as_str)
                    .eq(["project", "dependencies", "dependency"])
                {
                    found |= !unsupported
                        && groups == 1
                        && artifacts == 1
                        && group.trim() == "io.quarkus"
                        && supported(artifact.trim());
                }
                if stack.pop().is_none() {
                    return false;
                }
                if stack.is_empty() {
                    closed = true;
                }
            }
            Ok(Event::Empty(e)) => {
                if !namespace_valid(&reader, &e) {
                    return false;
                }
                if stack
                    .iter()
                    .map(String::as_str)
                    .eq(["project", "dependencies", "dependency"])
                    && matches!(
                        e.name().as_ref(),
                        "groupId" | "artifactId" | "scope" | "type" | "classifier" | "systemPath"
                    )
                {
                    unsupported = true;
                }
                if stack.is_empty() || e.name().as_ref().contains(':') {
                    return false;
                }
            }
            Ok(Event::Eof) => return found && closed && stack.is_empty(),
            Ok(Event::DocType(_) | Event::GeneralRef(_) | Event::CData(_)) | Err(_) => {
                return false;
            }
            _ => {}
        }
    }
}
fn proof(project: &Project, module: &str) -> Result<Proof, Error> {
    let path = format!("{module}/pom.xml");
    let metadata = project.stat(&path)?;
    if metadata.kind != FileKind::File || metadata.size > MAX_TEXT as u64 {
        return Err(Error::Stale);
    }
    let text = project.read_file(&path)?;
    if !qute_pom(&text) {
        return Err(Error::Invalid(
            "This module's pom.xml must list the io.quarkus:quarkus-qute dependency directly.".into(),
        ));
    }
    Ok(Proof {
        path,
        text,
        at: Instant::now(),
    })
}
fn collision(project: &Project, root: &str, target: &str) -> Result<(), Error> {
    if !inside(root, target) || absolute(target).as_deref() != Some(target) {
        return Err(Error::Stale);
    }
    if project.exists(target)? {
        return Err(Error::Invalid(
            "The template target already exists or is open; choose a new path.".into(),
        ));
    }
    let mut parent = target.rsplit_once('/').map(|(p, _)| p);
    while let Some(path) = parent {
        if !inside(root, path) {
            return Err(Error::Stale);
        }
        if project.exists(path)? && project.stat(path)?.kind != FileKind::Directory {
            return Err(Error::Invalid(
                "Template parents must be ordinary project directories.".into(),
            ));
        }
        if path == root {
            break;
        }
        parent = path.rsplit_once('/').map(|(p, _)| p);
    }
    Ok(())
}
impl QuarkusTools {
    pub(crate) fn warm_qute(&mut self, path: &str) {
        let Some(project) = project_of(path) else {
            return;
        };
        let Some(root) = project.root().ok().and_then(|r| absolute(&r)) else {
            return;
        };
        let Some(module) = module(&root, path) else {
            return;
        };
        let key = (root, module.clone());
        if self.live.build_events_lost
            || self
                .live
                .removed_builds
                .contains(&format!("{module}/pom.xml"))
        {
            self.qute.modules.remove(&key);
            return;
        }
        match proof(&project, &module) {
            Ok(proof) => {
                if self.qute.modules.len() >= 16 {
                    self.qute.modules.clear();
                }
                self.qute.modules.insert(key.clone(), proof);
                if self.live.watching
                    && !self
                        .qute
                        .inventories
                        .get(&key)
                        .is_some_and(|i| i.at.elapsed() < NAV_TTL)
                {
                    self.qute.inventories.remove(&key);
                    if let Ok(paths) = project.find_files(
                        "**/src/main/resources/templates/**",
                        (MAX_TEMPLATES + 1) as u32,
                    ) {
                        if paths.len() <= MAX_TEMPLATES {
                            let prefix = format!("{module}/src/main/resources/templates/");
                            let known = paths
                                .into_iter()
                                .filter(|path| {
                                    absolute(path).as_deref() == Some(path.as_str())
                                        && path.strip_prefix(&prefix).is_some_and(template_path)
                                        && project_of(path).and_then(|p| p.root().ok()).as_deref()
                                            == Some(key.0.as_str())
                                        && !self.qute.removed_templates.iter().any(|p| {
                                            path == p || path.starts_with(&format!("{p}/"))
                                        })
                                        && project
                                            .stat(path)
                                            .is_ok_and(|s| s.kind == FileKind::File)
                                })
                                .collect();
                            if self.qute.inventories.len() >= 16 {
                                self.qute.inventories.clear();
                            }
                            self.qute.inventories.insert(
                                key,
                                Inventory {
                                    at: Instant::now(),
                                    paths: known,
                                },
                            );
                        }
                    }
                }
            }
            Err(_) => {
                self.qute.modules.remove(&key);
                self.qute.inventories.remove(&key);
            }
        }
    }
}
impl QuarkusTools {
    /// Cache invalidation must precede discovery deadline checks, including removal.
    pub(super) fn invalidate_qute_navigation(&mut self, events: &[openidea_plugin::Event]) {
        use openidea_plugin::Event;
        for event in events {
            if let Event::FilesChanged(b) = event {
                for path in b.created.iter().chain(&b.modified) {
                    self.qute.removed_templates.remove(path);
                }
                for path in b
                    .removed
                    .iter()
                    .filter(|p| p.contains("/src/main/resources/templates/"))
                {
                    if path.len() > 4096 || self.qute.removed_templates.len() >= 1024 {
                        self.qute.navigation_events_lost = true;
                    } else {
                        self.qute.removed_templates.insert(path.clone());
                    }
                }
            }
            let paths: Vec<&str> = match event {
                Event::FilesChanged(b) => b
                    .created
                    .iter()
                    .chain(&b.modified)
                    .chain(&b.removed)
                    .map(String::as_str)
                    .collect(),
                Event::FileOpened(p) | Event::FileSaved(p) => vec![p.as_str()],
                _ => vec![],
            };
            self.qute.inventories.retain(|(_, module), _| {
                !paths.iter().any(|p| {
                    p.starts_with(&format!("{module}/src/main/resources/templates/"))
                        || BUILD_FILES.iter().any(|b| *p == format!("{module}/{b}"))
                })
            });
        }
    }
    pub(super) fn qute_folding(&self, doc: &Document) -> Vec<openidea_plugin::language_features::FoldRange> {
        if doc.text.len() > MAX_TEXT || !matches!(doc.language.as_str(), "html" | "plaintext" | "qute-html") { return vec![]; }
        let Some(root) = self.cached_module(doc) else { return vec![]; };
        let Some(module) = module(&root, &doc.path) else { return vec![]; };
        if !doc.path.strip_prefix(&format!("{module}/src/main/resources/templates/")).is_some_and(template_path) { return vec![]; }
        let mut cursor = 0;
        let mut stack: Vec<(String, usize)> = Vec::new();
        let mut folds = Vec::new();
        while let Some(next) = doc.text[cursor..].find('{') {
            let start = cursor + next;
            if doc.text[..start].bytes().rev().take_while(|b| *b == b'\\').count() % 2 != 0 { cursor = start + 1; continue; }
            let tail = &doc.text[start..];
            if tail.starts_with("{!") || tail.starts_with("{|") {
                let (ending, kind) = if tail.starts_with("{!") { ("!}", "comment") } else { ("|}", "region") };
                let Some(end) = tail.find(ending) else { break; };
                let lines = range(&doc.text, start, start+end+ending.len());
                if lines.start.line < lines.end.line { folds.push(openidea_plugin::language_features::FoldRange { start_line: lines.start.line, end_line: lines.end.line, kind: kind.into() }); }
                cursor = start + end + ending.len(); continue;
            }
            let Some(end) = qute_tag_end(tail) else { break; };
            cursor = start+end+1;
            let body = tail[1..end].trim();
            if let Some(open) = body.strip_prefix('#') {
                let name = open.split_whitespace().next().unwrap_or("");
                if !body.ends_with('/') && !matches!(name, "else" | "is" | "case") && !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                    if stack.len() >= 128 { return vec![]; }
                    stack.push((name.into(), start));
                }
            } else if let Some(close) = body.strip_prefix('/') {
                let name = close.trim();
                if name.is_empty() || stack.last().is_some_and(|(open, _)| open == name) {
                    if let Some((_, start)) = stack.pop() {
                        let lines = range(&doc.text, start, cursor);
                        if lines.start.line < lines.end.line { folds.push(openidea_plugin::language_features::FoldRange { start_line: lines.start.line, end_line: lines.end.line, kind: "region".into() }); }
                    }
                } else { stack.clear(); }
            }
            if folds.len() >= 256 { break; }
        }
        folds
    }
    pub(super) fn qute_diagnostics(&self, doc: &Document) -> Vec<Diagnostic> {
        if !matches!(doc.language.as_str(), "html" | "plaintext" | "qute-html") || doc.text.len() > MAX_TEXT || !self.live.watching || self.qute.navigation_events_lost { return vec![]; }
        let Some(root) = self.cached_module(doc) else { return vec![]; };
        let Some(module) = module(&root, &doc.path) else { return vec![]; };
        let prefix = format!("{module}/src/main/resources/templates/");
        if !doc.path.strip_prefix(&prefix).is_some_and(template_path) { return vec![]; }
        let key = (root, module);
        if !self.qute.modules.get(&key).is_some_and(|p| p.at.elapsed() < Duration::from_secs(120)) { return vec![]; }
        let Some(inventory) = self.qute.inventories.get(&key).filter(|i| i.at.elapsed() < NAV_TTL) else { return vec![]; };
        let Some(config) = qute_config(&key.1, &doc.path).filter(|c| !c.excluded) else { return vec![]; };
        include_literals(&doc.text).into_iter().filter_map(|(from, to, id)| {
            if !template_candidates(&inventory.paths, &prefix, id, &config.suffixes).is_empty() { return None; }
            let mut diagnostic = Diagnostic::new(range(&doc.text, from, to), openidea_plugin::Severity::Info,
                format!("No source template `{id}` exists in this module's conventional templates directory. A custom template locator may supply it at runtime."));
            diagnostic.code = Some("qute-source-include".into());
            Some(diagnostic)
        }).take(200).collect()
    }
    /// Warns only for a literal embedded reference when the fresh, complete inventory and config prove no file matches.
    pub(super) fn qute_java_diagnostics(&self, doc: &Document) -> Vec<Diagnostic> {
        if doc.language != "java" || doc.text.len() > MAX_TEXT || !balanced(&doc.text) || !self.live.watching || self.qute.navigation_events_lost { return vec![]; }
        let Some(root) = self.cached_module(doc) else { return vec![]; };
        let Some(module) = module(&root, &doc.path) else { return vec![]; };
        let key = (root, module);
        if !self.qute.modules.get(&key).is_some_and(|p| p.at.elapsed() < Duration::from_secs(120)) { return vec![]; }
        let Some(inventory) = self.qute.inventories.get(&key).filter(|i| i.at.elapsed() < NAV_TTL) else { return vec![]; };
        let Some(config) = qute_config(&key.1, &doc.path).filter(|c| !c.excluded) else { return vec![]; };
        let prefix = format!("{}/src/main/resources/templates/", key.1);
        embedded_templates(doc).into_iter().filter(|(.., id, _, _)| !id.is_empty() && !id.ends_with('/')).filter_map(|(from, to, id, _, _)| {
            if !template_candidates(&inventory.paths, &prefix, &id, &config.suffixes).is_empty() { return None; }
            let mut diagnostic = Diagnostic::new(range(&doc.text, from, to), openidea_plugin::Severity::Warning,
                format!("No source template `{id}` exists in this module's templates directory (suffixes: {}). A custom template locator may supply it at runtime.", config.suffixes.join(", ")));
            diagnostic.code = Some("qute-missing-template".into());
            Some(diagnostic)
        }).take(200).collect()
    }
    fn qute_data_completion(&self, doc: &Document, key: &(String, String), name_prefix: &str, replace: Range) -> Vec<CompletionItem> {
        let Some(project) = project_of(&doc.path) else { return vec![]; };
        let Some(config) = qute_config(&key.1, &doc.path) else { return vec![]; };
        let templates = format!("{}/src/main/resources/templates/", key.1);
        let Some(id) = doc.path.strip_prefix(&templates) else { return vec![]; };
        let Ok(files) = project.find_files("**/src/main/java/**/*.java", 501) else { return vec![]; };
        if files.len() > 500 { return vec![]; }
        let java = format!("{}/src/main/java/", key.1);
        let (mut names, mut spaces) = (BTreeSet::new(), BTreeSet::new());
        for file in files.iter().filter(|f| f.starts_with(&java)) {
            let Ok(text) = project.read_file(file) else { continue; };
            if text.len() > MAX_TEXT || !(text.contains("io.quarkus.qute") || text.contains("Qute")) { continue; }
            let source = Document { path: file.clone(), language: "java".into(), version: 0, text };
            names.extend(data_names(&source, id, &config.suffixes));
            spaces.extend(namespaces(&source.text).into_iter().map(|n| format!("{n}:")));
        }
        names.into_iter().chain(spaces).filter(|n| n.starts_with(name_prefix)).take(100).map(|n| {
            let mut item = CompletionItem::new(&n, CompletionKind::Variable);
            item.replace = Some(replace);
            item.detail = Some("Literal Qute data name from Java source".into());
            item
        }).collect()
    }
    pub(super) fn qute_completion(&self, doc: &Document, at: Position) -> Vec<CompletionItem> {
        if !matches!(doc.language.as_str(), "html" | "plaintext" | "qute-html") || doc.text.len() > MAX_TEXT || !self.live.watching || self.qute.navigation_events_lost { return vec![]; }
        let Some(root) = self.cached_module(doc) else { return vec![]; };
        let Some(module) = module(&root, &doc.path) else { return vec![]; };
        let prefix = format!("{module}/src/main/resources/templates/");
        if !doc.path.strip_prefix(&prefix).is_some_and(template_path) { return vec![]; }
        let key = (root, module);
        if !self.qute.modules.get(&key).is_some_and(|p| p.at.elapsed() < Duration::from_secs(120)) { return vec![]; }
        let Some(offset) = offset(&doc.text, at) else { return vec![]; };
        let mut cursor = 0;
        let before = &doc.text[..offset];
        let mut slot = None;
        while let Some(next) = before[cursor..].find('{') {
            let start = cursor + next;
            if before[..start].bytes().rev().take_while(|b| *b == b'\\').count() % 2 != 0 { cursor = start + 1; continue; }
            let tail = &before[start..];
            let end = if tail.starts_with("{!") { "!}" } else if tail.starts_with("{|") { "|}" } else { "}" };
            if let Some(close) = tail.find(end) { cursor = start + close + end.len(); continue; }
            if end == "}" { slot = Some((start, tail)); }
            break;
        }
        let Some((start, tail)) = slot else { return vec![]; };
        if let Some(name) = tail.strip_prefix("{#") {
            if name.bytes().all(|b| b.is_ascii_alphabetic()) {
                let sections = [("if", "if ${1:condition}}\n$0\n{/if}"), ("for", "for ${1:item} in ${2:items}}\n$0\n{/for}"), ("each", "each ${1:items}}\n$0\n{/each}"), ("let", "let ${1:name}=${2:value}}\n$0\n{/let}"), ("with", "with ${1:value}}\n$0\n{/with}"), ("include", "include ${1:template} /}"), ("insert", "insert ${1:name}}$0{/insert}"), ("fragment", "fragment ${1:name}}\n$0\n{/fragment}"), ("when", "when ${1:value}}\n$0\n{/when}")];
                return sections.into_iter().filter(|(label, _)| label.starts_with(name)).map(|(label, snippet)| {
                    let mut item = CompletionItem::new(label, CompletionKind::Snippet);
                    item.insert_text = snippet.into(); item.snippet = true;
                    item.replace = Some(range(&doc.text, start + 2, offset + usize::from(doc.text[offset..].starts_with('}'))));
                    item.detail = Some("Qute section".into()); item
                }).collect();
            }
        }
        if let Some(value) = tail.strip_prefix("{#include ") {
            if !value.bytes().all(|b| b.is_ascii_alphanumeric() || b"/_-.".contains(&b)) { return vec![]; }
            let Some(inventory) = self.qute.inventories.get(&key).filter(|i| i.at.elapsed() < NAV_TTL) else { return vec![]; };
            return inventory.paths.iter().filter_map(|path| path.strip_prefix(&prefix)).filter(|id| id.starts_with(value)).take(100).map(|id| {
                let mut item = CompletionItem::new(id, CompletionKind::File);
                item.replace = Some(range(&doc.text, start + "{#include ".len(), offset));
                item.detail = Some("Cached Qute template".into()); item
            }).collect();
        }
        let name = &tail[1..];
        if name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            return self.qute_data_completion(doc, &key, name, range(&doc.text, start + 1, offset));
        }
        vec![]
    }
    pub(super) fn qute_definition(
        &self,
        doc: &Document,
        at: Position,
    ) -> Vec<openidea_plugin::Location> {
        let Some(root) = self.cached_module(doc) else {
            return vec![];
        };
        if !self.live.watching || self.qute.navigation_events_lost {
            return vec![];
        }
        let Some(module) = module(&root, &doc.path) else {
            return vec![];
        };
        let key = (root, module.clone());
        if !self
            .qute
            .modules
            .get(&key)
            .is_some_and(|p| p.at.elapsed() < Duration::from_secs(120))
        {
            return vec![];
        }
        let Some(inventory) = self
            .qute
            .inventories
            .get(&key)
            .filter(|i| i.at.elapsed() < NAV_TTL)
        else {
            return vec![];
        };
        if matches!(doc.language.as_str(), "html" | "plaintext" | "qute-html")
            && doc.path.starts_with(&format!("{module}/src/main/resources/templates/"))
            && doc.path.strip_prefix(&format!("{module}/src/main/resources/templates/")).is_some_and(template_path)
        {
            let Some(offset) = offset(&doc.text, at) else { return vec![]; };
            let Some((_, _, id)) = include_literals(&doc.text).into_iter().find(|(from, to, _)| *from <= offset && offset < *to) else { return vec![]; };
            let Some(config) = qute_config(&module, &doc.path) else { return vec![]; };
            let candidates = template_candidates(&inventory.paths, &format!("{module}/src/main/resources/templates/"), id, &config.suffixes);
            let [target] = candidates.as_slice() else { return vec![]; };
            return vec![openidea_plugin::Location { path: (*target).clone(), range: Range::default() }];
        }
        if doc.language == "java" && doc.text.len() <= MAX_TEXT {
            if let Some(name) = injected_template(doc, at).or_else(|| checked_template(doc, at)) {
                let Some(config) = qute_config(&module, &doc.path) else { return vec![]; };
                let candidates = template_candidates(&inventory.paths, &format!("{module}/src/main/resources/templates/"), &name, &config.suffixes);
                if let [target] = candidates.as_slice() {
                    return vec![openidea_plugin::Location { path: (*target).clone(), range: Range::default() }];
                }
                return vec![];
            }
        }
        let Some((field_at, path)) = location(doc, Range::new(at, at)) else {
            return vec![];
        };
        // Go-to-definition belongs to the exact literal, not the whole field.
        let Some(off) = offset(&doc.text, at) else {
            return vec![];
        };
        let file = source::parse(&doc.text);
        let literal = file
            .types
            .iter()
            .flat_map(|t| t.fields())
            .filter(|f| f.at == field_at)
            .flat_map(|f| f.annotations.iter())
            .filter(|a| a.name == "Location")
            .filter_map(|a| a.arg("value"))
            .flat_map(|a| a.strings.iter())
            .any(|s| s.at <= off && off < s.end);
        let target = format!("{module}/src/main/resources/templates/{path}");
        if !literal || !inventory.paths.contains(&target) {
            return vec![];
        }
        vec![openidea_plugin::Location {
            path: target,
            range: Range::default(),
        }]
    }
    pub(super) fn qute_file_actions(
        &mut self,
        doc: &Document,
        selection: Range,
    ) -> Vec<FileAction> {
        if doc.text.len() > MAX_TEXT {
            return Vec::new();
        }
        let Some(root) = project_of(&doc.path)
            .and_then(|p| p.root().ok())
            .and_then(|r| absolute(&r))
        else {
            return Vec::new();
        };
        let Some(module) = module(&root, &doc.path) else {
            return Vec::new();
        };
        let source =
            if doc.language == "xml" && doc.path.ends_with("/pom.xml") && qute_pom(&doc.text) {
                None
            } else {
                let Some((at, path)) = location(doc, selection) else {
                    return Vec::new();
                };
                if !self
                    .qute
                    .modules
                    .get(&(root.clone(), module.clone()))
                    .is_some_and(|proof| proof.at.elapsed() < Duration::from_secs(120))
                {
                    return Vec::new();
                }
                Some((at, path))
            };
        let Some(next) = self.qute.next.checked_add(1) else {
            return Vec::new();
        };
        self.qute.next = next;
        if self.qute.pending.len() >= 4 {
            self.qute.pending.clear();
        }
        let token = format!("qute:{next}:{}", doc.version);
        let (field_at, template) = source
            .map(|(at, path)| (Some(at), Some(path)))
            .unwrap_or((None, None));
        let title = template
            .as_ref()
            .map(|path| format!("Create empty Qute template {path}…"))
            .unwrap_or_else(|| "Create empty Qute template in this Maven module…".into());
        self.qute.pending.insert(
            token.clone(),
            Pending {
                root,
                module,
                doc: doc.clone(),
                template,
                field_at,
            },
        );
        vec![FileAction {
            title,
            kind: Some("quickfix".into()),
            data: Some(token),
            ..FileAction::default()
        }]
    }
    pub(super) fn resolve_qute_file_action(
        &mut self,
        doc: &Document,
        action: FileAction,
    ) -> Result<FileAction, Error> {
        let token = action.data.as_deref().ok_or(Error::Stale)?;
        let pending = self.qute.pending.get(token).cloned().ok_or(Error::Stale)?;
        if doc.path != pending.doc.path
            || doc.language != pending.doc.language
            || doc.version != pending.doc.version
            || doc.text != pending.doc.text
        {
            return Err(Error::Stale);
        }
        let project = project_of(&doc.path).ok_or(Error::Stale)?;
        if project.root().ok().and_then(|r| absolute(&r)).as_deref() != Some(&pending.root)
            || project.read_file(&doc.path)? != doc.text
        {
            return Err(Error::Stale);
        }
        if self.live.build_events_lost
            || self
                .live
                .removed_builds
                .contains(&format!("{}/pom.xml", pending.module))
        {
            return Err(Error::Stale);
        }
        let build = proof(&project, &pending.module)?;
        if let Some(at) = pending.field_at {
            let pos = openidea_plugin::text::position(&doc.text, at);
            if location(doc, Range::new(pos, pos))
                .as_ref()
                .map(|(_, path)| path)
                != pending.template.as_ref()
            {
                return Err(Error::Stale);
            }
        }
        let template = match &pending.template {
            Some(path) => path.clone(),
            None => host::input(
                "Template path below src/main/resources/templates (.html or .txt)",
                None,
            )?
            .ok_or(Error::Cancelled)?,
        };
        if !template_path(&template) {
            return Err(Error::Invalid("Use a relative ASCII .html or .txt path without traversal, expressions or reserved names.".into()));
        }
        let target = format!("{}/src/main/resources/templates/{template}", pending.module);
        collision(&project, &pending.root, &target)?;
        let body = format!(
            "Create only: {target}\nModule: {}\nBuild file: {} (lists quarkus-qute)\nSource: {}\nSource version: {}\n\nExact new content: empty file (0 bytes).\n\nThe host creates and opens one non-overwriting file as an undoable workspace edit. Template parameters, bindings and runtime lookup are unresolved.",
            pending.module, build.path, doc.path, doc.version
        );
        if !ui_kit::prompts::confirm_exact("Create empty Qute template?", &body)? {
            return Err(Error::Cancelled);
        }
        if project_of(&doc.path)
            .and_then(|p| p.root().ok())
            .and_then(|r| absolute(&r))
            .as_deref()
            != Some(&pending.root)
            || project.read_file(&doc.path)? != doc.text
            || project.read_file(&build.path)? != build.text
        {
            return Err(Error::Stale);
        }
        collision(&project, &pending.root, &target)?;
        self.qute.pending.remove(token);
        Ok(FileAction {
            title: format!("Create Qute template {template}"),
            kind: Some("quickfix".into()),
            operations: vec![FileOperation::Create {
                path: target.clone(),
                text: String::new(),
                overwrite: false,
            }],
            open: Some(target),
            ..FileAction::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{mock::MockHost, text::position};
    const POM: &str = "<project><dependencies><dependency><groupId>io.quarkus</groupId><artifactId>quarkus-qute</artifactId></dependency></dependencies></project>";
    const JAVA: &str = "import io.quarkus.qute.Template; import io.quarkus.qute.Location; class View { /* 😀 */ @Location(\"pages/home.html\") Template home; }";
    fn doc(text: &str) -> Document {
        Document {
            path: "/project/src/main/java/View.java".into(),
            language: "java".into(),
            version: 7,
            text: text.into(),
        }
    }
    fn cursor(doc: &Document) -> Range {
        let pos = position(&doc.text, doc.text.rfind("home;").unwrap());
        Range::new(pos, pos)
    }
    fn setup() -> (MockHost, QuarkusTools, Document) {
        let host = MockHost::install();
        host.add_file("pom.xml", POM);
        host.add_file("src/main/java/View.java", JAVA);
        let mut tools = QuarkusTools::default();
        tools.warm_qute("/project/src/main/java/View.java");
        (host, tools, doc(JAVA))
    }
    #[test]
    fn literal_field_uses_utf16_and_exact_known_namespaces() {
        let doc = doc(JAVA);
        assert_eq!(location(&doc, cursor(&doc)).unwrap().1, "pages/home.html");
        let doc = self::doc(
            &JAVA
                .replace("@Location", "@io.quarkus.qute.Location")
                .replace(" Template home", " io.quarkus.qute.Template home"),
        );
        assert!(location(&doc, cursor(&doc)).is_some());
    }
    #[test]
    fn uncertain_or_nonliteral_fields_are_quiet() {
        for text in [
            JAVA.replace("io.quarkus.qute.Template", "other.Template"),
            JAVA.replace("io.quarkus.qute.Location", "other.Location"),
            JAVA.replace("class View", "class View<Template>"),
            JAVA.replace("class View", "class View<Location>"),
            JAVA.replace(" Template home", " static Template home"),
            JAVA.replace("pages/home.html", "../home.html"),
            JAVA.replace("\"pages/home.html\"", "PREFIX + \"pages/home.html\""),
            JAVA.replace("pages/home.html", "pages/ho\\u006de.html"),
            JAVA.replace("class View", "class io {} class View"),
            JAVA.trim_end_matches('}').into(),
        ] {
            let doc = doc(&text);
            assert!(location(&doc, cursor(&doc)).is_none(), "{text}");
        }
    }
    #[test]
    fn build_evidence_excludes_profile_managed_scoped_dynamic_or_malformed_dependencies() {
        assert!(qute_pom(POM));
        assert!(qute_pom(&POM.replace(
            "<project>",
            "<project xmlns=\"http://maven.apache.org/POM/4.0.0\">"
        )));
        for text in [
            POM.replace("<dependencies>", "<dependencyManagement><dependencies>")
                .replace("</dependencies>", "</dependencies></dependencyManagement>"),
            POM.replace("<dependencies>", "<profiles><profile><dependencies>")
                .replace("</dependencies>", "</dependencies></profile></profiles>"),
            POM.replace("quarkus-qute", "${qute}"),
            POM.replace("</artifactId>", "</artifactId><scope>test</scope>"),
            POM.replace(
                "</artifactId>",
                "</artifactId><groupId>io.quarkus</groupId>",
            ),
            POM.replace("<groupId>", "<groupId xmlns=\"foreign\">"),
            POM.replace("</dependency>", "</wrong>"),
            format!("{POM}{POM}"),
        ] {
            assert!(!qute_pom(&text), "{text}");
        }
    }
    #[test]
    fn path_policy_rejects_escape_and_platform_ambiguities() {
        for path in [
            "/home.html",
            "../home.html",
            "a/../home.html",
            "C:/home.html",
            "a//b.txt",
            "a/.b.html",
            "CON.html",
            "com1.txt",
            "a\\b.html",
            "a.html.",
            "a.html\n",
            "a.${x}.html",
        ] {
            assert!(!template_path(path), "{path}");
        }
        assert!(template_path("nested/a-b_1.html"));
        assert!(absolute("/").is_none());
        assert!(absolute("////").is_none());
        assert!(absolute("C:/").is_none());
        assert_eq!(
            module("/project", "/project/module/src/main/java/View.java").as_deref(),
            Some("/project/module")
        );
        assert!(module("/project", "/project-other/src/main/java/View.java").is_none());
    }
    #[test]
    fn current_unsaved_source_is_supported_without_a_disk_saved_claim() {
        let (host, mut tools, doc) = setup();
        host.add_file(
            &doc.path,
            &JAVA.replace("pages/home.html", "pages/disk.html"),
        );
        host.open_editor(&doc.path, "java", &doc.text, &[]);
        let action = tools.file_actions(&doc, cursor(&doc)).remove(0);
        host.answer_next_prompt(Some(&["yes"]));
        let resolved = tools.resolve_file_action(&doc, action).unwrap();
        assert_eq!(
            resolved.open.as_deref(),
            Some("/project/src/main/resources/templates/pages/home.html")
        );
        assert!(host.process_runs().is_empty());
    }
    #[test]
    fn delivered_build_deletion_rejects_pending_generation_despite_retained_readable_text() {
        let (_host, mut tools, doc) = setup();
        let action = tools.file_actions(&doc, cursor(&doc)).remove(0);
        tools.live_events(&[openidea_plugin::Event::FilesChanged(
            openidea_plugin::FilesChanged {
                created: vec![],
                modified: vec![],
                removed: vec!["/project/pom.xml".into()],
            },
        )]);
        tools.warm_qute(&doc.path);
        assert!(tools.file_actions(&doc, cursor(&doc)).is_empty());
        assert!(matches!(
            tools.resolve_file_action(&doc, action),
            Err(Error::Stale)
        ));
    }
    #[test]
    fn resolve_prepares_only_empty_nonoverwriting_create_and_consumes_token() {
        let (host, mut tools, doc) = setup();
        let action = tools.file_actions(&doc, cursor(&doc)).remove(0);
        let replay = action.clone();
        host.answer_next_prompt(Some(&["yes"]));
        let resolved = tools.resolve_file_action(&doc, action).unwrap();
        assert_eq!(resolved.operations.len(), 1);
        assert!(
            matches!(&resolved.operations[0], FileOperation::Create {path,text,overwrite:false} if path=="/project/src/main/resources/templates/pages/home.html" && text.is_empty())
        );
        assert_eq!(
            resolved.open.as_deref(),
            Some("/project/src/main/resources/templates/pages/home.html")
        );
        assert!(
            !project_of(&doc.path)
                .unwrap()
                .exists(resolved.open.as_ref().unwrap())
                .unwrap()
        );
        assert!(tools.resolve_file_action(&doc, replay).is_err());
        assert!(host.process_runs().is_empty());
        assert_eq!(host.fetches(), 0);
    }
    #[test]
    fn stale_visible_text_version_root_and_build_fail_before_create() {
        for mode in 0..4 {
            let (host, mut tools, mut doc) = setup();
            let action = tools.file_actions(&doc, cursor(&doc)).remove(0);
            match mode {
                0 => doc.version += 1,
                1 => host.add_file("src/main/java/View.java", "changed"),
                2 => host.set_project_root(Some("/other")),
                _ => host.add_file("pom.xml", "<project/>"),
            }
            host.answer_next_prompt(Some(&["yes"]));
            assert!(tools.resolve_file_action(&doc, action).is_err());
        }
    }
    #[test]
    fn cancellation_and_existing_target_or_parent_never_create() {
        for mode in 0..4 {
            let (host, mut tools, doc) = setup();
            let action = tools.file_actions(&doc, cursor(&doc)).remove(0);
            match mode {
                0 => host.answer_next_prompt(None),
                1 => host.add_file("src/main/resources/templates/pages/home.html", "existing"),
                2 => host.open_editor(
                    "/project/src/main/resources/templates/pages/home.html",
                    "html",
                    "unsaved",
                    &[],
                ),
                _ => host.add_file("src/main/resources/templates/pages", "file, not directory"),
            }
            assert!(
                tools.resolve_file_action(&doc, action).is_err(),
                "mode {mode}"
            );
        }
    }
    #[test]
    fn cold_java_is_quiet_but_current_pom_can_prompt_known_module_path() {
        let host = MockHost::install();
        host.add_file("pom.xml", POM);
        let mut tools = QuarkusTools::default();
        let java = doc(JAVA);
        assert!(tools.file_actions(&java, cursor(&java)).is_empty());
        let pom = Document {
            path: "/project/pom.xml".into(),
            language: "xml".into(),
            version: 3,
            text: POM.into(),
        };
        let action = tools.file_actions(&pom, Range::default()).remove(0);
        host.answer_next_prompt(Some(&["hello.txt"]));
        host.answer_next_prompt(Some(&["yes"]));
        let resolved = tools.resolve_file_action(&pom, action).unwrap();
        assert_eq!(
            resolved.open.as_deref(),
            Some("/project/src/main/resources/templates/hello.txt")
        );
    }
    #[test]
    fn malformed_namespace_bindings_cannot_offer_or_resolve_qute_creation() {
        for binding in [
            "xmlns:xml=\"urn:foreign\"",
            "xmlns:xmlns=\"urn:foreign\"",
            "xmlns:a=\"http://www.w3.org/XML/1998/namespace\"",
            "bad:flag=\"x\"",
        ] {
            let host = MockHost::install();
            let malformed = POM.replacen("<project>", &format!("<project {binding}>"), 1);
            host.add_file("pom.xml", &malformed);
            let mut tools = QuarkusTools::default();
            let pom = Document {
                path: "/project/pom.xml".into(),
                language: "xml".into(),
                version: 1,
                text: malformed,
            };
            assert!(
                tools.file_actions(&pom, Range::default()).is_empty(),
                "{binding}"
            );
            let clean = Document {
                text: POM.into(),
                ..pom.clone()
            };
            let action = tools.file_actions(&clean, Range::default()).remove(0);
            assert!(
                tools.resolve_file_action(&clean, action).is_err(),
                "{binding}"
            );
        }
        let legal = POM.replacen(
            "<project>",
            "<project xmlns:xml=\"http://www.w3.org/XML/1998/namespace\" xml:lang=\"en\">",
            1,
        );
        assert!(qute_pom(&legal));
    }
    fn navigation_setup() -> (MockHost, QuarkusTools, Document) {
        let host = MockHost::install();
        host.add_file("pom.xml", POM);
        host.add_file("src/main/java/View.java", JAVA);
        host.add_file(
            "src/main/resources/templates/pages/home.html",
            "<p>owned</p>",
        );
        let mut tools = QuarkusTools::default();
        tools.activate_live();
        tools.warm_live(
            "/project/src/main/java/View.java",
            None,
            Instant::now() + Duration::from_secs(1),
        );
        (host, tools, doc(JAVA))
    }
    fn literal_at(doc: &Document) -> Position {
        position(&doc.text, doc.text.find("pages/home.html").unwrap() + 3)
    }
    #[test]
    fn public_definition_uses_exact_cached_qute_literal_and_keeps_no_provider_discovery() {
        let (host, mut tools, doc) = navigation_setup();
        let at = literal_at(&doc);
        let found = tools.definition(&doc, at);
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].path,
            "/project/src/main/resources/templates/pages/home.html"
        );
        assert!(
            tools
                .definition(&doc, position(&doc.text, doc.text.rfind("home;").unwrap()))
                .is_empty()
        );
        // File created only after warm-up cannot enter this callback's inventory.
        host.add_file("src/main/resources/templates/pages/new.html", "new");
        let changed = Document {
            text: doc.text.replace("home.html", "new.html"),
            version: 8,
            ..doc.clone()
        };
        assert!(
            tools
                .definition(
                    &changed,
                    position(
                        &changed.text,
                        changed.text.find("pages/new.html").unwrap() + 3
                    )
                )
                .is_empty()
        );
        assert!(host.requests().is_empty() && host.process_runs().is_empty());
    }
    #[test]
    fn qute_link_missing_cold_expired_foreign_and_unsupported_sources_are_quiet() {
        let (host, mut tools, doc) = navigation_setup();
        assert!(
            QuarkusTools::default()
                .definition(&doc, literal_at(&doc))
                .is_empty()
        );
        for text in [
            JAVA.replace("io.quarkus.qute.Template", "other.Template"),
            JAVA.replace("pages/home.html", "../home.html"),
            JAVA.replace("\"pages/home.html\"", "TEMPLATE"),
            JAVA.replace(" Template home;", " static Template home;"),
        ] {
            let current = self::doc(&text);
            assert!(
                tools
                    .definition(
                        &current,
                        position(&text, text.find("@Location").unwrap() + 12)
                    )
                    .is_empty()
            );
        }
        for inventory in tools.qute.inventories.values_mut() {
            inventory.at = Instant::now() - NAV_TTL;
        }
        assert!(tools.definition(&doc, literal_at(&doc)).is_empty());
        host.set_project_root(Some("/other"));
        let other = Document {
            path: "/other/src/main/java/View.java".into(),
            ..doc.clone()
        };
        assert!(tools.definition(&other, literal_at(&other)).is_empty());
    }
    #[test]
    fn delivered_deletion_dominates_readable_old_inventory_and_creation_can_rewarm() {
        let (host, mut tools, doc) = navigation_setup();
        let path = "/project/src/main/resources/templates/pages/home.html";
        assert_eq!(tools.definition(&doc, literal_at(&doc)).len(), 1);
        tools.handle_events(
            &[openidea_plugin::Event::FilesChanged(
                openidea_plugin::FilesChanged {
                    created: vec![],
                    modified: vec![],
                    removed: vec![path.into()],
                },
            )],
            &host.invocation(),
        );
        // Deliberately retained MockHost source/disk must not resurrect deletion.
        tools.warm_qute(&doc.path);
        assert!(tools.definition(&doc, literal_at(&doc)).is_empty());
        tools.handle_events(
            &[openidea_plugin::Event::FilesChanged(
                openidea_plugin::FilesChanged {
                    created: vec![path.into()],
                    modified: vec![],
                    removed: vec![],
                },
            )],
            &host.invocation(),
        );
        assert_eq!(tools.definition(&doc, literal_at(&doc)).len(), 1);
    }
    #[test]
    fn qute_inventory_cap_and_denied_watch_never_enable_uncertain_targets() {
        let (host, mut tools, doc) = navigation_setup();
        for n in 0..MAX_TEMPLATES {
            host.add_file(&format!("src/main/resources/templates/many/{n}.html"), "x");
        }
        tools.qute.inventories.clear();
        tools.warm_qute(&doc.path);
        assert!(tools.definition(&doc, literal_at(&doc)).is_empty());
        tools.live.watching = false;
        assert!(tools.definition(&doc, literal_at(&doc)).is_empty());
        assert!(host.requests().is_empty() && host.process_runs().is_empty());
    }
    #[test]
    fn same_template_spelling_is_scoped_to_exact_module_and_nested_project_owner() {
        let host = MockHost::install();
        host.add_file("pom.xml", POM);
        host.add_file("src/main/java/View.java", JAVA);
        host.add_file("nested/pom.xml", POM);
        host.add_file("nested/src/main/java/View.java", JAVA);
        host.add_file(
            "nested/src/main/resources/templates/pages/home.html",
            "nested only",
        );
        let mut tools = QuarkusTools::default();
        tools.activate_live();
        tools.warm_live(
            "/project/src/main/java/View.java",
            None,
            Instant::now() + Duration::from_secs(1),
        );
        tools.warm_live(
            "/project/nested/src/main/java/View.java",
            None,
            Instant::now() + Duration::from_secs(1),
        );
        let root = self::doc(JAVA);
        let nested = Document {
            path: "/project/nested/src/main/java/View.java".into(),
            ..root.clone()
        };
        assert!(tools.definition(&root, literal_at(&root)).is_empty());
        assert_eq!(
            tools.definition(&nested, literal_at(&nested))[0].path,
            "/project/nested/src/main/resources/templates/pages/home.html"
        );
        host.add_project("/project/nested");
        assert!(tools.definition(&nested, literal_at(&nested)).is_empty());
    }
    #[test]
    fn include_navigation_uses_cached_exact_ids_and_unique_variants() {
        let (host, mut tools, _) = navigation_setup();
        let text = "😀 {#include pages/home limit=10 /}";
        let doc = Document { path: "/project/src/main/resources/templates/main.html".into(), language: "html".into(), version: 1, text: text.into() };
        host.add_file(&doc.path, text);
        tools.warm_live(&doc.path, Some(text), Instant::now() + Duration::from_secs(1));
        let at = position(text, text.find("pages/home").unwrap()+3);
        let found = tools.definition(&doc, at);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, "/project/src/main/resources/templates/pages/home.html");
        assert!(tools.definition(&doc, position(text, text.find("include").unwrap())).is_empty());
        host.add_file("src/main/resources/templates/pages/home.txt", "ambiguous variant");
        tools.invalidate_qute_navigation(&[openidea_plugin::Event::FileSaved("/project/src/main/resources/templates/pages/home.txt".into())]);
        tools.warm_qute(&doc.path);
        assert!(tools.definition(&doc, at).is_empty());
        let explicit = Document { text: text.replace("pages/home", "pages/home.html"), ..doc.clone() };
        assert_eq!(tools.definition(&explicit, at).len(), 1);
        assert!(host.requests().is_empty() && host.process_runs().is_empty());
    }
    #[test]
    fn include_parser_rejects_dynamic_escaped_commented_and_unsafe_ids() {
        for text in ["{#include _id=foo /}", "{#include foo _id=bar /}", "{#include foo _id = bar /}", "{#include ../foo /}", "{#include /foo /}", "{! {#include foo /} !}", "{|{#include foo /}|}", "\\{#include foo /}", "{#includeX foo /}", "{#include foo"] {
            assert!(include_literals(text).is_empty(), "{text}");
        }
        for text in ["{#include foo /}", "{#include foo}", "{#include foo limit=10 /}"] {
            let found = include_literals(text);
            assert_eq!(found.len(), 1);
            assert_eq!(&text[found[0].0..found[0].1], "foo");
        }
    }

    #[test]
    fn implicit_injected_templates_navigate_only_with_framework_owned_types() {
        let (host, mut tools, _) = navigation_setup();
        host.add_file("src/main/resources/templates/home.html", "owned");
        tools.invalidate_qute_navigation(&[openidea_plugin::Event::FileSaved("/project/src/main/resources/templates/home.html".into())]);
        tools.warm_qute("/project/src/main/java/View.java");
        let text = "import jakarta.inject.Inject; import io.quarkus.qute.Template; class View { @Inject Template home; }";
        let doc = doc(text);
        let at = position(text, text.find("home").unwrap()+1);
        assert_eq!(tools.definition(&doc, at)[0].path, "/project/src/main/resources/templates/home.html");
        for text in [text.replace("jakarta.inject.Inject", "other.Inject"), text.replace("io.quarkus.qute.Template", "other.Template"), text.replace("@Inject Template", "Template"), text.replace("@Inject Template", "@Inject static Template"), text.replace("class View", "class View<Template>")] {
            let at = position(&text, text.find("home").unwrap()+1);
            assert!(tools.definition(&self::doc(&text), at).is_empty(), "{text}");
        }
    }

    #[test]
    fn template_completions_are_scoped_cached_and_ignore_raw_regions() {
        let (host, mut tools, _) = navigation_setup();
        let path = "/project/src/main/resources/templates/main.html";
        host.add_file(path, "{#i}");
        tools.warm_live(path, Some("{#i}"), Instant::now() + Duration::from_secs(1));
        let doc = Document { path: path.into(), language: "html".into(), version: 1, text: "{#i}".into() };
        let items = tools.qute_completion(&doc, Position::new(0, 3));
        assert!(items.iter().any(|i| i.label == "if" && i.snippet));
        assert!(items.iter().all(|i| i.replace == Some(range(&doc.text, 2, 4))));
        let include = Document { text: "{#include pages/".into(), ..doc.clone() };
        let items = tools.qute_completion(&include, Position::new(0, include.text.len() as u32));
        assert_eq!(items.iter().map(|i|i.label.as_str()).collect::<Vec<_>>(), ["pages/home.html"]);
        for text in ["{! {#i", "{| {#i", "\\{#i"] {
            let d = Document { text: text.into(), ..doc.clone() };
            assert!(tools.qute_completion(&d, Position::new(0, text.len() as u32)).is_empty());
        }
        let foreign = Document { path: "/project/other.html".into(), ..doc.clone() };
        assert!(tools.qute_completion(&foreign, Position::new(0, 3)).is_empty());
        assert!(QuarkusTools::default().qute_completion(&doc, Position::new(0, 3)).is_empty());
        assert!(host.requests().is_empty() && host.process_runs().is_empty());
    }

    #[test]
    fn absent_include_notes_use_current_text_and_complete_fresh_inventory() {
        let (host, mut tools, _) = navigation_setup();
        let path = "/project/src/main/resources/templates/main.html";
        let text = "{#include pages/missing /} {#include pages/home /} {#include _id=runtime /}";
        host.add_file(path, text);
        tools.warm_live(path, Some(text), Instant::now() + Duration::from_secs(1));
        let doc = Document { path: path.into(), language: "html".into(), version: 1, text: text.into() };
        let found = tools.qute_diagnostics(&doc);
        assert_eq!(found.len(), 1);
        assert!(found[0].message.contains("custom template locator"));
        let fixed = Document { text: "{#include pages/home /}".into(), version: 2, ..doc.clone() };
        assert!(tools.qute_diagnostics(&fixed).is_empty());
        for inventory in tools.qute.inventories.values_mut() { inventory.at = Instant::now() - NAV_TTL; }
        assert!(tools.qute_diagnostics(&doc).is_empty());
    }

    #[test]
    fn extensionless_location_navigation_does_not_enable_extensionless_creation() {
        let (_host, mut tools, doc) = navigation_setup();
        let doc = Document { text: doc.text.replace("pages/home.html", "pages/home"), ..doc };
        let at = position(&doc.text, doc.text.find("pages/home").unwrap()+3);
        assert_eq!(tools.definition(&doc, at)[0].path, "/project/src/main/resources/templates/pages/home.html");
        assert!(tools.qute_file_actions(&doc, Range::new(at, at)).is_empty());
    }

    #[test]
    fn checked_template_methods_use_default_enclosing_and_literal_base_paths() {
        let imports = "import io.quarkus.qute.CheckedTemplate; import io.quarkus.qute.TemplateInstance; ";
        for (body, expected) in [("@CheckedTemplate class Templates { static native TemplateInstance home(); }", "home"), ("class Resource { @CheckedTemplate static class Templates { static native TemplateInstance home(); } }", "Resource/home"), ("@CheckedTemplate(basePath=\"pages\") class Templates { static native TemplateInstance home(); }", "pages/home")] {
            let text = format!("{imports}{body}"); let doc = self::doc(&text);
            assert_eq!(checked_template(&doc, position(&text, text.find("home()").unwrap())), Some(expected.into()));
        }
        for body in ["@CheckedTemplate class Templates<TemplateInstance> { static native TemplateInstance home(); }", "@CheckedTemplate(defaultName=OTHER) class Templates { static native TemplateInstance home(); }", "@CheckedTemplate class Templates { static TemplateInstance home() { return null; } }", "@CheckedTemplate class Templates { static native TemplateInstance home(); static native TemplateInstance home(String name); }"] {
            let text = format!("{imports}{body}");
            assert!(checked_template(&self::doc(&text), position(&text, text.find("home(").unwrap())).is_none());
        }
    }

    #[test]
    fn checked_template_custom_names_and_record_ids_follow_owned_source_contracts() {
        let imports = "import io.quarkus.qute.CheckedTemplate; import io.quarkus.qute.TemplateInstance; ";
        for (body, name, expected) in [
            ("@CheckedTemplate(defaultName=CheckedTemplate.HYPHENATED_ELEMENT_NAME) class Templates { static native TemplateInstance itemAndOrder(); }", "itemAndOrder", "item-and-order"),
            ("class Resource { @CheckedTemplate(defaultName=CheckedTemplate.UNDERSCORED_ELEMENT_NAME) static class Templates { static native TemplateInstance itemAndOrder(); } }", "itemAndOrder", "Resource/item_and_order"),
            ("record Greeting(String name) implements TemplateInstance {}", "Greeting", "Greeting"),
            ("class Resource { record Greeting(String name) implements TemplateInstance {} }", "Greeting", "Resource/Greeting"),
            ("@CheckedTemplate(basePath=\"pages\", defaultName=CheckedTemplate.HYPHENATED_ELEMENT_NAME) record HelloUser(String name) implements TemplateInstance {}", "HelloUser", "pages/hello-user"),
        ] {
            let text = format!("{imports}{body}");
            assert_eq!(checked_template(&self::doc(&text), position(&text, text.rfind(name).unwrap())), Some(expected.into()), "{body}");
        }
        for body in ["record Greeting(String name) implements other.TemplateInstance {}", "@CheckedTemplate(defaultName=Other.HYPHENATED_ELEMENT_NAME) class Templates { static native TemplateInstance Greeting(); }", "@CheckedTemplate(defaultName=HYPHENATED_ELEMENT_NAME) class Templates { static String HYPHENATED_ELEMENT_NAME; static native TemplateInstance Greeting(); }"] {
            let text = format!("{imports}{body}");
            assert!(checked_template(&self::doc(&text), position(&text, text.find("Greeting").unwrap())).is_none());
        }
    }

    #[test]
    fn qute_tag_ranges_ignore_quoted_closing_braces_and_dynamic_id_after_them() {
        let text = "{#include pages/home value='}' _id=runtime /}";
        assert!(include_literals(text).is_empty());
        assert_eq!(include_literals("{#include pages/home value='}' /}")[0].2, "pages/home");
        assert!(include_literals("{#include pages/home value='unterminated }").is_empty());
    }
    #[test]
    fn qute_folding_respects_nested_sections_comments_raw_blocks_and_unknown_owners() {
        let (host, mut tools, _) = navigation_setup();
        let text = "{#if active}\n{#for item in items}\n{item}\n{/for}\n{#else}\n{! comment\n{#ignored}\n!}\n{| literal\n{#ignored}\n|}\n{/if}";
        let path = "/project/src/main/resources/templates/main.qute.html";
        host.add_file(path, text);tools.warm_live(path,Some(text),Instant::now()+Duration::from_secs(1));
        let doc=Document{path:path.into(),language:"qute-html".into(),version:1,text:text.into()};
        let folds=tools.qute_folding(&doc);
        assert_eq!(folds.len(),4);assert!(folds.iter().any(|f|f.start_line==0&&f.end_line==11));
        assert!(folds.iter().any(|f|f.kind=="comment"&&f.start_line==5&&f.end_line==7));
        assert!(QuarkusTools::default().qute_folding(&doc).is_empty());
        assert!(tools.qute_folding(&Document{path:"/project/plain.html".into(),..doc.clone()}).is_empty());
        assert!(tools.qute_folding(&Document{text:"{#if active}\nbody\n{/for}".into(),..doc}).is_empty());
    }

    fn embedded_setup(props: &str, java: &str) -> (MockHost, QuarkusTools, Document) {
        let (host, mut tools, _) = navigation_setup();
        host.add_file("src/main/java/Templates.java", java);
        if !props.is_empty() { host.add_file("src/main/resources/application.properties", props); }
        tools.invalidate_qute_navigation(&[openidea_plugin::Event::FileSaved("/project/src/main/resources/templates/pages/home.html".into())]);
        tools.warm_qute("/project/src/main/java/View.java");
        (host, tools, doc(java))
    }
    const CHECKED: &str = "import io.quarkus.qute.CheckedTemplate; import io.quarkus.qute.TemplateInstance; class Resource { @CheckedTemplate(basePath=\"pages\", defaultName=CheckedTemplate.HYPHENATED_ELEMENT_NAME) static class T { static native TemplateInstance homePage(String user, int count); static native TemplateInstance home(String user); } }";
    #[test]
    fn missing_embedded_templates_warn_only_when_inventory_proves_absence() {
        let (_h, tools, d) = embedded_setup("", CHECKED);
        let found = tools.qute_java_diagnostics(&d);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].message.contains("pages/home-page"));
        let loc = "import io.quarkus.qute.Template; import io.quarkus.qute.Location; class V { @Location(\"pages/home.html\") Template a; @Location(\"nope.html\") Template b; @Location(dynamic) Template c; }";
        let (_h, tools, d) = embedded_setup("", loc);
        let found = tools.qute_java_diagnostics(&d);
        assert_eq!(found.len(), 1);
        assert!(found[0].message.contains("nope.html"));
        assert!(QuarkusTools::default().qute_java_diagnostics(&d).is_empty());
        let (_h, tools, d) = embedded_setup("quarkus.qute.template-path-exclude=x\n", loc);
        assert!(tools.qute_java_diagnostics(&d).is_empty());
    }
    #[test]
    fn literal_suffix_property_extends_resolution_and_dynamic_values_are_quiet() {
        let java = "import io.quarkus.qute.Template; import io.quarkus.qute.Location; class V { @Location(\"pages/home\") Template a; }";
        let (host, mut tools, d) = embedded_setup("", java);
        assert!(tools.qute_java_diagnostics(&d).is_empty());
        host.add_file("src/main/resources/templates/pages/other.qute.html", "x");
        let java2 = java.replace("pages/home", "pages/other");
        host.add_file("src/main/java/View.java", &java2);
        tools.invalidate_qute_navigation(&[openidea_plugin::Event::FileSaved("/project/src/main/resources/templates/pages/other.qute.html".into())]);
        tools.warm_qute("/project/src/main/java/View.java");
        assert!(tools.qute_java_diagnostics(&doc(&java2)).is_empty());
        host.add_file("src/main/resources/application.properties", "quarkus.qute.suffixes=txt\n");
        let d2 = tools.qute_java_diagnostics(&doc(&java2));
        assert_eq!(d2.len(), 1, "{d2:?}");
        for bad in ["quarkus.qute.suffixes=${S}\n", "%dev.quarkus.qute.suffixes=txt\n", "quarkus.qute.suffixes=xml\n"] {
            host.add_file("src/main/resources/application.properties", bad);
            assert!(tools.qute_java_diagnostics(&doc(&java2)).is_empty(), "{bad}");
        }
    }
    #[test]
    fn template_expression_completion_uses_literal_checked_params_and_data_chains() {
        let java = "import io.quarkus.qute.CheckedTemplate; import io.quarkus.qute.TemplateInstance; import io.quarkus.qute.TemplateData; @TemplateData(namespace=\"app\") class D {} class Resource { @CheckedTemplate(basePath=\"pages\") static class T { static native TemplateInstance home(String user, int count); } void f() { T.home(\"a\", 1).data(\"extra\", 2).data(dyn, 3); } }";
        let (host, mut tools, _) = embedded_setup("", java);
        let text = "<p>{";
        let path = "/project/src/main/resources/templates/pages/home.html";
        host.add_file("src/main/resources/templates/pages/home.html", text);
        tools.warm_live(path, Some(text), Instant::now() + Duration::from_secs(1));
        let d = Document { path: path.into(), language: "qute-html".into(), version: 1, text: text.into() };
        let labels: Vec<_> = tools.qute_completion(&d, position(text, text.len())).into_iter().map(|i| i.label).collect();
        assert_eq!(labels, ["count", "extra", "user", "app:"]);
        let other = Document { path: "/project/src/main/resources/templates/pages/other.html".into(), ..d };
        assert_eq!(tools.qute_completion(&other, position(text, text.len())).len(), 1);
    }

}
