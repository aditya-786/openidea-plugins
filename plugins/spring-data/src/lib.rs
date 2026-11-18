//! Spring Data Tools for OpenIDEA: checks derived query method names of
//! Spring Data repositories against the domain class, completes them, links
//! repositories to their entities and checks `@Query` parameters.
//! Bounded cached source model: exact package/import ownership, without classpath inference.

use openidea_plugin::providers::{CodeAction, CompletionItem, CompletionKind, Document, LineMarker, Providers};
use openidea_plugin::text::offset;
use openidea_plugin::{
    Diagnostic, Error, Invocation, Location, NotificationLevel, PickItem, Plugin, Position, Range, Severity, TextEdit, host,
    register_plugin,
};
use spring_text::index::{Index, Indexes};
use spring_text::java::{self, JavaFile, Member, TypeDecl, TypeRef};
use spring_text::{SOURCE_GLOBS, is_kotlin, parse_source, range};
use std::collections::BTreeMap;
use std::rc::Rc;

mod checks;
mod injections;
mod generation;
mod jpql;
mod live;
mod members;
mod mongo;
mod types;

const SUBJECTS: [&str; 10] = ["find", "read", "get", "query", "search", "stream", "count", "exists", "delete", "remove"];
/// `CrudRepository`/`JpaRepository` methods: implemented by the store, never derived.
const BUILT_IN: [&str; 9] = [
    "findById",
    "existsById",
    "getById",
    "getReferenceById",
    "deleteById",
    "findAllById",
    "deleteAllById",
    "deleteAllByIdInBatch",
    "findBy",
];
/// Parameters a derived query takes besides its criteria.
const SPECIAL_PARAMS: [&str; 7] = ["Pageable", "Sort", "Limit", "ScrollPosition", "OffsetScrollPosition", "KeysetScrollPosition", "Class"];
/// Types whose element is the property type for path traversal.
const CONTAINERS: [&str; 7] = ["List", "Set", "SortedSet", "Collection", "Iterable", "Map", "Optional"];
/// Operator keywords and the arguments they take (`None`: depends on the store).
const OPERATORS: [(&str, Option<usize>); 58] = [
    ("IsNotNull", Some(0)),
    ("NotNull", Some(0)),
    ("IsNull", Some(0)),
    ("Null", Some(0)),
    ("IsBetween", Some(2)),
    ("Between", Some(2)),
    ("IsLessThanEqual", Some(1)),
    ("LessThanEqual", Some(1)),
    ("IsLessThan", Some(1)),
    ("LessThan", Some(1)),
    ("IsGreaterThanEqual", Some(1)),
    ("GreaterThanEqual", Some(1)),
    ("IsGreaterThan", Some(1)),
    ("GreaterThan", Some(1)),
    ("IsBefore", Some(1)),
    ("Before", Some(1)),
    ("IsAfter", Some(1)),
    ("After", Some(1)),
    ("IsNotLike", Some(1)),
    ("NotLike", Some(1)),
    ("IsLike", Some(1)),
    ("Like", Some(1)),
    ("IsStartingWith", Some(1)),
    ("StartingWith", Some(1)),
    ("StartsWith", Some(1)),
    ("IsEndingWith", Some(1)),
    ("EndingWith", Some(1)),
    ("EndsWith", Some(1)),
    ("IsNotEmpty", Some(0)),
    ("NotEmpty", Some(0)),
    ("IsEmpty", Some(0)),
    ("Empty", Some(0)),
    ("IsNotContaining", Some(1)),
    ("NotContaining", Some(1)),
    ("NotContains", Some(1)),
    ("IsContaining", Some(1)),
    ("Containing", Some(1)),
    ("Contains", Some(1)),
    ("IsNotIn", Some(1)),
    ("NotIn", Some(1)),
    ("IsIn", Some(1)),
    ("In", Some(1)),
    ("IsNear", None),
    ("Near", None),
    ("IsWithin", None),
    ("Within", None),
    ("MatchesRegex", Some(1)),
    ("Matches", Some(1)),
    ("Regex", Some(1)),
    ("Exists", Some(0)),
    ("IsTrue", Some(0)),
    ("True", Some(0)),
    ("IsFalse", Some(0)),
    ("False", Some(0)),
    ("IsNot", Some(1)),
    ("Not", Some(1)),
    ("Is", Some(1)),
    ("Equals", Some(1)),
];
/// What completion offers after a complete property.
const CONTINUATIONS: [&str; 26] = [
    "And",
    "Or",
    "OrderBy",
    "IgnoreCase",
    "Not",
    "In",
    "NotIn",
    "Between",
    "LessThan",
    "LessThanEqual",
    "GreaterThan",
    "GreaterThanEqual",
    "Before",
    "After",
    "IsNull",
    "IsNotNull",
    "Like",
    "NotLike",
    "StartingWith",
    "EndingWith",
    "Containing",
    "NotContaining",
    "True",
    "False",
    "IsEmpty",
    "IsNotEmpty",
];

/// A persistent property: a field or a record component.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prop {
    pub name: String,
    /// Simple type name, collections unwrapped to their element.
    pub ty: String,
    pub range: Range,
    pub embedded: bool,
    /// Exact cached source type owner; None is reserved for historical lexical fixtures.
    pub target: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Class {
    pub name: String,
    pub range: Range,
    pub parent: Option<String>,
    pub props: Vec<Prop>,
    /// The JPA entity name of an `@Entity` class.
    pub entity: Option<String>,
    /// A property is the identifier (`@Id`, or named `id`).
    pub has_id: bool,
    /// Column and field names given by `@Field`, `@Column` and `@JsonProperty`.
    pub aliases: Vec<String>,
    pub record: bool,
    /// Getter names of a closed interface projection (no parents, no `@Value`, only getters).
    pub getters: Option<Vec<String>>,
}

fn element(ty: &TypeRef) -> String {
    match ty.args.last() {
        Some(arg) if CONTAINERS.contains(&ty.name.as_str()) => arg.name.clone(),
        _ => ty.name.clone(),
    }
}

/// The classes and records of a Java or Kotlin file with their persistent properties.
fn classes(path: &str, text: &str) -> Option<Vec<Class>> {
    let file = parse_source(path, text);
    let kotlin = is_kotlin(path);
    let out: Vec<Class> = file
        .types
        .iter()
        .filter(|t| t.kind == "class" || t.kind == "record" || (t.kind == "interface" && closed_getters(t, kotlin).is_some()))
        .map(|t| class(text, t, kotlin))
        .collect();
    (!out.is_empty()).then_some(out)
}

/// Property names of a Java interface made only of getters; `None` when it is open or not provably closed.
fn closed_getters(t: &TypeDecl, kotlin: bool) -> Option<Vec<String>> {
    if kotlin || !t.extends.is_empty() || t.annotation("Value").is_some() {
        return None;
    }
    let mut out = Vec::new();
    for m in t.methods() {
        if m.body.is_some() || m.modifiers.iter().any(|x| x == "default" || x == "static") {
            continue;
        }
        if !m.params.is_empty() || m.has(&["Value"]) {
            return None;
        }
        let bare = m.name.strip_prefix("get").or_else(|| m.name.strip_prefix("is")).filter(|b| b.starts_with(char::is_uppercase))?;
        out.push(bare.to_owned());
    }
    (!out.is_empty()).then_some(out)
}

fn class(text: &str, t: &TypeDecl, kotlin: bool) -> Class {
    let prop = |name: &str, at: usize, ty: &TypeRef| Prop {
        name: name.to_owned(),
        ty: element(ty),
        range: range(text, at, at + name.len()),
        embedded: false,
        target: None,
    };
    let mut props: Vec<Prop> = t.components.iter().map(|p| prop(&p.name, p.at, &p.ty)).collect();
    let persistent = t.fields().filter(|f| !f.modifiers.iter().any(|m| m == "static" || m == "transient") && !f.has(&["Transient"]));
    for f in persistent {
        let Some(ty) = &f.ty else { continue };
        props.push(Prop { embedded: f.has(&["Embedded"]), ..prop(&f.name, f.at, ty) });
        if kotlin {
            continue;
        }
        // `int a, b;`: the scanner keeps the first declarator only.
        let toks = java::tokens(&text[f.at..f.end]);
        for part in java::split_commas(&toks, true).into_iter().skip(1) {
            if let Some(n) = part.first().filter(|t| t.is_ident()) {
                props.push(prop(n.s, f.at + n.at, ty));
            }
        }
    }
    // Spring Data also sees bean properties without a field (`getFullName()`).
    for m in t.methods().filter(|m| m.params.is_empty() && !m.has(&["Transient"]) && !m.modifiers.iter().any(|x| x == "static")) {
        let Some(ty) = &m.ty else { continue };
        let bare = m.name.strip_prefix("get").or_else(|| m.name.strip_prefix("is"));
        if let Some(bare) = bare.filter(|b| b.starts_with(char::is_uppercase))
            && !props.iter().any(|p| p.name == uncapitalize(bare))
        {
            props.push(Prop {
                name: uncapitalize(bare),
                ty: element(ty),
                range: range(text, m.at, m.at + m.name.len()),
                embedded: false,
                target: None,
            });
        }
    }
    let parent = if t.kind == "class" { t.extends.first().map(|e| e.name.clone()) } else { None };
    let entity = t.annotation("Entity").map(|a| a.strings(&["name"]).first().map_or_else(|| t.name.clone(), |s| s.value.clone()));
    let has_id =
        t.fields().any(|f| f.has(&["Id"]) || f.name == "id") || t.components.iter().any(|c| c.annotation("Id").is_some() || c.name == "id");
    let annotated = t.fields().flat_map(|f| f.annotations.iter()).chain(t.components.iter().flat_map(|c| c.annotations.iter()));
    let aliases = annotated
        .filter(|a| matches!(a.name.as_str(), "Field" | "Column" | "JsonProperty"))
        .flat_map(|a| a.strings(&["value", "name"]).into_iter().map(|s| s.value.clone()))
        .collect();
    Class { name: t.name.clone(), range: range(text, t.at, t.at + t.name.len()), parent, props, entity, has_id, aliases, record: t.kind == "record", getters: closed_getters(t, kotlin) }
}

/// Equal ignoring case, or one edit away from names of 5+ characters, two from names of 8+.
fn is_near_miss(known: &str, name: &str) -> bool {
    if known.eq_ignore_ascii_case(name) {
        return true;
    }
    let shorter = known.chars().count().min(name.chars().count());
    let distance = edit_distance(known, name);
    (distance == 1 && shorter >= 5) || (distance == 2 && shorter >= 8)
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (diagonal + usize::from(ca != *cb)).min(above + 1).min(row[j] + 1);
            diagonal = above;
        }
    }
    row[b.len()]
}

/// The project's classes by simple name.
struct Model<'a> {
    by_name: BTreeMap<&'a str, Vec<(&'a str, &'a Class)>>,
    /// `@Entity` classes by entity name.
    entities: BTreeMap<&'a str, &'a Class>,
    /// Every project file has been read.
    complete: bool,
    facts: Rc<live::Facts>,
}

impl<'a> Model<'a> {
    fn unknown() -> Self {
        Self { by_name: BTreeMap::new(), entities: BTreeMap::new(), complete: false, facts: Rc::default() }
    }

    fn is_ambiguous(&self, name: &str) -> bool {
        self.facts.ambiguous.contains(name)
    }

    fn identity(&self, class: &Class) -> String {
        self.by_name
            .iter()
            .find(|(key, entries)| key.starts_with("@type:") && entries.iter().any(|(_, c)| std::ptr::eq(*c, class)))
            .map(|(key, _)| (*key).to_owned())
            .unwrap_or_else(|| class.name.clone())
    }
    fn label(&self, name: &str) -> String {
        self.class(name).map(|(_, c)| c.name.clone()).unwrap_or_else(|| name.to_owned())
    }
    /// The known entity or class name that `name` is probably a typo of.
    fn near_miss(&self, name: &str) -> Option<&'a str> {
        let known = self.entities.keys().chain(self.by_name.keys().filter(|k| !k.starts_with("@type:"))).copied();
        known.filter(|k| is_near_miss(k, name)).min_by_key(|k| edit_distance(k, name))
    }

    fn class(&self, name: &str) -> Option<(&'a str, &'a Class)> {
        self.by_name.get(name).filter(|c| c.len() == 1).and_then(|c| c.first()).copied()
    }

    /// Every property of `name` and its superclasses; `None` unless all of them are project classes.
    fn props(&self, name: &str) -> Option<Vec<(&'a str, &'a Prop)>> {
        let mut out = Vec::new();
        let mut next = Some(name);
        let mut types = std::collections::BTreeSet::new();
        let mut properties = std::collections::BTreeSet::new();
        for _ in 0..16 {
            let Some(name) = next else {
                return Some(out);
            };
            let (path, class) = self.class(name)?;
            if !types.insert((path, class.name.as_str())) {
                return None;
            }
            for prop in &class.props {
                if !properties.insert(prop.name.as_str()) {
                    return None;
                }
                out.push((path, prop));
            }
            next = class.parent.as_deref();
        }
        None
    }
}
fn target(prop: &Prop) -> &str {
    prop.target.as_deref().unwrap_or(&prop.ty)
}

/// The domain type of each repository interface: `(index in file.types, simple name)`.
pub(crate) fn repositories(file: &JavaFile) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for (i, t) in file.types.iter().enumerate() {
        if t.kind != "interface" || t.has(&["NoRepositoryBean"]) {
            continue;
        }
        let defined = t.annotation("RepositoryDefinition").and_then(|a| a.arg("domainClass")).map(|a| {
            a.text.trim_end_matches(".class").trim_end_matches("::class").rsplit('.').next().unwrap_or_default().trim().to_owned()
        });
        let extended = || {
            t.extends
                .iter()
                .find(|e| (e.name.ends_with("Repository") || e.name.ends_with("RepositoryBase")) && !e.args.is_empty())
                .map(|e| e.args[0].name.clone())
        };
        if let Some(domain) = defined.or_else(extended) {
            out.push((i, domain));
        }
    }
    out
}

/// A property part of a derived query method name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part {
    /// The property path's bytes in the method name.
    pub from: usize,
    pub to: usize,
    /// Arguments the operator takes (`None`: depends on the store); 0 for an `OrderBy` part.
    pub args: Option<usize>,
}

/// Whether `kw` starts at byte `i` of `s` as a keyword: followed by an upper-case letter.
fn keyword_at(s: &str, i: usize, kw: &str) -> bool {
    s.is_char_boundary(i) && s[i..].starts_with(kw) && s[i + kw.len()..].chars().next().is_some_and(char::is_uppercase)
}

/// `s` split at `kw` keywords: byte ranges of the pieces.
fn split_keyword(s: &str, kw: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = 0;
    for i in 1..s.len() {
        if i > start && keyword_at(s, i, kw) {
            out.push((start, i));
            start = i + kw.len();
        }
    }
    out.push((start, s.len()));
    out
}

/// The predicate start of a derived query name: the byte after `By`.
fn predicate_start(name: &str) -> Option<usize> {
    let verb = SUBJECTS.iter().find(|v| name.starts_with(**v))?;
    if name[verb.len()..].chars().next().is_some_and(|c| !c.is_uppercase()) {
        return None; // `getter`, `searchable`
    }
    Some(verb.len() + name[verb.len()..].find("By")? + 2)
}

/// The property parts of a derived query method name, per Spring Data's
/// method name grammar; `None` when it isn't one.
pub fn derived(name: &str) -> Option<Vec<Part>> {
    if BUILT_IN.contains(&name) {
        return None;
    }
    let at = predicate_start(name)?;
    let pred = &name[at..];
    let (criteria, order) = match pred.find("OrderBy") {
        Some(i) => (&pred[..i], Some(i + "OrderBy".len())),
        None => (pred, None),
    };
    let criteria = ["AllIgnoreCase", "AllIgnoringCase"].iter().find_map(|s| criteria.strip_suffix(s)).unwrap_or(criteria);
    let mut parts = Vec::new();
    for (of, ot) in split_keyword(criteria, "Or") {
        for (af, ato) in split_keyword(&criteria[of..ot], "And") {
            let part = &criteria[of + af..of + ato];
            let part = ["IgnoreCase", "IgnoringCase"].iter().find_map(|s| part.strip_suffix(s)).unwrap_or(part);
            let op = OPERATORS.iter().filter(|(k, _)| part.len() > k.len() && part.ends_with(k)).max_by_key(|(k, _)| k.len());
            let len = part.len() - op.map_or(0, |(k, _)| k.len());
            let from = at + of + af;
            if len > 0 {
                parts.push(Part { from, to: from + len, args: op.map_or(Some(1), |(_, a)| *a) });
            }
        }
    }
    if let Some(o) = order {
        let clause = &pred[o..];
        let mut start = 0;
        for i in (1..=clause.len()).filter(|&i| clause.is_char_boundary(i)) {
            let end = i == clause.len();
            let dir = ["Asc", "Desc"].iter().find(|d| clause[..i].ends_with(**d) && i - d.len() > start);
            if end || (dir.is_some() && clause[i..].chars().next().is_some_and(char::is_uppercase)) {
                let to = i - dir.map_or(0, |d| d.len());
                if to > start {
                    parts.push(Part { from: at + o + start, to: at + o + to, args: Some(0) });
                }
                start = i;
            }
        }
    }
    Some(parts)
}

/// How a property path resolved.
#[derive(Debug, PartialEq, Eq)]
enum Resolved<'a> {
    /// Each step: its bytes in the method name and the property.
    Found(Vec<(usize, usize, &'a str, &'a Prop)>),
    /// A traversed type isn't a project class: nothing can be said.
    Unknown,
    Missing {
        from: usize,
        to: usize,
        name: String,
        ty: String,
    },
}

fn uncapitalize(s: &str) -> String {
    let mut chars = s.chars();
    chars.next().map(|c| c.to_lowercase().chain(chars).collect()).unwrap_or_default()
}

/// Resolves the camel-case path `s` (at byte `at` of the name) on `ty`:
/// the whole as one property, else the longest head that is one, then the rest on its type.
fn resolve_camel<'a>(model: &Model<'a>, ty: &str, s: &str, at: usize) -> Resolved<'a> {
    let Some(props) = model.props(ty) else {
        return Resolved::Unknown;
    };
    let find = |s: &str| {
        let lower = uncapitalize(s);
        let mut matches = props.iter().filter(|(_, p)| p.name == s || p.name == lower);
        let first = matches.next().copied();
        if matches.next().is_some() { None } else { first }
    };
    if let Some((path, p)) = find(s) {
        return Resolved::Found(vec![(at, at + s.len(), path, p)]);
    }
    let heads = s.char_indices().skip(1).filter(|(_, c)| c.is_uppercase()).map(|(i, _)| i).collect::<Vec<_>>();
    for &k in heads.iter().rev() {
        let Some((path, p)) = find(&s[..k]) else {
            continue;
        };
        match resolve_camel(model, target(p), &s[k..], at + k) {
            Resolved::Found(mut steps) => {
                steps.insert(0, (at, at + k, path, p));
                return Resolved::Found(steps);
            }
            Resolved::Unknown => return Resolved::Unknown,
            missing @ Resolved::Missing { .. } => return missing,
        }
    }
    Resolved::Missing { from: at, to: at + s.len(), name: uncapitalize(s), ty: model.label(ty) }
}

/// Resolves the property path `name[from..to]` on `ty`; `_` separates explicitly.
fn resolve<'a>(model: &Model<'a>, ty: &str, name: &str, from: usize, to: usize) -> Resolved<'a> {
    let mut ty = ty.to_owned();
    let mut steps = Vec::new();
    let mut at = from;
    for piece in name[from..to].split('_') {
        if piece.is_empty() {
            return Resolved::Unknown;
        }
        match resolve_camel(model, &ty, piece, at) {
            Resolved::Found(found) => {
                ty = found.last().map(|s| target(s.3).to_owned()).unwrap_or_default();
                steps.extend(found);
            }
            other => return other,
        }
        at += piece.len() + 1;
    }
    Resolved::Found(steps)
}

pub(crate) fn derived_method(m: &Member) -> bool {
    !m.has(&["Query", "NativeQuery", "Procedure"]) && m.body.is_none() && !m.modifiers.iter().any(|x| x == "default" || x == "static")
}

/// The `:name` parameters of a query, by byte offset; SpEL, quotes and `::` casts skipped.
fn named_parameters(q: &str) -> Vec<(usize, &str)> {
    let b = q.as_bytes();
    let mut out = Vec::new();
    let mut quote = None;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        match (quote, c) {
            (Some(open), _) => {
                if c == open {
                    quote = None;
                }
            }
            (None, b'\'' | b'"') => quote = Some(c),
            (None, b'#') if b.get(i + 1) == Some(&b'{') => {
                i += b[i..].iter().position(|&c| c == b'}').unwrap_or(b.len() - i);
            }
            (None, b':') if b.get(i + 1) == Some(&b':') => i += 1,
            (None, b':') => {
                let start = i + 1;
                let len = b[start..].iter().take_while(|c| c.is_ascii_alphanumeric() || **c == b'_').count();
                if len > 0 && !b[start].is_ascii_digit() {
                    out.push((start, &q[start..start + len]));
                }
                i = start + len;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    out
}

/// What a path of properties led to.
enum Walk {
    Type(String),
    /// A type on the way isn't a project class.
    Unknown,
    /// The segment at this index isn't a property.
    Missing(usize, String),
}

fn walk(model: &Model, ty: &str, segments: &[&str]) -> Walk {
    let mut ty = ty.to_owned();
    for (i, segment) in segments.iter().enumerate() {
        let Some(props) = model.props(&ty) else {
            return Walk::Unknown;
        };
        let Some((_, prop)) = props.into_iter().find(|(_, p)| p.name == *segment) else {
            return Walk::Missing(i, model.label(&ty));
        };
        ty = target(prop).to_owned();
    }
    Walk::Type(ty)
}

/// The type of each identification variable (lower-cased) of a query's declarations.
fn alias_types(model: &Model, declarations: &[jpql::Decl]) -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    let mut conflicting = Vec::new();
    for decl in declarations {
        let Some(alias) = decl.alias else { continue };
        let ty = if decl.is_path() {
            let segments: Vec<&str> = decl.target.s.split('.').collect();
            let Some(root) = out.get(&segments[0].to_ascii_lowercase()) else {
                continue;
            };
            match walk(model, root, &segments[1..]) {
                Walk::Type(ty) => ty,
                _ => continue,
            }
        } else {
            model.entities.get(decl.target.s).map_or_else(|| decl.target.s.to_owned(), |c| model.identity(c))
        };
        let key = alias.to_ascii_lowercase();
        if out.get(&key).is_some_and(|known| *known != ty) {
            conflicting.push(key);
            continue;
        }
        out.insert(key, ty);
    }
    for key in conflicting {
        out.remove(&key);
    }
    out
}

/// A problem of a JPQL query: its bytes in the query and the diagnostic code and message.
struct QueryProblem {
    from: usize,
    to: usize,
    code: &'static str,
    message: String,
}

/// Unknown entities and unknown properties on identification variables of a project entity.
fn query_problems(model: &Model, query: &str, domain_in_project: bool) -> Vec<QueryProblem> {
    let mut out = Vec::new();
    if !model.complete {
        return out;
    }
    let words = jpql::words(query);
    let declarations = jpql::declarations(&words);
    if domain_in_project {
        for decl in declarations.iter().filter(|d| !d.is_path()) {
            if model.entities.contains_key(decl.target.s) || model.class(decl.target.s).is_some() || model.is_ambiguous(decl.target.s) {
                continue;
            }
            let Some(known) = model.near_miss(decl.target.s) else {
                continue;
            };
            let message = format!("`{}` is not an entity of this project. Did you mean `{known}`?", decl.target.s);
            out.push(QueryProblem {
                from: decl.target.at,
                to: decl.target.at + decl.target.s.len(),
                code: "unknown-query-entity",
                message,
            });
        }
    }
    let aliases = alias_types(model, &declarations);
    for word in words.iter().filter(|w| w.is_name() && w.s.contains('.')) {
        let segments: Vec<&str> = word.s.split('.').collect();
        let Some(root) = aliases.get(&segments[0].to_ascii_lowercase()) else {
            continue;
        };
        let rest: Vec<&str> = segments[1..].iter().copied().take_while(|s| *s != "class" && !s.is_empty()).collect();
        if let Walk::Missing(i, ty) = walk(model, root, &rest) {
            let from = word.at + segments[..=i].iter().map(|s| s.len() + 1).sum::<usize>();
            out.push(QueryProblem {
                from,
                to: from + rest[i].len(),
                code: "unknown-query-property",
                message: format!("`{ty}` has no property `{}`.", rest[i]),
            });
        }
    }
    out
}

fn warning(text: &str, from: usize, to: usize, code: &str, message: String) -> Diagnostic {
    Diagnostic::new(range(text, from, to), Severity::Warning, message).code(code)
}

/// The problems of the repositories of a file.
fn problems(model: &Model, text: &str, file: &JavaFile) -> Vec<Diagnostic> {
    let mongo = text.contains("springframework.data.mongodb");
    let mut out = Vec::new();
    for (ti, domain) in repositories(file) {
        for m in file.types[ti].methods() {
            if mongo && let Some(query) = m.annotation("Query") {
                out.extend(mongo_problems(model, &domain, query, text));
            }
            if let Some(query) = m.annotation("Query") {
                let names: Vec<String> = m
                    .params
                    .iter()
                    .map(|p| {
                        let bound = p.annotation("Param").and_then(|a| a.strings(&["value"]).first().map(|s| s.value.clone()));
                        bound.unwrap_or_else(|| p.name.clone())
                    })
                    .collect();
                for s in query.strings(&["value"]) {
                    if s.value.trim_start().starts_with(['{', '[']) {
                        continue; // a JSON query (MongoDB, Elasticsearch)
                    }
                    let exact = s.value.len() == s.end - s.at;
                    let native = query.arg("nativeQuery").is_some_and(|a| a.text.trim() == "true");
                    let single_literal = query.arg("value").is_none_or(|a| a.strings.len() == 1 && !a.text.contains('+'));
                    if !native && single_literal {
                        for p in query_problems(model, &s.value, model.class(&domain).is_some()) {
                            let (from, to) = if exact { (s.at + p.from, s.at + p.to) } else { (s.at, s.end) };
                            out.push(warning(text, from, to, p.code, p.message));
                        }
                    }
                    for (at, name) in named_parameters(&s.value) {
                        if names.iter().any(|n| n == name) {
                            continue;
                        }
                        let (from, to) = if exact { (s.at + at - 1, s.at + at + name.len()) } else { (s.at, s.end) };
                        let message = format!(
                            "No method parameter is bound to `:{name}`. Name a parameter `{name}` or annotate it with @Param(\"{name}\")."
                        );
                        out.push(warning(text, from, to, "unbound-query-parameter", message));
                    }
                }
            }
            let delete = m.name.starts_with("delete") || m.name.starts_with("remove");
            if let Some(a) = m.annotation("Modifying")
                && !m.has(&["Query", "NativeQuery"])
                && !(delete && derived_method(m))
            {
                out.push(warning(text, a.at, a.end, "modifying-without-query", "@Modifying only applies to @Query methods.".into()));
            }
            if text.contains("data.jpa") && let Some(a) = m.annotation("EntityGraph") {
                out.extend(entity_graph_problems(model, &domain, a, text));
            }
            if !derived_method(m) {
                continue;
            }
            let Some(parts) = derived(&m.name) else {
                continue;
            };
            out.extend(projection_problems(model, &domain, m, text));
            for part in &parts {
                if let Resolved::Missing { from, to, name, ty } = resolve(model, &domain, &m.name, part.from, part.to) {
                    out.push(warning(text, m.at + from, m.at + to, "unknown-property", format!("`{ty}` has no property `{name}`.")));
                }
            }
            let expected: Option<usize> = parts.iter().map(|p| p.args).sum();
            let declared = m.params.iter().filter(|p| !SPECIAL_PARAMS.contains(&p.ty.name.as_str())).count();
            if let Some(expected) = expected
                && expected != declared
            {
                let message = format!("`{}` needs {expected} argument(s) for its criteria but declares {declared}.", m.name);
                out.push(warning(text, m.at, m.at + m.name.len(), "query-argument-count", message));
            }
        }
    }
    out
}

const RESULT_WRAPPERS: [&str; 9] = ["List", "Set", "Collection", "Iterable", "Page", "Slice", "Stream", "Streamable", "Optional"];

/// A property the entity provably has: the bean spelling of a getter (`getURL` is `URL`, `getName` is `name`).
fn has_bean_property(props: &[(&str, &Prop)], bare: &str) -> bool {
    let mut chars = bare.chars();
    let upper_pair = chars.next().is_some_and(char::is_uppercase) && chars.next().is_some_and(char::is_uppercase);
    props.iter().any(|(_, p)| p.name == bare || (!upper_pair && p.name == uncapitalize(bare)))
}

/// A closed interface projection or a record DTO returned by a derived query must only name properties of the domain class.
fn projection_problems(model: &Model, domain: &str, m: &Member, text: &str) -> Vec<Diagnostic> {
    let Some(ty) = &m.ty else { return Vec::new() };
    let verb = ["find", "read", "get", "query", "search", "stream"].iter().any(|v| m.name.starts_with(v));
    if !verb || m.params.iter().any(|p| p.ty.name == "Class") {
        return Vec::new();
    }
    let result = match ty.args.last() {
        Some(arg) if RESULT_WRAPPERS.contains(&ty.name.as_str()) => arg,
        _ => ty,
    };
    if result.name == domain || result.array || !result.args.is_empty() {
        return Vec::new();
    }
    let Some((_, class)) = model.class(&result.name) else { return Vec::new() };
    let Some(props) = model.props(domain) else { return Vec::new() };
    if class.entity.is_some() {
        return Vec::new();
    }
    let at = (result.at, result.at + result.name.len());
    let mut out = Vec::new();
    let missing = |what: &str, name: &str| {
        warning(text, at.0, at.1, "projection-unknown-property", format!("`{}` has no property `{name}` for {what} `{}`.", model.label(domain), result.name))
    };
    if let Some(getters) = &class.getters {
        for bare in getters.iter().filter(|b| !has_bean_property(&props, b)) {
            out.push(missing("projection", &uncapitalize(bare)));
        }
    } else if class.record {
        for component in &class.props {
            let Some((_, property)) = props.iter().find(|(_, p)| p.name == component.name) else {
                out.push(missing("record", &component.name));
                continue;
            };
            if let Some(message) = types::kinds_differ(&property.ty, &component.ty) {
                let message = format!("Record `{}` component `{}`: {message}", result.name, component.name);
                out.push(Diagnostic::new(range(text, at.0, at.1), Severity::Info, message).code("projection-property-type"));
            }
        }
    }
    out
}

/// `@EntityGraph(attributePaths = {...})` literals: each path must walk the entity's properties.
fn entity_graph_problems(model: &Model, domain: &str, a: &java::Annotation, text: &str) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for s in a.strings(&["attributePaths", "value"]) {
        if s.value.len() != s.end - s.at {
            continue;
        }
        let mut ty = domain.to_owned();
        let segments: Vec<&str> = s.value.split('.').collect();
        for (i, segment) in segments.iter().enumerate() {
            let Some(props) = model.props(&ty) else { break };
            let Some((_, p)) = props.iter().find(|(_, p)| p.name == *segment) else {
                let message = format!("`{}` has no attribute `{segment}`.", model.label(&ty));
                out.push(warning(text, s.at, s.end, "entity-graph-unknown-attribute", message));
                break;
            };
            if i + 1 < segments.len() && types::kind_of(&p.ty).is_some() && !p.ty.contains('.') {
                let message = format!("`{segment}` is `{}`, not an association; `{}` cannot follow it.", p.ty, segments[i + 1]);
                out.push(warning(text, s.at, s.end, "entity-graph-not-association", message));
                break;
            }
            ty = target(p).to_owned();
        }
    }
    out
}

/// Top-level keys of a MongoDB `@Query` that name no property of the domain class, nor a `@Field` name.
fn mongo_problems(model: &Model, domain: &str, query: &java::Annotation, text: &str) -> Vec<Diagnostic> {
    let Some(props) = model.props(domain) else {
        return Vec::new();
    };
    let mut classes: Vec<&Class> = Vec::new();
    let mut next = Some(domain);
    while let Some(name) = next.and_then(|n| model.class(n)) {
        classes.push(name.1);
        next = name.1.parent.as_deref();
    }
    let mut out = Vec::new();
    for s in query.strings(&["value"]) {
        if s.value.len() != s.end - s.at {
            continue;
        }
        for (from, to, key) in mongo::top_level_keys(&s.value) {
            let root = key.split('.').next().unwrap_or_default();
            let known = root.starts_with('$')
                || matches!(root, "id" | "_id")
                || props.iter().any(|(_, p)| p.name == root)
                || classes.iter().any(|c| c.aliases.iter().any(|a| a == root));
            if !known {
                let message = format!("`{domain}` has no field `{root}`.");
                out.push(warning(text, s.at + from, s.at + to, "mongo-unknown-field", message));
            }
        }
    }
    out
}

#[derive(Default)]
pub struct DataTools {
    live: live::Live,
    generation: generation::Generation,
}

/// A derived query method name in words, for a tooltip: `findByAgeGreaterThanOrderByName` reads `age greater than, order by name`.
fn describe(name: &str, parts: &[Part]) -> String {
    let words = |s: &str| {
        let mut out = String::new();
        for c in s.chars() {
            if c.is_uppercase() && !out.is_empty() {
                out.push(' ');
            }
            out.extend(c.to_lowercase());
        }
        out
    };
    let predicate = predicate_start(name).unwrap_or(0);
    let order_at = name[predicate..].find("OrderBy").map_or(name.len(), |at| predicate + at);
    let (criteria, ordering): (Vec<&Part>, Vec<&Part>) = parts.iter().partition(|p| p.from < order_at);
    let mut text = String::new();
    for (i, part) in criteria.iter().enumerate() {
        let next = criteria.get(i + 1).map_or(order_at, |n| n.from);
        let gap = &name[part.to..next];
        let (operator, joiner) = match (gap.strip_suffix("And"), gap.strip_suffix("Or")) {
            (Some(op), _) => (op, " and "),
            (None, Some(op)) => (op, " or "),
            _ => (gap, ""),
        };
        text.push_str(&format!("{} {}{joiner}", words(&name[part.from..part.to]), words(operator)));
    }
    let mut text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if !ordering.is_empty() {
        let keys: Vec<String> = ordering
            .iter()
            .enumerate()
            .map(|(i, part)| {
                let next = ordering.get(i + 1).map_or(name.len(), |n| n.from);
                format!("{} {}", words(&name[part.from..part.to]), words(&name[part.to..next]))
            })
            .collect();
        text.push_str(&format!(", order by {}", keys.join(", ").split_whitespace().collect::<Vec<_>>().join(" ")));
    }
    text.trim_start_matches(", ").to_owned()
}

/// The repository whose body holds byte `at`, and its domain type.
/// The `repository` live template at the top level of a file: an interface extending a Spring Data repository.
fn repository_snippets(doc: &Document, offset: usize) -> Vec<CompletionItem> {
    let word = doc.text[..offset].rsplit(|c: char| !c.is_alphanumeric()).next().unwrap_or("");
    if word.len() < 3 || !"repository".starts_with(word) {
        return Vec::new();
    }
    let file = parse_source(&doc.path, &doc.text);
    if file.types.iter().any(|t| t.start < offset && offset <= t.end) {
        return Vec::new();
    }
    let insert_text = if is_kotlin(&doc.path) {
        "interface ${1:Entity}Repository : ${2:JpaRepository}<${1:Entity}, ${3:Long}> {\n    $0\n}"
    } else {
        "public interface ${1:Entity}Repository extends ${2:JpaRepository}<${1:Entity}, ${3:Long}> {\n    $0\n}"
    };
    vec![CompletionItem {
        detail: Some("Spring Data repository".into()),
        insert_text: insert_text.into(),
        snippet: true,
        ..CompletionItem::new("repository", CompletionKind::Snippet)
    }]
}

fn repository_at(file: &JavaFile, at: usize) -> Option<(&TypeDecl, String)> {
    repositories(file).into_iter().rev().map(|(i, d)| (&file.types[i], d)).find(|(t, _)| t.start <= at && at <= t.end)
}

/// Property names of `ty` capitalized, plus one level of nested ones (`AddressCity`).
fn completions(model: &Model, ty: &str) -> Vec<(String, String)> {
    let capitalize = |s: &str| {
        let mut chars = s.chars();
        chars.next().map(|c| c.to_uppercase().chain(chars).collect::<String>()).unwrap_or_default()
    };
    let exact = |spelling: &str, wanted: &[&Prop]| match resolve(model, ty, spelling, 0, spelling.len()) {
        Resolved::Found(steps) => {
            steps.len() == wanted.len() && steps.iter().zip(wanted).all(|(step, wanted)| std::ptr::eq(step.3, *wanted))
        }
        _ => false,
    };
    let mut out = Vec::new();
    for (_, prop) in model.props(ty).unwrap_or_default() {
        if prop.name.is_empty() {
            continue;
        }
        let head = capitalize(&prop.name);
        if exact(&head, &[prop]) {
            out.push((head.clone(), prop.ty.clone()));
        }
        for (_, nested) in model.props(target(prop)).unwrap_or_default() {
            if nested.name.is_empty() {
                continue;
            }
            let tail = capitalize(&nested.name);
            let joined = format!("{head}{tail}");
            if exact(&joined, &[prop, nested]) {
                out.push((joined, nested.ty.clone()));
            } else {
                let explicit = format!("{head}_{tail}");
                if exact(&explicit, &[prop, nested]) {
                    out.push((explicit, nested.ty.clone()));
                }
            }
        }
    }
    out.truncate(512);
    out
}

/// Where the part being typed starts in a predicate, and whether it is in the `OrderBy` clause.
fn tail_start(pred: &str) -> (usize, bool) {
    let (mut start, mut order) = (0, false);
    let word_at = |i: usize, kw: &str| {
        pred.is_char_boundary(i) && pred[i..].starts_with(kw) && pred[i + kw.len()..].chars().next().is_none_or(char::is_uppercase)
    };
    for i in 0..pred.len() {
        if i < start {
            continue;
        }
        if !order && word_at(i, "OrderBy") {
            (start, order) = (i + "OrderBy".len(), true);
            continue;
        }
        let separators: &[&str] = if order { &["Asc", "Desc"] } else { &["And", "Or"] };
        if let Some(kw) = separators.iter().find(|kw| i > start && word_at(i, kw)) {
            start = i + kw.len();
        }
    }
    (start, order)
}

fn declaration_position(text: &str, repository: &TypeDecl, start: usize, off: usize) -> bool {
    if repository
        .members
        .iter()
        .any(|m| m.body.is_some_and(|(a, b)| a <= off && off <= b) || (m.at <= off && off <= m.at + m.name.len() && !derived_method(m)))
    {
        return false;
    }
    let mut depth = 0i32;
    for token in java::tokens(text).iter().filter(|t| repository.start <= t.at && t.at < start) {
        match token.s {
            "{" => depth += 1,
            "}" => depth -= 1,
            _ => {}
        }
    }
    depth == 1
}
impl DataTools {
    /// Entity names after `from`/`join` and properties after `alias.` inside a `@Query` string.
    fn query_completion(&mut self, doc: &Document, off: usize) -> Vec<CompletionItem> {
        let file = self.repository_file(doc);
        let Some((t, domain)) = repository_at(&file, off) else {
            return Vec::new();
        };
        let Some(query) = t.methods().filter_map(|m| m.annotation("Query")).find(|a| a.at <= off && off <= a.end) else {
            return Vec::new();
        };
        if !file.imports.iter().any(|i| i.path == "org.springframework.data.jpa.repository.Query" && !i.is_static) {
            return Vec::new();
        }
        if query.arg("nativeQuery").is_some_and(|a| a.text.trim() == "true") {
            return Vec::new();
        }
        let Some(s) = query.strings(&["value"]).into_iter().find(|s| s.holds(off)) else {
            return Vec::new();
        };
        if s.value.len() != s.end - s.at {
            return Vec::new();
        }
        if s.value.contains(['\r', '\n']) || s.value.contains("#{") || s.value.contains("${") {
            return Vec::new();
        }
        let cursor = off - s.at;
        let typed_start = s.value[..cursor]
            .char_indices()
            .rev()
            .find(|(_, c)| !(c.is_alphanumeric() || matches!(c, '_' | '$' | '.')))
            .map_or(0, |(i, c)| i + c.len_utf8());
        let typed = &s.value[typed_start..cursor];
        let Some(model) = self.model_for(doc, &file, false) else {
            return Vec::new();
        };
        if model.class(&domain).is_none() {
            return Vec::new();
        }
        let item = |label: &str, detail: &str, kind, from: usize| CompletionItem {
            detail: Some(detail.to_owned()),
            replace: Some(range(&doc.text, from, off)),
            ..CompletionItem::new(label, kind)
        };
        let Some((head, _)) = typed.rsplit_once('.') else {
            let after_from = jpql::words(&s.value[..typed_start]).last().is_some_and(|w| w.is("from") || w.is("join") || w.is("update"));
            if !after_from {
                return Vec::new();
            }
            return model
                .entities
                .iter()
                .take(512)
                .map(|(name, c)| item(name, &c.name, CompletionKind::Class, s.at + typed_start))
                .collect();
        };
        let words = jpql::words(&s.value);
        let aliases = alias_types(&model, &jpql::declarations(&words));
        let segments: Vec<&str> = head.split('.').collect();
        let Some(root) = aliases.get(&segments[0].to_ascii_lowercase()) else {
            return Vec::new();
        };
        let Walk::Type(ty) = walk(&model, root, &segments[1..]) else {
            return Vec::new();
        };
        let from = s.at + typed_start + head.len() + 1;
        let mut seen = Vec::new();
        let mut items = Vec::new();
        for (_, p) in model.props(&ty).unwrap_or_default() {
            if !p.name.is_empty() && !seen.contains(&&p.name) {
                seen.push(&p.name);
                items.push(item(&p.name, &p.ty, CompletionKind::Field, from));
            }
        }
        items.truncate(512);
        items
    }
}

impl Providers for DataTools {
    fn line_markers(&mut self, doc: &Document) -> Vec<LineMarker> {
        self.cached_markers(doc)
    }

    fn definition(&mut self, doc: &Document, at: Position) -> Vec<Location> {
        if !live::completion_supported(doc) {
            return Vec::new();
        }
        let Some(off) = offset(&doc.text, at) else {
            return Vec::new();
        };
        let file = self.repository_file(doc);
        let Some((t, domain)) = repository_at(&file, off) else {
            return Vec::new();
        };
        let Some(m) = t.methods().find(|m| m.at <= off && off <= m.at + m.name.len() && derived_method(m)) else {
            return Vec::new();
        };
        let Some(parts) = derived(&m.name) else {
            return Vec::new();
        };
        let Some(model) = self.model_for(doc, &file, false) else {
            return Vec::new();
        };
        let rel = off - m.at;
        for part in parts.iter().filter(|p| p.from <= rel && rel <= p.to) {
            if let Resolved::Found(steps) = resolve(&model, &domain, &m.name, part.from, part.to)
                && let Some((_, _, path, p)) = steps.into_iter().find(|s| s.0 <= rel && rel <= s.1)
            {
                return vec![Location { path: path.into(), range: p.range }];
            }
        }
        Vec::new()
    }

    fn code_actions(&mut self, doc: &Document, selection: Range) -> Vec<CodeAction> {
        if !doc.text.contains("springframework.data.mongodb") {
            return Vec::new();
        }
        let file = self.repository_file(doc);
        let mut actions = Vec::new();
        for (ti, _) in repositories(&file) {
            for query in file.types[ti].methods().filter_map(|m| m.annotation("Query")) {
                for s in query.strings(&["value"]).into_iter().filter(|s| s.value.len() == s.end - s.at) {
                    let covered = range(&doc.text, s.at, s.end);
                    let Some(sorted) = mongo::sorted_entries(&s.value).filter(|_| spring_text::contains(covered, selection.start)) else {
                        continue;
                    };
                    actions.push(CodeAction {
                        title: "Sort the query fields alphabetically".into(),
                        kind: Some("refactor.rewrite".into()),
                        preferred: false,
                        edits: vec![(doc.path.clone(), vec![TextEdit { range: covered, text: sorted }])],
                        data: None,
                    });
                }
            }
        }
        actions
    }

    fn completion(&mut self, doc: &Document, at: Position, _trigger: Option<&str>) -> Vec<CompletionItem> {
        if !live::completion_supported(doc) {
            return Vec::new();
        }
        let Some(off) = offset(&doc.text, at) else {
            return Vec::new();
        };
        let templates = repository_snippets(doc, off);
        if !templates.is_empty() {
            return templates;
        }
        let queried = self.query_completion(doc, off);
        if !queried.is_empty() {
            return queried;
        }
        let start = doc.text[..off]
            .char_indices()
            .rev()
            .find(|(_, c)| !(c.is_alphanumeric() || *c == '_' || *c == '$'))
            .map_or(0, |(i, c)| i + c.len_utf8());
        let word = &doc.text[start..off];
        if word.len() > 256 || !java::tokens(&doc.text).iter().any(|t| t.is_ident() && t.at == start && off <= t.end()) {
            return Vec::new();
        }
        let Some(pred_at) = predicate_start(word) else {
            return Vec::new();
        };
        let file = self.repository_file(doc);
        let Some((repository, domain)) = repository_at(&file, off) else {
            return Vec::new();
        };
        let original = parse_source(&doc.path, &doc.text);
        let original_repository = original.types.iter().find(|t| t.at == repository.at).unwrap_or(repository);
        if !declaration_position(&doc.text, original_repository, start, off) {
            return Vec::new();
        }
        let Some(model) = self.model_for(doc, &file, false) else {
            return Vec::new();
        };
        let pred = &word[pred_at..];
        let (tail, order) = tail_start(pred);
        let head = &word[..pred_at + tail];
        let typed = &pred[tail..];
        let props = completions(&model, &domain);
        let replace = Some(range(&doc.text, start, off));
        let item = |label: String, detail: &str, kind| CompletionItem {
            detail: Some(detail.to_owned()),
            replace,
            ..CompletionItem::new(&label, kind)
        };
        let mut items: Vec<CompletionItem> = props.iter().map(|(p, ty)| item(format!("{head}{p}"), ty, CompletionKind::Field)).collect();
        if let Some((p, _)) = props.iter().filter(|(p, _)| typed.starts_with(p.as_str())).max_by_key(|(p, _)| p.len()) {
            let keywords: &[&str] = if order { &["Asc", "Desc"] } else { &CONTINUATIONS };
            items.extend(keywords.iter().map(|k| item(format!("{head}{p}{k}"), k, CompletionKind::Keyword)));
        }
        items.truncate(512);
        items
    }
}

impl Plugin for DataTools {
    fn activate(&mut self, _ctx: &openidea_plugin::Context) -> Result<(), Error> {
        self.activate_live();
        Ok(())
    }
    fn handle_events(&mut self, events: &[openidea_plugin::Event], _ctx: &Invocation) {
        self.live_events(events);
    }
    fn run_command(&mut self, id: &str, ctx: &Invocation, _arg: Option<&str>) -> Result<Option<String>, Error> {
        if id != "show-repository-members" {
            return Err(Error::NotFound(format!("command {id}")));
        }
        let editor = ctx.editor().ok_or("Open a repository interface first.")?;
        let (doc, selections) = match (editor.document(), editor.selections()) {
            (Ok(doc), Ok(selections)) => (doc, selections),
            (Err(Error::Denied(_)), _) | (_, Err(Error::Denied(_))) => {
                return Err("Showing inherited members needs the editor.read permission. Allow it in Settings › Plugins › Spring Data Tools.".into());
            }
            (Err(error), _) | (_, Err(error)) => return Err(error),
        };
        let file = parse_source(&editor.path(), &doc.text);
        let caret = selections.first().and_then(|r| offset(&doc.text, r.start)).unwrap_or(0);
        let (repository, _) = repository_at(&file, caret).ok_or("Put the caret in a Spring Data repository interface.")?;
        let parents: Vec<&str> = repository.extends.iter().map(|t| t.name.as_str()).collect();
        let found = members::inherited(&parents);
        if found.is_empty() {
            host::notify(
                NotificationLevel::Info,
                "Spring Data",
                &format!("{} extends no repository interface known to this plugin.", repository.name),
            );
            return Ok(None);
        }
        // The members live in library jars: the list is for reading, choosing one opens nothing.
        let items: Vec<PickItem> = found
            .iter()
            .enumerate()
            .map(|(i, (owner, member))| PickItem::new(&i.to_string(), member).description(&format!("from {owner}")))
            .collect();
        host::pick(&format!("{} inherits {} members", repository.name, items.len()), &items)?;
        Ok(None)
    }
}

register_plugin!(DataTools, injections, providers, file_actions, diagnostics, hover, inlay_hints);

pub(crate) const DOMAIN_MARKER: &str = "icons/marker-domain.svg";
pub(crate) const IMPLEMENTATION_MARKER: &str = "icons/marker-implementation.svg";
pub(crate) const QUERY_MARKER: &str = "icons/marker-query.svg";

#[cfg(test)]
mod tests {
    #[test]
    fn marker_icons_are_declared_in_the_manifest() {
        let manifest = include_str!("../plugin.toml");
        for icon in [crate::DOMAIN_MARKER, crate::IMPLEMENTATION_MARKER, crate::QUERY_MARKER] {
            assert!(manifest.contains(&format!("icon = \"{icon}\"")));
            assert!(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(icon).is_file());
        }
    }

    use super::*;
    use openidea_plugin::diagnostics::DiagnosticsProvider;
    use openidea_plugin::mock::MockHost;

    const BASE: &str = r#"package com.acme;

@MappedSuperclass
public abstract class BaseEntity {
    @Id private Long id;
    private Instant createdAt;
}
"#;
    const USER: &str = r#"package com.acme;

import java.util.List;

@Entity
public class User extends BaseEntity {
    private String firstName, lastName;
    private int age;
    private boolean active;
    @Embedded private Address address;
    @ManyToOne private Owner owner;
    @OneToMany private List<Order> orders;
    @Transient private String display;
    private static final long serialVersionUID = 1L;

    public String getFullName() { return firstName + " " + lastName; }
    @Transient public String getDisplay() { return display; }
}

@Embeddable
class Address {
    private String city;
    private String zipCode;
}

record Order(String number, BigDecimal total) {}
"#;
    const REPO: &str = r#"package com.acme;
import org.springframework.data.jpa.repository.JpaRepository; import org.springframework.data.jpa.repository.Query; import org.springframework.data.repository.query.Param; import org.springframework.data.jpa.repository.Modifying; import org.springframework.data.repository.RepositoryDefinition; import org.springframework.data.domain.Pageable;
public interface UserRepository extends JpaRepository<User, Long>, UserRepositoryCustom {
    List<User> findByFirstNameAndLastNameAllIgnoreCase(String first, String last);
    List<User> findDistinctTop3ByAddressZipCodeOrderByAgeDesc(String zip);
    Page<User> findByAgeBetween(int from, int to, Pageable page);
    long countByActiveTrue();
    List<User> findByOrdersNumber(String number);
    List<User> findByAddress_City(String city);
    List<User> findByCreatedAtAfter(Instant at);
    Optional<User> findByEmial(String email);
    List<User> findByAgeGreaterThan();
    List<User> findByAddressStreet(String street);
    List<User> findByDisplay(String d);
    List<User> findByFullNameAndLastName(String full, String last);

    @Query("select u from User u where u.age > :min and u.firstName = :name and u.id = :#{#user.x} and cast(u.age as text)::text = ':nope'")
    List<User> adults(@Param("min") int min, String name);

    @Query("select u from User u where u.lastName = :last")
    List<User> byLast(String name);

    @Modifying
    @Query("update User u set u.active = false")
    void deactivate();

    @Modifying
    void touch();

    default List<User> helper() { return findByAgeGreaterThan(); }
}

@RepositoryDefinition(domainClass = Unknown.class, idClass = Long.class)
interface OtherRepository {
    List<Unknown> findByWhatever(String w);
}
"#;
    const CUSTOMER: &str = "package com.acme;\n@Entity\nclass Customer {\n    private String name;\n}\n";
    const IMPL: &str = "package com.acme;\nclass UserRepositoryImpl implements UserRepositoryCustom {}\n";

    fn setup() -> MockHost {
        let host = MockHost::install();
        host.add_file("src/main/java/com/acme/BaseEntity.java", BASE);
        host.add_file("src/main/java/com/acme/User.java", USER);
        host.add_file("src/main/java/com/acme/UserRepositoryImpl.java", IMPL);
        host.add_file("src/main/java/com/acme/Customer.java", CUSTOMER);
        host
    }

    fn doc(text: &str) -> Document {
        Document {
            path: "/project/src/main/java/com/acme/UserRepository.java".into(),
            language: "java".into(),
            version: 1,
            text: text.into(),
        }
    }

    const SPRING_IMPORTS: [&str; 5] = [
        "org.springframework.data.jpa.repository.JpaRepository",
        "org.springframework.data.jpa.repository.Query",
        "org.springframework.data.repository.query.Param",
        "org.springframework.data.jpa.repository.Modifying",
        "org.springframework.data.jpa.repository.EntityGraph",
    ];

    fn spring(text: &str) -> String {
        if text.contains("mongodb") || text.contains("import org.springframework.data.jpa.repository.JpaRepository") {
            return text.into();
        }
        let semicolon = if text.starts_with("package com.acme;") { ";" } else { "" };
        let imports: String =
            SPRING_IMPORTS.iter().filter(|i| !text.contains(&format!("import {i}"))).map(|i| format!("\nimport {i}{semicolon}")).collect();
        text.replacen(&format!("package com.acme{semicolon}"), &format!("package com.acme{semicolon}{imports}"), 1)
    }

    fn ready() -> (MockHost, DataTools) {
        let host = setup();
        let mut tools = DataTools::default();
        tools.activate_live();
        (host, tools)
    }

    fn diagnose(tools: &mut DataTools, text: &str) -> Vec<Diagnostic> {
        tools.diagnostics(&doc(&spring(text)))
    }

    fn parts(name: &str) -> Vec<(&str, Option<usize>)> {
        derived(name).unwrap().into_iter().map(|p| (&name[p.from..p.to], p.args)).collect()
    }

    #[test]
    fn parses_derived_query_names() {
        assert_eq!(parts("findByFirstNameAndLastNameAllIgnoreCase"), [("FirstName", Some(1)), ("LastName", Some(1))]);
        assert_eq!(
            parts("findDistinctTop3ByAddressZipCodeOrderByAgeDescNameAsc"),
            [("AddressZipCode", Some(1)), ("Age", Some(0)), ("Name", Some(0))]
        );
        assert_eq!(parts("countByActiveTrueOrAgeIsNotNull"), [("Active", Some(0)), ("Age", Some(0))]);
        assert_eq!(parts("findByAgeBetweenAndNameContainingIgnoreCase"), [("Age", Some(2)), ("Name", Some(1))]);
        assert_eq!(parts("findByOrderDateAndOrigin"), [("OrderDate", Some(1)), ("Origin", Some(1))]);
        assert_eq!(parts("findTopByOrderByAgeDesc"), [("Age", Some(0))]);
        assert_eq!(parts("findByLocationNear"), [("Location", None)]);
        assert_eq!(derived("findAll"), None);
        assert_eq!(derived("getter"), None);
        assert_eq!(derived("findById"), None);
        assert_eq!(derived("save"), None);
        assert_eq!(named_parameters("a = :x and b = :#{#y} and ':z' and c::text and :1 and :y_2"), [(5, "x"), (55, "y_2")]);
    }

    #[test]
    fn checks_derived_queries_and_query_annotations() {
        let (_host, mut tools) = ready();
        let problems = &diagnose(&mut tools, REPO);
        let found: Vec<_> = problems
            .iter()
            .map(|d| (d.code.as_deref().unwrap(), &REPO[offset(REPO, d.range.start).unwrap()..offset(REPO, d.range.end).unwrap()]))
            .collect();
        assert_eq!(
            found,
            [
                ("unknown-property", "Emial"),
                ("query-argument-count", "findByAgeGreaterThan"),
                ("unknown-property", "Street"),
                ("unknown-property", "Display"),
                ("unbound-query-parameter", ":last"),
                ("modifying-without-query", "@Modifying"),
            ]
        );
        assert_eq!(problems[0].message, "`User` has no property `emial`.");
        assert_eq!(problems[2].message, "`Address` has no property `street`.");
    }

    #[test]
    fn links_repository_to_entity_and_properties() {
        let (_host, mut tools) = ready();
        let markers = tools.line_markers(&doc(REPO));
        let targets: Vec<_> = markers
            .iter()
            .filter(|m| m.target.is_some())
            .map(|m| (m.line, m.tooltip.as_str(), m.target.as_ref().unwrap().path.rsplit('/').next().unwrap()))
            .collect();
        assert_eq!(
            targets,
            [(2, "Domain type: User", "User.java"), (2, "Custom implementation: UserRepositoryImpl", "UserRepositoryImpl.java")]
        );

        let line = REPO.lines().position(|l| l.contains("findDistinctTop3")).unwrap() as u32;
        let col = REPO.lines().nth(line as usize).unwrap().find("ZipCode").unwrap() as u32 + 2;
        let found = tools.definition(&doc(REPO), Position::new(line, col));
        assert_eq!(found.len(), 1);
        assert!(found[0].path.ends_with("User.java"));
        assert_eq!(&USER.lines().nth(found[0].range.start.line as usize).unwrap()[found[0].range.start.character as usize..], "zipCode;");

        let line = REPO.lines().position(|l| l.contains("findByCreatedAtAfter")).unwrap() as u32;
        let found = tools.definition(&doc(REPO), Position::new(line, 23));
        assert!(found[0].path.ends_with("BaseEntity.java"), "inherited from the mapped superclass");
    }

    #[test]
    fn completes_derived_query_names() {
        let _host = setup();
        let mut tools = DataTools::default();
        tools.activate_live();
        let typing = REPO.replace("    long countByActiveTrue();", "    List<User> findByFirstNameAndAdd");
        let line = typing.lines().position(|l| l.contains("findByFirstNameAndAdd")).unwrap() as u32;
        let items = tools.completion(&doc(&typing), Position::new(line, 36), None);
        let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"findByFirstNameAndAddress"));
        assert!(labels.contains(&"findByFirstNameAndAddressZipCode"));
        assert!(labels.contains(&"findByFirstNameAndId"));
        assert!(!labels.iter().any(|l| l.ends_with("Display")));
        assert_eq!(items[0].replace.unwrap().start, Position::new(line, 15));

        let typing = REPO.replace("    long countByActiveTrue();", "    long countByAge");
        let items = tools.completion(&doc(&typing), Position::new(line, 19), None);
        assert!(items.iter().any(|i| i.label == "countByAgeGreaterThan"));
        let typing = REPO.replace("    long countByActiveTrue();", "    long countByAgeOrderByAge");
        let items = tools.completion(&doc(&typing), Position::new(line, 29), None);
        assert!(items.iter().any(|i| i.label == "countByAgeOrderByAgeDesc"));
        assert!(tools.completion(&doc(REPO), Position::new(0, 3), None).is_empty());
    }

    const QUERY_REPO: &str = r#"package com.acme;

public interface UserRepository extends JpaRepository<User, Long> {
    @Query("select u from Custmer u where u.age > 1")
    List<User> unknownEntity();

    @Query("select u from User u join u.address a where u.address.citty = 'x' and a.zipCode = '1' and u.nme = 2")
    List<User> unknownProperties();

    @Query("select u from User u join u.orders o where o.number = '1' and o.total > 1 and u.id = 1 and u.createdAt is null")
    List<User> fine();

    @Query("select u from User u join u.owner o where o.whatever = 1 and u.owner.name.first = 1")
    List<User> libraryTypes();
}
"#;

    fn query_diagnostics(text: &str) -> Vec<(String, String)> {
        let (_host, mut tools) = ready();
        let text = &spring(text);
        let problems = tools.diagnostics(&doc(text));
        problems
            .iter()
            .map(|d| (d.code.clone().unwrap(), text[offset(text, d.range.start).unwrap()..offset(text, d.range.end).unwrap()].to_owned()))
            .collect()
    }

    #[test]
    fn query_unknown_entity_warns() {
        let found = query_diagnostics(QUERY_REPO);
        assert_eq!(found[0], ("unknown-query-entity".to_owned(), "Custmer".to_owned()));
    }

    #[test]
    fn query_unknown_property_warns() {
        let found = query_diagnostics(QUERY_REPO);
        let codes: Vec<_> = found.iter().map(|(c, t)| (c.as_str(), t.as_str())).collect();
        assert_eq!(codes, [("unknown-query-entity", "Custmer"), ("unknown-query-property", "citty"), ("unknown-query-property", "nme"),]);
    }

    #[test]
    fn query_property_of_library_entity_is_quiet() {
        let text = "package com.acme;\ninterface UserRepository extends JpaRepository<User, Long> {\n    @Query(\"select u from User u join u.owner o where o.whatever = 1 and u.owner.name.first = 1\")\n    List<User> q();\n}\n";
        let found = query_diagnostics(text);
        assert!(found.is_empty(), "{found:?}");
        let library = "package com.acme;\ninterface AccountRepository extends JpaRepository<Account, Long> {\n    @Query(\"select a from Acct a where a.nope = 1\")\n    List<Account> q();\n}\n";
        assert!(query_diagnostics(library).is_empty());
    }

    #[test]
    fn short_names_are_not_near_misses() {
        assert!(!is_near_miss("Rule", "Role") && !is_near_miss("A", "B") && !is_near_miss("Usr", "User"));
        assert!(is_near_miss("Customer", "Custmer") && is_near_miss("User", "user"));
        assert!(!is_near_miss("Customer", "Cstmr") && is_near_miss("CustomerOrder", "CustmerOrdr"));
    }

    #[test]
    fn incomplete_index_stays_quiet() {
        let (host, mut tools) = ready();
        let typo = "select c from Custmer c where c.nme = 1";
        let text = format!("package com.acme;\ninterface CustomerRepository extends JpaRepository<Customer, Long> {{\n    @Query(\"{typo}\")\n    List<Customer> q();\n}}\n");
        assert_eq!(diagnose(&mut tools, &text).len(), 1);
        host.add_file("src/main/java/com/acme/Broken.java", "class Broken {");
        tools.live_events(&[openidea_plugin::Event::FileSaved("/project/src/main/java/com/acme/Broken.java".into())]);
        assert!(diagnose(&mut tools, &text).is_empty());
    }

    #[test]
    fn subquery_alias_same_type_still_checked() {
        let text = "package com.acme;\ninterface UserRepository extends JpaRepository<User, Long> {\n    @Query(\"select u from User u where u.age > 1 and exists (select 1 from User u where u.nme = 2)\")\n    List<User> q();\n}\n";
        let found: Vec<_> = query_diagnostics(text).into_iter().map(|(_, t)| t).collect();
        assert_eq!(found, ["nme"]);
    }

    #[test]
    fn kotlin_raw_string_query_is_checked() {
        let host = setup();
        host.add_file(
            "src/main/kotlin/com/acme/Person.kt",
            "package com.acme\n\n@Entity\nclass Person(@Id val id: Long, var name: String)\n",
        );
        let repo = "package com.acme\nimport org.springframework.data.jpa.repository.JpaRepository\nimport org.springframework.data.jpa.repository.Query\n\ninterface PersonRepository : JpaRepository<Person, Long> {\n    @Query(\"\"\"\n        select p from Person p\n        where p.nmae = :name\n    \"\"\")\n    fun byName(name: String): List<Person>\n}\n";
        let kt = Document {
            path: "/project/src/main/kotlin/com/acme/PersonRepository.kt".into(),
            language: "kotlin".into(),
            version: 1,
            text: repo.into(),
        };
        let mut tools = DataTools::default();
        tools.activate_live();
        let problems = tools.diagnostics(&kt);
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].message, "`Person` has no property `nmae`.");
    }

    const QUIET_REPO: &str = r#"package com.acme;

public interface UserRepository extends JpaRepository<User, Long> {
    @Query(value = "select * from Custmer u where u.nope = 1", nativeQuery = true)
    List<User> nativeOne();

    @NativeQuery("select * from Custmer u where u.nope = 1")
    List<User> nativeTwo();

    @Query("select u from User u where u.age = :#{u.nope}")
    List<User> spel(String a);

    @Query("select u from Custmer u " + "where u.age = 1")
    List<User> concatenated();

    @Query("select u from User u join AuditEvent e on e.user = u where e.kind = 1")
    List<User> unrelatedEntity();

    @Query("select u from User u where u.age > 1 and exists (select 1 from Order u where u.number = '1')")
    List<User> aliasReusedInSubquery();
}
"#;

    #[test]
    fn query_native_spel_and_concatenation_are_quiet() {
        let found = query_diagnostics(QUIET_REPO);
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn query_unrelated_entity_name_is_quiet() {
        let text = "package com.acme;\ninterface UserRepository extends JpaRepository<User, Long> {\n    @Query(\"select u from User u, AuditEvent e where e.kind = 1\")\n    List<User> q();\n}\n";
        assert!(query_diagnostics(text).is_empty());
    }

    #[test]
    fn function_from_is_not_an_entity() {
        let words = jpql::words(
            "select extract(year from current_date), trim(both from n), substring(x from 2) from User u where exists (select 1 from Order o)",
        );
        let targets: Vec<_> = jpql::declarations(&words).into_iter().map(|d| d.target.s).collect();
        assert_eq!(targets, ["User", "Order"]);
    }

    #[test]
    fn query_joins_resolve_fields_of_other_entities() {
        let text = "package com.acme;\ninterface UserRepository extends JpaRepository<User, Long> {\n    @Query(\"select u from User u join Order o on o.number = u.firstName where o.nmbr = 1\")\n    List<User> a();\n    @Query(\"select u from User u join fetch u.orders o where o.totl = 1 and o.total = 2\")\n    List<User> b();\n}\n";
        let found: Vec<_> = query_diagnostics(text).into_iter().map(|(_, t)| t).collect();
        assert_eq!(found, ["nmbr", "totl"]);
    }

    #[test]
    fn kotlin_query_is_checked() {
        let host = setup();
        host.add_file(
            "src/main/kotlin/com/acme/Person.kt",
            "package com.acme\n\n@Entity\nclass Person(@Id val id: Long, var name: String)\n",
        );
        let repo = "package com.acme\nimport org.springframework.data.jpa.repository.JpaRepository\nimport org.springframework.data.jpa.repository.Query\n\ninterface PersonRepository : JpaRepository<Person, Long> {\n    @Query(\"select p from Person p where p.nmae = :name\")\n    fun byName(name: String): List<Person>\n}\n";
        let kt = Document {
            path: "/project/src/main/kotlin/com/acme/PersonRepository.kt".into(),
            language: "kotlin".into(),
            version: 1,
            text: repo.into(),
        };
        let mut tools = DataTools::default();
        tools.activate_live();
        let problems = tools.diagnostics(&kt);
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].message, "`Person` has no property `nmae`.");
    }

    #[test]
    fn fixed_file_has_no_diagnostics() {
        let (_host, mut tools) = ready();
        let path = "/project/src/main/java/com/acme/Person.java";
        let bad = "import org.springframework.data.relational.core.mapping.Sequence;\nclass Person {\n    @Sequence(sequence = \"\")\n    Long id;\n}\n";
        let doc = |text: &str| Document { path: path.into(), language: "java".into(), version: 1, text: text.into() };
        assert_eq!(tools.diagnostics(&doc(bad)).len(), 1);
        assert!(tools.diagnostics(&doc(&bad.replace("\"\"", "\"person_seq\""))).is_empty());
    }

    #[test]
    fn repository_findings_come_with_problems() {
        let (_host, mut tools) = ready();
        let text = "package com.acme;\n\nimport org.springframework.data.domain.Page;\n\ninterface UserRepository extends JpaRepository<User, Long> {\n    Page<User> findByFirstName(String firstName);\n}\n";
        let codes: Vec<_> = diagnose(&mut tools, text).iter().map(|d| d.code.clone()).collect();
        assert_eq!(codes, [Some("page-without-pageable".to_owned())]);
    }

    fn complete_in(text: &str, marker: &str) -> Vec<CompletionItem> {
        let _host = setup();
        let mut tools = DataTools::default();
        tools.activate_live();
        let cleaned = text.replace(marker, "");
        let off = text.find(marker).unwrap();
        let before = &text[..off];
        let line = before.matches('\n').count() as u32;
        let col = (off - before.rfind('\n').map_or(0, |i| i + 1)) as u32;
        tools.completion(&doc(&cleaned), Position::new(line, col), None)
    }

    #[test]
    fn query_alias_property_completes() {
        let text = "package com.acme; import org.springframework.data.jpa.repository.JpaRepository; import org.springframework.data.jpa.repository.Query;\ninterface UserRepository extends JpaRepository<User, Long> {\n    @Query(\"select u from User u where u.address.CARET\")\n    List<User> q();\n}\n";
        let labels: Vec<_> = complete_in(text, "CARET").into_iter().map(|i| i.label).collect();
        assert_eq!(labels, ["city", "zipCode"]);
        let text = text.replace("u.address.CARET", "u.CARET");
        let labels: Vec<_> = complete_in(&text, "CARET").into_iter().map(|i| i.label).collect();
        assert!(
            labels.contains(&"firstName".to_owned()) && labels.contains(&"createdAt".to_owned()) && labels.contains(&"fullName".to_owned())
        );
        assert!(!labels.contains(&"display".to_owned()));
    }

    #[test]
    fn query_entity_names_complete_after_from_and_join() {
        let text = "package com.acme; import org.springframework.data.jpa.repository.JpaRepository; import org.springframework.data.jpa.repository.Query;\ninterface UserRepository extends JpaRepository<User, Long> {\n    @Query(\"select u from CARET\")\n    List<User> q();\n}\n";
        let labels: Vec<_> = complete_in(text, "CARET").into_iter().map(|i| i.label).collect();
        assert_eq!(labels, ["Customer", "User"]);
    }

    #[test]
    fn kotlin_repository_derives_entity() {
        let host = setup();
        host.add_file(
            "src/main/kotlin/com/acme/Person.kt",
            "package com.acme\n\n@Entity\nclass Person(@Id val id: Long, var name: String, var age: Int)\n",
        );
        let repo = "package com.acme\nimport org.springframework.data.jpa.repository.JpaRepository\n\ninterface PersonRepository : JpaRepository<Person, Long> {\n    fun findByName(name: String): List<Person>\n    fun findByNmae(name: String): List<Person>\n}\n";
        let kt = Document {
            path: "/project/src/main/kotlin/com/acme/PersonRepository.kt".into(),
            language: "kotlin".into(),
            version: 1,
            text: repo.into(),
        };
        let mut tools = DataTools::default();
        tools.activate_live();
        let markers = tools.line_markers(&kt);
        assert_eq!(markers[0].tooltip, "Domain type: Person");
        let problems = tools.diagnostics(&kt);
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].message, "`Person` has no property `nmae`.");
    }


    fn data_findings(text: &str) -> Vec<(String, String)> {
        let host = setup();
        host.add_file("src/main/java/com/acme/NameOnly.java", "package com.acme;\ninterface NameOnly {\n    String getFirstName();\n    String getFullName();\n    String getNmae();\n    default String both() { return getFirstName(); }\n}\n");
        host.add_file("src/main/java/com/acme/OpenView.java", "package com.acme;\ninterface OpenView {\n    @Value(\"#{target.x}\")\n    String getAnything();\n}\n");
        host.add_file("src/main/java/com/acme/UserDto.java", "package com.acme;\nrecord UserDto(String firstName, String age, String bogus) {}\n");
        let mut tools = DataTools::default();
        tools.activate_live();
        let text = &spring(text);
        let problems = tools.diagnostics(&doc(text));
        problems
            .iter()
            .map(|d| (d.code.clone().unwrap(), text[offset(text, d.range.start).unwrap()..offset(text, d.range.end).unwrap()].to_owned()))
            .collect()
    }

    fn repo_with(body: &str) -> String {
        format!("package com.acme;\nimport org.springframework.data.jpa.repository.EntityGraph;\nimport org.springframework.data.domain.Pageable;\ninterface UserRepository extends JpaRepository<User, Long> {{\n{body}\n}}\n")
    }

    #[test]
    fn nested_paths_top_distinct_and_order_by_resolve_against_literal_properties() {
        let ok = repo_with("List<User> findByAddressCityAndAgeGreaterThan(String c, int a);\nList<User> findDistinctTop3ByAddress_ZipCodeOrderByAgeDesc(String z);\nList<User> findTop3ByAddressCityOrderByAddressZipCodeAsc(String c);");
        assert_eq!(data_findings(&ok), []);
        let bad = repo_with("List<User> findByAddressTownAndAgeGreaterThan(String c, int a);\nList<User> findFirst3ByAddressCityOrderByAddressZip(String c);");
        let codes: Vec<_> = data_findings(&bad).into_iter().map(|(c, t)| (c, t)).collect();
        assert_eq!(codes, [("unknown-property".to_owned(), "Town".to_owned()), ("unknown-property".to_owned(), "Zip".to_owned())]);
    }

    #[test]
    fn closed_interface_projection_getters_must_be_entity_properties() {
        let found = data_findings(&repo_with("List<NameOnly> findByAge(int a);\nOptional<OpenView> findFirstByAge(int a);\nList<NameOnly> countFoo();"));
        assert_eq!(found, [("projection-unknown-property".to_owned(), "NameOnly".to_owned())]);
    }

    #[test]
    fn record_projection_components_are_checked_by_name_and_clear_type_conflict() {
        let found = data_findings(&repo_with("Page<UserDto> findByAge(int a, Pageable p);"));
        let codes: Vec<_> = found.iter().map(|(c, _)| c.as_str()).collect();
        assert_eq!(codes, ["projection-property-type", "projection-unknown-property"]);
    }

    #[test]
    fn entity_graph_paths_walk_entity_properties() {
        let ok = repo_with("@EntityGraph(attributePaths = {\"orders\", \"address.city\", \"owner.anything\"})\nList<User> findByAge(int a);");
        assert_eq!(data_findings(&ok), []);
        let bad = repo_with("@EntityGraph(attributePaths = {\"ordres\", \"age.x\", \"address.cty\"})\nList<User> findByAge(int a);");
        let codes: Vec<_> = data_findings(&bad).into_iter().map(|(c, _)| c).collect();
        assert_eq!(codes, ["entity-graph-unknown-attribute", "entity-graph-not-association", "entity-graph-unknown-attribute"]);
    }

    #[test]
    fn find_order_by_name_subject_tooltip() {
        let words = |name: &str| describe(name, &derived(name).unwrap());
        assert_eq!(words("findOrderByName"), "name");
        assert_eq!(words("findOrderByNameOrderByAgeDesc"), "name, order by age desc");
        assert_eq!(words("findByAgeOrderByName"), "age, order by name");
    }

    #[test]
    fn derived_query_methods_get_a_tooltip_in_words() {
        let (_host, mut tools) = ready();
        let markers = tools.line_markers(&doc(REPO));
        let tips: Vec<&str> = markers.iter().filter(|m| m.target.is_none()).map(|m| m.tooltip.as_str()).collect();
        assert!(tips.contains(&"Derived query: find User where first name and last name all ignore case"), "{tips:?}");
        assert!(tips.contains(&"Derived query: findDistinctTop3 User where address zip code, order by age desc"), "{tips:?}");
        assert!(tips.contains(&"Derived query: find User where age between"), "{tips:?}");
    }

    const MONGO_PERSON: &str = "package com.acme;\n@Document\nclass Person {\n    @Id String id;\n    String lastname;\n    @Field(\"first_name\") String firstname;\n    Address address;\n}\n";

    #[test]
    fn sort_action_reorders_mongo_query_fields() {
        let repo = "import org.springframework.data.mongodb.repository.Query; import org.springframework.data.mongodb.repository.MongoRepository;\ninterface PersonRepository extends MongoRepository<Person, String> {\n    @Query(\"{ 'lastname': ?0, age: ?1 }\")\n    List<Person> find(String a, int b);\n}\n";
        let _host = setup();
        let caret = repo.find("lastname").unwrap();
        let at = openidea_plugin::text::position(repo, caret);
        let actions = DataTools::default().code_actions(&doc(repo), Range::new(at, at));
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].edits[0].1[0].text, "{ age: ?1, 'lastname': ?0 }");
        let outside = DataTools::default().code_actions(&doc(repo), Range::new(Position::new(0, 0), Position::new(0, 0)));
        assert!(outside.is_empty());
    }

    #[test]
    fn repository_snippet_completes_at_top_level_only() {
        let _host = setup();
        let top = "package com.acme;\n\nrepo";
        let items = DataTools::default().completion(&doc(top), openidea_plugin::text::position(top, top.len()), None);
        assert!(items.iter().any(|i| i.label == "repository" && i.snippet));
        let inside = "interface A extends JpaRepository<User, Long> {\n    repo";
        let items = DataTools::default().completion(&doc(inside), openidea_plugin::text::position(inside, inside.len()), None);
        assert!(items.iter().all(|i| i.label != "repository"));
    }

    #[test]
    fn mongo_query_fields_must_exist_on_the_document() {
        let repo = "package com.acme;\nimport org.springframework.data.mongodb.repository.Query;\nimport org.springframework.data.mongodb.repository.MongoRepository;\ninterface PersonRepository extends MongoRepository<Person, String> {\n    @Query(\"{ 'lastname': ?0, 'address.city': ?1, first_name: ?2, $or: [ { nope: 1 } ], 'lastnme': ?3 }\")\n    List<Person> find(String a, String b, String c, String d);\n}\n";
        let host = setup();
        host.add_file("src/main/java/com/acme/Person.java", MONGO_PERSON);
        let mut tools = DataTools::default();
        tools.activate_live();
        let found = tools.diagnostics(&doc(repo));
        let mongo: Vec<_> = found.iter().filter(|d| d.code.as_deref() == Some("mongo-unknown-field")).collect();
        assert_eq!(mongo.len(), 1, "{found:?}");
        assert!(mongo[0].message.contains("lastnme"));

        let library = repo.replace("Person, String", "Library, String");
        assert!(tools.diagnostics(&doc(&library)).iter().all(|c| c.code.as_deref() != Some("mongo-unknown-field")));
    }

    #[test]
    fn show_repository_members_denied_names_permission() {
        let host = MockHost::install().with_capabilities(&["project.read"]);
        host.open_editor("/project/src/main/java/com/acme/UserRepository.java", "java", REPO, &[(2, 20, 2, 20)]);
        let error = DataTools::default().run_command("show-repository-members", &host.invocation(), None).unwrap_err().to_string();
        assert!(error.contains("needs the editor.read permission"), "{error}");
        assert!(error.contains("Settings › Plugins › Spring Data Tools"), "{error}");
    }

    #[test]
    fn members_of_known_parent_interfaces_are_listed_by_the_command() {
        let host = setup();
        host.open_editor("/project/src/main/java/com/acme/UserRepository.java", "java", REPO, &[(2, 20, 2, 20)]);
        assert_eq!(DataTools::default().run_command("show-repository-members", &host.invocation(), None).unwrap(), None);
        assert!(DataTools::default().run_command("nope", &host.invocation(), None).is_err());
    }

    #[test]
    fn inherited_members_are_listed_in_a_picker_by_interface() {
        let host = setup();
        host.open_editor("/project/src/main/java/com/acme/UserRepository.java", "java", REPO, &[(2, 20, 2, 20)]);
        host.answer_next_prompt(Some(&["0"]));
        assert_eq!(DataTools::default().run_command("show-repository-members", &host.invocation(), None).unwrap(), None);
        assert!(host.notifications().is_empty(), "no notification dump");
        assert!(host.opened_locations().is_empty(), "library members open nothing");
    }
}
