//! Reviewed atomic registration into one existing conventional service file.
use crate::*;
use openidea_plugin::file_actions::FileAction;
use std::cell::RefCell;
const MAX_TEXT: usize = 32 << 10;
#[derive(Clone)]
struct Pending {
    root: String,
    doc: Document,
    legacy: String,
}
#[derive(Default)]
pub(crate) struct Registration {
    next: u64,
    pending: BTreeMap<String, Pending>,
}
fn scoped(path: &str, root: &str) -> bool {
    path.len() <= 4096
        && under(path, root)
        && !path.contains(['\\', ':'])
        && !path.chars().any(char::is_control)
        && !path.split('/').any(|p| matches!(p, "." | ".."))
}
pub(crate) fn actions(owner: &mut Main, doc: &Document, selection: Range) -> Vec<FileAction> {
    if doc.language != "php" || !doc.path.ends_with(".php") || doc.text.len() > MAX_TEXT {
        return Vec::new();
    }
    let Some(offset) = text::offset(&doc.text, selection.start) else {
        return Vec::new();
    };
    let Some(root) = host::project_of(&doc.path).and_then(|p| p.root().ok()) else {
        return Vec::new();
    };
    if !root.starts_with('/')
        || root.ends_with('/')
        || root.contains("//")
        || !scoped(&doc.path, &root)
    {
        return Vec::new();
    }
    let Some(index) = owner.indexes.get(&root).filter(|i| !i.partial && i.settings_current()) else {
        return Vec::new();
    };
    let Some(action) = service_actions(index, doc, offset).into_iter().find(|a| {
        a.data
            .as_deref()
            .is_some_and(|d| d.starts_with("service\n"))
    }) else {
        return Vec::new();
    };
    if owner.service_registration.pending.len() >= 8 {
        owner.service_registration.pending.clear();
    }
    let Some(next) = owner.service_registration.next.checked_add(1) else {
        return Vec::new();
    };
    owner.service_registration.next = next;
    let key = format!("service-register:{next}:{}", doc.version);
    owner.service_registration.pending.insert(
        key.clone(),
        Pending {
            root,
            doc: doc.clone(),
            legacy: action.data.unwrap(),
        },
    );
    vec![FileAction {
        title: action.title,
        kind: action.kind,
        data: Some(key),
        ..FileAction::default()
    }]
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
    if project.root()? != pending.root || project.read_file(&doc.path)? != doc.text {
        return Err(Error::Stale);
    }
    Ok(project)
}
pub(crate) fn resolve(
    owner: &mut Main,
    doc: &Document,
    action: FileAction,
) -> Result<FileAction, Error> {
    let key = action.data.as_deref().ok_or(Error::Stale)?;
    let pending = owner
        .service_registration
        .pending
        .remove(key)
        .ok_or(Error::Stale)?;
    let project = current(&pending, doc)?;
    let index = owner
        .indexes
        .get(&pending.root)
        .filter(|i| !i.partial && i.settings_current())
        .ok_or(Error::Stale)?;
    let (target, source) = pending
        .legacy
        .strip_prefix("service\n")
        .and_then(|s| s.split_once('\n'))
        .ok_or(Error::Stale)?;
    if source != doc.path
        || !SERVICE_FILES
            .iter()
            .any(|p| target == format!("{}/{p}", pending.root))
    {
        return Err(Error::Stale);
    }
    let (chosen, services) = services_file(&project).ok_or(Error::Stale)?;
    if chosen != target || services.len() > MAX_TEXT {
        return Err(Error::Stale);
    }
    let model = php_text::class_model(&doc.text).ok_or(Error::Stale)?;
    if index
        .classes
        .get(&model.fqcn)
        .is_none_or(|e| e.path != doc.path)
        || index.services.contains_key(&model.fqcn)
        || service_gen::registered_by_prototype(&services, &model.fqcn)
    {
        return Err(Error::Stale);
    }
    let mut fresh = Names::new();
    if target.ends_with(".xml") {
        index::xml_services(target, &services, &mut fresh);
    } else {
        index::yaml_services(target, &services, &mut fresh);
    }
    if fresh.contains_key(&model.fqcn) {
        return Err(Error::Stale);
    }
    let reads = RefCell::new(BTreeMap::<String, String>::new());
    let read = |path: &str| {
        if !scoped(path, &pending.root) || reads.borrow().len() >= 32 {
            return None;
        }
        let value = project.read_file(path).ok()?;
        if value.len() > MAX_TEXT {
            return None;
        }
        reads.borrow_mut().insert(path.to_owned(), value.clone());
        Some(value)
    };
    let ctx = checks::Context::new(index, &read);
    let plan = service_gen::plan(
        &ctx,
        &model,
        &[],
        !service_gen::has_autoconfigure(&services),
    );
    let (range, replacement) = if target.ends_with(".xml") {
        service_gen::xml_insertion(&services, &plan).ok_or(Error::Stale)?
    } else {
        service_gen::yaml_insertion(&services, &plan)
    };
    let mut proposed = services.clone();
    proposed.replace_range(range.clone(), &replacement);
    let preview = format!(
        "Adds a service for {} to {target}.\nConstructor values are suggestions or placeholders; check them. The PHP class is unchanged.\nUndo in the editor.\n\nResulting file:\n{proposed}",
        model.fqcn
    );
    if !ui_kit::prompts::confirm_exact("Add service definition?", &preview)? {
        return Err(Error::Cancelled);
    }
    current(&pending, doc)?;
    if services_file(&project).is_none_or(|pair| pair != (target.to_owned(), services.clone())) {
        return Err(Error::Stale);
    }
    for (path, value) in reads.into_inner() {
        if project.read_file(&path)? != value {
            return Err(Error::Stale);
        }
    }
    Ok(FileAction {
        title: action.title,
        kind: action.kind,
        edits: vec![(
            target.to_owned(),
            vec![TextEdit {
                range: range_of(&services, range.start, range.end),
                text: replacement,
            }],
        )],
        open: Some(target.to_owned()),
        ..FileAction::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{file_actions::FileActions, mock::MockHost};
    const PATH: &str = "/project/src/Mailer.php";
    const PHP: &str = "<?php\nnamespace App;\nclass Mailer {}\n";
    fn setup() -> (MockHost, Main, Document) {
        let host = MockHost::install();
        host.add_file("composer.json",r#"{"require":{"symfony/framework-bundle":"^7.0"},"autoload":{"psr-4":{"App\\":"src/"}}}"#);
        host.add_file("src/Mailer.php", PHP);
        host.add_file("config/services.yaml", "services: {}\n");
        let mut owner = Main::default();
        owner.activate(&host.context("aritti.symfony")).unwrap();
        (
            host,
            owner,
            Document {
                path: PATH.into(),
                language: "php".into(),
                version: 3,
                text: PHP.into(),
            },
        )
    }
    fn nominated(owner: &mut Main, doc: &Document) -> FileAction {
        owner
            .file_actions(doc, Range::new(Position::new(2, 7), Position::new(2, 7)))
            .into_iter()
            .find(|a| {
                a.data
                    .as_deref()
                    .is_some_and(|d| d.starts_with("service-register:"))
            })
            .unwrap()
    }
    #[test]
    fn exact_review_produces_one_atomic_existing_file_edit_and_consumes_token() {
        let (host, mut owner, doc) = setup();
        assert!(
            !owner
                .code_actions(&doc, Range::new(Position::new(2, 7), Position::new(2, 7)))
                .iter()
                .any(|a| a
                    .data
                    .as_deref()
                    .is_some_and(|d| d.starts_with("service\n")))
        );
        let action = nominated(&mut owner, &doc);
        assert!(action.edits.is_empty() && action.operations.is_empty());
        let replay = action.clone();
        host.answer_next_prompt(Some(&[]));
        let resolved = owner.resolve_file_action(&doc, action).unwrap();
        assert!(resolved.operations.is_empty());
        assert_eq!(resolved.edits.len(), 1);
        assert_eq!(resolved.edits[0].0, "/project/config/services.yaml");
        assert!(resolved.edits[0].1[0].text.contains("App\\Mailer"));
        assert_eq!(
            host::project_of(PATH)
                .unwrap()
                .read_file("config/services.yaml")
                .ok()
                .as_deref(),
            Some("services: {}\n")
        );
        assert!(matches!(
            owner.resolve_file_action(&doc, replay),
            Err(Error::Stale)
        ));
    }
    #[test]
    fn stale_source_version_target_registration_and_foreign_root_refuse_without_writes() {
        let (host, mut owner, doc) = setup();
        let action = nominated(&mut owner, &doc);
        let stale = Document {
            version: 4,
            ..doc.clone()
        };
        assert!(matches!(
            owner.resolve_file_action(&stale, action),
            Err(Error::Stale)
        ));
        let action = nominated(&mut owner, &doc);
        host.add_file("config/services.yaml", "services:\n  App\\Mailer: ~\n");
        assert!(matches!(
            owner.resolve_file_action(&doc, action),
            Err(Error::Stale)
        ));
        host.add_file("config/services.yaml", "services: {}\n");
        let action = nominated(&mut owner, &doc);
        host.set_project_root(Some("/other"));
        assert!(matches!(
            owner.resolve_file_action(&doc, action),
            Err(Error::Stale)
        ));
    }
    #[test]
    fn cancelled_and_oversized_reviews_never_return_edits() {
        let (host, mut owner, doc) = setup();
        let action = nominated(&mut owner, &doc);
        host.answer_next_prompt(None);
        assert!(matches!(
            owner.resolve_file_action(&doc, action),
            Err(Error::Cancelled)
        ));
        host.add_file(
            "config/services.yaml",
            &format!("#{}\nservices: {{}}\n", "x".repeat(17000)),
        );
        let action = nominated(&mut owner, &doc);
        host.answer_next_prompt(Some(&[]));
        assert!(owner.resolve_file_action(&doc, action).is_err());
    }
}
