//! Cache-only route editor adornments. Stale or ambiguous source facts are withheld.
use crate::{Index, php, routes};
use openidea_plugin::inlay_hints::InlayHint;
use openidea_plugin::language_features::CodeLens;
use openidea_plugin::{Position, Range, providers::Document, text};

#[derive(Clone)]
struct Label {
    position: Position,
    label: String,
    tooltip: String,
    target: String,
}
fn inside(position: Position, range: Range) -> bool {
    (position.line, position.character) >= (range.start.line, range.start.character)
        && (position.line, position.character) <= (range.end.line, range.end.character)
}
fn token(doc: &Document, at: Position, width: u32) -> Option<&str> {
    let start = text::offset(&doc.text, at)?;
    let end = text::offset(
        &doc.text,
        Position::new(at.line, at.character.checked_add(width)?),
    )?;
    doc.text.get(start..end)
}
fn blade_labels(index: &Index, doc: &Document, owns: &impl Fn(&str) -> bool) -> Vec<Label> {
    let references = crate::refs::references("blade", &doc.text);
    let Some(bindings) =
        crate::config_inspections::prepare(index, "blade", &doc.text, references.len())
    else {
        return vec![];
    };
    if !crate::view_rename::binding_budget(doc.text.len(), references.len()) {
        return vec![];
    }
    let mut labels = Vec::new();
    for reference in references
        .into_iter()
        .filter(|r| r.closed && r.kind == crate::refs::Kind::Route)
    {
        if !bindings.safe(index, &reference) {
            continue;
        }
        let Some(name) = doc.text.get(reference.start..reference.end) else {
            continue;
        };
        let entries = index
            .route_defs
            .iter()
            .filter(|entry| entry.def.name.as_deref() == Some(name) && owns(&entry.path))
            .collect::<Vec<_>>();
        if entries.len() != 1 {
            continue;
        }
        let entry = entries[0];
        let uri = entry.endpoint_uri.as_deref().unwrap_or(&entry.def.uri);
        labels.push(Label {
            position: text::position(&doc.text, reference.end + 1),
            label: format!("[{}] {uri}", entry.def.methods),
            tooltip: crate::ui_text::format(
                "routeEditor.declarationTooltip",
                "{action}\n{path}\nGo to this route declaration.",
                &[("action", &entry.def.action), ("path", &entry.path)],
            ),
            target: route_target(entry),
        });
    }
    labels.truncate(128);
    labels
}
fn labels(index: &Index, doc: &Document, owns: &impl Fn(&str) -> bool) -> Vec<Label> {
    if index.partial || !owns(&doc.path) || doc.text.len() > 256 * 1024 {
        return Vec::new();
    }
    if doc.language == "blade" || doc.path.ends_with(".blade.php") {
        return blade_labels(index, doc, owns);
    }
    if doc.language != "php" {
        return vec![];
    }
    let parsed = index.parse_routes(&doc.path,&doc.text);
    let mut labels = Vec::new();
    for current in parsed {
        let matching: Vec<_> = index
            .route_defs
            .iter()
            .filter(|entry| {
                entry.path == doc.path
                    && entry.def.methods == current.methods
                    && entry.def.uri == current.uri
                    && entry.def.name == current.name
                    && entry.def.action == current.action
            })
            .collect();
        if matching.len() != 1 {
            continue;
        }
        let entry = matching[0];
        let position = text::position(&doc.text, current.offset + current.width);
        let uri = entry.endpoint_uri.as_deref().unwrap_or(&current.uri);
        let label = format!(
            "[{}] {}{}",
            current.methods,
            uri,
            current
                .name
                .as_ref()
                .map(|name| format!(" · {name}"))
                .unwrap_or_default()
        );
        labels.push(Label {
            position,
            label,
            target: route_target(entry),
            tooltip: crate::ui_text::format(
                "routeEditor.declarationTooltip",
                "{action}\n{path}\nGo to this route declaration.",
                &[("action", &entry.def.action), ("path", &entry.path)],
            ),
        });
    }
    // Match both the current class token and exact current method token, refusing
    // short-name ambiguity rather than attaching another controller's routes.
    let masked = php::mask_comments(&doc.text);
    let classes: Vec<_> = index
        .controllers
        .iter()
        .filter(|(_, entry)| {
            entry.path == doc.path
                && owns(&entry.path)
                && text::offset(&doc.text, entry.at)
                    .is_some_and(|at| masked[..at].trim_end().ends_with("class"))
                && token(doc, entry.at, entry.width).is_some_and(|name| {
                    !name.is_empty()
                        && index.controllers.iter().any(|(full, candidate)| {
                            candidate.path == entry.path && full.rsplit('\\').next() == Some(name)
                        })
                })
        })
        .collect();
    let identities=crate::eloquent_members::class_inventory(&index.eloquent_models);
    if classes.len() == 1 {
        let (class, _) = classes[0];
        if let Some(methods) = index.methods.get(class) {
            for (method, entry) in methods {
                if entry.path != doc.path
                    || token(doc, entry.at, entry.width) != Some(method.as_str())
                {
                    continue;
                }
                let Some(at) = text::offset(&doc.text, entry.at) else {
                    continue;
                };
                if !masked[..at].trim_end().ends_with("function") {
                    continue;
                }
                for route in &index.route_defs {
                    let Some((target, action)) = &route.def.controller else {
                        continue;
                    };
                    if !action.eq_ignore_ascii_case(method) || !owns(&route.path) {
                        continue;
                    }
                    let target=index.route_controller_full(route).unwrap_or(target);
                    let candidates: Vec<_> = index
                        .controllers
                        .keys()
                        .filter(|known| {
                            if target.contains('\\') {
                                index.controllers.get(*known).is_some_and(|entry|identities.iter().any(|identity|!identity.ambiguous&&identity.path==entry.path&&identity.full.eq_ignore_ascii_case(target)))
                            }else{
                                known.as_str() == target || known.rsplit('\\').next() == target.rsplit('\\').next()
                            }
                        })
                        .collect();
                    if candidates.len() != 1 || candidates[0] != class {
                        continue;
                    }
                    labels.push(Label {
                        position: entry.at,
                        target: route_target(route),
                        label: format!(
                            "[{}] {}{}",
                            route.def.methods,
                            route.endpoint_uri.as_deref().unwrap_or(&route.def.uri),
                            route
                                .def
                                .name
                                .as_ref()
                                .map(|n| format!(" · {n}"))
                                .unwrap_or_default()
                        ),
                        tooltip: crate::ui_text::format(
                            "routeEditor.declarationLocationTooltip",
                            "{path}:{line}\nGo to this route declaration.",
                            &[
                                ("path", &route.path),
                                ("line", &(route.at.line + 1).to_string()),
                            ],
                        ),
                    });
                }
            }
        }
    }
    labels.sort_by(|a, b| {
        (a.position.line, a.position.character, &a.label).cmp(&(
            b.position.line,
            b.position.character,
            &b.label,
        ))
    });
    labels.dedup_by(|a, b| a.position == b.position && a.label == b.label);
    labels.truncate(256);
    labels
}
pub(crate) fn hints(
    index: &Index,
    doc: &Document,
    range: Range,
    owns: &impl Fn(&str) -> bool,
) -> Vec<InlayHint> {
    labels(index, doc, owns)
        .into_iter()
        .filter(|label| inside(label.position, range))
        .map(|label| {
            InlayHint::new(label.position, label.label)
                .tooltip(label.tooltip)
                .padding_left()
        })
        .collect()
}
pub(crate) fn lenses(index: &Index, doc: &Document, owns: &impl Fn(&str) -> bool) -> Vec<CodeLens> {
    let mut result = Vec::new();
    for label in labels(index, doc, owns) {
        let range = Range::new(label.position, label.position);
        result.push(CodeLens {
            range,
            title: label.label,
            command: "route-editor-open".into(),
            argument: Some(label.target.clone()),
        });
        if let Some(entry) = index
            .route_defs
            .iter()
            .find(|entry| route_target(entry) == label.target && entry.endpoint_uri.is_some())
        {
            for (action, title) in [
                (
                    "browse",
                    crate::ui_text::text("routeEditor.browse", "Browse"),
                ),
                (
                    "copy",
                    crate::ui_text::text("routeEditor.copyUrl", "Copy URL"),
                ),
                (
                    "curl",
                    crate::ui_text::text("routeEditor.copyCurl", "Copy as cURL"),
                ),
                (
                    "http",
                    crate::ui_text::text("routeEditor.createHttp", "Create HTTP Request"),
                ),
            ] {
                if action == "browse" && !entry.def.methods.split('|').any(|method| method == "GET")
                {
                    continue;
                }
                result.push(CodeLens {
                    range,
                    title: title.into(),
                    command: "route-editor-action".into(),
                    argument: Some(action_target(&label.target, action)),
                });
            }
        }
    }
    result
}
fn route_target(entry: &crate::index::RouteEntry) -> String {
    serde_json::json!({"path":entry.path,"methods":entry.def.methods,"uri":entry.def.uri,"name":entry.def.name,"action":entry.def.action}).to_string()
}
/// Revalidate a code-lens target against current owned source before opening it.
fn resolve(
    index: &Index,
    ctx: &openidea_plugin::Invocation,
    raw: &str,
) -> Result<(openidea_plugin::Project, crate::index::RouteEntry, Range), openidea_plugin::Error> {
    use openidea_plugin::{Error, host};
    if raw.len() > 8192 || index.partial {
        return Err(Error::Stale);
    }
    let value: serde_json::Value = serde_json::from_str(raw).map_err(|_| Error::Stale)?;
    let path = value["path"].as_str().ok_or(Error::Stale)?;
    let project = ctx.project().ok_or(Error::Stale)?;
    let root = project.root()?;
    let owns = |path: &str| {
        host::project_of(path)
            .and_then(|p| p.root().ok())
            .as_deref()
            == Some(root.as_str())
    };
    if !owns(path)
        || !index
            .route_defs
            .iter()
            .any(|entry| route_target(entry) == raw)
    {
        return Err(Error::Stale);
    }
    // An explicit action must not reuse a changed provider/bootstrap mount.
    let mount_source=|relative:&str|->Result<String,Error>{
        if !owns(&format!("{root}/{relative}")){return Err(Error::Stale);}
        let source=project.read_file(relative).unwrap_or_default();
        if source.len()>512*1024{return Err(Error::Stale);}Ok(source)
    };
    let current_mounts=routes::application_mount_contexts(&mount_source("bootstrap/app.php")?,&mount_source("app/Providers/RouteServiceProvider.php")?);
    let relative=path.strip_prefix(&root).and_then(|p|p.strip_prefix('/')).ok_or(Error::Stale)?;
    if current_mounts.as_ref().and_then(|mounts|mounts.get(relative))!=index.route_mounts.get(path){return Err(Error::Stale);}
    let source = project.read_file(path)?;
    if source.len() > 256 * 1024 {
        return Err(Error::Stale);
    }
    let candidates: Vec<_> = index.parse_routes(path,&source)
        .into_iter()
        .filter(|route| {
            value["methods"].as_str() == Some(route.methods.as_str())
                && value["uri"].as_str() == Some(route.uri.as_str())
                && value["name"].as_str() == route.name.as_deref()
                && value["action"].as_str() == Some(route.action.as_str())
        })
        .collect();
    if candidates.len() != 1 || !owns(path) {
        return Err(Error::Stale);
    }
    let route = &candidates[0];
    let entry = index
        .route_defs
        .iter()
        .find(|entry| route_target(entry) == raw)
        .ok_or(Error::Stale)?
        .clone();
    Ok((
        project,
        entry,
        Range::new(
            text::position(&source, route.offset),
            text::position(&source, route.offset + route.width),
        ),
    ))
}
pub(crate) fn open(
    index: &Index,
    ctx: &openidea_plugin::Invocation,
    raw: &str,
) -> Result<(), openidea_plugin::Error> {
    let (_, entry, range) = resolve(index, ctx, raw)?;
    openidea_plugin::host::open_editor(&entry.path, Some(range))
}
fn action_target(raw: &str, action: &str) -> String {
    serde_json::json!({"target":serde_json::from_str::<serde_json::Value>(raw).unwrap_or_default(),"action":action}).to_string()
}
fn valid_base(base: &str) -> bool {
    let Some(rest) = base
        .strip_prefix("http://")
        .or_else(|| base.strip_prefix("https://"))
    else {
        return false;
    };
    !rest.is_empty()
        && !rest.starts_with('/')
        && !rest.contains('@')
        && !base.contains(['?', '#'])
        && !base.chars().any(|c| c.is_whitespace() || c.is_control())
}
fn valid_path(path: &str) -> bool {
    path.starts_with('/')
        && !path.starts_with("//")
        && !path.contains(['{', '}', '#'])
        && !path.chars().any(|c| c.is_whitespace() || c.is_control())
}
/// All process-free request actions revalidate again after the native prompts.
pub(crate) fn action(
    index: &Index,
    ctx: &openidea_plugin::Invocation,
    raw: &str,
    default_base: &str,
) -> Result<(), openidea_plugin::Error> {
    use openidea_plugin::{Error, PickItem, host};
    if raw.len() > 16384 {
        return Err(Error::Stale);
    }
    let value: serde_json::Value = serde_json::from_str(raw).map_err(|_| Error::Stale)?;
    let target = value.get("target").ok_or(Error::Stale)?.to_string();
    let (_, entry, _) = resolve(index, ctx, &target)?;
    let action = value["action"].as_str().ok_or(Error::Stale)?.to_owned();
    if !matches!(action.as_str(), "browse" | "copy" | "curl" | "http") {
        return Err(Error::Stale);
    }
    let mut path = entry.endpoint_uri.clone().ok_or_else(|| {
        Error::Invalid(crate::ui_text::t(
            "routeEditor.unknownMount",
            "The route mount is unknown; refresh routing metadata first.",
        ))
    })?;
    let methods: Vec<_> = entry
        .def
        .methods
        .split('|')
        .filter(|m| {
            matches!(
                *m,
                "GET"
                    | "HEAD"
                    | "POST"
                    | "PUT"
                    | "PATCH"
                    | "DELETE"
                    | "OPTIONS"
                    | "TRACE"
                    | "CONNECT"
            )
        })
        .collect();
    if methods.is_empty() {
        return Err(Error::Invalid(crate::ui_text::t(
            "routeEditor.concreteMethod",
            "Choose a route with a concrete HTTP method.",
        )));
    }
    let method = if action == "browse" {
        if !methods.contains(&"GET") {
            return Err(Error::Invalid(crate::ui_text::t(
                "routeEditor.browseRequiresGet",
                "Browse requires a GET route.",
            )));
        }
        "GET".to_owned()
    } else if matches!(action.as_str(), "curl" | "http") && methods.len() > 1 {
        let choices: Vec<_> = methods
            .iter()
            .map(|method| PickItem::new(method, method))
            .collect();
        let Some(method) = host::pick(
            crate::ui_text::text("routeEditor.method", "HTTP method"),
            &choices,
        )?
        else {
            return Ok(());
        };
        if !methods.contains(&method.as_str()) {
            return Err(Error::Stale);
        }
        method
    } else {
        methods[0].to_owned()
    };
    let Some(base) = host::input(
        crate::ui_text::text("routeEditor.baseUrl", "Base URL"),
        Some(default_base),
    )?
    else {
        return Ok(());
    };
    let base = base.trim();
    if !valid_base(base) {
        return Err(Error::Invalid(crate::ui_text::t(
            "routeEditor.invalidBaseUrl",
            "Enter an HTTP or HTTPS base URL without credentials, query or fragment.",
        )));
    }
    if path.contains(['{', '}']) {
        let Some(value) = host::input(
            crate::ui_text::text("routeEditor.pathPrompt", "Route path (replace parameters)"),
            Some(&path),
        )?
        else {
            return Ok(());
        };
        path = value;
    }
    if !valid_path(&path) {
        return Err(Error::Invalid(crate::ui_text::t(
            "routeEditor.invalidPath",
            "Enter a project route path with all parameters replaced.",
        )));
    }
    let (project, current, _) = resolve(index, ctx, &target)?;
    if current.endpoint_uri != entry.endpoint_uri {
        return Err(Error::Stale);
    }
    let url = format!("{}{}", base.trim_end_matches('/'), path);
    match action.as_str() {
        "browse" => host::open_url(&url),
        "copy" => host::copy_to_clipboard(&url),
        "curl" => host::copy_to_clipboard(&ui_kit::routes::curl(&method, base, &path)),
        "http" => {
            let slug: String = path
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() {
                        c.to_ascii_lowercase()
                    } else {
                        '-'
                    }
                })
                .collect();
            let slug = slug
                .split('-')
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join("-");
            let file = format!(
                "http/{}-{}.http",
                method.to_ascii_lowercase(),
                if slug.is_empty() { "root" } else { &slug }
            );
            let absolute = format!("{}/{}", project.root()?.trim_end_matches('/'), file);
            if host::project_of(&absolute).and_then(|p| p.root().ok()) != Some(project.root()?) {
                return Err(Error::Stale);
            }
            if project.exists(&file)? {
                return host::open_editor(&absolute, None);
            }
            let request = ui_kit::routes::http_request(&method, base, &path);
            match project.write_file(&file, &request) {
                Ok(()) => host::open_editor(&absolute, None),
                Err(Error::Denied(_)) => {
                    host::copy_to_clipboard(&request)?;
                    host::notify(
                        openidea_plugin::NotificationLevel::Warning,
                        "Laravel",
                        crate::ui_text::text(
                            "routeEditor.writeDenied",
                            "Project writes denied; HTTP request copied to clipboard.",
                        ),
                    );
                    Ok(())
                }
                Err(error) => Err(error),
            }
        }
        _ => Err(Error::Stale),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::RouteEntry;
    #[test]
    fn mounted_namespaces_names_and_uris_reparse_original_source_for_labels_and_navigation(){
        let host=openidea_plugin::mock::MockHost::install();
        host.add_file("composer.json",r#"{"require":{"laravel/framework":"^13.0"},"autoload":{"psr-4":{"App\\":"app/"}}}"#);
        host.add_file("artisan","<?php");
        host.add_file("app/Providers/RouteServiceProvider.php",r#"<?php use Illuminate\Support\Facades\Route as Router; Router::prefix('v2')->name('admin.')->namespace('App\Http\Controllers\Admin')->group(base_path('custom/admin.php'));"#);
        host.add_file("app/Http/Controllers/Admin/PostController.php",r#"<?php namespace App\Http\Controllers\Admin; class PostController { public function show(){} }"#);
        let source="<?php /* 😀 */ Route::get('/posts','PostController@show')->name('posts.show');";
        host.add_file("custom/admin.php",source);let project=host.invocation().project().unwrap();let index=Index::build(&project);
        let entry=index.route_defs.iter().find(|entry|entry.path.ends_with("/custom/admin.php")).unwrap_or_else(||panic!("mounts {:?}; files {:?}",routes::application_mount_contexts("",&project.read_file("app/Providers/RouteServiceProvider.php").unwrap()),index.route_files));
        assert_eq!(entry.def.name.as_deref(),Some("admin.posts.show"));assert_eq!(entry.def.uri,"/v2/posts");
        assert_eq!(entry.endpoint_uri.as_deref(),Some("/v2/posts"));assert_eq!(entry.def.action,"App\\Http\\Controllers\\Admin\\PostController@show");
        let doc=Document{path:"/project/custom/admin.php".into(),language:"php".into(),version:2,text:source.into()};
        let labels=labels(&index,&doc,&|_|true);assert_eq!(labels.len(),1);assert!(labels[0].label.contains("admin.posts.show"));
        assert_eq!(labels[0].position,text::position(source,source.find("posts.show'").unwrap()+"posts.show".len()));
        assert!(crate::route_inspections::current(&index,&doc).is_empty());
        let target=route_target(entry);let(_,_,range)=resolve(&index,&host.invocation(),&target).unwrap();
        assert_eq!(&source[text::offset(source,range.start).unwrap()..text::offset(source,range.end).unwrap()],"posts.show");
        host.add_file("app/Providers/RouteServiceProvider.php",r#"<?php Route::prefix('v3')->name('changed.')->namespace('App\Http\Controllers\Admin')->group(base_path('custom/admin.php'));"#);
        assert!(matches!(resolve(&index,&host.invocation(),&target),Err(openidea_plugin::Error::Stale)));
    }
    #[test]
    fn aliased_php_controller_types_keep_exact_method_hints_and_runtime_route_locations(){
        let host=openidea_plugin::mock::MockHost::install();
        host.add_file("composer.json",r#"{"require":{"laravel/framework":"^13.0"},"autoload":{"psr-4":{"App\\":"app/"}}}"#);host.add_file("artisan","<?php");
        let controller="<?php namespace App\\Http\\Controllers\\Admin; class PostController { public function SHOW(){} }";
        host.add_file("app/Http/Controllers/Admin/PostController.php",controller);
        host.add_file("app/Http/Controllers/Public/PostController.php","<?php namespace App\\Http\\Controllers\\Public; class PostController { public function show(){} }");
        let source=r#"<?php use App\Http\Controllers\Admin\PostController as Posts; Route::get('/posts',[Posts::class,'show'])->name('aliased');"#;
        host.add_file("routes/web.php",source);let index=Index::build(&host.invocation().project().unwrap());
        let current=Document{path:"/project/app/Http/Controllers/Admin/PostController.php".into(),language:"php".into(),version:1,text:controller.into()};
        let hints=labels(&index,&current,&|_|true);assert_eq!(hints.len(),1);assert!(hints[0].label.contains("aliased"));
        let other=Document{path:"/project/app/Http/Controllers/Public/PostController.php".into(),text:host.invocation().project().unwrap().read_file("app/Http/Controllers/Public/PostController.php").unwrap(),..current};
        assert!(labels(&index,&other,&|_|true).is_empty());
        let json=serde_json::json!([{"method":"GET|HEAD","uri":"posts","action":"App\\Http\\Controllers\\Admin\\PostController@show","name":"aliased"}]);
        let runtime=crate::route_runtime::parse(&json.to_string(),&index).unwrap();
        assert_eq!(runtime[0].location.as_ref().unwrap().path,"/project/routes/web.php");
        assert_eq!(runtime[0].location.as_ref().unwrap().range.start,text::position(source,source.find("aliased'").unwrap()));
    }
    fn fixture() -> (Index, Document) {
        let source = "<?php\nRoute::get('/hello', fn () => 'hi')->name('hello');\n";
        let def = routes::parse(source).remove(0);
        let mut index = Index::default();
        index.route_defs.push(RouteEntry {
            at: text::position(source, def.offset),
            width: def.width as u32,
            def,
            endpoint_uri: Some("/api/hello".into()),
            path: "/p/routes/web.php".into(),
        });
        (
            index,
            Document {
                path: "/p/routes/web.php".into(),
                language: "php".into(),
                text: source.into(),
                version: 1,
            },
        )
    }
    #[test]
    fn labels_current_routes_and_filters_requested_range() {
        let _host = openidea_plugin::mock::MockHost::install();
        let (index, doc) = fixture();
        let all = Range::new(Position::new(0, 0), Position::new(20, 0));
        let found = hints(&index, &doc, all, &|_| true);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].label, "[GET|HEAD] /api/hello · hello");
        assert!(
            hints(
                &index,
                &doc,
                Range::new(Position::new(0, 0), Position::new(0, 1)),
                &|_| true
            )
            .is_empty()
        );
        assert_eq!(
            lenses(&index, &doc, &|_| true)[0].command,
            "route-editor-open"
        );
    }
    #[test]
    fn blade_route_references_get_inline_hints_and_reviewed_route_actions() {
        let _host = openidea_plugin::mock::MockHost::install();
        let (index, mut doc) = fixture();
        doc.path = "/p/resources/views/test.blade.php".into();
        doc.language = "blade".into();
        doc.text = "😀 <a href=\"{{ route('hello') }}\">Hello</a>".into();
        let found = labels(&index, &doc, &|_| true);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].label, "[GET|HEAD] /api/hello");
        assert_eq!(
            found[0].position,
            text::position(&doc.text, doc.text.find("hello'").unwrap() + 6)
        );
        assert!(labels(&index, &doc, &|path| !path.contains("/routes/")).is_empty());
        doc.text =
            "{{-- {{ route('hello') }} --}} @verbatim {{ route('hello') }} @endverbatim".into();
        assert!(labels(&index, &doc, &|_| true).is_empty());
        doc.text = "{{ Vendor\\route('hello') }}".into();
        assert!(labels(&index, &doc, &|_| true).is_empty());
    }
    #[test]
    fn stale_foreign_partial_or_ambiguous_facts_are_withheld() {
        let (mut index, mut doc) = fixture();
        assert!(labels(&index, &doc, &|_| false).is_empty());
        index.partial = true;
        assert!(labels(&index, &doc, &|_| true).is_empty());
        index.partial = false;
        let original = doc.text.clone();
        doc.text = doc.text.replace("/hello", "/changed");
        assert!(labels(&index, &doc, &|_| true).is_empty());
        doc.text = original;
        index.route_defs.push(index.route_defs[0].clone());
        assert!(labels(&index, &doc, &|_| true).is_empty());
    }
    #[test]
    fn controller_labels_require_exact_current_class_method_and_unique_target() {
        let _host = openidea_plugin::mock::MockHost::install();
        let (mut index, _) = fixture();
        let source = "<?php\nclass HelloController {\n public function index() {}\n}\n";
        let path = "/p/app/Http/Controllers/HelloController.php";
        let class_at = source.find("HelloController").unwrap();
        let method_at = source.find("index").unwrap();
        let entry = |at, width| crate::index::Entry {
            detail: None,
            path: path.into(),
            at: text::position(source, at),
            width,
        };
        index
            .controllers
            .insert("HelloController".into(), entry(class_at, 15));
        index.methods.insert(
            "HelloController".into(),
            [("index".into(), entry(method_at, 5))].into(),
        );
        index.route_defs[0].def.controller = Some(("HelloController".into(), "index".into()));
        let mut doc = Document {
            path: path.into(),
            language: "php".into(),
            text: source.into(),
            version: 1,
        };
        assert_eq!(labels(&index, &doc, &|_| true).len(), 1);
        doc.text = source.replace("function index", "function other");
        assert!(labels(&index, &doc, &|_| true).is_empty());
        doc.text = source.into();
        index.controllers.insert(
            "Other\\HelloController".into(),
            crate::index::Entry {
                path: "/p/other.php".into(),
                ..entry(class_at, 15)
            },
        );
        assert!(labels(&index, &doc, &|_| true).is_empty());
    }

    #[test]
    fn navigation_revalidates_current_source_and_owning_project() {
        let h = openidea_plugin::mock::MockHost::install();
        h.set_project_root(Some("/p"));
        let (index, doc) = fixture();
        h.add_file("routes/web.php", &doc.text);
        let raw = route_target(&index.route_defs[0]);
        assert!(open(&index, &h.invocation(), &raw).is_ok());
        h.open_editor(
            &doc.path,
            "php",
            &doc.text.replace("/hello", "/changed"),
            &[],
        );
        assert!(open(&index, &h.invocation(), &raw).is_err());
        h.open_editor(&doc.path, "php", &doc.text, &[]);
        h.add_project("/p/routes");
        assert!(open(&index, &h.invocation(), &raw).is_err());
        assert!(open(&index, &h.invocation(), "{}").is_err());
    }
    #[test]
    fn request_actions_copy_exact_url_curl_and_create_http_without_execution() {
        let h = openidea_plugin::mock::MockHost::install().with_capabilities(&[
            "project.read",
            "project.write",
            "editor.read",
            "clipboard",
        ]);
        h.set_project_root(Some("/p"));
        let (index, doc) = fixture();
        h.add_file("routes/web.php", &doc.text);
        let raw = route_target(&index.route_defs[0]);
        h.answer_next_prompt(Some(&["https://example.test"]));
        action(&index, &h.invocation(), &action_target(&raw, "copy"), "").unwrap();
        assert_eq!(
            h.clipboard().as_deref(),
            Some("https://example.test/api/hello")
        );
        h.answer_next_prompt(Some(&["GET"]));
        h.answer_next_prompt(Some(&["https://example.test"]));
        action(&index, &h.invocation(), &action_target(&raw, "curl"), "").unwrap();
        assert_eq!(
            h.clipboard().as_deref(),
            Some("curl -X GET 'https://example.test/api/hello'")
        );
        h.answer_next_prompt(Some(&["https://example.test"]));
        action(&index, &h.invocation(), &action_target(&raw, "browse"), "").unwrap();
        assert_eq!(h.opened_urls(), vec!["https://example.test/api/hello"]);
        h.answer_next_prompt(Some(&["HEAD"]));
        h.answer_next_prompt(Some(&["https://example.test"]));
        action(&index, &h.invocation(), &action_target(&raw, "http"), "").unwrap();
        assert_eq!(
            h.invocation()
                .project()
                .unwrap()
                .read_file("http/head-api-hello.http")
                .unwrap(),
            "### HEAD /api/hello\nHEAD https://example.test/api/hello\n"
        );
    }
    #[test]
    fn unknown_mounts_and_unsafe_urls_do_not_produce_request_actions() {
        let h = openidea_plugin::mock::MockHost::install();
        h.set_project_root(Some("/p"));
        let (mut index, doc) = fixture();
        h.add_file("routes/web.php", &doc.text);
        let raw = route_target(&index.route_defs[0]);
        h.answer_next_prompt(Some(&["javascript:alert(1)"]));
        assert!(action(&index, &h.invocation(), &action_target(&raw, "copy"), "").is_err());
        assert!(h.clipboard().is_none());
        index.route_defs[0].endpoint_uri = None;
        assert_eq!(lenses(&index, &doc, &|_| true).len(), 1);
        assert!(action(&index, &h.invocation(), &action_target(&raw, "copy"), "").is_err());
        assert!(!valid_base("https://user:secret@host"));
        assert!(!valid_base("https://host/\r\nGET"));
        assert!(!valid_path("//evil.test"));
        assert!(!valid_path("/posts/{id}"));
    }
    #[test]
    fn denied_http_write_copies_request_and_post_has_no_browser_lens() {
        let h = openidea_plugin::mock::MockHost::install();
        h.set_project_root(Some("/p"));
        let (mut index, mut doc) = fixture();
        doc.text = doc.text.replace("Route::get", "Route::post");
        index.route_defs[0].def = routes::parse(&doc.text).remove(0);
        h.add_file("routes/web.php", &doc.text);
        let raw = route_target(&index.route_defs[0]);
        assert!(
            !lenses(&index, &doc, &|_| true)
                .iter()
                .any(|lens| lens.title == "Browse")
        );
        h.answer_next_prompt(Some(&["https://example.test"]));
        action(&index, &h.invocation(), &action_target(&raw, "http"), "").unwrap();
        assert_eq!(
            h.clipboard().as_deref(),
            Some("### POST /api/hello\nPOST https://example.test/api/hello\n")
        );
        assert!(
            !h.invocation()
                .project()
                .unwrap()
                .exists("http/post-api-hello.http")
                .unwrap()
        );
        assert!(action(&index, &h.invocation(), &action_target(&raw, "browse"), "").is_err());
    }
}
