//! Work on many files at once, and generators that write their own files: models picked in the
//! Project view, IDE helper PHPDoc removal for all models, and factory and resource bodies filled
//! from the migration columns. Generators use native project writes; bulk edits use one undoable FileAction.

use openidea_plugin::{
    Error, Project,
    file_actions::{FileAction, FileOperation},
    host,
};

use crate::index::Index;
use crate::unused;

#[path = "related_generation_config.rs"]
mod config;
#[path = "related_generation_selection.rs"]
mod selection;

/// Model class names of the model files among the selected Project view entries (a folder selects the models below it).
pub fn selected_models(index: &Index, selected: &[String]) -> Vec<(String, String)> {
    model_choices(index)
        .into_iter()
        .filter(|model| {
            selected.iter().any(|selected| {
                model.path == *selected
                    || model
                        .path
                        .starts_with(&format!("{}/", selected.trim_end_matches('/')))
            })
        })
        .map(|model| (model.full, model.path))
        .collect()
}

/// A model whose PHPDoc has IDE helper tags, to anchor the bulk removal action.
pub fn phpdoc_anchor(index: &Index, project: &Project) -> Option<String> {
    index.model_files.keys().find(|path| {
        crate::owns_current_path(project, path)
            && project.read_file(path).is_ok_and(|text| {
                text.len() <= 128 << 10
                    && text
                        .match_indices("class ")
                        .any(|(at, _)| unused::remove_model_phpdocs(&text, at).is_some())
            })
    }).cloned()
}

/// Writes a resolved editor action with project writes, for commands that can't hand the host a
/// workspace edit. Deletes and renames are refused before anything is written.
pub(crate) fn write_action(project: &Project, action: &FileAction) -> Result<usize, Error> {
    let mut files = std::collections::BTreeMap::<String, String>::new();
    for operation in &action.operations {
        match operation {
            FileOperation::Create { path, text, overwrite } => {
                if !overwrite && project.exists(path)? {
                    return Err(Error::Stale);
                }
                files.insert(path.clone(), text.clone());
            }
            FileOperation::Delete { path } | FileOperation::Rename { from: path, .. } => {
                return Err(Error::Invalid(crate::ui_text::format(
                    "native.bulk.removeByHand",
                    "Delete or move {path} yourself, then try again.",
                    &[("path", path)],
                )));
            }
        }
    }
    for (path, edits) in &action.edits {
        let source = match files.get(path) {
            Some(text) => text.clone(),
            None => project.read_file(path)?,
        };
        files.insert(path.clone(), crate::helper_automation::apply_edits(&source, edits)?);
    }
    for (path, text) in &files {
        project.write_file(path, text).map_err(|error| {
            ui_kit::denied::map(error, "Writing project files", "project.write", "Laravel")
        })?;
    }
    if let Some(path) = &action.open {
        host::open_editor(path, None)?;
    }
    Ok(files.len())
}

/// The key and anchor of the related-model generation prepared for this project.
pub(crate) fn pending_generation(plans: &ModelPlans, root: &str) -> Option<(String, String)> {
    plans.pending.borrow().get(root).map(|plan| (plan.key.clone(), plan.anchor.clone()))
}

fn cleanup_plan(
    main: &crate::Main,
    doc: &openidea_plugin::providers::Document,
) -> Result<Vec<(String, String, String)>, Error> {
    let (project, root) = crate::view_creation::owner(doc, None)?;
    let index = main
        .indexes
        .get(&root)
        .filter(|i| !i.partial)
        .ok_or(Error::Stale)?;
    if !index.model_files.contains_key(&doc.path)
        || project.read_file(&doc.path)? != doc.text
        || index.model_files.len() > 256
    {
        return Err(Error::Stale);
    }
    let mut plan = Vec::new();
    let mut bytes = 0usize;
    for path in index.model_files.keys() {
        if host::project_of(path)
            .and_then(|p| p.root().ok())
            .as_deref()
            != Some(&root)
        {
            return Err(Error::Stale);
        }
        let text = project.read_file(path)?;
        bytes += text.len();
        if text.len() > 128 << 10 || bytes > 4 << 20 {
            return Err(Error::Stale);
        }
        if let Some((range, replacement)) = text
            .match_indices("class ")
            .find_map(|(at, _)| unused::remove_model_phpdocs(&text, at))
        {
            let mut cleaned = text.clone();
            cleaned.replace_range(range, &replacement);
            plan.push((path.clone(), text, cleaned));
        }
    }
    Ok(plan)
}

pub fn actions(
    main: &crate::Main,
    doc: &openidea_plugin::providers::Document,
) -> Vec<openidea_plugin::file_actions::FileAction> {
    let admitted = crate::view_creation::owner(doc, None)
        .ok()
        .and_then(|(_, root)| main.indexes.get(&root))
        .is_some_and(|index| {
            !index.partial
                && index.model_files.len() <= 256
                && index.model_files.contains_key(&doc.path)
        });
    let mut found = generation_actions(main, doc);
    if admitted
        && doc.text.len() <= 128 << 10
        && doc
            .text
            .match_indices("class ")
            .any(|(at, _)| unused::remove_model_phpdocs(&doc.text, at).is_some())
    {
        found.push(openidea_plugin::file_actions::FileAction {
            title: crate::ui_text::text(
                "native.bulk.removeFromModels",
                "Remove IDE helper PHPDoc from models",
            )
            .into(),
            kind: Some("refactor.rewrite".into()),
            data: Some(format!("bulk-phpdocs:{}", doc.version)),
            ..Default::default()
        });
    }
    found
}

pub fn resolve(
    main: &crate::Main,
    doc: &openidea_plugin::providers::Document,
    action: openidea_plugin::file_actions::FileAction,
) -> Result<openidea_plugin::file_actions::FileAction, Error> {
    if action
        .data
        .as_deref()
        .is_some_and(|key| key.starts_with("bulk-generate:"))
    {
        return resolve_generation(main, doc, action);
    }
    if action.data.as_deref() != Some(&format!("bulk-phpdocs:{}", doc.version)) {
        return Err(Error::Stale);
    }
    let plan = cleanup_plan(main, doc)?;
    if plan.is_empty() {
        return Err(Error::Stale);
    }
    let preview = plan
        .iter()
        .map(|(path, _, _)| path.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    if !ui_kit::prompts::confirm_exact(
        crate::ui_text::text("final.bulk.removeConfirm", "Remove IDE helper PHPDoc?"),
        &crate::ui_text::format(
            "final.bulk.removeReview",
            "Remove helper tags from these {count} indexed model files in one undoable workspace edit:\n{preview}",
            &[("count", &plan.len().to_string()), ("preview", &preview)],
        ),
    )? {
        return Err(Error::Cancelled);
    }
    if cleanup_plan(main, doc)? != plan {
        return Err(Error::Stale);
    }
    Ok(openidea_plugin::file_actions::FileAction {
        title: action.title,
        edits: plan
            .into_iter()
            .map(|(path, text, cleaned)| {
                (
                    path,
                    vec![openidea_plugin::TextEdit {
                        range: openidea_plugin::Range::new(
                            openidea_plugin::Position::new(0, 0),
                            openidea_plugin::text::position(&text, text.len()),
                        ),
                        text: cleaned,
                    }],
                )
            })
            .collect(),
        ..Default::default()
    })
}

#[derive(Clone, Debug)]
pub(crate) struct ModelIdentity {
    pub full: String,
    pub path: String,
    pub table: String,
}

pub(crate) fn model_choices(index: &Index) -> Vec<ModelIdentity> {
    let mut identities = std::collections::BTreeMap::<String, Vec<ModelIdentity>>::new();
    let eligible = crate::eloquent_members::model_identities(&index.eloquent_models)
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
    for (path, table) in &index.model_files {
        let Some(project) = host::project_of(path) else {
            continue;
        };
        if !crate::owns_current_path(&project, path) {
            continue;
        }
        let Ok(source) = project.read_file(path) else {
            continue;
        };
        let Some(full) = crate::eloquent_members::declaration(&source) else {
            continue;
        };
        if !eligible.contains(&(full.clone(), path.clone()))
            || !crate::generator_plan::qualified(&full)
        {
            continue;
        }
        identities
            .entry(full.to_ascii_lowercase())
            .or_default()
            .push(ModelIdentity {
                full,
                path: path.clone(),
                table: table.clone(),
            });
    }
    identities
        .into_values()
        .filter_map(|mut candidates| (candidates.len() == 1).then(|| candidates.remove(0)))
        .collect()
}
fn identity(index: &Index, model: &str) -> Option<ModelIdentity> {
    let candidates = model_choices(index)
        .into_iter()
        .filter(|identity| {
            identity.full.eq_ignore_ascii_case(model)
                || !model.contains('\\')
                    && identity
                        .full
                        .rsplit('\\')
                        .next()
                        .is_some_and(|short| short.eq_ignore_ascii_case(model))
        })
        .collect::<Vec<_>>();
    match candidates.as_slice() {
        [identity] => Some(identity.clone()),
        _ => None,
    }
}
fn literal(value: &str) -> String {
    if !value.chars().any(char::is_control) {
        return format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"));
    }
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '$' => out.push_str("\\$"),
            c if c.is_control() => out.push_str(&format!("\\u{{{:x}}}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
fn property(column: &str) -> String {
    if crate::generator_plan::identifier(column) {
        format!("$this->{column}")
    } else {
        format!("$this->{{{}}}", literal(column))
    }
}
fn unresolved(column: &str, reason: &str) -> String {
    format!(
        "throw new \\LogicException({})",
        literal(&format!(
            "Unresolved factory mapping for {column}: {reason}"
        ))
    )
}
fn faker(column: &str, kind: &str) -> Option<String> {
    if matches!(
        kind,
        "id" | "increments"
            | "bigIncrements"
            | "tinyIncrements"
            | "smallIncrements"
            | "mediumIncrements"
    ) || matches!(
        column,
        "created_at" | "updated_at" | "deleted_at" | "remember_token"
    ) {
        return None;
    }
    // A spelling is not evidence of a foreign-key relationship or a usable
    // related factory. Keep the unresolved mapping explicit and executable PHP.
    if column.ends_with("_id") {
        return Some(unresolved(
            column,
            "select a value or verified related factory",
        ));
    }
    let named = match column {
        "email" => Some("fake()->unique()->safeEmail()"),
        "name" => Some("fake()->name()"),
        "password" => Some("bcrypt('password')"),
        "slug" => Some("fake()->slug()"),
        "title" => Some("fake()->sentence()"),
        "phone" => Some("fake()->phoneNumber()"),
        "url" => Some("fake()->url()"),
        "email_verified_at" => Some("now()"),
        _ => None,
    };
    if let Some(value) = named {
        return Some(value.into());
    }
    Some(
        match kind {
            "string" | "char" => "fake()->word()",
            "text" | "tinyText" | "mediumText" | "longText" => "fake()->paragraph()",
            "integer"
            | "tinyInteger"
            | "smallInteger"
            | "mediumInteger"
            | "bigInteger"
            | "unsignedInteger"
            | "unsignedTinyInteger"
            | "unsignedSmallInteger"
            | "unsignedMediumInteger"
            | "unsignedBigInteger" => "fake()->numberBetween(1, 1000)",
            "float" | "double" => "fake()->randomFloat(2, 1, 1000)",
            "decimal" | "unsignedDecimal" => {
                return Some(unresolved(
                    column,
                    "decimal precision and scale require explicit review",
                ));
            }
            "boolean" => "fake()->boolean()",
            "date" => "fake()->date()",
            "dateTime" | "dateTimeTz" | "timestamp" | "timestampTz" => "fake()->dateTime()",
            "time" | "timeTz" => "fake()->time()",
            "uuid" => "fake()->uuid()",
            "ulid" => "(string) \\Illuminate\\Support\\Str::ulid()",
            "ipAddress" => "fake()->ipv4()",
            "json" | "jsonb" => "[]",
            _ => {
                return Some(unresolved(
                    column,
                    "schema type is not sufficiently described",
                ));
            }
        }
        .into(),
    )
}
fn source(
    index: &Index,
    model: &ModelIdentity,
    kind: &str,
    name: &str,
    namespace: &str,
    selection: Option<&selection::Selection>,
    settings: &crate::generator_plan::GenerationSettings,
) -> String {
    let columns = index.columns.get(&model.table);
    let array_return = if settings.type_hints { ": array" } else { "" };
    let factory_base = settings.factory_base.trim_start_matches('\\');
    let resource_base = settings.resource_base.trim_start_matches('\\');
    if kind == "factory" {
        let rows = selection
            .map(|selection| selection.rows(index, model, kind))
            .unwrap_or_else(|| {
                columns
                    .into_iter()
                    .flatten()
                    .filter_map(|(column, entry)| {
                        faker(column, entry.detail.as_deref().unwrap_or(""))
                            .map(|value| format!("            {} => {value},\n", literal(column)))
                    })
                    .collect::<String>()
            });
        format!(
            "<?php\n\nnamespace {namespace};\n\n/**\n * @extends \\{factory_base}<\\{}>\n */\nclass {name} extends \\{factory_base}\n{{\n    protected $model = \\{}::class;\n\n    public function definition(){array_return}\n    {{\n        // Review every unresolved mapping before using this factory.\n        return [\n{rows}        ];\n    }}\n}}\n",
            model.full, model.full
        )
    } else {
        let rows = selection
            .map(|selection| selection.rows(index, model, kind))
            .unwrap_or_else(|| {
                columns
                    .into_iter()
                    .flatten()
                    .filter(|(column, _)| !matches!(column.as_str(), "password" | "remember_token"))
                    .map(|(column, _)| {
                        format!("            {} => {},\n", literal(column), property(column))
                    })
                    .collect::<String>()
            });
        format!(
            "<?php\n\nnamespace {namespace};\n\n/** @mixin \\{} */\nclass {name} extends \\{resource_base}\n{{\n    public function toArray(\\Illuminate\\Http\\Request $request){array_return}\n    {{\n        return [\n{rows}        ];\n    }}\n}}\n",
            model.full
        )
    }
}
fn resource_namespace(model: &ModelIdentity) -> String {
    let root = model
        .full
        .split_once("\\Models\\")
        .map(|(root, _)| root)
        .or_else(|| model.full.rsplit_once('\\').map(|(namespace, _)| namespace))
        .unwrap_or("App");
    format!("{root}\\Http\\Resources")
}
#[cfg(test)]
fn preview_source(index: &Index, model: &str, name: &str, kind: &str) -> String {
    let Some(model) = identity(index, model) else {
        return "<?php\n// Unresolved or ambiguous model identity; no class emitted.\n".into();
    };
    if !crate::generator_plan::qualified(name) || name.starts_with('\\') {
        return "<?php\n// Unsupported generated class name; no class emitted.\n".into();
    }
    let base = if kind == "factory" {
        "Database\\Factories".into()
    } else {
        resource_namespace(&model)
    };
    let (namespace, class) = if let Some((prefix, class)) = name.rsplit_once('\\') {
        (format!("{base}\\{prefix}"), class)
    } else {
        (base, name)
    };
    source(
        index,
        &model,
        kind,
        class,
        &namespace,
        None,
        &Default::default(),
    )
}
#[cfg(test)]
pub fn factory_source(index: &Index, model: &str, name: &str) -> String {
    preview_source(index, model, name, "factory")
}
#[cfg(test)]
pub fn resource_source(index: &Index, model: &str, name: &str) -> String {
    preview_source(index, model, name, "resource")
}

#[derive(Default)]
pub(crate) struct ModelPlans {
    next: std::cell::Cell<u64>,
    pending: std::cell::RefCell<std::collections::BTreeMap<String, Pending>>,
}
#[derive(Clone)]
struct Pending {
    key: String,
    model: String,
    anchor: String,
    source: String,
    kind: String,
    name: String,
    output: Output,
    evidence: String,
    selection: Option<selection::Selection>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Output {
    path: String,
    text: String,
}

fn owned_target(project: &Project, path: &str) -> Result<(), Error> {
    if !crate::owns_current_path(project, path) {
        return Err(Error::Stale);
    }
    let root = project.root()?;
    let relative = path.strip_prefix(&format!("{root}/")).ok_or(Error::Stale)?;
    if !crate::generator_plan::relative(relative) {
        return Err(Error::Invalid(
            crate::ui_text::text(
                "native.bulk.c1885be120",
                "Generated path is not inside the project",
            )
            .into(),
        ));
    }
    let mut parts = relative.split('/').collect::<Vec<_>>();
    parts.pop();
    while !parts.is_empty() {
        let parent = parts.join("/");
        match project.stat(&parent) {
            Ok(stat) if stat.kind == openidea_plugin::FileKind::Directory => {}
            Err(Error::NotFound(_)) => {}
            Ok(_) => {
                return Err(Error::Invalid(crate::ui_text::format(
                    "final.bulk.targetAncestor",
                    "Target ancestor is not an ordinary directory: {parent}",
                    &[("parent", &parent)],
                )));
            }
            Err(error) => return Err(error),
        }
        parts.pop();
    }
    match project.stat(path) {
        Err(Error::NotFound(_)) => Ok(()),
        Ok(_) => Err(Error::Invalid(crate::ui_text::format(
            "final.bulk.targetExists",
            "Generated target already exists: {relative}",
            &[("relative", relative)],
        ))),
        Err(error) => Err(error),
    }
}
fn output(
    project: &Project,
    index: &Index,
    model: &ModelIdentity,
    kind: &str,
    name: &str,
    selection: Option<&selection::Selection>,
) -> Result<(Output, String), Error> {
    output_in_module(project, index, model, kind, name, selection, None)
}
fn output_in_module(
    project: &Project,
    index: &Index,
    model: &ModelIdentity,
    kind: &str,
    name: &str,
    selection: Option<&selection::Selection>,
    module: Option<Option<&str>>,
) -> Result<(Output, String), Error> {
    if !matches!(kind, "factory" | "resource")
        || (!crate::generator_plan::qualified(name) || name.starts_with('\\'))
    {
        return Err(Error::Invalid(
            crate::ui_text::text(
                "native.bulk.bcc60978a0",
                "Unsupported related-model class name",
            )
            .into(),
        ));
    }
    let root = project.root()?;
    let (draft, fingerprint, module_context) =
        config::effective_in_module(project, model, kind, module).map_err(selection::ui_error)?;
    let base_namespace = draft.namespace_for(kind);
    let directory = draft.directory(kind);
    let configured_destination = draft.destinations.contains_key(kind) || module_context;
    let composer = match project.read_file("composer.json") {
        Ok(text) => text,
        Err(Error::NotFound(_)) => String::new(),
        Err(error) => return Err(error),
    };
    let config = if composer.is_empty() {
        serde_json::Value::Null
    } else {
        crate::strict_json::parse(&composer, 512 * 1024)?
    };
    let mut mappings = Vec::new();
    for section in ["autoload", "autoload-dev"] {
        if let Some(map) = config[section]["psr-4"].as_object() {
            for (prefix, paths) in map {
                if let Some(s) = paths.as_str() {
                    mappings.push((prefix.clone(), s.to_owned()));
                } else if let Some(paths) = paths.as_array() {
                    for path in paths {
                        mappings.push((
                            prefix.clone(),
                            path.as_str().ok_or(Error::Stale)?.to_owned(),
                        ));
                    }
                } else {
                    return Err(Error::Stale);
                }
            }
        }
    }
    let full_namespace = format!("{base_namespace}\\{name}");
    let mut paths = mappings
        .iter()
        .filter_map(|(prefix, path)| {
            full_namespace.strip_prefix(prefix).map(|tail| {
                (
                    prefix.len(),
                    format!(
                        "{}/{}.php",
                        path.trim_end_matches('/'),
                        tail.replace('\\', "/")
                    ),
                )
            })
        })
        .collect::<Vec<_>>();
    let max = paths.iter().map(|(length, _)| *length).max();
    paths.retain(|(length, _)| Some(*length) == max);
    paths.sort();
    paths.dedup();
    let relative = if configured_destination {
        format!("{directory}/{}.php", name.replace('\\', "/"))
    } else if paths.len() == 1 {
        paths.remove(0).1
    } else if paths.len() > 1 {
        return Err(Error::Invalid(
            crate::ui_text::text(
                "native.bulk.07a3fe6fcf",
                "Composer output mapping is ambiguous",
            )
            .into(),
        ));
    } else {
        if !base_namespace.starts_with("App\\") && base_namespace != "Database\\Factories" {
            return Err(Error::Invalid(
                crate::ui_text::text(
                    "native.bulk.1bd5554ed0",
                    "No Composer mapping for the generated namespace",
                )
                .into(),
            ));
        }
        format!("{directory}/{}.php", name.replace('\\', "/"))
    };
    let base = if kind == "factory" {
        &draft.settings.factory_base
    } else {
        &draft.settings.resource_base
    };
    if full_namespace.eq_ignore_ascii_case(base.trim_start_matches('\\'))
        || full_namespace.eq_ignore_ascii_case(&model.full)
    {
        return Err(Error::Invalid(
            crate::ui_text::text(
                "native.bulk.33e3b56735",
                "Generated class conflicts with its base or model class",
            )
            .into(),
        ));
    }
    let mapping_evidence = if configured_destination {
        crate::generator_workspace::related_mapping(
            project,
            &full_namespace,
            &relative,
            kind == "factory",
        )?
    } else {
        String::new()
    };
    let (namespace, class) = full_namespace.rsplit_once('\\').ok_or(Error::Stale)?;
    let path = format!("{root}/{relative}");
    owned_target(project, &path)?;
    let generated = source(
        index,
        model,
        kind,
        class,
        namespace,
        selection,
        &draft.settings,
    );
    let output = Output {
        path,
        text: config::render(
            project,
            model,
            kind,
            &draft,
            module,
            &full_namespace,
            &generated,
        )?,
    };
    let selection_evidence = selection
        .map(|selection| {
            selection
                .validate(project, index, model, kind)
                .map_err(selection::ui_error)
        })
        .transpose()?
        .unwrap_or_default();
    Ok((
        output,
        format!("{fingerprint}\n{composer}\n{mapping_evidence}\n{selection_evidence}"),
    ))
}

pub(crate) fn related_default_name(
    project: &Project,
    index: &Index,
    kind: &str,
    model: &str,
) -> Result<Option<String>, Error> {
    if !matches!(kind, "factory" | "resource") {
        return Ok(None);
    }
    let model = identity(index, model).ok_or(Error::Stale)?;
    let (draft, _, _) = config::effective(project, &model, kind).map_err(selection::ui_error)?;
    Ok(Some(draft.class_name(kind)))
}

pub(crate) fn related_catalog(
    project: &Project,
    index: &Index,
    model_name: &str,
) -> Result<serde_json::Value, Error> {
    if index.partial {
        return Err(Error::Stale);
    }
    let model = identity(index, model_name).ok_or(Error::Stale)?;
    Ok(selection::catalog(project, index, &model))
}
pub(crate) fn related_preview(
    project: &Project,
    index: &Index,
    kind: &str,
    model_name: &str,
    name: &str,
    choices: &serde_json::Value,
) -> Result<(String, String, String), Error> {
    if index.partial {
        return Err(Error::Stale);
    }
    let model = identity(index, model_name).ok_or(Error::Stale)?;
    let selection = selection::from_request(project, index, &model, kind, choices)
        .map_err(selection::ui_error)?;
    let (output, evidence) = output(project, index, &model, kind, name, Some(&selection))?;
    Ok((
        output.path,
        output.text,
        format!(
            "{evidence}\n{}:{}",
            model.path,
            project.read_file(&model.path)?
        ),
    ))
}

pub(crate) fn related_preview_in_module(
    project: &Project,
    index: &Index,
    kind: &str,
    model_name: &str,
    name: &str,
    module: Option<Option<&str>>,
    choices: &serde_json::Value,
) -> Result<(String, String, String), Error> {
    if index.partial {
        return Err(Error::Stale);
    }
    let model = identity(index, model_name).ok_or(Error::Stale)?;
    let selection = selection::from_request(project, index, &model, kind, choices)
        .map_err(selection::ui_error)?;
    let (output, evidence) =
        output_in_module(project, index, &model, kind, name, Some(&selection), module)?;
    Ok((
        output.path,
        output.text,
        format!(
            "{evidence}\n{}:{}",
            model.path,
            project.read_file(&model.path)?
        ),
    ))
}

#[cfg(test)]
pub(crate) fn prepare_model_file(
    plans: &ModelPlans,
    project: &Project,
    index: &Index,
    kind: &str,
    model: &str,
    name: &str,
) -> Result<Option<String>, Error> {
    prepare_model_file_with(plans, project, index, kind, model, name, None)
}

/// Collect native field/mapping choices before preparing the existing atomic action.
#[cfg(test)]
pub(crate) fn prepare_model_file_interactive(
    plans: &ModelPlans,
    project: &Project,
    index: &Index,
    kind: &str,
    model: &str,
    name: &str,
) -> Result<Option<String>, Error> {
    prepare_model_file_interactive_with_schema(plans, project, index, kind, model, name, None)
}

pub(crate) fn prepare_model_file_interactive_with_schema(
    plans: &ModelPlans,
    project: &Project,
    index: &Index,
    kind: &str,
    model: &str,
    name: &str,
    runtime: Option<(crate::artisan_runner::Runner, serde_json::Value)>,
) -> Result<Option<String>, Error> {
    if !matches!(kind, "factory" | "resource") {
        return Ok(None);
    }
    plans.pending.borrow_mut().remove(&project.root()?);
    if !crate::index::index_sources_current(project, index) {
        return Err(Error::Stale);
    }
    let current = index;
    let identity = identity(current, model).ok_or(Error::Stale)?;
    let selection = selection::choose(project, current, &identity, kind, runtime)?;
    prepare_model_file_with(plans, project, index, kind, model, name, Some(selection))
}

fn prepare_model_file_with(
    plans: &ModelPlans,
    project: &Project,
    index: &Index,
    kind: &str,
    model: &str,
    name: &str,
    selection: Option<selection::Selection>,
) -> Result<Option<String>, Error> {
    if !matches!(kind, "factory" | "resource") {
        return Ok(None);
    }
    let root = project.root()?;
    plans.pending.borrow_mut().remove(&root);
    if index.partial {
        return Err(Error::Stale);
    }
    if !crate::index::index_sources_current(project, index) {
        return Err(Error::Stale);
    }
    let current = index;
    let model = identity(current, model).ok_or_else(|| {
        Error::Invalid(
            crate::ui_text::text(
                "native.bulk.00d4e41434",
                "Model identity is missing or ambiguous",
            )
            .into(),
        )
    })?;
    let source = project.read_file(&model.path)?;
    let (output, evidence) = output(project, current, &model, kind, name, selection.as_ref())?;
    let next = plans.next.get().checked_add(1).ok_or(Error::Stale)?;
    if plans.pending.borrow().len() >= 8 {
        return Err(Error::Invalid(
            crate::ui_text::text(
                "native.bulk.5063d48c4a",
                "Too many pending projects; finish an existing generation first",
            )
            .into(),
        ));
    }
    let pending = Pending {
        key: next.to_string(),
        model: model.full,
        anchor: model.path.clone(),
        source,
        kind: kind.into(),
        name: name.into(),
        output,
        evidence,
        selection,
    };
    plans.next.set(next);
    plans.pending.borrow_mut().insert(root, pending);
    Ok(Some(model.path))
}
fn generation_actions(
    main: &crate::Main,
    doc: &openidea_plugin::providers::Document,
) -> Vec<openidea_plugin::file_actions::FileAction> {
    let Some(project) = host::project_of(&doc.path) else {
        return Vec::new();
    };
    let Ok(root) = project.root() else {
        return Vec::new();
    };
    if !crate::owns_current_path(&project, &doc.path) {
        return Vec::new();
    }
    main.artisan_terminals
        .model_plans
        .pending
        .borrow()
        .get(&root)
        .filter(|plan| plan.anchor == doc.path && plan.source == doc.text)
        .map(|plan| {
            vec![openidea_plugin::file_actions::FileAction {
                title: crate::ui_text::text(
                    "native.bulk.fed7fad9e7",
                    "Apply prepared related-model generation…",
                )
                .into(),
                kind: Some("refactor.rewrite".into()),
                data: Some(format!("bulk-generate:{}:{}", plan.key, doc.version)),
                ..Default::default()
            }]
        })
        .unwrap_or_default()
}
fn resolve_generation(
    main: &crate::Main,
    doc: &openidea_plugin::providers::Document,
    action: openidea_plugin::file_actions::FileAction,
) -> Result<openidea_plugin::file_actions::FileAction, Error> {
    let project = host::project_of(&doc.path).ok_or(Error::Stale)?;
    let root = project.root()?;
    let pending = main
        .artisan_terminals
        .model_plans
        .pending
        .borrow()
        .get(&root)
        .cloned()
        .ok_or(Error::Stale)?;
    if action.data.as_deref() != Some(&format!("bulk-generate:{}:{}", pending.key, doc.version))
        || pending.anchor != doc.path
        || pending.source != doc.text
        || !crate::owns_current_path(&project, &doc.path)
        || project.read_file(&doc.path)? != doc.text
    {
        return Err(Error::Stale);
    }
    let current = main.completed_index_for(&root).ok_or(Error::Stale)?;
    if !crate::index::index_sources_current(&project, current) {
        return Err(Error::Stale);
    }
    let model = identity(current, &pending.model)
        .filter(|model| model.path == pending.anchor)
        .ok_or(Error::Stale)?;
    if let Some(selection) = &pending.selection {
        selection.check_runtime(main, &project)?;
    }
    let (planned, evidence) = output(
        &project,
        current,
        &model,
        &pending.kind,
        &pending.name,
        pending.selection.as_ref(),
    )?;
    if planned != pending.output || evidence != pending.evidence {
        return Err(Error::Stale);
    }
    let mapping_note = if pending.kind == "factory"
        && planned.text.contains("throw new \\LogicException")
    {
        crate::ui_text::text(
            "final.bulk.mappingNote",
            "Unresolved factory mappings are explicit LogicException placeholders; review them before executing the generated factory. ",
        )
    } else {
        ""
    };
    let runtime_note = pending
        .selection
        .as_ref()
        .map(|selection| selection.runtime_note())
        .unwrap_or_default();
    let preview = crate::ui_text::format(
        "final.bulk.relatedReview",
        "Project: {root}\nModel: {model}\nCreate (no overwrite): {path}\n{runtime}\n{source}\n\n{mapping}Apply one undoable file creation?",
        &[
            ("root", &root),
            ("model", &pending.model),
            ("path", &planned.path),
            ("runtime", &runtime_note),
            ("source", &planned.text),
            ("mapping", mapping_note),
        ],
    );
    if !ui_kit::prompts::confirm_exact(
        crate::ui_text::text("final.bulk.relatedConfirm", "Create related model class?"),
        &preview,
    )? {
        return Err(Error::Cancelled);
    }
    if !crate::owns_current_path(&project, &doc.path) || project.read_file(&doc.path)? != doc.text {
        return Err(Error::Stale);
    }
    let fresh = main.completed_index_for(&root).ok_or(Error::Stale)?;
    if !crate::index::index_sources_current(&project, fresh) {
        return Err(Error::Stale);
    }
    let model = identity(fresh, &pending.model)
        .filter(|model| model.path == pending.anchor)
        .ok_or(Error::Stale)?;
    if let Some(selection) = &pending.selection {
        selection.check_runtime(main, &project)?;
    }
    let (checked, evidence) = output(
        &project,
        fresh,
        &model,
        &pending.kind,
        &pending.name,
        pending.selection.as_ref(),
    )?;
    if checked != planned
        || evidence != pending.evidence
        || main
            .artisan_terminals
            .model_plans
            .pending
            .borrow()
            .get(&root)
            .map(|plan| plan.key.as_str())
            != Some(pending.key.as_str())
    {
        return Err(Error::Stale);
    }
    main.artisan_terminals
        .model_plans
        .pending
        .borrow_mut()
        .remove(&root);
    Ok(openidea_plugin::file_actions::FileAction {
        title: action.title,
        operations: vec![openidea_plugin::file_actions::FileOperation::Create {
            path: planned.path.clone(),
            text: planned.text,
            overwrite: false,
        }],
        open: Some(planned.path),
        ..Default::default()
    })
}

/// Compatibility entry point kept for conflict checks; generation now requires
/// prepare_model_file and a host-applied FileAction and never writes directly.
#[cfg(test)]
pub fn model_file(
    project: &Project,
    index: &Index,
    kind: &str,
    model: &str,
    name: &str,
) -> Result<Option<String>, Error> {
    if !matches!(kind, "factory" | "resource") {
        return Ok(None);
    }
    let identity = identity(index, model).ok_or(Error::Stale)?;
    let _ = output(project, index, &identity, kind, name, None)?;
    Err(Error::Invalid(
        "Use the reviewed related-model generation editor action".into(),
    ))
}

#[cfg(test)]
mod related_tests {
    use super::*;
    use openidea_plugin::{
        Position, file_actions::FileOperation, mock::MockHost, providers::Document,
    };
    fn setup() -> (MockHost, crate::Main, Project, Document) {
        let mock = MockHost::install();
        mock.add_file("composer.json",r#"{"autoload":{"psr-4":{"Domain\\":"src/"}},"autoload-dev":{"psr-4":{"Database\\Factories\\":"database/factories/"}}}"#);
        let source = "<?php namespace Domain\\Models;use Illuminate\\Database\\Eloquent\\Model;class Post extends Model {}";
        mock.add_file("src/Models/Post.php", source);
        mock.add_file("database/migrations/001.php","<?php Schema::create('posts',function($t){$t->id();$t->string('title');$t->integer('external_id');$t->integer('équipe_id');$t->integer('_id');});");
        let project = host::projects().unwrap().remove(0);
        let mut main = crate::Main::default();
        main.indexes
            .insert(project.root().unwrap(), Index::build(&project));
        let doc = Document {
            path: "/project/src/Models/Post.php".into(),
            language: "php".into(),
            version: 1,
            text: source.into(),
        };
        (mock, main, project, doc)
    }
    fn prepare(main: &crate::Main, project: &Project, kind: &str, name: &str) {
        assert!(
            prepare_model_file(
                &main.artisan_terminals.model_plans,
                project,
                main.indexes.get("/project").unwrap(),
                kind,
                "Domain\\Models\\Post",
                name
            )
            .unwrap()
            .is_some()
        );
    }
    fn lazy(main: &crate::Main, doc: &Document) -> openidea_plugin::file_actions::FileAction {
        actions(main, doc)
            .into_iter()
            .find(|action| {
                action
                    .data
                    .as_deref()
                    .is_some_and(|key| key.starts_with("bulk-generate:"))
            })
            .unwrap()
    }
    fn export(name: &str, source: &str) {
        if let Ok(directory) = std::env::var("LARAVEL_RELATED_FIXTURES_DIR") {
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(
                std::path::Path::new(&directory).join(format!("{name}.php")),
                source,
            )
            .unwrap();
        }
    }
    #[test]
    fn factory_has_exact_source_fqn_and_unresolved_foreign_keys_without_unicode_panics() {
        let (mock, main, _, _) = setup();
        let index = main.indexes.get("/project").unwrap();
        let source = factory_source(index, "Domain\\Models\\Post", "Admin\\PostFactory");
        assert!(source.contains("namespace Database\\Factories\\Admin;"));
        assert!(source.contains("Factory<\\Domain\\Models\\Post>"));
        assert!(source.contains("protected $model = \\Domain\\Models\\Post::class;"));
        assert!(source.contains("Unresolved factory mapping for external_id"));
        assert!(source.contains("Unresolved factory mapping for équipe_id"));
        assert!(source.contains("Unresolved factory mapping for _id"));
        assert!(!source.contains("External::factory()"));
        assert!(!source.contains("Equipe::factory()"));
        export("foreign_keys", &source);
        assert!(mock.process_runs().is_empty());
    }
    #[test]
    fn resources_escape_every_column_key_and_dynamic_property_name() {
        use crate::index::AddName;
        let (_mock, mut main, _, _) = setup();
        let index = main.indexes.get_mut("/project").unwrap();
        for name in [
            "with-dash",
            "quote'field",
            "slash\\field",
            "💡",
            "line\nfield",
        ] {
            index.columns.get_mut("posts").unwrap().add(
                name,
                Some("string".into()),
                "/project/database/migrations/001.php",
                Position::new(0, 0),
                0,
            );
        }
        let source = resource_source(index, "Domain\\Models\\Post", "Nested\\PostResource");
        assert!(source.contains("namespace Domain\\Http\\Resources\\Nested;"));
        assert!(source.contains("$this->{'with-dash'}"));
        assert!(source.contains("$this->{'quote\\'field'}"));
        assert!(source.contains("$this->{'slash\\\\field'}"));
        assert!(source.contains("$this->{'💡'}"));
        assert!(!source.contains("$this->with-dash"));
        export("escaped_resource", &source);
    }
    #[test]
    fn preparation_and_resolution_only_return_atomic_non_overwriting_create_and_are_one_shot() {
        let (mock, main, project, doc) = setup();
        prepare(&main, &project, "factory", "PostFactory");
        assert!(
            !project
                .exists("database/factories/PostFactory.php")
                .unwrap()
        );
        assert!(mock.process_runs().is_empty());
        let action = lazy(&main, &doc);
        mock.answer_next_prompt(Some(&["yes"]));
        let resolved = resolve(&main, &doc, action.clone()).unwrap();
        assert_eq!(resolved.operations.len(), 1);
        let FileOperation::Create {
            path,
            text,
            overwrite,
        } = &resolved.operations[0]
        else {
            panic!()
        };
        assert_eq!(path, "/project/database/factories/PostFactory.php");
        assert!(!overwrite);
        assert!(text.contains("Domain\\Models\\Post::class"));
        export("atomic_factory", text);
        assert!(!project.exists(path).unwrap());
        assert!(resolve(&main, &doc, action).is_err());
    }
    #[test]
    fn resource_output_uses_real_composer_namespace_and_does_not_launch_artisan() {
        let (mock, main, project, doc) = setup();
        prepare(&main, &project, "resource", "Admin\\PostResource");
        mock.answer_next_prompt(Some(&["yes"]));
        let resolved = resolve(&main, &doc, lazy(&main, &doc)).unwrap();
        let FileOperation::Create { path, text, .. } = &resolved.operations[0] else {
            panic!()
        };
        assert_eq!(path, "/project/src/Http/Resources/Admin/PostResource.php");
        assert!(text.contains("namespace Domain\\Http\\Resources\\Admin;"));
        export("atomic_resource", text);
        assert!(mock.process_runs().is_empty());
    }
    #[test]
    fn stale_completed_index_does_not_prepare_or_silently_reindex_new_source_membership() {
        let (mock, main, project, _) = setup();
        mock.add_file("src/Models/Unrelated.php", "<?php namespace Domain\\Models;class Unrelated {}");
        assert!(matches!(prepare_model_file(
            &main.artisan_terminals.model_plans,
            &project,
            main.indexes.get("/project").unwrap(),
            "factory",
            "Domain\\Models\\Post",
            "PostFactory",
        ), Err(Error::Stale)));
        assert!(main.artisan_terminals.model_plans.pending.borrow().is_empty());
        assert!(!project.exists("database/factories/PostFactory.php").unwrap());
        assert!(mock.process_runs().is_empty());
    }
    #[test]
    fn cancellation_collision_stale_schema_source_and_new_foreign_ownership_never_write() {
        for failure in ["cancel", "collision", "schema", "source", "ownership", "membership"] {
            let (mock, main, project, doc) = setup();
            prepare(&main, &project, "factory", "PostFactory");
            let action = lazy(&main, &doc);
            match failure {
                "cancel" => mock.answer_next_prompt(None),
                "collision" => {
                    mock.add_file("database/factories/PostFactory.php", "existing user file")
                }
                "schema" => mock.add_file(
                    "database/migrations/002.php",
                    "<?php Schema::table('posts',function($t){$t->string('new_column');});",
                ),
                "source" => mock.open_editor(
                    &doc.path,
                    "php",
                    "<?php namespace Domain\\Models;class Changed {}",
                    &[],
                ),
                "ownership" => mock.add_project("/project/database"),
                // The selected model/output facts do not change, but a completed
                // index cannot vouch for source membership it never captured.
                "membership" => mock.add_file("src/Models/Unrelated.php","<?php namespace Domain\\Models;class Unrelated {}"),
                _ => unreachable!(),
            }
            assert!(resolve(&main, &doc, action).is_err(), "{failure}");
            if failure == "collision" {
                assert_eq!(
                    project
                        .read_file("database/factories/PostFactory.php")
                        .unwrap(),
                    "existing user file"
                );
            } else {
                assert!(
                    !project
                        .exists("database/factories/PostFactory.php")
                        .unwrap()
                );
            }
            assert!(mock.process_runs().is_empty());
        }
    }
    fn add_factory_mapping(mock: &MockHost) {
        mock.add_file("src/Models/User.php", "<?php namespace Domain\\Models;use Illuminate\\Database\\Eloquent\\Model;class User extends Model {}");
        mock.add_file("src/Factories/OwnerFactory.php", "<?php namespace Domain\\Factories;use Illuminate\\Database\\Eloquent\\Factories\\Factory;class OwnerFactory extends Factory {protected $model = \\Domain\\Models\\User::class;}");
    }
    #[test]
    fn interactive_factory_selection_uses_explicit_factory_class_and_preserves_atomic_review() {
        let (mock, mut main, project, doc) = setup();
        add_factory_mapping(&mock);
        main.indexes.insert("/project".into(), Index::build(&project));
        for answers in [
            vec!["select"],
            vec!["title", "external_id"],
            vec!["external_id"],
            vec!["Domain\\Factories\\OwnerFactory"],
        ] {
            mock.answer_next_prompt(Some(&answers));
        }
        prepare_model_file_interactive(
            &main.artisan_terminals.model_plans,
            &project,
            main.indexes.get("/project").unwrap(),
            "factory",
            "Domain\\Models\\Post",
            "PostFactory",
        )
        .unwrap();
        mock.answer_next_prompt(Some(&["yes"]));
        let action = resolve(&main, &doc, lazy(&main, &doc)).unwrap();
        let FileOperation::Create {
            text, overwrite, ..
        } = &action.operations[0]
        else {
            panic!()
        };
        assert!(!overwrite);
        assert!(text.contains("'external_id' => \\Domain\\Factories\\OwnerFactory::new()"));
        assert!(text.contains("'title' => fake()->sentence()"));
        assert!(!text.contains("équipe_id"));
        assert!(!text.contains("LogicException"));
        assert!(
            !project
                .exists("database/factories/PostFactory.php")
                .unwrap()
        );
        assert!(mock.process_runs().is_empty());
        export("selected_factory", text);
    }
    #[test]
    fn selected_factory_dependency_changes_or_foreign_ownership_reject_the_plan() {
        for failure in ["changed", "foreign"] {
            let (mock, mut main, project, doc) = setup();
            add_factory_mapping(&mock);
            main.indexes.insert("/project".into(), Index::build(&project));
            for answers in [
                vec!["select"],
                vec!["external_id"],
                vec!["external_id"],
                vec!["Domain\\Factories\\OwnerFactory"],
            ] {
                mock.answer_next_prompt(Some(&answers));
            }
            prepare_model_file_interactive(
                &main.artisan_terminals.model_plans,
                &project,
                main.indexes.get("/project").unwrap(),
                "factory",
                "Domain\\Models\\Post",
                "PostFactory",
            )
            .unwrap();
            let action = lazy(&main, &doc);
            if failure == "changed" {
                mock.add_file("src/Factories/OwnerFactory.php","<?php namespace Domain\\Factories;use Illuminate\\Database\\Eloquent\\Factories\\Factory;class OwnerFactory extends Factory {protected $model = \\Domain\\Models\\User::class; /* changed after preview */ }");
            } else {
                mock.add_project("/project/src/Factories");
            }
            assert!(resolve(&main, &doc, action).is_err(), "{failure}");
            assert!(
                !project
                    .exists("database/factories/PostFactory.php")
                    .unwrap()
            );
        }
    }
    #[test]
    fn interactive_resource_relations_use_cardinality_and_only_loaded_values() {
        let (mock, mut main, project, mut doc) = setup();
        add_factory_mapping(&mock);
        doc.text="<?php namespace Domain\\Models;use Illuminate\\Database\\Eloquent\\Model;class Post extends Model {public function author(){return $this->belongsTo(User::class);}public function readers(){return $this->hasMany(User::class);}}".into();
        mock.add_file("src/Models/Post.php", &doc.text);
        mock.add_file("src/Resources/PersonResource.php","<?php namespace Domain\\Resources;use Illuminate\\Http\\Resources\\Json\\JsonResource;/** @mixin \\Domain\\Models\\User */ class PersonResource extends JsonResource {}");
        main.indexes
            .insert("/project".into(), Index::build(&project));
        for answers in [
            vec!["select"],
            vec!["title"],
            vec!["author", "readers"],
            vec!["Domain\\Resources\\PersonResource"],
            vec!["Domain\\Resources\\PersonResource"],
        ] {
            mock.answer_next_prompt(Some(&answers));
        }
        prepare_model_file_interactive(
            &main.artisan_terminals.model_plans,
            &project,
            main.indexes.get("/project").unwrap(),
            "resource",
            "Domain\\Models\\Post",
            "PostResource",
        )
        .unwrap();
        mock.answer_next_prompt(Some(&["yes"]));
        let action = resolve(&main, &doc, lazy(&main, &doc)).unwrap();
        let FileOperation::Create { text, .. } = &action.operations[0] else {
            panic!()
        };
        assert!(text.contains("'title' => $this->title"));
        assert!(!text.contains("external_id"));
        assert!(text.contains(
            "'author' => new \\Domain\\Resources\\PersonResource($this->whenLoaded('author'))"
        ));
        assert!(text.contains("'readers' => \\Domain\\Resources\\PersonResource::collection($this->whenLoaded('readers'))"));
        export("selected_resource", text);
        assert!(mock.process_runs().is_empty());
    }
    #[test]
    fn interactive_selection_rejects_unknown_fields_and_cancel_clears_old_pending_plan() {
        for failure in ["forged", "cancel"] {
            let (mock, main, project, doc) = setup();
            prepare(&main, &project, "resource", "OldResource");
            mock.answer_next_prompt(Some(&["select"]));
            if failure == "forged" {
                mock.answer_next_prompt(Some(&["unknown_column"]));
            } else {
                mock.answer_next_prompt(None);
            }
            assert!(
                prepare_model_file_interactive(
                    &main.artisan_terminals.model_plans,
                    &project,
                    main.indexes.get("/project").unwrap(),
                    "resource",
                    "Domain\\Models\\Post",
                    "PostResource"
                )
                .is_err()
            );
            assert!(!actions(&main, &doc).iter().any(|action| {
                action
                    .data
                    .as_ref()
                    .is_some_and(|data| data.starts_with("bulk-generate:"))
            }));
            assert!(
                !project
                    .exists("src/Http/Resources/PostResource.php")
                    .unwrap()
            );
        }
    }
    #[test]
    fn related_generation_uses_native_settings_destinations_and_postfix_defaults() {
        let (mock, main, project, doc) = setup();
        mock.add_file(".openidea/laravel-generators.json",r#"{"version":1,"settings":{"factoryBase":"Domain\\Support\\BaseFactory","resourceBase":"Domain\\Support\\BaseResource","factoryPostfix":"Maker","resourcePostfix":"Data","typeHints":false},"generators":{"model":{"availableInApplication":false},"factory":{"directory":"${sourceRoot}/Generated/Factories","namespace":"${namespace}\\Generated\\Factories"},"resource":{"directory":"${sourceRoot}/Generated/Resources","namespace":"${namespace}\\Generated\\Resources"}}}"#);
        let index = main.indexes.get("/project").unwrap();
        assert_eq!(
            related_default_name(&project, index, "factory", "Domain\\Models\\Post")
                .unwrap()
                .as_deref(),
            Some("PostMaker")
        );
        assert_eq!(
            related_default_name(&project, index, "resource", "Domain\\Models\\Post")
                .unwrap()
                .as_deref(),
            Some("PostData")
        );
        for (kind, name, path, base) in [
            (
                "factory",
                "Custom",
                "/project/src/Generated/Factories/Custom.php",
                "Domain\\Support\\BaseFactory",
            ),
            (
                "resource",
                "Custom",
                "/project/src/Generated/Resources/Custom.php",
                "Domain\\Support\\BaseResource",
            ),
        ] {
            prepare(&main, &project, kind, name);
            mock.answer_next_prompt(Some(&["yes"]));
            let resolved = resolve(&main, &doc, lazy(&main, &doc)).unwrap();
            let FileOperation::Create {
                path: actual, text, ..
            } = &resolved.operations[0]
            else {
                panic!()
            };
            assert_eq!(actual, path);
            assert!(text.contains(&format!("class Custom extends \\{base}")));
            assert!(!text.contains("): array"));
            assert!(!text.contains("CustomMaker"));
            export(&format!("configured_{kind}"), text);
        }
    }
    #[test]
    fn related_availability_and_conflicting_composer_paths_are_enforced() {
        for config in [
            r#"{"version":1,"generators":{"factory":{"availableInApplication":false}}}"#,
            r#"{"version":1,"generators":{"factory":{"directory":"wrong/factories","namespace":"Domain\\Factories"}}}"#,
            r#"{"version":1,"settings":{"factoryBase":"Database\\Factories\\PostFactory"}}"#,
        ] {
            let (mock, main, project, _) = setup();
            mock.add_file(".openidea/laravel-generators.json", config);
            assert!(
                prepare_model_file(
                    &main.artisan_terminals.model_plans,
                    &project,
                    main.indexes.get("/project").unwrap(),
                    "factory",
                    "Domain\\Models\\Post",
                    "PostFactory"
                )
                .is_err()
            );
            assert!(
                !project
                    .exists("database/factories/PostFactory.php")
                    .unwrap()
            );
            assert!(mock.process_runs().is_empty());
        }
    }
    #[test]
    fn related_module_options_use_local_composer_and_invalidate_changed_proof() {
        for changed in [false, true] {
            let (mock, mut main, project, _) = setup();
            let source = "<?php namespace Modules\\Blog\\Models;use Illuminate\\Database\\Eloquent\\Model;class Post extends Model {}";
            mock.add_dir("modules");
            mock.add_dir("modules/Blog");
            mock.add_file("modules/Blog/app/Models/Post.php", source);
            mock.set_setting("modelDirectories", "\"modules/Blog/app/Models\"");
            mock.add_file("modules/Blog/composer.json",r#"{"autoload":{"psr-4":{"Modules\\Blog\\":"app/"}},"autoload-dev":{"psr-4":{"Modules\\Blog\\Database\\Factories\\":"database/factories/"}}}"#);
            mock.add_file(".openidea/laravel-generators.json",r#"{"version":1,"generators":{"factory":{"availableInModules":false}},"modules":[{"id":"blog","label":"Blog","namespace":"Modules\\Blog","sourceRoot":"modules/Blog/app","migrationRoot":"modules/Blog/database/migrations","viewRoot":"modules/Blog/resources/views","generators":{"factory":{"availableInModules":true}}}]}"#);
            main.indexes
                .insert("/project".into(), Index::build(&project));
            let doc = Document {
                path: "/project/modules/Blog/app/Models/Post.php".into(),
                language: "php".into(),
                version: 1,
                text: source.into(),
            };
            prepare_model_file(
                &main.artisan_terminals.model_plans,
                &project,
                main.indexes.get("/project").unwrap(),
                "factory",
                "Modules\\Blog\\Models\\Post",
                "PostFactory",
            )
            .unwrap();
            let action = lazy(&main, &doc);
            if changed {
                mock.add_file("modules/Blog/composer.json",r#"{"autoload-dev":{"psr-4":{"Modules\\Blog\\Database\\Factories\\":"different/"}}}"#);
                assert!(resolve(&main, &doc, action).is_err());
            } else {
                mock.answer_next_prompt(Some(&["yes"]));
                let resolved = resolve(&main, &doc, action).unwrap();
                let FileOperation::Create { path, text, .. } = &resolved.operations[0] else {
                    panic!()
                };
                assert_eq!(
                    path,
                    "/project/modules/Blog/database/factories/PostFactory.php"
                );
                assert!(text.contains("namespace Modules\\Blog\\Database\\Factories;"));
                export("module_local_factory", text);
            }
        }
    }
    #[test]
    fn availability_changes_invalidate_a_prepared_related_action() {
        let (mock, main, project, doc) = setup();
        prepare(&main, &project, "resource", "PostResource");
        let action = lazy(&main, &doc);
        mock.add_file(
            ".openidea/laravel-generators.json",
            r#"{"version":1,"generators":{"resource":{"availableInApplication":false}}}"#,
        );
        assert!(resolve(&main, &doc, action).is_err());
        assert!(
            !project
                .exists("src/Http/Resources/PostResource.php")
                .unwrap()
        );
    }
    fn add_readable_fields(
        mock: &MockHost,
        project: &Project,
        main: &mut crate::Main,
        doc: &mut Document,
    ) {
        mock.add_file("src/Models/BasePost.php","<?php namespace Domain\\Models;use Illuminate\\Database\\Eloquent\\Model;/** @property-read string $phpdoc_only */class BasePost extends Model {protected $appends=['label'];protected $casts=['cast_only'=>'integer'];public function getDisplayNameAttribute(): string{return 'Visible';}public function getLabelAttribute(){return 'Label';}}");
        doc.text = "<?php namespace Domain\\Models;class Post extends BasePost {}".into();
        mock.add_file("src/Models/Post.php", &doc.text);
        main.indexes
            .insert("/project".into(), Index::build(project));
    }
    #[test]
    fn resources_select_owned_inherited_accessors_casts_appends_and_phpdoc_fields() {
        let (mock, mut main, project, mut doc) = setup();
        add_readable_fields(&mock, &project, &mut main, &mut doc);
        mock.answer_next_prompt(Some(&["select"]));
        mock.answer_next_prompt(Some(&["display_name", "label", "cast_only", "phpdoc_only"]));
        prepare_model_file_interactive(
            &main.artisan_terminals.model_plans,
            &project,
            main.indexes.get("/project").unwrap(),
            "resource",
            "Domain\\Models\\Post",
            "RichResource",
        )
        .unwrap();
        mock.answer_next_prompt(Some(&["yes"]));
        let resolved = resolve(&main, &doc, lazy(&main, &doc)).unwrap();
        let FileOperation::Create { text, .. } = &resolved.operations[0] else {
            panic!()
        };
        for name in ["display_name", "label", "cast_only", "phpdoc_only"] {
            assert!(
                text.contains(&format!("'{name}' => $this->{name}")),
                "{name}"
            );
        }
        assert!(!text.contains("'title'"));
        assert!(mock.process_runs().is_empty());
        export("rich_resource", text);
        // Read-only virtual attributes never become factory write inputs.
        mock.answer_next_prompt(Some(&["select"]));
        mock.answer_next_prompt(Some(&["display_name"]));
        assert!(
            prepare_model_file_interactive(
                &main.artisan_terminals.model_plans,
                &project,
                main.indexes.get("/project").unwrap(),
                "factory",
                "Domain\\Models\\Post",
                "PostFactory"
            )
            .is_err()
        );
    }
    #[test]
    fn readable_resource_dependencies_are_revalidated_and_fake_phpdoc_is_ignored() {
        let (mock, mut main, project, mut doc) = setup();
        add_readable_fields(&mock, &project, &mut main, &mut doc);
        mock.answer_next_prompt(Some(&["select"]));
        mock.answer_next_prompt(Some(&["phpdoc_only"]));
        prepare_model_file_interactive(
            &main.artisan_terminals.model_plans,
            &project,
            main.indexes.get("/project").unwrap(),
            "resource",
            "Domain\\Models\\Post",
            "RichResource",
        )
        .unwrap();
        let action = lazy(&main, &doc);
        let changed = project
            .read_file("src/Models/BasePost.php")
            .unwrap()
            .replace("string $phpdoc_only", "int $phpdoc_only");
        mock.add_file("src/Models/BasePost.php", &changed);
        assert!(resolve(&main, &doc, action).is_err());
        doc.text="<?php namespace Domain\\Models;use Illuminate\\Database\\Eloquent\\Model; // /** @property-read string $fake */\nclass Post extends Model {public function data(){return '$appends=[\"fake\"];';}}".into();
        mock.add_file("src/Models/Post.php", &doc.text);
        main.indexes
            .insert("/project".into(), Index::build(&project));
        mock.answer_next_prompt(Some(&["select"]));
        mock.answer_next_prompt(Some(&["fake"]));
        assert!(
            prepare_model_file_interactive(
                &main.artisan_terminals.model_plans,
                &project,
                main.indexes.get("/project").unwrap(),
                "resource",
                "Domain\\Models\\Post",
                "RichResource"
            )
            .is_err()
        );
    }
    fn captured_fields(
        mock: &MockHost,
        main: &mut crate::Main,
        payload: serde_json::Value,
    ) -> serde_json::Value {
        mock.add_file("artisan", "<?php");
        main.runtime_inspection
            .capture_schema_fixture(&mock.invocation(), payload)
            .unwrap();
        main.runtime_inspection
            .schema_snapshot(&mock.invocation(), crate::artisan_runner::Runner::Php)
            .unwrap()
    }
    #[test]
    fn resources_can_select_matching_captured_fields_but_changed_capture_is_stale() {
        for changed in ["none", "snapshot", "config"] {
            let (mock, mut main, project, mut doc) = setup();
            let mock = mock.with_capabilities(&[
                "project.read",
                "project.write",
                "storage",
                "editor.read",
                "events",
                "process:php",
            ]);
            doc.text="<?php namespace Domain\\Models;use Illuminate\\Database\\Eloquent\\Model;class Post extends Model {protected $connection='main';}".into();
            mock.add_file("src/Models/Post.php", &doc.text);
            mock.add_file("artisan", "<?php");
            main.indexes
                .insert("/project".into(), Index::build(&project));
            let payload = serde_json::json!({"connection":"main","tables":[{"name":"posts","columns":[{"name":"runtime_only","type":"string","nullable":false}],"indexes":[]}]});
            let snapshot = captured_fields(&mock, &mut main, payload.clone());
            mock.answer_next_prompt(Some(&["select"]));
            mock.answer_next_prompt(Some(&["runtime_only"]));
            prepare_model_file_interactive_with_schema(
                &main.artisan_terminals.model_plans,
                &project,
                main.indexes.get("/project").unwrap(),
                "resource",
                "Domain\\Models\\Post",
                "RuntimeResource",
                Some((crate::artisan_runner::Runner::Php, snapshot)),
            )
            .unwrap();
            let action = lazy(&main, &doc);
            if changed == "snapshot" {
                let mut changed = payload;
                changed["tables"][0]["columns"][0]["name"] = serde_json::json!("replacement");
                captured_fields(&mock, &mut main, changed);
                assert!(resolve(&main, &doc, action).is_err());
            } else if changed == "config" {
                mock.add_file("config/database.php", "<?php return ['default'=>'main'];");
                assert!(resolve(&main, &doc, action).is_err());
            } else {
                mock.answer_next_prompt(Some(&["yes"]));
                let resolved = resolve(&main, &doc, action).unwrap();
                let FileOperation::Create { text, .. } = &resolved.operations[0] else {
                    panic!()
                };
                assert!(text.contains("'runtime_only' => $this->runtime_only"));
                export("runtime_resource", text);
                assert!(
                    mock.process_runs().is_empty(),
                    "generation only reads the explicit capture"
                );
            }
        }
    }
    #[test]
    fn runtime_resource_fields_require_matching_connection_and_unique_table() {
        for case in ["mismatch", "ambiguous", "dynamic"] {
            let (mock, mut main, project, mut doc) = setup();
            let mock = mock.with_capabilities(&[
                "project.read",
                "project.write",
                "storage",
                "editor.read",
                "events",
                "process:php",
            ]);
            doc.text="<?php namespace Domain\\Models;use Illuminate\\Database\\Eloquent\\Model;class Post extends Model {protected $connection='main';}".into();
            if case == "dynamic" {
                doc.text = doc.text.replace(
                    "$connection='main';",
                    "$connection='main'; public function getTable(){return 'unknown';}",
                );
            }
            mock.add_file("src/Models/Post.php", &doc.text);
            main.indexes
                .insert("/project".into(), Index::build(&project));
            let table = serde_json::json!({"name":"posts","schema":"public","columns":[{"name":"runtime_only","type":"string","nullable":false}],"indexes":[]});
            let mut payload = serde_json::json!({"connection":if case=="mismatch"{"tenant"}else{"main"},"tables":[table.clone()]});
            if case == "ambiguous" {
                let mut other = table;
                other["schema"] = serde_json::json!("other");
                payload["tables"].as_array_mut().unwrap().push(other);
            }
            let snapshot = captured_fields(&mock, &mut main, payload);
            mock.answer_next_prompt(Some(&["select"]));
            mock.answer_next_prompt(Some(&["runtime_only"]));
            assert!(
                prepare_model_file_interactive_with_schema(
                    &main.artisan_terminals.model_plans,
                    &project,
                    main.indexes.get("/project").unwrap(),
                    "resource",
                    "Domain\\Models\\Post",
                    "RuntimeResource",
                    Some((crate::artisan_runner::Runner::Php, snapshot))
                )
                .is_err()
            );
        }
    }
    #[test]
    fn explicit_module_factory_namespace_and_composer_root_are_preserved() {
        let (mock, mut main, project, doc) = setup();
        mock.add_file(".openidea/laravel-generators.json",r#"{"version":1,"modules":[{"id":"domain","label":"Domain","namespace":"Domain","sourceRoot":"src","migrationRoot":"database/migrations","viewRoot":"resources/views","factoryRoot":"generated/factories","factoryNamespace":"Domain\\Factories"}]}"#);
        mock.add_file("composer.json",r#"{"autoload":{"psr-4":{"Domain\\":"src/"}},"autoload-dev":{"psr-4":{"Domain\\Factories\\":"generated/factories/"}}}"#);
        main.indexes.insert("/project".into(), Index::build(&project));
        prepare(&main, &project, "factory", "PostFactory");
        mock.answer_next_prompt(Some(&["yes"]));
        let resolved = resolve(&main, &doc, lazy(&main, &doc)).unwrap();
        let FileOperation::Create { path, text, .. } = &resolved.operations[0] else {
            panic!()
        };
        assert_eq!(path, "/project/generated/factories/PostFactory.php");
        assert!(text.contains("namespace Domain\\Factories;"));
        assert!(text.contains("protected $model = \\Domain\\Models\\Post::class;"));
        export("module_factory", text);
    }
    #[test]
    fn cross_module_related_preview_preserves_model_and_captures_template_changes() {
        let (mock, _, project, _) = setup();
        mock.add_file("composer.json",r#"{"autoload":{"psr-4":{"Domain\\":"src/","Modules\\Shop\\":"modules/Shop/app/"}},"autoload-dev":{"psr-4":{"Modules\\Shop\\Factories\\":"modules/Shop/database/factories/"}}}"#);
        mock.add_file(".openidea/laravel-generators.json",r#"{"version":1,"modules":[{"id":"shop","label":"Shop","namespace":"Modules\\Shop","sourceRoot":"modules/Shop/app","migrationRoot":"modules/Shop/database/migrations","viewRoot":"modules/Shop/resources/views","factoryRoot":"modules/Shop/database/factories","factoryNamespace":"Modules\\Shop\\Factories","generators":{"factory":{"template":"factory.php"}}}]}"#);
        mock.add_file(
            ".openidea/laravel-templates/factory.php",
            "${generated}\n// model ${model}\n",
        );
        let index = Index::build(&project);
        let (path, text, evidence) = related_preview_in_module(
            &project,
            &index,
            "factory",
            "Domain\\Models\\Post",
            "PostFactory",
            Some(Some("shop")),
            &serde_json::json!({}),
        )
        .unwrap();
        assert_eq!(
            path,
            "/project/modules/Shop/database/factories/PostFactory.php"
        );
        assert!(
            text.contains("namespace Modules\\Shop\\Factories;"),
            "{text}"
        );
        assert!(text.contains("\\Domain\\Models\\Post::class"), "{text}");
        assert!(text.contains("// model Domain\\Models\\Post"));
        mock.add_file(
            ".openidea/laravel-templates/factory.php",
            "${generated}\n// changed\n",
        );
        let (_, changed, new_evidence) = related_preview_in_module(
            &project,
            &index,
            "factory",
            "Domain\\Models\\Post",
            "PostFactory",
            Some(Some("shop")),
            &serde_json::json!({}),
        )
        .unwrap();
        assert_ne!(evidence, new_evidence);
        assert_ne!(text, changed);
        assert!(mock.process_runs().is_empty());
    }
    #[test]
    fn same_short_name_in_two_namespaces_requires_explicit_fqn() {
        let (mock, main, project, _) = setup();
        mock.add_file("src/Admin/Models/Post.php","<?php namespace Domain\\Admin\\Models;use Illuminate\\Database\\Eloquent\\Model;class Post extends Model {}");
        let index = Index::build(&project);
        assert!(identity(&index, "Post").is_none());
        assert!(identity(&index, "Domain\\Admin\\Models\\Post").is_some());
        assert!(
            prepare_model_file(
                &main.artisan_terminals.model_plans,
                &project,
                &index,
                "factory",
                "Post",
                "PostFactory"
            )
            .is_err()
        );
    }
}
