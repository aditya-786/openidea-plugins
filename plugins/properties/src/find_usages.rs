//! Explicit, positive-only source usage search. Never publishes Rename anchors.
use super::*;
use std::time::{Duration, Instant};

const MAX_FILES: usize = 400;
const MAX_SOURCE: usize = 128 << 10;
const MAX_TOTAL: usize = 8 << 20;
const MAX_MATCHES: usize = 128;

struct Found {
    location: Location,
    text: String,
}
fn collect(project: &Project, doc: &Document, key: &str) -> Result<Vec<Found>, Error> {
    let root = project.root()?;
    let ownership = usages::SourceOwnership::current(&root).ok_or(Error::Stale)?;
    if !ownership.contains(&doc.path) { return Err(Error::Stale); }
    let started = Instant::now();
    let mut paths = project.find_files("**/*.java", MAX_FILES as u32 + 1)?;
    paths.extend(project.find_files("**/*.kt", MAX_FILES as u32 + 1)?);
    paths.sort();
    paths.dedup();
    if paths.len() > MAX_FILES {
        return Err("Source search exceeds 400 files; no complete supported-source result is available.".into());
    }
    let mut total = 0;
    let mut found = Vec::new();
    for path in paths {
        if started.elapsed() > Duration::from_secs(1) {
            return Err("Source usage search exceeded its time limit; no result is presented as complete.".into());
        }
        if !ownership.contains(&path) {
            continue;
        }
        let stat = project.stat(&path)?;
        if stat.kind != openidea_plugin::FileKind::File || stat.size > MAX_SOURCE as u64
            || stat.size > (MAX_TOTAL - total) as u64
        {
            return Err("A source file is too large to search (over 128 KiB, or over 8 MiB in total).".into());
        }
        let text = project.read_file(&path)?;
        total += text.len();
        if text.len() > MAX_SOURCE || total > MAX_TOTAL {
            return Err("Source usage search exceeds its 128 KiB/file or 8 MiB total limit.".into());
        }
        let source = Document {
            path: path.clone(),
            language: if path.ends_with(".kt") { "kotlin" } else { "java" }.into(),
            text: text.clone(),
            version: 0,
        };
        let mut occurrences = text.match_indices(key);
        for _ in 0..MAX_MATCHES {
            let Some((at, _)) = occurrences.next() else { break };
            if started.elapsed() > Duration::from_secs(1) {
                return Err("Source usage search exceeded its time limit.".into());
            }
            let Some(context) = source::context(&source, at) else { continue };
            if context.contains(&doc.path) && text.get(context.span.clone()) == Some(key) {
                let location = Location { path: path.clone(), range: range_of(&text, &context.span) };
                if !found.iter().any(|f: &Found| f.location == location) {
                    found.push(Found { location, text: text.clone() });
                    if found.len() > MAX_MATCHES {
                        return Err("More than 128 supported usages; refine the project scope.".into());
                    }
                }
            }
        }
        if occurrences.next().is_some() {
            return Err("More than 128 key occurrences in one source; search was not completed.".into());
        }
    }
    Ok(found)
}

pub(super) fn run(ctx: &Invocation) -> Result<Option<String>, Error> {
    let editor = properties_editor(ctx)?;
    let current = editor.document()?;
    let doc = Document { path: editor.path(), language: editor.language(), text: current.text, version: current.version };
    if doc.text.len() > MAX_SOURCE {
        return Err("Invoking properties file exceeds 128 KiB.".into());
    }
    let at = offset(&doc.text, editor.selections()?.first().ok_or(Error::Stale)?.start).ok_or(Error::Stale)?;
    let property = parse(&doc.text).into_iter().find(|p| p.key.start <= at && at < p.key.end).ok_or("Place the caret on a property key.")?;
    let key = property.exact_key(&doc.text).ok_or("This key cannot be represented losslessly by the supported source-usage adapter.")?;
    if key.is_empty() || key.len() > 512 {
        return Err("The key is empty or exceeds 512 bytes.".into());
    }
    let project = project_for(&doc.path)?;
    let root = project.root()?;
    let found = collect(&project, &doc, &key)?;
    if found.is_empty() {
        return Ok(Some("No supported explicit JDK bundle usages found. Ignored files, dynamic loaders and framework references were not resolved; this does not prove the key is unused.".into()));
    }
    let items: Vec<_> = found.iter().enumerate().map(|(i, f)| {
        openidea_plugin::PickItem::new(&i.to_string(), &format!("{}:{}:{}", f.location.path, f.location.range.start.line + 1, f.location.range.start.character + 1))
    }).collect();
    let chosen = host::pick("Explicit JDK bundle usages (ignored/dynamic/framework references excluded)", &items)?.ok_or(Error::Cancelled)?;
    let selected = chosen.parse::<usize>().ok().and_then(|i| found.get(i)).ok_or(Error::Stale)?;
    if host::project_of(&doc.path).and_then(|p| p.root().ok()).as_deref() != Some(&root)
        || host::project_of(&selected.location.path).and_then(|p| p.root().ok()).as_deref() != Some(&root)
        || editor.document()?.version != doc.version
        || editor.document()?.text != doc.text
        || project.read_file(&doc.path)? != doc.text
        || project.read_file(&selected.location.path)? != selected.text
    {
        return Err(Error::Stale);
    }
    host::open_location(&selected.location.path, selected.location.range.start.line, selected.location.range.start.character)?;
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    fn setup() -> (MockHost, Document) {
        let host = MockHost::install();
        let doc = Document { path: "/project/src/main/resources/messages.properties".into(), language: "properties".into(), text: "hello=World\n".into(), version: 1 };
        host.add_file(&doc.path, &doc.text);
        (host, doc)
    }
    #[test]
    fn explicit_bundle_sources_only_with_exact_utf16_literal_ranges() {
        let (host, doc) = setup();
        let source = "class A { String f() { /* 😀 */ return java.util.ResourceBundle.getBundle(\"messages\").getString(\"hello\"); } }";
        host.add_file("src/main/java/A.java", source);
        host.add_file("other/src/main/java/A.java", source);
        host.add_file("src/main/java/Unknown.java", "class U { String f() {return unknown.getString(\"hello\");} }");
        let found = collect(&project_for(&doc.path).unwrap(), &doc, "hello").unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].location.path, "/project/src/main/java/A.java");
        let range = found[0].location.range;
        assert_eq!(&source[offset(source, range.start).unwrap()..offset(source, range.end).unwrap()], "hello");
        assert_eq!(range.start.character as usize, source[..source.rfind("hello").unwrap()].encode_utf16().count());
    }
    #[test]
    fn command_opens_only_a_returned_exact_literal_and_rejects_forged_selection() {
        let (host, doc) = setup();
        host.add_file("src/main/java/A.java", "class A {String f(){return java.util.ResourceBundle.getBundle(\"messages\").getString(\"hello\");}}");
        host.open_editor(&doc.path, "properties", &doc.text, &[(0,1,0,1)]);
        host.answer_next_prompt(Some(&["0"]));
        assert!(run(&host.invocation()).unwrap().is_none());
        assert_eq!(host.opened_locations().len(), 1);
        assert_eq!(host.opened_locations()[0].0, "/project/src/main/java/A.java");
        host.answer_next_prompt(Some(&["999"]));
        assert!(run(&host.invocation()).is_err());
        assert_eq!(host.opened_locations().len(), 1);
    }
    #[test]
    fn source_caps_refuse_partial_results_and_do_not_create_an_index() {
        let (host, doc) = setup();
        host.add_file("src/main/java/Large.java", &"x".repeat(MAX_SOURCE + 1));
        let failure = collect(&project_for(&doc.path).unwrap(), &doc, "hello").err().unwrap();
        assert!(failure.to_string().contains("too large to search"));
        assert!(Properties::default().index.is_none());
        host.open_editor(&doc.path, "properties", &"x".repeat(MAX_SOURCE + 1), &[(0,0,0,0)]);
        assert!(run(&host.invocation()).is_err());
    }
    #[test]
    fn command_cancel_changes_nothing_and_returns_no_rename_edges() {
        let (host, doc) = setup();
        host.add_file("src/main/java/A.java", "class A {String f(){return java.util.ResourceBundle.getBundle(\"messages\").getString(\"hello\");}}");
        host.open_editor(&doc.path, "properties", &doc.text, &[(0,1,0,1)]);
        host.answer_next_prompt(None);
        assert!(matches!(run(&host.invocation()), Err(Error::Cancelled)));
        assert_eq!(project_for(&doc.path).unwrap().read_file(&doc.path).unwrap(), doc.text);
        assert!(!include_str!("../plugin.toml").contains("contributes.documentIndexers"));
    }
    #[test]
    fn lossless_key_required_before_source_usage_search() {
        let (host, mut doc) = setup();
        doc.text = "bad\\uD800=one\n".into();
        host.add_file(&doc.path, &doc.text);
        host.add_file("src/main/java/A.java", "class A {String f(){return java.util.ResourceBundle.getBundle(\"messages\").getString(\"bad�\");}}");
        host.open_editor(&doc.path, "properties", &doc.text, &[(0,1,0,1)]);
        host.answer_next_prompt(Some(&["0"]));
        assert!(run(&host.invocation()).is_err());
        assert!(host.opened_locations().is_empty());
    }
    #[test]
    fn usage_search_requires_bounded_current_project_ownership() {
        let (host, doc) = setup();
        host.add_file("src/main/java/A.java", "class A {String f(){return java.util.ResourceBundle.getBundle(\"messages\").getString(\"hello\");}}");
        for n in 0..64 { host.add_project(&format!("/unrelated{n}")); }
        assert!(collect(&project_for(&doc.path).unwrap(), &doc, "hello").is_err());
    }
    #[test]
    fn usage_search_does_not_cross_a_new_nested_resource_owner() {
        let (host, doc) = setup();
        host.add_file("src/main/java/A.java", "class A {String f(){return java.util.ResourceBundle.getBundle(\"messages\").getString(\"hello\");}}");
        let project = project_for(&doc.path).unwrap();
        assert_eq!(collect(&project, &doc, "hello").unwrap().len(), 1);
        host.add_project("/project/src/main/resources");
        assert!(collect(&project, &doc, "hello").is_err());
    }
}
