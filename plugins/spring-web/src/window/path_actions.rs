//! Reviewed route-template/explicit-binding rename. Never a Java symbol Rename.
use super::*;
use crate::{MAPPINGS, literal_paths, range, variables};
use openidea_plugin::{TextEdit, file_actions::FileAction, text::offset};
#[derive(Default)]
pub(super) struct State {
    next: u64,
    pending: BTreeMap<String, Pending>,
}
#[derive(Clone)]
struct Pending {
    doc: Document,
    root: String,
    binding: Binding,
}
#[derive(Clone, PartialEq, Eq)]
struct Binding {
    name: String,
    from: usize,
    to: usize,
    arg_from: usize,
    arg_to: usize,
    param: String,
    route: String,
    names: Vec<String>,
}
fn literal(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .enumerate()
            .all(|(i, b)| b.is_ascii_alphabetic() || b == b'_' || i > 0 && b.is_ascii_digit())
}
fn owner(doc: &Document) -> Option<String> {
    let root = host::project_of(&doc.path)?.root().ok()?;
    if !root.starts_with('/')
        || root == "/"
        || root.ends_with('/')
        || root.len() > 4096
        || doc.path.len() > 4096
        || !doc.path.starts_with(&format!("{root}/"))
        || root.chars().chain(doc.path.chars()).any(char::is_control)
        || root.contains('\\')
        || doc.path.contains('\\')
        || root.contains("//")
        || doc.path.contains("//")
        || root
            .split('/')
            .chain(doc.path.split('/'))
            .any(|p| matches!(p, "." | ".."))
    {
        return None;
    }
    Some(root)
}
fn closed(text: &str) -> bool {
    let b = text.as_bytes();
    let mut at = 0;
    while at < b.len() {
        if b[at..].starts_with(b"//") {
            at += b[at..]
                .iter()
                .position(|c| *c == b'\n')
                .unwrap_or(b.len() - at);
            continue;
        }
        if b[at..].starts_with(b"/*") {
            let Some(end) = b[at + 2..].windows(2).position(|w| w == b"*/") else {
                return false;
            };
            at += end + 4;
            continue;
        }
        if b[at..].starts_with(b"\"\"\"") {
            return false;
        }
        if matches!(b[at], b'\'' | b'"') {
            let quote = b[at];
            at += 1;
            let mut ended = false;
            while at < b.len() {
                if b[at] == quote {
                    at += 1;
                    ended = true;
                    break;
                }
                if matches!(b[at], b'\n' | b'\r') {
                    return false;
                }
                at += if b[at] == b'\\' { 2 } else { 1 };
            }
            if !ended {
                return false;
            }
            continue;
        }
        at += 1;
    }
    true
}
fn candidates(doc: &Document) -> Vec<Binding> {
    if doc.language != "java"
        || !doc.path.ends_with(".java")
        || doc.text.len() > 128 << 10
        || doc.text.contains("\\u")
        || !spring_text::text::balanced(&doc.text, doc.language == "kotlin")
        || !closed(&doc.text)
    {
        return vec![];
    }
    let file = crate::java::parse(&doc.text);
    if !super::live::imported(&file, "PathVariable")
        || file.imports.iter().any(|i| i.path.ends_with(".*"))
        || file
            .types
            .iter()
            .any(|t| t.kind != "class" || t.supers().next().is_some())
    {
        return vec![];
    }
    let mut out = Vec::new();
    for class in &file.types {
        if class.annotation("RequestMapping").is_some()
            || !class.annotations.iter().any(|a| {
                matches!(a.name.as_str(), "Controller" | "RestController")
                    && super::live::imported(&file, &a.name)
            })
        {
            continue;
        }
        for method in class.methods() {
            let maps: Vec<_> = method
                .annotations
                .iter()
                .filter(|a| MAPPINGS.contains(&a.name.as_str()))
                .collect();
            let [map] = maps.as_slice() else {
                continue;
            };
            if !super::live::imported(&file, &map.name) || !literal_paths(map) {
                continue;
            }
            let args: Vec<_> = map
                .args
                .iter()
                .filter(|a| matches!(a.name.as_str(), "value" | "path"))
                .collect();
            let [arg] = args.as_slice() else {
                continue;
            };
            let [path] = arg.strings.as_slice() else {
                continue;
            };
            if crate::java::tokens(&arg.text).len() != 1
                || path.value.len() != path.end - path.at
                || doc.text[path.at..path.end].contains('\\')
            {
                continue;
            }
            let vars = variables(&path.value);
            let mut names = Vec::new();
            let mut supported = true;
            for (name, a, b) in &vars {
                if !literal(name)
                    || path.value[*a..*b] != format!("{{{name}}}")
                    || names.contains(name)
                {
                    supported = false;
                    break;
                }
                names.push(name.clone());
            }
            if !supported || names.is_empty() {
                continue;
            }
            let mut bound = Vec::new();
            for param in &method.params {
                if let Some(annotation) = param.annotation("PathVariable") {
                    let names: Vec<_> = annotation
                        .args
                        .iter()
                        .filter(|a| matches!(a.name.as_str(), "name" | "value"))
                        .collect();
                    let [a] = names.as_slice() else {
                        supported = false;
                        break;
                    };
                    let [s] = a.strings.as_slice() else {
                        supported = false;
                        break;
                    };
                    if crate::java::tokens(&a.text).len() != 1
                        || s.value.len() != s.end - s.at
                        || !literal(&s.value)
                        || doc.text[s.at..s.end].contains('\\')
                    {
                        supported = false;
                        break;
                    }
                    if bound
                        .iter()
                        .any(|(name, _, _, _): &(String, usize, usize, String)| name == &s.value)
                    {
                        supported = false;
                        break;
                    }
                    bound.push((s.value.clone(), s.at, s.end, param.name.clone()));
                }
            }
            if !supported {
                continue;
            }
            let all_names: Vec<_> = names
                .iter()
                .cloned()
                .chain(bound.iter().map(|b| b.0.clone()))
                .collect();
            for (name, a, b) in vars {
                if let Some((_, arg_from, arg_to, param)) = bound.iter().find(|b| b.0 == name) {
                    out.push(Binding {
                        name,
                        from: path.at + a + 1,
                        to: path.at + b - 1,
                        arg_from: *arg_from,
                        arg_to: *arg_to,
                        param: param.clone(),
                        route: path.value.clone(),
                        names: all_names.clone(),
                    });
                }
            }
        }
    }
    out.truncate(32);
    out
}
fn unchanged(p: &Pending, doc: &Document) -> Result<(), Error> {
    if doc.path != p.doc.path
        || doc.language != p.doc.language
        || doc.version != p.doc.version
        || doc.text != p.doc.text
        || owner(doc).as_deref() != Some(p.root.as_str())
        || !candidates(doc).contains(&p.binding)
    {
        return Err(Error::Stale);
    }
    let project = host::project_of(&doc.path).ok_or(Error::Stale)?;
    if project.read_file(&doc.path)? != doc.text {
        return Err(Error::Stale);
    }
    Ok(())
}
impl State {
    pub(super) fn list(&mut self, doc: &Document, selection: Range) -> Vec<FileAction> {
        let Some(root) = owner(doc) else {
            return vec![];
        };
        let Some(at) = offset(&doc.text, selection.start) else {
            return vec![];
        };
        let Some(binding) = candidates(doc)
            .into_iter()
            .find(|b| b.from <= at && at <= b.to || b.arg_from <= at && at <= b.arg_to)
        else {
            return vec![];
        };
        if self.pending.len() >= 8 {
            self.pending.clear();
        }
        let Some(next) = self.next.checked_add(1) else {
            return vec![];
        };
        self.next = next;
        let key = format!("spring-path-variable:{next}:{}", doc.version);
        let title = format!(
            "Rename route variable `{}` and explicit binding…",
            binding.name
        );
        self.pending.insert(
            key.clone(),
            Pending {
                doc: doc.clone(),
                root,
                binding,
            },
        );
        vec![FileAction {
            title,
            kind: Some("refactor.rename".into()),
            data: Some(key),
            ..Default::default()
        }]
    }
    pub(super) fn resolve(
        &mut self,
        doc: &Document,
        action: FileAction,
    ) -> Result<FileAction, Error> {
        let key = action.data.as_deref().ok_or(Error::Stale)?;
        let p = self.pending.get(key).cloned().ok_or(Error::Stale)?;
        unchanged(&p, doc)?;
        let new = host::input(
            "New route variable name (Java parameter name stays unchanged)",
            Some(&p.binding.name),
        )?
        .ok_or(Error::Cancelled)?;
        unchanged(&p, doc)?;
        if !literal(&new) || p.binding.names.contains(&new) {
            return Err(Error::Invalid("Choose a different literal name that is not already a template variable or explicit binding in this mapping.".into()));
        }
        let preview = format!(
            "File: {}\nDocument version: {}\nLiteral source route: {}\n\nRename exactly these two source tokens:\nTemplate variable: {} → {}\n@PathVariable literal: {} → {}\n\nJava parameter `{}` stays unchanged. This changes this route's external variable spelling and its explicit binding only; it is not Java symbol Rename and does not update callers, other handlers, inherited/class-level routes, generated clients or runtime metadata.\n\nApply both undoable edits atomically?",
            doc.path,
            doc.version,
            p.binding.route,
            p.binding.name,
            new,
            p.binding.name,
            new,
            p.binding.param
        );
        if !ui_kit::prompts::confirm_exact("Rename local route variable and binding?", &preview)? {
            return Err(Error::Cancelled);
        }
        unchanged(&p, doc)?;
        self.pending.remove(key);
        Ok(FileAction {
            title: "Rename route variable and explicit binding".into(),
            edits: vec![(
                doc.path.clone(),
                vec![
                    TextEdit {
                        range: range(&doc.text, p.binding.from, p.binding.to),
                        text: new.clone(),
                    },
                    TextEdit {
                        range: range(&doc.text, p.binding.arg_from, p.binding.arg_to),
                        text: new,
                    },
                ],
            )],
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{
        file_actions::FileActions,
        mock::MockHost,
        text::{offset, position},
    };
    fn doc() -> Document {
        Document{path:"/project/Routes.java".into(),language:"java".into(),version:11,text:"import org.springframework.web.bind.annotation.RestController;\r\nimport org.springframework.web.bind.annotation.GetMapping;\r\nimport org.springframework.web.bind.annotation.PathVariable;\r\n@RestController class Routes { /* 😀 retained */ @GetMapping(\"/items/{id}\") String item(@PathVariable(\"id\") String accountKey) {return accountKey;} }".into()}
    }
    fn selection(d: &Document) -> Range {
        let p = position(&d.text, d.text.find("{id}").unwrap() + 1);
        Range::new(p, p)
    }
    #[test]
    fn scoped_atomic_rename_preserves_java_identifier_unsaved_source_crlf_and_utf16() {
        let h = MockHost::install();
        let d = doc();
        h.add_file("Routes.java", "disk differs");
        h.open_editor(&d.path, "java", &d.text, &[]);
        let mut owner = Endpoints::default();
        let a = owner.file_actions(&d, selection(&d)).remove(0);
        let replay = a.clone();
        assert!(a.edits.is_empty());
        h.answer_next_prompt(Some(&["itemId"]));
        h.answer_next_prompt(Some(&["yes"]));
        let resolved = owner.resolve_file_action(&d, a).unwrap();
        assert!(resolved.operations.is_empty());
        assert_eq!(resolved.edits.len(), 1);
        assert_eq!(resolved.edits[0].1.len(), 2);
        let mut after = d.text.clone();
        let mut edits = resolved.edits[0].1.clone();
        edits.sort_by_key(|e| std::cmp::Reverse(e.range.start));
        for e in edits {
            let from = offset(&d.text, e.range.start).unwrap();
            let to = offset(&d.text, e.range.end).unwrap();
            assert_eq!(&d.text[from..to], "id");
            after.replace_range(from..to, &e.text);
        }
        assert_eq!(
            after,
            d.text
                .replace("{id}", "{itemId}")
                .replace("PathVariable(\"id\")", "PathVariable(\"itemId\")")
        );
        assert!(after.contains("String accountKey) {return accountKey;}"));
        assert!(after.contains("\r\n"));
        assert!(owner.resolve_file_action(&d, replay).is_err());
        assert_eq!(
            h.invocation()
                .project()
                .unwrap()
                .read_file("Routes.java")
                .unwrap(),
            d.text
        );
        assert!(h.process_runs().is_empty());
        assert_eq!(h.fetches(), 0);
    }
    #[test]
    fn cancellation_stale_snapshot_nested_owner_and_colliding_names_never_return_edits() {
        for mode in 0..6 {
            let h = MockHost::install();
            let mut d = doc();
            h.add_file("Routes.java", &d.text);
            let mut owner = Endpoints::default();
            let a = owner.file_actions(&d, selection(&d)).remove(0);
            match mode {
                0 => h.answer_next_prompt(None),
                1 => {
                    h.answer_next_prompt(Some(&["nextId"]));
                    h.answer_next_prompt(None);
                }
                2 => d.version += 1,
                3 => h.set_project_root(Some("/other")),
                4 => h.add_file("Routes.java", "changed"),
                _ => h.answer_next_prompt(Some(&["id"])),
            }
            assert!(owner.resolve_file_action(&d, a).is_err(), "{mode}");
            assert!(h.process_runs().is_empty());
        }
        let h = MockHost::install();
        let mut d = doc();
        d.path = "/project/nested/Routes.java".into();
        h.add_file("nested/Routes.java", &d.text);
        let mut owner = Endpoints::default();
        let a = owner.file_actions(&d, selection(&d)).remove(0);
        h.add_project("/project/nested");
        assert!(owner.resolve_file_action(&d, a).is_err());
    }
    #[test]
    fn foreign_implicit_dynamic_regex_class_scope_and_malformed_sources_stay_quiet() {
        let _h = MockHost::install();
        let d = doc();
        for text in [
            d.text.replace(
                "org.springframework.web.bind.annotation.PathVariable",
                "foreign.PathVariable",
            ),
            d.text.replace("PathVariable(\"id\")", "PathVariable"),
            d.text.replace("{id}", "{id:[0-9]+}"),
            d.text.replace(
                "@RestController class Routes",
                "@RestController @RequestMapping(\"/base\") class Routes",
            ),
            d.text
                .replace("GetMapping(\"/items/{id}\")", "GetMapping(PATH)"),
            format!("{} /* unclosed", d.text),
            d.text.replace(
                "String accountKey)",
                "String accountKey, @PathVariable(\"id\") String duplicate)",
            ),
            d.text.replace(
                "PathVariable(\"id\")",
                "PathVariable(value=\"id\",name=\"id\")",
            ),
        ] {
            let changed = Document { text, ..d.clone() };
            assert!(
                State::default()
                    .list(
                        &changed,
                        Range::new(Position::new(0, 0), Position::new(u32::MAX, u32::MAX))
                    )
                    .is_empty()
            );
            assert!(candidates(&changed).is_empty(), "{}", changed.text);
        }
    }
    #[test]
    fn target_collision_and_invalid_name_are_refused_after_prompt() {
        for name in ["other", "../x", "${id}", "bad-name"] {
            let h = MockHost::install();
            let mut d = doc();
            d.text = d.text.replace("/items/{id}", "/items/{id}/{other}");
            h.add_file("Routes.java", &d.text);
            let mut owner = Endpoints::default();
            let a = owner.file_actions(&d, selection(&d)).remove(0);
            h.answer_next_prompt(Some(&[name]));
            assert!(owner.resolve_file_action(&d, a).is_err(), "{name}");
        }
    }
}
