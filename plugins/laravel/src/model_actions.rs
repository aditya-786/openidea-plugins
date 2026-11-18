//! Native model helper and field-migration actions. Every multi-file edit is
//! returned to the host as one undoable transaction, never written directly.
use crate::{eloquent_members, generator_plan, index::Index, php};
use openidea_plugin::{
    Error, Position, Range, TextEdit,
    file_actions::{FileAction, FileOperation},
    host,
    providers::Document,
    text,
};
use std::collections::BTreeSet;

#[path="model_helper_parameters.rs"]
pub(crate) mod parameters;
pub(crate) fn configure(ctx:&openidea_plugin::Invocation)->Result<(),Error>{parameters::configure(ctx)}
/// A model that anchors the project-wide helper regeneration action.
pub(crate) fn helper_anchor(ctx:&openidea_plugin::Invocation,index:&Index)->Result<String,Error>{
    let project=ctx.project().ok_or(crate::ui_text::text("native.model_actions.a74a0baad4", "Open a Laravel project first"))?;
    if !crate::index::index_sources_current(&project,index)||index.partial{return Err(crate::still_indexing());}
    let models=eloquent_members::model_identities(&index.eloquent_models).into_iter().filter(|(full,path)|helper_codegen::eligible(&index,full)&&crate::owns_current_path(&project,path)).collect::<Vec<_>>();
    let (_,path)=models.into_iter().next().ok_or_else(||Error::Invalid(crate::ui_text::t("native.model_actions.noHelperModels", "No Eloquent model in this project can have helpers generated.")))?;
    Ok(path)
}

fn model(index: &Index, path: &str) -> Option<(String, String)> {
    let (full,_)=eloquent_members::model_identities(&index.eloquent_models).into_iter().find(|(_,p)|p==path)?;
    let table=eloquent_members::model_inventory(&index.eloquent_models).into_iter().find(|(name,_,_)|name==&full).map(|(_,_,table)|table).unwrap_or_default();
    Some((full,table))
}
pub(crate) fn actions(index: &Index, doc: &Document) -> Vec<FileAction> {
    if doc.language != "php" || index.partial || model(index, &doc.path).is_none() {
        return Vec::new();
    }
    [
        ("helpers", "Generate model helpers…"),
        (
            "helpers-all",
            "Regenerate helpers for all models…",
        ),
        (
            "docs",
            "Refresh generated model PHPDoc (preserve handwritten)…",
        ),
        ("fields", "Create model fields and migration…"),
    ]
    .into_iter()
    .filter(|(kind, _)| {
        *kind == "fields"
            || model(index, &doc.path)
                .is_some_and(|(full, _)| helper_codegen::eligible(index, &full))
    })
    .map(|(kind, title)| FileAction {
        title: title.into(),
        kind: Some("refactor.rewrite".into()),
        data: Some(format!("model-native:{kind}:{}", doc.version)),
        ..Default::default()
    })
    .collect()
}
fn owner(doc: &Document) -> Result<openidea_plugin::Project, Error> {
    let project = host::project_of(&doc.path).ok_or(Error::Stale)?;
    if !crate::owns_current_path(&project, &doc.path) || project.read_file(&doc.path)? != doc.text {
        return Err(Error::Stale);
    }
    Ok(project)
}
#[path = "model_helper_codegen.rs"]
mod helper_codegen;
fn generation_options(main:Option<&crate::Main>,index:&Index,project:&openidea_plugin::Project,full:&str,preferences:&parameters::Parameters)->Result<helper_codegen::Options,Error>{
    let schema=if preferences.source=="migrations" {None}else{
        let main=main.ok_or_else(||Error::Invalid(crate::ui_text::text("native.model_actions.40648a12a2", "Use the project helper command with refreshed model/database captures").into()))?;
        let path=eloquent_members::model_identities(&index.eloquent_models).into_iter().find(|(name,_)|name==full).map(|(_,path)|path).ok_or(Error::Stale)?;
        let runner=main.artisan_workspace.runner_for_project(project)?;
        let capture=main.runtime_inspection.helper_schema(project,runner,full,&path);
        match capture {
            Ok(Some(value))=>Some(value),
            other=>{
                // A literal, currently proven named connection/table can join
                // a schema capture without re-running PHP after a model save.
                // Dynamic bindings still require their exact refreshed capture.
                let connection=match eloquent_members::related_facts::connection_selection(index,project,full) {
                    Some(eloquent_members::related_facts::ConnectionSelection::Named(value))=>Some(value),
                    Some(eloquent_members::related_facts::ConnectionSelection::ConfiguredDefault)=>main.runtime_inspection.captured_default_connection(project,runner)?,
                    None=>None,
                };
                let table=eloquent_members::helper_facts::facts(index,full,&|path|crate::owns_current_path(project,path)).and_then(|facts|facts.table);
                let schema=main.runtime_inspection.schema_for_generation(project,runner)?;
                let matches=schema["tables"].as_array().into_iter().flatten().filter(|candidate|{
                    let Some(name)=candidate["name"].as_str()else{return false;};
                    table.as_deref().is_some_and(|table|if table.contains('.') {table==format!("{}.{name}",candidate["schema"].as_str().unwrap_or(""))}else{table==name})
                }).collect::<Vec<_>>();
                if connection.as_deref().is_some_and(|connection|schema["connection"]==connection)&&matches.len()==1 {
                    let table=matches[0];Some(serde_json::json!({"model":full,"file":path,"connection":schema["connection"],"table":table["name"],"schema":table["schema"],"columns":table["columns"],"indexes":table["indexes"],"capturedAt":schema["capturedAt"],"runner":runner.name(),"binding":"proven literal source","modelSourceHash":crate::artisan_metadata::hash(&project.read_file(&path)?)}))
                }else{other?}
            }
        }
    };
    Ok(helper_codegen::Options{relation_counts:preferences.relation_counts,date_class:preferences.date_class.clone(),source:preferences.source.clone(),schema,collection_proxies:preferences.collection_proxies})
}
fn tags(index:&Index,project:&openidea_plugin::Project,full:&str,options:helper_codegen::Options)->Result<String,Error>{helper_codegen::tags(index,project,full,options)}


/// Regenerable helper-only output for an explicitly enabled save hook. Never
/// returns application source; the automation state validates ownership again.
pub(crate) fn automatic_helper_outputs_with_index(main:&crate::Main,project:&openidea_plugin::Project,index:&Index,path:&str)->Result<Vec<(String,String)>,Error>{
    let preferences=parameters::load(project)?;
    if !preferences.on_save||preferences.storage!="helpers"||path.contains("/.openidea/"){return Ok(vec![]);}
    if !crate::owns_current_path(project,path){return Err(Error::Stale);}
    if index.partial{return Err(Error::Stale);}
    let Some((full,_))=model(index,path)else{return Ok(vec![])};
    if !helper_codegen::eligible(&index,&full){return Ok(vec![]);}
    let before=project.read_file(path)?;
    let options=generation_options(Some(main),&index,project,&full,&preferences)?;
    let tags=helper_codegen::tags(&index,project,&full,options.clone())?;
    let mut files=Vec::new();
    if preferences.collection_proxies {files.push(proxy_output(project,&full)?);}
    files.push((format!("{}/.openidea/helpers/{}.php",project.root()?,full.replace('\\',"_")),helper_text(&full,&tags)?));
    for(target,_)in &files{
        check_target(project,target)?;
        match project.read_file(target){Ok(source)=>{let valid=if target.contains("/proxies/"){source.starts_with(&format!("<?php\n// Generated by OpenIDEA Laravel. Do not edit this helper.\n// Collection proxies for {full}\n"))}else{source.starts_with("<?php\n// Generated by OpenIDEA Laravel. Do not edit this helper.\n")&&eloquent_members::declaration(&source).as_deref()==Some(full.as_str())};if !valid{return Err(Error::Invalid(crate::ui_text::text("native.model_actions.8d162054ef", "Automatic helper target is not plugin-owned").into()));}},Err(Error::NotFound(_))=>{},Err(error)=>return Err(error)}
    }
    if parameters::load(project)?!=preferences||project.read_file(path)?!=before{return Err(Error::Stale);}
    Ok(files)
}

fn entire(text: &str, replacement: String) -> TextEdit {
    TextEdit {
        range: Range::new(Position::new(0, 0), text::position(text, text.len())),
        text: replacement,
    }
}
fn replace_docs(source: &str, tags: &str) -> Result<String, Error> {
    let (class, _) = eloquent_members::class_layout(source).ok_or(Error::Stale)?;
    let start_marker = " * @openidea-laravel-generated-start\n";
    let end_marker = " * @openidea-laravel-generated-end\n";
    let prefix = source[..class].trim_end();
    let manual = if prefix.ends_with("*/") {
        prefix
            .rfind("/**")
            .map(|begin| &prefix[begin..])
            .unwrap_or("")
    } else {
        ""
    };
    let mut manual = manual.to_owned();
    if let Some(begin) = manual.find(start_marker) {
        if let Some(end) = manual[begin..].find(end_marker) {
            manual.replace_range(begin..begin + end + end_marker.len(), "");
        }
    }
    let property_name = |line: &str| {
        line.split_whitespace()
            .find(|part| part.starts_with('$'))
            .map(|name| {
                name.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '_')
                    .to_owned()
            })
    };
    let method_name = |line: &str| {
        line.split_once('(')
            .and_then(|(before, _)| before.split_whitespace().last())
            .map(str::to_owned)
    };
    let manual_properties = manual
        .lines()
        .filter(|line| line.contains("@property"))
        .filter_map(property_name)
        .collect::<BTreeSet<_>>();
    let manual_methods = manual
        .lines()
        .filter(|line| line.contains("@method"))
        .filter_map(method_name)
        .collect::<BTreeSet<_>>();
    let tags = tags
        .lines()
        .filter(|line| {
            if line.contains("@property") {
                !property_name(line).is_some_and(|name| manual_properties.contains(&name))
            } else if line.contains("@method") {
                !method_name(line).is_some_and(|name| manual_methods.contains(&name))
            } else {
                true
            }
        })
        .map(|line| format!("{line}\n"))
        .collect::<String>();
    let block = format!("{start_marker}{tags}{end_marker}");
    let mut result = source.to_owned();
    if prefix.ends_with("*/") {
        let end = prefix.len() - 2;
        // Extend the immediately preceding PHPDoc only; arbitrary comments are
        // never interpreted as a generated documentation ownership marker.
        let begin = prefix.rfind("/**").ok_or(Error::Stale)?;
        if prefix[begin + 3..end].contains("*/") {
            return Err(Error::Stale);
        }
        if let Some(relative) = prefix[begin..end].find(start_marker) {
            let start = begin + relative;
            let finish = start
                + start_marker.len()
                + source[start + start_marker.len()..end]
                    .find(end_marker)
                    .ok_or(Error::Stale)?
                + end_marker.len();
            result.replace_range(start..finish, &block);
            return Ok(result);
        }
        result.insert_str(end, &format!("\n{block} "));
    } else {
        result.insert_str(class, &format!("/**\n{block} */\n"));
    }
    Ok(result)
}
fn fillable(source: &str, fields: &[generator_plan::Field]) -> Result<String, Error> {
    let (_, body, array) = eloquent_members::edit_layout(source).ok_or(Error::Stale)?;
    let mut result = source.to_owned();
    let mut additions = Vec::new();
    if let Some(range) = array {
        let mut existing = BTreeSet::new();
        for item in php::items(source, range.start) {
            let (name, _) = php::literal(source, item.value).ok_or_else(|| {
                Error::Invalid(crate::ui_text::text("native.model_actions.4218eb2c60", "Only literal $fillable arrays can be updated").into())
            })?;
            existing.insert(name.to_owned());
        }
        additions.extend(
            fields
                .iter()
                .filter(|f| !existing.contains(&f.name))
                .map(|f| format!("'{}'", f.name)),
        );
        if !additions.is_empty() {
            let body = source[range.start + 1..range.end - 1].trim();
            let separator = if body.is_empty() || body.ends_with(',') {
                ""
            } else {
                ","
            };
            result.insert_str(
                range.end - 1,
                &format!("{separator} {}", additions.join(", ")),
            );
        }
    } else {
        additions.extend(fields.iter().map(|f| format!("'{}'", f.name)));
        result.insert_str(
            body,
            &format!("\n    protected $fillable = [{}];\n", additions.join(", ")),
        );
    }
    Ok(result)
}

pub(crate) fn resolve(
    index: &Index,
    doc: &Document,
    action: FileAction,
) -> Result<FileAction, Error> {
    resolve_inner(None,index,doc,action)
}
pub(crate) fn resolve_current(main:&crate::Main,index:&Index,doc:&Document,action:FileAction)->Result<FileAction,Error>{
    let helper=action.data.as_deref().is_some_and(|data|data.starts_with("model-native:helpers")||data.starts_with("model-native:docs"));
    let result=resolve_inner(Some(main),index,doc,action)?;
    if helper {let project=owner(doc)?;let preferences=parameters::load(&project)?;main.helper_automation.arm(&project,main.artisan_workspace.runner_for_project(&project)?,&result,&preferences.post_commands)?;}
    Ok(result)
}
fn resolve_inner(main:Option<&crate::Main>,index:&Index,doc:&Document,action:FileAction)->Result<FileAction,Error>{
    let data = action.data.as_deref().ok_or(Error::Stale)?;
    let kind = ["helpers", "docs", "fields", "helpers-all"]
        .into_iter()
        .find(|kind| data == format!("model-native:{kind}:{}", doc.version))
        .ok_or(Error::Stale)?;
    if kind == "helpers-all" {
        return resolve_batch(main, index, doc, action);
    }
    let project = owner(doc)?;
    let root = project.root()?;
    if !crate::index::index_sources_current(&project,index){return Err(Error::Stale);}
    let current = index;
    let (full, table) = model(&current, &doc.path).ok_or(Error::Stale)?;
    if model(index, &doc.path) != Some((full.clone(), table.clone())) || current.partial {
        return Err(Error::Stale);
    }
    if kind != "fields" && !helper_codegen::eligible(&current, &full) {
        return Err(Error::Invalid(
            crate::ui_text::text("native.model_actions.93942f79df", "Model is excluded from helper generation by project metadata").into(),
        ));
    }
    let preferences=parameters::load(&project)?;
    let options=if kind=="fields"{helper_codegen::Options::default()}else{generation_options(main,&current,&project,&full,&preferences)?};
    let generated_tags = if kind=="fields"{String::new()}else{tags(&current,&project,&full,options.clone())?};
    let mut operations = Vec::new();
    let mut edits = Vec::new();
    let mut expected = Vec::new();
    let mut open = Some(doc.path.clone());
    if kind == "helpers" && preferences.storage=="helpers" {
        let path = format!("{root}/.openidea/helpers/{}.php", full.replace('\\', "_"));
        check_target(&project,&path)?;
        let text=helper_text(&full,&generated_tags)?;
        match project.read_file(&path) {
            Ok(previous)
                if previous.starts_with("<?php\n// Generated by OpenIDEA Laravel. Do not edit this helper.\n")
                    && eloquent_members::declaration(&previous).as_deref() == Some(&full) =>
            {
                expected.push((path.clone(), previous.clone()));
                edits.push((path.clone(), vec![entire(&previous, text)]));
            }
            Ok(_) => {
                return Err(Error::Invalid(
                    crate::ui_text::text("native.model_actions.b536efd01d", "Helper destination is not owned by this plugin").into(),
                ));
            }
            Err(Error::NotFound(_)) => operations.push(FileOperation::Create {
                path: path.clone(),
                text,
                overwrite: false,
            }),
            Err(e) => return Err(e),
        }
        open = Some(path);
    } else if kind == "docs" || kind=="helpers" {
        edits.push((
            doc.path.clone(),
            vec![entire(&doc.text, if kind=="helpers"&&preferences.storage=="overwrite"{replace_all_docs(&doc.text,&generated_tags)?}else{replace_docs(&doc.text, &generated_tags)?})],
        ));
    } else {
        let raw = host::input(
            crate::ui_text::text("native.model_actions.1afd7f9a96", "Fields JSON (name, kind, nullable, length, precision, scale, default)"),
            Some(r#"[{"name":"title","kind":"string"}]"#),
        )?
        .ok_or(Error::Cancelled)?;
        if raw.len() > 32 * 1024 {
            return Err(Error::Invalid(crate::ui_text::text("native.model_actions.0deed3c419", "Field specification too large").into()));
        }
        let fields: Vec<generator_plan::Field> =
            serde_json::from_str(&raw).map_err(|e| Error::Invalid(e.to_string()))?;
        if fields.is_empty() || fields.len() > 64 {
            return Err(Error::Invalid(crate::ui_text::text("native.model_actions.bf0a405a6e", "Choose 1–64 fields").into()));
        }
        let mut seen = BTreeSet::new();
        let mut lines = String::new();
        for field in &fields {
            if !generator_plan::identifier(&field.name)
                || !seen.insert(field.name.clone())
                || current
                    .columns
                    .get(&table)
                    .is_some_and(|names| names.contains_key(&field.name))
            {
                return Err(Error::Invalid(
                    crate::ui_text::text("native.model_actions.9b3d624ec9", "Field names must be unique new column identifiers").into(),
                ));
            }
            if field.cast.is_some() {
                return Err(Error::Invalid(
                    crate::ui_text::text("native.model_actions.abfa5cc0f8", "Create Fields currently requires casts to be edited separately").into(),
                ));
            }
            lines.push_str(&generator_plan::field_line(field).map_err(Error::Invalid)?);
        }
        let relative = host::input(
            crate::ui_text::text("generator.text.d163301ea9", "New migration path"),
            Some(&format!(
                "database/migrations/{}_add_fields_to_{table}.php",
                migration_stamp()
            )),
        )?
        .ok_or(Error::Cancelled)?;
        if !generator_plan::relative(&relative) || !relative.ends_with(".php") {
            return Err(Error::Invalid(
                crate::ui_text::text("native.model_actions.712fa8b1f4", "Choose a relative PHP migration path").into(),
            ));
        }
        let path = format!("{root}/{relative}");
        if !crate::owns_current_path(&project, &path) {
            return Err(Error::Stale);
        }
        match project.stat(&path) {
            Err(Error::NotFound(_)) => {}
            Ok(_) => return Err(Error::Invalid(crate::ui_text::text("native.model_actions.442d672cb0", "Migration already exists").into())),
            Err(e) => return Err(e),
        }
        if !generator_plan::identifier(&table) {
            return Err(Error::Invalid(
                crate::ui_text::text("native.model_actions.ded3508a60", "Table must be an ordinary identifier").into(),
            ));
        }
        let down = fields
            .iter()
            .map(|f| format!("'{}'", f.name))
            .collect::<Vec<_>>()
            .join(", ");
        let migration = format!(
            "<?php\n\nuse Illuminate\\Database\\Migrations\\Migration;\nuse Illuminate\\Database\\Schema\\Blueprint;\nuse Illuminate\\Support\\Facades\\Schema;\n\nreturn new class extends Migration {{\n    public function up(): void {{\n        Schema::table('{table}', function (Blueprint $table): void {{\n{lines}        }});\n    }}\n    public function down(): void {{\n        Schema::table('{table}', function (Blueprint $table): void {{\n            $table->dropColumn([{down}]);\n        }});\n    }}\n}};\n"
        );
        operations.push(FileOperation::Create {
            path: path.clone(),
            text: migration,
            overwrite: false,
        });
        let update = host::pick(
            crate::ui_text::text("native.model_actions.0ec5984edc", "Model mass-assignment update"),
            &[
                openidea_plugin::PickItem::new("fillable", crate::ui_text::text("native.model_actions.61d9bc3c23", "Add fields to $fillable")),
                openidea_plugin::PickItem::new("keep", crate::ui_text::text("native.model_actions.c1a6bc7a08", "Keep current model configuration")),
            ],
        )?
        .ok_or(Error::Cancelled)?;
        match update.as_str() {
            "fillable" => edits.push((
                doc.path.clone(),
                vec![entire(&doc.text, fillable(&doc.text, &fields)?)],
            )),
            "keep" => {}
            _ => return Err(Error::Stale),
        }
        open = Some(path);
    }
    if kind!="fields" {plan_proxies(&project,&full,preferences.collection_proxies,&mut operations,&mut edits,&mut expected)?;}
    let post_note=if kind!="fields"&&!preferences.post_commands.is_empty(){crate::ui_text::format("root.formatted.2aa1877117", "After successful application, run these Artisan argv arrays: {argv}\n", &[("argv", &format!("{}", serde_json::to_string(&preferences.post_commands).map_err(|_|Error::Stale)?))])}else{String::new()};
    let preview = crate::ui_text::format("root.formatted.7f9dfeda81", "{post_note}Project: {root}\nModel: {full}\n{warning}\n{operations}\n{edits}\nApply as one undoable workspace edit?", &[("post_note", &format!("{}", post_note)), ("root", &format!("{}", root)), ("full", &format!("{}", full)), ("warning", &format!("{}", if kind=="helpers"&&preferences.storage=="overwrite" { crate::ui_text::text("root.formatted.c5b2cef2f5", "Saved overwrite mode: replaces attached model PHPDoc, including handwritten documentation. Review the entire resulting model below.") } else { "" })), ("operations", &format!("{}", operations
            .iter()
            .map(|op| match op {
                FileOperation::Create { path, text, .. } =>
                    crate::ui_text::format("root.formatted.c27b321b8c", "Create (no overwrite): {path}\n{text}", &[("path", &format!("{}", path)), ("text", &format!("{}", text))]),
                FileOperation::Delete{path}=>crate::ui_text::format("root.formatted.d35e67100c", "Delete obsolete plugin-owned support file: {path}", &[("path", &format!("{}", path))]),
                _ => unreachable!("model actions do not rename files"),
            })
            .collect::<Vec<_>>()
            .join("\n"))), ("edits", &format!("{}", edits
            .iter()
            .map(|(path, edits)| format!(
                "{path}\n{}",
                edits
                    .iter()
                    .map(|e| e.text.as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            ))
            .collect::<Vec<_>>()
            .join("\n")))]);
    if !ui_kit::prompts::confirm_exact(crate::ui_text::text("root.formatted.2159df2e7f", "Apply model generation?"), &preview)? {
        return Err(Error::Cancelled);
    }
    owner(doc)?;
    // Regenerate facts after review to avoid applying a cached schema plan.
    if !crate::index::index_sources_current(&project,index){return Err(Error::Stale);}
    let fresh = index;
    if parameters::load(&project)?!=preferences || model(&fresh, &doc.path) != Some((full.clone(), table.clone()))
        || kind != "fields" && !helper_codegen::eligible(&fresh, &full)
        || kind!="fields" && (generation_options(main,&fresh,&project,&full,&preferences)?!=options || tags(&fresh,&project,&full,options.clone())? != generated_tags)
    {
        return Err(Error::Stale);
    }
    for (path, _) in &edits {
        check_target(&project, path)?;
    }
    for (path, source) in &expected {
        if project.read_file(path)? != *source {
            return Err(Error::Stale);
        }
    }
    for op in &operations {
        if let FileOperation::Create { path, .. } = op {
            check_target(&project, path)?;
            if !matches!(project.stat(path), Err(Error::NotFound(_))) {
                return Err(Error::Stale);
            }
        }
    }
    Ok(FileAction {
        title: action.title,
        operations,
        edits,
        open,
        ..Default::default()
    })
}

fn check_target(project: &openidea_plugin::Project, path: &str) -> Result<(), Error> {
    if !crate::owns_current_path(project, path) {
        return Err(Error::Stale);
    }
    let root = project.root()?;
    let relative = path.strip_prefix(&format!("{root}/")).ok_or(Error::Stale)?;
    if !generator_plan::relative(relative) {
        return Err(Error::Stale);
    }
    let mut parts = relative.split('/').collect::<Vec<_>>();
    parts.pop();
    while !parts.is_empty() {
        let parent = parts.join("/");
        match project.stat(&parent) {
            Ok(stat) if stat.kind == openidea_plugin::FileKind::Directory => {}
            Err(Error::NotFound(_)) => {}
            Ok(_) => {
                return Err(Error::Invalid(crate::ui_text::format("root.formatted.96701f8b55", "Target ancestor is not a directory: {parent}", &[("parent", &format!("{}", parent))])));
            }
            Err(e) => return Err(e),
        }
        parts.pop();
    }
    Ok(())
}

fn migration_stamp() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Proleptic Gregorian conversion, UTC, independent of desktop locale.
    let days = (seconds / 86400) as i64 + 719468;
    let era = days / 146097;
    let doe = days - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    if month <= 2 {
        year += 1;
    }
    format!(
        "{year:04}_{month:02}_{day:02}_{:02}{:02}{:02}",
        seconds / 3600 % 24,
        seconds / 60 % 60,
        seconds % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    fn setup() -> (MockHost, Index, Document) {
        let host = MockHost::install();
        host.add_file("composer.lock",r#"{"packages":[{"name":"laravel/framework","version":"v13.40.0"}]}"#);
        let source = "<?php\nnamespace App\\Models;\nuse Illuminate\\Database\\Eloquent\\Model;\n/** Human-written description. */\nfinal class Post extends Model { protected $fillable = ['title']; }";
        host.add_file("app/Models/Post.php", source);
        host.add_file(
            "database/migrations/001.php",
            "<?php Schema::create('posts',function($t){$t->string('title');});",
        );
        let project = host::projects().unwrap().remove(0);
        let index = Index::build(&project);
        (
            host,
            index,
            Document {
                path: "/project/app/Models/Post.php".into(),
                language: "php".into(),
                version: 1,
                text: source.into(),
            },
        )
    }
    #[test]
    fn phpdoc_preserves_handwritten_content_and_refresh_is_idempotent() {
        let source = "<?php /** Human-written description. */ final class Post extends Model {}";
        let tags = " * @property string $title\n";
        let first = replace_docs(source, tags).unwrap();
        assert!(first.contains("Human-written description."));
        assert!(first.contains("*/ final class"));
        assert_eq!(replace_docs(&first, tags).unwrap(), first);
    }
    #[test]
    fn helper_action_creates_owned_file_without_process_or_direct_write() {
        let (h, index, doc) = setup();
        let action = actions(&index, &doc)
            .into_iter()
            .find(|a| a.data.as_deref() == Some("model-native:helpers:1"))
            .unwrap();
        h.answer_next_prompt(Some(&["yes"]));
        let resolved = resolve(&index, &doc, action).unwrap();
        assert_eq!(resolved.operations.len(), 1);
        let FileOperation::Create {
            path,
            text,
            overwrite,
        } = &resolved.operations[0]
        else {
            panic!()
        };
        assert_eq!(path, "/project/.openidea/helpers/App_Models_Post.php");
        assert!(!overwrite);
        assert!(text.contains("@property string $title"));
        assert!(h.process_runs().is_empty());
    }
    #[test]
    fn fields_return_migration_and_literal_fillable_edit_in_one_action() {
        let (h, index, doc) = setup();
        let action = actions(&index, &doc)
            .into_iter()
            .find(|a| a.data.as_deref() == Some("model-native:fields:1"))
            .unwrap();
        h.answer_next_prompt(Some(&[
            r#"[{"name":"published","kind":"boolean","default":false}]"#,
        ]));
        h.answer_next_prompt(Some(&[
            "database/migrations/2026_11_10_120000_add_fields_to_posts.php",
        ]));
        h.answer_next_prompt(Some(&["fillable"]));
        h.answer_next_prompt(Some(&["yes"]));
        let resolved = resolve(&index, &doc, action).unwrap();
        assert_eq!(resolved.operations.len(), 1);
        assert_eq!(resolved.edits.len(), 1);
        assert!(
            resolved.edits[0].1[0]
                .text
                .contains("['title', 'published']")
        );
        assert!(h.process_runs().is_empty());
    }
    #[test]
    fn helper_exclusions_keep_existing_model_fields_workflow() {
        let (h, _, doc) = setup();
        h.add_file(
            "ide.json",
            r#"{"helperCode":{"eloquent":{"ignoreNamespaces":["App\\Models"]}}}"#,
        );
        let index = Index::build(&host::projects().unwrap().remove(0));
        let available = actions(&index, &doc);
        assert_eq!(available.len(), 1);
        assert_eq!(available[0].data.as_deref(), Some("model-native:fields:1"));
    }
    #[test]
    fn cancelled_generation_is_not_a_partial_edit() {
        let (h, index, doc) = setup();
        let action = actions(&index, &doc).remove(0);
        h.answer_next_prompt(None);
        assert_eq!(resolve(&index, &doc, action), Err(Error::Cancelled));
        assert!(h.process_runs().is_empty());
    }
}

fn replace_all_docs(source: &str, tags: &str) -> Result<String, Error> {
    let (class, _) = eloquent_members::class_layout(source).ok_or(Error::Stale)?;
    let prefix = source[..class].trim_end();
    let block = format!("/**\n{tags} */");
    let mut output = source.to_owned();
    if prefix.ends_with("*/") {
        let begin = prefix.rfind("/**").ok_or(Error::Stale)?;
        if prefix[begin + 3..prefix.len() - 2].contains("*/") {
            return Err(Error::Stale);
        }
        output.replace_range(begin..prefix.len(), &block);
    } else {
        output.insert_str(class, &format!("{block}\n"));
    }
    Ok(output)
}
fn proxy_output(project:&openidea_plugin::Project,full:&str)->Result<(String,String),Error>{
    let path=format!("{}/.openidea/helpers/proxies/{}.php",project.root()?,crate::artisan_metadata::hash(full));
    let text=format!("<?php\n// Generated by OpenIDEA Laravel. Do not edit this helper.\n// Collection proxies for {full}\n{}",helper_codegen::proxy_code(full));
    Ok((path,text))
}
fn plan_proxies(project:&openidea_plugin::Project,full:&str,enabled:bool,operations:&mut Vec<FileOperation>,edits:&mut Vec<(String,Vec<TextEdit>)>,expected:&mut Vec<(String,String)>)->Result<(),Error>{
    let(path,text)=proxy_output(project,full)?;check_target(project,&path)?;
    match project.read_file(&path){
        Ok(previous)=>{
            let marker=format!("<?php\n// Generated by OpenIDEA Laravel. Do not edit this helper.\n// Collection proxies for {full}\n");
            if !previous.starts_with(&marker){if enabled{return Err(Error::Invalid(crate::ui_text::text("native.model_actions.949ace9507", "Collection proxy destination is not owned by this model").into()));}else{return Ok(());}}
            expected.push((path.clone(),previous.clone()));
            if enabled{edits.push((path,vec![entire(&previous,text)]));}else{operations.push(FileOperation::Delete{path});}
        },Err(Error::NotFound(_))=>{if enabled{operations.push(FileOperation::Create{path,text,overwrite:false});}},Err(error)=>return Err(error),
    }
    Ok(())
}
fn helper_text(full: &str, tags: &str) -> Result<String, Error> {
    if !generator_plan::qualified(full) {
        return Err(Error::Stale);
    }
    let short = full.rsplit('\\').next().ok_or(Error::Stale)?;
    let namespace = full
        .rsplit_once('\\')
        .map(|(namespace, _)| format!("namespace {namespace};\n"))
        .unwrap_or_default();
    Ok(format!(
        "<?php\n// Generated by OpenIDEA Laravel. Do not edit this helper.\n{namespace}\n/**\n{tags} */\nclass {short} extends \\Illuminate\\Database\\Eloquent\\Model {{}}\n"
    ))
}

/// Selection and storage mode precede exact per-file reviews. No source changes
/// happen until the host applies the returned single workspace transaction.
fn resolve_batch(main:Option<&crate::Main>,index:&Index,doc: &Document, action: FileAction) -> Result<FileAction, Error> {
    let project = owner(doc)?;
    let root = project.root()?;
    if !crate::index::index_sources_current(&project,index){return Err(Error::Stale);}
    if index.partial {
        return Err(Error::Stale);
    }
    let models = eloquent_members::model_identities(&index.eloquent_models)
        .into_iter()
        .filter(|(full, path)| {
            helper_codegen::eligible(&index, full) && crate::owns_current_path(&project, path)
        })
        .collect::<Vec<_>>();
    let items = models
        .iter()
        .map(|(full, _)| openidea_plugin::PickItem::new(full, full))
        .collect::<Vec<_>>();
    let chosen = host::pick_many(
        crate::ui_text::text("native.model_actions.261b72d9f9", "Models to regenerate in one undoable edit (up to 32)"),
        &items,
    )?
    .ok_or(Error::Cancelled)?;
    let chosen = chosen.into_iter().collect::<BTreeSet<_>>();
    if chosen.is_empty()
        || chosen.len() > 32
        || chosen
            .iter()
            .any(|full| !models.iter().any(|(model, _)| model == full))
    {
        return Err(Error::Invalid(crate::ui_text::text("native.model_actions.e329fd87c6", "Select 1–32 indexed, owned models").into()));
    }
    let preferences=parameters::load(&project)?;
    let selected_mode = host::pick(
        crate::ui_text::text("native.model_actions.b4e0ee298b", "Generated helper storage (or use saved parameters)"),
        &[
            openidea_plugin::PickItem::new("saved",&crate::ui_text::format("root.formatted.9e88c6553c", "Use saved parameters ({storage})", &[("storage", &format!("{}", preferences.storage))])),
            openidea_plugin::PickItem::new("helpers", crate::ui_text::text("native.model_actions.1ae24650fe", "Only in plugin-owned helper files")),
            openidea_plugin::PickItem::new(
                "preserve",
                crate::ui_text::text("native.model_actions.ba35fbc48c", "In model PHPDoc, preserving handwritten documentation"),
            ),
            openidea_plugin::PickItem::new(
                "overwrite",
                crate::ui_text::text("native.model_actions.de50368565", "In model PHPDoc, replacing all attached PHPDoc"),
            ),
        ],
    )?
    .ok_or(Error::Cancelled)?;
    let mode=if selected_mode=="saved"{preferences.storage.clone()}else{selected_mode};
    if !matches!(mode.as_str(), "helpers" | "preserve" | "overwrite") {
        return Err(Error::Stale);
    }
    let flags = host::pick_many(
        crate::ui_text::text("native.model_actions.6f3fd7c111", "Additional annotations"),
        &[openidea_plugin::PickItem::new("saved",crate::ui_text::text("native.model_actions.25f6f6c1bd", "Use saved relationship count setting")),openidea_plugin::PickItem::new(
            "counts",
            crate::ui_text::text("native.model_actions.16bd7797b1", "Relationship count attributes (populated by withCount)"),
        )],
    )?
    .ok_or(Error::Cancelled)?;
    if flags.iter().any(|flag| !matches!(flag.as_str(),"counts"|"saved")) || flags.len()>1 {
        return Err(Error::Stale);
    }
    let options = helper_codegen::Options {
        relation_counts: flags.iter().any(|flag| flag == "counts")||flags.iter().any(|flag|flag=="saved")&&preferences.relation_counts,
        date_class:preferences.date_class.clone(),
        source:preferences.source.clone(),schema:None,collection_proxies:preferences.collection_proxies
    };
    let mut operations = Vec::new();
    let mut edits = Vec::new();
    let mut expected = Vec::new();
    let mut snapshots = Vec::new();
    let mut previews = Vec::new();
    let mut targets = BTreeSet::new();
    for (full, source_path) in models.iter().filter(|(full, _)| chosen.contains(full)) {
        let source = project.read_file(source_path)?;
        if source.len() > 128 * 1024
            || eloquent_members::declaration(&source).as_deref() != Some(full)
        {
            return Err(Error::Stale);
        }
        let mut model_options=generation_options(main,&index,&project,full,&preferences)?;
        model_options.relation_counts=options.relation_counts;
        let tags = helper_codegen::tags(&index, &project, full, model_options.clone())?;
        let (path, contents) = if mode == "helpers" {
            (
                format!("{root}/.openidea/helpers/{}.php", full.replace('\\', "_")),
                helper_text(full, &tags)?,
            )
        } else {
            (
                source_path.clone(),
                if mode == "overwrite" {
                    replace_all_docs(&source, &tags)?
                } else {
                    replace_docs(&source, &tags)?
                },
            )
        };
        if !targets.insert(path.clone()) {
            return Err(Error::Invalid(
                crate::ui_text::text("native.model_actions.d95beaf673", "Helper output paths collide for selected class names; use model PHPDoc mode")
                    .into(),
            ));
        }
        check_target(&project, &path)?;
        match project.read_file(&path) {
            Ok(previous) => {
                if mode == "helpers"
                    && (!previous.starts_with(
                        "<?php\n// Generated by OpenIDEA Laravel. Do not edit this helper.\n",
                    ) || eloquent_members::declaration(&previous).as_deref() != Some(full))
                {
                    return Err(Error::Invalid(crate::ui_text::format("root.formatted.eb996b8930", "Helper destination is not owned by this model: {path}", &[("path", &format!("{}", path))])));
                }
                expected.push((path.clone(), previous.clone()));
                edits.push((path.clone(), vec![entire(&previous, contents.clone())]));
            }
            Err(Error::NotFound(_)) if mode == "helpers" => {
                operations.push(FileOperation::Create {
                    path: path.clone(),
                    text: contents.clone(),
                    overwrite: false,
                })
            }
            Err(error) => return Err(error),
        }
        let extra_ops=operations.len();let extra_edits=edits.len();
        plan_proxies(&project,full,preferences.collection_proxies,&mut operations,&mut edits,&mut expected)?;
        let support=operations[extra_ops..].iter().map(|operation|match operation{FileOperation::Create{path,text,..}=>crate::ui_text::format("root.formatted.6578bb91f2", "\nCreate support: {path}\n{text}", &[("path", &format!("{}", path)), ("text", &format!("{}", text))]),FileOperation::Delete{path}=>crate::ui_text::format("root.formatted.6a14a3a0d7", "\nDelete support: {path}", &[("path", &format!("{}", path))]),_=>String::new()}).chain(edits[extra_edits..].iter().map(|(path,edits)|crate::ui_text::format("root.formatted.a98d09a8a7", "\nUpdate support: {path}\n{contents}", &[("path", &format!("{}", path)), ("contents", &format!("{}", edits.iter().map(|edit|edit.text.as_str()).collect::<String>()))]))).collect::<String>();
        let preview = crate::ui_text::format("root.formatted.bc017d1942", "Project: {root}\nModel: {full}\nStorage mode: {mode}\nTarget: {path}\n{warning}\n\nComplete resulting file:\n{contents}\n{support}\n\nInclude this file in the final atomic edit?", &[("root", &format!("{}", root)), ("full", &format!("{}", full)), ("mode", &format!("{}", mode)), ("path", &format!("{}", path)), ("warning", &format!("{}", if mode == "overwrite" {
                crate::ui_text::text("root.formatted.410c3b29cc", "All attached handwritten PHPDoc will be replaced.")
            } else {
                ""
            })), ("contents", &format!("{}", contents)), ("support", &format!("{}", support))]);
        if preview.len() > ui_kit::prompts::MAX_CONFIRM_BYTES {
            return Err(Error::Invalid(crate::ui_text::format("root.formatted.7c4f2fcd9b", "Complete file preview exceeds the host review limit: {path}; select a smaller model", &[("path", &format!("{}", path))])));
        }
        previews.push(preview);
        snapshots.push((full.clone(), source_path.clone(), source, tags, model_options));
    }
    for preview in &previews {
        if !ui_kit::prompts::confirm_exact(crate::ui_text::text("root.formatted.bc3deeb9fe", "Review model helper file?"), preview)? {
            return Err(Error::Cancelled);
        }
    }
    let post_note=if preferences.post_commands.is_empty(){String::new()}else{crate::ui_text::format("root.formatted.c33933a37b", "\nAfter successful application, run Artisan argv arrays: {argv}", &[("argv", &format!("{}", serde_json::to_string(&preferences.post_commands).map_err(|_|Error::Stale)?))])};
    let summary = crate::ui_text::format("root.formatted.b73d5608b7", "Apply {count} reviewed files in one undoable workspace edit?\nProject: {root}\nStorage: {mode}{post_note}\n{targets}", &[("count", &format!("{}", targets.len())), ("root", &format!("{}", root)), ("mode", &format!("{}", mode)), ("post_note", &format!("{}", post_note)), ("targets", &format!("{}", targets.iter().cloned().collect::<Vec<_>>().join("\n")))]);
    if !ui_kit::prompts::confirm_exact(crate::ui_text::text("native.model_actions.regenerateConfirm", "Regenerate model helpers?"), &summary)? {
        return Err(Error::Cancelled);
    }
    owner(doc)?;
    if parameters::load(&project)?!=preferences{return Err(Error::Stale);}
    if !crate::index::index_sources_current(&project,index){return Err(Error::Stale);}
    let fresh = index;
    if fresh.partial {
        return Err(Error::Stale);
    }
    for (full, path, source, tags, model_options) in snapshots {
        if !helper_codegen::eligible(&fresh, &full)
            || !crate::owns_current_path(&project, &path)
            || project.read_file(&path)? != source
            || {let mut fresh_options=generation_options(main,&fresh,&project,&full,&preferences)?;fresh_options.relation_counts=options.relation_counts;fresh_options!=model_options}
            || helper_codegen::tags(&fresh, &project, &full, model_options)? != tags
        {
            return Err(Error::Stale);
        }
    }
    for (path, source) in &expected {
        check_target(&project, path)?;
        if project.read_file(path)? != *source {
            return Err(Error::Stale);
        }
    }
    for operation in &operations {
        if let FileOperation::Create { path, .. } = operation {
            check_target(&project, path)?;
            if !matches!(project.stat(path), Err(Error::NotFound(_))) {
                return Err(Error::Stale);
            }
        }
    }
    Ok(FileAction {
        title: action.title,
        operations,
        edits,
        open: Some(doc.path.clone()),
        ..Default::default()
    })
}

#[cfg(test)]
mod batch_tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    fn setup() -> (MockHost, Index, Document) {
        let mock = MockHost::install();
        mock.add_file("composer.lock",r#"{"packages":[{"name":"laravel/framework","version":"v13.40.0"}]}"#);
        let source = "<?php namespace App\\Models;use Illuminate\\Database\\Eloquent\\Model;/** Handwritten Post documentation. */class Post extends Model {}";
        mock.add_file("app/Models/Post.php", source);
        mock.add_file("app/Models/User.php","<?php namespace App\\Models;use Illuminate\\Database\\Eloquent\\Model;/** Handwritten User documentation. */class User extends Model {}");
        mock.add_file("database/migrations/001.php","<?php Schema::create('posts',function($t){$t->string('title');});Schema::create('users',function($t){$t->string('email');});");
        let project = host::projects().unwrap().remove(0);
        let index = Index::build(&project);
        (
            mock,
            index,
            Document {
                path: "/project/app/Models/Post.php".into(),
                language: "php".into(),
                version: 1,
                text: source.into(),
            },
        )
    }
    fn lazy(index: &Index, doc: &Document) -> FileAction {
        actions(index, doc)
            .into_iter()
            .find(|a| a.data.as_deref() == Some("model-native:helpers-all:1"))
            .unwrap()
    }
    fn answers(mock: &MockHost, mode: &str) {
        mock.answer_next_prompt(Some(&["App\\Models\\Post", "App\\Models\\User"]));
        mock.answer_next_prompt(Some(&[mode]));
        mock.answer_next_prompt(Some(&[]));
    }
    fn export(label: &str, text: &str) {
        if let Ok(dir) = std::env::var("LARAVEL_HELPER_FIXTURES_DIR") {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                std::path::Path::new(&dir).join(format!("{label}.php")),
                text,
            )
            .unwrap();
        }
    }
    #[test]
    fn reviewed_batch_creates_helpers_and_both_doc_modes_as_one_transaction() {
        for mode in ["helpers", "preserve", "overwrite"] {
            let (mock, index, doc) = setup();
            answers(&mock, mode);
            for _ in 0..3 {
                mock.answer_next_prompt(Some(&["yes"]));
            }
            let result = resolve(&index, &doc, lazy(&index, &doc)).unwrap();
            assert_eq!(result.operations.len() + result.edits.len(), 2);
            if mode == "helpers" {
                for (i, op) in result.operations.iter().enumerate() {
                    let FileOperation::Create {
                        text, overwrite, ..
                    } = op
                    else {
                        panic!()
                    };
                    assert!(!overwrite);
                    export(&format!("helpers_{i}"), text);
                }
            } else {
                assert!(result.operations.is_empty());
                for (i, (_, edits)) in result.edits.iter().enumerate() {
                    let text = &edits[0].text;
                    assert_eq!(text.contains("Handwritten"), mode == "preserve");
                    assert!(text.contains("@property string"));
                    export(&format!("{mode}_{i}"), text);
                }
            }
            assert!(
                !host::projects()
                    .unwrap()
                    .remove(0)
                    .exists(".openidea/helpers/App_Models_Post.php")
                    .unwrap()
            );
            assert!(mock.process_runs().is_empty());
        }
    }
    #[test]
    fn cancellation_after_first_review_returns_no_partial_transaction() {
        let (mock, index, doc) = setup();
        answers(&mock, "helpers");
        mock.answer_next_prompt(Some(&["yes"]));
        mock.answer_next_prompt(None);
        assert_eq!(
            resolve(&index, &doc, lazy(&index, &doc)),
            Err(Error::Cancelled)
        );
        assert!(
            !host::projects()
                .unwrap()
                .remove(0)
                .exists(".openidea/helpers/App_Models_Post.php")
                .unwrap()
        );
        assert!(mock.process_runs().is_empty());
    }
    #[test]
    fn batch_refuses_existing_unowned_helper_or_another_models_marker() {
        for existing in [
            "<?php // user file",
            "<?php\n// Generated by OpenIDEA Laravel. Do not edit this helper.\nnamespace Other;class Model {}",
        ] {
            let (mock, index, doc) = setup();
            mock.add_file(".openidea/helpers/App_Models_Post.php", existing);
            answers(&mock, "helpers");
            assert!(resolve(&index, &doc, lazy(&index, &doc)).is_err());
        }
    }
    #[test]
    fn preserve_mode_does_not_add_conflicting_handwritten_property_or_method_annotations() {
        let source = "<?php\n/**\n * User notes.\n * @property int $title\n * @method static string active(bool $enabled)\n */\nclass Post extends Model {}";
        let tags = " * @property string $title\n * @property string $other\n * @method static object active()\n";
        let updated = replace_docs(source, tags).unwrap();
        assert_eq!(updated.matches("$title").count(), 1);
        assert_eq!(updated.matches("active(").count(), 1);
        assert!(updated.contains("@property int $title"));
        assert!(updated.contains("@property string $other"));
        assert_eq!(replace_docs(&updated, tags).unwrap(), updated);
    }
}
