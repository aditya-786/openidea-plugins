//! Bounded, source-only PHP type inference for Twig variables: what a controller passes to
//! `render()` and the members Twig can read from it. Matching is textual (a call is
//! recognised by its shape, not its resolved class); anything uncertain yields no type.
use std::collections::BTreeMap;

use crate::php_text::{self, ClassModel, Visibility, BUILT_IN_TYPES};

const FINDERS: [&str; 3] = ["find", "findOneBy", "findOneById"];
const LIST_FINDERS: [&str; 2] = ["findAll", "findBy"];
const MAX_PARENTS: usize = 8;

/// What a value is, before project sources are read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TypeHint {
    Class(String),
    /// The entity a repository class manages.
    EntityOf(String),
    /// A list of what the inner hint stands for.
    ElementsOf(Box<TypeHint>),
    /// A service by id, resolved through the service index.
    Service(String),
    /// A query builder over the entity the inner hint stands for.
    Builder(Box<TypeHint>),
    /// A form (or its view) of a form type class.
    Form(String),
}

/// A value Twig can read members from, or iterate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Object(String),
    List(String),
    /// The view of a form built from this form type class.
    Form(String),
    /// The view of one form field.
    Field,
    /// `form.vars`.
    Vars,
}

/// A member Twig can read with `object.name`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    pub name: String,
    pub detail: String,
    /// A class (resolved through `use`) or lower-case built-in.
    pub type_name: Option<String>,
    /// The class of the items, when the docblock types a collection or list.
    pub element_type: Option<String>,
    /// The class that declares it and where, as offsets in that class's source.
    pub owner: String,
    pub range: std::ops::Range<usize>,
}

/// The source of a class by its fully qualified name.
pub type Loader<'a> = &'a dyn Fn(&str) -> Option<String>;

struct Scope<'a> {
    known: BTreeMap<String, TypeHint>,
    properties: BTreeMap<String, String>,
    resolve: &'a dyn Fn(&str) -> String,
}

fn is_class_type(name: &str) -> bool {
    !BUILT_IN_TYPES.contains(&name.to_ascii_lowercase().as_str())
}

fn identifier_end(text: &str) -> usize {
    text.bytes().position(|byte| !(byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'\\'))).unwrap_or(text.len())
}

fn scope_at<'a>(model: &ClassModel, text: &str, masked: &str, resolve: &'a dyn Fn(&str) -> String, at: usize) -> Scope<'a> {
    let mut scope = Scope {
        known: BTreeMap::new(),
        properties: model.properties.iter().filter(|property| !property.is_static).filter_map(|property| Some((property.name.clone(), property.type_name.clone().filter(|name| is_class_type(name))?))).collect(),
        resolve,
    };

    if let Some(method) = model.methods.iter().find(|method| method.body.as_ref().is_some_and(|body| body.contains(&at))) {
        for param in &method.params {
            if let Some(name) = param.type_name.clone().filter(|name| is_class_type(name)) {
                scope.known.insert(param.name.clone(), TypeHint::Class(name));
            }
        }

        let body = method.body.clone().unwrap_or(at..at);
        let mut offset = body.start;
        // Statements split on the masked text (strings hidden), read from the real text.
        for statement in masked[body.start..at.max(body.start)].split(';') {
            let original = &text[offset..offset + statement.len()];
            offset += statement.len() + 1;
            let Some((name, rhs)) = assignment(original) else { continue };
            match expression_hint(rhs, &scope) {
                Some(hint) => scope.known.insert(name.to_owned(), hint),
                None => scope.known.remove(name),
            };
        }
    }
    scope
}

/// Type hints of the values of a `render()` call's literal array, from the method holding the call at `at`.
pub fn render_variable_types(text: &str, at: usize, entries: &[(String, String)]) -> BTreeMap<String, TypeHint> {
    let mut types = BTreeMap::new();
    let Some(model) = php_text::class_model(text) else { return types };
    let masked = php_text::mask(text);
    let resolve = php_text::resolver(text);
    let scope = scope_at(&model, text, &masked, &resolve, at);

    for (key, expression) in entries {
        if let Some(hint) = expression_hint(expression, &scope) {
            types.insert(key.clone(), hint);
        }
    }
    types
}

/// The type of a `$variable` or `$this->property` receiver at `at` in the method holding it.
pub fn receiver_hint_at(text: &str, at: usize, receiver: &str) -> Option<TypeHint> {
    let model = php_text::class_model(text)?;
    let masked = php_text::mask(text);
    let resolve = php_text::resolver(text);
    receiver_hint(receiver, &scope_at(&model, text, &masked, &resolve, at))
}

/// `$receiver->partial` at `at` in masked PHP: the receiver (`$var` or `$this->property`) and the word being typed.
pub fn member_access_at(masked: &str, at: usize) -> Option<(String, usize, usize)> {
    let bytes = masked.as_bytes();
    let mut start = at.min(bytes.len());
    while start > 0 && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_') {
        start -= 1;
    }
    let mut end = at.min(bytes.len());
    while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
        end += 1;
    }

    let before = masked[..start].strip_suffix("?->").or_else(|| masked[..start].strip_suffix("->"))?;
    let length = before.bytes().rev().take_while(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$' | b'>' | b'-')).count();
    let receiver = &before[before.len() - length..];
    let valid = receiver.starts_with('$') && receiver.matches('$').count() == 1 && (!receiver.contains("->") || receiver.starts_with("$this->") && receiver.matches("->").count() == 1);
    valid.then(|| (receiver.to_owned(), start, end))
}

/// What `$this->name` reaches inside the class declared in `text`: its own members of every
/// visibility, and the public and protected ones of project parents.
pub fn this_members(load: Loader, text: &str) -> Vec<Member> {
    let Some(model) = php_text::class_model(text) else { return Vec::new() };
    let mut found: BTreeMap<String, Member> = BTreeMap::new();
    let parents = model.parent.as_deref().map(|parent| chain(load, parent)).unwrap_or_default();

    for (class, own) in parents.iter().rev().map(|class| (class, false)).chain(std::iter::once((&model, true))) {
        let visible = |visibility: Visibility| own || visibility != Visibility::Private;
        let own_type = |name: &Option<String>| match name.as_deref() {
            Some("self" | "static") => Some(class.fqcn.clone()),
            other => other.map(str::to_owned),
        };
        for property in class.properties.iter().filter(|property| visible(property.visibility) && !property.is_static) {
            found.insert(property.name.clone(), Member { name: property.name.clone(), detail: format!("${}", property.name), type_name: own_type(&property.type_name), element_type: property.element_class.clone(), owner: class.fqcn.clone(), range: property.name_range.clone() });
        }
        for method in class.methods.iter().filter(|method| visible(method.visibility) && !method.name.starts_with("__")) {
            found.insert(method.name.clone(), Member { name: method.name.clone(), detail: format!("{}()", method.name), type_name: own_type(&method.return_class), element_type: method.element_class.clone(), owner: class.fqcn.clone(), range: method.name_range.clone() });
        }
    }
    found.into_values().collect()
}

/// Every instance member reachable as `$object->name`: public properties and public methods.
pub fn instance_members(load: Loader, fqcn: &str) -> Vec<Member> {
    let mut found: BTreeMap<String, Member> = BTreeMap::new();
    for model in chain(load, fqcn).iter().rev() {
        let own = |name: &Option<String>| match name.as_deref() {
            Some("self" | "static") => Some(model.fqcn.clone()),
            other => other.map(str::to_owned),
        };
        for property in model.properties.iter().filter(|property| property.visibility == Visibility::Public && !property.is_static) {
            found.insert(property.name.clone(), Member { name: property.name.clone(), detail: format!("${}", property.name), type_name: own(&property.type_name), element_type: property.element_class.clone(), owner: model.fqcn.clone(), range: property.name_range.clone() });
        }
        for method in model.methods.iter().filter(|method| method.visibility == Visibility::Public && !method.is_static && !method.name.starts_with("__")) {
            found.insert(method.name.clone(), Member { name: method.name.clone(), detail: format!("{}()", method.name), type_name: own(&method.return_class), element_type: method.element_class.clone(), owner: model.fqcn.clone(), range: method.name_range.clone() });
        }
    }
    found.into_values().collect()
}

/// `$name = rhs` (not `==`, not a compound assignment) of one statement.
fn assignment(statement: &str) -> Option<(&str, &str)> {
    let statement = statement.trim().rsplit(['{', '}']).next()?.trim();
    let rest = statement.strip_prefix('$')?;
    let end = identifier_end(rest);
    let (name, after) = rest.split_at(end);
    let after = after.trim_start();
    let rhs = after.strip_prefix('=').filter(|rhs| !rhs.starts_with(['=', '>']))?;
    (!name.is_empty()).then_some((name, rhs.trim()))
}

fn receiver_hint(receiver: &str, scope: &Scope) -> Option<TypeHint> {
    let receiver = receiver.trim();
    if let Some(property) = receiver.strip_prefix("$this->") {
        return scope.properties.get(property).map(|name| TypeHint::Class(name.clone()));
    }
    scope.known.get(receiver.strip_prefix('$')?).cloned()
}

fn class_constant(argument: &str, scope: &Scope) -> Option<String> {
    let name = argument.trim().strip_suffix("::class")?;
    (!name.is_empty() && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'\\'))).then(|| (scope.resolve)(name))
}

fn string_literal(argument: &str) -> Option<String> {
    let argument = argument.trim();
    let quote = argument.chars().next().filter(|quote| matches!(quote, '\'' | '"'))?;
    let inner = argument[1..].split(quote).next()?;
    (!inner.is_empty() && !inner.contains(['\\', '$', '{'])).then(|| inner.to_owned())
}

/// `root->a(x)->b->c(y)`: the root and the member accesses with their argument text (`None` for properties).
fn split_calls(expression: &str) -> Option<(String, Vec<(String, Option<String>)>)> {
    let expression = expression.replace("?->", "->");
    let bytes = expression.as_bytes();
    let (mut depth, mut start, mut parts) = (0usize, 0usize, Vec::new());
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth = depth.saturating_sub(1),
            b'-' if depth == 0 && bytes.get(at + 1) == Some(&b'>') => {
                parts.push(&expression[start..at]);
                start = at + 2;
                at += 1;
            }
            _ => {}
        }
        at += 1;
    }
    parts.push(&expression[start..]);

    let mut parts = parts.into_iter();
    let mut root = parts.next()?.trim().to_owned();
    let mut calls: Vec<(String, Option<String>)> = Vec::new();
    for part in parts {
        let part = part.trim();
        let name = &part[..identifier_end(part)];
        if name.is_empty() {
            return None;
        }
        let rest = part[name.len()..].trim_start();
        let arguments = match rest.strip_prefix('(') {
            Some(inner) => Some(inner[..inner.rfind(')')?].to_owned()),
            None if rest.is_empty() => None,
            None => return None,
        };
        calls.push((name.to_owned(), arguments));
    }
    if root == "$this" && calls.first().is_some_and(|(_, arguments)| arguments.is_none()) {
        root = format!("$this->{}", calls.remove(0).0);
    }
    Some((root, calls))
}

fn is_manager(class: &str) -> bool {
    ["EntityManagerInterface", "EntityManager", "ObjectManager"].iter().any(|name| class.ends_with(name))
}

fn is_container(class: &str) -> bool {
    class.ends_with("ContainerInterface") || class.ends_with("ServiceLocator")
}

const BUILDER_CALLS: [&str; 22] = [
    "select", "addSelect", "where", "andWhere", "orWhere", "join", "innerJoin", "leftJoin", "orderBy", "addOrderBy", "groupBy", "addGroupBy", "having", "andHaving", "orHaving", "setParameter", "setParameters",
    "setMaxResults", "setFirstResult", "distinct", "indexBy", "setCacheable",
];

/// Where a call chain currently is.
enum Flow {
    Form(String),
    Object(String),
    Repository(TypeHint),
    Builder(Option<TypeHint>),
    Query(Option<TypeHint>),
    Entity(TypeHint),
    Elements(TypeHint),
}

fn flow_hint(expression: &str, scope: &Scope) -> Option<TypeHint> {
    let (root, mut calls) = split_calls(expression)?;
    let first_is_get = calls.first().is_some_and(|(name, _)| name == "get");
    let first_argument = |arguments: &Option<String>| arguments.as_deref().and_then(|arguments| arguments.split(',').next()).unwrap_or_default().to_owned();
    let mut flow = match receiver_hint(&root, scope) {
        _ if root == "$this" && calls.first().is_some_and(|(name, arguments)| name == "createForm" && arguments.is_some()) => {
            let (_, arguments) = calls.remove(0);
            Flow::Form(class_constant(&first_argument(&arguments), scope)?)
        }
        Some(TypeHint::Form(form)) => Flow::Form(form),
        Some(TypeHint::Class(class)) => Flow::Object(class),
        Some(TypeHint::Builder(entity)) => Flow::Builder(Some(*entity)),
        _ if first_is_get && root.to_ascii_lowercase().contains("container") => Flow::Object("Container".into()),
        _ => return None,
    };

    for (name, arguments) in &calls {
        let arguments = arguments.as_deref();
        let first = arguments.and_then(|arguments| arguments.split(',').next()).unwrap_or_default();
        flow = match (flow, name.as_str()) {
            (Flow::Form(form), "createView" | "handleRequest" | "setData" | "add" | "remove") => Flow::Form(form),
            (Flow::Object(class), "create") if class.ends_with("FormFactoryInterface") => Flow::Form(class_constant(first, scope)?),
            (Flow::Object(class), "get") if is_container(&class) || class == "Container" => {
                if let Some(service) = class_constant(first, scope) {
                    Flow::Entity(TypeHint::Class(service))
                } else {
                    Flow::Entity(TypeHint::Service(string_literal(first)?))
                }
            }
            (Flow::Object(class), "getRepository") if is_manager(&class) => Flow::Repository(TypeHint::Class(class_constant(first, scope)?)),
            (Flow::Object(class), "createQueryBuilder") => Flow::Builder(if is_manager(&class) { None } else { Some(TypeHint::EntityOf(class)) }),
            (Flow::Object(class), "find") if is_manager(&class) => Flow::Entity(TypeHint::Class(class_constant(first, scope)?)),
            (Flow::Object(class), finder) if !is_manager(&class) && FINDERS.contains(&finder) => Flow::Entity(TypeHint::EntityOf(class)),
            (Flow::Object(class), finder) if !is_manager(&class) && LIST_FINDERS.contains(&finder) => Flow::Elements(TypeHint::EntityOf(class)),
            (Flow::Repository(entity), "createQueryBuilder") => Flow::Builder(Some(entity)),
            (Flow::Repository(entity), finder) if FINDERS.contains(&finder) => Flow::Entity(entity),
            (Flow::Repository(entity), finder) if LIST_FINDERS.contains(&finder) => Flow::Elements(entity),
            (Flow::Builder(_), "from") => Flow::Builder(Some(TypeHint::Class(class_constant(first, scope)?))),
            (Flow::Builder(entity), call) if BUILDER_CALLS.contains(&call) => Flow::Builder(entity),
            (Flow::Builder(entity), "getQuery") => Flow::Query(entity),
            (Flow::Query(entity), "getResult" | "execute") => Flow::Elements(entity?),
            (Flow::Query(entity), "getOneOrNullResult" | "getSingleResult") => Flow::Entity(entity?),
            _ => return None,
        };
    }

    match flow {
        Flow::Entity(hint) => Some(hint),
        Flow::Elements(hint) => Some(TypeHint::ElementsOf(Box::new(hint))),
        Flow::Builder(entity) => entity.map(|entity| TypeHint::Builder(Box::new(entity))),
        Flow::Form(form) => Some(TypeHint::Form(form)),
        _ => None,
    }
}

fn expression_hint(expression: &str, scope: &Scope) -> Option<TypeHint> {
    let expression = expression.split("??").next()?.trim().trim_start_matches('(').trim().trim_end_matches(';');
    let expression = expression.strip_suffix(')').filter(|rest| rest.matches('(').count() == rest.matches(')').count()).unwrap_or(expression).trim();

    if let Some(rest) = expression.strip_prefix("new ") {
        let rest = rest.trim_start();
        let name = &rest[..identifier_end(rest)];
        return (!name.is_empty() && !matches!(name, "static" | "self" | "parent" | "class")).then(|| TypeHint::Class((scope.resolve)(name)));
    }

    if let Some(variable) = expression.strip_prefix('$') {
        if variable.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_') {
            return scope.known.get(variable).cloned();
        }
    }

    flow_hint(expression, scope)
}

/// The entity class a repository manages: `@extends ServiceEntityRepository<Post>` or `parent::__construct($registry, Post::class)`.
fn repository_entity(source: &str) -> Option<String> {
    let resolve = php_text::resolver(source);
    if let Some(at) = source.find("@extends") {
        let rest = &source[at..];
        if let Some(open) = rest.find('<').filter(|open| rest[..*open].lines().count() == 1) {
            let inner = &rest[open + 1..];
            let name = inner[..inner.find(['>', ',']).unwrap_or(inner.len())].trim();
            if !name.is_empty() && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'\\')) {
                return Some(resolve(name));
            }
        }
    }

    let call = &source[source.find("parent::__construct(")? + "parent::__construct(".len()..];
    let arguments = &call[..call.find(')')?];
    arguments.split(',').find_map(|argument| {
        let name = argument.trim().strip_suffix("::class")?;
        (!name.is_empty() && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'\\'))).then(|| resolve(name))
    })
}

/// The class of a service by id.
pub type ServiceClass<'a> = &'a dyn Fn(&str) -> Option<String>;

/// The class a hint stands for, reading repository sources when needed.
pub fn hint_class(hint: &TypeHint, load: Loader, service: ServiceClass) -> Option<String> {
    match hint {
        TypeHint::Class(name) => Some(name.clone()),
        TypeHint::EntityOf(repository) => repository_entity(&load(repository)?),
        TypeHint::Service(id) => service(id),
        TypeHint::ElementsOf(_) | TypeHint::Builder(_) | TypeHint::Form(_) => None,
    }
}

/// What a hint stands for as a value: an object, or a list of objects.
pub fn hint_value(hint: &TypeHint, load: Loader, service: ServiceClass) -> Option<Value> {
    match hint {
        TypeHint::Form(form) => Some(Value::Form(form.clone())),
        TypeHint::ElementsOf(inner) => Some(Value::List(hint_class(inner, load, service)?)),
        other => Some(Value::Object(hint_class(other, load, service)?)),
    }
}

fn chain(load: Loader, fqcn: &str) -> Vec<ClassModel> {
    let mut models = Vec::new();
    let mut next = Some(fqcn.trim_start_matches('\\').to_owned());
    while let Some(name) = next.take() {
        if models.len() > MAX_PARENTS {
            break;
        }
        let Some(model) = load(&name).and_then(|source| php_text::class_model(&source)) else { break };
        next = model.parent.clone().map(|parent| parent.trim_start_matches('\\').to_owned());
        models.push(model);
    }
    models
}

fn lower_first(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map(|first| first.to_lowercase().chain(chars).collect()).unwrap_or_default()
}

/// What `object.name` can read on instances of `fqcn`: public properties and no-argument public
/// methods, with `getX`/`isX`/`hasX`/`canX` also reachable as `x`. Parents come from project sources.
pub fn members(load: Loader, fqcn: &str) -> Vec<Member> {
    let mut found: BTreeMap<String, Member> = BTreeMap::new();
    let models = chain(load, fqcn);

    for model in models.iter().rev() {
        let own = |name: &Option<String>| match name.as_deref() {
            Some("self" | "static") => Some(model.fqcn.clone()),
            other => other.map(str::to_owned),
        };

        for property in model.properties.iter().filter(|property| property.visibility == Visibility::Public && !property.is_static) {
            found.insert(property.name.clone(), Member { name: property.name.clone(), detail: format!("${}", property.name), type_name: own(&property.type_name), element_type: property.element_class.clone(), owner: model.fqcn.clone(), range: property.name_range.clone() });
        }

        for method in model.methods.iter().filter(|method| method.visibility == Visibility::Public && !method.is_static && !method.name.starts_with("__")) {
            if method.params.iter().any(|param| !param.has_default && !param.variadic) {
                continue;
            }
            let type_name = own(&method.return_class);
            let accessor = ["get", "is", "has", "can"].iter().find_map(|prefix| {
                let rest = method.name.strip_prefix(prefix)?;
                rest.chars().next().filter(|first| first.is_uppercase())?;
                Some(lower_first(rest))
            });
            let name = accessor.unwrap_or_else(|| method.name.clone());
            found.insert(name.clone(), Member { name, detail: format!("{}()", method.name), type_name, element_type: method.element_class.clone(), owner: model.fqcn.clone(), range: method.name_range.clone() });
        }
    }

    found.into_values().collect()
}

/// The value of `object.name`: an object when its type is a class, a list when a docblock types its items.
pub fn member_value(load: Loader, fqcn: &str, name: &str) -> Option<Value> {
    let member = members(load, fqcn).into_iter().find(|member| member.name == name)?;
    match (member.element_type, member.type_name) {
        (Some(element), _) => Some(Value::List(element)),
        (None, Some(class)) if is_class_type(&class) => Some(Value::Object(class)),
        _ => None,
    }
}

fn view_member(name: &str, detail: &str, owner: &str) -> Member {
    Member { name: name.to_owned(), detail: detail.to_owned(), type_name: None, element_type: None, owner: owner.to_owned(), range: 0..0 }
}

/// What a Twig value offers after a dot: an object's members, or a form view's fields and view members.
pub fn value_members(load: Loader, value: &Value) -> Vec<Member> {
    match value {
        Value::Object(class) => members(load, class),
        Value::List(_) => Vec::new(),
        Value::Vars => crate::forms::VARS.iter().map(|name| view_member(name, "form.vars", "")).collect(),
        Value::Field => crate::forms::VIEW_MEMBERS.iter().map(|name| view_member(name, "FormView", "")).collect(),
        Value::Form(form) => {
            let mut found: Vec<Member> = load(form)
                .map(|source| crate::forms::fields(&source))
                .unwrap_or_default()
                .into_iter()
                .map(|field| Member { name: field.name.clone(), detail: "field".into(), type_name: field.type_name, element_type: None, owner: form.clone(), range: field.range })
                .collect();
            found.extend(crate::forms::VIEW_MEMBERS.iter().map(|name| view_member(name, "FormView", "")));
            found
        }
    }
}

/// The value of `value.name`.
pub fn value_member(load: Loader, value: &Value, name: &str) -> Option<Value> {
    match value {
        Value::Object(class) => member_value(load, class, name),
        Value::List(_) | Value::Vars | Value::Field => None,
        Value::Form(form) => {
            if name == "vars" {
                return Some(Value::Vars);
            }
            let field = value_members(load, value).into_iter().find(|member| member.name == name && member.owner == *form)?;
            // An embedded project form type keeps its own fields.
            match field.type_name.filter(|class| load(class).is_some_and(|source| !crate::forms::fields(&source).is_empty())) {
                Some(nested) => Some(Value::Form(nested)),
                None => Some(Value::Field),
            }
        }
    }
}

/// The value a docblock-style type stands for: `Foo`, `Foo[]` or `Collection<Foo>`.
fn type_value(declared: &str) -> Option<Value> {
    let declared = declared.trim_start_matches('\\').trim_start_matches('?');
    let (name, list) = match (declared.find('<'), declared.rfind('>')) {
        (Some(open), Some(close)) if open < close => (declared[open + 1..close].rsplit(',').next()?.trim().trim_start_matches('\\'), true),
        _ => match declared.strip_suffix("[]") {
            Some(item) => (item, true),
            None => (declared, false),
        },
    };
    let valid = !name.is_empty() && is_class_type(name) && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'\\'));
    valid.then(|| if list { Value::List(name.to_owned()) } else { Value::Object(name.to_owned()) })
}

/// `{# @var name Type #}` comments before `offset`: variable name to value.
pub fn var_comment_values(text: &str, offset: usize) -> BTreeMap<String, Value> {
    let mut values = BTreeMap::new();
    for (at, _) in text[..offset.min(text.len())].match_indices("{#") {
        let Some(end) = text[at + 2..].find("#}").map(|c| c + 2) else { continue };
        let body = text[at + 2..at + end].trim();
        let Some(rest) = body.strip_prefix("@var") else { continue };
        let words: Vec<&str> = rest.split_whitespace().collect();
        let [first, second, ..] = words[..] else { continue };
        let name_first = first.starts_with('$') || !first.contains('\\') && second.contains('\\') || first.chars().next().is_some_and(char::is_lowercase);
        let (name, declared) = if name_first { (first.trim_start_matches('$'), second) } else { (second.trim_start_matches('$'), first) };
        if name.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_') {
            if let Some(value) = type_value(declared) {
                values.insert(name.to_owned(), value);
            }
        }
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    fn types(text: &str, entries: &[(&str, &str)]) -> BTreeMap<String, TypeHint> {
        let at = text.find("$this->render(").unwrap();
        render_variable_types(text, at, &entries.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect::<Vec<_>>())
    }

    const CONTROLLER: &str = "<?php\nnamespace App\\Controller;\nuse App\\Entity\\Post;\nuse App\\Repository\\PostRepository;\nuse Doctrine\\ORM\\EntityManagerInterface;\nclass BlogController\n{\n    public function __construct(private PostRepository $posts) {}\n    public function show(Post $typed, PostRepository $repo, EntityManagerInterface $em, int $id)\n    {\n        $fresh = new Post();\n        $found = $repo->find($id);\n        $via = $this->posts->findOneBy(['id' => $id]);\n        $direct = $em->find(Post::class, $id);\n        $chained = $em->getRepository(Post::class)->findOneBy([]) ?? throw new \\Exception();\n        $number = 5;\n        return $this->render('blog/show.html.twig', []);\n    }\n}\n";

    #[test]
    fn variables_are_typed_from_parameters_constructions_and_finders() {
        let found = types(CONTROLLER, &[("typed", "$typed"), ("fresh", "$fresh"), ("found", "$found"), ("via", "$via"), ("direct", "$direct"), ("chained", "$chained"), ("number", "$number"), ("literal", "'x'"), ("inline", "new Post()")]);
        let class = |name: &str| TypeHint::Class(name.into());
        assert_eq!(found["typed"], class("App\\Entity\\Post"));
        assert_eq!(found["fresh"], class("App\\Entity\\Post"));
        assert_eq!(found["found"], TypeHint::EntityOf("App\\Repository\\PostRepository".into()));
        assert_eq!(found["via"], TypeHint::EntityOf("App\\Repository\\PostRepository".into()));
        assert_eq!(found["direct"], class("App\\Entity\\Post"));
        assert_eq!(found["chained"], class("App\\Entity\\Post"));
        assert_eq!(found["inline"], class("App\\Entity\\Post"));
        assert!(!found.contains_key("number") && !found.contains_key("literal"));
    }

    #[test]
    fn a_reassigned_variable_loses_its_type_when_the_new_value_is_unknown() {
        let text = "<?php\nuse App\\Entity\\Post;\nclass C { public function a() { $x = new Post(); $x = compute(); return $this->render('t', []); } }\n";
        assert!(types(text, &[("x", "$x")]).is_empty());
    }

    #[test]
    fn repository_entities_come_from_the_extends_docblock_or_the_constructor() {
        let docblock = "<?php\nnamespace App\\Repository;\nuse App\\Entity\\Post;\n/**\n * @extends ServiceEntityRepository<Post>\n */\nclass PostRepository extends ServiceEntityRepository {}\n";
        assert_eq!(repository_entity(docblock).as_deref(), Some("App\\Entity\\Post"));
        let constructor = "<?php\nnamespace App\\Repository;\nuse App\\Entity\\Tag;\nclass TagRepository extends ServiceEntityRepository\n{\n    public function __construct(ManagerRegistry $registry)\n    {\n        parent::__construct($registry, Tag::class);\n    }\n}\n";
        assert_eq!(repository_entity(constructor).as_deref(), Some("App\\Entity\\Tag"));
        assert_eq!(repository_entity("<?php\nclass R {}\n"), None);
    }

    #[test]
    fn members_follow_twig_attribute_rules_across_project_parents() {
        let sources: BTreeMap<&str, &str> = BTreeMap::from([
            ("App\\Entity\\Base", "<?php\nnamespace App\\Entity;\nabstract class Base\n{\n    public function getId(): ?int { return 1; }\n    protected function hidden(): string { return ''; }\n}\n"),
            ("App\\Entity\\Post", "<?php\nnamespace App\\Entity;\nuse App\\Entity\\User;\nclass Post extends Base\n{\n    public string $slug = '';\n    private string $secret = '';\n    public function getAuthor(): ?User { return null; }\n    public function isPublished(): bool { return true; }\n    public function summary(int $length = 10): string { return ''; }\n    public function withArgument(int $required): string { return ''; }\n    public static function make(): self { return new self(); }\n}\n"),
        ]);
        let load = |name: &str| sources.get(name).map(|source| (*source).to_owned());
        let found: BTreeMap<_, _> = members(&load, "App\\Entity\\Post").into_iter().map(|member| (member.name.clone(), member)).collect();
        let names: Vec<_> = found.keys().map(String::as_str).collect();
        assert_eq!(names, ["author", "id", "published", "slug", "summary"]);
        assert_eq!(found["author"].type_name.as_deref(), Some("App\\Entity\\User"));
        assert_eq!(found["author"].detail, "getAuthor()");
        assert_eq!(member_value(&load, "App\\Entity\\Post", "author"), Some(Value::Object("App\\Entity\\User".into())));
        assert_eq!(member_value(&load, "App\\Entity\\Post", "published"), None);
    }

    #[test]
    fn var_comments_name_variables_in_either_order() {
        let text = "{# @var post \\App\\Entity\\Post #}\n{# @var \\App\\Entity\\User author #}\n{# @var $count int #}\n{# plain comment #}\n{{ post. }}";
        let found = var_comment_values(text, text.len());
        assert_eq!(found["post"], Value::Object("App\\Entity\\Post".into()));
        assert_eq!(found["author"], Value::Object("App\\Entity\\User".into()));
        assert!(!found.contains_key("count"));
        let lists = var_comment_values("{# @var posts \\App\\Entity\\Post[] #}\n{# @var tags Collection<App\\Entity\\Tag> #}", 200);
        assert_eq!(lists["posts"], Value::List("App\\Entity\\Post".into()));
        assert_eq!(lists["tags"], Value::List("App\\Entity\\Tag".into()));
    }

    #[test]
    fn container_get_with_a_class_constant_is_that_class() {
        let text = "<?php\nnamespace App;\nuse App\\Service\\Mailer;\nuse Psr\\Container\\ContainerInterface;\nclass C\n{\n    public function __construct(private ContainerInterface $locator) {}\n    public function a(ContainerInterface $box)\n    {\n        $one = $box->get(Mailer::class);\n        $two = $this->container->get(Mailer::class);\n        $three = $this->locator->get(Mailer::class);\n        $four = $box->get('app.mailer');\n        return $this->render('t', []);\n    }\n}\n";
        let at = text.find("$this->render(").unwrap();
        let found = render_variable_types(text, at, &[("one".into(), "$one".into()), ("two".into(), "$two".into()), ("three".into(), "$three".into()), ("four".into(), "$four".into())]);
        let mailer = TypeHint::Class("App\\Service\\Mailer".into());
        assert_eq!(found.get("one"), Some(&mailer));
        assert_eq!(found.get("two"), Some(&mailer));
        assert_eq!(found.get("three"), Some(&mailer));
        assert_eq!(found.get("four"), Some(&TypeHint::Service("app.mailer".into())));
    }

    #[test]
    fn member_access_finds_the_receiver_and_the_partial_word() {
        let masked = "$post->ti";
        assert_eq!(member_access_at(masked, masked.len()), Some(("$post".into(), 7, 9)));
        let masked = "$this->posts->";
        assert_eq!(member_access_at(masked, masked.len()), Some(("$this->posts".into(), 14, 14)));
        assert_eq!(member_access_at("$a->b->", 7), None);
        assert_eq!(member_access_at("foo()->x", 8), None);
    }

    #[test]
    fn list_finders_give_lists_of_the_entity() {
        let text = "<?php\nnamespace App;\nuse App\\Entity\\Post;\nuse App\\Repository\\PostRepository;\nclass C\n{\n    public function a(PostRepository $repo, $em)\n    {\n        $all = $repo->findAll();\n        $some = $repo->findBy(['x' => 1]);\n        $typed = $em->getRepository(Post::class)->findAll();\n        return $this->render('t', []);\n    }\n}\n";
        let at = text.find("$this->render(").unwrap();
        let found = render_variable_types(text, at, &[("all".into(), "$all".into()), ("some".into(), "$some".into()), ("typed".into(), "$typed".into())]);
        let repository = TypeHint::ElementsOf(Box::new(TypeHint::EntityOf("App\\Repository\\PostRepository".into())));
        assert_eq!(found["all"], repository);
        assert_eq!(found["some"], repository);
        assert!(!found.contains_key("typed"), "untyped $em receiver gives no hint");
    }

    #[test]
    fn query_builder_chains_end_in_lists_or_one_entity() {
        let text = "<?php\nnamespace App;\nuse App\\Entity\\Post;\nuse App\\Repository\\PostRepository;\nuse Doctrine\\ORM\\EntityManagerInterface;\nclass C\n{\n    public function a(PostRepository $repo, EntityManagerInterface $em)\n    {\n        $qb = $repo->createQueryBuilder('p')->where('p.x = 1')->orderBy('p.id');\n        $list = $repo->createQueryBuilder('p')->andWhere('p.x = :x')->setParameter('x', 1)->setMaxResults(5)->getQuery()->getResult();\n        $one = $qb->getQuery()->getOneOrNullResult();\n        $many = $qb->setMaxResults(3)->getQuery()->execute();\n        $manager = $em->createQueryBuilder()->select('p')->from(Post::class, 'p')->getQuery()->getResult();\n        $array = $repo->createQueryBuilder('p')->getQuery()->getArrayResult();\n        $unknown = $repo->createQueryBuilder('p')->customStep()->getQuery()->getResult();\n        return $this->render('t', []);\n    }\n}\n";
        let at = text.find("$this->render(").unwrap();
        let keys = ["qb", "list", "one", "many", "manager", "array", "unknown"];
        let entries: Vec<(String, String)> = keys.iter().map(|key| ((*key).to_owned(), format!("${key}"))).collect();
        let found = render_variable_types(text, at, &entries);
        let repository = TypeHint::EntityOf("App\\Repository\\PostRepository".into());
        assert_eq!(found["qb"], TypeHint::Builder(Box::new(repository.clone())));
        assert_eq!(found["list"], TypeHint::ElementsOf(Box::new(repository.clone())));
        assert_eq!(found["one"], repository);
        assert_eq!(found["many"], TypeHint::ElementsOf(Box::new(repository)));
        assert_eq!(found["manager"], TypeHint::ElementsOf(Box::new(TypeHint::Class("App\\Entity\\Post".into()))));
        assert!(!found.contains_key("array") && !found.contains_key("unknown"));
    }

    #[test]
    fn split_calls_reads_properties_and_nested_arguments() {
        let (root, calls) = split_calls("$this->posts->findBy(['a' => f(1)], null)?->x").unwrap();
        assert_eq!(root, "$this->posts");
        assert_eq!(calls, [("findBy".to_owned(), Some("['a' => f(1)], null".to_owned())), ("x".to_owned(), None)]);
        assert!(split_calls("$a->b(").is_none());
    }

    #[test]
    fn this_members_include_private_own_and_protected_parent_members() {
        let sources: BTreeMap<&str, &str> = BTreeMap::from([(
            "App\\Base",
            "<?php\nnamespace App;\nclass Base\n{\n    protected int $level = 1;\n    private int $hidden = 2;\n    protected function boot(): void {}\n    public function id(): int { return 1; }\n    private function secret(): void {}\n}\n",
        )]);
        let load = |name: &str| sources.get(name).map(|source| (*source).to_owned());
        let own = "<?php\nnamespace App;\nclass Child extends Base\n{\n    private string $label = '';\n    public function __construct(private Base $base) {}\n    private function helper(): static { return $this; }\n}\n";
        let names: Vec<_> = this_members(&load, own).into_iter().map(|member| member.name).collect();
        assert_eq!(names, ["base", "boot", "helper", "id", "label", "level"]);
    }
}
