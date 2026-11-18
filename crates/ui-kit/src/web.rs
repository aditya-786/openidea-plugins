//! Packaged resource views backed by the existing typed view/command handlers.
//! Native custom editors continue to use their document/undo integration.
use std::cell::{Cell, RefCell};

use openidea_plugin::views::{Body, FieldValue, ViewEvent, Viewport, Views};
use openidea_plugin::{Error, Invocation, Plugin, host};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

thread_local! {
    static RENDERING: Cell<bool> = const { Cell::new(false) };
    static UI_LANGUAGES: RefCell<std::collections::BTreeMap<String,String>> = const { RefCell::new(std::collections::BTreeMap::new()) };
    static ORIGIN_FRAME: RefCell<Option<String>> = const { RefCell::new(None) };
    static GROUP_SELECTIONS: RefCell<std::collections::BTreeMap<String, GroupSelection>> = const { RefCell::new(std::collections::BTreeMap::new()) };
}

struct GroupSelection {
    selected: String,
    observed: Option<String>,
    pending: bool,
}

struct MessageOrigin(Option<String>);
impl MessageOrigin {
    fn enter(frame: Option<String>) -> Self { Self(ORIGIN_FRAME.with(|value| value.replace(frame))) }
}
impl Drop for MessageOrigin {
    fn drop(&mut self) { ORIGIN_FRAME.with(|value| *value.borrow_mut() = self.0.take()); }
}

/// Cosmetic browser-language hint, captured only from correlated auto-language requests.
/// This is not a settings write and never affects project values or command arguments.
pub fn resolved_language(title:&str)->Option<String> {
    UI_LANGUAGES.with(|hints|hints.borrow().get(title).cloned()).or_else(||host::storage::get(&format!("ui-kit.language:{title}")).ok().flatten()).filter(|language|matches!(language.as_str(),"en"|"ja"|"ko"|"zh-CN"))
}
fn record_language(config:&Config,frame_id:Option<&str>,client_locale:Option<&str>) {
    if !matches!(config.ui_language.as_str(),""|"auto")||!frame_id.is_some_and(valid_frame_id){return;}
    let Some(language)=client_locale.filter(|language|matches!(*language,"en"|"ja"|"ko"|"zh-CN"))else{return;};
    let unchanged=UI_LANGUAGES.with(|hints|{let mut hints=hints.borrow_mut();let unchanged=hints.get(&config.title).is_some_and(|old|old==language);hints.insert(config.title.clone(),language.into());unchanged});
    if unchanged{return;}
    let _=host::storage::set(&format!("ui-kit.language:{}",config.title),language);
}

pub fn is_rendering() -> bool {
    RENDERING.with(Cell::get)
}

struct Rendering(bool);
impl Rendering {
    fn enter() -> Self {
        Self(RENDERING.with(|v| v.replace(true)))
    }
}
impl Drop for Rendering {
    fn drop(&mut self) {
        RENDERING.with(|v| v.set(self.0));
    }
}

#[derive(Deserialize)]
struct Config {
    title: String,
    #[serde(default, rename = "uiLanguage")]
    ui_language: String,
    views: std::collections::BTreeMap<String, String>,
    actions: Vec<Action>,
    #[serde(default)]
    project_independent: bool,
    #[serde(default)]
    project_bound_buttons: Vec<String>,
    #[serde(default)]
    project_free_buttons: Vec<String>,
    #[serde(default)]
    opaque_views: Vec<String>,
    #[serde(default)]
    field_actions: std::collections::BTreeMap<String, FieldAction>,
    #[serde(default)]
    context_buttons: std::collections::BTreeMap<String, Vec<String>>,
    #[serde(default)]
    toolbar_icon_buttons: std::collections::BTreeMap<String, Vec<String>>,
    #[serde(default)]
    toolbar_groups: std::collections::BTreeMap<String, Vec<ToolbarGroup>>,
    #[serde(default)]
    group: Option<Group>,
}

#[derive(Deserialize)]
struct Group {
    primary: String,
    tabs: Vec<ViewTab>,
}

#[derive(Deserialize, Serialize)]
struct ToolbarGroup {
    id: String,
    label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    icon: Option<String>,
    items: Vec<String>,
}

#[derive(Deserialize, Serialize)]
struct ViewTab {
    id: String,
    label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    count: Option<usize>,
}

impl Config {
    fn group_for(&self, view: &str) -> Option<&Group> {
        self.group.as_ref().filter(|group| group.tabs.iter().any(|tab| tab.id == view))
    }
    fn validate_group(&self) -> Result<(), Error> {
        let Some(group) = &self.group else { return Ok(()); };
        let mut ids = std::collections::BTreeSet::new();
        if group.tabs.is_empty() || group.tabs.len() > 32
            || !group.tabs.iter().any(|tab| tab.id == group.primary)
            || group.tabs.iter().any(|tab| tab.id.is_empty() || tab.id.len() > 128 || !self.views.contains_key(&tab.id) || !ids.insert(&tab.id)) {
            return Err(Error::Invalid("Invalid grouped view configuration.".into()));
        }
        Ok(())
    }
    fn group_key(&self, group: &Group) -> String {
        format!("ui-kit.view-group:{}:{}", self.title, group.primary)
    }
    fn active_view(&self, view: &str) -> String {
        let Some(group) = self.group_for(view) else { return view.into(); };
        let key = self.group_key(group);
        let stored = host::storage::get(&key);
        let selected = GROUP_SELECTIONS.with(|values| {
            let values = values.borrow();
            if let Some(local) = values.get(&key) {
                if stored.is_err() || (local.pending && stored.as_ref().ok() == Some(&local.observed)) {
                    return Some(local.selected.clone());
                }
            }
            stored.ok().flatten()
        });
        group.tabs.iter().find(|tab| Some(tab.id.as_str()) == selected.as_deref())
            .map_or_else(|| group.primary.clone(), |tab| tab.id.clone())
    }
    fn select_view(&self, physical: &str, target: &str) -> Result<(), Error> {
        let group = self.group_for(physical).ok_or(Error::Stale)?;
        if !group.tabs.iter().any(|tab| tab.id == target) { return Err(Error::Stale); }
        let key = self.group_key(group);
        let observed = host::storage::get(&key).ok().flatten();
        let pending = host::storage::set(&key, target).is_err();
        GROUP_SELECTIONS.with(|values| {
            values.borrow_mut().insert(key, GroupSelection { selected:target.into(), observed, pending });
        });
        Ok(())
    }
    fn physical_view<'a>(&'a self, view: &'a str) -> &'a str {
        self.group_for(view).map_or(view, |group| group.primary.as_str())
    }
}

fn configuration(source: &str) -> Result<Config, Error> {
    let config: Config = serde_json::from_str(source).map_err(|error| Error::Invalid(error.to_string()))?;
    config.validate_group()?;
    Ok(config)
}

/// The logical tab currently selected in this view's kit container.
pub fn active_view(view: &str, source: &str) -> Result<String, Error> {
    Ok(configuration(source)?.active_view(view))
}

/// Selects the logical kit tab before revealing its single native container.
/// Views outside the configured group retain their native routing.
pub fn reveal_view(view: &str, source: &str) -> Result<(), Error> {
    let config = configuration(source)?;
    if config.group_for(view).is_some() {
        config.select_view(view, view)?;
        let physical = config.physical_view(view);
        host::reveal_view(physical)?;
        host::refresh_view(physical);
        return host::post_view_message(physical, r#"{"channel":"ui-kit-refresh"}"#);
    }
    host::reveal_view(view)
}

/// Invalidates the container without changing its selected logical tab.
pub fn refresh_view(view: &str, source: &str) {
    match configuration(source) {
        Ok(config) => {
            let physical = config.physical_view(view);
            host::refresh_view(physical);
            if config.group_for(view).is_some() {
                let _ = host::post_view_message(physical, &json!({"channel":"ui-kit-refresh","targetView":view}).to_string());
            }
        }
        Err(error) => notify_error("Plugin view", "Plugin view", &error),
    }
}

/// Routes legacy web replies to their logical tab; hidden tabs never consume
/// another tab's reply. Native/non-group views retain their original messages.
pub fn post_view_message(view: &str, message: &str, source: &str) -> Result<(), Error> {
    let config = configuration(source)?;
    if config.group_for(view).is_none() { return host::post_view_message(view, message); }
    let payload = serde_json::from_str::<Value>(message).ok();
    let channel = payload.as_ref().and_then(|value| value["channel"].as_str());
    if matches!(channel, Some("gitlab-content-open" | "gitlab-content-reply")) {
        return host::post_view_message(config.physical_view(view), message);
    }
    if channel == Some("ui-kit-refresh") {
        return host::post_view_message(config.physical_view(view), &json!({"channel":"ui-kit-refresh","targetView":view}).to_string());
    }
    let frame = ORIGIN_FRAME.with(|value| value.borrow().clone());
    host::post_view_message(config.physical_view(view), &json!({"channel":"ui-kit-view-message","targetView":view,"frameId":frame,"message":message}).to_string())
}

#[derive(Deserialize, Serialize)]
struct FieldAction {
    button: String,
    #[serde(default)]
    primary: bool,
}

#[derive(Deserialize)]
struct Action {
    view: String,
    id: String,
    label: String,
    kind: String,
    #[serde(default)]
    keys: Option<Vec<String>>,
    #[serde(default)]
    icon: Option<String>,
    #[serde(default)]
    replaces_open: bool,
}

#[derive(Deserialize)]
struct Request {
    channel: String,
    sequence: u64,
    #[serde(default, rename = "frameId")]
    frame_id: Option<String>,
    project: Option<String>,
    #[serde(default, rename="clientLocale")]
    client_locale: Option<String>,
    #[serde(default, rename = "targetView")]
    target_view: Option<String>,
    #[serde(flatten)]
    operation: Operation,
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
enum Operation {
    Render,
    SelectView { target: String },
    OpaqueMessage { message: String },
    Notify { message: String, warning: bool },
    Event { event: Event },
    Command { id: String, key: String },
    ItemButton { key: String, button: String },
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum Event {
    Button { key: String },
    Select { key: String },
    Activate { key: String },
    Expand { key: String },
    Collapse { key: String },
    Field { key: String, value: String },
    Cell { row: u32, column: u32, value: String },
    TableSelect { row: u32 },
    TableActivate { row: u32 },
    Sort { column: u32, descending: bool },
    Message { value: String },
}

impl From<Event> for ViewEvent {
    fn from(event: Event) -> Self {
        match event {
            Event::Button { key } => Self::Button(key),
            Event::Select { key } => Self::Select(key),
            Event::Activate { key } => Self::Activate(key),
            Event::Expand { key } => Self::Expand(key),
            Event::Collapse { key } => Self::Collapse(key),
            Event::Field { key, value } => Self::Field(key, value),
            Event::Cell { row, column, value } => Self::Cell { row, column, value },
            Event::TableSelect { row } => Self::Other { kind: "table-select".into(), data: json!({"row":row}).to_string() },
            Event::TableActivate { row } => Self::Other { kind: "table-activate".into(), data: json!({"row":row}).to_string() },
            Event::Sort { column, descending } => {
                Self::Other { kind: "sort".into(), data: json!({"column":column,"descending":descending}).to_string() }
            }
            Event::Message { value } => Self::Message(value),
        }
    }
}

fn field(value: FieldValue) -> Value {
    match value {
        FieldValue::Text(text) => json!({"kind":"text","text":text}),
        FieldValue::Multiline(text) => json!({"kind":"multiline","text":text}),
        FieldValue::Checkbox(checked) => json!({"kind":"checkbox","checked":checked}),
        FieldValue::Choice(options, text) => json!({"kind":"choice","options":options,"text":text}),
        FieldValue::Other { kind, data } => {
            json!({"kind":kind,"data":serde_json::from_str::<Value>(&data).unwrap_or(Value::Null)})
        }
        _ => json!({"kind":"unsupported"}),
    }
}

fn body(body: Body) -> Value {
    match body {
        Body::Message(text) => json!({"kind":"message","text":text}),
        Body::Html(html) => json!({"kind":"html","html":html}),
        Body::Tree(items) => {
            json!({"kind":"tree","items":items.into_iter().map(|r|json!({"key":r.key,"parent":r.parent,"label":r.label,"icon":r.icon,"detail":r.detail,"expandable":r.expandable,"expanded":r.expanded})).collect::<Vec<_>>()})
        }
        Body::List(items) => {
            json!({"kind":"list","items":items.into_iter().map(|r|json!({"key":r.key,"label":r.label,"icon":r.icon,"detail":r.detail})).collect::<Vec<_>>()})
        }
        Body::Form(fields) => {
            json!({"kind":"form","fields":fields.into_iter().map(|f|json!({"key":f.key,"label":f.label,"value":field(f.value)})).collect::<Vec<_>>()})
        }
        Body::Table(table) => {
            json!({"kind":"table","columns":table.columns,"rows":table.rows,"firstRow":table.first_row,"totalRows":table.total_rows,"editable":table.editable})
        }
        Body::Other { kind, data } => {
            let mut data = serde_json::from_str::<Value>(&data).unwrap_or(json!({}));
            if let Some(map) = data.as_object_mut() {
                map.insert("kind".into(), kind.into());
            }
            data
        }
        _ => json!({"kind":"message","text":"This view needs a newer UI renderer."}),
    }
}

fn contains_key(body: &Body, key: &str) -> bool {
    match body {
        Body::Tree(rows) => rows.iter().any(|r| r.key == key),
        Body::List(rows) => rows.iter().any(|r| r.key == key),
        Body::Other { kind, data } if matches!(kind.as_str(), "resources" | "tree" | "list") => serde_json::from_str::<Value>(data)
            .ok()
            .and_then(|v| {
                v.get("items").and_then(Value::as_array).map(|rows| rows.iter().any(|r| r.get("key").and_then(Value::as_str) == Some(key)))
            })
            .unwrap_or(false),
        _ => false,
    }
}

fn item_selection(body: &Body, key: &str) -> Result<ViewEvent, Error> {
    let bounds = match body {
        Body::Table(table) => Some((u64::from(table.first_row), table.rows.len() as u64)),
        Body::Other { kind, data } if kind == "table" => {
            let table: Value = serde_json::from_str(data).map_err(|_|Error::Stale)?;
            Some((table.get("firstRow").or_else(||table.get("first_row")).and_then(Value::as_u64).unwrap_or(0),table["rows"].as_array().map_or(0,Vec::len) as u64))
        }
        _ => None,
    };
    if let Some((first, count)) = bounds {
        let row: u32 = key.parse().map_err(|_|Error::Stale)?;
        if u64::from(row) < first || u64::from(row)-first >= count { return Err(Error::Stale); }
        return Ok(ViewEvent::Other { kind:"table-select".into(), data:json!({"row":row}).to_string() });
    }
    if contains_key(body,key) { Ok(ViewEvent::Select(key.into())) } else { Err(Error::Stale) }
}

fn valid_frame_id(frame: &str) -> bool {
    frame.len() == 32 && frame.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// User-facing text for a failed view action. The SDK's `Display` keeps developer
/// prefixes ("invalid: …", "permission denied: … is not granted").
fn error_message(error: &Error, feature: &str, plugin: &str) -> String {
    match error {
        Error::Denied(capability) if !capability.contains(char::is_whitespace) => crate::denied::message(feature, capability, plugin),
        Error::NotFound(what) if !what.contains(char::is_whitespace) => format!("Not found: {what}"),
        Error::Denied(text) | Error::Invalid(text) | Error::NotFound(text) | Error::Failed(text) => text.clone(),
        Error::Stale => "This item changed. Refresh and try again.".into(),
        Error::Cancelled => "Cancelled.".into(),
    }
}

fn notify_error(title: &str, plugin: &str, error: &Error) {
    use openidea_plugin::NotificationLevel;
    let level = match error {
        Error::Cancelled => return,
        Error::Denied(_) | Error::Stale | Error::Invalid(_) | Error::NotFound(_) => NotificationLevel::Warning,
        _ => NotificationLevel::Error,
    };
    host::notify(level, title, &error_message(error, title, plugin));
}

/// Handles only the namespaced web protocol. Legacy messages and native events
/// fall through to the existing handler; commands are restricted to the view's
/// declared item actions, and operate only on a currently rendered item.
pub fn handle<T: Plugin + Views>(plugin: &mut T, view: &str, event: &ViewEvent, ctx: &Invocation, config: &str) -> Result<bool, Error> {
    handle_with_actions(plugin, view, event, ctx, config, |_, _, _| Ok(None))
}

/// Refresh row-specific actions from the final view after an operation changes its scope or contents.
pub fn handle_with_actions<T: Plugin + Views>(plugin: &mut T, view: &str, event: &ViewEvent, ctx: &Invocation, config: &str,
    actions: impl Fn(&T, &str, &openidea_plugin::views::View) -> Result<Option<Vec<Value>>, Error>) -> Result<bool, Error> {
    let ViewEvent::Message(text) = event else {
        return Ok(false);
    };
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return Ok(false);
    };
    if value.get("channel").and_then(Value::as_str) != Some("ui-kit") {
        return Ok(false);
    }
    // Correlates a broadcast reply to its requesting page, not an authorization
    // token. Older native mocks may omit it; current browser pages never do.
    let frame_id = value.get("frameId").cloned().unwrap_or(Value::Null);
    let sequence = value.get("sequence").and_then(Value::as_u64).unwrap_or(0);
    let decode = || -> Result<(Request, Config), Error> {
        if text.len() > 262_144 {
            return Err(Error::Invalid("View request is too large.".into()));
        }
        if !frame_id.is_null() && !frame_id.as_str().is_some_and(valid_frame_id) {
            return Err(Error::Invalid("Invalid view frame identity.".into()));
        }
        let request: Request = serde_json::from_value(value).map_err(|e| Error::Invalid(e.to_string()))?;
        let config = configuration(config)?;
        Ok((request, config))
    };
    let (request, config) = match decode() {
        Ok(decoded) => decoded,
        Err(error) => {
            notify_error("Plugin view", "Plugin view", &error);
            host::post_view_message(
                view,
                &json!({"channel":"ui-kit","sequence":sequence,"frameId":frame_id,"project":null,"error":error_message(&error, "Plugin view", "Plugin view")}).to_string(),
            )?;
            return Ok(true);
        }
    };
    if !config.views.contains_key(view) { return Ok(false); }
    let physical = config.physical_view(view).to_owned();
    let initial_view = config.active_view(view);
    let title = config.views.get(&initial_view).ok_or(Error::Stale)?.as_str();
    // Client rendering failures are native notifications too. They never invoke
    // plugin commands or render, including before a project identity is known.
    if let Operation::Notify { message, warning } = &request.operation {
        let message: String = message.chars().take(8192).collect();
        if !message.trim().is_empty() {
            host::notify(if *warning { openidea_plugin::NotificationLevel::Warning } else { openidea_plugin::NotificationLevel::Error }, title, &message);
        }
        host::post_view_message(&physical, &json!({"channel":request.channel,"sequence":request.sequence,"frameId":request.frame_id,"notified":true}).to_string())?;
        return Ok(true);
    }
    // Rendering is always available, including permission/empty-state messages.
    // A denied/missing project is not a common anonymous mutation scope.
    let root = ctx.project().and_then(|p| p.root().ok()).filter(|p| !p.is_empty());
    let project = root.clone().or_else(|| config.project_independent.then(|| "@ui-kit:project-independent".into()));
    let sequence = request.sequence;
    let _rendering = Rendering::enter();
    let _origin = MessageOrigin::enter(request.frame_id.clone());
    let opaque = matches!(&request.operation, Operation::OpaqueMessage { .. });
    let result = (|| -> Result<Value, Error> {
        let grouped = config.group_for(view).is_some();
        let navigation = matches!(request.operation, Operation::SelectView { .. });
        if grouped && !matches!(request.operation, Operation::Render | Operation::Notify { .. })
            && if navigation { request.target_view.as_deref().is_some_and(|target| target != initial_view) }
                else { request.target_view.as_deref() != Some(initial_view.as_str()) } {
            return Err(Error::Stale);
        }
        if !matches!(request.operation, Operation::Render | Operation::SelectView { .. }) {
            let project_free = root.is_none() && matches!(&request.operation,
                Operation::Event { event: Event::Button { key } } if config.project_free_buttons.contains(key));
            if project.is_none() && !project_free {
                return Err(Error::Denied(
                    "Project identity is unavailable. Open a project and allow project.read before using project actions.".into(),
                ));
            }
            if request.project != project {
                return Err(Error::Stale);
            }
            if root.is_none()
                && match &request.operation {
                    Operation::Command { .. } => true,
                    Operation::Event { event: Event::Button { key } } => config.project_bound_buttons.contains(key),
                    Operation::ItemButton { button, .. } => config.project_bound_buttons.contains(button),
                    _ => false,
                }
            {
                return Err(Error::Denied("This action needs an identifiable project. Open a project and allow project.read.".into()));
            }
        }
        record_language(&config,request.frame_id.as_deref(),request.client_locale.as_deref());
        let view = initial_view.as_str();
        match request.operation {
            Operation::Render | Operation::Notify { .. } => {}
            Operation::SelectView { target } => { config.select_view(&physical, &target)?; }
            Operation::OpaqueMessage { message } => {
                if !config.opaque_views.iter().any(|id| id == view) { return Err(Error::Stale); }
                plugin.handle(view, ViewEvent::Message(message), ctx)?;
                return Ok(Value::Null);
            }
            Operation::Event { event } => {
                let current = plugin.render(view, Some(Viewport { first_row: 0, rows: 5000 }), ctx)?;
                let allowed = match &event {
                    Event::Button { key } => current.toolbar.iter().any(|b| b.id == *key && b.enabled),
                    Event::Select { key } | Event::Activate { key } | Event::Expand { key } | Event::Collapse { key } => {
                        contains_key(&current.body, key)
                    }
                    Event::Field { key, .. } => {
                        matches!(&current.body, Body::Form(fields) if fields.iter().any(|f| f.key == *key && !matches!(f.value, FieldValue::Other { .. })))
                    }
                    Event::Cell { row, column, .. } => {
                        matches!(&current.body, Body::Table(t) if t.editable && *row >= t.first_row && (*row - t.first_row) < t.rows.len() as u32 && (*column as usize) < t.columns.len())
                    }
                    Event::TableSelect { row } | Event::TableActivate { row } => match &current.body {
                        Body::Table(t) => *row >= t.first_row && (*row - t.first_row) < t.rows.len() as u32,
                        Body::Other { kind, data } if kind == "table" => serde_json::from_str::<Value>(data).ok().is_some_and(|t| {
                            let first = t.get("firstRow").or_else(|| t.get("first_row")).and_then(Value::as_u64).unwrap_or(0);
                            let count = t.get("rows").and_then(Value::as_array).map_or(0, Vec::len) as u64;
                            u64::from(*row) >= first && u64::from(*row) - first < count
                        }),
                        _ => false,
                    },
                    Event::Sort { column, .. } => match &current.body {
                        Body::Table(t) => (*column as usize) < t.columns.len(),
                        Body::Other { kind, .. } => kind == "table",
                        _ => false,
                    },
                    Event::Message { .. } => matches!(&current.body, Body::Html(_)),
                };
                if !allowed {
                    return Err(Error::Invalid("This control is no longer available. Refresh the view.".into()));
                }
                plugin.handle(view, event.into(), ctx)?;
            }
            Operation::ItemButton { key, button } => {
                let current = plugin.render(view, Some(Viewport { first_row: 0, rows: 5000 }), ctx)?;
                let original = current.toolbar.iter().find(|action|action.id == button && action.enabled).ok_or(Error::Stale)?;
                let label = original.label.clone();let icon = original.icon.clone();
                let selection = item_selection(&current.body, &key)?;
                plugin.handle(view, selection, ctx)?;
                let selected = plugin.render(view, Some(Viewport { first_row: 0, rows: 5000 }), ctx)?;
                item_selection(&selected.body, &key)?;
                // Some plugins rotate scoped callback IDs on selection. Resolve only
                // an unambiguous, still-enabled action with the same presentation.
                let mut candidates = selected.toolbar.iter().filter(|action|action.enabled && action.label == label && action.icon == icon);
                let action = candidates.next().ok_or(Error::Stale)?;
                if candidates.next().is_some() { return Err(Error::Stale); }
                plugin.handle(view, ViewEvent::Button(action.id.clone()), ctx)?;
            }
            Operation::Command { id, key } => {
                let kind = key.split(':').next().unwrap_or_default();
                if !config.actions.iter().any(|a| a.view == view && a.id == id && (a.kind.is_empty() || a.kind == kind) && a.keys.as_ref().is_none_or(|keys| keys.contains(&key))) {
                    return Err(Error::Denied("This action is not declared for the selected resource.".into()));
                }
                let current = plugin.render(view, Some(Viewport { first_row: 0, rows: 5000 }), ctx)?;
                if !contains_key(&current.body, &key) {
                    return Err(Error::Stale);
                }
                if let Some(message) = plugin.run_command(&id, ctx, Some(&key))? {
                    host::notify(openidea_plugin::NotificationLevel::Info, title, &message);
                }
            }
        }
        // A command can reveal another logical tab; render the authoritative tab.
        let selected_view = config.active_view(&physical);
        let view = selected_view.as_str();
        let title = config.views.get(view).ok_or(Error::Stale)?;
        let model = plugin.render(view, Some(Viewport { first_row: 0, rows: 5000 }), ctx)?;
        let actions = actions(plugin, view, &model)?.unwrap_or_else(|| config.actions.iter().filter(|a|a.view==view)
            .map(|a|json!({"id":a.id,"label":a.label,"kind":a.kind,"keys":a.keys,"icon":a.icon,"replacesOpen":a.replaces_open})).collect());
        Ok(json!({"title":title,"plugin":config.title,"uiLanguage":config.ui_language,"body":body(model.body),"status":model.status,
            "toolbar":model.toolbar.iter().map(|b|json!({"key":b.id,"label":b.label,"icon":b.icon,"enabled":b.enabled})).collect::<Vec<_>>(),
            "actions":actions,"fieldActions":config.field_actions,"contextButtons":config.context_buttons.get(view),"iconButtons":config.toolbar_icon_buttons.get(view),"toolbarGroups":config.toolbar_groups.get(view),
            "activeView":view,"viewTabs":config.group_for(&physical).map(|group| &group.tabs)}))
    })();
    let answer = match result {
        Ok(_) if opaque => {
            json!({"channel":request.channel,"sequence":sequence,"frameId":request.frame_id,"project":project,"handled":true})
        }
        Ok(model) => {
            json!({"channel":request.channel,"sequence":sequence,"frameId":request.frame_id,"project":project,"model":model})
        }
        Err(error) => {
            // A rejected background poll is retried by its page; a notification per attempt would flood the user.
            if !(opaque && matches!(error, Error::Stale)) {
                notify_error(title, &config.title, &error);
            }
            // The page names a denied capability and the plugin's settings page in its empty state.
            let denied = match &error { Error::Denied(capability) => Some(capability), _ => None };
            json!({"channel":request.channel,"sequence":sequence,"frameId":request.frame_id,"project":project,"error":error_message(&error, title, &config.title),"denied":denied,"plugin":config.title})
        }
    };
    host::post_view_message(&physical, &answer.to_string())?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::views::{ListItem, View, Views};
    use openidea_plugin::{Plugin, mock::MockHost};
    #[derive(Default)]
    struct Demo {
        calls: Vec<String>,
    }
    impl Plugin for Demo {
        fn run_command(&mut self, id: &str, _: &Invocation, arg: Option<&str>) -> Result<Option<String>, Error> {
            self.calls.push(format!("{id}:{}", arg.unwrap_or_default()));
            Ok(None)
        }
    }
    impl Views for Demo {
        fn render(&mut self, _: &str, _: Option<Viewport>, _: &Invocation) -> Result<View, Error> {
            Ok(View::new(Body::List(vec![ListItem::new("route:1", "/orders").icon("file-code")])).button(openidea_plugin::views::Button::new("cli", "CLI").icon("terminal")))
        }
        fn handle(&mut self, _: &str, event: ViewEvent, _: &Invocation) -> Result<(), Error> {
            self.calls.push(format!("event:{event:?}"));
            Ok(())
        }
    }
    const CONFIG: &str =
        r#"{"title":"Demo","views":{"routes":"Routes"},"actions":[{"view":"routes","id":"copy","label":"Copy","kind":"route"}]}"#;
    #[test]
    fn failures_are_native_notifications_and_reported_once() {
        let mock = MockHost::install();
        let mut demo = Demo::default();
        let request = json!({"channel":"ui-kit","sequence":1,"op":"command","project":"/wrong","id":"copy","key":"route:1"});
        handle(&mut demo, "routes", &ViewEvent::Message(request.to_string()), &mock.invocation(), CONFIG).unwrap();
        let notes = mock.notifications();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].level, openidea_plugin::NotificationLevel::Warning);
        assert_eq!(notes[0].title, "Routes");
        assert!(demo.calls.is_empty());
        notify_error("Laravel", "Laravel", &Error::Failed("artisan optimize:clear failed".into()));
        assert_eq!(mock.notifications()[1].level, openidea_plugin::NotificationLevel::Error);
        notify_error("Laravel", "Laravel", &Error::Cancelled);
        assert_eq!(mock.notifications().len(), 2, "cancelling is not an error");
    }

    #[test]
    fn action_errors_use_plain_wording() {
        assert_eq!(error_message(&Error::Denied("clipboard".into()), "Routes", "Laravel"),
            "Routes needs the clipboard permission. Allow it in Settings › Plugins › Laravel.");
        assert_eq!(error_message(&Error::Denied("Open a project first.".into()), "Routes", "Laravel"), "Open a project first.");
        assert_eq!(error_message(&Error::Invalid("Select a route.".into()), "Routes", "Laravel"), "Select a route.");
        assert_eq!(error_message(&Error::NotFound("routes/web.php".into()), "Routes", "Laravel"), "Not found: routes/web.php");
        assert_eq!(error_message(&Error::Stale, "Routes", "Laravel"), "This item changed. Refresh and try again.");
    }

    #[test]
    fn client_failures_use_bounded_native_notifications_without_plugin_execution() {
        let mock = MockHost::install().with_capabilities(&[]);
        let mut demo = Demo::default();
        for warning in [false, true] {
            let request = json!({"channel":"ui-kit","sequence":0,"frameId":"0123456789abcdef0123456789abcdef","op":"notify","warning":warning,"message":"é".repeat(9000)});
            handle(&mut demo, "routes", &ViewEvent::Message(request.to_string()), &mock.invocation(), CONFIG).unwrap();
        }
        let notes = mock.notifications();
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0].level, openidea_plugin::NotificationLevel::Error);
        assert_eq!(notes[1].level, openidea_plugin::NotificationLevel::Warning);
        assert_eq!(notes[0].body.chars().count(), 8192);
        assert!(demo.calls.is_empty());
        let reply: Value = serde_json::from_str(&mock.view_messages().last().unwrap().1).unwrap();
        assert_eq!(reply["notified"], true);
        assert_eq!(reply["sequence"], 0);
        assert!(reply.get("model").is_none());
    }

    #[test]
    fn web_route_trees_keep_structured_methods_and_action_identity() {
        use crate::routes::{Route, RouteList, Handler};
        let _mock = MockHost::install();
        let mut routes = RouteList::new("badge-fixture", "aritti.openapi");
        routes.set_routes(vec![Route {method:"get".into(),path:"/user".into(),handler:Handler {class:"(no tag)".into(),member:None},name:None,location:None}]);
        routes.handle(&ViewEvent::Button("routes:mode".into()));
        let _rendering = Rendering::enter();
        let view = routes.render(View::new(Body::List(vec![])));
        assert!(contains_key(&view.body, "route:0"));
        assert!(contains_key(&view.body, "controller:(no tag)"));
        assert!(!contains_key(&view.body, "route:999"));
        let encoded = body(view.body);
        let row = encoded["items"].as_array().unwrap().iter().find(|row|row["key"] == "route:0").unwrap();
        assert_eq!(encoded["kind"], "tree");
        assert_eq!(row["method"], "GET");
        assert_eq!(row["detail"], "(no tag)");
        assert_eq!(row["parent"], "controller:(no tag)");
    }

    #[test]
    fn declared_icons_survive_render_without_changing_action_identity() {
        let mock = MockHost::install();
        handle(&mut Demo::default(), "routes",
            &ViewEvent::Message(json!({"channel":"ui-kit","sequence":9,"op":"render"}).to_string()),
            &mock.invocation(), CONFIG).unwrap();
        let answer: Value = serde_json::from_str(&mock.view_messages().last().unwrap().1).unwrap();
        assert_eq!(answer["sequence"], 9);
        assert_eq!(answer["model"]["toolbar"][0]["key"], "cli");
        assert_eq!(answer["model"]["toolbar"][0]["icon"], "terminal");
        assert_eq!(answer["model"]["body"]["items"][0]["icon"], "file-code");
        let tree = body(Body::Tree(vec![openidea_plugin::views::TreeItem::new("group", "Sources").icon("folder")]));
        assert_eq!(tree["items"][0]["icon"], "folder");
        assert_eq!(tree["items"][0]["key"], "group");
    }

    #[test]
    fn view_commands_require_current_scope_and_declared_resource() {
        let mock = MockHost::install();
        let ctx = mock.invocation();
        let mut demo = Demo::default();
        for (project, id, key) in [("/wrong", "copy", "route:1"), ("/project", "delete", "route:1"), ("/project", "copy", "route:9")] {
            let event =
                ViewEvent::Message(json!({"channel":"ui-kit","op":"command","sequence":1,"project":project,"id":id,"key":key}).to_string());
            assert!(handle(&mut demo, "routes", &event, &ctx, CONFIG).unwrap());
            assert!(demo.calls.is_empty());
        }
        let event = ViewEvent::Message(
            json!({"channel":"ui-kit","op":"command","sequence":2,"project":"/project","id":"copy","key":"route:1"}).to_string(),
        );
        handle(&mut demo, "routes", &event, &ctx, CONFIG).unwrap();
        assert_eq!(demo.calls, ["copy:route:1"]);
        assert!(!is_rendering());
    }
    #[test]
    fn native_and_legacy_events_are_not_intercepted() {
        let mock = MockHost::install();
        let mut demo = Demo::default();
        for event in [ViewEvent::Button("refresh".into()), ViewEvent::Message("node:1".into())] {
            assert!(!handle(&mut demo, "routes", &event, &mock.invocation(), CONFIG).unwrap());
        }
    }
    #[test]
    fn read_only_fields_keep_their_semantics() {
        let model = field(FieldValue::readonly_text("<unsafe>", true));
        assert_eq!(model["kind"], "readonly-text");
        assert_eq!(model["data"]["text"], "<unsafe>");
    }
    #[test]
    fn anonymous_scopes_cannot_dispatch_commands_or_events_but_can_render() {
        for denied in [false, true] {
            let mock = MockHost::install().with_capabilities(if denied { &[] } else { &["project.read"] });
            if !denied {
                mock.set_project_root(None);
            }
            let mut demo = Demo::default();
            for project in [Value::Null, json!("")] {
                for operation in
                    [json!({"op":"command","id":"copy","key":"route:1"}), json!({"op":"event","event":{"kind":"button","key":"cli"}})]
                {
                    let mut request = json!({"channel":"ui-kit","sequence":1,"project":project});
                    request.as_object_mut().unwrap().extend(operation.as_object().unwrap().clone());
                    handle(&mut demo, "routes", &ViewEvent::Message(request.to_string()), &mock.invocation(), CONFIG).unwrap();
                    assert!(demo.calls.is_empty());
                    let answer: Value = serde_json::from_str(&mock.view_messages().last().unwrap().1).unwrap();
                    assert!(answer.get("error").is_some());
                    assert!(answer["denied"].as_str().is_some_and(|denied| denied.contains("project.read")));
                }
            }
            handle(
                &mut demo,
                "routes",
                &ViewEvent::Message(json!({"channel":"ui-kit","sequence":2,"op":"render"}).to_string()),
                &mock.invocation(),
                CONFIG,
            )
            .unwrap();
            let answer: Value = serde_json::from_str(&mock.view_messages().last().unwrap().1).unwrap();
            assert!(answer.get("model").is_some());
            assert!(answer["project"].is_null());
        }
    }

    #[test]
    fn project_free_forms_opt_in_without_authorizing_project_actions() {
        let mock = MockHost::install().with_capabilities(&[]);
        let mut demo = Demo::default();
        let mut config: Value = serde_json::from_str(CONFIG).unwrap();
        config["project_independent"] = json!(true);
        config["project_bound_buttons"] = json!(["cli"]);
        for (operation, allowed) in [
            (json!({"op":"event","event":{"kind":"select","key":"route:1"}}), true),
            (json!({"op":"event","event":{"kind":"button","key":"cli"}}), false),
            (json!({"op":"item-button","key":"route:1","button":"cli"}), false),
            (json!({"op":"command","id":"copy","key":"route:1"}), false),
        ] {
            let before = demo.calls.len();
            let mut request = json!({"channel":"ui-kit","sequence":1,"project":"@ui-kit:project-independent"});
            request.as_object_mut().unwrap().extend(operation.as_object().unwrap().clone());
            handle(&mut demo, "routes", &ViewEvent::Message(request.to_string()), &mock.invocation(), &config.to_string()).unwrap();
            assert_eq!(demo.calls.len() > before, allowed);
        }
    }

    #[test]
    fn previous_project_actions_are_stale_after_a_switch() {
        let mock = MockHost::install();
        let mut demo = Demo::default();
        mock.set_project_root(Some("/second"));
        let request = json!({"channel":"ui-kit","sequence":1,"project":"/project","op":"command","id":"copy","key":"route:1"});
        handle(&mut demo, "routes", &ViewEvent::Message(request.to_string()), &mock.invocation(), CONFIG).unwrap();
        assert!(demo.calls.is_empty());
    }
    #[test]
    fn same_project_and_sequence_replies_preserve_distinct_requesting_frames() {
        let mock = MockHost::install();
        let mut demo = Demo::default();
        let frames = ["00000000000000000000000000000001", "00000000000000000000000000000002"];
        for frame in frames {
            let request = json!({"channel":"ui-kit","sequence":7,"project":"/project","frameId":frame,"op":"render"});
            handle(&mut demo, "routes", &ViewEvent::Message(request.to_string()), &mock.invocation(), CONFIG).unwrap();
            let answer: Value = serde_json::from_str(&mock.view_messages().last().unwrap().1).unwrap();
            assert_eq!(answer["frameId"], frame);
            assert_eq!(answer["sequence"], 7);
            assert_eq!(answer["project"], "/project");
            assert!(answer.get("model").is_some());
        }
        assert!(demo.calls.is_empty());
    }

    #[test]
    fn malformed_frame_replies_echo_identity_without_dispatching_mutations() {
        let mock = MockHost::install();
        let mut demo = Demo::default();
        for frame in [
            json!(""),
            json!("a".repeat(31)),
            json!("a".repeat(33)),
            json!("A".repeat(32)),
            json!("g".repeat(32)),
            json!(12),
            json!({"frame":"invalid"}),
        ] {
            let request = json!({"channel":"ui-kit","sequence":9,"project":"/project","frameId":frame,
                "op":"command","id":"copy","key":"route:1"});
            handle(&mut demo, "routes", &ViewEvent::Message(request.to_string()), &mock.invocation(), CONFIG).unwrap();
            let answer: Value = serde_json::from_str(&mock.view_messages().last().unwrap().1).unwrap();
            assert_eq!(answer["frameId"], frame);
            assert_eq!(answer["sequence"], 9);
            assert!(answer["error"].as_str().unwrap().contains("frame"));
            assert!(answer.get("model").is_none());
            assert!(demo.calls.is_empty());
        }
    }

    #[test]
    fn valid_frame_is_echoed_on_project_membership_and_decode_errors() {
        let mock = MockHost::install();
        let mut demo = Demo::default();
        let frame = "0123456789abcdef0123456789abcdef";
        for operation in [
            json!({"op":"command","id":"copy","key":"route:1","project":"/wrong"}),
            json!({"op":"command","id":"copy","key":"route:missing","project":"/project"}),
            json!({"op":"event","project":"/project","event":{"kind":"button","key":"missing"}}),
            json!({"op":"invalid","project":"/project"}),
        ] {
            let mut request = json!({"channel":"ui-kit","sequence":1,"frameId":frame});
            request.as_object_mut().unwrap().extend(operation.as_object().unwrap().clone());
            handle(&mut demo, "routes", &ViewEvent::Message(request.to_string()), &mock.invocation(), CONFIG).unwrap();
            let answer: Value = serde_json::from_str(&mock.view_messages().last().unwrap().1).unwrap();
            assert_eq!(answer["frameId"], frame);
            assert!(answer.get("error").is_some());
            assert!(answer.get("model").is_none());
            assert!(demo.calls.is_empty());
        }
    }
    #[test]
    fn row_specific_action_keys_are_enforced_and_final_actions_are_refreshed() {
        let mock = MockHost::install();
        let mut demo = Demo::default();
        let root = mock.invocation().project().unwrap().root().unwrap();
        for keys in [json!([]), json!(["route:2"])] {
            let mut config: Value = serde_json::from_str(CONFIG).unwrap();config["actions"][0]["keys"] = keys;
            let request = json!({"channel":"ui-kit","sequence":1,"op":"command","project":root,"id":"copy","key":"route:1"});
            handle(&mut demo,"routes",&ViewEvent::Message(request.to_string()),&mock.invocation(),&config.to_string()).unwrap();
            let answer: Value = serde_json::from_str(&mock.view_messages().last().unwrap().1).unwrap();
            assert!(answer.get("error").is_some());assert!(demo.calls.is_empty());
        }
        let request = json!({"channel":"ui-kit","sequence":2,"op":"command","project":root,"id":"copy","key":"route:1"});
        handle_with_actions(&mut demo,"routes",&ViewEvent::Message(request.to_string()),&mock.invocation(),CONFIG,|plugin,_,_| {
            assert_eq!(plugin.calls,["copy:route:1"]);
            Ok(Some(vec![json!({"id":"next","label":"Updated action","keys":["route:1"]})]))
        }).unwrap();
        let answer: Value = serde_json::from_str(&mock.view_messages().last().unwrap().1).unwrap();
        assert_eq!(answer["model"]["actions"][0]["id"],"next");
    }

    #[test]
    fn item_buttons_reselect_the_clicked_row_and_refresh_scoped_callbacks() {
        struct Selected { key: String, epoch: usize, calls: Vec<String> }
        impl Plugin for Selected { fn run_command(&mut self,_:&str,_:&Invocation,_:Option<&str>)->Result<Option<String>,Error>{Ok(None)} }
        impl Views for Selected {
            fn render(&mut self,_:&str,_:Option<Viewport>,_:&Invocation)->Result<View,Error> {
                Ok(View::new(Body::List(vec![ListItem::new("user","User"),ListItem::new("builtin","Built-in")]))
                    .button(openidea_plugin::views::Button::new(&format!("delete:{}",self.epoch),"Delete").icon("trash").enabled(self.key == "user")))
            }
            fn handle(&mut self,_:&str,event:ViewEvent,_:&Invocation)->Result<(),Error> {
                match event {
                    ViewEvent::Select(key)=>{self.key=key;self.epoch+=1;},
                    ViewEvent::Button(id)=>self.calls.push(format!("{id}:{}",self.key)),
                    _=>{}
                }
                Ok(())
            }
        }
        let mock=MockHost::install();let root=mock.invocation().project().unwrap().root().unwrap();
        let mut selected=Selected {key:"user".into(),epoch:0,calls:Vec::new()};
        let run=|selected:&mut Selected,key:&str,button:&str| {
            let request=json!({"channel":"ui-kit","sequence":1,"op":"item-button","project":root,"key":key,"button":button});
            handle(selected,"routes",&ViewEvent::Message(request.to_string()),&mock.invocation(),CONFIG).unwrap();
            serde_json::from_str::<Value>(&mock.view_messages().last().unwrap().1).unwrap()
        };
        assert!(run(&mut selected,"missing","delete:0").get("error").is_some());assert_eq!(selected.epoch,0);
        assert!(run(&mut selected,"builtin","delete:0").get("error").is_some());assert!(selected.calls.is_empty());
        selected.key="user".into();
        assert!(run(&mut selected,"user","delete:1").get("error").is_none());
        assert_eq!(selected.calls,["delete:2:user"]);
    }

}

#[cfg(test)]
mod group_tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    use openidea_plugin::views::{Button, ListItem, View};

    const CONFIG: &str = r#"{
        "title":"Grouped Bridge Tests",
        "views":{"routes":"Routes","details":"Details"},
        "group":{"primary":"routes","tabs":[{"id":"routes","label":"Routes","icon":"web"},{"id":"details","label":"Details"}]},
        "actions":[{"view":"details","id":"copy","label":"Copy","kind":"route"}],
        "opaque_views":["details"],
        "toolbar_groups":{"routes":[{"id":"more","label":"More","icon":"more","items":["open","create-token"]}]},
        "project_free_buttons":["create-token","disabled"]
    }"#;
    const FRAME: &str = "0123456789abcdef0123456789abcdef";

    #[derive(Default)]
    struct GroupDemo { renders: Vec<String>, calls: Vec<String> }
    impl Plugin for GroupDemo {
        fn run_command(&mut self, id: &str, _: &Invocation, arg: Option<&str>) -> Result<Option<String>, Error> {
            self.calls.push(format!("command:{id}:{}",arg.unwrap_or_default()));
            Ok(None)
        }
    }
    impl Views for GroupDemo {
        fn render(&mut self, view: &str, _: Option<Viewport>, _: &Invocation) -> Result<View, Error> {
            self.renders.push(view.into());
            Ok(View::new(Body::List(vec![ListItem::new("route:1",view)]))
                .button(Button::new("open","Open"))
                .button(Button::new("create-token","Create token"))
                .button(Button::new("disabled","Disabled").enabled(false)))
        }
        fn handle(&mut self, view: &str, event: ViewEvent, _: &Invocation) -> Result<(), Error> {
            self.calls.push(format!("{view}:{event:?}"));
            match event {
                ViewEvent::Button(id) if id == "open" => reveal_view("details", CONFIG)?,
                ViewEvent::Message(message) => post_view_message(view,&message,CONFIG)?,
                _ => {}
            }
            Ok(())
        }
    }
    fn request(host: &MockHost, demo: &mut GroupDemo, operation: Value) -> Value {
        let mut value = json!({"channel":"ui-kit","sequence":7,"frameId":FRAME,"project":"/project"});
        value.as_object_mut().unwrap().extend(operation.as_object().unwrap().clone());
        assert!(handle(demo,"routes",&ViewEvent::Message(value.to_string()),&host.invocation(),CONFIG).unwrap());
        let messages=host.view_messages();
        assert_eq!(messages.last().unwrap().0,"routes");
        serde_json::from_str(&messages.last().unwrap().1).unwrap()
    }

    #[test]
    fn logical_tabs_dispatch_and_reply_through_one_container() {
        let host=MockHost::install();let mut demo=GroupDemo::default();
        let initial=request(&host,&mut demo,json!({"op":"render"}));
        assert_eq!(initial["model"]["activeView"],"routes");
        assert_eq!(initial["model"]["viewTabs"].as_array().unwrap().len(),2);
        assert_eq!(initial["model"]["viewTabs"][0]["icon"],"web");
        assert_eq!(initial["model"]["toolbarGroups"][0]["items"],json!(["open","create-token"]));
        assert!(initial["model"]["viewTabs"][1].get("icon").is_none());
        let changed=request(&host,&mut demo,json!({"op":"select-view","target":"details","targetView":"routes"}));
        assert_eq!(changed["model"]["activeView"],"details");
        assert_eq!(changed["model"]["title"],"Details");
        assert_eq!(changed["frameId"],FRAME);assert_eq!(changed["sequence"],7);
        let copied=request(&host,&mut demo,json!({"op":"command","id":"copy","key":"route:1","targetView":"details"}));
        assert!(copied.get("error").is_none());
        assert_eq!(demo.calls,["command:copy:route:1"]);
        assert_eq!(demo.renders,["routes","details","details","details"]);
        assert_eq!(active_view("routes",CONFIG).unwrap(),"details");
    }

    #[test]
    fn a_stale_background_poll_is_answered_without_a_notification() {
        let host=MockHost::install();let mut demo=GroupDemo::default();
        request(&host,&mut demo,json!({"op":"select-view","target":"details","targetView":"routes"}));
        let before=host.notifications().len();
        let reply=request(&host,&mut demo,json!({"op":"opaque-message","targetView":"routes","message":"{\"op\":\"index-tick\"}"}));
        assert!(reply.get("error").is_some());
        assert_eq!(host.notifications().len(),before);
        let rejected=request(&host,&mut demo,json!({"op":"command","targetView":"routes","id":"copy","key":"route:1"}));
        assert!(rejected.get("error").is_some());
        assert_eq!(host.notifications().len(),before+1);
    }

    #[test]
    fn hidden_missing_and_unknown_targets_cannot_mutate_or_change_navigation() {
        let host=MockHost::install();let mut demo=GroupDemo::default();
        reveal_view("details",CONFIG).unwrap();
        for operation in [
            json!({"op":"event","event":{"kind":"button","key":"open"}}),
            json!({"op":"event","targetView":"routes","event":{"kind":"button","key":"open"}}),
            json!({"op":"select-view","targetView":"routes","target":"routes"}),
            json!({"op":"select-view","targetView":"details","target":"foreign"}),
            json!({"op":"command","targetView":"routes","id":"copy","key":"route:1"})
        ] {
            let reply=request(&host,&mut demo,operation);
            assert!(reply.get("error").is_some());assert!(demo.calls.is_empty());assert!(demo.renders.is_empty());
            assert_eq!(active_view("routes",CONFIG).unwrap(),"details");
        }
        let denied=request(&host,&mut demo,json!({"op":"command","targetView":"details","id":"undeclared","key":"route:1"}));
        assert!(denied.get("error").is_some());assert!(demo.calls.is_empty());
    }

    #[test]
    fn navigation_and_only_enabled_opted_in_buttons_work_without_a_project() {
        let host=MockHost::install().with_capabilities(&["storage"]);host.set_project_root(None);
        let mut demo=GroupDemo::default();
        let reply=request(&host,&mut demo,json!({"op":"select-view","target":"details","project":null}));
        assert_eq!(reply["model"]["activeView"],"details");assert!(reply["project"].is_null());
        let reply=request(&host,&mut demo,json!({"op":"event","targetView":"details","project":null,"event":{"kind":"button","key":"create-token"}}));
        assert!(reply.get("error").is_none());assert_eq!(demo.calls.len(),1);
        for operation in [
            json!({"op":"event","targetView":"details","project":null,"event":{"kind":"button","key":"disabled"}}),
            json!({"op":"event","targetView":"details","project":null,"event":{"kind":"button","key":"open"}}),
            json!({"op":"command","targetView":"details","project":null,"id":"copy","key":"route:1"}),
            json!({"op":"event","targetView":"details","project":"/other","event":{"kind":"button","key":"create-token"}})
        ] {
            assert!(request(&host,&mut demo,operation).get("error").is_some());
            assert_eq!(demo.calls.len(),1);
        }
    }

    #[test]
    fn command_navigation_renders_new_tab_and_invalidation_does_not_switch_tabs() {
        let host=MockHost::install();let mut demo=GroupDemo::default();
        let reply=request(&host,&mut demo,json!({"op":"event","targetView":"routes","event":{"kind":"button","key":"open"}}));
        assert_eq!(reply["model"]["activeView"],"details");
        assert_eq!(host.revealed_views(),["routes"]);
        assert!(host.view_messages().iter().any(|(_,message)|message==r#"{"channel":"ui-kit-refresh"}"#));
        refresh_view("routes",CONFIG);
        assert_eq!(active_view("routes",CONFIG).unwrap(),"details");
        let message:Value=serde_json::from_str(&host.view_messages().last().unwrap().1).unwrap();
        assert_eq!(message,json!({"channel":"ui-kit-refresh","targetView":"routes"}));
        assert!(host.refreshed_views().iter().all(|view|view=="routes"));
    }

    #[test]
    fn opaque_private_protocol_is_target_scoped_correlated_and_does_not_rerender() {
        let host=MockHost::install();let mut demo=GroupDemo::default();
        let denied=request(&host,&mut demo,json!({"op":"opaque-message","targetView":"routes","message":"private"}));
        assert!(denied.get("error").is_some());assert!(demo.renders.is_empty());assert!(demo.calls.is_empty());
        reveal_view("details",CONFIG).unwrap();
        let reply=request(&host,&mut demo,json!({"op":"opaque-message","targetView":"details","message":"private"}));
        assert_eq!(reply["handled"],true);assert!(reply.get("model").is_none());
        assert_eq!(reply["frameId"],FRAME);assert!(demo.renders.is_empty());
        assert_eq!(demo.calls,["details:Message(\"private\")"]);
        let envelope:Value=host.view_messages().iter().filter_map(|(_,message)|serde_json::from_str::<Value>(message).ok()).find(|message|message["channel"]=="ui-kit-view-message").unwrap();
        assert_eq!(envelope,json!({"channel":"ui-kit-view-message","targetView":"details","frameId":FRAME,"message":"private"}));
        assert!(ORIGIN_FRAME.with(|value|value.borrow().is_none()));
    }

    #[test]
    fn optional_storage_does_not_block_tab_navigation_or_reveal() {
        let host=MockHost::install().with_capabilities(&["project.read"]);
        let config=configuration(CONFIG).unwrap();
        GROUP_SELECTIONS.with(|values|values.borrow_mut().remove(&config.group_key(config.group.as_ref().unwrap())));
        let mut demo=GroupDemo::default();
        assert_eq!(active_view("routes",CONFIG).unwrap(),"routes");
        let reply=request(&host,&mut demo,json!({"op":"select-view","target":"details","targetView":"routes"}));
        assert_eq!(reply["model"]["activeView"],"details");
        reveal_view("routes",CONFIG).unwrap();
        assert_eq!(active_view("details",CONFIG).unwrap(),"routes");
        assert_eq!(host.revealed_views(),["routes"]);
        GROUP_SELECTIONS.with(|values|values.borrow_mut().clear());
    }

    #[test]
    fn failed_selection_persistence_does_not_revert_to_the_old_saved_tab() {
        let _host=MockHost::install();let config=configuration(CONFIG).unwrap();
        let key=config.group_key(config.group.as_ref().unwrap());
        host::storage::set(&key,"routes").unwrap();
        // MockHost grants reads/writes together; represent a failed write after
        // its old persisted value was observed, as a real storage failure does.
        GROUP_SELECTIONS.with(|values|values.borrow_mut().insert(key.clone(),GroupSelection {
            selected:"details".into(), observed:Some("routes".into()), pending:true,
        }));
        assert_eq!(active_view("routes",CONFIG).unwrap(),"details");
        host::storage::delete(&key).unwrap();
        assert_eq!(active_view("routes",CONFIG).unwrap(),"routes");
        GROUP_SELECTIONS.with(|values|values.borrow_mut().remove(&key));
    }

    #[test]
    fn native_views_and_host_content_controls_keep_their_existing_transport() {
        let host=MockHost::install();
        reveal_view("native-services",CONFIG).unwrap();refresh_view("native-services",CONFIG);
        post_view_message("native-services","native",CONFIG).unwrap();
        assert_eq!(host.revealed_views(),["native-services"]);
        assert_eq!(host.refreshed_views(),["native-services"]);
        assert_eq!(host.view_messages(),[("native-services".into(),"native".into())]);
        for channel in ["gitlab-content-open","gitlab-content-reply"] {
            let message=json!({"channel":channel,"frameId":FRAME,"payload":{"op":"content-tab-reply"}}).to_string();
            post_view_message("details",&message,CONFIG).unwrap();
            assert_eq!(host.view_messages().last().unwrap(), &("routes".into(),message));
        }
        post_view_message("details","unscoped external reply",CONFIG).unwrap();
        let envelope:Value=serde_json::from_str(&host.view_messages().last().unwrap().1).unwrap();
        assert_eq!(envelope["targetView"],"details");assert!(envelope["frameId"].is_null());
    }
    #[test]
    fn browser_locale_hint_requires_auto_and_a_correlated_frame_without_changing_settings() {
        let host=MockHost::install();
        let mut config=configuration(r#"{"title":"LocaleHintTest","views":{"routes":"Routes"},"actions":[],"uiLanguage":"auto"}"#).unwrap();
        record_language(&config,None,Some("ja"));assert!(resolved_language("LocaleHintTest").is_none());
        record_language(&config,Some("invalid"),Some("ja"));assert!(resolved_language("LocaleHintTest").is_none());
        record_language(&config,Some("0123456789abcdef0123456789abcdef"),Some("ja"));assert_eq!(resolved_language("LocaleHintTest").as_deref(),Some("ja"));
        assert!(host::setting_string("uiLanguage").is_none());
        config.ui_language="ko".into();record_language(&config,Some("0123456789abcdef0123456789abcdef"),Some("zh-CN"));assert_eq!(resolved_language("LocaleHintTest").as_deref(),Some("ja"));
        drop(host);
    }

}
