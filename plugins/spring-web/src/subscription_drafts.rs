//! Source-to-editable-document bridge; transport remains a separately reviewed action.
use openidea_plugin::{
    Error, Invocation, host,
    language_features::{CodeLens, Document},
};
use serde_json::{Value, json};
pub const COMMAND: &str = "create-source-graphql-subscription";
fn current(ctx: &Invocation) -> Result<Document, Error> {
    let editor = ctx
        .editor()
        .ok_or_else(|| Error::Invalid("Open the GraphQL handler source first.".into()))?;
    let snapshot = editor.document()?;
    Ok(Document {
        path: editor.path(),
        language: editor.language(),
        version: snapshot.version,
        text: snapshot.text,
    })
}
pub fn lenses(doc: &Document) -> Vec<CodeLens> {
    if !((doc.language == "java" && doc.path.ends_with(".java"))
        || (doc.language == "kotlin" && doc.path.ends_with(".kt")))
        || doc.text.len() > 128 << 10
    {
        return vec![];
    }
    graphql::handlers_in(&doc.text,spring_text::is_kotlin(&doc.path)).into_iter().filter(|h|h.op.as_deref()==Some("subscription")||h.op.is_none()&&openidea_plugin::text::offset(&doc.text,h.annotation.start).and_then(|at|doc.text.get(at..)).is_some_and(|s|s.starts_with("@SchemaMapping"))).take(256).map(|h|CodeLens{range:h.method,title:format!("Prepare subscription document: {}.{}",h.ty,h.field),command:COMMAND.into(),argument:Some(json!({"source":doc.path,"version":doc.version,"field":h.field,"type":h.ty,"line":h.method.start.line,"character":h.method.start.character}).to_string())}).collect()
}
pub fn run(
    tools: &mut graphql::GraphQlTools,
    ctx: &Invocation,
    arg: Option<&str>,
) -> Result<(), Error> {
    let doc = current(ctx)?;
    let choices = lenses(&doc);
    let selected = if let Some(argument) = arg {
        argument.to_owned()
    } else {
        if choices.is_empty() {
            return Err(Error::Invalid(
                "No GraphQL subscription handler found in this file. Use a @SubscriptionMapping method, or a @SchemaMapping method on the Subscription type.".into(),
            ));
        }
        if choices.len() == 1 {
            choices[0].argument.clone().unwrap_or_default()
        } else {
            let items = choices
                .iter()
                .enumerate()
                .map(|(i, l)| openidea_plugin::PickItem::new(&format!("source:{i}"), &l.title))
                .collect::<Vec<_>>();
            let Some(key) = host::pick("Choose subscription handler source", &items)? else {
                return Ok(());
            };
            key.strip_prefix("source:")
                .and_then(|key| key.parse::<usize>().ok())
                .and_then(|i| choices.get(i))
                .and_then(|l| l.argument.clone())
                .ok_or_else(|| Error::Invalid("Unknown subscription source.".into()))?
        }
    };
    let value: Value = serde_json::from_str(&selected)
        .map_err(|_| Error::Invalid("Invalid subscription source action.".into()))?;
    if current(ctx)? != doc
        || !choices.iter().any(|l| {
            l.argument
                .as_deref()
                .and_then(|a| serde_json::from_str::<Value>(a).ok())
                .as_ref()
                == Some(&value)
        })
    {
        return Err(Error::Stale);
    }
    let handler = graphql::handlers_in(&doc.text, spring_text::is_kotlin(&doc.path))
        .into_iter()
        .find(|h| {
            value["field"].as_str() == Some(h.field.as_str())
                && value["type"].as_str() == Some(h.ty.as_str())
                && value["line"].as_u64() == Some(h.method.start.line as u64)
                && value["character"].as_u64() == Some(h.method.start.character as u64)
        })
        .ok_or(Error::Stale)?;
    let draft = tools.subscription_draft(&doc, &handler)?;
    let project = host::project_of(&doc.path)
        .ok_or_else(|| Error::Invalid("Open the subscription source project.".into()))?;
    if ctx.project().map(|p| p.root()).transpose()? != Some(project.root()?) {
        return Err(Error::Stale);
    }
    let variables = draft
        .variables
        .iter()
        .map(|(name, ty)| {
            let kind = ty.trim_end_matches('!');
            let value = if kind.starts_with('[') {
                json!([])
            } else {
                match kind {
                    "Int" | "Float" => json!(0),
                    "Boolean" => json!(false),
                    "ID" | "String" => json!(""),
                    _ => Value::Null,
                }
            };
            (name.clone(), value)
        })
        .collect::<serde_json::Map<_, _>>();
    let sample = serde_json::to_string_pretty(&variables)
        .unwrap_or_default()
        .lines()
        .map(|l| format!("# {l}\n"))
        .collect::<String>();
    let content = format!(
        "# Source-qualified subscription draft. Review selections and argument values.\n# Suggested variables for the separate Start GraphQL Subscription command:\n{sample}# Replace null custom-input samples before connecting.\n# Endpoint and initialization payload are selected/reviewed separately; this action opens no connection.\n{}\n",
        draft.query
    );
    if current(ctx)? != doc {
        return Err(Error::Stale);
    }
    let relative = format!("graphql-documents/subscription-{}.graphql", draft.field);
    let path = format!("{}/{}", project.root()?.trim_end_matches('/'), relative);
    if project.exists(&path)? {
        return host::open_location(&path, 0, 0);
    }
    match project.write_file(&path, &content) {
        Ok(()) => host::open_location(&path, 0, 0),
        Err(Error::Denied(_)) => crate::copy_after_write_denied(&content),
        Err(error) => Err(error),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{Plugin, language_features::LanguageFeatures, mock::MockHost};
    #[test]
    fn registered_source_lens_creates_editable_subscription_document_without_connecting() {
        let host = MockHost::install().with_capabilities(&[
            "project.read",
            "project.write",
            "editor.read",
        ]);
        host.add_file("pom.xml", "spring-boot-starter-graphql");
        host.add_file(
            "src/main/resources/graphql/schema.graphqls",
            "type Subscription { changed(ids:[ID!]!): Item } type Item {id:ID}",
        );
        let source = "@Controller class Events { @SubscriptionMapping Item changed(){} }";
        host.open_editor("/project/Events.java", "java", source, &[]);
        let doc = current(&host.invocation()).unwrap();
        let mut owner = crate::window::Endpoints::default();
        owner.activate_live();
        let lens = owner
            .code_lenses(&doc)
            .into_iter()
            .find(|l| l.command == COMMAND)
            .unwrap();
        owner
            .run_command(COMMAND, &host.invocation(), lens.argument.as_deref())
            .unwrap();
        let content = host
            .invocation()
            .project()
            .unwrap()
            .read_file("graphql-documents/subscription-changed.graphql")
            .unwrap();
        assert!(content.contains("subscription OpenIdeaRequest($ids: [ID!]!)"));
        assert!(content.contains("changed(ids: $ids) { __typename }"));
        assert!(content.contains("#   \"ids\": []"));
        assert!(!content.contains("WEBSOCKET"));
        assert!(host.output().is_empty());
    }
    #[test]
    fn dynamic_foreign_and_stale_source_never_creates_a_document() {
        let mut doc=Document{path:"/project/C.java".into(),language:"java".into(),version:1,text:"import other.SubscriptionMapping; @Controller class C {@SubscriptionMapping String value(){} }".into()};
        assert!(lenses(&doc).is_empty());
        doc.text = "@Controller class C {@SubscriptionMapping(NAME) String value(){} }".into();
        assert!(lenses(&doc).is_empty());
        let host = MockHost::install();
        let source = "@Controller class C {@SubscriptionMapping String value(){} }";
        host.open_editor("/project/C.java", "java", source, &[]);
        let doc = current(&host.invocation()).unwrap();
        let lens = lenses(&doc).remove(0);
        host.type_in_editor("//changed");
        assert!(
            run(
                &mut graphql::GraphQlTools::default(),
                &host.invocation(),
                lens.argument.as_deref()
            )
            .is_err()
        );
        assert!(
            !host
                .invocation()
                .project()
                .unwrap()
                .exists("graphql-documents/subscription-value.graphql")
                .unwrap()
        );
    }
    #[test]
    fn native_actions_are_only_offered_for_proved_sdl_operation_roots(){
        let host=MockHost::install();host.add_file("pom.xml","spring-boot-starter-graphql");host.add_file("src/main/resources/graphql/schema.graphqls","type Query {now:String} type Book {title:String}");
        let doc=Document{path:"/project/C.java".into(),language:"java".into(),version:1,text:"@Controller class C {@SchemaMapping(typeName=\"Book\",field=\"title\") String title(){} }".into()};
        let mut owner=crate::window::Endpoints::default();let actions=owner.code_lenses(&doc);assert!(!actions.iter().any(|l|l.command==COMMAND||l.command==crate::source_lenses::GRAPHQL_COMMAND));
    }
}
