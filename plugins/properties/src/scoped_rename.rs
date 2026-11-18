//! Reviewed finite bundle + source-owned local JDK literal key updates.
use super::*;
use openidea_plugin::{FileKind, FileStat, file_actions::FileAction};
use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};
const MAX_FILES: usize = 400;
const MAX_SOURCE: usize = 128 << 10;
const MAX_TOTAL: usize = 8 << 20;
const MAX_MATCHES: usize = 128;
#[derive(Clone, Debug, PartialEq, Eq)]
struct Scope {
    module: String,
    set: &'static str,
}
fn scope(path: &str) -> Option<Scope> {
    let mut choices = [
        ("/src/main/resources/", "main"),
        ("/src/test/resources/", "test"),
    ]
    .into_iter()
    .filter_map(|(marker, set)| {
        path.rsplit_once(marker).map(|(module, _)| Scope {
            module: module.into(),
            set,
        })
    });
    let result = choices.next()?;
    if choices.next().is_some() {
        return None;
    }
    Some(result)
}
pub(super) fn offered(doc: &Document) -> bool {
    scope(&doc.path).is_some()
}
fn safe_new_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 512
        && key
            .bytes()
            .all(|b| (b' '..=b'~').contains(&b) && !b"\"\\$".contains(&b))
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct SourceFile {
    path: String,
    text: String,
    stat: FileStat,
    spans: Vec<Span<usize>>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Sources {
    files: Vec<SourceFile>,
    matches: usize,
}
fn live_owner(path: &str, root: &str) -> bool {
    host::project_of(path)
        .and_then(|p| p.root().ok())
        .as_deref()
        == Some(root)
}
fn collect(project: &Project, root: &str, doc: &Document, key: &str) -> Result<Sources, Error> {
    if project.root()?.as_str() != root {
        return Err(Error::Stale);
    }
    let owned = usages::SourceOwnership::current(root).ok_or(Error::Stale)?;
    if !owned.contains(&doc.path) || !live_owner(&doc.path, root) {
        return Err(Error::Stale);
    }
    let scope = scope(&doc.path)
        .ok_or("Local JDK updates require a conventional main/test resource bundle.")?;
    let relative = scope
        .module
        .strip_prefix(root)
        .filter(|rest| rest.is_empty() || rest.starts_with('/'))
        .ok_or(Error::Stale)?
        .trim_start_matches('/');
    let prefix = if relative.is_empty() {
        String::new()
    } else {
        format!("{relative}/")
    };
    let mut paths = BTreeSet::new();
    for (language, extension) in [("java", "java"), ("kotlin", "kt")] {
        let found = project.find_files(
            &format!("{prefix}src/{}/{language}/**/*.{extension}", scope.set),
            (MAX_FILES + 1) as u32,
        )?;
        if found.len() > MAX_FILES {
            return Err(
                "Local source inventory exceeds 400 files; no partial rename is prepared.".into(),
            );
        }
        paths.extend(found);
        if paths.len() > MAX_FILES {
            return Err(
                "Local source inventory exceeds 400 files; no partial rename is prepared.".into(),
            );
        }
    }
    let started = Instant::now();
    let mut total = 0usize;
    let mut result = Sources {
        files: vec![],
        matches: 0,
    };
    for path in paths {
        if started.elapsed() > Duration::from_secs(1) {
            return Err(
                "Local source scan exceeded its time limit; no partial rename is prepared.".into(),
            );
        }
        let allowed = [("java", "java"), ("kotlin", "kt")]
            .iter()
            .any(|(language, extension)| {
                path.starts_with(&format!("{}/src/{}/{language}/", scope.module, scope.set))
                    && path.ends_with(&format!(".{extension}"))
            });
        if !allowed
            || !owned.contains(&path)
            || !live_owner(&path, root)
            || path[scope.module.len() + 1..]
                .split('/')
                .any(|p| p.starts_with('.') || ["vendor", "node_modules", "storage"].contains(&p))
        {
            return Err(Error::Stale);
        }
        let stat = project.stat(&path)?;
        if stat.kind != FileKind::File
            || stat.size > MAX_SOURCE as u64
            || stat.size > (MAX_TOTAL - total) as u64
        {
            return Err("A source file is too large to update (over 128 KiB, or over 8 MiB in total), so nothing was renamed.".into());
        }
        let text = project.read_file(&path)?;
        if text.len() > MAX_SOURCE || text.len() > MAX_TOTAL - total {
            return Err("Local source byte limit exceeded; no partial rename is prepared.".into());
        }
        total += text.len();
        let source = Document {
            path: path.clone(),
            language: if path.ends_with(".kt") {
                "kotlin"
            } else {
                "java"
            }
            .into(),
            version: 0,
            text: text.clone(),
        };
        let mut spans = Vec::new();
        let mut occurrences = text.match_indices(key);
        for _ in 0..MAX_MATCHES {
            let Some((at, _)) = occurrences.next() else {
                break;
            };
            if started.elapsed() > Duration::from_secs(1) {
                return Err(
                    "Local source scan exceeded its time limit; no partial rename is prepared."
                        .into(),
                );
            }
            let Some(context) = source::context(&source, at) else {
                continue;
            };
            if context.indirect && context.contains(&doc.path) && text.get(context.span.clone()) == Some(key) {
                return Err("The key is held by a source constant or @PropertyKey parameter; rename it manually. No partial rename is prepared.".into());
            }
            if context.contains(&doc.path)
                && text.get(context.span.clone()) == Some(key)
                && !spans.contains(&context.span)
            {
                spans.push(context.span);
                result.matches += 1;
                if result.matches > MAX_MATCHES {
                    return Err("More than 128 supported literal references; no partial rename is prepared.".into());
                }
            }
        }
        if occurrences.next().is_some() {
            return Err(
                "More than 128 key mentions in one source; no partial rename is prepared.".into(),
            );
        }
        if started.elapsed() > Duration::from_secs(1) {
            return Err(
                "Local source scan exceeded its time limit; no partial rename is prepared.".into(),
            );
        }
        spans.sort_by_key(|s| s.start);
        if spans.windows(2).any(|p| p[0].end > p[1].start) {
            return Err(Error::Stale);
        }
        result.files.push(SourceFile {
            path,
            text,
            stat,
            spans,
        });
    }
    Ok(result)
}
fn same_sources(
    project: &Project,
    root: &str,
    doc: &Document,
    key: &str,
    before: &Sources,
) -> Result<(), Error> {
    if &collect(project, root, doc, key)? != before {
        return Err(Error::Stale);
    }
    Ok(())
}
fn plan(
    bundle: &[(String, String)],
    sources: &Sources,
    key: &str,
    new: &str,
) -> Result<Vec<(String, Vec<TextEdit>)>, Error> {
    let escaped = props::escape_key(new);
    if props::unescape_exact(&escaped).as_deref() != Some(new) {
        return Err(Error::Stale);
    }
    let mut edits = Vec::new();
    for (path, text) in bundle {
        let properties = parse(text);
        if properties
            .iter()
            .any(|p| p.exact_key(text).as_deref() == Some(new))
        {
            return Err("The new key already exists in a locale; no files were changed.".into());
        }
        let old: Vec<_> = properties
            .iter()
            .filter(|p| p.exact_key(text).as_deref() == Some(key))
            .collect();
        if old.len() > 1 {
            return Err("Resolve duplicate old keys before renaming this bundle.".into());
        }
        if let Some(p) = old.first() {
            edits.push((
                path.clone(),
                to_edits(text, vec![(p.key.clone(), escaped.clone())]),
            ));
        }
    }
    for file in &sources.files {
        if !file.spans.is_empty() {
            edits.push((
                file.path.clone(),
                to_edits(
                    &file.text,
                    file.spans
                        .iter()
                        .cloned()
                        .map(|s| (s, new.into()))
                        .collect(),
                ),
            ));
        }
    }
    edits.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(edits)
}
fn preview(
    root: &str,
    key: &str,
    new: &str,
    bundle: &[(String, String)],
    sources: &Sources,
    edits: &[(String, Vec<TextEdit>)],
) -> Result<String, Error> {
    let mut text = format!(
        "Project: {}\nDecoded key: {} → {}\n\nOnly listed visible bundle declarations and {} proven local JDK literals change. All admitted visible same-module/same-source-set Java/Kotlin files were inspected. Ignored files, other modules/source sets, dynamic/framework/unknown references and runtime classloader identity remain unresolved. This is scoped source Rename, not global Rename, safe-delete or move.\n\n",
        serde_json::to_string(root).unwrap_or_default(),
        serde_json::to_string(key).unwrap_or_default(),
        serde_json::to_string(new).unwrap_or_default(),
        sources.matches
    );
    for (path, changes) in edits {
        let source = bundle
            .iter()
            .find(|(p, _)| p == path)
            .map(|(_, t)| t.as_str())
            .or_else(|| {
                sources
                    .files
                    .iter()
                    .find(|f| f.path == *path)
                    .map(|f| f.text.as_str())
            })
            .ok_or(Error::Stale)?;
        text.push_str(&format!(
            "{}\n",
            serde_json::to_string(path).unwrap_or_default()
        ));
        for edit in changes {
            let start = offset(source, edit.range.start).ok_or(Error::Stale)?;
            let end = offset(source, edit.range.end).ok_or(Error::Stale)?;
            let old = source.get(start..end).ok_or(Error::Stale)?;
            text.push_str(&format!(
                "{}:{}–{}:{}\nBefore source:\n{}\n\nAfter source:\n{}\n\n",
                edit.range.start.line + 1,
                edit.range.start.character + 1,
                edit.range.end.line + 1,
                edit.range.end.character + 1,
                ui_kit::prompts::readable_source(old),
                ui_kit::prompts::readable_source(&edit.text)
            ));
        }
    }
    if text.len() > ui_kit::prompts::MAX_CONFIRM_BYTES {
        return Err(
            "The complete locale/source preview exceeds 16 KiB; no partial rename is prepared."
                .into(),
        );
    }
    Ok(text)
}
pub(super) fn resolve(
    doc: &Document,
    action: FileAction,
    root: &str,
    bundle: Vec<(String, String)>,
    key: &str,
) -> Result<FileAction, Error> {
    if key.is_empty()
        || key.len() > 512
        || key.chars().any(char::is_control)
        || parse(&doc.text)
            .iter()
            .filter(|p| p.exact_key(&doc.text).as_deref() == Some(key))
            .count()
            != 1
    {
        return Err("Choose one unambiguous lossless property key.".into());
    }
    let new = host::input(
        "New key (plain ASCII; updates shown local JDK literals)",
        Some(key),
    )?
    .ok_or(Error::Cancelled)?;
    if !safe_new_key(&new) || new == key {
        return Err("Use a different nonempty printable ASCII key of at most 512 bytes, without quotes, backslashes or dollar signs.".into());
    }
    atomic::unchanged(doc, root, &bundle)?;
    let project = project_for(&doc.path)?;
    let sources = collect(&project, root, doc, key)?;
    if sources.matches == 0 {
        return Err("No proven local JDK literal references found; use the separate bundle-only action if intended. Unknown/ignored/framework references are not resolved.".into());
    }
    let edits = plan(&bundle, &sources, key, &new)?;
    let review = preview(root, key, &new, &bundle, &sources, &edits)?;
    if !ui_kit::prompts::confirm_exact("Rename bundle key and local JDK literals?", &review)? {
        return Err(Error::Cancelled);
    }
    atomic::unchanged(doc, root, &bundle)?;
    same_sources(&project, root, doc, key, &sources)?;
    if plan(&bundle, &sources, key, &new)? != edits {
        return Err(Error::Stale);
    }
    Ok(FileAction {
        title: action.title,
        kind: action.kind,
        edits,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{
        file_actions::{FileAction, FileActions},
        mock::MockHost,
    };
    const JAVA: &str = "class A { String f() { /* 😀 */ return java.util.ResourceBundle.getBundle(\"messages\").getString(\"hello\"); } }\r\n";
    fn setup() -> (MockHost, Properties, Document) {
        let host = MockHost::install().with_capabilities(&["project.read", "editor.read"]);
        let doc = Document {
            path: "/project/src/main/resources/messages.properties".into(),
            language: "properties".into(),
            version: 17,
            text: "# Keep\nhel\\u006Co : World\n".into(),
        };
        host.add_file(&doc.path, &doc.text);
        host.add_file(
            "/project/src/main/resources/messages_fr.properties",
            "hello=Bonjour\nkeep=Stay\n",
        );
        host.add_file("/project/src/main/java/A.java", JAVA);
        (host, Properties::default(), doc)
    }
    fn action(plugin: &mut Properties, doc: &Document) -> FileAction {
        plugin
            .file_actions(doc, Range::new(Position::new(1, 1), Position::new(1, 1)))
            .into_iter()
            .find(|a| a.title == "Change bundle key and local JDK literals…")
            .expect("missing requested reference-updating action")
    }
    #[test]
    fn registered_scoped_rename_reviews_all_locales_and_proven_source_literals_without_writes() {
        let (host, mut plugin, doc) = setup();
        let lazy = action(&mut plugin, &doc);
        host.answer_next_prompt(Some(&["fresh:key"]));
        host.answer_next_prompt(Some(&["yes"]));
        let ready = plugin.resolve_file_action(&doc, lazy).unwrap();
        assert_eq!(ready.edits.len(), 3);
        assert!(ready.operations.is_empty());
        let source = ready
            .edits
            .iter()
            .find(|(p, _)| p == "/project/src/main/java/A.java")
            .expect("proven source update");
        assert_eq!(source.1.len(), 1);
        assert_eq!(source.1[0].text, "fresh:key");
        let start = offset(JAVA, source.1[0].range.start).unwrap();
        let end = offset(JAVA, source.1[0].range.end).unwrap();
        assert_eq!(&JAVA[start..end], "hello");
        assert_eq!(
            source.1[0].range.start.character as usize,
            JAVA[..start].encode_utf16().count()
        );
        for (path, edits) in &ready.edits {
            if path.ends_with(".properties") {
                assert_eq!(
                    props::unescape_exact(&edits[0].text).as_deref(),
                    Some("fresh:key")
                );
            }
        }
        let project = project_for(&doc.path).unwrap();
        assert_eq!(project.read_file(&doc.path).unwrap(), doc.text);
        assert_eq!(
            project.read_file("/project/src/main/java/A.java").unwrap(),
            JAVA
        );
        assert!(host.process_runs().is_empty());
    }
    #[test]
    fn all_proven_java_kotlin_sites_update_but_unknown_other_bundle_and_module_literals_do_not() {
        let (host, mut plugin, doc) = setup();
        host.add_file("/project/src/main/java/B.java","import java.util.ResourceBundle; class B { String f(){ final ResourceBundle bundle=ResourceBundle.getBundle(\"messages\"); return bundle.getString(\"hello\"); } }");
        host.add_file("/project/src/main/kotlin/C.kt","import java.util.ResourceBundle\nfun f(): String { val bundle=ResourceBundle.getBundle(\"messages\")\nreturn bundle.getString(\"hello\") }");
        for (path, text) in [
            (
                "/project/src/main/java/Unknown.java",
                "class U { String f(){return unknown.getString(\"hello\");}}",
            ),
            (
                "/project/src/main/java/Other.java",
                "class O { String f(){return java.util.ResourceBundle.getBundle(\"other\").getString(\"hello\");}}",
            ),
            ("/project/other/src/main/java/Other.java", JAVA),
            ("/project/src/test/java/Other.java", JAVA),
        ] {
            host.add_file(path, text);
        }
        let lazy = action(&mut plugin, &doc);
        host.answer_next_prompt(Some(&["fresh"]));
        host.answer_next_prompt(Some(&["yes"]));
        let ready = plugin.resolve_file_action(&doc, lazy).unwrap();
        assert_eq!(ready.edits.len(), 5);
        assert!(ready.edits.iter().any(|(p, _)| p.ends_with("B.java")));
        assert!(ready.edits.iter().any(|(p, _)| p.ends_with("C.kt")));
        assert!(
            ready
                .edits
                .iter()
                .all(|(p, _)| !p.contains("Other") && !p.contains("Unknown"))
        );
    }
    #[test]
    fn invalid_names_collisions_duplicates_cancel_version_and_owners_never_prepare_partial_edits() {
        for case in 0..13 {
            let (host, mut plugin, mut doc) = setup();
            let lazy = action(&mut plugin, &doc);
            let new = match case {
                0 => "hello",
                1 => "",
                2 => "café",
                3 => "bad\"quote",
                4 => "bad\\slash",
                5 => "bad$dollar",
                6 => "bad\nline",
                _ => "fresh",
            };
            match case {
                7 => host.add_file(
                    "/project/src/main/resources/messages_fr.properties",
                    "fresh=Collision\nhello=Bonjour\n",
                ),
                8 => host.add_file(
                    "/project/src/main/resources/messages_fr.properties",
                    "hello=One\nhel\\u006Co=Two\n",
                ),
                9 => doc.version += 1,
                10 => host.add_project("/project/src/main/java"),
                _ => {}
            }
            if case == 11 {
                host.answer_next_prompt(None);
            } else {
                host.answer_next_prompt(Some(&[new]));
                if case == 12 {
                    host.answer_next_prompt(None);
                } else {
                    host.answer_next_prompt(Some(&["yes"]));
                }
            }
            assert!(
                plugin.resolve_file_action(&doc, lazy).is_err(),
                "case {case}"
            );
            assert_eq!(
                project_for("/project/src/main/resources/messages.properties")
                    .unwrap()
                    .read_file("/project/src/main/java/A.java")
                    .unwrap(),
                JAVA
            );
            assert!(host.process_runs().is_empty());
        }
    }
    #[test]
    fn full_source_inventory_fingerprints_refuse_new_unmatched_changed_and_reassigned_files() {
        for case in 0..4 {
            let (host, _, doc) = setup();
            host.add_file("/project/src/main/java/Unmatched.java", "class Empty {}");
            let project = project_for(&doc.path).unwrap();
            let before = collect(&project, "/project", &doc, "hello").unwrap();
            match case {
                0 => host.add_file(
                    "/project/src/main/java/A.java",
                    &JAVA.replace("/* 😀 */", "/* moved */"),
                ),
                1 => host.add_file("/project/src/main/java/Unmatched.java", "class Changed {}"),
                2 => host.add_file("/project/src/main/java/New.java", "class New {}"),
                _ => host.add_project("/project/src/main/java"),
            }
            assert!(
                same_sources(&project, "/project", &doc, "hello", &before).is_err(),
                "case {case}"
            );
        }
    }
    #[test]
    fn source_file_byte_total_and_reference_caps_abort_without_partial_results() {
        for case in 0..4 {
            let (host, _, doc) = setup();
            let project = project_for(&doc.path).unwrap();
            match case {
                0 => {
                    for n in 0..MAX_FILES {
                        host.add_file(
                            &format!("/project/src/main/java/F{n}.java"),
                            "class Empty {}",
                        );
                    }
                }
                1 => host.add_file(
                    "/project/src/main/java/Big.java",
                    &" ".repeat(MAX_SOURCE + 1),
                ),
                2 => {
                    for n in 0..65 {
                        host.add_file(
                            &format!("/project/src/main/java/F{n}.java"),
                            &" ".repeat(MAX_SOURCE),
                        );
                    }
                }
                _ => host.add_file(
                    "/project/src/main/java/A.java",
                    &format!(
                        "class A {{void f(){{{}}}}}",
                        "java.util.ResourceBundle.getBundle(\"messages\").getString(\"hello\");"
                            .repeat(MAX_MATCHES + 1)
                    ),
                ),
            }
            assert!(
                collect(&project, "/project", &doc, "hello").is_err(),
                "case {case}"
            );
        }
    }
    #[test]
    fn complete_before_after_preview_is_bounded_and_unshown_approval_is_not_consumed() {
        let (host, mut plugin, doc) = setup();
        let project = project_for(&doc.path).unwrap();
        let (_, bundle) = atomic::snapshot(&doc).unwrap();
        let sources = collect(&project, "/project", &doc, "hello").unwrap();
        let edits = plan(&bundle, &sources, "hello", "fresh:key").unwrap();
        let review = preview("/project", "hello", "fresh:key", &bundle, &sources, &edits).unwrap();
        assert!(
            review.contains(r"hel\u006Co")
                && review.contains(r"fresh\:key")
                && review.contains("local JDK literals")
                && review.contains("classloader")
                && review.contains("A.java")
        );
        host.add_file(
            "/project/src/main/java/A.java",
            &format!(
                "class A {{void f(){{{}}}}}",
                "java.util.ResourceBundle.getBundle(\"messages\").getString(\"hello\");"
                    .repeat(MAX_MATCHES)
            ),
        );
        let lazy = action(&mut plugin, &doc);
        let new = "x".repeat(512);
        host.answer_next_prompt(Some(&[&new]));
        host.answer_next_prompt(Some(&["not-consumed"]));
        assert!(plugin.resolve_file_action(&doc, lazy).is_err());
        assert_eq!(
            host::input("unused confirmation", None).unwrap().as_deref(),
            Some("not-consumed")
        );
    }
    #[test]
    fn constant_held_and_property_key_literals_make_rename_refuse_without_partial_edits() {
        let held = [
            "import java.util.ResourceBundle; class K { static final String KEY = \"hello\"; String f(){return ResourceBundle.getBundle(\"messages\").getString(KEY);} }",
            "import org.jetbrains.annotations.PropertyKey; class K { static String m(@PropertyKey(resourceBundle = \"messages\") String key){return key;} String f(){return K.m(\"hello\");} }",
        ];
        for source in held {
            let (host, _, doc) = setup();
            host.add_file("/project/src/main/java/K.java", source);
            let project = project_for(&doc.path).unwrap();
            let error = collect(&project, "/project", &doc, "hello").err().expect("must refuse");
            assert!(error.to_string().contains("rename it manually"), "{source}");
        }
    }
    #[test]
    fn nested_module_test_resources_keep_bundle_namespace_source_set_and_unicode_ranges() {
        let host = MockHost::install().with_capabilities(&["project.read", "editor.read"]);
        let doc = Document {
            path: "/project/module/src/test/resources/pkg/messages.properties".into(),
            language: "properties".into(),
            version: 1,
            text: "hé😀=Value\r\n".into(),
        };
        host.add_file(&doc.path, &doc.text);
        host.add_file(
            "/project/module/src/test/resources/pkg/messages_fr.properties",
            "hé😀=Bonjour\r\n",
        );
        let java = "class C {String f(){return java.util.ResourceBundle.getBundle(\"pkg.messages\").getString(\"hé😀\");}}";
        host.add_file("/project/module/src/test/java/C.java", java);
        host.add_file("/project/module/src/main/java/Wrong.java", java);
        host.add_file("/project/other/src/test/java/Wrong.java", java);
        let mut plugin = Properties::default();
        let lazy = plugin
            .file_actions(&doc, Range::new(Position::new(0, 1), Position::new(0, 1)))
            .into_iter()
            .find(|a| a.title == "Change bundle key and local JDK literals…")
            .unwrap();
        host.answer_next_prompt(Some(&["!fresh=value space"]));
        host.answer_next_prompt(Some(&["yes"]));
        let ready = plugin.resolve_file_action(&doc, lazy).unwrap();
        assert_eq!(ready.edits.len(), 3);
        let edit = &ready
            .edits
            .iter()
            .find(|(p, _)| p.ends_with("C.java"))
            .unwrap()
            .1[0];
        assert_eq!(edit.range.end.character - edit.range.start.character, 4);
        assert_eq!(edit.text, "!fresh=value space");
        assert!(ready.edits.iter().all(|(p, _)| !p.contains("Wrong")));
    }
}
