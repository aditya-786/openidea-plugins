//! Project-owned editor hints, not edits to Symfony/Twig runtime configuration.
use crate::*;
use openidea_plugin::FileKind;
use openidea_plugin::views::{Field, FieldValue};
use serde_json::{Value, json};
pub const VIEW: &str = "twig-paths";
const MAX_ROWS: usize = 32;
const MAX_STORAGE: usize = 32 << 10;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mapping {
    pub namespace: String,
    pub directory: String,
    pub enabled: bool,
}
pub fn valid(mapping: &Mapping) -> bool {
    mapping.namespace.len() <= 64
        && mapping.namespace.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        && !mapping.directory.is_empty()
        && mapping.directory.len() <= 512
        && !mapping.directory.starts_with('/')
        && !mapping.directory.chars().any(|ch| ch.is_control() || "\\:*?[]{}%$\"'".contains(ch))
        && mapping.directory.split('/').count() <= 16
        && mapping.directory.split('/').all(|part| !part.is_empty() && part != "." && part != ".." && part.len() <= 255)
}
fn key(root: &str) -> String {
    let hash = root.bytes().fold(0xcbf29ce484222325u64, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3));
    format!("twig-paths:{hash:016x}")
}
fn decode(root: &str, raw: Option<&str>) -> Result<Vec<Mapping>, Error> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    if raw.len() > MAX_STORAGE {
        return Err("Twig path storage exceeds 32 KiB.".into());
    }
    let value: Value = serde_json::from_str(raw).map_err(|_| Error::Invalid("Invalid Twig path storage.".into()))?;
    if value.as_object().is_none_or(|value| value.len() != 3) || value["root"].as_str() != Some(root) || value["version"] != 1 {
        return Err(Error::Stale);
    }
    let rows = value["paths"].as_array().filter(|rows| rows.len() <= MAX_ROWS).ok_or("Twig paths need at most 32 rows.")?;
    let mut result = Vec::new();
    let mut seen = BTreeSet::new();
    for row in rows {
        if row.as_object().is_none_or(|row| row.len() != 3) {
            return Err("Unknown Twig path fields.".into());
        }
        let mapping = Mapping {
            namespace: row["namespace"].as_str().ok_or("Invalid namespace.")?.into(),
            directory: row["directory"].as_str().ok_or("Invalid directory.")?.into(),
            enabled: row["enabled"].as_bool().ok_or("Invalid Enabled value.")?,
        };
        if !valid(&mapping) || !seen.insert((mapping.namespace.clone(), mapping.directory.clone())) {
            return Err("Invalid or duplicate project Twig path mapping.".into());
        }
        result.push(mapping);
    }
    Ok(result)
}
pub fn load(root: &str) -> Result<Vec<Mapping>, Error> {
    if root.is_empty() {
        return Err("Open a project first.".into());
    }
    let raw = host::storage::get(&key(root))?;
    decode(root, raw.as_deref())
}
pub(crate) fn directory(project: &Project, root: &str, mapping: &Mapping) -> Result<(), Error> {
    if !valid(mapping) {
        return Err("Use a relative directory with no traversal/globs/parameters, and a namespace without @ (empty: main).".into());
    }
    if !mapping.enabled {
        return Ok(());
    }
    let mut path = root.to_string();
    for part in mapping.directory.split('/') {
        path.push('/');
        path.push_str(part);
        if project.stat(&path)?.kind != FileKind::Directory {
            return Err("Enabled mapping needs a project directory; files and host-rejected paths are refused.".into());
        }
    }
    Ok(())
}
/// Existing mappings and global compatibility entries never escape this root.
pub fn configured(project: &Project) -> (Vec<Mapping>, bool) {
    let Ok(root) = project.root() else {
        return (Vec::new(), true);
    };
    let mut unknown = false;
    let mut rows = match load(&root) {
        Ok(rows) => rows,
        Err(_) => {
            unknown = true;
            Vec::new()
        }
    };
    let legacy = host::setting_string("twigNamespaces").unwrap_or_default();
    if legacy.len() > 4096 {
        unknown = true;
    } else {
        let parsed = index::twig_namespaces(&legacy);
        if legacy.split(',').filter(|pair| !pair.trim().is_empty()).count() != parsed.len() {
            unknown = true;
        }
        for (directory, namespace) in parsed.into_iter().take(MAX_ROWS + 1) {
            let mapping = Mapping { namespace, directory, enabled: true };
            if valid(&mapping) {
                rows.push(mapping);
            } else {
                unknown = true;
            }
        }
    }
    if rows.len() > MAX_ROWS {
        rows.truncate(MAX_ROWS);
        unknown = true;
    }
    let mut found = Vec::new();
    let mut seen = BTreeSet::new();
    for row in rows {
        if !row.enabled {
            continue;
        }
        if directory(project, &root, &row).is_err() {
            unknown = true;
        }
        if seen.insert((row.namespace.clone(), row.directory.clone())) {
            found.push(row);
        }
    }
    (found, unknown)
}
pub fn affects(relative: &str, directories: &[String]) -> bool {
    directories.iter().any(|directory| {
        relative == directory || directory.starts_with(&format!("{relative}/")) || relative.starts_with(&format!("{directory}/"))
    })
}

struct Draft {
    token: String,
    root: String,
    baseline: Option<String>,
    rows: Vec<Mapping>,
    at: Option<usize>,
    mapping: Mapping,
    error: String,
    rejected: BTreeSet<String>,
}
#[derive(Default)]
pub struct Editor {
    draft: Option<Draft>,
    next: u64,
}
fn root(ctx: &Invocation) -> Result<(Project, String), Error> {
    let project = ctx.project().ok_or("Open a project before configuring Twig paths.")?;
    let root = project.root()?;
    if root.is_empty() {
        return Err("Open a project first.".into());
    }
    Ok((project, root))
}
impl Editor {
    fn control_key(&self, key: &str) -> String {
        self.draft.as_ref().map_or_else(|| key.to_owned(), |draft| format!("{key}@{}", draft.token))
    }
    fn scoped_key<'a>(&self, id: &'a str) -> Result<&'a str, Error> {
        let (key, token) = id.rsplit_once('@').ok_or(Error::Stale)?;
        if self.draft.as_ref().is_none_or(|draft| draft.token != token) {
            return Err(Error::Stale);
        }
        Ok(key)
    }
    pub fn render(&mut self, ctx: &Invocation) -> View {
        let Ok((_project, root)) = root(ctx) else {
            return View::message("Open a project to configure its Twig path hints.");
        };
        if self.draft.as_ref().is_some_and(|draft| draft.root != root) {
            self.draft = None;
        }
        if let Some(draft) = &self.draft {
            let destination = format!("{}/{}", draft.root, draft.mapping.directory);
            return View::new(Body::Form(vec![
                Field::new(
                    &self.control_key("twig-path-namespace"),
                    "Namespace (without @; empty = main)",
                    FieldValue::Text(draft.mapping.namespace.clone()),
                ),
                Field::new(
                    &self.control_key("twig-path-directory"),
                    "Existing project-relative directory",
                    FieldValue::Text(draft.mapping.directory.clone()),
                ),
                Field::new(
                    &self.control_key("twig-path-enabled"),
                    "Enabled for plugin navigation",
                    FieldValue::Checkbox(draft.mapping.enabled),
                ),
                Field::new("twig-path-preview", "Resolves to", FieldValue::readonly_text(&destination, false)),
                Field::new("twig-path-error", "Notes", FieldValue::readonly_text(&draft.error, true)),
            ]))
            .button(Button::new(&self.control_key("twig-path-save"), "Save").icon("check"))
            .button(Button::new(&self.control_key("twig-path-cancel"), "Cancel").icon("close"))
            .status("Editor hint only · does not edit effective runtime loader paths");
        }
        let rows = match load(&root) {
            Ok(rows) => rows,
            Err(error) => {
                return View::message(&format!("Cannot load project Twig hints: {error}. Optional storage permission is required."));
            }
        };
        let table = Table {
            columns: ["Namespace", "Directory", "Enabled", "Origin"].map(str::to_string).to_vec(),
            total_rows: rows.len() as u32,
            first_row: 0,
            rows: rows
                .iter()
                .map(|row| {
                    vec![
                        if row.namespace.is_empty() { "(main)".into() } else { format!("@{}", row.namespace) },
                        row.directory.clone(),
                        row.enabled.to_string(),
                        "Project plugin hint (not runtime config)".into(),
                    ]
                })
                .collect(),
            editable: false,
        };
        View::new(Body::Table(table))
            .button(Button::new("twig-path-add", "Add Path…").icon("plus").enabled(rows.len() < MAX_ROWS))
            .button(Button::new("twig-path-edit", "Edit Path…").icon("edit").enabled(!rows.is_empty()))
            .button(Button::new("twig-path-remove", "Remove Path…").icon("minus").enabled(!rows.is_empty()))
            .status(&format!("{} project hints · legacy scalar settings remain separate · runtime configuration unchanged", rows.len()))
    }
    pub fn handle(&mut self, event: &ViewEvent, ctx: &Invocation) -> Result<bool, Error> {
        let (project, root) = root(ctx)?;
        if self.draft.as_ref().is_some_and(|draft| draft.root != root) {
            self.draft = None;
            return Err(Error::Stale);
        }
        let normalized = match event {
            ViewEvent::Field(key, value) => ViewEvent::Field(self.scoped_key(key)?.into(), value.clone()),
            ViewEvent::Button(id) if id.starts_with("twig-path-save") || id.starts_with("twig-path-cancel") => {
                ViewEvent::Button(self.scoped_key(id)?.into())
            }
            _ => event.clone(),
        };
        match &normalized {
            ViewEvent::Field(key, value) => {
                let draft = self.draft.as_mut().ok_or(Error::Stale)?;
                if value.len() > 512 {
                    draft.rejected.insert(key.clone());
                    draft.error = "Field exceeds 512 bytes; correct it before saving.".into();
                    return Ok(false);
                }
                draft.rejected.remove(key);
                match key.as_str() {
                    "twig-path-namespace" => draft.mapping.namespace = value.clone(),
                    "twig-path-directory" => draft.mapping.directory = value.clone(),
                    "twig-path-enabled" => {
                        draft.mapping.enabled = match value.as_str() {
                            "true" => true,
                            "false" => false,
                            _ => return Err("Invalid Enabled value.".into()),
                        }
                    }
                    _ => return Err("Unknown Twig settings field.".into()),
                }
                draft.error = "Not saved yet. Click Save to apply.".into();
                Ok(false)
            }
            ViewEvent::Button(id) if id == "twig-path-cancel" => {
                self.draft = None;
                Ok(false)
            }
            ViewEvent::Button(id) if id == "twig-path-save" => {
                let draft = self.draft.as_mut().ok_or(Error::Stale)?;
                let result = (|| {
                    if !draft.rejected.is_empty() {
                        return Err("Correct oversized fields before saving.".into());
                    }
                    directory(&project, &root, &draft.mapping)?;
                    let fresh = host::storage::get(&key(&root))?;
                    if fresh != draft.baseline {
                        return Err(Error::Stale);
                    }
                    let mut rows = draft.rows.clone();
                    if let Some(at) = draft.at {
                        *rows.get_mut(at).ok_or(Error::Stale)? = draft.mapping.clone();
                    } else {
                        rows.push(draft.mapping.clone());
                    }
                    if rows.len() > MAX_ROWS {
                        return Err("At most 32 mappings are supported.".into());
                    }
                    let raw=json!({"version":1,"root":root,"paths":rows.iter().map(|row|json!({"namespace":row.namespace,"directory":row.directory,"enabled":row.enabled})).collect::<Vec<_>>()}).to_string();
                    decode(&root, Some(&raw))?;
                    host::storage::set(&key(&root), &raw)?;
                    Ok(())
                })();
                match result {
                    Ok(()) => {
                        self.draft = None;
                        Ok(true)
                    }
                    Err(error) => {
                        draft.error = format!("Not saved: {error}. Correct the path, or Cancel/reopen if another edit changed storage.");
                        Ok(false)
                    }
                }
            }
            ViewEvent::Button(id) if ["twig-path-add", "twig-path-edit", "twig-path-remove"].contains(&id.as_str()) => {
                if self.draft.is_some() {
                    return Err("Save or cancel the current path first.".into());
                }
                let baseline = host::storage::get(&key(&root))?;
                let rows = decode(&root, baseline.as_deref())?;
                let at = if id == "twig-path-add" {
                    if rows.len() >= MAX_ROWS {
                        return Err("At most 32 mappings are supported.".into());
                    }
                    None
                } else {
                    let items: Vec<_> = rows
                        .iter()
                        .enumerate()
                        .map(|(at, row)| PickItem::new(&at.to_string(), &format!("@{} → {}", row.namespace, row.directory)))
                        .collect();
                    let Some(chosen) = host::pick("Project Twig path mapping", &items)? else {
                        return Ok(false);
                    };
                    Some(chosen.parse::<usize>().ok().filter(|at| *at < rows.len()).ok_or(Error::Stale)?)
                };
                if ctx.project().and_then(|project| project.root().ok()).as_deref() != Some(root.as_str())
                    || host::storage::get(&key(&root))? != baseline
                {
                    return Err(Error::Stale);
                }
                if id == "twig-path-remove" {
                    let mut kept = rows;
                    kept.remove(at.ok_or(Error::Stale)?);
                    let raw=json!({"version":1,"root":root,"paths":kept.iter().map(|row|json!({"namespace":row.namespace,"directory":row.directory,"enabled":row.enabled})).collect::<Vec<_>>()}).to_string();
                    host::storage::set(&key(&root), &raw)?;
                    return Ok(true);
                }
                let mapping =
                    at.map(|at| rows[at].clone()).unwrap_or(Mapping { namespace: String::new(), directory: String::new(), enabled: true });
                use std::hash::BuildHasher;
                self.next = self.next.checked_add(1).ok_or(Error::Stale)?;
                let random = std::collections::hash_map::RandomState::new();
                let token = format!(
                    "{:016x}{:016x}",
                    random.hash_one((&root, self.next, "path draft")),
                    random.hash_one((&root, self.next, "path controls"))
                );
                self.draft = Some(Draft {
                    token,
                    root,
                    baseline,
                    rows,
                    at,
                    mapping,
                    error: "Used by this plugin's navigation only; your Symfony and Twig configuration is not changed.".into(),
                    rejected: BTreeSet::new(),
                });
                Ok(false)
            }
            _ => Ok(false),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    fn mapping(namespace: &str, directory: &str) -> Mapping {
        Mapping { namespace: namespace.into(), directory: directory.into(), enabled: true }
    }
    fn store(root: &str, rows: &[Mapping]) {
        let raw = json!({"version":1,"root":root,"paths":rows.iter().map(|row| json!({"namespace":row.namespace,"directory":row.directory,"enabled":row.enabled})).collect::<Vec<_>>()}).to_string();
        host::storage::set(&key(root), &raw).unwrap();
    }
    fn button(editor: &mut Editor, host: &MockHost, id: &str) -> bool {
        let id = if ["twig-path-save", "twig-path-cancel"].contains(&id) { editor.control_key(id) } else { id.into() };
        editor.handle(&ViewEvent::Button(id), &host.invocation()).unwrap()
    }
    fn field(editor: &mut Editor, host: &MockHost, key: &str, value: &str) {
        let key = editor.control_key(key);
        editor.handle(&ViewEvent::Field(key, value.into()), &host.invocation()).unwrap();
    }
    #[test]
    fn replaced_draft_rejects_old_fields_save_and_cancel() {
        let host = MockHost::install();
        host.add_dir("custom");
        let mut editor = Editor::default();
        button(&mut editor, &host, "twig-path-add");
        let old_field = editor.control_key("twig-path-directory");
        let old_save = editor.control_key("twig-path-save");
        let old_cancel = editor.control_key("twig-path-cancel");
        button(&mut editor, &host, "twig-path-cancel");
        button(&mut editor, &host, "twig-path-add");
        assert_ne!(old_field, editor.control_key("twig-path-directory"));
        for event in [ViewEvent::Field(old_field, "custom".into()), ViewEvent::Button(old_save), ViewEvent::Button(old_cancel)] {
            assert!(editor.handle(&event, &host.invocation()).is_err());
        }
        assert!(editor.draft.is_some());
        assert!(editor.draft.as_ref().unwrap().mapping.directory.is_empty());
        assert!(load("/project").unwrap().is_empty());
        field(&mut editor, &host, "twig-path-directory", "custom");
        assert!(button(&mut editor, &host, "twig-path-save"));
        assert_eq!(load("/project").unwrap(), vec![mapping("", "custom")]);
    }
    #[test]
    fn save_cancel_and_oversized_fields_are_storage_only() {
        let host = MockHost::install().with_capabilities(&["project.read", "storage"]);
        host.add_dir("custom");
        host.add_file("custom/a.twig", "original");
        let mut editor = Editor::default();
        assert!(!button(&mut editor, &host, "twig-path-add"));
        field(&mut editor, &host, "twig-path-namespace", "Mail");
        field(&mut editor, &host, "twig-path-directory", "custom");
        field(&mut editor, &host, "twig-path-directory", &"x".repeat(513));
        assert!(!button(&mut editor, &host, "twig-path-save"));
        assert!(load("/project").unwrap().is_empty());
        field(&mut editor, &host, "twig-path-directory", "custom");
        assert!(button(&mut editor, &host, "twig-path-save"));
        assert_eq!(load("/project").unwrap(), vec![mapping("Mail", "custom")]);
        assert!(!button(&mut editor, &host, "twig-path-add"));
        field(&mut editor, &host, "twig-path-directory", "custom");
        button(&mut editor, &host, "twig-path-cancel");
        assert_eq!(load("/project").unwrap().len(), 1);
        assert_eq!(host.invocation().project().unwrap().read_file("custom/a.twig").unwrap(), "original");
    }
    #[test]
    fn concurrent_storage_and_project_switch_refuse_stale_save() {
        let host = MockHost::install();
        host.add_dir("custom");
        let mut editor = Editor::default();
        button(&mut editor, &host, "twig-path-add");
        field(&mut editor, &host, "twig-path-directory", "custom");
        store("/project", &[mapping("Other", "custom")]);
        assert!(!button(&mut editor, &host, "twig-path-save"));
        assert_eq!(load("/project").unwrap(), vec![mapping("Other", "custom")]);
        host.set_project_root(Some("/other"));
        assert!(editor.handle(&ViewEvent::Button("twig-path-save".into()), &host.invocation()).is_err());
        assert!(editor.draft.is_none());
        assert!(load("/other").unwrap().is_empty());
    }
    #[test]
    fn storage_schema_and_paths_do_not_coerce_unknown_or_unsafe_values() {
        let _host = MockHost::install();
        for path in ["../other", "/absolute", "a/../b", "a//b", "./a", "a/*", "%kernel.project_dir%/a", "a\\b", "a/", ""] {
            assert!(!valid(&mapping("Mail", path)), "{path}");
        }
        assert!(!valid(&mapping("@Mail", "a")));
        assert!(valid(&mapping("", "nested/templates")));
        let raw = json!({"version":1,"root":"/other","paths":[]}).to_string();
        assert!(decode("/project", Some(&raw)).is_err());
        store("/project", &[mapping("Mail", "custom"), mapping("Mail", "custom")]);
        assert!(load("/project").is_err());
        let raw = json!({"version":1,"root":"/project","paths":[],"future":true}).to_string();
        assert!(decode("/project", Some(&raw)).is_err());
    }
    #[test]
    fn directory_validation_and_disabled_rows_are_explicit() {
        let host = MockHost::install();
        host.add_dir("nested");
        host.add_file("nested/file", "file");
        let project = host.invocation().project().unwrap();
        assert!(directory(&project, "/project", &mapping("Mail", "nested/file")).is_err());
        assert!(directory(&project, "/project", &mapping("Mail", "missing")).is_err());
        let mut disabled = mapping("Mail", "missing");
        disabled.enabled = false;
        assert!(directory(&project, "/project", &disabled).is_ok());
        store("/project", &[disabled]);
        assert_eq!(configured(&project), (vec![], false));
    }
    #[test]
    fn navigation_hints_are_root_scoped_and_collisions_are_suppressed() {
        let host = MockHost::install();
        for dir in ["custom", "other"] {
            host.add_dir(dir);
            host.add_file(&format!("{dir}/same.twig"), "body");
        }
        store("/project", &[mapping("Mail", "custom")]);
        let project = host.invocation().project().unwrap();
        let index = Index::build(&project);
        assert_eq!(index.templates["@Mail/same.twig"].path, "/project/custom/same.twig");
        assert!(index.templates["@Mail/same.twig"].detail.as_ref().unwrap().contains("may differ at runtime"));
        assert!(index.template_paths_unknown);
        store("/project", &[mapping("Mail", "custom"), mapping("Mail", "other")]);
        let index = Index::build(&project);
        assert!(!index.templates.contains_key("@Mail/same.twig"));
        assert!(index.ambiguous_templates.contains("@Mail/same.twig"));
        host.set_project_root(Some("/different"));
        assert!(load("/different").unwrap().is_empty());
    }
    #[test]
    fn invalid_legacy_and_missing_paths_remain_unknown_and_watchable() {
        let host = MockHost::install();
        host.set_setting("twigNamespaces", r#""Bad=/outside, Missing=custom""#);
        let project = host.invocation().project().unwrap();
        let (rows, unknown) = configured(&project);
        assert!(unknown);
        assert_eq!(rows, vec![mapping("Missing", "custom")]);
        let index = Index::build(&project);
        assert!(index.template_directories.contains(&"custom".to_string()));
        assert!(affects("custom/a.twig", &index.template_directories));
        assert!(affects("custom/readme.md", &index.template_directories));
        assert!(affects("custom/folder.with.dot", &index.template_directories));
        assert!(!affects("custom-other/a.twig", &index.template_directories));
        assert!(affects("custom", &index.template_directories));
        for path in ["config", "templates", "translations", "src/Controller"] {
            assert!(affects_index(path));
        }
    }
    #[test]
    fn optional_storage_denial_is_not_an_empty_complete_configuration() {
        let host = MockHost::install().with_capabilities(&["project.read"]);
        assert!(load("/project").is_err());
        assert!(configured(&host.invocation().project().unwrap()).1);
        let mut editor = Editor::default();
        assert!(editor.handle(&ViewEvent::Button("twig-path-add".into()), &host.invocation()).is_err());
    }
    #[test]
    fn filesystem_batches_filter_hint_changes_and_prune_closed_roots() {
        let host = MockHost::install();
        host.add_file("composer.json", r#"{"require":{"symfony/framework-bundle":"*"}}"#);
        host.add_dir("custom");
        host.add_file("custom/a.twig", "a");
        store("/project", &[mapping("Mail", "custom")]);
        let mut plugin = Main::default();
        plugin.index_project(&host.invocation().project().unwrap());
        assert!(plugin.indexes.contains_key("/project"));
        host.add_file("custom/new.twig", "new");
        let event = |path: &str| Event::FilesChanged(openidea_plugin::FilesChanged { created: vec![path.into()], ..Default::default() });
        plugin.handle_events(&[event("/project/README.md")], &host.invocation());
        assert!(!plugin.indexes["/project"].templates.contains_key("@Mail/new.twig"));
        plugin.handle_events(&[event("/project/custom/new.twig")], &host.invocation());
        assert!(plugin.indexes["/project"].templates.contains_key("@Mail/new.twig"));
        host.set_project_root(Some("/different"));
        plugin.handle_events(&[event("/different/README.md")], &host.invocation());
        assert!(!plugin.indexes.contains_key("/project"));
    }
    #[test]
    fn oversized_batches_drop_stale_facts_instead_of_truncating_refresh() {
        let host = MockHost::install();
        let mut plugin = Main::default();
        plugin.indexes.insert("/project".into(), Index::default());
        let files = openidea_plugin::FilesChanged {
            removed: (0..1025).map(|n| format!("/project/custom/{n}.twig")).collect(),
            ..Default::default()
        };
        plugin.handle_events(&[Event::FilesChanged(files)], &host.invocation());
        assert!(plugin.indexes.is_empty());
    }
}
