//! Explicit new configuration source; no classpath, application scan or runtime execution.
use crate::*;
use openidea_plugin::file_actions::{FileAction, FileOperation};
use spring_text::java;
const MAX_TEXT: usize = 12 * 1024;
#[derive(Clone)]
struct Pending {
    root: String,
    doc: Document,
    package: String,
    folder: String,
    link: bool,
}
#[derive(Default)]
pub(crate) struct Creation {
    next: u64,
    pending: BTreeMap<String, Pending>,
}
fn identifier(s: &str) -> bool {
    let mut chars = s.chars();
    s.len()<=128 && chars.next().is_some_and(|c|c.is_ascii_alphabetic() || c=='_') && chars.all(|c|c.is_ascii_alphanumeric() || c=='_')
        && !"_ abstract as assert boolean break byte case catch char class companion const continue data default do double else enum extends false final finally float for fun if implements import in instanceof int interface is long native new null object open out override package permits private protected public record return sealed short static strictfp super switch synchronized this throw throws trait transient true try typealias typeof val var void volatile when while yield by constructor init where get set field property receiver param setparam delegate file dynamic value expect actual inner lateinit tailrec operator infix inline noinline crossinline reified suspend external annotation".split_ascii_whitespace().any(|word|word==s)
}
fn path(s: &str) -> bool {
    s.starts_with('/')
        && s.len() <= 512
        && !s.contains(['\\', '*', '?', '[', ']', '{', '}'])
        && !s.chars().any(char::is_control)
        && !s.split('/').any(|p| matches!(p, "." | ".."))
}
pub(crate) fn context(doc: &Document) -> Option<(String, String)> {
    if doc.text.len() > MAX_TEXT
        || !path(&doc.path)
        || beans::ignored(&doc.path)
        || !spring_text::text::balanced(&doc.text, doc.path.ends_with(".kt"))
        || !matches!(
            (doc.language.as_str(), doc.path.rsplit('.').next()),
            ("java", Some("java")) | ("kotlin", Some("kt"))
        )
    {
        return None;
    }
    let file = spring_text::parse_source(&doc.path, &doc.text);
    if file.package.is_empty()
        || file.package.len() > 512
        || !file.package.split('.').all(identifier)
    {
        return None;
    }
    let tokens = java::tokens(&doc.text);
    if doc.language == "kotlin" && tokens.iter().any(|t| t.s == "typealias" || t.s == "as") {
        return None;
    }
    let integration = file
        .imports
        .iter()
        .any(|i| !i.is_static && i.path.starts_with("org.springframework.integration."))
        || tokens.windows(6).any(|w| {
            w.iter()
                .map(|t| t.s)
                .eq(["org", ".", "springframework", ".", "integration", "."])
        });
    if !integration {
        return None;
    }
    let configs = file
        .types
        .iter()
        .filter(|t| t.outer.is_none() && t.kind == "class")
        .filter(|t| {
            let annotations = t
                .annotations
                .iter()
                .filter(|a| a.name == "Configuration")
                .collect::<Vec<_>>();
            let [a] = annotations.as_slice() else {
                return false;
            };
            let written = &doc.text[a.at + 1..a.name_at + a.name.len()];
            if written == "org.springframework.context.annotation.Configuration" {
                !file.types.iter().any(|t| t.name == "org")
                    && !file
                        .imports
                        .iter()
                        .any(|i| i.path.rsplit('.').next() == Some("org"))
            } else {
                written == "Configuration"
                    && file
                        .imports
                        .iter()
                        .filter(|i| {
                            !i.is_static && i.path.rsplit('.').next() == Some("Configuration")
                        })
                        .map(|i| i.path.as_str())
                        .collect::<Vec<_>>()
                        == ["org.springframework.context.annotation.Configuration"]
                    && !file.types.iter().any(|t| t.name == "Configuration")
            }
        })
        .count();
    if configs != 1 {
        return None;
    }
    let folder = doc.path.rsplit_once('/')?.0;
    if !folder.ends_with(&format!("/{}", file.package.replace('.', "/"))) {
        return None;
    }
    Some((file.package, folder.into()))
}
fn current(pending: &Pending, doc: &Document) -> Result<Project, Error> {
    if doc.path != pending.doc.path
        || doc.language != pending.doc.language
        || doc.version != pending.doc.version
        || doc.text != pending.doc.text
    {
        return Err(Error::Stale);
    }
    let project = host::project_of(&doc.path).ok_or(Error::Stale)?;
    if project.root() != Ok(pending.root.clone())
        || project.read_file(&doc.path)? != doc.text
        || context(doc) != Some((pending.package.clone(), pending.folder.clone()))
    {
        return Err(Error::Stale);
    }
    Ok(project)
}
fn targets(project: &Project, pending: &Pending, class: &str) -> Result<(), Error> {
    for ext in ["java", "kt"] {
        let target = format!("{}/{class}.{ext}", pending.folder);
        if !path(&target)
            || host::project_of(&target)
                .and_then(|p| p.root().ok())
                .as_deref()
                != Some(&pending.root)
        {
            return Err(Error::Stale);
        }
        if project.exists(&target)? {
            return Err("A Java/Kotlin target already exists; nothing is overwritten.".into());
        }
    }
    Ok(())
}
/// An independent fully-qualified Import annotation is added only to one known
/// Configuration without an existing Import or lexical namespace shadow.
fn import_at(doc: &Document) -> Option<usize> {
    let (package, _) = context(doc)?;
    let file = spring_text::parse_source(&doc.path, &doc.text);
    let package_root = package.split('.').next()?;
    let roots = ["org", package_root];
    if java::tokens(&doc.text).windows(2).any(|w| matches!(w[0].s, "val" | "var" | "fun" | "class" | "interface" | "object" | "typealias") && roots.contains(&w[1].s))
        || file.imports.iter().any(|i| i.path.ends_with(".*") || roots.contains(&i.path.rsplit('.').next().unwrap_or("")))
        || file.types.iter().any(|t| roots.contains(&t.name.as_str()) || t.members.iter().any(|m| roots.contains(&m.name.as_str()))) {
        return None;
    }
    let configs: Vec<_> = file.types.iter().filter(|t| t.outer.is_none() && t.kind == "class" && t.annotations.iter().any(|a| a.name == "Configuration")).collect();
    let [config] = configs.as_slice() else { return None; };
    if config.annotations.iter().any(|a| a.name == "Import") || !config.extends.is_empty() || !config.implements.is_empty()
        || doc.text[config.at + config.name.len()..].trim_start().starts_with('<') { return None; }
    config.annotations.iter().find(|a| a.name == "Configuration").map(|a| a.at)
}
fn import_edit(doc: &Document, package: &str, class: &str) -> Option<openidea_plugin::TextEdit> {
    let at = import_at(doc)?;
    let eol = if doc.text.contains("\r\n") { "\r\n" } else { "\n" };
    let prefix = &doc.text[..at];
    let indent = prefix.rsplit('\n').next()?;
    let indent = if indent.chars().all(|c| c == ' ' || c == '\t') { indent } else { "" };
    let literal = if doc.language == "kotlin" { format!("{package}.{class}::class") } else { format!("{package}.{class}.class") };
    Some(openidea_plugin::TextEdit { range: range(&doc.text, at, at), text: format!("@org.springframework.context.annotation.Import({literal}){eol}{indent}") })
}
impl Integration {
    pub(crate) fn source_channel_actions(
        &mut self,
        doc: &Document,
        _selection: Range,
    ) -> Vec<FileAction> {
        let Some((package, folder)) = context(doc) else {
            return Vec::new();
        };
        let Some(root) = host::project_of(&doc.path)
            .and_then(|p| p.root().ok())
            .filter(|r| {
                path(r)
                    && doc
                        .path
                        .starts_with(&format!("{}/", r.trim_end_matches('/')))
            })
        else {
            return Vec::new();
        };
        let mut kinds = vec![(false, "Create DirectChannel configuration source…")];
        if import_at(doc).is_some() { kinds.push((true, "Create and import DirectChannel configuration source…")); }
        if self.source_creation.pending.len() + kinds.len() > 8 { self.source_creation.pending.clear(); }
        let mut actions = Vec::new();
        for (link, title) in kinds {
            let Some(next) = self.source_creation.next.checked_add(1) else { break; };
            self.source_creation.next = next;
            let token = format!("source-channel:{next}:{}", doc.version);
            self.source_creation.pending.insert(token.clone(), Pending { root: root.clone(), doc: doc.clone(), package: package.clone(), folder: folder.clone(), link });
            actions.push(FileAction { title: title.into(), kind: Some("refactor".into()), data: Some(token), ..Default::default() });
        }
        actions
    }
    pub(crate) fn resolve_source_channel(
        &mut self,
        doc: &Document,
        action: FileAction,
    ) -> Result<FileAction, Error> {
        let pending = self
            .source_creation
            .pending
            .remove(action.data.as_deref().ok_or(Error::Stale)?)
            .ok_or(Error::Stale)?;
        let project = current(&pending, doc)?;
        let Some(class) = host::input(
            "New sibling configuration class (same package)",
            Some("AdditionalChannelsConfiguration"),
        )?
        else {
            return Err(Error::Cancelled);
        };
        if !identifier(&class) || matches!(class.as_str(), "org" | "java" | "kotlin") {
            return Err("Choose a new ASCII class identifier up to 128 bytes without keywords or namespace roots.".into());
        }
        current(&pending, doc)?;
        let Some(channel) = host::input(
            "New channel bean/method name (ASCII identifier)",
            Some("newChannel"),
        )?
        else {
            return Err(Error::Cancelled);
        };
        if !identifier(&channel)
            || matches!(
                channel.as_str(),
                "nullChannel" | "errorChannel" | "org" | "java" | "kotlin"
            )
        {
            return Err(
                "Choose a nonreserved ASCII channel/method identifier up to 128 bytes.".into(),
            );
        }
        current(&pending, doc)?;
        targets(&project, &pending, &class)?;
        let inventory = crate::file_actions::inventory(&project, doc)?;
        let config_id = spring_text::decapitalize(&class);
        let package_root = pending.package.split('.').next().ok_or(Error::Stale)?;
        if pending.link && (class == package_root || inventory.types.contains(&(pending.package.clone(), package_root.into()))) {
            return Err("A source type shadows the package root used by the explicit configuration import.".into());
        }
        if config_id == channel
            || inventory.ids.contains(&channel)
            || inventory.ids.contains(&config_id)
            || [class.as_str(), "org", "java", "kotlin"]
                .iter()
                .any(|name| {
                    inventory
                        .types
                        .contains(&(pending.package.clone(), (*name).into()))
                })
        {
            return Err("A possible source name/type collision or generated namespace shadow prevents this creation.".into());
        }
        let (extension, text) = if doc.language == "kotlin" {
            (
                "kt",
                format!(
                    "package {}\n\n@org.springframework.context.annotation.Configuration(\"{config_id}\")\nopen class {class} {{\n    @org.springframework.context.annotation.Bean\n    open fun {channel}(): org.springframework.messaging.MessageChannel =\n        org.springframework.integration.channel.DirectChannel()\n}}\n",
                    pending.package
                ),
            )
        } else {
            (
                "java",
                format!(
                    "package {};\n\n@org.springframework.context.annotation.Configuration(\"{config_id}\")\npublic class {class} {{\n    @org.springframework.context.annotation.Bean\n    public org.springframework.messaging.MessageChannel {channel}() {{\n        return new org.springframework.integration.channel.DirectChannel();\n    }}\n}}\n",
                    pending.package
                ),
            )
        };
        let target = format!("{}/{class}.{extension}", pending.folder);
        let text = if doc.text.contains("\r\n") {
            text.replace('\n', "\r\n")
        } else {
            text
        };
        let edits = if pending.link {
            vec![(doc.path.clone(), vec![import_edit(doc, &pending.package, &class).ok_or(Error::Stale)?])]
        } else { Vec::new() };
        let mut after = doc.text.clone();
        if let Some((_, source_edits)) = edits.first() {
            let edit = &source_edits[0];
            let at = openidea_plugin::text::offset(&doc.text, edit.range.start).ok_or(Error::Stale)?;
            after.insert_str(at, &edit.text);
        }
        let source_preview = if pending.link { format!("\n\nAlso add one explicit Import annotation to the existing Configuration. Exact resulting source ({} bytes):\n{after}", after.len()) } else { "\n\nExisting source is unchanged; component-scan/import registration is unresolved.".into() };
        let preview = format!(
            "Create: {target}\nSource context: {}\nSource version: {}\nPackage: {}\nConfiguration bean: {config_id}\nChannel bean: {channel}\n\nRequires Spring context/messaging/integration classes; classpath and effective application configuration are not resolved. No runtime code executes now. Source collisions are bounded lexical checks, not an effective context.\n\nExact generated source ({} bytes):\n{text}{source_preview}",
            doc.path, doc.version, pending.package, text.len()
        );
        if !ui_kit::prompts::confirm_exact(if pending.link { "Create and import channel configuration source?" } else { "Create channel configuration source?" }, &preview)? { return Err(Error::Cancelled); }
        current(&pending, doc)?;
        targets(&project, &pending, &class)?;
        if crate::file_actions::inventory(&project, doc)? != inventory {
            return Err(Error::Stale);
        }
        current(&pending, doc)?;
        Ok(FileAction {
            title: format!("Create {}{class} with {channel}", if pending.link { "and import " } else { "" }),
            kind: Some("refactor".into()),
            edits,
            operations: vec![FileOperation::Create {
                path: target.clone(),
                text,
                overwrite: false,
            }],
            open: Some(target),
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{file_actions::FileActions, mock::MockHost};
    const PATH: &str = "/project/src/app/SourceConfig.java";
    #[test]
    fn bracket_check_ignores_literals_and_comments_and_rejects_unpaired_source() {
        for (text, kotlin, ok) in [
            ("class A { void f(int[] a) { g(\"}\"); } }", false, true),
            ("class A { /* { */ char c = '{'; // (\n}", false, true),
            ("class A { void f( { } }", false, false),
            ("class A { String s = \"open; }", false, false),
            ("class A { /* /* */ */ }", true, true),
            ("class A { }}", false, false),
        ] {
            assert_eq!(spring_text::text::balanced(text, kotlin), ok, "{text}");
        }
    }
    fn doc() -> Document {
        Document{path:PATH.into(),language:"java".into(),version:4,text:"package app;\nimport org.springframework.context.annotation.Configuration;\nimport org.springframework.integration.dsl.IntegrationFlow;\n@Configuration public class SourceConfig {}\n".into()}
    }
    fn setup(d: &Document) -> MockHost {
        let h = MockHost::install();
        h.add_file(&d.path, &d.text);
        h
    }
    fn action(owner: &mut impl FileActions, d: &Document) -> FileAction {
        owner
            .file_actions(d, Range::default())
            .into_iter()
            .find(|a| a.title.starts_with("Create DirectChannel"))
            .unwrap()
    }
    fn answer(host: &MockHost, class: &str, channel: &str) {
        host.answer_next_prompt(Some(&[class]));
        host.answer_next_prompt(Some(&[channel]));
        host.answer_next_prompt(Some(&["yes"]));
    }
    fn linked(owner: &mut impl FileActions, d: &Document) -> FileAction {
        owner.file_actions(d, Range::default()).into_iter().find(|a| a.title.starts_with("Create and import")).expect("linked source action")
    }
    fn apply_source(d: &Document, action: &FileAction) -> String {
        let [(path, edits)] = action.edits.as_slice() else { panic!("one source edit"); };
        assert_eq!(path, &d.path);
        let [edit] = edits.as_slice() else { panic!("one annotation"); };
        assert_eq!(edit.range.start, edit.range.end);
        let mut text = d.text.clone();
        text.insert_str(openidea_plugin::text::offset(&d.text, edit.range.start).unwrap(), &edit.text);
        text
    }
    #[test]
    fn registered_linked_java_returns_one_create_and_one_exact_configuration_import_edit() {
        let d = doc(); let h = setup(&d); let mut owner = structure::Structure::default();
        let lazy = linked(&mut owner, &d); answer(&h, "ExtraChannels", "orders");
        let resolved = owner.resolve_file_action(&d, lazy).unwrap();
        let [FileOperation::Create { path, text, overwrite: false }] = resolved.operations.as_slice() else { panic!("nonoverwriting create"); };
        assert_eq!(path, "/project/src/app/ExtraChannels.java");
        assert!(text.contains("MessageChannel orders()"));
        let after = apply_source(&d, &resolved);
        assert_eq!(after, d.text.replace("@Configuration", "@org.springframework.context.annotation.Import(app.ExtraChannels.class)\n@Configuration"));
        let file = spring_text::parse_source(&d.path, &after);
        assert_eq!(file.types[0].annotations.iter().filter(|a| a.name == "Import").count(), 1);
        assert_eq!(h.invocation().project().unwrap().read_file(PATH).unwrap(), d.text);
        assert!(!h.invocation().project().unwrap().exists(path).unwrap());
        assert!(h.requests().is_empty() && h.process_runs().is_empty());
    }
    #[test]
    fn linked_kotlin_uses_qualified_class_literal_and_preserves_crlf_indentation_unicode() {
        let d = Document { path: "/project/src/app/SourceConfig.kt".into(), language: "kotlin".into(), text: "package app\r\nimport org.springframework.context.annotation.Configuration\r\nimport org.springframework.integration.dsl.IntegrationFlow\r\n// 😀 café\r\n  @Configuration open class SourceConfig {}\r\n".into(), ..doc() };
        let h = setup(&d); let mut owner = structure::Structure::default();
        let lazy = linked(&mut owner, &d); answer(&h, "ExtraChannels", "orders");
        let resolved = owner.resolve_file_action(&d, lazy).unwrap();
        let after = apply_source(&d, &resolved);
        assert!(after.contains("  @org.springframework.context.annotation.Import(app.ExtraChannels::class)\r\n  @Configuration"));
        assert!(after.contains("😀 café")); assert!(!after.replace("\r\n", "").contains('\n'));
        assert!(matches!(resolved.operations.as_slice(), [FileOperation::Create { path, text, overwrite: false }] if path.ends_with(".kt") && text.contains("open class ExtraChannels")));
    }
    #[test]
    fn linked_action_refuses_existing_import_and_unknown_namespace_bindings_but_keeps_create_only() {
        for prefix in [
            "@org.springframework.context.annotation.Import(app.Other.class)\n",
            "@Import(Other.class)\n",
            "@other.Import(Other.class)\n",
        ] {
            let d = Document { text: doc().text.replace("@Configuration", &format!("{prefix}@Configuration")), ..doc() };
            let _h = setup(&d); let mut owner = structure::Structure::default();
            let actions = owner.file_actions(&d, Range::default());
            assert!(actions.iter().any(|a| a.title.starts_with("Create DirectChannel")));
            assert!(!actions.iter().any(|a| a.title.starts_with("Create and import")));
        }
        for addition in ["import other.*;\n", "import other.org;\n", "import other.app;\n", "class org {}\n", "class app {}\n"] {
            let d = Document { text: doc().text.replace("@Configuration", &format!("{addition}@Configuration")), ..doc() };
            let _h = setup(&d); let mut owner = structure::Structure::default();
            assert!(!owner.file_actions(&d, Range::default()).iter().any(|a| a.title.starts_with("Create and import")), "{addition}");
        }
    }
    #[test]
    fn linked_import_refuses_generic_inherited_and_cross_file_package_root_shadowing() {
        for declaration in ["class SourceConfig<app>", "class SourceConfig extends Base", "class SourceConfig implements Marker"] {
            let d = Document { text: doc().text.replace("class SourceConfig", declaration), ..doc() };
            let _h = setup(&d); let mut owner = structure::Structure::default();
            assert!(!owner.file_actions(&d, Range::default()).iter().any(|a| a.title.starts_with("Create and import")), "{declaration}");
        }
        for name in ["app", "ExtraChannels"] {
            let d = doc(); let h = setup(&d); let mut owner = structure::Structure::default();
            if name == "ExtraChannels" { h.add_file("other/Shadow.java", "package app; class app {}"); }
            let lazy = linked(&mut owner, &d); answer(&h, name, "orders");
            assert!(owner.resolve_file_action(&d, lazy).is_err());
            assert_eq!(h.invocation().project().unwrap().read_file(PATH).unwrap(), d.text);
        }
    }
    #[test]
    fn linked_cancellation_at_each_prompt_is_atomic_and_token_is_single_use() {
        for accepted in 0..3 {
            let d = doc(); let h = setup(&d); let mut owner = structure::Structure::default();
            let lazy = linked(&mut owner, &d); let replay = lazy.clone();
            if accepted >= 1 { h.answer_next_prompt(Some(&["ExtraChannels"])); }
            if accepted >= 2 { h.answer_next_prompt(Some(&["orders"])); }
            assert!(matches!(owner.resolve_file_action(&d, lazy), Err(Error::Cancelled)));
            assert!(matches!(owner.resolve_file_action(&d, replay), Err(Error::Stale)));
            assert_eq!(h.invocation().project().unwrap().read_file(PATH).unwrap(), d.text);
            assert!(!h.invocation().project().unwrap().exists("/project/src/app/ExtraChannels.java").unwrap());
        }
    }
    #[test]
    fn linked_source_version_and_target_collision_refuse_the_whole_operation() {
        let d = doc(); let h = setup(&d); let mut owner = structure::Structure::default();
        let lazy = linked(&mut owner, &d);
        assert!(matches!(owner.resolve_file_action(&Document { version: d.version + 1, ..d.clone() }, lazy), Err(Error::Stale)));
        let lazy = linked(&mut owner, &d); h.add_file("src/app/ExtraChannels.kt", "user data");
        answer(&h, "ExtraChannels", "orders"); assert!(owner.resolve_file_action(&d, lazy).is_err());
        assert_eq!(h.invocation().project().unwrap().read_file(PATH).unwrap(), d.text);
        assert_eq!(h.invocation().project().unwrap().read_file("/project/src/app/ExtraChannels.kt").unwrap(), "user data");
    }
    #[test]
    fn registered_java_creation_is_one_nonoverwriting_atomic_create_and_preserves_source() {
        let d = doc();
        let h = setup(&d);
        let mut owner = structure::Structure::default();
        let lazy = action(&mut owner, &d);
        answer(&h, "ExtraChannels", "orders");
        let resolved = owner.resolve_file_action(&d, lazy).unwrap();
        assert!(resolved.edits.is_empty());
        let [
            FileOperation::Create {
                path,
                text,
                overwrite: false,
            },
        ] = resolved.operations.as_slice()
        else {
            panic!("atomic create")
        };
        assert_eq!(path, "/project/src/app/ExtraChannels.java");
        assert_eq!(resolved.open.as_deref(), Some(path.as_str()));
        assert!(text.contains("Configuration(\"extraChannels\")"));
        assert!(text.contains("MessageChannel orders()"));
        assert!(text.contains("new org.springframework.integration.channel.DirectChannel()"));
        assert_eq!(
            h.invocation().project().unwrap().read_file(PATH).unwrap(),
            d.text
        );
        assert!(!h.invocation().project().unwrap().exists(path).unwrap());
        assert!(h.requests().is_empty() && h.process_runs().is_empty());
    }
    #[test]
    fn kotlin_generation_is_open_without_allopen_plugin_and_keeps_crlf_style() {
        let d=Document{path:"/project/src/app/SourceConfig.kt".into(),language:"kotlin".into(),text:"package app\r\nimport org.springframework.context.annotation.Configuration\r\nimport org.springframework.integration.dsl.IntegrationFlow\r\n@Configuration open class SourceConfig {}\r\n".into(),..doc()};
        let h = setup(&d);
        let mut owner = Integration::default();
        let lazy = action(&mut owner, &d);
        answer(&h, "ExtraChannels", "orders");
        let resolved = owner.resolve_file_action(&d, lazy).unwrap();
        let [FileOperation::Create { path, text, .. }] = resolved.operations.as_slice() else {
            panic!("create")
        };
        assert!(path.ends_with("ExtraChannels.kt"));
        assert!(text.contains("open class ExtraChannels"));
        assert!(text.contains("open fun orders()"));
        assert!(!text.replace("\r\n", "").contains('\n'));
    }
    #[test]
    fn source_context_alias_namespace_package_and_bounds_do_not_guess_applicability() {
        let h = MockHost::install();
        let mut owner = Integration::default();
        let d = doc();
        for changed in [
            d.text.replace(
                "org.springframework.context.annotation.Configuration",
                "other.Configuration",
            ),
            d.text.replace("@Configuration", "// @Configuration"),
            d.text.replace(
                "org.springframework.integration.dsl.IntegrationFlow",
                "other.IntegrationFlow",
            ),
            d.text.replace(
                "class SourceConfig",
                "class Configuration {} class SourceConfig",
            ),
            d.text.replace("package app;", "package different;"),
            format!("{}{}", d.text, " ".repeat(MAX_TEXT)),
        ] {
            let wrong = Document {
                text: changed,
                ..d.clone()
            };
            assert!(owner.file_actions(&wrong, Range::default()).is_empty());
        }
        let _denied = h.with_capabilities(&[]);
        assert!(owner.file_actions(&d, Range::default()).is_empty());
    }
    #[test]
    fn cancels_and_unknown_or_replayed_tokens_never_produce_source_operations() {
        for count in 0..3 {
            let d = doc();
            let h = setup(&d);
            let mut owner = Integration::default();
            let lazy = action(&mut owner, &d);
            let replay = lazy.clone();
            if count >= 1 {
                h.answer_next_prompt(Some(&["ExtraChannels"]));
            }
            if count >= 2 {
                h.answer_next_prompt(Some(&["orders"]));
            }
            assert!(matches!(
                owner.resolve_file_action(&d, lazy),
                Err(Error::Cancelled)
            ));
            assert!(matches!(
                owner.resolve_file_action(&d, replay),
                Err(Error::Stale)
            ));
            assert!(
                owner
                    .resolve_file_action(
                        &d,
                        FileAction {
                            data: Some("source-channel:forged".into()),
                            ..Default::default()
                        }
                    )
                    .is_err()
            );
        }
    }
    #[test]
    fn stale_document_dirty_text_and_project_owner_refuse_resolution() {
        let d = doc();
        let h = setup(&d);
        let mut owner = Integration::default();
        let lazy = action(&mut owner, &d);
        assert!(matches!(
            owner.resolve_file_action(
                &Document {
                    version: 5,
                    ..d.clone()
                },
                lazy
            ),
            Err(Error::Stale)
        ));
        let lazy = action(&mut owner, &d);
        h.open_editor(PATH, "java", &format!("{}// unsaved", d.text), &[]);
        assert!(matches!(
            owner.resolve_file_action(&d, lazy),
            Err(Error::Stale)
        ));
        h.open_editor(PATH, "java", &d.text, &[]);
        let lazy = action(&mut owner, &d);
        h.set_project_root(Some("/other"));
        assert!(owner.resolve_file_action(&d, lazy).is_err());
    }
    #[test]
    fn file_directory_type_and_source_bean_collisions_are_all_refused() {
        for collision in 0..5 {
            let d = doc();
            let h = setup(&d);
            let mut owner = Integration::default();
            match collision {
                0 => h.add_file("src/app/ExtraChannels.java", "existing"),
                1 => h.add_dir("src/app/ExtraChannels.kt"),
                2 => h.add_file(
                    "other-root/Other.java",
                    "package app; class ExtraChannels {}",
                ),
                3 => h.add_file(
                    "other-root/Alias.kt",
                    "package app\ntypealias ExtraChannels = String",
                ),
                _ => h.add_file(
                    "other-root/Beans.java",
                    "package x; class Beans { @Bean String orders(){return null;} }",
                ),
            }
            let lazy = action(&mut owner, &d);
            answer(&h, "ExtraChannels", "orders");
            assert!(
                owner.resolve_file_action(&d, lazy).is_err(),
                "case{collision}"
            );
        }
    }
    #[test]
    fn dynamic_or_concatenated_bean_names_cannot_be_treated_as_default_method_name() {
        for value in ["CONST", "{\"known\", CONST}", "\"or\" + \"ders\""] {
            let d = doc();
            let h = setup(&d);
            h.add_file("Other.java",&format!("package other; class Other {{ @Bean(name={value}) String factory() {{return null;}} }}"));
            let mut owner = Integration::default();
            let lazy = action(&mut owner, &d);
            answer(&h, "ExtraChannels", "orders");
            assert!(owner.resolve_file_action(&d, lazy).is_err());
        }
    }
    #[test]
    fn invalid_identifiers_reserved_ids_and_namespace_shadows_never_inject_generated_source() {
        for (class, channel) in [
            ("../Escape", "orders"),
            ("class", "orders"),
            ("ExtraChannels", "errorChannel"),
            ("ExtraChannels", "evil();"),
            ("org", "orders"),
        ] {
            let d = doc();
            let h = setup(&d);
            let mut owner = Integration::default();
            let lazy = action(&mut owner, &d);
            answer(&h, class, channel);
            assert!(owner.resolve_file_action(&d, lazy).is_err());
        }
        let d = doc();
        let h = setup(&d);
        h.add_file("Other.java", "package app; class org {}");
        let mut owner = Integration::default();
        let lazy = action(&mut owner, &d);
        answer(&h, "ExtraChannels", "orders");
        assert!(owner.resolve_file_action(&d, lazy).is_err());
    }
    #[test]
    fn existing_xml_channel_creation_uses_the_same_unknown_bean_name_guard() {
        let h = MockHost::install();
        let d = Document {
            path: "/project/channels.xml".into(),
            language: "xml".into(),
            version: 1,
            text: format!(
                "<beans xmlns='http://www.springframework.org/schema/beans' xmlns:int='{NAMESPACE}'>\n</beans>"
            ),
        };
        h.add_file(&d.path, &d.text);
        h.add_file(
            "Other.java",
            "package other; class Other { @Bean(name=CONST) String factory(){return null;} }",
        );
        let mut owner = structure::Structure::default();
        let lazy = owner.file_actions(&d, Range::default()).remove(0);
        h.answer_next_prompt(Some(&["orders"]));
        assert!(owner.resolve_file_action(&d, lazy).is_err());
        assert_eq!(
            h.invocation()
                .project()
                .unwrap()
                .read_file(&d.path)
                .unwrap(),
            d.text
        );
    }
}
