//! Existing-model field editor: one migration plus source changes in a single
//! host transaction. Preview, preparation and application use the same planner.
use crate::{
    eloquent_members,
    generator_plan::{self, Field, Index, Output},
    index::Index as SourceIndex,
    php,
};
use openidea_plugin::{
    Error, Invocation, Position, Project, Range, TextEdit,
    file_actions::{FileAction, FileOperation},
    host,
    providers::Document,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
#[derive(Default)]
pub(crate) struct State {
    projects: BTreeMap<String, Session>,
    next: u64,
}
struct Session {
    context: Context,
    pending: Option<Pending>,
}
#[derive(Clone)]
struct Context {
    path: String,
    text: String,
    full: String,
    table: String,
    known: BTreeSet<String>,
    composer: String,
    default_path: String,
    migration_glob: String,
    migration_files: Vec<String>,
    facts: BTreeMap<String, String>,
    config_fingerprint: String,
}
struct Pending {
    key: String,
    draft: Draft,
    plan: Plan,
}
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Draft {
    fields: Vec<Field>,
    #[serde(default)]
    indexes: Vec<Index>,
    migration_path: String,
    mass_assignment: String,
}
#[derive(Clone, PartialEq, Eq)]
struct Plan {
    migration: Output,
    source: String,
}
fn owned(root: &str, path: &str) -> bool {
    host::project_of(path)
        .and_then(|p| p.root().ok())
        .as_deref()
        == Some(root)
}
fn schema_files(project: &Project, glob: &str) -> Result<Vec<String>, Error> {
    let mut files = Vec::new();
    for (pattern, limit) in [
        (glob, 513),
        ("**/migrations/**/*.php", 513),
        ("**/ide.json", 129),
    ] {
        let found = project.find_files(pattern, limit)?;
        if found.len() >= limit as usize {
            return Err(Error::Invalid(
                "Schema metadata inventory exceeds exact-snapshot limits".into(),
            ));
        }
        files.extend(found);
    }
    files.sort();
    files.dedup();
    Ok(files)
}
fn owner(ctx: &Invocation) -> Result<(Project, String), Error> {
    let p = ctx.project().ok_or(Error::Stale)?;
    let root = p.root()?;
    if !host::projects()?
        .iter()
        .any(|p| p.root().ok().as_deref() == Some(&root))
    {
        return Err(Error::Stale);
    }
    Ok((p, root))
}
fn stamp() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let days = (seconds / 86400) as i64;
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    format!(
        "{year:04}_{month:02}_{day:02}_{:02}{:02}{:02}",
        (seconds % 86400) / 3600,
        (seconds % 3600) / 60,
        seconds % 60
    )
}
fn array(source: &str, property: &str) -> Result<Option<std::ops::Range<usize>>, String> {
    let (_, body) = eloquent_members::class_layout(source).ok_or("Unsupported class layout")?;
    if source.contains("<<<") || source.contains('`') {
        return Err("Heredoc and shell-string contexts are withheld".into());
    }
    let masked = php::mask_comments(source);
    let bytes = masked.as_bytes();
    let needle = format!("${property}");
    let mut at = body;
    let mut depth = 1usize;
    let mut statement = body;
    let mut found = None;
    while at < bytes.len() && depth > 0 {
        if matches!(bytes[at], b'\'' | b'"') {
            let quote = bytes[at];
            at += 1;
            while at < bytes.len() {
                if bytes[at] == b'\\' {
                    at = (at + 2).min(bytes.len());
                } else if bytes[at] == quote {
                    at += 1;
                    break;
                } else {
                    at += 1;
                }
            }
            continue;
        }
        match bytes[at] {
            b'{' => {
                depth += 1;
                at += 1;
                continue;
            }
            b'}' => {
                depth -= 1;
                at += 1;
                if depth == 1 {
                    statement = at;
                }
                continue;
            }
            b';' if depth == 1 => {
                statement = at + 1;
                at += 1;
                continue;
            }
            _ => {}
        }
        if depth == 1
            && bytes[at..].starts_with(needle.as_bytes())
            && bytes
                .get(at + needle.len())
                .is_none_or(|b| !b.is_ascii_alphanumeric() && *b != b'_')
        {
            let prefix = masked[statement..at].trim();
            if prefix.split_whitespace().any(|part| {
                ![
                    "public",
                    "protected",
                    "private",
                    "static",
                    "readonly",
                    "var",
                    "array",
                    "?array",
                ]
                .contains(&part)
            }) {
                return Err(format!("Unsupported ${property} declaration"));
            }
            let mut equal = at + needle.len();
            while bytes.get(equal).is_some_and(u8::is_ascii_whitespace) {
                equal += 1;
            }
            if bytes.get(equal) != Some(&b'=') {
                return Err(format!("${property} needs a literal initializer"));
            }
            let mut start = equal + 1;
            while bytes.get(start).is_some_and(u8::is_ascii_whitespace) {
                start += 1;
            }
            if bytes.get(start) != Some(&b'[') {
                return Err(format!("${property} must be a literal array"));
            }
            let close = php::closing(&masked, start).ok_or("Unclosed property array")?;
            if !masked[close + 1..].trim_start().starts_with(';') || found.is_some() {
                return Err(format!("Unsupported or duplicate ${property} property"));
            }
            found = Some(start..close + 1);
            at = close + 1;
            continue;
        }
        at += 1;
    }
    Ok(found)
}

fn append(source: &str, range: std::ops::Range<usize>, items: &[String]) -> String {
    if items.is_empty() {
        return source.to_owned();
    }
    let content = source[range.start + 1..range.end - 1].trim();
    let separator = if content.is_empty() || content.ends_with(',') {
        ""
    } else {
        ","
    };
    let mut out = source.to_owned();
    out.insert_str(range.end - 1, &format!("{separator} {}", items.join(", ")));
    out
}
fn casts_method(source: &str) -> Result<Option<std::ops::Range<usize>>, String> {
    let (_, body) = eloquent_members::class_layout(source).ok_or("Unsupported class layout")?;
    let mask = php::mask_comments(source);
    let b = mask.as_bytes();
    let mut at = body;
    let mut depth = 1usize;
    let mut found = None;
    while at < b.len() && depth > 0 {
        if matches!(b[at], b'\'' | b'"') {
            let quote = b[at];
            at += 1;
            while at < b.len() {
                if b[at] == b'\\' {
                    at = (at + 2).min(b.len());
                } else if b[at] == quote {
                    at += 1;
                    break;
                } else {
                    at += 1;
                }
            }
            continue;
        }
        if depth == 1
            && b[at..].starts_with(b"function")
            && b.get(at + 8).is_some_and(u8::is_ascii_whitespace)
        {
            let mut name = at + 8;
            while b.get(name).is_some_and(u8::is_ascii_whitespace) {
                name += 1;
            }
            let mut end = name;
            while b
                .get(end)
                .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
            {
                end += 1;
            }
            if mask[name..end].eq_ignore_ascii_case("casts") {
                if found.is_some() {
                    return Err("Duplicate casts() declarations".into());
                }
                let mut open = end;
                while b.get(open).is_some_and(u8::is_ascii_whitespace) {
                    open += 1;
                }
                if b.get(open) != Some(&b'(') {
                    return Err("Unsupported casts() signature".into());
                }
                let close = php::closing(&mask, open).ok_or("Invalid casts() arguments")?;
                if !mask[open + 1..close].trim().is_empty() {
                    return Err("casts() cannot take arguments".into());
                }
                let method_body = mask[close + 1..]
                    .find('{')
                    .map(|i| close + 1 + i)
                    .ok_or("casts() body unavailable")?;
                let signature = mask[close + 1..method_body].trim();
                if !signature.is_empty() && signature != ": array" && signature != ":array" {
                    return Err("Unsupported casts() return signature".into());
                }
                let method_end = php::closing(&mask, method_body).ok_or("Unclosed casts() body")?;
                let content = mask[method_body + 1..method_end].trim();
                if !content.starts_with("return") {
                    return Err("casts() must return one literal array".into());
                }
                let return_at = method_body
                    + 1
                    + mask[method_body + 1..method_end]
                        .find("return")
                        .ok_or("Missing return")?;
                let mut array_start = return_at + 6;
                while b.get(array_start).is_some_and(u8::is_ascii_whitespace) {
                    array_start += 1;
                }
                if b.get(array_start) != Some(&b'[') {
                    return Err("casts() must return one literal array".into());
                }
                let array_end =
                    php::closing(&mask, array_start).ok_or("Invalid casts() return array")?;
                if mask[array_end + 1..method_end].trim() != ";" {
                    return Err("casts() contains additional behavior".into());
                }
                found = Some(array_start..array_end + 1);
                at = method_end + 1;
                continue;
            }
        }
        match b[at] {
            b'{' => depth += 1,
            b'}' => depth -= 1,
            _ => {}
        }
        at += 1;
    }
    Ok(found)
}
fn update(context: &Context, draft: &Draft) -> Result<String, String> {
    let mut source = context.text.clone();
    if draft.mass_assignment == "keep" && !draft.fields.iter().any(|field| field.cast.is_some()) {
        return Ok(source);
    }
    let masked = php::mask_comments(&source);
    if draft.mass_assignment != "keep"
        && (masked.contains("function getFillable")
            || masked.contains("function getGuarded")
            || masked.contains("#[Fillable")
            || masked.contains("#[Guarded"))
    {
        return Err("Dynamic or attribute-based mass assignment needs manual review".into());
    }
    let (_, body) = eloquent_members::class_layout(&source)
        .ok_or("Model must have a supported single class declaration")?;
    match draft.mass_assignment.as_str() {
        "fillable" => {
            if let Some(range) = array(&source, "fillable")? {
                let mut known = BTreeSet::new();
                for item in php::items(&source, range.start) {
                    if item.key.is_some() {
                        return Err("$fillable cannot contain keyed elements".into());
                    }
                    known.insert(
                        php::literal(&source, item.value)
                            .ok_or("$fillable must contain string literals")?
                            .0
                            .to_owned(),
                    );
                }
                source = append(
                    &source,
                    range,
                    &draft
                        .fields
                        .iter()
                        .filter(|f| !known.contains(&f.name))
                        .map(|f| format!("'{}'", f.name))
                        .collect::<Vec<_>>(),
                );
            } else {
                source.insert_str(
                    body,
                    &format!(
                        "\n    protected $fillable = [{}];\n",
                        draft
                            .fields
                            .iter()
                            .map(|f| format!("'{}'", f.name))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                );
            }
        }
        "guarded" => {
            if let Some(range) = array(&source, "guarded")? {
                let mut keep = Vec::new();
                for item in php::items(&source, range.start) {
                    if item.key.is_some() {
                        return Err("$guarded cannot contain keyed elements".into());
                    }
                    let name = php::literal(&source, item.value)
                        .ok_or("$guarded must contain string literals")?
                        .0;
                    if name == "*" {
                        return Err("Wildcard guarding cannot allow selected fields; choose fillable or keep".into());
                    }
                    if !draft.fields.iter().any(|f| f.name == name) {
                        keep.push(format!(
                            "'{}'",
                            name.replace('\\', "\\\\").replace('\'', "\\'")
                        ));
                    }
                }
                source.replace_range(range, &format!("[{}]", keep.join(", ")));
            } else {
                return Err(
                    "No explicit literal $guarded array exists; choose fillable or keep".into(),
                );
            }
        }
        "keep" => {}
        _ => return Err("Choose fillable, guarded or keep".into()),
    }
    let cast_fields = draft
        .fields
        .iter()
        .filter(|f| f.cast.is_some())
        .collect::<Vec<_>>();
    if !cast_fields.is_empty() {
        let method = casts_method(&source)?;
        let items = cast_fields
            .iter()
            .map(|f| {
                Ok(format!(
                    "'{}' => {}",
                    f.name,
                    generator_plan::cast(f)?.ok_or("Cast unavailable")?
                ))
            })
            .collect::<Result<Vec<_>, String>>()?;
        if let Some(range) = method.or(array(&source, "casts")?) {
            let mut known = BTreeSet::new();
            for item in php::items(&source, range.start) {
                let key = item
                    .key
                    .and_then(|key| php::literal(&source, key))
                    .ok_or("$casts requires literal keys")?
                    .0;
                known.insert(key.to_owned());
            }
            if cast_fields.iter().any(|f| known.contains(&f.name)) {
                return Err("A cast already exists for one of the new fields".into());
            }
            source = append(&source, range, &items);
        } else {
            let (_, body) =
                eloquent_members::class_layout(&source).ok_or("Model layout changed")?;
            source.insert_str(
                body,
                &format!("\n    protected $casts = [{}];\n", items.join(", ")),
            );
        }
    }
    Ok(source)
}
fn plan(context: &Context, draft: &Draft) -> Result<Plan, String> {
    if draft.fields.is_empty() || draft.fields.len() > 64 || draft.indexes.len() > 64 {
        return Err("Choose 1–64 fields and at most 64 indexes".into());
    }
    if !generator_plan::relative(&draft.migration_path) || !draft.migration_path.ends_with(".php") {
        return Err("Migration path must be a project-relative PHP file".into());
    }
    if !context.table.split('.').all(generator_plan::identifier)
        || context.table.split('.').count() > 2
    {
        return Err("Unsupported table identifier".into());
    }
    let mut names = context.known.clone();
    let mut lines = String::new();
    for field in &draft.fields {
        if !generator_plan::identifier(&field.name) || !names.insert(field.name.clone()) {
            return Err("Fields must be distinct new column identifiers".into());
        }
        lines.push_str(&generator_plan::field_line(field)?);
    }
    let mut indexes = BTreeSet::new();
    for index in &draft.indexes {
        if !generator_plan::identifier(&index.name)
            || !indexes.insert(&index.name)
            || index.columns.is_empty()
            || index.columns.iter().any(|name| !names.contains(name))
            || index.columns.iter().collect::<BTreeSet<_>>().len() != index.columns.len()
        {
            return Err("Invalid index name or columns".into());
        }
        lines.push_str(&format!(
            "            $table->{}([{}], '{}');\n",
            if index.unique { "unique" } else { "index" },
            index
                .columns
                .iter()
                .map(|name| format!("'{name}'"))
                .collect::<Vec<_>>()
                .join(", "),
            index.name
        ));
    }
    let mut down = String::new();
    for index in draft.indexes.iter().rev() {
        down.push_str(&format!(
            "            $table->{}('{}');\n",
            if index.unique {
                "dropUnique"
            } else {
                "dropIndex"
            },
            index.name
        ));
    }
    down.push_str(&format!(
        "            $table->dropColumn([{}]);\n",
        draft
            .fields
            .iter()
            .map(|f| format!("'{}'", f.name))
            .collect::<Vec<_>>()
            .join(", ")
    ));
    let migration = format!(
        "<?php\n\nuse Illuminate\\Database\\Migrations\\Migration;\nuse Illuminate\\Database\\Schema\\Blueprint;\nuse Illuminate\\Support\\Facades\\Schema;\n\nreturn new class extends Migration\n{{\n    public function up(): void\n    {{\n        Schema::table('{}', function (Blueprint $table): void {{\n{lines}        }});\n    }}\n\n    public function down(): void\n    {{\n        Schema::table('{}', function (Blueprint $table): void {{\n{down}        }});\n    }}\n}};\n",
        context.table, context.table
    );
    Ok(Plan {
        migration: Output {
            path: draft.migration_path.clone(),
            text: migration,
        },
        source: update(context, draft)?,
    })
}
fn validate(project: &Project, root: &str, context: &Context, plan: &Plan) -> Result<(), Error> {
    if !owned(root, &context.path)
        || project.read_file(&context.path)? != context.text
        || project.read_file("composer.json")? != context.composer
    {
        return Err(Error::Stale);
    }
    if schema_files(project, &context.migration_glob)? != context.migration_files
        || crate::generator_presets::load(project)?.fingerprint() != context.config_fingerprint
    {
        return Err(Error::Stale);
    }
    for (path, text) in &context.facts {
        if !owned(root, path) || project.read_file(path)? != *text {
            return Err(Error::Stale);
        }
    }
    let path = format!("{root}/{}", plan.migration.path);
    if !owned(root, &path) {
        return Err(Error::Stale);
    }
    match project.stat(&path) {
        Err(Error::NotFound(_)) => {}
        Ok(_) => return Err(Error::Invalid("Migration target already exists".into())),
        Err(e) => return Err(e),
    }
    let mut parts = plan.migration.path.split('/').collect::<Vec<_>>();
    parts.pop();
    while !parts.is_empty() {
        let path = parts.join("/");
        match project.stat(&path) {
            Ok(s) if s.kind == openidea_plugin::FileKind::Directory => {}
            Ok(_) => {
                return Err(Error::Invalid(
                    "Migration parent must be an ordinary directory".into(),
                ));
            }
            Err(Error::NotFound(_)) => {}
            Err(e) => return Err(e),
        }
        parts.pop();
    }
    Ok(())
}
fn preview(context: &Context, plan: &Plan) -> Value {
    let mut files = vec![json!({"path":plan.migration.path,"text":plan.migration.text})];
    if context.text != plan.source {
        files.push(json!({"path":context.path,"before":context.text,"after":plan.source}));
    }
    json!({"files":files,"canPrepare":true})
}
impl State {
    pub(crate) fn seed(
        &mut self,
        ctx: &Invocation,
        index: &SourceIndex,
        path: &str,
    ) -> Result<(), Error> {
        let (project, root) = owner(ctx)?;
        if !self.projects.contains_key(&root) && self.projects.len() >= 8 {
            return Err(Error::Invalid(
                crate::ui_text::text(
                    "modelUi.existing_model_fields.3a866a15c4",
                    "Too many existing-model drafts; close a draft before opening another project",
                )
                .into(),
            ));
        }
        if index.partial || !owned(&root, path) {
            return Err(Error::Stale);
        }
        let (full, _, table) = eloquent_members::model_inventory(&index.eloquent_models)
            .into_iter()
            .find(|(_, source, _)| source == path)
            .ok_or_else(|| {
                Error::Invalid(
                    crate::ui_text::text(
                        "modelUi.existing_model_fields.ad5b078e38",
                        "Choose an indexed model source",
                    )
                    .into(),
                )
            })?;
        let text = project.read_file(path)?;
        if text.len() > 128 << 10 {
            return Err(Error::Invalid(
                crate::ui_text::text(
                    "modelUi.existing_model_fields.72c4a9872d",
                    "Model source is too large",
                )
                .into(),
            ));
        }
        eloquent_members::class_layout(&text).ok_or_else(|| {
            Error::Invalid(
                crate::ui_text::text(
                    "modelUi.existing_model_fields.eb5b4ec9b1",
                    "Unsupported model class layout",
                )
                .into(),
            )
        })?;
        if eloquent_members::declaration(&text).as_deref() != Some(&full) {
            return Err(Error::Stale);
        }
        let composer = project.read_file("composer.json")?;
        let config = crate::generator_presets::load(&project)?;
        let migration_root = config
            .migration_root_for_source(path.strip_prefix(&format!("{root}/")).ok_or(Error::Stale)?);
        let migration_glob = format!("{migration_root}/**/*.php");
        let migration_files = schema_files(&project, &migration_glob)?;
        let mut fact_paths = migration_files.iter().cloned().collect::<BTreeSet<_>>();
        for entry in index
            .columns
            .get(&table)
            .into_iter()
            .flatten()
            .map(|(_, entry)| entry)
        {
            fact_paths.insert(entry.path.clone());
        }
        let mut facts = BTreeMap::new();
        let mut total = 0usize;
        for path in fact_paths {
            if !owned(&root, &path) {
                return Err(Error::Stale);
            }
            let text = project.read_file(&path)?;
            total += text.len();
            if text.len() > 128 << 10 || total > 4 << 20 {
                return Err(Error::Invalid(
                    crate::ui_text::text(
                        "modelUi.existing_model_fields.a11321afc9",
                        "Schema source snapshot is too large",
                    )
                    .into(),
                ));
            }
            facts.insert(path, text);
        }
        let known = index
            .columns
            .get(&table)
            .map(|cols| cols.keys().cloned().collect())
            .unwrap_or_default();
        let default_path = format!(
            "{migration_root}/{}_add_fields_to_{}.php",
            stamp(),
            table.replace('.', "_")
        );
        self.projects.insert(
            root,
            Session {
                context: Context {
                    path: path.into(),
                    text,
                    full,
                    table,
                    known,
                    composer,
                    default_path,
                    migration_glob,
                    migration_files,
                    facts,
                    config_fingerprint: config.fingerprint(),
                },
                pending: None,
            },
        );
        Ok(())
    }
    pub(crate) fn state(&self, ctx: &Invocation) -> Result<Option<Value>, Error> {
        let (_, root) = owner(ctx)?;
        Ok(self.projects.get(&root).map(|session|json!({"model":session.context.full,"path":session.context.path,"table":session.context.table,"knownFields":session.context.known,"migrationPath":session.context.default_path})))
    }
    pub(crate) fn discard(&mut self, ctx: &Invocation) -> Result<(), Error> {
        let (_, root) = owner(ctx)?;
        if let Some(session) = self.projects.get_mut(&root) {
            session.pending = None;
        }
        Ok(())
    }
    pub(crate) fn close(&mut self, ctx: &Invocation) -> Result<(), Error> {
        let (_, root) = owner(ctx)?;
        self.projects.remove(&root);
        Ok(())
    }
    pub(crate) fn handle(
        &mut self,
        ctx: &Invocation,
        draft: Draft,
        prepare: bool,
    ) -> Result<Value, Error> {
        let (project, root) = owner(ctx)?;
        let session = self.projects.get_mut(&root).ok_or_else(|| {
            Error::Invalid(
                crate::ui_text::text(
                    "modelUi.existing_model_fields.4bf100fbe7",
                    "Select an existing model first",
                )
                .into(),
            )
        })?;
        session.pending = None;
        let plan = plan(&session.context, &draft).map_err(ui_error)?;
        validate(&project, &root, &session.context, &plan)?;
        if !prepare {
            return Ok(preview(&session.context, &plan));
        }
        if host::open_editors()?.iter().any(|editor| {
            editor.dirty
                && (editor.path == session.context.path
                    || editor.path == format!("{root}/{}", plan.migration.path))
        }) {
            return Err(Error::Invalid(
                crate::ui_text::text(
                    "modelUi.existing_model_fields.704c2b24f8",
                    "Save the model and close unsaved migration targets first",
                )
                .into(),
            ));
        }
        host::open_editor(&session.context.path, None)?;
        self.next = self.next.checked_add(1).ok_or(Error::Stale)?;
        session.pending = Some(Pending {
            key: format!("existing-model-fields:{}", self.next),
            draft,
            plan,
        });
        Ok(
            json!({"prepared":true,"instruction":"In the model editor, choose Apply prepared model fields…; one undo reverses the migration and source update."}),
        )
    }
    pub(crate) fn prepared(&self, root: &str) -> Option<(String, String)> {
        let session = self.projects.get(root)?;
        session.pending.as_ref().map(|pending| (pending.key.clone(), session.context.path.clone()))
    }
    pub(crate) fn actions(&self, doc: &Document) -> Vec<FileAction> {
        self.projects
            .iter()
            .find_map(|(root, session)| {
                session
                    .pending
                    .as_ref()
                    .filter(|_| {
                        session.context.path == doc.path
                            && session.context.text == doc.text
                            && owned(root, &doc.path)
                    })
                    .map(|p| FileAction {
                        title: crate::ui_text::text(
                            "modelUi.existing_model_fields.8276ac70c0",
                            "Apply prepared model fields…",
                        )
                        .into(),
                        kind: Some("refactor".into()),
                        data: Some(p.key.clone()),
                        ..Default::default()
                    })
            })
            .into_iter()
            .collect()
    }
    pub(crate) fn resolve(
        &mut self,
        doc: &Document,
        action: FileAction,
    ) -> Result<FileAction, Error> {
        let project = host::project_of(&doc.path).ok_or(Error::Stale)?;
        let root = project.root()?;
        let session = self.projects.get_mut(&root).ok_or(Error::Stale)?;
        let pending = session.pending.as_ref().ok_or(Error::Stale)?;
        if action.data.as_deref() != Some(&pending.key)
            || session.context.path != doc.path
            || session.context.text != doc.text
            || plan(&session.context, &pending.draft).map_err(ui_error)? != pending.plan
        {
            return Err(Error::Stale);
        }
        validate(&project, &root, &session.context, &pending.plan)?;
        let exact = crate::ui_text::format(
            "modelUi.fieldsReview",
            "Create {path}\n{migration}\n\nModel before:\n{before}\n\nModel after:\n{after}",
            &[
                ("path", &pending.plan.migration.path),
                ("migration", &pending.plan.migration.text),
                ("before", &session.context.text),
                ("after", &pending.plan.source),
            ],
        );
        if !ui_kit::prompts::confirm_exact(
            crate::ui_text::text("modelUi.fieldsConfirm", "Apply model fields and migration?"),
            &exact,
        )? {
            return Err(Error::Cancelled);
        }
        validate(&project, &root, &session.context, &pending.plan)?;
        let pending = session.pending.take().ok_or(Error::Stale)?;
        let edits = if pending.plan.source != session.context.text {
            vec![(
                doc.path.clone(),
                vec![TextEdit {
                    range: Range::new(
                        Position::new(0, 0),
                        openidea_plugin::text::position(&doc.text, doc.text.len()),
                    ),
                    text: pending.plan.source,
                }],
            )]
        } else {
            Vec::new()
        };
        let path = format!("{root}/{}", pending.plan.migration.path);
        Ok(FileAction {
            title: crate::ui_text::text(
                "modelUi.existing_model_fields.aed30edaf7",
                "Create model fields and migration",
            )
            .into(),
            operations: vec![FileOperation::Create {
                path: path.clone(),
                text: pending.plan.migration.text,
                overwrite: false,
            }],
            edits,
            open: Some(path),
            ..Default::default()
        })
    }
}

/// Localize only authored literal planner errors at the UI delivery boundary.
fn ui_error(message: String) -> Error {
    let localized = match message.as_str() {
        "Schema metadata inventory exceeds exact-snapshot limits" => {
            crate::ui_text::t("modelUi.existing_model_fields.ec8f842415", &message)
        }
        "Unsupported class layout" => {
            crate::ui_text::t("modelUi.existing_model_fields.ba5ff7d269", &message)
        }
        "Heredoc and shell-string contexts are withheld" => {
            crate::ui_text::t("modelUi.existing_model_fields.cdb77eab82", &message)
        }
        "Unclosed property array" => {
            crate::ui_text::t("modelUi.existing_model_fields.2f4c30b0d6", &message)
        }
        "Duplicate casts() declarations" => {
            crate::ui_text::t("modelUi.existing_model_fields.e9a2574550", &message)
        }
        "Unsupported casts() signature" => {
            crate::ui_text::t("modelUi.existing_model_fields.091838750a", &message)
        }
        "Invalid casts() arguments" => {
            crate::ui_text::t("modelUi.existing_model_fields.9132bb51e4", &message)
        }
        "casts() cannot take arguments" => {
            crate::ui_text::t("modelUi.existing_model_fields.2d059d1cb1", &message)
        }
        "casts() body unavailable" => {
            crate::ui_text::t("modelUi.existing_model_fields.0e3a937937", &message)
        }
        "Unsupported casts() return signature" => {
            crate::ui_text::t("modelUi.existing_model_fields.afdac4b4e2", &message)
        }
        "Unclosed casts() body" => {
            crate::ui_text::t("modelUi.existing_model_fields.7aa6097e3a", &message)
        }
        "casts() must return one literal array" => {
            crate::ui_text::t("modelUi.existing_model_fields.13deacc6ff", &message)
        }
        "Missing return" => crate::ui_text::t("modelUi.existing_model_fields.31c5ba852d", &message),
        "Invalid casts() return array" => {
            crate::ui_text::t("modelUi.existing_model_fields.aa9147b145", &message)
        }
        "casts() contains additional behavior" => {
            crate::ui_text::t("modelUi.existing_model_fields.f261a86a3a", &message)
        }
        "Dynamic or attribute-based mass assignment needs manual review" => {
            crate::ui_text::t("modelUi.existing_model_fields.7e37359009", &message)
        }
        "Model must have a supported single class declaration" => {
            crate::ui_text::t("modelUi.existing_model_fields.4bfc1a86e4", &message)
        }
        "$fillable cannot contain keyed elements" => {
            crate::ui_text::t("modelUi.existing_model_fields.7f0631744f", &message)
        }
        "$fillable must contain string literals" => {
            crate::ui_text::t("modelUi.existing_model_fields.2bc60ac378", &message)
        }
        "$guarded cannot contain keyed elements" => {
            crate::ui_text::t("modelUi.existing_model_fields.46a6e1b361", &message)
        }
        "$guarded must contain string literals" => {
            crate::ui_text::t("modelUi.existing_model_fields.a7f249c917", &message)
        }
        "Wildcard guarding cannot allow selected fields; choose fillable or keep" => {
            crate::ui_text::t("modelUi.existing_model_fields.cea948d57c", &message)
        }
        "No explicit literal $guarded array exists; choose fillable or keep" => {
            crate::ui_text::t("modelUi.existing_model_fields.0bd2245161", &message)
        }
        "Choose fillable, guarded or keep" => {
            crate::ui_text::t("modelUi.existing_model_fields.d53fe24753", &message)
        }
        "Cast unavailable" => {
            crate::ui_text::t("modelUi.existing_model_fields.51b5436181", &message)
        }
        "$casts requires literal keys" => {
            crate::ui_text::t("modelUi.existing_model_fields.3414f0b34a", &message)
        }
        "A cast already exists for one of the new fields" => {
            crate::ui_text::t("modelUi.existing_model_fields.e19e44e0f4", &message)
        }
        "Model layout changed" => {
            crate::ui_text::t("modelUi.existing_model_fields.48673a7be4", &message)
        }
        "Choose 1–64 fields and at most 64 indexes" => {
            crate::ui_text::t("modelUi.existing_model_fields.533e2c2535", &message)
        }
        "Migration path must be a project-relative PHP file" => {
            crate::ui_text::t("modelUi.existing_model_fields.5e3282e166", &message)
        }
        "Unsupported table identifier" => {
            crate::ui_text::t("modelUi.existing_model_fields.ddbcdb0216", &message)
        }
        "Fields must be distinct new column identifiers" => {
            crate::ui_text::t("modelUi.existing_model_fields.04ba99f939", &message)
        }
        "Invalid index name or columns" => {
            crate::ui_text::t("modelUi.existing_model_fields.5c58936c7d", &message)
        }
        "Migration target already exists" => {
            crate::ui_text::t("modelUi.existing_model_fields.e31df6bdff", &message)
        }
        "Migration parent must be an ordinary directory" => {
            crate::ui_text::t("modelUi.existing_model_fields.0c8bbd1f76", &message)
        }
        _ => crate::ui_text::owned_error(&message, UI_ERROR_TEMPLATES).unwrap_or(message),
    };
    Error::Invalid(localized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    fn setup(source: &str) -> (MockHost, State, Document) {
        let h = MockHost::install();
        h.add_file(
            "composer.json",
            r#"{"autoload":{"psr-4":{"App\\":"app/"}}}"#,
        );
        h.add_file("app/Models/Post.php", source);
        h.add_file(
            "database/migrations/001.php",
            "<?php Schema::create('posts',function($t){$t->string('title');});",
        );
        let p = h.invocation().project().unwrap();
        let index = SourceIndex::build(&p);
        let doc = Document {
            path: "/project/app/Models/Post.php".into(),
            language: "php".into(),
            text: source.into(),
            version: 1,
        };
        h.open_editor(&doc.path, "php", source, &[]);
        let mut state = State::default();
        state.seed(&h.invocation(), &index, &doc.path).unwrap();
        (h, state, doc)
    }
    const SOURCE: &str = "<?php namespace App\\Models; use Illuminate\\Database\\Eloquent\\Model; final class Post extends Model { protected $fillable = ['title']; protected function casts(): array { return ['title' => 'string']; } }";
    #[test]
    fn keep_mode_creates_only_migration_for_attribute_or_dynamic_mass_assignment() {
        for source in [
            "<?php namespace App\\Models; use Illuminate\\Database\\Eloquent\\Model; #[Fillable(['title'])] final class Post extends Model {}",
            "<?php namespace App\\Models; use Illuminate\\Database\\Eloquent\\Model; class Post extends Model { private const FIELDS = ['title']; protected $fillable = self::FIELDS; public function getFillable() { return self::FIELDS; } }",
        ] {
            let (h, mut state, doc) = setup(source);
            let mut d = draft();
            d.mass_assignment = "keep".into();
            d.fields[0].cast = None;
            let preview = state.handle(&h.invocation(), d.clone(), false).unwrap();
            assert_eq!(preview["files"].as_array().unwrap().len(), 1);
            state.handle(&h.invocation(), d, true).unwrap();
            let action = state.actions(&doc).remove(0);
            h.answer_next_prompt(Some(&["yes"]));
            let ready = state.resolve(&doc, action).unwrap();
            assert_eq!(ready.operations.len(), 1);
            assert!(ready.edits.is_empty());
            assert!(h.process_runs().is_empty());
        }
    }
    #[test]
    fn keep_mode_cast_updates_do_not_require_literal_fillable() {
        let source = "<?php namespace App\\Models; use Illuminate\\Database\\Eloquent\\Model; #[Fillable(['title'])] class Post extends Model { private const FIELDS = ['title']; protected $fillable = self::FIELDS; protected function casts(): array { return ['title' => 'string']; } }";
        let (h, mut state, _) = setup(source);
        let mut d = draft();
        d.mass_assignment = "keep".into();
        let preview = state.handle(&h.invocation(), d, false).unwrap();
        let model = preview["files"][1]["after"].as_str().unwrap();
        assert!(model.contains("protected $fillable = self::FIELDS;"));
        assert!(model.contains("#[Fillable(['title'])]"));
        assert!(model.contains("'amount' => 'decimal:4'"));
    }
    fn draft() -> Draft {
        serde_json::from_value(json!({"fields":[{"name":"amount","kind":"decimal","precision":12,"scale":4,"cast":"decimal:4"}],"indexes":[{"name":"posts_amount_unique","columns":["amount"],"unique":true}],"migrationPath":"database/migrations/2026_11_10_120000_add_fields_to_posts.php","massAssignment":"fillable"})).unwrap()
    }
    #[test]
    fn graphical_draft_returns_migration_fillable_and_method_cast_update_atomically() {
        let (h, mut state, doc) = setup(SOURCE);
        let preview = state.handle(&h.invocation(), draft(), false).unwrap();
        assert_eq!(preview["files"].as_array().unwrap().len(), 2);
        assert!(
            preview["files"][1]["after"]
                .as_str()
                .unwrap()
                .contains("'amount' => 'decimal:4'")
        );
        state.handle(&h.invocation(), draft(), true).unwrap();
        let action = state.actions(&doc).remove(0);
        h.answer_next_prompt(Some(&["yes"]));
        let ready = state.resolve(&doc, action).unwrap();
        assert_eq!(ready.operations.len(), 1);
        assert_eq!(ready.edits.len(), 1);
        assert!(ready.edits[0].1[0].text.contains("['title', 'amount']"));
        let FileOperation::Create {
            text, overwrite, ..
        } = &ready.operations[0]
        else {
            panic!()
        };
        assert!(!overwrite);
        assert!(text.contains("dropUnique('posts_amount_unique')"));
        assert!(text.contains("decimal('amount', 12, 4)"));
        assert!(h.process_runs().is_empty());
        assert!(
            h.invocation()
                .project()
                .unwrap()
                .read_file("database/migrations/2026_11_10_120000_add_fields_to_posts.php")
                .is_err()
        );
    }
    #[test]
    fn guarded_fields_are_removed_without_changing_other_guards() {
        let source = "<?php namespace App\\Models; use Illuminate\\Database\\Eloquent\\Model; class Post extends Model { protected $guarded = ['amount', 'secret']; }";
        let (h, mut state, _) = setup(source);
        let mut d = draft();
        d.mass_assignment = "guarded".into();
        let preview = state.handle(&h.invocation(), d, false).unwrap();
        assert!(
            preview["files"][1]["after"]
                .as_str()
                .unwrap()
                .contains("$guarded = ['secret']")
        );
    }
    #[test]
    fn lexical_decoys_do_not_become_property_edit_targets() {
        let source = "<?php namespace App\\Models; use Illuminate\\Database\\Eloquent\\Model; class Post extends Model { protected $note = '$fillable = [fake]'; /* $casts = ['fake']; */ protected $fillable = ['title']; }";
        let (h, mut state, _) = setup(source);
        let response = state.handle(&h.invocation(), draft(), false).unwrap();
        let after = response["files"][1]["after"].as_str().unwrap();
        assert!(after.contains("$note = '$fillable = [fake]'"));
        assert!(after.contains("$fillable = ['title', 'amount']"));
    }
    #[test]
    fn dynamic_methods_wildcard_guards_duplicate_columns_and_traversal_are_refused() {
        let (h, mut state, _) = setup(SOURCE);
        let mut d = draft();
        d.fields[0].name = "title".into();
        assert!(state.handle(&h.invocation(), d, false).is_err());
        let mut d = draft();
        d.migration_path = "../outside.php".into();
        assert!(state.handle(&h.invocation(), d, false).is_err());
        let source = "<?php namespace App\\Models; use Illuminate\\Database\\Eloquent\\Model; class Post extends Model { protected $guarded = ['*']; protected function casts(): array { return $this->custom(); } }";
        let (h, mut state, _) = setup(source);
        let mut d = draft();
        d.mass_assignment = "guarded".into();
        assert!(state.handle(&h.invocation(), d, false).is_err());
        let mut d = draft();
        d.mass_assignment = "keep".into();
        assert!(state.handle(&h.invocation(), d, false).is_err());
    }
    #[test]
    fn source_schema_inventory_and_config_changes_refuse_stale_preparations() {
        for scenario in ["source", "schema", "config"] {
            let (h, mut state, doc) = setup(SOURCE);
            state.handle(&h.invocation(), draft(), true).unwrap();
            let action = state.actions(&doc).remove(0);
            match scenario {
                "source" => h.add_file("app/Models/Post.php", &format!("{SOURCE} // changed")),
                "schema" => h.add_file("database/migrations/002.php", "<?php // new migration"),
                _ => h.add_file(
                    ".openidea/laravel-generators.json",
                    r#"{"version":1,"modules":[]}"#,
                ),
            }
            assert!(state.resolve(&doc, action).is_err());
        }
    }
    #[test]
    fn module_migration_root_and_cancellation_remain_project_bound() {
        let (h, mut state, doc) = setup(SOURCE);
        assert!(
            state.state(&h.invocation()).unwrap().unwrap()["migrationPath"]
                .as_str()
                .unwrap()
                .starts_with("database/migrations/")
        );
        state.handle(&h.invocation(), draft(), true).unwrap();
        let action = state.actions(&doc).remove(0);
        assert_eq!(state.resolve(&doc, action), Err(Error::Cancelled));
        state.discard(&h.invocation()).unwrap();
        assert!(state.actions(&doc).is_empty());
        h.add_project("/project/app");
        assert!(
            state
                .seed(&h.invocation(), &SourceIndex::default(), &doc.path)
                .is_err()
        );
    }
}

// Explicit formats emitted by this module; only UI delivery uses locale state.
const UI_ERROR_TEMPLATES: &[(&str, &str)] = &[
    (
        "final.existing_model_fields.unsupportedProperty",
        "Unsupported ${property} declaration",
    ),
    (
        "final.existing_model_fields.propertyInitializer",
        "${property} needs a literal initializer",
    ),
    (
        "final.existing_model_fields.propertyArray",
        "${property} must be a literal array",
    ),
    (
        "final.existing_model_fields.duplicateProperty",
        "Unsupported or duplicate ${property} property",
    ),
];
