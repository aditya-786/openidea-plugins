//! API1.4 read-only native highlights/lenses. DocumentIndexer string anchors
//! cannot opt out of unchecked host Rename, so they are not exported.
use super::*;
use openidea_plugin::language_features::{CodeLens, Highlight, LanguageFeatures};
use std::collections::BTreeMap;
use std::hash::BuildHasher;
#[derive(Clone)]
struct Snapshot {
    root: String,
    doc: Document,
    definitions: Vec<(String, Range)>,
}
#[derive(Default)]
pub(super) struct State {
    /// The newest snapshot of each document, with its token.
    snapshots: BTreeMap<String, (String, Snapshot)>,
}
fn edges(doc: &Document) -> openidea_plugin::indexing::Edges {
    if !((doc.language == "java" && doc.path.ends_with(".java"))
        || (doc.language == "kotlin" && doc.path.ends_with(".kt")))
    {
        return Default::default();
    }
    command_rename::public_edges(&openidea_plugin::indexing::Document {
        path: doc.path.clone(),
        text: doc.text.clone(),
        symbols: vec![],
    })
}
impl LanguageFeatures for ShellTools {
    fn document_highlights(&mut self, doc: &Document, at: Position) -> Vec<Highlight> {
        let edges = edges(doc);
        let Some(selected) = edges
            .definitions
            .iter()
            .chain(&edges.references)
            .find(|a| a.range.start <= at && at < a.range.end)
        else {
            return vec![];
        };
        let mut highlights = edges
            .definitions
            .iter()
            .filter(|a| a.key == selected.key)
            .map(|a| Highlight {
                range: a.range,
                kind: "write".into(),
            })
            .chain(
                edges
                    .references
                    .iter()
                    .filter(|a| a.key == selected.key)
                    .map(|a| Highlight {
                        range: a.range,
                        kind: "read".into(),
                    }),
            )
            .collect::<Vec<_>>();
        highlights.sort_by_key(|h| (h.range.start.line, h.range.start.character));
        highlights.dedup_by(|a, b| a.range == b.range);
        highlights
    }
    fn code_lenses(&mut self, doc: &Document) -> Vec<CodeLens> {
        let Some(root) = actions::root(doc) else {
            return vec![];
        };
        let edges = edges(doc);
        let mut definitions = edges
            .definitions
            .iter()
            .map(|a| (a.key.clone(), a.range))
            .take(64)
            .collect::<Vec<_>>();
        definitions.sort_by_key(|(_, r)| (r.start.line, r.start.character));
        if definitions.is_empty() {
            return vec![];
        }
        self.native_occurrences.snapshots.remove(&doc.path);
        if self.native_occurrences.snapshots.len() >= 64 {
            self.native_occurrences.snapshots.clear();
        }
        let random = std::collections::hash_map::RandomState::new();
        let token = format!(
            "native-shell:{:016x}{:016x}",
            random.hash_one(0u8),
            random.hash_one(1u8)
        );
        let lenses: Vec<_> = definitions
            .iter()
            .enumerate()
            .filter_map(|(i, (key, range))| {
                let count = edges
                    .definitions
                    .iter()
                    .chain(&edges.references)
                    .filter(|a| a.key == *key)
                    .count();
                (count > 1).then(|| CodeLens {
                    range: *range,
                    title: format!("{count} local occurrences"),
                    command: "command-occurrences".into(),
                    argument: Some(format!("{token}:{i}")),
                })
            })
            .collect();
        if lenses.is_empty() {
            return lenses;
        }
        self.native_occurrences.snapshots.insert(
            doc.path.clone(),
            (token, Snapshot {
                root,
                doc: doc.clone(),
                definitions,
            }),
        );
        lenses
    }
}
impl ShellTools {
    pub(super) fn native_occurrences(
        &mut self,
        ctx: &Invocation,
        arg: &str,
    ) -> Result<Option<String>, Error> {
        let (token, index) = arg.rsplit_once(':').ok_or(Error::Stale)?;
        let index = index.parse::<usize>().map_err(|_| Error::Stale)?;
        let snapshot = self
            .native_occurrences
            .snapshots
            .values()
            .find(|(own, _)| own == token)
            .map(|(_, snapshot)| snapshot)
            .ok_or(Error::Stale)?;
        let (key, span) = snapshot.definitions.get(index).ok_or(Error::Stale)?;
        let editor = ctx.editor().ok_or(Error::Stale)?;
        let current = editor.document()?;
        if editor.path() != snapshot.doc.path
            || current.text != snapshot.doc.text
            || current.version != snapshot.doc.version
            || ctx.project().ok_or(Error::Stale)?.root()? != snapshot.root
            || actions::root(&snapshot.doc).as_deref() != Some(&snapshot.root)
        {
            return Err(Error::Stale);
        }
        let current_edges = edges(&snapshot.doc);
        if !current_edges
            .definitions
            .iter()
            .any(|a| a.key == *key && a.range == *span)
        {
            return Err(Error::Stale);
        }
        command_rename::occurrences(ctx, Some(span.start))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{
        mock::MockHost,
        text::{offset, position},
    };
    const SOURCE: &str = "import org.springframework.shell.standard.ShellComponent; import org.springframework.shell.standard.ShellMethod; import org.springframework.shell.standard.ShellMethodAvailability; @ShellComponent class C { @ShellMethod(key={\"run task\",\"rt\"}) @ShellMethodAvailability(\"isReady\") void execute(){} Availability isReady(){return null;} @ShellMethodAvailability({\"run task\",\"rt\"}) Availability gate(){return null;} }";
    fn doc() -> Document {
        Document {
            path: "/project/C.java".into(),
            language: "java".into(),
            text: SOURCE.into(),
            version: 1,
        }
    }
    fn at(text: &str, needle: &str) -> Position {
        position(text, text.find(needle).unwrap())
    }
    #[test]
    fn native_highlights_keep_aliases_command_selectors_and_indicator_methods_separate() {
        let h = MockHost::install().with_capabilities(&[]);
        let d = doc();
        let mut nav = ShellTools::default();
        for (needle, expected) in [
            ("run task", "run task"),
            ("rt\"", "rt"),
            ("isReady\"", "isReady"),
        ] {
            let highlights = nav.document_highlights(&d, at(SOURCE, needle));
            assert_eq!(highlights.len(), 2);
            assert_eq!(highlights.iter().filter(|h| h.kind == "write").count(), 1);
            assert_eq!(highlights.iter().filter(|h| h.kind == "read").count(), 1);
            for highlight in highlights {
                assert_eq!(
                    &SOURCE[offset(SOURCE, highlight.range.start).unwrap()
                        ..offset(SOURCE, highlight.range.end).unwrap()],
                    expected
                );
            }
        }
        assert!(h.requests().is_empty() && h.process_runs().is_empty());
        let wrong = Document {
            language: "kotlin".into(),
            ..d
        };
        assert!(
            nav.document_highlights(&wrong, at(SOURCE, "run task"))
                .is_empty()
        );
    }
    #[test]
    fn native_lens_routes_its_identity_independently_of_caret_and_rejects_stale_context() {
        let h = MockHost::install();
        let d = doc();
        h.open_editor(&d.path, "java", SOURCE, &[(0, 0, 0, 0)]);
        let mut nav = ShellTools::default();
        let lenses = nav.code_lenses(&d);
        assert_eq!(lenses.len(), 3);
        assert!(lenses.iter().all(|l| l.title == "2 local occurrences"));
        let lens = lenses
            .iter()
            .find(|l| {
                &SOURCE
                    [offset(SOURCE, l.range.start).unwrap()..offset(SOURCE, l.range.end).unwrap()]
                    == "run task"
            })
            .unwrap();
        h.answer_next_prompt(Some(&["1"]));
        nav.run_command(&lens.command, &h.invocation(), lens.argument.as_deref())
            .unwrap();
        assert_eq!(
            h.opened_locations().last().unwrap().2,
            position(SOURCE, SOURCE.rfind("run task").unwrap()).character
        );
        h.open_editor(
            &d.path,
            "java",
            &SOURCE.replace("run task", "other task"),
            &[(0, 0, 0, 0)],
        );
        assert!(
            nav.run_command(&lens.command, &h.invocation(), lens.argument.as_deref())
                .is_err()
        );
        assert!(
            ShellTools::default()
                .run_command(&lens.command, &h.invocation(), lens.argument.as_deref())
                .is_err()
        );
        assert!(h.requests().is_empty() && h.process_runs().is_empty());
    }
    #[test]
    fn lens_in_other_file_not_stale() {
        let h = MockHost::install();
        let d = doc();
        let mut nav = ShellTools::default();
        let lens = nav.code_lenses(&d).remove(0);
        for i in 0..12 {
            nav.code_lenses(&Document { path: format!("/project/Other{i}.java"), ..doc() });
        }
        h.open_editor(&d.path, "java", SOURCE, &[(0, 0, 0, 0)]);
        h.answer_next_prompt(Some(&["1"]));
        let result = nav.run_command(&lens.command, &h.invocation(), lens.argument.as_deref());
        assert!(!matches!(result, Err(Error::Stale)), "{result:?}");
    }
    #[test]
    fn single_occurrence_gets_no_lens() {
        let _h = MockHost::install();
        let text = SOURCE.replace(" @ShellMethodAvailability({\"run task\",\"rt\"}) Availability gate(){return null;}", "")
            .replace("@ShellMethodAvailability(\"isReady\") ", "");
        let lenses = ShellTools::default().code_lenses(&Document { text, ..doc() });
        assert!(lenses.is_empty(), "{lenses:?}");
    }
    #[test]
    fn native_surfaces_withhold_escaped_foreign_ambiguous_and_inherited_scopes() {
        let _h = MockHost::install();
        let mut nav = ShellTools::default();
        for text in [
            SOURCE.replace("run task", "run\\u0020task"),
            SOURCE.replace("class C", "class C extends Other"),
            SOURCE.replace(
                "key={\"run task\",\"rt\"}",
                "key={\"run task\",\"run task\"}",
            ),
            SOURCE.replace(
                "import org.springframework.shell.standard.ShellMethod;",
                "import foreign.ShellMethod;",
            ),
        ] {
            let d = Document { text, ..doc() };
            assert!(nav.code_lenses(&d).is_empty());
            assert!(nav.document_highlights(&d, Position::default()).is_empty());
        }
        assert!(!include_str!("../plugin.toml").contains("contributes.documentIndexers"));
    }
}

#[cfg(test)]
mod kotlin_identity_tests {
    use super::*;
    use openidea_plugin::{
        mock::MockHost,
        text::{offset, position},
    };
    #[test]
    fn implicit_kotlin_commands_highlight_transformed_names_without_exposing_raw_rename() {
        let _h = MockHost::install().with_capabilities(&[]);
        let text = "import org.springframework.shell.standard.ShellComponent\nimport org.springframework.shell.standard.ShellMethod\nimport org.springframework.shell.standard.ShellMethodAvailability\n/*😀*/\r\n@ShellComponent class K { @ShellMethod(value=\"Run\") fun runTask(){} @ShellMethodAvailability(\"run-task\") fun ready(): Availability {return Availability.available()} }";
        let doc = Document {
            path: "/project/K.kt".into(),
            language: "kotlin".into(),
            text: text.into(),
            version: 1,
        };
        let values = ShellTools::default()
            .document_highlights(&doc, position(text, text.find("runTask").unwrap()));
        assert_eq!(values.len(), 2);
        let names = values
            .iter()
            .map(|v| {
                &text[offset(text, v.range.start).unwrap()..offset(text, v.range.end).unwrap()]
            })
            .collect::<Vec<_>>();
        assert_eq!(names, ["runTask", "run-task"]);
        assert!(!include_str!("../plugin.toml").contains("contributes.documentIndexers"));
    }
    #[test]
    fn lens_tokens_do_not_cross_paths_or_project_ownership() {
        let h = MockHost::install();
        let text = "import org.springframework.shell.standard.ShellComponent; import org.springframework.shell.standard.ShellMethod; import org.springframework.shell.standard.ShellMethodAvailability; @ShellComponent class C { @ShellMethod(key=\"run\") void run(){} @ShellMethodAvailability(\"run\") Availability g(){return null;} }";
        let doc = Document {
            path: "/project/C.java".into(),
            language: "java".into(),
            text: text.into(),
            version: 1,
        };
        let mut nav = ShellTools::default();
        let lens = nav.code_lenses(&doc).remove(0);
        h.open_editor("/project/Other.java", "java", text, &[(0, 0, 0, 0)]);
        assert!(
            nav.run_command(&lens.command, &h.invocation(), lens.argument.as_deref())
                .is_err()
        );
        h.open_editor(&doc.path, "java", text, &[(0, 0, 0, 0)]);
        h.add_project("/project/nested");
        h.set_project_root(Some("/project/nested"));
        assert!(
            nav.run_command(&lens.command, &h.invocation(), lens.argument.as_deref())
                .is_err()
        );
        assert!(h.opened_locations().is_empty());
    }
}
