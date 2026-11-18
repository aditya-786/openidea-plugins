//! Laravel application's MCP registrations and explicit protocol discovery.
//! These are app servers, distinct from the IDE plugin's own MCP tools.
use crate::{artisan_runner::Runner, php};
use openidea_plugin::{Error, HttpRequest, Invocation, Project, host};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};
const FACADE: &str = "Laravel\\Mcp\\Facades\\Mcp";
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Registration {
    pub transport: String,
    pub name: String,
    pub server: String,
    pub file: String,
    pub offset: usize,
    pub hash: String,
}
/// One line for the notification after connecting.
pub(crate) fn summary(value: &Value) -> String {
    let count = |key: &str| value[key].as_array().map_or(0, Vec::len).to_string();
    crate::ui_text::format(
        "process.mcp.summary",
        "Connected to {server}: {tools} tools, {resources} resources, {prompts} prompts.",
        &[
            ("server", value["registration"]["server"].as_str().unwrap_or("the application MCP server")),
            ("tools", &count("tools")),
            ("resources", &count("resources")),
            ("prompts", &count("prompts")),
        ],
    )
}
fn clean(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control)
}
fn parse(source: &str, file: &str) -> Vec<Registration> {
    if source.len() > 128 * 1024 {
        return vec![];
    }
    let mut result = Vec::new();
    let mut candidates = 0;
    for (open, _) in source.match_indices('(') {
        let prefix = source[..open].trim_end();
        let Some((before, method)) = prefix.rsplit_once("::") else {
            continue;
        };
        let method = method.trim();
        if !matches!(method, "web" | "local") || before.is_empty() {
            continue;
        }
        candidates += 1;
        if candidates > 64 {
            return vec![];
        }
        let Some(identity) =
            crate::eloquent_members::metadata_inference::call_identity(source, open)
        else {
            continue;
        };
        if identity.constructor
            || !identity
                .class
                .as_deref()
                .is_some_and(|class| class.eq_ignore_ascii_case(FACADE))
            || identity.method != method
        {
            continue;
        }
        let args = php::arguments(source, open);
        if args.len() != 2 {
            continue;
        }
        let Some((name, offset)) = php::literal(source, args[0].clone()) else {
            continue;
        };
        if !clean(name, 512) || name.contains(['\\', '$']) {
            continue;
        }
        if method == "web" {
            if !name.starts_with('/') || name.starts_with("//") || name.contains(['?', '#']) {
                continue;
            }
        } else if name.len() > 128
            || name.starts_with('-')
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
        {
            continue;
        }
        let Some(class) = php::class_constant(source, args[1].clone()) else {
            continue;
        };
        let Some(server) = crate::eloquent_members::source_class_name(source, class) else {
            continue;
        };
        result.push(Registration {
            transport: method.into(),
            name: name.into(),
            server,
            file: file.into(),
            offset,
            hash: crate::artisan_metadata::hash(source),
        });
    }
    result
}
fn registrations(project: &Project, extra: &[String]) -> Result<Vec<Registration>, Error> {
    let root = project.root()?;
    let mut paths = project.find_files("routes/**/*.php", 256)?;
    if paths.len() >= 256 {
        return Err(crate::ui_text::t(
            "process.mcp.text0",
            "MCP source inventory reached its file bound.",
        )
        .into());
    }
    paths.extend(extra.iter().filter(|path| path.ends_with(".php")).cloned());
    paths.sort();
    paths.dedup();
    if paths.len() > 512 {
        return Err(Error::Stale);
    }
    let mut result = Vec::new();
    let mut size = 0;
    for path in paths {
        if !path.starts_with(&format!("{root}/")) || !crate::owns_current_path(project, &path) {
            return Err(Error::Stale);
        }
        let source = project.read_file(&path)?;
        size += source.len();
        if size > 8 * 1024 * 1024 {
            return Err(crate::ui_text::t(
                "process.mcp.text1",
                "MCP source inventory exceeds 8 MiB.",
            )
            .into());
        }
        result.extend(parse(&source, &path));
        if result.len() > 256 {
            return Err(crate::ui_text::t(
                "process.mcp.text2",
                "MCP registration inventory exceeds 256 entries.",
            )
            .into());
        }
    }
    Ok(result)
}
pub(crate) fn inventory(project: &Project, extra: &[String]) -> Result<Value, Error> {
    let root = project.root()?;
    let entries = registrations(project, extra)?;
    Ok(
        json!({"servers":entries.iter().map(|r|json!({"transport":r.transport,"name":r.name,"server":r.server,"file":r.file.strip_prefix(&format!("{root}/")).unwrap_or(&r.file),"position":openidea_plugin::text::position(&project.read_file(&r.file).unwrap_or_default(),r.offset),"method":if r.transport=="web"{"POST"}else{"stdio"}})).collect::<Vec<_>>(),"provenance":"Owned literal Laravel MCP server registrations; no PHP or server connection performed"}),
    )
}
fn owner(ctx: &Invocation) -> Result<(Project, String), Error> {
    let p = ctx
        .project()
        .ok_or_else(|| crate::ui_text::t("process.mcp.text20", "Open a Laravel project first."))?;
    let root = p.root()?;
    if !crate::hover::canonical(&root)
        || !host::projects()?
            .iter()
            .any(|p| p.root().ok().as_deref() == Some(&root))
    {
        return Err(Error::Stale);
    }
    Ok((p, root))
}
fn binding(project: &Project, runner: Runner, r: &Registration) -> Result<Value, Error> {
    if !crate::owns_current_path(project, &r.file)
        || crate::artisan_metadata::hash(&project.read_file(&r.file)?) != r.hash
    {
        return Err(Error::Stale);
    }
    let target = if runner == Runner::Docker {
        let snapshot = crate::artisan_docker::snapshot(project)?;
        json!({"identity":snapshot["identity"],"proof":snapshot["proof"]})
    } else {
        Value::Null
    };
    let mut application = Vec::new();
    let mut total = 0usize;
    let mut paths = project.find_files("app/**/*.php", 4096)?;
    if paths.len() >= 4096 {
        return Err(Error::Stale);
    }
    paths.sort();
    for path in paths {
        if !crate::owns_current_path(project, &path) {
            return Err(Error::Stale);
        }
        let text = project.read_file(&path)?;
        total += text.len();
        if total > 16 * 1024 * 1024 {
            return Err(Error::Stale);
        }
        application.push(json!([path, crate::artisan_metadata::hash(&text)]));
    }
    Ok(
        json!({"applicationSources":application,"sources":crate::runtime_inspection::stamp(project)?,"registration":[r.transport,r.name,r.server,r.file,r.hash],"target":target}),
    )
}
fn curated_entries(value: &Value, key: &str) -> Result<Vec<Value>, Error> {
    let values = value[key]
        .as_array()
        .filter(|v| v.len() <= 128)
        .ok_or(Error::Stale)?;
    let mut entries = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for entry in values {
        let name = entry[if key == "resources" { "uri" } else { "name" }]
            .as_str()
            .filter(|s| clean(s, 1024))
            .ok_or(Error::Stale)?;
        if !seen.insert(name) {
            return Err(Error::Stale);
        }
        let mut safe =
            json!({"name":entry["name"].as_str().filter(|s|clean(s,512)).unwrap_or(name)});
        if key == "resources" {
            safe["uri"] = json!(name);
        }
        for field in ["title", "description", "mimeType"] {
            if let Some(text) = entry[field].as_str().filter(|s| clean(s, 4096)) {
                safe[field] = json!(text);
            }
        }
        if key == "tools" {
            let schema = entry["inputSchema"].as_object().ok_or(Error::Stale)?;
            if json!(schema).to_string().len() > 64 * 1024 {
                return Err(Error::Stale);
            }
            safe["inputSchema"] = json!(schema);
        }
        if key == "prompts" {
            if let Some(arguments) = entry["arguments"].as_array() {
                if arguments.len() > 64 {
                    return Err(Error::Stale);
                }
                safe["arguments"]=json!(arguments.iter().map(|a|json!({"name":a["name"].as_str().filter(|s|clean(s,256)).unwrap_or(""),"required":a["required"].as_bool().unwrap_or(false)})).collect::<Vec<_>>());
            }
        }
        entries.push(safe);
    }
    Ok(entries)
}
fn discover(
    mut rpc: impl FnMut(&str, Value, bool) -> Result<Value, Error>,
) -> Result<Value, Error> {
    let initialized = rpc(
        "initialize",
        json!({"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"OpenIDEA Laravel","version":"0.2.0"}}),
        false,
    )?;
    let protocol = initialized["protocolVersion"]
        .as_str()
        .filter(|s| {
            matches!(
                *s,
                "2024-11-05" | "2025-03-26" | "2025-06-18" | "2025-11-25"
            )
        })
        .ok_or_else(|| {
            crate::ui_text::t(
                "process.mcp.text21",
                "Application MCP server selected an unsupported protocol version.",
            )
        })?;
    rpc("notifications/initialized", json!({}), true)?;
    let mut result =
        json!({"protocolVersion":protocol,"serverInfo":{},"tools":[],"resources":[],"prompts":[]});
    for field in ["name", "version", "title"] {
        if let Some(value) = initialized["serverInfo"][field]
            .as_str()
            .filter(|v| clean(v, 512))
        {
            result["serverInfo"][field] = json!(value);
        }
    }
    for key in ["tools", "resources", "prompts"] {
        if initialized["capabilities"][key].is_object() {
            let mut entries = Vec::new();
            let mut seen = std::collections::BTreeSet::new();
            let mut cursor = None;
            for _ in 0..8 {
                let params = cursor
                    .as_ref()
                    .map(|c| json!({"cursor":c}))
                    .unwrap_or_else(|| json!({}));
                let response = rpc(&format!("{key}/list"), params, false)?;
                entries.extend(curated_entries(&response, key)?);
                if entries.len() > 512 {
                    return Err(crate::ui_text::t(
                        "process.mcp.text3",
                        "Application MCP inventory exceeds its bound.",
                    )
                    .into());
                }
                let Some(next) = response["nextCursor"].as_str().filter(|s| clean(s, 2048)) else {
                    cursor = None;
                    break;
                };
                if !seen.insert(next.to_owned()) {
                    return Err(crate::ui_text::t(
                        "process.mcp.text4",
                        "Application MCP server repeated its cursor.",
                    )
                    .into());
                }
                cursor = Some(next.to_owned());
            }
            if cursor.is_some() {
                return Err(crate::ui_text::t(
                    "process.mcp.text5",
                    "Application MCP pagination exceeds eight pages.",
                )
                .into());
            }
            result[key] = json!(entries);
        }
    }
    Ok(result)
}
fn response(value: Value, id: u64) -> Result<Value, Error> {
    if value["jsonrpc"] != "2.0" || value["id"].as_u64() != Some(id) {
        return Err(Error::Stale);
    }
    if value.get("error").is_some() {
        return Err(crate::ui_text::t(
            "process.mcp.text6",
            "Application MCP server rejected metadata discovery.",
        )
        .into());
    }
    value.get("result").cloned().ok_or(Error::Stale)
}
fn local(project: &Project, runner: Runner, argv: &[String]) -> Result<Value, Error> {
    let executable = runner.argv(project, argv, false)?;
    let child = crate::artisan_runner::start_background(runner, &executable, project)?;
    let result = stdio_discover(
        |text| child.write(text),
        || {
            let chunk = child.wait(25)?;
            Ok((chunk.stdout, chunk.status.is_some()))
        },
    );
    let _ = child.kill();
    let _ = child.close_stdin();
    result
}
fn stdio_discover(
    mut write: impl FnMut(&str) -> Result<(), Error>,
    mut wait: impl FnMut() -> Result<(String, bool), Error>,
) -> Result<Value, Error> {
    let mut id = 0u64;
    let mut buffer = String::new();
    discover(|method, params, notification| {
        id += 1;
        let request = if notification {
            json!({"jsonrpc":"2.0","method":method,"params":params})
        } else {
            json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
        };
        write(&(request.to_string() + "\n"))?;
        if notification {
            return Ok(Value::Null);
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(end) = buffer.find('\n') {
                let line = buffer[..end].to_owned();
                buffer.drain(..end + 1);
                if line.trim().is_empty() {
                    continue;
                }
                let value = crate::strict_json::parse(&line, 2 * 1024 * 1024).map_err(|_| {
                    Error::Invalid(crate::ui_text::t(
                        "process.mcp.text7",
                        "Application MCP stdout is not line-delimited JSON-RPC.",
                    ))
                })?;
                if value.get("id").is_none() {
                    continue;
                }
                return response(value, id);
            }
            if Instant::now() >= deadline {
                return Err(crate::ui_text::t(
                    "process.mcp.text8",
                    "Application MCP metadata request timed out.",
                )
                .into());
            }
            let (stdout, exited) = wait()?;
            if buffer.len() + stdout.len() > 2 * 1024 * 1024 {
                return Err(crate::ui_text::t(
                    "process.mcp.text9",
                    "Application MCP output exceeds 2 MiB.",
                )
                .into());
            }
            buffer.push_str(&stdout);
            if exited && buffer.is_empty() {
                return Err(crate::ui_text::t(
                    "process.mcp.text10",
                    "Application MCP server exited before responding.",
                )
                .into());
            }
        }
    })
}

fn http_url(base: &str, path: &str) -> Result<String, Error> {
    let tail = base
        .strip_prefix("http://")
        .or_else(|| base.strip_prefix("https://"))
        .ok_or_else(|| {
            crate::ui_text::t(
                "process.mcp.text22",
                "Enter an explicit loopback HTTP(S) origin.",
            )
        })?;
    let (hostport, path_part) = tail.split_once('/').unwrap_or((tail, ""));
    let (host, port) = hostport.split_once(':').unwrap_or((hostport, ""));
    if !matches!(host, "localhost" | "127.0.0.1")
        || !path_part.is_empty()
        || !port.is_empty() && port.parse::<u16>().ok().filter(|v| *v > 0).is_none()
        || !path.starts_with('/')
        || path.starts_with("//")
    {
        return Err(crate::ui_text::t("process.mcp.text11","Application MCP HTTP discovery currently requires an explicit localhost/127.0.0.1 origin.").into());
    }
    Ok(format!("{}{path}", base.trim_end_matches('/')))
}
fn http(url: &str) -> Result<Value, Error> {
    let mut id = 0;
    let mut session: Option<String> = None;
    let mut protocol = "2025-06-18".to_owned();
    discover(|method, params, notification| {
        id += 1;
        let payload = if notification {
            json!({"jsonrpc":"2.0","method":method,"params":params})
        } else {
            json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
        };
        let mut request = HttpRequest::post(url, payload.to_string().into_bytes())
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream");
        request.timeout_ms = Some(5000);
        if method != "initialize" {
            request = request.header("MCP-Protocol-Version", &protocol);
        }
        if let Some(session) = &session {
            request = request.header("Mcp-Session-Id", session);
        }
        if host::secret_is_set("application-mcp-token") {
            request = request.header("Authorization", "Bearer {{secret:application-mcp-token}}");
        }
        let raw = host::fetch(&request).map_err(|_| {
            Error::Failed(crate::ui_text::t(
                "process.mcp.text12",
                "Application MCP HTTP request failed; verify the explicit origin and capabilities.",
            ))
        })?;
        if notification && matches!(raw.status, 200 | 202 | 204) {
            return Ok(Value::Null);
        }
        if raw.status != 200 || raw.body.len() > 2 * 1024 * 1024 {
            return Err(crate::ui_text::t(
                "process.mcp.text13",
                "Application MCP HTTP metadata request was unsuccessful.",
            )
            .into());
        }
        let ids: Vec<_> = raw
            .headers
            .iter()
            .filter(|(key, _)| key.eq_ignore_ascii_case("mcp-session-id"))
            .collect();
        if ids.len() > 1 {
            return Err(Error::Stale);
        }
        if let Some((_, value)) = ids.first() {
            if !clean(value, 256) {
                return Err(Error::Stale);
            }
            session = Some(value.to_string());
        }
        let body = std::str::from_utf8(&raw.body).map_err(|_| Error::Stale)?;
        let value = if let Ok(value) = crate::strict_json::parse(body, 2 * 1024 * 1024) {
            value
        } else {
            let body = body.replace("\r\n", "\n");
            let mut found = None;
            for event in body.split("\n\n") {
                let data = event
                    .lines()
                    .filter_map(|line| line.strip_prefix("data:").map(str::trim_start))
                    .collect::<Vec<_>>()
                    .join("\n");
                if data.is_empty() {
                    continue;
                }
                let value =
                    crate::strict_json::parse(&data, 2 * 1024 * 1024).map_err(|_| Error::Stale)?;
                if value["id"].as_u64() == Some(id) {
                    if found.is_some() {
                        return Err(Error::Stale);
                    }
                    found = Some(value);
                }
            }
            found.ok_or_else(|| {
                crate::ui_text::t(
                    "process.mcp.text23",
                    "Application MCP HTTP response contains no matching JSON-RPC result.",
                )
            })?
        };
        let result = response(value, id)?;
        if method == "initialize" {
            protocol = result["protocolVersion"]
                .as_str()
                .ok_or(Error::Stale)?
                .to_owned();
        }
        Ok(result)
    })
}
#[derive(Default)]
pub(crate) struct State {
    projects: BTreeMap<String, Capture>,
}
struct Capture {
    runner: Runner,
    registration: Registration,
    identity: Value,
    value: Value,
}
impl State {
    pub(crate) fn connect(
        &mut self,
        ctx: &Invocation,
        runner: Runner,
        extra: &[String],
    ) -> Result<Value, Error> {
        let (project, root) = owner(ctx)?;
        let entries = registrations(&project, extra)?;
        let choices: Vec<_> = entries
            .iter()
            .enumerate()
            .map(|(n, r)| {
                openidea_plugin::PickItem::new(
                    &n.to_string(),
                    &format!("{} · {} · {}", r.transport, r.name, r.server),
                )
            })
            .collect();
        if choices.is_empty() {
            return Err(crate::ui_text::t(
                "process.mcp.text14",
                "No proven Laravel MCP web/local registrations found.",
            )
            .into());
        }
        let Some(choice) = host::pick(
            &crate::ui_text::t(
                "process.mcp.text16",
                "Connect to an application Laravel MCP server",
            ),
            &choices,
        )?
        else {
            return Err(Error::Cancelled);
        };
        let registration = entries
            .get(choice.parse::<usize>().map_err(|_| Error::Stale)?)
            .ok_or(Error::Stale)?
            .clone();
        let before = binding(&project, runner, &registration)?;
        if self.projects.len() >= 8 && !self.projects.contains_key(&root) {
            return Err(crate::ui_text::t(
                "process.mcp.text15",
                "Too many application MCP snapshots; reload the plugin.",
            )
            .into());
        }
        let value = if registration.transport == "local" {
            let argv = vec![
                "artisan".into(),
                "mcp:start".into(),
                registration.name.clone(),
            ];
            let command = std::iter::once(runner.name().to_owned())
                .chain(runner.argv(&project, &argv, false)?)
                .collect::<Vec<_>>()
                .join(" ");
            if !ui_kit::prompts::confirm_exact(
                &crate::ui_text::t(
                    "process.mcp.text18",
                    "Discover application MCP metadata (executes project PHP)?",
                ),
                &crate::ui_text::format(
                    "process.mcp.localSummary",
                    "Runs: {command}\nProject: {root}\nServer: {server} ({file})\nThis boots your application to list the server's tools, resources and prompts. No tool is called, and the process stops afterwards.",
                    &[("command", &command), ("root", &root), ("server", &registration.server), ("file", &registration.file)],
                ),
            )? {
                return Err(Error::Cancelled);
            }
            if binding(&project, runner, &registration)? != before {
                return Err(Error::Stale);
            }
            local(&project, runner, &argv)?
        } else {
            let Some(base) = host::input(
                &crate::ui_text::t("process.mcp.text17", "Application MCP loopback origin"),
                Some("http://127.0.0.1:8000"),
            )?
            else {
                return Err(Error::Cancelled);
            };
            let url = http_url(&base, &registration.name)?;
            if !ui_kit::prompts::confirm_exact(
                &crate::ui_text::t(
                    "process.mcp.text19",
                    "Discover application MCP HTTP metadata?",
                ),
                &crate::ui_text::format(
                    "process.mcp.httpReview",
                    "POST {url}\nProject: {root}\nRegistered server: {server}\nOnly MCP initialization and metadata lists are requested. URL-to-running-server association is explicitly selected by you, not automatically inferred. Optional application-mcp-token stays in the host keychain.",
                    &[
                        ("url", &url),
                        ("root", &root),
                        ("server", &registration.server),
                    ],
                ),
            )? {
                return Err(Error::Cancelled);
            }
            if binding(&project, runner, &registration)? != before {
                return Err(Error::Stale);
            }
            http(&url)?
        };
        if owner(ctx)?.1 != root || binding(&project, runner, &registration)? != before {
            return Err(Error::Stale);
        }
        let mut value = value;
        value["registration"] = json!({"name":registration.name,"transport":registration.transport,"server":registration.server,"file":registration.file});
        value["provenance"] = json!(
            "Explicit application MCP protocol metadata; no tool calls or resource contents requested"
        );
        self.projects.insert(
            root,
            Capture {
                runner,
                registration,
                identity: before,
                value: value.clone(),
            },
        );
        Ok(value)
    }
    pub(crate) fn cached(&self, ctx: &Invocation, runner: Runner) -> Result<Value, Error> {
        let (project, root) = owner(ctx)?;
        let Some(capture) = self.projects.get(&root) else {
            return Ok(json!({"tools":[],"resources":[],"prompts":[],"captured":false}));
        };
        if capture.runner != runner
            || binding(&project, runner, &capture.registration)? != capture.identity
        {
            return Err(Error::Stale);
        }
        Ok(capture.value.clone())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{HttpResponse, mock::MockHost};
    #[test]
    fn actual_facade_aliases_are_registered_but_decoys_foreign_types_and_dynamic_inputs_are_not() {
        let source = r#"<?php use Laravel\Mcp\Facades\Mcp as AI; use App\Mcp\Servers\WeatherServer as Weather; AI::local('weather',Weather::class); AI::web('/mcp/weather',Weather::class); $x="AI::local('fake',Weather::class)";"#;
        let values = parse(source, "/project/routes/ai.php");
        assert_eq!(values.len(), 2);
        assert_eq!(values[0].server, "App\\Mcp\\Servers\\WeatherServer");
        assert_eq!(values[1].transport, "web");
        assert!(
            parse(
                "<?php use Other\\Mcp; Mcp::local('bad',Server::class);",
                "/project/ai.php"
            )
            .is_empty()
        );
        assert!(
            parse(
                "<?php use Laravel\\Mcp\\Facades\\Mcp; Mcp::local($name,Server::class);",
                "/project/ai.php"
            )
            .is_empty()
        );
    }
    #[test]
    fn discovery_requests_metadata_only_and_paginates_without_tool_calls() {
        let mut calls = vec![];
        let result=discover(|method,params,notification|{calls.push(method.to_owned());Ok(match method{"initialize"=>json!({"protocolVersion":"2025-06-18","serverInfo":{"name":"Fixture","version":"1"},"capabilities":{"tools":{},"resources":{},"prompts":{}}}),"notifications/initialized"=>{assert!(notification);Value::Null},"tools/list" if params.get("cursor").is_none()=>json!({"tools":[{"name":"weather","inputSchema":{"type":"object"}}],"nextCursor":"2"}),"tools/list"=>json!({"tools":[]}),"resources/list"=>json!({"resources":[{"name":"forecast","uri":"weather://forecast"}]}),"prompts/list"=>json!({"prompts":[{"name":"daily","arguments":[{"name":"city","required":true}]}]}),_=>panic!("unexpected method {method}")})}).unwrap();
        assert_eq!(result["tools"][0]["name"], "weather");
        assert!(
            !calls
                .iter()
                .any(|m| m.contains("call") || m.contains("read") || m.contains("get"))
        );
    }
    #[test]
    fn http_origin_guard_and_secret_header_keep_remote_or_injected_origins_out() {
        let _guard = MockHost::install();
        assert!(http_url("http://127.0.0.1:8000", "/mcp").is_ok());
        for origin in [
            "https://evil.example",
            "http://localhost@evil.example",
            "http://localhost:0",
            "http://localhost/path",
        ] {
            assert!(http_url(origin, "/mcp").is_err());
        }
        let h = MockHost::install().with_capabilities(&["net:127.0.0.1"]);
        h.on_fetch(|r|{let value:Value=serde_json::from_slice(r.body.as_ref().unwrap()).unwrap();Ok(HttpResponse{status:200,headers:vec![],body:json!({"jsonrpc":"2.0","id":value["id"],"result":{"protocolVersion":"2025-06-18","serverInfo":{"name":"fixture"},"capabilities":{}}}).to_string().into_bytes()})});
        assert!(http("http://127.0.0.1:8000/mcp").is_ok());
    }
    #[test]
    #[ignore = "Requires disposable installed laravel/mcp fixture; see tests/fixtures/prepare-application-mcp.php"]
    fn installed_laravel_mcp_metadata_only_discovery() {
        use std::{
            io::{BufRead, BufReader, Write},
            process::{Command, Stdio},
            sync::mpsc,
        };
        let root = std::env::var("LARAVEL_APPLICATION_MCP_FIXTURE")
            .expect("set LARAVEL_APPLICATION_MCP_FIXTURE");
        let path = std::path::Path::new(&root);
        assert!(
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("laravel-mcp-milestone6-")
        );
        assert_eq!(
            std::fs::read_to_string(path.join("mcp-milestone6.fixture")).unwrap(),
            "openidea-laravel-mcp-fixture-v1"
        );
        assert!(!path.join("mcp-handler-executed.marker").exists());
        let mut child = Command::new("php")
            .args(["artisan", "mcp:start", "milestone"])
            .current_dir(path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut input = child.stdin.take().unwrap();
        let output = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let result = stdio_discover(
            |text| input.write_all(text.as_bytes()).map_err(|_| Error::Stale),
            || match rx.recv_timeout(Duration::from_millis(25)) {
                Ok(Ok(line)) => Ok((line + "\n", false)),
                Ok(Err(_)) => Err(Error::Stale),
                Err(mpsc::RecvTimeoutError::Timeout) => Ok((String::new(), false)),
                Err(_) => Ok((String::new(), true)),
            },
        );
        let _ = child.kill();
        drop(input);
        let _ = child.wait();
        let _ = reader.join();
        let value = result.expect("actual Laravel MCP discovery");
        assert_eq!(value["tools"][0]["name"], "milestone-weather");
        assert_eq!(
            value["tools"][0]["inputSchema"]["properties"]["city"]["type"],
            "string"
        );
        assert_eq!(value["resources"][0]["uri"], "fixture://guide");
        assert_eq!(value["prompts"][0]["name"], "milestone-summary");
        assert!(!path.join("mcp-handler-executed.marker").exists());
    }

    #[test]
    #[ignore = "Requires disposable installed Laravel MCP fixture and curl; starts loopback-only PHP server"]
    fn installed_laravel_mcp_http_metadata_only_discovery() {
        use std::{
            net::{TcpListener, TcpStream},
            process::{Command, Stdio},
        };
        let root = std::env::var("LARAVEL_APPLICATION_MCP_FIXTURE")
            .expect("set LARAVEL_APPLICATION_MCP_FIXTURE");
        let path = std::path::Path::new(&root);
        assert!(
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("laravel-mcp-milestone6-")
        );
        assert_eq!(
            std::fs::read_to_string(path.join("mcp-milestone6.fixture")).unwrap(),
            "openidea-laravel-mcp-fixture-v1"
        );
        assert!(!path.join("mcp-handler-executed.marker").exists());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let mut server = Command::new("php")
            .args([
                "-S",
                &format!("127.0.0.1:{port}"),
                "-t",
                ".",
                &path
                    .join("vendor/laravel/framework/src/Illuminate/Foundation/resources/server.php")
                    .to_string_lossy(),
            ])
            .current_dir(path.join("public"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while TcpStream::connect(("127.0.0.1", port)).is_err() {
            assert!(
                Instant::now() < deadline,
                "fixture HTTP server did not start"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        let h = MockHost::install().with_capabilities(&["net:127.0.0.1"]);
        h.on_fetch(|request| {
            let mut curl = Command::new("curl");
            curl.args([
                "--silent",
                "--show-error",
                "--max-time",
                "5",
                "--include",
                "--request",
                &request.method,
            ]);
            for (k, v) in &request.headers {
                curl.args(["--header", &format!("{k}: {v}")]);
            }
            if let Some(body) = &request.body {
                curl.args([
                    "--data-binary",
                    std::str::from_utf8(body).map_err(|_| Error::Stale)?,
                ]);
            }
            let output = curl.arg(&request.url).output().map_err(|_| Error::Stale)?;
            if !output.status.success() {
                return Err(Error::Stale);
            }
            let raw = String::from_utf8(output.stdout).map_err(|_| Error::Stale)?;
            let (header, body) = raw.split_once("\r\n\r\n").ok_or(Error::Stale)?;
            let status = header
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .and_then(|code| code.parse::<u16>().ok())
                .ok_or(Error::Stale)?;
            let headers = header
                .lines()
                .skip(1)
                .filter_map(|line| {
                    line.split_once(':')
                        .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
                })
                .collect();
            Ok(HttpResponse {
                status,
                headers,
                body: body.as_bytes().to_vec(),
            })
        });
        let result = http(&format!("http://127.0.0.1:{port}/mcp/milestone"));
        let _ = server.kill();
        let _ = server.wait();
        let value = result.expect("actual Laravel MCP HTTP discovery");
        assert_eq!(value["tools"][0]["name"], "milestone-weather");
        assert_eq!(value["resources"][0]["uri"], "fixture://guide");
        assert_eq!(value["prompts"][0]["name"], "milestone-summary");
        assert!(!path.join("mcp-handler-executed.marker").exists());
    }
}
