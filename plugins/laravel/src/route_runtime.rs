//! Explicit Artisan route capture. Views and editor providers never start PHP.
use crate::{artisan_metadata, index::Index};
use openidea_plugin::{Error, Invocation, Location, Position, Project, Range, host};
use serde_json::{Value, json};
use ui_kit::routes::{Handler, Route};

pub const ARGS: &[&str] = &[
    "artisan",
    "route:list",
    "--json",
    "--no-interaction",
    "--no-ansi",
];
const MAX_OUTPUT: usize = 4 * 1024 * 1024;
pub struct Snapshot {
    pub routes: Vec<Route>,
    pub captured: u64,
    source_stamp: Value,
    methods: Vec<String>,
}
impl Snapshot {
    #[cfg(test)]
    pub(crate) fn fixture(routes: Vec<Route>, captured: u64) -> Self {
        let methods = routes.iter().map(|route| route.method.clone()).collect();
        Self {
            routes,
            captured,
            source_stamp: Value::Null,
            methods,
        }
    }
    pub fn provenance(&self) -> String {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(self.captured, |d| d.as_secs());
        let seconds = now.saturating_sub(self.captured);
        if seconds < 60 {
            crate::ui_text::text("native.dockerRoute.f08ad01ed0","Artisan snapshot · refreshed just now").into()
        } else if seconds < 3600 {
            crate::ui_text::format("native.dockerRoute.fc9395290f","Artisan snapshot · refreshed {minutes} min ago",&[("minutes",&(seconds / 60).to_string())])
        } else {
            crate::ui_text::format("native.dockerRoute.9652806640","Artisan snapshot · refreshed {hours} h ago",&[("hours",&(seconds / 3600).to_string())])
        }
    }
}
fn stamp(project: &Project, index: &Index) -> Result<Value, Error> {
    let mut identity = artisan_metadata::identity(project)?;
    identity["argv"] = json!(ARGS);
    let root = project.root()?;
    if !host::projects()?
        .iter()
        .any(|p| p.root().ok().as_deref() == Some(&root))
    {
        return Err(Error::Stale);
    }
    let mut paths = vec![];
    for glob in [
        "routes/**/*.php",
        "config/**/*.php",
        "app/Providers/**/*.php",
        "bootstrap/**/*.php",
        "**/ide.json",
    ] {
        let found = project.find_files(glob, 1024)?;
        if found.len() >= 1024 {
            return Err(Error::Invalid("Too many route capture inputs.".into()));
        }
        paths.extend(found);
    }
    paths.extend(index.route_defs.iter().map(|entry| entry.path.clone()));
    paths.extend(
        index
            .metadata
            .route_files
            .iter()
            .map(|relative| format!("{root}/{relative}")),
    );
    for name in [
        ".env",
        "app/Http/routes.php",
        "app/routes.php",
        "ide.json",
        ".idea/ide.json",
        "ide-blade.json",
        ".idea/ide-blade.json",
    ] {
        let path = format!("{root}/{name}");
        if project.exists(&path)? {
            paths.push(path);
        }
    }
    paths.sort();
    paths.dedup();
    let mut total = 0;
    let mut hashes = vec![];
    for path in paths {
        if !crate::owns_current_path(project, &path) {
            return Err(Error::Stale);
        }
        let text = project.read_file(&path)?;
        total += text.len();
        if total > 8 * 1024 * 1024 {
            return Err(Error::Invalid("Route capture inputs exceed 8 MiB.".into()));
        }
        hashes.push(json!([path, artisan_metadata::hash(&text)]));
    }
    identity["sources"] = json!(hashes);
    Ok(identity)
}
fn field(value: &Value, key: &str, max: usize) -> Result<String, Error> {
    let s = value[key]
        .as_str()
        .ok_or_else(|| Error::Invalid(format!("Route JSON is missing {key}.")))?;
    if s.len() > max || s.chars().any(char::is_control) {
        return Err(Error::Invalid(format!("Route {key} exceeds text limits.")));
    }
    Ok(s.into())
}
fn source_action_matches(index:&Index,entry:&crate::index::RouteEntry,action:&str)->bool{
    let Some(full)=index.route_controller_full(entry)else{return entry.def.action==action;};
    let Some((_,method))=&entry.def.controller else{return entry.def.action==action;};
    if let Some((class,target))=action.rsplit_once('@'){
        full.trim_start_matches('\\').eq_ignore_ascii_case(class.trim_start_matches('\\'))&&method.eq_ignore_ascii_case(target)
    }else{method.eq_ignore_ascii_case("__invoke")&&full.trim_start_matches('\\').eq_ignore_ascii_case(action.trim_start_matches('\\'))}
}
pub fn parse(raw: &str, index: &Index) -> Result<Vec<Route>, Error> {
    let data = crate::strict_json::parse_with_budget(raw, MAX_OUTPUT, 200_000).map_err(|_| {
        Error::Invalid("Artisan route:list did not return bounded route JSON.".into())
    })?;
    let entries = data
        .as_array()
        .filter(|a| a.len() <= 5000)
        .ok_or("Expected an Artisan JSON array with at most 5,000 routes.")?;
    let mut routes = vec![];
    for entry in entries {
        let methods = field(entry, "method", 128)?;
        if methods.is_empty()
            || methods.split('|').any(|m| {
                ![
                    "GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS", "TRACE", "CONNECT",
                ]
                .contains(&m)
            })
        {
            return Err(Error::Invalid("Unsupported route HTTP method.".into()));
        }
        let uri = field(entry, "uri", 4096)?;
        let path = format!("/{}", uri.trim_start_matches('/'));
        let action = field(entry, "action", 2048)?;
        let namespace = host::setting_string("controllersNamespace")
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "App\\Http\\Controllers".into());
        let class_label = |class: &str| {
            class
                .strip_prefix(&format!("{namespace}\\"))
                .unwrap_or(class)
                .to_owned()
        };
        let (class, member) = if let Some((class, method)) = action.rsplit_once('@') {
            (class_label(class), Some(method.to_owned()))
        } else if action == "Closure" {
            ("Closure".into(), None)
        } else {
            (class_label(&action), Some("__invoke".into()))
        };
        let name = if entry["name"].is_null() {
            None
        } else {
            Some(field(entry, "name", 1024)?)
        };
        // Only cached, owned source locations are used; JSON paths are never trusted.
        let exact: Vec<_> = index
            .route_defs
            .iter()
            .filter(|r| {
                r.endpoint_uri.as_deref().unwrap_or(&r.def.uri) == path
                    && source_action_matches(index,r,&action)
                    && methods
                        .split('|')
                        .any(|m| r.def.methods.split('|').any(|known| known == m))
            })
            .collect();
        let location = if exact.len() == 1 {
            let e = exact[0];
            Some(Location {
                path: e.path.clone(),
                range: Range::new(e.at, Position::new(e.at.line, e.at.character + e.width)),
            })
        } else {
            member
                .as_deref()
                .and_then(|method| {
                    index
                        .methods
                        .get(&class)
                        .and_then(|methods| methods.get(method))
                        .or_else(|| {
                            (method == "__invoke")
                                .then(|| index.controllers.get(&class))
                                .flatten()
                        })
                })
                .map(|e| Location {
                    path: e.path.clone(),
                    range: Range::new(e.at, Position::new(e.at.line, e.at.character + e.width)),
                })
        };
        let method = methods
            .split('|')
            .filter(|m| !(*m == "HEAD" && methods.split('|').any(|v| v == "GET")))
            .collect::<Vec<_>>()
            .join("|");
        routes.push(Route {
            method,
            path,
            handler: Handler { class, member },
            name,
            location,
        });
    }
    Ok(routes)
}
/// Publish captured registrations only while their inputs and owned declaration
/// remain current. This performs bounded source reads, never Artisan execution.
pub fn endpoint_routes(snapshot: &Snapshot, index: &Index, project: &Project) -> Vec<Route> {
    if index.partial
        || snapshot.routes.len() > 5000
        || stamp(project, index).ok().as_ref() != Some(&snapshot.source_stamp)
    {
        return Vec::new();
    }
    let namespace = host::setting_string("controllersNamespace")
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "App\\Http\\Controllers".into());
    let normalize = |class: &str| {
        class
            .trim_start_matches('\\')
            .strip_prefix(&format!("{namespace}\\"))
            .unwrap_or(class.trim_start_matches('\\'))
            .to_owned()
    };
    let handler = |action: &str| {
        if action == "Closure" {
            Handler {
                class: "Closure".into(),
                member: None,
            }
        } else if let Some((class, member)) = action.rsplit_once('@') {
            Handler {
                class: normalize(class),
                member: Some(member.into()),
            }
        } else {
            Handler {
                class: normalize(action),
                member: Some("__invoke".into()),
            }
        }
    };
    let mut sources: std::collections::BTreeMap<String, Option<std::rc::Rc<String>>> =
        std::collections::BTreeMap::new();
    let mut bytes = 0usize;
    let mut read = |path: &str| -> Option<std::rc::Rc<String>> {
        if !crate::owns_current_path(project, path) {
            return None;
        }
        if let Some(cached) = sources.get(path) {
            return cached.clone();
        }
        let value = project.read_file(path).ok().filter(|source| {
            source.len() <= 256 * 1024 && bytes.saturating_add(source.len()) <= 8 * 1024 * 1024
        });
        if let Some(source) = &value {
            bytes += source.len();
        }
        let value = value.map(std::rc::Rc::new);
        sources.insert(path.into(), value.clone());
        value
    };
    let mut published = Vec::new();
    for (ordinal, route) in snapshot.routes.iter().enumerate() {
        let Some(location) = &route.location else {
            continue;
        };
        let static_matches: Vec<_> = index
            .route_defs
            .iter()
            .filter(|entry| {
                entry.path == location.path
                    && entry.at == location.range.start
                    && location.range.end
                        == Position::new(
                            entry.at.line,
                            entry.at.character.saturating_add(entry.width),
                        )
                    && entry.endpoint_uri.as_deref() == Some(route.path.as_str())
                    && handler(&index.route_controller_full(entry).and_then(|class|entry.def.controller.as_ref().map(|(_,method)|format!("{class}@{method}"))).unwrap_or_else(||entry.def.action.clone())) == route.handler
                    && route
                        .method
                        .split('|')
                        .all(|method| entry.def.methods.split('|').any(|known| method == known))
            })
            .collect();
        let static_valid = if static_matches.len() == 1 {
            let entry = static_matches[0];
            read(&entry.path).is_some_and(|source| {
                index.parse_routes(&entry.path,&source)
                    .iter()
                    .filter(|def| **def == entry.def)
                    .count()
                    == 1
            })
        } else {
            false
        };
        let mut controller_valid = false;
        if !static_valid && let Some(member) = &route.handler.member {
            let controllers: Vec<_> = index
                .controllers
                .iter()
                .filter(|(class, _)| normalize(class) == route.handler.class)
                .collect();
            if controllers.len() == 1 {
                let (class, class_entry) = controllers[0];
                if let Some(method) = index
                    .methods
                    .get(class)
                    .and_then(|methods| methods.get(member))
                    .or_else(|| (member == "__invoke").then_some(class_entry))
                {
                    let expected = Location {
                        path: method.path.clone(),
                        range: Range::new(
                            method.at,
                            Position::new(
                                method.at.line,
                                method.at.character.saturating_add(method.width),
                            ),
                        ),
                    };
                    if *location == expected && method.path == class_entry.path {
                        if let Some(source) = read(&method.path) {
                            if let Some(masked) = controller_source(&source) {
                                let expected_namespace = class
                                    .rsplit_once('\\')
                                    .map(|(parent, _)| format!("{namespace}\\{parent}"))
                                    .unwrap_or_else(|| namespace.clone());
                                let namespaces: Vec<_> = masked
                                    .match_indices("namespace ")
                                    .filter_map(|(at, _)| {
                                        masked[at + 10..]
                                            .split_once(';')
                                            .map(|(name, _)| name.trim())
                                    })
                                    .collect();
                                if namespaces == [expected_namespace.as_str()] {
                                    if let Some(current) = crate::controllers::parse(
                                        &format!("{}.php", class.replace('\\', "/")),
                                        &masked,
                                    ) {
                                        let current_class =
                                            text_position(&source, current.class_offset);
                                        controller_valid = current_class == class_entry.at
                                            && if member == "__invoke" {
                                                has_invokable(&masked)
                                            } else {
                                                current.methods.iter().any(|(name, at)| {
                                                    name == member
                                                        && text_position(&source, *at) == method.at
                                                })
                                            };
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        if (static_valid || controller_valid) && crate::owns_current_path(project, &location.path) {
            let mut route = route.clone();
            route.method = snapshot
                .methods
                .get(ordinal)
                .cloned()
                .unwrap_or_else(|| route.method.clone());
            published.push(route);
        }
    }
    published
}
fn has_invokable(masked: &str) -> bool {
    masked.match_indices("function __invoke").any(|(at, _)| {
        let line = masked[..at]
            .rsplit_once('\n')
            .map_or(&masked[..at], |(_, last)| last)
            .trim();
        matches!(line, "public" | "final public" | "public final" | "")
            && masked[at + "function __invoke".len()..]
                .trim_start()
                .starts_with('(')
    })
}

fn controller_source(source: &str) -> Option<String> {
    if !source.trim_start().starts_with("<?php") || source.contains("?>") {
        return None;
    }
    let masked = crate::php::mask_comments(source);
    if masked.contains("<<<") {
        return None;
    }
    let mut bytes = masked.as_bytes().to_vec();
    let mut at = 0;
    while at < bytes.len() {
        if matches!(bytes[at], b'\'' | b'"' | b'`') {
            let end = crate::php::string_end(&bytes, at).min(bytes.len());
            for byte in &mut bytes[at..end] {
                if !matches!(*byte, b'\n' | b'\r') {
                    *byte = b' ';
                }
            }
            at = end;
        } else {
            at += 1;
        }
    }
    String::from_utf8(bytes).ok()
}

fn text_position(source: &str, offset: usize) -> Position {
    openidea_plugin::text::position(source, offset)
}

pub fn capture_with(ctx:&Invocation,index:&Index,execute:impl FnOnce(&Project)->Result<String,Error>)->Result<Snapshot,Error>{capture_with_inner(ctx,index,execute).map_err(ui_error)}
fn capture_with_inner(
    ctx: &Invocation,
    index: &Index,
    execute: impl FnOnce(&Project) -> Result<String, Error>,
) -> Result<Snapshot, Error> {
    let project = ctx.project().ok_or(crate::ui_text::text("process.inspection.text4","Open a Laravel project first."))?;
    let before = stamp(&project, index)?;
    let output = execute(&project)?;
    if ctx.project().and_then(|p| p.root().ok()) != project.root().ok()
        || stamp(&project, index)? != before
    {
        return Err(Error::Stale);
    }
    let routes = parse(&output, index)?;
    // parse validated the bounded JSON and every method; retain HEAD for endpoint publication.
    let data: Value = serde_json::from_str(&output).map_err(|_| Error::Stale)?;
    let methods = data
        .as_array()
        .ok_or(Error::Stale)?
        .iter()
        .map(|entry| entry["method"].as_str().unwrap_or_default().to_owned())
        .collect();
    Ok(Snapshot {
        routes,
        source_stamp: before,
        methods,
        captured: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
    })
}
pub fn capture(ctx: &Invocation, index: &Index) -> Result<Snapshot, Error> {
    capture_with(ctx, index, |project| {
        let task = host::begin_task(
            crate::ui_text::text("native.dockerRoute.d22791bd83","Refresh registered Laravel routes (executes project PHP)"),
            true,
        );
        let child = crate::artisan_runner::start_background(crate::artisan_runner::Runner::Php, &ARGS.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>(), project)?;
        if let Err(error) = child.close_stdin() {
            let _ = child.kill();
            return Err(error);
        }
        let result =
            artisan_metadata::collect(|timeout| child.wait(timeout), || task.is_cancelled());
        if result.is_err() {
            let _ = child.kill();
        }
        result
    })
}


// Exact authored messages are localized only after validation reaches a UI caller.
fn ui_error(error:Error)->Error {
    fn message(message:String)->String{match message.as_str(){
        "Artisan route:list did not return bounded route JSON." => crate::ui_text::t("native.dockerRoute.e6db8fc84c",&message),
        "Expected an Artisan JSON array with at most 5,000 routes." => crate::ui_text::t("native.dockerRoute.df510792af",&message),
        "Route capture inputs exceed 8 MiB." => crate::ui_text::t("native.dockerRoute.94e1b025a1",&message),
        "Too many route capture inputs." => crate::ui_text::t("native.dockerRoute.9a9e4aa438",&message),
        "Unsupported route HTTP method." => crate::ui_text::t("native.dockerRoute.6801016079",&message),
        "Route JSON is missing method." => crate::ui_text::format("native.dockerRoute.2941409fc8","Route JSON is missing {key}.",&[("key",&("method").to_string())]),
        "Route method exceeds text limits." => crate::ui_text::format("native.dockerRoute.27dad91e8f","Route {key} exceeds text limits.",&[("key",&("method").to_string())]),
        "Route JSON is missing uri." => crate::ui_text::format("native.dockerRoute.2941409fc8","Route JSON is missing {key}.",&[("key",&("uri").to_string())]),
        "Route uri exceeds text limits." => crate::ui_text::format("native.dockerRoute.27dad91e8f","Route {key} exceeds text limits.",&[("key",&("uri").to_string())]),
        "Route JSON is missing action." => crate::ui_text::format("native.dockerRoute.2941409fc8","Route JSON is missing {key}.",&[("key",&("action").to_string())]),
        "Route action exceeds text limits." => crate::ui_text::format("native.dockerRoute.27dad91e8f","Route {key} exceeds text limits.",&[("key",&("action").to_string())]),
        "Route JSON is missing name." => crate::ui_text::format("native.dockerRoute.2941409fc8","Route JSON is missing {key}.",&[("key",&("name").to_string())]),
        "Route name exceeds text limits." => crate::ui_text::format("native.dockerRoute.27dad91e8f","Route {key} exceeds text limits.",&[("key",&("name").to_string())]),
        _=>message,
    }}
    match error{Error::Invalid(value)=>Error::Invalid(message(value)),Error::Failed(value)=>Error::Failed(message(value)),_=>error}
}
#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    fn setup() -> MockHost {
        let host = MockHost::install();
        host.add_file("artisan", "<?php");
        host.add_file("composer.json", "{}");
        host
    }
    fn sample() -> String {
        json!([{"method":"GET|HEAD","uri":"api/pregnancy/contractions","name":null,"action":"App\\Http\\Controllers\\Api\\ContractionController@index","middleware":["api"],"path":"/outside/untrusted.php"},{"method":"PUT|PATCH","uri":"api/pregnancy/contractions/{id}","name":"api.update","action":"App\\Http\\Controllers\\Api\\ContractionController@update"}]).to_string()
    }
    #[test]
    fn runtime_routes_preserve_real_paths_controller_names_and_all_methods() {
        let _host = setup();
        let routes = parse(&sample(), &Index::default()).unwrap();
        assert_eq!(routes.len(), 2);
        assert_eq!(routes[0].path, "/api/pregnancy/contractions");
        assert_eq!(routes[0].handler.class, "Api\\ContractionController");
        assert_eq!(routes[0].handler.member.as_deref(), Some("index"));
        assert_eq!(routes[0].method, "GET");
        assert_eq!(routes[0].location, None);
        assert_eq!(routes[1].method, "PUT|PATCH");
        assert_eq!(routes[1].name.as_deref(), Some("api.update"));
    }
    #[test]
    fn capture_refuses_changed_sources_and_malformed_json_without_running_on_read() {
        let host = setup();
        let index = Index::default();
        let ctx = host.invocation();
        let snapshot = capture_with(&ctx, &index, |project| {
            assert_eq!(project.root().unwrap(), "/project");
            Ok(sample())
        })
        .unwrap();
        assert_eq!(snapshot.routes.len(), 2);
        assert!(matches!(
            capture_with(&ctx, &index, |_| {
                host.add_file("routes/web.php", "<?php changed");
                Ok(sample())
            }),
            Err(Error::Stale)
        ));
        assert!(capture_with(&ctx, &index, |_| Ok("not JSON".into())).is_err());
        assert!(host.process_runs().is_empty());
    }
    #[test]
    fn denied_php_and_invalid_methods_are_errors() {
        let host = MockHost::install().with_capabilities(&["project.read"]);
        host.add_file("artisan", "<?php");
        assert!(matches!(
            capture(&host.invocation(), &Index::default()),
            Err(Error::Invalid(message)) if message.contains("process:php permission")
        ));
        assert!(parse(&sample().replace("GET|HEAD", "SHELL"), &Index::default()).is_err());
    }
    #[test]
    #[ignore = "requires LARAVEL_ROUTE_LIST_FIXTURE captured with artisan route:list --json"]
    fn captured_runtime_route_list_is_complete() {
        let _host = setup();
        let raw =
            std::fs::read_to_string(std::env::var("LARAVEL_ROUTE_LIST_FIXTURE").unwrap()).unwrap();
        let expected = serde_json::from_str::<Value>(&raw)
            .unwrap()
            .as_array()
            .unwrap()
            .len();
        let routes = parse(&raw, &Index::default()).unwrap();
        assert_eq!(routes.len(), expected);
        assert!(
            !routes
                .iter()
                .any(|r| r.path == "/?" || r.handler.class.contains("$controller"))
        );
        println!(
            "Parsed {expected} registered runtime routes without placeholder paths or variable controller labels"
        );
    }
    fn dynamic_fixture() -> (MockHost, Index, Snapshot) {
        use crate::index::AddName;
        let host = setup();
        let source = "<?php\nnamespace App\\Http\\Controllers\\Api;\nclass ContractionController {\n public function index() {}\n public function update() {}\n}\n";
        let path = "/project/app/Http/Controllers/Api/ContractionController.php";
        host.add_file(path, source);
        let class = crate::controllers::parse("Api/ContractionController.php", source).unwrap();
        let mut index = Index::default();
        index.controllers.add(
            &class.class,
            None,
            path,
            openidea_plugin::text::position(source, class.class_offset),
            "ContractionController".len(),
        );
        let mut methods = crate::index::Names::new();
        for (method, offset) in &class.methods {
            methods.add(
                method,
                None,
                path,
                openidea_plugin::text::position(source, *offset),
                method.len(),
            );
        }
        index.methods.insert(class.class, methods);
        let snapshot = capture_with(&host.invocation(), &index, |_| Ok(sample())).unwrap();
        (host, index, snapshot)
    }
    #[test]
    fn runtime_endpoints_publish_dynamic_registrations_with_proven_controller_source() {
        let (host, index, snapshot) = dynamic_fixture();
        let routes = endpoint_routes(&snapshot, &index, &host.invocation().project().unwrap());
        assert_eq!(routes.len(), 2);
        let endpoints = ui_kit::routes::endpoints(&routes, "Laravel");
        assert_eq!(endpoints.len(), 4);
        assert_eq!(endpoints[0].path, "/api/pregnancy/contractions");
        assert!(endpoints.iter().all(|endpoint| {
            endpoint
                .location
                .path
                .starts_with("/project/app/Http/Controllers/")
        }));
        assert!(host.process_runs().is_empty());
    }
    #[test]
    fn runtime_endpoints_withhold_changed_inputs_methods_namespaces_and_foreign_sources() {
        let (host, mut index, snapshot) = dynamic_fixture();
        let project = host.invocation().project().unwrap();
        let path = "/project/app/Http/Controllers/Api/ContractionController.php";
        let original = project.read_file(path).unwrap();
        host.open_editor(
            path,
            "php",
            &original.replace("function index", "function other"),
            &[],
        );
        assert_eq!(endpoint_routes(&snapshot, &index, &project).len(), 1);
        host.open_editor(
            path,
            "php",
            &original.replace("Controllers\\Api;", "Controllers\\Other;"),
            &[],
        );
        assert!(endpoint_routes(&snapshot, &index, &project).is_empty());
        host.open_editor(path, "php", &original, &[]);
        index.partial = true;
        assert!(endpoint_routes(&snapshot, &index, &project).is_empty());
        index.partial = false;
        host.add_file("config/changed.php", "<?php return [];");
        assert!(endpoint_routes(&snapshot, &index, &project).is_empty());
        assert!(host.process_runs().is_empty());
        let (host, index, snapshot) = dynamic_fixture();
        host.add_project("/project/app/Http");
        assert!(
            endpoint_routes(&snapshot, &index, &host.invocation().project().unwrap()).is_empty()
        );
    }
    #[test]
    fn runtime_endpoints_reject_unproven_snapshot_locations_and_ambiguous_controllers() {
        let (host, mut index, mut snapshot) = dynamic_fixture();
        snapshot.routes[0].location.as_mut().unwrap().path = "/untrusted/json.php".into();
        let project = host.invocation().project().unwrap();
        assert_eq!(endpoint_routes(&snapshot, &index, &project).len(), 1);
        let entry = index.controllers["Api\\ContractionController"].clone();
        index.controllers.insert(
            "App\\Http\\Controllers\\Api\\ContractionController".into(),
            entry,
        );
        assert!(endpoint_routes(&snapshot, &index, &project).is_empty());
    }
    #[test]
    fn static_runtime_endpoint_keeps_current_route_location_and_omits_vendor_closures() {
        let host = setup();
        let source = "<?php\nRoute::get('/hello', fn()=> 'hi')->name('hello');\n";
        let path = "/project/routes/web.php";
        host.add_file(path, source);
        let def = crate::routes::parse(source).remove(0);
        let mut index = Index::default();
        index.route_defs.push(crate::index::RouteEntry {
            at: openidea_plugin::text::position(source, def.offset),
            width: def.width as u32,
            def,
            path: path.into(),
            endpoint_uri: Some("/hello".into()),
        });
        let raw=json!([{"method":"GET|HEAD","uri":"hello","name":"hello","action":"Closure"},{"method":"GET","uri":"vendor","name":null,"action":"Closure","path":"/outside/route.php"}]).to_string();
        let snapshot = capture_with(&host.invocation(), &index, |_| Ok(raw)).unwrap();
        let project = host.invocation().project().unwrap();
        let published = endpoint_routes(&snapshot, &index, &project);
        assert_eq!(published.len(), 1);
        assert_eq!(published[0].method, "GET|HEAD");
        assert_eq!(published[0].location.as_ref().unwrap().path, path);
        host.open_editor(path, "php", &source.replace("/hello", "/changed"), &[]);
        assert!(endpoint_routes(&snapshot, &index, &project).is_empty());
    }
    #[test]
    fn runtime_invokable_controller_requires_current_public_invoke_declaration() {
        let (host, index, _) = dynamic_fixture();
        let source = "<?php\nnamespace App\\Http\\Controllers\\Api;\nclass ContractionController {\n public function __invoke() {}\n}\n";
        let path = "/project/app/Http/Controllers/Api/ContractionController.php";
        host.add_file(path, source);
        let raw=json!([{"method":"GET","uri":"runtime-only","name":null,"action":"App\\Http\\Controllers\\Api\\ContractionController"}]).to_string();
        let snapshot = capture_with(&host.invocation(), &index, |_| Ok(raw)).unwrap();
        let project = host.invocation().project().unwrap();
        let routes = endpoint_routes(&snapshot, &index, &project);
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].handler.member.as_deref(), Some("__invoke"));
        host.open_editor(
            path,
            "php",
            &source.replace("public function", "private function"),
            &[],
        );
        assert!(endpoint_routes(&snapshot, &index, &project).is_empty());
    }
}
