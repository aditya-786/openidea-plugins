//! Reviewed physical component rename in visible project sources. Unknown,
//! transformed, dynamic and incomplete source models never yield partial edits.
use crate::*;
const TITLE: &str = "Rename component across the project…";
fn root_for(path: &str) -> Result<(Project, String), Error> {
    let project = host::project_of(path).ok_or(Error::Stale)?;
    let root = project.root()?;
    if root.is_empty()
        || !path.starts_with(&format!("{}/", root.trim_end_matches('/')))
        || path.split('/').any(|part| matches!(part, "." | ".."))
    {
        return Err(Error::Stale);
    }
    Ok((project, root))
}
fn identity(doc: &Document, at: usize) -> Option<(String, String)> {
    let proof = pointer_status::Proof::parse(&doc.text)?;
    let navigation = proof.navigation_ref_at(at);
    let is_reference = navigation.is_some();
    let reference = navigation
        .map(|reference| reference.target)
        .or_else(|| proof.copy_pointer_at(at))?;
    let (file, pointer) = reference.split_once('#')?;
    let target = component_occurrences::physical_target(&doc.path, file)?;
    let (project, root) = root_for(&doc.path).ok()?;
    let (target_project, target_root) = root_for(&target).ok()?;
    if root != target_root {
        return None;
    }
    let text = if target == doc.path {
        doc.text.clone()
    } else {
        target_project.read_file(&target).ok()?
    };
    let proof = pointer_status::Proof::parse(&text)?;
    let base = proof.component_base(pointer)?;
    if !is_reference && pointer != base {
        return None;
    }
    if base.starts_with("/components/securitySchemes/") {
        return None;
    }
    let _ = project;
    Some((target, base))
}
pub(crate) fn offer(doc: &Document, selection: Range) -> Option<CodeAction> {
    let at = offset(&doc.text, selection.start)?;
    let (_, base) = identity(doc, at)?;
    let (_, root) = root_for(&doc.path).ok()?;
    Some(CodeAction {
        title: TITLE.into(),
        kind: Some("refactor.rename".into()),
        preferred: false,
        edits: Vec::new(),
        data: Some(
            serde_json::json!([
                "physical-component-rename",
                root,
                doc.path,
                doc.version,
                at,
                base
            ])
            .to_string(),
        ),
    })
}
fn files(
    project: &Project,
    path: &str,
    target: &str,
) -> Result<std::collections::BTreeSet<String>, Error> {
    let mut files = std::collections::BTreeSet::from([path.into(), target.into()]);
    for glob in ["**/*.json", "**/*.yaml", "**/*.yml"] {
        let found = project.find_files(glob, 501)?;
        if found.len() >= 501 {
            return Err("The project has over 500 JSON or YAML files of one kind, so not every usage can be found. Nothing was renamed.".into());
        }
        files.extend(found);
    }
    Ok(files)
}
fn simple_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
        && !["true", "false", "null", "yes", "no", "on", "off"]
            .iter()
            .any(|reserved| name.eq_ignore_ascii_case(reserved))
}
pub(crate) fn resolve(doc: &Document, action: CodeAction) -> Result<CodeAction, Error> {
    if action.title != TITLE || action.kind.as_deref() != Some("refactor.rename") {
        return Err(Error::Stale);
    }
    let raw = action
        .data
        .as_deref()
        .filter(|raw| raw.len() <= 8192)
        .ok_or(Error::Stale)?;
    let (tag, expected_root, path, version, at, expected_base): (
        String,
        String,
        String,
        u64,
        usize,
        String,
    ) = serde_json::from_str(raw).map_err(|_| Error::Stale)?;
    if tag != "physical-component-rename" || path != doc.path || version != doc.version {
        return Err(Error::Stale);
    }
    let (project, root) = root_for(&path)?;
    if root != expected_root || project.read_file(&path)? != doc.text {
        return Err(Error::Stale);
    }
    let (target, base) = identity(doc, at).ok_or(Error::Stale)?;
    if base != expected_base {
        return Err(Error::Stale);
    }
    let old = base.rsplit('/').next().ok_or(Error::Stale)?;
    if old.contains(['~', '%']) {
        return Err(
            "Component names with ~ or % can't be renamed here. Nothing was renamed.".into(),
        );
    }
    let Some(new) = host::input(
        "New component name (letters, digits, _ and -)",
        Some(old),
    )?
    else {
        return Err(Error::Cancelled);
    };
    if !simple_name(&new) || new == old {
        return Err("Use a new name that starts with a letter or _ and has only letters, digits, _ and -. Nothing was renamed.".into());
    }
    let members = files(&project, &path, &target)?;
    let mut before = std::collections::BTreeMap::new();
    let mut bytes = 0usize;
    for file in &members {
        if root_for(file)?.1 != root {
            return Err(Error::Stale);
        }
        let text = project.read_file(file)?;
        bytes = bytes.saturating_add(text.len());
        if bytes > 16 * 1024 * 1024 {
            return Err("The project's JSON and YAML files are over 16 MiB in total. Nothing was renamed.".into());
        }
        before.insert(file.clone(), text);
    }
    if before.get(&path) != Some(&doc.text) {
        return Err(Error::Stale);
    }
    let target_text = before.get(&target).ok_or(Error::Stale)?;
    let proof = pointer_status::Proof::parse(target_text)
        .ok_or("Couldn't read the file that declares this component. Nothing was renamed.")?;
    let new_base = format!("{}/{}", base.rsplit_once('/').ok_or(Error::Stale)?.0, new);
    if proof.status(&new_base) != pointer_status::Status::Missing {
        return Err(
            "A component with that name already exists. Choose another name.".into(),
        );
    }
    let declaration = proof.pointer_location(&base).ok_or(Error::Stale)?;
    if &target_text[offset(target_text, declaration.start).ok_or(Error::Stale)?
        ..offset(target_text, declaration.end).ok_or(Error::Stale)?]
        != old
    {
        return Err(
            "The component key is quoted or escaped in a way this rename can't edit. Nothing was renamed."
                .into(),
        );
    }
    let mut edits = std::collections::BTreeMap::<String, Vec<TextEdit>>::new();
    edits.entry(target.clone()).or_default().push(TextEdit {
        range: declaration,
        text: new.clone(),
    });
    let prefix = format!("{base}/");
    let mut occurrences = 1usize;
    for (file, text) in &before {
        let proof = pointer_status::Proof::parse(text)
            .ok_or("A JSON or YAML file in the project couldn't be read. Nothing was renamed.")?;
        let refs=proof.complete_refactor_references().ok_or("Some $ref values in the project couldn't be read, so not every usage can be updated. Nothing was renamed.")?;
        for reference in refs {
            let Some((relative, pointer)) = reference.target.split_once('#') else {
                continue;
            };
            if component_occurrences::physical_target(file, relative).as_deref()
                != Some(target.as_str())
                || !(pointer == base || pointer.starts_with(&prefix))
            {
                continue;
            }
            if text.get(reference.at..reference.end) != Some(reference.target.as_str()) {
                return Err(
                    "A $ref to this component is escaped and can't be edited here. Nothing was renamed."
                        .into(),
                );
            }
            let start = reference.at + relative.len() + 1 + base.len() - old.len();
            let end = start + old.len();
            if text.get(start..end) != Some(old) {
                return Err(Error::Stale);
            }
            edits.entry(file.clone()).or_default().push(TextEdit {
                range: spring_text::range(text, start, end),
                text: new.clone(),
            });
            occurrences += 1;
            if occurrences > 512 {
                return Err("More than 512 usages. Nothing was renamed.".into());
            }
        }
    }
    let summary = format!(
        "Changes {occurrences} places (the declaration and its $ref usages) in {} files.\nFiles outside this project and ignored files aren't updated.\nUndo in each edited file.\n\n{}",
        edits.len(),
        listed_files(edits.keys())
    );
    if !host::confirm(&format!("Rename component {old} to {new}?"), &summary)? {
        return Err(Error::Cancelled);
    }
    if root_for(&path)?.1 != root || files(&project, &path, &target)? != members {
        return Err(Error::Stale);
    }
    for (file, text) in &before {
        if root_for(file)?.1 != root || project.read_file(file)? != *text {
            return Err(Error::Stale);
        }
    }
    let mut result = action;
    result.data = None;
    result.edits = edits.into_iter().collect();
    Ok(result)
}

/// The host cuts confirm bodies at 16 KiB, so long lists end with a count.
fn listed_files<'a>(paths: impl ExactSizeIterator<Item = &'a String>) -> String {
    const SHOWN: usize = 20;
    let total = paths.len();
    let mut lines: Vec<String> = paths.take(SHOWN).cloned().collect();
    if total > SHOWN {
        lines.push(format!("…and {} more", total - SHOWN));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {

    #[test]
    fn long_file_lists_end_with_a_count() {
        let paths: Vec<String> = (0..25).map(|i| format!("/p/spec{i}.yaml")).collect();
        let listed = super::listed_files(paths.iter());
        assert_eq!(listed.lines().count(), 21);
        assert!(listed.ends_with("…and 5 more"));
        assert_eq!(super::listed_files(paths[..3].iter()).lines().count(), 3);
    }
    use super::*;
    use openidea_plugin::mock::MockHost;
    fn setup() -> (MockHost, Document, CodeAction) {
        let h = MockHost::install();
        let text = r##"{"openapi":"3.0.3","info":{"title":"😀"},"components":{"schemas":{"Foo":{},"Foo2":{}}},"a":{"$ref":"#/components/schemas/Foo"},"unrelated":"Foo"}"##;
        h.add_file("/project/openapi.json", text);
        h.add_file("/project/client.json",r##"{"x":[{"$ref":"./openapi.json#/components/schemas/Foo"}],"other":{"$ref":"./other.json#/components/schemas/Foo"}}"##);
        h.add_file(
            "/project/other.json",
            r##"{"components":{"schemas":{"Foo":{}}}}"##,
        );
        h.open_editor("/project/openapi.json", "json", text, &[(0, 0, 0, 0)]);
        let doc = Document {
            path: "/project/openapi.json".into(),
            language: "json".into(),
            text: text.into(),
            version: 1,
        };
        let caret = position(text, text.find("Foo").unwrap());
        let action = offer(&doc, Range::new(caret, caret)).unwrap();
        (h, doc, action)
    }
    fn apply(text: &str, edits: &[TextEdit]) -> String {
        let mut out = text.to_owned();
        let mut edits: Vec<_> = edits.iter().collect();
        edits.sort_by_key(|edit| std::cmp::Reverse(offset(text, edit.range.start).unwrap()));
        for edit in edits {
            out.replace_range(
                offset(text, edit.range.start).unwrap()..offset(text, edit.range.end).unwrap(),
                &edit.text,
            );
        }
        out
    }
    #[test]
    fn reviewed_cross_file_rename_changes_only_owned_component_and_refs_with_utf16_ranges() {
        let (h, doc, action) = setup();
        assert!(action.edits.is_empty());
        h.answer_next_prompt(Some(&["Bar"]));
        h.answer_next_prompt(Some(&["ok"]));
        let resolved = resolve(&doc, action).unwrap();
        assert_eq!(resolved.edits.len(), 2);
        let project = host::project_of(&doc.path).unwrap();
        for (path, edits) in &resolved.edits {
            let before = project.read_file(path).unwrap();
            let after = apply(&before, edits);
            if path == &doc.path {
                assert!(after.contains("😀"));
                assert!(after.contains("\"unrelated\":\"Foo\""));
                assert!(after.contains("\"Foo2\":{}"));
                let proof = pointer_status::Proof::parse(&after).unwrap();
                assert_eq!(
                    proof.status("/components/schemas/Bar"),
                    pointer_status::Status::Present
                );
                assert_eq!(
                    proof.status("/components/schemas/Foo"),
                    pointer_status::Status::Missing
                );
            } else {
                assert!(after.contains("./openapi.json#/components/schemas/Bar"));
                assert!(after.contains("./other.json#/components/schemas/Foo"));
            }
        }
        assert!(h.requests().is_empty() && h.process_runs().is_empty());
        assert_eq!(project.read_file(&doc.path).unwrap(), doc.text);
    }
    #[test]
    fn collision_dynamic_or_unknown_graph_and_stale_action_yield_no_partial_edits() {
        let (h, doc, action) = setup();
        h.answer_next_prompt(Some(&["Foo2"]));
        assert!(resolve(&doc, action).is_err());
        let (h, doc, action) = setup();
        h.add_file(
            "/project/dynamic.json",
            r##"{"$dynamicRef":"#/components/schemas/Foo"}"##,
        );
        h.answer_next_prompt(Some(&["Bar"]));
        assert!(resolve(&doc, action).is_err());
        let (h, doc, action) = setup();
        h.add_file("/project/broken.yaml", "broken: [");
        h.answer_next_prompt(Some(&["Bar"]));
        assert!(resolve(&doc, action).is_err());
        let (_h, mut doc, action) = setup();
        doc.version += 1;
        assert!(matches!(resolve(&doc, action), Err(Error::Stale)));
    }
    #[test]
    fn cancelling_review_or_using_reserved_yaml_key_never_returns_edits() {
        let (h, doc, action) = setup();
        h.answer_next_prompt(Some(&["Bar"]));
        h.answer_next_prompt(None);
        assert!(matches!(resolve(&doc, action), Err(Error::Cancelled)));
        let (h, doc, action) = setup();
        h.answer_next_prompt(Some(&["true"]));
        assert!(resolve(&doc, action).is_err());
    }
}
