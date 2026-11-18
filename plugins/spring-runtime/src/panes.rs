//! Cached observations and explicitly reviewed request actions. Source navigation is a unique qualified-name candidate only.
use super::*;
use openidea_plugin::{Range,Position};
use openidea_plugin::details::{Details, Pane, Tab};
use openidea_plugin::inlay_hints::{InlayHint, InlayHints};
use spring_text::index::Index;
const VIEW: &str = "runtime-snapshot";
const MAX_ROWS: usize = 512;
#[derive(Clone)]
struct Record {
    title: String,
    endpoint: &'static str,
    body: String,
    target: Option<Target>,
    request: Option<String>,
    methods: Vec<String>,
    requests: Vec<(String,String)>,
    examples: crate::request_conditions::RequestExamples,
    relations: Vec<(String,Target)>,
}
#[derive(Default)]
pub(crate) struct State {
    epoch: u64,
    scope: Option<Scope>,
    at: u64,
    rows: BTreeMap<String, Record>,
    selected: Option<String>,
    query: Option<String>,
    method: Option<String>,
    response: Option<(String,String)>,
}
impl State {
    pub(crate) fn retire_rows(&mut self) { self.rows.clear(); }
    pub(crate) fn clear(&mut self) {
        self.scope = None;
        self.rows.clear();
        self.selected = None;
        self.query = None;
        self.method = None;
        self.response = None;
    }
}
fn qualified(name: &str) -> bool {
    name.len() <= 4096
        && name.contains('.')
        && name.split('.').all(|p| {
            !p.is_empty()
                && p.bytes().enumerate().all(|(i, b)| b.is_ascii_alphabetic() || b == b'_' || b == b'$' || i > 0 && b.is_ascii_digit())
        })
}
pub(crate) fn source_owned(path: &str, root: &str) -> bool {
    path.len() <= 4096
        && !path.chars().any(char::is_control)
        && !path.contains('\\')
        && !path.split('/').any(|part| matches!(part, "." | ".."))
        && path.strip_prefix(root.trim_end_matches('/')).is_some_and(|tail| tail.starts_with('/'))
        && host::project_of(path).and_then(|project| project.root().ok()).as_deref() == Some(root)
}
fn known(value: &str) -> &str {
    if value.is_empty() { "unknown / not reported" } else { value }
}
fn candidate(ty: &str, method: Option<String>) -> Option<Target> {
    qualified(ty).then(|| Target::Type(ty.into(), method))
}
fn row(title: &str, endpoint: &'static str, body: String, target: Option<Target>) -> Record {
    Record { title: presentation(title, 256), endpoint, body: presentation(&body, 8192), target, request: None, methods: Vec::new(), requests:Vec::new(), examples:Default::default(), relations:Vec::new() }
}
fn endpoint_presentation(endpoint: &str) -> (&str, &str) {
    match endpoint {
        "beans" => ("Beans", "spring-boot"),
        "health" => ("Health", "info"),
        "env" => ("Environment", "settings"),
        "conditions" => ("Auto-configuration", "spring-config"),
        "mappings" => ("Mappings", "web"),
        "configprops" => ("Configuration Properties", "property"),
        "scheduledtasks" => ("Scheduled Tasks", "clock"),
        "loggers" => ("Loggers", "settings"),
        "caches" => ("Caches", "database"),
        "jmx" => ("JMX (localhost)", "info"),
        _ => (endpoint, "info"),
    }
}
fn records(s: &Snapshot) -> Vec<Record> {
    let mut out = Vec::new();
    macro_rules! add {
        ($value:expr) => {
            if out.len() < MAX_ROWS {
                out.push($value);
            }
        };
    }
    // Round-robin endpoint sections are independently capped, so one huge endpoint
    // cannot hide every other observed endpoint.
    for b in s.beans.iter().take(64) {
        let mut record=row(
            &b.name,
            "beans",
            format!(
                "Bean: {}\nContext: {}\nReported type: {}\nScope: {}\nDependencies (names, not resolved owners): {}",
                known(&b.name),
                known(&b.context),
                known(&b.ty),
                known(&b.scope),
                if b.deps.is_empty() {
                    "none reported".into()
                } else {
                    b.deps.iter().take(64).map(|v| presentation(v, 256)).collect::<Vec<_>>().join(", ")
                }
            ),
            candidate(&b.ty, None)
        );
        record.relations=bean_relations(s,b);
        record.body.push_str(&format!("\nDependency array verified: {}\nRelated source candidates (same reported context only):\n{}",b.dependencies_verified,record.relations.iter().map(|(label,_)|label.as_str()).collect::<Vec<_>>().join("\n")));
        record.body=presentation(&record.body,8192);
        add!(record);
    }
    for (path, status) in s.health.iter().take(32) {
        add!(row(
            if path.is_empty() { "Application health" } else { path },
            "health",
            format!(
                "Component: {}\nReported status: {}\nDetails (bounded, secrets masked):\n{}",
                if path.is_empty() { "application" } else { path },
                known(status),
                health_detail_lines(s, path)
            ),
            None
        ));
    }
    for source in s.sources.iter().take(32) {
        add!(row(
            &source.name,
            "env",
            format!(
                "Property source: {}\nProperty count: {}\nProfiles: {}\nValues are omitted from this detail pane. Source-file identity and current effective values are unverified.",
                known(&source.name),
                source.props.len(),
                s.profiles.iter().take(32).map(|v| presentation(v, 128)).collect::<Vec<_>>().join(", ")
            ),
            None
        ));
    }
    for c in s.conditions.iter().take(64) {
        let (class, member) = c.key.split_once('#').map_or((c.key.as_str(), None), |(class, member)| (class, Some(member)));
        let class = class.replace('$', ".");
        let (matched, unmatched) = s.conditions.iter().filter(|o| o.key.split('#').next() == c.key.split('#').next()).fold((0, 0), |(m, n), o| if o.matched { (m + 1, n) } else { (m, n + 1) });
        add!(row(
            &c.key,
            "conditions",
            format!(
                "Auto-configuration class: {}\nBean method: {}\nReported match: {}\nThis class, all reported entries: {matched} matched, {unmatched} not matched\nMessages ({}):\n{}",
                known(&class),
                member.unwrap_or("none (class-level condition)"),
                if c.matched { "positive match" } else { "negative match" },
                if c.matched { "why it matched" } else { "why it did not match" },
                c.messages.iter().take(32).map(|m| presentation(m, 256)).collect::<Vec<_>>().join("\n")
            ),
            candidate(&class, member.map(str::to_owned))
        ));
    }
    for m in s.mappings.iter().take(64) {
        let mut record = row(
            &m.predicate,
            "mappings",
            format!(
                "Group: {}\nPredicate: {}\nHandler: {}\nDeclared method: {}\nDescriptor: {}\nSource navigation matches the exact overload by parameter simple type names when a descriptor is reported.",
                known(&m.group),
                known(&m.predicate),
                known(&m.handler),
                m.method.as_ref().map(|(c, m)| format!("{c}#{m}")).unwrap_or_else(|| "not reported".into()),
                m.descriptor.as_deref().map(|d| presentation(d, 512)).unwrap_or_else(|| "not reported".into())
            ),
            m.method.as_ref().and_then(|(c, name)| {
                let params = m.descriptor.as_deref().and_then(crate::snapshot::descriptor_params);
                match params {
                    Some(params) if qualified(c) => Some(Target::Overload(c.clone(), name.clone(), params)),
                    _ => candidate(c, Some(name.clone())),
                }
            })
        );
        record.examples=m.examples.clone();
        record.body=presentation(&format!("{}\n\nReported request conditions:\n{}",record.body,m.examples.review_text()),8192);
        if !m.requests.is_empty() && m.requests.len() <= 32 && m.requests.iter().all(|(method,path)| matches!(method.as_str(), "GET"|"HEAD"|"POST"|"PUT"|"PATCH"|"DELETE"|"OPTIONS"|"TRACE") && path.starts_with('/') && !path.starts_with("//") && path.len() <= 2048 && !path.chars().any(|c|c.is_control() || c.is_whitespace())) {
            let mut request = String::from("# Set the application URL; its port and context path may differ from Actuator.\n# Replace path variables and supply required headers, parameters and bodies before sending.\n@applicationUrl = http://localhost:8080\n\n");
            for (method, path) in &m.requests {
                let review=m.examples.review_text().lines().map(|line|format!("# {line}\n")).collect::<String>();
                request.push_str(&format!("### Cached mapping\n{review}{method} {{{{applicationUrl}}}}{path}{}\n{}\n\n",m.examples.query(),m.examples.header_text()));
            }
            record.requests = m.requests.clone();
            record.methods = m.requests.iter().map(|(method,_)| method.clone()).collect();
            record.request = Some(request);
        }
        add!(record);
    }
    for p in s.configprops.iter().take(64) {
        let values: Vec<String> = p.props.iter().take(64).map(|(k, v)| format!("{}: {}", presentation(k, 128), crate::snapshot::masked_value(k, v))).collect();
        add!(row(
            &p.prefix,
            "configprops",
            format!(
                "Prefix: {}\nReported type: {}\nBound properties: {} (first {} shown, secrets masked)\n{}",
                known(&p.prefix),
                known(&p.ty),
                p.props.len(),
                values.len(),
                values.join("\n")
            ),
            (qualified(&p.ty) && !p.prefix.is_empty() && p.prefix.len() <= 512).then(|| Target::Props(p.prefix.clone(), p.ty.clone()))
        ));
    }
    for t in s.scheduled.iter().take(64) {
        add!(row(
            &t.target,
            "scheduledtasks",
            format!(
                "Target: {}\nKind: {}\nSchedule: {}\nTarget ownership and current execution state: unknown.",
                known(&t.target),
                known(&t.kind),
                known(&t.detail)
            ),
            None
        ));
    }
    for l in s.loggers.iter().take(64) {
        add!(row(
            &l.name,
            "loggers",
            format!(
                "Logger: {}\nConfigured level: {}\nEffective level at fetch: {}\nCurrent level: unknown.",
                known(&l.name),
                known(&l.configured),
                known(&l.effective)
            ),
            None
        ));
    }
    for c in s.caches.iter().take(64) {
        add!(row(
            &c.name,
            "caches",
            format!(
                "Cache: {}\nManager: {}\nReported target: {}\nContents, statistics and current state: unknown.",
                known(&c.name),
                known(&c.manager),
                known(&c.target)
            ),
            None
        ));
    }
    out
}
fn bean_relations(snapshot:&Snapshot,bean:&Bean)->Vec<(String,Target)> {
    if snapshot.beans.len()>2048 || snapshot.beans.iter().map(|b|b.deps.len()).sum::<usize>()>8192 || snapshot.beans.iter().filter(|b|b.name==bean.name&&b.context==bean.context).count()!=1 {return Vec::new()}
    let mut relations=Vec::new();
    if bean.dependencies_verified {
        for name in bean.deps.iter().take(32) {
            let matches:Vec<_>=snapshot.beans.iter().filter(|other|other.context==bean.context&&other.name==*name).take(2).collect();
            if let [other]=matches.as_slice() {if let Some(target)=candidate(&other.ty,None) {relations.push((presentation(&format!("Injected: {} · {}",other.name,other.ty),512),target));}}
        }
    }
    for other in snapshot.beans.iter().filter(|other|other.context==bean.context&&other.dependencies_verified&&other.deps.contains(&bean.name)).take(32) {
        if snapshot.beans.iter().filter(|b|b.name==other.name&&b.context==other.context).count()==1 {
            if let Some(target)=candidate(&other.ty,None) {relations.push((presentation(&format!("Dependent: {} · {}",other.name,other.ty),512),target));}
        }
    }
    relations
}
fn concrete_local_url(value:&str)->Result<String,Error> {
    if value.len()>4096 || value.contains(['{','}','*','#','@','\\']) || value.chars().any(|c|c.is_control()||c.is_whitespace()) {return Err(Error::Invalid("Use a concrete local HTTP URL without credentials, fragments or path patterns.".into()));}
    let (base,query)=value.split_once('?').map_or((value,None),|(base,query)|(base,Some(query)));
    let mut url=check_url(base).map_err(Error::Invalid)?;
    if let Some(query)=query {url.push('?');url.push_str(query);}
    Ok(url)
}
fn request_headers(text:&str)->Result<Vec<(String,String)>,Error> {
    if text.len()>8<<10 {return Err(Error::Invalid("Headers exceed 8 KiB.".into()));}
    let mut headers=Vec::new();
    for line in text.lines().filter(|line|!line.is_empty()) {
        let (name,value)=line.split_once(':').ok_or_else(||Error::Invalid("Use one Name: value header per line.".into()))?;
        if value.contains("<value>") {return Err(Error::Invalid("Fill required header values before sending.".into()));}
        if headers.len()>=32 || name.is_empty() || !name.bytes().all(|b|b.is_ascii_alphanumeric()||b"!#$%&'*+-.^_`|~".contains(&b)) || value.chars().any(|c|c.is_control()&&c!='\t') || matches!(name.to_ascii_lowercase().as_str(),"host"|"content-length"|"transfer-encoding"|"connection"|"upgrade"|"proxy-authorization") {
            return Err(Error::Invalid("Unsupported header name/value, transport header or more than 32 headers.".into()));
        }
        headers.push((name.into(),value.trim().into()));
    }
    Ok(headers)
}

impl Runtime {
    pub(crate) fn render_panes(&mut self, ctx: &Invocation) -> Result<View, Error> {
        let custom = ui_kit::web::is_rendering();
        // Native render retires row keys but keeps the current scope filter.
        if !custom { self.panes.rows.clear(); self.panes.selected = None; }
        let Some(project) = ctx.project() else {
            self.panes.clear();
            return Ok(View::message("Open a project to inspect its cached snapshot."));
        };
        let scope = match Scope::current(&project).and_then(|scope| { controls::verify(&project, &scope)?; Ok(scope) }) {
            Ok(scope) => scope,
            Err(error) => { self.panes.clear(); return Ok(View::message(&error.to_string())); }
        };
        self.load_scope(scope.clone());
        let Some(snap) = &self.snapshot else {
            return Ok(View::message("No runtime data yet. Start the application, then click Refresh to read its Actuator.").button(crate::refresh_button()));
        };
        let at = snap.fetched_at;
        if self.panes.scope.as_ref() != Some(&scope) || self.panes.at != at || self.panes.rows.is_empty() {
            let query = (self.panes.scope.as_ref() == Some(&scope)).then(|| self.panes.query.clone()).flatten();
            let method = (self.panes.scope.as_ref() == Some(&scope)).then(|| self.panes.method.clone()).flatten();
            self.panes.clear();
            self.panes.query = query;
            self.panes.method = method;
            self.panes.epoch = self.panes.epoch.checked_add(1).ok_or(Error::Stale)?;
            self.panes.scope = Some(scope);
            self.panes.at = at;
            let jmx = self.jmx.iter().filter(|(root, _)| *root == self.panes.scope.as_ref().map_or("", |s| s.root.as_str())).map(|(_, text)| row("JMX read (localhost)", "jmx", text.clone(), None));
            for (i, record) in records(snap).into_iter().chain(jmx).enumerate() {
                self.panes.rows.insert(format!("snapshot:{}:{i}", self.panes.epoch), record);
            }
        }
        let query = self.panes.query.as_deref().unwrap_or_default().to_lowercase();
        let mut items: Vec<_> = self.panes.rows.iter().filter(|(_,record)| self.panes.method.as_ref().is_none_or(|method| record.methods.contains(method))).filter(|(_, record)| query.is_empty() || format!("{} {} {}", record.title, record.endpoint, record.body).to_lowercase().contains(&query)).map(|(key, record)| {
            let (section, icon) = endpoint_presentation(record.endpoint);
            TreeItem::new(key, &record.title).detail(section).icon(icon)
        }).collect();
        items.sort_by_key(|item| item.key.rsplit(':').next().and_then(|index|index.parse::<usize>().ok()).unwrap_or(usize::MAX));
        let visible = items.len();
        let body = if custom {
            let mut rows = Vec::new();
            for item in items {
                // Observation/provenance are lazy and bounded, as in the native
                // detail provider: only the authoritative selected row gets them.
                let details: Vec<_> = if self.panes.selected.as_deref() == Some(item.key.as_str()) {
                    self.detail(VIEW, &item.key, ctx)?.into_iter().filter_map(|tab| {
                        match tab.body { Pane::Text(value) => Some(serde_json::json!({"label":tab.label,"value":value})), _ => None }
                    }).collect()
                } else { Vec::new() };
                let navigable = self.panes.rows.get(&item.key).is_some_and(|record|record.target.is_some());
                rows.push(serde_json::json!({"key":item.key,"label":item.label,"detail":item.detail,"icon":item.icon,
                    "details":details,"activationLabel":"Open Source","activationIcon":"file","activationEnabled":navigable}));
            }
            Body::Other { kind: "tree".into(), data: serde_json::json!({"items":rows}).to_string() }
        } else { Body::Tree(items) };
        let view = View::new(body)
            .button(Button::new("snapshot-related-beans", "Related Beans…").icon("hierarchy").enabled(self.panes.selected.as_ref().and_then(|key|self.panes.rows.get(key)).is_some_and(|row|!row.relations.is_empty())))
            .button(Button::new("snapshot-method", &format!("HTTP Method: {}", self.panes.method.as_deref().unwrap_or("All"))).icon("filter"))
            .button(Button::new("snapshot-open-browser", "Open GET in Browser…").icon("external-link").enabled(self.panes.selected.as_ref().and_then(|key|self.panes.rows.get(key)).is_some_and(|row|row.methods.iter().any(|method|method=="GET"))))
            .button(Button::new("snapshot-send-request", "Send Local Request…").icon("run").enabled(self.panes.selected.as_ref().and_then(|key| self.panes.rows.get(key)).is_some_and(|row| !row.methods.is_empty())))
            .button(Button::new("snapshot-create-request", "Create HTTP Request File…").icon("add").enabled(self.panes.selected.as_ref().and_then(|key| self.panes.rows.get(key)).is_some_and(|row| row.request.is_some())))
            .button(Button::new("snapshot-copy-request", "Copy HTTP Requests").icon("copy").enabled(self.panes.selected.as_ref().and_then(|key| self.panes.rows.get(key)).is_some_and(|row| row.request.is_some())))
            .status(&format!("{visible} of {} observations · read {} · right-click a row for its actions",self.panes.rows.len(),age(at)));
        // The packaged window has its own search box above the rows.
        if custom { return Ok(view); }
        Ok(view
            .button(Button::new("snapshot-search", "Search Observations…").icon("search"))
            .button(Button::new("snapshot-search-clear", "Clear Search").icon("close").enabled(self.panes.query.is_some())))
    }
    pub(crate) fn choose_related_bean(&mut self,ctx:&Invocation)->Result<(),Error> {
        let key=self.panes.selected.clone().ok_or(Error::Stale)?;
        let relations=self.pane_record(&key,ctx)?.relations.clone();
        if relations.is_empty(){return Err(Error::Invalid("No verified same-context related source candidates.".into()))}
        let items:Vec<_>=relations.iter().enumerate().map(|(i,(label,_))|openidea_plugin::PickItem::new(&i.to_string(),label)).collect();
        let Some(index)=host::pick("Related runtime beans (same context)",&items)? else {return Ok(())};
        let index=index.parse::<usize>().map_err(|_|Error::Stale)?;
        let target=relations.get(index).ok_or(Error::Stale)?.clone();
        if self.pane_record(&key,ctx)?.relations.get(index)!=Some(&target){return Err(Error::Stale)}
        self.open_pane_candidate(Some(target.1),ctx)
    }
    pub(crate) fn filter_pane_method(&mut self, ctx: &Invocation) -> Result<(), Error> {
        let project = ctx.project().ok_or(Error::Stale)?;
        let scope = Scope::current(&project)?;
        controls::verify(&project, &scope)?;
        if self.panes.scope.as_ref() != Some(&scope) { return Err(Error::Stale); }
        let methods = ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS", "TRACE"];
        let mut items = vec![openidea_plugin::PickItem::new("all", "All observations")];
        items.extend(methods.iter().map(|method| openidea_plugin::PickItem::new(method, method)));
        let selected = host::pick("Filter cached HTTP mappings", &items)?;
        if Scope::current(&project)? != scope { return Err(Error::Stale); }
        if let Some(selected) = selected {
            if selected != "all" && !methods.contains(&selected.as_str()) { return Err(Error::Stale); }
            self.panes.method = (selected != "all").then_some(selected);
            self.panes.selected = None;
            crate::refresh_view(VIEW);
        }
        Ok(())
    }
    pub(crate) fn open_pane_browser(&self,ctx:&Invocation)->Result<(),Error> {
        let key=self.panes.selected.as_deref().ok_or(Error::Stale)?;
        let paths:Vec<_>=self.pane_record(key,ctx)?.requests.iter().filter(|(method,_)|method=="GET").map(|(_,path)|path.clone()).collect();
        if paths.is_empty(){return Err(Error::Invalid("Select an explicit GET mapping.".into()))}
        let path=if paths.len()==1 {paths[0].clone()} else {
            let items:Vec<_>=paths.iter().enumerate().map(|(i,path)|openidea_plugin::PickItem::new(&i.to_string(),path)).collect();
            let Some(index)=host::pick("Choose a GET mapping",&items)? else {return Ok(())};
            paths.get(index.parse::<usize>().map_err(|_|Error::Stale)?).ok_or(Error::Stale)?.clone()
        };
        let project=ctx.project().ok_or(Error::Stale)?;let scope=Scope::current(&project)?;
        let Some(url)=host::input("Local application URL for browser (replace path variables)",Some(&format!("http://localhost:8080{path}")))? else {return Ok(())};
        let url=concrete_local_url(&url)?;
        if !ui_kit::prompts::confirm_exact("Open mapping in browser?",&format!("GET {url}\n\nYour browser's normal cookies and authentication apply. The application address is supplied explicitly and may differ from Actuator."))? {return Ok(())}
        controls::verify(&project,&scope)?;
        if Scope::current(&project)?!=scope || !self.pane_record(key,ctx)?.requests.contains(&("GET".into(),path)) {return Err(Error::Stale)}
        host::open_url(&url)
    }
    pub(crate) fn send_pane_request(&mut self, ctx: &Invocation) -> Result<(), Error> {
        let key = self.panes.selected.clone().ok_or(Error::Stale)?;
        let requests: Vec<_> = self.pane_record(&key,ctx)?.requests.clone();
        if requests.is_empty() { return Err(Error::Invalid("Select a mapping with explicit HTTP methods.".into())); }
        let index = if requests.len()==1 {0} else {
            let recent=ctx.project().and_then(|p|Scope::current(&p).ok()).map(|s|crate::persistence::recent_requests(&s)).unwrap_or_default();
            let rank=|r:&(String,String)|recent.iter().position(|x|x==r).unwrap_or(usize::MAX);
            let mut order:Vec<usize>=(0..requests.len()).collect();order.sort_by_key(|i|rank(&requests[*i]));
            let items:Vec<_>=order.iter().map(|&i|{let (method,path)=&requests[i];let tag=if rank(&requests[i])==usize::MAX{""}else{"Recent · "};openidea_plugin::PickItem::new(&i.to_string(),&format!("{tag}{method} {path}"))}).collect();
            let Some(index)=host::pick("Choose a reported mapping",&items)? else {return Ok(())};
            index.parse::<usize>().map_err(|_|Error::Stale)?
        };
        let (method,path)=requests.get(index).ok_or(Error::Stale)?;
        let examples=self.pane_record(&key,ctx)?.examples.clone();
        let project=ctx.project().ok_or(Error::Stale)?;
        let scope=Scope::current(&project)?;
        controls::verify(&project,&scope)?;
        let Some(url)=host::input("Local application request URL (replace path variables)",Some(&format!("http://localhost:8080{path}{}",examples.query())))? else {return Ok(())};
        let url=concrete_local_url(&url)?;
        let Some(header_text)=host::input_multiline("Request headers (Name: value; reported defaults supplied)",Some(&examples.header_text()))? else {return Ok(())};
        let headers=request_headers(&header_text)?;
        let Some(body)=host::input_multiline("Request body (UTF-8; blank for none; at most 8 KiB)",Some(""))? else {return Ok(())};
        if body.len()>8<<10 {return Err(Error::Invalid("The request body exceeds 8 KiB; use an exported HTTP-client file for larger bodies.".into()));}
        if !ui_kit::prompts::confirm_exact("Send local application request?",&format!("{method} {url}\n{}\n\n{}\n\nThis exact request may change application state. Credentials entered in headers are sent only as entered; Actuator credentials are not reused. The application may differ from the cached snapshot. Response text appears only in this selection's HTTP Response detail.\n\nReported conditions (not inferred body schema):\n{}",header_text,ui_kit::prompts::readable_source(&body),examples.review_text()))? {return Ok(())}
        controls::verify(&project,&scope)?;
        if Scope::current(&project)?!=scope || !self.pane_record(&key,ctx)?.requests.contains(&(method.clone(),path.clone())) {return Err(Error::Stale)}
        let task=host::begin_task("Sending local runtime mapping request",true);
        if task.is_cancelled(){return Err(Error::Cancelled)}
        let request=HttpRequest{method:method.clone(),url:url.clone(),headers,body:(!body.is_empty()).then(||body.into_bytes()),timeout_ms:Some(controls::timeout())};
        let response=host::fetch(&request).map_err(|error|match error {Error::Denied(_)=>Error::Failed(ui_kit::denied::message("Sending this request","net:localhost","Spring Runtime Insight")),Error::Cancelled=>Error::Cancelled,_=>Error::Failed("The local request failed; no response was adopted.".into())})?;
        if task.is_cancelled(){return Err(Error::Cancelled)}
        controls::verify(&project,&scope)?;
        if Scope::current(&project)?!=scope {return Err(Error::Stale)}
        self.pane_record(&key,ctx)?;
        crate::persistence::remember_request(&scope,method,path);
        let bytes=response.body.len();
        let body=String::from_utf8_lossy(&response.body[..bytes.min(64<<10)]);
        self.panes.response=Some((key,format!("{method} {url}\nHTTP {}\nReceived {bytes} bytes at Unix {}. Body shown up to 64 KiB; response headers omitted.\n\n{}",response.status,now(),presentation(&body,64<<10))));
        crate::refresh_view(VIEW);
        Ok(())
    }
    pub(crate) fn create_pane_requests(&self, ctx: &Invocation) -> Result<(), Error> {
        let key = self.panes.selected.as_deref().ok_or(Error::Stale)?;
        let request = self.pane_record(key,ctx)?.request.as_ref().ok_or(Error::Stale)?.clone();
        let project = ctx.project().ok_or(Error::Stale)?;
        let scope = Scope::current(&project)?;
        controls::verify(&project,&scope)?;
        let Some(name) = host::input("HTTP request filename in project root",Some("runtime-requests.http"))? else { return Ok(()) };
        if name.len() > 128 || !name.ends_with(".http") || name.starts_with('.') || !name.bytes().all(|b|b.is_ascii_alphanumeric() || b"._-".contains(&b)) {
            return Err(Error::Invalid("Use a plain .http filename of at most 128 bytes.".into()));
        }
        let path = format!("{}/{}",scope.root.trim_end_matches('/'),name);
        if project.exists(&path)? { return Err(Error::Invalid("The request file already exists; choose another filename.".into())); }
        if !ui_kit::prompts::confirm_exact("Create HTTP request file?",&format!("{path}\n\n{request}"))? { return Ok(()) }
        controls::verify(&project,&scope)?;
        if Scope::current(&project)? != scope || self.pane_record(key,ctx)?.request.as_deref() != Some(&request)
            || host::project_of(&path).and_then(|p|p.root().ok()).as_deref() != Some(&scope.root) || project.exists(&path)? { return Err(Error::Stale); }
        project.write_file(&path,&request).map_err(|e|ui_kit::denied::map(e,"Create HTTP Request File","project.write","Spring Runtime Insight"))?;
        host::open_location(&path,0,0)
    }
    pub(crate) fn copy_pane_requests(&self, ctx: &Invocation) -> Result<(), Error> {
        let key = self.panes.selected.as_deref().ok_or_else(|| Error::Invalid("Select a mapping observation first.".into()))?;
        let request = self.pane_record(key, ctx)?.request.as_ref().ok_or_else(|| Error::Invalid("The selected observation has no explicit HTTP methods and paths.".into()))?;
        host::copy_to_clipboard(request).map_err(|e|ui_kit::denied::map(e,"Copy HTTP Requests","clipboard","Spring Runtime Insight"))
    }
    pub(crate) fn search_panes(&mut self, clear: bool, ctx: &Invocation) -> Result<(), Error> {
        let project = ctx.project().ok_or(Error::Stale)?;
        let scope = Scope::current(&project)?;
        controls::verify(&project, &scope)?;
        if self.panes.scope.as_ref() != Some(&scope) { return Err(Error::Stale); }
        let query = if clear { Some(String::new()) } else { host::input("Search cached observations", self.panes.query.as_deref())? };
        // A prompt can outlive the displayed project/settings.
        if Scope::current(&project)? != scope { return Err(Error::Stale); }
        if let Some(query) = query {
            if query.len() > 256 { return Err(Error::Invalid("Search is limited to 256 bytes.".into())); }
            self.panes.query = Some(query.trim().to_owned()).filter(|q| !q.is_empty());
            self.panes.selected = None;
            crate::refresh_view(VIEW);
        }
        Ok(())
    }
    pub(crate) fn select_pane(&mut self, key: &str, ctx: &Invocation) -> Result<(), Error> {
        self.pane_record(key, ctx)?;
        self.panes.selected = Some(key.to_owned());
        Ok(())
    }
    fn pane_record(&self, key: &str, ctx: &Invocation) -> Result<&Record, Error> {
        let scope = Scope::current(&ctx.project().ok_or(Error::Stale)?)?;
        if self.scope.as_ref() != Some(&scope)
            || self.panes.scope.as_ref() != Some(&scope)
            || self.snapshot.as_ref().is_none_or(|s| s.fetched_at != self.panes.at)
        {
            return Err(Error::Stale);
        }
        self.panes.rows.get(key).ok_or(Error::Stale)
    }
    pub(crate) fn activate_pane(&mut self, key: &str, ctx: &Invocation) -> Result<(), Error> {
        let target = self.pane_record(key, ctx)?.target.clone();
        self.open_pane_candidate(target,ctx)
    }
    fn open_pane_candidate(&mut self,target:Option<Target>,ctx:&Invocation)->Result<(),Error> {
        let Some(target) = target else {
            return Ok(());
        };
        let project = ctx.project().ok_or(Error::Stale)?;
        let scope = Scope::current(&project)?;
        let (path, line, column) = match self.resolve(&project, &scope, &target, true) {
            Resolved::Found(path, line, column) => (path, line, column),
            Resolved::Warming => return Err(Error::Failed(WARMING.into())),
            Resolved::NotIndexed => return Err(Error::Failed(NOT_INDEXED.into())),
            Resolved::Library => return Err(Error::Failed("No project source with this exact name (library class); no file opened.".into())),
            Resolved::Refused(why) => return Err(Error::Failed(why)),
        };
        if Scope::current(&project)? != scope || !source_owned(&path, &scope.root) { return Err(Error::Stale); }
        host::open_location(&path, line, column)
    }
    /// `warm` lets the call read project files for a while; otherwise only the index as it is counts.
    fn resolve(&mut self, project: &Project, scope: &Scope, target: &Target, warm: bool) -> Resolved {
        let index: &Index<Vec<JavaType>> = if warm {
            self.java.get(project, COMMAND_BUDGET)
        } else {
            match self.java.cached(&scope.root) {
                Some(index) => index,
                None => return Resolved::Warming,
            }
        };
        if !index.fresh() {
            return Resolved::Warming;
        }
        let owned = || index.iter().filter(|(path, _)| source_owned(path, &scope.root)).flat_map(|(path, types)| types.iter().map(move |t| (path, t)));
        let (Target::Type(subject, _) | Target::Overload(subject, ..) | Target::Props(_, subject)) = target else {
            return Resolved::Refused("Not a source candidate.".into());
        };
        if owned().any(|(_, t)| t.could_hold(subject)) {
            return Resolved::Refused("A large source file in this project could declare this class and is not indexed; no file opened.".into());
        }
        let (class, method) = match target {
            Target::Type(class, method) => (class, method.as_ref().map(|m| (m, None))),
            Target::Overload(class, name, params) => (class, Some((name, Some(params)))),
            Target::Props(prefix, ty) => {
                let by_type: Vec<_> = owned().filter(|(_, t)| t.fqn == *ty).take(2).collect();
                if let [(path, t)] = by_type.as_slice() {
                    return Resolved::Found(path.to_string(), t.line, t.column);
                }
                if by_type.len() > 1 {
                    return Resolved::Refused("The reported type has several project declarations; no file opened.".into());
                }
                let by_prefix: Vec<_> = owned().filter(|(_, t)| t.config_prefix.as_deref() == Some(prefix.as_str())).take(2).collect();
                return match by_prefix.as_slice() {
                    [] => Resolved::Library,
                    [(path, t)] => Resolved::Found(path.to_string(), t.line, t.column),
                    _ => Resolved::Refused("Several project classes declare this exact @ConfigurationProperties prefix; no file opened.".into()),
                };
            }
            _ => return Resolved::Refused("Not a source candidate.".into()),
        };
        let hits: Vec<_> = owned().filter(|(_, t)| t.fqn == *class).take(2).collect();
        let [(path, ty)] = hits.as_slice() else {
            let in_project_package = || !package(class).is_empty() && owned().any(|(_, t)| package(&t.fqn) == package(class));
            return if hits.is_empty() { if in_project_package() { Resolved::NotIndexed } else { Resolved::Library } } else { Resolved::Refused("No unique exact qualified source candidate; no file opened.".into()) };
        };
        let Some((name, params)) = method else {
            return Resolved::Found(path.to_string(), ty.line, ty.column);
        };
        let matching: Vec<_> = (0..ty.methods.len())
            .filter(|&i| ty.methods[i].0 == *name && params.is_none_or(|p| ty.params.get(i) == Some(p)))
            .take(2)
            .collect();
        match matching.as_slice() {
            [i] => Resolved::Found(path.to_string(), ty.methods[*i].1, ty.methods[*i].2),
            [] => Resolved::Refused("No method with this name and parameter types in the project class; no source candidate opened.".into()),
            _ => Resolved::Refused("Method missing or overloaded; no source candidate opened.".into()),
        }
    }
}
const NOT_INDEXED: &str = "This class is in a project package but its source is not indexed yet; try again after the source index refreshes.";
const WARMING: &str = "Source index warming; try again in a moment.";
enum Resolved {
    Warming,
    NotIndexed,
    Found(String, u32, u32),
    Library,
    Refused(String),
}
impl Details for Runtime {
    fn detail(&mut self, view: &str, key: &str, ctx: &Invocation) -> Result<Vec<Tab>, Error> {
        if view != VIEW {
            return Ok(Vec::new());
        }
        let Ok(record) = self.pane_record(key, ctx) else {
            return Ok(vec![Tab::new(
                "expired",
                "Snapshot changed",
                Pane::Message("Selection expired. Refresh/select a current cached row.".into()),
            )]);
        };
        let scope = self.panes.scope.as_ref().ok_or(Error::Stale)?;
        let provenance = format!(
            "Project: {}\nActuator URL: {}\nAuthentication mode: {} (credential not shown)\nEndpoint: {}\nFetched Unix: {} ({})\n\nRead-only cached Actuator observation. Not breakpoint evaluation or current application state. Current source/runtime build identity: unknown.\nSource navigation: {}\nLast refresh error: {}\nMissing endpoints: {}",
            scope.root,
            scope.url,
            scope.auth,
            record.endpoint,
            self.panes.at,
            age(self.panes.at),
            if record.target.is_some() {
                "exact qualified-name candidate only; requires unique class and non-overloaded method"
            } else {
                "no established source owner"
            },
            self.error.as_deref().unwrap_or("none recorded"),
            self.snapshot.as_ref().map(|s| s.missing.join(", ")).unwrap_or_default()
        );
        let (body, endpoint, target) = (record.body.clone(), record.endpoint, record.target.clone());
        let scope = scope.clone();
        let source = match (target, matches!(endpoint, "conditions" | "configprops" | "mappings"), ctx.project()) {
            (Some(target), true, Some(project)) => Some(match self.resolve(&project, &scope, &target, false) {
                Resolved::Warming => "Source: index warming; open the item once to scan project sources.".to_owned(),
                Resolved::NotIndexed => "Source: in a project package but not indexed yet.".to_owned(),
                Resolved::Found(path, line, _) => format!("Project source: {}:{}", presentation(&path, 1024), line + 1),
                Resolved::Library => "library: no project source declares this exact name.".to_owned(),
                Resolved::Refused(why) => format!("Not navigable: {why}"),
            }),
            _ => None,
        };
        let mut tabs = vec![
            Tab::new("observation", "Observation", Pane::Text(body)),
            Tab::new("provenance", "Provenance", Pane::Text(presentation(&provenance, 16384))),
        ];
        if let Some(source) = source {
            tabs.insert(1, Tab::new("source", "Source", Pane::Text(source)));
        }
        if let Some((response_key,response))=&self.panes.response {
            if response_key==key {tabs.push(Tab::new("http-response","HTTP Response",Pane::Text(response.clone())));}
        }
        Ok(tabs)
    }
}
fn snapshot_inlay_sites(doc:&Document)->Vec<(String,Position)>{
    if matches!(doc.language.as_str(),"properties"|"yaml") {
        return config::keys(&doc.path,&doc.text).into_iter().take(200).map(|key|(key.key,position(&doc.text,key.end))).collect()
    }
    if !matches!(doc.language.as_str(),"java"|"kotlin"){return Vec::new()}
    let file=spring_text::parse_source(&doc.path,&doc.text);
    if !file.from_spring("Value"){return Vec::new()}
    let tokens=java::tokens(&doc.text);let mut sites=Vec::new();
    for ty in &file.types {
        let params=ty.members.iter().flat_map(|m|&m.params).chain(&ty.components);
        let annotations=ty.members.iter().flat_map(|m|&m.annotations).chain(params.flat_map(|p|&p.annotations));
        for annotation in annotations.filter(|a|a.name=="Value") {
            let Some(arg)=annotation.arg("value") else{continue};
            if arg.strings.len()!=1 {continue}
            let value=&arg.strings[0];
            let Some(token)=tokens.iter().find(|token|token.string().is_some_and(|s|s.at==value.at&&s.end==value.end)) else{continue};
            let delimiter=if token.s.starts_with("\"\"\""){ "\"\"\"" }else{"\""};
            if token.s.len()<2*delimiter.len()||!token.s.ends_with(delimiter){continue}
            // One physical literal only; concatenations do not supply a stable
            // annotation value contract. Position uses closing source token.
            if arg.text.trim()!=token.s {continue}
            if value.value.match_indices("${").any(|(from,_)|value.value[from+2..].find('}').is_some_and(|end|value.value[from+2..from+2+end].contains("${"))){continue}
            for (from,_) in value.value.match_indices("${") {
                let rest=&value.value[from+2..];let Some(end)=rest.find('}') else{continue};
                if rest[..end].contains("${"){continue}
                let key=rest[..end].split(':').next().unwrap_or("").trim();
                if !key.is_empty(){sites.push((key.to_owned(),position(&doc.text,token.end())));}
                if sites.len()>=200{return sites}
            }
        }
    }
    sites
}

impl InlayHints for Runtime {
    fn inlay_hints(&mut self, doc: &Document, range: Range) -> Vec<InlayHint> {
        if doc.text.len() > 128 << 10
            || !matches!(doc.language.as_str(), "properties" | "yaml" | "java" | "kotlin")
            || (matches!(doc.language.as_str(),"properties"|"yaml")&&!config::is_config_file(&doc.path))
            || !host::setting_bool("snapshotInlays").unwrap_or(true)
        {
            return Vec::new();
        }
        let Some((s, _)) = self.document_snapshot(&doc.path) else {
            return Vec::new();
        };
        if !s.has("env") {
            return Vec::new();
        }
        if s.sources.len() > 256 || s.sources.iter().map(|source| source.props.len()).sum::<usize>() > 8192 {
            return Vec::new();
        }
        let mut lookup: BTreeMap<&str, Option<(&str, &str)>> = BTreeMap::new();
        for source in &s.sources {
            let mut local: BTreeMap<&str, Option<&str>> = BTreeMap::new();
            for (key, value) in &source.props {
                if key.len() > 4096 {
                    continue;
                }
                local.entry(key).and_modify(|value| *value = None).or_insert(Some(value));
            }
            for (key, value) in local {
                lookup.entry(key).or_insert_with(|| value.map(|value| (source.name.as_str(), value)));
            }
        }
        snapshot_inlay_sites(doc).into_iter().filter_map(|(key,p)|{
            if key.len()>4096||p<range.start||p>range.end {return None;}
            // Exact key only: no relaxed aliases, source defaults, environment
            // transformations or inferred active profile/runtime identity.
            let (source,value)=lookup.get(key.as_str()).copied().flatten()?;
            Some(InlayHint::new(p,if value==MASK{"snapshot: redacted"}else{"snapshot: observed"}).padding_left().tooltip(presentation(&format!("Cached /env observation for exact key {}\nProperty source: {}\nFetched Unix {} ({})\nActuator: {}\nValues and source defaults omitted. Active profile, source-file/runtime identity and current effective value are unverified.",key,presentation(source,1024),s.fetched_at,age(s.fetched_at),s.url),4096)))
        }).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{mock::MockHost, text::offset};
    fn setup() -> (MockHost, Runtime) {
        let host = MockHost::install();
        let scope = Scope::current(&host.invocation().project().unwrap()).unwrap();
        let snapshot = Snapshot {
            url: scope.url.clone(),
            fetched_at: 123,
            beans: vec![Bean {
                name: "service".into(),
                context: "app".into(),
                ty: "com.acme.Service".into(),
                scope: "singleton".into(),
                deps: vec!["other".into()],
                dependencies_verified:true,
            }],
            sources: vec![Source {
                name: "test source".into(),
                props: vec![("demo.key".into(), "private-value".into()), ("demo.secret".into(), MASK.into())],
            }],
            ..Snapshot::default()
        };
        let runtime = Runtime { snapshot: Some(snapshot), scope: Some(scope), loaded: true, ..Runtime::default() };
        (host, runtime)
    }
    fn doc(text: &str, language: &str) -> Document {
        Document {
            path: format!("/project/src/main/resources/application.{}", if language == "yaml" { "yml" } else { "properties" }),
            language: language.into(),
            version: 5,
            text: text.into(),
        }
    }
    fn all(doc: &Document) -> Range {
        spring_text::range(&doc.text, 0, doc.text.len())
    }
    #[test]
    fn structured_conditions_are_reviewed_and_used_in_request_examples_without_predicate_guessing() {
        let mut snapshot=Snapshot::default();snapshot.read("mappings",&serde_json::json!({"contexts":{"app":{"mappings":{"dispatcherServlets":{"dispatcher":[{"predicate":"ignored presentation","details":{"requestMappingConditions":{"methods":["POST"],"patterns":["/users"],"headers":[{"name":"X-Tenant","value":"demo","negated":false}],"params":[{"name":"kind","value":"a&b","negated":false},{"name":"debug","negated":true}],"consumes":[{"mediaType":"application/json","negated":false}],"produces":[]}}}]}}}}}));
        let rows=records(&snapshot);let record=rows.iter().find(|r|r.endpoint=="mappings").unwrap();
        let request=record.request.as_deref().unwrap();assert!(request.contains("POST {{applicationUrl}}/users?kind=a%26b"));assert!(request.contains("X-Tenant: demo"));assert!(request.contains("Content-Type: application/json"));assert!(request.contains("debug must be absent"));assert!(record.body.contains("kind equals a&b"));assert!(request_headers("X-Required: <value>").is_err());
    }
    #[test]
    fn related_beans_chooser_is_verified_same_context_and_navigates_exact_source() {
        let (host,mut runtime)=setup();
        let mut beans=vec![
            Bean{name:"other".into(),context:"app".into(),ty:"com.acme.Other".into(),dependencies_verified:true,..Default::default()},
            Bean{name:"consumer".into(),context:"app".into(),ty:"com.acme.Consumer".into(),deps:vec!["service".into()],dependencies_verified:true,..Default::default()},
            Bean{name:"other".into(),context:"foreign".into(),ty:"foreign.Other".into(),dependencies_verified:true,..Default::default()},
            Bean{name:"unknown".into(),context:"app".into(),ty:"com.acme.Unknown".into(),deps:vec!["service".into()],dependencies_verified:false,..Default::default()}
        ];runtime.snapshot.as_mut().unwrap().beans.append(&mut beans);host.add_file("src/Other.java","package com.acme; class Other {}");
        let ctx=host.invocation();runtime.render_panes(&ctx).unwrap();let (key,record)=runtime.panes.rows.iter().find(|(_,r)|r.endpoint=="beans"&&r.title=="service").unwrap();let key=key.clone();
        assert_eq!(record.relations.len(),2);assert!(record.relations[0].0.starts_with("Injected: other"));assert!(record.relations[1].0.starts_with("Dependent: consumer"));assert!(!record.body.contains("foreign.Other"));assert!(!record.body.contains("Unknown"));
        runtime.select_pane(&key,&ctx).unwrap();host.answer_next_prompt(Some(&["0"]));runtime.choose_related_bean(&ctx).unwrap();assert_eq!(host.opened_locations().len(),1);assert_eq!(host.opened_locations()[0].0,"/project/src/Other.java");assert!(host.requests().is_empty());
        runtime.snapshot.as_mut().unwrap().beans[0].dependencies_verified=false;let s=runtime.snapshot.as_ref().unwrap();assert!(bean_relations(s,&s.beans[0]).iter().all(|(label,_)|!label.starts_with("Injected:")));
    }
    #[test]
    fn explicit_post_sender_preserves_reviewed_headers_and_body_without_actuator_credentials() {
        let (host,mut runtime)=setup();let host=host.with_capabilities(&["project.read","storage","net:localhost"]);
        runtime.snapshot.as_mut().unwrap().mappings.push(Mapping{predicate:"POST /users".into(),requests:vec![("POST".into(),"/users".into())],..Default::default()});
        let ctx=host.invocation();runtime.render_panes(&ctx).unwrap();let key=runtime.panes.rows.iter().find(|(_,r)|r.endpoint=="mappings").unwrap().0.clone();runtime.select_pane(&key,&ctx).unwrap();
        host.on_fetch(|request| {assert_eq!(request.method,"POST");assert_eq!(request.headers,vec![("Content-Type".into(),"application/json".into()),("Authorization".into(),"Bearer entered-token".into())]);assert_eq!(request.body.as_deref(),Some(&b"{\"name\":\"demo\"}"[..]));Ok(openidea_plugin::HttpResponse{status:201,headers:vec![],body:b"created".to_vec()})});
        host.answer_next_prompt(Some(&["http://localhost:8080/users"]));host.answer_next_prompt(Some(&["Content-Type: application/json\nAuthorization: Bearer entered-token"]));host.answer_next_prompt(Some(&["{\"name\":\"demo\"}"]));host.answer_next_prompt(Some(&[]));
        runtime.send_pane_request(&ctx).unwrap();assert_eq!(host.requests().len(),1);
        let tabs=runtime.detail(VIEW,&key,&ctx).unwrap();assert!(!format!("{tabs:?}").contains("entered-token"));
        for headers in ["Host: other", "Content-Length: 50", "Bad Header: value", "X-Key: value\rbroken", "missing colon"] {assert!(request_headers(headers).is_err(),"{headers}");}
    }
    #[test]
    fn recent_requests_are_bounded_deduplicated_credential_free_and_listed_first() {
        let (host,mut runtime)=setup();let host=host.with_capabilities(&["project.read","storage","net:localhost"]);
        runtime.snapshot.as_mut().unwrap().mappings.push(Mapping{predicate:"/users".into(),requests:vec![("GET".into(),"/users".into()),("POST".into(),"/users".into())],..Default::default()});
        let ctx=host.invocation();runtime.render_panes(&ctx).unwrap();let key=runtime.panes.rows.iter().find(|(_,r)|r.endpoint=="mappings").unwrap().0.clone();runtime.select_pane(&key,&ctx).unwrap();
        host.on_fetch(|_|Ok(openidea_plugin::HttpResponse{status:200,headers:vec![],body:b"secret-body".to_vec()}));
        host.answer_next_prompt(Some(&["1"]));host.answer_next_prompt(Some(&["http://localhost:8080/users?token=abc"]));host.answer_next_prompt(Some(&["Authorization: Bearer entered-token"]));host.answer_next_prompt(Some(&["secret-request-body"]));host.answer_next_prompt(Some(&[]));
        runtime.send_pane_request(&ctx).unwrap();
        let scope=Scope::current(&ctx.project().unwrap()).unwrap();
        assert_eq!(crate::persistence::recent_requests(&scope),vec![("POST".to_string(),"/users".to_string())]);
        let stored=host.storage().iter().filter(|(k,_)|k.ends_with(":recent-requests")).map(|(_,v)|v.clone()).collect::<String>();
        for leaked in ["entered-token","secret-body","secret-request-body","abc","8080"] {assert!(!stored.contains(leaked),"{leaked}");}
        for i in 0..15 {crate::persistence::remember_request(&scope,"GET",&format!("/p{i}"));}
        crate::persistence::remember_request(&scope,"GET","/p14");crate::persistence::remember_request(&scope,"GET",&"x".repeat(513));
        let recent=crate::persistence::recent_requests(&scope);assert_eq!(recent.len(),10);assert_eq!(recent[0].1,"/p14");assert_eq!(recent.iter().filter(|r|r.1=="/p14").count(),1);
    }
    #[test]
    fn browser_action_is_get_only_concrete_and_reviewed() {
        for method in ["GET","POST"] {
            let (host,mut runtime)=setup();runtime.snapshot.as_mut().unwrap().mappings.push(Mapping{predicate:"/users".into(),requests:vec![(method.into(),"/users".into())],..Default::default()});let ctx=host.invocation();runtime.render_panes(&ctx).unwrap();let key=runtime.panes.rows.iter().find(|(_,r)|r.endpoint=="mappings").unwrap().0.clone();runtime.select_pane(&key,&ctx).unwrap();
            host.answer_next_prompt(Some(&["http://localhost:9090/users?show=true"]));host.answer_next_prompt(Some(&[]));
            assert_eq!(runtime.open_pane_browser(&ctx).is_ok(),method=="GET");assert_eq!(host.opened_urls().len(),usize::from(method=="GET"));assert!(host.requests().is_empty());
        }
    }
    #[test]
    fn local_mapping_send_requires_concrete_local_review_and_scopes_response_details() {
        for mode in 0..5 {
            let (host,mut runtime)=setup();
            let host=host.with_capabilities(&["project.read","storage","net:localhost"]);
            runtime.snapshot.as_mut().unwrap().mappings.push(Mapping{predicate:"GET /users/{id}".into(),requests:vec![("GET".into(),"/users/{id}".into())],..Default::default()});
            let ctx=host.invocation();runtime.render_panes(&ctx).unwrap();
            let key=runtime.panes.rows.iter().find(|(_,r)|r.endpoint=="mappings").unwrap().0.clone();runtime.select_pane(&key,&ctx).unwrap();
            host.on_fetch(|request| {
                assert_eq!(request.method,"GET");assert_eq!(request.url,"http://localhost:8080/users/42?expand=true");
                assert!(request.headers.is_empty());assert!(request.body.is_none());
                Ok(openidea_plugin::HttpResponse{status:200,headers:vec![("Set-Cookie".into(),"secret-cookie".into())],body:b"{\"id\":42}".to_vec()})
            });
            host.answer_next_prompt(Some(&[match mode {1=>"https://example.com/users/42",2=>"http://localhost:8080/users/{id}",4=>"http://localhost:8080/users/42\r\nInjected: value",_=>"http://localhost:8080/users/42?expand=true"}]));
            host.answer_next_prompt(Some(&[""]));host.answer_next_prompt(Some(&[""]));
            host.answer_next_prompt(if mode==3 {None} else {Some(&[])});
            let result=runtime.send_pane_request(&ctx);
            assert_eq!(result.is_ok(),matches!(mode,0|3),"mode{mode}");
            assert_eq!(host.requests().len(),usize::from(mode==0));
            if mode==0 {
                let tabs=runtime.detail(VIEW,&key,&ctx).unwrap();
                let response=tabs.iter().find(|tab|tab.label=="HTTP Response").unwrap();
                let Pane::Text(text)=&response.body else {panic!()};
                assert!(text.contains("HTTP 200"));assert!(text.contains("42"));assert!(!text.contains("secret-cookie"));
                let other=runtime.panes.rows.iter().find(|(_,r)|r.endpoint=="beans").unwrap().0.clone();
                assert!(!runtime.detail(VIEW,&other,&ctx).unwrap().iter().any(|tab|tab.label=="HTTP Response"));
            }
        }
    }
    #[test]
    fn request_file_creation_is_reviewed_never_overwrites_and_rejects_traversal() {
        for mode in 0..4 {
            let (host,mut runtime) = setup();
            let host = host.with_capabilities(&["project.read","project.write","storage"]);
            runtime.snapshot.as_mut().unwrap().mappings.push(Mapping { predicate:"GET /users".into(), requests:vec![("GET".into(),"/users".into())], ..Default::default() });
            let ctx = host.invocation();
            runtime.render_panes(&ctx).unwrap();
            let key = runtime.panes.rows.iter().find(|(_,r)|r.endpoint=="mappings").unwrap().0.clone();
            runtime.select_pane(&key,&ctx).unwrap();
            if mode == 1 { host.add_file("runtime-requests.http","existing"); }
            host.answer_next_prompt(Some(&[if mode == 2 {"../outside.http"} else {"runtime-requests.http"}]));
            host.answer_next_prompt(if mode == 3 {None} else {Some(&[])});
            let result=runtime.create_pane_requests(&ctx);
            assert_eq!(result.is_ok(),matches!(mode,0|3),"mode {mode}");
            let project=ctx.project().unwrap();
            if mode==0 {assert!(project.read_file("runtime-requests.http").unwrap().contains("GET {{applicationUrl}}/users"));assert_eq!(host.opened_locations().len(),1);}
            if mode==1 {assert_eq!(project.read_file("runtime-requests.http").unwrap(),"existing");}
            if mode>=2 {assert!(!project.exists("runtime-requests.http").unwrap());}
            assert!(host.requests().is_empty());
        }
    }
    #[test]
    fn create_http_request_file_denied_names_permission() {
        let (host,mut runtime) = setup();
        let host = host.with_capabilities(&["project.read","storage"]);
        runtime.snapshot.as_mut().unwrap().mappings.push(Mapping { predicate:"GET /users".into(), requests:vec![("GET".into(),"/users".into())], ..Default::default() });
        let ctx = host.invocation();
        runtime.render_panes(&ctx).unwrap();
        let key = runtime.panes.rows.iter().find(|(_,r)|r.endpoint=="mappings").unwrap().0.clone();
        runtime.select_pane(&key,&ctx).unwrap();
        host.answer_next_prompt(Some(&["runtime-requests.http"]));
        host.answer_next_prompt(Some(&[]));
        let error = runtime.create_pane_requests(&ctx).unwrap_err().to_string();
        assert!(error.contains("needs the project.write permission"), "{error}");
        assert!(error.contains("Settings › Plugins › Spring Runtime Insight"), "{error}");
    }
    #[test]
    fn copy_http_requests_denied_names_permission() {
        let (host,mut runtime) = setup();
        let host = host.with_capabilities(&["project.read","storage"]);
        runtime.snapshot.as_mut().unwrap().mappings.push(Mapping { predicate:"GET /users".into(), requests:vec![("GET".into(),"/users".into())], ..Default::default() });
        let ctx = host.invocation();
        runtime.render_panes(&ctx).unwrap();
        let key = runtime.panes.rows.iter().find(|(_,r)|r.endpoint=="mappings").unwrap().0.clone();
        runtime.select_pane(&key,&ctx).unwrap();
        let error = runtime.copy_pane_requests(&ctx).unwrap_err().to_string();
        assert!(error.contains("needs the clipboard permission"), "{error}");
        assert!(error.contains("Settings › Plugins › Spring Runtime Insight"), "{error}");
    }
    #[test]
    fn explicit_mapping_requests_copy_from_selected_current_snapshot_without_network() {
        let (host, mut runtime) = setup();
        let host = host.with_capabilities(&["project.read", "storage", "clipboard"]);
        let ctx = host.invocation();
        runtime.snapshot.as_mut().unwrap().read("mappings", &serde_json::json!({"contexts":{"application":{"mappings":{"dispatcherServlets":{"dispatcherServlet":[{
            "predicate":"{GET [/users/{id}]} retained",
            "details":{"requestMappingConditions":{"methods":["GET","POST"],"patterns":["/users/{id}"]}}
        }]}}}}}));
        runtime.render_panes(&ctx).unwrap();
        let key = runtime.panes.rows.iter().find(|(_,r)| r.endpoint == "mappings").unwrap().0.clone();
        runtime.select_pane(&key, &ctx).unwrap();
        runtime.handle(VIEW, ViewEvent::Button("snapshot-copy-request".into()), &ctx).unwrap();
        let text = host.clipboard().unwrap();
        assert!(text.contains("GET {{applicationUrl}}/users/{id}"));
        assert!(text.contains("POST {{applicationUrl}}/users/{id}"));
        assert!(text.contains("may differ from Actuator"));
        assert!(!text.contains("retained"), "never parse or insert predicate presentation");
        host.set_setting("actuatorUrl", "\"http://localhost:9090/actuator\"");
        assert!(runtime.copy_pane_requests(&host.invocation()).is_err());
        assert_eq!(host.clipboard().unwrap(), text);
        assert!(host.requests().is_empty());
    }
    #[test]
    fn http_method_filter_uses_structured_metadata_and_survives_same_scope_render() {
        let (host, mut runtime) = setup();
        runtime.snapshot.as_mut().unwrap().mappings.extend([
            Mapping { predicate:"POST misleading presentation".into(), requests:vec![("GET".into(),"/get".into())], ..Default::default() },
            Mapping { predicate:"GET misleading presentation".into(), requests:vec![("POST".into(),"/post".into())], ..Default::default() },
            Mapping { predicate:"GET no metadata".into(), ..Default::default() }
        ]);
        let ctx = host.invocation();
        runtime.render_panes(&ctx).unwrap();
        runtime.panes.method = Some("GET".into());
        let view = runtime.render_panes(&ctx).unwrap();
        let Body::Tree(rows) = view.body else { panic!() };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label, "POST misleading presentation");
        assert!(view.status.unwrap().contains("1 of 5 observations"));
        assert_eq!(runtime.panes.method.as_deref(), Some("GET"));
        host.set_project_root(Some("/other"));
        runtime.render_panes(&host.invocation()).unwrap();
        assert!(runtime.panes.method.is_none());
        assert!(host.requests().is_empty());
    }
    #[test]
    fn missing_or_ambiguous_mapping_metadata_never_invents_get_or_exports_injected_lines() {
        for conditions in [
            serde_json::json!({"methods":[],"patterns":["/any"]}),
            serde_json::json!({"methods":["CUSTOM"],"patterns":["/custom"]}),
            serde_json::json!({"methods":["GET"],"patterns":["/good", "/bad\r\nHeader: value"]}),
            serde_json::json!({"methods":["GET"]})
        ] {
            let mut snapshot = Snapshot::default();
            snapshot.read("mappings", &serde_json::json!({"contexts":{"app":{"mappings":{"dispatcherHandlers":{"webHandler":[{"predicate":"GET /fake", "details":{"requestMappingConditions":conditions}}]}}}}}));
            assert!(snapshot.mappings[0].requests.is_empty());
            assert!(records(&snapshot).iter().all(|r|r.request.is_none()));
        }
        let mut legacy = Snapshot::default();
        legacy.mappings.push(Mapping { requests:vec![("GET".into(),"/bad\nGET /other".into())], ..Default::default() });
        assert!(records(&legacy).iter().all(|r|r.request.is_none()));
    }
    #[test]
    fn observation_search_is_cache_only_case_insensitive_and_clears_on_scope_change() {
        let (host, mut runtime) = setup();
        let ctx = host.invocation();
        runtime.render_panes(&ctx).unwrap();
        runtime.panes.query = Some("COM.ACME.SERVICE".into());
        let filtered = runtime.render_panes(&ctx).unwrap();
        let Body::Tree(rows) = filtered.body else { panic!("native tree") };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label, "service");
        assert!(filtered.status.unwrap().contains("1 of 2 observations"));
        runtime.panes.query = Some("nothing-matches".into());
        let empty = runtime.render_panes(&ctx).unwrap();
        assert!(matches!(empty.body, Body::Tree(rows) if rows.is_empty()));
        runtime.handle(VIEW, ViewEvent::Button("snapshot-search-clear".into()), &ctx).unwrap();
        assert!(runtime.panes.query.is_none());
        assert!(matches!(runtime.render_panes(&ctx).unwrap().body, Body::Tree(rows) if rows.len() == 2));
        runtime.panes.query = Some("service".into());
        host.set_project_root(Some("/other"));
        runtime.render_panes(&host.invocation()).unwrap();
        assert!(runtime.panes.query.is_none());
        assert!(host.requests().is_empty());
    }
    #[test]
    fn command_and_hub_button_reveal_only_the_supporting_native_view() {
        let (host, mut runtime) = setup();
        let ctx = host.invocation();
        runtime.run_command("snapshot-details", &ctx, None).unwrap();
        runtime.handle("runtime", ViewEvent::Button("snapshot-details".into()), &ctx).unwrap();
        assert_eq!(host.revealed_views(), vec!["runtime", "runtime"]);
        assert_eq!(host.fetches(), 0);
        host.set_project_root(None);
        assert!(runtime.run_command("snapshot-details", &host.invocation(), None).is_err());
    }
    #[test]
    fn grouped_observations_keep_lazy_details_and_source_navigation_through_the_real_bridge() {
        let (host, mut runtime) = setup();
        host.add_file("src/Service.java", "package com.acme; class Service {}");
        crate::reveal_view(VIEW).unwrap();
        let request = |runtime: &mut Runtime, operation: Value| -> Value {
            let mut operation = operation;
            operation["channel"] = serde_json::json!("ui-kit");
            operation["sequence"] = serde_json::json!(1);
            operation["project"] = serde_json::json!("/project");
            operation["targetView"] = serde_json::json!(VIEW);
            runtime.handle(crate::VIEW, ViewEvent::Message(operation.to_string()), &host.invocation()).unwrap();
            serde_json::from_str(&host.view_messages().last().unwrap().1).unwrap()
        };
        let rendered = request(&mut runtime, serde_json::json!({"op":"render"}));
        assert_eq!(rendered["model"]["activeView"], VIEW);
        let rows = rendered["model"]["body"]["items"].as_array().unwrap();
        let key = rows[0]["key"].as_str().unwrap().to_owned();
        assert!(rows.iter().all(|row|row["details"].as_array().unwrap().is_empty()), "no eager large per-row detail payload");
        let selected = request(&mut runtime, serde_json::json!({"op":"event","event":{"kind":"select","key":key}}));
        assert!(selected.get("error").is_none(), "{selected}");
        let rows = selected["model"]["body"]["items"].as_array().unwrap();
        assert_eq!(rows[0]["key"], key, "bridge validation render keeps the current row identity");
        let details = rows[0]["details"].as_array().unwrap();
        assert_eq!(details.len(), 2);
        assert_eq!(details[0]["label"], "Observation");
        assert!(details[0]["value"].as_str().unwrap().contains("com.acme.Service"));
        assert_eq!(details[1]["label"], "Provenance");
        assert!(details[1]["value"].as_str().unwrap().contains("Endpoint: beans"));
        assert!(rows[1]["details"].as_array().unwrap().is_empty());
        assert!(!selected.to_string().contains("private-value"));
        let activated = request(&mut runtime, serde_json::json!({"op":"event","event":{"kind":"activate","key":key}}));
        assert!(activated.get("error").is_none(), "{activated}");
        assert_eq!(host.opened_locations().len(), 1);
        runtime.panes.clear();
        let stale = request(&mut runtime, serde_json::json!({"op":"event","event":{"kind":"activate","key":key}}));
        assert!(stale.get("error").is_some());
        assert_eq!(host.opened_locations().len(), 1);
        assert_eq!(host.fetches(), 0);
    }

    #[test]
    fn details_are_cached_bounded_and_explicit_about_provenance_unknowns() {
        let (host, mut runtime) = setup();
        let ctx = host.invocation();
        let view = runtime.render_panes(&ctx).unwrap();
        let Body::Tree(items) = view.body else { panic!() };
        assert_eq!(items.len(), 2);
        let tabs = runtime.detail(VIEW, &items[0].key, &ctx).unwrap();
        assert_eq!(tabs.len(), 2);
        let Pane::Text(observation) = &tabs[0].body else { panic!() };
        assert!(observation.contains("com.acme.Service"));
        assert!(observation.contains("not resolved owners"));
        let Pane::Text(provenance) = &tabs[1].body else { panic!() };
        for expected in ["/project", "Endpoint: beans", "Unix: 123", "current application state", "identity: unknown"] {
            assert!(provenance.contains(expected), "{expected}: {provenance}");
        }
        let tabs = runtime.detail(VIEW, &items[1].key, &ctx).unwrap();
        assert!(!format!("{tabs:?}").contains("private-value"));
        assert_eq!(host.fetches(), 0);
        assert!(host.process_runs().is_empty());
    }
    #[test]
    fn stale_keys_and_cross_project_or_url_scope_never_show_previous_rows() {
        for mode in 0..4 {
            let (host, mut runtime) = setup();
            let ctx = host.invocation();
            let view = runtime.render_panes(&ctx).unwrap();
            let Body::Tree(items) = view.body else { panic!() };
            let key = items[0].key.clone();
            match mode {
                0 => {
                    runtime.render_panes(&ctx).unwrap();
                }
                1 => host.set_project_root(Some("/other")),
                2 => host.set_setting("actuatorUrl", "\"http://localhost:9090/actuator\""),
                _ => runtime.panes.clear(),
            }
            let tabs = runtime.detail(VIEW, &key, &host.invocation()).unwrap();
            assert!(matches!(tabs[0].body, Pane::Message(_)));
            assert!(runtime.activate_pane(&key, &host.invocation()).is_err());
            assert!(host.opened_locations().is_empty());
        }
    }
    fn pane_key(runtime: &Runtime, endpoint: &str, title: &str) -> String {
        runtime.panes.rows.iter().find(|(_, r)| r.endpoint == endpoint && r.title == title).unwrap().0.clone()
    }
    #[test]
    fn mapping_descriptor_selects_exact_overload_and_refuses_unmatched_or_equal_signatures() {
        let docs = serde_json::json!({"contexts":{"application":{"mappings":{"dispatcherServlets":{"dispatcherServlet":[{
            "details":{"handlerMethod":{"className":"com.acme.Users$Api","descriptor":"(Ljava/lang/String;I[Ljava/util/List;)Ljava/lang/String;","name":"find"},
            "requestMappingConditions":{"methods":["GET"],"patterns":["/users"]}},"handler":"x","predicate":"{GET [/users]}"}]}}}}});
        for (source, opens) in [
            ("class Users { static class Api { String find(){return \"\";} String find(String a, int b, List<String>[] c){return a;} } }", true),
            ("class Users { static class Api { String find(String a){return a;} String find(String a, long b, List[] c){return a;} } }", false),
            ("class Users { static class Api { String find(String a, int b, List[] c){return a;} String find(String x, int y, List[] z){return x;} } }", false),
        ] {
            let (host, mut runtime) = setup();
            host.add_file("src/Users.java", &format!("package com.acme; {source}"));
            runtime.snapshot.as_mut().unwrap().read("mappings", &docs);
            let ctx = host.invocation();
            runtime.render_panes(&ctx).unwrap();
            let key = runtime.panes.rows.iter().find(|(_, r)| r.endpoint == "mappings").unwrap().0.clone();
            assert_eq!(runtime.activate_pane(&key, &ctx).is_ok(), opens, "{source}");
        }
        assert_eq!(crate::snapshot::descriptor_params("(IJ[[BLjava/util/Map$Entry;)V").unwrap(), ["int", "long", "byte[]", "Entry"]);
        assert!(crate::snapshot::descriptor_params("(Lbroken)V").is_none());
    }
    #[test]
    fn conditions_show_class_matches_and_navigate_only_to_exact_project_classes() {
        let docs = serde_json::json!({"contexts":{"application":{
            "negativeMatches":{"com.acme.MailConfig":{"notMatched":[{"condition":"OnClassCondition","message":"@ConditionalOnClass did not find required class 'x.Y'"}],"matched":[]}},
            "positiveMatches":{"com.acme.MailConfig#sender":[{"condition":"OnBeanCondition","message":"@ConditionalOnMissingBean (types: x.Sender; SearchStrategy: all) did not find any beans"}],
                "org.springframework.boot.autoconfigure.web.WebAutoConfiguration":[{"condition":"OnWebApplicationCondition","message":"web"}],
                "EndpointAutoConfiguration#propertiesEndpointAccessResolver":[{"condition":"OnBeanCondition","message":"m"}]},
            "unconditionalClasses":["x.Y"]}}});
        let (host, mut runtime) = setup();
        host.add_file("src/MailConfig.java", "package com.acme; @Configuration class MailConfig { @Bean Sender sender(){return null;} }");
        runtime.snapshot.as_mut().unwrap().read("conditions", &docs);
        let ctx = host.invocation();
        runtime.render_panes(&ctx).unwrap();
        let class_row = pane_key(&runtime, "conditions", "com.acme.MailConfig");
        let body = &runtime.panes.rows[&class_row].body;
        assert!(body.contains("negative match") && body.contains("1 matched, 1 not matched"), "{body}");
        assert!(runtime.activate_pane(&class_row, &ctx).is_ok());
        assert!(runtime.activate_pane(&pane_key(&runtime, "conditions", "com.acme.MailConfig#sender"), &ctx).is_ok());
        assert_eq!(host.opened_locations().len(), 2);
        let library = pane_key(&runtime, "conditions", "org.springframework.boot.autoconfigure.web.WebAutoConfiguration");
        assert!(runtime.activate_pane(&library, &ctx).is_err());
        let tabs = runtime.detail(VIEW, &library, &ctx).unwrap();
        assert!(tabs.iter().any(|t| matches!(&t.body, Pane::Text(text) if text.starts_with("library"))));
        assert!(runtime.panes.rows[&pane_key(&runtime, "conditions", "EndpointAutoConfiguration#propertiesEndpointAccessResolver")].target.is_none());
        assert_eq!(host.opened_locations().len(), 2);
    }
    #[test]
    fn configprops_prefix_navigates_to_the_unique_literal_prefix_and_masks_values() {
        let docs = serde_json::json!({"contexts":{"application":{"beans":{
            "spring.web-org.springframework.boot.autoconfigure.web.WebProperties":{"prefix":"spring.web","properties":{"locale":"en","datasource":{"password":"hunter2","url":"jdbc:mysql://root:pw@db/x"}},"inputs":{}},
            "acme.mail-com.acme.Missing":{"prefix":"acme.mail","properties":{"host":"h"},"inputs":{}}}}}});
        for (extra, opens) in [("", true), ("@ConfigurationProperties(\"acme.mail\") class Other {}", false)] {
            let (host, mut runtime) = setup();
            host.add_file("src/MailProps.java", "package com.acme; @ConfigurationProperties(prefix = \"acme.mail\") class MailProps {}");
            if !extra.is_empty() { host.add_file("src/Other.java", &format!("package com.acme; {extra}")); }
            runtime.snapshot.as_mut().unwrap().read("configprops", &docs);
            let ctx = host.invocation();
            runtime.render_panes(&ctx).unwrap();
            let mail = pane_key(&runtime, "configprops", "acme.mail");
            assert_eq!(runtime.activate_pane(&mail, &ctx).is_ok(), opens);
            assert_eq!(host.opened_locations().len(), usize::from(opens));
            let web = &runtime.panes.rows[&pane_key(&runtime, "configprops", "spring.web")];
            assert!(web.body.contains("datasource.password: ******") && web.body.contains("jdbc:mysql://******@db/x") && !web.body.contains("hunter2"), "{}", web.body);
            assert!(runtime.activate_pane(&pane_key(&runtime, "configprops", "spring.web"), &ctx).is_err());
        }
    }
    #[test]
    fn source_navigation_uses_only_unique_fqn_and_nonoverloaded_method() {
        for mode in 0..4 {
            let (host, mut runtime) = setup();
            host.add_file("src/Service.java", "package com.acme; class Service {void run(){} }");
            let ctx = host.invocation();
            if mode == 1 {
                host.add_file("other/Service.java", "package com.acme; class Service {}");
            }
            if mode == 2 {
                host.add_file("src/Service.java", "package other; class Service {}");
            }
            if mode == 3 {
                host.add_file("src/Service.java", "package com.acme; class Service {void run(){} void run(int x){} }");
                runtime.snapshot.as_mut().unwrap().mappings.push(Mapping {
                    predicate: "GET /".into(),
                    method: Some(("com.acme.Service".into(), "run".into())),
                    ..Mapping::default()
                });
            }
            runtime.render_panes(&ctx).unwrap();
            let key =
                runtime.panes.rows.iter().find(|(_, r)| r.endpoint == if mode == 3 { "mappings" } else { "beans" }).unwrap().0.clone();
            let result = runtime.activate_pane(&key, &ctx);
            assert_eq!(result.is_ok(), mode == 0, "mode {mode}");
            assert_eq!(host.opened_locations().len(), usize::from(mode == 0));
            assert_eq!(host.fetches(), 0);
        }
    }
    #[test]
    fn source_candidate_never_belongs_to_another_open_nested_project() {
        for outer in [false, true] {
            let (host, mut runtime) = setup();
            host.add_project("/project/nested");
            host.add_file("/project/nested/Service.java", "package com.acme; class Service {}");
            if outer { host.add_file("/project/src/Service.java", "package com.acme; class Service {}"); }
            let ctx = host.invocation();
            runtime.render_panes(&ctx).unwrap();
            let key = runtime.panes.rows.iter().find(|(_, row)| row.endpoint == "beans").unwrap().0.clone();
            assert_eq!(runtime.activate_pane(&key, &ctx).is_ok(), outer);
            assert_eq!(host.opened_locations().len(), usize::from(outer));
            assert!(host.opened_locations().iter().all(|location| !location.0.contains("/nested/")));
            assert_eq!(host.fetches(), 0);
        }
    }
    #[test]
    fn oversized_source_does_not_silently_hide_ambiguous_owner() {
        let (host, mut runtime) = setup();
        host.add_file("src/Service.java", "package com.acme; class Service {}");
        host.add_file("src/Large.java", &format!("package com.acme;{}", " ".repeat(512 << 10)));
        let ctx = host.invocation();
        runtime.render_panes(&ctx).unwrap();
        let key = runtime.panes.rows.keys().next().unwrap().clone();
        assert!(runtime.activate_pane(&key, &ctx).is_err());
        assert!(host.opened_locations().is_empty());
    }
    #[test]
    fn inlays_use_cached_exact_keys_and_report_age_without_values_or_active_claims() {
        let (host, mut runtime) = setup();
        let d = doc("# 😀\ndemo.key=local\ndemo.secret=local\nDEMO_KEY=no\n", "properties");
        let hints = runtime.inlay_hints(&d, all(&d));
        assert_eq!(hints.len(), 2);
        assert_eq!(hints[0].label, "snapshot: observed");
        assert_eq!(hints[1].label, "snapshot: redacted");
        assert_eq!(offset(&d.text, hints[0].position), d.text.find("=local"));
        let text = format!("{hints:?}");
        assert!(text.contains("Unix 123"));
        assert!(text.contains("unverified"));
        assert!(!text.contains("private-value"));
        assert_eq!(host.fetches(), 0);
        let yaml = doc("# 😀\ndemo:\n  key: local\n", "yaml");
        let hints = runtime.inlay_hints(&yaml, all(&yaml));
        assert_eq!(hints.len(), 1);
        assert_eq!(offset(&yaml.text, hints[0].position), yaml.text.find(": local"));
    }
    #[test]
    fn source_value_inlays_use_current_physical_closing_string_and_never_source_defaults(){
        let (host,mut runtime)=setup();
        let source="class C { /* 😀 */ @Value(\"${demo.key:source-default}\") String value; @Value(\"${missing:source-default}\") String missing; @Value(\"${demo.secret}\") String secret; }";
        let mut d=doc(source,"java");d.path="/project/src/C.java".into();
        let hints=runtime.inlay_hints(&d,all(&d));assert_eq!(hints.len(),2);
        let physical=source.find("\") String value").unwrap()+1;
        assert_eq!(offset(source,hints[0].position),Some(physical));
        let displayed=format!("{hints:?}");assert!(displayed.contains("demo.key"));assert!(displayed.contains("unverified"));assert!(!displayed.contains("private-value"));assert!(!displayed.contains("source-default"));assert_eq!(host.fetches(),0);
        d.text="import other.Value; class C { @Value(\"${demo.key}\") String value; }".into();assert!(runtime.inlay_hints(&d,all(&d)).is_empty());
        d.text="class C { @Value(\"${demo.key}\" + suffix) String value; }".into();assert!(runtime.inlay_hints(&d,all(&d)).is_empty());
        d.text="class C { @Value(\"${missing:${demo.key}}\") String value; }".into();assert!(runtime.inlay_hints(&d,all(&d)).is_empty());
    }
    #[test]
    fn kotlin_value_inlays_respect_literal_physical_anchor(){
        let (_host,mut runtime)=setup();
        let source=r#"class C { @Value("\${demo.key}") lateinit var value:String }"#;
        let mut d=doc(source,"kotlin");d.path="/project/src/C.kt".into();
        let hints=runtime.inlay_hints(&d,all(&d));assert_eq!(hints.len(),1);
        assert_eq!(&source[offset(source,hints[0].position).unwrap()..][..1],")");
    }
    #[test]
    fn cold_disabled_missing_ambiguous_and_other_scope_inlays_are_quiet() {
        for mode in 0..7 {
            let (host, mut runtime) = setup();
            let d = doc("demo.key=local", "properties");
            match mode {
                0 => runtime.loaded = false,
                1 => host.set_setting("snapshotInlays", "false"),
                2 => runtime.snapshot.as_mut().unwrap().missing.push("env (404)".into()),
                3 => runtime.snapshot.as_mut().unwrap().sources[0].props.push(("demo.key".into(), "duplicate".into())),
                4 => host.set_project_root(Some("/other")),
                5 => runtime.scope = None,
                _ => runtime.snapshot.as_mut().unwrap().sources[0].props.resize(8193, ("overflow".into(), "".into())),
            }
            assert!(runtime.inlay_hints(&d, all(&d)).is_empty(), "mode {mode}");
            assert_eq!(host.fetches(), 0);
        }
    }
    #[test]
    fn every_endpoint_has_readonly_observations_and_record_limits() {
        let mut s = Snapshot::default();
        s.beans = vec![Bean::default(); 100];
        s.health = vec![("".into(), "".into()); 100];
        s.sources = vec![Source::default(); 100];
        s.conditions = vec![Condition::default(); 100];
        s.mappings = vec![Mapping::default(); 100];
        s.configprops = vec![PropsBean::default(); 100];
        s.scheduled = vec![Scheduled::default(); 100];
        s.loggers = vec![Logger::default(); 100];
        s.caches = vec![Cache::default(); 100];
        let rows = records(&s);
        assert_eq!(rows.len(), 512);
        for endpoint in ENDPOINTS {
            assert!(rows.iter().any(|r| r.endpoint == endpoint), "{endpoint}");
        }
        assert!(rows.iter().all(|r| r.body.len() <= 8192));
        assert!(rows.iter().all(|r| r.target.is_none()));
    }
    fn bean_key(runtime: &Runtime) -> String {
        runtime.panes.rows.iter().find(|(_, row)| row.endpoint == "beans").unwrap().0.clone()
    }
    #[test]
    fn open_source_does_not_reset_index() {
        let (host, mut runtime) = setup();
        host.add_file("src/Service.java", "package com.acme; class Service {}");
        let ctx = host.invocation();
        runtime.render_panes(&ctx).unwrap();
        let key = bean_key(&runtime);
        runtime.activate_pane(&key, &ctx).unwrap();
        host.add_file("src/Service.java", "package com.acme;\n\n\nclass Service {}");
        runtime.activate_pane(&key, &ctx).unwrap();
        let opened = host.opened_locations();
        assert_eq!(opened.len(), 2);
        assert_eq!(opened[0], opened[1], "the second click used the cached index");
    }
    #[test]
    fn huge_unrelated_file_does_not_block_navigation() {
        let (host, mut runtime) = setup();
        host.add_file("src/Service.java", "package com.acme; class Service {}");
        host.add_file("src/Generated.java", &format!("package org.other;{}", " ".repeat(512 << 10)));
        let ctx = host.invocation();
        runtime.render_panes(&ctx).unwrap();
        runtime.activate_pane(&bean_key(&runtime), &ctx).unwrap();
        assert_eq!(host.opened_locations().len(), 1);
    }
    #[test]
    fn detail_uses_the_cached_index_only() {
        let (host, mut runtime) = setup();
        host.add_file("src/MailConfig.java", "package com.acme; @Configuration class MailConfig {}");
        runtime.snapshot.as_mut().unwrap().read("conditions", &serde_json::json!({"contexts":{"application":{
            "negativeMatches":{"com.acme.MailConfig":{"notMatched":[{"condition":"OnClassCondition","message":"m"}],"matched":[]}},
            "positiveMatches":{},"unconditionalClasses":[]}}}));
        let ctx = host.invocation();
        runtime.render_panes(&ctx).unwrap();
        let row = pane_key(&runtime, "conditions", "com.acme.MailConfig");
        let tabs = runtime.detail(VIEW, &row, &ctx).unwrap();
        assert!(tabs.iter().any(|t| matches!(&t.body, Pane::Text(text) if text.contains("index warming"))));
        runtime.activate_pane(&row, &ctx).unwrap();
        let tabs = runtime.detail(VIEW, &row, &ctx).unwrap();
        assert!(tabs.iter().any(|t| matches!(&t.body, Pane::Text(text) if text.starts_with("Project source"))));
    }
    #[test]
    fn new_project_class_is_not_indexed_yet_not_a_library() {
        let (host, mut runtime) = setup();
        host.add_file("src/Service.java", "package com.acme; class Service {}");
        let project = host.invocation().project().unwrap();
        let scope = runtime.scope.clone().unwrap();
        let look = |runtime: &mut Runtime, class: &str| runtime.resolve(&project, &scope, &Target::Type(class.into(), None), true);
        assert!(matches!(look(&mut runtime, "com.acme.Fresh"), Resolved::NotIndexed));
        assert!(matches!(look(&mut runtime, "org.other.Lib"), Resolved::Library));
    }
    #[test]
    fn kotlin_inlays_cold_instance() {
        let (host, runtime) = setup();
        let scope = runtime.scope.clone().unwrap();
        crate::persistence::write(&scope, runtime.snapshot.as_ref().unwrap()).unwrap();
        let mut cold = Runtime::default();
        let source = r#"class C { @Value("\${demo.key}") lateinit var value:String }"#;
        let mut d = doc(source, "kotlin");
        d.path = "/project/src/C.kt".into();
        assert_eq!(cold.inlay_hints(&d, all(&d)).len(), 1);
        assert!(cold.scope.is_none(), "providers never adopt view state");
        assert_eq!(host.fetches(), 0);
    }

}

fn health_detail_lines(s: &Snapshot, path: &str) -> String {
    let lines: Vec<String> = s.health_details.iter().filter(|(p, ..)| p == path).map(|(_, k, v)| format!("{k}: {v}")).collect();
    if lines.is_empty() { "none reported".to_owned() } else { lines.join("\n") }
}
