//! Source-qualified request drafts. Request execution remains explicit in the HTTP client.
use openidea_plugin::{Error, Invocation, host};
use std::collections::{BTreeMap, BTreeSet};
use ui_kit::routes::Route;
fn text(value: &str) -> bool {
    !value.chars().any(|c| c.is_control())
}
fn token(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&c))
}
fn percent(value: &str) -> String {
    value
        .bytes()
        .map(|c| {
            if c.is_ascii_alphanumeric() || b"-._~".contains(&c) {
                (c as char).to_string()
            } else {
                format!("%{c:02X}")
            }
        })
        .collect()
}
fn sample(schema: &serde_json::Value) -> &'static str {
    match schema["type"].as_str() {
        Some("integer" | "number") => "0",
        Some("boolean") => "false",
        _ => "REVIEW",
    }
}

pub fn draft(route: &Route, source: &str, base: &str) -> Result<String, Error> {
    let data =
        crate::openapi_export::declared_operation(route, source).unwrap_or(serde_json::json!({}));
    let mut query = BTreeMap::new();
    let mut headers = BTreeMap::new();
    let mut notes = Vec::new();
    let mut fixed = BTreeMap::<(bool, String), String>::new();
    let mut absent = BTreeSet::<(bool, String)>::new();
    let mut forbidden = BTreeMap::<(bool, String), BTreeSet<String>>::new();
    for parameter in data["parameters"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| p["required"] == true)
    {
        let Some(name) = parameter["name"].as_str().filter(|name| text(name)) else {
            continue;
        };
        match parameter["in"].as_str() {
            Some("query") => {
                query.insert(name.to_owned(), sample(&parameter["schema"]).to_owned());
            }
            Some("header") if token(name) => {
                headers.insert(
                    name.to_ascii_lowercase(),
                    (name.to_owned(), sample(&parameter["schema"]).to_owned()),
                );
            }
            _ => {}
        }
    }
    for (kind, is_header) in [("params", false), ("headers", true)] {
        for value in data["x-spring-request-conditions"][kind]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str())
        {
            if !text(value) {
                return Err(Error::Invalid(
                    "Request condition contains unsupported control characters.".into(),
                ));
            }
            if let Some(name) = value.strip_prefix('!') {
                let required = if is_header {
                    headers.contains_key(&name.to_ascii_lowercase())
                } else {
                    query.contains_key(name)
                };
                if required {
                    return Err(Error::Invalid(
                        "Mapping absence condition conflicts with a required request binding."
                            .into(),
                    ));
                }
                absent.insert((
                    is_header,
                    if is_header {
                        name.to_ascii_lowercase()
                    } else {
                        name.into()
                    },
                ));
                notes.push(format!(
                    "# Mapping requires {} `{name}` to be absent.",
                    if is_header {
                        "header"
                    } else {
                        "query parameter"
                    }
                ));
                continue;
            }
            if let Some((name, excluded)) = value.split_once("!=") {
                let identity = (
                    is_header,
                    if is_header {
                        name.to_ascii_lowercase()
                    } else {
                        name.into()
                    },
                );
                if fixed.get(&identity).is_some_and(|value| value == excluded) {
                    return Err(Error::Invalid(
                        "Mapping literal input conflicts with a negative condition.".into(),
                    ));
                }
                forbidden
                    .entry(identity)
                    .or_default()
                    .insert(excluded.into());
                notes.push(format!("# Review negative {kind} condition: {value}"));
                continue;
            }
            let explicit = value.contains('=');
            let (name, value) = value.split_once('=').unwrap_or((value, "REVIEW"));
            if name.is_empty() {
                return Err(Error::Invalid(
                    "Request condition has an empty name.".into(),
                ));
            }
            let identity = (
                is_header,
                if is_header {
                    name.to_ascii_lowercase()
                } else {
                    name.into()
                },
            );
            if absent.contains(&identity) {
                return Err(Error::Invalid(
                    "Mapping requires the same input to be present and absent.".into(),
                ));
            }
            if explicit {
                if forbidden
                    .get(&identity)
                    .is_some_and(|values| values.contains(value))
                {
                    return Err(Error::Invalid(
                        "Mapping literal input conflicts with a negative condition.".into(),
                    ));
                }
                if fixed.get(&identity).is_some_and(|prior| prior != value) {
                    return Err(Error::Invalid(
                        "Mapping has conflicting literal input conditions.".into(),
                    ));
                }
                fixed.insert(identity, value.into());
            }
            if is_header {
                if !token(name) {
                    return Err(Error::Invalid(
                        "Request condition has an invalid header name.".into(),
                    ));
                }
                headers.insert(name.to_ascii_lowercase(), (name.into(), value.into()));
            } else {
                query.insert(name.into(), value.into());
            }
        }
    }
    let mut body_type = data["requestBody"]["content"]
        .as_object()
        .and_then(|c| c.keys().find(|media| !media.contains('*')))
        .cloned();
    if body_type.is_none()
        && data["requestBody"]["content"]
            .as_object()
            .is_some_and(|c| !c.is_empty())
    {
        notes.push("# Choose a concrete request Content-Type matching the declared media range before adding the body.".into());
    }
    let accept = data["responses"]["default"]["content"]
        .as_object()
        .and_then(|c| c.keys().next())
        .cloned();
    if let Some(media) = &body_type {
        if !fixed.contains_key(&(true, "content-type".into())) {
            headers.insert(
                "content-type".into(),
                ("Content-Type".into(), media.clone()),
            );
        } else {
            body_type = headers.get("content-type").map(|(_, value)| value.clone());
        }
    }
    if let Some(media) = accept {
        if !fixed.contains_key(&(true, "accept".into())) {
            headers.insert("accept".into(), ("Accept".into(), media));
        }
    }
    if headers
        .get("content-type")
        .is_some_and(|(_, value)| value.contains('*'))
    {
        headers.remove("content-type");
        body_type = None;
        notes.push("# Choose a concrete request Content-Type before adding the body.".into());
    }
    let mut url = format!("{}{}", base.trim_end_matches('/'), route.path);
    if !query.is_empty() {
        url.push('?');
        url.push_str(
            &query
                .iter()
                .map(|(name, value)| format!("{}={}", percent(name), percent(value)))
                .collect::<Vec<_>>()
                .join("&"),
        );
    }
    let review =
        !query.is_empty() || !headers.is_empty() || body_type.is_some() || !notes.is_empty();
    let mut output = format!("### {} {}\n", route.method, route.path);
    if review {
        output.push_str("# Review source-derived input examples before sending.\n");
    }
    for note in notes {
        output.push_str(&note);
        output.push('\n');
    }
    output.push_str(&format!("{} {url}\n", route.method));
    for (_, (name, value)) in headers {
        if !text(&value) {
            return Err(Error::Invalid(
                "Request header contains unsupported control characters.".into(),
            ));
        }
        if ["host", "content-length", "connection", "transfer-encoding"]
            .contains(&name.to_ascii_lowercase().as_str())
        {
            output.push_str(&format!("# Review required transport header {name}.\n"));
        } else {
            output.push_str(&format!("{name}: {value}\n"));
        }
    }
    if let Some(media) = body_type {
        output.push_str("\n");
        if media.contains("json") {
            output.push_str("{}\n");
        } else {
            output.push_str("# Add the required body using the declared content type.\n");
        }
    }
    Ok(output)
}
fn current(
    ctx: &Invocation,
    route: &Route,
) -> Result<(openidea_plugin::Project, String, Option<u64>), Error> {
    let location = route
        .location
        .as_ref()
        .ok_or_else(|| Error::Failed("Endpoint source is unavailable.".into()))?;
    let project = host::project_of(&location.path)
        .ok_or_else(|| Error::Failed("Endpoint source project is unavailable.".into()))?;
    if ctx.project().and_then(|p| p.root().ok()).as_deref() != Some(project.root()?.as_str()) {
        return Err(Error::Failed(
            "Endpoint project changed; refresh the route list.".into(),
        ));
    }
    let (source, version) =
        if let Some(editor) = ctx.editor().filter(|editor| editor.path() == location.path) {
            match editor.document() {
                Ok(snapshot) => (snapshot.text, Some(snapshot.version)),
                Err(_) => (project.read_file(&location.path)?, None),
            }
        } else {
            (project.read_file(&location.path)?, None)
        };
    let info = crate::endpoints::file_info(&location.path, &source)
        .ok_or_else(|| Error::Failed("Endpoint source changed.".into()))?;
    if !info.endpoints.iter().any(|e| {
        e.range == location.range
            && e.path == route.path
            && (e.verb == route.method
                || (e.verb.is_empty()
                    && [
                        "GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS", "TRACE",
                        "CONNECT",
                    ]
                    .contains(&route.method.as_str())))
    }) {
        return Err(Error::Failed(
            "Endpoint mapping changed; refresh before generating a request.".into(),
        ));
    }
    if let Some(method) = info.methods.iter().find(|m| m.range == location.range) {
        if route.handler.member.as_deref() != Some(method.name.as_str())
            || route.handler.class != method.ty
        {
            return Err(Error::Failed("Endpoint handler changed.".into()));
        }
    }
    Ok((project, source, version))
}
pub fn create(route: &Route, ctx: &Invocation) -> Result<(), Error> {
    let (project, source, version) = current(ctx, route)?;
    let Some(base) = host::input("Base URL", Some(crate::DEFAULT_BASE_URL))? else {
        return Ok(());
    };
    let base = base.trim().trim_end_matches('/');
    let authority = base
        .strip_prefix("http://")
        .or_else(|| base.strip_prefix("https://"))
        .and_then(|url| url.split('/').next())
        .filter(|authority| !authority.is_empty() && !authority.contains('@'));
    if authority.is_none()
        || base.chars().any(|c| c.is_whitespace() || c.is_control())
        || base.contains(['?', '#', '\\'])
    {
        return Err(Error::Invalid(
            "Enter an HTTP(S) base URL without credentials, query or fragment.".into(),
        ));
    }
    let (fresh, fresh_source, fresh_version) = current(ctx, route)?;
    if fresh.root() != project.root() || fresh_source != source || fresh_version != version {
        return Err(Error::Failed(
            "Endpoint source changed during request review.".into(),
        ));
    }
    let content = draft(route, &source, base)?;
    let slug = route
        .path
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    let slug = slug
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let slug = if slug.is_empty() { "root".into() } else { slug };
    let first_line = content.lines().next().unwrap_or_default().to_owned();
    let stem = format!(
        "{}/http/{}-{slug}",
        project.root()?.trim_end_matches('/'),
        route.method.to_ascii_lowercase()
    );
    let mut path = format!("{stem}.http");
    for n in 2.. {
        if !project.exists(&path)? {
            break;
        }
        let existing = project.read_file(&path).unwrap_or_default();
        if existing.lines().next() == Some(first_line.as_str()) {
            return host::open_location(&path, 0, 0);
        }
        path = format!("{stem}-{n}.http");
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
    use ui_kit::routes::Handler;
    fn route(source: &str) -> Route {
        let info = crate::endpoints::file_info("/project/C.java", source).unwrap();
        let endpoint = &info.endpoints[0];
        let method = info
            .methods
            .iter()
            .find(|m| m.range == endpoint.range)
            .unwrap();
        Route {
            method: endpoint.verb.clone(),
            path: endpoint.path.clone(),
            handler: Handler {
                class: method.ty.clone(),
                member: Some(method.name.clone()),
            },
            name: None,
            location: Some(openidea_plugin::Location {
                path: "/project/C.java".into(),
                range: endpoint.range,
            }),
        }
    }
    #[test]
    fn required_bindings_and_positive_conditions_generate_reviewable_examples() {
        let source = "@RestController class C { @PostMapping(value=\"/users\",params=\"mode=fast & safe\",headers=\"X-Tenant=web\",consumes=\"application/json\",produces=\"application/json\") void save(@RequestParam int limit,@RequestHeader(\"X-Trace\") String trace,@RequestBody UserDto body){} }";
        let text = draft(&route(source), source, "http://localhost:8080").unwrap();
        assert!(text.contains("POST http://localhost:8080/users?limit=0&mode=fast%20%26%20safe"));
        assert!(text.contains("X-Tenant: web\n"));
        assert!(text.contains("X-Trace: REVIEW\n"));
        assert!(text.contains("Content-Type: application/json\n"));
        assert!(text.contains("Accept: application/json\n"));
        assert!(text.ends_with("\n{}\n"));
        assert!(text.contains("Review source-derived"));
    }
    #[test]
    fn conflicting_or_negative_conditions_are_not_invented_as_positive_values() {
        let source = "@RestController @RequestMapping(params=\"mode=fast\") class C { @GetMapping(value=\"/users\",params=\"mode=slow\") void list(){} }";
        assert!(draft(&route(source), source, "http://localhost:8080").is_err());
        let source = "@RestController @RequestMapping(params=\"mode!=slow\") class C { @GetMapping(value=\"/users\",params=\"mode=slow\") void list(){} }";
        assert!(draft(&route(source), source, "http://localhost:8080").is_err());
        let source = "@RestController class C { @GetMapping(value=\"/users\",params=\"!mode\") void list(@RequestParam String mode){} }";
        assert!(draft(&route(source), source, "http://localhost:8080").is_err());
        let source = "@RestController class C { @GetMapping(value=\"/users\",params=\"mode!=slow\") void list(){} }";
        let text = draft(&route(source), source, "http://localhost:8080").unwrap();
        assert!(text.contains("Review negative params condition: mode!=slow"));
        assert!(!text.contains("users?"));
    }
    #[test]
    fn wildcard_consumes_does_not_become_an_invalid_literal_content_type() {
        let source = "@RestController class C { @PostMapping(value=\"/users\",consumes=\"application/*\") void save(@RequestBody UserDto body){} }";
        let text = draft(&route(source), source, "http://localhost:8080").unwrap();
        assert!(!text.contains("Content-Type: application/*"));
        assert!(text.contains("concrete request Content-Type"));
    }
    #[test]
    fn request_generation_refuses_a_changed_source_mapping_before_prompt_or_write() {
        let host = openidea_plugin::mock::MockHost::install()
            .with_capabilities(&["project.read", "project.write"]);
        let source = "@RestController class C { @GetMapping(\"/users\") void list(){} }";
        let route = route(source);
        host.add_file("C.java", &source.replace("/users", "/other"));
        host.answer_next_prompt(Some(&["http://localhost:8080"]));
        assert!(create(&route, &host.invocation()).is_err());
        assert!(host.opened_locations().is_empty());
    }
    fn create_for(host: &openidea_plugin::mock::MockHost, path: &str) {
        let source = format!("@RestController class C {{ @GetMapping(\"{path}\") void list(){{}} }}");
        host.add_file("C.java", &source);
        host.answer_next_prompt(Some(&["http://localhost:8080"]));
        create(&route(&source), &host.invocation()).unwrap();
    }
    #[test]
    fn request_example_slug_collision_suffixed() {
        let host = openidea_plugin::mock::MockHost::install()
            .with_capabilities(&["project.read", "project.write"]);
        create_for(&host, "/a-b");
        create_for(&host, "/a/b");
        create_for(&host, "/a/b");
        let opened: Vec<_> = host.opened_locations().into_iter().map(|(path, ..)| path).collect();
        assert_eq!(
            opened,
            [
                "/project/http/get-a-b.http",
                "/project/http/get-a-b-2.http",
                "/project/http/get-a-b-2.http"
            ]
        );
        let project = host.invocation().project().unwrap();
        assert!(project.read_file("http/get-a-b-2.http").unwrap().starts_with("### GET /a/b\n"));
    }
    #[test]
    fn denied_write_and_clipboard_give_a_friendly_message() {
        let host = openidea_plugin::mock::MockHost::install().with_capabilities(&["project.read"]);
        let source = "@RestController class C { @GetMapping(\"/users\") void list(){} }";
        host.add_file("C.java", source);
        host.answer_next_prompt(Some(&["http://localhost:8080"]));
        let error = create(&route(source), &host.invocation()).unwrap_err();
        assert!(error.to_string().contains("needs the clipboard permission"), "{error}");
        assert!(host.notifications().iter().any(|n| n.body.contains("needs the project.write permission")));
    }
}
