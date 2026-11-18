//! Observed CLI-generated XML, explicit changeset selection and FileAction handoff.
use super::*;
use crate::generated_changelog::{Changelog, MAX_BYTES};
#[derive(Clone)]
pub(super) struct Request { pub output: String, pub author: String, pub started: bool }
pub(super) struct Document {
    plan: Plan,
    log: Changelog,
    base: String,
    key: String,
    selected: BTreeSet<usize>,
    groups: BTreeMap<String, BTreeSet<usize>>,
    active: String,
    text: String,
    at: Instant,
}
pub(crate) struct PreparedGroups { pub groups: Vec<(String, String)>, pub provenance: String }
pub(crate) struct Prepared { pub text: String, pub provenance: String }
fn hash(parts: &[&str]) -> String {
    let mut hash = Sha256::new();
    for part in parts { hash.update((part.len() as u64).to_le_bytes()); hash.update(part.as_bytes()); }
    format!("{:x}", hash.finalize())
}
pub(super) fn prepare(root: &str, reference: &Input, target: &Input, kind: &str, author: &str) -> Result<(String, Vec<(String, String)>, Request), Error> {
    if !generate::identity(author) { return Err("Use a bounded literal generated-changelog author.".into()); }
    // This is a collision-checked private locator, not an authorization token.
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|_| Error::Stale)?.as_nanos();
    let (dir, staged) = stage_plan(root, reference, target, &format!("generated:{kind}:{nonce}"))?;
    let output = format!("{}-generated.xml", staged[0].0.strip_suffix("-reference.json").ok_or(Error::Stale)?);
    Ok((dir, staged, Request { output, author: author.into(), started: false }))
}
pub(super) fn vacant(plan: &Plan) -> Result<(), Error> {
    let Some(request) = &plan.generated else { return Ok(()); };
    let dir = host::fs::plugin_dir()?;
    let name = request.output.strip_prefix(&format!("{dir}/")).filter(|name| name.starts_with("lb-diff-") && !name.contains('/')).ok_or(Error::Stale)?;
    let entries = host::fs::read_dir(&dir)?;
    if entries.len() >= 4096 || entries.iter().any(|entry| entry.name == name) { return Err("Private generated output already exists or cannot be proved absent; it will not be overwritten.".into()); }
    Ok(())
}
pub(super) fn capture(state: &mut State, plan: &Plan) -> Result<(), Error> {
    let request = plan.generated.as_ref().ok_or(Error::Stale)?;
    let stat = host::fs::stat(&request.output)?;
    if stat.kind != openidea_plugin::FileKind::File || stat.size > MAX_BYTES as u64 { return Err(Error::Stale); }
    let bytes = host::fs::read_bytes(&request.output)?;
    if bytes.len() > MAX_BYTES { return Err(Error::Stale); }
    let text = String::from_utf8(bytes).map_err(|_| Error::Stale)?;
    let log = Changelog::parse(&text)?;
    if !log.author_matches(&request.author) { return Err(Error::Stale); }
    let base = hash(&[&plan.root, &plan.reference.path, &plan.reference.text, &plan.target.path, &plan.target.text, &request.output, &text]);
    let key = hash(&[&base, &text]);
    let selected: BTreeSet<_> = (0..log.len()).collect();
    let groups = BTreeMap::from([("Migration 1".to_owned(), selected.clone())]);
    state.generated = Some(Document { plan: plan.clone(), log, base, key, selected, groups, active: "Migration 1".into(), text, at: Instant::now() });
    state.generated_view = true;
    state.selected = None;
    state.review = None;
    state.visible = 0..0;
    Ok(())
}
fn update(document: &mut Document) -> Result<(), Error> {
    document.selected = document.groups.get(&document.active).cloned().ok_or(Error::Stale)?;
    document.text = if document.selected.is_empty() { String::new() } else { document.log.subset(&document.selected)? };
    document.key = hash(&[&document.base, &document.text, &serde_json::to_string(&document.groups).map_err(|_| Error::Stale)?, &document.active]);
    Ok(())
}
fn assigned(document: &Document) -> BTreeSet<usize> { document.groups.values().flat_map(|indexes| indexes.iter().copied()).collect() }
fn source_owned(project: &Project, document: &Document) -> Result<(), Error> {
    if project.root()? != document.plan.root || !host::projects()?.iter().any(|p| p.root().ok().as_deref() == Some(document.plan.root.as_str())) { return Err(Error::Stale); }
    for source in [&document.plan.reference, &document.plan.target] {
        let full = format!("{}/{}", document.plan.root.trim_end_matches('/'), source.path);
        if host::project_of(&full).and_then(|p| p.root().ok()).as_deref() != Some(document.plan.root.as_str()) || read(project, &source.path, MAX_FILE)? != source.text { return Err(Error::Stale); }
    }
    Ok(())
}
fn provenance(document: &Document) -> String {
    format!("Project: {}\nReference: {} [editor-text SHA256 {:x}]\nTarget: {} [editor-text SHA256 {:x}]\n{} selected /{} observed changesets · {}s old\nLiquibase4.29.2 offline diff-changelog contract; installed version/edition and full schema coverage unverified. Selection preserves source order; omitted changeset dependencies, SQL correctness, current database state and runtime parameters are NOT validated. Generated source may contain sensitive schema/default values and is not rewritten/redacted. No update/rollback/database execution occurs on preview or save.", document.plan.root, document.plan.reference.path, Sha256::digest(document.plan.reference.text.as_bytes()), document.plan.target.path, Sha256::digest(document.plan.target.text.as_bytes()), document.selected.len(), document.log.len(), document.at.elapsed().as_secs()) + &format!("\nActive migration group: {}\nGroups: {}\nIgnored changesets: {}\nOperation classes are lexical categories, not SQL safety or dependency analysis.", document.active, document.groups.iter().map(|(name, indexes)| format!("{name}: {}", indexes.len())).collect::<Vec<_>>().join("; "), document.log.len() - assigned(document).len())
}
pub(super) fn owner(state: &State) -> Option<&str> { state.generated.as_ref().map(|document| document.plan.root.as_str()) }
pub(super) fn body(state: &State, root: &str) -> Option<Body> {
    if !state.generated_view { return None; }
    let document = state.generated.as_ref().filter(|d| d.plan.root == root)?;
    if [&document.plan.reference, &document.plan.target].iter().any(|source| host::project_of(&format!("{}/{}", root.trim_end_matches('/'), source.path)).and_then(|p| p.root().ok()).as_deref() != Some(root)) {
        return Some(Body::Message("Generated source ownership is unavailable or changed; cached XML is withheld. Return to its original project/source ownership or regenerate.".into()));
    }
    Some(Body::Form(vec![
        Field::new("generated-provenance", "Observed source provenance", FieldValue::readonly_text(&provenance(document), true)),
        Field::new("generated-source", "Exact selected XML (not applied to a database)", FieldValue::readonly_text(&document.text, true)),
        Field::new("generated-save", "Save reviewed source", FieldValue::readonly_text("Open an XML/YAML/JSON/SQL editor in this project, then choose Save reviewed offline changesets… from its code actions. The final exact preview returns one non-overwriting, undoable FileAction Create. Choose a smaller subset if the 16KiB confirmation limit is exceeded. Source is instance-owned and disappears on reload/reclamation; no source file has been saved yet.", true)),
    ]))
}
impl Liquibase {
    pub(crate) fn generated_groups_key(&self, root: &str) -> Option<String> {
        if self.offline.running.is_some() { return None; }
        self.offline.generated.as_ref().filter(|document| document.plan.root == root && document.groups.len() > 1 && document.groups.values().any(|indexes| !indexes.is_empty())).map(|document| document.key.clone())
    }
    pub(crate) fn prepared_generated_groups(&self, project: &Project, root: &str, key: &str) -> Result<PreparedGroups, Error> {
        if self.offline.running.is_some() { return Err(Error::Stale); }
        let document = self.offline.generated.as_ref().filter(|document| document.plan.root == root && document.key == key).ok_or(Error::Stale)?;
        source_owned(project, document)?;
        let groups = document.groups.iter().filter(|(_, indexes)| !indexes.is_empty()).map(|(name, indexes)| Ok((name.clone(), document.log.subset(indexes)?))).collect::<Result<Vec<_>, Error>>()?;
        if groups.is_empty() { return Err(Error::Stale); }
        Ok(PreparedGroups { groups, provenance: provenance(document) })
    }
    pub(crate) fn generated_action_key(&self, root: &str) -> Option<String> {
        if self.offline.running.is_some() { return None; }
        self.offline.generated.as_ref().filter(|d| d.plan.root == root && !d.selected.is_empty()).map(|d| d.key.clone())
    }
    pub(crate) fn prepared_generated(&self, project: &Project, root: &str, key: &str) -> Result<Prepared, Error> {
        if self.offline.running.is_some() { return Err(Error::Stale); }
        let document = self.offline.generated.as_ref().filter(|d| d.plan.root == root && d.key == key && !d.selected.is_empty()).ok_or(Error::Stale)?;
        source_owned(project, document)?;
        Ok(Prepared { text: document.text.clone(), provenance: provenance(document) })
    }
    pub(crate) fn choose_generated_changesets(&mut self, ctx: &Invocation) -> Result<(), Error> {
        if self.offline.running.is_some() { return Err(Error::Stale); }
        let project = ctx.project().ok_or(Error::Stale)?;
        let document = self.offline.generated.as_ref().ok_or(Error::Stale)?;
        source_owned(&project, document)?;
        let base = document.base.clone();
        let choices: Vec<_> = document.log.labels().enumerate().map(|(index, label)| PickItem::new(&format!("generated:{base}:{index}"), label)).collect();
        let Some(chosen) = host::pick_many("Choose complete generated changesets (dependencies not evaluated)", &choices)? else { return Ok(()); };
        let document = self.offline.generated.as_ref().filter(|d| d.base == base).ok_or(Error::Stale)?;
        source_owned(&project, document)?;
        let available: BTreeMap<_, _> = choices.iter().enumerate().map(|(index, item)| (item.key.as_str(), index)).collect();
        let mut indexes = BTreeSet::new();
        for key in chosen {
            let index = available.get(key.as_str()).ok_or(Error::Stale)?;
            if !indexes.insert(*index) { return Err(Error::Stale); }
        }
        if indexes.is_empty() { return Err("Select at least one changeset; use Organize to ignore/remove groups.".into()); }
        let document = self.offline.generated.as_mut().ok_or(Error::Stale)?;
        for values in document.groups.values_mut() { values.retain(|index| !indexes.contains(index)); }
        document.groups.insert(document.active.clone(), indexes);
        update(document)?;
        self.offline.generated_view = true;
        self.offline.visible = 0..0;
        ui_kit::web::refresh_view(VIEW, include_str!("ui_config.json"));
        Ok(())
    }
    pub(crate) fn organize_generated_changesets(&mut self, ctx: &Invocation) -> Result<(), Error> {
        if self.offline.running.is_some() { return Err(Error::Stale); }
        let project = ctx.project().ok_or(Error::Stale)?;
        let document = self.offline.generated.as_ref().ok_or(Error::Stale)?;
        source_owned(&project, document)?;
        let expected = document.key.clone();
        let choices = [PickItem::new("new", "Add migration group"), PickItem::new("switch", "Switch active migration group"),
            PickItem::new("remove", "Remove active group and ignore its changesets"), PickItem::new("ignore", "Ignore chosen changesets"),
            PickItem::new("restore", "Restore ignored changesets to active group"), PickItem::new("category", "Move an operation class to active group")];
        let Some(action) = host::pick("Organize generated migrations", &choices)? else { return Ok(()); };
        let mut groups = document.groups.clone(); let mut active = document.active.clone();
        match action.as_str() {
            "new" => {
                if groups.len() >= 32 { return Err("At most 32 migration groups.".into()); }
                let Some(name) = host::input("Migration group label", Some(&format!("Migration {}", groups.len() + 1)))? else { return Ok(()); };
                if name.trim().is_empty() || name.len() > 80 || name.chars().any(char::is_control) || groups.contains_key(&name) { return Err("Use a unique nonempty bounded group label.".into()); }
                groups.insert(name.clone(), BTreeSet::new()); active = name;
            },
            "switch" => {
                let choices: Vec<_> = groups.keys().map(|name| PickItem::new(name, name)).collect();
                let Some(name) = host::pick("Active migration group", &choices)? else { return Ok(()); };
                if !groups.contains_key(&name) { return Err(Error::Stale); } active = name;
            },
            "remove" => {
                if groups.len() <= 1 { return Err("Keep at least one migration group; ignore individual changesets instead.".into()); }
                groups.remove(&active); active = groups.keys().next().ok_or(Error::Stale)?.clone();
            },
            "ignore" | "restore" => {
                let assigned = assigned(document);
                let choices: Vec<_> = document.log.labels().enumerate().filter(|(index, _)| if action == "restore" { !assigned.contains(index) } else { assigned.contains(index) })
                    .map(|(index, label)| PickItem::new(&index.to_string(), label)).collect();
                let Some(chosen) = host::pick_many(if action == "restore" { "Restore ignored changesets" } else { "Ignore changesets (excluded from saved groups)" }, &choices)? else { return Ok(()); };
                let available: BTreeSet<_> = choices.iter().map(|item| item.key.as_str()).collect();
                let mut indexes = BTreeSet::new();
                for key in chosen { if !available.contains(key.as_str()) || !indexes.insert(key.parse::<usize>().map_err(|_| Error::Stale)?) { return Err(Error::Stale); } }
                for values in groups.values_mut() { values.retain(|index| !indexes.contains(index)); }
                if action == "restore" { groups.get_mut(&active).ok_or(Error::Stale)?.extend(indexes); }
            },
            "category" => {
                let Some(category) = host::pick("Operation class (not SQL risk/dependency analysis)", &[PickItem::new("drop-delete", "Drop/delete"), PickItem::new("add-create", "Add/create"), PickItem::new("other-sql", "Other/SQL")])? else { return Ok(()); };
                if !matches!(category.as_str(), "drop-delete" | "add-create" | "other-sql") { return Err(Error::Stale); }
                let indexes: BTreeSet<_> = (0..document.log.len()).filter(|index| document.log.category(*index) == Some(category.as_str())).collect();
                for values in groups.values_mut() { values.retain(|index| !indexes.contains(index)); }
                groups.get_mut(&active).ok_or(Error::Stale)?.extend(indexes);
            },
            _ => return Err(Error::Stale),
        }
        let document = self.offline.generated.as_ref().filter(|document| document.key == expected).ok_or(Error::Stale)?;
        source_owned(&project, document)?;
        let document = self.offline.generated.as_mut().ok_or(Error::Stale)?;
        document.groups = groups; document.active = active; update(document)?;
        self.offline.generated_view = true; self.offline.visible = 0..0;
        ui_kit::web::refresh_view(VIEW, include_str!("ui_config.json")); Ok(())
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{mock::MockHost, file_actions::{FileActions, FileOperation}, providers::Document as EditorDocument};
    fn setup() -> (MockHost, Liquibase) {
        let host = MockHost::install().with_capabilities(&["project.read", "process:liquibase", "storage"]);
        for path in ["ref.json", "target.json"] { host.add_file(path, r#"{"snapshot":{"database":{"shortName":"h2","url":"PRIVATE-CONNECTION"},"objects":{}}}"#); }
        (host, Liquibase::default())
    }
    fn answers(host: &MockHost) {
        for answer in ["0", "1", "tables", "ann"] { host.answer_next_prompt(Some(&[answer])); }
        host.answer_next_prompt(Some(&[]));
    }
    fn sample() -> String { format!("<databaseChangeLog xmlns='{}'><!-- 😀 --><changeSet id='1' author='ann'><createTable tableName='CUSTOMER'/></changeSet>\n<changeSet id='2' author='ann'><sql>ALTER TABLE CUSTOMER ADD NAME VARCHAR(30);</sql></changeSet></databaseChangeLog>", generate::NS) }
    fn finish(host: &MockHost, plugin: &mut Liquibase, xml: &str) -> String {
        let run = plugin.offline.running.as_mut().unwrap();
        let output = run.plan.generated.as_ref().unwrap().output.clone();
        host::fs::write_bytes(&output, xml.as_bytes()).unwrap();
        run.child.wait(1).unwrap(); run.status = Some(0);
        plugin.poll_offline_diff(&host.invocation()).unwrap();
        output
    }
    fn editor(host: &MockHost) -> EditorDocument {
        let text = format!("<databaseChangeLog xmlns='{}'/>", generate::NS);
        host.add_file("db/master.xml", &text);
        host.open_editor("/project/db/master.xml", "xml", &text, &[]);
        EditorDocument { path: "/project/db/master.xml".into(), language: "xml".into(), version: 1, text }
    }
    fn action(plugin: &mut Liquibase, doc: &EditorDocument) -> openidea_plugin::file_actions::FileAction {
        plugin.file_actions(doc, Range::default()).into_iter().find(|action| action.title == "Save reviewed offline changesets…").unwrap()
    }
    #[test]
    fn grouped_candidates_move_ignore_restore_and_save_multiple_files_atomically() {
        let (host, mut plugin) = setup(); answers(&host);
        plugin.start_offline_generation(&host.invocation()).unwrap(); finish(&host, &mut plugin, &sample());
        let doc = editor(&host);
        let initial = action(&mut plugin, &doc);
        host.answer_next_prompt(Some(&["new"])); host.answer_next_prompt(Some(&["Data"]));
        plugin.organize_generated_changesets(&host.invocation()).unwrap();
        assert!(plugin.generated_action_key("/project").is_none(), "empty active group has no single-file save action");
        let base = plugin.offline.generated.as_ref().unwrap().base.clone();
        host.answer_next_prompt(Some(&[&format!("generated:{base}:1")]));
        plugin.choose_generated_changesets(&host.invocation()).unwrap();
        let grouped = plugin.prepared_generated_groups(&host.invocation().project().unwrap(), "/project", &plugin.generated_groups_key("/project").unwrap()).unwrap();
        assert_eq!(grouped.groups.len(), 2);
        assert_eq!(grouped.groups[0].0, "Data");
        assert!(grouped.groups[0].1.contains("ALTER TABLE") && !grouped.groups[0].1.contains("createTable"));
        assert!(grouped.groups[1].1.contains("createTable") && !grouped.groups[1].1.contains("ALTER TABLE"));
        assert!(matches!(plugin.resolve_file_action(&doc, initial), Err(Error::Stale)));
        let action = plugin.file_actions(&doc, Range::default()).into_iter().find(|action| action.title == "Save grouped offline migrations…").unwrap();
        for answer in ["db/data.xml", "db/schema.xml"] { host.answer_next_prompt(Some(&[answer])); } host.answer_next_prompt(Some(&[]));
        let resolved = plugin.resolve_file_action(&doc, action).unwrap();
        assert_eq!(resolved.operations.len(), 2);
        assert!(resolved.operations.iter().all(|operation| matches!(operation, FileOperation::Create { overwrite: false, .. })));
        assert!(!host.invocation().project().unwrap().exists("db/data.xml").unwrap());
        let before = plugin.generated_groups_key("/project").unwrap();
        host.answer_next_prompt(Some(&["ignore"])); host.answer_next_prompt(Some(&["0"]));
        plugin.organize_generated_changesets(&host.invocation()).unwrap();
        let document = plugin.offline.generated.as_ref().unwrap();
        assert_eq!(assigned(document), BTreeSet::from([1]));
        assert_ne!(document.key, before);
        host.answer_next_prompt(Some(&["restore"])); host.answer_next_prompt(Some(&["0"]));
        plugin.organize_generated_changesets(&host.invocation()).unwrap();
        assert_eq!(plugin.offline.generated.as_ref().unwrap().selected, BTreeSet::from([0, 1]));
    }
    #[test]
    fn organizer_cancel_forged_choices_and_category_selection_preserve_ownership() {
        let (host, mut plugin) = setup(); answers(&host);
        plugin.start_offline_generation(&host.invocation()).unwrap(); finish(&host, &mut plugin, &sample());
        let before = plugin.generated_action_key("/project").unwrap();
        host.answer_next_prompt(None); plugin.organize_generated_changesets(&host.invocation()).unwrap();
        assert_eq!(plugin.generated_action_key("/project").unwrap(), before);
        host.answer_next_prompt(Some(&["switch"])); host.answer_next_prompt(Some(&["forged"]));
        assert!(plugin.organize_generated_changesets(&host.invocation()).is_err());
        assert_eq!(plugin.generated_action_key("/project").unwrap(), before);
        host.answer_next_prompt(Some(&["new"])); host.answer_next_prompt(Some(&["Schema"])); plugin.organize_generated_changesets(&host.invocation()).unwrap();
        host.answer_next_prompt(Some(&["category"])); host.answer_next_prompt(Some(&["add-create"])); plugin.organize_generated_changesets(&host.invocation()).unwrap();
        let document = plugin.offline.generated.as_ref().unwrap();
        assert_eq!(document.selected, BTreeSet::from([0]));
        assert!(document.text.contains("createTable") && !document.text.contains("ALTER TABLE"));
        host.add_file("ref.json", "changed");
        assert!(plugin.prepared_generated_groups(&host.invocation().project().unwrap(), "/project", &document.key).is_err());
    }
    #[test]
    fn explicit_generation_captures_xml_and_returns_one_exact_nonoverwriting_fileaction_without_direct_source_write() {
        let (host, mut plugin) = setup(); answers(&host);
        plugin.start_offline_generation(&host.invocation()).unwrap();
        let run = plugin.offline.running.as_ref().unwrap();
        let request = run.plan.generated.as_ref().unwrap();
        assert!(run.plan.args.iter().any(|arg| arg == "diff-changelog"));
        assert!(run.plan.args.iter().any(|arg| arg == &format!("--changelog-file={}", request.output)));
        assert!(run.plan.args.iter().any(|arg| arg == "--author=ann"));
        assert!(!format!("{:?}", run.plan.args).contains("PRIVATE-CONNECTION"));
        let xml = sample(); let output = finish(&host, &mut plugin, &xml);
        assert!(host::fs::read_bytes(&output).unwrap().is_empty());
        assert!(plugin.offline.running.is_none());
        assert!(format!("{:?}", plugin.render_offline_diff(None, &host.invocation()).unwrap().body).contains("createTable"));
        let host = host.with_capabilities(&["project.read", "storage"]); // Save must work without any process grant.
        let doc = editor(&host); let action = action(&mut plugin, &doc);
        host.answer_next_prompt(Some(&["db/reviewed.xml"])); host.answer_next_prompt(Some(&[]));
        let resolved = plugin.resolve_file_action(&doc, action).unwrap();
        let [FileOperation::Create { path, text, overwrite }] = resolved.operations.as_slice() else { panic!("one Create") };
        assert_eq!(path, "/project/db/reviewed.xml"); assert_eq!(text, &xml); assert!(!overwrite);
        assert!(resolved.edits.is_empty()); assert!(!host.invocation().project().unwrap().exists(path).unwrap());
        assert_eq!(host.editor_text(), doc.text);
        assert!(host.process_runs().is_empty(), "Mock Child::start is separately owned; save uses no unmanaged process");
    }
    #[test]
    fn exact_subset_expires_old_actions_and_forged_or_cancelled_choices_preserve_the_candidate() {
        let (host, mut plugin) = setup(); answers(&host); plugin.start_offline_generation(&host.invocation()).unwrap(); finish(&host, &mut plugin, &sample());
        let host = host.with_capabilities(&["project.read", "storage"]);
        let doc = editor(&host); let old = action(&mut plugin, &doc);
        let base = plugin.offline.generated.as_ref().unwrap().base.clone();
        host.answer_next_prompt(Some(&[&format!("generated:{base}:1")])); plugin.choose_generated_changesets(&host.invocation()).unwrap();
        assert!(matches!(plugin.resolve_file_action(&doc, old), Err(Error::Stale)));
        let saved_key = plugin.generated_action_key("/project").unwrap();
        host.answer_next_prompt(None); plugin.choose_generated_changesets(&host.invocation()).unwrap();
        assert_eq!(plugin.generated_action_key("/project").unwrap(), saved_key);
        host.answer_next_prompt(Some(&["forged"])); assert!(plugin.choose_generated_changesets(&host.invocation()).is_err());
        let new = action(&mut plugin, &doc);
        host.answer_next_prompt(Some(&["db/subset.xml"])); host.answer_next_prompt(Some(&[]));
        let saved = plugin.resolve_file_action(&doc, new).unwrap();
        let FileOperation::Create { text, .. } = &saved.operations[0] else { panic!() };
        assert!(!text.contains("createTable")); assert!(text.contains("ALTER TABLE CUSTOMER"));
        assert_eq!(generate::xml_log(text).unwrap().identities, [("2".into(), "ann".into())]);
        assert!(host.process_runs().is_empty(), "Mock Child::start is separately owned; save uses no unmanaged process");
    }
    #[test]
    fn malformed_declaration_generated_callback_retains_last_good_candidate() {
        let (host, mut plugin) = setup(); answers(&host); plugin.start_offline_generation(&host.invocation()).unwrap(); finish(&host, &mut plugin, &sample());
        let key=plugin.generated_action_key("/project").unwrap();
        for xml in [sample().replace("<changeSet id='1'", "<?xml version='1.0'?><changeSet id='1'"),
            format!("<?xml version='1.0'?><?xml version='1.0'?>{}",sample()),
            format!("<?xml version='1.0' version='1.0'?>{}",sample())] {
            answers(&host);plugin.start_offline_generation(&host.invocation()).unwrap();finish(&host,&mut plugin,&xml);
            assert_eq!(plugin.generated_action_key("/project").unwrap(),key);
            assert!(plugin.offline.status.contains("previous candidate retained"));
            let doc=editor(&host);assert!(plugin.file_actions(&doc,Range::default()).iter().any(|a|a.title=="Save reviewed offline changesets…"));
        }
        let xml=format!("<?xml version='1.0' encoding='UTF-8' standalone='yes'?>{}",sample());
        answers(&host);plugin.start_offline_generation(&host.invocation()).unwrap();finish(&host,&mut plugin,&xml);
        assert_ne!(plugin.generated_action_key("/project").unwrap(),key);
    }
    #[test]
    fn generated_callback_rejects_invalid_xml_names_in_nested_changes() {
        let (host,mut plugin)=setup();answers(&host);plugin.start_offline_generation(&host.invocation()).unwrap();finish(&host,&mut plugin,&sample());
        let key=plugin.generated_action_key("/project").unwrap();
        for xml in [sample().replace("<createTable tableName='CUSTOMER'/>","<createTable tableName='CUSTOMER'><1column/></createTable>"),
            sample().replace("tableName='CUSTOMER'","1tableName='CUSTOMER'"),
            sample().replace("tableName='CUSTOMER'",&format!("xmlns:a='{}' xmlns:b='{}' a:tableName='CUSTOMER' b:tableName='OTHER'",generate::NS,generate::NS))] {
            answers(&host);plugin.start_offline_generation(&host.invocation()).unwrap();finish(&host,&mut plugin,&xml);
            assert_eq!(plugin.generated_action_key("/project").unwrap(),key);
            assert!(plugin.offline.status.contains("previous candidate retained"));
        }
    }
    #[test]
    fn cancellation_before_every_generation_prompt_never_starts_or_stages() {
        for stage in 0..5 {
            let (host, mut plugin) = setup();
            for value in ["0", "1", "tables", "ann"].iter().take(stage) { host.answer_next_prompt(Some(&[value])); }
            host.answer_next_prompt(None);
            plugin.start_offline_generation(&host.invocation()).unwrap();
            assert!(plugin.offline.running.is_none() && host.process_runs().is_empty());
            assert!(host::fs::read_dir(&host::fs::plugin_dir().unwrap()).unwrap().is_empty());
        }
    }
    #[test]
    fn invalid_or_mismatched_output_keeps_last_good_and_never_proves_no_changes() {
        let (host, mut plugin) = setup(); answers(&host); plugin.start_offline_generation(&host.invocation()).unwrap(); finish(&host, &mut plugin, &sample());
        let key = plugin.generated_action_key("/project").unwrap();
        for invalid in ["not XML".into(), sample().replace("author='ann'", "author='someone-else'"), "x".repeat(MAX_BYTES + 1)] {
            answers(&host); plugin.start_offline_generation(&host.invocation()).unwrap();
            let output = finish(&host, &mut plugin, &invalid);
            assert_eq!(plugin.generated_action_key("/project").unwrap(), key);
            assert!(plugin.offline.status.contains("previous candidate retained"));
            assert!(host::fs::read_bytes(&output).unwrap().is_empty());
        }
    }
    #[test]
    fn foreign_callbacks_do_not_stop_or_clean_owned_generation_and_foreign_views_do_not_show_xml() {
        let (host, mut plugin) = setup(); answers(&host); plugin.start_offline_generation(&host.invocation()).unwrap();
        let output = plugin.offline.running.as_ref().unwrap().plan.generated.as_ref().unwrap().output.clone();
        host::fs::write_bytes(&output, b"owned-in-progress").unwrap();
        host.set_project_root(Some("/other"));
        assert!(plugin.poll_offline_diff(&host.invocation()).is_err());
        assert!(plugin.handle_offline_diff(ViewEvent::Button("offline-stop".into()), &host.invocation()).is_err());
        assert_eq!(host::fs::read_bytes(&output).unwrap(), b"owned-in-progress");
        assert!(plugin.offline.running.is_some());
        host.set_project_root(Some("/project")); plugin.handle_offline_diff(ViewEvent::Button("offline-stop".into()), &host.invocation()).unwrap();
        assert!(host::fs::read_bytes(&output).unwrap().is_empty());
        answers(&host); plugin.start_offline_generation(&host.invocation()).unwrap(); finish(&host, &mut plugin, &sample());
        host.set_project_root(Some("/other"));
        assert!(!format!("{:?}", plugin.render_offline_diff(None, &host.invocation()).unwrap()).contains("CUSTOMER"));
        assert!(plugin.generated_action_key("/other").is_none());
        assert!(plugin.choose_generated_changesets(&host.invocation()).is_err());
        let (host, mut plugin) = setup();
        let text = host.invocation().project().unwrap().read_file("ref.json").unwrap();
        host.add_file("nested/ref.json", &text);
        answers(&host); plugin.start_offline_generation(&host.invocation()).unwrap();
        assert_eq!(plugin.offline.running.as_ref().unwrap().plan.reference.path, "nested/ref.json");
        finish(&host, &mut plugin, &sample());
        host.add_project("/project/nested");
        assert!(!format!("{:?}", plugin.render_offline_diff(None, &host.invocation()).unwrap()).contains("CUSTOMER"));
        assert!(plugin.choose_generated_changesets(&host.invocation()).is_err());
    }
    #[test]
    fn existing_private_generated_output_is_never_overwritten_or_partially_materialized() {
        let (host, plugin) = setup();
        let project = host.invocation().project().unwrap();
        let reference = input(&project, "/project", "/project/ref.json").unwrap();
        let target = input(&project, "/project", "/project/target.json").unwrap();
        let (_, staged, request) = prepare("/project", &reference, &target, "tables", "ann").unwrap();
        let output = request.output.clone();
        let plan = Plan { root: "/project".into(), reference, target, staged, kind: "tables".into(), args: vec![], generated: Some(request) };
        host::fs::write_bytes(&output, b"KEEP-EXISTING").unwrap();
        assert!(materialize(&plan).is_err());
        assert_eq!(host::fs::read_bytes(&output).unwrap(), b"KEEP-EXISTING");
        assert!(plan.staged.iter().all(|(path, _)| host::fs::read_bytes(path).is_err()));
        // If an unexpected output appears after input staging but before start,
        // pre-start failure cleanup owns only its inputs, never that output.
        for (path, text) in &plan.staged { host::fs::write_bytes(path, text.as_bytes()).unwrap(); }
        cleanup(&plan);
        assert_eq!(host::fs::read_bytes(&output).unwrap(), b"KEEP-EXISTING");
        assert!(plan.staged.iter().all(|(path, _)| host::fs::read_bytes(path).unwrap().is_empty()));
        assert!(plugin.offline.running.is_none());
    }
    #[test]
    fn changed_snapshots_collisions_and_cancelled_save_refuse_without_source_writes() {
        let (host, mut plugin) = setup(); answers(&host); plugin.start_offline_generation(&host.invocation()).unwrap(); finish(&host, &mut plugin, &sample());
        let host = host.with_capabilities(&["project.read", "storage"]);
        let doc = editor(&host); let lazy = action(&mut plugin, &doc);
        host.answer_next_prompt(None); assert!(matches!(plugin.resolve_file_action(&doc, lazy), Err(Error::Cancelled)));
        let lazy = action(&mut plugin, &doc); host.add_file("db/existing.xml", "KEEP"); host.answer_next_prompt(Some(&["db/existing.xml"]));
        assert!(plugin.resolve_file_action(&doc, lazy).is_err());
        assert_eq!(host.invocation().project().unwrap().read_file("db/existing.xml").unwrap(), "KEEP");
        let lazy = action(&mut plugin, &doc); host.add_file("ref.json", "{}");
        assert!(matches!(plugin.resolve_file_action(&doc, lazy), Err(Error::Stale)));
        assert_eq!(host.editor_text(), doc.text);
        assert!(host.process_runs().is_empty(), "Mock Child::start is separately owned; save uses no unmanaged process");
    }
}
