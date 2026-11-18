//! Current-source endpoint actions; lens arguments are revalidated at invocation.
use openidea_plugin::language_features::{CodeLens, Document};
use openidea_plugin::{Error, Invocation};
use ui_kit::routes::{Handler, Route};

pub const COMMAND: &str = "create-source-endpoint-http";
pub fn lenses(doc: &Document) -> Vec<CodeLens> {
    let Some(info) = crate::endpoints::file_info(&doc.path, &doc.text) else {
        return Vec::new();
    };
    info.endpoints.into_iter().filter(|e|crate::safe_request_path(&e.path)).take(256).map(|e|{
        let method=if e.verb.is_empty(){"ANY"}else{&e.verb};
        CodeLens{range:e.range,title:format!("HTTP request: {method} {}",e.path),command:COMMAND.into(),argument:Some(serde_json::json!({"source":doc.path,"version":doc.version,"method":e.verb,"path":e.path,"line":e.range.start.line,"character":e.range.start.character}).to_string())}
    }).collect()
}
fn request_argument(
    ctx: &Invocation,
    arg: Option<&str>,
    graphql: bool,
) -> Result<Option<String>, Error> {
    if let Some(arg) = arg {
        return Ok(Some(arg.into()));
    }
    let editor = ctx
        .editor()
        .ok_or_else(|| Error::Failed("Open an endpoint source file first.".into()))?;
    let snapshot = editor.document()?;
    let doc = Document {
        path: editor.path(),
        language: editor.language(),
        version: snapshot.version,
        text: snapshot.text,
    };
    if !matches!(doc.language.as_str(), "java" | "kotlin") {
        return Err(Error::Invalid(
            "Choose a Java or Kotlin handler source.".into(),
        ));
    }
    let choices = if graphql {
        graphql_lenses(&doc)
    } else {
        lenses(&doc)
    };
    if choices.is_empty() {
        return Err(Error::Failed(
            "No supported endpoint mapping is declared in this source.".into(),
        ));
    }
    let caret = editor
        .selections()?
        .first()
        .map(|selection| selection.start);
    let matching = choices
        .iter()
        .filter(|lens| caret.is_some_and(|caret| lens.range.start.line == caret.line))
        .collect::<Vec<_>>();
    if let [one] = matching.as_slice() {
        return Ok(one.argument.clone());
    }
    if choices.len() == 1 {
        return Ok(choices[0].argument.clone());
    }
    let items = choices
        .iter()
        .enumerate()
        .map(|(index, lens)| {
            openidea_plugin::PickItem::new(&format!("source:{index}"), &lens.title)
        })
        .collect::<Vec<_>>();
    let Some(key) = openidea_plugin::host::pick("Choose source endpoint", &items)? else {
        return Ok(None);
    };
    let choice = key
        .strip_prefix("source:")
        .and_then(|n| n.parse::<usize>().ok())
        .and_then(|n| choices.get(n))
        .ok_or_else(|| Error::Invalid("Unknown source endpoint selection.".into()))?;
    Ok(choice.argument.clone())
}
pub fn run(ctx: &Invocation, arg: Option<&str>) -> Result<(), Error> {
    let Some(argument) = request_argument(ctx, arg, false)? else {
        return Ok(());
    };
    let value: serde_json::Value = serde_json::from_str(&argument)
        .map_err(|_| Error::Invalid("Invalid endpoint lens.".into()))?;
    let editor = ctx
        .editor()
        .ok_or_else(|| Error::Failed("Open the endpoint source first.".into()))?;
    let snapshot = editor.document()?;
    let doc = Document {
        path: editor.path(),
        language: editor.language(),
        version: snapshot.version,
        text: snapshot.text,
    };
    if value["source"].as_str() != Some(doc.path.as_str())
        || value["version"].as_u64() != Some(doc.version)
    {
        return Err(Error::Failed(
            "Endpoint source changed; refresh its HTTP request lens.".into(),
        ));
    }
    let info = crate::endpoints::file_info(&doc.path, &doc.text)
        .ok_or_else(|| Error::Failed("Endpoint source is no longer available.".into()))?;
    let endpoint = info
        .endpoints
        .iter()
        .find(|e| {
            value["method"].as_str() == Some(e.verb.as_str())
                && value["path"].as_str() == Some(e.path.as_str())
                && value["line"].as_u64() == Some(e.range.start.line as u64)
                && value["character"].as_u64() == Some(e.range.start.character as u64)
        })
        .ok_or_else(|| Error::Failed("Endpoint changed; refresh its HTTP request lens.".into()))?;
    let declared = info.methods.iter().find(|m| m.range == endpoint.range);
    let mut route = Route {
        method: if endpoint.verb.is_empty() {
            "ANY".into()
        } else {
            endpoint.verb.clone()
        },
        path: endpoint.path.clone(),
        handler: Handler {
            class: declared
                .map(|m| m.ty.clone())
                .or_else(|| endpoint.handler.as_ref().and_then(|h| h.ty.clone()))
                .unwrap_or_default(),
            member: declared
                .map(|m| m.name.clone())
                .or_else(|| endpoint.handler.as_ref().map(|h| h.method.clone())),
        },
        name: None,
        location: Some(openidea_plugin::Location {
            path: doc.path,
            range: endpoint.range,
        }),
    };
    if !crate::select_http_method(&mut route)? {
        return Ok(());
    }
    crate::request_examples::create(&route, ctx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{Plugin, mock::MockHost};
    const SOURCE: &str =
        "@RestController class Users { @GetMapping(\"/users\") String list() {return \"\";} }";
    #[test]
    fn lens_generates_request_for_current_source_and_stale_lens_cannot_write() {
        let h = MockHost::install().with_capabilities(&[
            "project.read",
            "project.write",
            "editor.read",
            "clipboard",
        ]);
        h.open_editor("/project/Users.java", "java", SOURCE, &[]);
        let editor = h.invocation().editor().unwrap();
        let snapshot = editor.document().unwrap();
        let doc = Document {
            path: editor.path(),
            language: "java".into(),
            version: snapshot.version,
            text: snapshot.text,
        };
        let lens = lenses(&doc).remove(0);
        assert!(lens.title.contains("GET /users"));
        h.answer_next_prompt(Some(&["http://localhost:8080"]));
        let mut plugin = crate::window::Endpoints::default();
        plugin
            .run_command(COMMAND, &h.invocation(), lens.argument.as_deref())
            .unwrap();
        assert_eq!(
            h.invocation()
                .project()
                .unwrap()
                .read_file("http/get-users.http")
                .unwrap(),
            "### GET /users\nGET http://localhost:8080/users\n"
        );
        h.type_in_editor("// changed\n");
        assert!(
            plugin
                .run_command(COMMAND, &h.invocation(), lens.argument.as_deref())
                .is_err()
        );
    }
    #[test]
    fn command_palette_can_choose_a_source_endpoint_without_a_lens_payload() {
        let host = MockHost::install().with_capabilities(&[
            "project.read",
            "project.write",
            "editor.read",
        ]);
        let source = "@RestController class C { @GetMapping(\"/one\") void one(){} @GetMapping(\"/two\") void two(){} }";
        host.open_editor("/project/C.java", "java", source, &[]);
        host.answer_next_prompt(Some(&["source:1"]));
        host.answer_next_prompt(Some(&["http://localhost:8080"]));
        run(&host.invocation(), None).unwrap();
        assert!(
            host.invocation()
                .project()
                .unwrap()
                .read_file("http/get-two.http")
                .unwrap()
                .contains("GET http://localhost:8080/two")
        );
    }
    #[test]
    fn unrestricted_mapping_requires_explicit_method_choice_before_request_generation() {
        let h = MockHost::install().with_capabilities(&[
            "project.read",
            "project.write",
            "editor.read",
            "clipboard",
        ]);
        let source = "@RestController class Users { @RequestMapping(\"/all\") void all(){} }";
        h.open_editor("/project/Users.java", "java", source, &[]);
        let doc = Document {
            path: "/project/Users.java".into(),
            language: "java".into(),
            version: h.editor_version(),
            text: source.into(),
        };
        let lens = lenses(&doc).remove(0);
        h.answer_next_prompt(Some(&["PATCH"]));
        h.answer_next_prompt(Some(&["http://localhost:8080"]));
        run(&h.invocation(), lens.argument.as_deref()).unwrap();
        assert!(
            h.invocation()
                .project()
                .unwrap()
                .read_file("http/patch-all.http")
                .unwrap()
                .contains("PATCH http://localhost:8080/all")
        );
    }
    #[test]
    fn unrelated_controller_annotations_do_not_offer_spring_http_actions() {
        let source = "import custom.RestController; import custom.GetMapping; @RestController class Users { @GetMapping(\"/users\") void list(){} }";
        let doc = Document {
            path: "/project/Users.java".into(),
            language: "java".into(),
            version: 1,
            text: source.into(),
        };
        assert!(lenses(&doc).is_empty());
    }
    #[test]
    fn source_without_provable_mapping_does_not_offer_request_action() {
        let doc = Document {
            path: "/project/Users.java".into(),
            language: "java".into(),
            version: 1,
            text: "@RestController class Users { @GetMapping(PATH) void list(){} }".into(),
        };
        assert!(lenses(&doc).is_empty());
        let doc=Document{text:"@RestController class Users { @GetMapping(\"/users\\nGET /hidden\") void list(){} }".into(),..doc};
        assert!(lenses(&doc).is_empty());
    }
}

pub const GRAPHQL_COMMAND: &str = "create-source-graphql-http";
pub fn graphql_lenses(doc: &Document) -> Vec<CodeLens> {
    let file = spring_text::parse_source(&doc.path, &doc.text);
    graphql::handlers_in(&doc.text,spring_text::is_kotlin(&doc.path)).into_iter().filter(|h|matches!(h.op.as_deref(),Some("query"|"mutation")) || h.op.is_none() && doc.text.get(openidea_plugin::text::offset(&doc.text,h.annotation.start).unwrap_or(0)..).is_some_and(|s|s.starts_with("@SchemaMapping"))).filter(|h| {
        let annotation=match h.op.as_deref(){Some("query")=>"QueryMapping",Some("mutation")=>"MutationMapping",_=>"SchemaMapping"};
        file.from_spring(annotation) && file.types.iter().any(|c|h.label.starts_with(&format!("{}.",c.name)) && c.annotations.iter().any(|a|matches!(a.name.as_str(),"Controller"|"RestController")&&file.from_spring(&a.name)))
    }).take(256).map(|h|CodeLens{range:h.method,title:format!("GraphQL request: {} {}",h.op.as_deref().unwrap_or(&h.ty),h.field),command:GRAPHQL_COMMAND.into(),argument:Some(serde_json::json!({"source":doc.path,"version":doc.version,"operation":h.op,"field":h.field,"line":h.method.start.line,"character":h.method.start.character}).to_string())}).collect()
}
fn graphql_source(
    ctx: &Invocation,
    arg: &serde_json::Value,
) -> Result<(Document, graphql::Handler), Error> {
    let editor = ctx
        .editor()
        .ok_or_else(|| Error::Failed("Open the GraphQL handler source first.".into()))?;
    let snapshot = editor.document()?;
    let doc = Document {
        path: editor.path(),
        language: editor.language(),
        version: snapshot.version,
        text: snapshot.text,
    };
    if arg["source"].as_str() != Some(doc.path.as_str())
        || arg["version"].as_u64() != Some(doc.version)
    {
        return Err(Error::Failed(
            "GraphQL handler source changed; refresh its request lens.".into(),
        ));
    }
    if !graphql_lenses(&doc).iter().any(|lens| {
        lens.argument
            .as_deref()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
            .as_ref()
            == Some(arg)
    }) {
        return Err(Error::Failed(
            "This GraphQL handler changed. Run the action again from the updated code.".into(),
        ));
    }
    let handler = graphql::handlers_in(&doc.text, spring_text::is_kotlin(&doc.path))
        .into_iter()
        .find(|h| {
            arg["field"].as_str() == Some(h.field.as_str())
                && arg["operation"].as_str() == h.op.as_deref()
                && arg["line"].as_u64() == Some(h.method.start.line as u64)
                && arg["character"].as_u64() == Some(h.method.start.character as u64)
        })
        .ok_or_else(|| Error::Failed("GraphQL handler changed.".into()))?;
    Ok((doc, handler))
}
pub fn run_graphql(
    tools: &mut graphql::GraphQlTools,
    ctx: &Invocation,
    arg: Option<&str>,
) -> Result<(), Error> {
    let Some(argument) = request_argument(ctx, arg, true)? else {
        return Ok(());
    };
    let value: serde_json::Value = serde_json::from_str(&argument)
        .map_err(|_| Error::Invalid("Invalid GraphQL request lens.".into()))?;
    let (doc, handler) = graphql_source(ctx, &value)?;
    let draft = tools.request_draft(&doc, &handler)?;
    let Some(base) =
        openidea_plugin::host::input("GraphQL application origin", Some(crate::DEFAULT_BASE_URL))?
    else {
        return Ok(());
    };
    let base = base.trim().trim_end_matches('/');
    let authority = base
        .strip_prefix("http://")
        .or_else(|| base.strip_prefix("https://"))
        .filter(|s| !s.is_empty() && !s.contains(['/', '?', '#', '\\', '@']));
    if authority.is_none() || base.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(Error::Invalid(
            "Enter an HTTP(S) application origin without a path.".into(),
        ));
    }
    let (current, current_handler) = graphql_source(ctx, &value)?;
    let fresh = tools.request_draft(&current, &current_handler)?;
    if fresh.query != draft.query || fresh.path != draft.path || fresh.variables != draft.variables
    {
        return Err(Error::Failed(
            "GraphQL schema/configuration changed during request review.".into(),
        ));
    }
    let mut variables = serde_json::Map::new();
    for (name, ty) in &draft.variables {
        let value = if ty.starts_with('[') {
            serde_json::json!([])
        } else {
            match ty.trim_end_matches('!') {
                "Int" => serde_json::json!(0),
                "Float" => serde_json::json!(0.0),
                "Boolean" => serde_json::json!(false),
                "String" | "ID" => serde_json::json!(""),
                _ => serde_json::Value::Null,
            }
        };
        variables.insert(name.clone(), value);
    }
    let body = serde_json::to_string_pretty(
        &serde_json::json!({"query":draft.query,"variables":variables}),
    )
    .unwrap();
    let content = format!(
        "### GraphQL {} {}\n# Review argument values and replace null custom input values before sending.\nPOST {base}{}\nContent-Type: application/json\n\n{body}\n",
        draft.operation, draft.field, draft.path
    );
    let project = openidea_plugin::host::project_of(&doc.path)
        .ok_or_else(|| Error::Failed("GraphQL source project is unavailable.".into()))?;
    let relative = format!("http/graphql-{}-{}.http", draft.operation, draft.field);
    let path = format!("{}/{}", project.root()?.trim_end_matches('/'), relative);
    if project.exists(&path)? {
        return openidea_plugin::host::open_location(&path, 0, 0);
    }
    match project.write_file(&path, &content) {
        Ok(()) => openidea_plugin::host::open_location(&path, 0, 0),
        Err(Error::Denied(_)) => crate::copy_after_write_denied(&content),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod graphql_tests {
    use super::*;
    use openidea_plugin::{Plugin, language_features::LanguageFeatures, mock::MockHost};
    #[test]
    fn registered_graphql_request_lens_generates_schema_qualified_http_file() {
        let h = MockHost::install().with_capabilities(&[
            "project.read",
            "project.write",
            "editor.read",
            "clipboard",
        ]);
        h.add_file("pom.xml", "spring-boot-starter-graphql");
        h.add_file(
            "src/main/resources/graphql/schema.graphqls",
            "type Query { book(id: ID!): Book } type Book {id: ID}",
        );
        h.add_file(
            "src/main/resources/application.properties",
            "spring.graphql.path=/api/graphql\n",
        );
        let source = "@Controller class Books { @QueryMapping Book book(@Argument String id){} }";
        h.open_editor("/project/Books.java", "java", source, &[]);
        let doc = Document {
            path: "/project/Books.java".into(),
            language: "java".into(),
            version: h.editor_version(),
            text: source.into(),
        };
        let mut owner = crate::window::Endpoints::default();
        owner.activate_live();
        let lens = owner
            .code_lenses(&doc)
            .into_iter()
            .find(|l| l.command == GRAPHQL_COMMAND)
            .unwrap();
        h.answer_next_prompt(Some(&["http://localhost:8080"]));
        owner
            .run_command(GRAPHQL_COMMAND, &h.invocation(), lens.argument.as_deref())
            .unwrap();
        let content = h
            .invocation()
            .project()
            .unwrap()
            .read_file("http/graphql-query-book.http")
            .unwrap();
        assert_eq!(
            content.lines().find(|line| line.starts_with("POST ")),
            Some("POST http://localhost:8080/api/graphql")
        );
        let payload: serde_json::Value =
            serde_json::from_str(content.split_once("\n\n").unwrap().1).unwrap();
        assert!(
            payload["query"]
                .as_str()
                .unwrap()
                .contains("book(id: $id) { __typename }")
        );
        assert_eq!(payload["variables"]["id"], "");
        h.type_in_editor("// changed");
        assert!(
            owner
                .run_command(GRAPHQL_COMMAND, &h.invocation(), lens.argument.as_deref())
                .is_err()
        );
    }
    #[test]
    fn explicit_schema_mapping_custom_root_generates_real_request_while_fixed_shortcut_refuses(){
        let h=MockHost::install().with_capabilities(&["project.read","project.write","editor.read"]);
        h.add_file("pom.xml","spring-boot-starter-graphql");
        h.add_file("src/main/resources/graphql/schema.graphqls","schema {query: Root} type Root { book: String }");
        let source="@Controller class Books { @SchemaMapping(typeName=\"Root\",field=\"book\") String book(){} }";
        h.open_editor("/project/Books.java","java",source,&[]);
        let doc=Document{path:"/project/Books.java".into(),language:"java".into(),version:h.editor_version(),text:source.into()};
        let mut owner=crate::window::Endpoints::default();owner.activate_live();let lens=owner.code_lenses(&doc).into_iter().find(|l|l.command==GRAPHQL_COMMAND).unwrap();
        h.answer_next_prompt(Some(&["http://localhost:8080"]));owner.run_command(GRAPHQL_COMMAND,&h.invocation(),lens.argument.as_deref()).unwrap();
        let content=h.invocation().project().unwrap().read_file("http/graphql-query-book.http").unwrap();assert!(content.contains("POST http://localhost:8080/graphql"));
        let payload:serde_json::Value=serde_json::from_str(content.split_once("\n\n").unwrap().1).unwrap();assert_eq!(payload["query"],"query OpenIdeaRequest {\n  book\n}");
        let source="@Controller class Books { @QueryMapping String book(){} }";
        h.open_editor("/project/Books.java","java",source,&[]);
        let doc=Document{text:source.into(),version:h.editor_version(),..doc};
        assert!(!owner.code_lenses(&doc).iter().any(|lens|lens.command==GRAPHQL_COMMAND));
        assert!(owner.run_command(GRAPHQL_COMMAND,&h.invocation(),None).is_err());
    }
    #[test]
    fn subscriptions_and_foreign_annotations_do_not_offer_wrong_http_transport() {
        let mut doc = Document {
            path: "/project/C.java".into(),
            language: "java".into(),
            version: 1,
            text: "@Controller class C { @SubscriptionMapping String changed(){} }".into(),
        };
        assert!(graphql_lenses(&doc).is_empty());
        doc.text =
            "import other.QueryMapping; @Controller class C { @QueryMapping String value(){} }"
                .into();
        assert!(graphql_lenses(&doc).is_empty());
    }
}

/// Client-side request examples are separate from server endpoint publication.
pub fn feign_lenses(doc:&Document)->Vec<CodeLens>{crate::feign_requests::lenses(doc)}
pub fn run_feign(ctx:&Invocation,arg:Option<&str>)->Result<(),Error>{crate::feign_requests::run(ctx,arg)}
