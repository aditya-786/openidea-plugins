//! A deliberately small FileActions subset: a plain literal view creates one
//! conventional Blade file. No extraction, variable inference or runtime code.
use crate::{Main, index::Index, php, refs};
use openidea_plugin::{
    Error, FileKind, NotificationLevel, Project, Range,
    file_actions::{FileAction, FileActions, FileOperation},
    host,
    providers::Document,
};
use serde_json::{Value, json};
#[path="view_root_selection.rs"]
pub(super) mod roots;
const MAX_TEXT: usize = 128 * 1024;
const CONTENT: &str = "<div>\n</div>\n";
fn fingerprint(text: &str) -> String {
    format!("{:016x}", text.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100000001b3)))
}
pub(super) fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && !name.split('.').next().is_some_and(|part| part.eq_ignore_ascii_case("vendor"))
        && name.len() <= 128
        && name.split('.').count() <= 16
        && name.split('.').all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')))
}
pub(super) fn owner(doc: &Document, expected: Option<&str>) -> Result<(Project, String), Error> {
    let project = host::project_of(&doc.path).ok_or(crate::ui_text::text("native.view_creation.f734482e07", "The document has no owning project."))?;
    let root = project.root()?;
    if expected.is_some_and(|expected| expected != root)
        || !root.starts_with('/') || root=="/" || root.ends_with('/') || root.contains("//") || root.contains('\\') || root.split('/').any(|p|matches!(p,"."|".."))
        || root.is_empty()
        || root.len() > 4096
        || doc.path.len() > 4096
        || root.chars().any(char::is_control)
        || !doc.path.starts_with(&format!("{}/", root.trim_end_matches('/')))
        || doc.path.split('/').any(|part| matches!(part, "." | ".."))
        || doc.path.contains('\\')
        || doc.path.chars().any(char::is_control)
    {
        return Err(Error::Stale);
    }
    Ok((project, root))
}
fn literal(doc: &Document, at: usize, index: &Index) -> Option<(String, usize, usize)> {
    if doc.text.len() > MAX_TEXT || !doc.text.is_char_boundary(at) {
        return None;
    }
    let language = crate::language_of(&doc.path, &doc.language)?;
    if !matches!(language, "php" | "blade") || doc.text.contains("<<<") {
        return None;
    }
    let reference = refs::references(language, &doc.text)
        .into_iter()
        .find(|r| r.closed && r.kind == refs::Kind::View && r.start <= at && at <= r.end)?;
    let name = doc.text.get(reference.start..reference.end)?;
    let Some((namespace,_))=roots::split(name) else{return None;};
    if namespace.is_some_and(|namespace|!roots::known_namespace(index,doc,namespace)) || index.views.contains_key(name) {
        return None;
    }
    let quote = reference.start.checked_sub(1)?;
    let quoted_end = reference.end + 1;
    let before = doc.text[..quote].trim_end();
    let open = before.len().checked_sub(1)?;
    if doc.text.as_bytes().get(open) != Some(&b'(') {
        return None;
    }
    let arguments = php::arguments(&doc.text, open);
    let first = php::trimmed(&doc.text, arguments.first()?.clone());
    if first != (quote..quoted_end) || php::literal(&doc.text, first)?.0 != name {
        return None;
    }
    let callee = doc.text[..open].trim_end();
    let end = callee.len();
    let start = end - callee.bytes().rev().take_while(|b| b.is_ascii_alphanumeric() || *b == b'_').count();
    let function = &callee[start..];
    if language == "blade" {
        if start == 0
            || callee.as_bytes()[start - 1] != b'@'
            || !matches!(function, "include" | "includeIf" | "extends")
            || index.directives.contains_key(function)
        {
            return None;
        }
        let custom = index.directives.keys().map(String::as_str).collect();
        crate::directives::ranking(&doc.text, end, start, end, &custom)?;
    } else {
        if function != "view" {
            return None;
        }
        let masked = php::mask_comments(&doc.text);
        if masked.get(start..end) != Some("view") {
            return None;
        }
        let compact: String = masked.chars().filter(|c| !c.is_whitespace()).collect();
        if compact.contains("usefunction") || compact.contains("functionview(") || compact.contains("function&view(") {
            return None;
        }
        let prefix = &callee[..start];
        if prefix.ends_with([':', '>']) || prefix.trim_end().ends_with("new") {
            return None;
        }
        if let Some(prefix) = prefix.strip_suffix('\\') {
            if prefix.chars().next_back().is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '\\') {
                return None;
            }
        }
    }
    Some((name.into(), reference.start, reference.end))
}
pub(super) fn missing(project: &Project, path: &str) -> Result<(), Error> {
    let expected_root=project.root()?;
    if host::project_of(path).and_then(|p|p.root().ok()).as_deref()!=Some(expected_root.as_str()){return Err(Error::Stale);}
    match project.stat(path) {
        Ok(_) => return Err(Error::Invalid(crate::ui_text::format("view.destination-exists", "Destination/alternate already exists: {path}", &[("path", path)]))),
        Err(Error::NotFound(_)) => {}
        Err(error) => return Err(error),
    }
    match project.read_file(path) {
        Err(Error::NotFound(_)) => Ok(()),
        Ok(_) => Err(Error::Invalid(crate::ui_text::format("view.destination-open", "Destination is open or already exists: {path}", &[("path", path)]))),
        Err(error) => Err(error),
    }
}
fn configuration(project: &Project, root: &str) -> Result<String, Error> {
    if !host::setting_string("viewPaths").unwrap_or_default().trim().is_empty() {
        return Err(crate::ui_text::text("native.view_creation.8ec3fd0006", "Configured extra view roots are outside this creation subset.").into());
    }
    // Absence is only evidence about this app when the inspected configuration
    // path still belongs to it, including paths under another open project.
    for file in ["ide-blade.json", ".idea/ide-blade.json", "config/view.php"] {
        let path = format!("{root}/{file}");
        if host::project_of(&path).and_then(|owner| owner.root().ok()).as_deref() != Some(root) {
            return Err(Error::Stale);
        }
    }
    for file in ["ide-blade.json", ".idea/ide-blade.json"] {
        match project.stat(&format!("{root}/{file}")) {
            Err(Error::NotFound(_)) => {}
            Ok(_) => return Err(crate::ui_text::text("native.view_creation.5d094174ac", "ide-blade.json view ownership requires a separate namespace/root chooser.").into()),
            Err(error) => return Err(error),
        }
    }
    match project.read_file(&format!("{root}/config/view.php")) {
        Err(Error::NotFound(_)) => Ok("default".into()),
        Err(error) => Err(error),
        Ok(text) => {
            if text.len() > 64 * 1024 {
                return Err(crate::ui_text::text("native.view_creation.6937b31822", "View configuration exceeds the supported bound.").into());
            }
            let masked = php::mask_comments(&text);
            let returned = php::returned_array(&masked).ok_or(crate::ui_text::text("native.view_creation.2b5d971c57", "No literal returned view configuration array."))?;
            let prefix: String = masked[..returned].chars().filter(|c| !c.is_whitespace()).collect();
            if prefix != "<?phpreturn" {
                return Err(crate::ui_text::text("native.view_creation.1ede6f789a", "Executable or namespaced view configuration is outside this static subset.").into());
            }
            let returned_end = php::closing(&masked, returned).ok_or(crate::ui_text::text("native.view_creation.ee64e8afc8", "Incomplete returned view configuration."))?;
            let suffix: String = masked[returned_end + 1..].chars().filter(|c| !c.is_whitespace()).collect();
            if !matches!(suffix.as_str(), ";" | ";?>") {
                return Err(crate::ui_text::text("native.view_creation.1cfe45b504", "View configuration has unsupported trailing code.").into());
            }
            let mut paths = None;
            let mut seen = std::collections::BTreeSet::new();
            for item in php::items(&masked, returned) {
                if masked[item.value.clone()].trim().is_empty() && item.key.is_none() {
                    continue;
                }
                let key = item
                    .key
                    .and_then(|key| php::literal(&masked, key))
                    .map(|(key, _)| key)
                    .ok_or(crate::ui_text::text("native.view_creation.94e0ca92f2", "Computed/spread configuration keys are unsupported."))?;
                if !seen.insert(key) {
                    return Err(crate::ui_text::text("native.view_creation.503dd83080", "Ambiguous view configuration keys.").into());
                }
                if key == "paths" {
                    paths = Some(php::trimmed(&masked, item.value));
                }
            }
            let paths = paths.ok_or(crate::ui_text::text("native.view_creation.5d610aed1f", "No literal view paths declaration."))?;
            let open = paths.start;
            if masked.as_bytes().get(open) != Some(&b'[') {
                return Err(crate::ui_text::text("native.view_creation.fe0ec38aad", "Dynamic view paths are unsupported.").into());
            }
            let close = php::closing(&masked, open).ok_or(crate::ui_text::text("native.view_creation.9c6f10a547", "Incomplete view paths declaration."))?;
            if close + 1 != paths.end {
                return Err(crate::ui_text::text("native.view_creation.57daa56fa6", "Computed view paths are unsupported.").into());
            }
            let values = php::split(&masked, open + 1..close);
            let values: Vec<_> = values.into_iter().filter(|value| !masked[value.clone()].trim().is_empty()).collect();
            if values.len() != 1 {
                return Err(crate::ui_text::text("native.view_creation.b62727106f", "Multiple view roots require an explicit root chooser.").into());
            }
            let path: String = masked[values[0].clone()].chars().filter(|c| !c.is_whitespace()).collect();
            if !matches!(path.as_str(), "resource_path('views')" | "resource_path(\"views\")") {
                return Err(crate::ui_text::text("native.view_creation.31cde8da70", "The declared view root is not the supported resources/views convention.").into());
            }
            Ok(fingerprint(&text))
        }
    }
}
pub(super) fn validate(project: &Project, root: &str, doc: &Document, name: &str) -> Result<(String, String), Error> {
    if project.read_file(&doc.path)? != doc.text {
        return Err(Error::Stale);
    }
    let config = configuration(project, root)?;
    if project.stat(&format!("{root}/resources/views"))?.kind != FileKind::Directory {
        return Err(crate::ui_text::text("native.view_creation.ae5317ba17", "The conventional resources/views root must be an existing plain directory.").into());
    }
    let relative = name.replace('.', "/");
    let target = format!("{root}/resources/views/{relative}.blade.php");
    for directory in ["resources/views", "app/views", "resources/templates"] {
        for extension in ["blade.php", "php"] {
            missing(project, &format!("{root}/{directory}/{relative}.{extension}"))?;
        }
    }
    let mut parent = root.to_owned();
    let relative = target.strip_prefix(&format!("{root}/")).ok_or(Error::Stale)?;
    let parts: Vec<_> = relative.split('/').collect();
    for part in &parts[..parts.len() - 1] {
        parent.push('/');
        parent.push_str(part);
        match project.stat(&parent) {
            Ok(stat) if stat.kind == FileKind::Directory => {}
            Err(Error::NotFound(_)) => {}
            Ok(_) => return Err(crate::ui_text::text("native.view_creation.ca95572f49", "A parent path is not a plain directory.").into()),
            Err(error) => return Err(error),
        }
    }
    Ok((target, config))
}
/// The Create Missing command's path to the same view: confirm the destination, then write it.
pub(crate) fn create_from_command(index: &Index, doc: &Document, at: usize) -> Result<(), Error> {
    let (name, _, _) = literal(doc, at, index).ok_or_else(|| Error::Invalid(crate::ui_text::t(
        "native.view_creation.commandUnsupported",
        "Only views named by view('…'), @include, @includeIf or @extends can be created here.",
    )))?;
    let (project, root) = owner(doc, None)?;
    let (target, config) = roots::select(&project, &root, doc, &name)?;
    if !host::confirm(
        &crate::ui_text::t("native.view_creation.commandConfirm", "Create view?"),
        &crate::ui_text::format("native.view_creation.commandBody", "Create {target} for the view {name}?", &[("target", &target), ("name", &name)]),
    )? {
        return Ok(());
    }
    let (current, current_config) = roots::revalidate(&project, &root, doc, &name, &target, &config)?;
    if current != target || current_config != config {
        return Err(Error::Stale);
    }
    project.write_file(&target, CONTENT).map_err(|error| ui_kit::denied::map(error, "Creating views", "project.write", "Laravel"))?;
    host::open_editor(&target, None)
}
impl FileActions for Main {
    fn file_actions(&mut self, doc: &Document, selection: Range) -> Vec<FileAction> {
        let mut found=self.generator_workspace.actions(doc);
        found.extend(crate::vendor_overrides::actions(self,doc,selection));
        found.extend(crate::bulk::actions(self,doc));
        found.extend(crate::request_promotion::actions(self,doc,selection));
        found.extend(crate::request_creation::actions(self,doc,selection));
        found.extend(crate::feature_test::actions(self,doc,selection));
        found.extend(crate::view_extract::actions(self,doc,selection));
        let missing = (|| {
        let Ok((_, root)) = owner(doc, None) else {
            return Vec::new();
        };
        let Some(index) = self.indexes.get(&root).filter(|index| !index.partial) else {
            return Vec::new();
        };
        let Some(at) = openidea_plugin::text::offset(&doc.text, selection.start) else {
            return Vec::new();
        };
        let Some((name, start, end)) = literal(doc, at, index) else {
            return Vec::new();
        };
        let Some(selection_end) = openidea_plugin::text::offset(&doc.text, selection.end) else {
            return Vec::new();
        };
        if selection_end < start || selection_end > end {
            return Vec::new();
        }
        let data = json!([root, doc.path, doc.version, fingerprint(&doc.text), name, start, end]).to_string();
        if data.len() > 8192 {
            return Vec::new();
        }
        vec![FileAction {
            title: crate::ui_text::format("view.create-action", "Create missing literal Blade view: {name}", &[("name", &name)]),
            kind: Some("quickfix".into()),
            data: Some(data),
            ..Default::default()
        }]
        })();
        found.extend(missing);
        if let Some(language)=crate::language_of(&doc.path,&doc.language) {
            found.extend(crate::lang_actions::locale_actions(doc,language,selection));
        }
        if let Some(index)=self.index_for(&doc.path) { found.extend(crate::model_actions::actions(index,doc)); found.extend(crate::request_dto::actions(index,doc)); }
        if let Some(index)=self.index_for(&doc.path) { found.extend(crate::package_integrations::actions(index,doc,selection)); found.extend(crate::class_component_rename::actions(index,doc,selection)); found.extend(crate::member_refactor::actions(index,doc,selection)); found.extend(crate::semantic_refs::actions(index,doc,selection)); found.extend(crate::view_rename::actions(index,doc,selection)); }
        found
    }
    fn resolve_file_action(&mut self, doc: &Document, action: FileAction) -> Result<FileAction, Error> {
        if action.data.as_deref().is_some_and(|key|key.starts_with("livewire-action:")) {
            let index=self.index_for(&doc.path).ok_or(Error::Stale)?;
            return crate::package_integrations::resolve(index,doc,action);
        }
        if action.data.as_deref().is_some_and(|key|key.starts_with("class-component-rename:")) {
            let index=self.index_for(&doc.path).ok_or(Error::Stale)?;
            return crate::class_component_rename::resolve(index,doc,action);
        }
        if action.data.as_deref().is_some_and(|key|key.starts_with("member-rename:")) {
            let index=self.index_for(&doc.path).ok_or(Error::Stale)?;
            return crate::member_refactor::resolve(index,doc,action);
        }
        if action.data.as_deref().is_some_and(|key|key.starts_with("request-dto:")) {
            let index=self.index_for(&doc.path).ok_or(Error::Stale)?;
            return crate::request_dto::resolve(index,doc,action);
        }
        if action.data.as_deref().is_some_and(|key|key.starts_with("view-rename:")) {
            let index = self.index_for(&doc.path).ok_or(Error::Stale)?;
            return crate::view_rename::resolve(index, doc, action);
        }
        if action.data.as_deref().is_some_and(|key|key.starts_with("semantic-rename:")) {
            let index = self.index_for(&doc.path).ok_or(Error::Stale)?;
            return crate::semantic_refs::resolve(index, doc, action);
        }
        if action.data.as_deref().is_some_and(|key|key.starts_with("model-native:")) {
            let index=self.index_for(&doc.path).ok_or(Error::Stale)?;
            return crate::model_actions::resolve_current(self,index,doc,action);
        }
        if action.data.as_deref().is_some_and(|key|key.starts_with("generator-apply:") || key.starts_with("generator-presets:") || key.starts_with("existing-model-fields:")){
            if let Some(model)=self.generator_workspace.prepared_model(doc,&action) {
                let (project,_)=owner(doc,None)?;
                let index=self.index_for(&doc.path).filter(|index|!index.partial).ok_or(Error::Stale)?;
                let (_,path)=crate::eloquent_members::model_identities(&index.eloquent_models).into_iter().find(|(full,_)|full==&model).ok_or(Error::Stale)?;
                let runner=self.artisan_workspace.runner_for_project(&project)?;
                let snapshot=self.runtime_inspection.helper_schema(&project,runner,&model,&path)?;
                self.generator_workspace.set_model_schema_for_project(&project,&model,snapshot)?;
            }
            return self.generator_workspace.resolve(doc,action);
        }
        if action.data.as_deref().is_some_and(|key|key.starts_with("locale-json:") || key.starts_with("locale-php:")){return crate::lang_actions::resolve_locale(doc,action);}
        if crate::vendor_overrides::handles(&action){return crate::vendor_overrides::resolve(self,doc,action);}
        if action.data.as_deref().is_some_and(|key|key.starts_with("bulk-phpdocs:") || key.starts_with("bulk-generate:")){return crate::bulk::resolve(self,doc,action);}
        if action.data.as_deref().is_some_and(|key|key.starts_with("request-promote:")){return crate::request_promotion::resolve(self,doc,action);}
        if action.data.as_deref().is_some_and(|key|key.starts_with("feature-test:")){return crate::feature_test::resolve(self,doc,action);}
        if action.data.as_deref().is_some_and(|key|key.starts_with("request-create:")){return crate::request_creation::resolve(self,doc,action);}
        if action.data.as_deref().is_some_and(|key|key.starts_with("extract-view:")){return crate::view_extract::resolve(self,doc,action);}
        let result = (|| {
            let data: Value = serde_json::from_str(action.data.as_deref().filter(|data| data.len() <= 8192).ok_or(Error::Stale)?)
                .map_err(|_| Error::Stale)?;
            let data = data.as_array().filter(|data| data.len() == 7).ok_or(Error::Stale)?;
            if doc.text.len() > MAX_TEXT
                || data[1].as_str() != Some(&doc.path)
                || data[2].as_u64() != Some(doc.version)
                || data[3].as_str() != Some(&fingerprint(&doc.text))
            {
                return Err(Error::Stale);
            }
            let (project, root) = owner(doc, Some(data[0].as_str().ok_or(Error::Stale)?))?;
            let index = self.indexes.get(&root).filter(|index| !index.partial).ok_or(Error::Stale)?;
            let at = data[5].as_u64().and_then(|n| usize::try_from(n).ok()).ok_or(Error::Stale)?;
            let (name, start, end) = literal(doc, at, index).ok_or(Error::Stale)?;
            if data[4].as_str() != Some(&name) || data[5].as_u64() != Some(start as u64) || data[6].as_u64() != Some(end as u64) {
                return Err(Error::Stale);
            }
            let (target, config) = roots::select(&project, &root, doc, &name)?;
            let preview = crate::ui_text::format("view.create-review", "Project: {root}\nSource: {source} (editor version {version})\nLiteral: {literal}\n\nCreate (no overwrite): {target}\n\nExact new contents:\n{content}\nOne undoable file creation. The source is unchanged; no imports, variables, extraction or PHP/Artisan/process execution is inferred.", &[("root", &root), ("source", &doc.path), ("version", &doc.version.to_string()), ("literal", &json!(name).to_string()), ("target", &target), ("content", CONTENT)]);
            if !ui_kit::prompts::confirm_exact(crate::ui_text::text("view.create-confirm", "Create missing literal Blade view?"), &preview)? {
                return Err(Error::Cancelled);
            }
            owner(doc, Some(&root))?;
            let (current, current_config) = roots::revalidate(&project, &root, doc, &name,&target,&config)?;
            if target != current || config != current_config {
                return Err(Error::Stale);
            }
            Ok(FileAction {
                title: crate::ui_text::format("view.create-action", "Create missing literal Blade view: {name}", &[("name", &name)]),
                kind: Some("quickfix".into()),
                operations: vec![FileOperation::Create { path: target.clone(), text: CONTENT.into(), overwrite: false }],
                open: Some(target),
                ..Default::default()
            })
        })();
        if let Err(error) = &result {
            if *error != Error::Cancelled {
                host::notify(NotificationLevel::Warning, crate::ui_text::text("native.view_creation.8a3676cb28", "Laravel view creation"), &crate::ui_text::format("view.create-failed", "View was not created: {error}", &[("error", &error.to_string())]));
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{Position, mock::MockHost};
    fn setup(text: &str, blade: bool) -> (MockHost, Main, Document, Range) {
        let host = MockHost::install().with_capabilities(&["project.read", "storage", "editor.read"]);
        host.add_dir("resources/views");
        let path = if blade { "/project/resources/views/main.blade.php" } else { "/project/app/Http/Controllers/A.php" };
        let language = if blade { "blade" } else { "php" };
        host.add_file(path, text);
        host.open_editor(path, language, text, &[]);
        let document = Document { path: path.into(), language: language.into(), text: text.into(), version: 1 };
        let at = text.find("posts.create").map_or(0, |n| n + 2);
        let pos = openidea_plugin::text::position(text, at);
        let mut main = Main::default();
        main.indexes.insert("/project".into(), Index::default());
        (host, main, document, Range::new(pos, pos))
    }
    fn action(main: &mut Main, doc: &Document, range: Range) -> FileAction {
        main.file_actions(doc, range).into_iter().next().expect("literal missing-view action")
    }
    #[test]
    fn php_and_blade_literals_plan_one_non_overwrite_atomic_creation_without_execution() {
        for (source, blade) in
            [("<?php return view('posts.create');", false), ("@include('posts.create')", true), ("@extends('posts.create')", true)]
        {
            let (host, mut main, doc, range) = setup(source, blade);
            let lazy = action(&mut main, &doc, range);
            assert!(lazy.operations.is_empty());
            host.answer_next_prompt(Some(&["ok"]));
            let ready = main.resolve_file_action(&doc, lazy).unwrap();
            assert!(ready.edits.is_empty());
            assert_eq!(
                ready.operations,
                vec![FileOperation::Create {
                    path: "/project/resources/views/posts/create.blade.php".into(),
                    text: CONTENT.into(),
                    overwrite: false
                }]
            );
            assert_eq!(ready.open.as_deref(), Some("/project/resources/views/posts/create.blade.php"));
            assert!(
                host.invocation().project().unwrap().read_file("/project/resources/views/posts/create.blade.php").is_err(),
                "only host application creates files"
            );
            assert!(host.process_runs().is_empty());
            assert!(host.terminals().is_empty());
            assert!(host.requests().is_empty());
        }
    }
    #[test]
    fn cancelled_preview_and_stale_editor_snapshot_return_no_operations() {
        let (host, mut main, doc, range) = setup("<?php return view('posts.create');", false);
        let lazy = action(&mut main, &doc, range);
        assert_eq!(main.resolve_file_action(&doc, lazy.clone()), Err(Error::Cancelled));
        let mut changed = doc.clone();
        changed.version += 1;
        assert_eq!(main.resolve_file_action(&changed, lazy.clone()), Err(Error::Stale));
        let mut changed = doc.clone();
        changed.text.push(' ');
        assert_eq!(main.resolve_file_action(&changed, lazy.clone()), Err(Error::Stale));
        host.open_editor(&doc.path, &doc.language, "<?php return view('other');", &[]);
        assert_eq!(main.resolve_file_action(&doc, lazy), Err(Error::Stale));
        assert!(host.process_runs().is_empty());
    }
    #[test]
    fn unsaved_source_and_utf16_selection_are_validated_against_current_buffer() {
        let (host, mut main, mut doc, _) = setup("<?php return view('saved');", false);
        doc.text = "<?php /* 😀 */ return view('posts.create');".into();
        doc.version = 2;
        host.open_editor(&doc.path, &doc.language, &doc.text, &[]);
        let at = openidea_plugin::text::position(&doc.text, doc.text.find("posts.create").unwrap() + 2);
        assert_eq!(at.character as usize, doc.text[..doc.text.find("posts.create").unwrap() + 2].encode_utf16().count());
        let lazy = action(&mut main, &doc, Range::new(at, at));
        host.answer_next_prompt(Some(&["ok"]));
        assert_eq!(main.resolve_file_action(&doc, lazy).unwrap().operations.len(), 1);
    }
    #[test]
    fn namespace_dynamic_concatenation_import_and_suppressed_contexts_are_not_offered() {
        for (source, blade) in [
            ("<?php return view('posts.create' . $suffix);", false),
            ("<?php return view('pkg::posts.create');", false),
            ("<?php return view('../posts.create');", false),
            ("<?php use function Foreign\\view; return view('posts.create');", false),
            ("<?php // view('posts.create');", false),
            ("<?php return Other::view('posts.create');", false),
            ("<?php return $object->view('posts.create');", false),
            ("{{-- @include('posts.create') --}}", true),
            ("@verbatim @include('posts.create') @endverbatim", true),
        ] {
            let (_host, mut main, doc, range) = setup(source, blade);
            assert!(main.file_actions(&doc, range).is_empty(), "{source}");
        }
    }
    #[test]
    fn actual_collision_alternate_php_and_open_unsaved_target_are_rejected() {
        for target in ["resources/views/posts/create.blade.php", "resources/views/posts/create.php", "app/views/posts/create.blade.php"] {
            let (host, mut main, doc, range) = setup("<?php return view('posts.create');", false);
            let lazy = action(&mut main, &doc, range);
            host.add_file(target, "existing");
            host.answer_next_prompt(Some(&["ok"]));
            assert!(main.resolve_file_action(&doc, lazy).is_err());
        }
        let (host, mut main, doc, range) = setup("<?php return view('posts.create');", false);
        let lazy = action(&mut main, &doc, range);
        host.open_editor("/project/resources/views/posts/create.blade.php", "blade", "unsaved", &[]);
        assert!(main.resolve_file_action(&doc, lazy).is_err());
    }
    #[test]
    fn configured_or_unknown_view_roots_are_rejected_but_literal_default_is_supported() {
        for config in [
            "<?php return ['paths'=>[base_path('custom')]];",
            "<?php return ['paths'=>[resource_path('views'),base_path('custom')]];",
            "<?php return config('dynamic');",
            "<?php $ignored=['paths'=>[resource_path('views')]]; return config('dynamic');",
            "<?php namespace Other; return ['paths'=>[resource_path('views')]];",
            "<?php return ['paths'=>[resource_path('views')], ...$more];",
        ] {
            let (host, mut main, doc, range) = setup("<?php return view('posts.create');", false);
            host.add_file("config/view.php", config);
            let lazy = action(&mut main, &doc, range);
            assert!(main.resolve_file_action(&doc, lazy).is_err());
        }
        let (host, mut main, doc, range) = setup("<?php return view('posts.create');", false);
        host.add_file("config/view.php", "<?php return ['paths'=>[resource_path('views'),], 'compiled'=>'cache'];");
        let lazy = action(&mut main, &doc, range);
        host.answer_next_prompt(Some(&["ok"]));
        assert!(main.resolve_file_action(&doc, lazy).is_ok());
        let (host, mut main, doc, range) = setup("<?php return view('posts.create');", false);
        host.set_setting("viewPaths", "extra");
        let lazy = action(&mut main, &doc, range);
        assert!(main.resolve_file_action(&doc, lazy).is_err());
    }
    #[test]
    fn wrong_owner_partial_index_custom_override_and_data_tampering_are_rejected() {
        let (host, mut main, doc, range) = setup("@include('posts.create')", true);
        main.indexes.get_mut("/project").unwrap().partial = true;
        assert!(main.file_actions(&doc, range).is_empty());
        main.indexes.get_mut("/project").unwrap().partial = false;
        let mut lazy = action(&mut main, &doc, range);
        let mut data: Value = serde_json::from_str(lazy.data.as_deref().unwrap()).unwrap();
        data[0] = json!("/other");
        lazy.data = Some(data.to_string());
        assert_eq!(main.resolve_file_action(&doc, lazy), Err(Error::Stale));
        main.indexes.get_mut("/project").unwrap().directives.insert(
            "include".into(),
            crate::index::Entry { path: "/project/custom.php".into(), detail: None, at: Position::new(0, 0), width: 7 },
        );
        assert!(main.file_actions(&doc, range).is_empty());
        assert!(host.process_runs().is_empty());
    }
    #[test]
    fn oversized_source_or_selection_crossing_literal_is_not_offered() {
        let (_host, mut main, mut doc, range) = setup("<?php return view('posts.create');", false);
        doc.text.push_str(&" ".repeat(MAX_TEXT));
        assert!(main.file_actions(&doc, range).is_empty());
        let (_host, mut main, doc, range) = setup("<?php return view('posts.create');", false);
        assert!(main.file_actions(&doc, Range::new(range.start, openidea_plugin::text::position(&doc.text, doc.text.len()))).is_empty());
    }
}

#[cfg(test)]
mod owner_regressions {
    use super::*;
    use openidea_plugin::{Position, mock::MockHost};
    #[test]
    fn denied_project_and_nested_owner_without_own_cache_do_not_offer_actions() {
        let _denied_host = MockHost::install().with_capabilities(&[]);
        let mut main = Main::default();
        let doc = Document {
            path: "/project/view.blade.php".into(),
            language: "blade".into(),
            text: "@include('posts.create')".into(),
            version: 1,
        };
        assert!(main.file_actions(&doc, Range::new(Position::new(0, 12), Position::new(0, 12))).is_empty());
        let host = MockHost::install();
        host.add_project("/project/nested");
        main.indexes.insert("/project".into(), Index::default());
        let nested = Document { path: "/project/nested/view.blade.php".into(), ..doc };
        assert!(main.file_actions(&nested, Range::new(Position::new(0, 12), Position::new(0, 12))).is_empty());
        assert!(host.process_runs().is_empty());
    }
    #[test]
    fn parent_file_and_custom_namespace_metadata_block_resolution() {
        let host = MockHost::install();
        host.add_dir("resources/views");
        let source = "<?php return view('posts.create');";
        host.add_file("a.php", source);
        let doc = Document { path: "/project/a.php".into(), language: "php".into(), text: source.into(), version: 1 };
        let at = openidea_plugin::text::position(source, source.find("posts").unwrap() + 1);
        let mut main = Main::default();
        main.indexes.insert("/project".into(), Index::default());
        let lazy = main.file_actions(&doc, Range::new(at, at))[0].clone();
        host.add_file("resources/views/posts", "not a directory");
        assert!(main.resolve_file_action(&doc, lazy.clone()).is_err());
        host.add_file("ide-blade.json", r#"{"namespaces":[{"namespace":"pkg","path":"vendor/views"}]}"#);
        assert!(main.resolve_file_action(&doc, lazy).is_err());
    }
}

#[cfg(test)]
mod nested_target_tests{
    use super::*;
    use openidea_plugin::{mock::MockHost,text::position};
    #[test]
    fn separately_opened_target_project_cannot_receive_an_outer_missing_view_create(){
        let h=MockHost::install();h.add_dir("resources/views");let text="@include('posts.create')";let path="/project/resources/views/main.blade.php";h.add_file(path,text);h.open_editor(path,"blade",text,&[]);let mut main=Main::default();main.indexes.insert("/project".into(),Index::default());
        let d=Document{path:path.into(),language:"blade".into(),version:1,text:text.into()};let at=position(text,text.find("posts").unwrap());let a=main.file_actions(&d,Range::new(at,at)).remove(0);h.add_project("/project/resources/views/posts");h.answer_next_prompt(Some(&["yes"]));assert!(main.resolve_file_action(&d,a).is_err());assert!(h.process_runs().is_empty());
    }
    #[test]
    fn registered_missing_view_creation_refuses_config_owned_by_nested_project() {
        for existing in [false,true] {
            let host=MockHost::install();host.add_dir("resources/views");let source="<?php return view('posts.create');";host.add_file("a.php",source);
            let doc=Document{path:"/project/a.php".into(),language:"php".into(),version:1,text:source.into()};let at=position(source,source.find("posts").unwrap());
            let mut main=Main::default();main.indexes.insert("/project".into(),Index::default());let lazy=main.file_actions(&doc,Range::new(at,at)).remove(0);
            host.add_project("/project/config");
            if existing {host.add_file("config/view.php","<?php return ['paths' => [resource_path('views')]];");}
            host.answer_next_prompt(Some(&["ok"]));
            assert!(main.resolve_file_action(&doc,lazy).is_err(),"foreign config, existing={existing}");
            assert!(host.process_runs().is_empty() && host.requests().is_empty());
        }
    }

}
