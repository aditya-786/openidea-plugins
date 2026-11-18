//! Explicit local JDK compiler analysis. Never called by automatic providers.
use crate::*;
use openidea_plugin::file_actions::FileAction;
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};
const HELPER: &str = include_str!("../adapters/PropertiesGraph.java");
const LIMIT: usize = 256 << 10;
#[derive(Clone, Debug, PartialEq, Eq)]
struct Edge {
    path: String,
    start: usize,
    end: usize,
    bundle: String,
    key: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Graph {
    complete: bool,
    sources: BTreeMap<String, String>,
    profile: Option<(String, String)>,
    dependencies: BTreeMap<String, String>,
    bundles: Vec<String>,
    edges: Vec<Edge>,
    unknown: Vec<String>,
}
struct Private(String);
impl Drop for Private {
    fn drop(&mut self) {
        let _ = host::fs::write_file(&self.0, "");
        // A killed JDK cannot execute its finally cleanup. Empty only its known
        // bounded private JAR snapshots; directory listing is cleanup, not proof.
        if let Ok(entries) = host::fs::read_dir(&format!("{}.classpath", self.0)) {
            for entry in entries.into_iter().take(128).filter(|e| {
                e.kind == openidea_plugin::FileKind::File
                    && e.name.starts_with("dependency-")
                    && e.name.ends_with(".jar")
                    && !e.name.contains('/')
            }) {
                let _ = host::fs::write_file(&format!("{}.classpath/{}", self.0, entry.name), "");
            }
        }
    }
}
fn hex(value: &str) -> String {
    value
        .as_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn unhex(value: &str) -> Result<String, Error> {
    if value.len() > LIMIT * 2
        || value.len() % 2 != 0
        || !value.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err("Malformed compiler graph field.".into());
    }
    let bytes: Result<Vec<_>, _> = (0..value.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&value[i..i + 2], 16))
        .collect();
    String::from_utf8(bytes.map_err(|_| "Malformed compiler graph hex.")?)
        .map_err(|_| "Compiler graph is not UTF8.".into())
}
fn relative(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('/')
        && !value.contains(['\\', '\n', '\r', '\0'])
        && value
            .split('/')
            .all(|p| !p.is_empty() && !matches!(p, "." | ".."))
}
fn absolute_profile(path: &str) -> bool {
    path.starts_with('/')
        && path.len() <= 4096
        && !path.contains(['\\', '\0', '\n', '\r'])
        && path.split('/').all(|part| !matches!(part, "." | ".."))
}
fn configured_profile() -> Result<Option<String>, Error> {
    let path = host::setting_string("compilerClasspathProfile").unwrap_or_default();
    if path.is_empty() {
        Ok(None)
    } else if absolute_profile(&path) {
        Ok(Some(path))
    } else {
        Err("Compiler classpath profile must be a normalized absolute path.".into())
    }
}
fn decode(text: &str) -> Result<Graph, Error> {
    if text.len() > LIMIT {
        return Err("Compiler graph exceeds 256KiB.".into());
    }
    let mut lines = text.lines();
    let complete = match lines.next() {
        Some("PROPERTIES_GRAPH_V1\tcomplete") => true,
        Some("PROPERTIES_GRAPH_V1\tincomplete") => false,
        _ => return Err("Unknown compiler graph protocol.".into()),
    };
    let mut g = Graph {
        complete,
        sources: BTreeMap::new(),
        profile: None,
        dependencies: BTreeMap::new(),
        bundles: vec![],
        edges: vec![],
        unknown: vec![],
    };
    let mut ended = false;
    for line in lines {
        if ended {
            return Err("Compiler graph records follow END.".into());
        }
        let fields: Vec<_> = line.split('\t').collect();
        match fields.as_slice() {
            [kind @ ("PROFILE" | "CLASSPATH"), path, hash] => {
                let path = unhex(path)?;
                if !absolute_profile(&path)
                    || hash.len() != 64
                    || !hash.bytes().all(|b| b.is_ascii_hexdigit())
                {
                    return Err("Invalid dependency proof.".into());
                }
                if *kind == "PROFILE" {
                    if g.profile.replace((path, (*hash).into())).is_some() {
                        return Err("Duplicate classpath profile.".into());
                    }
                } else if !path.ends_with(".jar")
                    || g.dependencies.len() >= 128
                    || g.dependencies.insert(path, (*hash).into()).is_some()
                {
                    return Err("Invalid classpath inventory.".into());
                }
            }
            ["SOURCE", path, digest] => {
                let path = unhex(path)?;
                if !relative(&path) || !path.ends_with(".java") || digest.len() != 64 {
                    return Err("Invalid compiler source.".into());
                }
                let digest = (*digest).to_owned();
                if digest.len() != 64
                    || !digest.bytes().all(|b| b.is_ascii_hexdigit())
                    || g.sources.insert(path, digest).is_some()
                    || g.sources.len() > 400
                {
                    return Err("Invalid compiler inventory.".into());
                }
            }
            ["BUNDLE", path] => {
                let path = unhex(path)?;
                if !relative(&path)
                    || !path.ends_with(".properties")
                    || g.bundles.contains(&path)
                    || g.bundles.len() >= 200
                {
                    return Err("Invalid compiler bundle inventory.".into());
                }
                g.bundles.push(path);
            }
            ["EDGE", path, start, end, bundle, key] => {
                let path = unhex(path)?;
                let start = start.parse().map_err(|_| "Invalid compiler offset.")?;
                let end = end.parse().map_err(|_| "Invalid compiler offset.")?;
                let bundle = unhex(bundle)?;
                let key = unhex(key)?;
                if !relative(&path)
                    || start >= end
                    || end > 131072
                    || bundle.len() > 512
                    || key.len() > 4096
                    || g.edges.len() >= 1024
                {
                    return Err("Invalid compiler edge.".into());
                }
                g.edges.push(Edge {
                    path,
                    start,
                    end,
                    bundle,
                    key,
                });
            }
            ["UNKNOWN", why] => {
                if g.unknown.len() >= 128 {
                    return Err("Too many compiler diagnostics.".into());
                }
                g.unknown.push(unhex(why)?);
            }
            ["END", sources, edges] => {
                if sources.parse::<usize>().ok() != Some(g.sources.len())
                    || edges.parse::<usize>().ok() != Some(g.edges.len())
                {
                    return Err("Incomplete compiler response.".into());
                }
                ended = true;
            }
            _ => return Err("Malformed compiler graph record.".into()),
        }
    }
    if !ended
        || g.complete && !g.unknown.is_empty()
        || g.edges.iter().any(|e| !g.sources.contains_key(&e.path))
    {
        return Err("Incomplete compiler graph.".into());
    }
    g.bundles.sort();
    g.edges.sort_by(|a, b| {
        (&a.path, a.start, a.end, &a.bundle, &a.key)
            .cmp(&(&b.path, b.start, b.end, &b.bundle, &b.key))
    });
    g.edges.dedup();
    Ok(g)
}
fn roots(doc: &Document, root: &str) -> Result<(String, String, String), Error> {
    let (module, rest, set) = if let Some((m, r)) = doc.path.rsplit_once("/src/main/resources/") {
        (m, r, "main")
    } else if let Some((m, r)) = doc.path.rsplit_once("/src/test/resources/") {
        (m, r, "test")
    } else {
        return Err(
            "Compiler graph requires conventional src/main/resources or src/test/resources.".into(),
        );
    };
    let module = module
        .strip_prefix(root)
        .filter(|r| r.is_empty() || r.starts_with('/'))
        .ok_or(Error::Stale)?
        .trim_start_matches('/');
    let prefix = if module.is_empty() {
        String::new()
    } else {
        format!("{module}/")
    };
    let (directory, _) = rest.rsplit_once('/').unwrap_or(("", rest));
    let base = split_locale(stem(rest)).0;
    let bundle = if directory.is_empty() {
        base.into()
    } else {
        format!("{}.{}", directory.replace('/', "."), base)
    };
    if bundle
        .split('.')
        .any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
    {
        return Err("Use a conventional Java resource bundle basename.".into());
    }
    Ok((
        format!("{prefix}src/{set}/java"),
        format!("{prefix}src/{set}/kotlin"),
        bundle,
    ))
}
fn digest(text: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(text.as_bytes()))
}
fn run(doc: &Document, root: &str, sources: &BTreeMap<String, String>) -> Result<Graph, Error> {
    let (java, _, _) = roots(doc, root)?;
    let relative_bundle = doc
        .path
        .strip_prefix(&format!("{root}/"))
        .filter(|s| relative(s))
        .ok_or(Error::Stale)?;
    let mut request = format!(
        "PROPERTIES_GRAPH_V1\nROOT\t{}\nSOURCE_ROOT\t{}\nBUNDLE\t{}\n",
        hex(root),
        hex(&java),
        hex(relative_bundle)
    );
    if let Some(profile) = configured_profile()? {
        request.push_str(&format!("CLASSPATH_PROFILE\t{}\n", hex(&profile)));
    }
    for (path, text) in sources {
        request.push_str(&format!("SOURCE\t{}\t{}\n", hex(path), hex(text)));
    }
    if request.len() > 9 << 20 {
        return Err("Compiler request exceeds 9MiB.".into());
    }
    let directory = host::fs::plugin_dir()?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| Error::Stale)?
        .as_nanos();
    let helper = format!("{directory}/PropertiesGraph.java");
    host::fs::write_file(&helper, HELPER)?;
    let input = Private(format!("{directory}/properties-graph-{nonce}.in"));
    let output = Private(format!("{directory}/properties-graph-{nonce}.out"));
    host::fs::write_file(&input.0, &request)?;
    let task = host::begin_task("Analyze declared Java property contracts", true);
    let child = host::start(
        &host::Command::new("java")
            .args(["-Xmx256m", "--source", "17", &helper, &input.0, &output.0]),
        None,
    )?;
    child.close_stdin()?;
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut stdout = 0;
    let mut stderr = String::new();
    loop {
        if task.is_cancelled() || Instant::now() > deadline {
            let _ = child.kill();
            return Err(if task.is_cancelled() {
                Error::Cancelled
            } else {
                "Java compiler graph exceeded 15 seconds; nothing changed.".into()
            });
        }
        let chunk = child.wait(100)?;
        stdout += chunk.stdout.len();
        stderr.push_str(&chunk.stderr);
        if stdout > 8192 || stderr.len() > 8192 {
            let _ = child.kill();
            return Err("Compiler process output exceeded 8KiB.".into());
        }
        if let Some(status) = chunk.status {
            if status != 0 {
                return Err(format!("Java 17+ compiler helper failed (status{status}); no application was started and no sources changed. {}",stderr.chars().take(256).collect::<String>()).into());
            }
            break;
        }
    }
    let stat = host::fs::stat(&output.0)?;
    if stat.kind != openidea_plugin::FileKind::File || stat.size > LIMIT as u64 {
        return Err("Compiler result exceeds 256KiB.".into());
    }
    decode(&host::fs::read_file(&output.0)?)
}
fn snapshots(doc: &Document, root: &str, g: &Graph) -> Result<BTreeMap<String, String>, Error> {
    let project = project_for(&doc.path)?;
    let owner = usages::SourceOwnership::current(root).ok_or(Error::Stale)?;
    let (java, _, _) = roots(doc, root)?;
    let mut sources = BTreeMap::new();
    let mut total = 0;
    for path in g.sources.keys() {
        if !path.starts_with(&format!("{java}/")) {
            return Err(Error::Stale);
        }
        let absolute = format!("{root}/{path}");
        if !owner.contains(&absolute)
            || host::project_of(&absolute)
                .and_then(|p| p.root().ok())
                .as_deref()
                != Some(root)
        {
            return Err(Error::Stale);
        }
        let stat = project.stat(&absolute)?;
        if stat.kind != openidea_plugin::FileKind::File || stat.size > 128 << 10 {
            return Err("Compiler source exceeds 128KiB.".into());
        }
        let text = project.read_file(&absolute)?;
        total += text.len();
        if text.len() > 128 << 10 || total > 4 << 20 {
            return Err("Compiler sources exceed 128KiB/file or 4MiB total.".into());
        }
        sources.insert(path.clone(), text);
    }
    Ok(sources)
}
fn analyze(doc: &Document, root: &str) -> Result<(Graph, BTreeMap<String, String>), Error> {
    let initial = run(doc, root, &BTreeMap::new())?;
    let sources = snapshots(doc, root, &initial)?;
    let graph = run(doc, root, &sources)?;
    if initial.sources.keys().ne(graph.sources.keys())
        || graph
            .sources
            .iter()
            .any(|(p, h)| sources.get(p).is_none_or(|s| digest(s) != *h))
    {
        return Err(Error::Stale);
    }
    Ok((graph, sources))
}
fn ensure_same(
    doc: &Document,
    root: &str,
    graph: &Graph,
    sources: &BTreeMap<String, String>,
) -> Result<(), Error> {
    if &snapshots(doc, root, graph)? != sources
        || run(doc, root, sources)? != *graph
        || &snapshots(doc, root, graph)? != sources
    {
        return Err(Error::Stale);
    }
    Ok(())
}
fn utf16_offset(text: &str, index: usize) -> Option<usize> {
    let mut units = 0;
    for (at, ch) in text.char_indices() {
        if units == index {
            return Some(at);
        }
        units += ch.len_utf16();
    }
    (units == index).then_some(text.len())
}
fn plan(
    doc: &Document,
    root: &str,
    before: &[(String, String)],
    graph: &Graph,
    sources: &BTreeMap<String, String>,
    key: &str,
    new: &str,
) -> Result<Vec<(String, Vec<TextEdit>)>, Error> {
    if !graph.complete {
        return Err(format!(
            "Declared Java graph is incomplete: {}",
            graph
                .unknown
                .iter()
                .take(4)
                .cloned()
                .collect::<Vec<_>>()
                .join("; ")
        )
        .into());
    }
    let (_, _, target) = roots(doc, root)?;
    let expected: Vec<_> = before
        .iter()
        .map(|(p, _)| p.strip_prefix(&format!("{root}/")).unwrap_or("").to_owned())
        .collect();
    let actual: std::collections::BTreeSet<_> = graph
        .bundles
        .iter()
        .filter(|p| {
            dir(p) == dir(expected.first().map(String::as_str).unwrap_or(""))
                && split_locale(stem(p)).0 == split_locale(stem(&doc.path)).0
        })
        .cloned()
        .collect();
    if actual != expected.into_iter().collect() {
        return Err("The locale files on disk differ from the ones the IDE shows (a file was added or removed). Save or reload them, then try again.".into());
    }
    let default_name = split_locale(stem(&doc.path)).0;
    if !before.iter().any(|(p, _)| stem(p) == default_name) {
        return Err(
            "A physical default bundle file is required to prove this locale family.".into(),
        );
    }
    if graph.edges.iter().any(|e| {
        e.bundle != target
            && e.bundle.starts_with(&format!("{target}_"))
            && before
                .iter()
                .any(|(p, _)| stem(p) == e.bundle.rsplit('.').next().unwrap_or(""))
    }) {
        return Err("A locale-shaped basename is also an explicit Java bundle identity; this family is ambiguous.".into());
    }
    if sources
        .keys()
        .any(|p| p.ends_with(&format!("/{}.java", target.replace('.', "/"))))
    {
        return Err("A source class shares this resource bundle basename; physical bundle ownership is ambiguous.".into());
    }
    let escaped = props::escape_key(new);
    let mut edits = Vec::new();
    for (path, text) in before {
        let parsed = parse(text);
        if parsed
            .iter()
            .any(|p| p.exact_key(text).as_deref() == Some(new))
        {
            return Err("New key already exists in a locale.".into());
        }
        let old: Vec<_> = parsed
            .iter()
            .filter(|p| p.exact_key(text).as_deref() == Some(key))
            .collect();
        if old.len() > 1 {
            return Err("Resolve duplicate old keys before renaming.".into());
        }
        if let Some(p) = old.first() {
            edits.push((
                path.clone(),
                to_edits(text, vec![(p.key.clone(), escaped.clone())]),
            ));
        }
    }
    let encoded = serde_json::to_string(new).map_err(|_| Error::Stale)?;
    let mut references = 0;
    for (path, text) in sources {
        let mut spans = Vec::new();
        for edge in graph
            .edges
            .iter()
            .filter(|e| e.path == *path && e.bundle == target && e.key == key)
        {
            let start = utf16_offset(text, edge.start).ok_or(Error::Stale)?;
            let end = utf16_offset(text, edge.end).ok_or(Error::Stale)?;
            let raw = text.get(start..end).ok_or(Error::Stale)?;
            if !raw.starts_with('"') || !raw.ends_with('"') {
                return Err(Error::Stale);
            }
            spans.push((start..end, encoded.clone()));
            references += 1;
        }
        spans.sort_by_key(|(r, _)| r.start);
        spans.dedup();
        if spans.windows(2).any(|w| w[0].0.end > w[1].0.start) {
            return Err(Error::Stale);
        }
        if !spans.is_empty() {
            edits.push((format!("{root}/{path}"), to_edits(text, spans)));
        }
    }
    if references == 0 {
        return Err("The compiler found no Java code that reads this key. To change it in the bundle only, use Change key in this bundle only….".into());
    }
    edits.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(edits)
}
pub(crate) fn resolve(
    doc: &Document,
    action: FileAction,
    root: &str,
    before: Vec<(String, String)>,
    key: &str,
) -> Result<FileAction, Error> {
    if !ui_kit::prompts::confirm_exact(
        "Analyze Java sources with the compiler?",
        &format!(
            "Project: {root}\nRuns the Java compiler (Java 17 or later) on your sources to find the code that reads this key. Your application doesn't run. Dependency JARs come from the Optional compiler dependency JAR profile setting.\nKotlin sources and keys built at runtime aren't supported. Nothing changes until you confirm the edit."
        ),
    )? {
        return Err(Error::Cancelled);
    }
    let (graph, sources) = analyze(doc, root)?;
    let new = host::input(
        "New key",
        Some(key),
    )?
    .ok_or(Error::Cancelled)?;
    if new == key || new.is_empty() || new.len() > 512 || new.chars().any(char::is_control) {
        return Err("Use a different nonempty key up to 512 bytes without controls.".into());
    }
    atomic::unchanged(doc, root, &before)?;
    let edits = plan(doc, root, &before, &graph, &sources, key, &new)?;
    let mut preview = format!(
        "Project: {root}\nComplete admitted Java graph: {}files, {}literal contracts.\nOnly shown JDK ResourceBundle/@PropertyKey references and bundle declarations change. Reflection/framework conventions, other modules/source sets and runtime classloaders remain outside this declared-source operation.\n\n",
        sources.len(),
        graph.edges.len()
    );
    if let Some((path, hash)) = &graph.profile {
        preview.push_str(&format!(
            "Compiler dependency profile: {}\nSHA256: {hash}\n",
            serde_json::to_string(path).unwrap_or_default()
        ));
    }
    for (path, hash) in &graph.dependencies {
        preview.push_str(&format!(
            "Classpath JAR: {}\nSHA256: {hash}\n",
            serde_json::to_string(path).unwrap_or_default()
        ));
    }
    preview.push('\n');
    for (path, changes) in &edits {
        let text = before
            .iter()
            .find(|(p, _)| p == path)
            .map(|(_, t)| t)
            .or_else(|| {
                path.strip_prefix(&format!("{root}/"))
                    .and_then(|p| sources.get(p))
            })
            .ok_or(Error::Stale)?;
        preview.push_str(&format!(
            "{}\n",
            serde_json::to_string(path).unwrap_or_default()
        ));
        for edit in changes {
            let start = offset(text, edit.range.start).ok_or(Error::Stale)?;
            let end = offset(text, edit.range.end).ok_or(Error::Stale)?;
            preview.push_str(&format!(
                "{}:{}\nBefore:\n{}\nAfter:\n{}\n\n",
                edit.range.start.line + 1,
                edit.range.start.character + 1,
                ui_kit::prompts::readable_source(&text[start..end]),
                ui_kit::prompts::readable_source(&edit.text)
            ));
        }
    }
    ensure_same(doc, root, &graph, &sources)?;
    if !ui_kit::prompts::confirm_exact(
        "Rename the key in the bundle and in Java code?",
        &preview,
    )? {
        return Err(Error::Cancelled);
    }
    atomic::unchanged(doc, root, &before)?;
    ensure_same(doc, root, &graph, &sources)?;
    atomic::unchanged(doc, root, &before)?;
    Ok(FileAction {
        title: action.title,
        kind: action.kind,
        edits,
        ..Default::default()
    })
}

pub(crate) fn inspect(ctx: &Invocation) -> Result<Option<String>, Error> {
    let editor = properties_editor(ctx)?;
    let original = editor.document()?;
    let doc = Document {
        path: editor.path(),
        language: "properties".into(),
        version: original.version,
        text: original.text,
    };
    let project = project_for(&doc.path)?;
    let root = project.root()?;
    let at = editor
        .selections()?
        .first()
        .map(|s| s.start)
        .unwrap_or_default();
    let offset = offset(&doc.text, at).ok_or(Error::Stale)?;
    let prop = parse(&doc.text)
        .into_iter()
        .find(|p| p.contains(offset))
        .ok_or("Place the caret on a property key.")?;
    let key = prop
        .exact_key(&doc.text)
        .ok_or("Choose a losslessly decoded property key.")?;
    if !ui_kit::prompts::confirm_exact(
        "Inspect Java property contracts?",
        &format!(
            "Analyze current declared Java sources with the approved Java 17+ JDK compiler; no application startup, annotation processing or class generation.\nProject: {root}\nOptional compilerClasspathProfile selects dependency JARs; content hashes are rechecked during analysis.\nBounded conventional source inventory includes ignored Java files.\nOnly source-bound JDK ResourceBundle and canonical @PropertyKey literal contracts are listed. Missing dependencies/dynamic references are reported as incomplete."
        ),
    )? {
        return Err(Error::Cancelled);
    }
    let (graph, sources) = analyze(&doc, &root)?;
    let (_, _, bundle) = roots(&doc, &root)?;
    let found: Vec<_> = graph
        .edges
        .iter()
        .filter(|e| e.bundle == bundle && e.key == key)
        .collect();
    host::notify(
        if graph.complete {
            NotificationLevel::Info
        } else {
            NotificationLevel::Warning
        },
        "Declared Java property graph",
        &format!(
            "{} source files, {} matching literal references. {}",
            sources.len(),
            found.len(),
            if graph.complete {
                "Complete for admitted JDK/@PropertyKey contracts; runtime/framework references remain outside this graph.".into()
            } else {
                format!(
                    "Incomplete: {}",
                    graph
                        .unknown
                        .iter()
                        .take(4)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join("; ")
                )
            }
        ),
    );
    if found.is_empty() {
        return Ok(None);
    }
    let items: Vec<_> = found
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let text = &sources[&e.path];
            let start = utf16_offset(text, e.start).unwrap_or(0);
            let at = openidea_plugin::text::position(text, start);
            openidea_plugin::PickItem::new(
                &i.to_string(),
                &format!("{}:{}:{}", e.path, at.line + 1, at.character + 1),
            )
        })
        .collect();
    let selected = host::pick(
        "Compiler-proved bundle/key literals (declared sources only)",
        &items,
    )?
    .ok_or(Error::Cancelled)?;
    let edge = found
        .get(selected.parse::<usize>().map_err(|_| Error::Stale)?)
        .ok_or(Error::Stale)?;
    if editor.document()?.version != doc.version
        || editor.document()?.text != doc.text
        || snapshots(&doc, &root, &graph)? != sources
    {
        return Err(Error::Stale);
    }
    let at = openidea_plugin::text::position(
        &sources[&edge.path],
        utf16_offset(&sources[&edge.path], edge.start).ok_or(Error::Stale)?,
    );
    host::open_location(&format!("{root}/{}", edge.path), at.line, at.character)?;
    Ok(None)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (
        Document,
        Vec<(String, String)>,
        Graph,
        BTreeMap<String, String>,
    ) {
        let doc = Document {
            path: "/project/src/main/resources/pkg/messages.properties".into(),
            language: "properties".into(),
            version: 3,
            text: "hel\\u006co=World\n".into(),
        };
        let java = "class A { static final java.util.ResourceBundle b=java.util.ResourceBundle.getBundle(\"pkg.messages\"); String f(){/*😀*/return b.getString(\"hel\\u006co\");}}";
        let source = "src/main/java/A.java".to_owned();
        let start = java.rfind('"').unwrap();
        let from = java[..start].rfind('"').unwrap();
        let edge = Edge {
            path: source.clone(),
            start: java[..from].encode_utf16().count(),
            end: java[..start + 1].encode_utf16().count(),
            bundle: "pkg.messages".into(),
            key: "hello".into(),
        };
        let sources = BTreeMap::from([(source.clone(), java.into())]);
        let g = Graph {
            complete: true,
            profile: None,
            dependencies: BTreeMap::new(),
            sources: BTreeMap::from([(source, digest(java))]),
            bundles: vec!["src/main/resources/pkg/messages.properties".into()],
            edges: vec![edge],
            unknown: vec![],
        };
        let before = vec![(doc.path.clone(), doc.text.clone())];
        (doc, before, g, sources)
    }
    #[test]
    fn compiler_plan_uses_decoded_keys_whole_java_literals_and_utf16_positions() {
        let (d, b, g, s) = fixture();
        let edits = plan(&d, "/project", &b, &g, &s, "hello", "naïve.😀\\key\"value").unwrap();
        assert_eq!(edits.len(), 2);
        let source = &s["src/main/java/A.java"];
        let java = &edits.iter().find(|(p, _)| p.ends_with(".java")).unwrap().1[0];
        let from = offset(source, java.range.start).unwrap();
        let to = offset(source, java.range.end).unwrap();
        assert_eq!(&source[from..to], "\"hel\\u006co\"");
        assert_eq!(java.text, "\"naïve.😀\\\\key\\\"value\"");
        assert_eq!(utf16_offset("😀", 1), None);
    }
    #[test]
    fn compiler_plan_refuses_incomplete_collisions_hidden_locales_and_class_owners() {
        let (d, b, mut g, mut s) = fixture();
        g.complete = false;
        assert!(plan(&d, "/project", &b, &g, &s, "hello", "new").is_err());
        g.complete = true;
        g.bundles
            .push("src/main/resources/pkg/messages_fr.properties".into());
        assert!(plan(&d, "/project", &b, &g, &s, "hello", "new").is_err());
        g.bundles.pop();
        s.insert(
            "src/main/java/pkg/messages.java".into(),
            "class messages{}".into(),
        );
        assert!(plan(&d, "/project", &b, &g, &s, "hello", "new").is_err());
        s.remove("src/main/java/pkg/messages.java");
        let b = vec![(d.path.clone(), "hello=x\nnew=y\n".into())];
        assert!(plan(&d, "/project", &b, &g, &s, "hello", "new").is_err());
    }
    #[test]
    fn compiler_protocol_is_bounded_complete_and_never_panics_on_nonhex_utf8() {
        let proof = format!(
            "PROPERTIES_GRAPH_V1\tcomplete\nPROFILE\t{}\t{}\nCLASSPATH\t{}\t{}\nEND\t0\t0\n",
            hex("/profiles/classpath.txt"),
            "a".repeat(64),
            hex("/libraries/annotations.jar"),
            "b".repeat(64)
        );
        let parsed = decode(&proof).unwrap();
        assert_eq!(parsed.profile.unwrap().0, "/profiles/classpath.txt");
        assert_eq!(
            parsed.dependencies["/libraries/annotations.jar"],
            "b".repeat(64)
        );
        assert!(unhex("é").is_err());
        assert!(decode("PROPERTIES_GRAPH_V1\tcomplete\nEND\t0\t0\n").is_ok());
        assert!(decode("PROPERTIES_GRAPH_V1\tcomplete\nUNKNOWN\t61\nEND\t0\t0\n").is_err());
        assert!(decode("PROPERTIES_GRAPH_V1\tcomplete\nEND\t1\t0\n").is_err());
        assert!(decode("PROPERTIES_GRAPH_V1\tcomplete\nEND\t0\t0\nextra").is_err());
        assert!(!relative("../foreign.java"));
        assert!(unhex(&"f".repeat(LIMIT * 2 + 2)).is_err());
    }
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn real_jdk_graph_passes_production_decoder_and_exact_rename_planner() {
        use std::process::{Command, Stdio};
        if Command::new("java").arg("-version").output().is_err() {
            eprintln!("optional Java unavailable; compiler fixture skipped");
            return;
        }
        let (mut doc, _, _, mut sources) = fixture();
        let source = sources.get_mut("src/main/java/A.java").unwrap();
        *source = source.replace("class A {", "class A { static { System.exit(91); }");
        let temporary = std::env::temp_dir().join(format!(
            "properties-compiler-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&temporary).unwrap();
        let root = std::fs::canonicalize(&temporary).unwrap();
        struct Remove(std::path::PathBuf);
        impl Drop for Remove {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _remove = Remove(root.clone());
        std::fs::create_dir_all(root.join("src/main/java")).unwrap();
        std::fs::create_dir_all(root.join("src/main/resources/pkg")).unwrap();
        for (path, text) in &sources {
            std::fs::write(root.join(path), text).unwrap();
        }
        std::fs::write(
            root.join("src/main/resources/pkg/messages.properties"),
            &doc.text,
        )
        .unwrap();
        let helper = root.join("PropertiesGraph.java");
        std::fs::write(&helper, HELPER).unwrap();
        let mut request = format!(
            "PROPERTIES_GRAPH_V1\nROOT\t{}\nSOURCE_ROOT\t{}\nBUNDLE\t{}\n",
            hex(root.to_str().unwrap()),
            hex("src/main/java"),
            hex("src/main/resources/pkg/messages.properties")
        );
        for (path, text) in &sources {
            request.push_str(&format!("SOURCE\t{}\t{}\n", hex(path), hex(text)));
        }
        let input = root.join("input");
        let output = root.join("output");
        std::fs::write(&input, request).unwrap();
        let mut child = Command::new("java")
            .args(["-Xmx256m", "--source", "17"])
            .arg(&helper)
            .arg(&input)
            .arg(&output)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() > deadline {
                child.kill().unwrap();
                panic!("real compiler deadline");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let result = child.wait_with_output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let graph = decode(&std::fs::read_to_string(&output).unwrap()).unwrap();
        assert!(graph.complete, "{:?}", graph.unknown);
        assert_eq!(graph.edges.len(), 1);
        assert_eq!(graph.edges[0].key, "hello");
        doc.path = root
            .join("src/main/resources/pkg/messages.properties")
            .to_str()
            .unwrap()
            .into();
        let before = vec![(doc.path.clone(), doc.text.clone())];
        let edits = plan(
            &doc,
            root.to_str().unwrap(),
            &before,
            &graph,
            &sources,
            "hello",
            "new.key",
        )
        .unwrap();
        assert_eq!(edits.len(), 2);
        assert!(edits.iter().any(|(p, _)| p.ends_with("A.java")));
    }
}
