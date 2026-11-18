//! Prepared naming preview and atomic, non-overwriting editor FileActions.
use super::*;
mod source;
use openidea_plugin::file_actions::{FileAction, FileActions, FileOperation};
use openidea_plugin::views::{Field, FieldValue};
use openidea_plugin::{FileKind, Range};
use spring_text::index::glob_matches;

const MAX_TEXT: usize = 512 << 10;
const CREATE: &str = "create";
const PREPARED: &str = "prepared";
const RENAME: &str = "rename";
const CATEGORIES: &[&str] = &["Versioned", "Undo", "Baseline", "Repeatable"];

#[derive(Clone)]
pub(crate) struct Draft {
    root: String,
    naming: naming::Naming,
    locations: Vec<String>,
    directory: String,
    version: String,
    description: String,
    category: String,
    suffix: String,
    ready: bool,
}
fn root(project: &Project) -> Result<String, Error> {
    let root = project.root()?;
    if !root.starts_with('/') || root.len() > 4096 || root.split('/').any(|p| matches!(p, "." | "..")) {
        return Err(Error::Stale);
    }
    Ok(root)
}
fn relative(root: &str, path: &str) -> Result<String, Error> {
    let path =
        if path.starts_with('/') { path.strip_prefix(&format!("{}/", root.trim_end_matches('/'))).ok_or(Error::Stale)? } else { path };
    if path.is_empty()
        || path.len() > 1024
        || path.chars().any(|c| c.is_control() || matches!(c, '\\' | '*' | '?' | '[' | ']' | '{' | '}'))
        || path.split('/').any(|p| p.is_empty() || matches!(p, "." | ".."))
    {
        return Err("Use a concrete path inside the project without traversal or wildcard characters.".into());
    }
    Ok(path.into())
}
fn file_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 240
        && !value.chars().any(|c| c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '[' | ']' | '{' | '}'))
        && !matches!(value, "." | "..")
}
fn configuration(project: &Project) -> Result<Config, Error> {
    // The browsing reader is tolerant; mutation planning must not silently
    // replace an unreadable configuration or truncated discovery with defaults.
    let mut bytes = 0usize;
    let mut bounded = |text: String| -> Result<(), Error> {
        bytes = bytes.saturating_add(text.len());
        if text.len() > MAX_TEXT || bytes > 4 << 20 {
            return Err("Flyway configuration exceeds the bounded planning limit.".into());
        }
        Ok(())
    };
    for file in config::CONFIG_FILES {
        match project.read_file(file) {
            Ok(text) => {
                bounded(text)?;
                break;
            }
            Err(Error::NotFound(_)) => {}
            Err(error) => return Err(error),
        }
    }
    let spring = project.find_files("**/src/main/resources/application*", 100)?;
    if spring.len() >= 100 {
        return Err("Flyway application-configuration discovery reached its limit.".into());
    }
    for path in spring.iter().filter(|p| is_config_file(p)) {
        bounded(project.read_file(path)?)?;
    }
    Ok(load(project))
}

fn inventory(project: &Project, config: &Config) -> Result<Vec<String>, Error> {
    let root = root(project)?;
    let mut files = BTreeSet::new();
    for glob in config.locations.iter().filter_map(|l| location_glob(l, &root)) {
        let found = project.find_files(&glob, 10_000)?;
        if found.len() >= 10_000 {
            return Err("Migration discovery reached its limit; collision checking is incomplete.".into());
        }
        for path in found {
            relative(&root, &path)?;
            if config.naming.suffixes.iter().any(|s| path.ends_with(s)) {
                files.insert(path);
            }
        }
    }
    Ok(files.into_iter().collect())
}
fn supported_location(root: &str, path: &str, locations: &[String]) -> bool {
    relative(root, path).is_ok_and(|path| locations.iter().filter_map(|l| location_glob(l, root)).any(|g| glob_matches(&g, &path)))
}
fn vacant(project: &Project, path: &str) -> Result<(), Error> {
    if project.exists(path)? {
        return Err("The destination already exists. Nothing will be overwritten.".into());
    }
    match project.read_file(path) {
        Ok(_) => return Err("The destination has an open buffer. Choose another name.".into()),
        Err(Error::NotFound(_)) => {}
        Err(e) => return Err(e),
    }
    let path = relative(&root(project)?, path)?;
    let parts: Vec<_> = path.split('/').collect();
    for n in 1..parts.len() {
        let parent = parts[..n].join("/");
        if project.exists(&parent)? && project.stat(&parent)?.kind != FileKind::Directory {
            return Err("A destination parent is not a directory.".into());
        }
    }
    Ok(())
}
fn candidate(d: &Draft) -> Result<String, Error> {
    if d.description.len() > 200 {
        return Err("Description exceeds 200 bytes.".into());
    }
    let description = migration_description(&d.description);
    if description.is_empty() {
        return Err("Enter a description containing ASCII letters or digits.".into());
    }
    let (prefix, version, expected_kind) = match d.category.as_str() {
        "Versioned" => (&d.naming.versioned, d.version.as_str(), Kind::Versioned),
        "Undo" => (&d.naming.undo, d.version.as_str(), Kind::Undo),
        "Baseline" => (&d.naming.baseline, d.version.as_str(), Kind::Baseline),
        "Repeatable" => (&d.naming.repeatable, "", Kind::Repeatable),
        _ => return Err("Choose a supported migration category.".into()),
    };
    if d.version.len() > 128 || !d.naming.suffixes.contains(&d.suffix) {
        return Err("Use a bounded configured version/suffix.".into());
    }
    let name = format!("{prefix}{version}{}{description}{}", d.naming.separator, d.suffix);
    if !file_name(&name)
        || !matches!(parse(&name,&d.naming),Name::Migration{kind,description,..} if kind == expected_kind && !description.is_empty())
    {
        return Err("The preview does not match the configured migration naming scheme.".into());
    }
    let directory = relative(&d.root, &d.directory)?;
    let path = format!("{}/{directory}/{name}", d.root.trim_end_matches('/'));
    if !supported_location(&d.root, &path, &d.locations) {
        return Err("Choose a concrete directory within a configured migration location.".into());
    }
    Ok(path)
}
fn collision(project: &Project, config: &Config, target: &str, source: Option<&str>) -> Result<(), Error> {
    let root = root(project)?;
    vacant(project, target)?;
    let mut files = inventory(project, config)?;
    files.retain(|p| Some(p.as_str()) != source);
    files.push(target.into());
    let problems = checks::check(&root, &files, &config.naming, &vendor_prefixes(&config.locations, &root));
    if problems
        .get(target)
        .is_some_and(|problems| problems.iter().any(|p| matches!(p.code.as_deref(), Some("duplicate-version" | "duplicate-repeatable"))))
    {
        return Err("Another migration in this module/vendor has the same version or repeatable description.".into());
    }
    Ok(())
}
/// The highest-versioned migration in the draft's directory.
fn newest_in(project: &Project, config: &Config, d: &Draft) -> Option<String> {
    let root = d.root.trim_end_matches('/');
    let directory = match d.directory.trim_matches('/') {
        "" => root.to_owned(),
        directory => format!("{root}/{directory}"),
    };
    migration_files(project, config)
        .into_iter()
        .filter_map(|path| {
            let (parent, name) = path.rsplit_once('/')?;
            let Name::Migration { kind: Kind::Versioned | Kind::Baseline, version, .. } = parse(name, &config.naming) else { return None };
            let key: Vec<_> = version_key(&version).into_iter().map(|part| (part.len(), part)).collect();
            (parent == directory).then_some((key, path))
        })
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, path)| path)
}
fn unchanged(project: &Project, d: &Draft) -> Result<Config, Error> {
    if root(project)? != d.root {
        return Err(Error::Stale);
    }
    let config = configuration(project)?;
    if config.naming != d.naming || config.locations != d.locations {
        return Err("Flyway naming or locations changed. Reopen the generation preview.".into());
    }
    Ok(config)
}
fn context(doc: &Document, expected: &str) -> Result<Project, Error> {
    let project = project_of(&doc.path).ok_or(Error::Stale)?;
    if root(&project)? != expected {
        return Err(Error::Stale);
    }
    relative(expected, &doc.path)?;
    Ok(project)
}
fn fingerprint(text: &str) -> String {
    format!("{:016x}", text.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100000001b3)))
}
fn source_current(project: &Project, doc: &Document) -> Result<(), Error> {
    if project.read_file(&doc.path)? != doc.text {
        return Err(Error::Stale);
    }
    Ok(())
}
fn sql() -> String {
    include_str!("../templates/migration.sql").replace("$END$", "")
}
fn confirmation(path: &str, text: &str) -> String {
    format!(
        "Create: {path}\n\nBundled SQL template contents:\n{}\n\nCreate this file only as one undoable workspace action? It will not overwrite a destination, execute SQL, generate schema DDL or update Flyway's history table.",
        ui_kit::prompts::readable_source(text)
    )
}

impl Flyway {
    pub(crate) fn begin_generation(&mut self, project: &Project) -> Result<(), Error> {
        let root = root(project)?;
        let config = configuration(project)?;
        let (prefix, directory, suffix) = self.next_migration(project)?;
        let version = prefix
            .strip_prefix(&config.naming.versioned)
            .and_then(|p| p.strip_suffix(&config.naming.separator))
            .ok_or("Cannot derive the next version.")?
            .to_owned();
        self.generation = Some(Draft {
            root,
            naming: config.naming,
            locations: config.locations,
            directory,
            version,
            description: String::new(),
            category: "Versioned".into(),
            suffix,
            ready: false,
        });
        Ok(())
    }
    pub(crate) fn begin_generation_from(&mut self, project: &Project, source: Option<&str>) -> Result<(), Error> {
        self.begin_generation(project)?;
        if let Some(source) = source {
            let d = self.generation.as_mut().ok_or(Error::Stale)?;
            if !supported_location(&d.root, source, &d.locations) || project.read_file(source).is_err() { self.generation = None; return Err(Error::Stale); }
            let directory = source.rsplit_once('/').map(|(parent,_)|parent).ok_or(Error::Stale)?;
            d.directory = relative(&d.root, directory)?;
        }
        refresh_migrations();
        Ok(())
    }
    pub(crate) fn generation_view(&self, project: &Project) -> Result<Option<View>, Error> {
        let Some(d) = self.generation.as_ref().filter(|d| project.root().ok().as_deref() == Some(d.root.as_str())) else { return Ok(None) };
        let preview = candidate(d).unwrap_or_else(|e| format!("Preview unavailable: {e}"));
        let sql_preview = sql();
        Ok(Some(View::new(Body::Form(vec![
            Field::new("generation-directory","Directory",FieldValue::Text(d.directory.clone())),
            Field::new("generation-category","Category",FieldValue::Choice(CATEGORIES.iter().map(|s|s.to_string()).collect(),d.category.clone())),
            Field::new("generation-version","Version",FieldValue::Text(d.version.clone())),
            Field::new("generation-description","Description",FieldValue::Text(d.description.clone())),
            Field::new("generation-suffix","File extension",FieldValue::Choice(d.naming.suffixes.clone(),d.suffix.clone())),
            Field::new("generation-preview","Target file",FieldValue::readonly_text(&preview,true)),
            Field::new("generation-content","SQL preview",FieldValue::readonly_text(if sql_preview.trim().is_empty() {"No SQL statements (empty migration)"} else {&sql_preview},true)),
        ])).button(Button::new("prepare-generation","Prepare…").icon("plus")).button(Button::new("close-generation","Cancel").icon("close"))
            .status(if d.ready {"Prepared. In the opened migration (or any project SQL file), press Alt+Enter and choose Create Flyway migration from prepared form…; you confirm before the file is created."} else {"Naming preview only. Prepare it, then apply through the editor code action; no file has been written."})))
    }
    pub(crate) fn generation_event(&mut self, event: &ViewEvent, ctx: &Invocation) -> Result<bool, Error> {
        let relevant = matches!(event,ViewEvent::Field(key,_) if key.starts_with("generation-"))
            || matches!(event,ViewEvent::Button(id) if matches!(id.as_str(),"prepare-generation"|"close-generation"));
        if !relevant {
            return Ok(false);
        }
        let project = ctx.project().ok_or(Error::Stale)?;
        let d = self.generation.as_mut().ok_or(Error::Stale)?;
        if root(&project)? != d.root {
            return Err(Error::Stale);
        }
        match event {
            ViewEvent::Field(key, value) => {
                if value.len() > 1024 || value.chars().any(char::is_control) {
                    return Err("Use a bounded field value without control characters.".into());
                }
                match key.as_str() {
                    "generation-directory" => d.directory = value.clone(),
                    "generation-category" => d.category = value.clone(),
                    "generation-version" => d.version = value.clone(),
                    "generation-description" => d.description = value.clone(),
                    "generation-suffix" => d.suffix = value.clone(),
                    _ => return Err("This preview field is read-only.".into()),
                }
                d.ready = false;
            }
            ViewEvent::Button(id) if id == "close-generation" => self.generation = None,
            ViewEvent::Button(_) => {
                let config = unchanged(&project, d)?;
                let target = candidate(d)?;
                collision(&project, &config, &target, None)?;
                d.ready = true;
                // Opening is a convenience for reaching the create code action; the form stays prepared without it.
                if let Some(path) = newest_in(&project, &config, d) {
                    let _ = host::open_location(&path, 0, 0);
                }
            }
            _ => {}
        }
        ui_kit::web::refresh_view("migrations", include_str!("ui_config.json"));
        Ok(true)
    }
}
impl FileActions for Flyway {
    fn file_actions(&mut self, doc: &Document, _selection: Range) -> Vec<FileAction> {
        if doc.text.len() > MAX_TEXT || !(matches!(doc.language.as_str(), "sql" | "java" | "kotlin") || is_flyway_config(&doc.path) || is_config_file(&doc.path)) {
            return Vec::new();
        }
        let Some(project) = project_of(&doc.path) else { return Vec::new() };
        let Ok(root) = root(&project) else { return Vec::new() };
        if relative(&root, &doc.path).is_err() {
            return Vec::new();
        }
        let migrations = self.flyway_project(&project);
        let mut actions = Vec::new();
        if let Some(key) = self.hibernate_schema.as_ref().and_then(|candidate| candidate.key(&root)) {
            actions.push(FileAction { title: "Save Hibernate schema SQL as migration…".into(), kind: Some("refactor.rewrite".into()),
                data: Some(serde_json::json!([root, doc.path, doc.version, fingerprint(&doc.text), format!("{}{key}", hibernate_schema::ACTION)]).to_string()), ..FileAction::default() });
        }
        for (kind, title) in [
            (CREATE, "Create Flyway SQL migration…"),
            (source::JAVA, "Create Flyway Java migration…"),
            (source::KOTLIN, "Create Flyway Kotlin migration…"),
            (source::JAVA_CALLBACK, "Create Flyway Java callback…"),
            (source::SQL_CALLBACK, "Create Flyway SQL callback…"),
            (PREPARED, "Create Flyway migration from prepared form…"),
            (RENAME, "Rename Flyway migration…"),
        ] {
            if kind == PREPARED && !self.generation.as_ref().is_some_and(|d| d.root == root && d.ready) {
                continue;
            }
            if kind != PREPARED && !migrations {
                continue;
            }
            if kind == RENAME && doc.language != "sql" {
                continue;
            }
            actions.push(FileAction {
                title: title.into(),
                kind: Some(if kind == RENAME { "refactor.rename" } else { "refactor.rewrite" }.into()),
                data: Some(serde_json::json!([root, doc.path, doc.version, fingerprint(&doc.text), kind]).to_string()),
                ..FileAction::default()
            });
        }
        actions
    }
    fn resolve_file_action(&mut self, doc: &Document, action: FileAction) -> Result<FileAction, Error> {
        (|| {
        let data: serde_json::Value =
            serde_json::from_str(action.data.as_deref().filter(|s| s.len() < 8192).ok_or(Error::Stale)?).map_err(|_| Error::Stale)?;
        let data = data.as_array().filter(|a| a.len() == 5).ok_or(Error::Stale)?;
        let expected = data[0].as_str().ok_or(Error::Stale)?;
        if doc.text.len() > MAX_TEXT
            || data[1].as_str() != Some(doc.path.as_str())
            || data[2].as_u64() != Some(doc.version)
            || data[3].as_str() != Some(fingerprint(&doc.text).as_str())
        {
            return Err(Error::Stale);
        }
        let project = context(doc, expected)?;
        source_current(&project, doc)?;
        let kind = data[4].as_str().ok_or(Error::Stale)?;
        if let Some(key) = kind.strip_prefix(hibernate_schema::ACTION) {
            let snapshot = self.hibernate_schema.clone().ok_or(Error::Stale)?;
            snapshot.fresh(&project, key)?;
            self.begin_generation(&project)?;
            let mut draft = self.generation.clone().ok_or(Error::Stale)?;
            draft.version = host::input("Migration version", Some(&draft.version))?.ok_or(Error::Cancelled)?;
            draft.description = host::input("Migration description", Some("initial schema"))?.ok_or(Error::Cancelled)?;
            draft.directory = host::input("Migration directory inside configured locations", Some(&draft.directory))?.ok_or(Error::Cancelled)?;
            let config = unchanged(&project, &draft)?; let target = candidate(&draft)?;
            collision(&project, &config, &target, None)?;
            if !ui_kit::prompts::confirm_exact("Create migration from this Hibernate snapshot?", &format!("{}\n\nCreate: {target}\n\nExact captured creation SQL:\n{}\n\nOne non-overwriting undoable workspace Create. This does not execute SQL, compare a live DB or configure a persistence unit. Review dialect/database compatibility and dependencies before any later migration run.", snapshot.provenance(), ui_kit::prompts::readable_source(&snapshot.sql)))? { return Err(Error::Cancelled); }
            let project = context(doc, expected)?; source_current(&project, doc)?;
            if unchanged(&project, &draft)? != config { return Err(Error::Stale); }
            snapshot.fresh(&project, key)?;
            collision(&project, &config, &target, None)?;
            return Ok(FileAction { title: "Save Hibernate schema SQL as migration".into(), kind: action.kind, operations: vec![FileOperation::Create { path: target.clone(), text: snapshot.sql, overwrite: false }], open: Some(target), ..FileAction::default() });
        }
        if matches!(kind, source::JAVA | source::KOTLIN | source::JAVA_CALLBACK | source::SQL_CALLBACK) {
            return source::resolve(self, doc, action, &project, expected, kind);
        }
        if kind == RENAME {
            let config = configuration(&project)?;
            if !inventory(&project, &config)?.contains(&doc.path) {
                return Err("The source is not in a configured migration location.".into());
            }
            let old = doc.path.rsplit('/').next().ok_or(Error::Stale)?;
            if !matches!(parse(old, &config.naming), Name::Migration { .. } | Name::Invalid(_)) {
                return Err("Callbacks and unrelated SQL files are not migration rename targets.".into());
            }
            let name = host::input("Corrected migration file name (configured prefix/version/separator/suffix)", Some(old))?
                .ok_or(Error::Cancelled)?;
            if !file_name(&name) || !matches!(parse(&name,&config.naming),Name::Migration{description,..} if !description.is_empty()) {
                return Err("Enter a complete valid migration filename with a description.".into());
            }
            let target = format!("{}/{}", doc.path.rsplit_once('/').ok_or(Error::Stale)?.0, name);
            if target == doc.path {
                return Err("Choose a different migration name.".into());
            }
            collision(&project, &config, &target, Some(&doc.path))?;
            if !ui_kit::prompts::confirm_exact(
                "Rename Flyway migration?",
                &format!(
                    "From: {}\nTo: {target}\n\nRename only, in one undoable workspace action. SQL and references are unchanged. Save unsaved source changes first; the host rejects dirty resource buffers. Applied-migration history is not checked; renaming an applied migration can affect validation.",
                    doc.path
                ),
            )? {
                return Err(Error::Cancelled);
            }
            let project = context(doc, expected)?;
            source_current(&project, doc)?;
            let current = configuration(&project)?;
            if current != config {
                return Err(Error::Stale);
            }
            collision(&project, &current, &target, Some(&doc.path))?;
            return Ok(FileAction {
                title: "Rename Flyway migration".into(),
                kind: action.kind,
                operations: vec![FileOperation::Rename { from: doc.path.clone(), to: target.clone() }],
                open: Some(target),
                ..FileAction::default()
            });
        }
        let d = if kind == PREPARED {
            self.generation.as_ref().filter(|d| d.root == expected && d.ready).cloned().ok_or(Error::Stale)?
        } else if kind == CREATE {
            self.begin_generation(&project)?;
            let mut d = self.generation.clone().ok_or(Error::Stale)?;
            d.version = host::input("Migration version", Some(&d.version))?.ok_or(Error::Cancelled)?;
            d.description = host::input("Migration description", None)?.ok_or(Error::Cancelled)?;
            d.directory = host::input("Migration directory inside configured locations", Some(&d.directory))?.ok_or(Error::Cancelled)?;
            d
        } else {
            return Err(Error::Stale);
        };
        let config = unchanged(&project, &d)?;
        let target = candidate(&d)?;
        let text = sql();
        collision(&project, &config, &target, None)?;
        if !ui_kit::prompts::confirm_exact("Create Flyway migration?", &confirmation(&target, &text))? {
            return Err(Error::Cancelled);
        }
        let project = context(doc, expected)?;
        source_current(&project, doc)?;
        let current = unchanged(&project, &d)?;
        collision(&project, &current, &target, None)?;
        Ok(FileAction {
            title: "Create Flyway SQL migration".into(),
            kind: action.kind,
            operations: vec![FileOperation::Create { path: target.clone(), text, overwrite: false }],
            open: Some(target),
            ..FileAction::default()
        })
        })().inspect_err(|error| {
            if !matches!(error, Error::Cancelled) {
                host::notify(NotificationLevel::Warning, "Flyway action could not be prepared", &error.to_string());
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    const DIR: &str = "/project/src/main/resources/db/migration";
    fn setup() -> (MockHost, Flyway, Document) {
        let host = MockHost::install().with_capabilities(&["project.read", "storage"]);
        host.add_file("src/main/resources/db/migration/V1__init.sql", "-- existing\n");
        host.open_editor(&format!("{DIR}/V1__init.sql"), "sql", "-- existing\n", &[]);
        (
            host,
            Flyway::default(),
            Document { path: format!("{DIR}/V1__init.sql"), language: "sql".into(), version: 1, text: "-- existing\n".into() },
        )
    }
    fn action(plugin: &mut Flyway, doc: &Document, kind: &str) -> FileAction {
        plugin
            .file_actions(doc, Range::default())
            .into_iter()
            .find(|a| a.data.as_deref().unwrap().ends_with(&format!("\"{kind}\"]")))
            .unwrap()
    }
    fn answer_create(host: &MockHost, version: &str, description: &str, directory: &str, confirm: bool) {
        for answer in [version, description, directory] {
            host.answer_next_prompt(Some(&[answer]));
        }
        host.answer_next_prompt(if confirm { Some(&[]) } else { None });
    }
    #[test]
    fn lazy_create_returns_one_non_overwriting_template_operation_only_after_confirmation() {
        let (host, mut plugin, doc) = setup();
        let action = action(&mut plugin, &doc, CREATE);
        assert!(action.operations.is_empty() && action.edits.is_empty());
        answer_create(&host, "2", "add customer email!", "src/main/resources/db/migration", true);
        let resolved = plugin.resolve_file_action(&doc, action).unwrap();
        assert_eq!(
            resolved.operations,
            [FileOperation::Create { path: format!("{DIR}/V2__add_customer_email.sql"), text: sql(), overwrite: false }]
        );
        assert_eq!(resolved.open, Some(format!("{DIR}/V2__add_customer_email.sql")));
        assert!(resolved.edits.is_empty() && host.process_runs().is_empty());
        assert!(host.revealed_views().is_empty(), "action resolution must not invoke command-only view navigation");
        assert!(
            !host.invocation().project().unwrap().exists("src/main/resources/db/migration/V2__add_customer_email.sql").unwrap(),
            "resolving never writes outside host workspace apply"
        );
        let preview = confirmation("/project/sql/V2__a.sql", &sql());
        assert!(preview.contains("/project/sql/V2__a.sql") && preview.contains(&sql()) && preview.contains("undoable"));
    }
    #[test]
    fn parameterized_java_kotlin_and_callback_generation_are_exact_undoable_creates() {
        for (kind, extension, package_line) in [(source::JAVA, "java", "package com.acme;"), (source::KOTLIN, "kt", "package com.acme")] {
            let (host, mut plugin, doc) = setup();
            let selected = action(&mut plugin, &doc, kind);
            for answer in ["2", "add customers", "src/main/java", "com.acme", "Yes"] { host.answer_next_prompt(Some(&[answer])); }
            let resolved = plugin.resolve_file_action(&doc, selected).unwrap();
            let FileOperation::Create { path, text, overwrite } = &resolved.operations[0] else { panic!("create"); };
            assert_eq!(path, &format!("/project/src/main/java/com/acme/V2__add_customers.{extension}"));
            assert!(!overwrite);
            assert!(text.starts_with(package_line) && text.contains("V2__add_customers") && text.contains("BaseJavaMigration"));
            assert!(!text.contains("$NAME$") && !text.contains("$END$"));
            assert!(host.process_runs().is_empty());
        }
        let (host, mut plugin, doc) = setup();
        let selected = action(&mut plugin, &doc, source::JAVA_CALLBACK);
        host.answer_next_prompt(Some(&["AuditCallback"]));
        host.answer_next_prompt(Some(&["3", "4"]));
        for answer in ["false", "src/main/java", "com.acme", "Yes"] { host.answer_next_prompt(Some(&[answer])); }
        let resolved = plugin.resolve_file_action(&doc, selected).unwrap();
        let FileOperation::Create { text, .. } = &resolved.operations[0] else { panic!("create"); };
        assert!(text.contains("event == Event.BEFORE_MIGRATE || event == Event.AFTER_MIGRATE"));
        assert!(text.contains("return false;") && text.contains("class AuditCallback implements Callback"));
        let (host, mut plugin, doc) = setup();
        let selected = action(&mut plugin, &doc, source::SQL_CALLBACK);
        for answer in ["4", "audit", "src/main/resources/db/migration", "Yes"] { host.answer_next_prompt(Some(&[answer])); }
        let resolved = plugin.resolve_file_action(&doc, selected).unwrap();
        assert_eq!(resolved.open.as_deref(), Some("/project/src/main/resources/db/migration/afterMigrate__audit.sql"));
    }
    #[test]
    fn source_generation_cancellation_keyword_packages_and_version_conflicts_never_write() {
        for stage in 0..5 {
            let (host, mut plugin, doc) = setup();
            let selected = action(&mut plugin, &doc, source::JAVA);
            for answer in ["2", "add", "src/main/java", "com.acme"].iter().take(stage) { host.answer_next_prompt(Some(&[answer])); }
            assert!(matches!(plugin.resolve_file_action(&doc, selected), Err(Error::Cancelled)));
            assert!(host.process_runs().is_empty());
        }
        for (version, package) in [("1_0", "com.acme"), ("2", "com.class")] {
            let (host, mut plugin, doc) = setup();
            let selected = action(&mut plugin, &doc, source::JAVA);
            for answer in [version, "add", "src/main/java", package] { host.answer_next_prompt(Some(&[answer])); }
            assert!(plugin.resolve_file_action(&doc, selected).is_err());
            assert!(host.process_runs().is_empty());
        }
    }
    #[test]
    fn cancel_at_each_stage_and_denied_project_access_do_not_return_operations() {
        for stage in 0..4 {
            let (host, mut plugin, doc) = setup();
            let action = action(&mut plugin, &doc, CREATE);
            for answer in ["2", "add", "src/main/resources/db/migration"].iter().take(stage) {
                host.answer_next_prompt(Some(&[answer]));
            }
            assert!(matches!(plugin.resolve_file_action(&doc, action), Err(Error::Cancelled)));
            assert!(host.process_runs().is_empty());
        }
        let (host, mut plugin, doc) = setup();
        let action = action(&mut plugin, &doc, CREATE);
        let host = host.with_capabilities(&["storage"]);
        assert!(plugin.resolve_file_action(&doc, action).is_err());
        assert!(plugin.file_actions(&doc, Range::default()).is_empty());
        assert!(host.process_runs().is_empty());
    }
    #[test]
    fn create_rejects_disk_buffer_and_normalised_version_collisions() {
        for (version, existing) in [("2", "V2__other.sql"), ("1_0", "V1__init.sql")] {
            let (host, mut plugin, doc) = setup();
            host.add_file(&format!("src/main/resources/db/migration/{existing}"), "");
            let action = action(&mut plugin, &doc, CREATE);
            answer_create(&host, version, "add", "src/main/resources/db/migration", true);
            assert!(plugin.resolve_file_action(&doc, action).is_err());
        }
        let (host, mut plugin, doc) = setup();
        let action = action(&mut plugin, &doc, CREATE);
        host.open_editor(&format!("{DIR}/V2__add.sql"), "sql", "unsaved destination", &[]);
        answer_create(&host, "2", "add", "src/main/resources/db/migration", true);
        assert!(plugin.resolve_file_action(&doc, action).is_err());
        assert_eq!(host.editor_text(), "unsaved destination");
    }
    #[test]
    fn safe_rename_is_a_single_operation_and_refuses_callback_collision_or_outside_location() {
        let (host, mut plugin, doc) = setup();
        let a = action(&mut plugin, &doc, RENAME);
        host.answer_next_prompt(Some(&["V1__initial_schema.sql"]));
        host.answer_next_prompt(Some(&[]));
        let renamed = plugin.resolve_file_action(&doc, a).unwrap();
        assert_eq!(renamed.operations, [FileOperation::Rename { from: doc.path.clone(), to: format!("{DIR}/V1__initial_schema.sql") }]);
        assert!(renamed.edits.is_empty() && host.process_runs().is_empty());
        assert_eq!(
            host.invocation().project().unwrap().read_file(&doc.path).unwrap(),
            doc.text,
            "resolution did not rename or rewrite source"
        );
        host.add_file("src/main/resources/db/migration/V2__other.sql", "");
        for name in ["V2__renamed.sql", "../V3__escape.sql", "V3__escape.txt", "V1__init.sql"] {
            let a = action(&mut plugin, &doc, RENAME);
            host.answer_next_prompt(Some(&[name]));
            assert!(plugin.resolve_file_action(&doc, a).is_err(), "{name}");
        }
        for path in [format!("{DIR}/beforeMigrate.sql"), "/project/unrelated.sql".into()] {
            host.add_file(&path, "");
            let d = Document { path, language: "sql".into(), version: 1, text: String::new() };
            let a = action(&mut plugin, &d, RENAME);
            assert!(plugin.resolve_file_action(&d, a).is_err());
        }
    }
    #[test]
    fn malformed_names_can_be_renamed_but_source_snapshot_and_project_must_remain_current() {
        let (host, mut plugin, _) = setup();
        let path = format!("{DIR}/V2_bad.sql");
        host.add_file(&path, "-- body");
        let d = Document { path: path.clone(), language: "sql".into(), version: 3, text: "-- body".into() };
        let a = action(&mut plugin, &d, RENAME);
        host.answer_next_prompt(Some(&["V2__bad.sql"]));
        host.answer_next_prompt(Some(&[]));
        assert!(matches!(plugin.resolve_file_action(&d, a).unwrap().operations[0], FileOperation::Rename { .. }));
        let a = action(&mut plugin, &d, CREATE);
        let mut changed = d.clone();
        changed.version += 1;
        assert!(matches!(plugin.resolve_file_action(&changed, a), Err(Error::Stale)));
        let a = action(&mut plugin, &d, CREATE);
        let mut changed = d.clone();
        changed.text.push_str("new text");
        assert!(matches!(plugin.resolve_file_action(&changed, a), Err(Error::Stale)));
        let a = action(&mut plugin, &d, CREATE);
        host.add_file(&path, "changed disk contents");
        assert!(matches!(plugin.resolve_file_action(&d, a), Err(Error::Stale)));
        let a = action(&mut plugin, &d, CREATE);
        host.set_project_root(Some("/other"));
        assert!(matches!(plugin.resolve_file_action(&d, a), Err(Error::Stale)));
    }
    #[test]
    fn generation_form_previews_configured_names_and_requires_explicit_editor_application() {
        let (host, mut plugin, doc) = setup();
        host.add_file("flyway.conf","flyway.sqlMigrationPrefix=M\nflyway.sqlMigrationSeparator=-\nflyway.sqlMigrationSuffixes=.pgsql\nflyway.locations=filesystem:sql\n");
        plugin.run_command("new-migration", &host.invocation(), None).unwrap();
        assert!(host.process_runs().is_empty());
        for (key, value) in [("generation-directory", "sql"), ("generation-description", "add users"), ("generation-version", "3")] {
            plugin.handle("migrations", ViewEvent::Field(key.into(), value.into()), &host.invocation()).unwrap();
        }
        let view = plugin.render("migrations", None, &host.invocation()).unwrap();
        let Body::Form(fields) = view.body else { panic!("form") };
        assert!(
            fields.iter().any(|f| f.key == "generation-preview" && format!("{:?}", f.value).contains("/project/sql/M3-add_users.pgsql"))
        );
        host.add_file("sql/M2-older.pgsql", "select 1;");
        host.add_file("sql/M10-newest.pgsql", "select 1;");
        plugin.handle("migrations", ViewEvent::Button("prepare-generation".into()), &host.invocation()).unwrap();
        assert_eq!(host.opened_locations(), [("/project/sql/M10-newest.pgsql".to_owned(), 0, 0)]);
        let prepared = action(&mut plugin, &doc, PREPARED);
        host.answer_next_prompt(Some(&[]));
        let created = plugin.resolve_file_action(&doc, prepared).unwrap();
        assert!(matches!(&created.operations[0],FileOperation::Create{path,overwrite:false,..} if path=="/project/sql/M3-add_users.pgsql"));
        host.add_file("flyway.conf", "flyway.sqlMigrationPrefix=X\nflyway.locations=filesystem:sql\n");
        let prepared = action(&mut plugin, &doc, PREPARED);
        assert!(plugin.resolve_file_action(&doc, prepared).is_err(), "changed naming invalidates a prepared form");
    }
    #[test]
    fn paths_naming_and_category_are_validated_without_sql_generation_claims() {
        let (host, mut plugin, _) = setup();
        let project = host.invocation().project().unwrap();
        plugin.begin_generation(&project).unwrap();
        let d = plugin.generation.as_mut().unwrap();
        d.description = "views".into();
        for dir in ["../sql", "/other/sql", "src/main/resources/db/{vendor}", "src/main/resources/db/migration/*", "unconfigured"] {
            d.directory = dir.into();
            assert!(candidate(d).is_err(), "{dir}");
        }
        d.directory = "src/main/resources/db/migration".into();
        d.category = "Repeatable".into();
        d.version = "ignored".into();
        assert!(candidate(d).unwrap().ends_with("R__views.sql"));
        d.category = "Versioned".into();
        assert!(candidate(d).is_err());
        d.version = "2".into();
        d.naming.versioned = "../V".into();
        assert!(candidate(d).is_err());
        assert_eq!(next_version(&["340282366920938463463374607431768211455"]), "340282366920938463463374607431768211456");
        assert_eq!(next_version(&["0099"]), "0100");
    }
}

#[cfg(test)]
mod project_gate_tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    #[test]
    fn no_create_intentions_in_plain_project() {
        let host = MockHost::install();
        host.add_file("scripts/seed.sql", "select 1;");
        let doc = Document { path: "/project/scripts/seed.sql".into(), language: "sql".into(), version: 1, text: "select 1;".into() };
        let mut plugin = Flyway::default();
        assert!(plugin.file_actions(&doc, Range::default()).is_empty());
        host.add_file("flyway.toml", "[flyway]\nlocations = [\"filesystem:scripts\"]\n");
        assert!(plugin.file_actions(&doc, Range::default()).iter().any(|action| action.title == "Create Flyway SQL migration…"));
        assert!(host.process_runs().is_empty());
    }
}

#[cfg(test)]
mod collision_tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    #[test]
    fn inventory_limit_blocks_claims_of_complete_collision_validation() {
        let host = MockHost::install();
        for n in 0..10_000 {
            host.add_file(&format!("sql/V{n}__old.sql"), "");
        }
        let config = Config { locations: vec!["filesystem:sql".into()], ..Config::default() };
        assert!(inventory(&host.invocation().project().unwrap(), &config).unwrap_err().to_string().contains("limit"));
    }
    #[test]
    fn version_collisions_follow_existing_module_and_vendor_scopes() {
        let host = MockHost::install();
        let project = host.invocation().project().unwrap();
        let config = Config { locations: vec!["classpath:db/migration".into()], ..Config::default() };
        host.add_file("a/src/main/resources/db/migration/V1__old.sql", "");
        assert!(collision(&project, &config, "/project/b/src/main/resources/db/migration/V1__new.sql", None).is_ok());
        assert!(collision(&project, &config, "/project/a/src/main/resources/db/migration/V01_0__new.sql", None).is_err());
        let config = Config { locations: vec!["filesystem:sql/{vendor}".into()], ..Config::default() };
        host.add_file("sql/pg/V1__old.sql", "");
        assert!(collision(&project, &config, "/project/sql/mysql/V1__new.sql", None).is_ok());
        assert!(collision(&project, &config, "/project/sql/pg/V01_0__new.sql", None).is_err());
        host.add_file("blocked", "");
        assert!(vacant(&project, "/project/blocked/V2__new.sql").is_err());
    }
    #[test]
    fn rename_cancellation_and_bounded_provider_requests_are_safe() {
        let host = MockHost::install();
        let mut plugin = Flyway::default();
        let path = "/project/src/main/resources/db/migration/V1__old.sql";
        host.add_file(path, "");
        let doc = Document { path: path.into(), language: "sql".into(), version: 1, text: String::new() };
        for confirmed in [false, true] {
            let action = plugin.file_actions(&doc, Range::default()).into_iter().find(|a| a.title.starts_with("Rename")).unwrap();
            if confirmed {
                host.answer_next_prompt(Some(&["V1__renamed.sql"]));
            }
            assert!(matches!(plugin.resolve_file_action(&doc, action), Err(Error::Cancelled)));
        }
        let large = Document { text: "x".repeat(MAX_TEXT + 1), ..doc };
        assert!(plugin.file_actions(&large, Range::default()).is_empty());
        assert!(host.process_runs().is_empty());
    }
}

#[cfg(test)]
mod configuration_tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    #[test]
    fn truncated_application_config_discovery_does_not_fall_back_to_defaults() {
        let host = MockHost::install();
        for n in 0..100 {
            host.add_file(&format!("module{n}/src/main/resources/application.properties"), "spring.flyway.locations=classpath:custom");
        }
        assert!(configuration(&host.invocation().project().unwrap()).unwrap_err().to_string().contains("configuration discovery"));
        assert!(host.process_runs().is_empty());
    }
}

#[cfg(test)]
mod boundary_tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    #[test]
    fn oversized_config_and_ambiguous_category_are_rejected() {
        let host = MockHost::install();
        let project = host.invocation().project().unwrap();
        host.add_file("flyway.conf", &"x".repeat(MAX_TEXT + 1));
        assert!(configuration(&project).is_err());
        let d = Draft {
            root: "/project".into(),
            naming: naming::Naming { undo: "V".into(), ..naming::Naming::default() },
            locations: vec!["filesystem:sql".into()],
            directory: "sql".into(),
            version: "2".into(),
            description: "add".into(),
            category: "Undo".into(),
            suffix: ".sql".into(),
            ready: false,
        };
        assert!(candidate(&d).is_err(), "a duplicate prefix must not relabel the selected category");
    }
}
