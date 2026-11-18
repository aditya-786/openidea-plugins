//! Declared source facts, deliberately separate from effective runtime endpoints/configuration.
use openidea_plugin::inlay_hints::{InlayHint, InlayKind};
use openidea_plugin::language_features::{CodeLens, DocumentSymbol};
use openidea_plugin::providers::Document;
use openidea_plugin::{ContentTab, Error, Invocation, Range, host};
use spring_text::{
    java::{self, Annotation, JavaFile},
    range,
};
const MAX_TEXT: usize = 128 << 10;
pub(crate) const COMMAND: &str = "inspect-source-declaration";
struct Fact {
    title: String,
    detail: String,
    span: Range,
}
fn literal<'a>(a: &'a Annotation, name: &str) -> Option<&'a str> {
    let arg = a.arg(name)?;
    let [value] = arg.strings.as_slice() else {
        return None;
    };
    (java::tokens(&arg.text).len() == 1
        && arg.text.starts_with('"')
        && arg.text.ends_with('"')
        && !arg.text.contains('\\')
        && !value.value.chars().any(char::is_control))
    .then_some(value.value.as_str())
}
fn owned(file: &JavaFile, text: &str, a: &Annotation, qualified: &str) -> bool {
    crate::java::annotation_owned(file, text, a, qualified)
}
fn path(file: &JavaFile, text: &str, annotations: &[Annotation]) -> Option<String> {
    let paths: Vec<_> = annotations.iter().filter(|a| a.name == "Path").collect();
    let [annotation] = paths.as_slice() else {
        return None;
    };
    if !["jakarta.ws.rs.Path", "javax.ws.rs.Path"]
        .iter()
        .any(|q| owned(file, text, annotation, q))
    {
        return None;
    }
    let value = literal(annotation, "value")?;
    (!value.contains(['$', '?', '#']) && !value.contains("://") && value.len() <= 512)
        .then(|| value.trim_matches('/').into())
}
fn facts(doc: &Document) -> Vec<Fact> {
    facts_with(doc, None)
}
fn facts_with(doc: &Document, prefix: Option<&str>) -> Vec<Fact> {
    if doc.language != "java" || !doc.path.ends_with(".java") || doc.text.len() > MAX_TEXT {
        return vec![];
    }
    let file = java::parse(&doc.text);
    let mut out = vec![];
    for ty in &file.types {
        let root = path(&file, &doc.text, &ty.annotations);
        for method in ty.methods() {
            let span = range(&doc.text, method.at, method.at + method.name.len());
            if let Some(root) = &root {
                let method_path = if method.annotations.iter().any(|a| a.name == "Path") {
                    let Some(path) = path(&file, &doc.text, &method.annotations) else {
                        continue;
                    };
                    path
                } else {
                    String::new()
                };
                for annotation in &method.annotations {
                    let verb = annotation.name.as_str();
                    if !["GET", "POST", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS"].contains(&verb)
                        || !["jakarta.ws.rs", "javax.ws.rs"]
                            .iter()
                            .any(|ns| owned(&file, &doc.text, annotation, &format!("{ns}.{verb}")))
                    {
                        continue;
                    }
                    let declared = format!("/{}/{}", root, method_path)
                        .trim_end_matches('/')
                        .replace("//", "/");
                    let declared = if declared.is_empty() {
                        "/".into()
                    } else {
                        declared
                    };
                    let effective = prefix.map(|p| join_path(&[p, &declared])).filter(|e| *e != declared);
                    let (title, effective_note) = match &effective {
                        Some(e) => (
                            format!("{verb} {declared} (effective {e})"),
                            format!("Effective path: {e}\nComposed from quarkus.http.root-path, then @ApplicationPath or else quarkus.rest.path, class @Path and method @Path, as written in the source. Environment, profile and system-property overrides are not read.\n"),
                        ),
                        None => (format!("{verb} {declared}"), String::new()),
                    };
                    out.push(Fact{title,detail:format!("Declared JAX-RS source mapping\nHTTP method: {verb}\nPath: {declared}\n{effective_note}Java method: {}::{}\n\nInheritance, registration and runtime conditions are not resolved. This is not a running endpoint URL.",ty.name,method.name),span});
                }
            }
            for a in method
                .annotations
                .iter()
                .filter(|a| owned(&file, &doc.text, a, "io.quarkus.scheduler.Scheduled"))
            {
                // Quarkus gives cron precedence when both are declared.
                let trigger = if a.arg("cron").is_some() && literal(a, "cron") != Some("") {
                    literal(a, "cron").map(|s| ("cron", s))
                } else {
                    literal(a, "every")
                        .filter(|s| !s.is_empty())
                        .map(|s| ("every", s))
                };
                let Some((kind, value)) = trigger else {
                    continue;
                };
                if value.contains(['{', '$']) {
                    continue;
                }
                out.push(Fact{title:format!("Scheduled {kind}: {value}"),detail:format!("Declared scheduler trigger\n{kind}: {value}\nJava method: {}::{}\n\nCron syntax, time zone, scheduler state and effective configuration are not resolved. No job is executed.",ty.name,method.name),span});
            }
        }
    }
    out.truncate(256);
    out
}
fn join_path(parts: &[&str]) -> String {
    let joined: Vec<&str> = parts.iter().flat_map(|p| p.split('/')).filter(|s| !s.is_empty()).collect();
    format!("/{}", joined.join("/"))
}

fn config_values(text: &str, wanted: &[&str]) -> Option<std::collections::BTreeMap<String, String>> {
    let mut found = std::collections::BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with(['#', '!']) || line.is_empty() {
            continue;
        }
        let Some(at) = line.find(['=', ':']) else {
            continue;
        };
        let key = line[..at].trim();
        let bare = key.trim_start_matches('%');
        let profiled = key.starts_with('%') && bare.split_once('.').is_some_and(|(_, k)| wanted.contains(&k));
        if profiled || (wanted.contains(&key) && found.insert(key.to_owned(), line[at + 1..].trim().to_owned()).is_some()) {
            return None;
        }
    }
    Some(found)
}

fn literal_segment_path(value: &str) -> Option<&str> {
    (value.len() <= 256 && !value.contains(['$', '{', '}', '%', '\\', '?', '#', ' ', '"', '\'']) && !value.split('/').any(|s| s == "." || s == ".."))
        .then_some(value)
}

/// Literal composed prefix for resource paths: root-path, then `@ApplicationPath` (which replaces
/// `quarkus.rest.path`, per Quarkus source and its ApplicationPath test) or else `quarkus.rest.path`.
/// `None` when anything is non-literal or ambiguous.
pub(crate) fn rest_prefix(path: &str) -> Option<String> {
    let project = crate::project_of(path)?;
    let module = crate::navigation::module(path)?;
    let resources = format!("{module}/src/main/resources");
    for yaml in ["application.yaml", "application.yml"] {
        if project.read_file(&format!("{resources}/{yaml}")).is_ok() {
            return None;
        }
    }
    let props = match project.read_file(&format!("{resources}/application.properties")) {
        Ok(text) if text.len() <= MAX_TEXT => text,
        Ok(_) => return None,
        Err(Error::NotFound(_)) => String::new(),
        Err(_) => return None,
    };
    let config = config_values(&props, &["quarkus.http.root-path", "quarkus.rest.path"])?;
    let root = literal_segment_path(config.get("quarkus.http.root-path").map_or("/", String::as_str))?;
    let files = project.find_files("**/src/main/java/**/*.java", 2001).ok()?;
    if files.len() > 2000 {
        return None;
    }
    let mut application_path: Option<String> = None;
    for file in files.iter().filter(|f| f.starts_with(&format!("{module}/src/main/java/"))) {
        let Ok(text) = project.read_file(file) else {
            return None;
        };
        if !text.contains("ApplicationPath") || text.len() > MAX_TEXT {
            continue;
        }
        let parsed = java::parse(&text);
        for ty in &parsed.types {
            for a in ty.annotations.iter().filter(|a| a.name == "ApplicationPath") {
                if !["jakarta.ws.rs.ApplicationPath", "javax.ws.rs.ApplicationPath"].iter().any(|q| owned(&parsed, &text, a, q)) {
                    continue;
                }
                let value = literal(a, "value").and_then(literal_segment_path)?;
                if application_path.replace(value.to_owned()).is_some() {
                    return None;
                }
            }
        }
    }
    let rest = match application_path {
        Some(value) => value,
        None => literal_segment_path(config.get("quarkus.rest.path").map_or("", String::as_str))?.to_owned(),
    };
    Some(join_path(&[root, &rest]))
}

#[cfg(test)]
pub(crate) fn lenses(doc: &Document) -> Vec<CodeLens> {
    lenses_with(doc, None)
}
pub(crate) fn lenses_with(doc: &Document, prefix: Option<&str>) -> Vec<CodeLens> {
    facts_with(doc, prefix).into_iter().map(|f|CodeLens{range:f.span,title:f.title.clone(),command:COMMAND.into(),argument:Some(serde_json::json!({"path":doc.path,"version":doc.version,"line":f.span.start.line,"character":f.span.start.character,"title":f.title}).to_string())}).collect()
}
pub(crate) fn symbols(doc: &Document) -> Vec<DocumentSymbol> {
    facts(doc)
        .into_iter()
        .map(|f| DocumentSymbol {
            name: f.title,
            detail: "Declared Quarkus source mapping".into(),
            kind: "method".into(),
            range: f.span,
            selection_range: f.span,
            parent: None,
        })
        .collect()
}
pub(crate) fn inspect(ctx: &Invocation, arg: Option<&str>) -> Result<(), Error> {
    let editor = ctx.editor().ok_or("Open a Java file with a Quarkus REST endpoint or @Scheduled method first.")?;
    let snapshot = editor.document()?;
    let doc = Document {
        path: editor.path(),
        language: editor.language(),
        version: snapshot.version,
        text: snapshot.text,
    };
    let project = ctx.project().ok_or("Open a project first.")?;
    let root = project.root()?;
    if !crate::source_owned(&root, &doc.path) {
        return Err(Error::Invalid("This file isn't part of the open project.".into()));
    }
    let prefix = rest_prefix(&doc.path);
    let facts = facts_with(&doc, prefix.as_deref());
    let fact = match arg {
        None => {
            if facts.is_empty() {
                return Err(Error::Invalid("This file has no Quarkus REST endpoint or @Scheduled method.".into()));
            }
            let items: Vec<_> = facts
                .iter()
                .enumerate()
                .map(|(i, f)| openidea_plugin::PickItem::new(&i.to_string(), &f.title).description(&format!("line {}", f.span.start.line + 1)))
                .collect();
            let chosen = host::pick("Show Quarkus declaration", &items)?.ok_or(Error::Cancelled)?;
            let index: usize = chosen.parse().map_err(|_| Error::Stale)?;
            facts.into_iter().nth(index).ok_or(Error::Stale)?
        }
        Some(raw) => {
            let payload: serde_json::Value = serde_json::from_str(raw)
                .ok()
                .filter(|_| raw.len() <= 4096)
                .ok_or_else(|| Error::Invalid("This code lens is out of date. Click it again.".into()))?;
            if payload["path"].as_str() != Some(doc.path.as_str()) || payload["version"].as_u64() != Some(doc.version) {
                return Err(Error::Stale);
            }
            facts
                .into_iter()
                .find(|f| {
                    payload["title"].as_str() == Some(f.title.as_str())
                        && payload["line"].as_u64() == Some(f.span.start.line as u64)
                        && payload["character"].as_u64() == Some(f.span.start.character as u64)
                })
                .ok_or(Error::Stale)?
        }
    };
    host::open_content_tab(&ContentTab {
        key: format!(
            "quarkus-source:{}:{}:{}",
            doc.path, fact.span.start.line, fact.span.start.character
        ),
        title: fact.title,
        language: Some("plaintext".into()),
        content: format!(
            "{}\nSource: {}\nDocument version: {}",
            fact.detail, doc.path, doc.version
        ),
        read_only: true,
        ..Default::default()
    })
}

pub(crate) fn inlays(doc: &Document, requested: Range) -> Vec<InlayHint> {
    if doc.language != "java" || !doc.path.ends_with(".java") || doc.text.len() > MAX_TEXT {
        return vec![];
    }
    let file = java::parse(&doc.text);
    let mut out = vec![];
    for ty in &file.types {
        let annotations = ty
            .members
            .iter()
            .flat_map(|member| {
                member.annotations.iter().chain(
                    member
                        .params
                        .iter()
                        .flat_map(|parameter| &parameter.annotations),
                )
            })
            .chain(
                ty.components
                    .iter()
                    .flat_map(|component| &component.annotations),
            );
        for a in annotations {
            let value = if owned(
                &file,
                &doc.text,
                a,
                "org.eclipse.microprofile.config.inject.ConfigProperty",
            ) {
                literal(a, "defaultValue").map(|value| format!("declared default: {value}"))
            } else if ty
                .annotations
                .iter()
                .any(|m| owned(&file, &doc.text, m, "io.smallrye.config.ConfigMapping"))
                && owned(&file, &doc.text, a, "io.smallrye.config.WithDefault")
            {
                literal(a, "value").map(|value| format!("declared default: {value}"))
            } else if ty
                .annotations
                .iter()
                .any(|m| owned(&file, &doc.text, m, "io.smallrye.config.ConfigMapping"))
                && owned(&file, &doc.text, a, "io.smallrye.config.WithConverter")
            {
                a.arg("value").and_then(|arg| {
                    let expr: String = java::tokens(&arg.text).iter().map(|t| t.s).collect();
                    let class = expr.strip_suffix(".class")?;
                    (!class.is_empty()
                        && class.split('.').all(|part| {
                            !part.is_empty()
                                && part
                                    .bytes()
                                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'$')
                        }))
                    .then(|| format!("declared converter: {class}"))
                })
            } else {
                None
            };
            let Some(label) = value.filter(|s| s.len() <= 160 && !s.chars().any(char::is_control))
            else {
                continue;
            };
            let at = range(&doc.text, a.end, a.end).end;
            if at < requested.start || at > requested.end {
                continue;
            }
            out.push(InlayHint::new(at,label).kind(InlayKind::Parameter).padding_left().tooltip("Literal annotation source declaration; not an effective runtime value or resolved converter implementation."));
        }
    }
    out.truncate(200);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    fn doc(text: &str) -> Document {
        Document {
            path: "/project/src/main/java/Resource.java".into(),
            language: "java".into(),
            version: 3,
            text: text.into(),
        }
    }
    #[test]
    fn inspect_source_declaration_without_editor_explains() {
        let host = openidea_plugin::mock::MockHost::install();
        let error = inspect(&host.invocation(), None).unwrap_err();
        assert!(error.to_string().contains("Open a Java file with a Quarkus REST endpoint"), "{error:?}");
    }
    #[test]
    fn palette_run_picks_a_declaration_of_the_current_file() {
        use openidea_plugin::mock::MockHost;
        let host = MockHost::install().with_capabilities(&["project.read", "editor.read"]);
        let text = "import io.quarkus.scheduler.Scheduled; class Job { @Scheduled(every=\"10s\") void run() {} }";
        host.add_file("src/main/java/Job.java", text);
        host.open_editor("/project/src/main/java/Job.java", "java", text, &[]);
        host.answer_next_prompt(Some(&["0"]));
        inspect(&host.invocation(), None).unwrap();
        host.add_file("src/main/java/Plain.java", "class Plain {}");
        host.open_editor("/project/src/main/java/Plain.java", "java", "class Plain {}", &[]);
        let error = inspect(&host.invocation(), None).unwrap_err();
        assert!(error.to_string().contains("no Quarkus REST endpoint"), "{error}");
    }
    #[test]
    fn jaxrs_lenses_and_symbols_are_literal_declared_routes_not_runtime_urls() {
        let text = "import jakarta.ws.rs.Path; import jakarta.ws.rs.GET; @Path(\"/api\") class Resource { @GET @Path(\"/{id}\") String show() { return null; } }";
        let doc = doc(text);
        let lenses = lenses(&doc);
        assert_eq!(lenses.len(), 1);
        assert_eq!(lenses[0].title, "GET /api/{id}");
        let symbols = symbols(&doc);
        assert_eq!(symbols[0].selection_range, lenses[0].range);
        assert!(facts(&doc)[0].detail.contains("not a running endpoint"));
        for text in [
            text.replace("jakarta.ws.rs.GET", "foreign.GET"),
            text.replace("jakarta.ws.rs.Path", "foreign.Path"),
            text.replace("@Path(\"/{id}\")", "@Path(PATH)"),
            text.replace("@Path(\"/api\")", ""),
        ] {
            assert!(facts(&self::doc(&text)).is_empty());
        }
    }
    fn rest_titles(files: &[(&str, &str)], resource: &str) -> Vec<String> {
        let host = openidea_plugin::mock::MockHost::install();
        for (p, t) in files {
            host.add_file(p, t);
        }
        let path = "/project/src/main/java/Resource.java";
        host.add_file(path, resource);
        let prefix = rest_prefix(path);
        lenses_with(&doc(resource), prefix.as_deref()).into_iter().map(|l| l.title).collect()
    }
    const RES: &str = "import jakarta.ws.rs.Path; import jakarta.ws.rs.GET; @Path(\"r\") class Resource { @GET @Path(\"h\") String h() { return null; } }";
    const PROPS: &str = "/project/src/main/resources/application.properties";
    const APP: &str = "import jakarta.ws.rs.ApplicationPath; @ApplicationPath(\"/api\") class A extends jakarta.ws.rs.core.Application {}";
    const APP_FILE: &str = "/project/src/main/java/A.java";
    #[test]
    fn rest_root_composes_root_path_application_path_class_and_method() {
        let t = rest_titles(&[(PROPS, "quarkus.http.root-path=/app\nquarkus.rest.path=/ignored\n"), (APP_FILE, APP)], RES);
        assert_eq!(t, ["GET /r/h (effective /app/api/r/h)"]);
    }
    #[test]
    fn rest_path_applies_without_application_path_and_default_root_is_quiet() {
        assert_eq!(rest_titles(&[(PROPS, "quarkus.rest.path=v1\n")], RES), ["GET /r/h (effective /v1/r/h)"]);
        assert_eq!(rest_titles(&[(PROPS, "other=1\n")], RES), ["GET /r/h"]);
    }
    #[test]
    fn rest_root_stays_quiet_when_ambiguous_or_non_literal() {
        let second = APP.replace("class A", "class B");
        for files in [
            vec![(PROPS, "quarkus.rest.path=${x}\n")],
            vec![(PROPS, "quarkus.rest.path=/a\nquarkus.rest.path=/b\n")],
            vec![(PROPS, "%dev.quarkus.http.root-path=/dev\n")],
            vec![(APP_FILE, APP), ("/project/src/main/java/B.java", second.as_str())],
            vec![(APP_FILE, "import jakarta.ws.rs.ApplicationPath; @ApplicationPath(P) class A {}")],
        ] {
            assert_eq!(rest_titles(&files, RES), ["GET /r/h"]);
        }
    }
    #[test]
    fn schedule_lenses_follow_cron_precedence_and_keep_configured_expressions_unknown() {
        let imports = "import io.quarkus.scheduler.Scheduled; ";
        for (annotation, title) in [
            ("@Scheduled(every=\"10s\")", "Scheduled every: 10s"),
            (
                "@Scheduled(cron=\"0 0/5 * * * ?\",every=\"10s\")",
                "Scheduled cron: 0 0/5 * * * ?",
            ),
            (
                "@Scheduled(cron=\"\",every=\"10s\")",
                "Scheduled every: 10s",
            ),
        ] {
            let d = doc(&format!(
                "{imports}class Job {{ {annotation} void run() {{}} }}"
            ));
            assert_eq!(lenses(&d)[0].title, title);
        }
        assert!(lenses(&doc(&format!("{imports}class Job {{ @Scheduled(cron=\"${{cron}}\",every=\"10s\") void run() {{}} }}"))).is_empty());
    }
    #[test]
    fn java_defaults_and_converters_are_owned_source_declarations() {
        let text = "import org.eclipse.microprofile.config.inject.ConfigProperty; import io.smallrye.config.ConfigMapping; import io.smallrye.config.WithDefault; import io.smallrye.config.WithConverter; class App { @ConfigProperty(name=\"hello\",defaultValue=\"world\") String hello; } @ConfigMapping(prefix=\"server\") interface Config { @WithDefault(\"10\") int port(); @WithConverter(PortConverter.class) String value(); }";
        let d = doc(text);
        let all = Range::new(
            Default::default(),
            openidea_plugin::Position::new(u32::MAX, u32::MAX),
        );
        let hints = inlays(&d, all);
        assert_eq!(hints.len(), 3);
        assert_eq!(hints[0].label, "declared default: world");
        assert_eq!(hints[2].label, "declared converter: PortConverter");
        assert!(hints.iter().all(|h| {
            h.tooltip
                .as_ref()
                .unwrap()
                .contains("not an effective runtime")
        }));
        assert!(
            inlays(
                &doc(&text.replace("io.smallrye.config.WithConverter", "foreign.WithConverter")),
                all
            )
            .iter()
            .all(|h| !h.label.contains("converter"))
        );
        assert!(inlays(&d, Range::default()).is_empty());
        let parameters = doc(
            "import org.eclipse.microprofile.config.inject.ConfigProperty; record Bean(@ConfigProperty(name=\"hello\",defaultValue=\"record\") String greeting) {} class App { App(@ConfigProperty(name=\"world\",defaultValue=\"parameter\") String name) {} }",
        );
        let labels: Vec<_> = inlays(&parameters, all)
            .into_iter()
            .map(|h| h.label)
            .collect();
        assert!(labels.iter().any(|s| s == "declared default: record"));
        assert!(labels.iter().any(|s| s == "declared default: parameter"));
    }
    #[test]
    fn lens_dispatch_rechecks_current_document_snapshot() {
        use openidea_plugin::mock::MockHost;
        let host = MockHost::install();
        let text = "import io.quarkus.scheduler.Scheduled; class Job { @Scheduled(every=\"10s\") void run() {} }";
        host.add_file("src/main/java/Resource.java", text);
        host.open_editor("/project/src/main/java/Resource.java", "java", text, &[]);
        let current = host.invocation().editor().unwrap().document().unwrap();
        let current = Document {
            path: "/project/src/main/java/Resource.java".into(),
            language: "java".into(),
            version: current.version,
            text: current.text,
        };
        let item = lenses(&current).remove(0);
        inspect(&host.invocation(), item.argument.as_deref()).unwrap();
        host.type_in_editor(" ");
        assert!(inspect(&host.invocation(), item.argument.as_deref()).is_err());
        assert!(host.process_runs().is_empty() && host.requests().is_empty());
    }
    #[test]
    fn explicit_quarkus_properties_alias_retains_local_editor_features() {
        use openidea_plugin::{mock::MockHost, providers::Providers, hover::HoverProvider, diagnostics::DiagnosticsProvider, inlay_hints::InlayHints};
        let host=MockHost::install().with_capabilities(&["project.read","events"]);
        host.add_file("pom.xml","<project><dependencies><dependency><groupId>io.quarkus</groupId><artifactId>quarkus-qute</artifactId></dependency></dependencies></project>");
        let d=Document{path:"/project/src/main/resources/application.properties".into(),language:"quarkus-properties".into(),version:1,text:"quarkus.http.port=wrong\n".into()};
        host.add_file(&d.path,&d.text);
        let mut tools=crate::QuarkusTools::default();
        tools.activate_live();tools.warm_live(&d.path,Some(&d.text),std::time::Instant::now()+std::time::Duration::from_secs(1));
        assert!(tools.hover(&d,openidea_plugin::Position::new(0,5)).is_some());
        assert!(!tools.diagnostics(&d).is_empty());
        assert!(!tools.inlay_hints(&d,Range::new(Default::default(),openidea_plugin::Position::new(1,0))).is_empty());
        let prefix=Document{text:"quarkus.http.po".into(),version:2,..d};
        assert!(tools.completion(&prefix,openidea_plugin::Position::new(0,15),None).iter().any(|item|item.label=="quarkus.http.port"));
    }

}
