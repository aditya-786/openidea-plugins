//! Spring Modulith Tools for OpenIDEA: finds the application modules of a
//! project (direct sub-packages of the `@SpringBootApplication`/`@Modulithic`
//! package), warns about imports of another module's internal types and of
//! modules a module doesn't allow, and lists the modules in a tool window.
//! Text-based, Java only: imports and proved project-qualified type positions.

use openidea_plugin::providers::{CodeAction, CompletionItem, CompletionKind, Document, Providers};
use openidea_plugin::text::offset;
use openidea_plugin::views::{Body, Button, TreeItem, View, ViewEvent};
use openidea_plugin::{
    Diagnostic, Error, Invocation, Location, NotificationLevel, Position, Project, Range, Severity, TextEdit, host,
};
use spring_text::diagram::{self, Edge, Node};
use spring_text::index::{COMMAND_BUDGET, View as IndexView};
use spring_text::java::{self, Annotation, JavaFile, Str};
use spring_text::tree::Expansion;
use spring_text::{contains, range};
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

mod expose;

const DIAGRAM_VIEW: &str = "diagram";
const COPY_DOCS: &str = "copy-docs";
const TITLE: &str = "Spring Modulith";
const OPEN: &str = "type = ApplicationModule.Type.OPEN";

/// What the model needs of a `package-info.java`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Info {
    /// `@NamedInterface` on the package: its name.
    pub named_interface: Option<String>,
    /// `@ApplicationModule` on the package.
    pub module: Option<ModuleDecl>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModuleDecl {
    pub display_name: Option<String>,
    pub allowed: Option<Vec<String>>,
    pub open: bool,
    pub range: Range,
    /// Adds `type = OPEN` when the annotation has no `type`.
    pub open_fix: Option<TextEdit>,
}

/// What the model needs of one Java file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Unit {
    pub package: String,
    pub imports: Vec<String>,
    /// Qualified type-position candidates, checked against declared project types.
    pub qualified_uses: Vec<String>,
    pub public_types:BTreeSet<String>,
    /// Unsupported Kotlin aliases/metadata cannot establish absence facts.
    pub uncertain:bool,
    /// Top-level types: name, name range, `@NamedInterface` name.
    pub types: Vec<(String, Range, Option<String>)>,
    /// Application root packages declared here (`@SpringBootApplication`, `@Modulithic` + `additionalPackages`).
    pub roots: Vec<String>,
    pub info: Option<Info>,
}

fn last_segment(package: &str) -> &str {
    package.rsplit('.').next().unwrap_or(package)
}

/// A `@NamedInterface`'s name: its value, else the package's last segment.
fn interface_name(a: &Annotation, package: &str) -> String {
    a.strings(&["value", "name"]).first().map_or_else(|| last_segment(package).to_owned(), |s| s.value.clone())
}

fn module_decl(a: &Annotation, text: &str) -> ModuleDecl {
    let open_fix = a.arg("type").is_none().then(|| {
        let (at, insert) = if text[..a.end].ends_with(')') {
            (a.end - 1, if a.args.is_empty() { OPEN.to_owned() } else { format!(", {OPEN}") })
        } else {
            (a.end, format!("({OPEN})"))
        };
        TextEdit { range: range(text, at, at), text: insert }
    });
    ModuleDecl {
        display_name: a.strings(&["displayName"]).first().map(|s| s.value.clone()),
        allowed: a.arg("allowedDependencies").map(|arg| arg.strings.iter().map(|s| s.value.clone()).collect()),
        open: a.arg("type").is_some_and(|t| t.text.ends_with("OPEN")),
        range: range(text, a.at, a.end),
        open_fix,
    }
}

pub fn scan(path: &str, text: &str) -> Option<Unit> {
    let kotlin=path.ends_with(".kt");
    let f = spring_text::parse_source(path,text);
    if f.package.is_empty() {
        return None;
    }
    let types = f
        .types
        .iter()
        .filter(|t| t.outer.is_none())
        .map(|t| {
            let named = t.annotation("NamedInterface").map(|a| interface_name(a, &f.package));
            (t.name.clone(), range(text, t.at, t.at + t.name.len()), named)
        })
        .collect();
    let mut roots = Vec::new();
    for t in f.types.iter().filter(|t| t.has(&["SpringBootApplication", "Modulithic"])) {
        roots.push(f.package.clone());
        let more = t.annotation("Modulithic").map(|a| a.strings(&["additionalPackages"])).unwrap_or_default();
        roots.extend(more.into_iter().map(|s| s.value.clone()));
    }
    let metadata:Vec<_>=f.types.iter().filter(|t|t.outer.is_none()&&t.has(&["PackageInfo"])).collect();
    let annotations=if path.ends_with("package-info.java") {Some(&f.package_annotations)}else if metadata.len()==1 {Some(&metadata[0].annotations)}else {None};
    let info=annotations.map(|annotations|Info{named_interface:annotations.iter().find(|a|a.name=="NamedInterface").map(|a|interface_name(a,&f.package)),module:annotations.iter().find(|a|a.name=="ApplicationModule").map(|a|module_decl(a,text))});
    let tokens=java::tokens(text);
    let aliases=tokens.iter().any(|t|t.s=="typealias") || tokens.iter().filter(|t|t.s=="import").any(|token| {
        let tail=&text[token.at..];let end=tail.find(['\n','\r',';']).unwrap_or(tail.len());let declaration=java::tokens(&tail[..end]);
        declaration.len()==1||declaration.last().is_some_and(|t|t.s==".")||declaration.iter().any(|t|t.s=="as")
    });
    let uncertain=metadata.len()>1 || kotlin&&(aliases || f.types.iter().flat_map(|t|&t.annotations).filter(|a|matches!(a.name.as_str(),"SpringBootApplication"|"Modulithic"|"NamedInterface"|"ApplicationModule")).flat_map(|a|&a.args).filter(|arg|matches!(arg.name.as_str(),"value"|"name"|"additionalPackages"|"allowedDependencies"|"displayName")).any(|arg| {
        arg.strings.iter().any(|value|value.value.contains('$')||text.get(value.at..value.end)!=Some(value.value.as_str())) || java::tokens(&arg.text).iter().any(|token|token.string().is_none()&&!matches!(token.s,"["|"]"|"{"|"}"|","))
    }));
    let imports = f.imports.iter().map(|i| i.path.clone()).collect();
    let qualified_uses=qualified_type_uses(text,&f).into_iter().map(|(name,_,_)|name).collect();
    let public_types=f.types.iter().filter(|t|t.outer.is_none()&&(t.modifiers.iter().any(|m|m=="public")||kotlin&&!t.modifiers.iter().any(|m|matches!(m.as_str(),"private"|"protected"|"internal")))).map(|t|t.name.clone()).collect();
    Some(Unit { package: f.package, imports, qualified_uses, public_types, uncertain, types, roots, info })
}

/// Tests may reach into modules; Spring Modulith verifies production code.
fn is_test(path: &str) -> bool {
    path.replace('\\', "/").contains("/src/test/")
}

#[derive(Clone, Debug, Default)]
pub struct Module {
    pub name: String,
    pub base: String,
    pub display_name: Option<String>,
    pub open: bool,
    pub allowed: Option<Vec<String>>,
    /// Named interfaces: name and package (its sub-packages included).
    pub interfaces: Vec<(String, String)>,
    /// Types exposed one by one with `@NamedInterface`: interface name and qualified type name.
    pub exposed: Vec<(String, String)>,
    /// The package-info's `@ApplicationModule`, else the first type.
    pub location: Option<Location>,
    /// The package-info edit making the module open.
    pub open_fix: Option<(String, TextEdit)>,
}

impl Module {
    fn interface_names(&self) -> BTreeSet<&str> {
        self.interfaces.iter().chain(&self.exposed).map(|(n, _)| n.as_str()).collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Violation {
    /// A type outside the module's API and named interfaces.
    Internal,
    /// A dependency the using module's `allowedDependencies` doesn't list: `b` or `b::iface`.
    NotAllowed(String),
}

#[derive(Default)]
pub struct Model {
    pub roots: Vec<String>,
    pub modules: BTreeMap<String, Module>,
    packages: BTreeSet<String>,
    declared_types: BTreeSet<String>,
    uncertain:bool,
}

impl Model {
    pub fn build(index: &Units<'_>) -> Self {
        let units: Vec<(&str, &Unit)> = index.iter().filter(|(p, _)| !is_test(p)).collect();
        let mut metadata=BTreeMap::<&str,usize>::new();for (_,unit) in &units {if unit.info.as_ref().is_some_and(|info|info.module.is_some()||info.named_interface.is_some()) {*metadata.entry(&unit.package).or_default()+=1;}}
        let mut model = Model {
            roots: units.iter().find(|(_, u)| !u.roots.is_empty()).map(|(_, u)| u.roots.clone()).unwrap_or_default(),
            uncertain:metadata.values().any(|count|*count>1),
            ..Model::default()
        };
        for (path, u) in units {
            model.uncertain|=u.uncertain;
            model.packages.insert(u.package.clone());
            let Some((name, base)) = model.module_of(&u.package) else { continue };
            let m = model.modules.entry(name.clone()).or_insert_with(|| Module { name, base, ..Module::default() });
            for (ty, r, named) in &u.types {
                model.declared_types.insert(format!("{}.{ty}",u.package));
                m.location.get_or_insert_with(|| Location { path: path.into(), range: *r });
                if let Some(n) = named {
                    m.exposed.push((n.clone(), format!("{}.{ty}", u.package)));
                }
            }
            let Some(info) = &u.info else { continue };
            if let Some(n) = &info.named_interface {
                m.interfaces.push((n.clone(), u.package.clone()));
            }
            if let Some(d) = info.module.as_ref().filter(|_| u.package == m.base) {
                m.display_name = d.display_name.clone();
                m.allowed = d.allowed.clone();
                m.open = d.open;
                m.location = Some(Location { path: path.into(), range: d.range });
                m.open_fix = d.open_fix.clone().map(|e| (path.to_owned(), e));
            }
        }
        model
    }

    /// The module holding `package`: its name and base package.
    pub fn module_of(&self, package: &str) -> Option<(String, String)> {
        self.roots.iter().find_map(|root| {
            let name = package.strip_prefix(root.as_str())?.strip_prefix('.')?.split('.').next()?;
            Some((name.to_owned(), format!("{root}.{name}")))
        })
    }

    /// The module an import of module `from` reaches into, and what's wrong with it.
    pub fn check(&self, from: &str, import: &str) -> Option<(&Module, Option<Violation>)> {
        if self.uncertain {return None;}
        let segments: Vec<&str> = import.split('.').filter(|s| *s != "*").collect();
        let split = segments.iter().position(|s| !s.starts_with(|c: char| c.is_lowercase())).unwrap_or(segments.len());
        let package = segments[..split].join(".");
        if !self.packages.contains(&package) {
            return None;
        }
        let (name, _) = self.module_of(&package)?;
        let b = self.modules.get(&name).filter(|b| b.name != from)?;
        let qualified = segments.get(split).map(|ty| format!("{package}.{ty}"));
        let interface = b
            .interfaces
            .iter()
            .find(|(_, p)| package == *p || package.starts_with(&format!("{p}.")))
            .or_else(|| b.exposed.iter().find(|(_, q)| Some(q) == qualified.as_ref()))
            .map(|(n, _)| n.as_str())
            .or_else(|| (package == b.base || b.open).then_some(""));
        let Some(interface) = interface else { return Some((b, Some(Violation::Internal))) };
        let wanted = if interface.is_empty() { b.name.clone() } else { format!("{}::{interface}", b.name) };
        let allowed = self.modules.get(from).and_then(|a| a.allowed.as_ref()).is_none_or(|list| {
            list.iter().map(|d| d.split_whitespace().collect::<String>()).any(|d| d == wanted || d == format!("{}::*", b.name))
        });
        Some((b, (!allowed).then_some(Violation::NotAllowed(wanted))))
    }

    /// Whether an `allowedDependencies` entry names a module (and interface) of the project.
    fn knows(&self, dependency: &str) -> bool {
        let d: String = dependency.split_whitespace().collect();
        let (module, interface) = d.split_once("::").map_or((d.as_str(), None), |(m, i)| (m, Some(i)));
        self.modules.get(module).is_some_and(|m| interface.is_none_or(|i| i == "*" || m.interface_names().contains(i)))
    }
}

/// Lexical qualified names only in explicit type positions. Candidates do not
/// establish Java resolution: callers require an exact project declaration.
fn qualified_type_uses(text:&str,file:&JavaFile)->Vec<(String,usize,usize)> {
    if text.len()>256*1024 {return Vec::new();}
    let tokens=java::tokens(text);if tokens.len()>32768 {return Vec::new();}
    let mut out=Vec::new();let mut i=0;
    while i<tokens.len() {
        let first=tokens[i];
        if !first.is_ident() || !first.s.starts_with(|c:char|c.is_ascii_lowercase())
            || i>0 && matches!(tokens[i-1].s,"."|"package"|"import"|"static")
            || file.imports.iter().any(|import| import.at<=first.at && first.at<import.end) {i+=1;continue;}
        let mut end=i;let mut parts=vec![first.s];let mut type_end=None;
        while end+2<tokens.len() && tokens[end+1].s=="." && tokens[end+2].is_ident() {
            end+=2;parts.push(tokens[end].s);
            if tokens[end].s.starts_with(|c:char|c.is_ascii_uppercase()) {type_end=Some(end);break;}
            if parts.len()>32 {break;}
        }
        if let Some(last)=type_end {
            let before=i.checked_sub(1).map(|n|tokens[n].s);
            let after=tokens.get(last+1).map(|t|t.s);
            let explicit=matches!(before,Some("new"|"instanceof"|"extends"|"implements"|"throws"|"@"))
                || tokens.get(last+1).is_some_and(|t|t.is_ident() && !matches!(t.s,"class"|"return"))
                || after==Some(".") && tokens.get(last+2).is_some_and(|t|t.s=="class");
            if explicit && parts.len()>1 {out.push((parts.join("."),first.at,tokens[last].end()));}
            i=last+1;
        } else {i+=1;}
        if out.len()>=1024 {break;}
    }
    out
}

/// The problems of a Java file: `(range, module reached, violation)` per import.
fn violations<'m>(model: &'m Model, path: &str, f: &JavaFile, text: &str) -> Vec<(Range, &'m Module, Violation)> {
    let Some((from, _)) = model.module_of(&f.package).filter(|_| !is_test(path)) else { return Vec::new() };
    let mut uses:Vec<_>=f.imports.iter().map(|i|(i.path.clone(),i.at,i.end)).collect();
    uses.extend(qualified_type_uses(text,f).into_iter().filter(|(name,_,_)|model.declared_types.contains(name)));
    let mut out:Vec<_>=uses.into_iter().filter_map(|(name,at,end)| {
        let (b,v)=model.check(&from,&name)?;Some((range(text,at,end),b,v?))
    }).collect();
    if path.ends_with(".java") {out.extend(simple_name_uses(model,&from,f,text));}
    out
}

/// Uses of a type brought in by an exact single-type import: `extends`, `implements`,
/// annotations, generic arguments, parameter/return types, `new`, `Foo.member`.
/// Quiet for wildcard/static imports, names declared or imported twice, and type parameters.
fn simple_name_uses<'m>(model:&'m Model,from:&str,f:&JavaFile,text:&str)->Vec<(Range,&'m Module,Violation)> {
    let imports:Vec<_>=f.imports.iter().filter(|i|!i.is_static&&!i.path.ends_with('*')).collect();
    let mut found:BTreeMap<&str,(&Module,Violation)>=BTreeMap::new();
    for i in &imports {
        let simple=last_segment(&i.path);
        if !simple.starts_with(|c:char|c.is_ascii_uppercase())
            || imports.iter().filter(|o|last_segment(&o.path)==simple).count()>1
            || f.types.iter().any(|t|t.name==simple) {continue;}
        if let Some((b,Some(v)))=model.check(from,&i.path) {found.insert(simple,(b,v));}
    }
    if found.is_empty()||text.len()>256*1024 {return Vec::new();}
    let tokens=java::tokens(text);if tokens.len()>32768 {return Vec::new();}
    let mut shadowed=BTreeSet::new();
    let mut out=Vec::new();
    for (n,t) in tokens.iter().enumerate() {
        let Some((b,v))=found.get(t.s) else {continue};
        let before=n.checked_sub(1).map(|k|tokens[k].s);let after=tokens.get(n+1).map(|t|t.s);
        if f.imports.iter().any(|i|i.at<=t.at&&t.at<i.end)||before==Some(".")||matches!(before,Some("package"|"class"|"interface"|"enum"|"record")) {continue;}
        if matches!(before,Some("<"|","))&&after==Some("extends") {shadowed.insert(t.s);continue;}
        let explicit=matches!(before,Some("extends"|"implements"|"new"|"@"|"<"|"&"|"throws"|"instanceof"|"permits"|"("|"final"|"public"|"private"|"protected"|"static"|"abstract"|"?"|">"))
            ||matches!(after,Some("."|"<"|"["|">"|"..."))||tokens.get(n+1).is_some_and(|t|t.is_ident())||after==Some(",")&&matches!(before,Some("<"|","));
        if explicit&&after!=Some("(")||before==Some("new") {out.push((range(text,t.at,t.end()),*b,v.clone(),t.s));}
    }
    out.into_iter().filter(|o|!shadowed.contains(o.3)).map(|(r,b,v,_)|(r,b,v)).collect()
}

fn message(import: &str, b: &Module, v: &Violation, from: &str) -> String {
    match v {
        Violation::Internal => {
            let what = import.strip_suffix(".*").unwrap_or(last_segment(import));
            format!("`{what}` is internal to module `{}`: use its API package or a named interface.", b.name)
        }
        Violation::NotAllowed(d) => format!("Module `{from}` doesn't list `{d}` in its allowedDependencies."),
    }
}

/// The `allowedDependencies` literals of a `package-info.java`.
fn allowed_literals(f: &JavaFile) -> Vec<&Str> {
    f.package_annotations.iter().chain(f.types.iter().filter(|ty|ty.outer.is_none()&&ty.has(&["PackageInfo"])).flat_map(|ty|&ty.annotations)).filter(|a| a.name == "ApplicationModule").flat_map(|a| a.strings(&["allowedDependencies"])).collect()
}

/// Nothing until the project has been read once: a named interface not read yet would look internal.
fn diagnostics(model: &Model, complete: bool, path: &str, f: &JavaFile, text: &str) -> Vec<Diagnostic> {
    if !complete || model.uncertain {
        return Vec::new();
    }
    let from = model.module_of(&f.package).map(|m| m.0).unwrap_or_default();
    let mut out: Vec<Diagnostic> = violations(model, path, f, text)
        .into_iter()
        .map(|(r, b, v)| {
            let code = if v == Violation::Internal { "module-internal" } else { "module-not-allowed" };
            Diagnostic::new(r, Severity::Warning, message(&text_of(text, r), b, &v, &from)).code(code)
        })
        .collect();
    if path.ends_with("package-info.java")||f.types.iter().any(|ty|ty.outer.is_none()&&ty.has(&["PackageInfo"])) {
        for s in allowed_literals(f).into_iter().filter(|s| !model.knows(&s.value)) {
            let message = format!("`{}` is not an application module or named interface.", s.value);
            out.push(Diagnostic::new(range(text, s.at, s.end), Severity::Warning, message).code("unknown-module"));
        }
    }
    out
}

fn text_of(text: &str, r: Range) -> String {
    let from = offset(text, r.start).unwrap_or(0);
    let to = offset(text, r.end).unwrap_or(from);
    text[from..to].to_owned()
}

/// The module facts of every Java and Kotlin file, as one part of the owner's shared index.
pub type Units<'a> = IndexView<'a, Unit>;

/// Where the owner keeps the shared index: each file is read and parsed once for all modules.
pub trait Source {
    /// The index of the project holding `doc`, with `doc` applied, read on for a provider budget.
    fn for_document(&mut self, doc: &Document) -> Option<Units<'_>>;
    /// Like [`Self::for_document`], without reading any more of the project.
    fn cached(&mut self, doc: &Document) -> Option<Units<'_>>;
    /// The index of `project`, read on for at most `budget`.
    fn project(&mut self, project: &Project, budget: std::time::Duration) -> Units<'_>;
    /// Starts the index over, unless a scan of it is in progress (that would throw away what it has read).
    fn restart(&mut self, project: &Project);
}

impl Providers for ModulithTools {}

pub struct ModulithTools {
    expansion: Expansion,
    decoration_sets:BTreeSet<String>,
    expose:expose::Exposes,
}

impl Default for ModulithTools {
    fn default() -> Self {
        Self { expansion: Expansion::default(),decoration_sets:BTreeSet::new(),expose:Default::default() }
    }
}

impl ModulithTools {
    pub fn expose_actions(&mut self,doc:&Document,selection:Range,source:&mut impl Source)->Vec<openidea_plugin::file_actions::FileAction> {
        if spring_text::index::owning_root(&doc.path).is_none() {return vec![];}let Some(index)=source.cached(doc) else{return vec![];};self.expose.actions(&index,doc,selection)
    }
    pub fn resolve_expose(&mut self,doc:&Document,action:openidea_plugin::file_actions::FileAction)->Result<openidea_plugin::file_actions::FileAction,Error> {self.expose.resolve(doc,action)}
    pub fn warm_project(&mut self, project: &Project, budget: std::time::Duration, source: &mut impl Source) {
        use std::hash::{Hash,Hasher};
        let Ok(root)=project.root() else{return;};let mut hash=std::collections::hash_map::DefaultHasher::new();root.hash(&mut hash);
        let set=format!("modulith.modules.{:016x}",hash.finish());
        let _=host::clear_file_decorations(&set);self.decoration_sets.insert(set.clone());
        if spring_text::scope::scope_path(&root).is_none() {return;}let index=source.project(project,budget);let index=&index;if !index.fresh() || index.len()>1000 {return;}
        let model=Model::build(index);if model.uncertain {return;}let mut badges=BTreeMap::<String,bool>::new();let mut attempted=BTreeSet::new();
        for (path,unit) in index.iter().filter(|(path,_)|!is_test(path)) {
            let Some((name,base))=model.module_of(&unit.package) else{continue;};let Some(module)=model.modules.get(&name) else{continue;};
            let Some(directory)=path.rsplit_once('/').map(|(dir,_)|dir) else{continue;};
            let package_path=unit.package.replace('.',"/");let Some(source)=directory.strip_suffix(&package_path).filter(|prefix|prefix.ends_with('/')) else{continue;};
            let directory=format!("{source}{}",base.replace('.',"/"));
            if !attempted.insert(directory.clone()) {continue;}if attempted.len()>128 {return;}
            if host::project_of(&directory).and_then(|p|p.root().ok()).as_deref()!=Some(root.as_str()) || !directory.starts_with(&format!("{root}/")) {continue;}
            if !project.stat(&directory).is_ok_and(|stat|stat.kind==openidea_plugin::FileKind::Directory) {continue;}
            badges.insert(directory,module.open);
        }
        if badges.len()>128 {return;}
        for (path,open) in badges {
            let item=openidea_plugin::FileDecoration{badge:Some(if open {"O"}else{"C"}.into()),style:if open {"added"}else{"muted"}.into(),tooltip:Some(format!("Spring Modulith: {} module (current declared project source model)",if open {"open"}else{"closed"}))};
            if host::set_file_decoration(&path,&set,&item).is_err() {let _=host::clear_file_decorations(&set);break;}
        }
    }
    pub fn reset_live(&mut self) {
        for set in std::mem::take(&mut self.decoration_sets) {let _=host::clear_file_decorations(&set);}
    }

    /// Cached project model plus current text; no file discovery on the typing path.
    pub fn live_diagnostics(&mut self, doc: &Document, source: &mut impl Source) -> Vec<Diagnostic> {
        if spring_text::index::owning_root(&doc.path).is_none() {return Vec::new();}
        let Some(index) = source.cached(doc) else { return Vec::new() };
        let index = &index;
        if !index.fresh() || index.len() > 1000 { return Vec::new(); }
        let model = Model::build(index);
        diagnostics(&model, true, &doc.path, &spring_text::parse_source(&doc.path,&doc.text), &doc.text)
    }
}

impl ModulithTools {
    pub fn completion(&mut self, doc: &Document, at: Position, source: &mut impl Source) -> Vec<CompletionItem> {
        let f=spring_text::parse_source(&doc.path,&doc.text);
        if !doc.path.ends_with("package-info.java")&&!f.types.iter().any(|ty|ty.outer.is_none()&&ty.has(&["PackageInfo"])) {return Vec::new();}
        let Some(at) = offset(&doc.text, at) else { return Vec::new() };
        let Some(s) = allowed_literals(&f).into_iter().find(|s| s.holds(at)) else { return Vec::new() };
        let Some(index) = source.for_document(doc) else { return Vec::new() };
        let index = &index;
        let model = Model::build(index);if model.uncertain {return Vec::new();}
        let own = model.module_of(&f.package).map(|m| m.0);
        let replace = Some(range(&doc.text, s.at, s.end));
        let mut out = Vec::new();
        for m in model.modules.values().filter(|m| Some(&m.name) != own.as_ref()) {
            let detail = m.display_name.clone().unwrap_or_else(|| m.base.clone());
            out.push(CompletionItem { detail: Some(detail), replace, ..CompletionItem::new(&m.name, CompletionKind::Module) });
            out.push(CompletionItem {detail:Some("all named interfaces".into()),replace,..CompletionItem::new(&format!("{}::*",m.name),CompletionKind::Interface)});
            for i in m.interface_names() {
                let label = format!("{}::{i}", m.name);
                out.push(CompletionItem {
                    detail: Some("named interface".into()),
                    replace,
                    ..CompletionItem::new(&label, CompletionKind::Interface)
                });
            }
        }
        out
    }

    pub fn definition(&mut self,doc:&Document,at:Position,source:&mut impl Source)->Vec<Location> {
        if !matches!(doc.language.as_str(),"java"|"kotlin") {return Vec::new();}
        let file=spring_text::parse_source(&doc.path,&doc.text);if !doc.path.ends_with("package-info.java")&&!file.types.iter().any(|ty|ty.outer.is_none()&&ty.has(&["PackageInfo"])) {return Vec::new();}
        let Some(at)=offset(&doc.text,at) else{return Vec::new();};
        let Some(literal)=allowed_literals(&file).into_iter().find(|s|s.holds(at)) else{return Vec::new();};
        let dependency=literal.value.split_whitespace().collect::<String>();
        let Some(index)=source.for_document(doc) else{return Vec::new();};let index=&index;
        if !index.fresh() {return Vec::new();}
        let model=Model::build(index);if model.uncertain||!model.knows(&dependency) {return Vec::new();}
        let (name,interface)=dependency.split_once("::").map_or((dependency.as_str(),None),|(m,i)|(m,Some(i)));
        let Some(module)=model.modules.get(name) else{return Vec::new();};
        if let Some(interface)=interface.filter(|i|*i!="*") {
            let mut locations=Vec::new();
            for (path,unit) in index.iter().filter(|(path,unit)|!is_test(path)&&model.module_of(&unit.package).is_some_and(|(m,_)|m==name)) {
                if unit.info.as_ref().is_some_and(|info|info.named_interface.as_deref()==Some(interface)) {
                    let declaration=unit.types.iter().find(|(_,_,name)|name.as_deref()==Some(interface)).map(|(_,range,_)|*range).unwrap_or_default();locations.push(Location{path:path.into(),range:declaration});
                } else {for (_,range,named) in &unit.types {if named.as_deref()==Some(interface) {locations.push(Location{path:path.into(),range:*range});}}}
                if locations.len()>64 {return Vec::new();}
            }
            return locations;
        }
        module.location.clone().into_iter().collect()
    }

    pub fn code_actions(&mut self, doc: &Document, selection: Range, source: &mut impl Source) -> Vec<CodeAction> {
        let Some(index) = source.for_document(doc) else { return Vec::new() };
        let index = &index;
        let model = Model::build(index);
        violations(&model, &doc.path, &spring_text::parse_source(&doc.path,&doc.text), &doc.text)
            .into_iter()
            .filter(|(r, _, v)| *v == Violation::Internal && contains(*r, selection.start))
            .filter_map(|(_, b, _)| {
                let (path, edit) = b.open_fix.clone()?;
                Some(CodeAction {
                    title: format!("Make module `{}` open (exposes all its types)", b.name),
                    kind: Some("quickfix".into()),
                    preferred: false,
                    edits: vec![(path, vec![edit])],
                    data: None,
                })
            })
            .collect()
    }
}

/// Activation key of a tree item: the module to open, then what makes the key unique.
fn key(model: &Model, open: &str, rest: &str) -> String {
    let source = model.modules.get(open).and_then(|module| module.location.as_ref()).map(|at| at.path.as_str()).unwrap_or("");
    let encoded: String = source.as_bytes().iter().map(|byte|format!("{byte:02x}")).collect();
    format!("open:{open}#{rest}#source:{encoded}")
}

type Dependencies<'m> = BTreeMap<&'m str, BTreeMap<&'m str, Option<Violation>>>;

/// For each module, the modules it imports from and the worst violation of those imports.
fn dependencies<'m>(model: &'m Model, index: &Units<'_>) -> Dependencies<'m> {
    let mut dependencies: Dependencies = BTreeMap::new();
    for (_, u) in index.iter().filter(|(p, _)| !is_test(p)) {
        let Some((from, _)) = model.module_of(&u.package) else { continue };
        let Some(a) = model.modules.get(&from) else { continue };
        let found = dependencies.entry(a.name.as_str()).or_default();
        for i in u.imports.iter().chain(u.qualified_uses.iter().filter(|name|model.declared_types.contains(*name))) {
            if let Some((b, v)) = model.check(&from, i) {
                let worst = found.entry(b.name.as_str()).or_insert(None);
                if worst.is_none() {
                    *worst = v;
                }
            }
        }
    }
    dependencies
}

fn violation_label(v: &Violation) -> String {
    match v {
        Violation::Internal => "internal types".into(),
        Violation::NotAllowed(d) => format!("{d} not allowed"),
    }
}

/// Modules as nodes, one edge per module importing from another; violations are warning edges.
fn diagram_of(model: &Model, index: &Units<'_>) -> (Vec<Node>, Vec<Edge>) {
    let nodes = model
        .modules
        .values()
        .map(|m| Node::new(&key(&model,&m.name, ""), &m.name).detail(if m.open { "open" } else { "closed" }))
        .collect();
    let edges = dependencies(model, index)
        .into_iter()
        .flat_map(|(from, found)| {
            found.into_iter().map(move |(to, v)| {
                let edge = Edge::new(&key(&model,from, ""), &key(&model,to, "")).warning(v.is_some());
                match v {
                    Some(v) => edge.label(&violation_label(&v)),
                    None => edge,
                }
            })
        })
        .collect();
    (nodes, edges)
}

/// PlantUML component diagram of the modules and their dependencies.
pub fn plantuml(model: &Model, index: &Units<'_>) -> String {
    let mut out = String::from("@startuml\n");
    for m in model.modules.values() {
        out.push_str(&format!("component [{}]\n", m.name));
    }
    for (from, found) in dependencies(model, index) {
        for (to, v) in found {
            match v {
                Some(v) => out.push_str(&format!("[{from}] -[#red]-> [{to}] : {}\n", violation_label(&v))),
                None => out.push_str(&format!("[{from}] --> [{to}]\n")),
            }
        }
    }
    out.push_str("@enduml\n");
    out
}

impl ModulithTools {
    /// Typed source-model facts; no classpath/runtime module resolution is added.
    pub fn diagram_snapshot(&mut self, ctx: &Invocation, source: &mut impl Source) -> Result<(Vec<Node>,Vec<Edge>,Vec<(String,Location)>,String),Error> {
        let project=ctx.project().ok_or(Error::Stale)?;
        if project.root()?.is_empty(){return Err(Error::Stale);}
        spring_text::index::scoped_root(&project).ok_or(Error::Stale)?;let index=source.project(&project,COMMAND_BUDGET);let index=&index;
        if index.scanning(){refresh(&id(DIAGRAM_VIEW));}
        let model=Model::build(index);
        let (nodes,edges)=diagram_of(&model,index);
        let sources=model.modules.values().filter_map(|m|m.location.as_ref().map(|at|(key(&model,&m.name,""),at.clone()))).collect();
        let status=if model.modules.is_empty(){
            if !index.complete(){"Scanning application-module source…".into()}else if model.roots.is_empty(){"No @SpringBootApplication or @Modulithic source root found.".into()}else{"No application-module source candidates.".into()}
        }else{format!("{} modules · {} dependencies{}",nodes.len(),edges.len(),if index.complete(){""}else{" · scanning…"})};
        let status=if model.uncertain {format!("{status} · unresolved package/alias metadata; boundary diagnostics withheld")}else{status};
        Ok((nodes,edges,sources,status))
    }
}

impl ModulithTools {
    pub fn render(&mut self, view: &str, ctx: &Invocation, source: &mut impl Source) -> Result<View, Error> {
        (|| -> Result<View, Error> {
            let Some(project) = ctx.project() else { return Ok(View::message("Open a project to see its application modules.")) };
            spring_text::index::scoped_root(&project).ok_or(Error::Stale)?;let index = source.project(&project, COMMAND_BUDGET);let index = &index;
            let complete = index.complete();
            if index.scanning() {
                refresh(view);
            }
            let model = Model::build(index);
            let refresh = Button::new("refresh", "Refresh").icon("refresh");
            if model.modules.is_empty() {
                let text = if !complete {
                    "Scanning…"
                } else if model.roots.is_empty() {
                    "No @SpringBootApplication or @Modulithic class found."
                } else {
                    "No application modules: the application package has no sub-packages."
                };
                return Ok(View::message(text).button(refresh));
            }
            if view == id(DIAGRAM_VIEW) {
                let (nodes, edges) = diagram_of(&model, index);
                let status = format!("{} modules · {} dependencies", nodes.len(), edges.len());
                return Ok(View::new(Body::Html(diagram::html(&nodes, &edges, "No application modules."))).button(refresh).status(&status));
            }
            let dependencies = dependencies(&model, index);
            let mut items = Vec::new();
            for m in model.modules.values() {
                let mut detail = if m.open { "open".to_owned() } else { "closed".to_owned() };
                if let Some(d) = &m.display_name {
                    detail = format!("{detail} · {d}");
                }
                let parent = key(&model,&m.name, "");
                items.push(TreeItem::new(&parent, &m.name).detail(&detail).icon("module").expandable(false));
                for i in m.interface_names() {
                    items.push(TreeItem::new(&key(&model,&m.name, &format!("i:{i}")), i).parent(&parent).detail("named interface").icon("interface"));
                }
                for d in m.allowed.iter().flatten() {
                    items.push(TreeItem::new(&key(&model,&m.name, &format!("a:{d}")), d).parent(&parent).detail("allowed dependency").icon("module"));
                }
                for (b, v) in dependencies.get(m.name.as_str()).into_iter().flatten() {
                    let (detail, icon) = match v {
                        None => ("dependency".to_owned(), "module"),
                        Some(Violation::Internal) => ("dependency on internal types".to_owned(), "warning"),
                        Some(Violation::NotAllowed(d)) => (format!("dependency on {d}, not in allowedDependencies"), "warning"),
                    };
                    let item = TreeItem::new(&key(&model,b, &format!("d:{}", m.name)), b).parent(&parent).detail(&detail).icon(icon);
                    items.push(item);
                }
            }
            self.expansion.apply(&mut items);
            let mut status = format!("{} modules in {}", model.modules.len(), model.roots.join(", "));
            if model.uncertain {status.push_str(" · unresolved package/alias metadata; boundary diagnostics withheld");}
            if !complete {
                status.push_str(" · scanning…");
            }
            Ok(View::new(Body::Tree(items)).button(refresh).status(&status))
        })().map(ui_kit::actions::available)
    }

    pub fn handle(&mut self, view: &str, event: ViewEvent, ctx: &Invocation, source: &mut impl Source) -> Result<(), Error> {
        if self.expansion.handle(&event) {
            refresh(view);
            return Ok(());
        }

        match event {
            ViewEvent::Button(b) if b == "refresh" => {
                if let Some(project) = ctx.project() {
                    source.restart(&project);
                }
                refresh(view);
            }
            ViewEvent::Activate(key) | ViewEvent::Message(key) => {
                let project = ctx.project().ok_or(Error::Stale)?;
                let root = project.root()?;
                if root.is_empty() { return Err(Error::Stale); }
                let index = source.project(&project, COMMAND_BUDGET);
                let index = &index;
                let model = Model::build(index);
                let valid = if view == id(DIAGRAM_VIEW) {
                    diagram_of(&model,index).0.iter().any(|node| node.id == key)
                } else {
                    let dependencies = dependencies(&model,index);
                    model.modules.values().any(|module| {
                        key == self::key(&model,&module.name, "")
                            || module.interface_names().iter().any(|name| key == self::key(&model,&module.name,&format!("i:{name}")))
                            || module.allowed.iter().flatten().any(|name| key == self::key(&model,&module.name,&format!("a:{name}")))
                            || dependencies.get(module.name.as_str()).into_iter().flatten().any(|(target,_)| key == self::key(&model,target,&format!("d:{}",module.name)))
                    })
                };
                if !valid { return Err(Error::Stale); }
                let name = key.strip_prefix("open:").and_then(|k| k.split('#').next()).ok_or(Error::Stale)?;
                let at = model.modules.get(name).and_then(|m| m.location.as_ref()).ok_or(Error::Stale)?;
                if at.path.len() > 4096 || at.path.chars().any(char::is_control)
                    || !at.path.starts_with(&format!("{}/",root.trim_end_matches('/')))
                    || host::project_of(&at.path).and_then(|p| p.root().ok()).as_deref() != Some(root.as_str()) { return Err(Error::Stale); }
                let expected: Vec<_> = index.iter().filter(|(path,_)| *path == at.path).map(|(_,unit)|unit).collect();
                if expected.len() != 1 { return Err(Error::Stale); }
                let text = project.read_file(&at.path)?;
                if text.len() > 512 * 1024 { return Err("Selected Modulith source exceeds the navigation bound.".into()); }
                if scan(&at.path,&text).as_ref() != Some(expected[0])
                    || host::project_of(&at.path).and_then(|p| p.root().ok()).as_deref() != Some(root.as_str()) { return Err(Error::Stale); }
                host::open_location(&at.path,at.range.start.line,at.range.start.character)?;
            }
            _ => {}
        }
        Ok(())
    }
}

impl ModulithTools {
    pub fn run_command(&mut self, id: &str, ctx: &Invocation, source: &mut impl Source) -> Result<Option<String>, Error> {
        if id != self::id(COPY_DOCS) {
            return Err(Error::NotFound(format!("command {id}")));
        }
        let project = ctx.project().ok_or_else(|| Error::Invalid("Open a project first.".into()))?;
        let index = source.project(&project, COMMAND_BUDGET);
        let index = &index;
        let model = Model::build(index);
        if model.modules.is_empty() {
            host::notify(NotificationLevel::Info, TITLE, "No application modules found.");
            return Ok(None);
        }
        match host::copy_to_clipboard(&plantuml(&model, index)) {
            Ok(()) => host::notify(NotificationLevel::Info, TITLE, "PlantUML module diagram copied to the clipboard."),
            Err(Error::Denied(_)) => host::notify(
                NotificationLevel::Warning,
                TITLE,
                &ui_kit::denied::message("Copying the module diagram", "clipboard", "Spring Structure Tools"),
            ),
            Err(e) => return Err(e),
        }
        Ok(None)
    }
}

pub const WINDOW_SECTION: &str = "modulith";

const OWNER_WINDOW: &str = "structure";

fn window(_view: &str) -> &str {
    OWNER_WINDOW
}

fn id(name: &str) -> String {
    format!("{WINDOW_SECTION}.{name}")
}

pub(crate) fn refresh(view: &str) {
    host::refresh_view(window(view));
}

#[cfg(test)]
mod harness {
    use super::*;
    use openidea_plugin::views::Viewport;
    use spring_text::index::{Index, Indexes, PROVIDER_BUDGET};

    /// The module over an index of its own facts, as the owner's shared index is for it.
    pub struct ModulithTools {
        inner: super::ModulithTools,
        pub indexes: Indexes<Unit>,
    }

    impl Default for ModulithTools {
        fn default() -> Self {
            Self { inner: Default::default(), indexes: Indexes::new(&["**/*.java", "**/*.kt"], scan).excluding_nested_projects() }
        }
    }

    pub fn model(index: &Index<Unit>) -> Model {
        Model::build(&index.view(|unit| Some(unit)))
    }

    struct Own<'a>(&'a mut Indexes<Unit>);

    impl Source for Own<'_> {
        fn for_document(&mut self, doc: &Document) -> Option<Units<'_>> {
            spring_text::index::owning_root(&doc.path)?;
            Some(self.0.for_document(&doc.path, &doc.text, PROVIDER_BUDGET)?.view(|unit| Some(unit)))
        }

        fn cached(&mut self, doc: &Document) -> Option<Units<'_>> {
            spring_text::index::owning_root(&doc.path)?;
            Some(self.0.for_cached_document(&doc.path, &doc.text)?.view(|unit| Some(unit)))
        }

        fn project(&mut self, project: &Project, budget: std::time::Duration) -> Units<'_> {
            self.0.get(project, budget).view(|unit| Some(unit))
        }

        fn restart(&mut self, project: &Project) {
            let index = self.0.get(project, std::time::Duration::ZERO);
            if index.complete() && !index.scanning() {
                self.0.reset();
            }
        }
    }

    impl ModulithTools {
        pub fn completion(&mut self, doc: &Document, at: Position, _trigger: Option<&str>) -> Vec<CompletionItem> {
            self.inner.completion(doc, at, &mut Own(&mut self.indexes))
        }

        pub fn definition(&mut self, doc: &Document, at: Position) -> Vec<Location> {
            self.inner.definition(doc, at, &mut Own(&mut self.indexes))
        }

        pub fn code_actions(&mut self, doc: &Document, selection: Range) -> Vec<CodeAction> {
            self.inner.code_actions(doc, selection, &mut Own(&mut self.indexes))
        }

        pub fn live_diagnostics(&mut self, doc: &Document) -> Vec<Diagnostic> {
            self.inner.live_diagnostics(doc, &mut Own(&mut self.indexes))
        }

        pub fn warm_project(&mut self, project: &Project, budget: std::time::Duration) {
            self.inner.warm_project(project, budget, &mut Own(&mut self.indexes))
        }

        pub fn reset_live(&mut self) {
            self.inner.reset_live();
            self.indexes.reset();
        }

        pub fn expose_actions(&mut self, doc: &Document, selection: Range) -> Vec<openidea_plugin::file_actions::FileAction> {
            self.inner.expose_actions(doc, selection, &mut Own(&mut self.indexes))
        }

        pub fn resolve_expose(&mut self, doc: &Document, action: openidea_plugin::file_actions::FileAction) -> Result<openidea_plugin::file_actions::FileAction, Error> {
            self.inner.resolve_expose(doc, action)
        }

        pub fn render(&mut self, view: &str, _viewport: Option<Viewport>, ctx: &Invocation) -> Result<View, Error> {
            self.inner.render(view, ctx, &mut Own(&mut self.indexes))
        }

        pub fn handle(&mut self, view: &str, event: ViewEvent, ctx: &Invocation) -> Result<(), Error> {
            self.inner.handle(view, event, ctx, &mut Own(&mut self.indexes))
        }

        pub fn run_command(&mut self, id: &str, ctx: &Invocation, _arg: Option<&str>) -> Result<Option<String>, Error> {
            self.inner.run_command(id, ctx, &mut Own(&mut self.indexes))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::{ModulithTools, model};
    use openidea_plugin::mock::MockHost;
    use openidea_plugin::text::position;

    const VIEW: &str = "modules";

    const SRC: &str = "/project/src/main/java/com/acme/";

    fn setup() -> MockHost {
        let host = MockHost::install();
        let files = [
            ("Application.java", "package com.acme;\n\n@SpringBootApplication\npublic class Application {}\n"),
            ("order/OrderService.java", "package com.acme.order;\n\npublic class OrderService {}\n"),
            ("order/internal/OrderRepository.java", "package com.acme.order.internal;\n\nclass OrderRepository {}\n"),
            (
                "order/package-info.java",
                "@ApplicationModule(displayName = \"Orders\")\npackage com.acme.order;\n\nimport org.springframework.modulith.ApplicationModule;\n",
            ),
            ("inventory/Inventory.java", "package com.acme.inventory;\n\npublic class Inventory {}\n"),
            ("inventory/api/StockApi.java", "package com.acme.inventory.api;\n\npublic interface StockApi {}\n"),
            ("inventory/api/package-info.java", "@org.springframework.modulith.NamedInterface(\"api\")\npackage com.acme.inventory.api;\n"),
            ("inventory/spi/Hook.java", "package com.acme.inventory.spi;\n\n@NamedInterface(\"hooks\")\npublic interface Hook {}\n"),
            (
                "shipping/package-info.java",
                "@ApplicationModule(allowedDependencies = {\"order\", \"inventory :: api\", \"billing\"})\npackage com.acme.shipping;\n",
            ),
            ("shipping/Shipper.java", SHIPPER),
        ];
        for (path, text) in files {
            host.add_file(&format!("src/main/java/com/acme/{path}"), text);
        }
        host.add_file("src/test/java/com/acme/shipping/ShipperTest.java", "package com.acme.shipping;\nimport com.acme.order.internal.OrderRepository;\nclass ShipperTest {}\n");
        host
    }

    const SHIPPER: &str = "package com.acme.shipping;

import java.util.List;
import com.acme.order.OrderService;
import com.acme.order.internal.OrderRepository;
import com.acme.inventory.api.StockApi;
import com.acme.inventory.Inventory;
import com.acme.inventory.spi.Hook;
import com.acme.shipping.internal.Box;

class Shipper {}
";

    fn doc(path: &str, text: &str) -> Document {
        Document { path: format!("{SRC}{path}"), language: "java".into(), version: 1, text: text.into() }
    }

    fn pos(text: &str, needle: &str) -> Position {
        position(text, text.find(needle).unwrap())
    }

    #[test]
    fn qualified_project_types_drive_diagnostics_and_dependency_graph() {
        let host=setup();let text="package com.acme.shipping; class Qualified { com.acme.order.internal.OrderRepository repository; Object item = new com.acme.inventory.Inventory(); Class<?> type = com.acme.inventory.spi.Hook.class; }";
        let d=doc("shipping/Qualified.java",text);host.add_file(&d.path,&d.text);let mut tools=ModulithTools::default();
        tools.warm_project(&host.invocation().project().unwrap(),COMMAND_BUDGET);
        let problems=tools.live_diagnostics(&d);
        assert_eq!(problems.iter().filter(|p|p.code.as_deref()==Some("module-internal")).count(),1);
        assert_eq!(problems.iter().filter(|p|p.code.as_deref()==Some("module-not-allowed")).count(),2);
        assert!(problems.iter().any(|p|p.range.start==pos(text,"com.acme.order.internal.OrderRepository")));
        let index=tools.indexes.get(&host.invocation().project().unwrap(),COMMAND_BUDGET);let model=model(index);
        let refs=scan(&d.path,text).unwrap().qualified_uses;assert_eq!(refs.len(),3);
        assert!(refs.iter().all(|name|model.declared_types.contains(name)));
        let graph=dependencies(&model,&index.view(|u|Some(u)));assert!(graph["shipping"].contains_key("order"));assert!(graph["shipping"].contains_key("inventory"));
    }
    #[test]
    fn qualified_type_candidates_skip_comments_strings_unknown_types_and_receivers() {
        let host=setup();let text=r#"package com.acme.shipping;
        // com.acme.order.internal.OrderRepository ignored;
        class Qualified {
          String value = "com.acme.order.internal.OrderRepository";
          com.acme.order.internal.Missing unknown;
          void call() { com.acme.order.internal.OrderRepository.execute(); }
        }"#;
        let d=doc("shipping/Qualified.java",text);host.add_file(&d.path,text);let mut tools=ModulithTools::default();
        tools.warm_project(&host.invocation().project().unwrap(),COMMAND_BUDGET);
        assert!(tools.live_diagnostics(&d).is_empty());
        let candidates=qualified_type_uses(text,&java::parse(text));assert_eq!(candidates.len(),1);assert_eq!(candidates[0].0,"com.acme.order.internal.Missing");
        let test_doc=Document {path:"/project/src/test/java/com/acme/shipping/Test.java".into(),text:"package com.acme.shipping; class Test {com.acme.order.internal.OrderRepository value;}".into(),..d};
        assert!(tools.live_diagnostics(&test_doc).is_empty());
    }
    #[test]
    fn allowed_dependencies_navigate_modules_interfaces_and_wildcards() {
        let _host=setup();let mut tools=ModulithTools::default();
        let text="@ApplicationModule(allowedDependencies={\"order\",\"inventory::api\",\"inventory::hooks\",\"inventory::*\",\"unknown\"})\npackage com.acme.shipping;";
        let d=doc("shipping/package-info.java",text);
        let module=tools.definition(&d,pos(text,"order"));assert_eq!(module.len(),1);assert!(module[0].path.ends_with("/order/package-info.java"));
        let interface=tools.definition(&d,pos(text,"inventory::api"));assert_eq!(interface.len(),1);assert!(interface[0].path.ends_with("/inventory/api/package-info.java"));
        let exposed=tools.definition(&d,pos(text,"inventory::hooks"));assert_eq!(exposed.len(),1);assert!(exposed[0].path.ends_with("/inventory/spi/Hook.java"));
        let wildcard=tools.definition(&d,pos(text,"inventory::*"));assert_eq!(wildcard.len(),1);assert!(wildcard[0].path.contains("/inventory/"));
        assert!(tools.definition(&d,pos(text,"unknown")).is_empty());
        let completions=tools.completion(&d,pos(text,"unknown"),None);let wildcard=completions.iter().find(|c|c.label=="inventory::*").unwrap();assert_eq!(wildcard.detail.as_deref(),Some("all named interfaces"));assert!(wildcard.replace.is_some());
    }
    #[test]
    fn project_module_badges_use_declared_state_and_withdraw_on_reset() {
        let host=setup();for module in ["order","inventory","shipping"] {host.add_dir(&format!("{SRC}{module}"));}let mut tools=ModulithTools::default();let project=host.invocation().project().unwrap();tools.warm_project(&project,COMMAND_BUDGET);
        let badges=host.file_decorations();assert!(!badges.is_empty());
        let order=badges.iter().find(|((_,path),_)|path.ends_with("/com/acme/order")).unwrap().1;assert_eq!(order.badge.as_deref(),Some("C"));assert_eq!(order.style,"muted");
        tools.reset_live();assert!(host.file_decorations().is_empty());
        host.add_file("src/main/java/com/acme/order/package-info.java","@ApplicationModule(type = ApplicationModule.Type.OPEN) package com.acme.order;");
        tools.warm_project(&project,COMMAND_BUDGET);let badges=host.file_decorations();let order=badges.iter().find(|((_,path),_)|path.ends_with("/com/acme/order")).unwrap().1;assert_eq!(order.badge.as_deref(),Some("O"));assert_eq!(order.style,"added");
        tools.reset_live();host.add_project(&format!("{SRC}order"));tools.warm_project(&project,COMMAND_BUDGET);assert!(!host.file_decorations().keys().any(|(_,path)|path.ends_with("/com/acme/order")));
    }
    #[test]
    fn kotlin_roots_types_and_package_info_metadata_form_one_source_module_model() {
        let host=MockHost::install();
        host.add_file("/project/src/main/kotlin/app/Main.kt","package app\nimport org.springframework.boot.autoconfigure.SpringBootApplication\n@SpringBootApplication class Main");
        host.add_file("/project/src/main/kotlin/app/orders/internal/Repository.kt","package app.orders.internal\nclass Repository");
        host.add_file("/project/src/main/kotlin/app/orders/api/Api.kt","package app.orders.api\nclass Api");
        host.add_file("/project/src/main/kotlin/app/orders/api/Metadata.kt","package app.orders.api\nimport org.springframework.modulith.PackageInfo\nimport org.springframework.modulith.NamedInterface\n@PackageInfo @NamedInterface(\"api\") class Metadata");
        host.add_file("/project/src/main/kotlin/app/shipping/Metadata.kt","package app.shipping\nimport org.springframework.modulith.PackageInfo\nimport org.springframework.modulith.ApplicationModule\n@PackageInfo @ApplicationModule(allowedDependencies=[\"orders::api\"]) class Metadata");
        let source="package app.shipping\nimport app.orders.internal.Repository\nimport app.orders.api.Api\nclass Shipping";
        let d=Document{path:"/project/src/main/kotlin/app/shipping/Shipping.kt".into(),language:"kotlin".into(),text:source.into(),version:1};host.add_file(&d.path,source);let mut tools=ModulithTools::default();tools.warm_project(&host.invocation().project().unwrap(),COMMAND_BUDGET);
        let found=tools.live_diagnostics(&d);assert_eq!(found.len(),1,"{found:?}");assert_eq!(found[0].code.as_deref(),Some("module-internal"));
        let index=tools.indexes.get(&host.invocation().project().unwrap(),COMMAND_BUDGET);let model=model(index);assert_eq!(model.roots,["app"]);assert_eq!(model.modules.len(),2);assert_eq!(model.modules["shipping"].allowed,Some(vec!["orders::api".into()]));assert!(model.modules["orders"].interfaces.iter().any(|(name,package)|name=="api"&&package=="app.orders.api"));
        let fixed=Document{text:source.replace("import app.orders.internal.Repository\n",""),version:2,..d};assert!(tools.live_diagnostics(&fixed).is_empty());
        let path="/project/src/main/kotlin/app/shipping/Metadata.kt";let text=host.invocation().project().unwrap().read_file(path).unwrap();let metadata=Document{path:path.into(),language:"kotlin".into(),text:text.clone(),version:1};let at=pos(&text,"orders::api");
        assert!(tools.completion(&metadata,at,None).iter().any(|item|item.label=="orders::*"));let locations=tools.definition(&metadata,at);assert_eq!(locations.len(),1);assert!(locations[0].path.ends_with("/orders/api/Metadata.kt"));
        let invalid=Document{text:text.replace("orders::api","missing"),version:2,..metadata};assert!(tools.live_diagnostics(&invalid).iter().any(|d|d.code.as_deref()==Some("unknown-module")));
    }
    #[test]
    fn unsupported_kotlin_alias_metadata_withholds_negative_boundary_facts() {
        let host=setup();host.add_file("src/main/java/com/acme/inventory/api/KotlinMetadata.kt","package com.acme.inventory.api\nimport org.springframework.modulith.PackageInfo\nimport org.springframework.modulith.NamedInterface as API\n@PackageInfo @API(\"another\") class KotlinMetadata");let mut tools=ModulithTools::default();tools.warm_project(&host.invocation().project().unwrap(),COMMAND_BUDGET);
        let d=doc("shipping/Shipper.java",SHIPPER);assert!(tools.live_diagnostics(&d).is_empty());let view=tools.render(VIEW,None,&host.invocation()).unwrap();assert!(view.status.unwrap().contains("unresolved package/alias"));
    }
    #[test]
    fn kotlin_constant_expression_and_duplicate_package_metadata_are_not_absence_evidence() {
        let unit=scan("/project/Metadata.kt","package app.orders\n@PackageInfo @NamedInterface(API_NAME) class Metadata").unwrap();assert!(unit.uncertain);
        let cast=scan("/project/Cast.kt","package app.orders\nclass Cast { fun cast(value: Any): String = value as String }").unwrap();assert!(!cast.uncertain);
        let _host=setup();let mut index=spring_text::index::Index::new(&["**/*.java","**/*.kt"],scan);
        index.update("/project/Main.kt","package app\n@SpringBootApplication class Main");index.update("/project/One.kt","package app.orders\n@PackageInfo @ApplicationModule class One");index.update("/project/Two.kt","package app.orders\n@PackageInfo @NamedInterface(\"api\") class Two");let model=model(&index);assert!(model.uncertain);assert!(model.check("shipping","app.orders.Internal").is_none());
    }
    #[test]
    fn reads_package_info() {
        let u = scan("/p/package-info.java", "@ApplicationModule(\n  type = ApplicationModule.Type.OPEN)\npackage a.b;").unwrap();
        let m = u.info.unwrap().module.unwrap();
        assert!(m.open);
        assert_eq!(m.open_fix, None);
        let fix = |text: &str| {
            let u = scan("/p/package-info.java", text).unwrap();
            let e = u.info.unwrap().module.unwrap().open_fix.unwrap();
            let at = offset(text, e.range.start).unwrap();
            format!("{}{}{}", &text[..at], e.text, &text[at..])
        };
        assert_eq!(fix("@ApplicationModule\npackage a.b;"), format!("@ApplicationModule({OPEN})\npackage a.b;"));
        assert_eq!(fix("@ApplicationModule()\npackage a.b;"), format!("@ApplicationModule({OPEN})\npackage a.b;"));
        assert_eq!(fix("@ApplicationModule(displayName = \"B\")\npackage a.b;"), format!("@ApplicationModule(displayName = \"B\", {OPEN})\npackage a.b;"));
    }

    #[test]
    fn exactly_imported_types_are_checked_at_member_use_sites() {
        let host=setup();
        let text="package com.acme.shipping;
import java.util.List;
import com.acme.order.internal.OrderRepository;
import com.acme.inventory.spi.Hook;
import com.acme.order.OrderService;
import com.acme.other.*;
class Repo<T extends OrderRepository> extends OrderRepository implements Hook {
    List<OrderRepository> all;
    OrderRepository find(Hook h) { return new OrderRepository(); }
    void run() { OrderRepository.create(); OrderService s; int OrderRepository = 1; }
}
";
        let d=doc("shipping/Repo.java",text);host.add_file(&d.path,&d.text);let mut tools=ModulithTools::default();
        tools.warm_project(&host.invocation().project().unwrap(),COMMAND_BUDGET);
        let problems=tools.live_diagnostics(&d);
        let internal=problems.iter().filter(|p|p.code.as_deref()==Some("module-internal")).count();
        let unlisted=problems.iter().filter(|p|p.code.as_deref()==Some("module-not-allowed")).count();
        assert_eq!((internal,unlisted),(7,3));
        let shadow="package com.acme.shipping;
import com.acme.order.internal.OrderRepository;
class Box<OrderRepository extends Object> { OrderRepository value; }
";
        let d=doc("shipping/Box.java",shadow);host.add_file(&d.path,&d.text);
        let uses=tools.live_diagnostics(&d).into_iter().filter(|p|p.code.as_deref()==Some("module-internal")).count();
        assert_eq!(uses,1);
    }

    #[test]
    fn offers_to_open_the_module() {
        let _host = setup();
        let mut tools = ModulithTools::default();
        let d = doc("shipping/Shipper.java", SHIPPER);
        let at = pos(SHIPPER, "order.internal");
        let actions = tools.code_actions(&d, Range::new(at, at));
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].title, "Make module `order` open (exposes all its types)");
        let (path, edits) = &actions[0].edits[0];
        assert_eq!(path, &format!("{SRC}order/package-info.java"));
        assert_eq!(edits[0].text, format!(", {OPEN}"));
        let at = pos(SHIPPER, "inventory.Inventory");
        assert!(tools.code_actions(&d, Range::new(at, at)).is_empty());
    }

    #[test]
    fn modules_view_lists_modules_and_opens_them() {
        let host = setup();
        let mut tools = ModulithTools::default();
        let view = tools.render(VIEW, None, &host.invocation()).unwrap();
        let Body::Tree(items) = view.body else { panic!("tree expected") };
        let rows: Vec<_> = items
            .iter()
            .map(|i| (i.parent.is_some(), i.label.as_str(), i.detail.as_deref().unwrap_or_default(), i.icon.as_deref().unwrap()))
            .collect();
        assert_eq!(rows, [
            (false, "inventory", "closed", "module"),
            (true, "api", "named interface", "interface"),
            (true, "hooks", "named interface", "interface"),
            (false, "order", "closed · Orders", "module"),
            (false, "shipping", "closed", "module"),
            (true, "order", "allowed dependency", "module"),
            (true, "inventory :: api", "allowed dependency", "module"),
            (true, "billing", "allowed dependency", "module"),
            (true, "inventory", "dependency on inventory, not in allowedDependencies", "warning"),
            (true, "order", "dependency on internal types", "warning"),
        ]);
        assert_eq!(view.status.as_deref(), Some("3 modules in com.acme"));
        assert!(host.refreshed_views().is_empty(), "the scan is complete");
        let dependency = items.iter().rfind(|i| i.label == "order").unwrap();
        tools.handle(VIEW, ViewEvent::Activate(dependency.key.clone()), &host.invocation()).unwrap();
        assert_eq!(host.opened_locations(), [(format!("{SRC}order/package-info.java"), 0, 0)]);
    }

    #[test]
    fn modules_open_and_close() {
        let host = setup();
        let mut tools = ModulithTools::default();
        let module = |tools: &mut ModulithTools| {
            let view = tools.render(VIEW, None, &host.invocation()).unwrap();
            let Body::Tree(items) = view.body else { panic!("tree expected") };
            items.into_iter().find(|i| i.label == "inventory" && i.parent.is_none()).unwrap()
        };
        let inventory = module(&mut tools);
        assert!(inventory.expandable && !inventory.expanded);

        tools.handle(VIEW, ViewEvent::Expand(inventory.key.clone()), &host.invocation()).unwrap();
        assert_eq!(host.refreshed_views(), [window(VIEW)]);
        assert!(module(&mut tools).expanded);

        tools.handle(VIEW, ViewEvent::Collapse(inventory.key), &host.invocation()).unwrap();
        assert!(!module(&mut tools).expanded);
    }

    #[test]
    fn refresh_keeps_a_scan_in_progress() {
        let host = setup();
        let mut tools = ModulithTools::default();
        let project = host.invocation().project().unwrap();
        tools.indexes.get(&project, Duration::ZERO).update(&format!("{SRC}shipping/Shipper.java"), SHIPPER);
        tools.handle(VIEW, ViewEvent::Button("refresh".into()), &host.invocation()).unwrap();
        assert_eq!(tools.indexes.get(&project, Duration::ZERO).files().len(), 1, "not reset");
        assert_eq!(host.refreshed_views(), [window(VIEW)]);

        tools.indexes.get(&project, COMMAND_BUDGET);
        tools.handle(VIEW, ViewEvent::Button("refresh".into()), &host.invocation()).unwrap();
        assert!(tools.indexes.get(&project, Duration::ZERO).files().is_empty(), "a complete index starts over");
    }

    #[test]
    fn modulith_diagram_has_expected_nodes_and_edges() {
        let host = setup();
        let mut tools = ModulithTools::default();
        let index = tools.indexes.get(&host.invocation().project().unwrap(), COMMAND_BUDGET);
        let model = model(index);
        let (nodes, edges) = diagram_of(&model, &index.view(|u|Some(u)));
        let nodes: Vec<_> = nodes.iter().map(|n| (n.id.split("#source:").next().unwrap(), n.label.as_str(), n.detail.as_deref())).collect();
        assert_eq!(nodes, [
            ("open:inventory#", "inventory", Some("closed")),
            ("open:order#", "order", Some("closed")),
            ("open:shipping#", "shipping", Some("closed")),
        ]);
        let edges: Vec<_> = edges.iter().map(|e| (e.from.split("#source:").next().unwrap(), e.to.split("#source:").next().unwrap(), e.label.as_deref(), e.warning)).collect();
        assert_eq!(edges, [
            ("open:shipping#", "open:inventory#", Some("inventory not allowed"), true),
            ("open:shipping#", "open:order#", Some("internal types"), true),
        ]);

        let view = tools.render(&id(DIAGRAM_VIEW), None, &host.invocation()).unwrap();
        let Body::Html(page) = view.body else { panic!("html expected") };
        assert!(page.contains(&format!("data-id=\"{}\"", key(&model,"order",""))));
        assert_eq!(view.status.as_deref(), Some("3 modules · 2 dependencies"));
        tools.handle(DIAGRAM_VIEW, ViewEvent::Message(key(&model,"order","")), &host.invocation()).unwrap();
        assert_eq!(host.opened_locations(), [(format!("{SRC}order/package-info.java"), 0, 0)]);

        let empty = MockHost::install();
        let view = ModulithTools::default().render(&id(DIAGRAM_VIEW), None, &empty.invocation()).unwrap();
        assert_eq!(view.body, Body::Message("No @SpringBootApplication or @Modulithic class found.".into()));
    }

    #[test]
    fn copy_module_diagram_denied_names_permission() {
        let denied = MockHost::install().with_capabilities(&["project.read"]);
        denied.add_file("src/main/java/com/acme/Application.java", "package com.acme;\n@SpringBootApplication\nclass Application {}\n");
        denied.add_file("src/main/java/com/acme/order/Order.java", "package com.acme.order;\nclass Order {}\n");
        ModulithTools::default().run_command(&id(COPY_DOCS), &denied.invocation(), None).unwrap();
        assert!(denied.notifications().iter().any(|n| n.body.contains("needs the clipboard permission") && n.body.contains("Settings › Plugins › Spring Structure Tools")));
    }

    #[test]
    fn modulith_plantuml_text_lists_modules_and_dependencies() {
        let host = setup();
        let mut tools = ModulithTools::default();
        tools.run_command(&id(COPY_DOCS), &host.invocation(), None).unwrap();
        assert_eq!(host.clipboard().as_deref(), Some(PLANTUML));

        let denied = MockHost::install().with_capabilities(&["project.read"]);
        denied.add_file("src/main/java/com/acme/Application.java", "package com.acme;\n@SpringBootApplication\nclass Application {}\n");
        denied.add_file("src/main/java/com/acme/order/Order.java", "package com.acme.order;\nclass Order {}\n");
        ModulithTools::default().run_command(&id(COPY_DOCS), &denied.invocation(), None).unwrap();
        assert_eq!(denied.clipboard(), None);
        assert!(denied.notifications().iter().any(|n| n.body.contains("needs the clipboard permission") && n.body.contains("Settings › Plugins › Spring Structure Tools")));
    }

    const PLANTUML: &str = "@startuml
component [inventory]
component [order]
component [shipping]
[shipping] -[#red]-> [inventory] : inventory not allowed
[shipping] -[#red]-> [order] : internal types
@enduml
";

        #[test]
    fn refresh_goes_to_the_owner_window_when_embedded() {
        let host = MockHost::install();
        refresh("diagram");
        assert_eq!(host.refreshed_views(), ["structure"]);
    }

    #[test]
    fn diagram_and_tree_actions_require_current_keys_and_source_facts() {
        let host=setup();let mut tools=ModulithTools::default();tools.render(&id(DIAGRAM_VIEW),None,&host.invocation()).unwrap();
        let index=tools.indexes.get(&host.invocation().project().unwrap(),COMMAND_BUDGET);let valid=key(&model(index),"order","");
        for key in ["open:unknown#","open:order#forged","open:order#a:forged"] {
            assert!(tools.handle(&id(DIAGRAM_VIEW),ViewEvent::Message(key.into()),&host.invocation()).is_err());
        }
        assert!(tools.handle(VIEW,ViewEvent::Activate("open:order#forged".into()),&host.invocation()).is_err());
        host.add_file("src/main/java/com/acme/order/package-info.java","package com.acme.order;");
        assert!(tools.handle(&id(DIAGRAM_VIEW),ViewEvent::Message(valid.clone()),&host.invocation()).is_err());
        assert!(host.opened_locations().is_empty());
    }
    #[test]
    fn module_source_navigation_refuses_foreign_project_and_oversized_reread() {
        let host=setup();let mut tools=ModulithTools::default();tools.render(&id(DIAGRAM_VIEW),None,&host.invocation()).unwrap();
        let index=tools.indexes.get(&host.invocation().project().unwrap(),COMMAND_BUDGET);let valid=key(&model(index),"order","");
        host.add_file("src/main/java/com/acme/order/package-info.java",&" ".repeat(512*1024+1));
        assert!(tools.handle(&id(DIAGRAM_VIEW),ViewEvent::Message(valid.clone()),&host.invocation()).is_err());
        host.set_project_root(Some("/other"));
        assert!(tools.handle(&id(DIAGRAM_VIEW),ViewEvent::Message(valid.clone()),&host.invocation()).is_err());
        assert!(host.opened_locations().is_empty());
    }

}
