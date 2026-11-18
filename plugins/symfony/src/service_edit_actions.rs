//! Lazy reviewed service argument/tag generation in the current YAML/XML file.
use crate::*;
use openidea_plugin::file_actions::FileAction;
use std::cell::RefCell;
const MAX_TEXT: usize = 32 << 10;
#[derive(Clone)]
struct Pending {
    root: String,
    doc: Document,
    offset: usize,
    title: String,
}
#[derive(Default)]
pub(crate) struct Edits {
    next: u64,
    pending: BTreeMap<String, Pending>,
}
fn valid(root: &str, doc: &Document) -> bool {
    doc.text.len() <= MAX_TEXT
        && matches!(
            (doc.language.as_str(), doc.path.rsplit('.').next()),
            ("yaml", Some("yaml" | "yml")) | ("xml", Some("xml"))
        )
        && checks::is_service_file(&doc.path)
        && under(&doc.path, root)
        && crate::file_actions::normalized(root)
        && crate::file_actions::normalized(&doc.path)
}
fn generated(
    index: &Index,
    project: &Project,
    root: &str,
    doc: &Document,
    offset: usize,
) -> (Vec<CodeAction>, BTreeMap<String, String>) {
    let reads = RefCell::new(BTreeMap::<String, String>::new());
    let read = |path: &str| {
        if let Some(value) = reads.borrow().get(path) {
            return Some(value.clone());
        }
        if reads.borrow().len() >= 32
            || !under(path, root)
            || !crate::file_actions::normalized(path)
            || host::project_of(path)
                .and_then(|p| p.root().ok())
                .as_deref()
                != Some(root)
        {
            return None;
        }
        let value = project.read_file(path).ok()?;
        if value.len() > MAX_TEXT {
            return None;
        }
        reads.borrow_mut().insert(path.into(), value.clone());
        Some(value)
    };
    let actions = service_source_actions(index, doc, offset, &read);
    (actions, reads.into_inner())
}
pub(crate) fn actions(owner: &mut Main, doc: &Document, selection: Range) -> Vec<FileAction> {
    let Some(project) = host::project_of(&doc.path) else {
        return Vec::new();
    };
    let Ok(root) = project.root() else {
        return Vec::new();
    };
    if !valid(&root, doc) {
        return Vec::new();
    }
    let Some(index) = owner.indexes.get(&root).filter(|i| !i.partial && i.settings_current()) else {
        return Vec::new();
    };
    let Some(offset) = text::offset(&doc.text, selection.start) else {
        return Vec::new();
    };
    let (candidates, _) = generated(index, &project, &root, doc, offset);
    if owner.service_edits.pending.len() + candidates.len() > 8 {
        owner.service_edits.pending.clear();
    }
    let mut out = Vec::new();
    for action in candidates.into_iter().take(8) {
        let Some(next) = owner.service_edits.next.checked_add(1) else {
            break;
        };
        owner.service_edits.next = next;
        let key = format!("service-edit:{next}:{}", doc.version);
        owner.service_edits.pending.insert(
            key.clone(),
            Pending {
                root: root.clone(),
                doc: doc.clone(),
                offset,
                title: action.title.clone(),
            },
        );
        out.push(FileAction {
            title: action.title,
            kind: action.kind,
            data: Some(key),
            ..FileAction::default()
        });
    }
    out
}
fn current(owner: &Main, pending: &Pending, doc: &Document) -> Result<Project, Error> {
    if doc.path != pending.doc.path
        || doc.language != pending.doc.language
        || doc.version != pending.doc.version
        || doc.text != pending.doc.text
        || !valid(&pending.root, doc)
        || owner.indexes.get(&pending.root).is_none_or(|i| i.partial || !i.settings_current())
    {
        return Err(Error::Stale);
    }
    let project = host::project_of(&doc.path).ok_or(Error::Stale)?;
    if project.root()? != pending.root
        || project.read_file(&doc.path)? != doc.text
        || !index::is_symfony(&project)
    {
        return Err(Error::Stale);
    }
    Ok(project)
}
pub(crate) fn resolve(
    owner: &mut Main,
    doc: &Document,
    action: FileAction,
) -> Result<FileAction, Error> {
    let pending = owner
        .service_edits
        .pending
        .remove(action.data.as_deref().ok_or(Error::Stale)?)
        .ok_or(Error::Stale)?;
    let project = current(owner, &pending, doc)?;
    let index = owner.indexes.get(&pending.root).filter(|index|index.settings_current()).ok_or(Error::Stale)?;
    let (candidates, reads) = generated(index, &project, &pending.root, doc, pending.offset);
    let mut candidates = candidates.into_iter().filter(|a| a.title == pending.title);
    let selected = candidates.next().ok_or(Error::Stale)?;
    if candidates.next().is_some() {
        return Err(Error::Stale);
    }
    let [(target, edits)] = selected.edits.as_slice() else {
        return Err(Error::Stale);
    };
    if target != &doc.path {
        return Err(Error::Stale);
    }
    let [edit] = edits.as_slice() else {
        return Err(Error::Stale);
    };
    let start = text::offset(&doc.text, edit.range.start).ok_or(Error::Stale)?;
    let end = text::offset(&doc.text, edit.range.end).ok_or(Error::Stale)?;
    if start > end {
        return Err(Error::Stale);
    }
    let mut proposed = doc.text.clone();
    proposed.replace_range(start..end, &edit.text);
    let preview = format!(
        "Edits {}: {}.\nThe values are suggestions; check them. No other file changes.\nUndo in the editor.\n\nResulting file:\n{proposed}",
        doc.path, pending.title
    );
    if !ui_kit::prompts::confirm_exact("Add service arguments and tags?", &preview)? {
        return Err(Error::Cancelled);
    }
    current(owner, &pending, doc)?;
    for (path, value) in reads {
        if host::project_of(&path)
            .and_then(|p| p.root().ok())
            .as_deref()
            != Some(pending.root.as_str())
            || project.read_file(&path)? != value
        {
            return Err(Error::Stale);
        }
    }
    Ok(FileAction {
        title: selected.title,
        kind: selected.kind,
        edits: selected.edits,
        open: Some(doc.path.clone()),
        ..FileAction::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{file_actions::FileActions, mock::MockHost};
    const PATH: &str = "/project/config/services.yaml";
    const TEXT: &str = "services:\n    App\\Mailer:\n        public: true\n";
    fn setup() -> (MockHost, Main, Document) {
        let host = MockHost::install();
        host.add_file("composer.json",r#"{"require":{"symfony/framework-bundle":"^7.0"},"autoload":{"psr-4":{"App\\":"src/"}}}"#);
        host.add_file("src/Mailer.php","<?php\nnamespace App;\nclass Mailer { public function __construct(string $name) {} }\n");
        host.add_file(PATH, TEXT);
        let mut owner = Main::default();
        owner.activate(&host.context("aritti.symfony")).unwrap();
        (
            host,
            owner,
            Document {
                path: PATH.into(),
                language: "yaml".into(),
                version: 2,
                text: TEXT.into(),
            },
        )
    }
    fn nominated(owner: &mut Main, doc: &Document) -> FileAction {
        owner
            .file_actions(doc, Range::new(Position::new(2, 8), Position::new(2, 8)))
            .into_iter()
            .find(|a| a.title.starts_with("Resync"))
            .unwrap()
    }
    #[test]
    fn lazy_service_edits_have_one_atomic_result_and_no_legacy_codeaction_or_immediate_write() {
        let (host, mut owner, doc) = setup();
        assert!(
            !owner
                .code_actions(&doc, Range::new(Position::new(2, 8), Position::new(2, 8)))
                .iter()
                .any(|a| a.title.starts_with("Resync") || a.title.starts_with("Add tags"))
        );
        let pending = nominated(&mut owner, &doc);
        assert!(pending.operations.is_empty() && pending.edits.is_empty());
        let replay = pending.clone();
        // Supporting source is read freshly at resolution, then reviewed as a concrete plan.
        host.add_file(
            "src/Mailer.php",
            "<?php\nnamespace App;\nclass Mailer { public function __construct(int $count) {} }\n",
        );
        host.answer_next_prompt(Some(&[]));
        let resolved = owner.resolve_file_action(&doc, pending).unwrap();
        assert!(resolved.operations.is_empty());
        assert_eq!(resolved.edits.len(), 1);
        assert_eq!(resolved.edits[0].0, PATH);
        assert!(resolved.edits[0].1[0].text.contains("$count: 0"));
        assert!(!resolved.edits[0].1[0].text.contains("$name"));
        assert_eq!(
            host.invocation()
                .project()
                .unwrap()
                .read_file(PATH)
                .unwrap(),
            TEXT
        );
        assert!(matches!(
            owner.resolve_file_action(&doc, replay),
            Err(Error::Stale)
        ));
    }
    #[test]
    fn service_edit_cancellation_stale_source_partial_oversize_and_foreign_context_refuse() {
        let (host, mut owner, doc) = setup();
        let pending = nominated(&mut owner, &doc);
        host.answer_next_prompt(None);
        assert!(matches!(
            owner.resolve_file_action(&doc, pending),
            Err(Error::Cancelled)
        ));
        let pending = nominated(&mut owner, &doc);
        assert!(matches!(
            owner.resolve_file_action(
                &Document {
                    version: 3,
                    ..doc.clone()
                },
                pending
            ),
            Err(Error::Stale)
        ));
        let pending = nominated(&mut owner, &doc);
        host.add_file(PATH, "services: {}\n");
        assert!(matches!(
            owner.resolve_file_action(&doc, pending),
            Err(Error::Stale)
        ));
        host.add_file(PATH, TEXT);
        owner.indexes.get_mut("/project").unwrap().partial = true;
        assert!(
            owner
                .file_actions(&doc, Range::new(Position::new(2, 8), Position::new(2, 8)))
                .is_empty()
        );
        owner.indexes.get_mut("/project").unwrap().partial = false;
        let pending = nominated(&mut owner, &doc);
        host.set_project_root(Some("/other"));
        assert!(matches!(
            owner.resolve_file_action(&doc, pending),
            Err(Error::Stale)
        ));
        let (host, mut owner, doc) = setup();
        host.add_file("src/Mailer.php", &"x".repeat(32 * 1024 + 1));
        assert!(
            owner
                .file_actions(&doc, Range::new(Position::new(2, 8), Position::new(2, 8)))
                .is_empty()
        );
        let (host, mut owner, mut doc) = setup();
        doc.text.push_str(&format!("#{}\n", "x".repeat(17000)));
        host.add_file(PATH, &doc.text);
        let pending = nominated(&mut owner, &doc);
        host.answer_next_prompt(Some(&[]));
        assert!(
            owner.resolve_file_action(&doc, pending).is_err(),
            "oversized full-file preview refuses"
        );
        let (_host, mut owner, doc) = setup();
        let wrong = Document {
            language: "xml".into(),
            ..doc
        };
        assert!(
            owner
                .file_actions(&wrong, Range::new(Position::new(2, 8), Position::new(2, 8)))
                .is_empty()
        );
    }
}
