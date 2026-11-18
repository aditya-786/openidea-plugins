//! Selected-resource commands. No network, package-manager or disk writes.
use crate::*;
use openidea_plugin::TextEditor;

pub(crate) fn base(row: &Row) -> String {
    format!("{}|{} {}", row.path, row.name, row.version)
}

fn scoped_path(root: &str, path: &str) -> bool {
    !root.is_empty() && path.starts_with(&format!("{}/", root.trim_end_matches('/'))) && !path.split('/').any(|p| matches!(p, "." | ".."))
}

fn dependency<'a>(report: &'a Report, key: &str) -> Result<&'a Row, Error> {
    let key = key.strip_prefix("dep:").unwrap_or(key);
    let matches: Vec<_> = report.rows.iter().filter(|r| base(r) == key).collect();
    if matches.len() != 1 {
        return Err(Error::Stale);
    }
    Ok(matches[0])
}

fn advisory<'a>(report: &'a Report, key: &str) -> Result<&'a str, Error> {
    let key = key.strip_prefix("advisory:").unwrap_or(key);
    let matches: Vec<_> = report
        .rows
        .iter()
        .flat_map(|r| r.advisories.iter().map(move |id| (r, id)))
        .filter(|(r, id)| format!("{}|{id}", base(r)) == key)
        .collect();
    if matches.len() != 1 || !osv::is_safe_id(matches[0].1) {
        return Err(Error::Stale);
    }
    Ok(matches[0].1)
}

fn bound_declaration<'a>(root: &str, row: &'a Row) -> Option<&'a Declaration> {
    let bound: Vec<&Declaration> = row.sources.iter().filter(|d| d.evidence.contains(binding::DECLARED) && scoped_path(root, &d.path)).collect();
    match bound.as_slice() { [target] => Some(target), _ => None }
}

/// Palette fallback for row commands: choose one of the last scan's visible dependency rows.
pub(crate) fn pick_row(ctx: &Invocation) -> Result<Option<String>, Error> {
    let project = ctx.project().ok_or("Open a project first.")?;
    let root = project.root()?;
    let report = stored_report(&project)?.ok_or("Run Dependency Audit: Scan Project first, then choose a dependency in the Dependency Audit view.")?;
    let scopes = scoped::load(&root);
    let prefix = format!("{}/", root.trim_end_matches('/'));
    let items: Vec<_> = report.rows.iter().filter(|row| scoped::visible(&scopes, row))
        .map(|row| openidea_plugin::PickItem::new(&format!("dep:{}", base(row)), &format!("{} {}", row.name, row.version)).description(row.path.strip_prefix(&prefix).unwrap_or(&row.path)))
        .collect();
    if items.is_empty() {
        return Err("The last scan found no vulnerable dependencies.".into());
    }
    host::pick("Choose a vulnerable dependency", &items)
}

/// Final modal boundary: retained rows and their current source owners must
/// still be exactly the report that supplied the reviewed resource action.
pub(crate) fn current_report(project: &Project, before: &Option<String>) -> Result<Report, Error> {
    if host::storage::get(&rows_key(project)?)? != *before {return Err(Error::Stale);}
    stored_report(project)?.ok_or(Error::Stale)
}

/// Resolve a dependency menu action to one exact advisory, retaining all
/// existing advisory ownership, reason and confirmation checks downstream.
pub(crate) fn choose_advisory(ctx: &Invocation, key: &str, ignoring: bool) -> Result<Option<String>, Error> {
    let project = ctx.project().ok_or(Error::Stale)?;
    let root = project.root()?;
    let before = host::storage::get(&rows_key(&project)?)?;
    let report = stored_report(&project)?.ok_or(Error::Stale)?;
    let row = dependency(&report, key)?;
    let mut ids = row.advisories.iter().filter(|id|osv::is_safe_id(id)).collect::<Vec<_>>();
    ids.sort();ids.dedup();
    let id = match ids.as_slice() {
        [] => return Err(Error::Stale),
        [id] => (*id).to_owned(),
        _ => {
            let items = ids.iter().map(|id|openidea_plugin::PickItem::new(id, id)).collect::<Vec<_>>();
            let title = format!("Choose advisory to {} — {} {}", if ignoring {"ignore"} else {"open"}, row.name, row.version);
            let Some(id) = host::pick(&title, &items)? else { return Ok(None); };
            if !ids.iter().any(|known|known.as_str() == id) { return Err(Error::Stale); }
            id
        }
    };
    if ctx.project().and_then(|project|project.root().ok()).as_deref() != Some(root.as_str()) { return Err(Error::Stale); }
    let latest = current_report(&project, &before)?;
    let latest_row = dependency(&latest, key)?;
    if !latest_row.advisories.contains(&id) { return Err(Error::Stale); }
    Ok(Some(format!("advisory:{}|{id}",base(latest_row))))
}

struct Upgrade {
    root: String,
    doc: Document,
    edit: TextEdit,
    before: String,
    name: String,
    /// The whole document with the edit applied, for the diff view.
    upgraded: String,
}

fn safe_version(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.starts_with(|c: char| c.is_ascii_digit())
        && value.chars().all(|c| c.is_ascii_alphanumeric() || ".+-_".contains(c))
}

fn plan(root: &str, row: &Row, doc: Document) -> Result<Upgrade, Error> {
    if !scoped_path(root, &row.path) || !scoped_path(root, &doc.path) || doc.text.len() > 512 << 10 {
        return Err(Error::Invalid("The selected source is outside this project or exceeds the 512 KiB edit limit.".into()));
    }
    if doc.path != row.path && !row.sources.iter().any(|s| s.path == doc.path) {
        return Err(Error::Invalid("Open a declaration listed for this dependency, then choose Preview upgrade again.".into()));
    }
    let fix = row
        .fix
        .as_deref()
        .filter(|fix| safe_version(fix) && compare(fix, &row.version) == Ordering::Greater)
        .ok_or("No validated forward upgrade is recorded. Rescan or use your package manager.")?;
    // Deliberately no sibling reads: a ranged declaration requires fresh resolver evidence,
    // while this bounded workflow handles current exact declarations only.
    let deps = parse(&doc.path, &doc.text, None);
    let matches: Vec<_> = deps
        .iter()
        .filter(|d| d.name == row.name && d.version == row.version && row.ecosystem.as_deref().is_none_or(|e| e == d.ecosystem.osv()))
        .collect();
    let [dep] = matches.as_slice() else {
        return Err(Error::Invalid("The current document no longer has one exact matching declaration. Rescan; ranges, duplicate declarations and lockfile-only edits are not supported here.".into()));
    };
    let (from, to) = dep.version_at.ok_or("Lockfiles cannot be edited here. Use the package manager to update them.")?;
    if deps.iter().any(|other| other.version_at == Some((from, to)) && dep_key(other) != dep_key(dep)) {
        return Err(Error::Invalid(
            "This version is shared by other packages. Use the manifest editor's coordinated quick fix or update it manually.".into(),
        ));
    }
    let before = doc
        .text
        .get(from..to)
        .filter(|s| is_single_constraint(s))
        .ok_or("The current version is not a supported single constraint.")?
        .to_owned();
    let after = bumped(&before, fix);
    if !safe_version(after.trim_start_matches(['^', '~', 'v'])) {
        return Err(Error::Invalid("The replacement is not a supported literal version.".into()));
    }
    let upgraded = format!("{}{after}{}", &doc.text[..from], &doc.text[to..]);
    let edit = TextEdit { range: range(&doc.text, from, to), text: after };
    Ok(Upgrade { root: root.into(), doc, edit, before, name: row.name.clone(), upgraded })
}

fn preview(upgrade: &Upgrade) -> String {
    format!(
        "{}\n{}\nLine {}, columns {}–{} (editor version {})\n\nReplace exactly:\n{}\n\nWith:\n{}\n\nApply this one undoable editor edit? No lockfile, dependency install or build is run. The cached recommendation is not a compatibility guarantee.",
        upgrade.name,
        upgrade.doc.path,
        upgrade.edit.range.start.line + 1,
        upgrade.edit.range.start.character + 1,
        upgrade.edit.range.end.character + 1,
        upgrade.doc.version,
        upgrade.before,
        upgrade.edit.text
    )
}

fn apply(editor: &TextEditor, upgrade: &Upgrade) -> Result<(), Error> {
    let current = editor.document()?;
    if !owned_source_path(&upgrade.root,&upgrade.doc.path) || editor.path() != upgrade.doc.path || current.version != upgrade.doc.version || current.text != upgrade.doc.text {
        return Err(Error::Stale);
    }
    editor.apply_edits(upgrade.doc.version, std::slice::from_ref(&upgrade.edit))?;
    Ok(())
}

impl DependencyAudit {
    pub(crate) fn resource_command(&mut self, id: &str, ctx: &Invocation, key: Option<&str>) -> Result<Option<String>, Error> {
        let project = ctx.project().ok_or("Open a project first.")?;
        let root = project.root()?;
        if root.is_empty() {
            return Err(Error::Stale);
        }
        let report_before=host::storage::get(&rows_key(&project)?)?;
        let report = stored_report(&project)?.ok_or(Error::Stale)?;
        let key = key.ok_or("Choose a dependency in the Dependency Audit view first.")?;
        if id == "open-advisory" {
            let id = advisory(&report, key)?;
            host::open_url(&format!("https://osv.dev/vulnerability/{id}"))?;
            return Ok(None);
        }
        let row = dependency(&report, key)?;
        if id == "open-declaration" {
            let target = bound_declaration(&root, row)
                .ok_or("No single bound declaration for this dependency (transitive, ambiguous or alias); open the listed source manually.")?;
            host::open_location(&target.path, target.line, 0)?;
            return Ok(None);
        }
        let on_source = ctx.editor().filter(|editor| editor.path() == row.path || row.sources.iter().any(|d| d.path == editor.path()));
        let Some(editor) = on_source else {
            // Commands get no handle to an editor they open: open the declaration and ask for one more click.
            let target = bound_declaration(&root, row).ok_or("Open a declaration listed for this dependency, then choose Preview upgrade again.")?;
            host::open_location(&target.path, target.line, 0)?;
            return Ok(Some("Opened the declaration. Choose Preview upgrade again to review the change.".into()));
        };
        let current = editor.document()?;
        let doc = Document { path: editor.path(), language: editor.language(), version: current.version, text: current.text };
        if project_of(&doc.path).and_then(|p| p.root().ok()).as_deref() != Some(root.as_str()) {
            return Err(Error::Stale);
        }
        let upgrade = plan(&root, row, doc)?;
        // The diff is a view only; the confirmation below still decides, and a host without it keeps the text preview.
        let diff = openidea_plugin::ContentDiff {
            left: upgrade.doc.text.clone(),
            right: upgrade.upgraded.clone(),
            left_label: Some("Current".into()),
            right_label: Some(format!("{} upgraded to {}", upgrade.name, upgrade.edit.text)),
            title: Some(format!("Preview upgrade: {}", upgrade.name)),
        };
        let _ = host::open_diff(&diff, upgrade.doc.path.rsplit('/').next());
        if !ui_kit::prompts::confirm_exact("Preview dependency upgrade", &preview(&upgrade))? {
            return Ok(None);
        }
        if ctx.project().and_then(|p| p.root().ok()).as_deref() != Some(root.as_str()) {
            return Err(Error::Stale);
        }
        let latest=current_report(&project,&report_before)?;
        let latest_row=dependency(&latest,key)?;
        if serde_json::to_value(latest_row).map_err(|_|Error::Stale)? != serde_json::to_value(row).map_err(|_|Error::Stale)? {return Err(Error::Stale);}
        apply(&editor, &upgrade)?;
        self.live.clear();
        self.invalidate_report();
        Ok(Some("Upgrade applied to the editor. Review and test it, update lockfiles with the package manager, then rescan. The report still describes the previous scan.".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    const CAPS: &[&str] = &["project.read", "storage", "editor.read", "editor.write"];
    const TEXT: &str = "{\"dependencies\":{\"lodash\":\"4.17.15\"}}";
    fn row() -> Row {
        Row {
            path: "/project/package.json".into(),
            line: 0,
            name: "lodash".into(),
            version: "4.17.15".into(),
            fix: Some("4.17.21".into()),
            advisories: vec!["GHSA-35jh-r3h4-6jhm".into()],
            ecosystem: Some("npm".into()),
            sources: vec![],
            details: vec![],
        }
    }
    fn doc(path: &str, text: &str) -> Document {
        Document { path: path.into(), language: "json".into(), version: 1, text: text.into() }
    }
    fn saved(host: &MockHost, row: Row) -> String {
        let key = base(&row);
        host::storage::set("scan:/project", &serde_json::to_string(&Report { rows: vec![row], ..Report::default() }).unwrap()).unwrap();
        host.open_editor("/project/package.json", "json", TEXT, &[]);
        key
    }

    #[test]
    fn current_buffer_preview_is_concrete_and_confirmed_edit_is_versioned() {
        let host = MockHost::install().with_capabilities(CAPS);
        let key = saved(&host, row());
        host.add_file("package.json", "disk contents must not be used");
        let current = format!("// unsaved 😀\n{TEXT}");
        // JSON comments are outside our supported parser subset: refuse rather than reinterpret.
        assert!(plan("/project", &row(), doc("/project/package.json", &current)).is_err());
        let current = TEXT.replace("{\"dependencies", "{\"private\":true,\"dependencies");
        host.open_editor("/project/package.json", "json", &current, &[]);
        let upgrade = plan("/project", &row(), doc("/project/package.json", &current)).unwrap();
        let body = preview(&upgrade);
        assert!(
            body.contains("/project/package.json")
                && body.contains("4.17.15")
                && body.contains("4.17.21")
                && body.contains("editor version 1")
        );
        host.answer_next_prompt(Some(&[]));
        let result = DependencyAudit::default().run_command("preview-upgrade", &host.invocation(), Some(&format!("dep:{key}"))).unwrap();
        assert!(result.unwrap().contains("previous scan"));
        assert_eq!(host.editor_text(), current.replace("4.17.15", "4.17.21"));
        assert_eq!(host.editor_version(), 2);
        assert!(host.requests().is_empty());
        let diffs = host.opened_diffs();
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].0.left.as_deref(), Some(current.as_str()));
        assert_eq!(diffs[0].0.right, Some(current.replace("4.17.15", "4.17.21")));
        assert_eq!(diffs[0].1.as_deref(), Some("package.json"));
    }

    #[test]
    fn registered_upgrade_refuses_dependency_shaped_plugin_configuration_payload() {
        let host=MockHost::install().with_capabilities(CAPS);
        let mut r=row();r.path="/project/pom.xml".into();r.name="x:a".into();r.ecosystem=Some("Maven".into());
        let key=saved(&host,r);
        let text="<project><build><plugins><plugin><configuration><dependencies><dependency><groupId>x</groupId><artifactId>a</artifactId><version>4.17.15</version></dependency></dependencies></configuration></plugin></plugins></build></project>";
        host.open_editor("/project/pom.xml","xml",text,&[]);host.answer_next_prompt(Some(&[]));
        assert!(DependencyAudit::default().run_command("preview-upgrade",&host.invocation(),Some(&key)).is_err());
        assert_eq!(host.editor_text(),text);
    }

    #[test]
    fn registered_upgrade_refuses_foreign_default_namespace_pom_shapes() {
        for text in [
            "<project xmlns=\"urn:foreign\"><dependencies><dependency><groupId>x</groupId><artifactId>a</artifactId><version>4.17.15</version></dependency></dependencies></project>",
            "<project><dependencies xmlns=\"urn:foreign\"><dependency><groupId>x</groupId><artifactId>a</artifactId><version>4.17.15</version></dependency></dependencies></project>",
            "<project><dependencies><dependency><groupId>x</groupId><artifactId>a</artifactId><version xmlns=\"urn:foreign\">4.17.15</version></dependency></dependencies></project>",
        ] {
            let host=MockHost::install().with_capabilities(CAPS);
            let mut r=row();r.path="/project/pom.xml".into();r.name="x:a".into();r.ecosystem=Some("Maven".into());
            let key=saved(&host,r);host.open_editor("/project/pom.xml","xml",text,&[]);host.answer_next_prompt(Some(&[]));
            assert!(DependencyAudit::default().run_command("preview-upgrade",&host.invocation(),Some(&key)).is_err());
            assert_eq!(host.editor_text(),text);
        }
    }

    #[test]
    fn registered_upgrade_refuses_unclosed_or_mismatched_pom_containers() {
        for text in [
            "<project><dependencies><dependency><groupId>x</groupId><artifactId>a</artifactId><version>4.17.15</version></dependency>",
            "<project><dependencies><dependency><groupId>x</groupId><artifactId>a</artifactId><version>4.17.15</version></dependencies></project>",
        ] {
            let host=MockHost::install().with_capabilities(CAPS);
            let mut r=row();r.path="/project/pom.xml".into();r.name="x:a".into();r.ecosystem=Some("Maven".into());
            let key=saved(&host,r);host.open_editor("/project/pom.xml","xml",text,&[]);host.answer_next_prompt(Some(&[]));
            assert!(DependencyAudit::default().run_command("preview-upgrade",&host.invocation(),Some(&key)).is_err());
            assert_eq!(host.editor_text(),text);
        }
    }

    #[test]
    fn registered_upgrade_refuses_duplicate_pom_version_or_property_authority() {
        for text in [
            "<project><dependencies><dependency><groupId>x</groupId><artifactId>a</artifactId><version>4.17.15</version><version>9.0.0</version></dependency></dependencies></project>",
            "<project><properties><v>4.17.15</v><v>9.0.0</v></properties><dependencies><dependency><groupId>x</groupId><artifactId>a</artifactId><version>${v}</version></dependency></dependencies></project>",
            "<project><profiles><profile><properties><v>4.17.15</v></properties></profile></profiles><dependencies><dependency><groupId>x</groupId><artifactId>a</artifactId><version>${v}</version></dependency></dependencies></project>",
            "<project><properties><v>4.17.15</v></properties><profiles><profile><properties><v>9.0.0</v></properties></profile></profiles><dependencies><dependency><groupId>x</groupId><artifactId>a</artifactId><version>${v}</version></dependency></dependencies></project>",
        ] {
            let host=MockHost::install().with_capabilities(CAPS);
            let mut r=row();r.path="/project/pom.xml".into();r.name="x:a".into();r.ecosystem=Some("Maven".into());
            let key=saved(&host,r);host.open_editor("/project/pom.xml","xml",text,&[]);host.answer_next_prompt(Some(&[]));
            assert!(DependencyAudit::default().run_command("preview-upgrade",&host.invocation(),Some(&key)).is_err());
            assert_eq!(host.editor_text(),text);
        }
    }

    #[test]
    fn registered_upgrade_command_refuses_a_commented_gradle_coordinate() {
        let host=MockHost::install().with_capabilities(CAPS);
        let mut r=row();r.path="/project/build.gradle".into();r.name="x:a".into();r.ecosystem=Some("Maven".into());
        let key=saved(&host,r);
        let text="// implementation 'x:a:4.17.15'\n";
        host.open_editor("/project/build.gradle","groovy",text,&[]);
        host.answer_next_prompt(Some(&[]));
        let result=DependencyAudit::default().run_command("preview-upgrade",&host.invocation(),Some(&key));
        assert!(result.is_err(),"comment must not be an editable declaration: {result:?}");
        assert_eq!(host.editor_text(),text);
        assert!(host.requests().is_empty()&&host.process_runs().is_empty());
    }

    #[test]
    fn cancelling_and_missing_editor_permission_never_edit() {
        let host = MockHost::install().with_capabilities(CAPS);
        let key = saved(&host, row());
        let mut plugin = DependencyAudit::default();
        plugin.run_command("preview-upgrade", &host.invocation(), Some(&key)).unwrap(); // unscripted confirm cancels
        assert_eq!(host.editor_text(), TEXT);
        let host = host.with_capabilities(&["project.read", "storage", "editor.read"]);
        host.answer_next_prompt(Some(&[]));
        assert!(plugin.run_command("preview-upgrade", &host.invocation(), Some(&key)).is_err());
        assert_eq!(host.editor_text(), TEXT);
    }

    #[test]
    fn stale_project_keys_and_current_versions_are_rejected() {
        let host = MockHost::install().with_capabilities(CAPS);
        let key = saved(&host, row());
        let mut plugin = DependencyAudit::default();
        host.open_editor("/project/package.json", "json", &TEXT.replace("4.17.15", "4.17.20"), &[]);
        assert!(plugin.run_command("preview-upgrade", &host.invocation(), Some(&key)).is_err());
        assert_eq!(host.editor_version(), 1);
        host.set_project_root(Some("/another"));
        assert!(plugin.run_command("preview-upgrade", &host.invocation(), Some(&key)).is_err());
        assert!(plugin.run_command("open-advisory", &host.invocation(), Some(&format!("{key}|GHSA-35jh-r3h4-6jhm"))).is_err());
        assert!(host.opened_urls().is_empty());
    }

    #[test]
    fn modal_report_boundary_refuses_replaced_snapshot_or_reowned_source() {
        let host=MockHost::install().with_capabilities(CAPS);
        let mut r=row();r.path="/project/web/package.json".into();
        saved(&host,r);
        let project=host.invocation().project().unwrap();
        let before=host::storage::get(&rows_key(&project).unwrap()).unwrap();
        assert!(current_report(&project,&before).is_ok());
        host::storage::set(&rows_key(&project).unwrap(),"[]").unwrap();
        assert!(matches!(current_report(&project,&before),Err(Error::Stale)));
        host::storage::set(&rows_key(&project).unwrap(),before.as_ref().unwrap()).unwrap();
        host.add_project("/project/web");
        assert!(matches!(current_report(&project,&before),Err(Error::Stale)));
        assert_eq!(host.storage()["scan:/project"],before.unwrap());
        assert!(host.requests().is_empty()&&host.process_runs().is_empty());
    }

    #[test]
    fn post_preview_apply_refuses_new_innermost_project_ownership() {
        let host=MockHost::install().with_capabilities(CAPS);
        let mut r=row();r.path="/project/web/package.json".into();
        host.open_editor(&r.path,"json",TEXT,&[]);
        let upgrade=plan("/project",&r,doc(&r.path,TEXT)).unwrap();
        host.add_project("/project/web");
        assert!(matches!(apply(&host.invocation().editor().unwrap(),&upgrade),Err(Error::Stale)));
        assert_eq!(host.editor_text(),TEXT);
    }

    #[test]
    fn a_document_changed_after_preview_is_not_rebased_or_retried() {
        let host = MockHost::install().with_capabilities(CAPS);
        saved(&host, row());
        let upgrade = plan("/project", &row(), doc("/project/package.json", TEXT)).unwrap();
        host.type_in_editor(" ");
        let editor = host.invocation().editor().unwrap();
        assert!(matches!(apply(&editor, &upgrade), Err(Error::Stale)));
        assert_eq!(host.editor_text(), format!("{TEXT} "));
    }

    #[test]
    fn parser_subset_collisions_unknown_sources_and_downgrades_are_refused() {
        let _host = MockHost::install().with_capabilities(CAPS);
        for text in [
            TEXT.replace("4.17.15", "^4.17.15"),
            "{\"dependencies\":{\"lodash\":\"4.17.15\"},\"devDependencies\":{\"lodash\":\"4.17.15\"}}".into(),
        ] {
            assert!(plan("/project", &row(), doc("/project/package.json", &text)).is_err());
        }
        for path in ["/outside/package.json", "/project/../package.json", "/project/other.json", "/project/package-lock.json"] {
            assert!(plan("/project", &row(), doc(path, TEXT)).is_err(), "{path}");
        }
        for fix in ["4.17.14", "4.17.15", "4.17.21\"evil", ""] {
            let mut r = row();
            r.fix = Some(fix.into());
            assert!(plan("/project", &r, doc("/project/package.json", TEXT)).is_err(), "{fix}");
        }
        let mut r = row();
        r.ecosystem = Some("Maven".into());
        assert!(plan("/project", &r, doc("/project/package.json", TEXT)).is_err());
        let mut r = row();
        r.path = "/project/other.lock".into();
        r.sources.push(Declaration { path: "/project/package.json".into(), line: 0, evidence: "source".into() });
        assert!(plan("/project", &r, doc("/project/package.json", TEXT)).is_ok(), "an explicitly listed current declaration is allowed");
        let pom = "<project><properties><v>4.17.15</v></properties><dependencies><dependency><groupId>x</groupId><artifactId>a</artifactId><version>${v}</version></dependency><dependency><groupId>x</groupId><artifactId>b</artifactId><version>${v}</version></dependency></dependencies></project>";
        let mut r = row();
        r.path = "/project/pom.xml".into();
        r.name = "x:a".into();
        r.ecosystem = Some("Maven".into());
        assert!(plan("/project", &r, doc("/project/pom.xml", pom)).is_err(), "shared Maven property needs coordinated handling");
    }

    #[test]
    fn exact_source_parser_subset_keeps_its_version_spans() {
        let _host = MockHost::install().with_capabilities(CAPS);
        for (file, text, name, ecosystem) in [
            ("build.gradle", "dependencies { implementation 'x:a:4.17.15' }", "x:a", "Maven"),
            ("libs.versions.toml", "[libraries]\na = { module = \"x:a\", version = \"4.17.15\" }\n", "x:a", "Maven"),
            ("requirements.txt", "lodash==4.17.15\n", "lodash", "PyPI"),
            ("composer.json", "{\"require\":{\"vendor/lib\":\"4.17.15\"}}", "vendor/lib", "Packagist"),
        ] {
            let mut r = row();
            r.path = format!("/project/{file}");
            r.name = name.into();
            r.ecosystem = Some(ecosystem.into());
            let upgrade = plan("/project", &r, doc(&r.path, text)).unwrap();
            assert_eq!(upgrade.before, "4.17.15");
            assert_eq!(upgrade.edit.text, "4.17.21");
            assert_eq!(
                &text[openidea_plugin::text::offset(text, upgrade.edit.range.start).unwrap()
                    ..openidea_plugin::text::offset(text, upgrade.edit.range.end).unwrap()],
                "4.17.15"
            );
        }
    }

    #[test]
    fn advisory_urls_require_current_membership_and_safe_ids() {
        let host = MockHost::install().with_capabilities(CAPS);
        let key = saved(&host, row());
        let mut plugin = DependencyAudit::default();
        for key in [format!("{key}|GHSA-35jh-r3h4-6jhm"), format!("advisory:{key}|GHSA-35jh-r3h4-6jhm")] {
            plugin.run_command("open-advisory", &host.invocation(), Some(&key)).unwrap();
        }
        assert_eq!(host.opened_urls().len(), 2);
        assert!(plugin.run_command("open-advisory", &host.invocation(), Some(&format!("advisory:{key}|../bad"))).is_err());
        let mut bad = row();
        bad.advisories = vec!["../bad".into()];
        let key = saved(&host, bad);
        assert!(plugin.run_command("open-advisory", &host.invocation(), Some(&format!("{key}|../bad"))).is_err());
        let r = row();
        let base = base(&r);
        let report = Report { rows: vec![r, row()], ..Report::default() };
        assert!(dependency(&report, &base).is_err(), "colliding persisted row identities are not guessed");
        assert_eq!(host.opened_urls().len(), 2);
    }
    #[test]
    fn dependency_advisory_picker_cancels_and_rejects_unlisted_answers() {
        let host=MockHost::install().with_capabilities(CAPS);
        let mut r=row();r.advisories.push("CVE-2026-1234".into());
        let key=format!("dep:{}",saved(&host,r));
        assert!(choose_advisory(&host.invocation(),&key,false).unwrap().is_none());
        host.answer_next_prompt(Some(&["CVE-2026-9999"]));
        assert!(choose_advisory(&host.invocation(),&key,false).is_err());
        host.answer_next_prompt(Some(&["CVE-2026-1234"]));
        assert_eq!(choose_advisory(&host.invocation(),&key,true).unwrap().unwrap(),format!("advisory:{}|CVE-2026-1234",key.strip_prefix("dep:").unwrap()));
        assert!(host.opened_urls().is_empty()&&host.requests().is_empty()&&host.process_runs().is_empty());
    }


    #[test]
    fn open_declaration_requires_exactly_one_bound_in_project_declaration() {
        let host = MockHost::install().with_capabilities(CAPS);
        let mut bound = row();
        let key = saved(&host, row());
        assert!(DependencyAudit::default().run_command("open-declaration", &host.invocation(), Some(&key)).is_err());
        bound.sources = vec![Declaration { path: "/project/package.json".into(), line: 0, evidence: format!("{} package.json:1", binding::DECLARED) }];
        saved(&host, bound);
        DependencyAudit::default().run_command("open-declaration", &host.invocation(), Some(&key)).unwrap();
    }
}
