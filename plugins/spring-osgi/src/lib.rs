//! Spring OSGi (Spring Dynamic Modules) for OpenIDEA: inspections and
//! reference converters, evaluated on the XML text and
//! the project's Java sources (classes by fully qualified name, methods by
//! name and simple parameter types) instead of the IntelliJ Spring model.

use openidea_plugin::diagnostics::DiagnosticsProvider;
use openidea_plugin::file_actions::{FileAction, FileActions};
use openidea_plugin::providers::{CompletionItem, CompletionKind, Document, Providers};
use openidea_plugin::text::offset;
use openidea_plugin::{
    Context, Diagnostic, Error, Event, EventKind, Invocation, Location, PickItem, Plugin, Position, Range, Severity, TextEdit, host, register_plugin,
};
use spring_text::index::{DIAGNOSTICS_WARM, Index, Indexes, PROVIDER_BUDGET};
use spring_text::xml::{self, Attr, Element};
use spring_text::{beans, java, range};

mod hierarchy;
mod bean_facts;
mod ownership;
use ownership::Facts;

const GLOBS: &[&str] = &["**/*.java", "**/*.kt", "**/*.xml"];
const BEANS: &str = "http://www.springframework.org/schema/beans";
const OSGI: &str = "http://www.springframework.org/schema/osgi";
const COMPENDIUM: &str = "http://www.springframework.org/schema/osgi-compendium";
const LIFECYCLE_LISTENER: &str = "org.springframework.osgi.service.importer.OsgiServiceLifecycleListener";

// SpringOsgiBundle.properties
const ILLEGAL_INTERFACES: &str = "It is illegal to use both interface attribute and interfaces element at the same time";
const ILLEGAL_REF_AND_BEAN: &str = "It is illegal to use both ref attribute and bean element at the same time";
const ILLEGAL_COMPARATOR: &str = "It is illegal to use both comparator-ref attribute and comparator element at the same time";
const RETURN_TYPE: &str = "Method must have 'void' return type";
const PUBLIC: &str = "Method must be public";
const PARAMETERS: &str = "Method must have parameters: anyMethodName(ServiceType service, Dictionary properties), anyMethodName(ServiceType service, Map properties), anyMethodName(ServiceReference ref)";

#[derive(Clone, Debug)]
struct Method {
    name: String,
    range: Range,
    public: bool,
    void: bool,
    /// Simple names of the parameter types.
    params: Vec<String>,
    parameter_types: Vec<String>,
}

#[derive(Clone, Debug)]
struct Class {
    fqn: String,
    interface: bool,
    range: Range,
    /// Fully qualified, as far as the file's imports tell.
    supers: Vec<String>,
    unknown_supers: Vec<String>,
    methods: Vec<Method>,
}

#[derive(Clone, Debug)]
struct BeanDecl {
    name: String,
    class: Option<String>,
    range: Range,
}

/// What a project file declares: Java classes, or the beans of a Spring XML file / Java class.
#[derive(Clone, Debug, Default)]
struct Info {
    beans: Vec<BeanDecl>,
    classes: Vec<Class>,
    complete: bool,
    names_complete: bool,
}

fn parse(path: &str, text: &str) -> Option<Info> {
    if path.ends_with(".kt") {
        return Some(Info { complete: true, ..Info::default() });
    }
    if text.len() > 512 * 1024 {
        return Some(Info { complete: false, ..Info::default() });
    }
    if path.ends_with(".java") {
        return Some(java_info(text));
    }
    if beans::ignored(path) || !text.contains("springframework.org/schema/") {
        return None;
    }
    if !xml::analysis_valid(text) { return Some(Info::default()); }
    let els = xml::elements(text);
    if xml::root(&els) != Some("beans") { return None; }
    if xml::namespace(&els,0).is_some_and(|namespace|namespace!=BEANS) { return Some(Info::default()); }
    let mut beans = Vec::new();
    let mut names_complete=!els.iter().enumerate().any(|(i,e)|e.local()=="import" && xml::namespace(&els,i).is_none_or(|ns|ns==BEANS));
    for a in xml::bean_ids(&els) {
        let Some(owner)=xml::element_at(&els,a.at) else {names_complete=false;continue;};
        let global=els[owner].parent.is_some_and(|parent|els[parent].local()=="beans" && xml::namespace(&els,parent).is_none_or(|ns|ns==BEANS));
        if !global {continue;}
        if a.value.contains('&') || beans::is_expression(&a.value) {names_complete=false;continue;}
        let mut ancestor=Some(owner);
        let mut conditional=false;
        while let Some(i)=ancestor {
            conditional|=els[i].local()=="beans" && els[i].attr("profile").is_some();
            ancestor=els[i].parent;
        }
        let class = (!conditional && xml::namespace(&els,owner).is_none_or(|ns|ns==BEANS)).then(||declared_xml_class(&els[owner])).flatten();
        for (name, from, to) in split_names(&a.value, a.value_at) {
            beans.push(BeanDecl { name: name.to_owned(), class: class.clone(), range: range(text, from, to) });
        }
    }
    Some(Info { beans, classes: Vec::new(), complete: true, names_complete })
}

fn declared_xml_class(element:&Element)->Option<String> {
    if element.local()!="bean" || element.attr("factory-method").is_some() || element.attr("factory-bean").is_some() {return None;}
    element.value("class").filter(|class|!class.is_empty() && !class.contains('&') && !beans::is_expression(class)).map(|class|class.replace('$',"."))
}
fn java_info(text: &str) -> Info {
    let f = java::parse(text);
    if !spring_text::text::balanced(text, false) || f.types.len() > 512 || f.types.iter().map(|t| t.members.len()).sum::<usize>() > 4096 {
        return Info::default();
    }
    let classes = f
        .types
        .iter()
        .enumerate()
        .map(|(i, t)| Class {
            fqn: f.qualified(i),
            interface: t.kind == "interface",
            range: range(text, t.at, t.at + t.name.len()),
            supers: t
                .supers()
                .filter_map(|s| hierarchy::qualified(&f, i, s).ok())
                .chain(
                    (t.kind == "class" && t.extends.is_empty() && f.qualified(i) != "java.lang.Object").then(|| "java.lang.Object".into()),
                )
                .collect(),
            unknown_supers: t
                .supers()
                .filter_map(|s| hierarchy::qualified(&f, i, s).err())
                .chain(match t.kind.as_str() {
                    "record" => Some("implicit java.lang.Record hierarchy".into()),
                    "enum" => Some("implicit java.lang.Enum hierarchy".into()),
                    "@interface" => Some("implicit annotation hierarchy".into()),
                    _ => None,
                })
                .collect(),
            methods: t
                .methods()
                .map(|m| Method {
                    name: m.name.clone(),
                    range: range(text, m.at, m.at + m.name.len()),
                    public: t.kind == "interface" || m.modifiers.iter().any(|x| x == "public"),
                    void: m.ty.as_ref().is_some_and(|t| t.name == "void"),
                    params: m.params.iter().map(|p| p.ty.name.clone()).collect(),
                    parameter_types: m
                        .params
                        .iter()
                        .map(|p| {
                            if p.ty.array {
                                "[]".into()
                            } else if matches!(
                                p.ty.name.as_str(),
                                "boolean" | "byte" | "short" | "int" | "long" | "float" | "double" | "char"
                            ) {
                                p.ty.name.clone()
                            } else {
                                hierarchy::qualified(&f, i, &p.ty).unwrap_or_default()
                            }
                        })
                        .collect(),
                })
                .collect(),
        })
        .collect();
    let (beans,names_complete)=bean_facts::java_names(&f,text);
    Info { beans, classes, complete: true, names_complete }
}

/// The names of a `name`/`depends-on` list (`a, b c`) with their byte ranges.
fn split_names(value: &str, at: usize) -> Vec<(&str, usize, usize)> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, c) in value.char_indices().chain([(value.len(), ',')]) {
        if c == ',' || c == ';' || c.is_whitespace() {
            if let Some(s) = start.take() {
                out.push((&value[s..i], at + s, at + i));
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    out
}

fn class<'a>(index: &'a impl Facts, fqn: &str) -> Option<(&'a str, &'a Class)> {
    let mut matches =
        index.iter().flat_map(|(path, info)| info.classes.iter().filter(move |class| class.fqn == fqn).map(move |class| (path, class)));
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

fn named<'a>(index: &'a impl Facts, name: &'a str) -> impl Iterator<Item = (&'a str, &'a BeanDecl)> {
    index.iter().flat_map(move |(p, info)| info.beans.iter().filter(move |b| b.name == name).map(move |b| (p, b)))
}

fn bean_class(index: &impl Facts, name: &str) -> Option<String> {
    if name.is_empty() || beans::is_expression(name) {
        return None;
    }
    let mut declarations = named(index, name);
    let first = declarations.next()?.1;
    if declarations.next().is_some() {
        return None;
    }
    first.class.clone()
}

/// The methods named `name` (any name when `None`) of `fqn` and its project supertypes.
fn methods<'a>(index: &'a impl Facts, fqn: &str, name: Option<&str>) -> Vec<(&'a str, &'a Method)> {
    hierarchy::Graph::new(index).methods(fqn, name)
}

/// `ReferenceListenerMethodConverter.checkProperParameters`, or the
/// registration listener's `(ServiceType, Map|Dictionary)` for known qualified types.
fn proper_parameters(m: &Method, registration: bool) -> Option<bool> {
    let known = |name: &str, accepted: &[&str]| {
        if accepted.contains(&name) {
            Some(true)
        } else if matches!(
            name,
            "[]" | "boolean"
                | "byte"
                | "short"
                | "int"
                | "long"
                | "float"
                | "double"
                | "char"
                | "java.lang.String"
                | "java.lang.Integer"
                | "java.lang.Long"
                | "java.lang.Boolean"
        ) {
            Some(false)
        } else {
            None
        }
    };
    match m.parameter_types.as_slice() {
        [_, properties] => known(properties, &["java.util.Map", "java.util.Dictionary", "java.lang.Object"]),
        [reference] if !registration => known(reference, &["org.osgi.framework.ServiceReference", "java.lang.Object"]),
        _ => Some(false),
    }
}
fn accepts(m: &Method, registration: bool) -> bool {
    m.void && m.public && proper_parameters(m, registration) == Some(true)
}

/// Reject only source-established registration parameter incompatibility. External
/// or incomplete service/parameter types cannot prove an invalid listener.
fn registration_parameter_compatible(graph: &hierarchy::Graph, service: Option<&str>, method: &Method) -> bool {
    let Some(parameter) = method.parameter_types.first() else { return false; };
    if matches!(parameter.as_str(), "[]" | "boolean" | "byte" | "short" | "int" | "long" | "float" | "double" | "char") { return false; }
    let Some(service) = service else { return true; };
    if parameter == "java.lang.Object" || parameter == service { return true; }
    if graph.class(parameter).is_none() || graph.class(service).is_none() { return true; }
    let report = graph.report(service);
    !report.complete || report.types.iter().any(|ty| ty == parameter)
}

/// A Spring DM XML document with scoped core/Beans namespace admission.
struct OsgiXml<'a> {
    text: &'a str,
    els: Vec<Element>,
    core: std::collections::BTreeSet<usize>,
    beans: std::collections::BTreeSet<usize>,
    kids: Vec<Vec<usize>>,
}

impl<'a> OsgiXml<'a> {
    fn new(text: &'a str) -> Option<Self> {
        if text.len()>512*1024 || !text.contains(OSGI) {
            return None;
        }
        let els = xml::elements(text);
        if els.len()>2048 { return None; }
        let prefixes: Vec<String> = els
            .iter()
            .flat_map(|e| &e.attrs)
            .filter(|a| matches!(a.value.trim(), OSGI | COMPENDIUM))
            .filter_map(|a| if a.name == "xmlns" { Some(String::new()) } else { a.name.strip_prefix("xmlns:").map(str::to_owned) })
            .collect();
        if prefixes.is_empty() { return None; }
        let legacy=xml::root(&els)==Some("beans") && xml::namespace(&els,0).is_none();
        let core=els.iter().enumerate().filter(|(i,e)|xml::namespace(&els,*i)==Some(OSGI) && e.name.matches(':').count()<=1 && !e.name.starts_with("xml:") && !e.name.starts_with("xmlns:")).map(|(_,e)|e.at).collect();
        let beans=els.iter().enumerate().filter(|(i,_)|xml::namespace(&els,*i)==Some(BEANS) || legacy && xml::namespace(&els,*i).is_none()).map(|(_,e)|e.at).collect();
        let mut kids = vec![Vec::new(); els.len()];
        for (i, e) in els.iter().enumerate() {
            if let Some(parent) = e.parent {
                kids[parent].push(i);
            }
        }
        Some(Self { text, els, core, beans, kids })
    }

    fn osgi(&self, e: &Element) -> bool { self.core.contains(&e.at) }
    fn value(&self, e: &Element) -> bool { self.osgi(e) || self.beans.contains(&e.at) }
    fn declared_namespace(&self, mut i:usize, prefix:&str) -> Option<&str> {
        let key=format!("xmlns:{prefix}");
        loop {
            if let Some(binding)=self.els[i].attr(&key) {return Some(&binding.value);}
            i=self.els[i].parent?;
        }
    }

    fn children(&self, i: usize) -> impl Iterator<Item = &Element> {
        self.kids[i].iter().map(|&c| &self.els[c])
    }

    fn child(&self, i: usize, local: &str) -> Option<&Element> {
        self.children(i).find(|c| c.local() == local && if local=="bean" {self.beans.contains(&c.at)} else {self.osgi(c)})
    }

    fn name_range(&self, e: &Element) -> (usize, usize) {
        (e.at, e.at + e.name.len())
    }

    /// The trimmed text of an element without children (`<value>a.B</value>`).
    fn content(&self, e: &Element) -> Option<(usize, usize)> {
        let inner = &self.text[e.tag_end.min(e.end)..e.end];
        let close = inner.rfind("</")?;
        let raw = &inner[..close];
        let from = e.tag_end + (raw.len() - raw.trim_start().len());
        Some((from, from + raw.trim().len()))
    }

    /// The service interfaces of a service/reference: its `interface` and `<interfaces>` values.
    fn interfaces(&self, i: usize) -> Vec<(String, usize, usize)> {
        let e = &self.els[i];
        let mut out: Vec<_> = e.attr("interface").map(|a| (a.value.clone(), a.value_at, a.value_end)).into_iter().collect();
        if let Some(list) = self.kids[i].iter().copied().find(|&c| self.els[c].local() == "interfaces" && self.osgi(&self.els[c])) {
            for v in self.children(list).filter(|c| c.local() == "value" && self.value(c)) {
                if let Some((from, to)) = self.content(v) {
                    out.push((self.text[from..to].to_owned(), from, to));
                }
            }
        }
        out.retain(|(v, ..)| !v.is_empty() && !v.contains('&') && !v.chars().any(char::is_whitespace) && !beans::is_expression(v));
        out
    }

    /// The class of a listener's bean (its `ref`, or a nested `<bean class>`) and where it's named.
    fn listener_class(&self, index: &impl Facts, i: usize) -> Option<(String, (usize, usize))> {
        let e = &self.els[i];
        if let Some(r) = e.attr("ref") {
            let class = bean_class(index, &r.value)?;
            return Some((class, (r.value_at, r.value_end)));
        }
        let bean = self.child(i, "bean")?;
        Some((declared_xml_class(bean)?, self.name_range(bean)))
    }
}

enum Target {
    Bean,
    Class,
    /// A listener method of this class (registration listener or reference listener).
    Method(Option<String>, bool),
}

/// The reference under the byte offset `at`: what it names, its text and bytes.
fn target_at(doc: &OsgiXml, index: &impl Facts, at: usize) -> Option<(Target, String, usize, usize)> {
    if let Some(i) = xml::element_at(&doc.els, at) {
        let e = &doc.els[i];
        let a: &Attr = e.attrs.iter().find(|a| a.holds(at))?;
        if !doc.osgi(e) {
            return None;
        }
        let whole = |t| Some((t, a.value.clone(), a.value_at, a.value_end));
        return match (e.local(), a.name.as_str()) {
            ("service" | "listener" | "registration-listener", "ref") | ("list" | "set", "comparator-ref") => whole(Target::Bean),
            (_, "depends-on") => {
                let (name, from, to) =
                    split_names(&a.value, a.value_at).into_iter().find(|&(_, from, to)| from <= at && at <= to).unwrap_or(("", at, at));
                Some((Target::Bean, name.to_owned(), from, to))
            }
            ("service" | "reference" | "list" | "set", "interface") => whole(Target::Class),
            ("listener", "bind-method" | "unbind-method") => whole(Target::Method(doc.listener_class(index, i).map(|c| c.0), false)),
            ("registration-listener", "registration-method" | "unregistration-method") => {
                whole(Target::Method(doc.listener_class(index, i).map(|c| c.0), true))
            }
            _ => None,
        };
    }
    doc.els.iter().find_map(|v| {
        let list = &doc.els[v.parent?];
        if v.local() != "value" || !doc.value(v) || list.local() != "interfaces" || !doc.osgi(list) {
            return None;
        }
        let (from, to) = doc.content(v)?;
        (from <= at && at <= to).then(|| (Target::Class, doc.text[from..to].to_owned(), from, to))
    })
}

fn problem(text: &str, (from, to): (usize, usize), message: impl Into<String>, code: &str) -> Diagnostic {
    Diagnostic { code: Some(code.into()), ..Diagnostic::new(range(text, from, to), Severity::Error, message) }
}

/// The three inspections, plus unresolved listener methods.
fn check(doc: &OsgiXml, index: &impl Facts) -> Vec<Diagnostic> {
    let text = doc.text;
    let graph = hierarchy::Graph::new(index);
    let mut out = Vec::new();
    for (i, e) in doc.els.iter().enumerate().filter(|(_, e)| doc.osgi(e)) {
        let has = |local: &str| doc.child(i, local).is_some();
        let at = doc.name_range(e);
        let inconsistent = |message| problem(text, at, message, "osgi-elements-inconsistency");
        match e.local() {
            "service" | "reference" | "list" | "set" if e.attr("interface").is_some() && has("interfaces") => {
                out.push(inconsistent(ILLEGAL_INTERFACES));
            }
            _ => {}
        }
        check_values(doc, i, &mut out);
        match e.local() {
            "service" => {
                if e.attr("ref").is_some() && has("bean") {
                    out.push(inconsistent(ILLEGAL_REF_AND_BEAN));
                }
                check_service(doc, index, &graph, i, &mut out);
            }
            "list" | "set" if e.attr("comparator-ref").is_some() && has("comparator") => {
                out.push(inconsistent(ILLEGAL_COMPARATOR));
            }
            "listener" if e.parent.is_some_and(|p| doc.osgi(&doc.els[p]) && matches!(doc.els[p].local(), "reference" | "list" | "set")) => {
                check_listener(doc, index, &graph, i, false, &mut out);
            }
            "registration-listener" if e.parent.is_some_and(|p|doc.osgi(&doc.els[p]) && doc.els[p].local()=="service") => check_listener(doc, index, &graph, i, true, &mut out),
            _ => {}
        }
    }
    check_refs(doc, index, &mut out);
    out
}

/// Beans of libraries and of the container are invisible here, so an unknown `ref` is only information.
fn check_refs(doc: &OsgiXml, index: &impl Facts, out: &mut Vec<Diagnostic>) {
    if !index.fresh() || index.iter().any(|(_, info)| !info.complete || !info.names_complete) {
        return;
    }
    for e in doc.els.iter().filter(|e| doc.osgi(e) && matches!(e.local(), "service" | "listener" | "registration-listener")) {
        let Some(a) = e.attr("ref") else { continue };
        let name = a.value.trim();
        if name.is_empty() || name.contains('&') || beans::is_expression(name) || named(index, name).next().is_some() {
            continue;
        }
        let message = format!("No bean named '{name}' was found in the project sources. It may come from a library or be created at runtime.");
        out.push(Diagnostic {
            code: Some("unresolved-osgi-ref".into()),
            ..Diagnostic::new(range(doc.text, a.value_at, a.value_end), Severity::Info, message)
        });
    }
}

fn setting_of(code: &str) -> &'static str {
    match code {
        "osgi-service-common" => "checkService",
        "osgi-listener" => "checkListener",
        "unresolved-osgi-ref" => "checkUnresolvedRef",
        _ => "checkElements",
    }
}

const BOOLEANS: &[&str] = &["true", "false"];

/// The enumerated and numeric attributes of the Spring DM schema: allowed values per element.
fn check_values(doc: &OsgiXml, i: usize, out: &mut Vec<Diagnostic>) {
    let e = &doc.els[i];
    let local = e.local();
    let importer = matches!(local, "reference" | "list" | "set");
    let comparator_basis = local == "natural" && e.parent.is_some_and(|p| doc.osgi(&doc.els[p]) && doc.els[p].local() == "comparator");
    let enumerated: &[(&str, &[&str])] = match local {
        "service" => &[
            ("auto-export", &["disabled", "interfaces", "class-hierarchy", "all-classes"]),
            ("context-class-loader", &["unmanaged", "service-provider"]),
        ],
        "reference" => &[("cardinality", &["0..1", "1..1"]), ("context-class-loader", &["client", "service-provider", "unmanaged"])],
        "list" | "set" => &[
            ("cardinality", &["0..N", "1..N"]),
            ("context-class-loader", &["client", "service-provider", "unmanaged"]),
            ("member-type", &["service-object", "service-reference"]),
        ],
        "natural" if comparator_basis => &[("basis", &["service", "service-reference"])],
        _ => &[],
    };
    let booleans = (importer || local == "service").then_some(("greedy-proxying", BOOLEANS));
    for &(name, allowed) in enumerated.iter().chain(booleans.iter()) {
        let Some(a) = e.attr(name) else {
            continue;
        };
        if a.value.contains("${") || allowed.contains(&a.value.trim()) {
            continue;
        }
        let message = format!("Invalid value '{}' for '{name}': must be one of {}", a.value.trim(), allowed.join(", "));
        out.push(problem(doc.text, (a.value_at, a.value_end), message, "osgi-attribute-value"));
    }
    let numeric: Option<(&str, &str, bool)> = match local {
        "service" => Some(("ranking", "an integer", false)),
        _ if importer => Some(("timeout", "a non-negative integer", true)),
        _ => None,
    };
    if let Some((name, expected, non_negative)) = numeric
        && let Some(a) = e.attr(name)
        && !a.value.contains("${")
    {
        let valid = a.value.trim().parse::<i64>().is_ok_and(|n| !non_negative || n >= 0);
        if !valid {
            let message = format!("Invalid value '{}' for '{name}': must be {expected}", a.value.trim());
            out.push(problem(doc.text, (a.value_at, a.value_end), message, "osgi-attribute-value"));
        }
    }
}

/// `SpringOsgiServiceCommonInspection`: the service bean implements every exported interface.
fn check_service(doc: &OsgiXml, index: &impl Facts, graph: &hierarchy::Graph, i: usize, out: &mut Vec<Diagnostic>) {
    let e = &doc.els[i];
    let mut implementors = Vec::new();
    if let Some(r) = e.attr("ref")
        && let Some(class) = bean_class(index, &r.value)
    {
        implementors.push((class, (r.value_at, r.value_end)));
    }
    if let Some(bean) = doc.child(i, "bean")
        && let Some(class) = declared_xml_class(bean)
    {
        implementors.push((class, doc.name_range(bean)));
    }
    let interfaces = doc.interfaces(i);
    for (class, at) in implementors {
        let report = graph.report(&class);
        let (supers, known) = (report.types, report.complete);
        for (interface, ..) in &interfaces {
            let interface=interface.replace('$',".");
            if known && interface!="java.lang.Object" && graph.class(&interface).is_some() && !supers.contains(&interface) {
                let message = format!("Bean must be inheritor of '{interface}'");
                out.push(problem(doc.text, at, message, "osgi-service-common"));
            }
        }
    }
}

/// `SpringOsgiListenerInspection` (reference listeners) and the method
/// converters (registration listeners: unresolved methods only).
fn check_listener(doc: &OsgiXml, index: &impl Facts, graph: &hierarchy::Graph, i: usize, registration: bool, out: &mut Vec<Diagnostic>) {
    let e = &doc.els[i];
    let Some((class, named_at)) = doc.listener_class(index, i) else {
        return;
    };
    let report = graph.report(&class);
    let (supers, known) = (report.types, report.complete);
    let attrs: &[&str] = if registration { &["registration-method", "unregistration-method"] } else { &["bind-method", "unbind-method"] };
    for a in attrs.iter().filter_map(|n| e.attr(n)).filter(|a| !a.value.is_empty()) {
        let at = (a.value_at, a.value_end);
        let found = graph.methods(&class, Some(&a.value));
        let Some((_, first)) = found.first() else {
            if known {
                let simple = class.rsplit('.').next().unwrap_or(&class);
                out.push(problem(doc.text, at, format!("Cannot resolve method '{}' in '{simple}'", a.value), "osgi-listener"));
            }
            continue;
        };
        if !known
            || registration
            || found.iter().any(|(_, m)| accepts(m, false) || m.void && m.public && proper_parameters(m, false).is_none())
        {
            continue;
        }
        if !first.void {
            out.push(problem(doc.text, at, RETURN_TYPE, "osgi-listener"));
        }
        if !first.public {
            out.push(problem(doc.text, at, PUBLIC, "osgi-listener"));
        }
        if proper_parameters(first, false) == Some(false) {
            out.push(problem(doc.text, at, PARAMETERS, "osgi-listener"));
        }
    }
    // IDEADEV-40586: a listener with both methods needn't implement the interface.
    let both = attrs.iter().all(|n| e.attr(n).is_some_and(|a| !a.value.is_empty() && !graph.methods(&class, Some(&a.value)).is_empty()));
    if !registration && !both && known && !supers.iter().any(|s| s == LIFECYCLE_LISTENER) {
        out.push(problem(doc.text, named_at, format!("Listener must implement '{LIFECYCLE_LISTENER}'"), "osgi-listener"));
    }
}

pub struct SpringOsgi {
    indexes: Indexes<Info>,
    known_roots: Option<Vec<String>>,
    generation: std::collections::BTreeMap<u64, (String, Document, u32)>,
    next_generation: u64,
    project_notice: std::sync::atomic::AtomicBool,
}

impl Default for SpringOsgi {
    fn default() -> Self {
        Self { indexes: Indexes::new(GLOBS, parse), known_roots:None, generation: Default::default(), next_generation: 0, project_notice: Default::default() }
    }
}

impl SpringOsgi {
    fn source_scope(&mut self,path:&str,text:&str)->ownership::Scope {
        let roots=ownership::roots();
        // Only OSGi documents lose cross-file checks; a pom.xml must not raise the notice.
        if roots.is_none() && text.contains(OSGI) && matches!(host::projects(),Err(Error::Denied(_))) {
            ui_kit::denied::notify_once(&self.project_notice,"Service checks across files","project.read","Spring OSGi");
        }
        if roots.is_none() || self.known_roots.as_ref().is_some_and(|old|Some(old)!=roots.as_ref()) {self.indexes.reset();}
        self.known_roots=roots.clone();
        ownership::Scope::new(roots,path)
    }
    /// The OSGi document, the byte offset of `at` and the project index.
    fn open<'d>(&mut self, doc: &'d Document, at: Position) -> Option<(OsgiXml<'d>, usize, ownership::Scoped<'_>)> {
        let osgi = OsgiXml::new(&doc.text)?;
        let at = offset(&doc.text, at)?;
        let scope=self.source_scope(&doc.path, &doc.text);
        if !scope.known() {return None;}
        let index = self.indexes.for_document(&doc.path, &doc.text, PROVIDER_BUDGET)?;
        Some((osgi, at, ownership::Scoped::new(index,&scope)))
    }
}

impl DiagnosticsProvider for SpringOsgi {
    fn diagnostics(&mut self, doc: &Document) -> Vec<Diagnostic> {
        if !doc.path.ends_with(".xml") { return Vec::new(); }
        let scope=self.source_scope(&doc.path, &doc.text);
        if doc.text.len()>512*1024 {
            let _=self.indexes.for_cached_document(&doc.path,&doc.text);
            return Vec::new();
        }
        let mut local = Index::new(GLOBS, parse);
        let index = match self.indexes.for_cached_document_warm(&doc.path,&doc.text,DIAGNOSTICS_WARM) {
            Some(index)=>index,
            None=>{local.update(&doc.path,&doc.text);&mut local}
        };
        let index=ownership::Scoped::new(index,&scope);
        let mut found = Some(&doc.text).filter(|text|xml::analysis_valid(text)).and_then(|text|OsgiXml::new(text)).map(|osgi| check(&osgi, &index)).unwrap_or_default();
        found.retain(|d| host::setting_bool(setting_of(d.code.as_deref().unwrap_or_default())).unwrap_or(true));
        found.truncate(500);
        found
    }
}

impl Providers for SpringOsgi {
    fn definition(&mut self, doc: &Document, at: Position) -> Vec<Location> {
        if doc.language!="xml" || !doc.path.ends_with(".xml") || doc.text.len()>512*1024 || !xml::analysis_valid(&doc.text) {return Vec::new();}
        let Some((osgi, at, index)) = self.open(doc, at) else {
            return Vec::new();
        };
        let Some((target, value, ..)) = target_at(&osgi, &index, at) else {
            return Vec::new();
        };
        let location = |path: &str, range| Location { path: path.into(), range };
        match target {
            Target::Bean => named(&index, &value).map(|(p, b)| location(p, b.range)).collect(),
            Target::Class => class(&index, &value.replace('$', ".")).map(|(p, c)| location(p, c.range)).into_iter().collect(),
            Target::Method(Some(class), _) => methods(&index, &class, Some(&value)).into_iter().map(|(p, m)| location(p, m.range)).collect(),
            Target::Method(None, _) => Vec::new(),
        }
    }

    fn completion(&mut self, doc: &Document, at: Position, _trigger: Option<&str>) -> Vec<CompletionItem> {
        let Some((osgi, at, index)) = self.open(doc, at) else {
            return Vec::new();
        };
        let Some((target, _, from, to)) = target_at(&osgi, &index, at) else {
            return compendium_tags(&osgi, &doc.text, at);
        };
        let replace = Some(range(&doc.text, from, to));
        let graph = hierarchy::Graph::new(&index);
        let exported = exported_interfaces(&osgi, at);
        let registration_service = xml::element_at(&osgi.els, at)
            .filter(|&i| osgi.els[i].local() == "registration-listener" && osgi.osgi(&osgi.els[i]))
            .and_then(|i| osgi.els[i].parent)
            .filter(|&i| osgi.els[i].local() == "service" && osgi.osgi(&osgi.els[i]))
            .and_then(|i| osgi.listener_class(&index, i)).map(|(class, _)| class);
        let mut items: Vec<CompletionItem> = match target {
            Target::Bean => index
                .iter()
                .flat_map(|(_, info)| &info.beans)
                .map(|b| {
                    let simple = b.class.as_deref().map(|c| c.rsplit('.').next().unwrap_or(c).to_owned());
                    let implements = !exported.is_empty()
                        && b.class.as_deref().is_some_and(|c| {
                            let supers = graph.report(c).types;
                            exported.iter().any(|i| supers.contains(i))
                        });
                    let sort = (!exported.is_empty()).then(|| format!("{}{}", if implements { 0 } else { 1 }, b.name));
                    CompletionItem { detail: simple, sort_text: sort, insert_text:escape(&b.name).replace('\'',"&apos;"), ..CompletionItem::new(&b.name, CompletionKind::Reference) }
                })
                .collect(),
            Target::Class => index
                .iter()
                .flat_map(|(_, info)| &info.classes)
                .map(|c| {
                    let kind = if c.interface { CompletionKind::Interface } else { CompletionKind::Class };
                    let sort = format!("{}{}", if c.interface { 0 } else { 1 }, c.fqn);
                    CompletionItem { sort_text: Some(sort), ..CompletionItem::new(&c.fqn, kind) }
                })
                .collect(),
            Target::Method(Some(class), registration) => graph
                .methods(&class, None)
                .into_iter()
                .filter(|(_, m)| accepts(m, registration) && (!registration || registration_parameter_compatible(&graph, registration_service.as_deref(), m)))
                .map(|(_, m)| {
                    let detail = format!("({})", m.params.join(", "));
                    CompletionItem { detail: Some(detail), ..CompletionItem::new(&m.name, CompletionKind::Method) }
                })
                .collect(),
            Target::Method(None, _) => Vec::new(),
        };
        items.sort_by(|a, b| a.label.cmp(&b.label));
        items.dedup_by(|a, b| a.label == b.label);
        for item in &mut items {
            item.replace = replace;
        }
        items
    }
}

/// The interfaces a `<service ref>` under the cursor exports; empty elsewhere.
fn exported_interfaces(doc: &OsgiXml, at: usize) -> Vec<String> {
    let Some(i) = xml::element_at(&doc.els, at) else {
        return Vec::new();
    };
    if !doc.osgi(&doc.els[i]) || doc.els[i].local() != "service" {
        return Vec::new();
    }
    doc.interfaces(i).into_iter().map(|(v, ..)| v.replace('$', ".")).collect()
}

/// Elements of the Spring DM compendium schema, written by hand: (name, attributes worth knowing).
const COMPENDIUM_TAGS: &[(&str, &str)] = &[
    ("cm-properties", "id, persistent-id"),
    ("managed-properties", "persistent-id, update-strategy, update-method"),
    ("managed-service-factory", "id, factory-pid, interface, auto-export"),
];

/// Offers the compendium elements while a `<prefix:name` tag name is being typed.
fn compendium_tags(doc: &OsgiXml, text: &str, at: usize) -> Vec<CompletionItem> {
    let Some(head) = text.get(..at) else {
        return Vec::new();
    };
    let typed = head.rsplit(|c: char| !(c.is_alphanumeric() || matches!(c, ':' | '-' | '_' | '.'))).next().unwrap_or("");
    let from = at - typed.len();
    if !head[..from].ends_with('<') {
        return Vec::new();
    }
    let Some((prefix, _)) = typed.split_once(':') else {
        return Vec::new();
    };
    let compendium = xml::element_at(&doc.els,from).is_some_and(|i|doc.declared_namespace(i,prefix)==Some(COMPENDIUM));
    if !compendium {
        return Vec::new();
    }
    let replace = Some(range(text, from, at));
    COMPENDIUM_TAGS
        .iter()
        .map(|(name, attrs)| CompletionItem {
            detail: Some(attrs.to_string()),
            replace,
            ..CompletionItem::new(&format!("{prefix}:{name}"), CompletionKind::Keyword)
        })
        .collect()
}

impl Plugin for SpringOsgi {
    fn activate(&mut self, _ctx: &Context) -> Result<(), Error> {
        // Event delivery maintains caches only; providers own diagnostic publication.
        let _ = host::subscribe_paths("files-changed", GLOBS);
        for kind in [EventKind::FileOpened, EventKind::FileSaved] { let _ = host::subscribe(kind); }
        Ok(())
    }
    fn handle_events(&mut self, events: &[Event], _ctx: &Invocation) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        self.indexes.on_events(events, deadline);
        let changed: Vec<&str> = events
            .iter()
            .flat_map(|event| match event {
                Event::FileSaved(path) => vec![path.as_str()],
                Event::FilesChanged(batch) => batch.created.iter().chain(&batch.modified).chain(&batch.removed).map(String::as_str).collect(),
                _ => Vec::new(),
            })
            .collect();
        self.generation.retain(|_, (_, document, _)| !changed.contains(&document.path.as_str()));
    }
    fn run_command(&mut self, id: &str, _ctx: &Invocation, _arg: Option<&str>) -> Result<Option<String>, Error> {
        Err(Error::NotFound(format!("command {id}; use the OSGi generation editor intention")))
    }
}

// Generator ownership is independent of the navigation/source cache.
fn generation_owner(doc: &Document) -> Result<String, Error> {
    if doc.language != "xml" || !doc.path.ends_with(".xml") || doc.path.len() > 1024 {
        return Err(Error::Stale);
    }
    spring_text::index::owning_root(&doc.path).ok_or(Error::Stale)
}

fn validate_generation_xml(text: &str) -> Result<(), Error> {
    if xml::generation_valid(text) {
        return Ok(());
    }
    Err("Complete well-formed XML is required for generation.".into())
}

// Only root declarations apply to generated top-level children. Nested or shadowed
// namespace declarations must not authorize generation in a different scope.
fn generation_site(text: &str, at: Position) -> Result<String, Error> {
    if text.len() > 512 * 1024 {
        return Err("Generation is limited to XML buffers up to 512 KiB.".into());
    }
    validate_generation_xml(text)?;
    let els = xml::elements(text);
    let root =
        els.first().filter(|e| e.parent.is_none() && e.local() == "beans").ok_or("Generate OSGi beans only inside a Spring beans root.")?;
    let binding = |name: &str| root.attrs.iter().find(|a| a.name == name).map(|a| a.value.as_str());
    let root_prefix = root.name.rsplit_once(':').map_or("xmlns".to_owned(), |(p, _)| format!("xmlns:{p}"));
    if binding(&root_prefix) != Some("http://www.springframework.org/schema/beans") {
        return Err("The root must use the Spring beans namespace.".into());
    }
    let prefix = root
        .attrs
        .iter()
        .find_map(|a| {
            if a.value != OSGI {
                return None;
            }
            if a.name == "xmlns" {
                Some(String::new())
            } else {
                a.name.strip_prefix("xmlns:").filter(|p| xml_prefix(p)).map(|p| format!("{p}:"))
            }
        })
        .ok_or("Declare the Spring OSGi namespace on the beans root first.")?;
    let pos = offset(text, at).ok_or("The insertion position is no longer valid.")?;
    if els.iter().skip(1).any(|e| e.parent.is_none()) {
        return Err("Use a single beans root.".into());
    }
    let line = text.lines().nth(at.line as usize).unwrap_or("").trim();
    let close = text[..root.end].rfind(&format!("</{}", root.name)).ok_or("Close the beans root before generating a child.")?;
    for (open, close) in [("<!--", "-->"), ("<![CDATA[", "]]>"), ("<?", "?>")] {
        let before = &text[..pos];
        if before.rfind(open).is_some_and(|start| before.rfind(close).is_none_or(|end| start > end)) {
            return Err("Generate outside comments, CDATA and processing instructions.".into());
        }
    }
    let top_line = line.is_empty()
        || text[pos..].trim_start().starts_with(&format!("</{}", root.name))
        || els
            .iter()
            .skip(1)
            .any(|e| e.parent == Some(0) && e.start >= pos && !text[pos..e.start].contains('\n') && text[pos..e.start].trim().is_empty());
    if !top_line || pos < root.tag_end || pos > close || els.iter().skip(1).any(|e| e.start < pos && pos < e.end) {
        return Err("Place the caret on a top-level bean line or a blank line inside beans.".into());
    }
    Ok(prefix)
}

fn xml_prefix(prefix: &str) -> bool {
    let mut chars = prefix.chars();
    !matches!(prefix, "xml" | "xmlns")
        && chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

fn java_fqn(value: &str) -> bool {
    value.split('.').all(|s| {
        let mut c = s.chars();
        c.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
            && c.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
    })
}

fn generation_edit(text: &str, at: Position, kind: &str, interface: &str, target: Option<&str>) -> Result<TextEdit, Error> {
    let prefix = generation_site(text, at)?;
    if !java_fqn(interface) {
        return Err("Use a Java class name without generics, placeholders or XML markup.".into());
    }
    let els = xml::elements(text);
    let element = match kind {
        "service" => {
            let target = target
                .filter(|s| !s.is_empty() && !s.chars().any(char::is_whitespace))
                .ok_or("Enter a nonempty bean reference without whitespace.")?;
            format!("<{prefix}service ref=\"{}\" interface=\"{}\"/>", escape(target), escape(interface))
        }
        "reference" | "list" | "set" => {
            // Imported bean names are not proven locally. Refuse instead of claiming
            // collision freedom for a resource glob, library or dynamic import.
            if els.iter().any(|e| e.local() == "import") {
                return Err("Reference generation cannot prove name uniqueness with imported contexts; add it manually.".into());
            }
            if els.iter().any(|e| {
                e.attrs.iter().any(|a| {
                    matches!(a.name.as_str(), "id" | "name" | "alias") && (a.value.contains('&') || beans::is_expression(&a.value))
                })
            }) {
                return Err("Reference generation cannot prove uniqueness for entity-encoded bean names.".into());
            }
            let simple = interface.rsplit('.').next().unwrap();
            let suffix = match kind {
                "list" => "List",
                "set" => "Set",
                _ => "Reference",
            };
            let id = format!("{}{}{suffix}", simple[..1].to_ascii_lowercase(), &simple[1..]);
            if els.iter().any(|e| {
                e.attrs.iter().any(|a| {
                    matches!(a.name.as_str(), "id" | "name" | "alias") && split_names(&a.value, 0).iter().any(|(name, ..)| *name == id)
                })
            }) {
                return Err(format!("Bean name {id} already exists in this document.").into());
            }
            format!("<{prefix}{kind} id=\"{id}\" interface=\"{}\"/>", escape(interface))
        }
        _ => return Err("Unknown OSGi bean kind.".into()),
    };
    let line = text.lines().nth(at.line as usize).unwrap_or("");
    let indent: String = line.chars().take_while(|c| c.is_whitespace()).collect();
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    Ok(TextEdit { range: Range::new(at, at), text: format!("{indent}{element}{newline}") })
}

fn prompt_generation(text: &str, at: Position) -> Result<Option<TextEdit>, Error> {
    generation_site(text, at)?;
    let items = [
        PickItem::new("service", "OSGi service"),
        PickItem::new("reference", "OSGi reference"),
        PickItem::new("list", "OSGi service reference list"),
        PickItem::new("set", "OSGi service reference set"),
    ];
    let Some(kind) = host::pick("Generate OSGi bean", &items)? else {
        return Ok(None);
    };
    if !matches!(kind.as_str(), "service" | "reference" | "list" | "set") {
        return Err("Unknown OSGi bean kind.".into());
    }
    let Some(interface) = host::input("Interface (fully qualified class name)", None)? else {
        return Ok(None);
    };
    let target = if kind == "service" {
        let Some(target) = host::input("Bean to export (ref)", None)? else {
            return Ok(None);
        };
        Some(target)
    } else {
        None
    };
    generation_edit(text, at, &kind, interface.trim(), target.as_deref().map(str::trim)).map(Some)
}

impl FileActions for SpringOsgi {
    fn file_actions(&mut self, doc: &Document, selection: Range) -> Vec<FileAction> {
        let at = Position::new(selection.start.line, 0);
        if doc.language != "xml" || generation_site(&doc.text, at).is_err() {
            return Vec::new();
        }
        let Ok(root) = generation_owner(doc) else {
            return vec![];
        };
        let Some(next) = self.next_generation.checked_add(1) else {
            return vec![];
        };
        self.next_generation = next;
        if self.generation.len() >= 8 {
            self.generation.clear();
        }
        self.generation.insert(next, (root, doc.clone(), at.line));
        vec![FileAction {
            title: "Generate OSGi service, reference or collection bean…".into(),
            kind: Some("refactor.rewrite".into()),
            data: Some(format!("osgi:{next}")),
            ..FileAction::default()
        }]
    }

    fn resolve_file_action(&mut self, doc: &Document, action: FileAction) -> Result<FileAction, Error> {
        let ticket =
            action.data.as_deref().and_then(|s| s.strip_prefix("osgi:")).and_then(|s| s.parse::<u64>().ok()).ok_or(Error::Stale)?;
        let (root, original, line) = self.generation.get(&ticket).ok_or(Error::Stale)?.clone();
        let current = || -> Result<(), Error> {
            if doc.path != original.path
                || doc.version != original.version
                || doc.text != original.text
                || doc.language != "xml"
                || generation_owner(doc)? != root
            {
                return Err(Error::Stale);
            }
            let project=host::project_of(&doc.path).ok_or(Error::Stale)?;
            if project.read_file(&doc.path)?!=doc.text {return Err(Error::Invalid("Save the file first".into()));}
            Ok(())
        };
        current()?;
        let Some(edit) = prompt_generation(&doc.text, Position::new(line, 0))? else {
            return Err(Error::Cancelled);
        };
        // A resolved single-file action can apply immediately in the host.
        // Show the concrete edit before returning it; lazy list preview has no edits yet.
        if !ui_kit::prompts::confirm_exact("Insert OSGi bean?", &format!("{} at line {}:\n\n{}", doc.path, line + 1, edit.text))? {
            return Err(Error::Cancelled);
        }
        current()?;
        // Return edits only: the host owns atomic application and undo.
        Ok(FileAction { title: action.title, kind: action.kind, edits: vec![(doc.path.clone(), vec![edit])], ..FileAction::default() })
    }
}

fn escape(value: &str) -> String {
    value.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;")
}

register_plugin!(SpringOsgi, providers, diagnostics, file_actions, hover);

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;

    const GREETER: &str = "package com.acme.api;\n\npublic interface Greeter {\n    String greet(String name);\n}\n";
    const IMPL: &str = "package com.acme.impl;\n\nimport com.acme.api.Greeter;\n\npublic class GreeterImpl implements Greeter {\n}\n";
    const PLAIN: &str = "package com.acme.impl;\n\npublic class Plain {\n}\n";
    const LISTENER: &str = r#"package com.acme.impl;

import java.util.Map;
import org.osgi.framework.ServiceReference;

public class GreeterListener {
    public void bound(Greeter g, Map<String, ?> props) {}
    public void unbound(ServiceReference ref) {}
    void hidden(Greeter g, Map props) {}
    public String wrong(Greeter g, Map props) { return null; }
    public void badParams() {}
}
"#;
    const AUDIT: &str = r#"package com.acme.impl;

import org.springframework.stereotype.Component;
@Component
public class AuditListener implements OsgiServiceLifecycleListener {
}
"#;
    const CONTEXT: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<beans xmlns="http://www.springframework.org/schema/beans"
       xmlns:osgi="http://www.springframework.org/schema/osgi">
  <bean id="greeter" class="com.acme.impl.GreeterImpl"/>
  <bean id="plain" class="com.acme.impl.Plain"/>
  <bean id="listener" class="com.acme.impl.GreeterListener"/>

  <osgi:service id="greeterService" ref="greeter" interface="com.acme.api.Greeter">
    <osgi:registration-listener ref="listener" registration-method="bound" unregistration-method="missing"/>
  </osgi:service>
  <osgi:service id="plainService" ref="plain" interface="com.acme.api.Greeter"/>
  <osgi:service ref="greeter" interface="com.acme.api.Greeter">
    <osgi:interfaces><value> com.acme.api.Greeter </value></osgi:interfaces>
    <bean class="com.acme.impl.GreeterImpl"/>
  </osgi:service>
  <osgi:reference id="greeterRef" interface="com.acme.api.Greeter" depends-on="greeter, plain">
    <osgi:listener ref="listener" bind-method="bound" unbind-method="hidden"/>
    <osgi:listener ref="listener" bind-method="wrong"/>
    <osgi:listener ref="auditListener" bind-method="x"/>
  </osgi:reference>
  <osgi:list id="greeters" interface="com.acme.api.Greeter" comparator-ref="cmp">
    <osgi:comparator><osgi:natural basis="service"/></osgi:comparator>
  </osgi:list>
</beans>
"#;
    /// The OSGi namespace as the default one, beans under a prefix.
    const DEFAULT_NS: &str = r#"<beans:beans xmlns="http://www.springframework.org/schema/osgi"
             xmlns:beans="http://www.springframework.org/schema/beans">
  <reference id="r" interface="com.acme.api.Greeter">
    <listener ref="listener" bind-method="bound" unbind-method="unbound"/>
  </reference>
</beans:beans>
"#;
    const ROOT: &str = "/project/src/main/";

    fn setup() -> MockHost {
        let host = MockHost::install();
        host.add_file("src/main/java/com/acme/api/Greeter.java", GREETER);
        host.add_file("src/main/java/com/acme/impl/GreeterImpl.java", IMPL);
        host.add_file("src/main/java/com/acme/impl/Plain.java", PLAIN);
        host.add_file("src/main/java/com/acme/impl/GreeterListener.java", LISTENER);
        host.add_file("src/main/java/com/acme/impl/AuditListener.java", AUDIT);
        host.add_file("src/main/resources/META-INF/spring/context.xml", CONTEXT);
        host.add_file("src/main/resources/META-INF/spring/default.xml", DEFAULT_NS);
        host
    }

    fn doc(name: &str, text: &str) -> Document {
        Document { path: format!("{ROOT}resources/META-INF/spring/{name}"), language: "xml".into(), version: 1, text: text.into() }
    }

    /// The position right after the first `needle` in `text`.
    fn after(text: &str, needle: &str) -> Position {
        openidea_plugin::text::position(text, text.find(needle).unwrap() + needle.len())
    }

    fn warmed_diagnostics(doc: &Document) -> Vec<Diagnostic> {
        let mut plugin = SpringOsgi::default();
        plugin.indexes.for_document(&doc.path, &doc.text, PROVIDER_BUDGET);
        plugin.diagnostics(doc)
    }

    #[test]
    fn diagnostics_survive_file_opened() {
        let host = setup();
        let d = doc("context.xml", CONTEXT);
        let mut plugin = SpringOsgi::default();
        plugin.indexes.for_document(&d.path, &d.text, PROVIDER_BUDGET);
        let before = plugin.diagnostics(&d).len();
        assert!(before >= 8, "{before}");
        plugin.handle_events(&[Event::FileOpened(d.path.clone())], &host.invocation());
        assert_eq!(plugin.diagnostics(&d).len(), before);
        plugin.handle_events(&[Event::FileSaved(format!("{ROOT}java/com/acme/impl/Plain.java"))], &host.invocation());
        assert_eq!(plugin.diagnostics(&d).len(), before);
    }

    #[test]
    fn generation_tickets_clear_only_for_their_own_path() {
        let host = setup();
        let d = doc("context.xml", CONTEXT);
        let other = doc("other.xml", CONTEXT);
        let mut plugin = SpringOsgi::default();
        plugin.generation.insert(1, ("/project".into(), d.clone(), 0));
        plugin.generation.insert(2, ("/project".into(), other.clone(), 0));
        plugin.handle_events(&[Event::FileSaved(d.path.clone())], &host.invocation());
        assert!(!plugin.generation.contains_key(&1));
        assert!(plugin.generation.contains_key(&2));
    }

    #[test]
    fn doctype_and_amp_keep_diagnostics() {
        let _host = setup();
        let text = CONTEXT.replace("<beans ", "<!DOCTYPE beans>\n<beans ").replace("</beans>", "<!-- a &amp; b --><x>a &amp; b</x></beans>");
        let d = doc("doctype.xml", &text);
        assert!(!warmed_diagnostics(&d).is_empty());
        let mut plugin = SpringOsgi::default();
        assert_eq!(plugin.definition(&d, after(&text, "interface=\"com.acme.api.Gre")).len(), 1);
        assert!(validate_generation_xml(&text).is_err());
    }

    #[test]
    fn oversized_unrelated_java_keeps_service_checks() {
        let host = setup();
        host.add_file("src/main/java/com/acme/Big.java", &format!("package com.acme;\nclass Big {{ /* {} */ }}\n", "x".repeat(600 * 1024)));
        let d = doc("context.xml", CONTEXT);
        let mut plugin = SpringOsgi::default();
        plugin.indexes.for_document(&d.path, &d.text, PROVIDER_BUDGET);
        assert!(plugin.diagnostics(&d).iter().any(|p| p.code.as_deref() == Some("osgi-service-common")));
    }

    #[test]
    fn kotlin_project_no_false_unresolved_ref() {
        let host = setup();
        host.add_file("src/main/kotlin/App.kt", "package com.acme\n@Component class Cmp\n");
        let text = CONTEXT.replace("ref=\"auditListener\"", "ref=\"unknownBean\"");
        let d = doc("context.xml", &text);
        let mut plugin = SpringOsgi::default();
        plugin.indexes.for_document(&d.path, &d.text, PROVIDER_BUDGET);
        assert!(plugin.diagnostics(&d).iter().all(|p| p.code.as_deref() != Some("unresolved-osgi-ref")));
    }

    #[test]
    fn foreign_rebound_xml_cannot_borrow_osgi_prefix_or_child_names() {
        let _host=setup(); let mut plugin=SpringOsgi::default();
        for body in [
            "<scope xmlns:osgi='urn:foreign'><osgi:service interface='com.acme.api.Greeter'><osgi:interfaces/></osgi:service></scope>",
            "<osgi:service interface='com.acme.api.Greeter'><foreign:interfaces xmlns:foreign='urn:foreign'/></osgi:service>",
            "<osgi:service ref='greeter'><foreign:bean xmlns:foreign='urn:foreign' class='external.X'/></osgi:service>",
        ] {
            let text=format!("<beans xmlns='http://www.springframework.org/schema/beans' xmlns:osgi='{OSGI}'>{body}</beans>");
            let d=doc("scoped.xml",&text);
            assert!(plugin.diagnostics(&d).is_empty(),"{text}");
        }
    }
    #[test]
    fn scoped_namespace_navigation_values_and_compendium_do_not_borrow_foreign_bindings() {
        use openidea_plugin::hover::HoverProvider;
        let _host=setup(); let mut plugin=SpringOsgi::default();
        for body in [
            "<scope xmlns:osgi='urn:foreign'><osgi:reference interface='com.acme.api.Greeter'/></scope>",
            "<osgi:reference xmlns:f='urn:foreign' f:interface='com.acme.api.Greeter'/>",
            "<osgi:reference><osgi:interfaces><f:value xmlns:f='urn:foreign'>com.acme.api.Greeter</f:value></osgi:interfaces></osgi:reference>",
        ] {
            let text=format!("<beans xmlns='{BEANS}' xmlns:osgi='{OSGI}'><!-- 😀 -->{body}</beans>");
            let d=doc("scope-nav.xml",&text); let at=after(&text,"com.acme.api.Gre");
            assert!(plugin.definition(&d,at).is_empty()); assert!(plugin.completion(&d,at,None).is_empty()); assert!(plugin.hover(&d,at).is_none());
        }
        let text=format!("<beans xmlns='{BEANS}' xmlns:osgi='{OSGI}'><scope><q:reference xmlns:q='{OSGI}' interface='com.acme.api.Greeter'/></scope></beans>");
        let d=doc("local-prefix.xml",&text); assert_eq!(plugin.definition(&d,after(&text,"com.acme.api.Gre")).len(),1);
        let text=format!("<beans xmlns='{BEANS}' xmlns:osgix='{COMPENDIUM}'><scope xmlns:osgix='urn:foreign'><osgix:cm\n</scope></beans>");
        assert!(plugin.completion(&doc("cm-scope.xml",&text),after(&text,"<osgix:cm"),None).is_empty());
        let restored=text.replace(" xmlns:osgix='urn:foreign'","");
        assert_eq!(plugin.completion(&doc("cm-scope.xml",&restored),after(&restored,"<osgix:cm"),None).len(),3);
    }
    #[test]
    fn malformed_or_oversized_xml_removes_current_cached_bean_ownership() {
        let _host=setup(); let mut plugin=SpringOsgi::default();
        let good=format!("<beans xmlns='{BEANS}' xmlns:o='{OSGI}'><bean id='localBean' class='com.acme.impl.Plain'/><o:service ref='localBean' interface='com.acme.api.Greeter'/></beans>");
        let d=doc("current.xml",&good); plugin.indexes.for_document(&d.path,&d.text,PROVIDER_BUDGET);
        assert!(bean_class(plugin.indexes.for_cached_document(&d.path,&d.text).unwrap(),"localBean").is_some());
        for text in [
            good.replace("xmlns:o=", "xmlns:xml="),
            good.replace("<bean", "<unbound:bean"),
            good.replace("<bean", "<?xml version='1.0'?><bean"),
            "x".repeat(512*1024+1),
        ] {
            let invalid=Document {text,version:d.version+1,..d.clone()}; assert!(plugin.diagnostics(&invalid).is_empty());
            let index=plugin.indexes.for_cached_document(&invalid.path,&invalid.text).unwrap();
            assert!(bean_class(index,"localBean").is_none()); assert!(index.iter().any(|(_,info)|!info.complete));
            plugin.diagnostics(&d);
            assert!(bean_class(plugin.indexes.for_cached_document(&d.path,&d.text).unwrap(),"localBean").is_some());
        }
    }
    #[test]
    fn foreign_java_stereotype_cannot_manufacture_an_osgi_bean_type_error() {
        let host=MockHost::install();
        host.add_file("Fake.java","package app; @foreign.Component(\"fake\") class Fake {} ");
        host.add_file("Api.java","package external; interface Api {} ");
        let text=format!("<beans xmlns='{BEANS}' xmlns:o='{OSGI}'><o:service ref='fake' interface='external.Api'/></beans>");
        let d=doc("foreign-bean.xml",&text); let mut plugin=SpringOsgi::default();
        let at=after(&text,"ref='fa"); plugin.completion(&d,at,None);
        assert!(plugin.diagnostics(&d).iter().all(|d|d.code.as_deref()!=Some("osgi-service-common")));
        assert!(plugin.definition(&d,at).is_empty());
    }
    #[test]
    fn java_bean_facts_require_owned_literal_metadata_without_poisoning_class_facts() {
        for text in [
            "package app; import org.springframework.stereotype.Component; @Component(\"named\") class Item {}",
            "package app; @org.springframework.stereotype.Service(\"named\") class Item {}",
            "package app; import org.springframework.context.annotation.Bean; class Config { @Bean(name={\"named\",\"alias\"}) Item make(){return null;} } class Item {}",
        ] {
            let info=java_info(text); assert!(info.names_complete); assert!(info.beans.iter().any(|b|b.name=="named"));
        }
        for text in [
            "package app; @Component(\"named\") class Item {}",
            "package app; import other.Component; @Component(\"named\") class Item {}",
            "package app; import org.springframework.stereotype.Component; @other.Component(\"named\") class Item {}",
            "package app; import org.springframework.stereotype.Component; class Component {} @Component(\"named\") class Item {}",
            "package app; import org.springframework.stereotype.Component; @Component(NAME) class Item {}",
            "package app; import org.springframework.stereotype.Component; @Component(\"na\"+\"med\") class Item {}",
            "package app; import org.springframework.context.annotation.Bean; class Config { @Bean(name={\"named\"+SUFFIX}) Item make(){return null;} } class Item {}",
            "package app; import org.springframework.context.annotation.Bean; class Config { @Bean(name=\"named\",value=\"alias\") Item make(){return null;} } class Item {}",
            "package app; import org.springframework.stereotype.Component; class Outer { @Component class Item {} }",
        ] {
            let info=java_info(text); assert!(info.complete,"class source remains admitted"); assert!(!info.names_complete,"{text}"); assert!(info.beans.is_empty(),"{text}"); assert!(!info.classes.is_empty());
        }
        let advice=java_info("package app; import org.springframework.web.bind.annotation.ControllerAdvice; @ControllerAdvice(\"app.controllers\") class Advice {}");
        assert_eq!(advice.beans[0].name,"advice","advice value identifies packages, not bean name");
        let conditional=java_info("package app; import org.springframework.stereotype.Component; @Component @Profile(\"dev\") class Item {}");
        assert_eq!(conditional.beans[0].name,"item"); assert!(conditional.beans[0].class.is_none());
        let host=MockHost::install(); host.add_file("Named.java","package app; @org.springframework.stereotype.Component(\"a&b'c<d\") class Named {}");
        let text=format!("<beans xmlns='{BEANS}' xmlns:o='{OSGI}'><o:service ref=''/></beans>");
        let d=doc("escaped-completion.xml",&text); let mut plugin=SpringOsgi::default();
        let item=plugin.completion(&d,after(&text,"ref='"),None).into_iter().find(|item|item.label=="a&b'c<d").unwrap();
        assert_eq!(item.insert_text,"a&amp;b&apos;c&lt;d");
        let updated=text.replace("ref=''",&format!("ref='{}'",item.insert_text));
        validate_generation_xml(&updated).unwrap(); assert!(plugin.diagnostics(&doc("escaped-completion.xml",&updated)).is_empty());
    }
    #[test]
    fn inner_dynamic_factory_and_profile_xml_names_do_not_establish_global_bean_types() {
        let text=format!("<beans xmlns='{BEANS}'><bean id='outer' class='app.Outer'><property name='x'><bean id='inner' class='app.Inner'/></property></bean><bean id='${{computed}}' class='app.Dynamic'/><bean id='encoded&#65;' class='app.Encoded'/><bean id='factory' class='app.Factory' factory-method='create'/><beans profile='dev'><bean id='conditional' class='app.Conditional'/></beans></beans>");
        let info=parse("/project/source.xml",&text).unwrap(); assert!(info.complete); assert!(!info.names_complete);
        assert!(info.beans.iter().all(|b|!matches!(b.name.as_str(),"inner"|"${computed}"|"encoded&#65;")));
        assert_eq!(info.beans.iter().find(|b|b.name=="outer").unwrap().class.as_deref(),Some("app.Outer"));
        for name in ["factory","conditional"] {assert!(info.beans.iter().find(|b|b.name==name).unwrap().class.is_none());}
    }
    #[test]
    fn unresolved_exported_types_and_inline_factories_remain_unknown() {
        let host=MockHost::install();
        host.add_file("Plain.java","package app; class Plain {} class Factory {} interface Api {} class Container {interface Nested {}} class Implements implements Container.Nested {}");
        let mut plugin=SpringOsgi::default();
        for body in [
            "<bean id='plain' class='app.Plain'/><o:service ref='plain' interface='vendor.Unresolved'/>",
            "<o:service interface='app.Api'><bean class='app.Factory' factory-method='make'/></o:service>",
            "<o:reference interface='app.Api'><o:listener bind-method='missing'><bean class='app.Factory' factory-method='make'/></o:listener></o:reference>",
        ] {
            let text=format!("<beans xmlns='{BEANS}' xmlns:o='{OSGI}'>{body}</beans>");
            let d=doc("unknown-types.xml",&text); plugin.completion(&d,after(&text,"<beans"),None);
            assert!(plugin.diagnostics(&d).is_empty(),"{text}");
        }
        let text=format!("<beans xmlns='{BEANS}' xmlns:o='{OSGI}'><bean id='plain' class='app.Plain'/><o:service ref='plain' interface='app.Api'/></beans>");
        assert!(plugin.diagnostics(&doc("unknown-types.xml",&text)).iter().any(|d|d.code.as_deref()==Some("osgi-service-common")));
        for (class,expected) in [("app.Implements",false),("app.Plain",true)] {
            let text=format!("<beans xmlns='{BEANS}' xmlns:o='{OSGI}'><bean id='impl' class='{class}'/><o:service ref='impl' interface='app.Container$Nested'/></beans>");
            assert_eq!(plugin.diagnostics(&doc("unknown-types.xml",&text)).iter().any(|d|d.code.as_deref()==Some("osgi-service-common")),expected,"source canonical nested type ownership");
        }
    }
    #[test]
    fn cached_external_hierarchies_remain_unknown() {
        let host = setup();
        host.add_file("src/main/java/com/acme/External.java", "package com.acme; public class External extends vendor.Base {}");
        let text = r#"<beans xmlns="http://www.springframework.org/schema/beans" xmlns:osgi="http://www.springframework.org/schema/osgi"><bean id="external" class="com.acme.External"/><osgi:service ref="external" interface="vendor.Api"/><osgi:reference interface="vendor.Api"><osgi:listener ref="external" bind-method="fromLibrary"/></osgi:reference></beans>"#;
        assert!(warmed_diagnostics(&doc("external.xml", text)).is_empty());
        assert!(host.diagnostics().is_empty());
    }

    #[test]
    fn diagnostics_skip_oversized_or_other_document_types() {
        let _host = setup();
        let mut plugin = SpringOsgi::default();
        let large = "x".repeat(512 * 1024 + 1);
        assert!(plugin.diagnostics(&doc("large.xml", &large)).is_empty());
        assert!(plugin.diagnostics(&doc("other.txt", CONTEXT)).is_empty());
    }

    #[test]
    fn live_unsaved_local_checks_survive_denial_and_clear_after_fix() {
        for caps in [vec!["project.read"], vec![]] {
            let host = MockHost::install().with_capabilities(&caps);
            let bad = r#"<beans xmlns:osgi="http://www.springframework.org/schema/osgi"><osgi:service ref="missing" auto-export="bad" interface="external.Api"/><osgi:reference interface="external.Api"><osgi:listener><bean class="external.Listener"/></osgi:listener></osgi:reference></beans>"#;
            let path = doc("unsaved.xml", bad).path;
            host.add_file("resources/META-INF/spring/unsaved.xml", "<beans/>");
            host.open_editor(&path, "xml", bad, &[]);
            let mut plugin = SpringOsgi::default();
            host.refresh_diagnostics(&mut plugin);
            let problems = host.provider_diagnostics();
            let problems = &problems[&path];
            let unresolved = problems.iter().filter(|p| p.code.as_deref() == Some("unresolved-osgi-ref")).count();
            assert_eq!(unresolved, usize::from(!caps.is_empty()), "a cold cache leaves missing refs unknown: {problems:?}");
            assert_eq!(problems.len(), 1 + unresolved);
            assert!(problems.iter().any(|p| p.code.as_deref() == Some("osgi-attribute-value")));
            assert!(host.diagnostics().is_empty());
            host.open_editor(&path, "xml", &bad.replace("auto-export=\"bad\"", "auto-export=\"disabled\""), &[]);
            host.refresh_diagnostics(&mut plugin);
            let remaining = host.provider_diagnostics();
            assert!(remaining.values().flatten().all(|p| p.code.as_deref() == Some("unresolved-osgi-ref")));
        }
    }

    #[test]
    fn service_check_notice_shown_once() {
        let host = MockHost::install().with_capabilities(&[]);
        let mut plugin = SpringOsgi::default();
        plugin.diagnostics(&doc("plain.xml", "<beans/>"));
        assert!(host.notifications().is_empty(), "non-OSGi XML stays quiet");
        let osgi = format!("<beans xmlns:osgi=\"{OSGI}\"/>");
        plugin.diagnostics(&doc("osgi.xml", &osgi));
        plugin.diagnostics(&doc("osgi.xml", &osgi));
        let notices: Vec<_> = host.notifications().into_iter().filter(|n| n.body.contains("needs the project.read permission")).collect();
        assert_eq!(notices.len(), 1);
        assert!(notices[0].body.contains("Settings › Plugins › Spring OSGi"));
    }

    #[test]
    fn reports_the_original_inspections() {
        let _host = setup();
        let problems = warmed_diagnostics(&doc("context.xml", CONTEXT));
        let found: Vec<_> = problems
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .map(|d| {
                let r = d.range;
                let line = CONTEXT.lines().nth(r.start.line as usize).unwrap();
                let at = line[r.start.character as usize..r.end.character as usize].to_owned();
                (at, d.message.as_str())
            })
            .collect();
        assert_eq!(
            found,
            [
                ("missing".to_owned(), "Cannot resolve method 'missing' in 'GreeterListener'"),
                ("plain".into(), "Bean must be inheritor of 'com.acme.api.Greeter'"),
                ("osgi:service".into(), ILLEGAL_INTERFACES),
                ("osgi:service".into(), ILLEGAL_REF_AND_BEAN),
                ("hidden".into(), PUBLIC),
                ("wrong".into(), RETURN_TYPE),
                ("listener".into(), "Listener must implement 'org.springframework.osgi.service.importer.OsgiServiceLifecycleListener'"),
                ("osgi:list".into(), ILLEGAL_COMPARATOR),
            ]
        );
        assert!(problems.iter().all(|d| d.severity == Severity::Error));
    }

    #[test]
    fn unresolved_ref_check_can_be_disabled() {
        let host = setup();
        let text = "<beans xmlns=\"http://www.springframework.org/schema/beans\"\n       xmlns:osgi=\"http://www.springframework.org/schema/osgi\">\n<osgi:service ref=\"ghost\"/>\n</beans>\n";
        assert_eq!(warmed_diagnostics(&doc("refs.xml", text)).len(), 1);

        host.set_setting("checkUnresolvedRef", "false");

        assert!(warmed_diagnostics(&doc("refs.xml", text)).is_empty());
    }

    #[test]
    fn unresolved_ref_is_quiet_for_known_and_dynamic_names() {
        let _host = setup();
        let text = "<beans xmlns=\"http://www.springframework.org/schema/beans\"\n       xmlns:osgi=\"http://www.springframework.org/schema/osgi\">\n<osgi:service ref=\"greeter\"/>\n<osgi:service ref=\"${bean}\"/>\n<osgi:service ref=\"ghost\"/>\n</beans>\n";
        let found = warmed_diagnostics(&doc("refs.xml", text));
        let codes: Vec<_> = found.iter().map(|d| (d.code.as_deref().unwrap(), d.severity, d.range.start.line)).collect();
        assert_eq!(codes, [("unresolved-osgi-ref", Severity::Info, 4)]);
    }

    #[test]
    fn service_ref_completion_puts_implementors_first() {
        let _host = setup();
        let mut p = SpringOsgi::default();
        let d = doc("context.xml", CONTEXT);
        let items = p.completion(&d, after(CONTEXT, "<osgi:service id=\"plainService\" ref=\"pl"), None);
        let sort = |label: &str| items.iter().find(|i| i.label == label).unwrap().sort_text.clone().unwrap();
        assert_eq!(sort("greeter"), "0greeter");
        assert_eq!(sort("plain"), "1plain");
    }

    #[test]
    fn completes_compendium_tags_only_when_declared() {
        let _host = setup();
        let mut p = SpringOsgi::default();
        let text = "<beans xmlns=\"http://www.springframework.org/schema/beans\"\n       xmlns:osgi=\"http://www.springframework.org/schema/osgi\"\n       xmlns:osgix=\"http://www.springframework.org/schema/osgi-compendium\">\n<osgix:cm\n</beans>\n";
        let d = doc("cm.xml", text);
        let items = p.completion(&d, after(text, "<osgix:cm"), None);
        assert_eq!(
            items.iter().map(|i| i.label.as_str()).collect::<Vec<_>>(),
            ["osgix:cm-properties", "osgix:managed-properties", "osgix:managed-service-factory"]
        );
        assert_eq!(items[0].replace.unwrap().start, after(text, "\n<"));
        let undeclared = text.replace("xmlns:osgix=", "xmlns:other=");
        assert!(p.completion(&doc("cm2.xml", &undeclared), after(&undeclared, "<osgix:cm"), None).is_empty());
    }

    #[test]
    fn badly_shaped_parameters_are_reported() {
        let _host = setup();
        let text = DEFAULT_NS.replace("unbind-method=\"unbound\"", "unbind-method=\"badParams\"");
        let problems = warmed_diagnostics(&doc("default.xml", &text));
        assert_eq!(problems.iter().map(|d| d.message.as_str()).collect::<Vec<_>>(), [PARAMETERS]);
    }

    #[test]
    fn navigates_from_references() {
        let _host = setup();
        let mut p = SpringOsgi::default();
        let d = doc("default.xml", DEFAULT_NS);
        let listener = p.definition(&d, after(DEFAULT_NS, "ref=\"list"));
        assert_eq!(listener.len(), 1);
        assert!(listener[0].path.ends_with("context.xml"));
        assert_eq!(listener[0].range.start, Position::new(5, 12));

        let bound = p.definition(&d, after(DEFAULT_NS, "bind-method=\"bo"));
        assert!(bound[0].path.ends_with("GreeterListener.java"));
        assert_eq!(bound[0].range.start, Position::new(6, 16));

        let interface = p.definition(&d, after(DEFAULT_NS, "interface=\"com"));
        assert!(interface[0].path.ends_with("Greeter.java"));

        let d = doc("context.xml", CONTEXT);
        let value = p.definition(&d, after(CONTEXT, "<value> com.acme"));
        assert!(value[0].path.ends_with("Greeter.java"));
        let plain = p.definition(&d, after(CONTEXT, "depends-on=\"greeter, pl"));
        assert_eq!(plain[0].range.start, Position::new(4, 12));
        assert!(p.definition(&d, after(CONTEXT, "<bean id=\"gr")).is_empty(), "not an OSGi reference");
    }

    #[test]
    fn completes_beans_interfaces_and_listener_methods() {
        let _host = setup();
        let mut p = SpringOsgi::default();
        let d = doc("default.xml", DEFAULT_NS);
        let labels = |items: Vec<CompletionItem>| items.into_iter().map(|i| i.label).collect::<Vec<_>>();
        assert_eq!(labels(p.completion(&d, after(DEFAULT_NS, "bind-method=\"bo"), None)), ["bound", "unbound"]);
        assert_eq!(
            labels(p.completion(&d, after(DEFAULT_NS, "ref=\"list"), None)),
            ["auditListener", "greeter", "greeterRef", "greeterService", "greeters", "listener", "plain", "plainService", "r"]
        );
        let classes = p.completion(&d, after(DEFAULT_NS, "interface=\"com"), None);
        let greeter = classes.iter().find(|c| c.label == "com.acme.api.Greeter").unwrap();
        assert_eq!(greeter.kind, Some(CompletionKind::Interface));
        assert_eq!(greeter.replace.unwrap().start, after(DEFAULT_NS, "interface=\""));

        let d = doc("context.xml", CONTEXT);
        let registration = p.completion(&d, after(CONTEXT, "unregistration-method=\"mi"), None);
        assert_eq!(labels(registration), ["bound"], "(ServiceType, Map) only");
    }

    fn values(name: &str, body: &str) -> Vec<(String, String)> {
        let _host = setup();
        let text = format!(
            "<beans xmlns=\"http://www.springframework.org/schema/beans\"\n       xmlns:osgi=\"http://www.springframework.org/schema/osgi\">\n{body}\n</beans>\n"
        );
        let found = warmed_diagnostics(&doc(name, &text));
        found
            .iter()
            .filter(|d| d.code.as_deref() == Some("osgi-attribute-value") && d.severity == Severity::Error)
            .map(|d| {
                let line = text.lines().nth(d.range.start.line as usize).unwrap();
                let at = line[d.range.start.character as usize..d.range.end.character as usize].to_owned();
                (at, d.message.clone())
            })
            .collect()
    }

    #[test]
    fn wrong_cardinality_is_reported() {
        let found = values("c1.xml", r#"<osgi:reference interface="a.B" cardinality="0..N"/>"#);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, "0..N");
        assert!(found[0].1.contains("0..1") && found[0].1.contains("1..1"), "{}", found[0].1);
    }

    #[test]
    fn cardinality_for_list_and_reference_differ() {
        let found = values(
            "c2.xml",
            r#"<osgi:reference interface="a.B" cardinality="1..1"/>
<osgi:list interface="a.B" cardinality="1..N"/>
<osgi:set interface="a.B" cardinality="0..N"/>
<osgi:list interface="a.B" cardinality="1..1"/>
<osgi:set interface="a.B" cardinality="0..1"/>"#,
        );
        assert_eq!(found.iter().map(|f| f.0.as_str()).collect::<Vec<_>>(), ["1..1", "0..1"]);
        assert!(found[0].1.contains("0..N") && found[0].1.contains("1..N"));
    }

    #[test]
    fn wrong_auto_export_is_reported() {
        let found = values("a.xml", r#"<osgi:service ref="x" auto-export="everything"/>"#);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, "everything");
        for allowed in ["disabled", "interfaces", "class-hierarchy", "all-classes"] {
            assert!(found[0].1.contains(allowed), "{}", found[0].1);
        }
    }

    #[test]
    fn wrong_context_class_loader_is_reported() {
        let found = values(
            "l.xml",
            r#"<osgi:service ref="x" context-class-loader="client"/>
<osgi:reference interface="a.B" context-class-loader="client"/>
<osgi:list interface="a.B" context-class-loader="bogus"/>"#,
        );
        assert_eq!(found.iter().map(|f| f.0.as_str()).collect::<Vec<_>>(), ["client", "bogus"]);
        assert!(!found[0].1.contains("client,") && found[0].1.contains("unmanaged") && found[0].1.contains("service-provider"));
        assert!(found[1].1.contains("client"));
    }

    #[test]
    fn non_integer_ranking_is_reported() {
        let found = values(
            "r.xml",
            r#"<osgi:service ref="x" ranking="high"/>
<osgi:service ref="x" ranking="-3"/>
<osgi:reference interface="a.B" timeout="-1"/>
<osgi:reference interface="a.B" timeout="soon"/>
<osgi:list interface="a.B" timeout="5"/>"#,
        );
        assert_eq!(found.iter().map(|f| f.0.as_str()).collect::<Vec<_>>(), ["high", "-1", "soon"]);
    }

    #[test]
    fn wrong_member_type_is_reported() {
        let found = values(
            "m.xml",
            r#"<osgi:list interface="a.B" member-type="service-object"/>
<osgi:set interface="a.B" member-type="proxy"/>"#,
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, "proxy");
        assert!(found[0].1.contains("service-object") && found[0].1.contains("service-reference"));
    }

    #[test]
    fn wrong_ordering_basis_is_reported() {
        let found = values(
            "b.xml",
            r#"<osgi:list interface="a.B"><osgi:comparator><osgi:natural basis="service-reference"/></osgi:comparator></osgi:list>
<osgi:list interface="a.B"><osgi:comparator><osgi:natural basis="object"/></osgi:comparator></osgi:list>"#,
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, "object");
        assert!(found[0].1.contains("service") && found[0].1.contains("service-reference"));
    }

    #[test]
    fn attribute_values_are_quiet_for_library_or_dynamic_name() {
        let found = values(
            "q.xml",
            r#"<osgi:service ref="x" auto-export="all-classes" ranking="${rank}" context-class-loader="${l}"/>
<osgi:reference interface="a.B" cardinality="${card}" timeout="${t}" greedy-proxying="${g}"/>
<osgi:list interface="a.B" member-type="service-reference" greedy-proxying="true"/>
<foo:service xmlns:foo="urn:other" auto-export="nonsense" ranking="x"/>
<bean id="b" class="a.B" cardinality="nonsense"/>"#,
        );
        assert!(found.is_empty(), "{found:?}");
        let _host = setup();
        let text = DEFAULT_NS.replace("interface=", "cardinality=\"bad\" greedy-proxying=\"maybe\" interface=");
        let found = warmed_diagnostics(&doc("default.xml", &text));
        let at: Vec<_> = found
            .iter()
            .map(|d| {
                &text.lines().nth(d.range.start.line as usize).unwrap()[d.range.start.character as usize..d.range.end.character as usize]
            })
            .collect();
        assert_eq!(at, ["bad", "maybe"]);
        let _host = setup();
        assert!(warmed_diagnostics(&doc("default.xml", DEFAULT_NS)).is_empty());
    }

    #[test]
    fn value_ranges_use_utf16_columns_with_multibyte_text() {
        let found = {
            let _host = setup();
            let text = "<beans xmlns:osgi=\"http://www.springframework.org/schema/osgi\"><!-- \u{1F600}\u{e9} --><osgi:service ref=\"greeter\" auto-export=\"bad\"/></beans>";
            let d = warmed_diagnostics(&doc("u.xml", text));
            let prefix = text.find("bad").unwrap();
            let utf16 = text[..prefix].encode_utf16().count() as u32;
            (d, utf16)
        };
        assert_eq!(found.0.len(), 1);
        assert_eq!((found.0[0].range.start.character, found.0[0].range.end.character), (found.1, found.1 + 3));
    }

    #[test]
    fn value_lines_are_right_in_crlf_files() {
        let _host = setup();
        let text = "<beans xmlns:osgi=\"http://www.springframework.org/schema/osgi\">\r\n<osgi:service ref=\"greeter\"\r\n    auto-export=\"bad\"/>\r\n</beans>\r\n";
        let d = warmed_diagnostics(&doc("crlf.xml", text));
        assert_eq!(d.len(), 1);
        assert_eq!((d[0].range.start.line, d[0].range.start.character, d[0].range.end.character), (2, 17, 20));
    }

    #[test]
    fn ignores_other_xml() {
        let _host = setup();
        let mut p = SpringOsgi::default();
        let beans = r#"<beans xmlns="http://www.springframework.org/schema/beans"><bean id="a" ref="b"/></beans>"#;
        assert!(p.diagnostics(&doc("plain.xml", beans)).is_empty());
        assert!(p.completion(&doc("plain.xml", beans), after(beans, "ref=\""), None).is_empty());
    }

    #[test]
    fn optional_event_grant_admits_subscriptions_and_denial_stays_quiet() {
        assert!(include_str!("../plugin.toml").contains("optional = [\"project.read\", \"events\"]"));
        for caps in [vec!["events"],vec![]] {
            let host=MockHost::install().with_capabilities(&caps);
            let mut plugin=SpringOsgi::default();
            plugin.activate(&host.context("aritti.spring-osgi")).unwrap();
            assert_eq!(host.subscriptions().contains(&EventKind::FilesChanged),!caps.is_empty());
            assert_eq!(host.subscriptions().contains(&EventKind::FileOpened),!caps.is_empty());
            assert_eq!(host.subscriptions().contains(&EventKind::FileSaved),!caps.is_empty());
            assert_eq!(host.path_subscriptions().get("files-changed").is_some(),!caps.is_empty());
            assert!(plugin.diagnostics(&doc("plain.xml","<beans/>")).is_empty());
            assert!(host.process_runs().is_empty() && host.requests().is_empty());
        }
    }

    #[test]
    fn empty_or_unresolved_listener_methods_do_not_bypass_lifecycle_contract() {
        for methods in ["bind-method=\"\" unbind-method=\"\"", "bind-method=\"missing\" unbind-method=\"absent\""] {
            let host=setup();
            let text=format!("<beans xmlns=\"http://www.springframework.org/schema/beans\" xmlns:o=\"http://www.springframework.org/schema/osgi\"><bean id=\"onlyListener\" class=\"com.acme.impl.GreeterListener\"/><o:reference interface=\"com.acme.api.Greeter\"><o:listener ref=\"onlyListener\" {methods}/></o:reference></beans>");
            let d=doc("listener-contract.xml",&text);
            let mut plugin=SpringOsgi::default();
            plugin.completion(&d,after(&text,"ref=\""),None);
            assert!(plugin.diagnostics(&d).iter().any(|p|p.message.contains("Listener must implement")),"{methods}");
            assert!(host.process_runs().is_empty());
        }
    }

    #[test]
    fn lifecycle_events_update_only_the_changed_path() {
        let host = setup();
        let d = doc("context.xml", CONTEXT);
        let mut plugin = SpringOsgi::default();
        plugin.completion(&d, after(CONTEXT, "ref=\""), None);
        let has = |plugin: &mut SpringOsgi, fqn: &str| class(plugin.indexes.for_cached_document(&d.path, &d.text).unwrap(), fqn).is_some();
        assert!(has(&mut plugin, "com.acme.impl.GreeterImpl") && has(&mut plugin, "com.acme.impl.Plain"));
        let renamed = format!("{ROOT}java/com/acme/impl/GreeterImpl.java");
        host.add_file("src/main/java/com/acme/impl/GreeterImpl.java", &IMPL.replace("GreeterImpl", "Renamed"));
        plugin.handle_events(&[Event::FileSaved(renamed)], &host.invocation());
        assert!(has(&mut plugin, "com.acme.impl.Renamed") && !has(&mut plugin, "com.acme.impl.GreeterImpl"));
        let removed = openidea_plugin::FilesChanged { created: vec![], modified: vec![], removed: vec![format!("{ROOT}java/com/acme/impl/Plain.java")] };
        plugin.handle_events(&[Event::FilesChanged(removed)], &host.invocation());
        assert!(!has(&mut plugin, "com.acme.impl.Plain") && has(&mut plugin, "com.acme.impl.Renamed"));
        assert!(host.process_runs().is_empty() && host.requests().is_empty());
    }

    #[test]
    fn registration_completion_rejects_only_established_service_parameter_mismatch() {
        let host=setup();
        host.add_file("src/main/Source.java", "package app; public interface Api {} class Service implements Api {} class Other {} class Listener { public void yes(Api service, java.util.Map props) {} public void object(java.lang.Object service, java.util.Map props) {} public void wrong(Other service, java.util.Map props) {} public void primitive(int service, java.util.Map props) {} public void external(external.Unknown service, java.util.Map props) {} }");
        let text = "<beans xmlns=\"http://www.springframework.org/schema/beans\" xmlns:o=\"http://www.springframework.org/schema/osgi\"><bean id=\"s\" class=\"app.Service\"/><bean id=\"l\" class=\"app.Listener\"/><o:service ref=\"s\" interface=\"app.Api\"><o:registration-listener ref=\"l\" registration-method=\"\"/></o:service></beans>";
        let d=doc("registration.xml",text);
        let items=SpringOsgi::default().completion(&d,after(text,"registration-method=\""),None);
        let labels:Vec<_>=items.iter().map(|i|i.label.as_str()).collect();
        assert!(labels.contains(&"yes") && labels.contains(&"object") && labels.contains(&"external"),"{labels:?}");
        assert!(!labels.contains(&"wrong") && !labels.contains(&"primitive"),"{labels:?}");
    }

    const EMPTY_CONTEXT: &str = "<beans xmlns=\"http://www.springframework.org/schema/beans\"\n       xmlns:osgi=\"http://www.springframework.org/schema/osgi\">\n  <bean id=\"a\" class=\"A\"/>\n</beans>\n";

    fn resolve_generation(host: &MockHost, answers: &[Option<&[&str]>]) -> Result<FileAction, Error> {
        host.open_editor("/project/ctx.xml", "xml", EMPTY_CONTEXT, &[(2,0,2,0)]);
        let d = Document { path: "/project/ctx.xml".into(), language: "xml".into(), version: 1, text: EMPTY_CONTEXT.into() };
        let mut plugin = SpringOsgi::default();
        let action = plugin.file_actions(&d, Range::new(Position::new(2,0),Position::new(2,0))).remove(0);
        for answer in answers { host.answer_next_prompt(*answer); }
        plugin.resolve_file_action(&d,action)
    }
    #[test]
    fn service_action_preserves_output_without_writing() {
        let host=MockHost::install();
        let action=resolve_generation(&host,&[Some(&["service"]),Some(&["com.acme.Api"]),Some(&["a"]),Some(&[])]).unwrap();
        assert_eq!(action.edits[0].1[0].text,"  <osgi:service ref=\"a\" interface=\"com.acme.Api\"/>\n");
        assert_eq!(host.editor_text(),EMPTY_CONTEXT);
    }
    #[test]
    fn action_prompt_cancellation_never_writes() {
        for answers in [vec![None],vec![Some(&["service"][..]),None],vec![Some(&["service"][..]),Some(&["X"][..]),None],vec![Some(&["reference"][..]),Some(&["a.Api"][..]),None]] {
            let host=MockHost::install();
            assert!(matches!(resolve_generation(&host,&answers),Err(Error::Cancelled)));
            assert_eq!(host.editor_text(),EMPTY_CONTEXT);
        }
    }
    #[test]
    fn source_only_generation_needs_no_editor_capabilities() {
        let host=MockHost::install().with_capabilities(&["project.read"]);
        assert!(resolve_generation(&host,&[Some(&["reference"]),Some(&["a.Api"]),Some(&[])]).is_ok());
        assert_eq!(host.editor_text(),EMPTY_CONTEXT);
    }
    #[test]
    fn action_is_absent_without_osgi_namespace() {
        let _host=MockHost::install();
        assert!(SpringOsgi::default().file_actions(&doc("plain.xml","<beans/>"),Range::new(Position::new(0,0),Position::new(0,0))).is_empty());
    }

    #[test]
    fn generation_is_only_a_lazy_host_applied_action() {
        let host = MockHost::install();
        host.open_editor("/project/ctx.xml", "xml", EMPTY_CONTEXT, &[(2, 0, 2, 0)]);
        let d = Document { path: "/project/ctx.xml".into(), language: "xml".into(), version: 1, text: EMPTY_CONTEXT.into() };
        let mut plugin = SpringOsgi::default();
        let action = plugin.file_actions(&d, Range::new(Position::new(2,0), Position::new(2,0))).remove(0);
        assert!(action.edits.is_empty());
        for answer in [Some(&["reference"][..]), Some(&["com.acme.Greeter"][..]), Some(&[][..])] { host.answer_next_prompt(answer); }
        let resolved = plugin.resolve_file_action(&d, action).unwrap();
        assert_eq!(resolved.edits.len(), 1);
        assert!(resolved.edits[0].1[0].text.contains("<osgi:reference id=\"greeterReference\""));
        assert_eq!(host.editor_text(), EMPTY_CONTEXT);
        assert!(plugin.run_command("generate-osgi-bean", &host.invocation(), None).is_err());
        assert_eq!(host.editor_text(), EMPTY_CONTEXT);
    }

    #[test]
    fn reference_collision_checks_ids_names_and_aliases_in_current_buffer() {
        for declaration in [
            "<bean id=\"greeterReference\"/>",
            "<bean name=\"other,greeterReference\"/>",
            "<alias name=\"other\" alias=\"greeterReference\"/>",
            "<osgi:reference id=\"greeterReference\"/>",
        ] {
            let text = EMPTY_CONTEXT.replace("<bean id=\"a\" class=\"A\"/>", declaration);
            assert!(generation_edit(&text, Position::new(2, 0), "reference", "a.Greeter", None).is_err());
        }
    }

    #[test]
    fn generation_refuses_imports_invalid_names_and_unknown_kinds() {
        let encoded = EMPTY_CONTEXT.replace("id=\"a\"", "id=\"api&#82;eference\"");
        assert!(generation_edit(&encoded, Position::new(2, 0), "reference", "a.Api", None).is_err());
        let text = EMPTY_CONTEXT.replace("<bean id=\"a\" class=\"A\"/>", "<import resource=\"other.xml\"/>");
        assert!(generation_edit(&text, Position::new(2, 0), "reference", "a.Api", None).is_err());
        for value in ["", "a..Api", "a.Écran", "a.Api<T>", "${api}", "a.Api\"", ".Api"] {
            assert!(generation_edit(EMPTY_CONTEXT, Position::new(2, 0), "reference", value, None).is_err(), "{value}");
        }
        assert!(generation_edit(EMPTY_CONTEXT, Position::new(2, 0), "unexpected", "a.Api", None).is_err());
        assert!(generation_edit(EMPTY_CONTEXT, Position::new(2, 0), "service", "a.Api", Some(" ")).is_err());
    }

    #[test]
    fn generation_uses_root_binding_and_refuses_nested_or_wrong_namespace() {
        let text = EMPTY_CONTEXT.replace("xmlns:osgi=\"", "xmlns:dynamic=\"").replace('"', "'");
        assert!(generation_edit(&text, Position::new(2, 0), "reference", "a.Api", None).unwrap().text.contains("<dynamic:reference"));
        let nested = "<beans xmlns='http://www.springframework.org/schema/beans'>\n  <bean xmlns:osgi='http://www.springframework.org/schema/osgi'/>\n</beans>";
        assert!(generation_edit(nested, Position::new(1, 0), "reference", "a.Api", None).is_err());
        let wrong = EMPTY_CONTEXT.replace("schema/beans", "schema/other");
        assert!(generation_edit(&wrong, Position::new(2, 0), "reference", "a.Api", None).is_err());
        let nested = EMPTY_CONTEXT.replace("<bean id=\"a\" class=\"A\"/>", "<bean id=\"a\">\n    <property name=\"x\"/>\n  </bean>");
        assert!(generation_edit(&nested, Position::new(3, 0), "reference", "a.Api", None).is_err());
        let comment = EMPTY_CONTEXT.replace("<bean id=\"a\" class=\"A\"/>", "<!--\n    comment\n  -->");
        assert!(generation_edit(&comment, Position::new(3, 0), "reference", "a.Api", None).is_err());
    }

    #[test]
    fn file_action_resolves_current_buffer_without_mutating_it_and_cancels() {
        let host = MockHost::install();
        host.open_editor("/project/src/main/resources/META-INF/spring/ctx.xml", "xml", EMPTY_CONTEXT, &[(2, 0, 2, 0)]);
        let mut plugin = SpringOsgi::default();
        let mut d = doc("ctx.xml", EMPTY_CONTEXT);
        let action = plugin.file_actions(&d, Range::new(Position::new(2, 0), Position::new(2, 0))).pop().unwrap();
        assert!(action.edits.is_empty());
        for answers in [
            vec![None],
            vec![Some(&["reference"][..]), None],
            vec![Some(&["service"][..]), Some(&["a.Api"][..]), None],
            vec![Some(&["reference"][..]), Some(&["a.Greeter"][..]), None],
        ] {
            for answer in answers {
                host.answer_next_prompt(answer);
            }
            assert!(matches!(plugin.resolve_file_action(&d, action.clone()), Err(Error::Cancelled)));
        }
        host.answer_next_prompt(Some(&["reference"]));
        host.answer_next_prompt(Some(&["a.Greeter"]));
        host.answer_next_prompt(Some(&[])); // Concrete XML confirmation.
        let resolved = plugin.resolve_file_action(&d, action.clone()).unwrap();
        assert!(resolved.operations.is_empty());
        assert_eq!(resolved.edits[0].0, d.path);
        assert!(resolved.edits[0].1[0].text.contains("greeterReference"));
        assert_eq!(host.editor_text(), EMPTY_CONTEXT, "resolution returns preview edits only");
        d.version += 1;
        assert!(plugin.resolve_file_action(&d, action.clone()).is_err());
        d.version -= 1;
        d.path = "/project/other.xml".into();
        assert!(plugin.resolve_file_action(&d, action).is_err());
    }

    #[test]
    fn generation_works_under_a_windows_drive_root() {
        let host = MockHost::install();
        host.set_project_root(Some(r"C:\work\app"));
        let path = "C:/work/app/ctx.xml";
        host.add_file(path, EMPTY_CONTEXT);
        let d = Document { path: path.into(), language: "xml".into(), version: 1, text: EMPTY_CONTEXT.into() };
        let mut plugin = SpringOsgi::default();
        let action = plugin.file_actions(&d, Range::new(Position::new(2, 0), Position::new(2, 0))).pop().expect("offered under C:");
        for answer in [Some(&["reference"][..]), Some(&["a.Api"][..]), Some(&[][..])] {
            host.answer_next_prompt(answer);
        }
        assert_eq!(plugin.resolve_file_action(&d, action).unwrap().edits.len(), 1);
    }

    #[test]
    fn ownership_scopes_windows_roots() {
        let roots = vec![r"C:\work".to_owned(), r"C:\work\app".to_owned()];
        let scope = ownership::Scope::new(Some(roots), r"c:\work\app\ctx.xml");
        assert!(scope.known());
    }

    #[test]
    fn generation_unsaved_says_save_first() {
        let host=MockHost::install(); let mut plugin=SpringOsgi::default();
        let d=doc("actual.xml",EMPTY_CONTEXT);
        host.open_editor(&d.path,"xml",&d.text,&[(2,0,2,0)]);
        let action=plugin.file_actions(&d,Range::new(Position::new(2,0),Position::new(2,0))).pop().unwrap();
        host.type_in_editor("<!-- current source changed -->");
        for answer in [Some(&["reference"][..]),Some(&["a.Api"][..]),Some(&[][..])] {host.answer_next_prompt(answer);}
        let result=plugin.resolve_file_action(&d,action);
        assert!(matches!(result,Err(Error::Invalid(ref m)) if m=="Save the file first"),"{result:?}");
        assert!(host.editor_text().contains("current source changed"));
    }
    #[test]
    fn reference_collision_is_rechecked_on_resolution_and_crlf_is_preserved() {
        let host = MockHost::install();
        let mut d = doc("unsaved.xml", &EMPTY_CONTEXT.replace('\n', "\r\n"));
        let mut plugin = SpringOsgi::default();
        let action = plugin.file_actions(&d, Range::new(Position::new(2, 0), Position::new(2, 0))).pop().unwrap();
        // Resolver receives the current text, not a cached action-time XML tree.
        d.text = d.text.replace("id=\"a\"", "id=\"apiReference\"");
        host.answer_next_prompt(Some(&["reference"]));
        host.answer_next_prompt(Some(&["a.Api"]));
        assert!(plugin.resolve_file_action(&d, action).is_err());
        let edit = generation_edit(&d.text, Position::new(2, 0), "service", "a.Api", Some("a&b")).unwrap();
        assert!(edit.text.ends_with("\r\n"));
        assert!(edit.text.contains("a&amp;b"));
    }
    #[test]
    fn collection_generation_is_explicit_and_checks_all_local_name_owners() {
        for (kind, id) in [("list", "apiList"), ("set", "apiSet")] {
            let e = generation_edit(EMPTY_CONTEXT, Position::new(2, 0), kind, "a.Api", None).unwrap();
            assert!(e.text.contains(&format!("<osgi:{kind} id=\"{id}\" interface=\"a.Api\"/>")));
            for owner in
                [format!("<bean name='x;{id}'/>"), format!("<alias name='a' alias='{id}'/>"), format!("<osgi:reference id='{id}'/>")]
            {
                let text = EMPTY_CONTEXT.replace("<bean id=\"a\" class=\"A\"/>", &owner);
                assert!(generation_edit(&text, Position::new(2, 0), kind, "a.Api", None).is_err());
            }
            for unknown in ["<import resource='other.xml'/>", "<bean id='${dynamic}'/>"] {
                let text = EMPTY_CONTEXT.replace("<bean id=\"a\" class=\"A\"/>", unknown);
                assert!(generation_edit(&text, Position::new(2, 0), kind, "a.Api", None).is_err());
            }
        }
    }
    #[test]
    fn default_osgi_namespace_is_safe_with_a_prefixed_beans_root() {
        let text = "<s:beans xmlns:s='http://www.springframework.org/schema/beans' xmlns='http://www.springframework.org/schema/osgi'>\r\n  \r\n</s:beans>";
        let edit = generation_edit(text, Position::new(1, 0), "list", "a.Api", None).unwrap();
        assert_eq!(edit.text, "  <list id=\"apiList\" interface=\"a.Api\"/>\r\n");
        let mut after = text.to_owned();
        after.insert_str(offset(text, edit.range.start).unwrap(), &edit.text);
        validate_generation_xml(&after).unwrap();
    }
    #[test]
    fn malformed_generation_xml_never_offers_actions() {
        let _host = MockHost::install();
        let mut p = SpringOsgi::default();
        for text in [
            EMPTY_CONTEXT.replace("</beans>", "</wrong>"),
            EMPTY_CONTEXT.replace("xmlns:osgi=", "xmlns:xml="),
            EMPTY_CONTEXT.replace("class=\"A\"", "id='duplicate'"),
            format!("<!DOCTYPE beans [<!ENTITY custom 'x'>]>{EMPTY_CONTEXT}"),
            EMPTY_CONTEXT.replace("<bean id=\"a\" class=\"A\"/>", "<bean><other></bean>"),
        ] {
            assert!(p.file_actions(&doc("a.xml", &text), Range::new(Position::new(2, 0), Position::new(2, 0))).is_empty(), "{text}");
        }
    }
    #[test]
    fn action_tickets_are_instance_source_and_project_owned() {
        let host = MockHost::install();
        let mut p = SpringOsgi::default();
        let d = doc("ctx.xml", EMPTY_CONTEXT);
        let a = p.file_actions(&d, Range::new(Position::new(2, 0), Position::new(2, 0))).pop().unwrap();
        assert!(matches!(SpringOsgi::default().resolve_file_action(&d, a.clone()), Err(Error::Stale)));
        let mut changed = d.clone();
        changed.text.push(' ');
        assert!(matches!(p.resolve_file_action(&changed, a.clone()), Err(Error::Stale)));
        host.set_project_root(None);
        assert!(matches!(p.resolve_file_action(&d, a), Err(Error::Stale)));
    }
    #[test]
    fn collection_action_preview_returns_one_atomic_edit_without_writing() {
        let host = MockHost::install();
        host.open_editor("/project/src/main/resources/META-INF/spring/ctx.xml", "xml", EMPTY_CONTEXT, &[(2, 0, 2, 0)]);
        let mut p = SpringOsgi::default();
        let d = doc("ctx.xml", EMPTY_CONTEXT);
        let a = p.file_actions(&d, Range::new(Position::new(2, 0), Position::new(2, 0))).pop().unwrap();
        for answer in [Some(&["set"][..]), Some(&["a.Api"][..]), Some(&[][..])] {
            host.answer_next_prompt(answer);
        }
        let action = p.resolve_file_action(&d, a).unwrap();
        assert!(action.operations.is_empty());
        assert_eq!(action.edits.len(), 1);
        assert_eq!(action.edits[0].0, d.path);
        assert!(action.edits[0].1[0].text.contains("apiSet"));
        assert_eq!(host.editor_text(), EMPTY_CONTEXT);
    }
    #[test]
    fn oversized_concrete_generation_preview_is_refused_before_confirming() {
        let host = MockHost::install();
        let mut p = SpringOsgi::default();
        let d = doc("ctx.xml", EMPTY_CONTEXT);
        host.open_editor(&d.path,"xml",&d.text,&[(2,0,2,0)]);
        let a = p.file_actions(&d, Range::new(Position::new(2, 0), Position::new(2, 0))).pop().unwrap();
        let huge = "A".repeat(20_000);
        host.answer_next_prompt(Some(&["service"]));
        host.answer_next_prompt(Some(&["a.Api"]));
        host.answer_next_prompt(Some(&[&huge]));
        host.answer_next_prompt(Some(&[]));
        assert!(matches!(p.resolve_file_action(&d, a), Err(Error::Invalid(_))));
        assert!(host::confirm("Unconsumed", "No partial preview was shown").unwrap());
    }
    #[test]
    fn reserved_namespace_bindings_and_aliases_refuse_generation() {
        let _h = MockHost::install();
        let mut p = SpringOsgi::default();
        for declaration in [
            "xmlns:xml='urn:foreign'",
            "xmlns:xmlns='urn:foreign'",
            "xmlns:q='http://www.w3.org/XML/1998/namespace'",
            "xmlns:q='http://www.w3.org/2000/xmlns/'",
            "xmlns:q='http://www.w3.org/XML/1998/namespac&#101;'",
            "xmlns:q=''",
        ] {
            let text = EMPTY_CONTEXT.replace("<beans ", &format!("<beans {declaration} "));
            assert!(
                p.file_actions(&doc("ctx.xml", &text), Range::new(Position::new(2, 0), Position::new(2, 0))).is_empty(),
                "{declaration}"
            );
        }
        for namespace in ["http://www.w3.org/XML/1998/namespace", "http://www.w3.org/2000/xmlns/", "http://www.w3.org/2000/xmln&#115;/"] {
            let text = format!(
                "<b:beans xmlns:b='http://www.springframework.org/schema/beans' xmlns:osgi='{OSGI}' xmlns='{namespace}'>\n\n</b:beans>"
            );
            assert!(generation_site(&text, Position::new(1, 0)).is_err());
        }
        let text = EMPTY_CONTEXT.replace("<beans ", "<beans xmlns:xml='http://www.w3.org/XML/1998/namespace' xml:lang='en' ");
        assert!(generation_edit(&text, Position::new(2, 0), "list", "a.Api", None).is_ok());
        let implicit = EMPTY_CONTEXT.replace("<beans ", "<beans xml:lang='en' ");
        assert!(generation_edit(&implicit, Position::new(2, 0), "set", "a.Api", None).is_ok());
    }
}
