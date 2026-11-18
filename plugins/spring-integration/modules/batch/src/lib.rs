//! Spring Batch Tools for OpenIDEA: navigation, completion and checks for
//! XML job configuration (steps, flows, transitions, bean references), job
//! names in `spring.batch.job.name`, and Spring Batch 5 migration hints in
//! Java configuration. Text-based: names match as written.

use openidea_plugin::views::{Body, Button, View, ViewEvent};
use openidea_plugin::providers::{CompletionItem, CompletionKind, Document, Providers};
use openidea_plugin::text::offset;
use openidea_plugin::{Diagnostic, Error, Invocation, Location, Plugin, Position, Project, Range, Severity, host};
use spring_text::diagram::{self, Edge, Node};
use spring_text::index::{COMMAND_BUDGET, View as IndexView};
use spring_text::{beans, config, contains, java, range, xml};
use std::collections::BTreeSet;

mod scope;
mod xml_rules;

const NAMESPACE: &str = "http://www.springframework.org/schema/batch";
/// Elements a transition can lead to.
const STATES: [&str; 4] = ["step", "flow", "split", "decision"];
/// Attributes of the batch namespace that name a bean.
const BEAN_ATTRS: [&str; 18] = [
    "ref",
    "reader",
    "writer",
    "processor",
    "skip-policy",
    "retry-policy",
    "decider",
    "job-repository",
    "transaction-manager",
    "task-executor",
    "job-launcher",
    "job-operator",
    "job-parameters-extractor",
    "incrementer",
    "aggregator",
    "handler",
    "partitioner",
    "chunk-completion-policy",
];
const JOB_NAME_KEY: &str = "spring.batch.job.name";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// `<job id>` or an explicitly owned Spring JobBuilder constructor.
    Job,
    /// A step, flow, split or decision id (or a Java step name).
    State,
    /// A bean of any kind.
    Bean,
    /// A `@SpringBootApplication` class.
    BootApp,
    /// A job parameter key: `#{jobParameters['key']}` or `addString("key", …)`.
    JobParam,
    /// A builder/factory source context whose job-name identity is unproved.
    UnresolvedJob,
    UnresolvedBean,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Name {
    pub name: String,
    pub kind: Kind,
    pub range: Range,
    /// The enclosing job (or top-level flow) element of an XML state.
    pub scope: Option<usize>,
    /// The element index of an XML job or state, for matching transitions to it.
    pub el: usize,
    /// `job`, `step`, `flow`, `split` or `decision`; empty for beans.
    pub detail: &'static str,
    /// Transitions leaving an XML state: target id and its label (`on` value).
    pub goes: Vec<(String, Option<String>)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkKind {
    /// `next`, `to`, `restart`: a state of the same job.
    Target,
    /// `parent`: a step, flow or job anywhere.
    Parent,
    Bean,
}

/// A reference written in a batch XML attribute.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub value: String,
    pub kind: LinkKind,
    pub range: Range,
    pub scope: Option<usize>,
}

/// The nearest enclosing `<job>`, else the outermost enclosing `<flow>`.
fn scope_of(els: &[xml::Element], i: usize) -> Option<usize> {
    let mut flow = None;
    let mut at = els[i].parent;
    while let Some(p) = at {
        match els[p].local() {
            "job" => return Some(p),
            "flow" => flow = Some(p),
            _ => {}
        }
        at = els[p].parent;
    }
    flow
}

/// The names and references of a Spring XML file.
fn owned_batch_context(elements:&[xml::Element], mut at:usize)->bool {
    const BEANS:&str="http://www.springframework.org/schema/beans";
    while let Some(parent)=elements[at].parent {
        let element=&elements[parent];let namespace=xml::namespace(elements,parent);
        if namespace!=Some(NAMESPACE) && !(element.local()=="beans" && matches!(namespace,None|Some(BEANS))) {return false;}
        at=parent;
    }
    true
}
pub fn parse_xml(text: &str) -> (Vec<Name>, Vec<Link>) {
    if !text.contains("www.springframework.org/schema/") || !spring_text::xml::analysis_valid(text) {
        return Default::default();
    }
    let els = xml::elements(text);
    let attr_range = |a: &xml::Attr| range(text, a.value_at, a.value_end);
    let mut names: Vec<Name> = xml::bean_ids(&els)
        .into_iter()
        .map(|a| Name { name: a.value.clone(), kind: Kind::Bean, range: attr_range(a), scope: None, el: 0, detail: "", goes: Vec::new() })
        .collect();
    let mut links = Vec::new();
    if !text.contains(NAMESPACE) {
        return (names, links);
    }
    let mut owners = std::collections::BTreeMap::new();
    let mut transitions: Vec<(usize, String, Option<String>)> = Vec::new();
    for (i, e) in els.iter().enumerate().filter(|(i, _)| xml::namespace(&els, *i) == Some(NAMESPACE) && owned_batch_context(&els,*i)) {
        let scope = scope_of(&els, i);
        if let Some(id) = e.attr("id") {
            let kind = match e.local() {
                "job" => Some(Kind::Job),
                l if STATES.contains(&l) => Some(Kind::State),
                _ => None,
            };
            if let Some(kind) = kind {
                let detail = if e.local() == "job" { "job" } else { STATES.iter().find(|s| **s == e.local()).copied().unwrap_or("") };
                owners.insert(i, names.len());
                names.push(Name { name: id.value.clone(), kind, range: attr_range(id), scope, el: i, detail, goes: Vec::new() });
            }
        }
        for a in &e.attrs {
            if a.name!=a.local() {continue;}
            let kind = match a.local() {
                "next" | "to" | "restart" => LinkKind::Target,
                "parent" => LinkKind::Parent,
                l if BEAN_ATTRS.contains(&l) => LinkKind::Bean,
                _ => continue,
            };
            links.push(Link { value: a.value.trim().to_owned(), kind, range: attr_range(a), scope });
            if kind != LinkKind::Target {
                continue;
            }
            let mut owner = Some(i);
            while let Some(o) = owner.filter(|o| !STATES.contains(&els[*o].local())) {
                owner = els[o].parent;
            }
            let on = e.attr("on").map(|on| on.value.clone());
            let label = if a.local() == "restart" { Some(format!("{} restart", on.unwrap_or_default()).trim().to_owned()) } else { on };
            if let Some(owner) = owner {
                transitions.push((owner, a.value.trim().to_owned(), label));
            }
        }
    }
    for (owner, target, label) in transitions {
        if let Some(&n) = owners.get(&owner) {
            names[n].goes.push((target, label));
        }
    }
    (names, links)
}

/// The names a Java or Kotlin source declares.
pub fn parse_source(path: &str, text: &str) -> Vec<Name> {
    let file = spring_text::parse_source(path, text);
    let name = |name: String, kind, from, to| Name { name, kind, range: range(text, from, to), scope: None, el: 0, detail: "", goes: Vec::new() };
    let kotlin=path.ends_with(".kt");
    let tokens=java::tokens(text);
    let supported=!(kotlin && tokens.iter().any(|t|matches!(t.s,"as"|"typealias")) || !kotlin && text.contains("\\u"))
        && file.types.iter().all(|owner|owner.supers().next().is_none() && !text[owner.at+owner.name.len()..].trim_start().starts_with('<'));
    let mut out: Vec<Name> = java::bean_names(&file).into_iter().map(|b| {
        let kind=if supported && spring_text::provenance::source_bean_owned(&file,text,&b,kotlin) {Kind::Bean}else{Kind::UnresolvedBean};
        name(b.name,kind,b.at,b.at+b.len)
    }).collect();
    if !supported {out.push(name(String::new(),Kind::UnresolvedBean,0,0));}
    out.extend(file.types.iter().filter(|t| supported && t.annotations.iter().any(|a|a.name=="SpringBootApplication" && text.get(a.at+1..a.name_at+a.name.len()).is_some_and(|written|spring_text::provenance::source_type_owned(&file,written,"org.springframework.boot.autoconfigure.SpringBootApplication")))).map(|t| {
        name(t.name.clone(), Kind::BootApp, t.at, t.at + t.name.len())
    }));
    if !text.contains("Builder") {
        return out;
    }
    let tokens=java::tokens(text);
    let supported=!(path.ends_with(".java") && text.contains("\\u") || path.ends_with(".kt") && tokens.iter().any(|t|matches!(t.s,"as"|"typealias")))
        && file.types.iter().all(|owner|owner.supers().next().is_none() && !text[owner.at+owner.name.len()..].trim_start().starts_with('<'));
    let mut uncertain=false;
    for c in spring_text::source_calls(path, text) {
        let receiver = c.receiver.as_deref().unwrap_or_default().to_lowercase();
        let kind = match c.name.as_str() {
            "JobBuilder" if c.is_new => Kind::Job,
            "StepBuilder" if c.is_new => Kind::State,
            "get" if receiver.contains("jobbuilder") || receiver.contains("stepbuilder") || text.contains("JobBuilderFactory") || text.contains("StepBuilderFactory") => {uncertain=true;continue;},
            _ => continue,
        };
        let Some(s)=c.args.first().and_then(|arg|arg.string.as_ref()) else{uncertain=true;continue;};
        let qualified=if kind==Kind::Job {"org.springframework.batch.core.job.builder.JobBuilder"}else{"org.springframework.batch.core.step.builder.StepBuilder"};
        let Some(at)=tokens.iter().position(|t|t.at==c.at) else{uncertain=true;continue;};
        let mut start=at;while start>=2 && tokens[start-1].s=="." && tokens[start-2].is_ident(){start-=2;}
        let written=tokens[start..=at].iter().map(|t|t.s).collect::<String>();
        let owned=if written==qualified {!file.types.iter().any(|t|t.name=="org") && !file.imports.iter().any(|i|i.path.rsplit('.').next()==Some("org"))}else{
            written==c.name && !file.types.iter().any(|t|t.name==c.name) && !file.imports.iter().any(|i|i.path.ends_with(".*"))
                && file.imports.iter().filter(|i|!i.is_static && i.path.rsplit('.').next()==Some(c.name.as_str())).map(|i|i.path.as_str()).collect::<Vec<_>>()==[qualified]
        };
        if !supported || !owned || s.value.is_empty() || s.value.contains('\\') || path.ends_with(".kt") && s.value.contains('$') || c.args[0].text.starts_with("\"\"\"") || text.get(s.at..s.end)!=Some(s.value.as_str()) || tokens.iter().find(|t|t.at>=c.end).is_some_and(|t|t.s=="{") {uncertain=true;continue;}
        out.push(Name { detail: if kind == Kind::Job { "job" } else { "step" }, ..name(s.value.clone(), kind, s.at, s.end) });
    }
    if uncertain {out.push(name(String::new(),Kind::UnresolvedJob,0,0));}
    out
}

const PARAMETER_ADDERS: [&str; 5] = ["addString", "addLong", "addDouble", "addDate", "addLocalDateTime"];

/// The job parameter keys a file reads in late-binding expressions or sets through `JobParametersBuilder`.
fn job_parameters(path: &str, text: &str) -> Vec<Name> {
    let mut out = Vec::new();
    if path.ends_with(".xml") {
        if !spring_text::xml::analysis_valid(text) {return Vec::new();}
        let elements=xml::elements(text);const BEANS:&str="http://www.springframework.org/schema/beans";
        for (index,element) in elements.iter().enumerate() {
            let batch=xml::namespace(&elements,index)==Some(NAMESPACE) && owned_batch_context(&elements,index);
            let bean_value=xml::namespace(&elements,index)==Some(BEANS) && matches!(element.local(),"property"|"constructor-arg") && element.parent.is_some_and(|parent| {
                elements[parent].local()=="bean" && xml::namespace(&elements,parent)==Some(BEANS) && elements[parent].parent.is_some_and(|root|elements[root].local()=="beans" && xml::namespace(&elements,root)==Some(BEANS) && elements[root].parent.is_none())
            });
            for attribute in element.attrs.iter().filter(|a|!a.name.contains(':') && (batch && BEAN_ATTRS.contains(&a.name.as_str()) || bean_value && a.name=="value")) {
                if text.get(attribute.value_at..attribute.value_end)!=Some(attribute.value.as_str()) || attribute.value.contains('&') {continue;}
                let prefix="#{jobParameters['";let Some(key)=attribute.value.strip_prefix(prefix).and_then(|v|v.strip_suffix("']}")) else{continue;};
                if key.is_empty() || key.contains(['\'','[',']','\n','\r']) {continue;}
                out.push((key.to_owned(),attribute.value_at+prefix.len(),attribute.value_at+prefix.len()+key.len()));
            }
        }
    } else {
        let file=spring_text::parse_source(path,text);let kotlin=path.ends_with(".kt");let tokens=java::tokens(text);
        let supported=!(kotlin && tokens.iter().any(|t|matches!(t.s,"as"|"typealias")) || !kotlin && text.contains("\\u")) && file.types.iter().all(|t|t.supers().next().is_none());
        for annotation in file.types.iter().flat_map(|t|t.members.iter()).flat_map(|m|m.annotations.iter().chain(m.params.iter().flat_map(|p|p.annotations.iter()))).filter(|a|a.name=="Value" && supported) {
            let Some(written)=text.get(annotation.at+1..annotation.name_at+annotation.name.len()) else{continue;};
            if !spring_text::provenance::source_type_owned(&file,written,"org.springframework.beans.factory.annotation.Value") {continue;}
            let [arg]=annotation.args.as_slice() else{continue;};if arg.name!="value" {continue;}
            let [literal]=arg.strings.as_slice() else{continue;};
            if java::tokens(&arg.text).len()!=1 || arg.text.trim_start().starts_with("\"\"\"") || literal.value.contains('\\') || kotlin && literal.value.contains('$') || text.get(literal.at..literal.end)!=Some(literal.value.as_str()) {continue;}
            let prefix="#{jobParameters['";let Some(key)=literal.value.strip_prefix(prefix).and_then(|v|v.strip_suffix("']}")) else{continue;};
            if key.is_empty() || key.contains(['\'','[',']','\n','\r']) {continue;}
            out.push((key.to_owned(),literal.at+prefix.len(),literal.at+prefix.len()+key.len()));
        }
    }
    if !path.ends_with(".xml") && text.contains("add") {
        let file=spring_text::parse_source(path,text);let tokens=java::tokens(text);let kotlin=path.ends_with(".kt");
        let supported=!(kotlin && tokens.iter().any(|t|matches!(t.s,"as"|"typealias")) || !kotlin && text.contains("\\u")) && file.types.iter().all(|t|t.supers().next().is_none());
        let mut ends=Vec::new();
        for c in spring_text::source_calls(path,text) {
            if c.is_new && c.name=="JobParametersBuilder" && supported {
                if let Some(at)=tokens.iter().position(|t|t.at==c.at) {
                    let mut start=at;while start>=2 && tokens[start-1].s=="." && tokens[start-2].is_ident(){start-=2;}
                    let written=tokens[start..=at].iter().map(|t|t.s).collect::<String>();
                    if spring_text::provenance::source_type_owned(&file,&written,"org.springframework.batch.core.JobParametersBuilder") && !tokens.iter().find(|t|t.at>=c.end).is_some_and(|t|t.s=="{") {ends.push(c.end);}
                }
            } else if PARAMETER_ADDERS.contains(&c.name.as_str()) && c.chained && ends.iter().any(|end|*end<=c.at && text[*end..c.at].trim()==".") {
                if let Some(s)=c.args.first().filter(|a|!a.text.trim_start().starts_with("\"\"\"")).and_then(|a|a.string.as_ref()).filter(|s|!s.value.contains('\\') && !(kotlin && s.value.contains('$')) && text.get(s.at..s.end)==Some(s.value.as_str())) {
                    out.push((s.value.clone(),s.at,s.end));ends.push(c.end);
                }
            }
        }
    }
    out.into_iter()
        .filter(|(name, ..)| !name.is_empty() && !name.contains(['\n', '#', '$']))
        .map(|(name, from, to)| Name { name, kind: Kind::JobParam, range: range(text, from, to), scope: None, el: 0, detail: "job parameter", goes: Vec::new() })
        .collect()
}

/// The key being typed inside `jobParameters['…`: its bytes up to the closing quote.
fn parameter_key_at(text: &str, caret: usize) -> Option<(usize, usize)> {
    let head = text.get(..caret)?;
    let line_start = head.rfind('\n').map_or(0, |i| i + 1);
    let marker = head[line_start..].rfind("jobParameters[")? + line_start + "jobParameters[".len();
    let quote = head[marker..].chars().next().filter(|c| matches!(c, '\'' | '"'))?;
    let start = marker + 1;
    if head[start..].contains(quote) {
        return None;
    }
    let tail = &text[caret..];
    let end = caret + tail.find([quote, '\n']).filter(|&i| tail[i..].starts_with(quote)).unwrap_or(0);
    Some((start, end))
}

// Validate the completion destination with the same source/XML admission as
// indexed facts. A local sentinel also admits an empty key without publishing it.
fn parameter_context(path:&str,text:&str,from:usize,to:usize)->bool {
    if text.len()>512*1024 || from>to || !text.is_char_boundary(from) || !text.is_char_boundary(to) {return false;}
    let mut candidate=text.to_owned();candidate.replace_range(from..to,"openideaParameterContext");
    let expected=range(&candidate,from,from+"openideaParameterContext".len());
    job_parameters(path,&candidate).iter().any(|name|name.name=="openideaParameterContext" && name.range==expected)
}

pub fn index_file(path: &str, text: &str) -> Option<Vec<Name>> {
    let mut names = if path.ends_with(".xml") { parse_xml(text).0 } else { parse_source(path, text) };
    if text.contains("jobParameters") || text.contains("JobParametersBuilder") {
        names.extend(job_parameters(path, text));
    }
    Some(names).filter(|n| !beans::ignored(path) && !n.is_empty())
}

/// Spring Batch 5 hints for a Java source: deprecated builder factories, and
/// `@EnableBatchProcessing` next to Spring Boot.
pub fn java_problems(text: &str, boot: bool) -> Vec<Diagnostic> {
    let file = java::parse(text);
    let imported = |at: usize| file.imports.iter().any(|i| i.at <= at && at <= i.end);
    let deprecated = |from: usize, to: usize, factory: &str| {
        let builder = factory.trim_end_matches("Factory");
        Diagnostic {
            deprecated: true,
            ..Diagnostic::new(
                range(text, from, to),
                Severity::Warning,
                format!("{factory} is deprecated since Spring Batch 5. Use new {builder}(name, jobRepository) instead."),
            )
            .code("deprecated-builder-factory")
        }
    };
    let mut out: Vec<Diagnostic> = java::tokens(text)
        .into_iter()
        .filter(|t| matches!(t.s, "JobBuilderFactory" | "StepBuilderFactory") && !imported(t.at))
        .map(|t| deprecated(t.at, t.end(), t.s))
        .collect();
    for c in java::calls(text).into_iter().filter(|c| c.name == "get") {
        let Some(receiver) = c.receiver.as_deref() else { continue };
        let lower = receiver.to_lowercase();
        let factory = if lower.contains("jobbuilder") {
            "JobBuilderFactory"
        } else if lower.contains("stepbuilder") {
            "StepBuilderFactory"
        } else {
            continue;
        };
        let written = format!("{receiver}.");
        let from = c.at.checked_sub(written.len()).filter(|&f| text[f..c.at] == written).unwrap_or(c.at);
        out.push(deprecated(from, c.at + c.name.len(), factory));
    }
    if boot {
        let enable = file.types.iter().flat_map(|t| &t.annotations).filter(|a| a.name == "EnableBatchProcessing");
        out.extend(enable.map(|a| {
            Diagnostic::new(
                range(text, a.at, a.end),
                Severity::Info,
                "With Spring Boot 3, @EnableBatchProcessing switches off Boot's batch auto-configuration (running jobs at startup, schema initialisation, spring.batch.* properties). Remove it unless you want that.",
            )
            .code("enable-batch-processing-with-boot")
        }));
    }
    out
}

/// The job names of a `spring.batch.job.name` value (comma-separated): `(name, from, to)` bytes.
fn job_name_items(path: &str, text: &str) -> Vec<(String, usize, usize)> {
    let key = config::canonical(JOB_NAME_KEY);
    let mut out = Vec::new();
    for k in config::keys(path, text).into_iter().filter(|k| config::canonical(&k.key) == key) {
        let mut at = k.value_at;
        for part in text[k.value_at..k.value_end].split(',') {
            let lead = part.len() - part.trim_start().len();
            out.push((part.trim().to_owned(), at + lead, at + lead + part.trim().len()));
            at += part.len() + 1;
        }
    }
    out
}

fn file_kind(doc: &Document) -> Option<&'static str> {
    match doc.language.as_str() {
        "xml" if doc.text.contains(NAMESPACE) => Some("xml"),
        "java" if doc.text.contains("batch") => Some("java"),
        "kotlin" if doc.text.contains("batch") => Some("kotlin"),
        _ if config::is_config_file(&doc.path) => Some("config"),
        _ => None,
    }
}

/// The batch facts of every file of the project, as one part of the owner's shared index.
pub type Names<'a> = IndexView<'a, Vec<Name>>;

/// Where the owner keeps the shared index: each file is read and parsed once for all modules.
pub trait Source {
    /// The index of the project holding `doc`, with `doc` applied, read on for a provider budget.
    fn for_document(&mut self, doc: &Document) -> Option<Names<'_>>;
    /// Like [`Self::for_document`], without reading any more of the project.
    fn cached(&mut self, doc: &Document) -> Option<Names<'_>>;
    /// The index of `project`, read on for at most `budget`.
    fn project(&mut self, project: &Project, budget: std::time::Duration) -> Names<'_>;
    /// Starts the index over, unless a scan of it is in progress (that would throw away what it has read).
    fn restart(&mut self, project: &Project);
}

fn all<'a: 'b, 'b>(index: &'b Names<'a>) -> impl Iterator<Item = (&'a str, &'a Name)> + 'b {
    index.iter().flat_map(|(path, names)| names.iter().map(move |n| (path, n)))
}

fn locations<'a>(found: impl Iterator<Item = (&'a str, &'a Name)>) -> Vec<Location> {
    found.map(|(path, n)| Location { path: path.into(), range: n.range }).collect()
}

fn completions<'a>(names: impl Iterator<Item = &'a str>, detail: &str, replace: Option<Range>) -> Vec<CompletionItem> {
    let names: BTreeSet<&str> = names.filter(|n| !n.is_empty() && !beans::is_expression(n)).collect();
    names
        .into_iter()
        .map(|n| CompletionItem { detail: Some(detail.into()), replace, ..CompletionItem::new(n, CompletionKind::Reference) })
        .collect()
}

#[derive(Default)]
pub struct Batch;

impl Batch {
    /// Literal, source-proved job names (XML `<job id>` and owned `new JobBuilder("name", ..)`), sorted; empty until the project is scanned and owned.
    pub fn job_names(&mut self, project: &Project, source: &mut impl Source) -> Vec<String> {
        if spring_text::index::scoped_root(project).is_none() {return Vec::new();}
        let index=source.project(project,COMMAND_BUDGET);
        let index=&index;
        all(index).filter(|(_,n)|n.kind==Kind::Job && !n.name.is_empty()).map(|(_,n)|n.name.clone()).collect::<BTreeSet<_>>().into_iter().collect()
    }

    /// Owner calls this for live results. It never discovers or reads project files.
    pub fn live_diagnostics(&mut self, doc: &Document, source: &mut impl Source) -> Vec<Diagnostic> {
        let Some(kind) = file_kind(doc) else { return Vec::new() };
        if spring_text::index::owning_root(&doc.path).is_none() {return Vec::new();}
        let Some(index) = source.cached(doc) else { return Vec::new() };
        let index = &index;
        match kind {
            "xml" => {
                let mut found = xml_problems(&doc.text, index);
                if host::setting_bool("springBatch.checkChunk").unwrap_or(true) { found.extend(chunk_problems(&doc.text)); }
                found.extend(xml_rules::configured_problems(&doc.text,host::setting_string("springBatch.xmlCompatibility").as_deref()));
                found.extend(scope::xml_problems(&doc.text));
                found
            }
            "java" => {
                let boot = index.fresh() && all(index).any(|(_, n)| n.kind == Kind::BootApp);
                let mut found = java_problems(&doc.text, boot);
                found.extend(scope::java_problems(&doc.text));
                found
            }
            "kotlin" => Vec::new(),
            _ => config_problems(doc, index),
        }
    }

}

/// The XML link under the caret, with the names of its file.
fn xml_link(doc: &Document, at: Position) -> Option<(Link, Vec<Name>)> {
    let (names, links) = parse_xml(&doc.text);
    let link = links.into_iter().find(|l| contains(l.range, at))?;
    Some((link, names))
}

/// The names of the other files of the project.
fn elsewhere<'a>(index: &Names<'a>, doc: &Document) -> Vec<(&'a str, &'a Name)> {
    all(index).filter(|(p, _)| *p != doc.path).collect()
}

/// Transition targets never fall through to another job/file/project. Parent/bean lookup retains its legacy model.
fn targets(link: &Link, names: &[Name], project: &[(&str, &Name)], path: &str) -> Vec<Location> {
    let here = |n: &Name| Location { path: path.into(), range: n.range };
    let elsewhere = |kinds: &[Kind]| {
        locations(project.iter().copied().filter(|(_, n)| kinds.contains(&n.kind) && n.name == link.value))
    };
    if link.kind == LinkKind::Bean {
        let mut found: Vec<_> = names.iter().filter(|n| n.kind == Kind::Bean && n.name == link.value).map(here).collect();
        found.extend(elsewhere(&[Kind::Bean]));
        return found;
    }
    if link.kind==LinkKind::Target {
        if link.scope.is_none(){return Vec::new();}
        let found:Vec<_>=names.iter().filter(|n|n.kind==Kind::State && n.scope==link.scope && n.name==link.value).map(here).collect();
        return if found.len()==1{found}else{Vec::new()};
    }
    let kinds = [Kind::State, Kind::Job];
    let named = |n: &&Name| kinds.contains(&n.kind) && n.name == link.value;
    let in_file: Vec<_> = names.iter().filter(named).map(here).collect();
    if !in_file.is_empty() {
        return in_file;
    }
    elsewhere(&kinds)
}

fn xml_problems(text: &str, index: &Names<'_>) -> Vec<Diagnostic> {
    let (names, links) = parse_xml(text);
    let mut out = Vec::new();
    for l in links.iter().filter(|l| !l.value.is_empty() && !beans::is_expression(&l.value)) {
        match l.kind {
            LinkKind::Target if l.scope.is_some() => {
                if !names.iter().any(|n| n.kind == Kind::State && n.scope == l.scope && n.name == l.value) {
                    out.push(
                        Diagnostic::new(l.range, Severity::Error, format!("This job has no step, flow, split or decision '{}'.", l.value))
                            .code("unknown-transition-target"),
                    );
                }
            }
            LinkKind::Bean if index.fresh() && !all(index).any(|(_,n)|n.kind==Kind::UnresolvedBean) => {
                let single = !l.value.contains([',', ' ']);
                let known = all(index).any(|(_, n)| n.kind == Kind::Bean && n.name == l.value);
                if single && !known && !beans::INFRASTRUCTURE.contains(&l.value.as_str()) {
                    out.push(
                        Diagnostic::new(l.range, Severity::Warning, format!("No bean named '{}' appears in the supported literal source index; imported and runtime context remains unresolved.", l.value))
                            .code("unknown-bean"),
                    );
                }
            }
            _ => {}
        }
    }
    out
}

/// A `<chunk>` needs a `commit-interval` or a `chunk-completion-policy` (Spring Batch reference, "Configuring a Step"). A step that inherits (`parent`) or is `abstract` may get it from the other side of the inheritance, so it is left alone.
fn chunk_problems(text: &str) -> Vec<Diagnostic> {
    if !text.contains(NAMESPACE) || !text.contains("chunk") {
        return Vec::new();
    }
    let els = xml::elements(text);
    let inherits = |i: usize| {
        let mut at = Some(i);
        while let Some(e) = at.map(|a| &els[a]) {
            if e.local() == "step" && (e.attr("parent").is_some() || e.attr("abstract").is_some()) {
                return true;
            }
            at = e.parent;
        }
        false
    };
    els.iter()
        .enumerate()
        .filter(|(i, e)| e.local() == "chunk" && xml::namespace(&els, *i) == Some(NAMESPACE))
        .filter(|(_, e)| e.attr("commit-interval").is_none() && e.attr("chunk-completion-policy").is_none())
        .filter(|(i, _)| !inherits(*i))
        .map(|(_, e)| {
            Diagnostic::new(
                range(text, e.at, e.at + e.name.len()),
                Severity::Warning,
                "A chunk needs a commit-interval or a chunk-completion-policy; without either the step fails to start.",
            )
            .code("chunk-missing-commit-interval")
        })
        .collect()
}

fn config_problems(doc: &Document, index: &Names<'_>) -> Vec<Diagnostic> {
    if !index.fresh() || all(index).any(|(_,name)|name.kind==Kind::UnresolvedJob) {
        return Vec::new();
    }
    job_name_items(&doc.path, &doc.text)
        .into_iter()
        .filter(|(n, _, _)| !n.is_empty() && !beans::is_expression(n))
        .filter(|(n, _, _)| !all(index).any(|(_, x)| x.kind == Kind::Job && x.name == *n))
        .map(|(n, from, to)| {
            Diagnostic::new(range(&doc.text, from, to), Severity::Warning, format!("No supported literal job named '{n}' was found in the source index; factory/imported/runtime contexts are unresolved."))
                .code("unknown-job")
        })
        .collect()
}

impl Providers for Batch {}

impl Batch {
    pub fn completion(&mut self, doc: &Document, at: Position, source: &mut impl Source) -> Vec<CompletionItem> {
        if let Some(caret) = offset(&doc.text, at)
            && let Some((from, to)) = parameter_key_at(&doc.text, caret)
            && matches!(file_kind(doc), Some("xml" | "java" | "kotlin"))
        {
            if !parameter_context(&doc.path,&doc.text,from,to) {return Vec::new();}
            let Some(index) = source.for_document(doc) else { return Vec::new() };
                let index = &index;
            let typing = range(&doc.text, from, to);
            let keys = all(index).filter(|(p, n)| n.kind == Kind::JobParam && !(*p == doc.path && n.range == typing));
            return completions(keys.map(|(_, n)| n.name.as_str()), "job parameter", Some(typing));
        }
        match file_kind(doc) {
            Some("xml") => {
                let Some((link, names)) = xml_link(doc, at) else { return Vec::new() };
                let Some(index) = source.for_document(doc) else { return Vec::new() };
                let index = &index;
                let (kinds, detail) = match link.kind {
                    LinkKind::Target => (&[Kind::State][..], "step, flow, split or decision"),
                    LinkKind::Parent => (&[Kind::State, Kind::Job][..], "step, flow or job"),
                    LinkKind::Bean => (&[Kind::Bean][..], "bean"),
                };
                let local = names
                    .iter()
                    .filter(|n| kinds.contains(&n.kind) && (link.kind != LinkKind::Target || n.scope == link.scope));
                let elsewhere = elsewhere(index, doc)
                    .into_iter()
                    .map(|(_, n)| n)
                    .filter(|n| link.kind != LinkKind::Target && kinds.contains(&n.kind));
                completions(local.chain(elsewhere).map(|n| n.name.as_str()), detail, Some(link.range))
            }
            Some("config") => {
                let caret = offset(&doc.text, at).unwrap_or(0);
                let item = job_name_items(&doc.path, &doc.text).into_iter().find(|(_, from, to)| *from <= caret && caret <= *to);
                // An empty YAML value ends at its colon: a caret after it on the line still counts.
                let on_key = config::keys(&doc.path, &doc.text).into_iter().any(|k| {
                    config::canonical(&k.key) == config::canonical(JOB_NAME_KEY)
                        && k.end <= caret
                        && doc.text.get(k.end..caret).is_some_and(|s| !s.contains('\n'))
                });
                if item.is_none() && !on_key {
                    return Vec::new();
                }
                let replace = item.map(|(_, from, to)| range(&doc.text, from, to));
                let Some(index) = source.for_document(doc) else { return Vec::new() };
                let index = &index;
                completions(all(index).filter(|(_, n)| n.kind == Kind::Job).map(|(_, n)| n.name.as_str()), "batch job", replace)
            }
            _ => Vec::new(),
        }
    }

    pub fn definition(&mut self, doc: &Document, at: Position, source: &mut impl Source) -> Vec<Location> {
        match file_kind(doc) {
            Some("xml") => {
                let Some((link, names)) = xml_link(doc, at) else { return Vec::new() };
                let Some(index) = source.for_document(doc) else { return Vec::new() };
                let index = &index;
                targets(&link, &names, &elsewhere(index, doc), &doc.path)
            }
            Some("config") => {
                let caret = offset(&doc.text, at).unwrap_or(0);
                let Some((name, _, _)) = job_name_items(&doc.path, &doc.text).into_iter().find(|(_, f, t)| *f <= caret && caret <= *t)
                else {
                    return Vec::new();
                };
                let Some(index) = source.for_document(doc) else { return Vec::new() };
                let index = &index;
                locations(all(index).filter(|(_, n)| n.kind == Kind::Job && n.name == name))
            }
            _ => Vec::new(),
        }
    }

}

const VIEW: &str = "diagram";

fn location_key(path: &str, range: Range) -> String {
    format!("b:{}:{}:{path}", range.start.line, range.start.character)
}

/// Jobs and their steps, flows, splits and decisions as nodes; the job points at its first state, states at the states they lead to.
fn diagram_of(index: &Names<'_>) -> (Vec<Node>, Vec<Edge>) {
    let shown = |n: &&Name| matches!(n.kind, Kind::Job | Kind::State);
    let mut nodes = Vec::new();
    let mut edges = BTreeSet::new();
    for (path, names) in index.iter() {
        let id = |n: &Name| location_key(path, n.range);
        for n in names.iter().filter(shown) {
            nodes.push(Node::new(&id(n), &n.name).detail(n.detail).accent(n.kind == Kind::Job));
            for (target, label) in &n.goes {
                let found = names.iter().find(|t| t.kind == Kind::State && t.scope == n.scope && t.name == *target);
                if let Some(t) = found {
                    edges.insert((id(n), id(t), label.clone()));
                }
            }
            if n.kind == Kind::Job || n.detail == "flow" {
                if let Some(first) = names.iter().find(|t| t.kind == Kind::State && t.scope == Some(n.el)) {
                    edges.insert((id(n), id(first), None));
                }
            }
        }
    }
    let edges = edges.into_iter().map(|(a, b, label)| {
        let edge = Edge::new(&a, &b);
        match label {
            Some(l) => edge.label(&l),
            None => edge,
        }
    });
    (nodes, edges.collect())
}

impl Batch {
    /// Typed source-model facts for the registered owner's packaged presentation.
    pub fn diagram_snapshot(&mut self, ctx: &Invocation, source: &mut impl Source) -> Result<(Vec<Node>,Vec<Edge>,Vec<(String,Location)>,String),Error> {
        let project=ctx.project().ok_or(Error::Stale)?;
        if project.root()?.is_empty(){return Err(Error::Stale);}
        spring_text::index::scoped_root(&project).ok_or(Error::Stale)?;
        let index=source.project(&project,COMMAND_BUDGET);
        let index=&index;
        if index.scanning(){refresh(&id(VIEW));}
        let (nodes,edges)=diagram_of(index);
        let sources=index.iter().flat_map(|(path,names)|names.iter().filter(|n|matches!(n.kind,Kind::Job|Kind::State)).map(move|name|(location_key(path,name.range),Location{path:path.into(),range:name.range}))).collect();
        let status=format!("{} nodes · {} transitions{}",nodes.len(),edges.len(),if index.complete(){""}else{" · scanning…"});
        Ok((nodes,edges,sources,status))
    }
}

impl Batch {
    pub fn render(&mut self, _view: &str, ctx: &Invocation, source: &mut impl Source) -> Result<View, Error> {
        (|| -> Result<View, Error> {
            let refresh = Button::new("refresh", "Refresh").icon("refresh");
            let Some(project) = ctx.project() else { return Ok(View::message("Open a project to see its batch jobs.").button(refresh)) };
            if spring_text::index::scoped_root(&project).is_none() {return Ok(View::message("This project's folder path can't be read. Open the project from a local folder.").button(refresh));}
            let index = source.project(&project, COMMAND_BUDGET);
            let index = &index;
            let complete = index.complete();
            if index.scanning() {
                self::refresh(&id(VIEW));
            }
            let (nodes, edges) = diagram_of(index);
            let status = format!("{} nodes · {} transitions{}", nodes.len(), edges.len(), if complete { "" } else { " · scanning…" });
            Ok(View::new(Body::Html(diagram::html(&nodes, &edges, "No batch jobs found."))).button(refresh).status(&status))
        })().map(ui_kit::actions::available)
    }

    pub fn handle(&mut self, view: &str, event: ViewEvent, ctx: &Invocation, source: &mut impl Source) -> Result<(), Error> {
        let key = match event {
            ViewEvent::Button(b) if b == "refresh" => {
                if let Some(project) = ctx.project() {
                    source.restart(&project);
                }
                refresh(view);
                return Ok(());
            }
            ViewEvent::Activate(key) | ViewEvent::Message(key) => key,
            _ => return Ok(()),
        };
        let project = ctx.project().ok_or(Error::Stale)?;
        let root = project.root()?;
        if root.is_empty() { return Err(Error::Stale); }
        spring_text::scope::scope_path(&root).ok_or(Error::Stale)?;
        let index = source.project(&project, COMMAND_BUDGET);
        let index = &index;
        let (nodes, _) = diagram_of(index);
        if nodes.iter().filter(|node| node.id == key).count() != 1 { return Err(Error::Stale); }
        let mut parts = key.splitn(4, ':');
        let (Some("b"), Some(line), Some(col), Some(path)) = (parts.next(), parts.next(), parts.next(), parts.next()) else { return Err(Error::Stale); };
        let (line, col) = (line.parse::<u32>().map_err(|_| Error::Stale)?, col.parse::<u32>().map_err(|_| Error::Stale)?);
        if path.len() > 4096 || path.chars().any(char::is_control)
            || !path.starts_with(&format!("{}/", root.trim_end_matches('/')))
            || host::project_of(path).and_then(|p| p.root().ok()).as_deref() != Some(root.as_str()) { return Err(Error::Stale); }
        let expected: Vec<_> = index.iter().filter(|(p,_)| *p == path).flat_map(|(_,names)| names)
            .filter(|name| matches!(name.kind, Kind::Job | Kind::State) && location_key(path,name.range) == key).collect();
        if expected.len() != 1 { return Err(Error::Stale); }
        let text = project.read_file(path)?;
        if text.len() > 512 * 1024 { return Err("Selected Batch source exceeds the navigation bound.".into()); }
        if !index_file(path,&text).is_some_and(|names| names.contains(expected[0]))
            || host::project_of(path).and_then(|p| p.root().ok()).as_deref() != Some(root.as_str()) { return Err(Error::Stale); }
        host::open_location(path,line,col)
    }
}

impl Plugin for Batch {
    fn run_command(&mut self, id: &str, _ctx: &Invocation, _arg: Option<&str>) -> Result<Option<String>, Error> {
        Err(Error::NotFound(format!("command {id}")))
    }
}

pub const WINDOW_SECTION: &str = "batch";

const OWNER_WINDOW: &str = "structure";

fn id(name: &str) -> String {
    format!("{WINDOW_SECTION}.{name}")
}

fn window(_view: &str) -> &str {
    OWNER_WINDOW
}

pub(crate) fn refresh(view: &str) {
    host::refresh_view(window(view));
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    use openidea_plugin::text::position;
    use openidea_plugin::views::{Viewport, ViewEvent};
    use spring_text::index::{Indexes, PROVIDER_BUDGET};

    /// The module over an index of its own facts, as the owner's shared index is for it.
    struct Batch {
        inner: super::Batch,
        indexes: Indexes<Vec<Name>>,
    }

    impl Default for Batch {
        fn default() -> Self {
            Self { inner: super::Batch, indexes: Indexes::new(&beans::GLOBS, index_file).excluding_nested_projects() }
        }
    }

    struct Own<'a>(&'a mut Indexes<Vec<Name>>);

    impl Source for Own<'_> {
        fn for_document(&mut self, doc: &Document) -> Option<Names<'_>> {
            spring_text::index::owning_root(&doc.path)?;
            Some(self.0.for_document(&doc.path, &doc.text, PROVIDER_BUDGET)?.view(|names| Some(names)))
        }

        fn cached(&mut self, doc: &Document) -> Option<Names<'_>> {
            spring_text::index::owning_root(&doc.path)?;
            Some(self.0.for_cached_document(&doc.path, &doc.text)?.view(|names| Some(names)))
        }

        fn project(&mut self, project: &Project, budget: std::time::Duration) -> Names<'_> {
            self.0.get(project, budget).view(|names| Some(names))
        }

        fn restart(&mut self, project: &Project) {
            let index = self.0.get(project, std::time::Duration::ZERO);
            if index.complete() && !index.scanning() {
                self.0.reset();
            }
        }
    }

    impl Batch {
        fn completion(&mut self, doc: &Document, at: Position, _trigger: Option<&str>) -> Vec<CompletionItem> {
            self.inner.completion(doc, at, &mut Own(&mut self.indexes))
        }

        fn definition(&mut self, doc: &Document, at: Position) -> Vec<Location> {
            self.inner.definition(doc, at, &mut Own(&mut self.indexes))
        }

        fn render(&mut self, view: &str, _viewport: Option<Viewport>, ctx: &Invocation) -> Result<View, Error> {
            self.inner.render(view, ctx, &mut Own(&mut self.indexes))
        }

        fn handle(&mut self, view: &str, event: ViewEvent, ctx: &Invocation) -> Result<(), Error> {
            self.inner.handle(view, event, ctx, &mut Own(&mut self.indexes))
        }
    }

    const JOB_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<beans:beans xmlns="http://www.springframework.org/schema/batch"
             xmlns:beans="http://www.springframework.org/schema/beans">
    <job id="importJob" job-repository="jobRepository">
        <step id="load" next="check">
            <tasklet transaction-manager="transactionManager">
                <chunk reader="csvReader" writer="dbWriter" processor="missingProcessor" commit-interval="10"/>
            </tasklet>
        </step>
        <decision id="check" decider="importDecider">
            <next on="FAILED" to="report"/>
            <next on="*" to="lost"/>
        </decision>
        <step id="report" parent="baseStep">
            <tasklet ref="reportTasklet"/>
            <stop on="RETRY" restart="load"/>
        </step>
    </job>
    <step id="baseStep" abstract="true"/>
    <beans:bean id="csvReader" class="com.acme.CsvReader"/>
</beans:beans>
"#;
    const JOB_CONFIG: &str = r#"package com.acme;

import org.springframework.batch.core.configuration.annotation.EnableBatchProcessing; import org.springframework.context.annotation.Bean; import org.springframework.context.annotation.Configuration;
import org.springframework.batch.core.configuration.annotation.JobBuilderFactory; import org.springframework.batch.core.job.builder.JobBuilder;

@Configuration
@EnableBatchProcessing
public class BatchConfig {
    @Autowired private JobBuilderFactory jobBuilderFactory;

    @Bean
    public Job exportJob(JobRepository repo, Step step) {
        return new JobBuilder("exportJob", repo).start(step).build();
    }

    @Bean
    public Job legacyJob(JobRepository repo) {
        return new JobBuilder("legacyJob", repo).start(step()).build();
    }

    @Bean ItemWriter<Row> dbWriter() { return null; }
    @Bean Tasklet reportTasklet() { return null; }
    @Bean JobExecutionDecider importDecider() { return null; }
}
"#;
    const APP: &str = "package com.acme; import org.springframework.boot.autoconfigure.SpringBootApplication;\n\n@SpringBootApplication\npublic class App {}\n";
    const XML_PATH: &str = "/project/src/main/resources/jobs/import.xml";
    const JAVA_PATH: &str = "/project/src/main/java/com/acme/BatchConfig.java";

    fn setup() -> MockHost {
        let host = MockHost::install();
        host.add_file("src/main/resources/jobs/import.xml", JOB_XML);
        host.add_file("src/main/java/com/acme/BatchConfig.java", JOB_CONFIG);
        host.add_file("src/main/java/com/acme/App.java", APP);
        host
    }
    #[test]
    fn batch_xml_attributes_and_ancestors_require_source_namespace_ownership() {
        let prefix="<beans xmlns='http://www.springframework.org/schema/beans' xmlns:b='http://www.springframework.org/schema/batch' xmlns:q='urn:foreign'>";
        let (names,links)=parse_xml(&format!("{prefix}<b:job id='j'><b:step id='one' q:next='missing' q:parent='missing' q:ref='missing'/></b:job></beans>"));
        assert!(links.is_empty());assert_eq!(names.iter().filter(|n|n.kind==Kind::State).count(),1);
        let (names,links)=parse_xml(&format!("{prefix}<q:job id='foreign'><b:step id='one' next='missing'/></q:job></beans>"));
        assert!(links.is_empty());assert!(names.iter().all(|n|n.kind!=Kind::State));
        let (names,links)=parse_xml(&format!("{prefix}<b:job id='j'><b:step id='one' next='two'/><b:step id='two'/></b:job></beans>"));
        assert_eq!(links.len(),1);assert_eq!(links[0].value,"two");assert_eq!(names.iter().filter(|n|n.kind==Kind::State).count(),2);
    }
    #[test]
    fn cached_batch_xml_requires_the_same_complete_envelope_as_the_owner() {
        let prefix="<beans xmlns='http://www.springframework.org/schema/beans' xmlns:b='http://www.springframework.org/schema/batch'>";
        for text in [format!("{prefix}<b:job id='j'/>") ,format!("{prefix}<b:job id='j'/></beans><other/>")] {
            let (names,links)=parse_xml(&text);assert!(names.is_empty() && links.is_empty(),"{text}");
        }
        let (names,links)=parse_xml(&format!("{prefix}<b:job id='j'><b:step id='a' next='b'/><b:step id='b'/></b:job></beans>"));
        assert!(names.iter().any(|name|name.kind==Kind::Job && name.name=="j"));assert_eq!(links.len(),1);
        let (names,_)=parse_xml(&format!("<!DOCTYPE beans>{prefix}<b:job id='j'/><!-- a &lt; b --></beans>"));
        assert!(names.iter().any(|name|name.kind==Kind::Job && name.name=="j"),"a DOCTYPE and entities keep the file analysable");
    }

    fn doc(path: &str, language: &str, text: &str) -> Document {
        Document { path: path.into(), language: language.into(), version: 1, text: text.into() }
    }

    fn at(text: &str, needle: &str) -> Position {
        position(text, text.find(needle).unwrap() + 1)
    }

    fn after(text: &str, needle: &str) -> Position {
        position(text, text.find(needle).unwrap() + needle.len())
    }

    #[test]
    fn batch_bean_and_boot_source_facts_require_owned_literal_annotations() {
        let positive="import org.springframework.context.annotation.Bean; import org.springframework.boot.autoconfigure.SpringBootApplication; @SpringBootApplication class App { @Bean(name={\"one\",\"two\"}) Object make(){return null;} }";
        let facts=parse_source(JAVA_PATH,positive);
        assert_eq!(facts.iter().filter(|n|n.kind==Kind::Bean).map(|n|n.name.as_str()).collect::<Vec<_>>(),["app","one","two"]);
        assert!(facts.iter().any(|n|n.kind==Kind::BootApp));
        for source in ["import fake.Bean; import fake.SpringBootApplication; @SpringBootApplication class App { @Bean Object ghost(){return null;} }","import org.springframework.context.annotation.Bean; class Jobs { @Bean(name=\"ghost\"+suffix) Object make(){return null;} }"] {
            let facts=parse_source(JAVA_PATH,source);
            assert!(facts.iter().all(|n|!matches!(n.kind,Kind::Bean|Kind::BootApp)));
            assert!(facts.iter().any(|n|n.kind==Kind::UnresolvedBean));
        }
    }

    #[test]
    fn builder_constructor_facts_require_exact_origin_and_literal_argument() {
        for source in [
            "import org.springframework.batch.core.job.builder.JobBuilder; class Jobs { Object job(){return new JobBuilder(\"actual\", repo);} }",
            "class Jobs { Object job(){return new org.springframework.batch.core.job.builder.JobBuilder(\"actual\", repo);} }",
        ] {
            let facts=parse_source(JAVA_PATH,source);
            let job=facts.iter().find(|n|n.kind==Kind::Job).expect("owned constructor");
            assert_eq!(job.name,"actual");
            assert_eq!(job.range,range(source,source.find("actual").unwrap(),source.find("actual").unwrap()+6));
        }
        for source in [
            "import org.springframework.batch.core.job.builder.JobBuilder; class JobBuilder {} class Jobs { Object job(){return new JobBuilder(\"ghost\");} }",
            "import org.springframework.batch.core.job.builder.JobBuilder; class Jobs { Object job(){return new JobBuilder(\"ghost\" + suffix, repo);} }",
            "class Jobs { Object job(){return rogueJobBuilder.get(\"ghost\");} }",
        ] {
            let facts=parse_source(JAVA_PATH,source);
            assert!(!facts.iter().any(|n|n.kind==Kind::Job));
            assert!(facts.iter().any(|n|n.kind==Kind::UnresolvedJob));
        }
    }

    #[test]
    fn reads_jobs_states_and_links() {
        let (names, links) = parse_xml(JOB_XML);
        let named = |kind| names.iter().filter(|n| n.kind == kind).map(|n| n.name.as_str()).collect::<Vec<_>>();
        assert_eq!(named(Kind::Job), ["importJob"]);
        assert_eq!(named(Kind::State), ["load", "check", "report", "baseStep"]);
        assert_eq!(named(Kind::Bean), ["importJob", "baseStep", "csvReader"]);
        let targets: Vec<_> = links.iter().filter(|l| l.kind == LinkKind::Target).map(|l| l.value.as_str()).collect();
        assert_eq!(targets, ["check", "report", "lost", "load"]);
        assert!(parse_xml("<beans><job id=\"x\"/></beans>").0.is_empty());

        let java: Vec<_> = parse_source(JAVA_PATH, JOB_CONFIG).into_iter().filter(|n| n.kind == Kind::Job).map(|n| n.name).collect();
        assert_eq!(java, ["exportJob", "legacyJob"], "builder names, not bean names");
    }

    #[test]
    fn navigates_transitions_parents_and_beans() {
        let _host = setup();
        let mut b = Batch::default();
        let d = doc(XML_PATH, "xml", JOB_XML);
        let line = |found: Vec<Location>| found.iter().map(|l| (l.path.rsplit('/').next().unwrap().to_owned(), l.range.start.line)).collect::<Vec<_>>();
        assert_eq!(line(b.definition(&d, at(JOB_XML, "\"check\""))), [("import.xml".into(), 9)]);
        assert_eq!(line(b.definition(&d, at(JOB_XML, "\"load\"/>"))), [("import.xml".into(), 4)]);
        assert_eq!(line(b.definition(&d, at(JOB_XML, "\"baseStep\""))), [("import.xml".into(), 18)]);
        assert_eq!(line(b.definition(&d, at(JOB_XML, "\"dbWriter\""))), [("BatchConfig.java".into(), 20)]);
        assert_eq!(line(b.definition(&d, at(JOB_XML, "\"csvReader\""))), [("import.xml".into(), 19)]);
    }

    #[test]
    fn modern_operator_and_additional_schema_bean_refs_are_indexed_at_exact_source_ranges() {
        let text=format!("<beans xmlns='http://www.springframework.org/schema/beans' xmlns:b='{NAMESPACE}' xmlns:f='urn:foreign'><b:job id='j' incrementer='sequence'><b:step id='s'><b:job ref='nested' job-operator='operator' job-parameters-extractor='extractor' f:job-operator='foreign'/></b:step><b:step id='p'><b:partition handler='handler' aggregator='aggregator'/></b:step></b:job></beans>");
        let (_,links)=parse_xml(&text);let values:Vec<_>=links.iter().filter(|l|l.kind==LinkKind::Bean).map(|l|l.value.as_str()).collect();assert_eq!(values,["sequence","nested","operator","extractor","handler","aggregator"]);
        for link in links {let from=offset(&text,link.range.start).unwrap();let to=offset(&text,link.range.end).unwrap();assert_eq!(&text[from..to],link.value);}
    }
    #[test]
    fn completes_states_and_beans() {
        let _host = setup();
        let mut b = Batch::default();
        let d = doc(XML_PATH, "xml", JOB_XML);
        let labels = |items: Vec<CompletionItem>| items.into_iter().map(|i| i.label).collect::<Vec<_>>();
        assert_eq!(labels(b.completion(&d, at(JOB_XML, "\"lost\""), None)), ["check", "load", "report"]);
        let beans = labels(b.completion(&d, at(JOB_XML, "\"csvReader\""), None));
        assert!(beans.contains(&"dbWriter".to_owned()) && beans.contains(&"csvReader".to_owned()) && beans.contains(&"app".to_owned()));
    }

    fn chunk_codes(text: &str) -> Vec<(u32, u32)> {
        chunk_problems(text).iter().map(|d| (d.range.start.line, d.range.start.character)).collect()
    }

    const CHUNKLESS: &str = "<job id=\"j\" xmlns=\"http://www.springframework.org/schema/batch\">\n  <step id=\"é\"><tasklet><chunk reader=\"r\" writer=\"w\"/></tasklet></step>\n</job>";

    #[test]
    fn chunk_missing_commit_interval_warns() {
        assert_eq!(chunk_codes(CHUNKLESS), [(1, 25)]);
    }

    #[test]
    fn chunk_missing_commit_interval_is_quiet_for_library_or_dynamic_name() {
        assert!(chunk_codes(&CHUNKLESS.replace("reader=", "commit-interval=\"${size}\" reader=")).is_empty());
        assert!(chunk_codes(&CHUNKLESS.replace("reader=", "chunk-completion-policy=\"p\" reader=")).is_empty());
        assert!(chunk_codes(&CHUNKLESS.replace("id=\"é\"", "id=\"é\" parent=\"base\"")).is_empty());
        assert!(chunk_codes(&CHUNKLESS.replace("id=\"é\"", "id=\"é\" abstract=\"true\"")).is_empty());
        assert!(chunk_codes(&CHUNKLESS.replace("http://www.springframework.org/schema/batch", "urn:other")).is_empty());
    }

    const BROKEN: &str = "<job xmlns=\"http://www.springframework.org/schema/batch\" restartable=\"yes\">\n  <step><tasklet allow-start-if-complete=\"maybe\"/></step>\n  <decision id=\"d\"><next on=\"*\"/><end/><stop on=\"X\"/></decision>\n</job>";

    #[test]
    fn xml_rules_report_missing_attributes_and_wrong_booleans() {
        let found: Vec<_> = xml_rules::problems(BROKEN)
            .iter()
            .map(|d| (d.code.clone().unwrap(), BROKEN.lines().nth(d.range.start.line as usize).unwrap()[d.range.start.character as usize..d.range.end.character as usize].to_owned()))
            .collect();
        assert_eq!(found, [
            ("missing-attribute".to_owned(), "job".to_owned()),
            ("invalid-attribute-value".into(), "yes".into()),
            ("missing-attribute".into(), "step".into()),
            ("invalid-attribute-value".into(), "maybe".into()),
            ("missing-attribute".into(), "decision".into()),
            ("missing-attribute".into(), "next".into()),
            ("missing-attribute".into(), "end".into()),
            ("missing-attribute".into(), "stop".into()),
        ]);
    }

    #[test]
    fn xml_rules_are_quiet_for_valid_dynamic_and_foreign_elements() {
        assert!(xml_rules::problems(JOB_XML).is_empty());
        let dynamic = BROKEN.replace("restartable=\"yes\"", "id=\"j\" restartable=\"${r}\"").replace("<step>", "<step id=\"s\">").replace("maybe", "true");
        let codes: Vec<_> = xml_rules::problems(&dynamic).iter().map(|d| d.message.clone()).collect();
        assert!(codes.iter().all(|m| m.contains("decider") || m.contains("\"to\"") || m.contains("\"on\"") || m.contains("restart")), "{codes:?}");
        assert!(xml_rules::problems(&BROKEN.replace("http://www.springframework.org/schema/batch", "urn:other")).is_empty());
    }

    const PARAMS_JAVA: &str = "package a;\nimport org.springframework.batch.core.JobParametersBuilder; import org.springframework.beans.factory.annotation.Value;\nclass P {\n  @Value(\"#{jobParameters['inputFile']}\") String f;\n  void go() { new JobParametersBuilder().addString(\"runId\", \"1\").addLong(\"size\", 2L); }\n}\n";

    #[test]
    fn parameter_caret_context_reuses_fact_admission_and_accepts_empty_keys() {
        for key in ["","é","in"] {
            let source=format!("import org.springframework.beans.factory.annotation.Value; class P {{ @Value(\"#{{jobParameters['{key}']}}\") String value; }}");
            let from=source.find("['").unwrap()+2;assert!(parameter_context(JAVA_PATH,&source,from,from+key.len()));
            let xml=format!("<beans xmlns='http://www.springframework.org/schema/beans'><bean id='reader'><property name='file' value=\"#{{jobParameters['{key}']}}\"/></bean></beans>");
            let from=xml.find("['").unwrap()+2;assert!(parameter_context(XML_PATH,&xml,from,from+key.len()));
        }
        let foreign="import fake.Value; class P { @Value(\"#{jobParameters['g']}\") String value; }";let from=foreign.find("['").unwrap()+2;
        assert!(!parameter_context(JAVA_PATH,foreign,from,from+1));
        assert!(!parameter_context(JAVA_PATH,"/* jobParameters['g'] */",18,19));
    }

    #[test]
    fn xml_parameter_keys_require_complete_owned_attribute_context() {
        let valid="<beans xmlns='http://www.springframework.org/schema/beans'><bean id='reader'><property name='file' value=\"#{jobParameters['input']}\"/></bean></beans>";
        let facts=job_parameters(XML_PATH,valid);assert_eq!(facts.len(),1);assert_eq!(facts[0].name,"input");assert_eq!(facts[0].range,range(valid,valid.find("input").unwrap(),valid.find("input").unwrap()+5));
        for invalid in [valid.replace("value=","q:value=").replace("<beans ","<beans xmlns:q='urn:foreign' "),valid.replace("schema/beans","schema/foreign"),valid.replace("['input']","['in&#112;ut']"),valid.replace("</beans>",""),"<beans xmlns='http://www.springframework.org/schema/beans'><!-- jobParameters['ghost'] --></beans>".into()] {
            assert!(job_parameters(XML_PATH,&invalid).is_empty(),"{invalid}");
        }
    }

    #[test]
    fn parameter_value_facts_require_owned_complete_literal_expression() {
        let source="class P { @org.springframework.beans.factory.annotation.Value(\"#{jobParameters['input']}\") String value; }";
        let facts=job_parameters(JAVA_PATH,source);assert_eq!(facts.len(),1);assert_eq!(facts[0].name,"input");assert_eq!(facts[0].range,range(source,source.find("input").unwrap(),source.find("input").unwrap()+5));
        for source in ["import fake.Value; class P { @Value(\"#{jobParameters['ghost']}\") String value; }","import org.springframework.beans.factory.annotation.Value; class P { @Value(\"#{jobParameters['ghost']}\"+suffix) String value; }","class P { /* jobParameters['ghost'] */ String value=\"jobParameters['ghost']\"; }"] {
            assert!(job_parameters(JAVA_PATH,source).is_empty(),"{source}");
        }
    }

    #[test]
    fn parameter_adders_require_owned_contiguous_builder_chain() {
        let valid="class P { void go(){new org.springframework.batch.core.JobParametersBuilder().addString(\"first\",\"v\").addLong(\"second\",1L);} }";
        assert_eq!(job_parameters(JAVA_PATH,valid).iter().map(|n|n.name.as_str()).collect::<Vec<_>>(),["first","second"]);
        for source in ["import fake.JobParametersBuilder; class P {void go(){new JobParametersBuilder().addString(\"ghost\",\"v\");}}","import org.springframework.batch.core.JobParametersBuilder; class P {void go(){other.addString(\"ghost\",\"v\");}}","import org.springframework.batch.core.JobParametersBuilder; class P {void go(){new JobParametersBuilder().other().addString(\"ghost\",\"v\");}}","import org.springframework.batch.core.JobParametersBuilder; class P {void go(){new JobParametersBuilder().addString(\"ghost\"+suffix,\"v\");}}"] {
            assert!(job_parameters(JAVA_PATH,source).is_empty(),"{source}");
        }
    }

    #[test]
    fn completes_job_parameter_keys_in_java_and_xml() {
        let host = setup();
        host.add_file("src/main/java/a/P.java", PARAMS_JAVA);
        let mut b = Batch::default();
        let xml = JOB_XML.replace("reader=\"csvReader\"", "reader=\"#{jobParameters['in']}\"");
        let labels = |items: Vec<CompletionItem>| items.into_iter().map(|i| i.label).collect::<Vec<_>>();
        let d = doc(XML_PATH, "xml", &xml);
        assert_eq!(labels(b.completion(&d, after(&xml, "jobParameters['i"), None)), ["inputFile", "runId", "size"]);
        assert!(labels(b.completion(&d, at(&xml, "\"dbWriter\""), None)).contains(&"csvReader".to_owned()), "bean completion stays");
        let j = doc("/project/src/main/java/a/P.java", "java", PARAMS_JAVA);
        let items = b.completion(&j, after(PARAMS_JAVA, "jobParameters['inp"), None);
        assert_eq!(items.len(), 3);
        assert!(b.completion(&j, after(PARAMS_JAVA, "inputFile'"), None).is_empty(), "closed key");
    }

    #[test]
    fn batch_diagram_has_expected_nodes_and_edges() {
        let host = setup();
        let (nodes, edges) = diagram_of(&Batch::default().indexes.get(&host.invocation().project().unwrap(), COMMAND_BUDGET).view(|names| Some(names)));
        let label_of = |id: &str| nodes.iter().find(|n| n.id == id).map(|n| n.label.as_str()).unwrap_or("?");
        let mut labels: Vec<(&str, Option<&str>)> = nodes.iter().map(|n| (n.label.as_str(), n.detail.as_deref())).collect();
        labels.sort();
        assert_eq!(labels, [
            ("baseStep", Some("step")),
            ("check", Some("decision")),
            ("exportJob", Some("job")),
            ("importJob", Some("job")),
            ("legacyJob", Some("job")),
            ("load", Some("step")),
            ("report", Some("step")),
        ]);
        let mut pairs: Vec<(&str, &str, Option<&str>)> =
            edges.iter().map(|e| (label_of(&e.from), label_of(&e.to), e.label.as_deref())).collect();
        pairs.sort();
        assert_eq!(pairs, [
            ("check", "report", Some("FAILED")),
            ("importJob", "load", None),
            ("load", "check", None),
            ("report", "load", Some("RETRY restart")),
        ]);

        let mut b = Batch::default();
        let view = b.render(VIEW, None, &host.invocation()).unwrap();
        let Body::Html(page) = view.body else { panic!("html expected") };
        let job = position(JOB_XML, JOB_XML.find("importJob").unwrap());
        let job_key = format!("b:{}:{}:{XML_PATH}", job.line, job.character);
        assert!(page.contains(&format!("data-id=\"{job_key}\"")), "{page}");
        assert_eq!(view.status.as_deref(), Some("7 nodes · 4 transitions"));
        b.handle(VIEW, ViewEvent::Message(job_key), &host.invocation()).unwrap();
        assert_eq!(host.opened_locations(), [(XML_PATH.to_string(), job.line, job.character)]);
    }

    #[test]
    fn refresh_during_scan_keeps_the_partial_index() {
        let host = setup();
        let mut b = Batch::default();
        let project = host.invocation().project().unwrap();
        b.indexes.get(&project, std::time::Duration::ZERO).update(XML_PATH, JOB_XML);
        b.handle(VIEW, ViewEvent::Button("refresh".into()), &host.invocation()).unwrap();
        assert_eq!(b.indexes.get(&project, std::time::Duration::ZERO).files().len(), 1, "not reset");

        b.indexes.get(&project, COMMAND_BUDGET);
        b.handle(VIEW, ViewEvent::Button("refresh".into()), &host.invocation()).unwrap();
        assert!(b.indexes.get(&project, std::time::Duration::ZERO).files().is_empty(), "a complete index starts over");
    }

    #[test]
    fn batch_diagram_is_empty_without_jobs() {
        let host = MockHost::install();
        let view = Batch::default().render(VIEW, None, &host.invocation()).unwrap();
        let Body::Html(page) = view.body else { panic!("html expected") };
        assert!(page.contains("No batch jobs found."));
    }

        #[test]
    fn refresh_goes_to_the_owner_window_when_embedded() {
        let host = MockHost::install();
        refresh("diagram");
        assert_eq!(host.refreshed_views(), ["structure"]);
    }

    #[test]
    fn diagram_callback_refuses_unlisted_foreign_and_drifted_source_targets() {
        let host=setup();let mut batch=Batch::default();batch.render(VIEW,None,&host.invocation()).unwrap();
        let job=position(JOB_XML,JOB_XML.find("importJob").unwrap());let valid=location_key(XML_PATH,Range::new(job,job));
        for key in ["b:0:0:/private/secret.xml".to_owned(),format!("b:0:0:{XML_PATH}"),format!("b:no:0:{XML_PATH}")] {
            assert!(batch.handle(VIEW,ViewEvent::Message(key),&host.invocation()).is_err());
        }
        host.add_file("src/main/resources/jobs/import.xml",&JOB_XML.replace("importJob","changedJob"));
        assert!(batch.handle(VIEW,ViewEvent::Message(valid),&host.invocation()).is_err());
        assert!(host.opened_locations().is_empty());
    }
    #[test]
    fn diagram_callback_requires_current_project_source_owner_and_bounded_text() {
        let host=setup();let mut batch=Batch::default();batch.render(VIEW,None,&host.invocation()).unwrap();
        let job=position(JOB_XML,JOB_XML.find("importJob").unwrap());let valid=location_key(XML_PATH,Range::new(job,job));
        host.add_file("src/main/resources/jobs/import.xml",&format!("{JOB_XML}{}"," ".repeat(512*1024)));
        assert!(batch.handle(VIEW,ViewEvent::Message(valid.clone()),&host.invocation()).is_err());
        host.add_file("src/main/resources/jobs/import.xml",JOB_XML);host.set_project_root(Some("/other"));
        assert!(batch.handle(VIEW,ViewEvent::Message(valid),&host.invocation()).is_err());assert!(host.opened_locations().is_empty());
    }

    #[test]
    fn unresolved_transition_never_uses_another_job_or_project_state() {
        let text=format!("<beans xmlns='http://www.springframework.org/schema/beans' xmlns:b='{NAMESPACE}'><b:job id='one'><b:step id='a' next='finish'/></b:job><b:job id='two'><b:step id='finish'/></b:job></beans>");
        let (names,links)=parse_xml(&text);let link=links.iter().find(|l|l.kind==LinkKind::Target).unwrap();
        let unrelated=Name{name:"finish".into(),kind:Kind::State,range:Range::default(),scope:None,el:0,detail:"step",goes:vec![]};
        assert!(targets(link,&names,&[("/project/other.xml",&unrelated)],"/project/jobs.xml").is_empty());
    }

}
