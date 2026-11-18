//! Applies a resolved `FileAction` from a command: new files go through the
//! project, edits through the open editor. Not one undo step.
use crate::{denied, prompts};
use openidea_plugin::file_actions::{FileAction, FileActions, FileOperation};
use openidea_plugin::providers::Document;
use openidea_plugin::{Error, Project, TextEdit, TextEditor, host, text};

const ONLY_FROM_CODE_ACTIONS: &str = "This action can only be applied from ⌥⏎.";

fn unsupported() -> Error {
    Error::Invalid(ONLY_FROM_CODE_ACTIONS.into())
}

/// `editor` pairs the open editor with the document version the action was resolved from.
pub fn apply(
    project: &Project,
    editor: Option<(&TextEditor, u64)>,
    action: &FileAction,
    plugin: &str,
) -> Result<(), Error> {
    let mut created: Vec<(&str, String)> = Vec::new();
    for operation in &action.operations {
        let FileOperation::Create { path, text, overwrite: false } = operation else {
            return Err(unsupported());
        };
        if project.exists(path)? {
            return Err(Error::Invalid(format!("{path} already exists; nothing was written.")));
        }
        created.push((path, text.clone()));
    }

    let mut editor_edits: &[TextEdit] = &[];
    for (path, edits) in &action.edits {
        if let Some((_, text)) = created.iter_mut().find(|(created_path, _)| created_path == path) {
            apply_to_text(text, edits)?;
            continue;
        }
        match editor {
            Some((editor, _)) if editor.path() == *path => editor_edits = edits,
            _ => return Err(unsupported()),
        }
    }

    for (path, text) in &created {
        project
            .write_file(path, text)
            .map_err(|e| denied::map(e, "Creating files", "project.write", plugin))?;
    }

    if let Some((editor, version)) = editor.filter(|_| !editor_edits.is_empty()) {
        editor
            .apply_edits(version, editor_edits)
            .map_err(|e| denied::map(e, "Editing the file", "editor.write", plugin))?;
    }

    if let Some(path) = &action.open {
        host::open_location(path, 0, 0)?;
    }

    Ok(())
}

/// Resolves and applies the active editor's ⌥⏎ action whose title starts with
/// `prefix`. `Ok(false)` when the editor offers no such action; a cancelled
/// prompt is `Ok(true)` with nothing written.
pub fn run_titled(
    provider: &mut impl FileActions,
    project: &Project,
    editor: &TextEditor,
    prefix: &str,
    plugin: &str,
) -> Result<bool, Error> {
    let read = |e| denied::map(e, "Reading the open file", "editor.read", plugin);
    let document = editor.document().map_err(read)?;
    let selection = editor.selections().map_err(read)?.first().copied().unwrap_or_default();
    let doc = Document { path: editor.path(), language: editor.language(), version: document.version, text: document.text };
    let Some(action) = provider.file_actions(&doc, selection).into_iter().find(|a| a.title.starts_with(prefix)) else {
        return Ok(false);
    };

    match provider.resolve_file_action(&doc, action) {
        Err(Error::Cancelled) => Ok(true),
        resolved => apply(project, Some((editor, doc.version)), &resolved?, plugin).map(|()| true),
    }
}

/// Creates the empty file `target` after an exact confirmation, then opens it.
pub fn create_empty(project: &Project, target: &str, title: &str, plugin: &str) -> Result<(), Error> {
    if project.exists(target)? {
        return Err(Error::Invalid(format!("{target} already exists; nothing was written.")));
    }

    if !prompts::confirm_exact(title, &format!("{target}\n\n(empty file)"))? {
        return Ok(());
    }

    let action = FileAction {
        operations: vec![FileOperation::Create { path: target.into(), text: String::new(), overwrite: false }],
        open: Some(target.into()),
        ..FileAction::default()
    };
    apply(project, None, &action, plugin)
}

fn apply_to_text(text: &mut String, edits: &[TextEdit]) -> Result<(), Error> {
    let mut spans = Vec::with_capacity(edits.len());
    for edit in edits {
        let start = text::offset(text, edit.range.start);
        let end = text::offset(text, edit.range.end);
        match (start, end) {
            (Some(a), Some(b)) if a <= b => spans.push((a, b, edit.text.as_str())),
            _ => return Err(Error::Invalid("An edit lies outside the new file.".into())),
        }
    }
    spans.sort_by_key(|(a, b, _)| (*a, *b));
    for (a, b, replacement) in spans.into_iter().rev() {
        text.replace_range(a..b, replacement);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{Position, Range};
    use openidea_plugin::mock::MockHost;

    fn create(path: &str, text: &str) -> FileOperation {
        FileOperation::Create { path: path.into(), text: text.into(), overwrite: false }
    }

    fn writable() -> MockHost {
        MockHost::install().with_capabilities(&["project.read", "project.write", "editor.read", "editor.write"])
    }

    fn run(host: &MockHost, action: &FileAction) -> Result<(), Error> {
        let invocation = host.invocation();
        apply(&invocation.project().unwrap(), invocation.editor().as_ref().map(|e| (e, e.document().unwrap().version)), action, "P")
    }

    #[test]
    fn create_writes_new_file_and_opens_it() {
        let host = writable();
        let action = FileAction {
            operations: vec![create("a/b.txt", "x")],
            open: Some("a/b.txt".into()),
            ..FileAction::default()
        };

        run(&host, &action).unwrap();

        let invocation = host.invocation();
        assert_eq!(invocation.project().unwrap().read_file("a/b.txt").unwrap(), "x");
        assert_eq!(host.opened_locations(), vec![("a/b.txt".to_string(), 0, 0)]);
    }

    #[test]
    fn create_refuses_existing_file() {
        let host = MockHost::install();
        host.add_file("a/b.txt", "old");
        let action = FileAction { operations: vec![create("a/b.txt", "x")], ..FileAction::default() };

        let result = run(&host, &action);

        assert!(matches!(result, Err(Error::Invalid(m)) if m.contains("already exists")));
        assert_eq!(host.invocation().project().unwrap().read_file("a/b.txt").unwrap(), "old");
    }

    #[test]
    fn edits_apply_to_open_editor() {
        let host = writable();
        host.open_editor("log.xml", "xml", "<a/>", &[]);
        let insert = TextEdit { range: Range::new(Position::new(0, 0), Position::new(0, 0)), text: "<!-- x -->".into() };
        let action = FileAction { edits: vec![("log.xml".into(), vec![insert])], ..FileAction::default() };

        run(&host, &action).unwrap();

        assert_eq!(host.editor_text(), "<!-- x --><a/>");
    }

    #[test]
    fn edits_fail_with_stale_when_the_document_changed_since_resolution() {
        let host = writable();
        host.open_editor("log.xml", "xml", "<a/>", &[]);
        let invocation = host.invocation();
        let editor = invocation.editor().unwrap();
        let resolved_version = editor.document().unwrap().version;
        host.type_in_editor("x");
        let insert = TextEdit { range: Range::new(Position::new(0, 0), Position::new(0, 0)), text: "<!-- x -->".into() };
        let action = FileAction { edits: vec![("log.xml".into(), vec![insert])], ..FileAction::default() };

        let result = apply(&invocation.project().unwrap(), Some((&editor, resolved_version)), &action, "P");

        assert!(matches!(result, Err(Error::Stale)), "{result:?}");
        assert!(!host.editor_text().contains("<!--"));
    }

    #[test]
    fn edits_for_created_file_are_applied_before_writing() {
        let host = writable();
        let insert = TextEdit { range: Range::new(Position::new(0, 1), Position::new(0, 1)), text: "Y".into() };
        let action = FileAction {
            operations: vec![create("n.txt", "ab")],
            edits: vec![("n.txt".into(), vec![insert])],
            ..FileAction::default()
        };

        run(&host, &action).unwrap();

        assert_eq!(host.invocation().project().unwrap().read_file("n.txt").unwrap(), "aYb");
    }

    #[test]
    fn denied_write_names_project_write() {
        let host = MockHost::install().with_capabilities(&["project.read"]);
        let action = FileAction { operations: vec![create("a.txt", "x")], ..FileAction::default() };

        let result = run(&host, &action);

        assert!(matches!(result, Err(Error::Invalid(m)) if m.contains("project.write") && m.contains("Settings › Plugins › P")));
    }

    struct Stub;

    impl FileActions for Stub {
        fn file_actions(&mut self, _doc: &Document, selection: Range) -> Vec<FileAction> {
            ["Other", "Make note…"]
                .map(|title| FileAction { title: title.into(), data: Some(format!("{}", selection.start.line)), ..FileAction::default() })
                .into()
        }

        fn resolve_file_action(&mut self, doc: &Document, action: FileAction) -> Result<FileAction, Error> {
            let text = format!("{} {} {}", doc.path, doc.version, action.data.unwrap_or_default());
            Ok(FileAction { operations: vec![create("note.txt", &text)], ..FileAction::default() })
        }
    }

    #[test]
    fn run_titled_resolves_and_applies_the_matching_action() {
        let host = writable();
        host.open_editor("/project/a.txt", "text", "", &[(2, 0, 2, 0)]);
        let invocation = host.invocation();

        let found = run_titled(&mut Stub, &invocation.project().unwrap(), &invocation.editor().unwrap(), "Make note", "P").unwrap();

        assert!(found);
        assert_eq!(invocation.project().unwrap().read_file("note.txt").unwrap(), "/project/a.txt 1 2");
    }

    #[test]
    fn run_titled_reports_a_missing_action() {
        let host = writable();
        host.open_editor("/project/a.txt", "text", "", &[]);
        let invocation = host.invocation();

        let found = run_titled(&mut Stub, &invocation.project().unwrap(), &invocation.editor().unwrap(), "Absent", "P").unwrap();

        assert!(!found);
    }

    #[test]
    fn run_titled_denied_editor_read_names_permission() {
        let host = MockHost::install().with_capabilities(&["project.read"]);
        host.open_editor("/project/a.txt", "text", "", &[]);
        let invocation = host.invocation();

        let result = run_titled(&mut Stub, &invocation.project().unwrap(), &invocation.editor().unwrap(), "Make note", "P");

        assert!(matches!(result, Err(Error::Invalid(m)) if m.contains("editor.read")));
    }

    #[test]
    fn create_empty_writes_after_exact_confirmation() {
        let host = writable();
        host.answer_next_prompt(Some(&[]));

        create_empty(&host.invocation().project().unwrap(), "/project/.gitignore", "Create?", "P").unwrap();

        assert_eq!(host.invocation().project().unwrap().read_file(".gitignore").unwrap(), "");
        assert_eq!(host.opened_locations(), vec![("/project/.gitignore".to_string(), 0, 0)]);
    }

    #[test]
    fn create_empty_writes_nothing_when_cancelled() {
        let host = writable();
        host.answer_next_prompt(None);

        create_empty(&host.invocation().project().unwrap(), "/project/.gitignore", "Create?", "P").unwrap();

        assert!(!host.invocation().project().unwrap().exists(".gitignore").unwrap());
    }

    #[test]
    fn create_empty_refuses_existing_before_confirming() {
        let host = writable();
        host.add_file(".gitignore", "old");

        let result = create_empty(&host.invocation().project().unwrap(), "/project/.gitignore", "Create?", "P");

        assert!(matches!(result, Err(Error::Invalid(m)) if m.contains("already exists")));
        assert_eq!(host.invocation().project().unwrap().read_file(".gitignore").unwrap(), "old");
    }

    #[test]
    fn rename_is_refused_without_side_effects() {
        let host = MockHost::install();
        let action = FileAction {
            operations: vec![create("a.txt", "x"), FileOperation::Rename { from: "a".into(), to: "b".into() }],
            ..FileAction::default()
        };

        let result = run(&host, &action);

        assert!(matches!(result, Err(Error::Invalid(m)) if m.contains("⌥⏎")));
        assert!(host.invocation().project().unwrap().read_file("a.txt").is_err());
    }
}
