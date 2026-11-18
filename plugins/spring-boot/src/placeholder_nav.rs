//! Source-local placeholder candidates are not effective configuration owners.
use super::*;
use openidea_plugin::hover::Hover;
use std::time::{Duration, Instant};
const MAX_TEXT: usize = 32 << 10;
const MAX_KEYS: usize = 256;
const MAX_REFERENCES: usize = 256;
const UNSUPPORTED_FILE: &str = "Find Property works in application*.properties and bootstrap*.properties files of this project, up to 32 KiB. Open one and place the caret on a key.";
const PLACE_CARET: &str = "Place the caret on a property key or on the name inside a ${…} placeholder.";
struct Reference {
    key: String,
    from: usize,
    to: usize,
}
fn literal(key: &str) -> bool {
    key.len() <= 256
        && key
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
}
fn model(doc: &Document) -> Option<(Vec<Key>, Vec<Reference>)> {
    if doc.language != "properties"
        || !doc.path.starts_with('/')
        || !doc.path.ends_with(".properties")
        || !is_config_file(&doc.path)
        || doc.text.len() > MAX_TEXT
        || doc.path.contains('\\')
        || doc.path.split('/').any(|p| p == "." || p == "..")
        || source_scope(&doc.path).is_none()
    {
        return None;
    }
    // This source model uses LF/CRLF physical lines and the shared lexical
    // parser. It cannot prove uniqueness across bare CR or form-feed key
    // separators, nor treat Unicode whitespace as Java Properties indentation.
    if doc
        .text
        .as_bytes()
        .iter()
        .enumerate()
        .any(|(i, b)| *b == b'\r' && doc.text.as_bytes().get(i + 1) != Some(&b'\n'))
        || doc.text.contains('\u{c}')
        || doc.text.split('\n').any(|line| {
            line.chars()
                .take_while(|c| c.is_whitespace())
                .any(|c| !c.is_ascii())
        })
    {
        return None;
    }
    if doc
        .text
        .lines()
        .any(|line| matches!(line.trim(), "#---" | "!---"))
    {
        return None;
    }
    let deadline = Instant::now() + Duration::from_millis(10);
    let keys = config::properties(&doc.text);
    if keys.len() > MAX_KEYS {
        return None;
    }
    let mut references = Vec::new();
    for key in &keys {
        if Instant::now() >= deadline {
            return None;
        }
        let value = doc.text.get(key.value_at..key.value_end)?;
        // Escaped dollars, continued values and dynamic nested defaults require
        // the full Boot placeholder/property decoding contract; do not guess.
        if value.contains('\\') || value.contains("#{") {
            continue;
        }
        let mut rest = value;
        let mut base = key.value_at;
        while let Some(at) = rest.find("${") {
            let from = at + 2;
            let Some(end) = rest[from..].find('}').map(|i| from + i) else {
                break;
            };
            let body = &rest[from..end];
            let name = body.split_once(':').map_or(body, |(name, _)| name);
            if literal(name) && !body.contains(['{', '$', '\r', '\n']) {
                if references.len() == MAX_REFERENCES {
                    return None;
                }
                references.push(Reference {
                    key: name.into(),
                    from: base + from,
                    to: base + from + name.len(),
                });
            }
            base += end + 1;
            rest = &rest[end + 1..];
        }
    }
    Some((keys, references))
}
fn selected(doc: &Document, at: Position) -> Option<(Vec<Key>, Reference)> {
    let offset = offset(&doc.text, at)?;
    let (keys, refs) = model(doc)?;
    let reference = refs
        .into_iter()
        .find(|r| r.from <= offset && offset < r.to)?;
    Some((keys, reference))
}
fn candidates<'a>(doc: &Document, keys: &'a [Key], name: &str) -> Vec<&'a Key> {
    let matches: Vec<_> = keys.iter().filter(|k| k.key == name).collect();
    if matches.len() != 1 {
        return Vec::new();
    }
    let key = matches[0];
    // A decoded/relaxed name must never become an unconditional source token.
    if doc.text.get(key.at..key.end) != Some(name) || !literal(name) {
        return Vec::new();
    }
    matches
}
pub(crate) fn definition(doc: &Document, at: Position) -> Option<Vec<Location>> {
    let (keys, reference) = selected(doc, at)?;
    Some(
        candidates(doc, &keys, &reference.key)
            .into_iter()
            .map(|key| Location {
                path: doc.path.clone(),
                range: range(&doc.text, key.at, key.end),
            })
            .collect(),
    )
}
pub(crate) fn hover(doc: &Document, at: Position) -> Option<Hover> {
    let (keys, reference) = selected(doc, at)?;
    let targets = candidates(doc, &keys, &reference.key);
    let owner = match targets.as_slice() {
        [one] => format!("One exact source candidate in this file at line {}.", range(&doc.text, one.at, one.end).start.line + 1),
        _ => "No unique exact current-file source candidate. Imported files and external values may still supply this key.".into(),
    };
    Some(Hover::new(format!("**Configuration placeholder** `{}`\n\n{owner}\n\nCurrent unsaved file only; active profiles, imports, relaxed binding, environment overrides and effective runtime values are unresolved. No values are displayed and no host Rename/Find Usages anchors are published.",reference.key)).range(range(&doc.text,reference.from,reference.to)))
}
fn live_source(ctx: &Invocation) -> Result<(String, Document, Position), Error> {
    let root = ctx.project().ok_or(Error::Stale)?.root()?;
    let editor = ctx
        .editor()
        .ok_or("Open a supported properties file and place the caret on a key or placeholder.")?;
    let d = editor.document().map_err(|error| ui_kit::denied::map(error, "Find Property", "editor.read", "Spring Boot Tools"))?;
    let at = editor
        .selections()?
        .first()
        .map(|s| s.start)
        .ok_or(Error::Stale)?;
    let doc = Document {
        path: editor.path(),
        language: editor.language(),
        text: d.text,
        version: d.version,
    };
    if !root.starts_with('/')
        || root == "/"
        || !doc.path.starts_with(&format!("{root}/"))
        || source_scope(&doc.path).map(|(_, owner)| owner).as_deref() != Some(root.as_str())
    {
        return Err(Error::Stale);
    }
    Ok((root, doc, at))
}
fn unchanged(ctx: &Invocation, root: &str, doc: &Document) -> Result<(), Error> {
    let (current_root, current, _) = live_source(ctx)?;
    if current_root != root || &current != doc {
        return Err(Error::Stale);
    }
    Ok(())
}
/// Explicit current-source pick list, deliberately separate from host usages/Rename.
pub(crate) fn occurrences(ctx: &Invocation) -> Result<Option<String>, Error> {
    let (root, doc, at) = live_source(ctx)?;
    let off = offset(&doc.text, at).ok_or(Error::Stale)?;
    let (keys, references) =
        model(&doc).ok_or(UNSUPPORTED_FILE)?;
    let name = references
        .iter()
        .find(|r| r.from <= off && off < r.to)
        .map(|r| r.key.as_str())
        .or_else(|| {
            keys.iter()
                .find(|k| k.at <= off && off < k.end)
                .map(|k| k.key.as_str())
        })
        .ok_or(PLACE_CARET)?;
    let targets = candidates(&doc, &keys, name);
    let [key] = targets.as_slice() else {
        return Err(Error::Invalid("Find Property in File needs the key declared exactly once in this file. Try Find Property in Project.".into()));
    };
    let mut locations = vec![("Property spelling", range(&doc.text, key.at, key.end))];
    locations.extend(
        references
            .iter()
            .filter(|r| r.key == name)
            .map(|r| ("Placeholder spelling", range(&doc.text, r.from, r.to))),
    );
    let file = doc.path.rsplit('/').next().unwrap_or(&doc.path);
    let items: Vec<_> = locations
        .iter()
        .enumerate()
        .map(|(i, (kind, range))| {
            PickItem::new(
                &i.to_string(),
                &format!("{kind} · line {}", range.start.line + 1),
            )
            .description(&format!(
                "{file} · UTF-16 column {}",
                range.start.character + 1
            ))
        })
        .collect();
    let title = format!(
        "{name} · {} occurrences in this file",
        items.len()
    );
    let Some(selected) = host::pick(&title, &items)? else {
        return Ok(None);
    };
    let target = selected
        .parse::<usize>()
        .ok()
        .and_then(|i| locations.get(i))
        .ok_or(Error::Stale)?
        .1;
    unchanged(ctx, &root, &doc)?;
    host::open_location(&doc.path, target.start.line, target.start.character)?;
    Ok(None)
}

/// An explicitly requested physical-source graph. Profile/import/runtime
/// relationships are displayed as file provenance, never merged into ownership.
pub(crate) fn project_occurrences(ctx: &Invocation) -> Result<Option<String>, Error> {
    let (root, doc, at) = live_source(ctx)?;
    let off = offset(&doc.text, at).ok_or(Error::Stale)?;
    let (keys, references) =
        model(&doc).ok_or(UNSUPPORTED_FILE)?;
    let name = references
        .iter()
        .find(|r| r.from <= off && off < r.to)
        .map(|r| r.key.as_str())
        .or_else(|| {
            keys.iter()
                .find(|k| k.at <= off && off < k.end)
                .map(|k| k.key.as_str())
        })
        .filter(|name| literal(name))
        .ok_or(PLACE_CARET)?;
    let project = ctx.project().ok_or(Error::Stale)?;
    let mut paths = std::collections::BTreeSet::new();
    paths.insert(doc.path.clone());
    for glob in ["**/application*.properties", "**/bootstrap*.properties"] {
        let found = project.find_files(glob, 129)?;
        if found.len() > 128 {
            return Err("Configuration source inventory exceeds 128 files; narrow the project before searching.".into());
        }
        for path in found {
            if source_scope(&path).map(|(_, owner)| owner).as_deref() == Some(root.as_str()) {
                paths.insert(path);
            }
        }
    }
    if paths.len() > 128 {
        return Err("Configuration source inventory exceeds 128 files.".into());
    }
    let mut locations = Vec::new();
    let mut snapshots = std::collections::BTreeMap::new();
    let mut total = 0usize;
    for path in paths {
        let text = if path == doc.path {
            doc.text.clone()
        } else {
            project.read_file(&path)?
        };
        total += text.len();
        if total > 1024 * 1024 {
            return Err("Configuration source inventory exceeds 1 MiB.".into());
        }
        let file = Document {
            path: path.clone(),
            language: "properties".into(),
            text: text.clone(),
            version: 0,
        };
        let (keys, refs)=model(&file).ok_or_else(|| format!("{path} can't be searched (it is over 32 KiB or uses syntax this search doesn't read), so no results are shown."))?;
        for key in keys
            .iter()
            .filter(|k| k.key == name && file.text.get(k.at..k.end) == Some(name))
        {
            locations.push((
                path.clone(),
                range(&text, key.at, key.end),
                "Declaration spelling",
            ));
        }
        for reference in refs.iter().filter(|r| r.key == name) {
            locations.push((
                path.clone(),
                range(&text, reference.from, reference.to),
                "Placeholder spelling",
            ));
        }
        if locations.len() > 1024 {
            return Err("Too many property occurrences for a bounded source search.".into());
        }
        snapshots.insert(path, text);
    }
    let items: Vec<_> = locations
        .iter()
        .enumerate()
        .map(|(i, (path, range, kind))| {
            PickItem::new(
                &i.to_string(),
                &format!(
                    "{kind} · {}:{}",
                    path.strip_prefix(&format!("{root}/")).unwrap_or(path),
                    range.start.line + 1
                ),
            )
            .description(if path == &doc.path {
                "Current editor source; profile/import/runtime binding unresolved"
            } else {
                "Saved file source; profile/import/runtime binding unresolved"
            })
        })
        .collect();
    if items.is_empty() {
        return Ok(Some(format!(
            "No exact literal source occurrences for {name}."
        )));
    }
    let Some(selected) = host::pick(
        &format!("Project source occurrences: {name} · {}", items.len()),
        &items,
    )?
    else {
        return Ok(None);
    };
    let (path, range, _) = selected
        .parse::<usize>()
        .ok()
        .and_then(|i| locations.get(i))
        .ok_or(Error::Stale)?;
    unchanged(ctx, &root, &doc)?;
    if source_scope(path).map(|(_, owner)| owner).as_deref() != Some(root.as_str()) {
        return Err(Error::Stale);
    }
    if path != &doc.path && project.read_file(path)? != snapshots[path] {
        return Err(Error::Stale);
    }
    host::open_location(path, range.start.line, range.start.character)?;
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{hover::HoverProvider, mock::MockHost};
    fn doc(text: &str) -> Document {
        Document {
            path: "/project/application.properties".into(),
            language: "properties".into(),
            version: 5,
            text: text.into(),
        }
    }
    fn at(doc: &Document, name: &str) -> Position {
        let from = doc.text.rfind(name).unwrap();
        range(&doc.text, from, from + name.len()).start
    }
    #[test]
    fn registered_placeholder_shortcuts_reject_malformed_and_unknown_current_scope() {
        let h = MockHost::install();
        let mut tools = BootTools::default();
        for path in [
            "/project//application.properties",
            "/project/\napplication.properties",
        ] {
            let d = Document {
                path: path.into(),
                ..doc("app.name=secret\nx=${app.name}")
            };
            assert!(
                tools.definition(&d, at(&d, "app.name")).is_empty(),
                "{path:?}"
            );
            assert!(tools.hover(&d, at(&d, "app.name")).is_none(), "{path:?}");
        }
        let d = doc("app.name=secret\nx=${app.name}");
        assert_eq!(tools.definition(&d, at(&d, "app.name")).len(), 1);
        for i in 0..65 {
            h.add_project(&format!("/other-{i}"));
        }
        assert!(tools.definition(&d, at(&d, "app.name")).is_empty());
        assert!(tools.hover(&d, at(&d, "app.name")).is_none());
        assert!(
            h.opened_locations().is_empty()
                && h.requests().is_empty()
                && h.process_runs().is_empty()
        );
    }
    #[test]
    fn nested_project_reassignment_refuses_captured_source_and_accepts_fresh_local_owner() {
        let text = "app.name=secret\r\nlabel=😀 ${app.name:fallback}\r\n";
        let h = command_host(text, Position::new(0, 3));
        h.open_editor(
            "/project/nested/application.properties",
            "properties",
            text,
            &[(0, 3, 0, 3)],
        );
        let (root, captured, _) = live_source(&h.invocation()).unwrap();
        assert_eq!(root, "/project");
        h.add_project("/project/nested");
        assert!(unchanged(&h.invocation(), &root, &captured).is_err());
        h.answer_next_prompt(Some(&["1"]));
        assert!(
            BootTools::default()
                .run_command(
                    "current-file-placeholder-occurrences",
                    &h.invocation(),
                    None
                )
                .is_err()
        );
        assert!(h.opened_locations().is_empty());
        h.set_project_root(Some("/project/nested"));
        h.answer_next_prompt(Some(&["1"]));
        BootTools::default()
            .run_command(
                "current-file-placeholder-occurrences",
                &h.invocation(),
                None,
            )
            .unwrap();
        assert_eq!(
            h.opened_locations(),
            vec![("/project/nested/application.properties".into(), 1, 11)]
        );
        assert!(h.requests().is_empty() && h.process_runs().is_empty());
    }
    #[test]
    fn source_local_physical_bare_cr_cannot_hide_duplicate_declarations() {
        let _h = MockHost::install();
        let d = doc("app.name=first\rapp.name=second\nx=${app.name}");
        assert!(
            definition(&d, at(&d, "app.name")).is_none(),
            "unsupported physical line grammar cannot prove unique source declaration"
        );
    }
    #[test]
    fn source_local_physical_whitespace_cannot_invent_literal_declarations() {
        let _h = MockHost::install();
        for text in [
            "\u{a0}app.name=first\nx=${app.name}",
            "app.name=first\napp.name\u{c}=second\nx=${app.name}",
        ] {
            let d = doc(text);
            assert!(definition(&d, at(&d, "app.name")).is_none(), "{text:?}");
        }
    }
    #[test]
    fn same_unsaved_file_reference_with_literal_default_uses_exact_utf16_key_ranges() {
        let _host = MockHost::install();
        let d = doc("app.name=private-secret\r\nlabel=😀 ${app.name:fallback}\r\n");
        let found = definition(&d, at(&d, "app.name")).unwrap();
        assert_eq!(
            found,
            vec![Location {
                path: d.path.clone(),
                range: Range::new(Position::new(0, 0), Position::new(0, 8))
            }]
        );
        let hover = hover(&d, at(&d, "app.name")).unwrap();
        assert_eq!(hover.range.unwrap().start, Position::new(1, 11));
        assert!(!hover.markdown.contains("private-secret"));
        assert!(!hover.markdown.contains("fallback"));
        let changed = doc("app.renamed=private-secret\r\nlabel=😀 ${app.name:fallback}\r\n");
        assert!(
            definition(&changed, at(&changed, "app.name"))
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn duplicate_decoded_escaped_and_relaxed_names_are_not_owners() {
        let _host = MockHost::install();
        for text in [
            "app.name=a\napp.name=b\nx=${app.name}",
            "app\\.name=a\nx=${app.name}",
            "app.name=a\napp\\.name=b\nx=${app.name}",
            "appName=a\nx=${app-name}",
        ] {
            let d = doc(text);
            let name = if text.contains("app-name") {
                "app-name"
            } else {
                "app.name"
            };
            assert!(definition(&d, at(&d, name)).unwrap().is_empty(), "{text}");
        }
    }
    #[test]
    fn escaped_nested_commented_default_and_boundary_positions_stay_quiet() {
        let _host = MockHost::install();
        for text in [
            "app.name=a\nx=\\${app.name}",
            "app.name=a\nx=${outer:${app.name}}",
            "app.name=a\n# x=${app.name}",
            "app.name=a\nx=${app.name:default}",
        ] {
            let d = doc(text);
            if text.ends_with("default}") {
                let end = d.text.find(":default").unwrap();
                assert!(definition(&d, range(&d.text, end, end).start).is_none());
            } else {
                assert!(definition(&d, at(&d, "app.name")).is_none(), "{text}");
            }
        }
    }
    #[test]
    fn native_provider_shortcuts_cache_discovery_and_never_crosses_projects_or_profiles() {
        let host = MockHost::install().with_capabilities(&["project.read"]);
        host.add_file("application-prod.properties", "app.name=other-secret");
        let d = doc("app.name=current\nx=${app.name}");
        let mut boot = BootTools::default();
        assert_eq!(boot.definition(&d, at(&d, "app.name")).len(), 1);
        assert!(boot.hover(&d, at(&d, "app.name")).is_some());
        assert!(host.requests().is_empty());
        host.set_project_root(None);
        assert!(definition(&d, at(&d, "app.name")).is_none());
        host.set_project_root(Some("/second"));
        assert!(definition(&d, at(&d, "app.name")).is_none());
    }
    #[test]
    fn standard_properties_only_and_byte_key_reference_caps() {
        let _host = MockHost::install();
        let mut d = doc("app.name=a\nx=${app.name}");
        d.language = "yaml".into();
        assert!(model(&d).is_none());
        d = doc(&"x=1\n".repeat(MAX_KEYS + 1));
        assert!(model(&d).is_none());
        d = doc(&format!("x={}\n", "${a}".repeat(MAX_REFERENCES + 1)));
        assert!(model(&d).is_none());
        d = doc("app.name=a\n#---\nx=${app.name}");
        assert!(model(&d).is_none());
        d = doc(&"x".repeat(MAX_TEXT + 1));
        assert!(model(&d).is_none());
    }
    fn command_host(text: &str, at: Position) -> MockHost {
        let h = MockHost::install().with_capabilities(&["project.read", "editor.read"]);
        h.open_editor(
            "/project/application.properties",
            "properties",
            text,
            &[(at.line, at.character, at.line, at.character)],
        );
        h
    }
    #[test]
    fn project_source_graph_retains_profile_file_provenance_without_values() {
        let text = "app.name=unsaved-secret\nlabel=😀 ${app.name}\n";
        let h = command_host(text, Position::new(0, 3));
        h.add_file(
            "application-dev.properties",
            "app.name=profile-secret\nx=${app.name:fallback}\n",
        );
        h.add_file("other.properties", "app.name=excluded-secret");
        h.answer_next_prompt(Some(&["1"]));
        let result = BootTools::default()
            .run_command("project-placeholder-occurrences", &h.invocation(), None)
            .unwrap();
        assert_eq!(
            h.opened_locations(),
            vec![("/project/application-dev.properties".into(), 1, 4)]
        );
        assert!(result.is_none());
        assert!(h.requests().is_empty() && h.process_runs().is_empty());
    }
    #[test]
    fn project_source_graph_refuses_incomplete_input_and_separates_nested_project() {
        let h = command_host("app.name=one\nx=${app.name}", Position::new(0, 3));
        h.add_file("application-dev.properties", "app.name=one\rapp.name=two");
        assert!(
            BootTools::default()
                .run_command("project-placeholder-occurrences", &h.invocation(), None)
                .is_err()
        );
        assert!(h.opened_locations().is_empty());
        let h = command_host("app.name=one\nx=${app.name}", Position::new(0, 3));
        h.add_project("/project/nested");
        h.add_file("nested/application.properties", "app.name=nested");
        h.answer_next_prompt(Some(&["2"]));
        assert!(
            BootTools::default()
                .run_command("project-placeholder-occurrences", &h.invocation(), None)
                .is_err()
        );
        assert!(h.opened_locations().is_empty());
    }

    #[test]
    fn public_current_file_picker_navigates_exact_utf16_unsaved_occurrences_both_directions() {
        let text =
            "app.name=private-secret\r\nlabel=😀 ${app.name:fallback}\r\nother=${app.name}\r\n";
        let h = command_host(text, Position::new(0, 3));
        h.type_in_editor("# unsaved revision\r\n");
        h.add_file(
            "other/application.properties",
            "app.name=another-private-value",
        );
        h.answer_next_prompt(Some(&["1"]));
        let mut tools = BootTools::default();
        let result = tools
            .run_command(
                "current-file-placeholder-occurrences",
                &h.invocation(),
                None,
            )
            .unwrap();
        assert_eq!(
            h.opened_locations(),
            vec![("/project/application.properties".into(), 1, 11)]
        );
        assert!(result.is_none());
        let h = command_host(text, Position::new(1, 14));
        h.answer_next_prompt(Some(&["0"]));
        tools
            .run_command(
                "current-file-placeholder-occurrences",
                &h.invocation(),
                None,
            )
            .unwrap();
        assert_eq!(
            h.opened_locations(),
            vec![("/project/application.properties".into(), 0, 0)]
        );
        assert!(h.process_runs().is_empty() && h.requests().is_empty());
    }
    #[test]
    fn unsupported_files_get_a_plain_reason_and_menus_need_a_properties_editor() {
        let h = command_host("#---\napp.name=a", Position::new(1, 3));
        for command in ["current-file-placeholder-occurrences", "project-placeholder-occurrences"] {
            let error = BootTools::default().run_command(command, &h.invocation(), None).unwrap_err().to_string();
            assert!(error.contains(UNSUPPORTED_FILE), "{error}");
        }
        let manifest = include_str!("../plugin.toml");
        assert_eq!(manifest.matches("when = \"editorLangId == properties\"").count(), 4);
    }
    #[test]
    fn cancellation_unknown_selection_denied_editor_and_foreign_owner_never_navigate() {
        for answer in [None, Some(&["999"][..])] {
            let h = command_host("app.name=secret\nx=${app.name}", Position::new(0, 3));
            h.answer_next_prompt(answer);
            let _ = BootTools::default().run_command(
                "current-file-placeholder-occurrences",
                &h.invocation(),
                None,
            );
            assert!(h.opened_locations().is_empty());
        }
        let h = command_host("app.name=secret", Position::new(0, 3))
            .with_capabilities(&["project.read"]);
        let denied = BootTools::default()
            .run_command("current-file-placeholder-occurrences", &h.invocation(), None)
            .unwrap_err()
            .to_string();
        assert!(denied.contains("Find Property needs the editor.read permission"), "{denied}");
        assert!(h.opened_locations().is_empty());
        let h = command_host("app.name=secret", Position::new(0, 3));
        h.add_project("/project");
        h.set_project_root(Some("/other"));
        assert!(
            BootTools::default()
                .run_command(
                    "current-file-placeholder-occurrences",
                    &h.invocation(),
                    None
                )
                .is_err()
        );
        assert!(h.opened_locations().is_empty());
    }
    #[test]
    fn ambiguous_escaped_dynamic_multidoc_and_over_budget_source_do_not_offer_owned_occurrences() {
        for text in [
            "app.name=a\napp.name=b\nx=${app.name}",
            "app\\.name=a\nx=${app.name}",
            "appName=a\nx=${app-name}",
            "#---\napp.name=a\nx=${app.name}",
            "app.name=a\nx=${${selector}.name}",
        ] {
            let needle = if text.contains("selector") {
                "selector"
            } else if text.contains("app-name") {
                "app-name"
            } else {
                "app.name"
            };
            let off = text.rfind(needle).unwrap();
            let at = range(text, off, off).start;
            let h = command_host(text, at);
            assert!(
                BootTools::default()
                    .run_command(
                        "current-file-placeholder-occurrences",
                        &h.invocation(),
                        None
                    )
                    .is_err(),
                "{text}"
            );
            assert!(h.opened_locations().is_empty());
        }
        let many = format!(
            "app.name=a\nx={}\n",
            "${app.name}".repeat(MAX_REFERENCES + 1)
        );
        let h = command_host(&many, Position::new(0, 2));
        assert!(occurrences(&h.invocation()).is_err());
        let h = command_host(&"x".repeat(MAX_TEXT + 1), Position::new(0, 0));
        assert!(occurrences(&h.invocation()).is_err());
    }
    #[test]
    fn post_picker_current_text_version_path_and_root_guards_refuse_stale_source() {
        let h = command_host("app.name=secret\nx=${app.name}", Position::new(0, 3));
        let (root, initial, _) = live_source(&h.invocation()).unwrap();
        h.type_in_editor("# changed");
        assert!(unchanged(&h.invocation(), &root, &initial).is_err());
        let h = command_host(&initial.text, Position::new(0, 3));
        h.open_editor(
            "/project/other.properties",
            "properties",
            &initial.text,
            &[(0, 3, 0, 3)],
        );
        assert!(unchanged(&h.invocation(), &root, &initial).is_err());
        let h = command_host(&initial.text, Position::new(0, 3));
        h.add_project("/project");
        h.set_project_root(Some("/other"));
        assert!(unchanged(&h.invocation(), &root, &initial).is_err());
        assert!(h.opened_locations().is_empty());
    }
}
