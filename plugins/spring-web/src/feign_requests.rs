//! Reviewed HTTP examples for explicitly owned literal OpenFeign client source.
//! Client declarations are never published as server endpoints.
use openidea_plugin::{
    Error, Invocation, Range, host, language_features::CodeLens, providers::Document,
};
use spring_text::{
    java::{self, Annotation, JavaFile},
    parse_source, range,
};
use std::collections::{BTreeMap, BTreeSet};
pub const COMMAND: &str = "create-source-feign-http";
#[derive(Clone, Debug, PartialEq, Eq)]
struct Binding {
    kind: &'static str,
    name: String,
    sample: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Request {
    source: String,
    client: String,
    member: String,
    method: String,
    path: String,
    origin: Option<String>,
    range: Range,
    bindings: Vec<Binding>,
    query: BTreeMap<String, Option<String>>,
    headers: BTreeMap<String, (String, String)>,
    consumes: Option<String>,
    produces: Vec<String>,
}
fn import_name<'a>(text: &'a str, i: &'a java::Import) -> (&'a str, bool) {
    let tokens = java::tokens(
        text.get(i.end..)
            .unwrap_or("")
            .split('\n')
            .next()
            .unwrap_or(""),
    );
    if let [a, n, ..] = tokens.as_slice() {
        if a.s == "as" && n.is_ident() {
            return (n.s, true);
        }
    }
    (i.path.rsplit('.').next().unwrap_or(""), false)
}
fn shadowed(f: &JavaFile, text: &str, name: &str) -> bool {
    if f.types.iter().any(|t| t.name == name) {
        return true;
    }
    let tokens = java::tokens(text);
    let mut depth = 0usize;
    for t in &tokens {
        match t.s {
            "<" => depth += 1,
            ">" => depth = depth.saturating_sub(1),
            _ if depth > 0 && t.s == name => return true,
            _ => {}
        }
    }
    tokens
        .windows(2)
        .any(|p| p[0].s == "typealias" && p[1].s == name)
}
fn written(f: &JavaFile, text: &str, value: &str, expected: &str) -> bool {
    let short = expected.rsplit('.').next().unwrap();
    if value == expected {
        return !shadowed(f, text, "org")
            && !f.imports.iter().any(|i| import_name(text, i).0 == "org");
    }
    if value != short || shadowed(f, text, short) {
        return false;
    }
    let imports = f
        .imports
        .iter()
        .filter(|i| import_name(text, i).0 == short)
        .collect::<Vec<_>>();
    if let [i] = imports.as_slice() {
        return i.path == expected && !i.is_static && !import_name(text, i).1;
    }
    imports.is_empty()
        && f.imports.iter().filter(|i| i.path.ends_with(".*")).count() == 1
        && f.imports.iter().any(|i| {
            !i.is_static && i.path == format!("{}.*", expected.rsplit_once('.').unwrap().0)
        })
}
fn owned(f: &JavaFile, text: &str, a: &Annotation, expected: &str) -> bool {
    let Some(raw) = text.get(a.at + 1..a.name_at + a.name.len()) else {
        return false;
    };
    written(
        f,
        text,
        &java::tokens(raw).iter().map(|t| t.s).collect::<String>(),
        expected,
    )
}
fn values(a: &Annotation, field: &str, text: &str) -> Option<Vec<String>> {
    if a.args.iter().filter(|a| a.name == field).count() > 1 {
        return None;
    }
    let Some(arg) = a.arg(field) else {
        return Some(vec![]);
    };
    let tokens = java::tokens(&arg.text);
    if tokens.iter().any(|t| t.s.starts_with("\"\"\""))
        || tokens.iter().filter(|t| t.s.starts_with('"')).count() != arg.strings.len()
        || tokens
            .iter()
            .any(|t| !t.s.starts_with('"') && !matches!(t.s, "{" | "}" | "[" | "]" | ","))
        || arg.text.contains(['\\', '$'])
    {
        return None;
    }
    if arg.strings.iter().any(|s| {
        text.get(s.at..s.end) != Some(s.value.as_str())
            || s.value.chars().any(char::is_control)
            || s.value.len() > 4096
    }) {
        return None;
    }
    let mut atoms = tokens.as_slice();
    if atoms.first().is_some_and(|t| matches!(t.s, "{" | "[")) {
        let end = if atoms[0].s == "{" { "}" } else { "]" };
        if atoms.last().is_none_or(|t| t.s != end) {
            return None;
        }
        atoms = &atoms[1..atoms.len() - 1];
    } else if atoms.len() != 1 {
        return None;
    }
    for (i, t) in atoms.iter().enumerate() {
        if i % 2 == 0 && !t.s.starts_with('"') || i % 2 == 1 && t.s != "," {
            return None;
        }
    }
    Some(arg.strings.iter().map(|s| s.value.clone()).collect())
}
fn one(a: &Annotation, field: &str, text: &str) -> Option<Option<String>> {
    let values = values(a, field, text)?;
    if values.len() > 1 {
        return None;
    }
    Some(values.into_iter().next())
}
fn alias(a: &Annotation, left: &str, right: &str, text: &str) -> Option<Option<String>> {
    let l = one(a, left, text)?;
    let r = one(a, right, text)?;
    if l.is_some() && r.is_some() && l != r {
        return None;
    }
    Some(l.or(r))
}
fn token(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}
fn identifier(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
}
fn media(s: &str, wildcard: bool) -> bool {
    if s.chars().any(char::is_control) {
        return false;
    }
    let mut parts = s.split(';');
    let Some((a, b)) = parts.next().unwrap_or("").trim().split_once('/') else {
        return false;
    };
    if !token(a)
        || !token(b)
        || a.contains('*') && a != "*"
        || b.contains('*') && b != "*" && !(b.starts_with("*+") && !b[2..].contains('*'))
        || a == "*" && b != "*"
        || !wildcard && (a.contains('*') || b.contains('*'))
    {
        return false;
    }
    let mut names = BTreeSet::new();
    parts.all(|p| {
        p.trim().split_once('=').is_some_and(|(n, v)| {
            token(n.trim()) && token(v.trim()) && names.insert(n.trim().to_ascii_lowercase())
        })
    })
}
fn path(s: &str, variables: bool) -> bool {
    s.starts_with('/')
        && !s.contains("//")
        && s.len() <= 2048
        && !s.contains(['?', '#', '\\', '*', '$', '"'])
        && !s.chars().any(|c| c.is_whitespace() || c.is_control())
        && (variables || !s.contains(['{', '}']))
}
fn names(path: &str) -> Option<BTreeSet<String>> {
    let mut rest = path;
    let mut names = BTreeSet::new();
    while let Some((head, tail)) = rest.split_once('{') {
        if head.contains('}') {
            return None;
        }
        let (name, after) = tail.split_once('}')?;
        if !identifier(name) {
            return None;
        }
        names.insert(name.into());
        rest = after;
    }
    if rest.contains('}') {
        return None;
    }
    Some(names)
}
fn origin(s: &str) -> bool {
    let Some(authority) = s
        .strip_prefix("http://")
        .or_else(|| s.strip_prefix("https://"))
    else {
        return false;
    };
    let port = |p: &str| {
        !p.is_empty()
            && p.bytes().all(|b| b.is_ascii_digit())
            && p.parse::<u16>().is_ok_and(|p| p > 0)
    };
    if authority.starts_with('[') {
        let Some((address, tail)) = authority[1..].split_once(']') else {
            return false;
        };
        return address.parse::<std::net::Ipv6Addr>().is_ok()
            && (tail.is_empty() || tail.strip_prefix(':').is_some_and(port));
    }
    let (hostname, valid_port) = authority
        .split_once(':')
        .map_or((authority, true), |(h, p)| (h, port(p)));
    valid_port
        && !hostname.is_empty()
        && hostname.len() <= 253
        && hostname.trim_end_matches('.').split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label.as_bytes()[0].is_ascii_alphanumeric()
                && label.as_bytes()[label.len() - 1].is_ascii_alphanumeric()
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}
fn target(value: Option<String>) -> Option<(Option<String>, String)> {
    let Some(value) = value.filter(|s| !s.is_empty()) else {
        return Some((None, String::new()));
    };
    let value = if value.starts_with("http://") || value.starts_with("https://") {
        value
    } else {
        format!("http://{value}")
    };
    let scheme = value.find("://")? + 3;
    let end = value[scheme..]
        .find('/')
        .map_or(value.len(), |i| scheme + i);
    let base = &value[..end];
    let prefix = &value[end..];
    if !origin(base) || !prefix.is_empty() && !path(prefix, false) {
        return None;
    }
    Some((Some(base.into()), prefix.into()))
}
fn verb(a: &Annotation, f: &JavaFile, text: &str) -> Option<String> {
    let fixed = match a.name.as_str() {
        "GetMapping" => Some("GET"),
        "PostMapping" => Some("POST"),
        "PutMapping" => Some("PUT"),
        "PatchMapping" => Some("PATCH"),
        "DeleteMapping" => Some("DELETE"),
        _ => None,
    };
    if let Some(fixed) = fixed {
        if a.arg("method").is_some() {
            return None;
        }
        return Some(fixed.into());
    }
    let Some(arg) = a.arg("method") else {
        return Some("GET".into());
    };
    let raw = java::tokens(&arg.text)
        .iter()
        .filter(|t| !matches!(t.s, "{" | "}" | "[" | "]"))
        .map(|t| t.s)
        .collect::<String>();
    if raw.is_empty() {
        return Some("GET".into());
    }
    let (owner, method) = raw.rsplit_once('.')?;
    if !written(
        f,
        text,
        owner,
        "org.springframework.web.bind.annotation.RequestMethod",
    ) || ![
        "GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS", "TRACE",
    ]
    .contains(&method)
    {
        return None;
    }
    Some(method.into())
}
fn sample(param: &java::Param) -> String {
    match param.ty.text.as_str() {
        "byte" | "short" | "int" | "long" | "float" | "double" | "kotlin.Int" | "kotlin.Long" => {
            "0"
        }
        "boolean" | "kotlin.Boolean" => "false",
        _ => "REVIEW",
    }
    .into()
}
fn requests(doc: &Document) -> Vec<Request> {
    if !((doc.language == "java" && doc.path.ends_with(".java"))
        || (doc.language == "kotlin" && doc.path.ends_with(".kt")))
        || doc.text.len() > 128 << 10
        || doc.text.contains("\\u")
    {
        return vec![];
    }
    let tokens = java::tokens(&doc.text);
    if tokens.len() > 16384 {
        return vec![];
    }
    let mut stack = vec![];
    for t in &tokens {
        match t.s {
            "(" => stack.push(")"),
            "{" => stack.push("}"),
            "[" => stack.push("]"),
            ")" | "}" | "]" => {
                if stack.pop() != Some(t.s) {
                    return vec![];
                }
            }
            _ => {}
        }
    }
    if !stack.is_empty() {
        return vec![];
    }
    let f = parse_source(&doc.path, &doc.text);
    let mut out = vec![];
    for (i, ty) in f.types.iter().enumerate() {
        if let Some(found) = exchange_requests(&f, doc, i) {
            out.extend(found);
            if out.len() >= 128 {
                out.truncate(128);
                return out;
            }
            continue;
        }
        if ty.kind != "interface"
            || ty.supers().next().is_some()
            || ty.annotations.len() != 1
            || ty.annotations.iter().any(|a| a.name != "FeignClient")
        {
            continue;
        }
        let Some(feign) = ty.annotation("FeignClient").filter(|a| {
            owned(
                &f,
                &doc.text,
                a,
                "org.springframework.cloud.openfeign.FeignClient",
            )
        }) else {
            continue;
        };
        if feign.args.iter().any(|a| {
            feign
                .args
                .iter()
                .filter(|other| other.name == a.name)
                .count()
                > 1
        }) || feign.arg("configuration").is_some()
            || feign
                .args
                .iter()
                .any(|a| !["name", "value", "url", "path", "contextId"].contains(&a.name.as_str()))
        {
            continue;
        }
        let Some(client) = alias(feign, "name", "value", &doc.text)
            .flatten()
            .filter(|s| identifier(s))
        else {
            continue;
        };
        let Some((base, prefix)) = one(feign, "url", &doc.text).and_then(target) else {
            continue;
        };
        let Some(client_path) = one(feign, "path", &doc.text)
            .map(|s| s.unwrap_or_default())
            .filter(|s| s.is_empty() || path(s, false))
        else {
            continue;
        };
        if feign.arg("contextId").is_some() && one(feign, "contextId", &doc.text).is_none() {
            continue;
        }
        let class = f.qualified(i);
        for method in ty.methods() {
            if method.body.is_some()
                || method
                    .modifiers
                    .iter()
                    .any(|m| matches!(m.as_str(), "static" | "default"))
                || method.params.len() > 32
                || method.annotations.len() != 1
            {
                continue;
            }
            let a = &method.annotations[0];
            if ![
                "RequestMapping",
                "GetMapping",
                "PostMapping",
                "PutMapping",
                "PatchMapping",
                "DeleteMapping",
            ]
            .contains(&a.name.as_str())
                || !owned(
                    &f,
                    &doc.text,
                    a,
                    &format!("org.springframework.web.bind.annotation.{}", a.name),
                )
            {
                continue;
            }
            if a.args
                .iter()
                .any(|arg| a.args.iter().filter(|other| other.name == arg.name).count() > 1)
                || a.args.iter().any(|a| {
                    ![
                        "name", "path", "value", "method", "headers", "params", "consumes",
                        "produces",
                    ]
                    .contains(&a.name.as_str())
                })
            {
                continue;
            }
            let Some(method_verb) = verb(a, &f, &doc.text) else {
                continue;
            };
            let Some(mapping) = alias(a, "value", "path", &doc.text).map(|s| s.unwrap_or_default())
            else {
                continue;
            };
            let mapping = if mapping.is_empty() {
                String::new()
            } else if mapping.starts_with('/') {
                mapping
            } else {
                format!("/{mapping}")
            };
            let request_path = java::join_paths(&java::join_paths(&prefix, &client_path), &mapping);
            if !path(&request_path, true) {
                continue;
            }
            let Some(path_names) = names(&request_path) else {
                continue;
            };
            let Some(produces) = values(a, "produces", &doc.text)
                .filter(|ss| ss.iter().all(|s| s.is_empty() || media(s, true)))
            else {
                continue;
            };
            let Some(consumes) = values(a, "consumes", &doc.text)
                .filter(|ss| ss.iter().all(|s| s.is_empty() || media(s, true)))
                .map(|ss| ss.into_iter().next().filter(|s| !s.is_empty()))
            else {
                continue;
            };
            let mut query = BTreeMap::new();
            let mut headers = BTreeMap::new();
            let mut valid = true;
            for field in ["params", "headers"] {
                let Some(values) = values(a, field, &doc.text) else {
                    valid = false;
                    break;
                };
                for value in values {
                    if value.starts_with('!') || value.contains("!=") {
                        valid = false;
                        break;
                    }
                    let (name, value) =
                        value
                            .split_once('=')
                            .map_or((value.as_str(), None), |(n, v)| {
                                (
                                    n,
                                    Some(if field == "headers" {
                                        v.trim().to_owned()
                                    } else {
                                        v.to_owned()
                                    }),
                                )
                            });
                    if !identifier(name) || field == "headers" && (!token(name) || value.is_none())
                    {
                        valid = false;
                        break;
                    }
                    if field == "params" {
                        if query.insert(name.into(), value).is_some() {
                            valid = false;
                        }
                    } else {
                        let key = name.to_ascii_lowercase();
                        if ["host", "content-length", "connection", "transfer-encoding"]
                            .contains(&key.as_str())
                            || headers.insert(key, (name.into(), value.unwrap())).is_some()
                        {
                            valid = false;
                        }
                    }
                }
            }
            if !valid
                || headers
                    .get("content-type")
                    .is_some_and(|(_, v)| !media(v, false))
                || headers
                    .get("accept")
                    .is_some_and(|(_, v)| v.split(',').any(|part| !media(part.trim(), true)))
                || consumes.is_some() && headers.contains_key("content-type")
                || !produces.is_empty() && headers.contains_key("accept")
            {
                continue;
            }
            let Some(bindings) = bind(&f, &doc.text, method, &query, &headers, &path_names) else {
                continue;
            };
            if !valid {
                continue;
            }
            out.push(Request {
                source: doc.path.clone(),
                client: format!("{client} ({class})"),
                member: method.name.clone(),
                method: method_verb,
                path: request_path,
                origin: base.clone(),
                range: range(&doc.text, method.at, method.at + method.name.len()),
                bindings,
                query,
                headers,
                consumes,
                produces: produces.into_iter().filter(|s| !s.is_empty()).collect(),
            });
            if out.len() == 128 {
                return out;
            }
        }
    }
    out
}
fn bind(
    f: &JavaFile,
    text: &str,
    method: &java::Member,
    query: &BTreeMap<String, Option<String>>,
    headers: &BTreeMap<String, (String, String)>,
    path_names: &BTreeSet<String>,
) -> Option<Vec<Binding>> {
    let mut bindings = vec![];
    let mut bound = BTreeSet::new();
    let mut actual_paths = BTreeSet::new();
    let mut bodies = 0;
    for param in &method.params {
        if param.annotations.len() != 1
            || param.ty.array
            || !param.ty.args.is_empty()
            || [
                "Map",
                "List",
                "Set",
                "Collection",
                "MultiValueMap",
                "Optional",
            ]
            .contains(&param.ty.name.as_str())
        {
            return None;
        }
        let binding = &param.annotations[0];
        let kind = match binding.name.as_str() {
            "PathVariable" => "path",
            "RequestParam" => "query",
            "RequestHeader" => "header",
            "RequestBody" => "body",
            _ => {
                return None;
            }
        };
        if !owned(
            f,
            text,
            binding,
            &format!("org.springframework.web.bind.annotation.{}", binding.name),
        ) {
            return None;
        }
        if binding.args.iter().any(|a| {
            binding
                .args
                .iter()
                .filter(|other| other.name == a.name)
                .count()
                > 1
        }) || binding.args.iter().any(|a| {
            if kind == "body" {
                a.name != "required"
            } else if kind == "path" {
                !["name", "value", "required"].contains(&a.name.as_str())
            } else {
                !["name", "value", "required", "defaultValue"].contains(&a.name.as_str())
            }
        }) {
            return None;
        }
        let Some(name) = alias(binding, "name", "value", text).map(|s| {
            s.filter(|s| !s.is_empty())
                .unwrap_or_else(|| param.name.clone())
        }) else {
            return None;
        };
        if kind != "body" && !identifier(&name)
            || kind == "header"
                && (!token(&name)
                    || [
                        "host",
                        "content-length",
                        "connection",
                        "transfer-encoding",
                        "content-type",
                        "accept",
                    ]
                    .contains(&name.to_ascii_lowercase().as_str()))
        {
            return None;
        }
        if !bound.insert((
            kind,
            if kind == "header" {
                name.to_ascii_lowercase()
            } else {
                name.clone()
            },
        )) {
            return None;
        }
        if kind == "path" {
            actual_paths.insert(name.clone());
        }
        if kind == "body" {
            bodies += 1;
        }
        if kind == "query" && query.contains_key(&name)
            || kind == "header" && headers.contains_key(&name.to_ascii_lowercase())
        {
            return None;
        }
        bindings.push(Binding {
            kind,
            name,
            sample: sample(param),
        });
    }
    (actual_paths == *path_names && bodies <= 1).then_some(bindings)
}
const EXCHANGE: &str = "org.springframework.web.service.annotation";
struct Exchange {
    verb: Option<String>,
    url: String,
    content_type: Option<String>,
    accept: Vec<String>,
}
fn exchange_fields(f: &JavaFile, text: &str, a: &Annotation, fixed: Option<&str>) -> Option<Exchange> {
    let method_ok = fixed.is_none() || a.arg("method").is_none();
    if !method_ok
        || !owned(f, text, a, &format!("{EXCHANGE}.{}", a.name))
        || a.args.iter().any(|x| !["value", "url", "method", "contentType", "accept"].contains(&x.name.as_str()))
    {
        return None;
    }
    let verb = match fixed {
        Some(v) => Some(v.to_owned()),
        None => one(a, "method", text)?.filter(|v| !v.is_empty()),
    };
    let accept = values(a, "accept", text).filter(|v| v.iter().all(|s| media(s, true)))?;
    let content_type = one(a, "contentType", text)?.filter(|c| !c.is_empty());
    if content_type.as_deref().is_some_and(|c| !media(c, false)) {
        return None;
    }
    Some(Exchange { verb, url: alias(a, "value", "url", text)?.unwrap_or_default(), content_type, accept })
}
/// Spring Framework 6 `@HttpExchange` interface clients; `None` when the type is not one.
fn exchange_requests(f: &JavaFile, doc: &Document, i: usize) -> Option<Vec<Request>> {
    let ty = &f.types[i];
    let fixed = |n: &str| {
        Some(match n {
            "GetExchange" => "GET",
            "PostExchange" => "POST",
            "PutExchange" => "PUT",
            "PatchExchange" => "PATCH",
            "DeleteExchange" => "DELETE",
            _ => return None,
        })
    };
    if ty.kind != "interface"
        || ty.supers().next().is_some()
        || ty.annotations.len() > 1
        || !ty.annotations.iter().all(|a| a.name == "HttpExchange")
        || !ty.methods().any(|m| m.annotations.iter().any(|a| a.name == "HttpExchange" || fixed(&a.name).is_some()))
    {
        return None;
    }
    let mut out = vec![];
    let class = match ty.annotations.first() {
        Some(a) => match exchange_fields(f, &doc.text, a, None) {
            Some(c) => c,
            None => return Some(out),
        },
        None => Exchange { verb: None, url: String::new(), content_type: None, accept: vec![] },
    };
    let (base, prefix) = match class.url.split_once("://") {
        Some((scheme, rest)) => {
            let end = rest.find('/').unwrap_or(rest.len());
            let origin_text = format!("{scheme}://{}", &rest[..end]);
            if !matches!(scheme, "http" | "https") || !origin(&origin_text) {
                return Some(out);
            }
            (Some(origin_text), rest[end..].to_owned())
        }
        None => (None, class.url.clone()),
    };
    let name = ty.name.clone();
    let qualified = f.qualified(i);
    for method in ty.methods() {
        if method.body.is_some()
            || method.modifiers.iter().any(|m| matches!(m.as_str(), "static" | "default"))
            || method.params.len() > 32
            || method.annotations.len() != 1
        {
            continue;
        }
        let a = &method.annotations[0];
        let kind = fixed(&a.name);
        if kind.is_none() && a.name != "HttpExchange" {
            continue;
        }
        let Some(m) = exchange_fields(f, &doc.text, a, kind) else { continue };
        let Some(verb) = m.verb.or_else(|| class.verb.clone()) else { continue };
        let verb = verb.to_ascii_uppercase();
        if !["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"].contains(&verb.as_str()) || m.url.contains("://") {
            continue;
        }
        let request_path = java::join_paths(&prefix, &m.url);
        let Some(path_names) = path(&request_path, true).then(|| names(&request_path)).flatten() else { continue };
        let consumes = m.content_type.or_else(|| class.content_type.clone());
        let produces = if m.accept.is_empty() { class.accept.clone() } else { m.accept };
        let Some(bindings) = bind(f, &doc.text, method, &BTreeMap::new(), &BTreeMap::new(), &path_names) else { continue };
        let form = consumes.as_deref().is_some_and(|c| c.eq_ignore_ascii_case("application/x-www-form-urlencoded"));
        if form && bindings.iter().any(|b| b.kind == "query") {
            continue;
        }
        out.push(Request {
            source: doc.path.clone(),
            client: format!("{name} ({qualified})"),
            member: method.name.clone(),
            method: verb,
            path: request_path,
            origin: base.clone(),
            range: range(&doc.text, method.at, method.at + method.name.len()),
            bindings,
            query: BTreeMap::new(),
            headers: BTreeMap::new(),
            consumes,
            produces: produces.into_iter().filter(|s| !s.is_empty()).collect(),
        });
    }
    Some(out)
}
fn digest(text: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hash);
    format!("{:016x}", hash.finish())
}
fn argument(doc: &Document, request: &Request) -> String {
    serde_json::json!({"source":doc.path,"source_hash":digest(&doc.text),"version":doc.version,"client":request.client,"member":request.member,"method":request.method,"path":request.path,"line":request.range.start.line,"character":request.range.start.character}).to_string()
}
pub fn lenses(doc: &Document) -> Vec<CodeLens> {
    requests(doc)
        .iter()
        .map(|r| CodeLens {
            range: r.range,
            title: format!("Feign request: {} {}", r.method, r.path),
            command: COMMAND.into(),
            argument: Some(argument(doc, r)),
        })
        .collect()
}
fn current(
    ctx: &Invocation,
    value: &serde_json::Value,
) -> Result<(Document, Request, String), Error> {
    let editor = ctx.editor().ok_or("Open a Feign client source file.")?;
    let snapshot = editor.document()?;
    let doc = Document {
        path: editor.path(),
        language: editor.language(),
        version: snapshot.version,
        text: snapshot.text,
    };
    let project = host::project_of(&doc.path).ok_or(Error::Stale)?;
    let root = project.root()?;
    if root.is_empty()
        || root == "/"
        || doc.path.contains('\\')
        || doc.path.chars().any(char::is_control)
        || doc.path.split('/').any(|s| matches!(s, "." | ".."))
        || ctx.project().ok_or(Error::Stale)?.root()? != root
    {
        return Err(Error::Stale);
    }
    let request = requests(&doc)
        .into_iter()
        .find(|r| {
            serde_json::from_str::<serde_json::Value>(&argument(&doc, r))
                .ok()
                .as_ref()
                == Some(value)
        })
        .ok_or(Error::Stale)?;
    Ok((doc, request, root))
}
fn percent(value: &str) -> String {
    value
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}
fn media_matches(range: &str, value: &str) -> bool {
    if !media(value, false) {
        return false;
    }
    let mut ranges = range.split(';');
    let mut values = value.split(';');
    let rb = ranges.next().unwrap_or("").trim().to_ascii_lowercase();
    let vb = values.next().unwrap_or("").trim().to_ascii_lowercase();
    let Some((rt, rs)) = rb.split_once('/') else {
        return false;
    };
    let Some((vt, vs)) = vb.split_once('/') else {
        return false;
    };
    if !(rt == "*" || rt == vt)
        || !(rs == "*"
            || rs == vs
            || rs
                .strip_prefix("*+")
                .is_some_and(|suffix| vs.ends_with(&format!("+{suffix}"))))
    {
        return false;
    }
    let parameters = values
        .filter_map(|p| {
            p.trim()
                .split_once('=')
                .map(|(n, v)| (n.trim().to_ascii_lowercase(), v.trim().to_owned()))
        })
        .collect::<BTreeMap<_, _>>();
    ranges.all(|p| {
        p.trim().split_once('=').is_some_and(|(n, v)| {
            parameters
                .get(&n.trim().to_ascii_lowercase())
                .is_some_and(|actual| actual == v.trim())
        })
    })
}
fn filename(request: &Request) -> String {
    let slug = |s: &str| {
        s.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .collect::<String>()
            .split('-')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("-")
    };
    format!(
        "http/feign-{}-{}-{}-{}-{}.http",
        slug(&request.client).chars().take(64).collect::<String>(),
        slug(&request.member),
        request.method.to_ascii_lowercase(),
        slug(&request.path).chars().take(64).collect::<String>(),
        digest(&format!(
            "{}:{}:{}:{}",
            request.source, request.member, request.method, request.path
        ))
    )
}
pub fn run(ctx: &Invocation, arg: Option<&str>) -> Result<(), Error> {
    let argument = if let Some(arg) = arg {
        arg.to_owned()
    } else {
        let editor = ctx.editor().ok_or("Open a Feign client source file.")?;
        let snapshot = editor.document()?;
        let doc = Document {
            path: editor.path(),
            language: editor.language(),
            version: snapshot.version,
            text: snapshot.text,
        };
        let choices = lenses(&doc);
        if choices.is_empty() {
            return Err(Error::Failed(
                "No Feign client request found in this file. Put the cursor in an @FeignClient interface with a mapped method.".into(),
            ));
        }
        if choices.len() == 1 {
            choices[0].argument.clone().unwrap()
        } else {
            let items = choices
                .iter()
                .enumerate()
                .map(|(i, l)| openidea_plugin::PickItem::new(&i.to_string(), &l.title))
                .collect::<Vec<_>>();
            let Some(choice) = host::pick("Choose Feign client request", &items)? else {
                return Ok(());
            };
            choices
                .get(choice.parse::<usize>().map_err(|_| Error::Stale)?)
                .and_then(|l| l.argument.clone())
                .ok_or(Error::Stale)?
        }
    };
    let value = serde_json::from_str::<serde_json::Value>(&argument).map_err(|_| Error::Stale)?;
    let (doc, request, root) = current(ctx, &value)?;
    let Some(base) = host::input(
        "Feign target URL (scheme, host and port)",
        Some(request.origin.as_deref().unwrap_or(crate::DEFAULT_BASE_URL)),
    )?
    else {
        return Ok(());
    };
    let base = base.trim().trim_end_matches('/');
    if !origin(base) {
        return Err(Error::Invalid(
            "Enter an HTTP(S) origin without path, credentials, query or fragment.".into(),
        ));
    }
    let mut request_path = request.path.clone();
    let mut query = request.query.clone();
    let mut headers = request.headers.clone();
    let mut body = None;
    for binding in &request.bindings {
        let title = format!("Sample {} value for {}", binding.kind, binding.name);
        let default = if binding.kind == "body" {
            "{}"
        } else {
            &binding.sample
        };
        let Some(value) = host::input(&title, Some(default))? else {
            return Ok(());
        };
        if value.len() > 8192
            || value
                .chars()
                .any(|c| c.is_control() && !(binding.kind == "body" && matches!(c, '\n' | '\t')))
            || value.lines().any(|l| l.starts_with("###"))
        {
            return Err(Error::Invalid(
                "Sample contains unsupported control characters or HTTP request separators.".into(),
            ));
        }
        match binding.kind {
            "path" => {
                request_path =
                    request_path.replace(&format!("{{{}}}", binding.name), &percent(&value))
            }
            "query" => {
                query.insert(binding.name.clone(), Some(value));
            }
            "header" => {
                headers.insert(
                    binding.name.to_ascii_lowercase(),
                    (binding.name.clone(), value),
                );
            }
            "body" => body = Some(value),
            _ => return Err(Error::Stale),
        }
    }
    let mut content_type = request
        .consumes
        .clone()
        .or_else(|| headers.get("content-type").map(|(_, v)| v.clone()));
    if body.is_some() && content_type.is_none()
        || content_type.as_ref().is_some_and(|v| v.contains('*'))
    {
        let constraint = content_type.clone();
        let Some(chosen) = host::input("Reviewed request Content-Type", Some("application/json"))?
        else {
            return Ok(());
        };
        if !media(&chosen, false)
            || constraint
                .as_ref()
                .is_some_and(|range| !media_matches(range, &chosen))
        {
            return Err(Error::Invalid(
                "Choose a concrete Content-Type matching the declared media range.".into(),
            ));
        }
        content_type = Some(chosen);
    }
    if let Some(content_type) = content_type {
        if body.is_some()
            && content_type
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
                .split_once('/')
                .is_some_and(|(_, subtype)| subtype == "json" || subtype.ends_with("+json"))
            && serde_json::from_str::<serde_json::Value>(body.as_ref().unwrap()).is_err()
        {
            return Err(Error::Invalid(
                "The sample body is not valid JSON.".into(),
            ));
        }
        headers.insert("content-type".into(), ("Content-Type".into(), content_type));
    }
    if !request.produces.is_empty() {
        headers.insert(
            "accept".into(),
            ("Accept".into(), request.produces.join(", ")),
        );
    }
    let query = query
        .into_iter()
        .map(|(k, v)| {
            v.map_or_else(
                || percent(&k),
                |v| format!("{}={}", percent(&k), percent(&v)),
            )
        })
        .collect::<Vec<_>>()
        .join("&");
    let mut content = format!(
        "### Feign {}.{}\n# Source: {}\n# Reviewed source example only; discovery, configuration, interceptors and encoders are not resolved.\n{} {base}{request_path}{}\n",
        request.client,
        request.member,
        request.source,
        request.method,
        if query.is_empty() {
            String::new()
        } else {
            format!("?{query}")
        }
    );
    for (_, (name, value)) in headers {
        if value.chars().any(char::is_control) {
            return Err(Error::Invalid(
                "Header sample contains control characters.".into(),
            ));
        }
        content.push_str(&format!("{name}: {value}\n"));
    }
    if let Some(body) = body {
        content.push('\n');
        content.push_str(&body);
        content.push('\n');
    }
    if content.len() > 16384 {
        return Err(Error::Invalid(
            "The reviewed request example is too large.".into(),
        ));
    }
    let (fresh, fresh_request, fresh_root) = current(ctx, &value)?;
    if fresh.text != doc.text || fresh_request != request || fresh_root != root {
        return Err(Error::Stale);
    }
    if !ui_kit::prompts::confirm_exact("Create reviewed Feign HTTP request?", &content)? {
        return Ok(());
    }
    let (fresh, fresh_request, fresh_root) = current(ctx, &value)?;
    if fresh.text != doc.text || fresh_request != request || fresh_root != root {
        return Err(Error::Stale);
    }
    let project = ctx.project().ok_or(Error::Stale)?;
    let path = format!("{root}/{}", filename(&request));
    if project.exists(&path)? {
        return host::open_location(&path, 0, 0);
    }
    match project.write_file(&path, &content) {
        Ok(()) => host::open_location(&path, 0, 0),
        Err(Error::Denied(_)) => crate::copy_after_write_denied(&content),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{Plugin, mock::MockHost};
    const SOURCE: &str = "import org.springframework.cloud.openfeign.FeignClient; import org.springframework.web.bind.annotation.*; @FeignClient(name=\"stores\",url=\"https://api.example/v1\",path=\"/api\") interface Stores { @PostMapping(value=\"/items/{id}\",params={\"mode=fast\",\"flag\"},headers=\"X-Tenant=blue\",consumes=\"application/json\",produces={\"application/json\",\"application/problem+json\"}) String save(@PathVariable(\"id\") long id,@RequestParam(\"q\") String query,@RequestHeader(\"X-Trace\") String trace,@RequestBody String body); }";
    fn doc(text: &str) -> Document {
        Document {
            path: "/project/Stores.java".into(),
            language: "java".into(),
            text: text.into(),
            version: 1,
        }
    }
    fn review(h: &MockHost) {
        for value in [
            "https://reviewed.example:8443",
            "a/b",
            "fast & safe",
            "trace-1",
            "{\"name\":\"review\"}",
            "ok",
        ] {
            h.answer_next_prompt(Some(&[value]));
        }
    }
    #[test]
    fn http_exchange_interface_requests_join_class_prefix_and_stay_literal() {
        let src = "import org.springframework.web.service.annotation.HttpExchange; import org.springframework.web.service.annotation.GetExchange; import org.springframework.web.service.annotation.PatchExchange; import org.springframework.web.service.annotation.PostExchange; import org.springframework.web.bind.annotation.*; @HttpExchange(url=\"https://api.example/repos/{owner}\", accept=\"application/json\") interface Repos { @GetExchange(\"/{repo}\") String get(@PathVariable String owner, @PathVariable(\"repo\") String r, @RequestParam int page, @RequestHeader(\"X-T\") String t); @PatchExchange(url=\"/x\", contentType=\"application/json\") void patch(@PathVariable String owner, @RequestBody String b); @PostExchange(\"/f\", contentType=\"application/x-www-form-urlencoded\") void form(@PathVariable String owner, @RequestParam String q); @GetExchange(URL) void dyn(@PathVariable String owner); @GetExchange(\"/m\") void missing(); }";
        let model = requests(&doc(src));
        let got: Vec<_> = model.iter().map(|r| (r.member.as_str(), r.method.as_str(), r.path.as_str())).collect();
        assert_eq!(got, [("get", "GET", "/repos/{owner}/{repo}"), ("patch", "PATCH", "/repos/{owner}/x")]);
        assert_eq!(model[0].origin.as_deref(), Some("https://api.example"));
        assert_eq!(model[0].produces, ["application/json"]);
        assert_eq!(model[1].consumes.as_deref(), Some("application/json"));
        assert_eq!(model[0].bindings.len(), 4);
        let foreign = src.replace("org.springframework.web.service.annotation.", "com.acme.");
        assert!(requests(&doc(&foreign)).is_empty());
    }
    #[test]
    fn literal_client_request_lens_reviews_samples_and_never_publishes_a_server_route() {
        let h = MockHost::install().with_capabilities(&[
            "project.read",
            "project.write",
            "editor.read",
            "clipboard",
        ]);
        h.open_editor("/project/Stores.java", "java", SOURCE, &[]);
        let d = doc(SOURCE);
        let model = requests(&d);
        assert_eq!(model.len(), 1);
        assert_eq!(model[0].path, "/v1/api/items/{id}");
        assert!(
            crate::endpoints::file_info(&d.path, &d.text)
                .is_none_or(|info| info.endpoints.is_empty())
        );
        let lens = crate::source_lenses::feign_lenses(&d).remove(0);
        assert_eq!(lens.command, COMMAND);
        review(&h);
        crate::window::Endpoints::default()
            .run_command(&lens.command, &h.invocation(), lens.argument.as_deref())
            .unwrap();
        let text = h
            .invocation()
            .project()
            .unwrap()
            .read_file(&filename(&model[0]))
            .unwrap();
        assert!(text.contains("POST https://reviewed.example:8443/v1/api/items/a%2Fb?flag&mode=fast&q=fast%20%26%20safe\n"));
        assert!(text.contains("# Source: /project/Stores.java\n"));
        let mut other = model[0].clone();
        other.source = "/project/other-module/Stores.java".into();
        assert_ne!(filename(&model[0]), filename(&other));
        assert!(text.contains("X-Trace: trace-1\n"));
        assert!(text.contains("X-Tenant: blue\n"));
        assert!(text.contains("Content-Type: application/json\n"));
        assert!(text.contains("Accept: application/json, application/problem+json\n"));
        assert!(text.ends_with("\n{\"name\":\"review\"}\n"));
        assert!(h.requests().is_empty() && h.process_runs().is_empty());
    }
    #[test]
    fn existing_file_is_opened_unchanged_and_denied_write_uses_reviewed_clipboard_fallback() {
        let h = MockHost::install();
        h.open_editor("/project/Stores.java", "java", SOURCE, &[]);
        let model = requests(&doc(SOURCE)).remove(0);
        let path = filename(&model);
        h.add_file(&path, "KEEP");
        review(&h);
        run(&h.invocation(), lenses(&doc(SOURCE))[0].argument.as_deref()).unwrap();
        assert_eq!(
            h.invocation().project().unwrap().read_file(&path).unwrap(),
            "KEEP"
        );
        assert!(h.clipboard().is_none());
        let h =
            MockHost::install().with_capabilities(&["project.read", "editor.read", "clipboard"]);
        h.open_editor("/project/Stores.java", "java", SOURCE, &[]);
        review(&h);
        run(&h.invocation(), lenses(&doc(SOURCE))[0].argument.as_deref()).unwrap();
        assert!(
            h.clipboard()
                .unwrap()
                .contains("POST https://reviewed.example:8443")
        );
        assert!(!h.invocation().project().unwrap().exists(&path).unwrap());
        assert!(h.requests().is_empty() && h.process_runs().is_empty());
    }
    #[test]
    fn default_request_mapping_get_enum_methods_and_kotlin_bindings_follow_the_client_contract() {
        for (mapping, expected) in [
            ("@RequestMapping(\"/items/{id}\")", "GET"),
            (
                "@RequestMapping(path=\"/items/{id}\",method=RequestMethod.PUT)",
                "PUT",
            ),
        ] {
            let start = SOURCE.find("@PostMapping").unwrap();
            let end = SOURCE[start..].find(") String save").unwrap() + start + 1;
            let text = format!("{}{}{}", &SOURCE[..start], mapping, &SOURCE[end..]);
            let values = requests(&doc(&text));
            assert_eq!(values.len(), 1);
            assert_eq!(values[0].method, expected);
        }
        let kotlin = "import org.springframework.cloud.openfeign.FeignClient\nimport org.springframework.web.bind.annotation.GetMapping\nimport org.springframework.web.bind.annotation.PathVariable\nimport org.springframework.web.bind.annotation.RequestParam\n@FeignClient(name=\"stores\",url=\"https://api.example\") interface K { @GetMapping(\"/items/{id}\") fun load(@PathVariable id: Long,@RequestParam(\"q\") query: String): String }";
        let d = Document {
            path: "/project/K.kt".into(),
            language: "kotlin".into(),
            text: kotlin.into(),
            version: 1,
        };
        assert_eq!(lenses(&d).len(), 1);
        let aliased = Document {
            text: kotlin.replace(
                "import org.springframework.cloud.openfeign.FeignClient\n",
                "import org.springframework.cloud.openfeign.FeignClient as Other\n",
            ),
            ..d
        };
        assert!(lenses(&aliased).is_empty());
    }
    #[test]
    fn foreign_dynamic_ambiguous_and_unmodeled_binding_contracts_stay_unavailable() {
        for text in [
            SOURCE.replace(
                "org.springframework.cloud.openfeign.FeignClient;",
                "foreign.FeignClient;",
            ),
            SOURCE.replace(
                "name=\"stores\"",
                "name=\"stores\",configuration=Custom.class",
            ),
            SOURCE.replace("interface Stores {", "interface Stores extends Other {"),
            SOURCE.replace("url=\"https://api.example/v1\"", "url=\"${remote}\""),
            SOURCE.replace("value=\"/items/{id}\"", "value=PATH"),
            SOURCE.replace("@PathVariable(\"id\")", "@PathVariable(\"other\")"),
            SOURCE.replace(
                "@RequestParam(\"q\") String query",
                "@RequestParam(\"q\") List<String> query",
            ),
            SOURCE.replace("@RequestBody String body", "@SpringQueryMap Object body"),
            SOURCE.replace("mode=fast", "mode!=slow"),
            SOURCE.replace("path=\"/api\"", "path=\"/api/{prefix}\""),
            SOURCE.replace(
                "interface Stores {",
                "@RequestMapping(\"/legacy\") interface Stores {",
            ),
        ] {
            assert!(lenses(&doc(&text)).is_empty(), "{text}");
        }
    }
    #[test]
    fn stale_source_or_project_and_injected_samples_never_write() {
        let h = MockHost::install();
        let lens = lenses(&doc(SOURCE)).remove(0);
        h.open_editor(
            "/project/Stores.java",
            "java",
            &SOURCE.replace("X-Tenant=blue", "X-Tenant=red"),
            &[],
        );
        assert!(run(&h.invocation(), lens.argument.as_deref()).is_err());
        assert!(h.opened_locations().is_empty());
        h.open_editor("/project/Stores.java", "java", SOURCE, &[]);
        h.answer_next_prompt(Some(&["https://example.test"]));
        h.answer_next_prompt(Some(&["value\r\nInjected: yes"]));
        assert!(run(&h.invocation(), lens.argument.as_deref()).is_err());
        assert!(h.opened_locations().is_empty());
        assert!(h.clipboard().is_none());
        h.set_project_root(Some("/other"));
        assert!(run(&h.invocation(), lens.argument.as_deref()).is_err());
        assert!(h.requests().is_empty() && h.process_runs().is_empty());
    }
    #[test]
    fn origin_and_media_review_reject_ambiguous_targets_and_support_declared_wildcards() {
        for bad in [
            "http://a:port",
            "http://a:0",
            "http://a:65536",
            "https://user@example.test",
            "https://example.test/path",
            "https://example.test?query",
            "http://[broken]",
        ] {
            assert!(!origin(bad), "{bad}");
        }
        for good in [
            "http://localhost:8080",
            "https://api.example",
            "http://[::1]:8080",
        ] {
            assert!(origin(good), "{good}");
        }
        assert!(media_matches("*/*", "application/json"));
        assert!(media_matches(
            "application/*+json",
            "application/problem+json"
        ));
        assert!(!media_matches("application/*+json", "text/plain"));
        assert!(media("application/json; charset=UTF-8", false));
    }
    #[test]
    fn canceled_review_and_invalid_json_leave_no_example_file() {
        let h = MockHost::install();
        h.open_editor("/project/Stores.java", "java", SOURCE, &[]);
        let lens = lenses(&doc(SOURCE)).remove(0);
        h.answer_next_prompt(None);
        run(&h.invocation(), lens.argument.as_deref()).unwrap();
        assert!(h.opened_locations().is_empty());
        for value in ["https://api.example", "1", "query", "trace", "not-json"] {
            h.answer_next_prompt(Some(&[value]));
        }
        assert!(run(&h.invocation(), lens.argument.as_deref()).is_err());
        assert!(h.opened_locations().is_empty());
        assert!(
            !h.invocation()
                .project()
                .unwrap()
                .exists(&filename(&requests(&doc(SOURCE))[0]))
                .unwrap()
        );
    }
}
