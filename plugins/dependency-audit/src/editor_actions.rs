//! Lazy editor actions: listing has no effects; resolution rechecks current source/cache.
use crate::*;
use serde_json::{Value, json};
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reference {
    revision: u8,
    operation: String,
    root: String,
    path: String,
    version: u64,
    selection: [u32; 4],
    effect: Value,
}
fn selection(r: [u32; 4]) -> Range {
    Range::new(
        openidea_plugin::Position::new(r[0], r[1]),
        openidea_plugin::Position::new(r[2], r[3]),
    )
}
fn effect(action: &CodeAction) -> Value {
    json!(
        action
            .edits
            .iter()
            .map(|(path, edits)| json!([
                path,
                edits
                    .iter()
                    .map(|e| json!([
                        e.range.start.line,
                        e.range.start.character,
                        e.range.end.line,
                        e.range.end.character,
                        e.text
                    ]))
                    .collect::<Vec<_>>()
            ]))
            .collect::<Vec<_>>()
    )
}
fn identity(f: &Finding, id: &str) -> Value {
    json!([
        f.dep.ecosystem.osv(),
        f.dep.name,
        f.dep.version,
        f.dep.at.0,
        f.dep.at.1,
        id
    ])
}
fn reference(
    doc: &Document,
    root: &str,
    range: Range,
    operation: &str,
    effect: Value,
) -> Option<String> {
    let data = serde_json::to_string(&Reference {
        revision: 1,
        operation: operation.into(),
        root: root.into(),
        path: doc.path.clone(),
        version: doc.version,
        selection: [
            range.start.line,
            range.start.character,
            range.end.line,
            range.end.character,
        ],
        effect,
    })
    .ok()?;
    (data.len() <= 16 * 1024).then_some(data)
}
pub fn offer(doc: &Document, range: Range, findings: &[Finding]) -> Vec<CodeAction> {
    let Some(root) = project_of(&doc.path)
        .and_then(|p| p.root().ok())
        .filter(|r| !r.is_empty())
    else {
        return Vec::new();
    };
    let mut result = Vec::new();
    for mut action in upgrade_actions(findings, doc).into_iter().take(128) {
        let Some(data) = reference(doc, &root, range, "upgrade", effect(&action)) else {
            continue;
        };
        action.data = Some(data);
        action.edits.clear();
        result.push(action);
    }
    for f in findings.iter().take(64) {
        for a in f
            .advisories
            .iter()
            .filter(|a| osv::is_safe_id(&a.id))
            .take(32)
        {
            for (operation, title) in [
                ("open", format!("Open OSV advisory {}", a.id)),
                (
                    "ignore",
                    format!("Ignore {} for {} {}…", a.id, f.dep.name, f.dep.version),
                ),
            ] {
                if result.len() >= 128 {
                    return result;
                }
                let Some(data) = reference(doc, &root, range, operation, identity(f, &a.id)) else {
                    continue;
                };
                result.push(CodeAction {
                    title,
                    kind: Some("quickfix".into()),
                    preferred: false,
                    edits: Vec::new(),
                    data: Some(data),
                });
            }
        }
    }
    result
}
fn current(doc: &Document, r: &Reference) -> Result<Project, Error> {
    if r.revision != 1
        || doc.path != r.path
        || doc.version != r.version
        || doc.text.len() > 512 << 10
    {
        return Err(Error::Stale);
    }
    let project = project_of(&doc.path).ok_or(Error::Stale)?;
    if project.root()? != r.root || !owned_source_path(&r.root, &doc.path) {
        return Err(Error::Stale);
    }
    if project.read_file(&doc.path)? != doc.text {
        return Err(Error::Invalid("Save the file first".into()));
    }
    Ok(project)
}
fn finding(doc: &Document, r: &Reference) -> Result<(Finding, String), Error> {
    let found: Vec<_> = editor_findings(doc, selection(r.selection))
        .into_iter()
        .flat_map(|f| {
            f.advisories
                .iter()
                .filter(|a| osv::is_safe_id(&a.id) && identity(&f, &a.id) == r.effect)
                .map(|a| (f.clone(), a.id.clone()))
                .collect::<Vec<_>>()
        })
        .collect();
    if found.len() != 1 {
        return Err(Error::Stale);
    }
    Ok(found.into_iter().next().unwrap())
}
pub fn resolve(
    plugin: &mut DependencyAudit,
    doc: &Document,
    mut action: CodeAction,
) -> Result<CodeAction, Error> {
    let data = action
        .data
        .as_deref()
        .filter(|d| d.len() <= 16 * 1024)
        .ok_or(Error::Stale)?;
    let r: Reference = serde_json::from_str(data).map_err(|_| Error::Stale)?;
    let project = current(doc, &r)?;
    let lock = owned_lock(&project, &doc.path);
    match r.operation.as_str() {
        "upgrade" => {
            let plans = upgrade_actions(&editor_findings(doc, selection(r.selection)), doc);
            let mut matching = plans
                .into_iter()
                .filter(|p| p.title == action.title && effect(p) == r.effect);
            let plan = matching.next().ok_or(Error::Stale)?;
            if matching.next().is_some() {
                return Err(Error::Stale);
            }
            let mut preview = format!(
                "Project: {}\nSource: {}\nEditor version: {}\n\n",
                json!(r.root),
                json!(doc.path),
                doc.version
            );
            for (path, edits) in &plan.edits {
                for edit in edits {
                    if path != &doc.path {
                        return Err(Error::Stale);
                    }
                    let from = openidea_plugin::text::offset(&doc.text, edit.range.start)
                        .ok_or(Error::Stale)?;
                    let to = openidea_plugin::text::offset(&doc.text, edit.range.end)
                        .ok_or(Error::Stale)?;
                    let before = doc.text.get(from..to).ok_or(Error::Stale)?;
                    preview.push_str(&format!(
                        "{}:{}–{}:{}\nReplace {}\nWith {}\n\n",
                        edit.range.start.line + 1,
                        edit.range.start.character + 1,
                        edit.range.end.line + 1,
                        edit.range.end.character + 1,
                        json!(before),
                        json!(edit.text)
                    ));
                }
            }
            preview.push_str("Return these edits for one undoable host apply? Cached OSV advice is not compatibility or current resolver proof. No package install or lock regeneration runs.");
            if !ui_kit::prompts::confirm_exact("Preview dependency editor fix", &preview)? {
                return Err(Error::Cancelled);
            }
            current(doc, &r)?;
            if owned_lock(&project, &doc.path) != lock
                || !upgrade_actions(&editor_findings(doc, selection(r.selection)), doc)
                    .iter()
                    .any(|p| p.title == plan.title && effect(p) == r.effect)
            {
                return Err(Error::Stale);
            }
            return Ok(plan);
        }
        "open" => {
            let (f, id) = finding(doc, &r)?;
            let url = format!("https://osv.dev/vulnerability/{id}");
            if !ui_kit::prompts::confirm_exact(
                "Open cached advisory?",
                &format!(
                    "Project: {}\nSource: {} (version {})\nPackage: {} {}\nURL: {}\n\nOpen this exact external advisory? No scan or package command runs.",
                    json!(r.root),
                    json!(doc.path),
                    doc.version,
                    json!(f.dep.name),
                    json!(f.dep.version),
                    url
                ),
            )? {
                return Err(Error::Cancelled);
            }
            current(doc, &r)?;
            if owned_lock(&project, &doc.path) != lock || finding(doc, &r)?.0 != f {
                return Err(Error::Stale);
            }
            host::open_url(&url)?;
        }
        "ignore" => {
            let (f, id) = finding(doc, &r)?;
            scoped::ignore_editor(&r.root, &f, &id, || {
                current(doc, &r)?;
                if owned_lock(&project, &doc.path) != lock || finding(doc, &r)?.0 != f {
                    return Err(Error::Stale);
                }
                Ok(())
            })?;
            plugin.invalidate_report();
        }
        _ => return Err(Error::Stale),
    }
    action.data = None;
    action.edits.clear();
    Ok(action)
}
