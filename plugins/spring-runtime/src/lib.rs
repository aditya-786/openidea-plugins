//! Spring Runtime Insight for OpenIDEA: reads a running Spring Boot
//! application's Actuator endpoints on localhost into a snapshot (on demand,
//! never from an editor provider), shows it in a tool window and marks the
//! beans, `@Value` keys and configuration keys it knows about in the editor.

mod panes;
mod request_conditions;
mod snapshot;
mod snapshot_database;
mod graph;
mod persistence;
mod controls;
mod jmx;
use persistence::Scope;

pub use snapshot::*;

use openidea_plugin::providers::{Document, LineMarker, Providers};
use openidea_plugin::text::position;
use openidea_plugin::views::{Body, Button, TreeItem, View, ViewEvent, Viewport, Views};
use openidea_plugin::{Diagnostic, Error, Event, HttpRequest, Invocation, LogLevel, Plugin, Project, Severity, host, register_plugin};
use serde_json::Value;
use spring_text::config::{self, canonical};
use spring_text::index::{COMMAND_BUDGET, Indexes};
use spring_text::java;
use std::collections::{BTreeMap, BTreeSet};
use std::time::{SystemTime, UNIX_EPOCH, Instant, Duration};

const VIEW: &str = "runtime";
const MAX_ENDPOINT_BYTES: usize = 4 << 20;
const MAX_FETCH_BYTES: usize = 16 << 20;
const DEFAULT_URL: &str = "http://localhost:8080/actuator";
const CONFIG_GLOBS: [&str; 6] = [
    "**/application*.properties",
    "**/application*.yml",
    "**/application*.yaml",
    "**/bootstrap*.properties",
    "**/bootstrap*.yml",
    "**/bootstrap*.yaml",
];

/// Where a Java type of the project is, for opening runtime items in source.
#[derive(Clone, Debug)]
struct JavaType {
    fqn: String,
    line: u32,
    column: u32,
    methods: Vec<(String, u32, u32)>,
    /// Per method, parallel to `methods`: parameter simple type names (`int[]` for arrays).
    params: Vec<Vec<String>>,
    /// The one literal `@ConfigurationProperties` prefix of the type, if it has exactly that.
    config_prefix: Option<String>,
    /// Set when the file was too big to index: its package and file stem, to tell what it might hold.
    incomplete: Option<(String, String)>,
}

impl JavaType {
    /// Whether an unindexed file could declare `class` (qualified, or `Outer.Inner`).
    fn could_hold(&self, class: &str) -> bool {
        let Some((package, stem)) = &self.incomplete else { return false };
        let in_package = !package.is_empty() && class.strip_prefix(package.as_str()).is_some_and(|rest| rest.starts_with('.'));
        in_package || class.split('.').any(|part| part == stem) || (package.is_empty() && !class.contains('.'))
    }
}

fn file_stem(path: &str) -> String {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    name.strip_suffix(".java").unwrap_or(name).to_owned()
}

fn incomplete_file(path: &str, package: &str) -> Option<Vec<JavaType>> {
    Some(vec![JavaType {
        fqn: "\0incomplete".into(),
        line: 0,
        column: 0,
        methods: Vec::new(),
        params: Vec::new(),
        config_prefix: None,
        incomplete: Some((package.to_owned(), file_stem(path))),
    }])
}

fn index_java(path: &str, text: &str) -> Option<Vec<JavaType>> {
    if text.len()>512<<10 {
        let mut head=text.len().min(8<<10);
        while !text.is_char_boundary(head) {head-=1;}
        return incomplete_file(path,&java::parse(&text[..head]).package);
    }
    let file = java::parse(text);
    if file.types.len()>512 || file.types.iter().map(|t|t.members.len()).sum::<usize>()>4096 {return incomplete_file(path,&file.package);}
    let at = |offset| {
        let p = position(text, offset);
        (p.line, p.character)
    };
    let types: Vec<JavaType> = file
        .types
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let (line, column) = at(t.at);
            let methods = t.methods().map(|m| (m.name.clone(), at(m.at).0, at(m.at).1)).collect();
            let params = t
                .methods()
                .map(|m| m.params.iter().map(|p| if p.ty.array { format!("{}[]", p.ty.name) } else { p.ty.name.clone() }).collect())
                .collect();
            let config_prefix = t.annotation("ConfigurationProperties").and_then(|a| {
                let arg = a.arg("prefix").or_else(|| a.arg("value"))?;
                let [only] = arg.strings.as_slice() else { return None };
                (arg.text.trim().len() == only.value.len() + 2).then(|| only.value.clone())
            });
            JavaType { fqn: file.qualified(i), line, column, methods, params, config_prefix, incomplete: None }
        })
        .collect();
    (!types.is_empty()).then_some(types)
}

/// A config file's keys (relaxed form) and where they are.
fn index_config(path: &str, text: &str) -> Option<Vec<(String, u32, u32)>> {
    config::is_config_file(path).then(|| {
        config::keys(path, text)
            .into_iter()
            .map(|k| {
                let p = position(text, k.at);
                (canonical(&k.key), p.line, p.character)
            })
            .collect()
    })
}

/// What activating a tool window item opens.
#[derive(Clone, Debug, PartialEq)]
enum Target {
    /// A class by qualified (or `Outer.Inner`) name, at a method if given.
    Type(String, Option<String>),
    /// A configuration key, preferably in the named file.
    Key(String, Option<String>),
    File(String),
    /// A class by exact qualified name, at the one method whose parameter simple type names are these.
    Overload(String, String, Vec<String>),
    /// A `@ConfigurationProperties` literal prefix and the reported bean type.
    Props(String, String),
}

impl Target {
    fn bounded(self)->Option<Self> {
        let valid=match &self {
            Self::Type(class,method)=>class.len()<=4096 && method.as_ref().is_none_or(|m|m.len()<=512),
            Self::Key(key,file)=>key.len()<=4096 && file.as_ref().is_none_or(|f|f.len()<=1024),
            Self::File(file)=>file.len()<=1024,
            Self::Overload(class,method,params)=>class.len()<=4096 && method.len()<=512 && params.len()<=64,
            Self::Props(prefix,ty)=>prefix.len()<=4096 && ty.len()<=4096,
        };
        valid.then_some(self)
    }
}

/// Checks the Actuator URL: plain HTTP is only allowed to this machine, and
/// the plugin only asks for `net:localhost` and `net:127.0.0.1`.
pub fn check_url(url: &str) -> Result<String, String> {
    let url=url.trim().trim_end_matches('/');
    let bad=||format!("Spring Runtime only reads applications running on this machine. Set Settings › Plugins › Spring Runtime Insight › Actuator base URL to http://localhost:<port>/actuator (without credentials, query or fragment).");
    if url.len()>2048 || url.chars().any(|c|c.is_control() || c.is_whitespace()) || url.contains(['@','?','#','\\']) {return Err(bad());}
    let (scheme,rest)=url.strip_prefix("http://").map(|r|("http",r)).or_else(||url.strip_prefix("https://").map(|r|("https",r))).ok_or_else(bad)?;
    let (authority,path)=rest.split_once('/').map_or((rest,""),|(a,p)|(a,p));
    let (host,port)=authority.split_once(':').map_or((authority,None),|(h,p)|(h,Some(p)));
    let host=host.to_ascii_lowercase();if !matches!(host.as_str(),"localhost"|"127.0.0.1") {return Err(bad());}
    let port=match port {Some(p)=>{if !p.bytes().all(|b|b.is_ascii_digit()) {return Err(bad());}let p=p.parse::<u16>().ok().filter(|p|*p>0).ok_or_else(bad)?;format!(":{p}")},None=>String::new()};
    Ok(format!("{scheme}://{host}{port}{}",if path.is_empty(){String::new()}else{format!("/{path}")}))
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn age(at: u64) -> String {
    if at==0 {return "at an unrecorded date".into();}
    if at>now() {return format!("at a future timestamp (Unix {at})");}
    match now().saturating_sub(at) {
        0..60 => "just now".into(),
        s @ 60..3600 => format!("{} min ago", s / 60),
        s => format!("{} h ago", s / 3600),
    }
}

/// Turns a denied optional capability into something the user can act on.
fn allow<T>(result: Result<T, Error>, capability: &str) -> Result<T, Error> {
    result.map_err(|error| match error {
        Error::Denied(_) => Error::Failed(format!("Allow {capability} for Spring Runtime Insight in Settings › Plugins.")),
        other => other,
    })
}

fn simple(ty: &str) -> &str {
    ty.rsplit('.').next().unwrap_or(ty)
}

fn package(ty: &str) -> &str {
    ty.rsplit_once('.').map_or("", |(p, _)| p)
}

/// `com.acme.Jobs.cleanup` → (`com.acme.Jobs`, `cleanup`).
fn class_and_method(target: &str) -> Target {
    match target.rsplit_once('.') {
        Some((class, method)) => Target::Type(class.into(), Some(method.into())),
        None => Target::Type(target.into(), None),
    }
}

/// The keys of the `${key:default}` placeholders in a string.
fn placeholders(s: &str) -> Vec<&str> {
    s.match_indices("${")
        .filter_map(|(i, _)| {
            let rest = &s[i + 2..];
            let end = rest.find([':', '}'])?;
            Some(rest[..end].trim()).filter(|k| !k.is_empty())
        })
        .collect()
}

fn presentation(text:&str,limit:usize)->String {
    let mut result=String::new();
    for ch in text.chars() {
        let ch=if ch.is_control() && !matches!(ch,'\n'|'\t') {'\u{fffd}'} else {ch};
        if result.len()+ch.len_utf8()>limit {result.push_str("… (truncated)");return result;}
        result.push(ch);
    }
    result
}

fn marker(text: &str, at: usize, tooltip: String) -> LineMarker {
    LineMarker { line: position(text, at).line, icon: Some(RUNTIME_MARKER.into()), tooltip:presentation(&tooltip,4096), target: None }
}

/// The token is a secret the host substitutes into the header; the plugin never reads it.
pub fn actuator_request(url: &str, endpoint: &str, timeout: u32, auth: &str) -> HttpRequest {
    let request = HttpRequest { timeout_ms: Some(timeout), ..HttpRequest::get(&format!("{url}/{endpoint}")) };
    match auth {
        "bearer" => request.header("Authorization", "Bearer {{secret:actuator-token}}"),
        "basic" => request.header("Authorization", "Basic {{secret:actuator-token}}"),
        _ => request,
    }
}

/// Said in the view when an endpoint answered 401 or 403.
pub fn auth_hint(missing: &[String]) -> Option<&'static str> {
    missing
        .iter()
        .any(|m| m.ends_with("(401)") || m.ends_with("(403)"))
        .then_some("needs authentication: check actuatorAuth and the actuator-token secret")
}

#[cfg(test)]
fn fetch_snapshot() -> Result<Snapshot, Error> {
    let url=check_url(&host::setting_string("actuatorUrl").unwrap_or_else(||DEFAULT_URL.into()))?;
    let auth=host::setting_string("actuatorAuth").unwrap_or_default();
    fetch_snapshot_for(&url,&auth)
}

#[cfg(test)]
fn fetch_snapshot_for(url:&str,auth:&str)->Result<Snapshot,Error>{fetch_snapshot_guarded(url,auth,||Ok(()))}
fn fetch_snapshot_guarded(url:&str,auth:&str,mut guard:impl FnMut()->Result<(),Error>) -> Result<Snapshot, Error> {
    let timeout=controls::timeout();
    let deadline=Instant::now()+Duration::from_millis(u64::from(controls::budget()));
    let task = host::begin_task("Reading Spring Boot Actuator endpoints", true);
    let mut snap = Snapshot { url: url.to_owned(), fetched_at: now(), ..Snapshot::default() };
    let mut bytes=0usize;
    for (i, endpoint) in ENDPOINTS.iter().enumerate() {
        guard()?;
        let remaining=deadline.saturating_duration_since(Instant::now()).as_millis().min(u128::from(u32::MAX)) as u32;
        if remaining<100 {return Err("Actuator refresh deadline reached; previous snapshot retained.".into());}
        if task.is_cancelled() {
            return Err(Error::Cancelled);
        }
        task.report(i as u32, Some(ENDPOINTS.len() as u32), Some(endpoint));
        let request = actuator_request(&url, endpoint, timeout.min(remaining), &auth);
        let fetched=host::fetch(&request);
        guard()?;
        if task.is_cancelled(){return Err(Error::Cancelled);}
        if Instant::now()>=deadline {return Err("Actuator refresh deadline reached; previous snapshot retained.".into());}
        let response = match fetched {
            Ok(r) => r,
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            Err(Error::Denied(cap)) if cap == "secrets" => {
                return Err(format!(
                    "{} Or set actuatorAuth to none.",
                    ui_kit::denied::message("The actuator-token secret", "secrets", "Spring Runtime Insight")
                )
                .into());
            }
            Err(Error::Denied(cap)) => {
                let cap=if matches!(cap.as_str(),"net:localhost"|"net:127.0.0.1"){cap.as_str()}else{"net:localhost"};
                return Err(ui_kit::denied::message("Refreshing the runtime snapshot",cap,"Spring Runtime Insight").into());
            }
            Err(Error::NotFound(_)) if i == 0 && matches!(auth, "bearer" | "basic") && !host::secret_is_set("actuator-token") => {
                return Err("Set the actuator-token secret in Settings › Plugins › Spring Runtime Insight, or set actuatorAuth to none.".into());
            }
            // The application isn't up: don't wait for every endpoint to time out.
            Err(_) if i == 0 => {
                return Err(format!("Could not reach requested Actuator URL {url}. Transport failure details are omitted; verify the application and endpoint configuration.").into());
            }
            Err(_) => {
                snap.missing.push(format!("{endpoint} (transport failed; details omitted)"));
                continue;
            }
        };
        if response.status != 200 {
            snap.missing.push(format!("{endpoint} ({})", response.status));
            continue;
        }
        bytes=bytes.saturating_add(response.body.len());
        if response.body.len()>MAX_ENDPOINT_BYTES || bytes>MAX_FETCH_BYTES {return Err("Actuator responses exceed the bounded refresh limit; previous snapshot retained.".into());}
        match serde_json::from_slice::<Value>(&response.body) {
            Ok(json) if snapshot::supported_shape(endpoint,&json) => snap.read(endpoint, &json),
            Ok(_) => snap.missing.push(format!("{endpoint} (unsupported JSON shape)")),
            Err(_) => snap.missing.push(format!("{endpoint} (not JSON)")),
        }
    }
    if snap.missing.len() == ENDPOINTS.len() {
        return Err(format!(
            "No supported Actuator endpoint payload at {url}. Verify exposure and the supported endpoint response shapes. {}",auth_hint(&snap.missing).unwrap_or("")
        )
        .into());
    }
    guard()?;
    if task.is_cancelled() {return Err(Error::Cancelled);}
    if Instant::now()>=deadline {return Err("Actuator refresh deadline reached; previous snapshot retained.".into());}
    task.report(ENDPOINTS.len() as u32,Some(ENDPOINTS.len() as u32),Some("Endpoint reads complete; scoped adoption still required"));
    Ok(snap)
}

/// Builds the tool window's tree; only expanded items get their children listed.
struct Tree<'a> {
    items: Vec<TreeItem>,
    expanded: &'a BTreeSet<String>,
    targets: &'a mut BTreeMap<String, Target>,
    bytes: usize,
    limited: bool,
}

impl Tree<'_> {
    fn push(&mut self,mut item:TreeItem)->bool {
        if item.key.len()>4096 || item.parent.as_ref().is_some_and(|p|p.len()>4096) || self.items.len()>=5000 {self.limited=true;return false;}
        item.label=presentation(&item.label,1024);item.detail=item.detail.map(|s|presentation(&s,4096));
        let bytes=item.key.len()+item.parent.as_ref().map_or(0,String::len)+item.label.len()+item.detail.as_ref().map_or(0,String::len)+item.icon.as_ref().map_or(0,String::len)+96;
        if self.bytes.saturating_add(bytes)>3<<20 {self.limited=true;return false;}
        self.bytes+=bytes;self.items.push(item);true
    }

    /// Adds an item; whether its children are to be listed.
    fn node(&mut self, key: &str, parent: Option<&str>, label: &str, detail: &str, children: bool) -> bool {
        let mut item = TreeItem::new(key, label);
        if let Some(p) = parent {
            item = item.parent(p);
        }
        if !detail.is_empty() {
            item = item.detail(detail);
        }
        let open = children && self.expanded.contains(key);
        if children {
            item = item.expandable(open);
        }
        if !self.push(item) {return false;}
        open
    }

    /// [`Tree::node`] with an icon.
    fn group(&mut self, icon: &str, key: &str, parent: Option<&str>, label: &str, detail: &str, children: bool) -> bool {
        let before=self.items.len();
        let open = self.node(key, parent, label, detail, children);
        if self.items.len()>before {
            let item=self.items.last_mut().expect("added");
            item.icon = Some(icon.into());
        }
        open
    }

    fn leaf(&mut self, key: &str, parent: &str, label: &str, detail: &str, target: Option<Target>) {
        let before=self.items.len();
        self.node(key, Some(parent), label, detail, false);
        if self.items.len()>before {if let Some(t)=target.and_then(Target::bounded) {self.targets.insert(key.into(),t);}}
    }

    fn beans(&mut self, snap: &Snapshot) {
        if !snap.has("beans") || !self.group("spring-boot", "beans", None, "Beans", &snap.beans.len().to_string(), true) {
            return;
        }
        let mut contexts:BTreeMap<&str,Vec<(usize,&Bean)>>=BTreeMap::new();
        for (i,b) in snap.beans.iter().enumerate() {contexts.entry(&b.context).or_default().push((i,b));}
        for (context,beans) in contexts {
            if self.limited {break;}
            let ck = format!("beans/{context}");
            let count = beans.len();
            if !self.node(&ck, Some("beans"), context, &count.to_string(), true) {
                continue;
            }
            for (i, b) in beans {
                if self.limited {break;}
                let bk = format!("bean/{i}");
                let detail = format!("{} · {}", b.scope, simple(&b.ty));
                let before=self.items.len();let open=self.node(&bk, Some(&ck), &b.name, &detail, !b.deps.is_empty());
                if self.items.len()>before && b.ty.len()<=4096 {self.targets.insert(bk.clone(),Target::Type(b.ty.clone(),None));}
                if !open {
                    continue;
                }
                for (j, dep) in b.deps.iter().enumerate() {
                    let ty = snap.beans.iter().find(|d| d.context == context && d.name == *dep).map(|d| d.ty.as_str());
                    let target = ty.filter(|t|t.len()<=4096).map(|t| Target::Type(t.into(), None));
                    self.leaf(&format!("{bk}/{j}"), &bk, dep, ty.map(simple).unwrap_or_default(), target);
                }
            }
        }
    }

    /// Small: listed whole, every component open.
    fn health(&mut self, snap: &Snapshot) {
        let Some((_, status)) = snap.health.first().filter(|_| snap.has("health")) else {
            return;
        };
        let mut root = TreeItem::new("health", "Health").detail(status).icon(health_icon(status));
        if snap.health.len() > 1 {
            root = root.expandable(self.expanded.contains("health"));
        }
        let open = root.expanded;
        if !self.push(root) {return;}
        if !open {
            return;
        }
        self.health_details(snap, "");
        for (path, status) in &snap.health[1..] {
            let parent = path.rsplit_once('/').map_or("health".to_owned(), |(p, _)| format!("health/{p}"));
            let mut item =
                TreeItem::new(&format!("health/{path}"), simple_path(path)).parent(&parent).detail(status).icon(health_icon(status));
            if snap.health.iter().any(|(p, _)| p.starts_with(&format!("{path}/"))) || snap.health_details.iter().any(|(p, ..)| p == path) {
                item = item.expandable(true);
            }
            if !self.push(item) {break;}
            self.health_details(snap, path);
        }
    }

    fn health_details(&mut self, snap: &Snapshot, path: &str) {
        let parent = if path.is_empty() { "health".to_owned() } else { format!("health/{path}") };
        for (i, (_, key, value)) in snap.health_details.iter().enumerate().filter(|(_, (p, ..))| p == path) {
            if !self.push(TreeItem::new(&format!("{parent}#{i}"), key).parent(&parent).detail(value)) {
                return;
            }
        }
    }

    fn configuration(&mut self, snap: &Snapshot) {
        let profiles = match snap.profiles.as_slice() {
            [] => "default profile".to_owned(),
            p => format!("profiles: {}", p.join(", ")),
        };
        if !snap.has("env") || !self.group("settings", "env", None, "Configuration", &profiles, !snap.sources.is_empty()) {
            return;
        }
        let file_of=|s:&Source| {
            let value=short_source(&s.name).rsplit(['/',':']).next().unwrap_or("");
            (is_config_source(&s.name) && value.len()<=1024).then(||value.to_owned())
        };
        let files: Vec<&Source> = snap.sources.iter().filter(|s| is_config_source(&s.name)).collect();
        if !files.is_empty() && self.group("file-properties", "env/files", Some("env"), "Config files", &files.len().to_string(), true) {
            for (i, s) in files.iter().enumerate() {
                let detail = format!("{} properties", s.props.len());
                self.leaf(&format!("env/files/{i}"), "env/files", short_source(&s.name), &detail, file_of(s).map(Target::File));
            }
        }
        for (i, s) in snap.sources.iter().enumerate() {
            let sk = format!("env/{i}");
            let file=file_of(s);
            if !self.node(&sk, Some("env"), short_source(&s.name), &format!("{} properties", s.props.len()), !s.props.is_empty()) {
                continue;
            }
            for (j, (key, value)) in s.props.iter().enumerate() {
                self.leaf(&format!("{sk}/{j}"), &sk, key, value, (key.len()<=4096).then(||Target::Key(key.clone(),file.clone())));
            }
        }
    }

    fn conditions(&mut self, snap: &Snapshot) {
        if !snap.has("conditions") || !self.group("spring-config", "auto", None, "Auto-configuration", "", !snap.conditions.is_empty()) {
            return;
        }
        for (matched, key, label, icon) in [(true, "auto/+", "Positive matches", "success"), (false, "auto/-", "Negative matches", "error")]
        {
            let list: Vec<(usize, &Condition)> = snap.conditions.iter().enumerate().filter(|(_, c)| c.matched == matched).collect();
            if !self.group(icon, key, Some("auto"), label, &list.len().to_string(), !list.is_empty()) {
                continue;
            }
            for (i, c) in list {
                let ck = format!("{key}/{i}");
                let target = match c.key.split_once('#') {
                    Some((class, method)) => Target::Type(class.into(), Some(method.into())),
                    None => Target::Type(c.key.clone(), None),
                };
                let before=self.items.len();let open=self.node(&ck,Some(key),&c.key,"",!c.messages.is_empty());
                if self.items.len()>before {if let Some(target)=target.bounded() {self.targets.insert(ck.clone(),target);}}
                if open {
                    for (j, m) in c.messages.iter().enumerate() {
                        self.leaf(&format!("{ck}/{j}"), &ck, m, "", None);
                    }
                }
            }
        }
    }

    fn mappings(&mut self, snap: &Snapshot) {
        if !snap.has("mappings") || !self.group("web", "mappings", None, "Mappings", &snap.mappings.len().to_string(), true) {
            return;
        }
        let mut groups:BTreeMap<&str,Vec<(usize,&Mapping)>>=BTreeMap::new();
        for (i,m) in snap.mappings.iter().enumerate() {groups.entry(&m.group).or_default().push((i,m));}
        for (group,mappings) in groups {
            if self.limited {break;}
            let gk = format!("mappings/{group}");
            let count = mappings.len();
            if !self.node(&gk, Some("mappings"), group, &count.to_string(), true) {
                continue;
            }
            for (i, m) in mappings {
                if self.limited {break;}
                let target = m.method.clone().map(|(class, name)| Target::Type(class, Some(name))).or_else(|| {
                    let (class, rest) = m.handler.split_once('#')?;
                    let method = rest.split('(').next().unwrap_or(rest);
                    (!class.contains(' ')).then(|| Target::Type(class.into(), Some(method.into())))
                });
                self.leaf(&format!("mapping/{i}"), &gk, &m.predicate, &m.handler, target);
            }
        }
    }

    fn configprops(&mut self, snap: &Snapshot) {
        let count = snap.configprops.len().to_string();
        if !snap.has("configprops") || !self.group("property", "props", None, "Configuration properties", &count, true) {
            return;
        }
        for (i, p) in snap.configprops.iter().enumerate() {
            let pk = format!("props/{i}");
            let before=self.items.len();let open=self.node(&pk,Some("props"),&p.prefix,simple(&p.ty),!p.props.is_empty());
            if self.items.len()>before && p.ty.len()<=4096 {self.targets.insert(pk.clone(),Target::Type(p.ty.clone(),None));}
            if !open {
                continue;
            }
            for (j, (key, value)) in p.props.iter().enumerate() {
                let target = (p.prefix.len()+key.len()+1<=4096).then(||Target::Key(format!("{}.{key}", p.prefix), None));
                self.leaf(&format!("{pk}/{j}"), &pk, key, value, target);
            }
        }
    }

    fn scheduled(&mut self, snap: &Snapshot) {
        let count = snap.scheduled.len().to_string();
        if !snap.has("scheduledtasks") || !self.group("clock", "sched", None, "Scheduled tasks", &count, !snap.scheduled.is_empty()) {
            return;
        }
        for (i, t) in snap.scheduled.iter().enumerate() {
            let detail = format!("{} {}", t.kind, t.detail);
            self.leaf(&format!("sched/{i}"), "sched", &t.target, &detail, Some(class_and_method(&t.target)));
        }
    }

    fn loggers(&mut self, snap: &Snapshot) {
        let count = snap.loggers.len().to_string();
        if !snap.has("loggers") || !self.group("settings", "log", None, "Loggers", &count, !snap.loggers.is_empty()) {
            return;
        }
        for (i, l) in snap.loggers.iter().enumerate() {
            let detail = format!("configured {}, effective {}", l.configured, l.effective);
            self.leaf(&format!("log/{i}"), "log", &l.name, &detail, None);
        }
    }

    fn caches(&mut self, snap: &Snapshot) {
        let count = snap.caches.len().to_string();
        if !snap.has("caches") || !self.group("database", "cache", None, "Caches", &count, !snap.caches.is_empty()) {
            return;
        }
        for (i, c) in snap.caches.iter().enumerate() {
            let label = format!("{} / {}", c.manager, c.name);
            self.leaf(&format!("cache/{i}"), "cache", &label, simple(&c.target), None);
        }
    }
}

fn health_icon(status: &str) -> &'static str {
    match status {
        "UP" => "success",
        "DOWN" | "OUT_OF_SERVICE" => "error",
        _ => "warning",
    }
}

fn simple_path(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

pub struct Runtime {
    panes: panes::State,
    graph: graph::State,
    polling: controls::Polling,
    jmx: Option<(String, String)>,
    snapshot: Option<Snapshot>,
    /// Built from `snapshot` on first use by the editor providers; dropped whenever it changes.
    lookup: Option<Lookup>,
    /// Snapshots of the other projects the editor providers asked about; the view state never follows them.
    others: BTreeMap<Scope, Option<(Snapshot, Lookup)>>,
    scope: Option<Scope>,
    /// Storage was read (the snapshot may survive instance reclaim).
    loaded: bool,
    /// Why the last refresh failed.
    error: Option<String>,
    poll_status: Option<String>,
    expanded: BTreeSet<String>,
    /// Tree item key → what activating it opens, as last rendered.
    targets: BTreeMap<String, Target>,
    java: Indexes<Vec<JavaType>>,
    configs: Indexes<Vec<(String, u32, u32)>>,
}

impl Default for Runtime {
    fn default() -> Self {
        Self {
            panes: panes::State::default(),
            graph: graph::State::default(),
            polling: controls::Polling::default(),
            jmx: None,
            snapshot: None,
            lookup: None,
            others: BTreeMap::new(),
            scope: None,
            loaded: false,
            error: None,
            poll_status: None,
            expanded: BTreeSet::new(),
            targets: BTreeMap::new(),
            java: Indexes::new(&["**/*.java"], index_java),
            configs: Indexes::new(&CONFIG_GLOBS, index_config),
        }
    }
}

fn reveal_view(view: &str) -> Result<(), Error> {
    ui_kit::web::reveal_view(view, include_str!("ui_config.json"))
}
fn refresh_view(view: &str) {
    ui_kit::web::refresh_view(view, include_str!("ui_config.json"));
}
fn post_view_message(view: &str, message: &str) -> Result<(), Error> {
    ui_kit::web::post_view_message(view, message, include_str!("ui_config.json"))
}

// Payload-free invalidation only: each mounted page renders through its own
// frame/project-correlated bridge. Never send a snapshot over this broadcast.
fn invalidate_snapshot_views() {
    for view in [VIEW, graph::VIEW, "runtime-snapshot"] {
        crate::refresh_view(view);
    }
}

impl Runtime {
    fn load_scope(&mut self, scope: Scope) {
        if self.scope.as_ref()!=Some(&scope) {
            self.panes.clear();self.graph.clear();self.snapshot=None;self.lookup=None;self.loaded=false;self.error=None;self.targets.clear();self.expanded.clear();self.polling.clear();self.poll_status=None;self.scope=Some(scope.clone());
        }
        if !self.loaded {
            self.loaded=true;
            match persistence::read(&scope) {Ok(snapshot)=>{self.snapshot=snapshot;self.lookup=None;self.error=persistence::failure(&scope);},Err(error)=>self.error=Some(error.to_string())}
        }
    }
    /// What the editor providers read: the stored snapshot of the document's project, never network, polling or view state.
    fn document_snapshot(&mut self,path:&str)->Option<(&Snapshot,&Lookup)> {
        let scope=spring_text::index::project_of(path).and_then(|p|Scope::current(&p).ok())?;
        if self.loaded && self.scope.as_ref()==Some(&scope) {
            let snapshot=self.snapshot.as_ref()?;
            let lookup=self.lookup.get_or_insert_with(||Lookup::new(snapshot));
            return Some((snapshot,lookup));
        }
        if !self.others.contains_key(&scope) {
            if self.others.len()>=8 {self.others.clear();}
            let loaded=persistence::read(&scope).ok().flatten().map(|snapshot|{let lookup=Lookup::new(&snapshot);(snapshot,lookup)});
            self.others.insert(scope.clone(),loaded);
        }
        self.others.get(&scope)?.as_ref().map(|(snapshot,lookup)|(snapshot,lookup))
    }
    fn refresh(&mut self, project:&Project) -> Result<String, Error> {
        let scope=Scope::current(project)?;
        self.load_scope(scope.clone());
        // Suspend future authorization before blocking host work: a host-level
        // interruption/trap cannot leave an executable polling lease behind.
        let mut suspended_polling=std::mem::take(&mut self.polling);
        let result=(|| {
            let snapshot=fetch_snapshot_guarded(&scope.url,&scope.auth,|| {
                controls::verify(project,&scope)
            })?;
            let current=Scope::current(project)?;
            if current!=scope {return Err(Error::Stale);}
            let mut snapshot=snapshot;
            if let Some(previous)=&self.snapshot {
                snapshot.keep_unreachable(previous);
                let lost:Vec<_>=snapshot.missing.iter().filter(|m|m.split(' ').next().is_some_and(|e|previous.has(e))).cloned().collect();
                if !lost.is_empty() {return Err(Error::Failed(format!("Refresh lost previously available endpoints: {}. Previous snapshot retained; Clear first if endpoint removal was intentional.",lost.join(", "))));}
            }
            persistence::write(&scope,&snapshot)?;
            Ok(snapshot)
        })();
        if matches!(&result,Err(Error::Cancelled|Error::Stale)) {suspended_polling.clear();}
        self.polling=suspended_polling;
        match result {
            Ok(snapshot)=> {
                self.panes.clear();self.graph.clear();self.lookup=None;self.others.remove(&scope);
                persistence::success(&scope);self.error=None;self.poll_status=None;self.targets.clear();
                let mut summary=format!("Read {} beans and {} property sources from {}.",snapshot.beans.len(),snapshot.sources.len(),snapshot.url);
                if !snapshot.missing.is_empty() {summary.push_str(&format!(" Not available: {}.",snapshot.missing.join(", ")));}
                if !snapshot.kept.is_empty() {summary.push_str(&format!(" Kept from the previous read: {}.",snapshot.kept.join(", ")));}
                self.snapshot=Some(snapshot);self.loaded=true;invalidate_snapshot_views();Ok(summary)
            }
            Err(error)=> {
                let message=if error==Error::Cancelled {"Refresh cancelled; previous snapshot retained.".into()} else {error.to_string()};
                self.error=Some(presentation(&message,1600));if error!=Error::Cancelled {persistence::failed(&scope,&message);}
                invalidate_snapshot_views();Err(error)
            }
        }
    }
    fn confirm_clear(&mut self,project:&Project)->Result<bool,Error> {
        let scope=Scope::current(project)?;
        let body=format!("This deletes the saved snapshot of {} for project {} and stops auto-refresh.\nClick Refresh to read the application again.",scope.url,scope.root);
        if !host::confirm("Clear Spring Runtime snapshot?",&body)? {return Ok(false);}
        self.clear(project)?;
        Ok(true)
    }

    fn clear(&mut self,project:&Project)->Result<(),Error> {
        let scope=Scope::current(project)?;persistence::clear(&scope)?;self.panes.clear();self.graph.clear();self.polling.clear();self.others.remove(&scope);self.lookup=None;self.scope=Some(scope);self.snapshot=None;self.loaded=true;self.error=None;self.poll_status=None;self.targets.clear();invalidate_snapshot_views();Ok(())
    }

    fn java_markers(&mut self, doc: &Document) -> Vec<LineMarker> {
        let text = doc.text.as_str();
        let Some((snap, lookup)) = self.document_snapshot(&doc.path) else {
            let _ = host::publish_diagnostics(&doc.path, &[]);
            return Vec::new();
        };
        let file = java::parse(text);
        let mut markers = Vec::new();
        let mut problems = Vec::new();
        if snap.has("beans") {
            for b in java::bean_names(&file) {
                let class = file.types.iter().position(|t| t.at == b.at);
                if class.is_some_and(|i| file.types[i].kind == "@interface") {
                    continue;
                }
                let fqn = class.map(|i| file.qualified(i));
                let loaded = snap.beans.iter().find(|r| match &fqn {
                    Some(fqn) => r.ty == *fqn || r.name == b.name && simple(&r.ty) == b.ty,
                    None => r.name == b.name,
                });
                if let Some(r) = loaded {
                    let dependents = snap.beans.iter().filter(|d| d.deps.contains(&r.name)).count();
                    let mut tip = format!("Runtime bean `{}` ({})", r.name, r.scope);
                    if dependents > 0 {
                        tip.push_str(&format!(" · {dependents} dependents"));
                    }
                    markers.push(marker(text, b.at, tip));
                } else if let Some(fqn) = fqn
                    && snap.beans.iter().any(|r| package(&r.ty) == file.package)
                {
                    let short = fqn.strip_prefix(&format!("{}.", file.package)).unwrap_or(&fqn);
                    let why = snap.conditions.iter().find(|c| !c.matched && [fqn.as_str(), short, &b.name].contains(&c.key.as_str()));
                    let message = match why {
                        Some(c) => format!("`{}` is not loaded in the running application: {}.", b.ty, c.messages.join("; ")),
                        None => format!("`{}` is not loaded in the running application. A profile or a condition may exclude it.", b.ty),
                    };
                    let range = spring_text::range(text, b.at, b.at + b.len);
                    problems.push(Diagnostic::new(range, Severity::Info, message).code("not-loaded"));
                }
            }
        }
        for t in &file.types {
            let params = t.members.iter().flat_map(|m| &m.params).chain(&t.components);
            let annotations = t.members.iter().flat_map(|m| &m.annotations).chain(params.flat_map(|p| &p.annotations));
            for s in annotations.filter(|a| a.name == "Value").flat_map(|a| a.strings(&["value"])) {
                let lines: Vec<String> = placeholders(&s.value)
                    .into_iter()
                    .filter_map(|key| {
                        let (value, source) = lookup.get(snap, key)?;
                        Some(format!("`{key}` = {value} (from {})", short_source(source)))
                    })
                    .collect();
                if !lines.is_empty() {
                    markers.push(marker(text, s.at, lines.join("\n")));
                }
            }
            let Some(prefix) =
                t.annotation("ConfigurationProperties").and_then(|a| a.strings(&["prefix", "value"]).first().map(|s| s.value.clone()))
            else {
                continue;
            };
            let bound = snap.configprops.iter().find(|p| canonical(&p.prefix) == canonical(&prefix));
            if let Some(b) = bound {
                markers.push(marker(text, t.at, format!("`{prefix}` bound at runtime ({} properties)", b.props.len())));
            }
            let fields = t.fields().filter(|f| !f.modifiers.iter().any(|m| m == "static")).map(|f| (&f.name, f.at));
            for (name, at) in fields.chain(t.components.iter().map(|c| (&c.name, c.at))) {
                let tip = match lookup.get(snap, &format!("{prefix}.{name}")) {
                    Some((value, source)) => {
                        format!("Runtime: {value} from {}", short_source(source))
                    }
                    None => match bound.and_then(|b| b.props.iter().find(|(k, _)| canonical(k) == canonical(name))) {
                        Some((_, value)) => {
                            format!("Runtime: {value} (no property set, so the field's default)")
                        }
                        None => continue,
                    },
                };
                markers.push(marker(text, at, tip));
            }
        }
        let _ = host::publish_diagnostics(&doc.path, &problems);
        markers
    }

    fn config_markers(&mut self, doc: &Document) -> Vec<LineMarker> {
        let Some((snap, lookup)) = self.document_snapshot(&doc.path) else {
            return Vec::new();
        };
        let file = doc.path.rsplit(['/', '\\']).next().unwrap_or(&doc.path);
        config::keys(&doc.path, &doc.text)
            .into_iter()
            .filter(|k| !k.value.is_empty() && !k.value.starts_with(['|', '>']))
            .filter_map(|k| {
                let (value, source) = lookup.get(snap, &k.key)?;
                let tip = if is_file_source(source, file) && (value == k.value || value == MASK) {
                    format!("Active (from {})", short_source(source))
                } else {
                    format!("Runtime: {value} from {}", short_source(source))
                };
                Some(marker(&doc.text, k.at, tip))
            })
            .collect()
    }

    fn open(&mut self, target: &Target, project: &Project) -> Result<(), Error> {
        let found = match target {
            Target::Type(class, method) => {
                let nested = format!(".{class}");
                self.java.get(project, COMMAND_BUDGET).iter().find_map(|(path, types)| {
                    let t = types.iter().find(|t| t.fqn == *class || t.fqn.ends_with(&nested))?;
                    let m = method.as_ref().and_then(|m| t.methods.iter().find(|x| x.0 == *m));
                    Some(m.map_or((path.to_owned(), t.line, t.column), |m| (path.to_owned(), m.1, m.2)))
                })
            }
            Target::Key(key, file) => {
                let key = canonical(key);
                let index = self.configs.get(project, COMMAND_BUDGET);
                let hits: Vec<(&str, u32, u32)> =
                    index.iter().flat_map(|(path, keys)| keys.iter().filter(|k| k.0 == key).map(move |k| (path, k.1, k.2))).collect();
                let in_file = hits.iter().find(|h| file.as_ref().is_some_and(|f| h.0.ends_with(&format!("/{f}"))));
                in_file.or(hits.first()).map(|h| (h.0.to_owned(), h.1, h.2))
            }
            Target::Overload(..) | Target::Props(..) => None,
            Target::File(name) => {
                let suffix = format!("/{name}");
                let index = self.configs.get(project, COMMAND_BUDGET);
                index.iter().find(|(path, _)| path.ends_with(&suffix)).map(|(path, _)| (path.to_owned(), 0, 0))
            }
        };
        match found {
            Some((path, line, column)) => host::open_location(&path, line, column),
            None => Ok(()),
        }
    }
}

impl Providers for Runtime {
    /// Reads only the in-memory (or stored) snapshot: never the network.
    fn line_markers(&mut self, doc: &Document) -> Vec<LineMarker> {
        match doc.language.as_str() {
            "java" => self.java_markers(doc),
            _ if config::is_config_file(&doc.path) => self.config_markers(doc),
            _ => Vec::new(),
        }
    }
}

pub(crate) fn refresh_button() -> Button {
    Button::new("refresh", "Refresh").icon("refresh")
}

const SETTINGS_PAGE: &str = "plugins.aritti-spring-runtime";

fn open_settings() -> Result<(), Error> {
    match host::execute_command("file.settings", Some(SETTINGS_PAGE)) {
        Err(Error::Denied(_)) => Err("Open Settings › Plugins › Spring Runtime Insight. Allow the ide.commands permission to open it from this button.".into()),
        result => result.map(drop),
    }
}

impl Views for Runtime {
    fn render(&mut self, _view: &str, _viewport: Option<Viewport>, _ctx: &Invocation) -> Result<View, Error> {
        (|| -> Result<View, Error> {
            if _view == controls::VIEW {return self.render_controls(_ctx);}
            if _view == graph::VIEW {return self.render_graph(_ctx);}
            if _view == "runtime-snapshot" { return self.render_panes(_ctx); }
            let refresh = refresh_button();
            let Some(project)=_ctx.project() else {self.snapshot=None;self.scope=None;self.loaded=false;self.targets.clear();return Ok(View::message("Open a project to see its runtime snapshot.").button(refresh));};
            let scope=match Scope::current(&project).and_then(|scope| {controls::verify(&project,&scope)?;Ok(scope)}) {Ok(scope)=>scope,Err(error)=>{self.snapshot=None;self.scope=None;self.loaded=false;self.targets.clear();return Ok(View::message(&error.to_string()).button(refresh));}};
            self.load_scope(scope.clone());
            let Some(snap) = &self.snapshot else {
                let text = self.error.clone().unwrap_or_else(|| {
                    let url = host::setting_string("actuatorUrl").unwrap_or_else(|| DEFAULT_URL.into());
                    format!("No runtime data yet. Start the application with Spring Boot Actuator, then click Refresh to read {url}. Change the URL in Settings › Plugins › Spring Runtime Insight.")
                });
                return Ok(View::message(&text).button(refresh).button(Button::new("open-settings","Open Settings").icon("settings")).button(Button::new("clear","Clear Snapshot").icon("trash").enabled(self.error.is_some())));
            };
            self.targets.clear();
            let mut tree = Tree { items: Vec::new(), expanded: &self.expanded, targets: &mut self.targets, bytes:0, limited:false };
            tree.beans(snap);
            tree.health(snap);
            tree.configuration(snap);
            tree.conditions(snap);
            tree.mappings(snap);
            tree.configprops(snap);
            tree.scheduled(snap);
            tree.loggers(snap);
            tree.caches(snap);
            let mut status = format!("Read {} from {} · project {} · cached, not live state", age(snap.fetched_at), snap.url,scope.root);
            if !snap.kept.is_empty() {status.push_str(&format!(" · kept from before: {}", snap.kept.join(", ")));}
            if !snap.missing.is_empty() {
                status.push_str(&format!(" · not available: {}", snap.missing.join(", ")));
            }
            if let Some(hint) = auth_hint(&snap.missing) {
                status.push_str(&format!(" · {hint}"));
            }
            if snap.trimmed {
                status.push_str(" · legacy partial snapshot");
            }
            if let Some(message)=&self.poll_status {status.push_str(&format!(" · {message}"));}
            if tree.limited {status.push_str(" · partial view: row limit reached");}
            if let Some(e) = &self.error {
                status = format!("Refresh failed: {e} · {status}");
            }
            Ok(View::new(Body::Tree(tree.items)).button(refresh).button(Button::new("clear", "Clear Snapshot").icon("trash")).button(Button::new("collapse-all", "Collapse All").icon("collapse-all").enabled(!self.expanded.is_empty())).status(&status))
        })().map(ui_kit::actions::available)
    }

    fn handle(&mut self, view: &str, event: ViewEvent, ctx: &Invocation) -> Result<(), Error> {
        let before = ui_kit::web::active_view(view, include_str!("ui_config.json"))?;
        if ui_kit::web::handle(self, view, &event, ctx, include_str!("ui_config.json"))? {
            let after = ui_kit::web::active_view(view, include_str!("ui_config.json"))?;
            if before == controls::VIEW && after != controls::VIEW {
                // Leaving the owning tab retires the lease even if Start's private
                // reply was still in flight when its driver was disposed.
                self.polling.clear();
            }
            return Ok(());
        }
        if view == controls::VIEW {
            if let ViewEvent::Message(ref raw) = event { return self.controls_message(raw, ctx); }
        }

        match event {
            ViewEvent::Button(b) if b == "snapshot-related-beans" && view == "runtime-snapshot" => { self.choose_related_bean(ctx)?; },
            ViewEvent::Button(b) if b == "snapshot-method" && view == "runtime-snapshot" => { self.filter_pane_method(ctx)?; },
            ViewEvent::Button(b) if b == "snapshot-open-browser" && view == "runtime-snapshot" => { self.open_pane_browser(ctx)?; },
            ViewEvent::Button(b) if b == "snapshot-send-request" && view == "runtime-snapshot" => { self.send_pane_request(ctx)?; },
            ViewEvent::Button(b) if b == "snapshot-create-request" && view == "runtime-snapshot" => { self.create_pane_requests(ctx)?; },
            ViewEvent::Button(b) if b == "snapshot-copy-request" && view == "runtime-snapshot" => { self.copy_pane_requests(ctx)?; },
            ViewEvent::Button(b) if (b == "snapshot-search" || b == "snapshot-search-clear") && view == "runtime-snapshot" => { self.search_panes(b == "snapshot-search-clear", ctx)?; },
            ViewEvent::Button(b) if b == "runtime-overview" && view == "runtime-snapshot" => {
                let project = ctx.project().ok_or(Error::Stale)?;
                let scope = Scope::current(&project)?;
                controls::verify(&project, &scope)?;
                crate::reveal_view(VIEW)?;
            },
            ViewEvent::Button(b) if b == "collapse-all" && view == VIEW => {
                let project = ctx.project().ok_or(Error::Stale)?;
                let scope = Scope::current(&project)?;
                controls::verify(&project, &scope)?;
                if self.scope.as_ref() != Some(&scope) { return Err(Error::Stale); }
                self.expanded.clear();
                crate::refresh_view(VIEW);
            },
            ViewEvent::Button(b) if b == "runtime-controls"=>{ctx.project().ok_or(Error::Stale)?;crate::reveal_view(controls::VIEW)?;},
            ViewEvent::Button(b) if b == "poll-start"=>{self.request_poll_start(ctx)?;},
            ViewEvent::Button(b) if b == "poll-stop"=>{self.stop_polling(&ctx.project().ok_or(Error::Stale)?)?;},
            ViewEvent::Button(b) if b == "runtime-graph"=>{ctx.project().ok_or(Error::Stale)?;crate::reveal_view(graph::VIEW)?;},
            ViewEvent::Button(b) if b == "graph-back" && view==graph::VIEW=>self.graph_back(),
            ViewEvent::Button(b) if b == "graph-observations" && view==graph::VIEW=>{ctx.project().ok_or(Error::Stale)?;crate::reveal_view("runtime-snapshot")?;},
            ViewEvent::Activate(key)|ViewEvent::Message(key) if view==graph::VIEW=>self.graph_select(&key,ctx)?,
            ViewEvent::Button(b) if b == "snapshot-details" => {ctx.project().ok_or("Open a project first.")?;crate::reveal_view("runtime-snapshot")?;},
            // A failure is shown in the view.
            ViewEvent::Button(b) if b == "refresh" => {let project=ctx.project().ok_or("Open a project first.")?;drop(self.refresh(&project));},
            ViewEvent::Button(b) if b == "clear" => {let project=ctx.project().ok_or("Open a project first.")?;self.confirm_clear(&project)?;},
            ViewEvent::Button(b) if b == "open-settings" => open_settings()?,
            ViewEvent::Expand(key) => {
                self.expanded.insert(key);
                crate::refresh_view(view);
            }
            ViewEvent::Collapse(key) => {
                self.expanded.remove(&key);
                crate::refresh_view(view);
            }
            ViewEvent::Select(key) if view == "runtime-snapshot" => { self.select_pane(&key,ctx)?; },
            ViewEvent::Activate(key) if view == "runtime-snapshot" => { self.activate_pane(&key,ctx)?; },
            ViewEvent::Activate(key) => {
                if let Some(project)=ctx.project() {
                    self.load_scope(Scope::current(&project)?);
                    if let Some(target)=self.targets.get(&key).cloned() {
                    self.open(&target, &project)?;
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }
}

impl Plugin for Runtime {
    fn run_command(&mut self, id: &str, ctx: &Invocation, _arg: Option<&str>) -> Result<Option<String>, Error> {
        match id {
            "configure-snapshot-database"=>self.configure_snapshot_database(ctx),
            "runtime-controls"=>{ctx.project().ok_or(Error::Stale)?;crate::reveal_view(controls::VIEW)?;Ok(None)},
            "poll-start"=>self.request_poll_start(ctx).map(Some),
            "poll-stop"=>self.stop_polling(&ctx.project().ok_or(Error::Stale)?).map(Some),
            "runtime-graph"=>{ctx.project().ok_or(Error::Stale)?;crate::reveal_view(graph::VIEW)?;Ok(None)},
            "snapshot-details" => {ctx.project().ok_or("Open a project first.")?;crate::reveal_view("runtime-snapshot")?;Ok(None)},
            "read-jmx" => self.read_jmx(ctx).map(Some),
            "refresh" => self.refresh(&ctx.project().ok_or("Open a project first.")?).map(Some),
            "clear" => {
                let cleared = self.confirm_clear(&ctx.project().ok_or("Open a project first.")?)?;
                Ok(cleared.then(|| "Spring Runtime snapshot cleared.".into()))
            }
            _ => Err(Error::NotFound(format!("command {id}"))),
        }
    }

    fn handle_events(&mut self,events:&[Event],ctx:&Invocation) {
        let _ = (events, ctx); // No scheduled HTTP: pinned host schedules only inactive instances without a project.
    }

}

register_plugin!(Runtime, providers, views, details, inlay_hints);

pub(crate) const RUNTIME_MARKER: &str = "icons/marker-runtime.svg";

#[cfg(test)]
mod tests {
    #[test]
    fn marker_icons_are_declared_in_the_manifest() {
        let manifest = include_str!("../plugin.toml");
        for icon in [crate::RUNTIME_MARKER] {
            assert!(manifest.contains(&format!("icon = \"{icon}\"")));
            assert!(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(icon).is_file());
        }
    }

    use super::*;
    use openidea_plugin::HttpResponse;
    use openidea_plugin::mock::MockHost;

    #[test]
    fn marks_health_and_conditions_with_status_icons() {
        let host = MockHost::install();
        let health = [("", "UP"), ("db", "DOWN"), ("disk", "OUT_OF_SERVICE"), ("mail", "UNKNOWN")];
        let snap = Snapshot {
            health: health.iter().map(|(p, s)| (p.to_string(), s.to_string())).collect(),
            conditions: vec![Condition { key: "A".into(), matched: true, messages: vec![] }],
            sources: vec![Source { name: "Config resource 'class path resource [application.yml]'".into(), props: vec![] }],
            ..Snapshot::default()
        };
        let mut runtime = Runtime { snapshot: Some(snap), scope:Some(Scope::current(&host.invocation().project().unwrap()).unwrap()), loaded: true, ..Runtime::default() };
        runtime.expanded.extend(["health", "env", "auto"].map(String::from));
        let view = runtime.render(VIEW, None, &host.invocation()).unwrap();
        let Body::Tree(items) = view.body else { panic!("tree expected") };
        let icons: Vec<_> = items.iter().map(|i| (i.key.as_str(), i.icon.as_deref().unwrap_or_default())).collect();
        assert_eq!(
            icons,
            [
                ("beans", "spring-boot"),
                ("health", "success"),
                ("health/db", "error"),
                ("health/disk", "error"),
                ("health/mail", "warning"),
                ("env", "settings"),
                ("env/files", "file-properties"),
                ("env/0", ""),
                ("auto", "spring-config"),
                ("auto/+", "success"),
                ("auto/-", "error"),
                ("mappings", "web"),
                ("props", "property"),
                ("sched", "clock"),
                ("log", "settings"),
                ("cache", "database"),
            ]
        );
        assert_eq!(view.toolbar[1].icon.as_deref(), Some("trash"));
    }

    #[test]
    fn names_the_setting_as_shown_in_settings() {
        let error = check_url("http://example.com/actuator").unwrap_err();
        assert!(error.contains("Settings › Plugins › Spring Runtime Insight › Actuator base URL"), "{error}");
    }

    fn authorization(host: &MockHost, auth: &str) -> Option<String> {
        host.set_setting("actuatorAuth", &format!("\"{auth}\""));
        host.set_secret("actuator-token", "s3cret");
        host.on_fetch(|_| Ok(HttpResponse { status: 404, headers: vec![], body: vec![] }));
        let _ = fetch_snapshot();
        let requests = host.requests();
        assert!(!requests.is_empty());
        requests[0].headers.iter().find(|(k, _)| k == "Authorization").map(|(_, v)| v.clone())
    }

    #[test]
    fn bearer_header_uses_secret_substitution() {
        let host = MockHost::install().with_capabilities(&["secrets", "net:localhost"]);
        assert_eq!(authorization(&host, "bearer").as_deref(), Some("Bearer s3cret"));
        let host = MockHost::install().with_capabilities(&["secrets", "net:localhost"]);
        assert_eq!(authorization(&host, "basic").as_deref(), Some("Basic s3cret"));
        let host = MockHost::install().with_capabilities(&["secrets", "net:localhost"]);
        assert_eq!(authorization(&host, "none"), None);
    }

    #[test]
    fn loggers_and_caches_endpoints_parse() {
        let mut snap = Snapshot::default();
        snap.read(
            "loggers",
            &serde_json::json!({"loggers": {
                "ROOT": {"configuredLevel": "INFO", "effectiveLevel": "INFO"},
                "com.acme": {"configuredLevel": "DEBUG", "effectiveLevel": "DEBUG"},
                "com.acme.x": {"effectiveLevel": "DEBUG"}
            }}),
        );
        let names: Vec<_> = snap.loggers.iter().map(|l| (l.name.as_str(), l.configured.as_str(), l.effective.as_str())).collect();
        assert_eq!(names, [("ROOT", "INFO", "INFO"), ("com.acme", "DEBUG", "DEBUG")]);
        snap.read(
            "caches",
            &serde_json::json!({"cacheManagers": {"cm": {"caches": {
                "users": {"target": "java.util.concurrent.ConcurrentHashMap"}
            }}}}),
        );
        assert_eq!(
            snap.caches,
            [Cache { manager: "cm".into(), name: "users".into(), target: "java.util.concurrent.ConcurrentHashMap".into() }]
        );
        assert!(snap.trimmed().loggers.is_empty());
    }

    #[test]
    fn authentication_failure_explains_itself() {
        let host = MockHost::install().with_capabilities(&["net:localhost","project.read"]);
        host.on_fetch(|r| {
            let status = if r.url.ends_with("/beans") { 401 } else { 200 };
            Ok(HttpResponse { status, headers: vec![], body: if r.url.ends_with("/health") {b"{\"status\":\"UP\"}".to_vec()}else{b"{}".to_vec()} })
        });
        let snap = fetch_snapshot().unwrap();
        assert!(snap.missing.iter().any(|m| m == "beans (401)"), "{:?}", snap.missing);
        let mut runtime = Runtime { snapshot: Some(snap), scope:Some(Scope::current(&host.invocation().project().unwrap()).unwrap()), loaded: true, ..Runtime::default() };
        let view = runtime.render(VIEW, None, &host.invocation()).unwrap();
        let status = view.status.unwrap_or_default();
        assert!(status.contains("check actuatorAuth and the actuator-token secret"), "{status}");
    }

    #[test]
    fn unset_actuator_token_is_not_reported_as_unreachable() {
        let host = MockHost::install().with_capabilities(&["secrets", "net:localhost"]);
        host.set_setting("actuatorAuth", "\"bearer\"");
        host.on_fetch(|_| Ok(HttpResponse { status: 200, headers: vec![], body: b"{}".to_vec() }));

        let error = fetch_snapshot().unwrap_err().to_string();

        assert!(error.contains("Set the actuator-token secret"), "{error}");
        assert!(!error.contains("Could not reach"), "{error}");
    }

    #[test]
    fn refreshing_the_snapshot_denied_names_permission() {
        let _host = MockHost::install().with_capabilities(&["project.read"]);
        let error = fetch_snapshot().unwrap_err().to_string();
        assert!(error.contains("needs the net:localhost permission"), "{error}");
        assert!(error.contains("Settings › Plugins › Spring Runtime Insight"), "{error}");
        assert!(!error.contains("redirect peer"), "{error}");
    }

    #[test]
    fn denied_secrets_capability_explains_itself() {
        let host = MockHost::install().with_capabilities(&["net:localhost"]);
        host.set_setting("actuatorAuth", "\"bearer\"");
        let error = fetch_snapshot().unwrap_err().to_string();
        assert!(error.contains("actuator-token") && error.contains("none"), "{error}");
        assert!(!error.contains("http://localhost"), "{error}");
    }
}

#[cfg(test)]
mod persistence_flow_tests {
    use super::*;
    use openidea_plugin::{HttpResponse,mock::MockHost};
    const CAPS:&[&str]=&["project.read","storage","net:localhost"];
    fn response(request:&HttpRequest)->Result<HttpResponse,Error> {
        let body=if request.url.ends_with("/beans") {r#"{"contexts":{"application":{"beans":{"demo":{"type":"com.acme.Demo","scope":"singleton","dependencies":[]}}}}}"#}
            else if request.url.ends_with("/env") {r#"{"propertySources":[{"name":"Config resource 'class path resource [application.properties]'","properties":{"server.port":{"value":"8080"}}}]}"#}
            else if request.url.ends_with("/loggers") {r#"{"loggers":{"ROOT":{"configuredLevel":"INFO","effectiveLevel":"INFO"}}}"#}
            else if request.url.ends_with("/health") {r#"{"status":"UP"}"#} else {"{}"};
        Ok(HttpResponse {status:200,headers:vec![],body:body.as_bytes().to_vec()})
    }
    fn refreshed()->(MockHost,Runtime,Scope) {
        let host=MockHost::install().with_capabilities(CAPS);host.on_fetch(response);let mut runtime=Runtime::default();runtime.run_command("refresh",&host.invocation(),None).unwrap();let scope=Scope::current(&host.invocation().project().unwrap()).unwrap();(host,runtime,scope)
    }
    fn config_doc()->Document {Document {path:"/project/application.properties".into(),language:"properties".into(),version:1,text:"server.port=8080\n".into()}}
    fn invalidations(host: &MockHost) -> Vec<String> {
        host.view_messages().into_iter().filter_map(|(view, raw)| {
            let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
            if value.get("channel").and_then(|v| v.as_str()) != Some("ui-kit-refresh") { return None; }
            let target = value["targetView"].as_str().unwrap();
            assert_eq!(view, VIEW, "all logical invalidations use the single container");
            assert_eq!(value, serde_json::json!({"channel":"ui-kit-refresh","targetView":target}), "broadcast must contain only its logical destination, no cached model or owner data");
            Some(target.to_owned())
        }).collect()
    }
    #[test]
    fn external_refresh_failure_and_clear_notify_both_mounted_pages_without_render_loops() {
        let (host, mut runtime, _) = refreshed();
        assert_eq!(invalidations(&host), [VIEW, graph::VIEW, "runtime-snapshot"]);
        let requests = host.requests().len();
        for view in [VIEW, graph::VIEW, "runtime-snapshot"] { runtime.render(view, None, &host.invocation()).unwrap(); }
        assert_eq!(invalidations(&host).len(), 3, "render must not invalidate itself");
        assert_eq!(host.requests().len(), requests);
        host.on_fetch(|_| Err(Error::Failed("offline".into())));
        assert!(runtime.run_command("refresh", &host.invocation(), None).is_err());
        assert!(runtime.snapshot.is_some(), "last-good retained");
        assert_eq!(invalidations(&host), [VIEW, graph::VIEW, "runtime-snapshot", VIEW, graph::VIEW, "runtime-snapshot"]);
        host.answer_next_prompt(Some(&[]));
        runtime.run_command("clear", &host.invocation(), None).unwrap();
        assert_eq!(invalidations(&host).len(), 9);
        for view in [VIEW, graph::VIEW, "runtime-snapshot"] {
            assert!(matches!(runtime.render(view, None, &host.invocation()).unwrap().body, Body::Message(_)));
        }
        assert_eq!(invalidations(&host).len(), 9);
    }
    #[test]
    fn receiving_pages_rebind_to_current_owner_and_never_fetch_or_reuse_foreign_snapshot() {
        let (host, mut runtime, _) = refreshed();
        let requests = host.requests().len();
        host.on_fetch(|_| panic!("invalidated render must be cache only"));
        host.set_project_root(Some("/foreign"));
        for view in [VIEW, graph::VIEW, "runtime-snapshot"] {
            assert!(matches!(runtime.render(view, None, &host.invocation()).unwrap().body, Body::Message(_)));
        }
        assert!(runtime.snapshot.is_none());
        host.set_project_root(None);
        for view in [VIEW, graph::VIEW, "runtime-snapshot"] {
            assert!(matches!(runtime.render(view, None, &host.invocation()).unwrap().body, Body::Message(_)));
        }
        assert_eq!(host.requests().len(), requests);
        assert_eq!(invalidations(&host).len(), 3);
    }
    #[test]
    fn denied_or_invalid_scope_graph_render_returns_empty_state_instead_of_retained_model_error() {
        let (host, mut runtime, _) = refreshed();
        host.set_setting("actuatorAuth", "\"unsupported\"");
        assert!(matches!(runtime.render(graph::VIEW, None, &host.invocation()).unwrap().body, Body::Message(_)));
        assert!(matches!(runtime.render(VIEW, None, &host.invocation()).unwrap().body, Body::Message(_)));
        let host = host.with_capabilities(&["storage"]);
        for view in [VIEW, graph::VIEW, "runtime-snapshot"] {
            assert!(matches!(runtime.render(view, None, &host.invocation()).unwrap().body, Body::Message(_)));
        }
        assert_eq!(invalidations(&host).len(), 3);
    }
    #[test]
    fn clear_asks_first_and_keeps_the_snapshot_when_cancelled() {
        let (host, mut runtime, scope) = refreshed();
        assert_eq!(runtime.run_command("clear", &host.invocation(), None).unwrap(), None);
        assert!(host.storage().contains_key(&scope.key()));
        assert!(runtime.snapshot.is_some());
    }
    #[test]
    fn empty_graph_and_observations_offer_refresh() {
        let host = MockHost::install().with_capabilities(CAPS);
        let mut runtime = Runtime::default();
        for view in [graph::VIEW, "runtime-snapshot"] {
            let rendered = runtime.render(view, None, &host.invocation()).unwrap();
            assert!(matches!(rendered.body, Body::Message(ref text) if text.contains("click Refresh")));
            assert!(rendered.toolbar.iter().any(|b| b.id == "refresh"), "{view}");
        }
    }
    #[test]
    fn external_stop_control_notifies_changed_status_without_http() {
        let (host, mut runtime, _) = refreshed();
        let requests = host.requests().len();
        runtime.run_command("poll-stop", &host.invocation(), None).unwrap();
        assert_eq!(invalidations(&host).len(), 6);
        let view = runtime.render(VIEW, None, &host.invocation()).unwrap();
        assert!(view.status.unwrap().contains("Auto-refresh isn't running"));
        assert_eq!(host.requests().len(), requests);
    }
    #[test]
    fn native_collapse_and_observation_navigation_are_scoped_and_cache_only() {
        let (host, mut runtime, _) = refreshed();
        let count = host.requests().len();
        runtime.expanded.extend(["beans".into(), "env".into(), "health".into()]);
        let ctx = host.invocation();
        runtime.handle(VIEW, ViewEvent::Button("collapse-all".into()), &ctx).unwrap();
        assert!(runtime.expanded.is_empty());
        let view = runtime.render(VIEW, None, &ctx).unwrap();
        assert!(!view.toolbar.iter().any(|button| button.id == "collapse-all"));
        let Body::Tree(items) = view.body else { panic!("native resource tree") };
        assert!(!items.iter().any(|item| item.parent.is_some()));
        runtime.handle("runtime-snapshot", ViewEvent::Button("runtime-overview".into()), &ctx).unwrap();
        assert_eq!(host.revealed_views(), vec![VIEW]);
        runtime.expanded.insert("beans".into());
        host.set_project_root(Some("/another"));
        assert!(runtime.handle(VIEW, ViewEvent::Button("collapse-all".into()), &host.invocation()).is_err());
        assert!(runtime.expanded.contains("beans"));
        assert_eq!(host.requests().len(), count);
    }
    #[test]
    fn rendering_markers_expansion_and_reclaim_are_cache_only_without_network_grants() {
        let (host,_,scope)=refreshed();let requests=host.requests().len();let saved=host.storage();
        let host=host.with_capabilities(&["project.read","storage"]);host.on_fetch(|_|panic!("read-only requests cannot fetch"));let mut runtime=Runtime::default();
        let view=runtime.render(VIEW,None,&host.invocation()).unwrap();assert!(view.status.unwrap().contains("project /project"));assert_eq!(runtime.snapshot.as_ref().unwrap().loggers.len(),1);
        assert_eq!(runtime.line_markers(&config_doc()).len(),1);runtime.handle(VIEW,ViewEvent::Expand("env".into()),&host.invocation()).unwrap();runtime.render(VIEW,None,&host.invocation()).unwrap();
        assert_eq!(host.requests().len(),requests);assert_eq!(host.storage(),saved);assert_eq!(runtime.scope.as_ref(),Some(&scope));
    }
    #[test]
    fn transient_timeout_keeps_previous_beans() {
        let (host, mut runtime, _) = refreshed();
        assert_eq!(runtime.snapshot.as_ref().unwrap().loggers.len(), 1);
        host.on_fetch(|request| if request.url.ends_with("/loggers") || request.url.ends_with("/health") { Err(Error::Failed("timed out".into())) } else { response(request) });
        runtime.run_command("refresh", &host.invocation(), None).unwrap();
        let snapshot = runtime.snapshot.as_ref().unwrap();
        assert_eq!(snapshot.loggers.len(), 1);
        assert_eq!(snapshot.health.len(), 1);
        assert!(snapshot.has("loggers") && snapshot.has("health"));
        assert_eq!(snapshot.beans.len(), 1);
        host.on_fetch(|request| if request.url.ends_with("/loggers") { Ok(HttpResponse { status: 404, headers: vec![], body: vec![] }) } else { response(request) });
        assert!(runtime.run_command("refresh", &host.invocation(), None).is_err(), "a 404 still counts as lost");
    }
    #[test]
    fn line_markers_for_other_project_keep_polling_lease() {
        let (host, mut runtime, scope) = refreshed();
        host.add_project("/other");
        host.set_setting("autoRefresh", "true");
        runtime.start_polling(&host.invocation().project().unwrap()).unwrap();
        runtime.expanded.insert("env".into());
        assert_eq!(runtime.line_markers(&config_doc()).len(), 1);
        let other = Document { path: "/other/application.properties".into(), ..config_doc() };
        assert!(runtime.line_markers(&other).is_empty());
        let outside = Document { path: "/elsewhere/application.properties".into(), ..config_doc() };
        assert!(runtime.line_markers(&outside).is_empty());
        assert!(runtime.polling.active(&scope), "provider paths never touch the lease");
        assert!(runtime.expanded.contains("env"));
        assert_eq!(runtime.scope.as_ref(), Some(&scope));
        assert!(runtime.snapshot.is_some());
        assert_eq!(runtime.line_markers(&config_doc()).len(), 1);
    }
    #[test]
    fn transport_auth_partial_and_oversized_failures_keep_last_good_memory_and_storage() {
        let (host,mut runtime,scope)=refreshed();let saved=host.storage()[&scope.key()].clone();let snapshot=runtime.snapshot.clone().unwrap();
        host.on_fetch(|_|Err(Error::Failed("application offline".into())));assert!(runtime.run_command("refresh",&host.invocation(),None).is_err());assert_eq!(runtime.snapshot.as_ref(),Some(&snapshot));assert_eq!(host.storage()[&scope.key()],saved);
        host.on_fetch(|request|if request.url.ends_with("/env") {Ok(HttpResponse {status:503,headers:vec![],body:vec![]})} else {response(request)});
        assert!(runtime.run_command("refresh",&host.invocation(),None).unwrap_err().to_string().contains("env (503)"));assert_eq!(runtime.snapshot.as_ref(),Some(&snapshot));assert_eq!(host.storage()[&scope.key()],saved);
        host.on_fetch(|_|Ok(HttpResponse {status:200,headers:vec![],body:vec![b'x';MAX_ENDPOINT_BYTES+1]}));assert!(runtime.run_command("refresh",&host.invocation(),None).is_err());assert_eq!(host.storage()[&scope.key()],saved);
        let host=host.with_capabilities(&["project.read","storage"]);assert!(runtime.run_command("refresh",&host.invocation(),None).is_err());assert_eq!(runtime.snapshot.as_ref(),Some(&snapshot));assert_eq!(host.storage()[&scope.key()],saved);
        let mut fresh=Runtime::default();let view=fresh.render(VIEW,None,&host.invocation()).unwrap();assert!(view.status.unwrap().contains("Refresh failed"));assert_eq!(fresh.snapshot.as_ref(),Some(&snapshot));
    }
    #[test]
    fn url_auth_and_project_switches_never_show_another_scope_and_clear_is_local() {
        let (host,mut runtime,first)=refreshed();let requests=host.requests().len();
        for (key,value) in [("actuatorUrl","http://localhost:9090/actuator"),("actuatorAuth","bearer")] {
            host.set_setting(key,&serde_json::to_string(value).unwrap());let view=runtime.render(VIEW,None,&host.invocation()).unwrap();assert!(matches!(view.body,Body::Message(_)));assert!(runtime.line_markers(&config_doc()).is_empty());
        }
        host.set_setting("actuatorUrl",&serde_json::to_string(DEFAULT_URL).unwrap());host.set_setting("actuatorAuth","\"none\"");runtime.render(VIEW,None,&host.invocation()).unwrap();assert!(runtime.snapshot.is_some());
        host.set_project_root(Some("/another"));let view=runtime.render(VIEW,None,&host.invocation()).unwrap();assert!(matches!(view.body,Body::Message(_)));host.answer_next_prompt(Some(&[]));runtime.run_command("clear",&host.invocation(),None).unwrap();assert!(host.storage().contains_key(&first.key()));
        host.set_project_root(Some("/project"));runtime.render(VIEW,None,&host.invocation()).unwrap();assert!(runtime.snapshot.is_some());assert_eq!(host.requests().len(),requests);
        host.answer_next_prompt(Some(&[]));runtime.run_command("clear",&host.invocation(),None).unwrap();assert!(!host.storage().contains_key(&first.key()));assert!(runtime.snapshot.is_none());
    }
    #[test]
    fn opt_in_scheduled_refresh_requires_verified_project_and_legacy_global_data_is_not_adopted() {
        let host=MockHost::install().with_capabilities(CAPS);host.on_fetch(response);host::storage::set("snapshot",&serde_json::to_string(&Snapshot {url:DEFAULT_URL.into(),..Snapshot::default()}).unwrap()).unwrap();
        let mut runtime=Runtime::default();runtime.render(VIEW,None,&host.invocation()).unwrap();assert!(runtime.snapshot.is_none());assert!(host.requests().is_empty());
        runtime.handle_events(&[Event::Scheduled],&host.invocation());assert!(host.requests().is_empty());
        host.set_setting("autoRefresh","true");host.set_project_root(None);runtime.handle_events(&[Event::Scheduled],&host.invocation());assert!(host.requests().is_empty());
        host.set_project_root(Some("/project"));runtime.start_polling(&host.invocation().project().unwrap()).unwrap();runtime.polling.due_now();runtime.handle_events(&[Event::Scheduled],&host.invocation());assert!(host.requests().is_empty());assert!(runtime.snapshot.is_none());assert!(host.storage().contains_key("snapshot"),"unowned legacy evidence is untouched");
    }
    #[test]
    fn local_url_identity_rejects_ambiguous_authorities_and_output_is_bounded_without_trimming_cache() {
        for url in ["http://localhost:80@evil.test/actuator","http://localhost:0/actuator","http://localhost:/actuator","http://localhost/actuator?q=x","http://localhost\\evil/actuator","http://localhost/#fragment","http://example.com/actuator"] {assert!(check_url(url).is_err(),"{url}");}
        assert_eq!(check_url(" http://LOCALHOST:08080/actuator/ ").unwrap(),DEFAULT_URL);
        let (host,mut runtime,scope)=refreshed();let snap=runtime.snapshot.as_mut().unwrap();snap.beans=(0..6000).map(|i|Bean {context:"app".into(),name:format!("bean{i}"),ty:"com.acme.Demo".into(),..Bean::default()}).collect();
        persistence::write(&scope,snap).unwrap();runtime.expanded.extend(["beans".into(),"beans/app".into()]);let view=runtime.render(VIEW,None,&host.invocation()).unwrap();let Body::Tree(items)=view.body else {panic!()};assert!(items.len()<=5000);assert!(view.status.unwrap().contains("partial view"));assert!(runtime.targets.len()<=items.len());assert_eq!(persistence::read(&scope).unwrap().unwrap().beans.len(),6000);
        assert!(presentation(&"🦀".repeat(3000),4096).ends_with("(truncated)"));
    }
}

#[cfg(test)]
mod failure_recovery_tests {
    use super::*;
    use openidea_plugin::{HttpResponse,mock::MockHost};
    const CAPS:&[&str]=&["project.read","storage","net:localhost"];
    #[test]
    fn storage_write_failure_and_cancellation_do_not_adopt_new_memory_or_delete_saved_data() {
        let host=MockHost::install().with_capabilities(CAPS);host.on_fetch(|request|Ok(HttpResponse {status:200,headers:vec![],body:if request.url.ends_with("/beans"){b"{\"contexts\":{}}".to_vec()}else{b"{}".to_vec()}}));
        let mut runtime=Runtime::default();runtime.run_command("refresh",&host.invocation(),None).unwrap();let previous=runtime.snapshot.clone().unwrap();let storage=host.storage();
        let host=host.with_capabilities(&["project.read","net:localhost"]);assert!(runtime.run_command("refresh",&host.invocation(),None).is_err());assert_eq!(runtime.snapshot,Some(previous.clone()));assert_eq!(host.storage(),storage);
        let host=host.with_capabilities(CAPS);host.on_fetch(|_|Err(Error::Cancelled));assert!(matches!(runtime.run_command("refresh",&host.invocation(),None),Err(Error::Cancelled)));assert_eq!(runtime.snapshot,Some(previous));assert_eq!(host.storage(),storage);assert!(runtime.error.unwrap().contains("cancelled"));
    }
    #[test]
    fn corrupt_cache_is_not_replaced_silently_and_clear_remains_available_for_recovery() {
        let host=MockHost::install().with_capabilities(&["project.read","storage"]);let scope=Scope::current(&host.invocation().project().unwrap()).unwrap();host::storage::set(&scope.key(),"invalid json").unwrap();
        let mut runtime=Runtime::default();let view=runtime.render(VIEW,None,&host.invocation()).unwrap();assert!(matches!(view.body,Body::Message(ref text) if text.contains("unreadable")));assert!(view.toolbar.iter().any(|b|b.id=="clear"));
        host.answer_next_prompt(Some(&[]));runtime.handle(VIEW,ViewEvent::Button("clear".into()),&host.invocation()).unwrap();assert!(!host.storage().contains_key(&scope.key()));assert!(host.requests().is_empty());
    }
    #[test]
    fn empty_view_offers_no_clear_when_nothing_is_stored() {
        let host=MockHost::install().with_capabilities(&["project.read","storage"]);
        let view=Runtime::default().render(VIEW,None,&host.invocation()).unwrap();
        assert!(matches!(view.body,Body::Message(ref text) if text.contains("Settings › Plugins")));
        assert!(!view.toolbar.iter().any(|b|b.id=="clear"));
    }
    #[test]
    fn open_settings_runs_the_settings_action_on_this_plugin_page_and_explains_a_denial() {
        let host=MockHost::install().with_capabilities(&["project.read","storage"]);
        let view=Runtime::default().render(VIEW,None,&host.invocation()).unwrap();
        assert!(view.toolbar.iter().any(|b|b.id=="open-settings"));
        let opened=std::rc::Rc::new(std::cell::RefCell::new(None));
        let seen=opened.clone();
        host.on_command("file.settings",move|arg|{*seen.borrow_mut()=arg.map(str::to_owned);Ok(None)});
        let denied=Runtime::default().handle(VIEW,ViewEvent::Button("open-settings".into()),&host.invocation()).unwrap_err();
        assert!(denied.to_string().contains("Settings › Plugins › Spring Runtime Insight"));
        assert!(opened.borrow().is_none());
        let host=host.with_capabilities(&["project.read","storage","ide.commands"]);
        Runtime::default().handle(VIEW,ViewEvent::Button("open-settings".into()),&host.invocation()).unwrap();
        assert_eq!(opened.borrow().as_deref(),Some(SETTINGS_PAGE));
    }
}
