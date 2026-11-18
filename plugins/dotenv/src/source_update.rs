//! Explicit current-file token rewrite; never a project-wide Rename graph.
use super::*;
use openidea_plugin::file_actions::{FileAction, FileActions};
const MAX_TEXT: usize = 12 << 10;
const MAX_EDITS: usize = 128;
#[derive(Clone)]
struct Pending {
    root: String,
    doc: Document,
    line: u32,
    edits: Vec<TextEdit>,
}
#[derive(Default)]
pub(crate) struct Updates {
    next: u64,
    pending: BTreeMap<String, Pending>,
}
fn literal(key: &str) -> bool {
    let mut bytes = key.bytes();
    key.len() <= 128
        && bytes.next().is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}
fn path(path: &str) -> bool {
    path.starts_with('/')
        && path.len() <= 512
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
        && !path.split('/').any(|s| s == "." || s == "..")
}
fn edits(plugin: &DotEnv, doc: &Document, line: u32) -> Option<(String, String, Vec<TextEdit>)> {
    if doc.language != "dotenv" || !is_env_file(&doc.path) || !path(&doc.path) || doc.text.len() > MAX_TEXT {
        return None;
    }
    let (root, _) = project_of(&doc.path)?;
    let index = plugin.projects.get(&root)?;
    if index.declarations.len() > 8192 {
        return None;
    }
    let properties = parse::properties(&doc.text, &parse::lex(&doc.text));
    if properties.len() > 256 {
        return None;
    }
    let renamed = properties.iter().find(|p| range(&doc.text, p.key.clone()).start.line == line)?;
    let new = renamed.name(&doc.text);
    let saved: Vec<_> = index.declarations.iter().filter(|d| d.location.path == doc.path).collect();
    let old = saved.iter().find(|d| d.location.range.start.line == line)?;
    if !literal(&new)
        || !literal(&old.key)
        || old.key == new
        || old.value != renamed.short_value()
        || saved.len() != properties.len()
        || properties.iter().any(|p| p.name(&doc.text) == old.key)
        || properties.iter().filter(|p| p.name(&doc.text) == new).count() != 1
    {
        return None;
    }
    let usages = nested_usages(&doc.path, &doc.text);
    let edits: Vec<_> =
        usages.into_iter().filter(|u| u.key == old.key).map(|u| TextEdit { range: u.location.range, text: new.clone() }).collect();
    if edits.is_empty() || edits.len() > MAX_EDITS {
        return None;
    }
    Some((old.key.clone(), new, edits))
}
impl FileActions for DotEnv {
    fn file_actions(&mut self, doc: &Document, selection: Range) -> Vec<FileAction> {
        let Some((old, new, edits)) = edits(self, doc, selection.start.line) else { return Vec::new() };
        let Some((root, _)) = project_of(&doc.path) else { return Vec::new() };
        self.updates.pending.retain(|_, pending| {
            pending.doc.path != doc.path
                || (pending.root == root
                    && pending.doc.version == doc.version
                    && pending.doc.text == doc.text
                    && pending.doc.language == doc.language)
        });
        if let Some((token, _)) = self
            .updates
            .pending
            .iter()
            .find(|(_, p)| p.root == root && p.doc.path == doc.path && p.doc.version == doc.version && p.line == selection.start.line)
        {
            return vec![FileAction {
                title: format!("Update ${{{old}}} → ${{{new}}} in this file…"),
                kind: Some("refactor.rewrite".into()),
                data: Some(token.clone()),
                ..Default::default()
            }];
        }
        if self.updates.pending.len() >= 8 {
            return Vec::new();
        }
        let Some(next) = self.updates.next.checked_add(1) else { return Vec::new() };
        self.updates.next = next;
        let token = format!("source-update-{next}");
        self.updates.pending.insert(token.clone(), Pending { root, doc: doc.clone(), line: selection.start.line, edits });
        vec![FileAction {
            title: format!("Update ${{{old}}} → ${{{new}}} in this file…"),
            kind: Some("refactor.rewrite".into()),
            data: Some(token),
            ..Default::default()
        }]
    }
    fn resolve_file_action(&mut self, doc: &Document, action: FileAction) -> Result<FileAction, Error> {
        let token = action.data.as_deref().ok_or(Error::Stale)?;
        let pending = self.updates.pending.remove(token).ok_or(Error::Stale)?;
        if doc.path != pending.doc.path
            || doc.version != pending.doc.version
            || doc.text != pending.doc.text
            || doc.language != pending.doc.language
        {
            return Err(Error::Stale);
        }
        let verify = |plugin: &DotEnv| -> Result<(), Error> {
            let (root, project) = project_of(&doc.path).ok_or(Error::Stale)?;
            if root != pending.root
                || project.read_file(&doc.path)? != doc.text
                || edits(plugin, doc, pending.line).map(|p| p.2) != Some(pending.edits.clone())
            {
                return Err(Error::Stale);
            }
            Ok(())
        };
        verify(self)?;
        let old = edits(self, doc, pending.line).ok_or(Error::Stale)?.0;
        let mut preview = format!(
            "Current-file source rewrite only: {:?}\nThe previous declaration name is inferred from a cached same-line/value observation. This is not semantic Rename; external files, Compose services and runtime environments are unchanged.\nExact token replacements:\n",
            doc.path
        );
        for edit in &pending.edits {
            preview.push_str(&format!(
                "{}:{}–{}:{}: {} -> {}\n",
                edit.range.start.line + 1,
                edit.range.start.character + 1,
                edit.range.end.line + 1,
                edit.range.end.character + 1,
                old,
                edit.text
            ));
        }
        if !ui_kit::prompts::confirm_exact("Update references in this file?", &preview)? {
            return Err(Error::Cancelled);
        }
        verify(self)?;
        Ok(FileAction {
            title: "Update references in this file".into(),
            kind: Some("refactor.rewrite".into()),
            edits: vec![(doc.path.clone(), pending.edits)],
            ..Default::default()
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    fn setup() -> (MockHost, DotEnv, Document) {
        let host = MockHost::install().with_capabilities(&["project.read"]);
        host.set_project_root(Some("/project"));
        let mut plugin = DotEnv::default();
        let mut index = Index::default();
        index.declarations = env_declarations("/project/.env", "OLD=private-value\r\nUSE=\"😀${OLD}${OLD}\"\r\n");
        plugin.projects.insert("/project".into(), index);
        let doc = Document {
            path: "/project/.env".into(),
            language: "dotenv".into(),
            version: 9,
            text: "NEW=private-value\r\nUSE=\"😀${OLD}${OLD}\"\r\n".into(),
        };
        host.add_file(".env", &doc.text);
        host.add_file("app.py", "os.getenv('OLD')");
        (host, plugin, doc)
    }
    fn offer(plugin: &mut DotEnv, doc: &Document) -> FileAction {
        plugin.file_actions(doc, Range::new(Position::new(0, 1), Position::new(0, 1))).pop().unwrap()
    }
    #[test]
    fn exact_atomic_current_buffer_tokens_no_external_edits_or_secret_preview() {
        let (host, mut plugin, doc) = setup();
        host.answer_next_prompt(Some(&[]));
        let action = offer(&mut plugin, &doc);
        assert!(action.edits.is_empty());
        let resolved = plugin.resolve_file_action(&doc, action).unwrap();
        assert!(resolved.operations.is_empty());
        assert_eq!(resolved.edits.len(), 1);
        assert_eq!(resolved.edits[0].0, doc.path);
        assert_eq!(resolved.edits[0].1.len(), 2);
        let first = &resolved.edits[0].1[0];
        assert_eq!(first.range, Range::new(Position::new(1, 9), Position::new(1, 12)));
        assert_eq!(first.text, "NEW");
        let mut applied = doc.text.clone();
        for edit in resolved.edits[0].1.iter().rev() {
            let from = text::offset(&applied, edit.range.start).unwrap();
            let to = text::offset(&applied, edit.range.end).unwrap();
            applied.replace_range(from..to, &edit.text);
        }
        assert_eq!(applied, doc.text.replace("${OLD}", "${NEW}"));
        assert_eq!(applied.matches("\r\n").count(), 2);
        assert_eq!(host.invocation().project().unwrap().read_file("/project/app.py").unwrap(), "os.getenv('OLD')");
    }
    #[test]
    fn cancel_is_single_use_and_has_no_eager_edits() {
        let (host, mut p, d) = setup();
        host.answer_next_prompt(None);
        let a = offer(&mut p, &d);
        assert!(matches!(p.resolve_file_action(&d, a.clone()), Err(Error::Cancelled)));
        assert!(matches!(p.resolve_file_action(&d, a), Err(Error::Stale)));
    }
    #[test]
    fn version_root_or_newer_buffer_rejects_before_confirmation() {
        for change in ["version", "root", "buffer"] {
            let (host, mut p, mut d) = setup();
            let a = offer(&mut p, &d);
            match change {
                "version" => d.version += 1,
                "root" => host.set_project_root(Some("/other")),
                _ => host.add_file(".env", "LATER=secret"),
            };
            assert!(p.resolve_file_action(&d, a).is_err(), "{change}");
        }
    }
    #[test]
    fn token_cache_and_pending_caps_and_forged_operations_are_bounded() {
        let (host, mut p, d) = setup();
        let mut a = offer(&mut p, &d);
        a.operations.push(openidea_plugin::file_actions::FileOperation::Delete { path: "/project/app.py".into() });
        a.open = Some("/project/app.py".into());
        host.answer_next_prompt(Some(&[]));
        let resolved = p.resolve_file_action(&d, a).unwrap();
        assert!(resolved.operations.is_empty());
        assert!(resolved.open.is_none());
        for _ in 0..8 {
            offer(&mut p, &d);
        }
        assert_eq!(p.updates.pending.len(), 1, "repeat provider requests reuse one ticket");
        let pending = p.updates.pending.values().next().unwrap().clone();
        p.updates.pending.clear();
        for i in 0..8 {
            let mut other = pending.clone();
            other.doc.path = format!("/project/.env.{i}");
            p.updates.pending.insert(format!("other-{i}"), other);
        }
        assert!(p.file_actions(&d, Range::default()).is_empty());
        let (_host, mut p, mut d) = setup();
        d.text = format!("NEW=private-value\nUSE=\"{}\"", "${OLD}".repeat(MAX_EDITS + 1));
        assert!(p.file_actions(&d, Range::default()).is_empty());
        let (_host, mut p, d) = setup();
        let declarations = &mut p.projects.get_mut("/project").unwrap().declarations;
        declarations.resize(8193, declarations[0].clone());
        assert!(p.file_actions(&d, Range::default()).is_empty());
    }
    #[test]
    fn uncertain_names_duplicates_missing_history_and_caps_are_quiet() {
        for text in [
            "NEW=private-value\nNEW=duplicate\nUSE=${OLD}",
            "NEW=changed\nUSE=${OLD}",
            "NEW-NAME=private-value\nUSE=${OLD}",
            "NEW=private-value\nOLD=still-owned\nUSE=${OLD}",
        ] {
            let (_host, mut p, mut d) = setup();
            d.text = text.into();
            assert!(p.file_actions(&d, Range::default()).is_empty());
        }
        let (_host, mut p, mut d) = setup();
        p.projects.clear();
        assert!(p.file_actions(&d, Range::default()).is_empty());
        d.text = "x".repeat(MAX_TEXT + 1);
        assert!(p.file_actions(&d, Range::default()).is_empty());
    }
}
