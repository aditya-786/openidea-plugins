//! Spring Web Services Tools for OpenIDEA: links `@PayloadRoot` endpoint
//! methods to the top-level elements of the project's XML schemas (`.xsd`
//! files and schemas embedded in `.wsdl`) and checks the classpath resources
//! given to `ClassPathResource`. Text-based, Java and Kotlin.

mod generate;
mod indexing;
mod resource_completion;
mod namespace_values;
mod xpath;
mod xpath_syntax;
mod resource_paths;
mod annotation_owners;
mod schema_qnames;
mod xml_refs;
mod wsdl_pairs;
mod wsdl_definitions;
mod wsdl_qnames;
mod xml_contracts;
mod typed_xml;
mod xml_resources;

use openidea_plugin::diagnostics::DiagnosticsProvider;
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use openidea_plugin::providers::{CompletionItem, CompletionKind, Document, LineMarker, Providers};
use openidea_plugin::{
    Context, Diagnostic, Error, Event, EventKind, Invocation, Location, Plugin, Position, Range, Severity, host, register_plugin,
};
use spring_text::index::{COMMAND_BUDGET, DIAGNOSTICS_WARM, Index, Indexes, PROVIDER_BUDGET, project_of};
use spring_text::java::{self, Arg, JavaFile, Str};
use spring_text::{contains, parse_source, range, source_calls, xml};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    /// `None` when the namespace isn't a literal or a constant of the file.
    pub namespace: Option<String>,
    pub local: String,
    /// The `localPart` literal.
    pub local_range: Range,
    /// `Class.method`.
    pub label: String,
    /// The method name.
    pub target: Range,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Schema {
    pub namespace: String,
    /// Top-level `element` names with the range of their `name` value.
    pub elements: Vec<(String, Range)>,
}

/// Literal namespace or uniquely owned source compile-time constant. Unknown
/// expressions/owners are not a known namespace merely because a field shares a name.
fn constant(f: &JavaFile, text: &str, arg: &Arg, owner: usize, kotlin: bool) -> Option<String> {
    let literal = |code: &str| {
        let toks = java::tokens(code);
        let [token] = toks.as_slice() else {
            return None;
        };
        let value = token.string()?;
        let raw = code.get(value.at..value.end)?;
        (raw == value.value
            && !raw.contains(['\\', '$'])
            && raw.len() <= 1024
            && !raw.chars().any(|c| c.is_control() || c.is_whitespace())
            && !raw.contains("${")
            && !raw.contains("#{"))
        .then_some(value.value)
    };
    if let Some(value) = literal(&arg.text) {
        return Some(value);
    }
    let tokens = java::tokens(&arg.text);
    let (owners, name): (Vec<usize>, &str) = match tokens.as_slice() {
        [name] if name.is_ident() => {
            let mut owners = vec![owner];
            if kotlin {
                owners.extend(
                    f.types
                        .iter()
                        .enumerate()
                        .filter(|(_, t)| {
                            t.outer == Some(owner)
                                && t.kind == "object"
                                && t.modifiers.iter().any(|m| m == "companion")
                        })
                        .map(|(i, _)| i),
                );
            }
            (owners, name.s)
        }
        [ty, dot, name] if ty.is_ident() && dot.s == "." && name.is_ident() => {
            let owners: Vec<_> = f
                .types
                .iter()
                .enumerate()
                .filter(|(_, t)| t.name == ty.s && t.outer.is_none())
                .map(|(i, _)| i)
                .collect();
            if owners.len() != 1
                // A nested declaration can hide the top-level written owner.
                // Without a lexical type resolver, withhold this namespace.
                || f.types.iter().filter(|t| t.name == ty.s).count() != 1
                || annotation_owners::constant_owner_shadowed(f, text, ty.s, kotlin)
                || f.imports
                    .iter()
                    .any(|i| i.path.rsplit('.').next() == Some(ty.s))
            {
                return None;
            }
            (owners, name.s)
        }
        _ => return None,
    };
    let found: Vec<_> = owners
        .iter()
        .flat_map(|index| f.types[*index].fields().map(move |field| (*index, field)))
        .filter(|(_, field)| field.name == name)
        .collect();
    let [(index, field)] = found.as_slice() else {
        return None;
    };
    if kotlin {
        if f.types[*index].kind != "object" || !field.modifiers.iter().any(|m| m == "const") {
            return None;
        }
    } else {
        if !field.modifiers.iter().any(|m| m == "static")
            || !field.modifiers.iter().any(|m| m == "final")
            || field.ty.as_ref().is_none_or(|t| {
                t.name != "String" || !matches!(t.text.as_str(), "String" | "java.lang.String")
                    || (t.text == "String" && annotation_owners::constant_owner_shadowed(f, text, "String", false))
                    || (t.text == "java.lang.String" && (
                        annotation_owners::constant_owner_shadowed(f, text, "java", false)
                        || f.types.iter().any(|ty| ty.name == "java")
                        || f.imports.iter().any(|i| i.path.rsplit('.').next() == Some("java"))))
            })
            || f.types.iter().any(|t| t.name == "String")
            || f.imports
                .iter()
                .any(|i| i.path.ends_with(".String") && i.path != "java.lang.String")
        {
            return None;
        }
    }
    let (from, to) = field.body?;
    literal(text.get(from..to)?)
}

// Container members are parsed at their original byte offsets. Never collect
// arbitrary nested strings: each entry must be a complete PayloadRoot annotation.
fn payload_annotations(annotation: &java::Annotation, text: &str, kotlin: bool) -> Vec<java::Annotation> {
    if annotation.name == "PayloadRoot" {
        return if balanced_annotation(&text[annotation.at..annotation.end]) { vec![annotation.clone()] } else { Vec::new() };
    }
    if annotation.name != "PayloadRoots" || annotation.args.is_empty() || annotation.args.iter().any(|arg| arg.name != "value") {
        return Vec::new();
    }
    if annotation.args.len() > 1
        && (!kotlin
            || annotation.args.iter().any(|arg| java::tokens(&text[annotation.at..arg.at]).last().is_some_and(|token| token.s == "=")))
    {
        return Vec::new();
    }
    let source = &text[annotation.at..annotation.end];
    if !balanced_annotation(source) {
        return Vec::new();
    }
    let mut found = Vec::new();
    for arg in &annotation.args {
        let mut tokens = java::tokens(&arg.text);
        for token in &mut tokens {
            token.at += arg.at;
        }
        let mut entries = tokens.as_slice();
        if entries.first().is_some_and(|token| matches!(token.s, "{" | "[")) {
            if entries.len() < 2 {
                return Vec::new();
            }
            entries = &entries[1..entries.len() - 1];
        }
        for entry in java::split_commas(entries, false) {
            if entry.is_empty() {
                continue;
            }
            let mut entry = entry.to_vec();
            if entry[0].s != "@" {
                if !kotlin {
                    return Vec::new();
                }
                entry.insert(0, java::Tok { s: "@", at: entry[0].at });
            }
            let mut parsed = Vec::new();
            let end = java::annotation(text, &entry, 0, &mut parsed);
            if end != entry.len()
                || parsed.len() != 1
                || parsed[0].name != "PayloadRoot"
                || !balanced_annotation(&text[parsed[0].name_at..parsed[0].end])
            {
                return Vec::new();
            }
            found.extend(parsed);
        }
    }
    found
}

fn balanced_annotation(source: &str) -> bool {
    let tokens = java::tokens(source);
    if tokens.last().is_none_or(|token| token.s != ")") {
        return false;
    }
    let mut stack = Vec::new();
    for token in tokens {
        match token.s {
            "(" => stack.push(")"),
            "{" => stack.push("}"),
            "[" => stack.push("]"),
            ")" | "}" | "]" => {
                if stack.pop() != Some(token.s) {
                    return false;
                }
            }
            _ => {}
        }
    }
    stack.is_empty()
}

pub fn endpoints(f: &JavaFile, text: &str) -> Vec<Endpoint> {
    endpoints_with_syntax(f, text, false)
}

fn endpoints_with_syntax(f: &JavaFile, text: &str, kotlin: bool) -> Vec<Endpoint> {
    let mut out = Vec::new();
    let ownership = annotation_owners::Owners::new(f, text, kotlin);
    for (owner, t) in f
        .types
        .iter()
        .enumerate()
        .filter(|(_, t)| ownership.endpoint(text, t))
    {
        for m in t.methods() {
            for a in m
                .annotations
                .iter()
                .filter(|a| ownership.payload(text, a))
                .flat_map(|a| payload_annotations(a, text, kotlin))
                .filter(|a| ownership.payload(text, a))
            {
                if a.args.iter().filter(|arg| arg.name == "namespace").count() > 1 {
                    continue;
                }
                let Some(arg) = a.arg("localPart") else {
                    continue;
                };
                let tokens = java::tokens(&arg.text);
                if tokens.len() != 1
                    || !tokens[0].s.ends_with('"')
                    || a.args.iter().filter(|arg| arg.name == "localPart").count() != 1
                {
                    continue;
                }
                let Some(local) = arg.strings.first() else {
                    continue;
                };
                if text.get(local.at..local.end) != Some(local.value.as_str())
                    || !schema_qnames::local(&local.value)
                {
                    continue;
                }
                out.push(Endpoint {
                    namespace: a.arg("namespace").map_or_else(
                        || Some(String::new()),
                        |arg| constant(f, text, arg, owner, kotlin),
                    ),
                    local: local.value.clone(),
                    local_range: range(text, local.at, local.end),
                    label: format!("{}.{}", t.name, m.name),
                    target: range(text, m.at, m.at + m.name.len()),
                });
            }
        }
    }
    out
}

pub fn schemas(text: &str) -> Vec<Schema> {
    schema_qnames::read(text).0
}

fn scan_java(path: &str, text: &str) -> Option<Vec<Endpoint>> {
    if !(path.ends_with(".java") || path.ends_with(".kt")) {
        return None;
    }
    let found = endpoints_with_syntax(&parse_source(path, text), text, path.ends_with(".kt"));
    (!found.is_empty()).then_some(found)
}

/// Every schema file is kept, even without a schema: it tells resources exist.
const MAX_DOCUMENT_BYTES: usize = 256 * 1024;
const MAX_SCHEMA_BYTES: usize = 128 * 1024;
const SOURCE_FACT_GLOBS: &[&str] = &["**/*.java", "**/*.kt", "**/*.xml"];
const SCHEMA_GLOBS: &[&str] = &["**/*.xsd", "**/*.wsdl"];
const WATCH_GLOBS: &[&str] = &["**/*.java", "**/*.kt", "**/*.xsd", "**/*.wsdl", "**/*.xslt", "**/*.xml"];

#[derive(Clone)]
struct SchemaFile {
    schemas: Vec<Schema>,
    complete: bool,
    qnames_complete: bool,
}

fn scan_schemas(_path: &str, text: &str) -> Option<SchemaFile> {
    let complete = text.len() <= MAX_SCHEMA_BYTES;
    let (schemas, qnames_complete) = if complete { schema_qnames::read(text) } else { (Vec::new(), false) };
    Some(SchemaFile { schemas, complete, qnames_complete })
}

fn complete_schemas(index: &Index<SchemaFile>) -> bool {
    index.complete() && index.iter().all(|(_, file)| file.complete)
}

/// The `ClassPathResource` paths of a file, `classpath:` and leading `/` removed.
fn resources(path: &str, text: &str) -> Vec<(String, Str)> {
    let file=parse_source(path,text);
    let ownership=annotation_owners::Owners::new(&file,text,path.ends_with(".kt"));
    let tokens=java::tokens(text);
    source_calls(path, text)
        .into_iter()
        .filter(|c| c.is_new && c.name == "ClassPathResource" && c.args.len()==1)
        .filter(|c| {
            let Some(mut first)=tokens.iter().position(|t| t.at==c.at) else {return false;};
            let last=first;
            while first>=2 && tokens[first-1].s=="." && tokens[first-2].is_ident(){first-=2;}
            let written:String=tokens[first..=last].iter().map(|t|t.s).collect();
            ownership.resource_constructor(&written)
        })
        .filter_map(|c| c.args.into_iter().next()?.string)
        .filter(|s| text.get(s.at..s.end)==Some(s.value.as_str()) && !s.value.contains(['\\','$']))
        .filter_map(|s| {
            let value=s.value.strip_prefix("classpath:").unwrap_or(&s.value);
            let value=value.strip_prefix('/').unwrap_or(value);
            let literal=format!("classpath:{value}");
            let extension=value.rsplit_once('.')?.1;
            xml_resources::literal_path(&literal,extension).map(|p|(p.to_owned(),s))
        })
        .collect()
}

const SWS_NAMESPACE: &str = "http://www.springframework.org/schema/web-services";

fn xml_namespace<'a>(elements: &'a [xml::Element], mut index: usize) -> Option<&'a str> {
    let prefix = elements[index].name.split_once(':').map(|(prefix, _)| prefix);
    let declaration = prefix.map_or_else(|| "xmlns".to_owned(), |prefix| format!("xmlns:{prefix}"));
    loop {
        let mut attrs = elements[index].attrs.iter().filter(|attr| attr.name == declaration);
        if let Some(attr) = attrs.next() {
            return attrs.next().is_none().then_some(attr.value.as_str());
        }
        index = elements[index].parent?;
    }
}

/// Only explicit classpath resources have a project-local meaning here.
/// Relative servlet-context paths, URLs, patterns and placeholders stay unknown.
fn sws_resources(text: &str) -> Vec<(String, Range)> {
    if text.len() > MAX_DOCUMENT_BYTES || !generate::xml_valid(text) {
        return Vec::new();
    }
    let elements = xml::elements(text);
    elements
        .iter()
        .enumerate()
        .filter_map(|(index, element)| {
            if xml_namespace(&elements, index) != Some(SWS_NAMESPACE) || !text[..element.tag_end].ends_with('>') {
                return None;
            }
            let extension = match element.local() {
                "static-wsdl" => ".wsdl",
                "xsd"
                    if element.parent.is_some_and(|parent| {
                        elements[parent].local() == "dynamic-wsdl" && xml_namespace(&elements, parent) == Some(SWS_NAMESPACE)
                    }) =>
                {
                    ".xsd"
                }
                _ => return None,
            };
            let mut attrs = element.attrs.iter().filter(|attr| attr.name == "location");
            let attr = attrs.next()?;
            if attrs.next().is_some() {
                return None;
            }
            let quote = text.as_bytes().get(attr.value_at.checked_sub(1)?)?;
            if !matches!(quote, b'\'' | b'"') || text.as_bytes().get(attr.value_end) != Some(quote) {
                return None;
            }
            let path = attr.value.strip_prefix("classpath:")?.trim_start_matches('/');
            if !path.ends_with(extension)
                || path.contains(['$', '#', '&', '*', '?', ':', '\\'])
                || path.split('/').any(|part| part.is_empty() || part == "." || part == "..")
            {
                return None;
            }
            Some((path.to_owned(), range(text, attr.value_at, attr.value_end)))
        })
        .collect()
}

const RESOURCE_DIRS: [&str; 2] = ["src/main/resources", "src/test/resources"];

/// Only an established exact namespace/local pair supplies QName authority.
/// Most markers one line gets: the IDE folds same-line, same-icon markers into one button with a chooser.
const MAX_LINE_TARGETS: usize = 200;

fn elements_for<'a>(schemas: &'a Index<SchemaFile>, namespace: Option<&str>) -> Vec<(&'a str, &'a str, Range)> {
    let Some(namespace) = namespace else { return Vec::new(); };
    schemas.iter().flat_map(|(p, file)| file.schemas.iter()
        .filter(move |schema| schema.namespace == namespace)
        .flat_map(move |schema| schema.elements.iter().map(move |(n, r)| (p, n.as_str(), *r))))
        .collect()
}

fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

#[derive(Clone)]
struct SourceFacts {
    endpoints: Vec<Endpoint>,
    types: Vec<typed_xml::SourceType>,
    legacy_resolver: bool,
}

fn scan_source_facts(path: &str, text: &str) -> Option<SourceFacts> {
    let endpoints = scan_java(path, text).unwrap_or_default();
    let types = typed_xml::scan_source(path, text).unwrap_or_default();
    let legacy_resolver = xpath::scan_resolver_customization(path, text).is_some();
    (!endpoints.is_empty() || !types.is_empty() || legacy_resolver).then_some(SourceFacts { endpoints, types, legacy_resolver })
}

pub struct WsTools {
    source: Indexes<SourceFacts>,
    generation: generate::Generation,
    schemas: Indexes<SchemaFile>,
    xpath_packages: Indexes<xpath::PackageNamespaces>,
    resource_paths: resource_paths::Paths,
    watching: bool,
    applies: std::collections::BTreeMap<String, (Instant, bool)>,
}

impl Default for WsTools {
    fn default() -> Self {
        Self {
            source: Indexes::new(SOURCE_FACT_GLOBS, scan_source_facts).with_limits(MAX_DOCUMENT_BYTES, usize::MAX),
            generation:generate::Generation::default(),
            schemas: Indexes::new(SCHEMA_GLOBS, scan_schemas),
            xpath_packages: Indexes::new(&["**/package-info.java"],xpath::scan_package),
            resource_paths: resource_paths::Paths::default(),
            watching: false,
            applies: std::collections::BTreeMap::new(),
        }
    }
}

impl WsTools {
    fn cached_resource_definition(&mut self, doc: &Document, path: &str) -> Vec<Location> {
        // Shared scalar source/XML navigation never discovers/reads project files.
        // A complete fresh watched inventory is needed to rule out collisions.
        if !self.watching || doc.path.contains('\\') || doc.path.split('/').any(|p| matches!(p, "." | "..")) { return vec![]; }
        let Some(root) = project_of(&doc.path).and_then(|p| p.root().ok()) else { return vec![]; };
        if root.is_empty() || root.chars().any(char::is_control) || !doc.path.starts_with(&format!("{}/",root.trim_end_matches('/'))) { return vec![]; }
        if let Some(project) = project_of(&doc.path) { self.resource_paths.warm(&project, PROVIDER_BUDGET); }
        let Some(paths) = self.resource_paths.paths(&root, path) else { return vec![]; };
        let suffixes = RESOURCE_DIRS.map(|directory|format!("/{directory}/{path}"));
        let candidates: Vec<_> = paths.iter().filter(|file| suffixes.iter().any(|suffix|file.ends_with(suffix))).collect();
        let [target] = candidates.as_slice() else { return vec![]; };
        if !target.starts_with(&format!("{}/", root.trim_end_matches('/'))) || project_of(target).and_then(|p| p.root().ok()).as_deref() != Some(&root) { return vec![]; }
        return vec![Location { path: (*target).clone(), range: Range::default() }];
    }

    fn java_markers(&mut self, doc: &Document) -> Vec<LineMarker> {
        let text = &doc.text;
        let f = parse_source(&doc.path, text);
        let Some(schemas) = self.schemas.for_document(&doc.path, text, PROVIDER_BUDGET) else {
            return Vec::new();
        };
        let mut markers = Vec::new();
        for e in endpoints_with_syntax(&f, text, doc.path.ends_with(".kt")) {
            let found: Vec<_> = elements_for(schemas, e.namespace.as_deref()).into_iter().filter(|(_, n, _)| *n == e.local).collect();
            let shown = if found.len() > MAX_LINE_TARGETS { format!(" (first {MAX_LINE_TARGETS} of {})", found.len()) } else { String::new() };
            markers.extend(found.iter().take(MAX_LINE_TARGETS).map(|(path, _, r)| LineMarker {
                line: e.target.start.line,
                icon: Some(SCHEMA_MARKER.into()),
                tooltip: format!("Payload element <{}> in {}{shown}", e.local, file_name(path)),
                target: Some(Location { path: (*path).into(), range: *r }),
            }));
        }
        for b in java::bean_names(&f).into_iter().filter(|b| b.ty == "DefaultWsdl11Definition") {
            let line = openidea_plugin::text::position(text, b.at).line;
            markers.push(LineMarker { line, icon: Some(WSDL_MARKER.into()), tooltip: format!("Published as /{}.wsdl", b.name), target: None });
        }
        markers
    }

    fn xml_markers(&mut self, doc: &Document) -> Vec<LineMarker> {
        let found = schemas(&doc.text);
        if found.iter().all(|s| s.elements.is_empty()) {
            return Vec::new();
        }
        let Some(java) = self.source.for_document(&doc.path, &doc.text, PROVIDER_BUDGET) else {
            return Vec::new();
        };
        let mut markers = Vec::new();
        for s in &found {
            for (name, r) in &s.elements {
                let handlers: Vec<(&str, &Endpoint)> = java
                    .iter()
                    .flat_map(|(p, facts)| facts.endpoints.iter().map(move |e| (p, e)))
                    .filter(|(_, e)| &e.local == name && e.namespace.as_ref().is_none_or(|ns| ns == &s.namespace))
                    .collect();
                let labels: Vec<&str> = handlers.iter().map(|(_, e)| e.label.as_str()).collect();
                let shown = if handlers.len() > MAX_LINE_TARGETS { format!(" (first {MAX_LINE_TARGETS} of {})", handlers.len()) } else { String::new() };
                let tooltip = format!("Handled by {}{shown}", labels.join(", "));
                markers.extend(handlers.iter().take(MAX_LINE_TARGETS).map(|(path, e)| LineMarker {
                    line: r.start.line,
                    icon: Some(ENDPOINT_MARKER.into()),
                    tooltip: tooltip.clone(),
                    target: Some(Location { path: (*path).into(), range: e.target }),
                }));
            }
        }
        markers
    }
}

impl Providers for WsTools {
    fn line_markers(&mut self, doc: &Document) -> Vec<LineMarker> {
        if !self.spring_ws_document(&doc.path) {
            return Vec::new();
        }
        if matches!(doc.language.as_str(), "java" | "kotlin") { self.java_markers(doc) } else { self.xml_markers(doc) }
    }

    fn definition(&mut self, doc: &Document, at: Position) -> Vec<Location> {
        if !self.spring_ws_document(&doc.path) {
            return Vec::new();
        }
        if let Some(locations)=namespace_values::definition(self,doc,at){return locations;}
        if let Some(locations)=xpath::definition(self,doc,at){return locations;}
        if doc.language == "xml" {
            if let Some(locations)=typed_xml::definition(self,doc,at){return locations;}
            if let Some((path, _)) = xml_resources::references(doc).into_iter().find(|(_, r)| r.start <= at && at < r.end) {
                return self.cached_resource_definition(doc, &path);
            }
            let wsdl = wsdl_qnames::definition(doc, at);
            if !wsdl.is_empty() { return wsdl; }
            let local = xml_refs::definition(doc, at);
            if !local.is_empty() { return local; }
            let Some((path, _)) = sws_resources(&doc.text).into_iter().find(|(_, location)| contains(*location, at)) else {
                return Vec::new();
            };
            let Some(index) = self.schemas.for_document(&doc.path, &doc.text, PROVIDER_BUDGET) else {
                return Vec::new();
            };
            return index
                .files()
                .keys()
                .filter(|file| RESOURCE_DIRS.iter().any(|dir| file.ends_with(&format!("{dir}/{path}"))))
                .map(|file| Location { path: file.clone(), range: Range::default() })
                .collect();
        }
        let text = &doc.text;
        let endpoint = endpoints_with_syntax(&parse_source(&doc.path, text), text, doc.path.ends_with(".kt"))
            .into_iter()
            .find(|e| contains(e.local_range, at));
        if let Some(e) = endpoint {
            let Some(schemas) = self.schemas.for_document(&doc.path, text, PROVIDER_BUDGET) else {
                return Vec::new();
            };
            return elements_for(schemas, e.namespace.as_deref())
                .into_iter()
                .filter(|(_, n, _)| *n == e.local)
                .map(|(p, _, r)| Location { path: p.into(), range: r })
                .collect();
        }
        let Some((path, _)) = resources(&doc.path, text).into_iter().find(|(_, s)| contains(range(text, s.at, s.end), at)) else {
            return Vec::new();
        };
        self.cached_resource_definition(doc, &path)
    }

    fn completion(&mut self, doc: &Document, at: Position, _trigger: Option<&str>) -> Vec<CompletionItem> {
        if !self.spring_ws_document(&doc.path) {
            return Vec::new();
        }
        if let Some(items)=namespace_values::completion(self,doc,at){return items;}
        if let Some(items)=xpath::completion(self,doc,at){return items;}
        if doc.language == "xml" {
            if let Some(items)=typed_xml::completion(self,doc,at){return items;}
            if let Some(items)=xml_contracts::completion(doc,at){return items;}
            if let Some(items)=resource_completion::completion(self,doc,at){return items;}
            return xml_refs::completion(doc, at);
        }
        let text = &doc.text;
        let Some(e) = endpoints_with_syntax(&parse_source(&doc.path, text), text, doc.path.ends_with(".kt"))
            .into_iter()
            .find(|e| contains(e.local_range, at))
        else {
            return Vec::new();
        };
        let Some(schemas) = self.schemas.for_document(&doc.path, text, PROVIDER_BUDGET) else {
            return Vec::new();
        };
        let mut out: Vec<CompletionItem> = elements_for(schemas, e.namespace.as_deref())
            .into_iter()
            .map(|(p, n, _)| CompletionItem {
                detail: Some(file_name(p).into()),
                replace: Some(e.local_range),
                ..CompletionItem::new(n, CompletionKind::Struct)
            })
            .collect();
        out.sort_by(|a, b| a.label.cmp(&b.label));
        out.dedup_by(|a, b| a.label == b.label);
        out
    }
}

impl DiagnosticsProvider for WsTools {
    fn diagnostics(&mut self, doc: &Document) -> Vec<Diagnostic> {
        let mut found = self.all_diagnostics(doc);
        found.retain(|d| setting_of(d.code.as_deref().unwrap_or_default()).is_none_or(|key| host::setting_bool(key).unwrap_or(true)));
        found
    }
}

fn setting_of(code: &str) -> Option<&'static str> {
    match code {
        "unknown-payload-element" => Some("checkUnknownElements"),
        "missing-resource" => Some("checkMissingResources"),
        "wsdl-unpaired-request" => Some("checkRequestResponsePairs"),
        "unresolved-wsdl-reference" => Some("checkWsdlDefinitions"),
        code if code.starts_with("wsdl-missing-") => Some("checkWsdlDefinitions"),
        _ => None,
    }
}

impl WsTools {
    fn all_diagnostics(&mut self, doc: &Document) -> Vec<Diagnostic> {
        if !self.spring_ws_document(&doc.path) {
            return Vec::new();
        }
        if doc.language == "xml" {
            let mut problems=xml_contracts::diagnostics(doc);
            problems.extend(wsdl_qnames::diagnostics(doc));
            problems.extend(wsdl_definitions::required(doc));
            // Current unsaved schema facts update only this cached file; no scan.
            let Some(index) = self.schemas.for_cached_document(&doc.path, &doc.text) else {
                return problems;
            };
            if !self.watching || !complete_schemas(index) {
                return problems;
            }
            problems.extend(wsdl_pairs::unpaired_requests(doc, index));
            problems.extend(wsdl_definitions::pairs(doc, index));
            problems.extend(sws_resources(&doc.text)
                .into_iter()
                .filter(|(path, _)| {
                    !index.files().keys().any(|file| RESOURCE_DIRS.iter().any(|dir| file.ends_with(&format!("{dir}/{path}"))))
                })
                .map(|(path, location)| {
                    Diagnostic::new(location, Severity::Info, format!("`{path}` is not in src/main/resources or src/test/resources; it may come from a dependency."))
                        .code("missing-resource")
                })
                .collect::<Vec<_>>());
            return problems;
        }
        if !matches!(doc.language.as_str(), "java" | "kotlin") || doc.text.len() > MAX_DOCUMENT_BYTES {
            return Vec::new();
        }
        let mut problems = xpath::syntax_diagnostics(doc);
        problems.extend(wsdl_pairs::void_response_payload(doc));
        problems.extend(wsdl_definitions::required(doc));
        if !self.watching {
            return problems;
        }
        problems.extend(xpath::undeclared_prefixes(self, doc));
        let Some(index) = self.schemas.for_cached_document(&doc.path, &doc.text) else { return problems };
        if !complete_schemas(index) {
            return problems;
        }
        if index.iter().all(|(_, file)| file.qnames_complete) {
            for endpoint in endpoints_with_syntax(&parse_source(&doc.path, &doc.text), &doc.text, doc.path.ends_with(".kt")) {
                let Some(namespace) = endpoint.namespace else { continue };
                let mut known = index.iter().flat_map(|(_, file)| &file.schemas).filter(|schema| schema.namespace == namespace).peekable();
                // No matching project schema means unknown/external, not missing.
                if known.peek().is_some() && !known.any(|schema| schema.elements.iter().any(|(name, _)| name == &endpoint.local)) {
                    problems.push(
                        Diagnostic::new(
                            endpoint.local_range,
                            Severity::Warning,
                            format!("No schema of namespace {namespace} has a top-level element `{}`.", endpoint.local),
                        )
                        .code("unknown-payload-element"),
                    );
                }
            }
        }
        if doc.language == "java" {
            problems.extend(wsdl_definitions::pairs(doc, index));
        }
        for (path, source) in resources(&doc.path, &doc.text) {
            let schema_file = path.ends_with(".xsd") || path.ends_with(".wsdl");
            let exists =
                index.files().keys().any(|file| RESOURCE_DIRS.iter().any(|directory| file.ends_with(&format!("{directory}/{path}"))));
            if schema_file && !exists {
                problems.push(
                    Diagnostic::new(
                        range(&doc.text, source.at, source.end),
                        Severity::Info,
                        format!("`{path}` is not in src/main/resources or src/test/resources; it may come from a dependency."),
                    )
                    .code("missing-resource"),
                );
            }
        }
        problems
    }
}

const APPLIES_TTL: Duration = Duration::from_secs(60);

impl WsTools {
    fn spring_ws(&mut self, project: &openidea_plugin::Project) -> bool {
        let Ok(root) = project.root() else {
            return true;
        };
        if let Some((at, answer)) = self.applies.get(&root)
            && at.elapsed() < APPLIES_TTL
        {
            return *answer;
        }
        let answer = spring_text::build::applies(project, &["spring-ws"]);
        self.applies.insert(root, (Instant::now(), answer));
        answer
    }

    fn spring_ws_document(&mut self, path: &str) -> bool {
        match project_of(path) {
            Some(project) => self.spring_ws(&project),
            None => true,
        }
    }

    fn observe_resource_paths(&mut self, events: &[Event], deadline: Instant) {
        let mut changed = spring_text::index::changed_paths(events);
        changed.extend(events.iter().filter_map(|event| match event {
            Event::FileOpened(path) => Some((path.as_str(), false)),
            _ => None,
        }));
        if changed.iter().filter(|(path, _)| resource_paths::relevant(path)).count() > 256 {
            self.resource_paths.reset();
            return;
        }
        for (path, removed) in changed {
            if !resource_paths::relevant(path) {
                continue;
            }
            let Some(project) = project_of(path) else { continue };
            if !self.spring_ws(&project) {
                continue;
            }
            let budget = deadline.saturating_duration_since(Instant::now());
            if budget.is_zero() {
                self.resource_paths.reset();
                return;
            }
            self.resource_paths.observe(&project, path, removed);
            self.resource_paths.warm(&project, budget.min(Duration::from_millis(250)));
        }
    }
}

impl Plugin for WsTools {
    fn activate(&mut self, _ctx: &Context) -> Result<(), Error> {
        self.watching = host::subscribe_paths("files-changed", WATCH_GLOBS).is_ok();
        let _ = host::subscribe(EventKind::FileOpened);
        let _ = host::subscribe(EventKind::FileSaved);
        let deadline = Instant::now() + COMMAND_BUDGET;
        for project in host::projects().unwrap_or_default() {
            let budget = deadline.saturating_duration_since(Instant::now());
            if budget.is_zero() {
                break;
            }
            if !self.spring_ws(&project) {
                continue;
            }
            let step = |deadline: Instant| deadline.saturating_duration_since(Instant::now()).min(Duration::from_millis(250));
            self.schemas.get(&project, budget.min(Duration::from_millis(250)));
            self.xpath_packages.get(&project, step(deadline));
            self.source.get(&project, step(deadline));
            self.resource_paths.warm(&project, step(deadline));
        }
        Ok(())
    }

    fn handle_events(&mut self, events: &[Event], _ctx: &Invocation) {
        let deadline = Instant::now() + COMMAND_BUDGET;
        self.observe_resource_paths(events, deadline);
        self.schemas.on_events(events, deadline);
        self.source.on_events(events, deadline);
        self.xpath_packages.on_events(events, deadline);
    }

    fn run_command(&mut self, id: &str, ctx: &Invocation, _arg: Option<&str>) -> Result<Option<String>, Error> {
        if id == "refresh-models" {
            let project=ctx.project().ok_or("Open a project first.")?;
            if !self.watching {
                match host::subscribe_paths("files-changed",WATCH_GLOBS) {
                    Ok(()) => self.watching=true,
                    Err(Error::Denied(_)) => return Err(Error::Failed(ui_kit::denied::message("Refresh Schemas and Endpoints","events","Spring Web Services Tools"))),
                    Err(error) => return Err(error),
                }
            }
            let deadline=Instant::now()+COMMAND_BUDGET;
            let left=||deadline.saturating_duration_since(Instant::now());
            let mut ready=true;
            if left().is_zero(){ready=false;}else{ready&=self.source.get(&project,left()).fresh();}
            if left().is_zero(){ready=false;}else{ready&=self.xpath_packages.get(&project,left()).fresh();}
            if left().is_zero(){ready=false;}else{ready&=self.schemas.get(&project,left()).fresh();}
            if left().is_zero(){ready=false;}else{
                self.resource_paths.warm(&project,left());
                ready&=project.root().is_ok_and(|root|self.resource_paths.ready(&root));
            }
            return Ok(Some(if ready{"Schemas and endpoints refreshed."}else{"Refresh incomplete. Run Spring WS: Refresh Schemas and Endpoints again to continue."}.into()));
        }
        if id == "qname-occurrences" { return indexing::occurrences(self,ctx); }
        Err(Error::NotFound(format!("command {id}")))
    }
}

register_plugin!(WsTools, providers, diagnostics, file_actions);

pub(crate) const SCHEMA_MARKER: &str = "icons/marker-schema.svg";
pub(crate) const ENDPOINT_MARKER: &str = "icons/marker-endpoint.svg";
pub(crate) const WSDL_MARKER: &str = "icons/marker-wsdl.svg";

#[cfg(test)]
mod tests {
    #[test]
    fn marker_icons_are_declared_in_the_manifest() {
        let manifest = include_str!("../plugin.toml");
        for icon in [crate::SCHEMA_MARKER, crate::ENDPOINT_MARKER, crate::WSDL_MARKER] {
            assert!(manifest.contains(&format!("icon = \"{icon}\"")));
            assert!(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(icon).is_file());
        }
    }

    use super::*;
    use openidea_plugin::mock::MockHost;
    use openidea_plugin::text::position;

    const ENDPOINT: &str = r#"package com.acme.ws;

@org.springframework.ws.server.endpoint.annotation.Endpoint
public class CountryEndpoint {
    private static final String NAMESPACE_URI = "http://acme.com/countries";

    @org.springframework.ws.server.endpoint.annotation.PayloadRoot(namespace = NAMESPACE_URI, localPart = "getCountryRequest")
    @ResponsePayload
    public GetCountryResponse getCountry(@RequestPayload GetCountryRequest request) { return null; }

    @org.springframework.ws.server.endpoint.annotation.PayloadRoot(namespace = NAMESPACE_URI, localPart = "deleteCountryRequest")
    public void delete(@RequestPayload Object request) {}

    @org.springframework.ws.server.endpoint.annotation.PayloadRoot(namespace = "http://acme.com/orders", localPart = "placeOrder")
    public void order(@RequestPayload Object request) {}
}

@Configuration
class WsConfig {
    @Bean(name = "countries")
    public DefaultWsdl11Definition defaultWsdl11Definition(XsdSchema countriesSchema) { return null; }

    @Bean
    public XsdSchema countriesSchema() { return new SimpleXsdSchema(new org.springframework.core.io.ClassPathResource("countries.xsd")); }

    @Bean
    public XsdSchema missing() { return new SimpleXsdSchema(new org.springframework.core.io.ClassPathResource("/xsd/missing.xsd")); }
}
"#;

    const XSD: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" targetNamespace="http://acme.com/countries">
    <xs:element name="getCountryRequest">
        <xs:complexType><xs:sequence><xs:element name="name" type="xs:string"/></xs:sequence></xs:complexType>
    </xs:element>
    <xs:element name="getCountryResponse"/>
</xs:schema>
"#;

    const WSDL: &str = r#"<wsdl:definitions xmlns:wsdl="http://schemas.xmlsoap.org/wsdl/">
  <wsdl:types>
    <schema xmlns="http://www.w3.org/2001/XMLSchema" targetNamespace="http://acme.com/orders">
      <element name="placeOrder"/>
    </schema>
  </wsdl:types>
</wsdl:definitions>
"#;

    const JAVA_PATH: &str = "/project/src/main/java/com/acme/ws/CountryEndpoint.java";
    const XSD_PATH: &str = "/project/src/main/resources/countries.xsd";

    fn setup() -> MockHost {
        let host = MockHost::install();
        host.add_file("src/main/java/com/acme/ws/CountryEndpoint.java", ENDPOINT);
        host.add_file("src/main/resources/countries.xsd", XSD);
        host.add_file("src/main/resources/wsdl/orders.wsdl", WSDL);
        host
    }

    fn java() -> Document {
        Document { path: JAVA_PATH.into(), language: "java".into(), version: 1, text: ENDPOINT.into() }
    }

    fn pos(text: &str, needle: &str) -> Position {
        position(text, text.find(needle).unwrap())
    }

    const KOTLIN_ENDPOINT: &str = r#"package com.acme.ws

@org.springframework.ws.server.endpoint.annotation.Endpoint
class KtCountryEndpoint {
    @org.springframework.ws.server.endpoint.annotation.PayloadRoot(namespace = NAMESPACE_URI, localPart = "getCountryRequest")
    @ResponsePayload
    suspend fun getCountry(@RequestPayload request: GetCountryRequest): GetCountryResponse = TODO()

    @org.springframework.ws.server.endpoint.annotation.PayloadRoot(namespace = NAMESPACE_URI, localPart = "ghostRequest")
    fun ghost(@RequestPayload request: Any) {}

    companion object {
        const val NAMESPACE_URI = "http://acme.com/countries"
    }
}
"#;

    #[test]
    fn kotlin_payload_root_is_indexed() {
        let kt = parse_source("K.kt", KOTLIN_ENDPOINT);
        let found = endpoints_with_syntax(&kt, KOTLIN_ENDPOINT, true);
        let names: Vec<_> = found.iter().map(|e| (e.namespace.as_deref().unwrap(), e.local.as_str())).collect();
        assert_eq!(names, [("http://acme.com/countries", "getCountryRequest"), ("http://acme.com/countries", "ghostRequest")]);
        assert_eq!(scan_java("K.kt", KOTLIN_ENDPOINT).unwrap().len(), 2);
        assert_eq!(endpoints(&java::parse(ENDPOINT), ENDPOINT).len(), 3);

        let host = setup();
        let path = "/project/src/main/kotlin/com/acme/ws/KtCountryEndpoint.kt";
        host.add_file("src/main/kotlin/com/acme/ws/KtCountryEndpoint.kt", KOTLIN_ENDPOINT);
        let mut ws = WsTools::default();
        ws.activate(&host.context("aritti.spring-ws")).unwrap();
        let doc = Document { path: path.into(), language: "kotlin".into(), version: 1, text: KOTLIN_ENDPOINT.into() };
        let tips: Vec<_> = ws.line_markers(&doc).into_iter().map(|m| m.tooltip).collect();
        assert_eq!(tips, ["Payload element <getCountryRequest> in countries.xsd"]);
        let problems = ws.diagnostics(&doc);
        assert!(host.diagnostics().is_empty(), "markers must not duplicate provider diagnostics");
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].code.as_deref(), Some("unknown-payload-element"));
        let xsd_tips: Vec<_> = ws
            .line_markers(&Document { path: XSD_PATH.into(), language: "xml".into(), version: 1, text: XSD.into() })
            .into_iter()
            .map(|m| m.tooltip)
            .collect();
        assert!(xsd_tips[0].contains("KtCountryEndpoint.getCountry"), "{xsd_tips:?}");
    }

    #[test]
    fn reads_endpoints_and_schemas() {
        let found = endpoints(&java::parse(ENDPOINT), ENDPOINT);
        let names: Vec<_> = found.iter().map(|e| (e.namespace.as_deref().unwrap(), e.local.as_str())).collect();
        assert_eq!(
            names,
            [
                ("http://acme.com/countries", "getCountryRequest"),
                ("http://acme.com/countries", "deleteCountryRequest"),
                ("http://acme.com/orders", "placeOrder")
            ]
        );
        let s = schemas(XSD);
        assert_eq!(s[0].namespace, "http://acme.com/countries");
        assert_eq!(s[0].elements.iter().map(|e| e.0.as_str()).collect::<Vec<_>>(), ["getCountryRequest", "getCountryResponse"]);
        assert_eq!(schemas(WSDL)[0].elements[0].0, "placeOrder");
    }

    #[test]
    fn every_matching_element_or_handler_gets_a_marker_on_its_line() {
        let host = setup();
        host.add_file("src/main/resources/legacy/countries.xsd", XSD);
        host.add_file(
            "src/main/java/com/acme/ws/LegacyEndpoint.java",
            &ENDPOINT.replace("class CountryEndpoint", "class LegacyEndpoint").replace("class WsConfig", "class LegacyConfig"),
        );
        let mut ws = WsTools::default();
        ws.activate(&host.context("aritti.spring-ws")).unwrap();
        let markers = ws.line_markers(&java());
        let schemas: Vec<_> = markers.iter().filter(|m| m.line == 8).map(|m| m.target.as_ref().unwrap().path.as_str()).collect();
        assert_eq!(schemas.len(), 2, "{schemas:?}");
        assert!(schemas.contains(&XSD_PATH) && schemas.contains(&"/project/src/main/resources/legacy/countries.xsd"));
        assert!(markers.iter().filter(|m| m.line == 8).all(|m| m.icon.as_deref() == Some(SCHEMA_MARKER)));
        assert_eq!(markers.iter().filter(|m| m.line == 14).count(), 1, "one schema, one marker");
        assert_eq!(ws.definition(&java(), pos(ENDPOINT, "getCountryRequest\"")).len(), 2, "definition lists the same candidates");

        let xsd = ws.line_markers(&Document { path: XSD_PATH.into(), language: "xml".into(), version: 1, text: XSD.into() });
        let handlers: Vec<_> = xsd.iter().map(|m| m.target.as_ref().unwrap().path.rsplit('/').next().unwrap()).collect();
        assert_eq!(handlers.len(), 2, "{handlers:?}");
        assert!(handlers.contains(&"CountryEndpoint.java") && handlers.contains(&"LegacyEndpoint.java"));
        assert!(xsd.iter().all(|m| m.line == xsd[0].line && m.icon.as_deref() == Some(ENDPOINT_MARKER)));
    }

    #[test]
    fn links_endpoints_to_schema_elements_and_checks_them() {
        let host = setup();
        let mut ws = WsTools::default();
        ws.activate(&host.context("aritti.spring-ws")).unwrap();
        let markers = ws.line_markers(&java());
        let tips: Vec<_> = markers.iter().map(|m| (m.line, m.tooltip.as_str())).collect();
        assert_eq!(
            tips,
            [
                (8, "Payload element <getCountryRequest> in countries.xsd"),
                (14, "Payload element <placeOrder> in orders.wsdl"),
                (20, "Published as /countries.wsdl"),
            ]
        );
        assert_eq!(markers[0].target.as_ref().unwrap().path, XSD_PATH);
        assert_eq!(markers[0].target.as_ref().unwrap().range.start, pos(XSD, "getCountryRequest\""));

        let problems = ws.diagnostics(&java());
        assert!(host.diagnostics().is_empty(), "markers must not duplicate provider diagnostics");
        let codes: Vec<_> = problems.iter().map(|p| (p.code.as_deref().unwrap(), p.range.start)).collect();
        assert_eq!(
            codes,
            [("unknown-payload-element", pos(ENDPOINT, "deleteCountryRequest")), ("missing-resource", pos(ENDPOINT, "/xsd/missing.xsd"))]
        );
    }

    #[test]
    fn navigates_and_completes_payload_elements() {
        let _host = setup();
        let mut ws = WsTools::default();
        let found = ws.definition(&java(), pos(ENDPOINT, "etCountryRequest"));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, XSD_PATH);

        let labels: Vec<_> = ws.completion(&java(), pos(ENDPOINT, "eleteCountryRequest"), None).into_iter().map(|i| i.label).collect();
        assert_eq!(labels, ["getCountryRequest", "getCountryResponse"]);
    }

    #[test]
    fn navigates_to_classpath_resources() {
        let host = setup();
        let mut ws = WsTools::default();
        ws.activate(&host.context("aritti.spring-ws")).unwrap();
        let found = ws.definition(&java(), pos(ENDPOINT, "ountries.xsd"));
        assert_eq!(found, [Location { path: XSD_PATH.into(), range: Range::default() }]);
        assert!(ws.definition(&java(), pos(ENDPOINT, "sd/missing.xsd")).is_empty());
    }

    #[test]
    fn marks_schema_elements_with_their_endpoint() {
        let _host = setup();
        let mut ws = WsTools::default();
        let doc = Document { path: XSD_PATH.into(), language: "xml".into(), version: 1, text: XSD.into() };
        let markers = ws.line_markers(&doc);
        assert_eq!(markers.len(), 1);
        assert_eq!(markers[0].line, 2);
        assert_eq!(markers[0].tooltip, "Handled by CountryEndpoint.getCountry");
        assert_eq!(markers[0].target.as_ref().unwrap().range.start, pos(ENDPOINT, "getCountry("));
    }

    fn endpoint_doc(namespace: &str, local: &str) -> Document {
        Document {
            text: format!(r#"@org.springframework.ws.server.endpoint.annotation.Endpoint class E {{ @org.springframework.ws.server.endpoint.annotation.PayloadRoot(namespace = {namespace}, localPart = "{local}") void handle() {{}} }}"#),
            ..java()
        }
    }

    #[test]
    fn cold_provider_does_not_discover_project_schemas() {
        let _host = setup();
        let mut ws = WsTools { watching: true, ..WsTools::default() };
        assert!(ws.diagnostics(&java()).is_empty());
        let cache = ws.schemas.for_cached_document(JAVA_PATH, ENDPOINT).unwrap();
        assert!(!cache.complete());
        assert!(cache.files().is_empty(), "diagnostics must not discover disk schemas");
    }

    #[test]
    fn unsaved_endpoint_changes_and_fixes_replace_diagnostics() {
        let host = setup();
        let mut ws = WsTools::default();
        ws.activate(&host.context("aritti.spring-ws")).unwrap();
        let broken = endpoint_doc(r#""http://acme.com/countries""#, "ghostRequest");
        host.open_editor(JAVA_PATH, "java", &broken.text, &[]);
        host.refresh_diagnostics(&mut ws);
        assert_eq!(host.provider_diagnostics()[JAVA_PATH].len(), 1);
        let fixed = endpoint_doc(r#""http://acme.com/countries""#, "getCountryRequest");
        host.open_editor(JAVA_PATH, "java", &fixed.text, &[]);
        host.type_in_editor(" // unsaved version two");
        host.refresh_diagnostics(&mut ws);
        assert_eq!(host.editor_version(), 2);
        assert!(host.provider_diagnostics().is_empty());
        assert!(host.diagnostics().is_empty());
    }

    #[test]
    fn diagnostic_namespace_matching_stays_exact_and_unknown_stays_quiet() {
        let host = setup();
        let mut ws = WsTools::default();
        ws.activate(&host.context("aritti.spring-ws")).unwrap();
        // placeOrder exists, but only in the orders schema.
        assert_eq!(ws.diagnostics(&endpoint_doc(r#""http://acme.com/countries""#, "placeOrder")).len(), 1);
        assert!(ws.diagnostics(&endpoint_doc(r#""http://external/schema""#, "ghost")).is_empty());
        assert!(ws.diagnostics(&endpoint_doc("Runtime.namespace()", "ghost")).is_empty());
    }

    #[test]
    fn unsaved_schema_snapshot_updates_facts_without_disk_reread() {
        let host = setup();
        let mut ws = WsTools::default();
        ws.activate(&host.context("aritti.spring-ws")).unwrap();
        let doc = endpoint_doc(r#""http://acme.com/countries""#, "newRequest");
        assert_eq!(ws.diagnostics(&doc).len(), 1);
        let xml =
            Document { path: XSD_PATH.into(), language: "xml".into(), version: 2, text: XSD.replace("getCountryRequest", "newRequest") };
        assert!(ws.diagnostics(&xml).is_empty());
        assert!(ws.diagnostics(&doc).is_empty());
        assert_eq!(ws.diagnostics(&endpoint_doc(r#""http://acme.com/countries""#, "getCountryRequest")).len(), 1);
    }

    #[test]
    fn oversized_schema_and_denied_events_suppress_absence_warnings() {
        let host = setup();
        host.add_file("src/main/resources/large.xsd", &"x".repeat(MAX_SCHEMA_BYTES + 1));
        let mut ws = WsTools::default();
        ws.activate(&host.context("aritti.spring-ws")).unwrap();
        assert!(ws.diagnostics(&java()).is_empty());
        let host = host.with_capabilities(&["project.read"]);
        let mut ws = WsTools::default();
        ws.activate(&host.context("aritti.spring-ws")).unwrap();
        assert!(!ws.watching);
        assert!(ws.diagnostics(&java()).is_empty());
        assert!(!ws.definition(&java(), pos(ENDPOINT, "etCountryRequest")).is_empty());
    }

    const ROOTS_JAVA: &str = r#"@org.springframework.ws.server.endpoint.annotation.Endpoint class E {
        static final String NS = "http://acme.com/countries";
        @org.springframework.ws.server.endpoint.annotation.PayloadRoots(value = {
            @org.springframework.ws.server.endpoint.annotation.PayloadRoot(namespace = NS, localPart = "getCountryRequest"),
            @org.springframework.ws.server.endpoint.annotation.PayloadRoot(namespace = "http://acme.com/orders", localPart = "placeOrder"),
            @org.springframework.ws.server.endpoint.annotation.PayloadRoot(namespace = NS, localPart = "ghostRequest")
        })
        void handle() {}
    }"#;

    #[test]
    fn java_payload_roots_share_endpoint_navigation_completion_and_diagnostics() {
        let host = setup();
        host.add_file("src/main/java/com/acme/ws/CountryEndpoint.java", ROOTS_JAVA);
        let mut ws = WsTools::default();
        ws.activate(&host.context("aritti.spring-ws")).unwrap();
        let doc = Document { text: ROOTS_JAVA.into(), ..java() };
        let found = endpoints(&java::parse(ROOTS_JAVA), ROOTS_JAVA);
        assert_eq!(found.iter().map(|e| e.local.as_str()).collect::<Vec<_>>(), ["getCountryRequest", "placeOrder", "ghostRequest"]);
        assert!(found.iter().all(|e| e.label == "E.handle"));
        let markers = ws.line_markers(&doc);
        assert_eq!(markers.len(), 2);
        assert_eq!(ws.definition(&doc, pos(ROOTS_JAVA, "placeOrder")).len(), 1);
        let labels: Vec<_> = ws.completion(&doc, pos(ROOTS_JAVA, "ghostRequest"), None).into_iter().map(|item| item.label).collect();
        assert_eq!(labels, ["getCountryRequest", "getCountryResponse"]);
        let problems = ws.diagnostics(&doc);
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].range.start, pos(ROOTS_JAVA, "ghostRequest"));
        let reverse = ws.line_markers(&Document { path: XSD_PATH.into(), language: "xml".into(), text: XSD.into(), version: 1 });
        assert_eq!(reverse.len(), 1);
        assert_eq!(reverse[0].tooltip, "Handled by E.handle");
        let fixed = Document { text: ROOTS_JAVA.replace("ghostRequest", "getCountryResponse"), version: 2, ..doc };
        assert!(ws.diagnostics(&fixed).is_empty());
    }

    #[test]
    fn kotlin_payload_roots_support_array_and_vararg_annotation_constructors() {
        for args in [
            r#"value = [org.springframework.ws.server.endpoint.annotation.PayloadRoot(namespace = "http://acme.com/countries", localPart = "getCountryRequest"), org.springframework.ws.server.endpoint.annotation.PayloadRoot(namespace = runtime(), localPart = "ghost")]"#,
            r#"org.springframework.ws.server.endpoint.annotation.PayloadRoot(namespace = "http://acme.com/countries", localPart = "getCountryRequest"), org.springframework.ws.server.endpoint.annotation.PayloadRoot(namespace = runtime(), localPart = "ghost")"#,
        ] {
            let host = setup();
            let text = format!("@org.springframework.ws.server.endpoint.annotation.Endpoint class E {{ @org.springframework.ws.server.endpoint.annotation.PayloadRoots({args}) fun handle() {{}} }}");
            let path = "/project/src/main/kotlin/E.kt";
            let doc = Document { path: path.into(), language: "kotlin".into(), text, version: 1 };
            let mut ws = WsTools::default();
            ws.activate(&host.context("aritti.spring-ws")).unwrap();
            let roots = scan_java(path, &doc.text).unwrap();
            assert_eq!(roots.len(), 2);
            assert_eq!(roots[0].namespace.as_deref(), Some("http://acme.com/countries"));
            assert_eq!(roots[1].namespace, None);
            assert_eq!(ws.line_markers(&doc).len(), 1);
            assert!(ws.diagnostics(&doc).is_empty(), "dynamic namespace has no absence assertion");
        }
    }

    #[test]
    fn malformed_or_dynamic_container_values_do_not_invent_endpoints() {
        for args in [
            r#"value = roots()"#,
            r#"value = {@Other(localPart = "ghost", namespace = "urn:test")}"#,
            r#"value = {@org.springframework.ws.server.endpoint.annotation.PayloadRoot(localPart = "ghost")}, value = {@org.springframework.ws.server.endpoint.annotation.PayloadRoot(localPart = "other")}"#,
            r#"value = {@org.springframework.ws.server.endpoint.annotation.PayloadRoot(localPart = "ghost") + extra}"#,
            r#"value = {@org.springframework.ws.server.endpoint.annotation.PayloadRoot(localPart = "ghost")]"#,
            r#"value = {org.springframework.ws.server.endpoint.annotation.PayloadRoot(localPart = "ghost")}"#,
            r#"value = {@org.springframework.ws.server.endpoint.annotation.PayloadRoot(localPart = "ghost" + suffix)}"#,
            r#"value = {@org.springframework.ws.server.endpoint.annotation.PayloadRoot(localPart = "ghost", localPart = "other")}"#,
            r#"value = {@org.springframework.ws.server.endpoint.annotation.PayloadRoot(namespace = "a", namespace = "b", localPart = "ghost")}"#,
        ] {
            let text = format!("@org.springframework.ws.server.endpoint.annotation.Endpoint class E {{ @org.springframework.ws.server.endpoint.annotation.PayloadRoots({args}) void handle() {{}} }}");
            assert!(endpoints(&java::parse(&text), &text).is_empty(), "{args}");
        }
    }

    #[test]
    fn container_ranges_use_utf16_and_omitted_namespace_is_empty_namespace() {
        let host = setup();
        host.add_file("src/main/resources/no-namespace.xsd", r#"<schema xmlns="http://www.w3.org/2001/XMLSchema"><element name="known"/></schema>"#);
        let mut ws = WsTools::default();
        ws.activate(&host.context("aritti.spring-ws")).unwrap();
        let text = r#"@org.springframework.ws.server.endpoint.annotation.Endpoint class E { @org.springframework.ws.server.endpoint.annotation.PayloadRoots({@org.springframework.ws.server.endpoint.annotation.PayloadRoot(namespace = "urn:🦀", localPart = "external"), @org.springframework.ws.server.endpoint.annotation.PayloadRoot(localPart = "ghost")}) void handle() {} }"#;
        let doc = Document { text: text.into(), ..java() };
        let found = endpoints(&java::parse(text), text);
        assert_eq!(found.len(), 2);
        assert_eq!(found[1].namespace.as_deref(), Some(""));
        let problems = ws.diagnostics(&doc);
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].range.start, pos(text, "ghost"));
        assert_eq!(problems[0].range.start.character as usize, text[..text.find("ghost").unwrap()].encode_utf16().count());
        let fixed = Document { text: text.replace("ghost", "known"), version: 2, ..doc };
        assert!(ws.diagnostics(&fixed).is_empty());
    }

    fn xml_doc(text: &str) -> Document {
        Document { path: "/project/src/main/resources/services.xml".into(), language: "xml".into(), text: text.into(), version: 1 }
    }

    #[test]
    fn sws_classpath_locations_navigate_and_unsaved_fixes_clear_warnings() {
        let host = setup();
        let text = r#"<!-- 🦀 --><beans xmlns:s="http://www.springframework.org/schema/web-services"><s:static-wsdl location="classpath:wsdl/orders.wsdl"/><s:dynamic-wsdl id="x" portTypeName="P"><s:xsd location="classpath:/countries.xsd"/><s:xsd location="classpath:missing.xsd"/></s:dynamic-wsdl></beans>"#;
        let doc = xml_doc(text);
        let mut ws = WsTools::default();
        ws.activate(&host.context("aritti.spring-ws")).unwrap();
        let resources = sws_resources(text);
        assert_eq!(
            resources.iter().map(|(path, _)| path.as_str()).collect::<Vec<_>>(),
            ["wsdl/orders.wsdl", "countries.xsd", "missing.xsd"]
        );
        assert_eq!(ws.definition(&doc, pos(text, "classpath:/countries")).first().unwrap().path, XSD_PATH);
        assert_eq!(ws.definition(&doc, pos(text, "classpath:wsdl")).first().unwrap().path, "/project/src/main/resources/wsdl/orders.wsdl");
        let problems = ws.diagnostics(&doc);
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].code.as_deref(), Some("missing-resource"));
        assert_eq!(problems[0].range.start, pos(text, "classpath:missing"));
        assert_eq!(problems[0].range.start.character as usize, text[..text.find("classpath:missing").unwrap()].encode_utf16().count());
        let fixed = Document { text: text.replace("missing.xsd", "countries.xsd"), version: 2, ..doc };
        assert!(ws.diagnostics(&fixed).is_empty());
        assert!(host.diagnostics().is_empty());
    }

    #[test]
    fn sws_locations_require_bound_namespace_valid_parent_and_unqualified_attribute() {
        let accepted =
            r#"<dynamic-wsdl xmlns="http://www.springframework.org/schema/web-services"><xsd location="classpath:a.xsd"/></dynamic-wsdl>"#;
        assert_eq!(sws_resources(accepted).len(), 1);
        for text in [
            r#"<s:static-wsdl location="classpath:a.wsdl"/>"#,
            r#"<static-wsdl location="classpath:a.wsdl"/>"#,
            r#"<s:static-wsdl xmlns:s="urn:other" location="classpath:a.wsdl"/>"#,
            r#"<beans xmlns:s="http://www.springframework.org/schema/web-services"><s:static-wsdl xmlns:s="urn:other" location="classpath:a.wsdl"/></beans>"#,
            r#"<s:xsd xmlns:s="http://www.springframework.org/schema/web-services" location="classpath:a.xsd"/>"#,
            r#"<other xmlns:s="http://www.springframework.org/schema/web-services"><s:xsd location="classpath:a.xsd"/></other>"#,
            r#"<s:static-wsdl xmlns:s="http://www.springframework.org/schema/web-services" s:location="classpath:a.wsdl"/>"#,
            r#"<s:static-wsdl xmlns:s="http://www.springframework.org/schema/web-services" location="classpath:a.wsdl" location="classpath:b.wsdl"/>"#,
            r#"<s:static-wsdl xmlns:s="http://www.springframework.org/schema/web-services" location="classpath:a.wsdl""#,
            r#"<s:static-wsdl xmlns:s="http://www.springframework.org/schema/web-services" location="classpath:a.wsdl/>"#,
        ] {
            assert!(sws_resources(text).is_empty(), "{text}");
        }
    }

    #[test]
    fn sws_external_dynamic_and_ambiguous_resource_locations_stay_unknown() {
        for location in [
            "https://example.org/a.wsdl",
            "file:/tmp/a.wsdl",
            "a.wsdl",
            "classpath*:a.wsdl",
            "classpath:${name}.wsdl",
            "classpath:#{name}.wsdl",
            "classpath:../a.wsdl",
            "classpath:a&amp;b.wsdl",
            "classpath:*.wsdl",
            "classpath:a.xsd",
        ] {
            let doc =
                xml_doc(&format!(r#"<s:static-wsdl xmlns:s="http://www.springframework.org/schema/web-services" location="{location}"/>"#));
            assert!(sws_resources(&doc.text).is_empty(), "{location}");
        }
        let _host = setup();
        let mut ws = WsTools { watching: true, ..WsTools::default() };
        let doc =
            xml_doc(r#"<s:static-wsdl xmlns:s="http://www.springframework.org/schema/web-services" location="classpath:missing.wsdl"/>"#);
        assert!(ws.diagnostics(&doc).is_empty());
        assert!(
            ws.schemas.for_cached_document(&doc.path, &doc.text).unwrap().files().is_empty(),
            "cold XML diagnostics do not discover schemas"
        );
    }

    #[test]
    fn xsd_with_2000_elements_is_valid() {
        let elements: String = (0..2000).map(|n| format!("<xs:element name=\"e{n}\"></xs:element>\n")).collect();
        let text = format!("<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" targetNamespace=\"urn:big\">\n{elements}</xs:schema>");
        assert!(text.len() < MAX_SCHEMA_BYTES);
        assert!(generate::xml_valid(&text));
    }

    #[test]
    fn created_then_deleted_schema_not_unreadable() {
        let host = setup();
        let mut ws = WsTools::default();
        ws.activate(&host.context("aritti.spring-ws")).unwrap();
        let path = "/project/src/main/resources/new.xsd";
        let batch = Event::FilesChanged(openidea_plugin::FilesChanged {
            created: vec![path.into()],
            modified: vec![],
            removed: vec![path.into()],
        });
        ws.handle_events(&[batch], &host.invocation());
        ws.line_markers(&java());
        assert_eq!(ws.diagnostics(&java()).len(), 2);
    }

    #[test]
    fn unreadable_schema_events_stay_quiet_until_facts_recover() {
        let host = setup();
        let mut ws = WsTools::default();
        ws.activate(&host.context("aritti.spring-ws")).unwrap();
        let path = "/project/src/main/resources/new.xsd";
        host.add_file_bytes("src/main/resources/new.xsd", &[0xff, 0xfe]);
        let event = Event::FileSaved(path.into());
        ws.handle_events(&[event.clone()], &host.invocation());
        ws.line_markers(&java());
        assert!(ws.diagnostics(&java()).is_empty());
        host.add_file("src/main/resources/new.xsd", XSD);
        ws.handle_events(&[event], &host.invocation());
        assert_eq!(ws.diagnostics(&java()).len(), 2);
    }

    #[test]
    fn no_warm_without_spring_ws_dependency() {
        let walked = |pom: &str| {
            let host = setup();
            host.add_file("pom.xml", pom);
            let mut ws = WsTools::default();
            ws.activate(&host.context("aritti.spring-ws")).unwrap();
            ws.handle_events(&[Event::FileSaved("/project/pom.xml".into())], &host.invocation());
            ws.line_markers(&java());
            ws.diagnostics(&java());
            let schemas = ws.schemas.for_cached_document(JAVA_PATH, "").unwrap().walked();
            (schemas, ws.resource_paths.ready("/project"))
        };
        let plain = "<project><dependencies><dependency><artifactId>spring-web</artifactId></dependency></dependencies></project>";
        assert_eq!(walked(plain), (false, false));
        let with_ws = "<project><dependencies><dependency><artifactId>spring-ws-core</artifactId></dependency></dependencies></project>";
        assert_eq!(walked(with_ws), (true, true));
    }

    #[test]
    fn missing_resource_is_info() {
        let host = setup();
        let mut ws = WsTools::default();
        ws.activate(&host.context("aritti.spring-ws")).unwrap();
        let from_java = ws.diagnostics(&java());
        let missing: Vec<_> = from_java.iter().filter(|p| p.code.as_deref() == Some("missing-resource")).collect();
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].severity, Severity::Info);
        let text = r#"<beans xmlns:s="http://www.springframework.org/schema/web-services"><s:dynamic-wsdl id="x" portTypeName="P"><s:xsd location="classpath:missing.xsd"/></s:dynamic-wsdl></beans>"#;
        let from_xml = ws.diagnostics(&xml_doc(text));
        assert_eq!(from_xml.len(), 1);
        assert_eq!(from_xml[0].severity, Severity::Info);
        host.set_setting("checkMissingResources", "false");
        assert!(ws.diagnostics(&java()).iter().all(|p| p.code.as_deref() != Some("missing-resource")));
        assert!(ws.diagnostics(&xml_doc(text)).is_empty());
    }

    #[test]
    fn every_setting_maps_to_a_declared_check() {
        let schema = include_str!("../settings.schema.json");
        for code in ["unknown-payload-element", "missing-resource", "wsdl-unpaired-request", "unresolved-wsdl-reference", "wsdl-missing-schema"] {
            assert!(schema.contains(&format!("\"{}\": {{", setting_of(code).unwrap())), "{code}");
        }
        assert_eq!(setting_of("invalid-xpath"), None);
    }

    #[test]
    fn refresh_models_denied_events_message() {
        let host = MockHost::install().with_capabilities(&["project.read"]);
        let mut ws = WsTools::default();
        let error = ws.run_command("refresh-models", &host.invocation(), None).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("needs the events permission") && message.contains("Settings › Plugins › Spring Web Services Tools"), "{message}");
        assert!(!message.contains("permission denied"), "{message}");
    }

    #[test]
    fn refresh_models_refreshes_resource_paths() {
        let host = setup();
        host.add_file("src/main/resources/a/resource.xslt", "opaque");
        let mut ws = WsTools::default();
        ws.activate(&host.context("aritti.spring-ws")).unwrap();
        ws.resource_paths.expire();
        ws.run_command("refresh-models", &host.invocation(), None).unwrap();
        assert!(ws.resource_paths.ready("/project"));
    }
}

#[cfg(test)]
mod namespace_source_ownership {
    use super::*;
    use openidea_plugin::mock::MockHost;
    fn source(field: &str, expression: &str) -> String {
        format!(
            "@org.springframework.ws.server.endpoint.annotation.Endpoint class E {{ {field} @org.springframework.ws.server.endpoint.annotation.PayloadRoot(namespace={expression},localPart=\"owned\") void handle(){{}} }}"
        )
    }
    fn namespace(text: &str) -> Option<String> {
        endpoints(&java::parse(text), text)[0].namespace.clone()
    }
    #[test]
    fn java_namespace_constants_require_unique_lexical_or_explicit_owned_compile_time_field() {
        let local = source("private static final String URI=\"urn:owned\";", "URI");
        assert_eq!(namespace(&local).as_deref(), Some("urn:owned"));
        let qualified = source("private static final String URI=\"urn:owned\";", "E.URI");
        assert_eq!(namespace(&qualified).as_deref(), Some("urn:owned"));
        let external = format!(
            "{} class Ns {{ static final java.lang.String URI=\"urn:other\"; }}",
            source("", "Ns.URI")
        );
        assert_eq!(namespace(&external).as_deref(), Some("urn:other"));
        let literal = source("", "\"urn:literal#fragment\"");
        assert_eq!(namespace(&literal).as_deref(), Some("urn:literal#fragment"));
    }
    #[test]
    fn mutable_unrelated_foreign_shadowed_duplicate_and_runtime_constants_remain_unknown() {
        for text in [
            source("String URI=\"urn:wrong\";", "URI"),
            source("static String URI=\"urn:wrong\";", "URI"),
            source("final String URI=\"urn:wrong\";", "URI"),
            source("static final String URI=runtime();", "URI"),
            source("static final String URI=\"urn:wrong\";", "Missing.URI"),
            format!(
                "{} class Other {{ static final String URI=\"urn:wrong\"; }}",
                source("", "URI")
            ),
            format!(
                "{} class Ns {{static final String URI=\"urn:a\";}} class Ns {{static final String URI=\"urn:b\";}}",
                source("", "Ns.URI")
            ),
            source(
                "static final String URI=\"urn:a\"; static final String URI=\"urn:b\";",
                "URI",
            ),
            format!(
                "import foreign.String; {}",
                source("static final String URI=\"urn:wrong\";", "URI")
            ),
            format!(
                "class String {{}} {}",
                source("static final String URI=\"urn:wrong\";", "URI")
            ),
            format!(
                "import foreign.Ns; {} class Ns {{static final String URI=\"urn:wrong\";}}",
                source("", "Ns.URI")
            ),
            source("static final String URI=\"urn:wrong\";", "app.E.URI"),
        ] {
            assert_eq!(namespace(&text), None, "{text}");
        }
    }
    #[test]
    fn kotlin_constants_are_owned_const_object_or_companion_facts_not_arbitrary_vals() {
        for (field, expression, expected) in [
            (
                "companion object { const val URI=\"urn:owned\" }",
                "URI",
                Some("urn:owned"),
            ),
            ("companion object { val URI=\"urn:wrong\" }", "URI", None),
            ("val URI=\"urn:wrong\"", "URI", None),
        ] {
            let text = format!(
                "@org.springframework.ws.server.endpoint.annotation.Endpoint class E {{ {field}\n @org.springframework.ws.server.endpoint.annotation.PayloadRoot(namespace={expression},localPart=\"owned\") fun handle(){{}} }}"
            );
            let found = endpoints_with_syntax(&parse_source("E.kt", &text), &text, true);
            assert_eq!(found[0].namespace.as_deref(), expected, "{text}");
        }
        let text = "object Ns { const val URI=\"urn:explicit\" } @org.springframework.ws.server.endpoint.annotation.Endpoint class E { @org.springframework.ws.server.endpoint.annotation.PayloadRoot(namespace=Ns.URI,localPart=\"owned\") fun handle(){} }";
        assert_eq!(
            endpoints_with_syntax(&parse_source("E.kt", text), text, true)[0]
                .namespace
                .as_deref(),
            Some("urn:explicit")
        );
    }
    #[test]
    fn escaped_interpolated_dynamic_or_unsupported_local_qnames_do_not_become_known_facts() {
        for expression in [
            "\"urn:\\u0061\"",
            "\"${namespace}\"",
            "\"$NAMESPACE\"",
            "\"urn:bad space\"",
            "\"#{namespace}\"",
        ] {
            assert_eq!(namespace(&source("", expression)), None, "{expression}");
        }
        for local in ["bad:name", "bad name", "${local}", "\\u006fwned", ""] {
            let text = source("", "\"urn:owned\"")
                .replace("localPart=\"owned\"", &format!("localPart=\"{local}\""));
            assert!(endpoints(&java::parse(&text), &text).is_empty(), "{local}");
        }
    }
    #[test]
    fn unknown_constant_namespace_suppresses_false_missing_element_warning_and_unsaved_fix_recovers()
     {
        let h = MockHost::install();
        h.add_file("src/main/resources/schema.xsd","<schema xmlns='http://www.w3.org/2001/XMLSchema' targetNamespace='urn:owned'><element name='other'/></schema>");
        let mut p = WsTools::default();
        p.activate(&h.context("aritti.spring-ws")).unwrap();
        let known = source("static final String URI=\"urn:owned\";", "URI");
        let doc = Document {
            path: "/project/E.java".into(),
            language: "java".into(),
            version: 1,
            text: known.clone(),
        };
        assert_eq!(p.diagnostics(&doc).len(), 1);
        let unknown = Document {
            version: 2,
            text: known.replace("static final String", "String"),
            ..doc.clone()
        };
        assert!(p.diagnostics(&unknown).is_empty());
        let fixed = Document {
            version: 3,
            text: known.replace("localPart=\"owned\"", "localPart=\"other\""),
            ..doc
        };
        assert!(p.diagnostics(&fixed).is_empty());
        assert!(h.requests().is_empty() && h.process_runs().is_empty());
    }
}

#[cfg(test)]
mod sws_xml_context_safety {
    use super::*;
    use openidea_plugin::mock::MockHost;
    #[test]
    fn reserved_or_malformed_xml_cannot_own_sws_resource_locations() {
        for text in [
            format!(
                "<beans xmlns:xml='{SWS_NAMESPACE}'><xml:static-wsdl location='classpath:missing.wsdl'/></beans>"
            ),
            format!(
                "<beans xmlns:s='{SWS_NAMESPACE}'><s:static-wsdl location='classpath:missing.wsdl'/></wrong>"
            ),
            format!(
                "<beans xmlns:s='{SWS_NAMESPACE}' xmlns:xml='urn:foreign'><s:static-wsdl location='classpath:missing.wsdl'/></beans>"
            ),
            format!(
                "<!DOCTYPE beans []><beans xmlns:s='{SWS_NAMESPACE}'><s:static-wsdl location='classpath:missing.wsdl'/></beans>"
            ),
        ] {
            assert!(sws_resources(&text).is_empty(), "{text}");
        }
    }
    #[test]
    fn invalid_current_sws_xml_stays_quiet_and_exact_valid_unsaved_context_recovers() {
        let h = MockHost::install();
        let mut p = WsTools::default();
        p.activate(&h.context("aritti.spring-ws")).unwrap();
        let valid = format!(
            "<beans xmlns:s='{SWS_NAMESPACE}'><!-- 😀 --><s:static-wsdl location='classpath:missing.wsdl'/></beans>"
        );
        let mut d = Document {
            path: "/project/services.xml".into(),
            language: "xml".into(),
            version: 1,
            text: valid.clone(),
        };
        assert_eq!(p.diagnostics(&d).len(), 1);
        d.text = valid.replace("<beans ", "<beans xmlns:xml='urn:foreign' ");
        d.version = 2;
        assert!(p.diagnostics(&d).is_empty());
        d.text = valid;
        d.version = 3;
        assert_eq!(p.diagnostics(&d).len(), 1);
        assert!(h.requests().is_empty() && h.process_runs().is_empty());
    }
    #[test]
    fn public_navigation_requires_valid_namespace_context_and_keeps_legal_xml_binding() {
        let h = MockHost::install();
        h.add_file(
            "src/main/resources/orders.wsdl",
            "<definitions xmlns='http://schemas.xmlsoap.org/wsdl/'/>",
        );
        let mut p = WsTools::default();
        p.activate(&h.context("aritti.spring-ws")).unwrap();
        let valid = format!(
            "<static-wsdl xmlns='{SWS_NAMESPACE}' xmlns:xml='http://www.w3.org/XML/1998/namespace' xml:lang='en' location='classpath:orders.wsdl'/>"
        );
        let doc = Document {
            path: "/project/services.xml".into(),
            language: "xml".into(),
            version: 1,
            text: valid,
        };
        let at =
            openidea_plugin::text::position(&doc.text, doc.text.find("classpath:").unwrap() + 12);
        assert_eq!(p.definition(&doc, at).len(), 1);
        let bad = Document {
            text: format!(
                "<xml:static-wsdl xmlns:xml='{SWS_NAMESPACE}' location='classpath:orders.wsdl'/>"
            ),
            ..doc.clone()
        };
        let at =
            openidea_plugin::text::position(&bad.text, bad.text.find("classpath:").unwrap() + 12);
        assert!(p.definition(&bad, at).is_empty());
        let huge = format!("{}{}", doc.text, " ".repeat(MAX_SCHEMA_BYTES));
        assert!(sws_resources(&huge).is_empty());
    }
}

#[cfg(test)]
mod endpoint_annotation_source_ownership {
    use super::*;
    use openidea_plugin::mock::MockHost;
    const PACKAGE: &str = "org.springframework.ws.server.endpoint.annotation";
    fn owned() -> String {
        format!(
            "import {PACKAGE}.Endpoint; import {PACKAGE}.PayloadRoot; @Endpoint class E {{ @PayloadRoot(namespace=\"urn:owned\",localPart=\"missing\") void handle(){{}} }}"
        )
    }
    #[test]
    fn foreign_unimported_or_mixed_annotation_owners_do_not_create_endpoint_qnames() {
        for source in [
            owned().replace(PACKAGE, "foreign"),
            owned().replace(&format!("import {PACKAGE}.Endpoint; "), ""),
            owned().replace(&format!("import {PACKAGE}.PayloadRoot; "), ""),
            owned().replace(
                &format!("import {PACKAGE}.Endpoint;"),
                "import foreign.Endpoint;",
            ),
            owned().replace(
                &format!("import {PACKAGE}.PayloadRoot;"),
                "import foreign.PayloadRoot;",
            ),
        ] {
            assert!(
                endpoints(&java::parse(&source), &source).is_empty(),
                "{source}"
            );
        }
    }
    #[test]
    fn foreign_annotation_current_source_never_emits_missing_payload_diagnostic() {
        let h = MockHost::install();
        h.add_file("src/main/resources/schema.xsd","<schema xmlns='http://www.w3.org/2001/XMLSchema' targetNamespace='urn:owned'><element name='other'/></schema>");
        let mut p = WsTools::default();
        p.activate(&h.context("aritti.spring-ws")).unwrap();
        let d = Document {
            path: "/project/E.java".into(),
            language: "java".into(),
            version: 1,
            text: owned().replace(PACKAGE, "foreign"),
        };
        assert!(p.diagnostics(&d).is_empty());
        let known = Document {
            text: owned(),
            version: 2,
            ..d.clone()
        };
        assert_eq!(p.diagnostics(&known).len(), 1);
        let foreign = Document { version: 3, ..d };
        assert!(p.diagnostics(&foreign).is_empty());
    }
    #[test]
    fn explicit_java_kotlin_imports_and_fully_qualified_written_owners_are_supported() {
        assert_eq!(endpoints(&java::parse(&owned()), &owned()).len(), 1);
        let qualified = owned()
            .replace("@Endpoint", &format!("@{PACKAGE}.Endpoint"))
            .replace("@PayloadRoot", &format!("@{PACKAGE}.PayloadRoot"))
            .replace(&format!("import {PACKAGE}.Endpoint; "), "")
            .replace(&format!("import {PACKAGE}.PayloadRoot; "), "");
        assert_eq!(endpoints(&java::parse(&qualified), &qualified).len(), 1);
        let kotlin = format!(
            "import {PACKAGE}.Endpoint\nimport {PACKAGE}.PayloadRoot\n@Endpoint class E {{ @PayloadRoot(namespace=\"urn:owned\",localPart=\"owned\") fun handle(){{}} }}"
        );
        assert_eq!(
            endpoints_with_syntax(&parse_source("E.kt", &kotlin), &kotlin, true).len(),
            1
        );
    }
    #[test]
    fn local_generic_typealias_and_aliased_or_conflicting_import_owners_stay_unknown() {
        for source in [
            format!("{} @interface Endpoint {{}}", owned()),
            format!("{} @interface PayloadRoot {{}}", owned()),
            owned().replace("class E", "class E<PayloadRoot>"),
            format!("import foreign.PayloadRoot; {}", owned()),
            owned().replace(
                &format!("import {PACKAGE}.PayloadRoot;"),
                &format!("import {PACKAGE}.*;"),
            ),
        ] {
            assert!(
                endpoints(&java::parse(&source), &source).is_empty(),
                "{source}"
            );
        }
        let kotlin = format!(
            "import {PACKAGE}.Endpoint\nimport {PACKAGE}.PayloadRoot\n@Endpoint class E {{ @PayloadRoot(namespace=\"urn:owned\",localPart=\"owned\") fun handle(){{}} }}"
        );
        for source in [
            kotlin.replace(".Endpoint\n", ".Endpoint as Alias\n"),
            format!("{}\ntypealias Endpoint = foreign.Endpoint", kotlin),
            format!("import foreign.Other as PayloadRoot\n{kotlin}"),
        ] {
            assert!(
                endpoints_with_syntax(&parse_source("E.kt", &source), &source, true).is_empty(),
                "{source}"
            );
        }
        let qualified = owned()
            .replace("@Endpoint", &format!("@{PACKAGE}.Endpoint"))
            .replace("@PayloadRoot", &format!("@{PACKAGE}.PayloadRoot"));
        let local_short = format!("{qualified} @interface PayloadRoot {{}}");
        assert_eq!(endpoints(&java::parse(&local_short), &local_short).len(), 1);
        let root_shadow = format!("class org {{}} {qualified}");
        assert!(endpoints(&java::parse(&root_shadow), &root_shadow).is_empty());
    }
    #[test]
    fn payload_roots_container_and_each_nested_member_require_their_own_written_owner() {
        let source = format!(
            "@{PACKAGE}.Endpoint class E {{ @{PACKAGE}.PayloadRoots({{ @{PACKAGE}.PayloadRoot(namespace=\"urn:a\",localPart=\"owned\"), @foreign.PayloadRoot(namespace=\"urn:b\",localPart=\"foreign\") }}) void handle(){{}} }}"
        );
        let facts = endpoints(&java::parse(&source), &source);
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].local, "owned");
        let foreign_container =
            source.replace(&format!("@{PACKAGE}.PayloadRoots"), "@foreign.PayloadRoots");
        assert!(endpoints(&java::parse(&foreign_container), &foreign_container).is_empty());
        let unimported_inner = source.replace(&format!("@{PACKAGE}.PayloadRoot("), "@PayloadRoot(");
        assert!(endpoints(&java::parse(&unimported_inner), &unimported_inner).is_empty());
        let kotlin = format!(
            "import {PACKAGE}.Endpoint\nimport {PACKAGE}.PayloadRoot\nimport {PACKAGE}.PayloadRoots\n@Endpoint class E {{ @PayloadRoots(PayloadRoot(namespace=\"urn:a\",localPart=\"owned\"), foreign.PayloadRoot(namespace=\"urn:b\",localPart=\"foreign\")) fun handle(){{}} }}"
        );
        let facts = endpoints_with_syntax(&parse_source("E.kt", &kotlin), &kotlin, true);
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].local, "owned");
    }
}

#[cfg(test)]
mod qualified_namespace_shadow_regression {
    use super::*;
    use openidea_plugin::mock::MockHost;
    #[test]
    fn generic_namespace_owner_is_not_the_same_named_top_level_type() {
        let text="class Ns { static final String URI=\"urn:outer\"; } @org.springframework.ws.server.endpoint.annotation.Endpoint class E<Ns> { @org.springframework.ws.server.endpoint.annotation.PayloadRoot(namespace=Ns.URI, localPart=\"missing\") void handle(){} }";
        assert_eq!(endpoints(&java::parse(text),text)[0].namespace,None);
        let host=MockHost::install();
        host.add_file("src/main/resources/schema.xsd", "<schema xmlns='http://www.w3.org/2001/XMLSchema' targetNamespace='urn:outer'><element name='other'/></schema>");
        let mut tools=WsTools::default();
        tools.activate(&host.context("aritti.spring-ws")).unwrap();
        let doc=Document { path:"/project/E.java".into(),language:"java".into(),version:1,text:text.into() };
        assert!(tools.diagnostics(&doc).is_empty());
    }
    #[test]
    fn nested_namespace_owner_shadow_does_not_emit_a_false_live_qname_warning() {
        let host=MockHost::install();
        host.add_file("src/main/resources/schema.xsd", "<schema xmlns='http://www.w3.org/2001/XMLSchema' targetNamespace='urn:outer'><element name='other'/></schema>");
        let mut tools=WsTools::default();
        tools.activate(&host.context("aritti.spring-ws")).unwrap();
        let text="class Ns { static final String URI=\"urn:outer\"; } @org.springframework.ws.server.endpoint.annotation.Endpoint class E { static class Ns { static final String URI=\"urn:inner\"; } @org.springframework.ws.server.endpoint.annotation.PayloadRoot(namespace=Ns.URI, localPart=\"missing\") void handle(){} }";
        let doc=Document { path:"/project/E.java".into(),language:"java".into(),version:1,text:text.into() };
        assert!(tools.diagnostics(&doc).is_empty(), "{:?}",tools.diagnostics(&doc));
        assert_eq!(endpoints(&java::parse(text),text)[0].namespace,None);
    }
}

#[cfg(test)]
mod exact_namespace_provider_regression {
    use super::*;
    use openidea_plugin::mock::MockHost;
    #[test]
    fn registered_definition_never_links_an_unmatched_namespace_to_other_schema() {
        let host=MockHost::install();
        host.add_file("src/main/resources/schema.xsd","<schema xmlns='http://www.w3.org/2001/XMLSchema' targetNamespace='urn:other'><element name='missing'/></schema>");
        let mut tools=WsTools::default();tools.activate(&host.context("aritti.spring-ws")).unwrap();
        for ns in ["\"urn:external\"", "runtime()"] {
            let text=format!("@org.springframework.ws.server.endpoint.annotation.Endpoint class E {{ @org.springframework.ws.server.endpoint.annotation.PayloadRoot(namespace={ns},localPart=\"missing\") void handle(){{}} }}");
            let at=range(&text,text.find("missing").unwrap(),text.find("missing").unwrap()+7).start;
            let doc=Document{path:"/project/E.java".into(),language:"java".into(),version:1,text};
            assert!(tools.definition(&doc,at).is_empty(),"{ns}");
            assert!(tools.completion(&doc,at,None).is_empty(),"{ns}");
            assert!(tools.line_markers(&doc).is_empty(),"{ns}");
        }
    }
}

#[cfg(test)]
mod resource_owner_regression {
    use super::*;
    use openidea_plugin::mock::MockHost;
    #[test]
    fn foreign_classpath_constructor_does_not_publish_spring_resource_diagnostics() {
        let host=MockHost::install();host.add_file("src/main/resources/known.xsd","<schema xmlns='http://www.w3.org/2001/XMLSchema'/>");
        let mut tools=WsTools::default();tools.activate(&host.context("aritti.spring-ws")).unwrap();
        let doc=Document{path:"/project/E.java".into(),language:"java".into(),version:1,text:"import foreign.ClassPathResource; class E { Object value=new ClassPathResource(\"missing.xsd\"); }".into()};
        assert!(tools.diagnostics(&doc).is_empty());
    }
}

#[cfg(test)]
mod source_resource_cache_regression {
    use super::*;
    use openidea_plugin::mock::MockHost;
    #[test]
    fn source_resource_navigation_is_quiet_without_fresh_watched_inventory() {
        let host=MockHost::install();host.add_file("src/main/resources/a.xsd","<schema xmlns='http://www.w3.org/2001/XMLSchema'/>");
        let text="class E { Object x=new org.springframework.core.io.ClassPathResource(\"a.xsd\"); }";
        let doc=Document{path:"/project/E.java".into(),language:"java".into(),version:1,text:text.into()};
        let at=range(text,text.find("a.xsd").unwrap(),text.find("a.xsd").unwrap()+5).start;
        assert!(WsTools::default().definition(&doc,at).is_empty());
        let mut tools=WsTools::default();tools.activate(&host.context("aritti.spring-ws")).unwrap();
        assert_eq!(tools.definition(&doc,at).len(),1);
        host.add_file("src/test/resources/a.xsd","<schema xmlns='http://www.w3.org/2001/XMLSchema'/>");
        // Unobserved files cannot alter a cached request; lifecycle warming can.
        let mut tools=WsTools::default();tools.activate(&host.context("aritti.spring-ws")).unwrap();
        assert!(tools.definition(&doc,at).is_empty());
    }
}

#[cfg(test)]
mod constant_string_type_regression {
    use super::*;
    #[test]
    fn generic_string_field_type_does_not_establish_java_lang_string_constant() {
        let text="@org.springframework.ws.server.endpoint.annotation.Endpoint class E<String> { static final String URI=\"urn:wrong\"; @org.springframework.ws.server.endpoint.annotation.PayloadRoot(namespace=URI,localPart=\"missing\") void handle(){} }";
        assert_eq!(endpoints(&java::parse(text),text)[0].namespace,None);
        let host=openidea_plugin::mock::MockHost::install();
        host.add_file("src/main/resources/schema.xsd","<schema xmlns='http://www.w3.org/2001/XMLSchema' targetNamespace='urn:wrong'><element name='other'/></schema>");
        let mut tools=WsTools::default();tools.activate(&host.context("aritti.spring-ws")).unwrap();
        let doc=Document{path:"/project/E.java".into(),language:"java".into(),version:1,text:text.into()};
        assert!(tools.diagnostics(&doc).is_empty());
        let exact=text.replace("final String", "final java.lang.String");
        assert_eq!(endpoints(&java::parse(&exact),&exact)[0].namespace.as_deref(),Some("urn:wrong"));
    }
}

#[cfg(test)]
mod qualified_string_root_regression {
 use super::*;
 #[test]
 fn generic_java_package_root_does_not_prove_java_lang_string() {
  let text="@org.springframework.ws.server.endpoint.annotation.Endpoint class E<java> { static final java.lang.String URI=\"urn:wrong\"; @org.springframework.ws.server.endpoint.annotation.PayloadRoot(namespace=URI,localPart=\"missing\") void handle(){} }";
  assert_eq!(endpoints(&java::parse(text),text)[0].namespace,None);
 }
}

#[cfg(test)]
mod namespace_property_provider_tests {
 use super::*;
 use openidea_plugin::mock::MockHost;
 fn source(class:&str,value:&str)->String {format!("<b:beans xmlns:b='http://www.springframework.org/schema/beans'><b:bean class='{class}'><b:property name='namespaces'><b:props><b:prop key='p'>{value}</b:prop></b:props></b:property></b:bean></b:beans>")}
 const ADAPTER:&str="org.springframework.ws.server.endpoint.adapter.XPathParamAnnotationMethodEndpointAdapter";
 fn document(text:String)->Document {Document{path:"/project/context.xml".into(),language:"xml".into(),version:1,text}}
 #[test]
 fn exact_xpath_adapter_namespace_property_completes_and_navigates_cached_uri() {
  let host=MockHost::install();host.add_file("src/main/resources/schema.xsd","<schema xmlns='http://www.w3.org/2001/XMLSchema' targetNamespace='urn:owned'><element name='a'/></schema>");
  let mut tools=WsTools::default();tools.activate(&host.context("aritti.spring-ws")).unwrap();
  let doc=document(source(ADAPTER,"urn:owned"));let at=range(&doc.text,doc.text.find("urn:owned").unwrap(),doc.text.find("urn:owned").unwrap()+9).start;
  assert_eq!(tools.definition(&doc,at).len(),1);
  assert_eq!(tools.completion(&doc,at,None).iter().map(|x|x.label.as_str()).collect::<Vec<_>>(),["urn:owned"]);
 }
 #[test]
 fn namespace_property_owners_and_duplicate_prefixes_are_conservative() {
  let host=MockHost::install();host.add_file("src/main/resources/schema.xsd","<schema xmlns='http://www.w3.org/2001/XMLSchema' targetNamespace='urn:owned'><element name='a'/></schema>");
  let mut tools=WsTools::default();tools.activate(&host.context("aritti.spring-ws")).unwrap();
  for text in [source("foreign.XPathParamAnnotationMethodEndpointAdapter","urn:owned"),source(ADAPTER,"${uri}"),source(ADAPTER,"urn:owned").replace("</b:props>","<b:prop key='p'>urn:other</b:prop></b:props>"),source(ADAPTER,"urn:owned").replace("name='namespaces'","name='other'"),source(ADAPTER,"urn:owned").replace("<b:props>","<b:props merge='true'>"),source(ADAPTER,"urn:owned").replace("<b:bean class=","<b:bean parent='x' class=")] {
   let doc=document(text);let at=range(&doc.text,doc.text.find('>').unwrap()+1,doc.text.find('>').unwrap()+1).start;
   assert!(tools.definition(&doc,at).is_empty());
   // Every value caret must stay quiet, including unknown ownership.
   for needle in ["urn:owned","${uri}"] {if let Some(n)=doc.text.find(needle){let at=range(&doc.text,n,n+needle.len()).start;assert!(tools.completion(&doc,at,None).is_empty(),"{}",doc.text);}}
  }
 }
 #[test]
 fn namespace_property_completion_replaces_unsaved_text_with_utf16_ranges() {
  let host=MockHost::install();host.add_file("src/main/resources/schema.xsd","<schema xmlns='http://www.w3.org/2001/XMLSchema' targetNamespace='urn:owned'/>");
  let mut tools=WsTools::default();tools.activate(&host.context("aritti.spring-ws")).unwrap();
  let text=source(ADAPTER,"urn:ow").replace("<b:bean", "<!--🦀-->\r\n<b:bean");let doc=document(text);
  let n=doc.text.find("urn:ow").unwrap();let at=range(&doc.text,n,n+6).end;
  let items=tools.completion(&doc,at,None);assert_eq!(items.len(),1);assert_eq!(items[0].label,"urn:owned");assert_eq!(items[0].replace,Some(range(&doc.text,n,n+6)));
 }
 #[test]
 fn namespace_property_contexts_and_malformed_inputs_are_quiet() {
  let host=MockHost::install();host.add_file("src/main/resources/schema.xsd","<schema xmlns='http://www.w3.org/2001/XMLSchema' targetNamespace='urn:owned'/>");
  let mut tools=WsTools::default();tools.activate(&host.context("aritti.spring-ws")).unwrap();
  for text in [source(ADAPTER,"urn:owned").replace("<b:bean", "<b:import resource='other.xml'/><b:bean"),source(ADAPTER,"urn:owned").replace("<b:beans ","<b:beans profile='dev' "),source(ADAPTER,"urn:owned").replace("</b:prop>","</bad:prop>"),source(ADAPTER,"urn:owned").replace("key='p'","key='xmlns'"),source(ADAPTER,"urn:owned").replace("</b:prop>","<!--hidden--></b:prop>"),source(ADAPTER,"urn:owned").replace("http://www.springframework.org/schema/beans","urn:foreign")] {
   let doc=document(text);let n=doc.text.find("urn:owned").unwrap();let at=range(&doc.text,n,n+9).start;
   assert!(tools.definition(&doc,at).is_empty(),"{}",doc.text);assert!(tools.completion(&doc,at,None).is_empty(),"{}",doc.text);
  }
 }
 #[test]
 fn namespace_property_cold_ambiguous_and_external_roots_withhold_targets() {
  let host=MockHost::install();host.add_file("src/main/resources/schema.xsd","<schema xmlns='http://www.w3.org/2001/XMLSchema' targetNamespace='urn:owned'/>");
  let doc=document(source(ADAPTER,"urn:owned"));let at=range(&doc.text,doc.text.find("urn:owned").unwrap(),doc.text.find("urn:owned").unwrap()+9).start;
  assert!(WsTools::default().completion(&doc,at,None).is_empty());
  host.add_file("src/test/resources/schema.xsd","<schema xmlns='http://www.w3.org/2001/XMLSchema' targetNamespace='urn:owned'/>");
  let mut tools=WsTools::default();tools.activate(&host.context("aritti.spring-ws")).unwrap();
  assert!(tools.definition(&doc,at).is_empty());assert!(tools.completion(&doc,at,None).is_empty());
 }
}
