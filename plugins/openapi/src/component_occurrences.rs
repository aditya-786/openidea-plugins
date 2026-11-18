//! Read-only current-document component occurrences. No claims about incoming
//! cross-file references and no automatic string replacement/host Rename.
use crate::*;
pub(crate) fn run(ctx: &Invocation) -> Result<Option<String>, Error> {
    let editor = ctx
        .editor()
        .ok_or("Open an OpenAPI document and select a component key or local reference.")?;
    let path = editor.path();
    let document = editor.document()?;
    let caret = editor
        .selections()?
        .first()
        .map(|s| s.start)
        .ok_or(Error::Stale)?;
    if !is_spec_file(&path) {
        return Err(Error::Invalid("Select an OpenAPI document.".into()));
    }
    let proof = pointer_status::Proof::parse(&document.text)
        .ok_or("Couldn't read this document as an OpenAPI spec. Fix syntax errors and try again.")?;
    let at = offset(&document.text, caret).ok_or(Error::Stale)?;
    let occurrences = proof
        .component_occurrences(at)
        .ok_or("Select a known component key or a supported local component reference.")?;
    let items: Vec<_> = occurrences
        .iter()
        .enumerate()
        .map(|(i, (label, range))| {
            openidea_plugin::PickItem::new(
                &i.to_string(),
                &format!("{label} · line {}", range.start.line + 1),
            )
            .description("Current document only; external incoming references are not searched")
        })
        .collect();
    let Some(choice) = host::pick("OpenAPI component occurrences in this document", &items)? else {
        return Ok(None);
    };
    let destination = occurrences
        .get(choice.parse::<usize>().map_err(|_| Error::Stale)?)
        .ok_or(Error::Stale)?
        .1
        .start;
    let current = ctx.editor().ok_or(Error::Stale)?;
    let snapshot = current.document()?;
    if current.path() != path
        || snapshot.version != document.version
        || snapshot.text != document.text
    {
        return Err(Error::Stale);
    }
    host::open_location(&path, destination.line, destination.character)?;
    Ok(None)
}

/// Physical same-project incoming references, including JSON/YAML fragments.
/// This remains read-only; partial discovery is shown explicitly in the picker.
pub(crate) fn project_usages(ctx: &Invocation) -> Result<Option<String>, Error> {
    let editor = ctx.editor().ok_or("Select an OpenAPI component or component reference.")?;
    let doc = editor.document()?; let path = editor.path();
    let root = comparison_owner(ctx, &path)?;
    let project = ctx.project().ok_or(Error::Stale)?;
    let proof = pointer_status::Proof::parse(&doc.text).ok_or("Couldn't read this document as an OpenAPI spec. Fix syntax errors and try again.")?;
    let caret = editor.selections()?.first().map(|range| range.start).ok_or(Error::Stale)?;
    let at = offset(&doc.text, caret).ok_or(Error::Stale)?;
    let reference = proof.navigation_ref_at(at).map(|reference| reference.target)
        .or_else(|| proof.copy_pointer_at(at)).ok_or("Put the caret on a component declaration or reference.")?;
    let (file, pointer) = reference.split_once('#').ok_or("Select a supported component pointer.")?;
    let target = physical_target(&path, file).ok_or("Remote and encoded references have no usages in project.")?;
    if comparison_owner(ctx, &target)? != root { return Err(Error::Stale); }
    let target_text = project.read_file(&target)?;
    let target_proof = pointer_status::Proof::parse(&target_text).ok_or("Couldn't read the file this $ref points to as an OpenAPI spec.")?;
    let base = target_proof.component_base(pointer).ok_or("This $ref doesn't point to a component.")?;
    let declaration = target_proof.pointer_location(&base).ok_or(Error::Stale)?;
    let mut locations = vec![(target.clone(), declaration, std::sync::Arc::new(target_text.clone()), "Component declaration")];
    let mut files = std::collections::BTreeSet::new(); files.insert(path.clone()); files.insert(target.clone());
    let mut partial = 0usize;
    for glob in ["**/*.json", "**/*.yaml", "**/*.yml"] {
        let found = project.find_files(glob, 501)?;
        if found.len() >= 501 { partial += 1; }
        files.extend(found);
    }
    let mut bytes = 0usize;
    for file in files {
        if comparison_owner(ctx, &file).ok().as_deref() != Some(root.as_str()) { partial += 1; continue; }
        let text = if file == path { doc.text.clone() } else { match project.read_file(&file) { Ok(text) => text, Err(_) => {partial += 1; continue;} } };
        bytes = bytes.saturating_add(text.len());
        if bytes > 16 * 1024 * 1024 { partial += 1; break; }
        let Some(proof) = pointer_status::Proof::parse(&text) else {partial += 1; continue;};
        let Some(references) = proof.physical_references() else {partial += 1; continue;};
        let text = std::sync::Arc::new(text);
        for reference in references {
            let Some((relative, pointer)) = reference.target.split_once('#') else {continue;};
            if physical_target(&file, relative).as_deref() != Some(target.as_str()) { continue; }
            if pointer != base && !pointer.starts_with(&format!("{base}/")) {continue;}
            locations.push((file.clone(), spring_text::range(&text, reference.at, reference.end), text.clone(), "$ref"));
            if locations.len() > 512 {return Err("More than 512 component usages; narrow the project before navigating.".into());}
        }
    }
    let items: Vec<_> = locations.iter().enumerate().map(|(i,(path,range,_,kind))| PickItem::new(&i.to_string(), &format!("{kind} · {}:{}", path.strip_prefix(&format!("{root}/")).unwrap_or(path), range.start.line+1))).collect();
    let title = if partial == 0 { "OpenAPI component usages in project".into() } else {format!("OpenAPI component usages — partial ({partial} unreadable, unsupported or limited sources)")};
    let Some(choice) = host::pick(&title, &items)? else {return Ok(None);};
    let (selected, range, before, _) = locations.get(choice.parse::<usize>().map_err(|_|Error::Stale)?).ok_or(Error::Stale)?;
    let current = ctx.editor().ok_or(Error::Stale)?; let fresh = current.document()?;
    if current.path() != path || fresh.version != doc.version || fresh.text != doc.text
        || comparison_owner(ctx, selected)? != root || project.read_file(selected)?.as_str() != before.as_str() || project.read_file(&target)? != target_text { return Err(Error::Stale); }
    host::open_location(selected,range.start.line,range.start.character)?; Ok(None)
}

pub(crate) fn physical_target(from: &str, file: &str) -> Option<String> {
    if file.contains([':', '%', '\\', '?']) || file.chars().any(char::is_control) || file.starts_with('/') {return None;}
    Some(if file.is_empty() {from.into()} else {resolve_path(from,file)})
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    #[test]
    fn project_usages_follow_physical_file_identity_and_array_refs() {
        let h = MockHost::install();
        let text = r##"{"openapi":"3.0.3","components":{"schemas":{"Foo":{},"Foo2":{}}},"paths":{}}"##;
        let client = r##"{"x":[{"$ref":"./openapi.json#/components/schemas/Foo"}],"other":{"$ref":"./other.json#/components/schemas/Foo"},"prefix":{"$ref":"./openapi.json#/components/schemas/Foo2"}}"##;
        h.add_file("/project/openapi.json",text);h.add_file("/project/client.json",client);
        h.add_file("/project/other.json",text);
        let at = position(text,text.find("Foo").unwrap());h.open_editor("/project/openapi.json","json",text,&[(at.line,at.character,at.line,at.character)]);
        h.answer_next_prompt(Some(&["1"]));OpenApi::default().run_command("component-project-usages",&h.invocation(),None).unwrap();
        let expected = position(client,client.find("./openapi.json").unwrap());
        assert_eq!(h.opened_locations(),[("/project/client.json".into(),expected.line,expected.character)]);
        assert!(h.process_runs().is_empty() && h.requests().is_empty());
    }
    #[test]
    fn incoming_reference_can_select_target_declaration_and_remote_refs_are_refused() {
        let h = MockHost::install();let target = r##"{"openapi":"3.0.3","components":{"schemas":{"Foo":{}}}}"##;
        let client = r##"{"x":{"$ref":"./openapi.json#/components/schemas/Foo"}}"##;
        h.add_file("/project/openapi.json",target);h.add_file("/project/client.json",client);
        let at=position(client,client.find("./openapi").unwrap());h.open_editor("/project/client.json","json",client,&[(at.line,at.character,at.line,at.character)]);
        h.answer_next_prompt(Some(&["0"]));project_usages(&h.invocation()).unwrap();
        let expected=position(target,target.find("Foo").unwrap());assert_eq!(h.opened_locations(),[("/project/openapi.json".into(),expected.line,expected.character)]);
        for remote in ["https://example.com/api.json","file:other.json","../outside/other.json","other%20file.json"] {
            assert!(physical_target("/project/openapi.json",remote).is_none() || !physical_target("/project/openapi.json",remote).unwrap().starts_with("/project/"));
        }
    }
    #[test]
    fn registered_command_uses_unsaved_source_and_refuses_forged_selection() {
        let h = MockHost::install();
        let text = r##"{"openapi":"3.1.1","components":{"schemas":{"Foo":{}}},"a":{"$ref":"#/components/schemas/Foo"}}"##;
        let at = position(text, text.find("Foo").unwrap());
        h.open_editor(
            "/project/openapi.json",
            "json",
            text,
            &[(at.line, at.character, at.line, at.character)],
        );
        h.answer_next_prompt(Some(&["1"]));
        OpenApi::default()
            .run_command("component-occurrences", &h.invocation(), None)
            .unwrap();
        let location = position(text, text.find("#/components/schemas/Foo").unwrap());
        assert_eq!(
            h.opened_locations().last(),
            Some(&(
                String::from("/project/openapi.json"),
                location.line,
                location.character
            ))
        );
        h.answer_next_prompt(Some(&["999"]));
        assert!(
            OpenApi::default()
                .run_command("component-occurrences", &h.invocation(), None)
                .is_err()
        );
        assert_eq!(h.opened_locations().len(), 1);
        assert!(h.process_runs().is_empty() && h.requests().is_empty());
    }
}
