//! What the running application's Actuator endpoints said, parsed into a
//! small model (Spring Boot 2.x and 3.x JSON shapes) that is kept in memory
//! and in storage.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use spring_text::config::canonical;
use std::collections::HashMap;

/// The endpoints read, in order; the first one tells whether the application is up.
pub const ENDPOINTS: [&str; 9] = ["beans", "env", "conditions", "health", "mappings", "configprops", "scheduledtasks", "loggers", "caches"];
/// What Actuator shows for a value it hides.
pub const MASK: &str = "******";
pub const CONFIGPROPS_VALUES: usize = 256;
pub const HEALTH_DEPTH: usize = 4;
pub const HEALTH_NODES: usize = 200;

/// Control characters dropped, length bounded.
fn health_text(s: &str, max: usize) -> String {
    let clean: String = s.chars().filter(|c| !c.is_control()).collect();
    if clean.chars().count() <= max {
        return clean;
    }
    format!("{}…", clean.chars().take(max).collect::<String>())
}

/// A bound or reported value as it may be shown: sensitive keys hidden, URL userinfo masked, bounded.
pub fn masked_value(key: &str, raw: &str) -> String {
    if sensitive_key(key) {
        return MASK.to_owned();
    }
    let command_line = matches!(key, "sun.java.command" | "JAVA_TOOL_OPTIONS" | "JDK_JAVA_OPTIONS" | "_JAVA_OPTIONS");
    let raw = if command_line { mask_arguments(raw) } else { raw.to_owned() };
    health_text(&mask_userinfo(&raw), 200)
}

/// `--db.password=x -Dapi.key=y` becomes `--db.password=****** -Dapi.key=******`, as InputArguments are shown.
fn mask_arguments(raw: &str) -> String {
    raw.split(' ')
        .map(|arg| match arg.split_once('=') {
            Some((key, _)) if sensitive_key(key) => format!("{key}={MASK}"),
            _ => arg.to_owned(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Boot's Sanitizer rule, widened: the last word of the last key segment names a secret.
/// Words split on `-`, `_` and camel/acronym boundaries (`ABCKey` is `ABC`, `Key`).
pub fn sensitive_key(key: &str) -> bool {
    const ENDINGS: [&str; 6] = ["password", "passwd", "secret", "token", "credentials", "credential"];
    const KEYS: [&str; 8] = ["key", "apikey", "accesskey", "secretkey", "privatekey", "signingkey", "encryptionkey", "masterkey"];
    let segment = key.rsplit('.').next().unwrap_or(key);
    let segment = segment.split('[').next().unwrap_or(segment);
    let chars: Vec<char> = segment.chars().collect();
    let mut last_word = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if matches!(c, '-' | '_') {
            last_word.clear();
            continue;
        }
        let previous = i.checked_sub(1).map(|p| chars[p]);
        let next_lower = chars.get(i + 1).is_some_and(|n| n.is_lowercase());
        let camel = previous.is_some_and(|p| p.is_lowercase() || p.is_ascii_digit());
        let acronym_end = previous.is_some_and(char::is_uppercase) && next_lower;
        if c.is_uppercase() && (camel || acronym_end) {
            last_word.clear();
        }
        last_word.extend(c.to_lowercase());
    }
    ENDINGS.iter().any(|e| last_word.ends_with(e)) || last_word == "authorization" || KEYS.contains(&last_word.as_str())
}

/// `scheme://user:pw@host` becomes `scheme://******@host`, also when the password holds `/`, `?` or `#`.
pub(crate) fn mask_userinfo(v: &str) -> String {
    let mut masked = String::new();
    let mut rest = v;
    while let Some(i) = rest.find("://") {
        masked.push_str(&rest[..i + 3]);
        rest = &rest[i + 3..];
        if let Some(at) = userinfo_end(rest) {
            masked.push_str(MASK);
            rest = &rest[at..];
        }
    }
    masked.push_str(rest);
    masked
}

/// Where the userinfo of the text after `://` ends (its last `@`), if it has one.
fn userinfo_end(after_scheme: &str) -> Option<usize> {
    let token = after_scheme.split(char::is_whitespace).next().unwrap_or("");
    let authority = token.split(['/', '?', '#']).next().unwrap_or("");
    if let Some(at) = authority.rfind('@') {
        return Some(at);
    }
    // ponytail: `user:pa/ss@host` is told apart from `host:5432/db?u=a@b` only by a non-numeric text after the colon;
    // an all-digit password cut by `/`, `?` or `#` still shows.
    let first_at = token.find('@')?;
    let (_, after_colon) = authority.rsplit_once(':')?;
    if after_colon.is_empty() || after_colon.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let host = token[first_at + 1..].split(['/', '?', '#']).next().unwrap_or("");
    Some(host.rfind('@').map_or(first_at, |j| first_at + 1 + j))
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub url: String,
    /// Seconds since the Unix epoch.
    pub fetched_at: u64,
    /// Endpoints that answered nothing usable, with why (`conditions (404)`).
    pub missing: Vec<String>,
    /// Legacy subset flag; the current persistence writer never trims automatically.
    pub trimmed: bool,
    pub beans: Vec<Bean>,
    pub profiles: Vec<String>,
    /// Property sources, highest precedence first.
    pub sources: Vec<Source>,
    pub conditions: Vec<Condition>,
    /// `""` is the application, `db`, `db/primary`… its components.
    pub health: Vec<(String, String)>,
    /// `(component path, key, value)`: bounded, escaped and masked at parse time. Absent in schema 1 caches.
    #[serde(default)]
    pub health_details: Vec<(String, String, String)>,
    pub mappings: Vec<Mapping>,
    pub configprops: Vec<PropsBean>,
    pub scheduled: Vec<Scheduled>,
    /// Only `ROOT` and the loggers with a configured level.
    pub loggers: Vec<Logger>,
    pub caches: Vec<Cache>,
    /// Endpoints whose data came from the previous refresh because this one could not reach them.
    #[serde(default)]
    pub kept: Vec<String>,
    /// The raw `spring.datasource.password` of the highest-precedence source that had one, with that source's index; memory only, never stored.
    #[serde(skip)]
    pub datasource_password: Option<(usize, String)>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Bean {
    pub context: String,
    pub name: String,
    pub scope: String,
    /// Fully qualified, `$` nesting as `.`, without the CGLIB suffix.
    pub ty: String,
    pub deps: Vec<String>,
    /// True only when the fetched dependency JSON was an array of strings. Legacy caches cannot prove this.
    #[serde(default)]
    pub dependencies_verified: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Source {
    pub name: String,
    pub props: Vec<(String, String)>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Condition {
    /// `com.acme.MailConfig`, `DataSourceAutoConfiguration#dataSource`…
    pub key: String,
    pub matched: bool,
    pub messages: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Mapping {
    /// The dispatcher servlet or handler.
    pub group: String,
    pub predicate: String,
    pub handler: String,
    /// The handler method's class and name, when it is one.
    pub method: Option<(String, String)>,
    /// The handler method's JVM descriptor (`(Ljava/lang/String;I)V`); absent in older caches.
    #[serde(default)]
    pub descriptor: Option<String>,
    /// Exact method/path pairs reported by requestMappingConditions; legacy snapshots have none.
    #[serde(default)]
    pub requests: Vec<(String, String)>,
    #[serde(default)]
    pub examples: crate::request_conditions::RequestExamples,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PropsBean {
    pub prefix: String,
    pub ty: String,
    pub props: Vec<(String, String)>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Scheduled {
    /// `cron`, `fixedDelay`, `fixedRate`, `custom`.
    pub kind: String,
    /// `com.acme.Jobs.cleanup`.
    pub target: String,
    pub detail: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Logger {
    pub name: String,
    pub configured: String,
    pub effective: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Cache {
    pub manager: String,
    pub name: String,
    pub target: String,
}

/// Masks every bound value (caches written before schema 3 stored them as reported).
pub fn mask_props(props: &mut [(String, String)]) {
    for (key, value) in props {
        *value = masked_value(key, value);
    }
}

/// Parameter simple type names of a JVM method descriptor (`(Ljava/util/List;[I)V` is `List`, `int[]`).
pub fn descriptor_params(descriptor: &str) -> Option<Vec<String>> {
    let body = descriptor.strip_prefix('(')?.split_once(')')?.0;
    let mut out = Vec::new();
    let mut chars = body.chars().peekable();
    while chars.peek().is_some() {
        let mut array = false;
        while chars.peek() == Some(&'[') {
            chars.next();
            array = true;
        }
        let mut name = match chars.next()? {
            'B' => "byte".to_owned(),
            'C' => "char".to_owned(),
            'D' => "double".to_owned(),
            'F' => "float".to_owned(),
            'I' => "int".to_owned(),
            'J' => "long".to_owned(),
            'S' => "short".to_owned(),
            'Z' => "boolean".to_owned(),
            'L' => {
                let mut class = String::new();
                loop {
                    match chars.next()? {
                        ';' => break,
                        c => class.push(c),
                    }
                }
                class.rsplit(['/', '$']).next().filter(|n| !n.is_empty())?.to_owned()
            }
            _ => return None,
        };
        if array {
            name.push_str("[]");
        }
        out.push(name);
        if out.len() > 64 {
            return None;
        }
    }
    Some(out)
}

fn mapping_requests(conditions: &Value) -> Vec<(String, String)> {
    let Some(methods) = conditions.get("methods").and_then(Value::as_array) else { return Vec::new() };
    let Some(paths) = conditions.get("patterns").and_then(Value::as_array) else { return Vec::new() };
    if methods.is_empty() || paths.is_empty() || methods.len().saturating_mul(paths.len()) > 32 { return Vec::new() }
    let mut result = Vec::new();
    for method in methods {
        let Some(method) = method.as_str().filter(|m| matches!(*m, "GET"|"HEAD"|"POST"|"PUT"|"PATCH"|"DELETE"|"OPTIONS"|"TRACE")) else { return Vec::new() };
        for path in paths {
            let Some(path) = path.as_str().filter(|p| p.starts_with('/') && !p.starts_with("//") && p.len() <= 2048 && !p.chars().any(|c| c.is_control() || c.is_whitespace())) else { return Vec::new() };
            if !result.iter().any(|pair| pair == &(method.to_owned(),path.to_owned())) { result.push((method.into(),path.into())); }
        }
    }
    result
}

fn entries(v: &Value) -> impl Iterator<Item = (&String, &Value)> {
    v.as_object().into_iter().flatten()
}

fn items(v: &Value) -> impl Iterator<Item = &Value> {
    v.as_array().into_iter().flatten()
}

fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn type_name(raw: &str) -> String {
    raw.split("$$").next().unwrap_or(raw).replace('$', ".")
}

/// `{a: {b: 1}, c: [x]}` → `a.b = 1`, `c[0] = x`.
fn flatten(prefix: &str, v: &Value, out: &mut Vec<(String, String)>) {
    match v {
        Value::Object(map) => {
            for (k, v) in map {
                let key = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
                flatten(&key, v, out);
            }
        }
        Value::Array(list) => {
            for (i, v) in list.iter().enumerate() {
                flatten(&format!("{prefix}[{i}]"), v, out);
            }
        }
        other => out.push((prefix.to_owned(), text(other))),
    }
}

impl Snapshot {
    /// Reads one endpoint's JSON.
    pub fn read(&mut self, endpoint: &str, json: &Value) {
        let contexts = || entries(&json["contexts"]);
        match endpoint {
            "beans" => {
                for (context, c) in contexts() {
                    for (name, b) in entries(&c["beans"]) {
                        self.beans.push(Bean {
                            context: context.clone(),
                            name: name.clone(),
                            scope: text(&b["scope"]),
                            ty: type_name(&text(&b["type"])),
                            deps: items(&b["dependencies"]).map(text).collect(),
                            dependencies_verified: b["dependencies"]
                                .as_array()
                                .is_some_and(|dependencies| dependencies.iter().all(Value::is_string)),
                        });
                    }
                }
            }
            "env" => {
                self.profiles = items(&json["activeProfiles"]).map(text).collect();
                for s in items(&json["propertySources"]) {
                    let mut props = Vec::new();
                    for (k, p) in entries(&s["properties"]) {
                        let raw = text(&p["value"]);
                        if k == "spring.datasource.password" && self.datasource_password.is_none() && !raw.is_empty() && !raw.chars().all(|c| c == '*') {
                            self.datasource_password = Some((self.sources.len(), raw.clone()));
                        }
                        props.push((k.clone(), masked_value(k, &raw)));
                    }
                    self.sources.push(Source { name: text(&s["name"]), props });
                }
            }
            "conditions" => {
                let messages = |list: &Value| -> Vec<String> {
                    items(list)
                        .map(|o| {
                            let m = text(&o["message"]);
                            if m.is_empty() { text(&o["condition"]) } else { m }
                        })
                        .collect()
                };
                for (_, c) in contexts() {
                    for (key, list) in entries(&c["positiveMatches"]) {
                        self.conditions.push(Condition { key: key.clone(), matched: true, messages: messages(list) });
                    }
                    for (key, o) in entries(&c["negativeMatches"]) {
                        self.conditions.push(Condition { key: key.clone(), matched: false, messages: messages(&o["notMatched"]) });
                    }
                }
            }
            "health" => self.read_health("", json),
            "mappings" => {
                for (_, c) in contexts() {
                    let m = &c["mappings"];
                    for (group, list) in entries(&m["dispatcherServlets"]).chain(entries(&m["dispatcherHandlers"])) {
                        for h in items(list) {
                            let method = &h["details"]["handlerMethod"];
                            self.mappings.push(Mapping {
                                examples: crate::request_conditions::read(&h["details"]["requestMappingConditions"]),
                                requests: mapping_requests(&h["details"]["requestMappingConditions"]),
                                group: group.clone(),
                                predicate: text(&h["predicate"]),
                                handler: text(&h["handler"]),
                                method: method["className"].as_str().map(|class| (type_name(class), text(&method["name"]))),
                                descriptor: method["descriptor"].as_str().filter(|d| d.len() <= 512).map(str::to_owned),
                            });
                        }
                    }
                }
            }
            "configprops" => {
                for (_, c) in contexts() {
                    for (key, b) in entries(&c["beans"]) {
                        let prefix = text(&b["prefix"]);
                        let ty = key.strip_prefix(&format!("{prefix}-")).unwrap_or(key);
                        let mut props = Vec::new();
                        flatten("", &b["properties"], &mut props);
                        props.truncate(CONFIGPROPS_VALUES);
                        mask_props(&mut props);
                        self.configprops.push(PropsBean { prefix, ty: type_name(ty), props });
                    }
                }
            }
            "scheduledtasks" => {
                for (kind, list) in entries(json) {
                    for t in items(list) {
                        let detail = match (t.get("expression"), t.get("interval"), t.get("trigger")) {
                            (Some(e), _, _) => text(e),
                            (_, Some(i), _) => format!("every {} ms", text(i)),
                            (_, _, Some(t)) => text(t),
                            _ => String::new(),
                        };
                        self.scheduled.push(Scheduled { kind: kind.clone(), target: text(&t["runnable"]["target"]), detail });
                    }
                }
            }
            "loggers" => {
                for (name, l) in entries(&json["loggers"]) {
                    let configured = text(&l["configuredLevel"]);
                    if name == "ROOT" || !configured.is_empty() {
                        self.loggers.push(Logger { name: name.clone(), configured, effective: text(&l["effectiveLevel"]) });
                    }
                }
            }
            "caches" => {
                for (manager, m) in entries(&json["cacheManagers"]) {
                    for (name, c) in entries(&m["caches"]) {
                        self.caches.push(Cache { manager: manager.clone(), name: name.clone(), target: text(&c["target"]) });
                    }
                }
            }
            _ => {}
        }
    }

    /// Boot 2.2+ nests `components`; older versions put them under `details`.
    /// At most `HEALTH_DEPTH` levels and `HEALTH_NODES` components plus detail rows are kept.
    fn read_health(&mut self, path: &str, v: &Value) {
        let Some(status) = v["status"].as_str() else { return };
        if self.health.len() + self.health_details.len() >= HEALTH_NODES {
            return;
        }
        self.health.push((path.to_owned(), health_text(status, 64)));
        let depth = if path.is_empty() { 0 } else { path.split('/').count() };
        let mut children: Vec<(&String, &Value)> = entries(&v["components"]).collect();
        let is_component = |c: &Value| c.get("status").is_some_and(Value::is_string);
        for (name, c) in entries(&v["details"]) {
            if is_component(c) {
                children.push((name, c));
            } else {
                self.read_health_detail(path, &health_text(name, 64), c, 0);
            }
        }
        if depth >= HEALTH_DEPTH {
            return;
        }
        for (name, c) in children {
            let name = health_text(&name.replace('/', "_"), 64);
            let child = if path.is_empty() { name } else { format!("{path}/{name}") };
            self.read_health(&child, c);
        }
    }

    fn read_health_detail(&mut self, path: &str, key: &str, v: &Value, level: usize) {
        if self.health.len() + self.health_details.len() >= HEALTH_NODES {
            return;
        }
        match v {
            Value::Object(o) if level < 2 => {
                for (k, c) in o {
                    self.read_health_detail(path, &format!("{key}.{}", health_text(k, 64)), c, level + 1);
                }
            }
            Value::Null => {}
            _ => {
                let raw = match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                let value = masked_value(key, &raw);
                self.health_details.push((path.to_owned(), health_text(key, 128), value));
            }
        }
    }

    /// The value `key` has at runtime and the property source it comes from,
    /// matched with relaxed binding (`SPRING_DATASOURCE_URL` is `spring.datasource.url`).
    /// Scans every property: marker paths use [`Lookup`] instead.
    #[cfg(test)]
    pub fn effective(&self, key: &str) -> Option<(&str, &str)> {
        let relaxed = canonical(key);
        let env_form = relaxed.replace(['.', '[', ']'], "");
        self.sources.iter().find_map(|s| {
            let env = s.name.contains("systemEnvironment");
            s.props
                .iter()
                .find(|(k, _)| if env { canonical(k).replace(['.', '[', ']'], "") == env_form } else { canonical(k) == relaxed })
                .map(|(_, v)| (v.as_str(), s.name.as_str()))
        })
    }

    /// Takes over what the previous snapshot knew of the endpoints this one
    /// could not reach (a timeout is not a removed endpoint).
    pub fn keep_unreachable(&mut self, previous: &Snapshot) {
        let unreachable: Vec<String> = self
            .missing
            .iter()
            .filter(|m| m.contains("(transport failed"))
            .filter_map(|m| m.split(' ').next().map(str::to_owned))
            .filter(|endpoint| previous.has(endpoint))
            .collect();
        for endpoint in unreachable {
            match endpoint.as_str() {
                "beans" => self.beans = previous.beans.clone(),
                "env" => {
                    self.profiles = previous.profiles.clone();
                    self.sources = previous.sources.clone();
                }
                "conditions" => self.conditions = previous.conditions.clone(),
                "health" => {
                    self.health = previous.health.clone();
                    self.health_details = previous.health_details.clone();
                }
                "mappings" => self.mappings = previous.mappings.clone(),
                "configprops" => self.configprops = previous.configprops.clone(),
                "scheduledtasks" => self.scheduled = previous.scheduled.clone(),
                "loggers" => self.loggers = previous.loggers.clone(),
                "caches" => self.caches = previous.caches.clone(),
                _ => continue,
            }
            self.missing.retain(|m| m.split(' ').next() != Some(endpoint.as_str()));
            self.kept.push(endpoint);
        }
    }

    /// Legacy conversion helper. Current persistence stores the full model or rejects it.
    pub fn trimmed(&self) -> Snapshot {
        Snapshot {
            url: self.url.clone(),
            fetched_at: self.fetched_at,
            missing: self.missing.clone(),
            trimmed: true,
            beans: self.beans.clone(),
            profiles: self.profiles.clone(),
            sources: self.sources.clone(),
            ..Snapshot::default()
        }
    }

    /// Whether the snapshot holds what `endpoint` said.
    pub fn has(&self, endpoint: &str) -> bool {
        let kept = !self.trimmed || matches!(endpoint, "beans" | "env");
        kept && !self.missing.iter().any(|m| m.split(' ').next() == Some(endpoint))
    }
}

/// [`Snapshot::effective`] answered from two maps built once per snapshot.
/// Environment sources match with separators dropped, the others relaxed.
#[derive(Default)]
pub struct Lookup {
    relaxed: HashMap<String, (usize, usize)>,
    environment: HashMap<String, (usize, usize)>,
}

impl Lookup {
    pub fn new(snapshot: &Snapshot) -> Self {
        let mut lookup = Self::default();
        for (source, s) in snapshot.sources.iter().enumerate() {
            let environment = s.name.contains("systemEnvironment");
            for (prop, (key, _)) in s.props.iter().enumerate() {
                let relaxed = canonical(key);
                let (map, key) = if environment {
                    (&mut lookup.environment, relaxed.replace(['.', '[', ']'], ""))
                } else {
                    (&mut lookup.relaxed, relaxed)
                };
                map.entry(key).or_insert((source, prop));
            }
        }
        lookup
    }

    pub fn get<'a>(&self, snapshot: &'a Snapshot, key: &str) -> Option<(&'a str, &'a str)> {
        let relaxed = canonical(key);
        let environment = relaxed.replace(['.', '[', ']'], "");
        let (source, prop) = [self.relaxed.get(&relaxed), self.environment.get(&environment)].into_iter().flatten().min().copied()?;
        let source = snapshot.sources.get(source)?;
        let (_, value) = source.props.get(prop)?;
        Some((value.as_str(), source.name.as_str()))
    }
}

/// Whether a property source is the configuration file named `file`
/// (`Config resource 'class path resource [application.yml]' via …` in Boot 3,
/// `applicationConfig: [classpath:/application.yml]` in Boot 2).
pub fn is_file_source(source: &str, file: &str) -> bool {
    source.contains(&format!("[{file}]")) || source.contains(&format!("/{file}]"))
}

/// Whether a property source is a configuration file at all.
pub fn is_config_source(source: &str) -> bool {
    source.starts_with("Config resource") || source.starts_with("applicationConfig")
}

/// A property source's name, short enough for a tooltip: a config file's
/// location (`application.yml`), else the name.
pub fn short_source(source: &str) -> &str {
    if is_config_source(source)
        && let Some(open) = source.find('[')
        && let Some(close) = source[open..].find(']')
    {
        return &source[open + 1..open + close];
    }
    source
}

/// Known root/container shapes consumed by the retained adapters. Empty typed
/// collections are observations; an absent/wrong container is not proof of absence.
pub(crate) fn supported_shape(endpoint: &str, json: &Value) -> bool {
    if !json.is_object() {
        return false;
    }
    let contexts = |field: &str| {
        json.get("contexts").and_then(Value::as_object).is_some_and(|contexts| {
            contexts.values().all(|context| context.is_object() && context.get(field).is_some_and(Value::is_object))
        })
    };
    match endpoint {
        "beans" => {
            contexts("beans")
                && json["contexts"].as_object().is_some_and(|contexts| {
                    contexts.values().all(|context| context["beans"].as_object().is_some_and(|beans| beans.values().all(Value::is_object)))
                })
        }
        "env" => json.get("propertySources").and_then(Value::as_array).is_some_and(|sources| {
            sources.iter().all(|source| {
                source.is_object()
                    && source.get("name").is_some_and(Value::is_string)
                    && source.get("properties").is_some_and(Value::is_object)
            })
        }),
        "health" => json.get("status").is_some_and(Value::is_string),
        "conditions" => json.get("contexts").and_then(Value::as_object).is_some_and(|contexts| {
            contexts.values().all(|context| {
                context.is_object()
                    && (context.get("positiveMatches").is_some() || context.get("negativeMatches").is_some())
                    && ["positiveMatches", "negativeMatches"].iter().all(|key| context.get(*key).is_none_or(Value::is_object))
            })
        }),
        "mappings" => contexts("mappings"),
        "configprops" => {
            contexts("beans")
                && json["contexts"].as_object().is_some_and(|contexts| {
                    contexts.values().all(|context| {
                        context["beans"].as_object().is_some_and(|beans| {
                            beans.values().all(|bean| bean.is_object() && bean.get("properties").is_some_and(Value::is_object))
                        })
                    })
                })
        }
        "scheduledtasks" => {
            let keys = ["cron", "fixedDelay", "fixedRate", "custom"];
            keys.iter().any(|key| json.get(*key).is_some())
                && keys
                    .iter()
                    .all(|key| json.get(*key).is_none_or(|value| value.as_array().is_some_and(|tasks| tasks.iter().all(Value::is_object))))
        }
        "loggers" => json.get("loggers").and_then(Value::as_object).is_some_and(|loggers| loggers.values().all(Value::is_object)),
        "caches" => json.get("cacheManagers").and_then(Value::as_object).is_some_and(|managers| {
            managers.values().all(|manager| manager.is_object() && manager.get("caches").is_some_and(Value::is_object))
        }),
        _ => false,
    }
}

#[cfg(test)]
mod health_tests {
    use super::*;
    use serde_json::json;

    fn parse(v: Value) -> Snapshot {
        let mut s = Snapshot::default();
        s.read_health("", &v);
        s
    }

    #[test]
    fn keeps_details_and_masks_secrets_before_storage() {
        let s = parse(json!({"status":"UP","components":{"db":{"status":"UP","details":{
            "database":"PostgreSQL","apiToken":"abc","url":"jdbc:x://bob:pw@h:1/d","pool":{"password":"p","size":3}}}}}));
        let d: std::collections::HashMap<_, _> = s.health_details.iter().map(|(_, k, v)| (k.as_str(), v.as_str())).collect();
        assert_eq!(d["database"], "PostgreSQL");
        assert_eq!(d["apiToken"], MASK);
        assert_eq!(d["pool.password"], MASK);
        assert_eq!(d["pool.size"], "3");
        assert_eq!(d["url"], "jdbc:x://******@h:1/d");
        assert!(!serde_json::to_string(&s).unwrap().contains("pw@"));
    }

    #[test]
    fn legacy_details_children_stay_components() {
        let s = parse(json!({"status":"UP","details":{"db":{"status":"DOWN"},"free":10}}));
        assert_eq!(s.health, vec![("".into(), "UP".into()), ("db".into(), "DOWN".into())]);
        assert_eq!(s.health_details, vec![("".into(), "free".into(), "10".into())]);
    }

    #[test]
    fn depth_nodes_and_value_length_are_bounded() {
        let mut deep = json!({"status":"UP"});
        for _ in 0..8 {
            deep = json!({"status":"UP","components":{"c":deep}});
        }
        assert_eq!(parse(deep).health.len(), HEALTH_DEPTH + 1);
        let wide: serde_json::Map<String, Value> = (0..500).map(|i| (format!("c{i}"), json!({"status":"UP"}))).collect();
        assert_eq!(parse(json!({"status":"UP","components":wide})).health.len(), HEALTH_NODES);
        let s = parse(json!({"status":"UP","details":{"note":"é\u{7}".repeat(500)}}));
        assert!(s.health_details[0].2.chars().count() <= 201 && !s.health_details[0].2.contains('\u{7}'));
    }
}

#[cfg(test)]
mod boot_shape_tests {
    use super::*;
    use serde_json::json;

    // Shapes verified against spring-boot 3.5.x sources: ConditionsReportEndpoint.ContextConditionsDescriptor
    // (positiveMatches, negativeMatches{notMatched,matched}, exclusions, unconditionalClasses, parentId),
    // ConfigurationPropertiesReportEndpoint.ConfigurationPropertiesBeanDescriptor (prefix, properties, inputs),
    // MappingsEndpoint.ContextMappingsDescriptor.mappings + HandlerMethodDescription (className, name, descriptor).
    #[test]
    fn reads_boot_3_descriptor_shapes() {
        let mut s = Snapshot::default();
        s.read("conditions", &json!({"contexts":{"app":{
            "positiveMatches":{"A#a":[{"condition":"OnClassCondition","message":"found class"}]},
            "negativeMatches":{"B":{"notMatched":[{"condition":"OnBeanCondition","message":"no bean"}],"matched":[]}},
            "exclusions":[],"unconditionalClasses":["C"],"parentId":null}}}));
        assert_eq!(s.conditions.len(), 2);
        assert_eq!(s.conditions[1].messages, vec!["no bean"]);
        s.read("configprops", &json!({"contexts":{"app":{"beans":{"acme.mail-com.acme.Mail":{
            "prefix":"acme.mail","properties":{"host":"h"},"inputs":{"host":{"value":"h","origin":"x"}}}},"parentId":null}}}));
        assert_eq!((s.configprops[0].prefix.as_str(), s.configprops[0].ty.as_str()), ("acme.mail", "com.acme.Mail"));
        assert_eq!(s.configprops[0].props, vec![("host".to_owned(), "h".to_owned())]);
        s.read("mappings", &json!({"contexts":{"app":{"mappings":{"dispatcherServlets":{"dispatcherServlet":[{
            "handler":"h","predicate":"p","details":{"handlerMethod":{"className":"com.acme.Users","name":"find","descriptor":"()V"},
            "requestMappingConditions":{"methods":["GET"],"patterns":["/u"]}}}]}},"parentId":null}}}));
        assert_eq!(s.mappings[0].method, Some(("com.acme.Users".to_owned(), "find".to_owned())));
    }
}

#[cfg(test)]
mod env_secret_tests {
    use super::*;
    use serde_json::json;

    fn env_snapshot() -> Snapshot {
        let mut s = Snapshot::default();
        s.read("env", &json!({"activeProfiles":[],"propertySources":[{"name":"application.properties","properties":{
            "spring.datasource.password":{"value":"s3cret"},"spring.datasource.url":{"value":"jdbc:postgresql://h/db"}}}]}));
        s
    }

    #[test]
    fn sensitive_key_by_segment() {
        assert!(sensitive_key("spring.datasource.password"));
        assert!(sensitive_key("x.client-secret"));
        assert!(sensitive_key("x.api-key"));
        assert!(sensitive_key("apiToken"));
        assert!(sensitive_key("DB_PASSWORD"));
        assert!(!sensitive_key("keyStoreType"));
        assert!(!sensitive_key("oauth2.authorization-uri"));
        assert!(!sensitive_key("monkey"));
    }

    #[test]
    fn userinfo_masked_when_password_holds_url_delimiters() {
        assert_eq!(mask_userinfo("jdbc:mysql://u:pa/ss@h/db"), "jdbc:mysql://******@h/db");
        assert_eq!(mask_userinfo("http://u:p?w#x@h/x"), "http://******@h/x");
        assert_eq!(mask_userinfo("http://u:p@w@h/x?a=b@c"), "http://******@h/x?a=b@c");
        assert_eq!(mask_userinfo("jdbc:postgresql://h:5432/db?user=a@b"), "jdbc:postgresql://h:5432/db?user=a@b");
        assert_eq!(mask_userinfo("http://h/path@x"), "http://h/path@x");
    }

    #[test]
    fn command_line_values_mask_secret_arguments() {
        let shown = masked_value("sun.java.command", "app.jar --spring.datasource.password=hunter2 --server.port=8080");
        assert_eq!(shown, "app.jar --spring.datasource.password=****** --server.port=8080");
        assert_eq!(masked_value("JAVA_TOOL_OPTIONS", "-Dapi.key=abc -Xmx1g"), "-Dapi.key=****** -Xmx1g");
    }

    #[test]
    fn sensitive_key_masks_compound_and_acronym_secret_names() {
        for key in ["APIKEY", "apikey", "secretkey", "accesskey", "PRIVATEKEY", "AWSKey", "ABCKey", "jwtsecret", "authtoken", "dbpassword", "x.authorization", "signingKey", "x.encryption-key", "masterkey", "key", "my.credential"] {
            assert!(sensitive_key(key), "{key} should be masked");
        }
        for key in ["monkey", "keyStoreType", "oauth2.authorization-uri", "turkey", "keyAlias", "x.token-uri"] {
            assert!(!sensitive_key(key), "{key} should show");
        }
    }

    #[test]
    fn env_password_masked_in_tree_and_tooltip() {
        let s = env_snapshot();
        assert!(!serde_json::to_string(&s.sources).unwrap().contains("s3cret"));
        assert_eq!(s.effective("spring.datasource.password").unwrap().0, MASK);
        assert_eq!(s.effective("spring.datasource.url").unwrap().0, "jdbc:postgresql://h/db");
    }

    #[test]
    fn persisted_snapshot_has_no_raw_env_secret() {
        let s = env_snapshot();
        assert!(!serde_json::to_string(&s).unwrap().contains("s3cret"));
    }

    #[test]
    fn datasource_password_available_in_memory() {
        assert_eq!(env_snapshot().datasource_password, Some((0, "s3cret".to_owned())));
        let mut s = Snapshot::default();
        s.read("env", &json!({"propertySources":[{"name":"a","properties":{"spring.datasource.password":{"value":"******"}}}]}));
        assert_eq!(s.datasource_password, None);
    }

    #[test]
    fn effective_lookup_matches_scan() {
        let s = Snapshot {
            sources: vec![
                Source { name: "systemEnvironment".into(), props: vec![("SPRING_DATASOURCE_URL".into(), "env-url".into()), ("PATH".into(), "p".into())] },
                Source { name: "application.yml".into(), props: vec![("spring.datasource.url".into(), "file-url".into()), ("my-prop.some_key".into(), "v".into()), ("spring.datasource.url".into(), "dup".into())] },
                Source { name: "defaults".into(), props: vec![("spring.datasource.username".into(), "u".into()), ("list[0].name".into(), "n".into())] },
            ],
            ..Snapshot::default()
        };
        let lookup = Lookup::new(&s);
        for key in ["spring.datasource.url", "SPRING_DATASOURCE_URL", "spring.datasource.username", "myProp.someKey", "my-prop.some-key", "list[0].name", "path", "missing.key", ""] {
            assert_eq!(lookup.get(&s, key), s.effective(key), "{key}");
        }
    }
}
