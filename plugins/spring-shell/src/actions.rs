//! One exact annotation insertion; never an import guess or framework migration.
use super::*;
use openidea_plugin::file_actions::{FileAction, FileActions};
use openidea_plugin::{Error, host};
use std::collections::BTreeMap;
use std::hash::BuildHasher;
const ANNOTATION: &str = "@org.springframework.shell.standard.ShellComponent";
struct Pending {
    root: String,
    doc: Document,
    class_at: usize,
    start: usize,
}
#[derive(Default)]
pub(crate) struct State {
    pending: BTreeMap<String, Pending>,
}
pub(super) fn root(doc: &Document) -> Option<String> {
    spring_text::index::owning_root(&doc.path)
}
fn candidates(doc: &Document, selection: Range, extended: &dyn Fn(&str) -> bool) -> Vec<(usize, usize)> {
    let Some(f) = document_model(doc) else {
        return Vec::new();
    };
    if f.types.iter().any(|t| {
        t.name == "org" || {
            let tokens = java::tokens(&doc.text[t.at + t.name.len()..t.end]);
            tokens.first().is_some_and(|t| t.s == "<") && tokens.iter().take_while(|t| t.s != ">").any(|t| t.s == "org")
        }
    }) || java::tokens(&doc.text).windows(2).any(|w| w[0].s == "typealias" && w[1].s == "org")
        || f.imports.iter().any(|i| i.path.ends_with(".*") || i.path.rsplit('.').next() == Some("org"))
    {
        return Vec::new();
    }
    f.types
        .iter()
        .enumerate()
        .filter(|(ti, t)| {
            t.kind == "class"
                && t.outer.is_none()
                && t.annotations.is_empty()
                && t.supers().next().is_none()
                && !t.modifiers.iter().any(|m| m == "abstract")
                && !extended(&f.qualified(*ti))
        })
        .map(|(_, t)| t)
        .filter(|t| {
            t.methods().any(|m| {
                m.annotation("ShellMethod").is_some_and(|a| rules::supported(&f, &doc.text, a))
                    && (overlap(range(&doc.text, m.at, m.at + m.name.len()), selection)
                        || overlap(range(&doc.text, t.at, t.at + t.name.len()), selection))
            })
        })
        .map(|t| (t.at, t.start))
        .collect()
}
fn overlap(a: Range, b: Range) -> bool {
    a.start <= b.end && b.start <= a.end
}
impl FileActions for ShellTools {
    fn file_actions(&mut self, doc: &Document, selection: Range) -> Vec<FileAction> {
        let Some(root) = root(doc) else {
            return Vec::new();
        };
        let cached = self.indexes.cached(&doc.path);
        let candidates = candidates(doc, selection, &|qualified| cached.is_none_or(|index| may_be_extended(index, qualified)));
        if self.actions.pending.len() + candidates.len() > 8 {
            self.actions.pending.clear();
        }
        let mut actions:Vec<_> = candidates
            .into_iter()
            .take(8)
            .map(|(class_at, start)| {
                let random = std::collections::hash_map::RandomState::new();
                let token = format!("shell-component:{:016x}{:016x}", random.hash_one(0u8), random.hash_one(1u8));
                self.actions.pending.insert(token.clone(), Pending { root: root.clone(), doc: doc.clone(), class_at, start });
                FileAction {
                    title: "Add @ShellComponent…".into(),
                    kind: Some("quickfix".into()),
                    data: Some(token),
                    ..FileAction::default()
                }
            })
            .collect();
        actions.extend(self.command_rename_actions(doc,selection));
        actions
    }
    fn resolve_file_action(&mut self, doc: &Document, action: FileAction) -> Result<FileAction, Error> {
        if action.data.as_deref().is_some_and(|s|s.starts_with("shell-command:")) { return self.resolve_command_rename(doc,action); }
        let token = action.data.as_deref().ok_or(Error::Stale)?;
        let p = self.actions.pending.get(token).ok_or(Error::Stale)?;
        if doc.path != p.doc.path
            || doc.language != p.doc.language
            || doc.version != p.doc.version
            || doc.text != p.doc.text
            || root(doc).as_deref() != Some(&p.root)
        {
            return Err(Error::Stale);
        }
        let pos = position(&doc.text, p.class_at);
        let cached = self.indexes.cached(&doc.path);
        let still_offered = candidates(doc, Range::new(pos, pos), &|qualified| cached.is_none_or(|index| may_be_extended(index, qualified)));
        if !still_offered.contains(&(p.class_at, p.start)) {
            return Err(Error::Stale);
        }
        let project = host::project_of(&doc.path).ok_or(Error::Stale)?;
        if project.read_file(&doc.path)? != doc.text {
            return Err(Error::Invalid("Save the file first".into()));
        }
        let line = doc.text[..p.start].rfind('\n').map_or(0, |i| i + 1);
        let indent = &doc.text[line..p.start];
        let indent = if indent.chars().all(char::is_whitespace) { indent } else { "" };
        let newline = if doc.text[p.start..].split_once('\n').is_some_and(|(line, _)| line.ends_with('\r')) { "\r\n" } else { "\n" };
        let text = format!("{ANNOTATION}{newline}{indent}");
        let at = position(&doc.text, p.start);
        let body = format!(
            "File: {}\nDocument version: {}\nInsert at line {}, UTF-16 column {}:\n\n{text}\n\nExact change: one annotation insertion; all original bytes remain unchanged. No imports or names are rewritten. Bean scanning, construction, classpath and runtime registration remain unverified.",
            doc.path,
            doc.version,
            at.line + 1,
            at.character + 1
        );
        if !ui_kit::prompts::confirm_exact("Add explicit ShellComponent?", &body)? {
            return Err(Error::Cancelled);
        }
        if root(doc).as_deref() != Some(&p.root) || project.read_file(&doc.path)? != doc.text {
            return Err(Error::Stale);
        }
        self.actions.pending.remove(token);
        Ok(FileAction {
            title: "Add explicit ShellComponent annotation".into(),
            kind: Some("quickfix".into()),
            edits: vec![(doc.path.clone(), vec![openidea_plugin::TextEdit { range: Range::new(at, at), text }])],
            ..FileAction::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    const SOURCE: &str =
        "import org.springframework.shell.standard.ShellMethod;\r\n/*😀*/ public class C {\r\n @ShellMethod(\"Run\") void run(){}\r\n}\r\n";
    fn setup() -> (MockHost, ShellTools, Document) {
        let host = MockHost::install();
        host.add_file("C.java", SOURCE);
        (host, ShellTools::default(), Document { path: "/project/C.java".into(), language: "java".into(), version: 5, text: SOURCE.into() })
    }
    fn action(tools: &mut ShellTools, d: &Document) -> FileAction {
        let p = position(&d.text, d.text.find("run(){}").unwrap());
        tools.line_markers(d);
        tools.file_actions(d, Range::new(p, p)).remove(0)
    }
    #[test]
    fn exact_fully_qualified_annotation_is_one_versioned_crlf_edit_with_no_write() {
        let (host, mut tools, d) = setup();
        let pending = action(&mut tools, &d);
        let replay = pending.clone();
        host.answer_next_prompt(Some(&["yes"]));
        let resolved = tools.resolve_file_action(&d, pending).unwrap();
        assert!(resolved.operations.is_empty());
        assert_eq!(resolved.edits.len(), 1);
        let edit = &resolved.edits[0].1[0];
        let at = offset(&d.text, edit.range.start).unwrap();
        assert_eq!(at, offset(&d.text, edit.range.end).unwrap());
        let mut after = d.text.clone();
        after.insert_str(at, &edit.text);
        assert!(after.contains(&format!("{ANNOTATION}\r\npublic class C")), "{after}");
        assert_eq!(host::project_of(&d.path).unwrap().read_file(&d.path).unwrap(), SOURCE);
        assert!(tools.resolve_file_action(&d, replay).is_err());
        assert_eq!(host.fetches(), 0);
        assert!(host.process_runs().is_empty());
    }
    #[test]
    fn cancel_stale_version_text_root_and_replayed_session_return_no_edit() {
        for mode in 0..4 {
            let (host, mut tools, mut d) = setup();
            let pending = action(&mut tools, &d);
            match mode {
                0 => host.answer_next_prompt(None),
                1 => d.version += 1,
                2 => d.text.push(' '),
                _ => host.set_project_root(Some("/other")),
            }
            assert!(tools.resolve_file_action(&d, pending).is_err());
        }
    }
    #[test]
    fn inherited_annotated_nested_foreign_and_wildcard_classes_offer_no_component_fix() {
        for source in [
            SOURCE.replace("public class C", "@Custom public class C"),
            SOURCE.replace("public class C", "public class C extends Unknown"),
            SOURCE.replace("public class C", "class Outer { public class C") + "}",
            SOURCE.replace("org.springframework.shell.standard.ShellMethod", "custom.ShellMethod"),
            SOURCE.replace("import org.springframework.shell.standard.ShellMethod;", "import org.springframework.shell.standard.*;"),
        ] {
            let (_host, mut tools, mut d) = setup();
            d.text = source;
            tools.line_markers(&d);
            let p = position(&d.text, d.text.find("run(){}").unwrap());
            assert!(tools.file_actions(&d, Range::new(p, p)).is_empty(), "{}", d.text);
        }
    }
    #[test]
    fn kotlin_fix_preserves_current_source_and_known_org_type_parameter_blocks_qualification() {
        let (host, mut tools, mut d) = setup();
        d.path = "/project/C.kt".into();
        d.language = "kotlin".into();
        d.text = "import org.springframework.shell.standard.ShellMethod\n// 😀\nclass C { @ShellMethod(\"Run\") fun run(){} }".into();
        host.add_file("C.kt", &d.text);
        tools.line_markers(&d);
        let p = position(&d.text, d.text.find("run(){}").unwrap());
        let pending = tools.file_actions(&d, Range::new(p, p)).remove(0);
        host.answer_next_prompt(Some(&["yes"]));
        let resolved = tools.resolve_file_action(&d, pending).unwrap();
        let edit = &resolved.edits[0].1[0];
        assert_eq!(edit.text, format!("{ANNOTATION}\n"));
        assert_eq!(edit.range.start, position(&d.text, d.text.find("class C").unwrap()));
        d.text = d.text.replace("class C", "class C<org>");
        let p = position(&d.text, d.text.find("run(){}").unwrap());
        assert!(tools.file_actions(&d, Range::new(p, p)).is_empty());
    }
    #[test]
    fn fix_on_unsaved_buffer_says_save_first() {
        let (_host, mut tools, mut d) = setup();
        d.text.push_str("\r\n// unsaved");
        let pending = action(&mut tools, &d);
        let result = tools.resolve_file_action(&d, pending);
        assert!(matches!(&result, Err(Error::Invalid(m)) if m == "Save the file first"), "{result:?}");
    }
    #[test]
    fn cold_index_offers_no_component_fix() {
        let (_host, mut tools, d) = setup();
        let p = position(&d.text, d.text.find("run(){}").unwrap());
        assert!(tools.file_actions(&d, Range::new(p, p)).is_empty());
    }
    #[test]
    fn windows_root_fixes_available() {
        let host = MockHost::install();
        host.set_project_root(Some("C:/work/app"));
        host.add_file("C.java", SOURCE);
        let d = Document { path: "C:/work/app/C.java".into(), language: "java".into(), version: 5, text: SOURCE.into() };
        let mut tools = ShellTools::default();
        tools.line_markers(&d);
        let p = position(&d.text, d.text.find("run(){}").unwrap());
        assert!(!tools.file_actions(&d, Range::new(p, p)).is_empty());
    }
}
