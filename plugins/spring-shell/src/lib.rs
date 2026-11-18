//! Spring Shell Tools for OpenIDEA: indexes the shell commands of a project
//! (`@ShellMethod` and `@Command` methods), checks availability methods and
//! providers, and completes and links the names they refer to. Text-based,
//! Java and Kotlin.

use openidea_plugin::diagnostics::DiagnosticsProvider;
use openidea_plugin::providers::{CompletionItem, CompletionKind, Document, LineMarker, Providers};
use openidea_plugin::text::{offset, position};
use openidea_plugin::{host, Diagnostic, Error, Event, EventKind, Invocation, NotificationLevel, Location, Plugin, Position, Range, Severity, register_plugin};
use spring_text::index::{COMMAND_BUDGET, Index, Indexes, PROVIDER_BUDGET};
use std::cell::RefCell;
use std::time::{Duration, Instant};
use std::collections::HashMap;
use std::rc::Rc;
use spring_text::java::{self, Annotation, Arg, JavaFile, Member, Str};
use spring_text::text::balanced;
use spring_text::{SOURCE_GLOBS, parse_source, range};

mod actions;
mod command_rename;
mod native_occurrences;
mod hover;
mod options;
mod literals;
mod rules;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    /// The full command, words separated by one space.
    pub key: String,
    /// Qualified name of the declaring class.
    pub class: String,
    pub method: String,
    /// Declared with the standard `@ShellMethod`, whose registrar walks the bean's superclass chain.
    pub legacy: bool,
    /// The key's literal, or the method name when the key is derived from it.
    pub anchor: Range,
    /// The method name.
    pub target: Range,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Shell {
    pub incomplete: bool,
    pub commands: Vec<Command>,
    pub groups: Vec<String>,
    /// Bean names, and whether the bean is an `AvailabilityProvider`.
    pub beans: Vec<(String, Range, bool)>,
    pub core_beans: Vec<(String, Range, bool)>,
    pub classes: Vec<ClassInfo>,
}

/// What a literal superclass-chain walk needs of a declared type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClassInfo {
    pub qualified: String,
    pub kind: String,
    pub base: Base,
    /// Zero-argument methods by name.
    pub methods: Vec<(String, Range)>,
    /// Every method name, whatever its parameters.
    pub method_names: Vec<String>,
    /// Literally annotated `@ShellComponent`.
    pub component: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Base {
    /// No `extends`/`implements`: the chain ends at `Object`.
    Root,
    /// Exactly one plain supertype, resolved by import or package.
    Named(String),
    Unknown,
}

/// Spring Shell's default command name: `addUser` → `add-user`.
pub fn kebab(name: &str) -> String {
    let mut out = String::new();
    let mut lower = false;
    for c in name.chars() {
        if lower && c.is_ascii_uppercase() {
            out.push('-');
        }
        lower = c.is_ascii_lowercase();
        out.push(c.to_ascii_lowercase());
    }
    out
}

fn words(strings: &[&Str]) -> String {
    options::normalized(&strings.iter().map(|s| s.value.as_str()).collect::<Vec<_>>().join(" "))
}

pub fn commands(f: &JavaFile, text: &str) -> Vec<Command> {
    let mut out = Vec::new();
    for (ti, t) in f.types.iter().enumerate() {
        let class = f.qualified(ti);
        let class_command = t.annotation("Command")
            .filter(|a| rules::modern_field(f, text, a) == Some("command"));
        let prefix = class_command.map(|a| words(&a.strings(&["command"])));
        for m in t.methods() {
            let target = range(text, m.at, m.at + m.name.len());
            let legacy = m.annotation("ShellMethod").is_some_and(|a| rules::supported(f, text, a));
            let mut push = |key: String, anchor: Range| {
                if !key.is_empty() && !key.contains('$') {
                    out.push(Command { key, class: class.clone(), method: m.name.clone(), legacy, anchor, target });
                }
            };
            if let Some(a) = m.annotation("ShellMethod").filter(|a| rules::supported(f, text, a)) {
                if a.arg("key").is_some_and(|arg| !hover::literal_values(arg) || arg.text.contains('\\')) {
                    continue;
                }
                let keys = a.strings(&["key"]);
                if keys.is_empty() {
                    if !m.name.is_ascii() {
                        continue;
                    }
                    push(kebab(&m.name), target);
                }
                for s in keys {
                    push(words(&[s]), range(text, s.at, s.end));
                }
            } else if let Some(a) =
                m.annotation("Command").filter(|a| rules::modern_field(f, text, a).is_some())
            {
                let field = rules::modern_field(f, text, a).unwrap();
                let class_command = class_command.filter(|_| field == "command");
                if a.arg(field).is_some() && rules::literals(a, field).is_none()
                    || class_command.is_some_and(|a| a.arg("command").is_some() && rules::literals(a, "command").is_none())
                { continue; }
                let core_group = t.annotation("CommandGroup").filter(|a| rules::supported(f, text, a));
                let core_prefix = if field == "name" {
                    match core_group.filter(|a| a.arg("prefix").is_some()) {
                        Some(group) => {
                            let Some(values) = rules::literals(group, "prefix").filter(|v| v.len() == 1) else { continue; };
                            words(&values)
                        }
                        None => String::new(),
                    }
                } else { String::new() };
                let own = a.strings(&[field]);
                let anchor = own.first().map_or(target, |s| range(text, s.at, s.end));
                let own = words(&own);
                let base = if field == "name" && own.is_empty() && m.name.is_ascii() { kebab(&m.name) } else { own };
                let combined = options::normalized(&format!("{} {}", if field == "command" { prefix.as_deref().unwrap_or_default() } else { &core_prefix }, &base));
                if combined.is_empty() && !m.name.is_ascii() { continue; }
                push(if combined.is_empty() { kebab(&m.name) } else { combined }, anchor);
                // Each method alias is a separate command; class aliases prefix
                // v3 aliases only. v4 has no class-level Command configuration.
                if let Some(aliases) = rules::literals(a, "alias") {
                    let alias_prefix = class_command.and_then(|a| rules::literals(a, "alias"));
                    if class_command.is_some_and(|a| a.arg("alias").is_some()) && alias_prefix.is_none() { continue; }
                    let prefix = alias_prefix.map(|ss| words(&ss)).unwrap_or_default();
                    for alias in aliases {
                        let key = options::normalized(&format!("{prefix} {}", alias.value));
                        if !key.is_empty() { push(key, range(text, alias.at, alias.end)); }
                    }
                }

            }
        }
    }
    out
}

/// Every annotation of the file's types and members.
fn annotations(f: &JavaFile) -> impl Iterator<Item = &Annotation> {
    f.types.iter().flat_map(|t| t.annotations.iter().chain(t.members.iter().flat_map(|m| &m.annotations)))
}

fn groups(f: &JavaFile, text: &str) -> Vec<String> {
    let mut out: Vec<String> = annotations(f)
        .filter(|a| rules::supported(f, text, a))
        .flat_map(|a| {
            let name = match a.name.as_str() {
                "ShellCommandGroup" => "value",
                "CommandGroup" => "name",
                "ShellMethod" | "Command" => "group",
                _ => return Vec::new(),
            };
            rules::literals(a, name).unwrap_or_default()
        })
        .map(|s| s.value.clone())
        .filter(|g| !g.is_empty() && !g.contains("${"))
        .collect();
    out.sort();
    out.dedup();
    out
}

fn beans(f: &JavaFile, text: &str) -> Vec<(String, Range, bool)> {
    beans_for(f,text,"org.springframework.shell.AvailabilityProvider")
}
fn beans_for(f: &JavaFile, text: &str, qualified:&str) -> Vec<(String, Range, bool)> {
    let provider_type = |ty: &java::TypeRef| rules::type_owned(f,text,ty,qualified);
    let providers: Vec<_> = f.types.iter().enumerate().filter(|(_,t)| t.implements.iter().any(&provider_type)).map(|(i,_)|i).collect();
    java::bean_names(f)
        .into_iter()
        .filter_map(|b| {
            let annotation = f
                .types
                .iter()
                .find(|t| t.at == b.at)
                .and_then(|t| {
                    t.annotations.iter().find(|a| {
                        let q = match a.name.as_str() {
                            "ShellComponent" => "org.springframework.shell.standard.ShellComponent",
                            "Component" => "org.springframework.stereotype.Component",
                            "Service" => "org.springframework.stereotype.Service",
                            "Repository" => "org.springframework.stereotype.Repository",
                            "Controller" => "org.springframework.stereotype.Controller",
                            "Configuration" => "org.springframework.context.annotation.Configuration",
                            _ => return false,
                        };
                        rules::owned(f, text, a, q)
                    })
                })
                .or_else(|| {
                    f.types
                        .iter()
                        .flat_map(|t| t.methods())
                        .find(|m| m.at == b.at)
                        .and_then(|m| m.annotation("Bean"))
                        .filter(|a| rules::owned(f, text, a, "org.springframework.context.annotation.Bean"))
                })?;
            if ["name", "value"]
                .iter()
                .any(|name| annotation.arg(name).is_some_and(|arg| !hover::literal_values(arg) || arg.text.contains(['$', '\\'])))
            {
                return None;
            }
            let ty = f.types.iter().flat_map(|t| t.methods()).find(|m| m.at == b.at).and_then(|m| m.ty.as_ref());
            let provider = if let Some(ty)=ty {
                provider_type(ty) || providers.iter().any(|i|rules::local_type_owned(f,text,ty,*i))
            } else {providers.iter().any(|i|f.types[*i].at==b.at)};
            Some((b.name, range(text, b.at, b.at + b.len), provider))
        })
        .collect()
}

fn source_model(path: &str, text: &str) -> Option<JavaFile> {
    // Java translates eligible Unicode escapes before token/comment scanning.
    // Without that preprocessing and offset mapping, source ownership is unknown.
    if path.ends_with(".java") && !literals::java_source_supported(text) {
        return None;
    }
    if text.len() > 128 << 10 {
        return None;
    }
    if !balanced(text, path.ends_with(".kt")) {
        return None;
    }
    let file = parse_source(path, text);
    if file.imports.len() > 256
        || file.types.len() > 128
        || file.types.iter().any(|t| t.members.len() > 128)
        || file.types.iter().map(|t| t.members.len()).sum::<usize>() > 512
    {
        return None;
    }
    Some(file)
}
fn document_model(doc: &Document) -> Option<JavaFile> {
    if !matches!((doc.language.as_str(), doc.path.rsplit('.').next()), ("java", Some("java")) | ("kotlin", Some("kt"))) {
        return None;
    }
    source_model(&doc.path, &doc.text)
}
fn scan(path: &str, text: &str) -> Option<Shell> {
    let Some(f) = source_model(path, text) else {
        return Some(Shell { incomplete: true, ..Default::default() });
    };
    let shell = Shell { incomplete: false, commands: commands(&f, text), groups: groups(&f, text), beans: beans(&f, text), core_beans: beans_for(&f,text,"org.springframework.shell.core.command.availability.AvailabilityProvider"), classes: class_infos(&f, text) };
    (shell != Shell::default()).then_some(shell)
}

fn class_infos(f: &JavaFile, text: &str) -> Vec<ClassInfo> {
    let aliased = f.imports.iter().any(|i| rules::import_aliased(text, i));
    f.types.iter().enumerate().map(|(i, t)| {
        let base = match t.supers().collect::<Vec<_>>().as_slice() {
            [] => Base::Root,
            [s] if !aliased && s.args.is_empty() && !s.array && s.text == s.name => Base::Named(f.resolve(&s.name)),
            _ => Base::Unknown,
        };
        let methods = t.methods().filter(|m| m.params.is_empty()).map(|m| (m.name.clone(), range(text, m.at, m.at + m.name.len()))).collect();
        let method_names = t.methods().map(|m| m.name.clone()).collect();
        let component = t.annotation("ShellComponent").is_some_and(|a| rules::supported(f, text, a));
        ClassInfo { qualified: f.qualified(i), kind: t.kind.clone(), base, methods, method_names, component }
    }).collect()
}

enum Inherited {
    Found(String, Range),
    Missing,
    Unknown,
}

/// Declared classes by qualified name, with the file declaring each.
type ClassMap = HashMap<String, Vec<(String, ClassInfo)>>;

struct Derived {
    classes: ClassMap,
    /// Qualified names some indexed class extends.
    extended: std::collections::HashSet<String>,
    inherited: Vec<(String, String, Command)>,
}

thread_local! {
    static DERIVED: RefCell<Option<((u64, u64), Rc<Derived>)>> = const { RefCell::new(None) };
}

/// What every project-wide lookup derives from the index, built once per
/// index generation.
fn derived(index: &Index<Shell>) -> Rc<Derived> {
    let key = (index.id(), index.generation());
    if let Some(found) = DERIVED.with(|cell| cell.borrow().as_ref().filter(|(k, _)| *k == key).map(|(_, d)| d.clone())) {
        return found;
    }
    let mut classes = ClassMap::new();
    for (path, shell) in index.iter() {
        for class in &shell.classes {
            classes.entry(class.qualified.clone()).or_default().push((path.to_owned(), class.clone()));
        }
    }
    let extended = index
        .iter()
        .flat_map(|(_, shell)| &shell.classes)
        .filter_map(|c| match &c.base {
            Base::Named(q) => Some(q.clone()),
            _ => None,
        })
        .collect();
    let inherited = index.iter().flat_map(|(p, s)| inherited_in(index, &classes, p, &s.classes, &s.commands)).collect();
    let built = Rc::new(Derived { classes, extended, inherited });
    DERIVED.with(|cell| *cell.borrow_mut() = Some((key, built.clone())));
    built
}

/// Project-declared superclass chain of `start`, nearest first, and whether it
/// ends literally at `Object`. Anything not declared once as a class in the
/// project ends the walk as incomplete.
fn walk(index: &Index<Shell>, path: &str, local: &[ClassInfo], start: &ClassInfo) -> (Vec<(String, ClassInfo)>, bool) {
    walk_in(&derived(index).classes, path, local, start)
}

fn walk_in(classes: &ClassMap, path: &str, local: &[ClassInfo], start: &ClassInfo) -> (Vec<(String, ClassInfo)>, bool) {
    let declared = |qualified: &str| -> Vec<(String, &ClassInfo)> {
        let others = classes.get(qualified).into_iter().flatten().filter(|(p, _)| p != path).map(|(p, c)| (p.clone(), c));
        others.chain(local.iter().filter(|c| c.qualified == qualified).map(|c| (path.to_owned(), c))).collect()
    };
    let mut chain = Vec::new();
    let mut next = start.base.clone();
    let mut seen = vec![start.qualified.clone()];
    loop {
        let parent = match next {
            Base::Root => return (chain, true),
            Base::Unknown => return (chain, false),
            Base::Named(q) => q,
        };
        let found = declared(&parent);
        let [(p, info)] = found.as_slice() else { return (chain, false) };
        if info.kind != "class" || seen.contains(&parent) || seen.len() > 16 {
            return (chain, false);
        }
        chain.push((p.clone(), (*info).clone()));
        seen.push(parent);
        next = info.base.clone();
    }
}

/// Looks `name` up the project-declared superclass chain of `class`, like
/// `ReflectionUtils.findMethod` (nearest declaration wins).
fn inherited_indicator(doc: &Document, f: &JavaFile, index: &Index<Shell>, class: &str, name: &str) -> Inherited {
    let local = class_infos(f, &doc.text);
    let Some(start) = local.iter().find(|c| c.qualified == class) else { return Inherited::Unknown };
    let (chain, complete) = walk(index, &doc.path, &local, start);
    for (path, info) in chain {
        if let Some((_, at)) = info.methods.iter().find(|(n, _)| n == name) {
            return Inherited::Found(path, *at);
        }
    }
    if complete { Inherited::Missing } else { Inherited::Unknown }
}

/// `@ShellMethod` commands a `@ShellComponent` class inherits through a fully
/// literal project superclass chain, as `(owner file, declaring file, command)`
/// with the command re-homed to the inheriting class. Spring Shell registers
/// `ReflectionUtils.doWithMethods(bean.getClass())`, so they belong to the
/// subclass bean; their group still comes from the declaring class
/// (`method.getDeclaringClass()`), so nothing about the group changes.
/// Methods redeclared nearer in the chain are skipped.
fn inherited_commands(index: &Index<Shell>, path: &str, classes: &[ClassInfo], own: &[Command]) -> Vec<(String, String, Command)> {
    inherited_in(index, &derived(index).classes, path, classes, own)
}

fn inherited_in(index: &Index<Shell>, map: &ClassMap, path: &str, classes: &[ClassInfo], own: &[Command]) -> Vec<(String, String, Command)> {
    let mut out = Vec::new();
    for class in classes.iter().filter(|c| c.component && c.kind == "class") {
        let (chain, complete) = walk_in(map, path, classes, class);
        if !complete {
            continue;
        }
        let mut redeclared = class.method_names.clone();
        for (declaring, info) in chain {
            let declared: Vec<Command> = if declaring == path {
                own.to_vec()
            } else {
                index.iter().filter(|(p, _)| *p == declaring).flat_map(|(_, s)| s.commands.clone()).collect()
            };
            for c in declared.into_iter().filter(|c| c.class == info.qualified && c.legacy && !redeclared.contains(&c.method)) {
                out.push((path.to_owned(), declaring.clone(), Command { class: class.qualified.clone(), ..c }));
            }
            redeclared.extend(info.method_names);
        }
    }
    out
}

/// Every file parsed, none cut short, and nothing left unread: only then does
/// a missing command or class prove anything.
fn trustworthy(index: &Index<Shell>) -> bool {
    index.complete() && !index.scanning() && index.iter().all(|(_, shell)| !shell.incomplete)
}

/// Whether `qualified` may be a base class: an indexed class extends it, or
/// the index is too incomplete to say it is not.
fn may_be_extended(index: &Index<Shell>, qualified: &str) -> bool {
    !trustworthy(index) || derived(index).extended.contains(qualified)
}

fn all_commands(index: &Index<Shell>) -> impl Iterator<Item = (&str, &Command)> {
    index.iter().flat_map(|(p, s)| s.commands.iter().map(move |c| (p, c)))
}

fn all_beans_for(index:&Index<Shell>,core:bool)->impl Iterator<Item=(&str,&(String,Range,bool))> {
    index.iter().flat_map(move |(p,s)| if core {&s.core_beans} else {&s.beans}.iter().map(move |b|(p,b)))
}

fn unique_provider_names_for(index:&Index<Shell>,core:bool)->std::collections::BTreeSet<&str> {
    let mut names:std::collections::BTreeMap<&str,(std::collections::BTreeSet<(&str,u32,u32)>,bool)>=std::collections::BTreeMap::new();
    for (path,bean) in all_beans_for(index,core) {
        let entry=names.entry(&bean.0).or_insert_with(||(std::collections::BTreeSet::new(),true));
        entry.0.insert((path,bean.1.start.line,bean.1.start.character));
        entry.1 &= bean.2;
    }
    names.into_iter().filter(|(_,entry)|entry.0.len()==1&&entry.1).map(|(name,_)|name).collect()
}

fn simple(class: &str) -> &str {
    class.rsplit('.').next().unwrap_or(class)
}

fn problem(range: Range, severity: Severity, code: &str, message: String) -> Diagnostic {
    Diagnostic::new(range, severity, message).code(code)
}

/// Legacy indicator selectors address commands in their declaring class. An
/// unrelated class's equal command name is not an availability target.
fn named(index: &Index<Shell>, class: &str, key: &str) -> Vec<(String, Command)> {
    let own = all_commands(index).map(|(p, c)| (p.to_owned(), c.clone()));
    let derived = derived(index);
    let inherited = derived.inherited.iter().cloned().map(|(_, declaring, c)| (declaring, c));
    own.chain(inherited).filter(|(_, c)| c.class == class && c.key == key).collect()
}

fn availability_owner<'a>(f: &'a JavaFile, text: &str, annotation: &Annotation) -> Option<&'a java::TypeDecl> {
    f.types.iter().find(|t| {
        t.methods().any(|m| {
            m.annotation("ShellMethod").is_some_and(|a| rules::supported(f, text, a)) && m.annotations.iter().any(|a| a.at == annotation.at)
        })
    })
}

/// Problems and gutter links of a Java file.
fn analyze(doc: &Document, f: &JavaFile, index: &Index<Shell>) -> (Vec<Diagnostic>, Vec<LineMarker>) {
    let text = &doc.text;
    let at = |from: usize, to: usize| range(text, from, to);
    let name_of = |m: &Member| at(m.at, m.at + m.name.len());
    let line_of = |m: &Member| position(text, m.at).line;
    let mine = commands(f, text);
    let mut problems = Vec::new();
    let mut markers = Vec::new();

    for c in &mine {
        let other = all_commands(index)
            .find(|(p, o)| (*p == doc.path || index.fresh()) && o.key == c.key && !(*p == doc.path && o.anchor == c.anchor));
        if let Some((p, o)) = other {
            let message = format!("Command `{}` is also declared by {}.{} in {}.", c.key, simple(&o.class), o.method, p);
            problems.push(problem(c.anchor, Severity::Error, "duplicate-command", message));
        }
    }

    let local_infos = class_infos(f, text);
    let local_inherited = inherited_commands(index, &doc.path, &local_infos, &mine);
    if !local_inherited.is_empty() {
        let siblings: Vec<_> = if index.fresh() {
            derived(index).inherited.iter().filter(|(owner, _, _)| *owner != doc.path).cloned().collect()
        } else {
            Vec::new()
        };
        for (_, declaring, c) in &local_inherited {
            let Some((_, t)) = f.types.iter().enumerate().find(|(ti, _)| f.qualified(*ti) == c.class) else { continue };
            let direct = all_commands(index)
                .filter(|(p, o)| (*p == doc.path || index.fresh()) && o.key == c.key && !(*p == declaring && o.anchor == c.anchor))
                .map(|(p, o)| (simple(&o.class).to_owned(), o.method.clone(), p.to_owned()));
            let sibling = siblings.iter().filter(|(_, _, o)| o.key == c.key).map(|(owner, _, o)| (simple(&o.class).to_owned(), o.method.clone(), owner.clone()));
            let local = local_inherited.iter().filter(|(_, _, o)| o.key == c.key && o.class != c.class).map(|(owner, _, o)| (simple(&o.class).to_owned(), o.method.clone(), owner.clone()));
            if let Some((class, method, path)) = direct.chain(sibling).chain(local).next() {
                let message = format!("Command `{}` inherited from {} is also declared by {}.{} in {}.", c.key, simple(&c.class), class, method, path);
                problems.push(problem(at(t.at, t.at + t.name.len()), Severity::Error, "duplicate-command", message));
            }
        }
    }

    for (ti, t) in f.types.iter().enumerate() {
        let class = f.qualified(ti);
        let legacy: Vec<&Member> =
            t.methods().filter(|m| m.annotation("ShellMethod").is_some_and(|a| rules::supported(f, text, a))).collect();
        for m in t.methods() {
            for (arg,core) in m.annotations.iter().filter_map(|a| {
                if a.name=="CommandAvailability" && rules::supported(f,text,a) {a.arg("provider").map(|arg|(arg,false))}
                else if rules::modern_field(f,text,a)==Some("name") {a.arg("availabilityProvider").map(|arg|(arg,true))}
                else {None}
            }).filter(|(arg,_)|hover::literal_values(arg) && !arg.text.contains('\\')) {
                let names=unique_provider_names_for(index,core);
                for s in &arg.strings {
                    if s.value.contains('$') {continue;}
                    if let Some((path,b))=all_beans_for(index,core).find(|(_,b)|b.0==s.value && names.contains(s.value.as_str())) {
                        markers.push(LineMarker{line:line_of(m),icon:Some(AVAILABILITY_MARKER.into()),tooltip:format!("Availability provider: {}",s.value),target:Some(Location{path:path.into(),range:b.1})});
                    }
                }
            }
        }
        for m in &legacy {
            let keys: Vec<&str> = mine.iter().filter(|c| c.class == class && c.method == m.name).map(|c| c.key.as_str()).collect();
            let conventional = format!("{}Availability", m.name);
            let mut inherited_conventional = None;
            let available = if let Some(explicit) = m.annotation("ShellMethodAvailability").filter(|a| rules::supported(f, text, a)) {
                explicit.arg("value").filter(|arg| hover::literal_values(arg)).and_then(|arg| match arg.strings.as_slice() {
                    [name] => t.methods().find(|m| m.name == name.value && m.params.is_empty()),
                    _ => None,
                })
            } else {
                let candidate = |wildcard: bool| {
                    t.methods().find(|m| {
                        m.params.is_empty()
                            && !m.annotation("ShellMethod").is_some_and(|a| rules::supported(f, text, a))
                            && m.annotation("ShellMethodAvailability").filter(|a| rules::supported(f, text, a)).is_some_and(|a| {
                                if a.arg("value").is_none() {
                                    return wildcard;
                                }
                                let Some(arg) = a.arg("value").filter(|arg| hover::literal_values(arg)) else { return false };
                                arg.strings.iter().any(|s| if wildcard { s.value == "*" } else { keys.contains(&words(&[s]).as_str()) })
                            })
                    })
                };
                match t.methods().find(|a| a.name == conventional && a.params.is_empty()) {
                    Some(local) => Some(local),
                    None => match inherited_indicator(doc, f, index, &class, &conventional) {
                        Inherited::Missing => candidate(false).or_else(|| candidate(true)),
                        other => {
                            inherited_conventional = Some(other);
                            None
                        }
                    },
                }
            };
            if let Some(Inherited::Found(path, range)) = inherited_conventional {
                markers.push(LineMarker {
                    line: line_of(m),
                    icon: Some(AVAILABILITY_MARKER.into()),
                    tooltip: format!("Availability method: {conventional}() (inherited)"),
                    target: Some(Location { path, range }),
                });
            }
            if available.is_none() && t.supers().next().is_some() {
                let explicit = m.annotation("ShellMethodAvailability").filter(|a| rules::supported(f, text, a));
                let named = explicit.and_then(|a| rules::literals(a, "value")).filter(|v| v.len() == 1 && !v[0].value.contains('$'));
                if let Some(s) = named.map(|v| v[0]) {
                    match inherited_indicator(doc, f, index, &class, &s.value) {
                        Inherited::Found(path, range) => markers.push(LineMarker {
                            line: line_of(m),
                            icon: Some(AVAILABILITY_MARKER.into()),
                            tooltip: format!("Availability method: {}() (inherited)", s.value),
                            target: Some(Location { path, range }),
                        }),
                        Inherited::Missing if index.fresh() && trustworthy(index) && !matches!(s.value.as_str(), "getClass" | "hashCode" | "toString" | "clone" | "finalize" | "wait" | "notify" | "notifyAll") => problems.push(problem(
                            rules::literal_range(text, s),
                            Severity::Warning,
                            "availability-method-reference",
                            "Neither this class nor its superclasses have a method with this name and no parameters.".into(),
                        )),
                        _ => {}
                    }
                }
            }
            if let Some(a) = available {
                markers.push(LineMarker {
                    line: line_of(m),
                    icon: Some(AVAILABILITY_MARKER.into()),
                    tooltip: format!("Availability method: {}()", a.name),
                    target: Some(Location { path: doc.path.clone(), range: name_of(a) }),
                });
            }
        }
    }
    problems.extend(rules::check(doc, f, &|qualified| may_be_extended(index, qualified)));
    (problems, markers)
}

/// The annotation string literal at byte `at`, with its annotation and argument.
fn literal_at(f: &JavaFile, at: usize) -> Option<(&Annotation, &Arg, &Str)> {
    annotations(f).find_map(|a| a.args.iter().find_map(|arg| Some((a, arg, arg.strings.iter().find(|s| s.holds(at))?))))
}

/// The qualified name of the type declaring byte `at`.
fn class_at(f: &JavaFile, at: usize) -> String {
    f.types.iter().enumerate().rfind(|(_, t)| t.start <= at && at <= t.end).map(|(i, _)| f.qualified(i)).unwrap_or_default()
}

pub struct ShellTools {
    actions: actions::State,
    command_renames: command_rename::State,
    native_occurrences: native_occurrences::State,
    indexes: Indexes<Shell>,
}

impl Default for ShellTools {
    fn default() -> Self {
        Self { actions: Default::default(), command_renames: Default::default(), native_occurrences:Default::default(), indexes: Indexes::new(SOURCE_GLOBS, scan) }
    }
}

impl DiagnosticsProvider for ShellTools {
    fn diagnostics(&mut self, doc: &Document) -> Vec<Diagnostic> {
        let Some(file) = document_model(doc) else {
            return Vec::new();
        };
        let mut local = Index::new(SOURCE_GLOBS, scan);
        local.update(&doc.path, &doc.text);
        let index = self.indexes.for_cached_document(&doc.path, &doc.text).unwrap_or(&mut local);
        let mut diagnostics = analyze(doc, &file, index).0;
        diagnostics.truncate(500);
        diagnostics
    }
}

impl Providers for ShellTools {
    fn line_markers(&mut self, doc: &Document) -> Vec<LineMarker> {
        let Some(f) = document_model(doc) else {
            return Vec::new();
        };
        let Some(index) = self.indexes.for_document(&doc.path, &doc.text, PROVIDER_BUDGET) else {
            return Vec::new();
        };
        let (_, markers) = analyze(doc, &f, index);
        markers
    }

    fn definition(&mut self, doc: &Document, at: Position) -> Vec<Location> {
        let Some(f) = document_model(doc) else {
            return Vec::new();
        };
        let Some(offset) = offset(&doc.text, at) else {
            return Vec::new();
        };
        let Some((a, arg, s)) = literal_at(&f, offset) else {
            return Vec::new();
        };
        if !rules::supported(&f, &doc.text, a) || !hover::literal_values(arg) || arg.text.contains('\\') || s.value.contains('$') {
            return Vec::new();
        }
        if a.name == "ShellMethodAvailability" && arg.name == "value" {
            if !hover::literal_values(arg) {
                return Vec::new();
            }
            if let Some(owner) = availability_owner(&f, &doc.text, a) {
                return owner
                    .methods()
                    .filter(|m| m.name == s.value && m.params.is_empty())
                    .map(|m| Location { path: doc.path.clone(), range: range(&doc.text, m.at, m.at + m.name.len()) })
                    .collect();
            }
        }
        if a.name=="Command" && arg.name=="availabilityProvider" && rules::modern_field(&f,&doc.text,a)!=Some("name") { return vec![]; }
        let Some(index) = self.indexes.for_document(&doc.path, &doc.text, PROVIDER_BUDGET) else {
            return Vec::new();
        };
        match (a.name.as_str(), arg.name.as_str()) {
            ("ShellMethodAvailability", "value") => named(index, &class_at(&f, offset), &words(&[s]))
                .into_iter()
                .map(|(path, c)| Location { path: path.into(), range: c.target })
                .collect(),
            ("CommandAvailability", "provider") | ("Command", "availabilityProvider") => {
                all_beans_for(index,a.name=="Command").filter(|(_, b)| b.0 == s.value).map(|(path, b)| Location { path: path.into(), range: b.1 }).collect()
            }
            _ => Vec::new(),
        }
    }

    fn completion(&mut self, doc: &Document, at: Position, _trigger: Option<&str>) -> Vec<CompletionItem> {
        let Some(f) = document_model(doc) else {
            return Vec::new();
        };
        let Some(offset) = offset(&doc.text, at) else {
            return Vec::new();
        };
        let Some((a, arg, s)) = literal_at(&f, offset) else {
            return Vec::new();
        };
        if !rules::supported(&f, &doc.text, a) || !hover::literal_values(arg) || arg.text.contains('\\') || s.value.contains('$') {
            return Vec::new();
        }
        let replace = Some(range(&doc.text, s.at, s.end));
        let item = |label: &str, kind: CompletionKind, detail: &str, sort: &str| CompletionItem {
            detail: Some(detail.into()),
            replace,
            sort_text: Some(format!("{sort}{label}")),
            ..CompletionItem::new(label, kind)
        };
        if a.name == "ShellMethodAvailability" && arg.name == "value" {
            if let Some(owner) = availability_owner(&f, &doc.text, a) {
                return owner
                    .methods()
                    .filter(|m| m.params.is_empty() && m.ty.as_ref().is_some_and(|t| rules::availability_type(&f, &doc.text, t)))
                    .map(|m| item(&m.name, CompletionKind::Method, "Availability indicator method", "0"))
                    .collect();
            }
        }
        let Some(index) = self.indexes.for_document(&doc.path, &doc.text, PROVIDER_BUDGET) else {
            return Vec::new();
        };
        if a.name=="Command" && arg.name=="availabilityProvider" && rules::modern_field(&f,&doc.text,a)!=Some("name") {return vec![];}
        let mut out: Vec<CompletionItem> = match (a.name.as_str(), arg.name.as_str()) {
            ("ShellMethodAvailability", "value") => {
                let class = class_at(&f, offset);
                let derived = derived(index);
                let inherited = derived.inherited.iter().cloned().map(|(_, _, c)| c);
                let mut out: Vec<_> = all_commands(index).map(|(_, c)| c.clone()).chain(inherited)
                    .filter(|c| c.class == class)
                    .map(|c| {
                        let detail = format!("{}.{}()", simple(&c.class), c.method);
                        item(&c.key, CompletionKind::Method, &detail, if c.class == class { "0" } else { "1" })
                    })
                    .collect();
                out.push(item("*", CompletionKind::Keyword, "every command of the class", "2"));
                out
            }
            ("ShellMethod" | "Command", "group") | ("ShellCommandGroup", "value") => {
                index.iter().flat_map(|(_, s)| &s.groups).map(|g| item(g, CompletionKind::Module, "command group", "0")).collect()
            }
            ("CommandAvailability", "provider") | ("Command", "availabilityProvider") => unique_provider_names_for(index,a.name=="Command").into_iter()
                .map(|name| item(name, CompletionKind::Reference, "Declared AvailabilityProvider bean", "0"))
                .collect(),
            _ => Vec::new(),
        };
        out.sort_by(|a, b| a.label.cmp(&b.label));
        out.dedup_by(|a, b| a.label == b.label);
        out
    }
}

impl Plugin for ShellTools {
    fn activate(&mut self, _ctx: &openidea_plugin::Context) -> Result<(), Error> {
        let tracked = host::subscribe_paths("files-changed", SOURCE_GLOBS).and(host::subscribe(EventKind::FileSaved));
        if let Err(Error::Denied(_)) = tracked {
            host::notify(
                NotificationLevel::Info,
                "Spring Shell Tools",
                "File tracking needs the optional events permission. Allow events for Spring Shell Tools in Settings › Plugins.",
            );
        }
        Ok(())
    }

    fn handle_events(&mut self, events: &[Event], _ctx: &Invocation) {
        self.indexes.on_events(events, Instant::now() + Duration::from_secs(3));
    }

    fn run_command(&mut self, id: &str, ctx: &Invocation, arg: Option<&str>) -> Result<Option<String>, Error> {
        if id == "command-occurrences" { return if let Some(arg)=arg{self.native_occurrences(ctx,arg)}else{command_rename::occurrences(ctx,None)}; }
        Err(Error::NotFound(format!("command {id}")))
    }
}

register_plugin!(ShellTools, providers, diagnostics, hover, file_actions, language_features);

pub(crate) const AVAILABILITY_MARKER: &str = "icons/marker-availability.svg";

#[cfg(test)]
mod tests {
    #[test]
    fn marker_icons_are_declared_in_the_manifest() {
        let manifest = include_str!("../plugin.toml");
        for icon in [crate::AVAILABILITY_MARKER] {
            assert!(manifest.contains(&format!("icon = \"{icon}\"")));
            assert!(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(icon).is_file());
        }
    }

    use super::*;
    use openidea_plugin::mock::MockHost;

    const LEGACY: &str = r#"package com.acme.cli;

import org.springframework.shell.standard.ShellComponent;
import org.springframework.shell.standard.ShellMethod; import org.springframework.shell.standard.ShellMethodAvailability; import org.springframework.shell.Availability;

@ShellComponent
public class UserCommands {
    @ShellMethod(value = "Add a user", group = "Users")
    public String addUser(@ShellOption String name) { return name; }

    @ShellMethod(key = {"remove", "rm"}, value = "Remove a user")
    public void remove(String name) {}

    @ShellMethod("List users")
    public void list() {}

    public Availability addUserAvailability() { return Availability.available(); }

    @ShellMethodAvailability({"remove", "nope"})
    public boolean canRemove() { return true; }

    @ShellMethodAvailability("*")
    public Availability always() { return Availability.available(); }
}
"#;

    const MODERN: &str = r#"package com.acme.cli;

import org.springframework.shell.command.annotation.Command;
import org.springframework.shell.command.annotation.CommandAvailability; import org.springframework.context.annotation.Bean; import org.springframework.context.annotation.Configuration; import org.springframework.shell.AvailabilityProvider;

@Command(command = "db", group = "Database")
class DbCommands {
    @Command(command = {"dump", "all"})
    @CommandAvailability(provider = "connected")
    void dumpAll() {}

    @Command
    @CommandAvailability(provider = "missing")
    void showTables() {}

    @Command(command = "list")
    void list() {}
}

@Configuration
class ShellConfig {
    @Bean
    AvailabilityProvider connected() { return () -> Availability.available(); }

    @Bean
    Clock clock() { return null; }
}
"#;

    fn doc(name: &str, text: &str) -> Document {
        Document { path: format!("/project/src/main/java/com/acme/cli/{name}"), language: "java".into(), version: 1, text: text.into() }
    }

    fn setup() -> MockHost {
        let host = MockHost::install();
        host.add_file("src/main/java/com/acme/cli/UserCommands.java", LEGACY);
        host.add_file("src/main/java/com/acme/cli/DbCommands.java", MODERN);
        host.add_file("src/main/java/com/acme/cli/Other.java", "import org.springframework.shell.standard.ShellComponent; import org.springframework.shell.standard.ShellMethod; @ShellComponent class Other { @ShellMethod(key = \"list\") void ls() {} }");
        host
    }

    fn pos(text: &str, needle: &str, delta: usize) -> Position {
        position(text, text.find(needle).unwrap() + delta)
    }

    const KOTLIN: &str = r#"package com.acme.cli

import org.springframework.shell.command.annotation.Command
import org.springframework.shell.command.annotation.CommandAvailability
import org.springframework.context.annotation.Bean
import org.springframework.shell.AvailabilityProvider
import org.springframework.shell.standard.ShellComponent
import org.springframework.shell.standard.ShellMethod
import org.springframework.shell.standard.ShellMethodAvailability

@ShellComponent
class KtCommands {
    @ShellMethod(key = ["greet", "hi"], value = "Say hello")
    suspend fun greet(name: String): String = "hi $name"

    @Command(command = ["kt", "dump"], group = "Kt")
    @CommandAvailability(provider = "ktMissing")
    fun dump() {}

    @ShellMethodAvailability(["greet", "nope"])
    fun canGreet(): Availability = Availability.available()
}

@Configuration
class KtConfig {
    @Bean
    fun ktConnected(): AvailabilityProvider = AvailabilityProvider { Availability.available() }
}
"#;

    #[test]
    fn abstract_base_no_missing_component() {
        let host = setup();
        let imports = "package com.acme.cli; import org.springframework.shell.standard.ShellMethod;";
        let child = "package com.acme.cli; import org.springframework.shell.standard.ShellComponent; @ShellComponent class Child extends Base {}";
        let concrete = format!("{imports} class Base {{ @ShellMethod(\"Run\") void run(){{}} }}");
        let abstract_base = concrete.replace("class Base", "abstract class Base");
        let warns = |tools: &mut ShellTools, text: &str, warm: bool| {
            let d = doc("Base.java", text);
            if warm {
                tools.line_markers(&d);
            }
            tools.diagnostics(&d).into_iter().any(|d| d.code.as_deref() == Some("missing-shell-component"))
        };
        assert!(!warns(&mut ShellTools::default(), &concrete, false), "cold index stays quiet");
        assert!(warns(&mut ShellTools::default(), &concrete, true), "trusted index without subclasses warns");
        assert!(!warns(&mut ShellTools::default(), &abstract_base, true));
        host.add_file("src/main/java/com/acme/cli/Child.java", child);
        assert!(!warns(&mut ShellTools::default(), &concrete, true), "extended elsewhere");
    }

    #[test]
    fn deleted_file_drops_duplicate_command() {
        let host = setup();
        let mut tools = ShellTools::default();
        let d = doc("UserCommands.java", LEGACY);
        tools.line_markers(&d);
        let duplicates = |tools: &mut ShellTools| tools.diagnostics(&d).into_iter().filter(|d| d.code.as_deref() == Some("duplicate-command")).count();
        assert_eq!(duplicates(&mut tools), 1);
        let gone = "/project/src/main/java/com/acme/cli/Other.java".to_owned();
        tools.handle_events(&[openidea_plugin::Event::FilesChanged(openidea_plugin::FilesChanged { removed: vec![gone], ..Default::default() })], &host.invocation());
        assert_eq!(duplicates(&mut tools), 0);
    }

    #[test]
    fn diagnostics_skip_oversized_or_other_document_types() {
        let _host = setup();
        let mut tools = ShellTools::default();
        let large = "x".repeat(512 * 1024 + 1);
        assert!(tools.diagnostics(&doc("Large.java", &large)).is_empty());
        assert!(tools.diagnostics(&doc("Other.txt", LEGACY)).is_empty());
    }

    #[test]
    fn live_unsaved_checks_work_without_project_access_and_clear_after_fix() {
        for caps in [vec!["project.read"], vec![]] {
            let host = MockHost::install().with_capabilities(&caps);
            host.add_file(
                "src/main/java/com/acme/cli/UserCommands.java",
                &LEGACY.replace("public boolean canRemove()", "public Availability canRemove()"),
            );
            let path = doc("UserCommands.java", LEGACY).path;
            host.open_editor(&path, "java", LEGACY, &[]);
            let mut tools = ShellTools::default();
            host.refresh_diagnostics(&mut tools);
            let problems = host.provider_diagnostics();
            let problems = &problems[&path];
            assert!(problems.iter().any(|d| d.code.as_deref() == Some("availability-return-type")));
            assert!(problems.iter().any(|d| d.code.as_deref() == Some("unknown-command")));
            assert!(host.diagnostics().is_empty());
            let fixed = LEGACY.replace("public boolean canRemove()", "public Availability canRemove()");
            host.open_editor(&path, "java", &fixed, &[]);
            host.refresh_diagnostics(&mut tools);
            assert!(host.provider_diagnostics().values().flatten().all(|d| d.code.as_deref() != Some("availability-return-type")));
        }
    }

    #[test]
    fn kotlin_shell_command_is_indexed() {
        let kt = |text: &str| commands(&parse_source("K.kt", text), text).into_iter().map(|c| c.key).collect::<Vec<_>>();
        assert_eq!(kt(KOTLIN), ["greet", "hi", "kt dump"]);
        let beans = scan("K.kt", KOTLIN).unwrap().beans;
        assert!(beans.iter().any(|b| b.0 == "ktConnected" && b.2), "{beans:?}");
        let host = setup();
        host.add_file("src/main/kotlin/com/acme/cli/KtCommands.kt", KOTLIN);
        let mut tools = ShellTools::default();
        let d = Document {
            path: "/project/src/main/kotlin/com/acme/cli/KtCommands.kt".into(),
            language: "kotlin".into(),
            version: 1,
            text: KOTLIN.into(),
        };
        tools.line_markers(&d);
        assert!(host.diagnostics().is_empty());
        let problems = tools.diagnostics(&d);
        let messages: Vec<_> = problems.iter().map(|p| p.message.as_str()).collect();
        assert!(messages.iter().any(|m| m.contains("nope")), "{messages:?}");
        assert!(!messages.iter().any(|m| m.contains("ktMissing")), "{messages:?}");
        assert_eq!(messages.len(), 1, "{messages:?}");
    }

    #[test]
    fn kotlin_template_keys_are_not_commands() {
        let src = "import org.springframework.shell.standard.ShellMethod\n@ShellComponent\nclass K {\n    @ShellMethod(key = [\"$p-run\", \"ok\"])\n    fun a() {}\n}\n";
        let file = parse_source("K.kt", src);
        let keys: Vec<_> = commands(&file, src).into_iter().map(|c| c.key).collect();
        assert!(keys.is_empty(), "Mixed dynamic arrays are not evaluated.");
    }

    #[test]
    fn names_commands_of_both_models() {
        assert_eq!(kebab("addUser"), "add-user");
        assert_eq!(kebab("getURL"), "get-url");
        let keys = |text: &str| commands(&java::parse(text), text).into_iter().map(|c| c.key).collect::<Vec<_>>();
        assert_eq!(keys(LEGACY), ["add-user", "remove", "rm", "list"]);
        assert_eq!(keys(MODERN), ["db dump all", "db", "db list"]);
        assert!(keys("import picocli.CommandLine.Command;\n@Command(name = \"x\") class A { @Command void run() {} }").is_empty());
        let shell = scan("", MODERN).unwrap();
        assert_eq!(shell.groups, ["Database"]);
        assert_eq!(
            shell.beans.iter().map(|b| (b.0.as_str(), b.2)).collect::<Vec<_>>(),
            [("shellConfig", false), ("connected", true), ("clock", false)]
        );
    }

    #[test]
    fn checks_availability_and_duplicates() {
        let host = setup();
        let mut tools = ShellTools::default();
        let markers = tools.line_markers(&doc("UserCommands.java", LEGACY));
        let tips: Vec<_> = markers.iter().map(|m| (m.line, m.tooltip.as_str())).collect();
        assert_eq!(
            tips,
            [
                (8, "Availability method: addUserAvailability()"),
                (11, "Availability method: canRemove()"),
                (14, "Availability method: always()")
            ]
        );
        assert_eq!(markers[1].target.as_ref().unwrap().range.start, pos(LEGACY, "canRemove", 0));

        assert!(host.diagnostics().is_empty());
        let problems = tools.diagnostics(&doc("UserCommands.java", LEGACY));
        let codes: Vec<_> = problems.iter().map(|p| (p.code.as_deref().unwrap(), p.range.start.line)).collect();
        assert_eq!(codes, [("duplicate-command", 14), ("unknown-command", 18), ("availability-return-type", 19)]);
        assert_eq!(problems[1].range.start, pos(LEGACY, "nope", 0));
    }

    #[test]
    fn checks_availability_providers() {
        let host = setup();
        let mut tools = ShellTools::default();
        let markers = tools.line_markers(&doc("DbCommands.java", MODERN));
        assert_eq!(markers.len(), 1);
        assert_eq!(markers[0].line, 9);
        assert_eq!(markers[0].target.as_ref().unwrap().range.start, Position::new(22, 25));
        assert!(host.diagnostics().is_empty());
        let problems = tools.diagnostics(&doc("DbCommands.java", MODERN));
        let codes: Vec<_> = problems.iter().map(|p| (p.code.as_deref().unwrap(), p.severity)).collect();
        assert!(codes.is_empty(), "External/runtime beans are unknown: {codes:?}");
    }

    #[test]
    fn completes_names() {
        let _host = setup();
        let mut tools = ShellTools::default();
        let labels = |items: Vec<CompletionItem>| items.into_iter().map(|i| i.label).collect::<Vec<_>>();
        let d = doc("UserCommands.java", LEGACY);
        let at = pos(LEGACY, "\"nope\"", 1);
        assert_eq!(labels(tools.completion(&d, at, None)), ["*", "add-user", "list", "remove", "rm"]);
        assert_eq!(labels(tools.completion(&d, pos(LEGACY, "\"Users\"", 1), None)), ["Database", "Users"]);
        let d = doc("DbCommands.java", MODERN);
        assert_eq!(labels(tools.completion(&d, pos(MODERN, "\"missing\"", 1), None)), ["connected"]);
        assert!(tools.completion(&d, pos(MODERN, "\"list\"", 1), None).is_empty());
    }

    #[test]
    fn navigates_from_names() {
        let _host = setup();
        let mut tools = ShellTools::default();
        let found = tools.definition(&doc("UserCommands.java", LEGACY), pos(LEGACY, "\"remove\", \"nope\"", 2));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].range.start, Position::new(11, 16));
        let found = tools.definition(&doc("DbCommands.java", MODERN), pos(MODERN, "\"connected\"", 3));
        assert_eq!(found[0].range.start, Position::new(22, 25));
    }
}

#[cfg(test)]
mod annotation_owner_safety_regressions {
 use super::*;
 use openidea_plugin::{hover::HoverProvider,mock::MockHost};
 #[test]
 fn aliased_kotlin_annotation_import_does_not_publish_command_hover() {
  let _host=MockHost::install().with_capabilities(&[]);
  let text="import org.springframework.shell.standard.ShellMethod as Other\nclass C { @ShellMethod(key=\"run\",value=\"Unowned description\") fun run(){} }";
  let doc=Document{path:"/project/C.kt".into(),language:"kotlin".into(),version:1,text:text.into()};
  assert!(ShellTools::default().hover(&doc,position(text,text.find("run(){}").unwrap())).is_none());
 }
 #[test]
 fn conflicting_static_alias_and_package_root_owners_are_quiet() {
  let _host=MockHost::install().with_capabilities(&[]);
  for text in [
   "import org.springframework.shell.standard.ShellMethod; import static foreign.Owner.ShellMethod; class C { @ShellMethod(key=\"run\",value=\"Unowned\") void run(){} }",
   "import foreign.org; class C { @org.springframework.shell.standard.ShellMethod(key=\"run\",value=\"Unowned\") void run(){} }",
   "class C<org> { @org.springframework.shell.standard.ShellMethod(key=\"run\",value=\"Unowned\") void run(){} }",
  ] {
   let doc=Document{path:"/project/C.java".into(),language:"java".into(),version:1,text:text.into()};
   assert!(ShellTools::default().hover(&doc,position(text,text.find("run(){}").unwrap())).is_none(),"{text}");
  }
  for text in [
   "import org.springframework.shell.standard.ShellMethod\ntypealias ShellMethod=foreign.Other\nclass C { @ShellMethod(key=\"run\",value=\"Unowned\") fun run(){} }",
   "import org.springframework.shell.standard.ShellMethod\nimport foreign.Other as ShellMethod\nclass C { @ShellMethod(key=\"run\",value=\"Unowned\") fun run(){} }",
  ] {
   let doc=Document{path:"/project/C.kt".into(),language:"kotlin".into(),version:1,text:text.into()};
   assert!(ShellTools::default().hover(&doc,position(text,text.find("run(){}").unwrap())).is_none(),"{text}");
  }
 }
 #[test]
 fn canonical_fqn_annotation_survives_unrelated_short_shadow_and_alias() {
  let _host=MockHost::install().with_capabilities(&[]);
  let text="import org.springframework.shell.standard.ShellMethod as Other\ntypealias ShellMethod=foreign.Other\nclass C { @org.springframework.shell.standard.ShellMethod(key=\"run\",value=\"Owned\") fun run(){} }";
  let doc=Document{path:"/project/C.kt".into(),language:"kotlin".into(),version:1,text:text.into()};
  assert!(ShellTools::default().hover(&doc,position(text,text.find("run(){}").unwrap())).is_some());
 }
 #[test]
 fn generic_shadowed_annotation_name_does_not_publish_command_hover() {
  let _host=MockHost::install().with_capabilities(&[]);
  let text="import org.springframework.shell.standard.ShellMethod; class C<ShellMethod> { @ShellMethod(key=\"run\",value=\"Unowned description\") void run(){} }";
  let doc=Document{path:"/project/C.java".into(),language:"java".into(),version:1,text:text.into()};
  assert!(ShellTools::default().hover(&doc,position(text,text.find("run(){}").unwrap())).is_none());
 }
}

#[cfg(test)]
mod availability_type_owner_regressions {
 use super::*;
 use openidea_plugin::mock::MockHost;
 #[test]
 fn generic_return_type_does_not_become_availability_indicator_candidate() {
  let _host=MockHost::install().with_capabilities(&[]);
  let text="import org.springframework.shell.standard.ShellMethod; import org.springframework.shell.standard.ShellMethodAvailability; import org.springframework.shell.Availability; class C<Availability> { @ShellMethod(key=\"run\",value=\"Run\") @ShellMethodAvailability(\"ga\") void run(){} Availability gate(){return null;} }";
  let doc=Document{path:"/project/C.java".into(),language:"java".into(),version:1,text:text.into()};
  let at=position(text,text.find("\"ga\"").unwrap()+1);
  assert!(ShellTools::default().completion(&doc,at,None).is_empty());
 }
 #[test]
 fn explicit_canonical_availability_return_type_is_a_candidate_despite_short_shadow() {
  let _host=MockHost::install().with_capabilities(&[]);
  let text="import org.springframework.shell.standard.ShellMethod; import org.springframework.shell.standard.ShellMethodAvailability; class C<Availability> { @ShellMethod(key=\"run\",value=\"Run\") @ShellMethodAvailability(\"ga\") void run(){} org.springframework.shell.Availability gate(){return null;} }";
  let doc=Document{path:"/project/C.java".into(),language:"java".into(),version:1,text:text.into()};
  let at=position(text,text.find("\"ga\"").unwrap()+1);
  assert_eq!(ShellTools::default().completion(&doc,at,None).iter().map(|i|i.label.as_str()).collect::<Vec<_>>(),["gate"]);
 }
 #[test]
 fn aliased_return_type_import_does_not_become_availability_indicator_candidate() {
  let _host=MockHost::install().with_capabilities(&[]);
  let text="import org.springframework.shell.standard.ShellMethod\nimport org.springframework.shell.standard.ShellMethodAvailability\nimport org.springframework.shell.Availability as Other\nclass C { @ShellMethod(key=\"run\",value=\"Run\") @ShellMethodAvailability(\"ga\") fun run(){} fun gate(): Availability {return null;} }";
  let doc=Document{path:"/project/C.kt".into(),language:"kotlin".into(),version:1,text:text.into()};
  let at=position(text,text.find("\"ga\"").unwrap()+1);
  assert!(ShellTools::default().completion(&doc,at,None).is_empty());
 }
}

#[cfg(test)]
mod description_literal_safety_regressions {
 use super::*;
 use openidea_plugin::{hover::HoverProvider,mock::MockHost};
 #[test]
 fn escaped_description_values_are_not_guessed_from_partial_lexer_decoding() {
  let _host=MockHost::install().with_capabilities(&[]);
  for value in [r#""\141bc""#,r#""\u0061bc""#, "\"\"\"\n  First\n   Second\n  \"\"\""] {
   let text=format!(r#"import org.springframework.shell.standard.ShellMethod; class C {{ @ShellMethod(key="run",value={value}) void run(){{}} }}"#);
   let doc=Document{path:"/project/C.java".into(),language:"java".into(),version:1,text:text.clone()};
   assert!(ShellTools::default().hover(&doc,position(&text,text.find("run(){}").unwrap())).is_none(),"{text}");
  }
 }
 #[test]
 fn owned_kotlin_interpolated_description_is_not_a_known_declared_description() {
  let _host=MockHost::install().with_capabilities(&[]);
  for text in [
   "import org.springframework.shell.standard.ShellMethod\nclass C { @ShellMethod(key=\"run\",value=\"Hello $name\") fun run(){} }",
   "import org.springframework.shell.command.annotation.Command\n@Command(description=\"Hello ${name}\") class C { @Command(command=\"run\") fun run(){} }",
  ] {
   let doc=Document{path:"/project/C.kt".into(),language:"kotlin".into(),version:1,text:text.into()};
   assert!(ShellTools::default().hover(&doc,position(text,text.find("run(){}").unwrap())).is_none(),"{text}");
  }
 }
}

#[cfg(test)]
mod java_unicode_source_safety_regression {
 use super::*;
 use openidea_plugin::{hover::HoverProvider,mock::MockHost};
 #[test]
 fn unicode_preprocessed_comment_does_not_publish_nonexistent_command_description() {
  let _host=MockHost::install().with_capabilities(&[]);
  let text=r#"import org.springframework.shell.standard.ShellMethod;
class C {
 \u002f\u002a
 @ShellMethod(key="run",value="Not a declaration") void run(){}
 \u002a\u002f
}"#;
  let doc=Document{path:"/project/C.java".into(),language:"java".into(),version:1,text:text.into()};
  let at=position(text,text.find("run(){}").unwrap());
  let mut tools=ShellTools::default();
  assert!(tools.hover(&doc,at).is_none());
  assert!(tools.diagnostics(&doc).is_empty());
  assert!(tools.line_markers(&doc).is_empty());
  assert!(tools.definition(&doc,at).is_empty());
  assert!(tools.completion(&doc,at,None).is_empty());
 }
}

#[cfg(test)]
mod provider_type_owner_regressions {
 use super::*;
 use openidea_plugin::mock::MockHost;
 fn candidates(text:&str,kotlin:bool)->Vec<CompletionItem> {
  let host=MockHost::install();let path=if kotlin{"C.kt"}else{"C.java"};host.add_file(path,text);
  let doc=Document{path:format!("/project/{path}"),language:if kotlin{"kotlin"}else{"java"}.into(),version:1,text:text.into()};
  let mut tools=ShellTools::default();tools.activate(&host.context("aritti.spring-shell")).unwrap();
  tools.completion(&doc,position(text,text.find("\"pa\"").unwrap()+1),None)
 }
 #[test]
 fn generic_availability_provider_return_type_is_not_a_known_provider_bean() {
  let text="import org.springframework.context.annotation.Bean; import org.springframework.shell.AvailabilityProvider; import org.springframework.shell.command.annotation.Command; import org.springframework.shell.command.annotation.CommandAvailability; class C<AvailabilityProvider> { @Bean AvailabilityProvider candidate(){return null;} @Command(command=\"run\") @CommandAvailability(provider=\"pa\") void run(){} }";
  assert!(candidates(text,false).is_empty());
 }
 #[test]
 fn canonical_provider_return_type_survives_short_generic_shadow() {
  let text="import org.springframework.context.annotation.Bean; import org.springframework.shell.command.annotation.Command; import org.springframework.shell.command.annotation.CommandAvailability; class C<AvailabilityProvider> { @Bean org.springframework.shell.AvailabilityProvider candidate(){return null;} @Command(command=\"run\") @CommandAvailability(provider=\"pa\") void run(){} }";
  assert_eq!(candidates(text,false).iter().map(|i|i.label.as_str()).collect::<Vec<_>>(),["candidate"]);
 }
 #[test]
 fn aliased_availability_provider_import_is_not_a_known_provider_bean() {
  let text="import org.springframework.context.annotation.Bean\nimport org.springframework.shell.AvailabilityProvider as Other\nimport org.springframework.shell.command.annotation.Command\nimport org.springframework.shell.command.annotation.CommandAvailability\nclass C { @Bean fun candidate(): AvailabilityProvider {return null;} @Command(command=\"run\") @CommandAvailability(provider=\"pa\") fun run(){} }";
  assert!(candidates(text,true).is_empty());
 }
 #[test]
 fn foreign_or_generic_subclass_return_cannot_borrow_local_provider_interface() {
  for ty in ["foreign.P","P"] {
   let generic=if ty=="P"{"<P>"}else{""};
   let text=format!("import org.springframework.context.annotation.Bean; import org.springframework.shell.command.annotation.Command; import org.springframework.shell.command.annotation.CommandAvailability; abstract class P implements org.springframework.shell.AvailabilityProvider {{}} class C{generic} {{ @Bean {ty} candidate(){{return null;}} @Command(command=\"run\") @CommandAvailability(provider=\"pa\") void run(){{}} }}");
   assert!(candidates(&text,false).is_empty(),"{ty}");
  }
 }

 #[test]
 fn unambiguous_local_provider_return_type_remains_a_candidate() {
  for ty in ["P","com.acme.P"] {
   let text=format!("package com.acme; import org.springframework.context.annotation.Bean; import org.springframework.shell.command.annotation.Command; import org.springframework.shell.command.annotation.CommandAvailability; abstract class P implements org.springframework.shell.AvailabilityProvider {{}} class C {{ @Bean {ty} candidate(){{return null;}} @Command(command=\"run\") @CommandAvailability(provider=\"pa\") void run(){{}} }}");
   assert_eq!(candidates(&text,false).iter().map(|i|i.label.as_str()).collect::<Vec<_>>(),["candidate"],"{ty}");
  }
 }

 #[test]
 fn ambiguous_provider_bean_names_are_not_presented_as_known_candidates() {
  for other in ["String","org.springframework.shell.AvailabilityProvider"] {
   let text=format!("import org.springframework.context.annotation.Bean; import org.springframework.shell.command.annotation.Command; import org.springframework.shell.command.annotation.CommandAvailability; class C {{ @Bean(\"shared\") org.springframework.shell.AvailabilityProvider first(){{return null;}} @Bean(\"shared\") {other} second(){{return null;}} @Command(command=\"run\") @CommandAvailability(provider=\"pa\") void run(){{}} }}");
   assert!(candidates(&text,false).is_empty(),"{other}");
  }
 }

 #[test]
 fn repeated_alias_of_one_physical_provider_is_not_a_second_declaration() {
  let text="import org.springframework.context.annotation.Bean; import org.springframework.shell.command.annotation.Command; import org.springframework.shell.command.annotation.CommandAvailability; class C { @Bean(name={\"shared\",\"shared\"}) org.springframework.shell.AvailabilityProvider first(){return null;} @Command(command=\"run\") @CommandAvailability(provider=\"pa\") void run(){} }";
  assert_eq!(candidates(text,false).iter().map(|i|i.label.as_str()).collect::<Vec<_>>(),["shared"]);
 }

 #[test]
 fn ambiguous_or_unproved_provider_names_do_not_pick_an_arbitrary_gutter_target() {
  for other in ["String","org.springframework.shell.AvailabilityProvider"] {
   let text=format!("import org.springframework.context.annotation.Bean; import org.springframework.shell.command.annotation.Command; import org.springframework.shell.command.annotation.CommandAvailability; class C {{ @Bean(\"shared\") org.springframework.shell.AvailabilityProvider first(){{return null;}} @Bean(\"shared\") {other} second(){{return null;}} @Command(command=\"run\") @CommandAvailability(provider=\"shared\") void run(){{}} }}");
   let host=MockHost::install();host.add_file("C.java",&text);let doc=Document{path:"/project/C.java".into(),language:"java".into(),version:1,text};
   let mut tools=ShellTools::default();tools.activate(&host.context("aritti.spring-shell")).unwrap();
   assert!(tools.line_markers(&doc).is_empty(),"{other}");
   if other=="String" {
    let mut unproved=doc.clone();unproved.version=2;
    unproved.text=unproved.text.replace("@Bean(\"shared\") org.springframework.shell.AvailabilityProvider first(){return null;}","");
    assert!(tools.line_markers(&unproved).is_empty());
   }
  }
 }

}

#[cfg(test)]
mod option_numeric_owner_regressions {
 use super::*;
 use openidea_plugin::mock::MockHost;
 #[test]
 fn aliased_foreign_numeric_type_names_do_not_trigger_builtin_format_diagnostics() {
  let _host=MockHost::install().with_capabilities(&[]);
  for (imports,ty) in [
   ("import java.lang.Integer as Other\nimport foreign.Custom as Integer\n","Integer"),
   ("import foreign.Custom as kotlin\n","kotlin.Int"),
  ] {
   let text=format!("import org.springframework.shell.command.annotation.Command\\nimport org.springframework.shell.command.annotation.Option\\n{imports}@Command class C {{ @Command(command=\\\"run\\\") fun run(@Option(defaultValue=\\\"bad\\\") n:{ty}){{}} }}").replace("\\n","\n").replace("\\\"","\"");
   let doc=Document{path:"/project/C.kt".into(),language:"kotlin".into(),version:1,text};
   assert!(!ShellTools::default().diagnostics(&doc).iter().any(|d|d.code.as_deref()==Some("option-default-format")),"{ty}");
  }
 }
}

#[cfg(test)]
mod availability_wrong_type_shadow_regressions {
 use super::*;
 use openidea_plugin::mock::MockHost;
 #[test]
 fn availability_bounded_generic_named_string_is_not_java_lang_string_return() {
  let _host=MockHost::install().with_capabilities(&[]);
  for (class_generic,method_generic) in [("<String extends org.springframework.shell.Availability>",""),("","<String extends org.springframework.shell.Availability> ")] {
   let text=format!("import org.springframework.shell.standard.ShellMethod; import org.springframework.shell.standard.ShellMethodAvailability; @org.springframework.shell.standard.ShellComponent class C{class_generic} {{ @ShellMethod(key=\"run\",value=\"Run\") void run(){{}} @ShellMethodAvailability(\"run\") {method_generic}String gate(){{return null;}} }}");
   let doc=Document{path:"/project/C.java".into(),language:"java".into(),version:1,text};
   assert!(!ShellTools::default().diagnostics(&doc).iter().any(|d|d.code.as_deref()==Some("availability-return-type")),"{class_generic}/{method_generic}");
   let exact=Document{text:doc.text.replace("String gate()","java.lang.String gate()"),..doc};
   assert!(ShellTools::default().diagnostics(&exact).iter().any(|d|d.code.as_deref()==Some("availability-return-type")));
  }
 }
}

#[cfg(test)]
mod modern_contract_tests {
    use super::*;
    use openidea_plugin::{hover::HoverProvider, mock::MockHost};
    fn keys(text:&str,kotlin:bool)->Vec<String> {
        let path=if kotlin {"/project/C.kt"} else {"/project/C.java"};
        commands(&parse_source(path,text),text).into_iter().map(|c|c.key).collect()
    }
    #[test]
    fn v3_aliases_are_separate_routes_with_the_class_alias_prefix() {
        let text="import org.springframework.shell.command.annotation.Command; @Command(command=\"db\",alias=\"database\") class C { @Command(command={\"dump\",\"all\"},alias={\"export\",\"backup all\"}) void dump(){} }";
        assert_eq!(keys(text,false),["db dump all","database export","database backup all"]);
        assert_eq!(keys(&text.replace("alias=\"database\"", "alias=PREFIX"),false),["db dump all"]);
    }
    #[test]
    fn v4_owned_names_aliases_implicit_methods_and_group_prefixes_work_in_java_and_kotlin() {
        let java="import org.springframework.shell.core.command.annotation.Command; import org.springframework.shell.core.command.annotation.CommandGroup; @CommandGroup(name=\"Database\",prefix=\"db\") class C { @Command(name={\"dump\",\"all\"},alias={\"export\",\"backup all\"},description=\"Dump data\") void dump(){} @Command(\"Show tables\") void showTables(){} }";
        assert_eq!(keys(java,false),["db dump all","export","backup all","db show-tables"]);
        let kotlin="import org.springframework.shell.core.command.annotation.Command\nclass C { @Command(name=[\"dump\",\"all\"],alias=[\"export\",\"backup all\"]) fun dump(){} @Command fun showTables(){} }";
        assert_eq!(keys(kotlin,true),["dump all","export","backup all","show-tables"]);
        assert!(keys(&java.replace("prefix=\"db\"","prefix=PREFIX"),false).is_empty());
        assert!(keys(&java.replace("org.springframework.shell.core.command.annotation.Command;","foreign.Command;"),false).is_empty());
    }
    #[test]
    fn v4_hover_uses_value_as_description_and_aliases_share_method_identity() {
        let _h=MockHost::install().with_capabilities(&[]);
        let text="import org.springframework.shell.core.command.annotation.Command; class C { @Command(name=\"show\",alias=\"list\",value=\"List data\") void show(){} }";
        let doc=Document {path:"/project/C.java".into(),language:"java".into(),version:1,text:text.into()};
        let hover=ShellTools::default().hover(&doc,position(text,text.find("\"list\"").unwrap()+1)).unwrap();
        assert!(hover.markdown.contains("show, list"));
        assert!(hover.markdown.ends_with("List data"));
    }
}

#[cfg(test)]
mod core_provider_tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    #[test]
    fn core_availability_completion_and_markers_do_not_mix_legacy_interface_identity() {
        let h=MockHost::install();
        let text="import org.springframework.context.annotation.Bean; import org.springframework.shell.core.command.annotation.Command; class C { @Bean org.springframework.shell.core.command.availability.AvailabilityProvider ready(){return null;} @Bean org.springframework.shell.AvailabilityProvider legacy(){return null;} @Command(name=\"run\",availabilityProvider=\"ready\") void run(){} }";
        h.add_file("src/main/java/C.java",text);
        let d=Document{path:"/project/src/main/java/C.java".into(),text:text.into(),language:"java".into(),version:1};
        let at=position(text,text.rfind("ready").unwrap());
        let mut tools=ShellTools::default();
        assert_eq!(tools.completion(&d,at,None).iter().map(|c|c.label.as_str()).collect::<Vec<_>>(),["ready"]);
        assert_eq!(tools.definition(&d,at).len(),1);
        let markers=tools.line_markers(&d);
        assert!(markers.iter().any(|m|m.tooltip=="Availability provider: ready"));
        assert!(!markers.iter().any(|m|m.tooltip.contains("legacy")));
        assert!(h.process_runs().is_empty()&&h.requests().is_empty());
    }
}

#[cfg(test)]
mod inherited_availability_tests {
    use super::*;
    use openidea_plugin::mock::MockHost;

    const IMPORTS: &str = "import org.springframework.shell.standard.ShellComponent; import org.springframework.shell.standard.ShellMethod; import org.springframework.shell.standard.ShellMethodAvailability; import org.springframework.shell.Availability;\n";

    fn child(name: &str, base: &str) -> String {
        format!("package p;\n{IMPORTS}@ShellComponent class Child extends {base} {{ @ShellMethod(key=\"run\",value=\"Run\") @ShellMethodAvailability(\"{name}\") void run(){{}} }}")
    }

    fn doc(path: &str, text: &str) -> Document {
        Document { path: format!("/project/src/main/java/p/{path}"), language: "java".into(), version: 1, text: text.into() }
    }

    fn run(files: &[(&str, &str)], name: &str, base: &str) -> (Vec<String>, Vec<Diagnostic>) {
        let host = MockHost::install();
        for (path, text) in files {
            host.add_file(&format!("src/main/java/p/{path}"), text);
        }
        let text = child(name, base);
        host.add_file("src/main/java/p/Child.java", &text);
        let mut tools = ShellTools::default();
        let d = doc("Child.java", &text);
        let markers = tools.line_markers(&d).into_iter().map(|m| format!("{}|{}", m.tooltip, m.target.map(|t| t.path).unwrap_or_default())).collect();
        (markers, tools.diagnostics(&d))
    }

    const BASE: &str = "package p; import org.springframework.shell.Availability; abstract class Base { Availability gate(){return null;} }";

    #[test]
    fn links_availability_method_declared_in_project_superclass_chain() {
        let mid = "package p; class Mid extends Base {}";
        let (markers, problems) = run(&[("Base.java", BASE), ("Mid.java", mid)], "gate", "Mid");
        assert_eq!(markers, ["Availability method: gate() (inherited)|/project/src/main/java/p/Base.java"]);
        assert!(problems.is_empty(), "{problems:?}");
    }

    #[test]
    fn reports_missing_name_only_for_a_fully_literal_chain() {
        let (_, problems) = run(&[("Base.java", BASE)], "nope", "Base");
        assert_eq!(problems.iter().filter(|p| p.code.as_deref() == Some("availability-method-reference")).count(), 1, "{problems:?}");
        let with_interface = "package p; abstract class Base implements java.lang.Runnable {}";
        let (_, quiet) = run(&[("Base.java", with_interface)], "nope", "Base");
        assert!(quiet.is_empty(), "{quiet:?}");
        let (_, quiet) = run(&[], "nope", "Library");
        assert!(quiet.is_empty(), "{quiet:?}");
        let (_, quiet) = run(&[("Base.java", BASE)], "toString", "Base");
        assert!(quiet.is_empty(), "{quiet:?}");
    }

    #[test]
    fn ambiguous_or_non_class_ancestors_stay_quiet() {
        let twin = "package p; class Base { }";
        let (markers, quiet) = run(&[("Base.java", BASE), ("sub/Base.java", twin)], "gate", "Base");
        assert!(markers.is_empty() && quiet.is_empty(), "{markers:?} {quiet:?}");
        let (_, quiet) = run(&[("Base.java", "package p; interface Base { }")], "nope", "Base");
        assert!(quiet.is_empty(), "{quiet:?}");
    }

    #[test]
    fn duplicate_command_names_the_other_file() {
        let host = MockHost::install();
        let text = format!("package p;\n{IMPORTS}@ShellComponent class A {{ @ShellMethod(key=\"run\",value=\"Run\") void a(){{}} }}");
        host.add_file("src/main/java/p/A.java", &text);
        host.add_file("src/main/java/p/B.java", &text.replace("class A", "class B"));
        let mut tools = ShellTools::default();
        tools.line_markers(&doc("A.java", &text));
        let problems = tools.diagnostics(&doc("A.java", &text));
        let message = problems.iter().find(|p| p.code.as_deref() == Some("duplicate-command")).map(|p| p.message.clone()).unwrap_or_default();
        assert!(message.ends_with("B.a in /project/src/main/java/p/B.java."), "{message}");
    }

    fn lint(files: &[(&str, &str)], name: &str) -> (Vec<String>, Vec<Diagnostic>) {
        let host = MockHost::install();
        for (path, text) in files {
            host.add_file(&format!("src/main/java/p/{path}"), text);
        }
        let text = files.iter().find(|(p, _)| *p == name).unwrap().1;
        let mut tools = ShellTools::default();
        let d = doc(name, text);
        let markers = tools.line_markers(&d).into_iter().map(|m| format!("{}|{}", m.tooltip, m.target.map(|t| t.path).unwrap_or_default())).collect();
        (markers, tools.diagnostics(&d))
    }

    const CHILD_CONVENTIONAL: &str = "package p;\nimport org.springframework.shell.standard.ShellComponent; import org.springframework.shell.standard.ShellMethod; import org.springframework.shell.standard.ShellMethodAvailability; import org.springframework.shell.Availability;\n@ShellComponent class Child extends Base { @ShellMethod(key=\"run\",value=\"Run\") void run(){} @ShellMethodAvailability(\"*\") Availability any(){return null;} }";
    const BASE_CONVENTIONAL: &str = "package p; import org.springframework.shell.Availability; abstract class Base { Availability runAvailability(){return null;} }";

    #[test]
    fn inherited_conventional_availability_beats_local_wildcard_marker() {
        let (markers, problems) = lint(&[("Base.java", BASE_CONVENTIONAL), ("Child.java", CHILD_CONVENTIONAL)], "Child.java");
        assert_eq!(markers, ["Availability method: runAvailability() (inherited)|/project/src/main/java/p/Base.java"], "{problems:?}");
    }

    #[test]
    fn unknown_chain_suppresses_local_wildcard_fallback_and_missing_chain_keeps_it() {
        let (markers, _) = lint(&[("Child.java", CHILD_CONVENTIONAL)], "Child.java");
        assert!(markers.is_empty(), "{markers:?}");
        let base = "package p; abstract class Base { }";
        let (markers, _) = lint(&[("Base.java", base), ("Child.java", CHILD_CONVENTIONAL)], "Child.java");
        assert_eq!(markers, ["Availability method: any()|/project/src/main/java/p/Child.java"]);
    }

    const BASE_COMMANDS: &str = "package p; import org.springframework.shell.standard.ShellComponent; import org.springframework.shell.standard.ShellMethod; import org.springframework.shell.standard.ShellMethodAvailability; abstract class Base { @ShellMethod(key=\"shared\",value=\"S\") void shared(){} @ShellMethod(key=\"over\",value=\"O\") void over(){} }";

    fn child_class(name: &str, body: &str) -> String {
        format!("package p; import org.springframework.shell.standard.ShellComponent; import org.springframework.shell.standard.ShellMethod; import org.springframework.shell.standard.ShellMethodAvailability; @ShellComponent class {name} extends Base {{ {body} }}")
    }

    fn duplicates(problems: &[Diagnostic]) -> usize {
        problems.iter().filter(|p| p.code.as_deref() == Some("duplicate-command")).count()
    }

    #[test]
    fn inherited_shell_method_commands_belong_to_the_subclass() {
        let child = child_class("Child", "@ShellMethod(key=\"own\",value=\"O\") void own(){} @ShellMethodAvailability(\"shared\") org.springframework.shell.Availability gate(){return null;}");
        let host = MockHost::install();
        host.add_file("src/main/java/p/Base.java", BASE_COMMANDS);
        host.add_file("src/main/java/p/Child.java", &child);
        let mut tools = ShellTools::default();
        let d = doc("Child.java", &child);
        tools.line_markers(&d);
        let index = tools.indexes.for_document(&d.path, &d.text, PROVIDER_BUDGET).unwrap();
        let mut keys: Vec<_> = named(index, "p.Child", "shared").into_iter().map(|(p, c)| (p, c.method)).collect();
        keys.sort();
        assert_eq!(keys, [("/project/src/main/java/p/Base.java".to_string(), "shared".to_string())]);
        assert!(named(index, "p.Child", "own").len() == 1);
    }

    #[test]
    fn sibling_subclasses_inheriting_one_command_conflict_but_overrides_and_unknown_chains_do_not() {
        let one = child_class("One", "");
        let two = child_class("Two", "");
        let (_, problems) = lint(&[("Base.java", BASE_COMMANDS), ("One.java", &one), ("Two.java", &two)], "One.java");
        assert_eq!(duplicates(&problems), 2, "{problems:?}");
        let (_, problems) = lint(&[("Base.java", BASE_COMMANDS), ("One.java", &one)], "One.java");
        assert_eq!(duplicates(&problems), 0, "{problems:?}");
        let overriding = child_class("Two", "@Override void shared(){} @Override void over(){}");
        let (_, problems) = lint(&[("Base.java", BASE_COMMANDS), ("One.java", &one), ("Two.java", &overriding)], "One.java");
        assert_eq!(duplicates(&problems), 0, "{problems:?}");
        let unknown = one.replace("extends Base", "extends Library");
        let (_, problems) = lint(&[("Base.java", BASE_COMMANDS), ("One.java", &unknown), ("Two.java", &two)], "One.java");
        assert_eq!(duplicates(&problems), 0, "{problems:?}");
    }
}
