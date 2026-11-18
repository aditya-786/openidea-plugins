//! Atomic editor intentions. Resolution confirms concrete edits before returning them.
use super::*;
use generate::{MAX_TEXT, ParentLog};
use openidea_plugin::file_actions::{FileAction, FileActions, FileOperation};
use openidea_plugin::{FileKind, TextEdit};
const NEW: &str = "new-changelog";
const SET: &str = "new-changeset";
const INCLUDE: &str = "include-existing";
const GENERATED: &str = "save-generated:";
const GENERATED_GROUPS: &str = "save-generated-groups:";
fn root(project: &Project) -> Result<String, Error> {
    let root = project.root()?;
    if !root.starts_with('/') || root.len() > 4096 || root.split('/').any(|p| matches!(p, "." | "..")) {
        return Err(Error::Stale);
    }
    Ok(root)
}
fn path(root: &str, input: &str) -> Result<String, Error> {
    let relative =
        if input.starts_with('/') { input.strip_prefix(&format!("{}/", root.trim_end_matches('/'))).ok_or(Error::Stale)? } else { input };
    if relative.is_empty()
        || relative.len() > 1024
        || relative.chars().any(|c| c.is_control() || matches!(c, '\\' | ':' | '*' | '?' | '{' | '}' | '[' | ']'))
        || relative.split('/').any(|p| p.is_empty() || matches!(p, "." | ".."))
    {
        return Err("Use a concrete path inside the project without traversal, properties or wildcards.".into());
    }
    Ok(format!("{}/{relative}", root.trim_end_matches('/')))
}
fn current_project(doc: &Document, expected: &str) -> Result<Project, Error> {
    let project = project_of(&doc.path).ok_or(Error::Stale)?;
    if root(&project)? != expected || path(expected, &doc.path)? != doc.path || project.read_file(&doc.path)? != doc.text {
        return Err(Error::Stale);
    }
    Ok(project)
}
fn owned(root: &str, target: &str) -> Result<(), Error> {
    if project_of(target).and_then(|p| p.root().ok()).as_deref() != Some(root) {
        return Err(Error::Stale);
    }
    Ok(())
}
fn vacant(project: &Project, target: &str) -> Result<(), Error> {
    if project.exists(target)? {
        return Err("The destination already exists; nothing will be overwritten.".into());
    }
    match project.read_file(target) {
        Ok(_) => return Err("The destination has an open buffer.".into()),
        Err(Error::NotFound(_)) => {}
        Err(e) => return Err(e),
    }
    let base = root(project)?;
    let relative = target.strip_prefix(&format!("{}/", base.trim_end_matches('/'))).ok_or(Error::Stale)?;
    let parts: Vec<_> = relative.split('/').collect();
    for n in 1..parts.len() {
        let parent = parts[..n].join("/");
        if project.exists(&parent)? && project.stat(&parent)?.kind != FileKind::Directory {
            return Err("A destination parent is not a directory.".into());
        }
    }
    Ok(())
}
fn fingerprint(text: &str) -> String {
    format!("{:016x}", text.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100000001b3)))
}
fn link(log: &ParentLog, doc: &Document, root: &str, target: &str) -> Result<TextEdit, Error> {
    if log.opaque {
        return Err("xml:base/logicalFilePath makes include ownership ambiguous for this workflow.".into());
    }
    let relative = target
        .strip_prefix(&format!("{}/", directory(&doc.path)))
        .ok_or("Linked changelogs must be beside or below the current parent.")?;
    for include in &log.includes {
        let candidates = changelog::candidates(&doc.path, root, include);
        if include.dir || candidates.is_empty() {
            return Err("An includeAll or unresolved include prevents conservative duplicate-include checking.".into());
        }
        if candidates.iter().any(|p| p == target) {
            return Err("This changelog already includes the selected destination.".into());
        }
    }
    log.include(&doc.text, relative)
}
fn preview(doc: &Document, edit: &TextEdit) -> String {
    let before =
        doc.text.get(offset(&doc.text, edit.range.start).unwrap_or(0)..offset(&doc.text, edit.range.end).unwrap_or(0)).unwrap_or("");
    format!(
        "Edit: {} (editor version {})\nRange: {}:{}–{}:{}\nReplace source:\n{}\n\nWith source:\n{}",
        doc.path,
        doc.version,
        edit.range.start.line + 1,
        edit.range.start.character + 1,
        edit.range.end.line + 1,
        edit.range.end.character + 1,
        ui_kit::prompts::readable_source(before),
        ui_kit::prompts::readable_source(&edit.text)
    )
}
fn suggested_author(plugin: &Liquibase) -> String {
    // Lazy actions may prompt, but must not launch Git or reveal views. Use
    // configured/already-cached hints only; the author is explicitly editable.
    host::setting_string("author")
        .filter(|a| generate::identity(a))
        .or_else(|| plugin.git_user.as_ref().and_then(Clone::clone).filter(|a| generate::identity(a)))
        .unwrap_or_else(|| "author".into())
}

fn done(title: &str, edits: Vec<(String, Vec<TextEdit>)>, operations: Vec<FileOperation>, open: Option<String>) -> FileAction {
    FileAction { title: title.into(), kind: Some("refactor.rewrite".into()), edits, operations, open, ..FileAction::default() }
}
impl Liquibase {
    pub(crate) fn generator_entry(&mut self, ctx: &Invocation, id: &str) -> Result<Option<String>, Error> {
        const UNAVAILABLE: &str = "Open a Liquibase changelog (XML, YAML, JSON or formatted SQL) first.";
        let project = ctx.project().ok_or("Open a project first.")?;
        let applied = match ctx.editor() {
            Some(editor) => {
                let prefix = if id == SET { "Add Liquibase changeset" } else { "Create Liquibase changelog" };
                self.command_resolving = true;
                let applied = ui_kit::command_file_action::run_titled(self, &project, &editor, prefix, TITLE);
                self.command_resolving = false;
                applied?
            }
            None => false,
        };
        if applied {
            return Ok(None);
        }
        if id == SET {
            return Err(UNAVAILABLE.into());
        }
        self.first_changelog(&project)
    }
    /// New Changelog… without a changelog open: asks for a path and creates a standalone changelog there.
    fn first_changelog(&self, project: &Project) -> Result<Option<String>, Error> {
        const DEFAULT: &str = "src/main/resources/db/changelog/db.changelog-master.yaml";
        let root = root(project)?;
        let Some(answer) = host::input("New changelog path (.xml, .yaml, .sql or .json)", Some(DEFAULT))? else { return Ok(None) };
        let target = path(&root, answer.trim())?;
        let format = [Format::Xml, Format::Yaml, Format::Sql, Format::Json]
            .into_iter()
            .find(|f| target.ends_with(&format!(".{}", generate::extension(*f))))
            .ok_or("End the changelog path with .xml, .yaml, .sql or .json.")?;
        vacant(project, &target)?;
        let source = generate::changelog_source(format, &suggested_author(self))?;
        project.write_file(&target, &source).map_err(|e| ui_kit::denied::map(e, "Creating a changelog", "project.write", TITLE))?;
        let _ = host::open_editor(&target, None);
        Ok(None)
    }
}
impl FileActions for Liquibase {
    fn file_actions(&mut self, doc: &Document, _selection: Range) -> Vec<FileAction> {
        if doc.text.len() > MAX_TEXT || ![".xml", ".yaml", ".yml", ".sql", ".json"].iter().any(|e| doc.path.ends_with(e)) {
            return Vec::new();
        }
        let Some(project) = project_of(&doc.path) else { return Vec::new() };
        let Ok(root) = root(&project) else { return Vec::new() };
        if path(&root, &doc.path).is_err() {
            return Vec::new();
        }
        let parent = generate::parent_log(&doc.path, &doc.text).is_ok();
        let sql = doc.path.ends_with(".sql") && changelog::parse(&doc.path, &doc.text).is_some_and(|l| l.format == Format::Sql && l.header);
        let mut actions: Vec<FileAction> = [(NEW, "Create Liquibase changelog…"), (SET, "Add Liquibase changeset…"), (INCLUDE, "Include existing changelog…")]
            .into_iter()
            .filter(|(kind, _)| *kind == NEW || *kind == SET && (parent || sql) || *kind == INCLUDE && parent)
            .map(|(kind, title)| FileAction {
                title: title.into(),
                kind: Some("refactor.rewrite".into()),
                data: Some(serde_json::json!([root, doc.path, doc.version, fingerprint(&doc.text), kind]).to_string()),
                ..FileAction::default()
            })
            .collect();
        if let Some(key) = self.generated_groups_key(&root) {
            actions.push(FileAction { title: "Save grouped offline migrations…".into(), kind: Some("refactor.rewrite".into()),
                data: Some(serde_json::json!([root, doc.path, doc.version, fingerprint(&doc.text), format!("{GENERATED_GROUPS}{key}")]).to_string()), ..FileAction::default() });
        }
        if let Some(key) = self.generated_action_key(&root) {
            actions.push(FileAction { title: "Save reviewed offline changesets…".into(), kind: Some("refactor.rewrite".into()), data: Some(serde_json::json!([root, doc.path, doc.version, fingerprint(&doc.text), format!("{GENERATED}{key}")]).to_string()), ..FileAction::default() });
        }
        actions
    }
    fn resolve_file_action(&mut self, doc: &Document, action: FileAction) -> Result<FileAction, Error> {
        let notify_failure = !self.command_resolving;
        (|| {
        let data: serde_json::Value =
            serde_json::from_str(action.data.as_deref().filter(|d| d.len() < 8192).ok_or(Error::Stale)?).map_err(|_| Error::Stale)?;
        let data = data.as_array().filter(|a| a.len() == 5).ok_or(Error::Stale)?;
        let expected = data[0].as_str().ok_or(Error::Stale)?;
        if doc.text.len() > MAX_TEXT
            || data[1].as_str() != Some(doc.path.as_str())
            || data[2].as_u64() != Some(doc.version)
            || data[3].as_str() != Some(fingerprint(&doc.text).as_str())
        {
            return Err(Error::Stale);
        }
        let project = current_project(doc, expected)?;
        let kind = data[4].as_str().ok_or(Error::Stale)?;
        let parent = generate::parent_log(&doc.path, &doc.text).ok();
        if let Some(key) = kind.strip_prefix(GENERATED_GROUPS) {
            let source = self.prepared_generated_groups(&project, expected, key)?;
            let mut targets = BTreeSet::new(); let mut operations = Vec::new(); let mut preview = source.provenance.clone();
            for (index, (label, text)) in source.groups.iter().enumerate() {
                let Some(input) = host::input(&format!("Destination XML for {label}"), Some(&format!("db/migration-{}.xml", index + 1)))? else { return Err(Error::Cancelled); };
                let target = path(expected, &input)?;
                if !target.ends_with(".xml") || !targets.insert(target.clone()) { return Err("Choose distinct XML output files for migration groups.".into()); }
                owned(expected, &target)?; vacant(&project, &target)?;
                preview.push_str(&format!("\n\nGroup: {label}\nCreate: {target}\n{text}"));
                operations.push(FileOperation::Create { path: target, text: text.clone(), overwrite: false });
            }
            preview.push_str("\nOne atomic non-overwriting workspace action; no CLI or database apply. Groups are saved in label order; dependencies and execution order require review.");
            if !ui_kit::prompts::confirm_exact("Save these grouped offline migrations?", &preview)? { return Err(Error::Cancelled); }
            let current = current_project(doc, expected)?;
            let fresh = self.prepared_generated_groups(&current, expected, key)?;
            if fresh.groups != source.groups { return Err(Error::Stale); }
            for target in &targets { owned(expected, target)?; vacant(&current, target)?; }
            let open = targets.iter().next().cloned();
            return Ok(done("Save grouped offline migrations", vec![], operations, open));
        }
        if let Some(key) = kind.strip_prefix(GENERATED) {
            if key.len() != 64 || !key.bytes().all(|b| b.is_ascii_hexdigit()) { return Err(Error::Stale); }
            let source = self.prepared_generated(&project, expected, key)?;
            let input = host::input("New project XML file for the reviewed offline changesets", Some("db/offline-changes.xml"))?.ok_or(Error::Cancelled)?;
            let mut target = path(expected, input.trim())?;
            if !target.ends_with(".xml") {
                if target.rsplit('/').next().is_some_and(|name| name.contains('.')) { return Err("Use an XML destination for generated changesets.".into()); }
                target.push_str(".xml");
            }
            owned(expected, &target)?;
            vacant(&project, &target)?;
            if !ui_kit::prompts::confirm_exact("Save these generated offline changesets?", &format!("Create (JSON): {}\n\n{}\n\nExact selected XML:\n{}\n\nOne non-overwriting, undoable workspace Create only. No CLI/database command runs; review omitted dependencies before future execution.", serde_json::to_string(&target).unwrap_or_default(), source.provenance, source.text))? { return Err(Error::Cancelled); }
            let current = current_project(doc, expected)?;
            owned(expected, &target)?;
            vacant(&current, &target)?;
            if self.prepared_generated(&current, expected, key)?.text != source.text { return Err(Error::Stale); }
            return Ok(done("Save reviewed offline changesets", vec![], vec![FileOperation::Create { path: target.clone(), text: source.text, overwrite: false }], Some(target)));
        }
        if kind == SET {
            let sql = changelog::parse(&doc.path, &doc.text).filter(|l| l.format == Format::Sql && l.header);
            let suggested = if let Some(log) = &parent {
                generate::next_numeric(log.identities.iter().map(|(id, _)| id.as_str()))
            } else if let Some(log) = &sql {
                generate::next_id(log)
            } else {
                return Err("Changesets require namespaced XML, a YAML/JSON changelog array or formatted SQL.".into());
            };
            if suggested.len() > 128 {
                return Err("Numeric changeset ids exceed this workflow's bound.".into());
            }
            let id = host::input("Changeset id (unique with author in this file)", Some(&suggested))?.ok_or(Error::Cancelled)?;
            let id = id.trim();
            let author = suggested_author(self);
            let author = host::input("Changeset author", Some(&author))?.ok_or(Error::Cancelled)?;
            let author = author.trim();
            if sql.as_ref().is_some_and(|l|l.change_sets.iter().any(|s|s.id.as_deref().is_none_or(|id|!generate::identity(id)) || s.author.as_deref().is_none_or(|a|!generate::identity(a)))) { return Err("Existing SQL changeset identities are incomplete or dynamic.".into()); }
            let edit = if let Some(log) = parent {
                log.changeset(&doc.text, id, author)?
            } else {
                if sql
                    .as_ref()
                    .is_some_and(|l| l.change_sets.iter().any(|s| s.id.as_deref() == Some(id) && s.author.as_deref() == Some(author)))
                {
                    return Err("This id/author pair already exists in this file.".into());
                }
                generate::sql_changeset(&doc.text, id, author)?
            };
            if !ui_kit::prompts::confirm_exact(
                "Add Liquibase changeset?",
                &format!(
                    "{}\n\nApply this skeleton only? No database command runs; fill its changes before execution.",
                    preview(doc, &edit)
                ),
            )? {
                return Err(Error::Cancelled);
            }
            current_project(doc, expected)?;
            return Ok(done("Add Liquibase changeset", vec![(doc.path.clone(), vec![edit])], vec![], None));
        }
        if kind == INCLUDE {
            let parent = parent.ok_or("Includes require a validated XML/YAML/JSON changelog parent.")?;
            let input = host::input("Existing XML, YAML or JSON changelog path inside this project", None)?.ok_or(Error::Cancelled)?;
            let target = path(expected, input.trim())?;
            owned(expected, &target)?;
            if target == doc.path || ![".xml", ".yaml", ".yml", ".json"].iter().any(|e| target.ends_with(e)) {
                return Err("Choose another XML, YAML or JSON changelog.".into());
            }
            let child = project.read_file(&target)?;
            let child_log = generate::parent_log(&target, &child)?;
            if !child_log.includes.is_empty() || child_log.opaque {
                return Err("Existing children with include graphs or opaque ownership are outside this safe subset.".into());
            }
            let edit = link(&parent, doc, expected, &target)?;
            if !ui_kit::prompts::confirm_exact(
                "Include existing Liquibase changelog?",
                &format!(
                    "Child: {target}\n\n{}\n\nApply one parent edit? Child contents are unchanged; no database command runs.",
                    preview(doc, &edit)
                ),
            )? {
                return Err(Error::Cancelled);
            }
            let current = current_project(doc, expected)?;
            owned(expected, &target)?;
            if current.read_file(&target)? != child {
                return Err(Error::Stale);
            }
            return Ok(done("Include Liquibase changelog", vec![(doc.path.clone(), vec![edit])], vec![], None));
        }
        if kind != NEW {
            return Err(Error::Stale);
        }
        let formats = [Format::Xml, Format::Yaml, Format::Sql, Format::Json];
        let items: Vec<_> =
            formats.iter().map(|f| PickItem::new(generate::extension(*f), &generate::extension(*f).to_uppercase())).collect();
        let selected = host::pick("New changelog format", &items)?.ok_or(Error::Cancelled)?;
        let format = formats.into_iter().find(|f| generate::extension(*f) == selected).ok_or(Error::Stale)?;
        let suggestion = format!("{}/changelog-1.{}", root_relative(directory(&doc.path), expected), generate::extension(format));
        let target = host::input("New changelog path inside this project", Some(&suggestion))?.ok_or(Error::Cancelled)?;
        let mut target = path(expected, target.trim())?;
        if !target.ends_with(&format!(".{}", generate::extension(format))) {
            if target.rsplit('/').next().is_some_and(|n| n.contains('.')) {
                return Err("The path suffix must match the selected format.".into());
            }
            target.push('.');
            target.push_str(generate::extension(format));
        }
        owned(expected, &target)?;
        vacant(&project, &target)?;
        let author = suggested_author(self);
        let author = host::input("Initial changeset author", Some(&author))?.ok_or(Error::Cancelled)?;
        let source = generate::changelog_source(format, author.trim())?;
        let edit = if let Some(parent) = parent {
            let mode = host::pick(
                "Parent relationship",
                &[
                    PickItem::new("linked", "Create + include atomically in this parent"),
                    PickItem::new("standalone", "Create standalone changelog"),
                ],
            )?
            .ok_or(Error::Cancelled)?;
            match mode.as_str() {
                "linked" => Some(link(&parent, doc, expected, &target)?),
                "standalone" => None,
                _ => return Err(Error::Stale),
            }
        } else {
            None
        };
        let summary =
            edit.as_ref().map(|e| preview(doc, e)).unwrap_or_else(|| "No parent include/edit. The new file is standalone.".into());
        if !ui_kit::prompts::confirm_exact(
            "Create Liquibase changelog?",
            &format!(
                "Create: {target}\n\nExact new contents:\n{source}\n\n{summary}\n\nApply as one undoable workspace action? No overwrite or database command; fill the skeleton before execution."
            ),
        )? {
            return Err(Error::Cancelled);
        }
        let current = current_project(doc, expected)?;
        owned(expected, &target)?;
        vacant(&current, &target)?;
        let edits = edit.map(|e| vec![(doc.path.clone(), vec![e])]).unwrap_or_default();
        Ok(done(
            "Create Liquibase changelog",
            edits,
            vec![FileOperation::Create { path: target.clone(), text: source, overwrite: false }],
            Some(target),
        ))
        })().inspect_err(|error| {
            if notify_failure && !matches!(error,Error::Cancelled) {host::notify(NotificationLevel::Warning,"Liquibase action could not be prepared",&error.to_string());}
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    fn setup() -> (MockHost, Liquibase, Document) {
        let host = MockHost::install().with_capabilities(&["project.read"]);
        host.set_setting("author", "\"ann\"");
        let text = format!("<lb:databaseChangeLog xmlns:lb=\"{}\">\n</lb:databaseChangeLog>\n", generate::NS);
        host.add_file("db/master.xml", &text);
        host.open_editor("/project/db/master.xml", "xml", &text, &[]);
        (host, Liquibase::default(), Document { path: "/project/db/master.xml".into(), language: "xml".into(), version: 1, text })
    }
    fn action(plugin: &mut Liquibase, doc: &Document, kind: &str) -> FileAction {
        plugin
            .file_actions(doc, Range::default())
            .into_iter()
            .find(|a| serde_json::from_str::<serde_json::Value>(a.data.as_deref().unwrap()).unwrap()[4] == kind)
            .unwrap()
    }
    fn answers(host: &MockHost, values: &[&str], confirm: bool) {
        for value in values {
            host.answer_next_prompt(Some(&[value]));
        }
        host.answer_next_prompt(if confirm { Some(&[]) } else { None });
    }
    fn edit_text(doc: &Document, edit: &TextEdit) -> String {
        let from = offset(&doc.text, edit.range.start).unwrap();
        let to = offset(&doc.text, edit.range.end).unwrap();
        format!("{}{}{}", &doc.text[..from], edit.text, &doc.text[to..])
    }
    const COMMAND_CAPABILITIES: [&str; 3] = ["project.read", "editor.read", "editor.write"];
    #[test]
    fn new_changeset_command_inserts_into_open_changelog() {
        let (host, mut plugin, _) = setup();
        let host = host.with_capabilities(&COMMAND_CAPABILITIES);
        answers(&host, &["7", "ann"], true);
        assert_eq!(plugin.run_command("new-changeset", &host.invocation(), None).unwrap(), None);
        assert_eq!(generate::xml_log(&host.editor_text()).unwrap().identities, [("7".into(), "ann".into())]);
        assert!(host.process_runs().is_empty());
    }
    #[test]
    fn new_changeset_command_reports_a_resolver_failure_once() {
        let (host, mut plugin, _) = setup();
        let host = host.with_capabilities(&COMMAND_CAPABILITIES);
        answers(&host, &["bad:id", "ann"], true);
        let error = plugin.run_command("new-changeset", &host.invocation(), None).unwrap_err();
        assert!(!error.to_string().is_empty());
        assert!(host.notifications().is_empty());
    }
    #[test]
    fn new_changelog_command_creates_file() {
        let (host, mut plugin, doc) = setup();
        let host = host.with_capabilities(&[COMMAND_CAPABILITIES.as_slice(), &["project.write"]].concat());
        answers(&host, &["xml", "db/two.xml", "ann", "standalone"], true);
        plugin.run_command("new-changelog", &host.invocation(), None).unwrap();
        let created = host.invocation().project().unwrap().read_file("db/two.xml").unwrap();
        assert_eq!(generate::xml_log(&created).unwrap().identities[0].1, "ann");
        assert_eq!(host.editor_text(), doc.text);
        assert_eq!(host.opened_locations(), [("/project/db/two.xml".to_owned(), 0, 0)]);
    }
    #[test]
    fn new_changelog_denied_write_names_permission() {
        let (host, mut plugin, _) = setup();
        let host = host.with_capabilities(&COMMAND_CAPABILITIES);
        answers(&host, &["xml", "db/two.xml", "ann", "standalone"], true);
        let error = plugin.run_command("new-changelog", &host.invocation(), None).unwrap_err().to_string();
        assert!(error.contains("needs the project.write permission"), "{error}");
        assert!(error.contains("Settings › Plugins › Liquibase Changelogs"), "{error}");
        assert!(!host.invocation().project().unwrap().exists("db/two.xml").unwrap());
    }
    #[test]
    fn generator_commands_need_a_changelog_editor() {
        let (host, mut plugin, _) = setup();
        let host = host.with_capabilities(&COMMAND_CAPABILITIES);
        host.add_file("notes.txt", "");
        host.open_editor("/project/notes.txt", "text", "", &[]);
        let error = plugin.run_command("new-changeset", &host.invocation(), None).unwrap_err().to_string();
        assert_eq!(error, "Open a Liquibase changelog (XML, YAML, JSON or formatted SQL) first.");
    }
    #[test]
    fn new_changelog_without_an_open_changelog_asks_for_a_path_and_creates_it() {
        let host = MockHost::install().with_capabilities(&["project.read"]);
        let mut plugin = Liquibase::default();
        host.answer_next_prompt(Some(&["db/two.xml"]));
        let denied = plugin.run_command("new-changelog", &host.invocation(), None).unwrap_err().to_string();
        assert!(denied.contains("needs the project.write permission"), "{denied}");
        let host = host.with_capabilities(&["project.read", "project.write", "editor.read"]);
        host.add_file("db/existing.xml", "");
        for (answer, error) in [("db/notes.txt", "End the changelog path"), ("db/existing.xml", "already exists"), ("../outside.xml", "inside the project")] {
            host.answer_next_prompt(Some(&[answer]));
            let message = plugin.run_command("new-changelog", &host.invocation(), None).unwrap_err().to_string();
            assert!(message.contains(error), "{answer}: {message}");
        }
        host.answer_next_prompt(None);
        plugin.run_command("new-changelog", &host.invocation(), None).unwrap();
        let project = host.invocation().project().unwrap();
        assert!(!project.exists("src/main/resources/db/changelog/db.changelog-master.yaml").unwrap());
        host.answer_next_prompt(Some(&["src/main/resources/db/changelog/db.changelog-master.yaml"]));
        plugin.run_command("new-changelog", &host.invocation(), None).unwrap();
        assert!(project.read_file("src/main/resources/db/changelog/db.changelog-master.yaml").unwrap().contains("databaseChangeLog"));
    }
    #[test]
    fn registered_xml_mutation_preserves_legal_unicode_namespace_prefix() {
        let (host,mut plugin,mut doc)=setup();doc.text=doc.text.replace("lb:","é:").replace("xmlns:lb", "xmlns:é")
            .replace("xmlns:é=", "xmlns:a='urn:a' xmlns:b='urn:b' a:hint='one' b:hint='two' hint='plain' xmlns:é=");
        host.add_file("db/master.xml",&doc.text);host.open_editor(&doc.path,"xml",&doc.text,&[]);
        let lazy=action(&mut plugin,&doc,SET);answers(&host,&["1","ann"],true);
        let resolved=plugin.resolve_file_action(&doc,lazy).unwrap();let changed=edit_text(&doc,&resolved.edits[0].1[0]);
        assert!(changed.contains("<é:changeSet"));assert!(changed.contains("a:hint='one' b:hint='two' hint='plain'"));assert!(generate::xml_log(&changed).is_ok());assert!(host.process_runs().is_empty());
    }
    #[test]
    fn registered_xml_mutation_requires_legal_declaration_context_and_preserves_prolog() {
        for prefix in ["<?xml version='1.0' version='1.0'?>", "<?xml encoding='UTF-8' version='1.0'?>", "<?xml version='1.0' standalone='maybe'?>", "<?XML version='1.0'?>", " <?xml version='1.0'?>", "<?xml version='1.0' extra='value'?>"] {
            let (_host,mut plugin,mut doc)=setup();doc.text=format!("{prefix}{}",doc.text);
            assert!(!plugin.file_actions(&doc,Range::default()).iter().any(|a|a.title=="Add Liquibase changeset…"),"{prefix}");
        }
        let (host,mut plugin,mut doc)=setup();
        let prefix="<?xml version='1.0' encoding='UTF-8' standalone='yes'?>\n<?m2e retained?>\n";
        doc.text=format!("{prefix}{}",doc.text);host.add_file("db/master.xml",&doc.text);host.open_editor(&doc.path,"xml",&doc.text,&[]);
        let lazy=action(&mut plugin,&doc,SET);answers(&host,&["1","ann"],true);
        let resolved=plugin.resolve_file_action(&doc,lazy).unwrap();let changed=edit_text(&doc,&resolved.edits[0].1[0]);
        assert!(changed.starts_with(prefix));assert!(generate::xml_log(&changed).is_ok());assert!(host.process_runs().is_empty());
    }
    #[test]
    fn linked_creation_is_one_non_overwriting_create_plus_one_parent_edit_after_exact_confirmation() {
        let (host, mut plugin, doc) = setup();
        let lazy = action(&mut plugin, &doc, NEW);
        assert!(lazy.operations.is_empty() && lazy.edits.is_empty());
        answers(&host, &["xml", "db/changes/two.xml", "Jane & \"Doe\"", "linked"], true);
        let result = plugin.resolve_file_action(&doc, lazy).unwrap();
        let [FileOperation::Create { path, text, overwrite }] = result.operations.as_slice() else { panic!("one create") };
        assert_eq!(path, "/project/db/changes/two.xml");
        assert!(!overwrite);
        assert_eq!(result.open.as_deref(), Some(path.as_str()));
        assert_eq!(generate::xml_log(text).unwrap().identities[0].1, "Jane & \"Doe\"");
        assert_eq!(result.edits.len(), 1);
        assert_eq!(result.edits[0].0, doc.path);
        let changed = edit_text(&doc, &result.edits[0].1[0]);
        let log = generate::xml_log(&changed).unwrap();
        assert_eq!(log.includes[0].path, "changes/two.xml");
        assert!(changed.contains("<lb:include"));
        assert!(!host.invocation().project().unwrap().exists(path).unwrap(), "resolve returns a transaction, never writes directly");
        assert_eq!(host.editor_text(), doc.text);
        assert!(host.process_runs().is_empty());
        let preview = preview(&doc, &result.edits[0].1[0]);
        assert!(preview.contains("/project/db/master.xml") && preview.contains("editor version 1") && preview.contains("changes/two.xml"));
    }
    #[test]
    fn standalone_formats_create_without_unintended_parent_edits() {
        for format in ["xml", "yaml", "sql", "json"] {
            let (host, mut plugin, doc) = setup();
            let lazy = action(&mut plugin, &doc, NEW);
            answers(&host, &[format, &format!("db/new.{format}"), "Jane Doe", "standalone"], true);
            let result = plugin.resolve_file_action(&doc, lazy).unwrap();
            assert!(result.edits.is_empty());
            assert_eq!(result.operations.len(), 1);
            let FileOperation::Create { text, overwrite, .. } = &result.operations[0] else { panic!() };
            assert!(!overwrite);
            assert!(changelog::parse(&format!("new.{format}"), text).is_some());
            assert!(host.process_runs().is_empty());
        }
    }
    #[test]
    fn xml_and_sql_changesets_are_confirmed_local_edits_and_reject_duplicate_identity() {
        let (host, mut plugin, doc) = setup();
        let lazy = action(&mut plugin, &doc, SET);
        answers(&host, &["2", "Jane & \"Doe\""], true);
        let result = plugin.resolve_file_action(&doc, lazy).unwrap();
        assert!(result.operations.is_empty());
        let text = edit_text(&doc, &result.edits[0].1[0]);
        assert_eq!(generate::xml_log(&text).unwrap().identities, [("2".into(), "Jane & \"Doe\"".into())]);
        host.open_editor(&doc.path, "xml", &text, &[]);
        let d = Document { text, ..doc };
        let lazy = action(&mut plugin, &d, SET);
        answers(&host, &["2", "Jane & \"Doe\""], true);
        assert!(plugin.resolve_file_action(&d, lazy).is_err());
        let (host, mut plugin, _) = setup();
        let text = "--liquibase formatted sql\r\n--changeset \"Jane Doe\":1\r\nselect 1;";
        host.add_file("db/log.sql", text);
        host.open_editor("/project/db/log.sql", "sql", text, &[]);
        let d = Document { path: "/project/db/log.sql".into(), language: "sql".into(), version: 1, text: text.into() };
        let lazy = action(&mut plugin, &d, SET);
        answers(&host, &["2", "Jane Doe"], true);
        let result = plugin.resolve_file_action(&d, lazy).unwrap();
        assert!(result.edits[0].1[0].text.contains("\r\n--changeset \"Jane Doe\":2\r\n"));
        let lazy = action(&mut plugin, &d, SET);
        answers(&host, &["1", "Jane Doe"], true);
        assert!(plugin.resolve_file_action(&d, lazy).is_err());
    }
    #[test]
    fn cancellations_denial_stale_context_and_forged_action_state_never_write() {
        for stage in 0..5 {
            let (host, mut plugin, doc) = setup();
            let lazy = action(&mut plugin, &doc, NEW);
            for value in ["xml", "db/new.xml", "ann", "linked"].iter().take(stage) {
                host.answer_next_prompt(Some(&[value]));
            }
            assert!(matches!(plugin.resolve_file_action(&doc, lazy), Err(Error::Cancelled)));
            assert_eq!(host.editor_text(), doc.text);
            assert!(host.process_runs().is_empty());
        }
        let (host, mut plugin, doc) = setup();
        let lazy = action(&mut plugin, &doc, SET);
        let mut changed = doc.clone();
        changed.version += 1;
        assert!(matches!(plugin.resolve_file_action(&changed, lazy), Err(Error::Stale)));
        let lazy = action(&mut plugin, &doc, SET);
        host.type_in_editor(" ");
        assert!(matches!(plugin.resolve_file_action(&doc, lazy), Err(Error::Stale)));
        let lazy = action(&mut plugin, &doc, SET);
        host.set_project_root(Some("/other"));
        assert!(matches!(plugin.resolve_file_action(&doc, lazy), Err(Error::Stale)));
        let host = host.with_capabilities(&[]);
        assert!(plugin.file_actions(&doc, Range::default()).is_empty());
        assert!(host.process_runs().is_empty());
    }
    #[test]
    fn existing_buffer_traversal_duplicate_and_opaque_includes_are_refused() {
        for target in ["../bad.xml", "/elsewhere/bad.xml", "db/new.sql", "db/${dynamic}.xml", "db/master.xml"] {
            let (host, mut plugin, doc) = setup();
            let lazy = action(&mut plugin, &doc, NEW);
            answers(&host, &["xml", target, "ann", "linked"], true);
            assert!(plugin.resolve_file_action(&doc, lazy).is_err(), "{target}");
        }
        let (host, mut plugin, doc) = setup();
        host.open_editor("/project/db/new.xml", "xml", "unsaved", &[]);
        let lazy = action(&mut plugin, &doc, NEW);
        answers(&host, &["xml", "db/new.xml", "ann", "linked"], true);
        assert!(plugin.resolve_file_action(&doc, lazy).is_err());
        assert_eq!(host.editor_text(), "unsaved");
        for element in [
            "<lb:include file=\"changes/new.xml\" relativeToChangelogFile=\"true\"/>",
            "<lb:includeAll path=\"changes\"/>",
            "<lb:include file=\"${folder}/old.xml\"/>",
        ] {
            let (host, mut plugin, mut doc) = setup();
            doc.text = doc.text.replace("</lb:databaseChangeLog>", &format!("{element}\n</lb:databaseChangeLog>"));
            host.open_editor(&doc.path, "xml", &doc.text, &[]);
            let lazy = action(&mut plugin, &doc, NEW);
            answers(&host, &["xml", "db/changes/new.xml", "ann", "linked"], true);
            assert!(plugin.resolve_file_action(&doc, lazy).is_err(), "{element}");
        }
    }
    #[test]
    fn including_existing_leaf_xml_is_atomic_and_does_not_claim_cycle_resolution() {
        let (host, mut plugin, doc) = setup();
        let child = format!("<databaseChangeLog xmlns=\"{}\"/>", generate::NS);
        host.add_file("db/child.xml", &child);
        let lazy = action(&mut plugin, &doc, INCLUDE);
        answers(&host, &["db/child.xml"], true);
        let result = plugin.resolve_file_action(&doc, lazy).unwrap();
        assert!(result.operations.is_empty());
        assert_eq!(result.edits.len(), 1);
        assert!(generate::xml_log(&edit_text(&doc, &result.edits[0].1[0])).unwrap().includes.iter().any(|i| i.path == "child.xml"));
        for child in [
            format!("<databaseChangeLog xmlns=\"{}\"><include file=\"master.xml\"/></databaseChangeLog>", generate::NS),
            "<databaseChangeLog xmlns=\"urn:other\"/>".into(),
        ] {
            host.add_file("db/child.xml", &child);
            let lazy = action(&mut plugin, &doc, INCLUDE);
            answers(&host, &["db/child.xml"], true);
            assert!(plugin.resolve_file_action(&doc, lazy).is_err());
        }
    }
    #[test]
    fn yaml_and_json_changesets_and_linked_children_are_one_reviewed_atomic_action() {
        for (ext, format) in [("yaml", Format::Yaml), ("json", Format::Json)] {
            let (host, mut plugin, mut doc) = setup();
            doc.path = format!("/project/db/master.{ext}");
            doc.language = ext.into();
            doc.text = generate::changelog_source(format, "ann").unwrap();
            host.add_file(&doc.path, &doc.text);
            host.open_editor(&doc.path, &doc.language, &doc.text, &[]);
            let lazy = action(&mut plugin, &doc, SET);
            answers(&host, &["2", "Jane \"Doe\" 😀"], true);
            let resolved = plugin.resolve_file_action(&doc, lazy).unwrap();
            assert!(resolved.operations.is_empty());
            let changed = edit_text(&doc, &resolved.edits[0].1[0]);
            assert!(generate::parent_log(&doc.path, &changed).unwrap().identities.contains(&("2".into(), "Jane \"Doe\" 😀".into())));
            let lazy = action(&mut plugin, &doc, NEW);
            answers(&host, &["yaml", "db/child.yaml", "ann", "linked"], true);
            let resolved = plugin.resolve_file_action(&doc, lazy).unwrap();
            let [FileOperation::Create { overwrite, .. }] = resolved.operations.as_slice() else { panic!("one create") };
            assert!(!overwrite);
            assert_eq!(resolved.edits.len(), 1);
            let changed = edit_text(&doc, &resolved.edits[0].1[0]);
            assert_eq!(generate::parent_log(&doc.path, &changed).unwrap().includes[0].path, "child.yaml");
            assert_eq!(host.editor_text(), doc.text);
            assert!(!host.invocation().project().unwrap().exists("db/child.yaml").unwrap());
            assert!(host.process_runs().is_empty());
        }
    }
    #[test]
    fn existing_yaml_json_leaf_includes_and_cancellation_keep_children_and_parents_unchanged() {
        for (ext, format) in [("yaml", Format::Yaml), ("json", Format::Json)] {
            let (host, mut plugin, doc) = setup();
            let child = generate::changelog_source(format, "ann").unwrap();
            let path = format!("db/child.{ext}");
            host.add_file(&path, &child);
            let lazy = action(&mut plugin, &doc, INCLUDE);
            answers(&host, &[&path], false);
            assert!(matches!(plugin.resolve_file_action(&doc, lazy), Err(Error::Cancelled)));
            assert_eq!(host.editor_text(), doc.text);
            let lazy = action(&mut plugin, &doc, INCLUDE);
            answers(&host, &[&path], true);
            let resolved = plugin.resolve_file_action(&doc, lazy).unwrap();
            assert!(resolved.operations.is_empty());
            assert!(edit_text(&doc, &resolved.edits[0].1[0]).contains(&format!("child.{ext}")));
            assert_eq!(host.invocation().project().unwrap().read_file(&path).unwrap(), child);
            assert!(host.process_runs().is_empty());
        }
    }
    #[test]
    fn reserved_namespace_bindings_cannot_authorize_current_parent_mutation() {
        for binding in [
            "xmlns:xml=\"urn:foreign\"",
            "xmlns:xmlns=\"urn:foreign\"",
            "xmlns:bad=\"http://www.w3.org/XML/1998/namespace\"",
            "xmlns=\"http://www.w3.org/2000/xmlns/\"",
            "xmlns:bad=\"\"",
        ] {
            let (host, mut plugin, mut doc) = setup();
            doc.text = format!("<databaseChangeLog xmlns=\"{}\" {binding}/>", generate::NS);
            host.open_editor(&doc.path, "xml", &doc.text, &[]);
            assert!(generate::xml_log(&doc.text).is_err(), "{binding}");
            let offered = plugin.file_actions(&doc, Range::default());
            assert!(!offered.iter().any(|a| a.title.starts_with("Add Liquibase changeset") || a.title.starts_with("Include existing")), "{binding}");
            assert_eq!(host.editor_text(), doc.text);
            assert!(host.process_runs().is_empty() && host.requests().is_empty());
        }
    }
    #[test]
    fn legal_predefined_xml_attributes_and_liquibase_prefixes_keep_reviewed_source_edits() {
        let (host, mut plugin, mut doc) = setup();
        doc.text = format!("<lb:databaseChangeLog xmlns:lb=\"{}\" xmlns:xml=\"http://www.w3.org/XML/1998/namespace\" xml:lang=\"en\"/>", generate::NS);
        host.open_editor(&doc.path, "xml", &doc.text, &[]);
        let lazy = action(&mut plugin, &doc, SET);
        answers(&host, &["1", "ann"], true);
        let result = plugin.resolve_file_action(&doc, lazy).unwrap();
        let text = edit_text(&doc, &result.edits[0].1[0]);
        assert!(text.contains("<lb:changeSet") && text.contains("xml:lang=\"en\""));
        assert_eq!(generate::xml_log(&text).unwrap().identities, [("1".into(), "ann".into())]);
        assert_eq!(host.editor_text(), doc.text);
        assert!(host.process_runs().is_empty() && host.requests().is_empty());
    }
    #[test]
    fn wrong_namespace_and_unsupported_parent_formats_do_not_get_mutating_intentions() {
        let (host, mut plugin, _) = setup();
        for (file, text) in [
            ("bad.xml", "<databaseChangeLog xmlns=\"urn:other\"/>"),
            ("parent.yaml", "databaseChangeLog:\n"),
            ("parent.json", "{\"databaseChangeLog\":{}}"),
        ] {
            host.add_file(file, text);
            let d = Document { path: format!("/project/{file}"), language: "xml".into(), version: 1, text: text.into() };
            let actions = plugin.file_actions(&d, Range::default());
            assert_eq!(actions.len(), 1, "standalone creation only: {file}");
        }
        let large = Document { path: "/project/big.xml".into(), language: "xml".into(), version: 1, text: "x".repeat(MAX_TEXT + 1) };
        assert!(plugin.file_actions(&large, Range::default()).is_empty());
        assert!(host.process_runs().is_empty());
    }
}

#[cfg(test)]
mod prompt_context_tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    #[test]
    fn lazy_resolution_never_fetches_git_or_reveals_views_and_failures_are_actionable() {
        let host = MockHost::install().with_capabilities(&["project.read", "process:git"]);
        let mut plugin = Liquibase::default();
        let text = format!("<databaseChangeLog xmlns=\"{}\"/>", generate::NS);
        host.add_file("db/log.xml", &text);
        host.open_editor("/project/db/log.xml", "xml", &text, &[]);
        host.on_process("git", |_, _| panic!("a lazy resolver must not launch a process"));
        let doc = Document { path: "/project/db/log.xml".into(), language: "xml".into(), version: 1, text };
        let lazy = plugin.file_actions(&doc, Range::default()).into_iter().find(|a| a.title.starts_with("Add")).unwrap();
        assert!(matches!(plugin.resolve_file_action(&doc, lazy), Err(Error::Cancelled)));
        assert!(host.notifications().is_empty());
        let lazy = plugin.file_actions(&doc, Range::default()).into_iter().find(|a| a.title.starts_with("Add")).unwrap();
        host.answer_next_prompt(Some(&["bad:id"]));
        host.answer_next_prompt(Some(&["ann"]));
        assert!(plugin.resolve_file_action(&doc, lazy).is_err());
        assert_eq!(host.notifications().len(), 1);
        assert!(host.notifications()[0].title.contains("could not be prepared"));
        assert!(host.process_runs().is_empty() && host.revealed_views().is_empty());
    }
}
