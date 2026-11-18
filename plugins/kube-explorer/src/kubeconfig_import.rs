//! Explicit, disconnected import. Credentials live only in the SDK's owner-only
//! private directory; generic plugin storage contains redacted metadata only.
use openidea_plugin::{Error, FileKind, NotificationLevel, PickItem, host};
use crate::KubeExplorer;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use yaml_rust2::{
    Yaml, YamlLoader,
    parser::{Event, EventReceiver, Parser},
};
const MAX_INPUT: usize = 128 * 1024;
const MAX_RECORD: usize = 192 * 1024;
const MAX_PROFILES: usize = 32;
const MAX_METADATA: usize = 4096;
const PREFIX: &str = "imported-kubeconfig-v1:";
const FILE_PREFIX: &str = "imported-kubeconfig-";
const SELECTED: &str = "imported-kubeconfig-selected";
const ACTIVE_FILE: &str = "active-imported-kubeconfig.json";
fn invalid(message: &str) -> Error {
    Error::Invalid(message.into())
}
enum Frame {
    Map(bool, BTreeSet<String>),
    Sequence,
}
#[derive(Default)]
struct Guard {
    frames: Vec<Frame>,
    nodes: usize,
    docs: usize,
    bad: bool,
}
impl Guard {
    fn value(&mut self) {
        if let Some(Frame::Map(key, _)) = self.frames.last_mut() {
            if *key {
                self.bad = true;
            } else {
                *key = true;
            }
        }
    }
}
impl EventReceiver for Guard {
    fn on_event(&mut self, event: Event) {
        self.nodes += 1;
        if self.nodes > 4096 {
            self.bad = true;
        }
        match event {
            Event::DocumentStart => {
                self.docs += 1;
                if self.docs > 1 {
                    self.bad = true;
                }
            }
            Event::Alias(_) => self.bad = true,
            Event::Scalar(text, _, anchor, tag) => {
                if anchor != 0 || tag.is_some() {
                    self.bad = true;
                }
                if let Some(Frame::Map(key, seen)) = self.frames.last_mut() {
                    if *key {
                        if !seen.insert(text) {
                            self.bad = true;
                        }
                        *key = false;
                    } else {
                        *key = true;
                    }
                }
            }
            Event::MappingStart(anchor, tag) => {
                if anchor != 0 || tag.is_some() {
                    self.bad = true;
                }
                self.value();
                self.frames.push(Frame::Map(true, BTreeSet::new()));
            }
            Event::SequenceStart(anchor, tag) => {
                if anchor != 0 || tag.is_some() {
                    self.bad = true;
                }
                self.value();
                self.frames.push(Frame::Sequence);
            }
            Event::MappingEnd | Event::SequenceEnd => {
                self.frames.pop();
            }
            _ => {}
        }
        if self.frames.len() > 16 {
            self.bad = true;
        }
    }
}
fn value(yaml: &Yaml) -> Result<Value, Error> {
    match yaml {
        Yaml::String(s) => Ok(json!(s)),
        Yaml::Integer(i) => Ok(json!(i)),
        Yaml::Boolean(b) => Ok(json!(b)),
        Yaml::Null => Ok(Value::Null),
        Yaml::Array(values) => values.iter().map(value).collect::<Result<Vec<_>, _>>().map(Value::Array),
        Yaml::Hash(values) => {
            let mut object = serde_json::Map::new();
            for (key, item) in values {
                let key = key.as_str().ok_or_else(|| invalid("Kubeconfig mapping keys must be plain strings."))?;
                if object.insert(key.into(), value(item)?).is_some() {
                    return Err(invalid("Duplicate kubeconfig key."));
                }
            }
            Ok(Value::Object(object))
        }
        _ => Err(invalid("Unsupported kubeconfig scalar or alias.")),
    }
}
pub(super) fn bounded_structure(raw: &str) -> Result<(), Error> {
    if raw.is_empty() || raw.len() > MAX_INPUT {
        return Err(invalid("Pasted kubeconfig must be 1..128 KiB."));
    }
    let mut guard = Guard::default();
    Parser::new_from_str(raw)
        .load(&mut guard, true)
        .map_err(|_| invalid("Kubeconfig YAML/JSON is malformed; no credentials were stored."))?;
    if guard.bad || guard.docs != 1 {
        return Err(invalid("Paste one kubeconfig document without duplicate keys, aliases, anchors, tags or complex keys."));
    }
    Ok(())
}
pub(super) fn parse(raw: &str) -> Result<Value, Error> {
    bounded_structure(raw)?;
    let documents = YamlLoader::load_from_str(raw).map_err(|_| invalid("Kubeconfig could not be parsed."))?;
    value(documents.first().ok_or_else(|| invalid("Empty kubeconfig."))?)
}
fn fields(value: &Value, allowed: &[&str]) -> Result<(), Error> {
    let object = value.as_object().ok_or_else(|| invalid("Kubeconfig objects have an unsupported shape."))?;
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(invalid(
            "Kubeconfig contains unsupported fields; executable auth, credential paths, proxy and extensions are not imported.",
        ));
    }
    Ok(())
}
fn text<'a>(value: &'a Value, key: &str, max: usize) -> Result<&'a str, Error> {
    value[key]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control))
        .ok_or_else(|| invalid("A kubeconfig text field is missing, invalid or too large."))
}
fn label(s: &str) -> bool {
    !s.is_empty() && s.len() <= 128 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"_.:@/-".contains(&b)) && !s.starts_with('-')
}
fn base64(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64 * 1024
        && s.len() % 4 == 0
        && s.trim_end_matches('=').bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/'))
        && s.len() - s.trim_end_matches('=').len() <= 2
}
fn server_port(port:&str)->bool {
    !port.is_empty()&&port.len()<=5&&port.bytes().all(|byte|byte.is_ascii_digit())
        &&port.parse::<u16>().ok().is_some_and(|port|port!=0)
}
fn server(value: &str) -> bool {
    let Some(authority) = value.strip_prefix("https://") else {
        return false;
    };
    let authority = authority.strip_suffix('/').unwrap_or(authority);
    if authority.is_empty() || authority.len() > 512 || authority.contains(['@', '/', '?', '#', '\\', '{', '}']) {
        return false;
    }
    if let Some(bracketed)=authority.strip_prefix('[') {
        let Some((literal,tail))=bracketed.split_once(']') else{return false;};
        return literal.parse::<std::net::Ipv6Addr>().is_ok()
            &&(tail.is_empty()||tail.strip_prefix(':').is_some_and(server_port));
    }
    let host = match authority.rsplit_once(':') {
        Some((host,port)) if server_port(port)=>host,
        Some(_)=>return false,
        None=>authority,
    };
    if host.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        let parts: Vec<_> = host.split('.').collect();
        return parts.len() == 4
            && parts.iter().all(|part| !part.is_empty() && !(part.len() > 1 && part.starts_with('0')) && part.parse::<u8>().is_ok());
    }
    !host.is_empty()
        && host.split('.').all(|part| {
            !part.is_empty()
                && part.len() <= 63
                && !part.starts_with('-')
                && !part.ends_with('-')
                && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}
fn entries<'a>(config: &'a Value, key: &str, item_key: &str) -> Result<BTreeMap<&'a str, &'a Value>, Error> {
    let array =
        config[key].as_array().filter(|v| v.len() <= 64).ok_or_else(|| invalid("The kubeconfig clusters, contexts and users must be lists of at most 64 entries."))?;
    let mut result = BTreeMap::new();
    for entry in array {
        fields(entry, &["name", item_key])?;
        let name = text(entry, "name", 128)?;
        if !label(name) || result.insert(name, &entry[item_key]).is_some() {
            return Err(invalid("Kubeconfig names are invalid or duplicated."));
        }
    }
    Ok(result)
}
fn secrets(config: &Value) -> Vec<&str> {
    config["users"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|entry| {
            ["token", "client-certificate-data", "client-key-data"].into_iter().filter_map(move |key| entry["user"][key].as_str())
        })
        .chain(
            config["clusters"].as_array().into_iter().flatten().filter_map(|entry| entry["cluster"]["certificate-authority-data"].as_str()),
        )
        .collect()
}
fn exposes(value: &str, secret_values: &[&str]) -> bool {
    secret_values.iter().any(|secret| value == *secret || (secret.len() >= 8 && value.contains(secret)))
}
struct Parsed {
    config: Value,
    contexts: Vec<String>,
}
fn validated(raw: &str) -> Result<Parsed, Error> {
    let config = parse(raw)?;
    fields(&config, &["apiVersion", "kind", "clusters", "users", "contexts", "current-context", "preferences"])?;
    if config["apiVersion"] != "v1" || config["kind"] != "Config" {
        return Err(invalid("Only Kubernetes v1 Config documents are supported."));
    }
    if config.get("preferences").is_some_and(|v| !v.as_object().is_some_and(|v| v.is_empty())) {
        return Err(invalid("Nonempty kubeconfig preferences are outside this import subset."));
    }
    let clusters = entries(&config, "clusters", "cluster")?;
    let users = entries(&config, "users", "user")?;
    let contexts = entries(&config, "contexts", "context")?;
    if clusters.is_empty() || contexts.is_empty() {
        return Err(invalid("At least one cluster and context is required."));
    }
    for cluster in clusters.values() {
        fields(cluster, &["server", "certificate-authority-data", "insecure-skip-tls-verify"])?;
        if !server(text(cluster, "server", 1024)?) {
            return Err(invalid(
                "Only HTTPS DNS/IPv4 or bracketed IPv6 API server authorities with optional port are supported; no credentials, zones, query, fragment or base path.",
            ));
        }
        if cluster.get("insecure-skip-tls-verify").is_some_and(|v| v != false) {
            return Err(invalid("Imported TLS verification cannot be disabled."));
        }
        if let Some(ca) = cluster.get("certificate-authority-data") {
            if !ca.as_str().is_some_and(base64) {
                return Err(invalid("Inline CA data is not supported base64."));
            }
        }
    }
    for user in users.values() {
        fields(user, &["token", "client-certificate-data", "client-key-data"])?;
        let token = user.get("token");
        let certificate = user.get("client-certificate-data");
        let key = user.get("client-key-data");
        if token.is_some() {
            text(user, "token", 8192)?;
            if certificate.is_some() || key.is_some() {
                return Err(invalid("Choose one static authentication mode per user."));
            }
        }
        if certificate.is_some() != key.is_some() {
            return Err(invalid("Inline client certificate and key must be paired."));
        }
        for data in [certificate, key].into_iter().flatten() {
            if !data.as_str().is_some_and(base64) {
                return Err(invalid("Inline credential data is not supported base64."));
            }
        }
    }
    let secret_values = secrets(&config);
    for (name, data) in &clusters {
        if exposes(name, &secret_values) || data["server"].as_str().is_some_and(|value| exposes(value, &secret_values)) {
            return Err(invalid("Credential material overlaps kubeconfig metadata; choose nonsecret names and server addresses."));
        }
    }
    for name in users.keys() {
        if exposes(name, &secret_values) {
            return Err(invalid("Credential material overlaps a kubeconfig user alias."));
        }
    }
    let mut names = Vec::new();
    for (name, context) in &contexts {
        fields(context, &["cluster", "user", "namespace"])?;
        if !clusters.contains_key(text(context, "cluster", 128)?) {
            return Err(invalid("A context names an unknown cluster."));
        }
        if let Some(user) = context.get("user") {
            if !users.contains_key(user.as_str().ok_or_else(|| invalid("Invalid context user."))?) {
                return Err(invalid("A context names an unknown user."));
            }
        }
        if let Some(namespace) = context.get("namespace") {
            if !namespace.as_str().is_some_and(crate::is_dns_label) {
                return Err(invalid("Context namespace must be a Kubernetes DNS label."));
            }
        }
        if exposes(name, &secret_values)
            || ["cluster", "user", "namespace"].iter().any(|key| context[*key].as_str().is_some_and(|value| exposes(value, &secret_values)))
        {
            return Err(invalid("Credential material overlaps context metadata; choose nonsecret aliases."));
        }
        names.push((*name).to_owned());
    }
    if let Some(current) = config.get("current-context") {
        if !current.as_str().is_some_and(|name| contexts.contains_key(name)) {
            return Err(invalid("Current context is missing or ambiguous."));
        }
    }
    Ok(Parsed { config, contexts: names })
}
fn selected(parsed: &Parsed, context: &str) -> Result<Value, Error> {
    let contexts = entries(&parsed.config, "contexts", "context")?;
    let context_data = contexts.get(context).ok_or_else(|| invalid("Choose a listed context."))?;
    let cluster_name = text(context_data, "cluster", 128)?;
    let clusters = entries(&parsed.config, "clusters", "cluster")?;
    let cluster = clusters.get(cluster_name).ok_or_else(|| invalid("Unknown selected cluster."))?;
    let user_name = context_data["user"].as_str();
    let users = entries(&parsed.config, "users", "user")?;
    let namespace = context_data["namespace"].as_str().unwrap_or("default");
    let mut scope = json!({"cluster":cluster_name,"namespace":namespace});
    let mut chosen_users = Vec::new();
    if let Some(user) = user_name {
        scope["user"] = json!(user);
        chosen_users.push(json!({"name":user,"user":users[user]}));
    }
    Ok(
        json!({"apiVersion":"v1","kind":"Config","clusters":[{"name":cluster_name,"cluster":cluster}],"users":chosen_users,"contexts":[{"name":context,"context":scope}],"current-context":context}),
    )
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn metadata(name: &str, config: &Value, id: &str, path: &str) -> Value {
    let context = &config["contexts"][0];
    let cluster = &config["clusters"][0];
    let user = &config["users"][0]["user"];
    json!({"schema":1,"id":id,"name":name,"path":path,"context":context["name"],"cluster":cluster["name"],"namespace":context["context"]["namespace"],"userAlias":context["context"].get("user").cloned().unwrap_or(Value::Null),"server":cluster["cluster"]["server"],"auth":if user.get("token").is_some(){"static token (redacted)"}else if user.get("client-key-data").is_some(){"inline client certificate/key (redacted)"}else{"anonymous"},"inlineCA":cluster["cluster"].get("certificate-authority-data").is_some()})
}
fn redacted(metadata: &Value) -> String {
    format!(
        "Profile: {}\nContext: {}\nCluster: {}\nNamespace: {}\nDeclared user alias: {}\nAPI server: {}\nAuthentication: {}\nInline CA data present: {}\n\nOnly this selected context is stored. Credentials are redacted and retained only in the SDK private directory. No kubectl, auth hook, installation, context switch or cluster connection is performed.",
        metadata["name"].as_str().unwrap_or(""),
        metadata["context"].as_str().unwrap_or(""),
        metadata["cluster"].as_str().unwrap_or(""),
        metadata["namespace"].as_str().unwrap_or(""),
        metadata["userAlias"].as_str().unwrap_or("(anonymous)"),
        metadata["server"].as_str().unwrap_or(""),
        metadata["auth"].as_str().unwrap_or(""),
        metadata["inlineCA"]
    )
}
fn profiles() -> Result<Vec<Value>, Error> {
    let directory = host::fs::plugin_dir()?;
    let keys = host::storage::keys(PREFIX)?;
    if keys.len() > MAX_PROFILES {
        return Err(invalid("Imported profile count exceeds its bound."));
    }
    let mut profiles = Vec::new();
    for key in keys {
        let raw = host::storage::get(&key)?.ok_or_else(|| invalid("Imported profile metadata is unavailable."))?;
        if raw.len() > MAX_METADATA {
            return Err(invalid("Imported profile metadata exceeds its bound."));
        }
        let meta: Value = serde_json::from_str(&raw).map_err(|_| invalid("Imported profile metadata is unreadable."))?;
        let id = meta["id"]
            .as_str()
            .filter(|id| id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or_else(|| invalid("This imported kubeconfig is damaged. Delete it and import it again."))?;
        let path = format!("{directory}/{FILE_PREFIX}{id}.json");
        if key != format!("{PREFIX}{id}") || meta["path"] != path {
            return Err(invalid("This imported kubeconfig does not match its saved location. Delete it and import it again."));
        }
        let stat = host::fs::stat(&path)?;
        if stat.kind != FileKind::File || stat.size > MAX_RECORD as u64 {
            return Err(invalid("This imported kubeconfig is not a regular file or is too large. Delete it and import it again."));
        }
        let payload = host::fs::read_bytes(&path)?;
        if payload.len() > MAX_RECORD || digest(&payload) != id {
            return Err(invalid("This imported kubeconfig changed on disk. Delete it and import it again."));
        }
        let record: Value = serde_json::from_slice(&payload).map_err(|_| invalid("Imported private profile is unreadable."))?;
        fields(&record, &["schema", "name", "config"])?;
        if record["schema"] != 1 {
            return Err(invalid("Unsupported private profile schema."));
        }
        let name = text(&record, "name", 64)?;
        if !name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')) {
            return Err(invalid("Invalid stored profile label."));
        }
        let parsed = validated(&record["config"].to_string())?;
        if exposes(name, &secrets(&parsed.config)) {
            return Err(invalid("Stored profile label overlaps credential material."));
        }
        let current = text(&parsed.config, "current-context", 128)?;
        let config = selected(&parsed, current)?;
        if metadata(name, &config, id, &path) != meta {
            return Err(invalid("Imported profile redacted metadata does not match its private record."));
        }
        profiles.push(meta);
    }
    Ok(profiles)
}
pub fn import() -> Result<(), Error> {
    let existing = profiles()?;
    if existing.len() >= MAX_PROFILES {
        return Err(invalid("At most 32 imported profiles are supported."));
    }
    let Some(raw) = host::input_multiline("Paste kubeconfig YAML/JSON (stored privately only after redacted approval)", None)? else {
        return Ok(());
    };
    let parsed = validated(&raw)?;
    let Some(name) = host::input("New profile label (letters, digits, underscore or hyphen)", None)? else {
        return Ok(());
    };
    if name.is_empty() || name.len() > 64 || !name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')) {
        return Err(invalid("Profile label must be 1..64 letters, digits, underscore or hyphen."));
    }
    if exposes(&name, &secrets(&parsed.config)) {
        return Err(invalid("Profile label must not contain credential material."));
    }
    if existing.iter().any(|profile| profile["name"] == name) {
        return Err(invalid("A profile with that label already exists; it was retained."));
    }
    let contexts: Vec<_> = parsed.contexts.iter().map(|name| PickItem::new(name, name)).collect();
    let Some(context) = host::pick("Select the exact context to import (no connection)", &contexts)? else {
        return Ok(());
    };
    let config = selected(&parsed, &context)?;
    validated(&config.to_string())?;
    let payload = json!({"schema":1,"name":name,"config":config}).to_string();
    if payload.len() > MAX_RECORD {
        return Err(invalid("Normalized private profile exceeds its bound."));
    }
    let directory = host::fs::plugin_dir()?;
    let id = digest(payload.as_bytes());
    let path = format!("{directory}/{FILE_PREFIX}{id}.json");
    let meta = metadata(&name, &config, &id, &path);
    let metadata_bytes = meta.to_string();
    if metadata_bytes.len() > MAX_METADATA {
        return Err(invalid("Redacted profile metadata exceeds its bound."));
    }
    if !ui_kit::prompts::confirm_exact("Import private kubeconfig (no connection)?", &redacted(&meta))? {
        return Ok(());
    }
    let latest = profiles()?;
    if latest.len() >= MAX_PROFILES || latest.iter().any(|profile| profile["name"] == name) {
        return Err(invalid("Profile state changed; existing profiles were retained."));
    }
    let files = host::fs::read_dir(&directory)?;
    if files.len() >= 4096 {
        return Err(invalid("Private directory listing reached the host limit; import was not written."));
    }
    let mut private_count = 0;
    for entry in files.iter().filter(|entry| entry.name.starts_with(FILE_PREFIX)) {
        let placeholder = host::fs::stat(&format!("{directory}/{}", entry.name)).is_ok_and(|stat| stat.size == 0);
        private_count += usize::from(!placeholder);
    }
    // The pinned host resolves existing private paths before stat and maps a
    // missing path to Invalid, unlike the SDK mock's NotFound. The complete
    // bounded listing above proves absence without interpreting error text.
    let filename = format!("{FILE_PREFIX}{id}.json");
    if files.iter().any(|entry| entry.name == filename) {
        if host::fs::stat(&path)?.kind != FileKind::File {
            return Err(invalid("Private import destination is not a plain file."));
        }
        let existing = host::fs::read_bytes(&path)?;
        if existing.is_empty() {
            host::fs::write_bytes(&path, payload.as_bytes())?;
        } else if existing != payload.as_bytes() {
            return Err(invalid("Private import destination already exists with different content."));
        }
    } else {
        if private_count >= 64 {
            return Err(invalid("Too many imported kubeconfigs. Delete some with Kube Explorer: Delete Imported Kubeconfig, then try again."));
        }
        host::fs::write_bytes(&path, payload.as_bytes())?;
    }
    if host::fs::read_bytes(&path)? != payload.as_bytes() {
        return Err(invalid("Private imported contents changed; profile was not published."));
    }
    let key = format!("{PREFIX}{id}");
    if host::storage::get(&key)?.is_some() {
        return Err(invalid("Imported profile metadata already exists; no replacement is allowed."));
    }
    host::storage::set(&key, &metadata_bytes)?;
    host::notify(
        NotificationLevel::Info,
        "Kubeconfig import",
        "Private profile saved. Existing settings, active context, namespace and connections were not changed. Use Review Imported Kubeconfigs to inspect redacted metadata.",
    );
    Ok(())
}
pub fn review() -> Result<(), Error> {
    let profiles = profiles()?;
    let choices: Vec<_> = profiles
        .iter()
        .map(|profile| PickItem::new(profile["id"].as_str().unwrap_or(""), profile["name"].as_str().unwrap_or("")))
        .collect();
    let Some(id) = host::pick("Imported kubeconfigs (redacted review only)", &choices)? else {
        return Ok(());
    };
    let profile = profiles.iter().find(|profile| profile["id"] == id).ok_or_else(|| invalid("Select a listed profile."))?;
    let _ = ui_kit::prompts::confirm_exact("Imported kubeconfig metadata (not connected)", &redacted(profile))?;
    Ok(())
}


/// The kubeconfig file of the imported profile in use, for `--kubeconfig`.
pub fn selected_path() -> Option<String> {
    host::storage::get(SELECTED).ok().flatten()?;
    Some(format!("{}/{ACTIVE_FILE}", host::fs::plugin_dir().ok()?))
}

/// The label of the imported profile in use.
pub fn selected_name() -> Option<String> {
    let id = host::storage::get(SELECTED).ok().flatten()?;
    let meta = host::storage::get(&format!("{PREFIX}{id}")).ok().flatten()?;
    serde_json::from_str::<Value>(&meta).ok()?["name"].as_str().map(str::to_owned)
}

fn choose_profile(title: &str, extra: Option<PickItem>) -> Result<Option<(Vec<Value>, String)>, Error> {
    let profiles = profiles()?;
    if profiles.is_empty() {
        return Err(invalid("Import a kubeconfig first."));
    }
    let mut choices: Vec<_> = profiles
        .iter()
        .map(|profile| PickItem::new(profile["id"].as_str().unwrap_or(""), profile["name"].as_str().unwrap_or("")))
        .collect();
    choices.extend(extra);
    Ok(host::pick(title, &choices)?.map(|id| (profiles, id)))
}

fn clear_active_file() -> Result<(), Error> {
    host::fs::write_bytes(&format!("{}/{ACTIVE_FILE}", host::fs::plugin_dir()?), b"")
}

impl KubeExplorer {
    fn imported_selection_changed(&mut self) {
        self.context = None;
        self.namespace = None;
        self.save();
        self.reload();
    }

    pub fn use_imported_kubeconfig(&mut self) -> Result<(), Error> {
        let stop = host::storage::get(SELECTED)?.map(|_| PickItem::new("", "Stop using an imported kubeconfig"));
        let Some((profiles, id)) = choose_profile("Use an imported kubeconfig for kubectl", stop)? else {
            return Ok(());
        };
        if id.is_empty() {
            host::storage::delete(SELECTED)?;
            clear_active_file()?;
            self.imported_selection_changed();
            return Ok(());
        }
        let profile = profiles.iter().find(|profile| profile["id"] == id).ok_or_else(|| invalid("Select a listed profile."))?;
        let body = format!("{}\n\nEvery kubectl command in Kube Explorer uses this profile until you stop using it.", redacted(profile));
        if !ui_kit::prompts::confirm_exact("Use this imported kubeconfig?", &body)? {
            return Ok(());
        }
        let record: Value = serde_json::from_slice(&host::fs::read_bytes(profile["path"].as_str().unwrap_or(""))?)
            .map_err(|_| invalid("Imported private profile is unreadable."))?;
        if !record["config"].is_object() {
            return Err(invalid("Imported private profile has no kubeconfig; import it again."));
        }
        let active = format!("{}/{ACTIVE_FILE}", host::fs::plugin_dir()?);
        host::storage::set(SELECTED, &id)?;
        if let Err(error) = host::fs::write_bytes(&active, record["config"].to_string().as_bytes()) {
            let _ = host::storage::delete(SELECTED);
            return Err(error);
        }
        self.imported_selection_changed();
        crate::config::notify_success("Imported kubeconfig in use", profile["name"].as_str().unwrap_or(""));
        Ok(())
    }

    pub fn delete_imported_kubeconfig(&mut self) -> Result<(), Error> {
        let Some((profiles, id)) = choose_profile("Delete an imported kubeconfig", None)? else {
            return Ok(());
        };
        let profile = profiles.iter().find(|profile| profile["id"] == id).ok_or_else(|| invalid("Select a listed profile."))?;
        if !ui_kit::prompts::confirm_exact("Delete this imported kubeconfig?", &redacted(profile))? {
            return Ok(());
        }
        let was_selected = host::storage::get(SELECTED)?.as_deref() == Some(id.as_str());
        host::fs::write_bytes(profile["path"].as_str().unwrap_or(""), b"")?;
        host::storage::delete(&format!("{PREFIX}{id}"))?;
        if was_selected {
            host::storage::delete(SELECTED)?;
            clear_active_file()?;
            self.imported_selection_changed();
        }
        crate::config::notify_success("Imported kubeconfig deleted", profile["name"].as_str().unwrap_or(""));
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{Plugin, mock::MockHost, views::Views};
    const SECRET: &str = "TOP_SECRET_TOKEN_123";
    fn config() -> Value {
        json!({"apiVersion":"v1","kind":"Config","clusters":[{"name":"cluster-a","cluster":{"server":"https://api.example.test:6443","certificate-authority-data":"Q0E="}}],"users":[{"name":"user-a","user":{"token":SECRET}}],"contexts":[{"name":"dev","context":{"cluster":"cluster-a","user":"user-a","namespace":"tenant-a"}},{"name":"other","context":{"cluster":"cluster-a","user":"user-a","namespace":"tenant-b"}}],"current-context":"dev"})
    }
    fn host() -> MockHost {
        let host = MockHost::install().with_capabilities(&["storage"]);
        host.set_project_root(None);
        host
    }
    fn queue(host: &MockHost, name: &str, raw: &str, approve: bool) {
        host.answer_next_prompt(Some(&[raw]));
        host.answer_next_prompt(Some(&[name]));
        host.answer_next_prompt(Some(&["dev"]));
        host.answer_next_prompt(if approve { Some(&["ok"]) } else { None });
    }
    fn seed(host: &MockHost) {
        queue(host, "development", &config().to_string(), true);
        import().unwrap();
    }
    fn use_profile(host: &MockHost, kube: &mut crate::KubeExplorer, id: &str) {
        host.answer_next_prompt(Some(&[id]));
        host.answer_next_prompt(Some(&["ok"]));
        kube.run_command("use-imported-kubeconfig", &host.invocation(), None).unwrap();
    }

    #[test]
    fn imported_profile_used_in_kubectl_args() {
        let host = host().with_capabilities(&["storage", "process:kubectl"]);
        host.on_process(crate::KUBECTL, |_, _| Ok(openidea_plugin::ProcessOutput { status: Some(0), stdout: String::new(), stderr: String::new() }));
        seed(&host);
        let id = profiles().unwrap()[0]["id"].as_str().unwrap().to_owned();
        let mut kube = crate::KubeExplorer { context: Some("stale".into()), ..Default::default() };
        assert!(kube.args(&["get"]).iter().all(|arg| !arg.starts_with("--kubeconfig")));

        use_profile(&host, &mut kube, &id);
        let path = selected_path().unwrap();
        assert_eq!(kube.args(&["get"])[0], format!("--kubeconfig={path}"));
        assert!(kube.context.is_none(), "a context of the old kubeconfig is dropped");
        kube.kubectl(&["get", "pods"], None).unwrap();
        let run = host.process_runs().into_iter().last().unwrap();
        assert_eq!(run.args[0], format!("--kubeconfig={path}"));
        let written: Value = serde_json::from_str(&host::fs::read_file(&path).unwrap()).unwrap();
        assert_eq!(written["current-context"], "dev");
        assert_eq!(written["users"][0]["user"]["token"], SECRET);
        assert_eq!(selected_name().as_deref(), Some("development"));
        assert!(!format!("{:?}{:?}", host.logs(), host.notifications()).contains(SECRET));

        host.answer_next_prompt(Some(&[""]));
        kube.run_command("use-imported-kubeconfig", &host.invocation(), None).unwrap();
        assert!(kube.args(&["get"]).iter().all(|arg| !arg.starts_with("--kubeconfig")));
        assert!(host::fs::read_bytes(&path).unwrap().is_empty());
    }

    #[test]
    fn sixty_five_import_delete_cycles_still_import() {
        let host = host();
        for n in 0..65 {
            let mut cfg = config();
            cfg["clusters"][0]["cluster"]["server"] = json!(format!("https://api{n}.example.test:6443"));
            queue(&host, "cycle", &cfg.to_string(), true);
            import().unwrap_or_else(|error| panic!("cycle {n}: {error}"));
            let meta = profiles().unwrap().remove(0);
            host::fs::write_bytes(meta["path"].as_str().unwrap(), b"").unwrap();
            host::storage::delete(&format!("{PREFIX}{}", meta["id"].as_str().unwrap())).unwrap();
        }
    }

    #[test]
    fn delete_imported_profile_removes_file() {
        let host = host().with_capabilities(&["storage", "process:kubectl"]);
        host.on_process(crate::KUBECTL, |_, _| Ok(openidea_plugin::ProcessOutput { status: Some(0), stdout: String::new(), stderr: String::new() }));
        seed(&host);
        let meta = profiles().unwrap().remove(0);
        let id = meta["id"].as_str().unwrap().to_owned();
        let record_path = meta["path"].as_str().unwrap().to_owned();
        let mut kube = crate::KubeExplorer::default();
        use_profile(&host, &mut kube, &id);
        let active = selected_path().unwrap();

        host.answer_next_prompt(Some(&[&id]));
        host.answer_next_prompt(Some(&["ok"]));
        kube.run_command("delete-imported-kubeconfig", &host.invocation(), None).unwrap();
        assert!(profiles().unwrap().is_empty());
        assert!(selected_path().is_none() && crate::config::kubeconfig().is_none());
        for file in [&record_path, &active] {
            assert!(!host::fs::read_file(file).unwrap().contains(SECRET), "{file}");
        }
        assert!(kube.args(&["get"]).iter().all(|arg| !arg.starts_with("--kubeconfig")));
        assert!(!format!("{:?}{:?}", host.logs(), host.notifications()).contains(SECRET));

        seed(&host);
        assert_eq!(profiles().unwrap().len(), 1, "the same kubeconfig can be imported again");
    }

    #[test]
    fn ipv6_private_import_and_review_keep_exact_selected_server_without_connection() {
        let host=host();let server="https://[2001:db8::433]:6443";let mut cfg=config();
        cfg["clusters"][0]["cluster"]["server"]=json!(server);
        queue(&host,"ipv6-private",&cfg.to_string(),true);
        let mut kube=crate::KubeExplorer::default();
        kube.run_command("import-pasted-kubeconfig",&host.invocation(),None).unwrap();
        let profiles=profiles().unwrap();assert_eq!(profiles.len(),1);let meta=&profiles[0];
        assert_eq!(meta["server"],server);assert_eq!(meta["context"],"dev");assert_eq!(meta["namespace"],"tenant-a");
        let private=host::fs::read_file(meta["path"].as_str().unwrap()).unwrap();
        let payload:Value=serde_json::from_str(&private).unwrap();assert_eq!(payload["config"]["clusters"][0]["cluster"]["server"],server);
        assert!(private.contains(SECRET));assert!(!private.contains("tenant-b"));
        assert!(!format!("{:?}",host.storage()).contains(SECRET));assert!(!redacted(meta).contains(SECRET));
        host.answer_next_prompt(Some(&[meta["id"].as_str().unwrap()]));host.answer_next_prompt(Some(&["ok"]));
        kube.run_command("review-imported-kubeconfigs",&host.invocation(),None).unwrap();
        assert!(host.process_runs().is_empty()&&host.terminals().is_empty()&&host.requests().is_empty());
        assert!(crate::config::kubeconfig().is_none());assert!(kube.context.is_none()&&kube.namespace.is_none());
        assert!(!format!("{:?}{:?}",host.logs(),host.notifications()).contains(SECRET));
    }
    #[test]
    fn ipv6_server_subset_accepts_only_bracketed_literals_and_existing_port_shape() {
        for address in ["https://[::1]","https://[2001:db8::1]:6443","https://[2001:0db8:0000:0000:0000:0000:0000:0001]/","https://[::ffff:192.0.2.1]:443"] {
            let mut cfg=config();cfg["clusters"][0]["cluster"]["server"]=json!(address);
            let parsed=validated(&cfg.to_string()).unwrap();assert_eq!(selected(&parsed,"dev").unwrap()["clusters"][0]["cluster"]["server"],address);
        }
        for address in ["https://2001:db8::1","https://[2001:db8::1","https://2001:db8::1]","https://[]","https://[2001:db8::g]","https://[::1]:0","https://[::1]:65536","https://[::1]:+443","https://[::1]:","https://[::1]tail","https://[::1]:443/path","https://[::1]?token=example","https://user:password@[::1]","https://[fe80::1%25en0]","https://[::1]#fragment","https://[v1.future]"] {
            let mut cfg=config();cfg["clusters"][0]["cluster"]["server"]=json!(address);
            assert!(validated(&cfg.to_string()).is_err(),"outside bounded IPv6 input subset: {address}");
        }
    }
    #[test]
    fn yaml_and_json_parse_into_the_same_supported_single_context() {
        let json = validated(&config().to_string()).unwrap();
        let yaml=validated("apiVersion: v1\nkind: Config\nclusters:\n- name: cluster-a\n  cluster:\n    server: https://api.example.test:6443\nusers:\n- name: user-a\n  user:\n    token: secret\ncontexts:\n- name: dev\n  context:\n    cluster: cluster-a\n    user: user-a\ncurrent-context: dev\n").unwrap();
        assert_eq!(selected(&yaml, "dev").unwrap()["contexts"][0]["context"]["namespace"], "default");
        assert_eq!(selected(&json, "dev").unwrap()["contexts"].as_array().unwrap().len(), 1);
    }
    #[test]
    fn import_persists_secrets_only_privately_and_review_is_redacted_without_process_grants() {
        let host = host();
        seed(&host);
        let entries = profiles().unwrap();
        assert_eq!(entries.len(), 1);
        let meta = &entries[0];
        assert_eq!(meta["namespace"], "tenant-a");
        assert_eq!(meta["context"], "dev");
        let private = host::fs::read_file(meta["path"].as_str().unwrap()).unwrap();
        assert!(private.contains(SECRET));
        assert!(!private.contains("tenant-b"));
        let summary = redacted(meta);
        assert!(!summary.contains(SECRET));
        assert!(!summary.contains("Q0E="));
        assert!(!format!("{:?}", host.storage()).contains(SECRET));
        let id = meta["id"].as_str().unwrap();
        host.answer_next_prompt(Some(&[id]));
        host.answer_next_prompt(Some(&["ok"]));
        review().unwrap();
        assert!(host.process_runs().is_empty());
        assert!(host.terminals().is_empty());
        assert!(host.requests().is_empty());
        assert!(!format!("{:?}{:?}", host.logs(), host.notifications()).contains(SECRET));
    }
    #[test]
    fn cancellation_does_not_store_payload_or_metadata() {
        let host = host();
        queue(&host, "cancelled", &config().to_string(), false);
        import().unwrap();
        assert!(host.storage().is_empty());
        assert!(host::fs::read_dir(&host::fs::plugin_dir().unwrap()).unwrap().is_empty());
    }
    #[test]
    fn executable_or_external_credential_fields_and_insecure_server_options_are_rejected() {
        for field in ["exec", "auth-provider", "tokenFile", "client-certificate", "client-key"] {
            let mut cfg = config();
            cfg["users"][0]["user"][field] = json!({"command":"must-not-run"});
            assert!(validated(&cfg.to_string()).is_err(), "{field}");
        }
        for field in ["certificate-authority", "proxy-url", "tls-server-name"] {
            let mut cfg = config();
            cfg["clusters"][0]["cluster"][field] = json!("external");
            assert!(validated(&cfg.to_string()).is_err(), "{field}");
        }
        let mut cfg = config();
        cfg["clusters"][0]["cluster"]["insecure-skip-tls-verify"] = json!(true);
        assert!(validated(&cfg.to_string()).is_err());
        for url in [
            "http://api.example.test",
            "https://name:password@api.example.test",
            "https://api.example.test?token=SECRET",
            "https://api.example.test/path",
            "https://999.0.0.1",
            "https://127.1",
            "https://api.example.test:+443",
        ] {
            let mut cfg = config();
            cfg["clusters"][0]["cluster"]["server"] = json!(url);
            let error = validated(&cfg.to_string()).err().unwrap().to_string();
            assert!(!error.contains("password"));
            assert!(!error.contains("SECRET"));
        }
    }
    #[test]
    fn duplicate_alias_tag_depth_size_and_malformed_inputs_are_bounded_and_secret_free() {
        for raw in [
            "kind: Config\nkind: Config",
            "a: &anchor [x]\nb: *anchor",
            "a: !!str test",
            "---\na: b\n---\nc: d",
            "secret: [TOP_SECRET_TOKEN_123",
        ] {
            let error = parse(raw).unwrap_err().to_string();
            assert!(!error.contains(SECRET));
        }
        assert!(parse(&"x".repeat(MAX_INPUT + 1)).is_err());
        assert!(parse(&format!("{}x{}", "[".repeat(30), "]".repeat(30))).is_err());
        assert!(parse(r#"{"kind":"Config","kind":"Config"}"#).is_err());
    }
    #[test]
    fn duplicate_or_unknown_references_namespaces_and_auth_modes_do_not_replace_existing_profile() {
        let host = host();
        seed(&host);
        let before = host.storage();
        let old = profiles().unwrap();
        let path = old[0]["path"].as_str().unwrap();
        let private = host::fs::read_file(path).unwrap();
        let mut duplicate = config();
        let user = duplicate["users"][0].clone();
        duplicate["users"].as_array_mut().unwrap().push(user);
        let mut unknown = config();
        unknown["contexts"][0]["context"]["cluster"] = json!("missing");
        let mut namespace = config();
        namespace["contexts"][0]["context"]["namespace"] = json!("--all-namespaces");
        for cfg in [duplicate, unknown, namespace] {
            host.answer_next_prompt(Some(&[&cfg.to_string()]));
            assert!(import().is_err());
            assert_eq!(host.storage(), before);
            assert_eq!(host::fs::read_file(path).unwrap(), private);
        }
    }
    #[test]
    fn profile_label_traversal_and_duplicate_labels_never_overwrite() {
        let host = host();
        host.answer_next_prompt(Some(&[&config().to_string()]));
        host.answer_next_prompt(Some(&["../escape"]));
        assert!(import().is_err());
        assert!(host.storage().is_empty());
        seed(&host);
        let before = host.storage();
        host.answer_next_prompt(Some(&[&config().to_string()]));
        host.answer_next_prompt(Some(&["development"]));
        assert!(import().is_err());
        assert_eq!(host.storage(), before);
    }
    #[test]
    fn private_payload_tampering_fails_closed_without_leaking_values() {
        let host = host();
        seed(&host);
        let old = profiles().unwrap();
        let before = host.storage();
        host::fs::write_file(old[0]["path"].as_str().unwrap(), "TOP_SECRET_TOKEN_123 invalid").unwrap();
        let error = profiles().err().unwrap().to_string();
        assert!(!error.contains(SECRET));
        assert_eq!(host.storage(), before);
    }
    #[test]
    fn client_certificate_pair_and_anonymous_subset_are_supported_without_material_validation_claims() {
        let mut cfg = config();
        cfg["users"][0]["user"] = json!({"client-certificate-data":"Q0VSVA==","client-key-data":"S0VZ"});
        let parsed = validated(&cfg.to_string()).unwrap();
        let selected = selected(&parsed, "dev").unwrap();
        let meta = metadata("cert", &selected, "id", "private");
        assert!(!redacted(&meta).contains("Q0VSVA=="));
        assert!(!redacted(&meta).contains("S0VZ"));
        cfg["users"] = json!([]);
        for context in cfg["contexts"].as_array_mut().unwrap() {
            context["context"].as_object_mut().unwrap().remove("user");
        }
        let parsed = validated(&cfg.to_string()).unwrap();
        let cfg = super::selected(&parsed, "dev").unwrap();
        assert!(redacted(&metadata("anonymous", &cfg, "id", "private")).contains("anonymous"));
    }
    #[test]
    fn import_does_not_change_existing_active_scope_or_make_browsing_grant_required() {
        let host = host();
        host::storage::set("context", "active-prod").unwrap();
        host::storage::set("namespace", "production").unwrap();
        let mut kube = crate::KubeExplorer::default();
        kube.context = Some("active-prod".into());
        kube.namespace = Some("production".into());
        let before = kube.args(&["get", "pods"]);
        queue(&host, "development", &config().to_string(), true);
        kube.run_command("import-pasted-kubeconfig", &host.invocation(), None).unwrap();
        assert_eq!(kube.args(&["get", "pods"]), before);
        assert_eq!(host.storage()["context"], "active-prod");
        assert_eq!(host.storage()["namespace"], "production");
        let _ = kube.render("kube", None, &host.invocation());
        assert!(host.process_runs().is_empty());
        assert!(host.terminals().is_empty());
        assert!(host.requests().is_empty());
    }
}

#[cfg(test)]
mod boundary_regressions {
    use super::*;
    use openidea_plugin::mock::MockHost;
    #[test]
    fn credential_values_reused_as_context_or_profile_labels_are_not_published() {
        let secret = "SECRET_TOKEN_123";
        let cfg = json!({"apiVersion":"v1","kind":"Config","clusters":[{"name":"cluster","cluster":{"server":"https://api.example.test"}}],"users":[{"name":"user","user":{"token":secret}}],"contexts":[{"name":secret,"context":{"cluster":"cluster","user":"user"}}],"current-context":secret});
        assert!(validated(&cfg.to_string()).is_err());
        let host = MockHost::install().with_capabilities(&["storage"]);
        let mut cfg = cfg;
        cfg["contexts"][0]["name"] = json!("dev");
        cfg["current-context"] = json!("dev");
        host.answer_next_prompt(Some(&[&cfg.to_string()]));
        host.answer_next_prompt(Some(&[secret]));
        assert!(import().is_err());
        assert!(host.storage().is_empty());
        assert!(host::fs::read_dir(&host::fs::plugin_dir().unwrap()).unwrap().is_empty());
    }
    #[test]
    fn metadata_path_cannot_be_redirected_outside_the_private_directory() {
        let host = MockHost::install().with_capabilities(&["storage"]);
        let id = "0".repeat(64);
        host::storage::set(&format!("{PREFIX}{id}"), &json!({"schema":1,"id":id,"path":"/outside/credentials"}).to_string()).unwrap();
        assert!(profiles().err().unwrap().to_string().contains("saved location"));
        assert!(host.process_runs().is_empty());
        assert!(host.requests().is_empty());
    }
}

#[cfg(test)]
mod persistence_regressions {
    use super::*;
    use openidea_plugin::mock::MockHost;
    fn fixture() -> (String, String) {
        let raw=json!({"apiVersion":"v1","kind":"Config","clusters":[{"name":"c","cluster":{"server":"https://api.example.test"}}],"users":[],"contexts":[{"name":"dev","context":{"cluster":"c"}}],"current-context":"dev"}).to_string();
        let parsed = validated(&raw).unwrap();
        let config = selected(&parsed, "dev").unwrap();
        (raw, json!({"schema":1,"name":"recovery","config":config}).to_string())
    }
    fn queue(host: &MockHost, raw: &str) {
        host.answer_next_prompt(Some(&[raw]));
        host.answer_next_prompt(Some(&["recovery"]));
        host.answer_next_prompt(Some(&["dev"]));
        host.answer_next_prompt(Some(&["ok"]));
    }
    #[test]
    fn identical_unpublished_private_payload_can_be_reused_after_metadata_failure() {
        let host = MockHost::install().with_capabilities(&["storage"]);
        let (raw, payload) = fixture();
        let path = format!("{}/{FILE_PREFIX}{}.json", host::fs::plugin_dir().unwrap(), digest(payload.as_bytes()));
        host::fs::write_file(&path, &payload).unwrap();
        queue(&host, &raw);
        import().unwrap();
        assert_eq!(profiles().unwrap().len(), 1);
        assert_eq!(host::fs::read_file(&path).unwrap(), payload);
        assert!(host.process_runs().is_empty());
    }
    #[test]
    fn preexisting_different_private_payload_is_not_overwritten_or_published() {
        let host = MockHost::install().with_capabilities(&["storage"]);
        let (raw, payload) = fixture();
        let path = format!("{}/{FILE_PREFIX}{}.json", host::fs::plugin_dir().unwrap(), digest(payload.as_bytes()));
        host::fs::write_file(&path, "existing different private content").unwrap();
        queue(&host, &raw);
        assert!(import().is_err());
        assert!(host.storage().is_empty());
        assert_eq!(host::fs::read_file(&path).unwrap(), "existing different private content");
    }
}
