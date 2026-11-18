//! Hub > Request: runs one route through the app's HTTP kernel inside a PHP process,
//! without a server. Only an explicit Send executes project PHP. With rollback on,
//! the request is not sent unless a transaction really started (checked in PHP).
use crate::artisan_runner::Runner;
use openidea_plugin::{
    Error, Project, host,
    views::{Body, Button, Field, FieldValue, View},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};
use ui_kit::routes::Route;

pub(crate) const VIEW: &str = "laravel-request";
const MARKER: &str = "@@laravel-request@@";
const MAX_OUTPUT: usize = 1024 * 1024;
const MAX_BODY: usize = 16 * 1024;
const MAX_DRAFTS: usize = 8;
const CONFIRM_WINDOW: Duration = Duration::from_secs(30);
const SEND_BUDGET: Duration = Duration::from_secs(60);
const WRITE_METHODS: [&str; 4] = ["POST", "PUT", "PATCH", "DELETE"];
const NO_BODY_METHODS: [&str; 2] = ["GET", "HEAD"];

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Request {
    method: String,
    uri: String,
    query: String,
    body: Option<Value>,
    user: Option<String>,
    json: bool,
    rollback: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Response {
    status: u64,
    reason: String,
    headers: Vec<(String, String)>,
    body: String,
    size: u64,
    ms: f64,
    queries: Option<u64>,
    redirect: Option<String>,
    rolled_back: bool,
    warning: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Outcome {
    NotSent(String),
    Error(String),
    Response(Box<Response>),
}

pub(crate) enum Sent {
    NeedsConfirm,
    Done,
}

#[derive(Clone, Debug)]
struct Draft {
    label: String,
    template: String,
    path: String,
    methods: Vec<String>,
    method: String,
    params: Vec<(String, String)>,
    query: String,
    body: String,
    user: String,
    json: bool,
    rollback: bool,
    outcome: Option<Outcome>,
    confirm_since: Option<Instant>,
}

#[derive(Default)]
pub(crate) struct State {
    drafts: BTreeMap<String, Draft>,
}

fn is_write(method: &str) -> bool {
    WRITE_METHODS.contains(&method)
}

fn param_names(path: &str) -> Vec<(String, bool)> {
    let mut names: Vec<(String, bool)> = Vec::new();
    let mut rest = path;
    while let Some(open) = rest.find('{') {
        let Some(close) = rest[open..].find('}') else {
            break;
        };
        let inner = &rest[open + 1..open + close];
        let optional = inner.ends_with('?');
        let name = inner.trim_end_matches('?').split(':').next().unwrap_or_default();
        if !name.is_empty() && !names.iter().any(|(known, _)| known == name) {
            names.push((name.to_owned(), optional));
        }
        rest = &rest[open + close + 1..];
    }
    names
}

fn encode_segment(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => char::from(byte).to_string(),
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

fn fill_path(path: &str, params: &[(String, String)]) -> Result<String, String> {
    let mut uri = String::new();
    let mut rest = path;
    while let Some(open) = rest.find('{') {
        let Some(close) = rest[open..].find('}') else {
            break;
        };
        let inner = &rest[open + 1..open + close];
        let optional = inner.ends_with('?');
        let name = inner.trim_end_matches('?').split(':').next().unwrap_or_default();
        let value = params
            .iter()
            .find(|(known, _)| known == name)
            .map_or("", |(_, value)| value.trim());
        uri.push_str(&rest[..open]);
        if value.is_empty() && !optional {
            return Err(crate::ui_text::format("request.error.parameter", "Fill in the {parameter} parameter.", &[("parameter", name)]));
        }
        if value.is_empty() {
            if uri.ends_with('/') {
                uri.pop();
            }
        } else {
            uri.push_str(&encode_segment(value));
        }
        rest = &rest[open + close + 1..];
    }
    uri.push_str(rest);
    Ok(if uri.starts_with('/') { uri } else { format!("/{uri}") })
}

fn sample_json(literal: &str) -> Value {
    match literal {
        "true" => json!(true),
        "false" => json!(false),
        "[]" => json!([]),
        number if number.parse::<i64>().is_ok() => json!(number.parse::<i64>().unwrap_or_default()),
        text => json!(text.trim_matches('\'')),
    }
}

fn method_region<'a>(source: &'a str, method: &str) -> Option<&'a str> {
    let start = source.find(&format!("function {method}("))?;
    let rest = &source[start + 1..];
    let end = rest.find("function ").map_or(source.len(), |next| start + 1 + next);
    Some(&source[start..end])
}

fn inline_rules(source: &str, method: &str) -> Vec<(String, String)> {
    let Some(region) = method_region(source, method) else {
        return Vec::new();
    };
    let masked = crate::php::mask_comments(region);
    let Some(call) = masked.find("validate(") else {
        return Vec::new();
    };
    let Some(open) = masked[call..].find('[').map(|at| call + at) else {
        return Vec::new();
    };
    let Some(close) = crate::php::closing(&masked, open) else {
        return Vec::new();
    };
    crate::feature_test::rule_fields(&format!("function rules() {{ return {}; }}", &masked[open..=close]))
}

fn controller_source(project: &Project, class: &str) -> Option<String> {
    let relative = class.trim_start_matches('\\').replace('\\', "/");
    if let Ok(source) = project.read_file(&format!("app/Http/Controllers/{relative}.php")) {
        return Some(source);
    }
    let short = relative.rsplit('/').next()?;
    let paths = project.find_files(&format!("app/**/{short}.php"), 5).ok()?;
    paths.iter().find_map(|path| project.read_file(path).ok())
}

/// Request-body sample from the Form Request `rules()` or an inline `validate()` call, as pretty JSON.
pub(crate) fn body_template(project: &Project, route: &Route) -> String {
    let Some(member) = &route.handler.member else {
        return String::new();
    };
    let Some(source) = controller_source(project, &route.handler.class) else {
        return String::new();
    };
    let mut fields = crate::feature_test::form_request_fields(project, &source, member);
    if fields.is_empty() {
        fields = inline_rules(&source, member);
    }
    if fields.is_empty() {
        return String::new();
    }
    let body: serde_json::Map<String, Value> = fields.into_iter().map(|(name, literal)| (name, sample_json(&literal))).collect();
    serde_json::to_string_pretty(&body).unwrap_or_default()
}

fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

fn form_pairs(prefix: &str, value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => map.iter().for_each(|(key, item)| form_pairs(&if prefix.is_empty() { key.clone() } else { format!("{prefix}[{key}]") }, item, out)),
        Value::Array(items) => items.iter().enumerate().for_each(|(at, item)| form_pairs(&format!("{prefix}[{at}]"), item, out)),
        Value::String(text) => out.push(format!("{prefix}={text}")),
        Value::Null => out.push(format!("{prefix}=")),
        other => out.push(format!("{prefix}={other}")),
    }
}

impl Request {
    pub(crate) fn curl(&self, base: &str) -> String {
        let mut url = format!("{}{}", base.trim_end_matches('/'), self.uri);
        if !self.query.is_empty() {
            url.push('?');
            url.push_str(&self.query);
        }
        let accept = if self.json { "application/json" } else { "text/html,application/xhtml+xml" };
        let mut parts = vec![
            "curl".to_owned(),
            if self.method == "HEAD" { "-I".to_owned() } else { format!("-X {}", self.method) },
            quote(&url),
            "-H".into(),
            quote(&format!("Accept: {accept}")),
        ];
        let Some(body) = self.body.as_ref().filter(|_| !NO_BODY_METHODS.contains(&self.method.as_str())) else {
            return parts.join(" ");
        };
        if self.json {
            parts.extend(["-H".into(), quote("Content-Type: application/json"), "--data".into(), quote(&body.to_string())]);
            return parts.join(" ");
        }
        let mut pairs = Vec::new();
        form_pairs("", body, &mut pairs);
        for pair in pairs {
            parts.extend(["--data-urlencode".into(), quote(&pair)]);
        }
        parts.join(" ")
    }

    pub(crate) fn php_args(&self) -> Vec<String> {
        let input = json!({
            "method": self.method,
            "uri": self.uri,
            "query": self.query,
            "body": self.body,
            "user": self.user,
            "json": self.json,
            "rollback": self.rollback,
        });
        vec![
            "-r".into(),
            include_str!("route_request.php").trim_start_matches("<?php").into(),
            input.to_string(),
        ]
    }
}

fn mismatch_warning() -> String {
    crate::ui_text::t(
        "request.warning.rollback",
        "Roll back was on, but its transaction was not open when the request finished (the app ended it itself), so its database changes may have been kept.",
    )
}

pub(crate) fn parse_output(raw: &str, rollback: bool) -> Outcome {
    let Some(at) = raw.rfind(MARKER) else {
        return Outcome::Error(crate::ui_text::t(
            if rollback { "request.error.noReplyRollback" } else { "request.error.noReply" },
            if rollback {
                "The app ended the PHP process without a response. Roll back was on, so the database discards the unfinished transaction when its connection closes."
            } else {
                "The app ended the PHP process without a response."
            },
        ));
    };
    let line = raw[at + MARKER.len()..].lines().next().unwrap_or_default();
    let Ok(reply) = serde_json::from_str::<Value>(line) else {
        return Outcome::Error(crate::ui_text::t("request.error.badReply", "The request helper returned an unreadable reply."));
    };
    let rolled_back = reply["rolledBack"] == json!(true);
    if reply["notSent"] == json!(true) {
        return Outcome::NotSent(reply["error"].as_str().unwrap_or("Nothing was sent.").chars().take(600).collect());
    }
    if let Some(error) = reply["error"].as_str() {
        let note = if rollback && !rolled_back { format!(" {}", mismatch_warning()) } else { String::new() };
        return Outcome::Error(format!("{}{note}", error.chars().take(600).collect::<String>()));
    }
    let Some(status) = reply["status"].as_u64() else {
        return Outcome::Error(crate::ui_text::t("request.error.badReply", "The request helper returned an unreadable reply."));
    };
    let headers = reply["headers"]
        .as_object()
        .map(|map| map.iter().take(64).map(|(name, value)| (name.clone(), value.as_str().unwrap_or_default().chars().take(2000).collect())).collect())
        .unwrap_or_default();
    Outcome::Response(Box::new(Response {
        status,
        reason: reply["reason"].as_str().unwrap_or_default().to_owned(),
        headers,
        body: reply["body"].as_str().unwrap_or_default().chars().take(200_000).collect(),
        size: reply["size"].as_u64().unwrap_or_default(),
        ms: reply["ms"].as_f64().unwrap_or_default(),
        queries: reply["queries"].as_u64(),
        redirect: reply["redirect"].as_str().map(str::to_owned),
        rolled_back,
        warning: (rollback && !rolled_back).then(mismatch_warning),
    }))
}

pub(crate) fn execute(project: &Project, runner: Runner, args: &[String]) -> Result<String, Error> {
    runner.verify(project)?;
    let args = runner.argv(project, args, false)?;
    let task = host::begin_task(crate::ui_text::text("request.task", "Send Laravel request"), true);
    let child = crate::artisan_runner::start_background(runner, &args, project)?;
    if let Err(error) = child.close_stdin() {
        let _ = child.kill();
        return Err(error);
    }
    let started = Instant::now();
    let mut output = String::new();
    loop {
        if task.is_cancelled() {
            let _ = child.kill();
            return Err(Error::Cancelled);
        }
        if started.elapsed() > SEND_BUDGET {
            let _ = child.kill();
            return Err(Error::Failed(crate::ui_text::t("request.error.timeout", "The request ran longer than 60 seconds and was stopped.")));
        }
        let chunk = child.wait(250)?;
        output.push_str(&chunk.stdout);
        if output.len() > MAX_OUTPUT {
            let _ = child.kill();
            return Err(Error::Invalid(crate::ui_text::t("request.error.output", "The request output exceeded 1 MiB.")));
        }
        if chunk.status.is_some() && chunk.stdout.is_empty() && chunk.stderr.is_empty() {
            return Ok(output);
        }
    }
}

fn invalid(message: String) -> Error {
    Error::Invalid(message)
}

fn size_text(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    format!("{:.1} KiB", bytes as f64 / 1024.0)
}

impl Draft {
    fn new(route: &Route, template: String) -> Self {
        let methods: Vec<String> = route
            .method
            .split('|')
            .map(|method| method.trim().to_ascii_uppercase())
            .filter(|method| !method.is_empty() && (method != "HEAD" || route.method == "HEAD"))
            .collect();
        let methods = if methods.is_empty() { vec!["GET".to_owned()] } else { methods };
        let path = route.path.clone();
        Self {
            label: format!("{} {}  {}", route.method, route.path, handler_text(route)),
            params: param_names(&path).into_iter().map(|(name, _)| (name, String::new())).collect(),
            method: methods[0].clone(),
            methods,
            path,
            template: template.clone(),
            query: String::new(),
            body: template,
            user: String::new(),
            json: route.path.trim_start_matches('/').starts_with("api"),
            rollback: true,
            outcome: None,
            confirm_since: None,
        }
    }

    fn request(&self) -> Result<Request, Error> {
        let uri = fill_path(&self.path, &self.params).map_err(invalid)?;
        let query = self.query.trim().trim_start_matches('?').to_owned();
        if query.len() > 4096 || query.chars().any(|c| c.is_control() || c == ' ') {
            return Err(invalid(crate::ui_text::t("request.error.query", "Enter the query as key=value pairs joined with &, without spaces.")));
        }
        let user = self.user.trim();
        if user.len() > 64 || !user.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') {
            return Err(invalid(crate::ui_text::t("request.error.user", "The user id may contain letters, digits, - and _ (up to 64).")));
        }
        let mut body = None;
        let text = self.body.trim();
        if !text.is_empty() && !NO_BODY_METHODS.contains(&self.method.as_str()) {
            if text.len() > MAX_BODY {
                return Err(invalid(crate::ui_text::t("request.error.bodySize", "The body exceeds 16 KiB.")));
            }
            let value: Value = serde_json::from_str(text).map_err(|error| invalid(crate::ui_text::format("request.error.json", "The body is not valid JSON: {error}", &[("error", &error.to_string())])))?;
            if !value.is_object() && !value.is_array() {
                return Err(invalid(crate::ui_text::t("request.error.jsonShape", "The body must be a JSON object or array.")));
            }
            body = Some(value);
        }
        Ok(Request {
            method: self.method.clone(),
            uri,
            query,
            body,
            user: (!user.is_empty()).then(|| user.to_owned()),
            json: self.json,
            rollback: self.rollback,
        })
    }

    fn confirm_pending(&self) -> bool {
        self.confirm_since.is_some_and(|since| since.elapsed() < CONFIRM_WINDOW)
    }

    fn fields(&self) -> Vec<Field> {
        let mut fields = vec![Field::new("route", crate::ui_text::text("request.field.route", "Route"), FieldValue::readonly_text(&self.label, false))];
        fields.push(Field::new(
            "method",
            crate::ui_text::text("request.field.method", "Method"),
            FieldValue::Choice(self.methods.clone(), self.method.clone()),
        ));
        for (name, value) in &self.params {
            fields.push(Field::new(&format!("param:{name}"), name, FieldValue::Text(value.clone())));
        }
        fields.extend([
            Field::new("query", crate::ui_text::text("request.field.query", "Query"), FieldValue::Text(self.query.clone())),
            Field::new("body", crate::ui_text::text("request.field.body", "JSON body (POST, PUT, PATCH, DELETE)"), FieldValue::Multiline(self.body.clone())),
            Field::new("user", crate::ui_text::text("request.field.user", "Act as user id"), FieldValue::Text(self.user.clone())),
            Field::new("json", crate::ui_text::text("request.field.json", "Accept JSON"), FieldValue::Checkbox(self.json)),
            Field::new(
                "rollback",
                crate::ui_text::text("request.field.rollback", "Roll back database changes (default connection)"),
                FieldValue::Checkbox(self.rollback),
            ),
        ]);
        let result = crate::ui_text::text("request.field.result", "Result");
        match &self.outcome {
            None => {}
            Some(Outcome::NotSent(reason)) => fields.push(Field::new("result", result, FieldValue::readonly_text(reason, true))),
            Some(Outcome::Error(message)) => fields.push(Field::new("result", result, FieldValue::readonly_text(message, true))),
            Some(Outcome::Response(response)) => {
                let queries = response.queries.map_or_else(
                    || crate::ui_text::t("request.queriesNotCounted", "queries not counted"),
                    |count| crate::ui_text::format("request.queries", "{count} queries", &[("count", &count.to_string())]),
                );
                let mut summary = format!("{} {} · {:.1} ms · {queries} · {}", response.status, response.reason, response.ms, size_text(response.size));
                if self.rollback && response.rolled_back {
                    summary.push_str(&format!(" · {}", crate::ui_text::t("request.rolledBack", "changes rolled back")));
                }
                fields.push(Field::new("result", result, FieldValue::readonly_text(&summary, false)));
                if let Some(warning) = &response.warning {
                    fields.push(Field::new("warning", crate::ui_text::text("request.field.warning", "Warning"), FieldValue::readonly_text(warning, true)));
                }
                if let Some(redirect) = &response.redirect {
                    fields.push(Field::new("redirect", crate::ui_text::text("request.field.redirect", "Redirect"), FieldValue::readonly_text(redirect, false)));
                }
                let headers = response.headers.iter().map(|(name, value)| format!("{name}: {value}")).collect::<Vec<_>>().join("\n");
                fields.push(Field::new("headers", crate::ui_text::text("request.field.headers", "Response headers"), FieldValue::readonly_text(&headers, true)));
                fields.push(Field::new("response-body", crate::ui_text::text("request.field.responseBody", "Response body"), FieldValue::readonly_text(&response.body, true)));
            }
        }
        fields
    }
}

fn handler_text(route: &Route) -> String {
    match &route.handler.member {
        Some(member) => format!("{}@{member}", route.handler.class),
        None => route.handler.class.clone(),
    }
}

impl State {
    pub(crate) fn select(&mut self, root: &str, route: &Route, template: String) -> Result<(), Error> {
        if !self.drafts.contains_key(root) && self.drafts.len() >= MAX_DRAFTS {
            return Err(invalid(crate::ui_text::t("request.error.sessions", "Too many request drafts; reload the plugin.")));
        }
        self.drafts.insert(root.to_owned(), Draft::new(route, template));
        Ok(())
    }

    pub(crate) fn has_draft(&self, root: &str) -> bool {
        self.drafts.contains_key(root)
    }

    pub(crate) fn set_field(&mut self, root: &str, key: &str, value: &str) -> Result<(), Error> {
        let draft = self.drafts.get_mut(root).ok_or(Error::Stale)?;
        draft.confirm_since = None;
        match key {
            "method" if draft.methods.iter().any(|method| method == value) => draft.method = value.to_owned(),
            "query" => draft.query = value.chars().take(8192).collect(),
            "body" => draft.body = value.chars().take(4 * MAX_BODY).collect(),
            "user" => draft.user = value.chars().take(128).collect(),
            "json" => draft.json = value == "true",
            "rollback" => draft.rollback = value == "true",
            _ => match key.strip_prefix("param:").and_then(|name| draft.params.iter_mut().find(|(known, _)| known == name)) {
                Some((_, slot)) => *slot = value.chars().take(512).collect(),
                None => {}
            },
        }
        Ok(())
    }

    pub(crate) fn reset_body(&mut self, root: &str) -> Result<(), Error> {
        let draft = self.drafts.get_mut(root).ok_or(Error::Stale)?;
        draft.body = draft.template.clone();
        draft.confirm_since = None;
        Ok(())
    }

    pub(crate) fn request(&self, root: &str) -> Result<Request, Error> {
        self.drafts.get(root).ok_or(Error::Stale)?.request()
    }

    pub(crate) fn send(&mut self, root: &str, run: impl FnOnce(&[String]) -> Result<String, Error>) -> Result<Sent, Error> {
        let draft = self.drafts.get_mut(root).ok_or(Error::Stale)?;
        let request = draft.request()?;
        if is_write(&request.method) && !request.rollback && !draft.confirm_pending() {
            draft.confirm_since = Some(Instant::now());
            draft.outcome = None;
            return Ok(Sent::NeedsConfirm);
        }
        draft.confirm_since = None;
        draft.outcome = None;
        let raw = run(&request.php_args())?;
        draft.outcome = Some(parse_output(&raw, request.rollback));
        Ok(Sent::Done)
    }

    pub(crate) fn render(&self, root: &str) -> View {
        let Some(draft) = self.drafts.get(root) else {
            return View::message(crate::ui_text::text("request.empty", "Choose a route in Routes and run Send Request… to build a request."));
        };
        let confirming = draft.confirm_pending();
        let send_label = if confirming {
            crate::ui_text::text("request.sendConfirm", "Send for Real (keeps database changes)")
        } else {
            crate::ui_text::text("request.send", "Send Request")
        };
        let mut view = View::new(Body::Form(draft.fields()))
            .button(Button::new("request-send", send_label).icon(if confirming { "warning" } else { "run" }))
            .button(Button::new("request-copy-curl", crate::ui_text::text("action.route-copy-curl", "Copy as cURL")).icon("copy"));
        if !draft.template.is_empty() {
            view = view.button(Button::new("request-reset-body", crate::ui_text::text("request.resetBody", "Reset Body from Rules")).icon("refresh"));
        }
        if confirming {
            view = view.status(&crate::ui_text::format(
                "request.confirmStatus",
                "Roll back is off: {method} {path} will change your database. Click again within 30 seconds to send it.",
                &[("method", &draft.method), ("path", &draft.path)],
            ));
        }
        view
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui_kit::routes::Handler;

    fn route(method: &str, path: &str) -> Route {
        Route {
            method: method.into(),
            path: path.into(),
            handler: Handler { class: "PostController".into(), member: Some("store".into()) },
            name: None,
            location: None,
        }
    }

    fn state(method: &str, path: &str) -> State {
        let mut state = State::default();
        state.select("/p", &route(method, path), "{\n  \"title\": \"Example\"\n}".into()).unwrap();
        state
    }

    const NOT_SENT: &str = r#"noise
@@laravel-request@@{"notSent":true,"error":"Nothing was sent: Roll back database changes is on, but no transaction could be started on the default connection (boom)."}
"#;
    const SENT: &str = r#"@@laravel-request@@{"status":201,"reason":"Created","headers":{"content-type":"application/json","set-cookie":"s=•••"},"body":"{}","size":2,"ms":4.5,"queries":null,"redirect":null,"rollbackRequested":true,"rolledBack":true}
"#;

    #[test]
    fn no_transaction_means_not_sent_and_rollback_stays_on() {
        let _host = openidea_plugin::mock::MockHost::install();
        let mut state = state("POST", "/posts");
        let mut args_seen = Vec::new();
        state.send("/p", |args| { args_seen = args.to_vec(); Ok(NOT_SENT.into()) }).unwrap();
        let sent: Value = serde_json::from_str(args_seen.last().unwrap()).unwrap();
        assert_eq!(sent["rollback"], json!(true));
        let draft = &state.drafts["/p"];
        assert!(draft.rollback);
        let Some(Outcome::NotSent(reason)) = &draft.outcome else { panic!("expected not sent") };
        assert!(reason.starts_with("Nothing was sent:"));
        let fields = draft.fields();
        assert!(fields.iter().all(|field| field.key != "response-body" && field.key != "headers"));
    }

    #[test]
    fn rollback_off_write_needs_a_second_click_before_anything_runs() {
        let _host = openidea_plugin::mock::MockHost::install();
        let mut state = state("POST", "/posts");
        state.send("/p", |_| Ok(NOT_SENT.into())).unwrap();
        state.set_field("/p", "rollback", "false").unwrap();
        let mut runs = 0;
        assert!(matches!(state.send("/p", |_| { runs += 1; Ok(SENT.into()) }).unwrap(), Sent::NeedsConfirm));
        assert_eq!(runs, 0);
        assert!(matches!(state.send("/p", |args| { runs += 1; assert!(args.last().unwrap().contains(r#""rollback":false"#)); Ok(SENT.into()) }).unwrap(), Sent::Done));
        assert_eq!(runs, 1);
    }

    #[test]
    fn editing_after_the_first_click_asks_again() {
        let _host = openidea_plugin::mock::MockHost::install();
        let mut state = state("DELETE", "/posts/{post}");
        state.set_field("/p", "param:post", "7").unwrap();
        state.set_field("/p", "rollback", "false").unwrap();
        assert!(matches!(state.send("/p", |_| panic!("must not run")).unwrap(), Sent::NeedsConfirm));
        state.set_field("/p", "query", "a=1").unwrap();
        assert!(matches!(state.send("/p", |_| panic!("must not run")).unwrap(), Sent::NeedsConfirm));
    }

    #[test]
    fn rollback_on_and_reads_send_on_the_first_click() {
        let _host = openidea_plugin::mock::MockHost::install();
        let mut state = state("POST", "/posts");
        assert!(matches!(state.send("/p", |_| Ok(SENT.into())).unwrap(), Sent::Done));
        let mut reads = state_for_get();
        reads.set_field("/p", "rollback", "false").unwrap();
        assert!(matches!(reads.send("/p", |_| Ok(SENT.into())).unwrap(), Sent::Done));
    }

    fn state_for_get() -> State {
        state("GET|HEAD", "/posts")
    }

    #[test]
    fn a_response_without_a_rolled_back_transaction_is_flagged_not_trusted() {
        let _host = openidea_plugin::mock::MockHost::install();
        let reply = r#"@@laravel-request@@{"status":200,"reason":"OK","headers":{},"body":"","size":0,"ms":1,"queries":2,"rolledBack":false}"#;
        let Outcome::Response(response) = parse_output(reply, true) else { panic!("response") };
        assert!(response.warning.is_some());
        let Outcome::Response(response) = parse_output(reply, false) else { panic!("response") };
        assert!(response.warning.is_none());
    }

    #[test]
    fn a_failed_query_count_does_not_turn_rollback_off() {
        let _host = openidea_plugin::mock::MockHost::install();
        let Outcome::Response(response) = parse_output(SENT, true) else { panic!("response") };
        assert_eq!(response.queries, None);
        assert!(response.rolled_back);
        assert!(response.warning.is_none());
        let mut state = state("POST", "/posts");
        state.send("/p", |_| Ok(SENT.into())).unwrap();
        let summary = state.drafts["/p"].fields().into_iter().find(|field| field.key == "result").unwrap();
        let FieldValue::Other { data, .. } = summary.value else { panic!("readonly text") };
        assert!(data.contains("queries not counted") && data.contains("rolled back"));
    }

    #[test]
    fn missing_reply_and_errors_are_not_responses() {
        let _host = openidea_plugin::mock::MockHost::install();
        assert!(matches!(parse_output("PHP Fatal error", true), Outcome::Error(_)));
        let boom = r#"@@laravel-request@@{"error":"RuntimeException: x","rollbackRequested":true,"rolledBack":true}"#;
        assert!(matches!(parse_output(boom, true), Outcome::Error(message) if message.starts_with("RuntimeException")));
    }

    #[test]
    fn cookies_in_the_reply_stay_masked_and_headers_are_bounded() {
        let _host = openidea_plugin::mock::MockHost::install();
        let Outcome::Response(response) = parse_output(SENT, true) else { panic!("response") };
        assert!(response.headers.iter().any(|(name, value)| name == "set-cookie" && value == "s=•••"));
    }

    const STUBS: &str = r#"<?php
namespace Illuminate\Contracts\Http { interface Kernel {} }
namespace Illuminate\Http { class Request { public static function create($uri, $method, $p, $c, $f, $s, $content) { return new static; } } }
namespace Symfony\Component\HttpFoundation {
    class Response { public static $statusTexts = [200 => 'OK']; public $headers; function __construct() { $this->headers = new Headers; } function getStatusCode() { return 200; } function getContent() { return 'ok'; } }
    class Headers { function all() { return ['content-type' => ['text/plain'], 'set-cookie' => ['secret=hunter2'], 'authorization' => ['Bearer x']]; } function getCookies() { return [new Cookie]; } function get($n) { return null; } }
    class Cookie { function getName() { return 'session'; } }
}
namespace {
    function mode() { return trim(file_get_contents(getcwd().'/mode.txt')); }
    class Conn {
        public $level = 0;
        function beginTransaction() { if (mode() === 'throws') { throw new RuntimeException('no tx'); } if (mode() !== 'level0') { $this->level++; } }
        function transactionLevel() { return $this->level; }
        function rollBack() { $this->level--; file_put_contents(getcwd().'/rolledback.marker', '1'); }
        function listen($cb) { if (mode() === 'nolisten') { throw new RuntimeException('no listen'); } }
    }
    class Db { public $c; function connection() { return $this->c ??= new Conn; } }
    class Kernel implements Illuminate\Contracts\Http\Kernel {
        function bootstrap() {}
        function handle($r) { file_put_contents(getcwd().'/sent.marker', '1'); return new Symfony\Component\HttpFoundation\Response; }
        function terminate($r, $s) {}
    }
    class App implements ArrayAccess {
        public $db; function __construct() { $this->db = new Db; }
        function make($c) { return new Kernel; } function instance($a, $b) {}
        function offsetGet($k): mixed { return $this->db; } function offsetExists($k): bool { return true; } function offsetSet($k, $v): void {} function offsetUnset($k): void {}
    }
}
"#;

    fn run_php(mode: &str, rollback: bool) -> Option<(String, bool)> {
        let dir = std::env::temp_dir().join(format!("laravel-request-{}-{mode}-{rollback}", std::process::id()));
        std::fs::create_dir_all(dir.join("vendor")).ok()?;
        std::fs::create_dir_all(dir.join("bootstrap")).ok()?;
        std::fs::write(dir.join("vendor/autoload.php"), STUBS).ok()?;
        std::fs::write(dir.join("bootstrap/app.php"), "<?php return new App;").ok()?;
        std::fs::write(dir.join("mode.txt"), mode).ok()?;
        let request = Request { method: "POST".into(), uri: "/posts".into(), query: String::new(), body: Some(json!({"a": 1})), user: None, json: true, rollback };
        let args = request.php_args();
        let output = std::process::Command::new("php").args(&args).current_dir(&dir).output().ok()?;
        let sent = dir.join("sent.marker").exists();
        let _ = std::fs::remove_dir_all(&dir);
        Some((String::from_utf8_lossy(&output.stdout).into_owned(), sent))
    }

    #[test]
    fn php_never_sends_with_rollback_on_unless_a_transaction_started() {
        let _host = openidea_plugin::mock::MockHost::install();
        for mode in ["throws", "level0"] {
            let Some((output, sent)) = run_php(mode, true) else { return };
            assert!(!sent, "{mode}: the kernel must not run");
            assert!(matches!(parse_output(&output, true), Outcome::NotSent(reason) if reason.starts_with("Nothing was sent:")), "{mode}: {output}");
        }
        let (output, sent) = run_php("throws", false).unwrap();
        assert!(sent, "rollback off sends for real");
        assert!(matches!(parse_output(&output, false), Outcome::Response(_)));
    }

    #[test]
    fn php_rolls_back_masks_secrets_and_survives_a_failed_query_count() {
        let _host = openidea_plugin::mock::MockHost::install();
        let Some((output, sent)) = run_php("ok", true) else { return };
        assert!(sent);
        let Outcome::Response(response) = parse_output(&output, true) else { panic!("{output}") };
        assert!(response.rolled_back && response.warning.is_none() && response.queries == Some(0));
        assert!(!output.contains("hunter2") && !output.contains("Bearer"));
        assert!(response.headers.iter().any(|(name, value)| name == "set-cookie" && value == "session=•••"));
        let (output, _) = run_php("nolisten", true).unwrap();
        let Outcome::Response(response) = parse_output(&output, true) else { panic!("{output}") };
        assert_eq!(response.queries, None);
        assert!(response.rolled_back, "a failed count must not turn rollback off");
    }

    #[test]
    fn path_parameters_are_filled_encoded_and_required() {
        let _host = openidea_plugin::mock::MockHost::install();
        let params = vec![("post".to_owned(), "a b/1".to_owned()), ("tag".to_owned(), String::new())];
        assert_eq!(fill_path("/posts/{post:slug}/tags/{tag?}", &params).unwrap(), "/posts/a%20b%2F1/tags");
        assert!(fill_path("/posts/{post}", &[("post".into(), " ".into())]).is_err());
    }

    #[test]
    fn curl_is_built_from_the_same_request_that_is_sent() {
        let _host = openidea_plugin::mock::MockHost::install();
        let mut state = state("POST", "/posts/{post}");
        state.set_field("/p", "param:post", "7").unwrap();
        state.set_field("/p", "query", "draft=1").unwrap();
        state.set_field("/p", "json", "true").unwrap();
        let request = state.request("/p").unwrap();
        assert_eq!(
            request.curl("http://localhost:8000/"),
            r#"curl -X POST 'http://localhost:8000/posts/7?draft=1' -H 'Accept: application/json' -H 'Content-Type: application/json' --data '{"title":"Example"}'"#
        );
        let sent: Value = serde_json::from_str(request.php_args().last().unwrap()).unwrap();
        assert_eq!((sent["uri"].as_str(), sent["query"].as_str(), sent["body"]["title"].as_str()), (Some("/posts/7"), Some("draft=1"), Some("Example")));
        state.set_field("/p", "json", "false").unwrap();
        assert!(state.request("/p").unwrap().curl("http://h").contains("--data-urlencode 'title=Example'"));
    }

    #[test]
    fn invalid_input_is_refused_before_anything_runs() {
        let _host = openidea_plugin::mock::MockHost::install();
        let mut state = state("POST", "/posts");
        state.set_field("/p", "body", "{nope").unwrap();
        assert!(state.send("/p", |_| panic!("must not run")).is_err());
        state.set_field("/p", "body", "{}").unwrap();
        state.set_field("/p", "user", "1; drop").unwrap();
        assert!(state.send("/p", |_| panic!("must not run")).is_err());
        state.set_field("/p", "user", "").unwrap();
        state.set_field("/p", "query", "a b").unwrap();
        assert!(state.send("/p", |_| panic!("must not run")).is_err());
    }

    #[test]
    fn body_samples_come_from_form_requests_and_inline_validation() {
        use openidea_plugin::mock::MockHost;
        let host = MockHost::install();
        host.add_file(
            "app/Http/Controllers/PostController.php",
            "<?php class PostController { public function store(StorePostRequest $request) {} public function update(Request $request) { $request->validate(['title' => 'required|string', 'views' => 'integer', 'published' => 'boolean']); } }",
        );
        host.add_file("app/Http/Requests/StorePostRequest.php", "<?php class StorePostRequest { public function rules() { return ['email' => 'required|email', 'age' => 'integer']; } }");
        let project = host::projects().unwrap().remove(0);
        let store = route("POST", "/posts");
        assert_eq!(serde_json::from_str::<Value>(&body_template(&project, &store)).unwrap(), json!({"email": "user@example.com", "age": 1}));
        let mut update = route("PUT", "/posts/{post}");
        update.handler.member = Some("update".into());
        assert_eq!(serde_json::from_str::<Value>(&body_template(&project, &update)).unwrap(), json!({"title": "text", "views": 1, "published": true}));
    }
}
