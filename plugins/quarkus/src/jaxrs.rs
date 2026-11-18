//! JAX-RS resource checks that need only literal source: path-parameter names, HTTP method designators,
//! duplicate resource methods and duplicate `@Scheduled` identities. Written from the Jakarta REST 3.1
//! specification (3.3 resource methods, 3.4 URI templates, 3.7.2 matching), the Quarkus REST guide
//! ("Declaring URI parameters") and `io.quarkus.scheduler.Scheduled#identity` (the scheduler processor
//! rejects an identity another method already uses). Anything not a plain literal stays quiet.

use spring_text::finding::Finding;
use spring_text::java::{self, Annotation, JavaFile, Member, TypeDecl};

const VERBS: [&str; 7] = ["GET", "POST", "PUT", "DELETE", "HEAD", "OPTIONS", "PATCH"];
const REST_PATH: &str = "org.jboss.resteasy.reactive.RestPath";
const IMPLICIT_TYPES: [&str; 14] = [
    "UriInfo", "HttpHeaders", "Request", "SecurityContext", "Providers", "ResourceInfo", "Application",
    "RoutingContext", "HttpServerRequest", "HttpServerResponse", "ContainerRequestContext", "Sse", "SseEventSink",
    "ResteasyReactiveRequestContext",
];

/// A literal resource method, comparable across the module.
#[derive(Clone, Debug)]
pub struct Route {
    pub verb: String,
    pub key: String,
    /// Empty means no restriction.
    pub consumes: Vec<String>,
    pub produces: Vec<String>,
    pub at: usize,
    pub end: usize,
    pub owner: String,
}

#[derive(Clone, Debug, Default)]
pub struct Facts {
    pub routes: Vec<Route>,
    pub identities: Vec<(String, usize, usize)>,
    pub applications: usize,
}

impl Facts {
    pub fn is_empty(&self) -> bool {
        self.routes.is_empty() && self.identities.is_empty() && self.applications == 0
    }
}

fn owned(file: &JavaFile, text: &str, a: &Annotation, names: &[&str]) -> bool {
    names.iter().any(|q| crate::java::annotation_owned(file, text, a, q))
}

fn ws(name: &str) -> [String; 2] {
    [format!("jakarta.ws.rs.{name}"), format!("javax.ws.rs.{name}")]
}

fn ws_owned(file: &JavaFile, text: &str, a: &Annotation, name: &str) -> bool {
    let [jakarta, javax] = ws(name);
    owned(file, text, a, &[&jakarta, &javax])
}

fn literal<'a>(a: &'a Annotation, name: &str) -> Option<&'a java::Str> {
    let arg = a.arg(name)?;
    let [value] = arg.strings.as_slice() else { return None };
    (java::tokens(&arg.text).len() == 1
        && arg.text.starts_with('"')
        && arg.text.ends_with('"')
        && !arg.text.contains('\\')
        && !value.value.chars().any(char::is_control))
    .then_some(value)
}

struct Var {
    name: String,
    regex: Option<String>,
    /// Byte offset of `{` inside the value.
    at: usize,
    len: usize,
}

/// `None` when the template is not a well-formed run of literal text and `{name}` / `{name: regex}` variables.
fn variables(template: &str) -> Option<Vec<Var>> {
    let mut vars = Vec::new();
    let mut chars = template.char_indices();
    while let Some((at, c)) = chars.next() {
        if c == '}' {
            return None;
        }
        if c != '{' {
            continue;
        }
        let mut depth = 1;
        let mut end = None;
        for (i, c) in chars.by_ref() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(i);
                        break;
                    }
                }
                _ => {}
            }
        }
        let end = end?;
        let inner = &template[at + 1..end];
        let (name, regex) = match inner.split_once(':') {
            Some((name, regex)) => (name.trim(), Some(regex.trim().to_owned())),
            None => (inner.trim(), None),
        };
        let mut letters = name.chars();
        let first = letters.next()?;
        if !(first.is_alphabetic() || first == '_') || !letters.all(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '-')) {
            return None;
        }
        vars.push(Var { name: name.to_owned(), regex, at, len: end + 1 - at });
    }
    Some(vars)
}

fn normalised(template: &str, vars: &[Var]) -> String {
    let mut out = String::new();
    let mut from = 0;
    for v in vars {
        out.push_str(&template[from..v.at]);
        match &v.regex {
            Some(re) => out.push_str(&format!("{{:{re}}}")),
            None => out.push_str("{}"),
        }
        from = v.at + v.len;
    }
    out.push_str(&template[from..]);
    out.split('/').filter(|s| !s.is_empty()).collect::<Vec<_>>().join("/")
}

fn media_constant(name: &str) -> Option<&'static str> {
    Some(match name {
        "APPLICATION_JSON" => "application/json",
        "APPLICATION_XML" => "application/xml",
        "APPLICATION_FORM_URLENCODED" => "application/x-www-form-urlencoded",
        "APPLICATION_OCTET_STREAM" => "application/octet-stream",
        "MULTIPART_FORM_DATA" => "multipart/form-data",
        "TEXT_PLAIN" => "text/plain",
        "TEXT_HTML" => "text/html",
        "TEXT_XML" => "text/xml",
        "SERVER_SENT_EVENTS" => "text/event-stream",
        "WILDCARD" => "*/*",
        _ => return None,
    })
}

/// `Some(empty)` when the annotation is absent; `None` when any value is not a plain literal.
fn media_types(file: &JavaFile, text: &str, annotations: &[Annotation], name: &str) -> Option<Option<Vec<String>>> {
    let found: Vec<_> = annotations.iter().filter(|a| a.name == name).collect();
    let [a] = found.as_slice() else { return if found.is_empty() { Some(None) } else { None } };
    if !ws_owned(file, text, a, name) {
        return None;
    }
    let arg = a.arg("value")?;
    let media_imported = file.imports.iter().any(|i| !i.is_static && matches!(i.path.as_str(), "jakarta.ws.rs.core.MediaType" | "javax.ws.rs.core.MediaType"));
    let toks = java::tokens(&arg.text);
    let mut out = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        let t = toks[i].s;
        if matches!(t, "{" | "}" | ",") {
            i += 1;
        } else if t.starts_with('"') && t.ends_with('"') && t.len() >= 2 && !t.contains('\\') {
            out.extend(t[1..t.len() - 1].split(',').map(|s| s.trim().to_ascii_lowercase()).filter(|s| !s.is_empty()));
            i += 1;
        } else if media_imported && t == "MediaType" && toks.get(i + 1).is_some_and(|d| d.s == ".") {
            out.push(media_constant(toks.get(i + 2)?.s)?.to_owned());
            i += 3;
        } else {
            return None;
        }
    }
    Some(Some(out))
}

fn compatible(a: &str, b: &str) -> bool {
    let strip = |s: &str| s.split(';').next().unwrap_or(s).trim().to_owned();
    let (a, b) = (strip(a), strip(b));
    let (Some((at, asub)), Some((bt, bsub))) = (a.split_once('/'), b.split_once('/')) else { return false };
    (at == "*" || bt == "*" || at == bt) && (asub == "*" || bsub == "*" || asub == bsub)
}

fn overlap(a: &[String], b: &[String]) -> bool {
    a.is_empty() || b.is_empty() || a.iter().any(|x| b.iter().any(|y| compatible(x, y)))
}

fn class_path<'a>(file: &JavaFile, text: &str, ty: &'a TypeDecl) -> Option<(&'a java::Str, Vec<Var>)> {
    let paths: Vec<_> = ty.annotations.iter().filter(|a| a.name == "Path").collect();
    let [a] = paths.as_slice() else { return None };
    if !ws_owned(file, text, a, "Path") {
        return None;
    }
    let value = literal(a, "value")?;
    let vars = variables(&value.value)?;
    Some((value, vars))
}

fn method_path<'a>(file: &JavaFile, text: &str, m: &'a Member) -> Result<Option<(&'a java::Str, Vec<Var>)>, ()> {
    let paths: Vec<_> = m.annotations.iter().filter(|a| a.name == "Path").collect();
    let [a] = paths.as_slice() else { return if paths.is_empty() { Ok(None) } else { Err(()) } };
    if !ws_owned(file, text, a, "Path") {
        return Err(());
    }
    let value = literal(a, "value").ok_or(())?;
    let vars = variables(&value.value).ok_or(())?;
    Ok(Some((value, vars)))
}

fn verbs<'a>(file: &JavaFile, text: &str, m: &'a Member) -> Vec<&'a Annotation> {
    m.annotations.iter().filter(|a| VERBS.contains(&a.name.as_str()) && ws_owned(file, text, a, &a.name)).collect()
}

fn effective(own: Option<Option<Vec<String>>>, class: &Option<Option<Vec<String>>>) -> Option<Vec<String>> {
    match own? {
        Some(values) => Some(values),
        None => Some(class.clone()?.unwrap_or_default()),
    }
}

pub fn facts(file: &JavaFile, text: &str) -> Facts {
    let mut facts = Facts::default();
    for ty in &file.types {
        facts.applications += ty.annotations.iter().filter(|a| a.name == "ApplicationPath" && ws_owned(file, text, a, "ApplicationPath")).count();
        let root = class_path(file, text, ty);
        let class_consumes = media_types(file, text, &ty.annotations, "Consumes");
        let class_produces = media_types(file, text, &ty.annotations, "Produces");
        for m in ty.methods() {
            if let Some(a) = m.annotation("Scheduled").filter(|a| owned(file, text, a, &["io.quarkus.scheduler.Scheduled"]))
                && let Some(s) = literal(a, "identity").filter(|s| !s.value.is_empty() && !s.value.contains(['{', '$']))
            {
                facts.identities.push((s.value.clone(), s.at, s.end));
            }
            let Some((root_value, root_vars)) = &root else { continue };
            let found = verbs(file, text, m);
            let [verb] = found.as_slice() else { continue };
            let Ok(own) = method_path(file, text, m) else { continue };
            let Some(consumes) = effective(media_types(file, text, &m.annotations, "Consumes"), &class_consumes) else { continue };
            let Some(produces) = effective(media_types(file, text, &m.annotations, "Produces"), &class_produces) else { continue };
            let template = match &own {
                Some((value, _)) => format!("{}/{}", root_value.value, value.value),
                None => root_value.value.clone(),
            };
            let mut all: Vec<Var> = root_vars.iter().map(|v| Var { name: v.name.clone(), regex: v.regex.clone(), at: v.at, len: v.len }).collect();
            if let Some((_, vars)) = &own {
                let offset = root_value.value.len() + 1;
                all.extend(vars.iter().map(|v| Var { name: v.name.clone(), regex: v.regex.clone(), at: v.at + offset, len: v.len }));
            }
            facts.routes.push(Route {
                verb: verb.name.clone(),
                key: normalised(&template, &all),
                consumes,
                produces,
                at: m.at,
                end: m.at + m.name.len(),
                owner: format!("{}.{}", ty.name, m.name),
            });
        }
    }
    facts
}

fn is_bound_elsewhere(file: &JavaFile, text: &str, ty: &TypeDecl, m: &Member) -> bool {
    let annotated = |a: &Annotation| {
        (a.name == "BeanParam" && ws_owned(file, text, a, "BeanParam"))
            || (a.name == "PathParam" && ws_owned(file, text, a, "PathParam"))
            || (a.name == "RestPath" && owned(file, text, a, &[REST_PATH]))
    };
    ty.fields().any(|f| f.annotations.iter().any(annotated))
        || m.params.iter().any(|p| p.annotations.iter().any(|a| a.name == "BeanParam"))
}

pub fn check(file: &JavaFile, text: &str, module: Option<&Facts>, out: &mut Vec<Finding>) {
    use openidea_plugin::Severity;
    let own = facts(file, text);
    for ty in &file.types {
        let Some((root_value, root_vars)) = class_path(file, text, ty) else { continue };
        for m in ty.methods() {
            let found = verbs(file, text, m);
            if found.len() > 1 {
                let names: Vec<_> = found.iter().map(|a| format!("@{}", a.name)).collect();
                out.push(Finding::new(m.at, m.at + m.name.len(), "multiple-http-methods", format!("{} carry more than one HTTP method designator; jakarta.ws.rs.HttpMethod calls this an error and Quarkus REST fails the build.", names.join(" and "))));
            }
            let Ok(path) = method_path(file, text, m) else { continue };
            if found.is_empty() && path.is_none() {
                continue;
            }
            let mut vars: Vec<&str> = root_vars.iter().map(|v| v.name.as_str()).collect();
            if let Some((_, v)) = &path {
                vars.extend(v.iter().map(|v| v.name.as_str()));
            }
            let mut bound: Vec<String> = Vec::new();
            for p in &m.params {
                let declared = p.annotations.iter().find_map(|a| {
                    let ours = (a.name == "PathParam" && ws_owned(file, text, a, "PathParam")) || (a.name == "RestPath" && owned(file, text, a, &[REST_PATH]));
                    ours.then_some(a)
                });
                if let Some(a) = declared {
                    let name = match literal(a, "value") {
                        Some(s) => Some(s),
                        None if a.name == "RestPath" && a.args.is_empty() => None,
                        None => continue,
                    };
                    let (key, at, end) = match name {
                        Some(s) => (s.value.clone(), s.at, s.end),
                        None => (p.name.clone(), p.at, p.at + p.name.len()),
                    };
                    if !vars.contains(&key.as_str()) {
                        out.push(Finding::new(at, end, "path-param-unknown", format!("`{key}` is not a `{{{key}}}` variable of the class and method @Path ({}).", if vars.is_empty() { "none declared".to_owned() } else { vars.iter().map(|v| format!("`{v}`")).collect::<Vec<_>>().join(", ") })));
                    }
                    bound.push(key);
                } else if p.annotations.is_empty() {
                    bound.push(p.name.clone());
                }
            }
            if !found.is_empty() && !is_bound_elsewhere(file, text, ty, m) {
                let mut unbound = Vec::new();
                if let Some((value, v)) = &path {
                    unbound.extend(v.iter().map(|v| (v, value.at)));
                }
                unbound.extend(root_vars.iter().map(|v| (v, root_value.at)));
                for (v, base) in unbound {
                    if !bound.contains(&v.name) {
                        let message = format!("`{{{0}}}` is bound by no parameter of this method; Quarkus REST binds it with `@RestPath` or a parameter named `{0}`.", v.name);
                        out.push(Finding::new(base + v.at, base + v.at + v.len, "path-variable-unbound", message).with_severity(Severity::Info));
                    }
                }
            }
            if let [verb] = found.as_slice()
                && verb.name == "GET"
                && let Some(p) = m.params.iter().find(|p| p.annotations.is_empty() && !vars.contains(&p.name.as_str()) && !IMPLICIT_TYPES.contains(&p.ty.name.as_str()))
            {
                out.push(Finding::new(p.at, p.at + p.name.len(), "get-with-entity", "A @GET method with an entity (unannotated) parameter; Jakarta REST 3.3.2.1 defines no semantics for a GET request body and Quarkus REST logs a warning.").with_severity(Severity::Info));
            }
        }
    }
    let Some(module) = module else { return };
    if own.applications + module.applications <= 1 {
        for r in &own.routes {
            let clash = own.routes.iter().filter(|o| o.at != r.at).chain(module.routes.iter()).find(|o| o.verb == r.verb && o.key == r.key && overlap(&o.consumes, &r.consumes) && overlap(&o.produces, &r.produces));
            if let Some(other) = clash {
                let message = format!("`{}` and `{}` both serve {} /{} with overlapping @Consumes/@Produces; the request matcher cannot choose between them (Jakarta REST 3.7.2).", r.owner, other.owner, r.verb, r.key);
                out.push(Finding::new(r.at, r.end, "duplicate-resource-method", message));
            }
        }
    }
    for (identity, at, end) in &own.identities {
        let repeated = own.identities.iter().filter(|(i, a, _)| i == identity && a != at).count() + module.identities.iter().filter(|(i, _, _)| i == identity).count();
        if repeated > 0 {
            out.push(Finding::new(*at, *end, "duplicate-scheduled-identity", format!("Another @Scheduled method uses identity `{identity}`; Quarkus requires unique identities and fails the build otherwise.")));
        }
    }
}

/// What the other source files of the module declare.
pub fn merge<'a>(others: impl Iterator<Item = &'a Facts>) -> Facts {
    let mut all = Facts::default();
    for f in others {
        all.routes.extend(f.routes.iter().cloned());
        all.identities.extend(f.identities.iter().cloned());
        all.applications += f.applications;
    }
    all
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::Severity;

    const IMPORTS: &str = "import jakarta.ws.rs.*; import jakarta.ws.rs.core.MediaType; import io.quarkus.scheduler.Scheduled; import org.jboss.resteasy.reactive.RestPath;\n";

    fn imports() -> String {
        IMPORTS.replace("jakarta.ws.rs.*;", "jakarta.ws.rs.Path; import jakarta.ws.rs.PathParam; import jakarta.ws.rs.GET; import jakarta.ws.rs.POST; import jakarta.ws.rs.PUT; import jakarta.ws.rs.Consumes; import jakarta.ws.rs.Produces;")
    }

    fn run(body: &str, module: Option<&Facts>) -> Vec<(&'static str, Severity, String)> {
        let text = format!("{}{body}", imports());
        let file = java::parse(&text);
        let mut out = Vec::new();
        check(&file, &text, module, &mut out);
        out.into_iter().map(|f| (f.code, f.severity, text[f.start..f.end].to_owned())).collect()
    }

    fn facts_of(body: &str) -> Facts {
        let text = format!("{}{body}", imports());
        facts(&java::parse(&text), &text)
    }

    fn codes(found: &[(&'static str, Severity, String)]) -> Vec<&'static str> {
        found.iter().map(|f| f.0).collect()
    }

    #[test]
    fn path_param_must_match_a_template_variable() {
        let found = run(r#"@Path("/a/{id}") class R { @GET @Path("{name:[a-z]+}") String g(@PathParam("id") String i, @PathParam("name") String n, @PathParam("other") String o, @RestPath String missing, @RestPath("x") String x) { return null; } }"#, None);
        assert_eq!(found.iter().filter(|f| f.0 == "path-param-unknown").map(|f| f.2.as_str()).collect::<Vec<_>>(), ["other", "missing", "x"]);
        assert!(found.iter().all(|f| f.0 != "path-variable-unbound"));
    }

    #[test]
    fn unbound_variables_are_info_and_rest_path_by_name_binds() {
        let found = run(r#"@Path("/a") class R { @GET @Path("{id}/{n}") String g(String id) { return null; } }"#, None);
        assert_eq!(found, [("path-variable-unbound", Severity::Info, "{n}".into())]);
    }

    #[test]
    fn dynamic_paths_and_foreign_annotations_stay_quiet() {
        assert!(run(r#"@Path(P) class R { @GET String g(@PathParam("x") String x) { return null; } }"#, None).is_empty());
        assert!(run(r#"@Path("/a") class R { @GET @Path(Q) String g(@PathParam("x") String x) { return null; } }"#, None).is_empty());
        assert!(run(r#"@Path("/a/{broken") class R { @GET String g(@PathParam("x") String x) { return null; } }"#, None).is_empty());
        assert!(run(r#"@Path("/a/{x}") class R { @PathParam("x") String x; @GET String g() { return null; } }"#, None).is_empty());
        assert!(run(r#"@Path("/a/{x}") class R { @GET String g(@BeanParam Bean b) { return null; } }"#, None).is_empty());
        let foreign = format!("import other.PathParam; import jakarta.ws.rs.Path; import jakarta.ws.rs.GET; @Path(\"/a\") class R {{ @GET String g(@PathParam(\"x\") String x) {{ return null; }} }}");
        let file = java::parse(&foreign);
        let mut out = Vec::new();
        check(&file, &foreign, None, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn httpmethod_javadoc_and_endpointindexer_multiple_designators_warn_and_quarkus_get_body_warning_is_info() {
        let found = run(r#"@Path("/a") class R { @GET @POST String both() { return null; } @GET String body(String payload) { return null; } @GET String ctx(UriInfo info) { return null; } @PUT String put(String payload) { return null; } }"#, None);
        assert_eq!(found, [("multiple-http-methods", Severity::Warning, "both".into()), ("get-with-entity", Severity::Info, "payload".into())]);
    }

    #[test]
    fn duplicate_methods_need_same_template_verb_and_overlapping_media_types() {
        let other = facts_of(r#"@Path("/api") class A { @GET @Path("/{id}") @Produces(MediaType.APPLICATION_JSON) String a() { return null; } }"#);
        let module = merge([&other].into_iter());
        let clash = run(r#"@Path("api") @Produces("application/json") class B { @GET @Path("{key}") String b() { return null; } }"#, Some(&module));
        assert!(codes(&clash).contains(&"duplicate-resource-method"), "variable names are normalised away: {clash:?}");
        let clash = run(r#"@Path("api") @Produces("application/json") class B { @GET @Path("{id}") String b(@PathParam("id") String id) { return null; } }"#, Some(&module));
        assert_eq!(codes(&clash), ["duplicate-resource-method"]);
        for quiet in [
            r#"@Path("api") @Produces("text/plain") class B { @GET @Path("{id}") String b(@PathParam("id") String id) { return null; } }"#,
            r#"@Path("api") class B { @POST @Path("{id}") String b(@PathParam("id") String id) { return null; } }"#,
            r#"@Path("api") class B { @GET @Path("{id:[0-9]+}") String b(@PathParam("id") String id) { return null; } }"#,
            r#"@Path("api") class B { @GET @Path(DYNAMIC) String b() { return null; } }"#,
            r#"@Path("api") @Produces(TYPES) class B { @GET @Path("{id}") String b(@PathParam("id") String id) { return null; } }"#,
        ] {
            assert!(run(quiet, Some(&module)).is_empty(), "{quiet}");
        }
        assert!(run(r#"@Path("api") class B { @GET @Path("{id}") String b(@PathParam("id") String id) { return null; } }"#, None).is_empty());
    }

    #[test]
    fn duplicates_inside_one_file_and_several_applications() {
        let body = r#"@Path("/x") class R { @GET String a() { return null; } @GET String b() { return null; } }"#;
        assert_eq!(codes(&run(body, Some(&Facts::default()))), ["duplicate-resource-method", "duplicate-resource-method"]);
        let two = Facts { applications: 2, ..Facts::default() };
        assert!(run(body, Some(&two)).is_empty());
    }

    #[test]
    fn scheduled_identities_must_be_unique_literals() {
        let other = merge([&facts_of(r#"class J { @Scheduled(every = "1s", identity = "job") void a() {} @Scheduled(every = "1s", identity = "${id}") void c() {} }"#)].into_iter());
        let found = run(r#"class K { @Scheduled(every = "1s", identity = "job") void a() {} @Scheduled(every = "1s", identity = "fresh") void b() {} @Scheduled(every = "1s", identity = "${id}") void c() {} @Scheduled(every = "1s", identity = ID) void d() {} }"#, Some(&other));
        assert_eq!(found, [("duplicate-scheduled-identity", Severity::Warning, "job".into())]);
        let same_file = run(r#"class K { @Scheduled(every = "1s", identity = "a") void a() {} @Scheduled(every = "2s", identity = "a") void b() {} }"#, Some(&Facts::default()));
        assert_eq!(same_file.len(), 2);
        assert!(run(r#"class K { @Scheduled(every = "1s", identity = "a") void a() {} @Scheduled(every = "2s", identity = "a") void b() {} }"#, None).is_empty());
    }
}
