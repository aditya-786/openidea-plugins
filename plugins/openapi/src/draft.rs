//! Draft OpenAPI 3.0.3 from source mappings: Spring MVC, JAX-RS (Quarkus) and Micronaut controllers.
//!
//! Only literal paths and literally named parameters are exported. Everything
//! else (constants, ant patterns, regex path variables, unresolved types) is
//! skipped or left as an empty schema and reported in the summary.
use openidea_plugin::{Error, Invocation, NotificationLevel, host};
use serde_json::{Map, Value, json};
use spring_text::java::{Annotation, Member, Param, TypeDecl, TypeRef, JavaFile, tokens};
use std::collections::{BTreeMap, BTreeSet, HashMap};

const MAX_FILES: usize = 1000;
const MAX_FILE_BYTES: usize = 256 * 1024;
const MAX_TOTAL_BYTES: usize = 8 * 1024 * 1024;
const MAX_NOTES: usize = 100;
const SKIPPED_DIRS: [&str; 6] = ["/src/test/", "/build/", "/target/", "/node_modules/", "/out/", "/.git/"];
const JAXRS_VERBS: [&str; 7] = ["GET", "POST", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS"];
const BODY_VERBS: [&str; 3] = ["post", "put", "patch"];
const COLLECTIONS: [&str; 7] = ["List", "Set", "Collection", "Iterable", "ArrayList", "Array", "MutableList"];

pub struct Draft {
    pub yaml: String,
    pub operations: usize,
    pub skipped: Vec<String>,
}

struct Op {
    path: String,
    verb: String,
    class: String,
    method: String,
    location: String,
    parameters: Vec<Value>,
    body: Option<Value>,
    status: u16,
}

type Dtos = HashMap<String, Option<Vec<(String, TypeRef)>>>;

struct Cx {
    dtos: Dtos,
    schemas: BTreeMap<String, Value>,
    notes: Vec<String>,
}

fn primitive(name: &str) -> Option<Value> {
    Some(match name {
        "String" | "CharSequence" | "Character" | "char" | "Char" => json!({"type": "string"}),
        "UUID" => json!({"type": "string", "format": "uuid"}),
        "LocalDate" => json!({"type": "string", "format": "date"}),
        "LocalDateTime" | "OffsetDateTime" | "ZonedDateTime" | "Instant" | "Date" => json!({"type": "string", "format": "date-time"}),
        "int" | "Integer" | "short" | "Short" | "Int" => json!({"type": "integer", "format": "int32"}),
        "long" | "Long" => json!({"type": "integer", "format": "int64"}),
        "boolean" | "Boolean" => json!({"type": "boolean"}),
        "double" | "Double" => json!({"type": "number", "format": "double"}),
        "float" | "Float" => json!({"type": "number", "format": "float"}),
        "BigDecimal" | "BigInteger" => json!({"type": "number"}),
        _ => return None,
    })
}

fn schema_name(name: &str) -> String {
    name.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' }).collect()
}

impl Cx {
    /// `top` turns an unreadable type into a referenced placeholder schema; otherwise it is an empty schema.
    fn schema(&mut self, ty: &TypeRef, top: bool, context: &str) -> Value {
        let name = ty.name.trim_end_matches('?');
        if name == "Optional" && ty.args.len() == 1 {
            return self.schema(&ty.args[0].clone(), top, context);
        }
        if COLLECTIONS.contains(&name) && ty.args.len() == 1 {
            let items = self.schema(&ty.args[0].clone(), top, context);
            return json!({"type": "array", "items": items});
        }
        if ty.array && ty.args.is_empty() {
            let element = TypeRef { array: false, ..ty.clone() };
            let items = self.schema(&element, top, context);
            return json!({"type": "array", "items": items});
        }
        if let Some(schema) = primitive(name) {
            return schema;
        }
        let key = schema_name(name);
        let fields = self.dtos.get(name).cloned();
        match fields {
            Some(Some(fields)) => {
                if !self.schemas.contains_key(&key) {
                    self.schemas.insert(key.clone(), json!({"type": "object"}));
                    let mut properties = Map::new();
                    for (field, field_ty) in &fields {
                        let property = self.schema(field_ty, false, context);
                        properties.insert(field.clone(), property);
                    }
                    self.schemas.insert(key.clone(), json!({"type": "object", "properties": properties}));
                }
                json!({"$ref": format!("#/components/schemas/{key}")})
            }
            _ if top && !name.is_empty() => {
                self.schemas.entry(key.clone()).or_insert_with(|| json!({
                    "type": "object",
                    "description": format!("TODO: the shape of {name} is not readable from source."),
                }));
                self.notes.push(format!("{context}: `{name}` is not a readable DTO; placeholder schema."));
                json!({"$ref": format!("#/components/schemas/{key}")})
            }
            _ => {
                self.notes.push(format!("{context}: unresolved type `{name}`; empty schema."));
                json!({})
            }
        }
    }
}

fn dtos(files: &[JavaFile]) -> Dtos {
    let mut out: Dtos = HashMap::new();
    for file in files {
        for ty in file.types.iter().filter(|t| matches!(t.kind.as_str(), "class" | "record")) {
            let json_annotated = ty.annotations.iter().any(|a| a.name.starts_with("Json"))
                || ty.fields().any(|f| f.annotations.iter().any(|a| a.name.starts_with("Json")))
                || ty.components.iter().any(|c| c.annotations.iter().any(|a| a.name.starts_with("Json")));
            let readable = !json_annotated && ty.extends.is_empty() && ty.name != "";
            let fields: Vec<(String, TypeRef)> = if !ty.components.is_empty() {
                ty.components.iter().map(|c| (c.name.clone(), c.ty.clone())).collect()
            } else {
                ty.fields()
                    .filter(|f| !f.modifiers.iter().any(|m| m == "static"))
                    .filter_map(|f| Some((f.name.clone(), f.ty.clone()?)))
                    .collect()
            };
            let entry = (readable && !fields.is_empty()).then_some(fields);
            // A simple name declared twice is ambiguous.
            match out.get(&ty.name) {
                Some(_) => out.insert(ty.name.clone(), None),
                None => out.insert(ty.name.clone(), entry),
            };
        }
    }
    out
}

/// Literal string values of the first of `keys` present; `Err` when the value is not purely literal.
fn literals(annotation: &Annotation, keys: &[&str]) -> Result<Vec<String>, ()> {
    let Some(arg) = keys.iter().find_map(|k| annotation.arg(k)) else { return Ok(Vec::new()) };
    let pure = tokens(&arg.text).iter().all(|t| {
        t.string().is_some() || matches!(t.s, "{" | "}" | "[" | "]" | "," | "(" | ")" | "arrayOf" | "listOf")
    });
    if !pure || arg.strings.is_empty() || arg.strings.iter().any(|s| s.value.contains("${") || s.value.contains("$")) {
        return Err(());
    }
    Ok(arg.strings.iter().map(|s| s.value.clone()).collect())
}

fn single(annotation: &Annotation, keys: &[&str]) -> Result<Option<String>, ()> {
    let mut found = literals(annotation, keys)?;
    match found.len() {
        0 => Ok(None),
        1 => Ok(found.pop()),
        _ => Err(()),
    }
}

fn status_of(member: &Member) -> Option<u16> {
    let annotation = member.annotation("ResponseStatus").or_else(|| member.annotation("Status"))?;
    let arg = ["value", "code"].iter().find_map(|k| annotation.arg(k))?;
    let name = arg.text.rsplit('.').next()?.trim();
    Some(match name {
        "OK" => 200,
        "CREATED" => 201,
        "ACCEPTED" => 202,
        "NO_CONTENT" => 204,
        "BAD_REQUEST" => 400,
        "NOT_FOUND" => 404,
        "CONFLICT" => 409,
        _ => return None,
    })
}

/// Validated `{name}` variables of a path template.
fn template_vars(path: &str) -> Result<Vec<String>, String> {
    if !path.starts_with('/') {
        return Err("path does not start with `/`".into());
    }
    if path.contains(['*', '?']) {
        return Err("ant pattern or optional segment".into());
    }
    let mut vars = Vec::new();
    let mut rest = path;
    while let Some(open) = rest.find(['{', '}']) {
        if !rest[open..].starts_with('{') {
            return Err("unbalanced braces".into());
        }
        let Some(close) = rest[open..].find('}') else { return Err("unbalanced braces".into()) };
        let inner = rest[open + 1..open + close].trim();
        if inner.contains(':') {
            return Err("regex path variable".into());
        }
        if inner.is_empty() || !inner.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')) {
            return Err("unsupported path template".into());
        }
        vars.push(inner.to_owned());
        rest = &rest[open + close + 1..];
    }
    if rest.contains('}') {
        return Err("unbalanced braces".into());
    }
    Ok(vars)
}

fn join(prefix: &str, path: &str) -> String {
    let joined = format!("/{}/{}", prefix.trim_matches('/'), path.trim_start_matches('/'));
    let mut out = String::new();
    for c in joined.chars() {
        if !(c == '/' && out.ends_with('/')) {
            out.push(c);
        }
    }
    if out.len() > 1 && out.ends_with('/') {
        out.pop();
    }
    out
}

fn optional(ty: &TypeRef) -> bool {
    ty.text.ends_with('?') || ty.name == "Optional"
}

fn false_arg(annotation: &Annotation, key: &str) -> bool {
    annotation.arg(key).is_some_and(|a| a.text.trim() == "false")
}

#[derive(Clone, Copy, PartialEq)]
enum Style {
    Spring,
    JaxRs,
    Micronaut,
}

/// The path variables, query parameters and body declared by a handler's parameters.
struct Bound {
    path: Vec<(String, Value)>,
    query: Vec<Value>,
    body: Option<Value>,
}

fn bind(cx: &mut Cx, style: Style, verb: &str, method: &Member, template_vars: &[String], context: &str) -> Option<Bound> {
    let mut bound = Bound { path: Vec::new(), query: Vec::new(), body: None };
    for param in &method.params {
        match bind_param(cx, style, verb, param, template_vars, context, &mut bound) {
            Ok(()) => {}
            Err(reason) => {
                cx.notes.push(format!("{context}: parameter `{}` {reason}", param.name));
            }
        }
    }
    Some(bound)
}

fn bind_param(
    cx: &mut Cx, style: Style, verb: &str, param: &Param, vars: &[String], context: &str, bound: &mut Bound,
) -> Result<(), String> {
    let (path_names, query_names, body_names): (&[&str], &[&str], &[&str]) = match style {
        Style::Spring => (&["PathVariable"], &["RequestParam"], &["RequestBody"]),
        Style::JaxRs => (&["PathParam"], &["QueryParam"], &[]),
        Style::Micronaut => (&["PathVariable"], &["QueryValue"], &["Body"]),
    };
    let find = |names: &[&str]| param.annotations.iter().find(|a| names.contains(&a.name.as_str()));
    let explicit = |a: &Annotation| -> Result<String, String> {
        match style {
            Style::JaxRs => single(a, &["value"]).map_err(|_| "has a computed name; skipped".to_owned())?.ok_or_else(|| "has no name; skipped".to_owned()),
            _ => Ok(single(a, &["value", "name"]).map_err(|_| "has a computed name; skipped".to_owned())?.unwrap_or_else(|| param.name.clone())),
        }
    };
    if let Some(a) = find(path_names) {
        let name = explicit(a)?;
        let schema = cx.schema(&param.ty, false, context);
        bound.path.push((name, schema));
        return Ok(());
    }
    if let Some(a) = find(query_names) {
        let name = explicit(a)?;
        let schema = cx.schema(&param.ty, false, context);
        let required = style == Style::Spring && !false_arg(a, "required") && a.arg("defaultValue").is_none() && !optional(&param.ty)
            || style == Style::Micronaut && a.arg("defaultValue").is_none() && !optional(&param.ty);
        let mut entry = json!({"name": name, "in": "query", "schema": schema});
        if required {
            entry["required"] = json!(true);
        }
        bound.query.push(entry);
        return Ok(());
    }
    if let Some(a) = find(body_names) {
        let schema = cx.schema(&param.ty, true, context);
        let mut body = json!({"content": {"application/json": {"schema": schema}}});
        if style == Style::Spring && !false_arg(a, "required") || style != Style::Spring {
            body["required"] = json!(true);
        }
        bound.body = Some(body);
        return Ok(());
    }
    let plain = param.annotations.iter().all(|a| matches!(a.name.as_str(), "Valid" | "Validated" | "NotNull" | "NonNull" | "Nullable"));
    if style == Style::JaxRs && plain && BODY_VERBS.contains(&verb) {
        let schema = cx.schema(&param.ty, true, context);
        bound.body = Some(json!({"content": {"application/json": {"schema": schema}}}));
    }
    if style == Style::Micronaut && param.annotations.is_empty() && vars.contains(&param.name) {
        let schema = cx.schema(&param.ty, false, context);
        bound.path.push((param.name.clone(), schema));
    }
    Ok(())
}

fn expand(
    cx: &mut Cx, ops: &mut Vec<Op>, class: &TypeDecl, method: &Member, location: &str, style: Style,
    verb: &str, prefixes: &[String], paths: &[String],
) {
    let context = format!("{location}: {}.{}", class.name, method.name);
    let status = status_of(method).unwrap_or(200);
    for prefix in prefixes {
        for path in paths {
            let full = join(prefix, path);
            let vars = match template_vars(&full) {
                Ok(vars) => vars,
                Err(reason) => {
                    cx.notes.push(format!("{context}: skipped `{verb} {full}`: {reason}."));
                    continue;
                }
            };
            let Some(bound) = bind(cx, style, verb, method, &vars, &context) else { continue };
            let mut parameters = Vec::new();
            for var in &vars {
                let schema = match bound.path.iter().find(|(name, _)| name == var) {
                    Some((_, schema)) => json!({"name": var, "in": "path", "required": true, "schema": schema}),
                    None => json!({"name": var, "in": "path", "required": true, "schema": {"type": "string"},
                        "description": "TODO: not bound to a method parameter."}),
                };
                parameters.push(schema);
            }
            for (name, _) in &bound.path {
                if !vars.contains(name) {
                    cx.notes.push(format!("{context}: path parameter `{name}` is not in `{full}`; dropped."));
                }
            }
            let mut seen = BTreeSet::new();
            for query in &bound.query {
                if seen.insert(query["name"].as_str().unwrap_or_default().to_owned()) {
                    parameters.push(query.clone());
                }
            }
            ops.push(Op {
                path: full,
                verb: verb.to_owned(),
                class: class.name.clone(),
                method: method.name.clone(),
                location: location.to_owned(),
                parameters,
                body: bound.body.clone(),
                status,
            });
        }
    }
}

fn spring(cx: &mut Cx, ops: &mut Vec<Op>, class: &TypeDecl, location: &str) {
    let prefixes = match class.annotation("RequestMapping").map(|a| literals(a, &["value", "path"])) {
        None => vec![String::new()],
        Some(Ok(p)) if p.is_empty() => vec![String::new()],
        Some(Ok(p)) => p,
        Some(Err(())) => {
            cx.notes.push(format!("{location}: {}: computed class path; controller skipped.", class.name));
            return;
        }
    };
    for method in class.methods() {
        for a in &method.annotations {
            let verbs: Vec<String> = match a.name.as_str() {
                "GetMapping" => vec!["get".into()],
                "PostMapping" => vec!["post".into()],
                "PutMapping" => vec!["put".into()],
                "DeleteMapping" => vec!["delete".into()],
                "PatchMapping" => vec!["patch".into()],
                "RequestMapping" => {
                    let named: BTreeSet<String> = a.arg("method").map(|m| {
                        m.text.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| JAXRS_VERBS.contains(w)).map(|w| w.to_lowercase()).collect()
                    }).unwrap_or_default();
                    if named.len() != 1 {
                        cx.notes.push(format!("{location}: {}.{}: @RequestMapping without exactly one fixed method; skipped.", class.name, method.name));
                        continue;
                    }
                    named.into_iter().collect()
                }
                _ => continue,
            };
            let paths = match literals(a, &["value", "path"]) {
                Ok(p) if p.is_empty() => vec![String::new()],
                Ok(p) => p,
                Err(()) => {
                    cx.notes.push(format!("{location}: {}.{}: computed path; skipped.", class.name, method.name));
                    continue;
                }
            };
            for verb in verbs {
                expand(cx, ops, class, method, location, Style::Spring, &verb, &prefixes, &paths);
            }
        }
    }
}

fn micronaut(cx: &mut Cx, ops: &mut Vec<Op>, class: &TypeDecl, location: &str) {
    let Some(controller) = class.annotation("Controller") else { return };
    let prefixes = match literals(controller, &["value", "uri"]) {
        Ok(p) if p.is_empty() => vec![String::new()],
        Ok(p) => p,
        Err(()) => {
            cx.notes.push(format!("{location}: {}: computed controller path; skipped.", class.name));
            return;
        }
    };
    for method in class.methods() {
        for a in &method.annotations {
            let verb = match a.name.as_str() {
                "Get" | "Post" | "Put" | "Delete" | "Patch" | "Head" | "Options" => a.name.to_lowercase(),
                _ => continue,
            };
            let paths = match literals(a, &["value", "uri"]) {
                Ok(p) if p.is_empty() => vec![String::new()],
                Ok(p) => p,
                Err(()) => {
                    cx.notes.push(format!("{location}: {}.{}: computed path; skipped.", class.name, method.name));
                    continue;
                }
            };
            expand(cx, ops, class, method, location, Style::Micronaut, &verb, &prefixes, &paths);
        }
    }
}

fn jaxrs(cx: &mut Cx, ops: &mut Vec<Op>, class: &TypeDecl, location: &str) {
    let Some(class_path) = class.annotation("Path") else { return };
    let prefixes = match literals(class_path, &["value"]) {
        Ok(p) if p.len() == 1 => p,
        _ => {
            cx.notes.push(format!("{location}: {}: computed @Path; skipped.", class.name));
            return;
        }
    };
    for method in class.methods() {
        let Some(verb) = JAXRS_VERBS.iter().find(|v| method.annotation(v).is_some()) else { continue };
        let paths = match method.annotation("Path").map(|a| literals(a, &["value"])) {
            None => vec![String::new()],
            Some(Ok(p)) if p.len() == 1 => p,
            _ => {
                cx.notes.push(format!("{location}: {}.{}: computed @Path; skipped.", class.name, method.name));
                continue;
            }
        };
        expand(cx, ops, class, method, location, Style::JaxRs, &verb.to_lowercase(), &prefixes, &paths);
    }
}

fn emit(value: &Value, indent: usize, out: &mut String) {
    let pad = " ".repeat(indent);
    let scalar = |v: &Value| match v {
        Value::Object(m) if m.is_empty() => Some("{}".to_owned()),
        Value::Array(a) if a.is_empty() => Some("[]".to_owned()),
        Value::Object(_) | Value::Array(_) => None,
        other => Some(other.to_string()),
    };
    match value {
        Value::Object(map) => {
            for (key, v) in map {
                let key = Value::String(key.clone()).to_string();
                match scalar(v) {
                    Some(s) => out.push_str(&format!("{pad}{key}: {s}\n")),
                    None => {
                        out.push_str(&format!("{pad}{key}:\n"));
                        emit(v, indent + 2, out);
                    }
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                match scalar(item) {
                    Some(s) => out.push_str(&format!("{pad}- {s}\n")),
                    None => {
                        let mut inner = String::new();
                        emit(item, indent + 2, &mut inner);
                        out.push_str(&format!("{pad}- {}", &inner[indent + 2..]));
                    }
                }
            }
        }
        other => out.push_str(&format!("{pad}{other}\n")),
    }
}

/// `files` are `(project-relative path, text)`.
pub fn generate(files: &[(String, String)]) -> Draft {
    let parsed: Vec<JavaFile> = files.iter().map(|(path, text)| spring_text::parse_source(path, text)).collect();
    let mut cx = Cx { dtos: dtos(&parsed), schemas: BTreeMap::new(), notes: Vec::new() };
    let mut ops = Vec::new();
    for ((path, _), file) in files.iter().zip(&parsed) {
        let is_micronaut = file.imports.iter().any(|i| i.path.starts_with("io.micronaut."));
        for class in &file.types {
            if is_micronaut {
                micronaut(&mut cx, &mut ops, class, path);
            } else if class.has(&["RestController", "Controller"]) {
                spring(&mut cx, &mut ops, class, path);
            } else if class.annotation("Path").is_some() {
                jaxrs(&mut cx, &mut ops, class, path);
            }
        }
    }
    ops.sort_by(|a, b| (&a.path, &a.verb, &a.location, &a.class, &a.method).cmp(&(&b.path, &b.verb, &b.location, &b.class, &b.method)));

    let mut paths: BTreeMap<String, Map<String, Value>> = BTreeMap::new();
    let mut ids = BTreeSet::new();
    let mut count = 0;
    for op in ops {
        let item = paths.entry(op.path.clone()).or_default();
        if item.contains_key(&op.verb) {
            cx.notes.push(format!("{}: {}.{}: `{} {}` is already mapped; skipped.", op.location, op.class, op.method, op.verb, op.path));
            continue;
        }
        let mut id = op.method.clone();
        if !ids.insert(id.clone()) {
            id = format!("{}_{}", op.class, op.method);
            let mut n = 1;
            let base = id.clone();
            while !ids.insert(id.clone()) {
                n += 1;
                id = format!("{base}{n}");
            }
        }
        let description = if op.status == 200 { "Successful response (draft; body not inferred)" } else { "Response (draft; body not inferred)" };
        let mut operation = json!({
            "operationId": id,
            "responses": {op.status.to_string(): {"description": description}},
        });
        if !op.parameters.is_empty() {
            operation["parameters"] = Value::Array(op.parameters);
        }
        if let Some(body) = op.body {
            operation["requestBody"] = body;
        }
        item.insert(op.verb, operation);
        count += 1;
    }
    paths.retain(|_, item| !item.is_empty());

    let mut yaml = String::from(
        "# Draft generated from source mappings. Review before use: only paths and parameter names written as plain strings are read;\n\
         # DTO properties are field names (Jackson annotations and naming strategies are not applied); response bodies are not inferred.\n\
         openapi: \"3.0.3\"\ninfo:\n  title: \"Draft API\"\n  version: \"0.1.0\"\n",
    );
    if paths.is_empty() {
        yaml.push_str("paths: {}\n");
    } else {
        yaml.push_str("paths:\n");
        let value = Value::Object(paths.into_iter().map(|(k, v)| (k, Value::Object(v))).collect());
        emit(&value, 2, &mut yaml);
    }
    if !cx.schemas.is_empty() {
        yaml.push_str("components:\n  schemas:\n");
        let value = Value::Object(cx.schemas.into_iter().collect());
        emit(&value, 4, &mut yaml);
    }
    cx.notes.sort();
    cx.notes.dedup();
    Draft { yaml, operations: count, skipped: cx.notes }
}

fn scan(project: &openidea_plugin::Project, root: &str) -> Result<Vec<(String, String)>, Error> {
    let mut paths = Vec::new();
    for pattern in spring_text::SOURCE_GLOBS {
        paths.extend(project.find_files(pattern, 500)?);
    }
    paths.sort();
    paths.dedup();
    paths.retain(|p| crate::canonical_reference_path(p) && !SKIPPED_DIRS.iter().any(|d| p.contains(d)));
    paths.truncate(MAX_FILES);
    let (mut total, mut files) = (0, Vec::new());
    for path in paths {
        let Ok(text) = project.read_file(&path) else { continue };
        if text.len() > MAX_FILE_BYTES {
            continue;
        }
        total += text.len();
        if total > MAX_TOTAL_BYTES {
            break;
        }
        let relative = path.strip_prefix(root.trim_end_matches('/')).unwrap_or(&path).trim_start_matches('/').to_owned();
        files.push((relative, text));
    }
    Ok(files)
}

pub fn run(ctx: &Invocation) -> Result<Option<String>, Error> {
    let project = ctx.project().ok_or("No project is open.")?;
    let root = project.root()?;
    let files = scan(&project, &root)?;
    let draft = generate(&files);
    if draft.operations == 0 {
        host::notify(NotificationLevel::Info, "OpenAPI", &format!("No exportable Spring, JAX-RS or Micronaut mappings found; {} skipped.", draft.skipped.len()));
        return Ok(None);
    }
    let mut target = None;
    for n in 1..=100 {
        let name = if n == 1 { "openapi-draft.yaml".to_owned() } else { format!("openapi-draft-{n}.yaml") };
        let candidate = format!("{}/{name}", root.trim_end_matches('/'));
        if !project.exists(&candidate)? {
            target = Some(candidate);
            break;
        }
    }
    let target = target.ok_or("openapi-draft*.yaml names are all taken; remove one first.")?;
    let name = target.rsplit('/').next().unwrap_or(&target);
    let message = format!("{} operations from {} files; {} mappings or types skipped (listed at the top of the file). Write new file {name}?", draft.operations, files.len(), draft.skipped.len());
    if !host::confirm("Draft OpenAPI spec", &message)? {
        return Ok(None);
    }
    let mut text = String::new();
    for note in draft.skipped.iter().take(MAX_NOTES) {
        text.push_str(&format!("# skipped: {}\n", note.replace(['\r', '\n'], " ")));
    }
    if draft.skipped.len() > MAX_NOTES {
        text.push_str(&format!("# ... and {} more skipped\n", draft.skipped.len() - MAX_NOTES));
    }
    text.push_str(&draft.yaml);
    crate::save_or_copy(&target, &text, "Draft OpenAPI spec")
}

#[cfg(test)]
mod tests {
    use super::*;
    use yaml_rust2::YamlLoader;

    fn draft(files: &[(&str, &str)]) -> Draft {
        let files: Vec<(String, String)> = files.iter().map(|(p, t)| (p.to_string(), t.to_string())).collect();
        let draft = generate(&files);
        assert!(YamlLoader::load_from_str(&draft.yaml).is_ok(), "{}", draft.yaml);
        assert!(crate::spec::parse(&draft.yaml).is_some(), "{}", draft.yaml);
        draft
    }

    const SPRING: &str = r#"package a;
import org.springframework.web.bind.annotation.*;
@RestController
@RequestMapping("/api/users")
class UserController {
    static final String P = "/x";
    @GetMapping("/{id}")
    User get(@PathVariable Long id, @RequestParam(required = false) String q, @RequestParam("page") int page) { return null; }
    @PostMapping
    @ResponseStatus(HttpStatus.CREATED)
    void create(@RequestBody CreateUser body) {}
    @GetMapping(P)
    void constant() {}
    @GetMapping("/files/**")
    void ant() {}
    @GetMapping("/re/{id:[0-9]+}")
    void regex() {}
    @RequestMapping("/any")
    void any() {}
    @RequestMapping(value = "/old", method = RequestMethod.PUT)
    void old(@RequestBody Mystery m) {}
}
record CreateUser(String name, int age, List<String> tags) {}
"#;

    #[test]
    fn spring_mappings_become_operations() {
        let d = draft(&[("a/UserController.java", SPRING)]);
        assert_eq!(d.operations, 3);
        assert!(d.yaml.contains("\"/api/users/{id}\":"), "{}", d.yaml);
        assert!(d.yaml.contains("\"201\":"));
        assert!(d.yaml.contains("\"required\": true"));
        assert!(d.yaml.contains("\"CreateUser\":"));
        assert!(d.yaml.contains("\"age\":"));
        assert!(d.yaml.contains("TODO: the shape of Mystery"));
        let notes = d.skipped.join("\n");
        assert!(notes.contains("computed path"));
        assert!(notes.contains("ant pattern"));
        assert!(notes.contains("regex path variable"));
        assert!(notes.contains("@RequestMapping without exactly one"));
    }

    #[test]
    fn jaxrs_and_micronaut_and_kotlin() {
        let jaxrs = "import jakarta.ws.rs.*;\n@Path(\"/pets\")\nclass Pets {\n @GET @Path(\"{id}\") public Pet one(@PathParam(\"id\") String id, @QueryParam(\"v\") int v) { return null; }\n @POST public void add(Pet p) {}\n}\nclass Pet { private String name; private Long id; }\n";
        let mn = "import io.micronaut.http.annotation.*;\n@Controller(\"/m\")\nclass M {\n @Get(\"/{id}\") String one(String id, @QueryValue Optional<Integer> n) { return null; }\n}\n";
        let kt = "@RestController\nclass K { @GetMapping(\"/k/{id}\") fun one(@PathVariable id: Long, @RequestParam name: String?): String = \"\" }\n";
        let d = draft(&[("Pets.java", jaxrs), ("M.java", mn), ("K.kt", kt)]);
        assert_eq!(d.operations, 4, "{}", d.yaml);
        assert!(d.yaml.contains("\"/pets/{id}\":") && d.yaml.contains("\"/m/{id}\":") && d.yaml.contains("\"/k/{id}\":"));
        assert!(d.yaml.contains("\"name\":"));
    }

    #[test]
    fn operation_ids_are_unique_and_output_is_deterministic() {
        let a = "@RestController\nclass A { @GetMapping(\"/a\") void list() {} }\n";
        let b = "@RestController\nclass B { @GetMapping(\"/b\") void list() {} }\n";
        let first = draft(&[("A.java", a), ("B.java", b)]);
        let second = draft(&[("B.java", b), ("A.java", a)]);
        assert_eq!(first.yaml, second.yaml);
        assert!(first.yaml.contains("\"operationId\": \"list\"") && first.yaml.contains("\"operationId\": \"B_list\""));
    }

    #[test]
    fn strings_are_escaped_and_empty_draft_parses() {
        let d = draft(&[("A.java", "@RestController\nclass A { @GetMapping(\"/q\\\"x\") void m() {} }\n")]);
        assert_eq!(d.operations, 1);
        assert_eq!(draft(&[]).operations, 0);
    }
}
