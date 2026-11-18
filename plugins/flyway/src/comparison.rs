//! Offline review adapter for documented Flyway comparison JSON. Imported review stays
//! offline; the managed comparison runs only with an installed Flyway CLI profile.
mod managed;
use super::*;
use openidea_plugin::PickItem;
use openidea_plugin::views::{Field, FieldValue};
use serde_json::Value;
use std::hash::BuildHasher;
pub(crate) const VIEW: &str = "comparison-review";
const MAX_REPORT: usize = 1 << 20;
const MAX_SQL: usize = 64 << 10;
#[derive(Clone, Debug)]
struct Side {
    schema: String,
    name: String,
    sql: Option<String>,
}
#[derive(Clone, Debug)]
struct Change {
    id: String,
    kind: String,
    object: String,
    from: Option<Side>,
    to: Option<Side>,
}
fn label(v: &Value, key: &str, empty: bool) -> Result<String, Error> {
    let s = v
        .get(key)
        .and_then(Value::as_str)
        .ok_or("Comparison JSON has a missing/non-string label.")?;
    if (!empty && s.is_empty()) || s.len() > 512 || s.chars().any(char::is_control) {
        return Err("Comparison JSON label exceeds supported bounds.".into());
    }
    Ok(s.into())
}
fn side(v: &Value) -> Result<Option<Side>, Error> {
    if v.is_null() {
        return Ok(None);
    }
    if !v.is_object() {
        return Err("Comparison side must be an object or null.".into());
    }
    let sql = match v.get("definition") {
        Some(Value::String(s)) if s.len() <= MAX_SQL && !s.chars().any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t')) => {
            Some(s.clone())
        }
        Some(Value::Null) => None,
        _ => return Err("Unsupported/missing/oversized SQL definition; use bounded diffText JSON.".into()),
    };
    Ok(Some(Side {
        schema: label(v, "schema", true)?,
        name: label(v, "name", false)?,
        sql,
    }))
}
// serde_json::Value alone accepts duplicate object keys. Reject those rather
// than letting a report silently replace an earlier ID/SQL/differences value.
pub(crate) struct Unique(pub(crate) Value);
impl<'de> serde::Deserialize<'de> for Unique {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = Unique;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("JSON with unique object keys")
            }
            fn visit_map<M: serde::de::MapAccess<'de>>(self, mut m: M) -> Result<Unique, M::Error> {
                let mut out = serde_json::Map::new();
                while let Some((k, v)) = m.next_entry::<String, Unique>()? {
                    if out.insert(k, v.0).is_some() {
                        return Err(serde::de::Error::custom("duplicate JSON key"));
                    }
                }
                Ok(Unique(Value::Object(out)))
            }
            fn visit_seq<S: serde::de::SeqAccess<'de>>(self, mut s: S) -> Result<Unique, S::Error> {
                let mut out = Vec::new();
                while let Some(v) = s.next_element::<Unique>()? {
                    out.push(v.0);
                }
                Ok(Unique(Value::Array(out)))
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Unique, E> {
                Ok(Unique(Value::String(v.into())))
            }
            fn visit_string<E: serde::de::Error>(self, v: String) -> Result<Unique, E> {
                Ok(Unique(Value::String(v)))
            }
            fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Unique, E> {
                Ok(Unique(Value::Bool(v)))
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Unique, E> {
                Ok(Unique(Value::Null))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Unique, E> {
                Ok(Unique(Value::Number(v.into())))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Unique, E> {
                Ok(Unique(Value::Number(v.into())))
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Unique, E> {
                Ok(Unique(Value::Number(
                    serde_json::Number::from_f64(v).ok_or_else(|| E::custom("nonfinite number"))?,
                )))
            }
        }
        d.deserialize_any(Visitor)
    }
}
fn scoped_path(project: &Project, value: &str) -> Result<String, Error> {
    path(value)?;
    let root = project.root()?;
    let absolute = format!("{}/{value}", root.trim_end_matches('/'));
    if host::project_of(&absolute).and_then(|p| p.root().ok()).as_deref() != Some(root.as_str()) {
        return Err(Error::Stale);
    }
    Ok(absolute)
}
fn read(project: &Project, value: &str) -> Result<String, Error> {
    project.read_file(&scoped_path(project, value)?)
}
fn stat(project: &Project, value: &str) -> Result<openidea_plugin::FileStat, Error> {
    project.stat(&scoped_path(project, value)?)
}
pub(crate) fn refresh() {
    ui_kit::web::refresh_view(VIEW, include_str!("ui_config.json"));
}
fn vacant_output(project: &Project, p: &str) -> Result<(), Error> {
    match stat(project, p) {
        Err(Error::NotFound(_)) => {}
        Err(error) => return Err(error),
        Ok(_) => return Err("Recipe output already exists; choose a fresh snapshot/artifact path.".into()),
    }
    if let Some((parent, _)) = p.rsplit_once('/') {
        if stat(project, parent)?.kind != openidea_plugin::FileKind::Directory {
            return Err("Recipe output parent must already be a project directory.".into());
        }
    }
    Ok(())
}
fn parse(text: &str) -> Result<Vec<Change>, Error> {
    if text.len() > MAX_REPORT {
        return Err("Comparison JSON exceeds 1 MiB; no truncated review is accepted.".into());
    }
    let Unique(v) = serde_json::from_str::<Unique>(text)
        .map_err(|_| Error::Invalid("Expected a complete standalone Flyway diffText JSON object, without CLI banners/logs.".into()))?;
    if v.get("error").is_some_and(|e| !e.is_null()) {
        return Err("The report contains a CLI error, not a successful comparison.".into());
    }
    let rows = v
        .get("differences")
        .and_then(Value::as_array)
        .ok_or("Expected differences[] in documented diffText JSON.")?;
    if rows.len() > 500 {
        return Err("Comparison has more than 500 changes; narrow it externally first.".into());
    }
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for row in rows {
        let id = label(row, "id", false)?;
        if id == "-" || id.len() > 128 || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b)) || !seen.insert(id.clone()) {
            return Err("Change IDs must be unique bounded CLI-safe identifiers.".into());
        }
        let kind = label(row, "differenceType", false)?;
        if !["Add", "Edit", "Delete"].contains(&kind.as_str()) {
            return Err("Unsupported comparison change type; no mapping is inferred.".into());
        }
        let from = side(row.get("from").ok_or("Missing comparison source side.")?)?;
        let to = side(row.get("to").ok_or("Missing comparison target side.")?)?;
        if !match kind.as_str() {
            "Add" => from.is_some() && to.is_none(),
            "Delete" => from.is_none() && to.is_some(),
            _ => from.is_some() && to.is_some(),
        } {
            return Err("Change type and source/target sides disagree.".into());
        }
        out.push(Change {
            id,
            kind,
            object: label(row, "objectType", false)?,
            from,
            to,
        });
    }
    Ok(out)
}
fn path(value: &str) -> Result<&str, Error> {
    if value.is_empty()
        || value.len() > 1024
        || value.starts_with('/')
        || value.split('/').any(|p| p.is_empty() || matches!(p, "." | ".."))
        || !value.bytes().all(|b| b.is_ascii_alphanumeric() || b"/._-".contains(&b))
    {
        return Err("Use a concrete ASCII project-relative path without traversal, spaces, options or URLs.".into());
    }
    Ok(value)
}
fn owner(ctx: &Invocation) -> Result<(Project, String), Error> {
    let p = ctx.project().ok_or(Error::Stale)?;
    let root = p.root()?;
    if !root.starts_with('/') || root.len() > 4096 || root.contains(['\\', '\n', '\r']) || root.split('/').any(|p| matches!(p, "." | ".."))
    {
        return Err(Error::Stale);
    }
    Ok((p, root))
}
#[derive(Default)]
pub(crate) struct Review {
    next: u64,
    draft: Option<Draft>,
}
struct Draft {
    root: String,
    token: String,
    report: String,
    version: String,
    edition: String,
    model: String,
    source: String,
    target: String,
    artifact: String,
    loaded: Option<(String, String, Vec<Change>)>,
    selected: Vec<String>,
    recipe: Option<String>,
    notice: String,
    managed: managed::Core,
    loaded_managed: bool,
}
fn accept_managed_report(d:&mut Draft)->Result<(),Error> {
    if let Some(source)=&d.managed.report {
        let rows=parse(source)?;
        d.loaded=Some((d.report.clone(),source.clone(),rows));
        d.loaded_managed=true;d.selected.clear();d.recipe=None;
        d.notice="Comparison finished. Select Changes… to review their definitions.".into();
    }
    Ok(())
}
impl Review {
    pub(crate) fn cancel_owned(&mut self) {
        if let Some(d)=&mut self.draft {let _=d.managed.cancel();}
    }
    /// Preserve migration filters while observing the draft's bounded source
    /// subtrees, including parent deletion and arbitrary report extensions.
    fn watch(&self) {
        let mut filters: BTreeSet<String> = ["**/*.sql", "**/*.conf", "**/*.toml", "**/application*"].into_iter().map(str::to_owned).collect();
        if let Some(d) = &self.draft {
            for value in [&d.report, &d.model, &d.source, &d.artifact].into_iter().chain((d.target != "empty").then_some(&d.target)) {
                if let Some(first) = value.split('/').next() {
                    filters.insert(first.into());
                    if first.len() + 3 <= 1024 { filters.insert(format!("{first}/**")); }
                }
            }
        }
        let filters: Vec<_> = filters.iter().map(String::as_str).collect();
        let _ = host::subscribe_paths("files-changed", &filters);
    }
    pub(crate) fn invalidate(&mut self, events: &[Event]) {
        let Some(d) = &mut self.draft else { return };
        d.managed.membership();
        let related = |relative: &str, changed: &str| {
            let absolute = format!("{}/{relative}", d.root.trim_end_matches('/'));
            !changed.is_empty() && (changed == absolute || absolute.starts_with(&format!("{}/", changed.trim_end_matches('/')))
                || changed.starts_with(&format!("{absolute}/")))
        };
        let changed: Vec<_> = events.iter().flat_map(|event| match event {
            Event::FileSaved(path) => vec![path.as_str()],
            Event::FilesChanged(batch) => batch.created.iter().chain(&batch.modified).chain(&batch.removed).map(String::as_str).collect(),
            _ => Vec::new(),
        }).collect();
        let managed_changed=d.managed.invalidate(&changed);
        if managed_changed && d.loaded_managed {d.loaded=None;d.loaded_managed=false;d.selected.clear();}
        let report_changed = d.loaded.is_some() && changed.iter().any(|p| related(&d.report, p));
        let recipe_changed = d.recipe.is_some() && changed.iter().any(|p| [&d.model,&d.source,&d.target,&d.artifact].iter().any(|v| related(v, p)));
        if !report_changed && !recipe_changed && !managed_changed { return; }
        if report_changed { d.loaded = None; d.selected.clear(); }
        d.recipe = None;
        d.notice = "Observed report/recipe input change or removal; reload and reselect before reviewing again.".into();
        self.next = self.next.wrapping_add(1);
        let random = std::collections::hash_map::RandomState::new();
        d.token = format!("{:016x}{:016x}", random.hash_one((&d.root,self.next,"comparison")), random.hash_one((&d.root,self.next,"controls")));
        refresh();
    }
    pub(crate) fn begin(&mut self, ctx: &Invocation) -> Result<(), Error> {
        let (_, root) = owner(ctx)?;
        self.next = self.next.wrapping_add(1);
        let random = std::collections::hash_map::RandomState::new();
        let token = format!(
            "{:016x}{:016x}",
            random.hash_one((&root, self.next, "comparison")),
            random.hash_one((&root, self.next, "controls"))
        );
        if self.draft.as_ref().is_some_and(|d|d.managed.running()){return Err("Stop the running comparison first.".into());}
        self.draft = Some(Draft {
            root,
            token,
            report: "review/diff-text.json".into(),
            version: String::new(),
            edition: "Unknown".into(),
            model: "schema-model".into(),
            source: "review/source.snapshot.json".into(),
            target: "empty".into(),
            artifact: "review/changes.diff".into(),
            loaded: None,
            selected: Vec::new(),
            recipe: None,
            managed: managed::Core::default(),
            loaded_managed: false,
            notice: "Offline only. Load an existing local JSON report; rendering does not execute Flyway.".into(),
        });
        self.watch();
        Ok(())
    }
    pub(crate) fn view(&self, ctx: &Invocation) -> Result<View, Error> {
        let Ok((project, root)) = owner(ctx) else { return Ok(View::message("Open a project to review comparison reports.")); };
        let Some(d) = self.draft.as_ref().filter(|d| d.root == root) else {
            return Ok(View::message("No comparison report yet. Run Flyway: Review Local Comparison Report… to load one for this project."));
        };
        if d.loaded.as_ref().is_some_and(|(report,_,_)| scoped_path(&project, report).is_err()) {
            return Ok(View::message("The loaded report is no longer inside this project. Run Flyway: Review Local Comparison Report… again."));
        }
        let key = |s: &str| format!("{s}@{}", d.token);
        let mut fields = vec![
            Field::new(&key("report"), "Local diffText JSON report", FieldValue::Text(d.report.clone())),
            Field::new(
                &key("version"),
                "Reported CLI version (unverified)",
                FieldValue::Text(d.version.clone()),
            ),
            Field::new(
                &key("edition"),
                "Reported edition (unverified)",
                FieldValue::Choice(
                    vec!["Unknown".into(), "Community".into(), "Teams".into(), "Enterprise".into()],
                    d.edition.clone(),
                ),
            ),
            Field::new(
                &key("model"),
                "Schema model directory",
                FieldValue::Text(d.model.clone()),
            ),
            Field::new(&key("source"), "Source snapshot/output path", FieldValue::Text(d.source.clone())),
            Field::new(&key("target"), "Target snapshot path or empty", FieldValue::Text(d.target.clone())),
            Field::new(&key("artifact"), "Diff artifact output path", FieldValue::Text(d.artifact.clone())),
            Field::new(
                "provenance",
                "Review provenance",
                FieldValue::readonly_text(
                    &format!(
                        "Project: {}\nReport: {}\n{}\nNo installed binary/license/artifact/report pairing or effective DB target is verified. Object definitions are NOT a migration/deployment script.",
                        d.root,
                        if d.loaded_managed {"managed in-memory diffText output (no report file written)"} else {d.loaded.as_ref().map_or("not loaded", |(p, _, _)| p.as_str())},
                        d.notice
                    ),
                    true,
                ),
            ),
        ];
        if let Some((_, _, rows)) = &d.loaded {
            fields.push(Field::new(
                "selection",
                "Selected change IDs",
                FieldValue::readonly_text(&serde_json::to_string(&d.selected).unwrap_or_default(), true),
            ));
            let mut bytes = 0;
            for row in rows.iter().filter(|r| d.selected.contains(&r.id)) {
                for (label, side) in [("SOURCE", &row.from), ("TARGET", &row.to)] {
                    if let Some(side) = side {
                        let sql = side
                            .sql
                            .as_deref()
                            .unwrap_or("Definition absent. Import diffText output, not only a diff summary.");
                        bytes += sql.len();
                        if bytes > MAX_SQL {
                            return Err("Selected SQL exceeds the 64 KiB preview budget; select fewer changes.".into());
                        }
                        fields.push(Field::new(
                            &format!("sql-{}-{label}", row.id),
                            &format!(
                                "{} {} · {} · {label} object definition · {}.{}",
                                row.kind, row.object, row.id, side.schema, side.name
                            ),
                            FieldValue::readonly_text(sql, true),
                        ));
                    }
                }
            }
        }
        if d.managed.prepared() {
            fields.push(Field::new("managed-status", "Managed comparison readiness", FieldValue::readonly_text(&d.managed.status, true)));
        }
        if let Some(recipe) = &d.recipe {
            fields.push(Field::new(
                "recipe",
                "Reviewed argv recipe — NOT EXECUTED",
                FieldValue::readonly_text(recipe, true),
            ));
        }
        let mut view=View::new(Body::Form(fields))
            .button(Button::new(&key("load"), "Load Report…").icon("folder-open"))
            .button(Button::new(&key("select"), "Select Changes…").icon("list").enabled(d.loaded.is_some()));
        if d.managed.startable() {view=view.button(Button::new(&key("start-managed"), "Start Comparison").icon("run"));}
        if d.managed.continuable() {view=view.button(Button::new(&key("continue-managed"), "Continue Comparison").icon("run"));}
        if d.managed.running() {view=view.button(Button::new(&key("check-managed"), "Check Progress").icon("refresh")).button(Button::new(&key("cancel-managed"), "Stop Comparison").icon("stop"));}
        view=view.button(Button::new(&key("more"), "More…").icon("more"));
        Ok(view.status("Offline report review, or a managed comparison with the installed Flyway CLI."))
    }
    pub(crate) fn handle(&mut self, event: ViewEvent, ctx: &Invocation) -> Result<(), Error> {
        let (project, root) = owner(ctx)?;
        let d = self.draft.as_mut().filter(|d| d.root == root).ok_or(Error::Stale)?;
        let id = match &event {
            ViewEvent::Field(id, _) | ViewEvent::Button(id) => id.clone(),
            _ => return Ok(()),
        };
        let (key, token) = id.rsplit_once('@').ok_or(Error::Stale)?;
        if token != d.token {
            return Err(Error::Stale);
        }
        let picked;
        let key = if key == "more" && matches!(event, ViewEvent::Button(_)) {
            let actions = [("plan", "Show CLI Commands…"), ("prepare-managed", "Prepare Comparison…"), ("clear", "Clear")];
            let items: Vec<_> = actions.iter().map(|(id, label)| PickItem::new(id, label)).collect();
            let Some(chosen) = host::pick("More comparison actions", &items)? else { return Ok(()) };
            picked = actions.iter().find(|(id, _)| *id == chosen).ok_or(Error::Stale)?.0;
            picked
        } else {
            key
        };
        let result: Result<(), Error> = (|| {
            match event {
                ViewEvent::Field(_, value) => {
                    if d.managed.running(){return Err("Stop the running comparison before changing its inputs.".into());}
                    if d.loaded_managed {d.loaded=None;d.selected.clear();}
                    d.managed=managed::Core::default();
                    d.loaded_managed=false;
                    if value.len() > 1024 || value.chars().any(char::is_control) {
                        return Err("Review input exceeds supported bounds.".into());
                    }
                    match key {
                        "report" => {
                            path(&value)?;
                            d.report = value;
                            d.loaded = None;
                            d.selected.clear();
                        }
                        "version" => d.version = value,
                        "edition" if ["Unknown", "Community", "Teams", "Enterprise"].contains(&value.as_str()) => d.edition = value,
                        "model" => {
                            path(&value)?;
                            d.model = value;
                        }
                        "source" => {
                            path(&value)?;
                            d.source = value;
                        }
                        "target" => {
                            path(&value)?;
                            d.target = value;
                        }
                        "artifact" => {
                            path(&value)?;
                            d.artifact = value;
                        }
                        _ => return Err("This review field is read-only or unsupported.".into()),
                    }
                    d.recipe = None;
                }
                ViewEvent::Button(_) => match key {
                    "prepare-managed" => {
                        let plan=managed::Plan::capture(&project,d)?;
                        if ui_kit::prompts::confirm_exact("Prepare offline comparison?",&plan.review())? {
                            plan.revalidate_prepare(&project)?;
                            let profile=managed::offline_profile(&project);
                            d.managed.prepare(plan, profile)?;
                            d.notice=d.managed.status.clone();
                        }
                    }
                    "start-managed" => d.managed.start(&project)?,
                    "continue-managed" => d.managed.next(&project)?,
                    "cancel-managed" => d.managed.cancel()?,
                    "check-managed" => {
                        d.managed.check(&project)?;
                        accept_managed_report(d)?;
                    }
                    "clear" => {
                        if d.managed.running(){return Err("Stop the running comparison before clearing it.".into());}
                        d.managed=managed::Core::default();d.loaded_managed=false;
                        d.loaded = None;
                        d.selected.clear();
                        d.recipe = None;
                        d.notice = "Review cleared; no files changed.".into();
                    }
                    "load" => {
                        if d.managed.running(){return Err("Stop the running comparison before loading another report.".into());}
                        d.loaded_managed=false;
                        // Failed reload invalidates old selection rather than mixing reports.
                        d.loaded = None;
                        d.selected.clear();
                        d.recipe = None;
                        let report = path(&d.report)?.to_owned();
                        let source = read(&project, &report)?;
                        let rows = parse(&source)?;
                        d.notice = format!(
                            "Cached {} changes from imported project-visible text (may be unsaved/stale). Snapshot age, producer identity and artifact pairing unknown.",
                            rows.len()
                        );
                        d.loaded = Some((report, source, rows));
                    }
                    "select" => {
                        d.recipe = None;
                        let (report, source, rows) = d.loaded.as_ref().ok_or("Load a local diffText report first.")?;
                        if d.loaded_managed {d.managed.report_fresh(&project,source)?;}
                        if !d.loaded_managed && read(&project, report)? != *source {
                            d.selected.clear();
                            d.notice = "The imported report changed. Reload before selecting definitions.".into();
                            return Err(Error::Stale);
                        }
                        let items: Vec<_> = rows
                            .iter()
                            .map(|r| PickItem {
                                key: r.id.clone(),
                                label: format!(
                                    "{} {} · {} · {}",
                                    r.kind,
                                    r.object,
                                    r.from
                                        .as_ref()
                                        .or(r.to.as_ref())
                                        .map(|s| format!("{}.{}", s.schema, s.name))
                                        .unwrap_or_default(),
                                    r.id
                                ),
                                description: Some("Choose raw source/target object definitions, not executable migration SQL".into()),
                            })
                            .collect();
                        let Some(ids) = host::pick_many("Select comparison definitions to review", &items)? else {
                            return Ok(());
                        };
                        if ids.len() > 32
                            || ids.iter().collect::<BTreeSet<_>>().len() != ids.len()
                            || ids.iter().any(|id| !rows.iter().any(|r| &r.id == id))
                        {
                            return Err("Select up to 32 distinct known change IDs.".into());
                        }
                        let bytes: usize = rows
                            .iter()
                            .filter(|r| ids.contains(&r.id))
                            .flat_map(|r| [&r.from, &r.to])
                            .flatten()
                            .filter_map(|s| s.sql.as_ref())
                            .map(String::len)
                            .sum();
                        if bytes > MAX_SQL {
                            return Err("Select fewer changes: SQL preview exceeds 64 KiB.".into());
                        }
                        if !d.loaded_managed && read(&project, report)? != *source {
                            return Err(Error::Stale);
                        }
                        if owner(ctx)?.1 != d.root {
                            return Err(Error::Stale);
                        }
                        if d.loaded_managed {d.managed.report_fresh(&project,source)?;}
                        d.selected = ids;
                        d.recipe = None;
                    }
                    "plan" => {
                        if d.loaded_managed {return Err("Generated IDs belong to this completed comparison; do not reuse them in a new recipe.".into());}
                        d.recipe = None;
                        if d.version != "13.12.1" || d.edition != "Enterprise" {
                            return Err("Recipe profile is the documented Flyway 13.12.1 Enterprise CLI only. Inputs are self-reported; no installed version/license is attested. Offline JSON review does not require this recipe gate.".into());
                        }
                        for p in [&d.model, &d.source, &d.artifact] {
                            path(p)?;
                        }
                        if d.target != "empty" {
                            path(&d.target)?;
                        }
                        if [&d.model, &d.source, &d.artifact, &d.report]
                            .iter()
                            .enumerate()
                            .any(|(i, p)| [&d.model, &d.source, &d.artifact, &d.report][..i].contains(p))
                            || d.target == d.source
                            || d.target == d.artifact
                            || d.target == d.report
                            || d.target == d.model
                        {
                            return Err("Use distinct schema-model, report, source, target and artifact paths.".into());
                        }
                        vacant_output(&project, &d.source)?;
                        vacant_output(&project, &d.artifact)?;
                        let model = stat(&project, &d.model)?;
                        if model.kind != openidea_plugin::FileKind::Directory {
                            return Err("Schema model path must be an existing project directory.".into());
                        }
                        if d.target != "empty" && stat(&project, &d.target)?.kind != openidea_plugin::FileKind::File {
                            return Err("Target snapshot path must be an existing local file.".into());
                        }
                        let changes = if d.selected.is_empty() {
                            return Err("Load a report and select changes first.".into());
                        } else {
                            d.selected.join(",")
                        };
                        let (report, source, _) = d.loaded.as_ref().ok_or(Error::Stale)?;
                        if !d.loaded_managed && read(&project, report)? != *source {
                            return Err(Error::Stale);
                        }
                        let target = if d.target == "empty" {
                            "empty".into()
                        } else {
                            format!("snapshot:{}", d.target)
                        };
                        let mut commands = vec![
                            vec![
                                "snapshot".to_owned(),
                                "-snapshot.source=schemaModel".into(),
                                format!("-schemaModelLocation={}", d.model),
                                format!("-snapshot.filename={}", d.source),
                                "-outputType=json".into(),
                            ],
                            vec![
                                "diff".into(),
                                format!("-diff.source=snapshot:{}", d.source),
                                format!("-diff.target={target}"),
                                format!("-diff.artifactFilename={}", d.artifact),
                                "-outputType=json".into(),
                            ],
                            vec![
                                "diffText".into(),
                                format!("-diffText.artifactFilename={}", d.artifact),
                                format!("-diffText.changes={changes}"),
                                "-outputType=json".into(),
                            ],
                        ];
                        for command in &mut commands {
                            command.push(format!("-workingDirectory={}", d.root));
                        }
                        let body = format!(
                            "Project: {}\nReported profile: Flyway 13.12.1 Enterprise (unverified)\nProgram: flyway\nArgv arrays:\n{}\nThis only records a recipe. It does not execute, connect or write. Actual external execution can read inherited config, connect, or write snapshot/artifact files. Report IDs are not proven to belong to a newly generated artifact; rerun comparison and reselect externally. No migration SQL is generated.",
                            d.root,
                            serde_json::to_string_pretty(&commands).unwrap_or_default()
                        );
                        if ui_kit::prompts::confirm_exact("Record non-executing comparison recipe?", &body)? {
                            if owner(ctx)?.1 != d.root || read(&project, report)? != *source {
                                return Err(Error::Stale);
                            }
                            vacant_output(&project, &d.source)?;
                            vacant_output(&project, &d.artifact)?;
                            if stat(&project, &d.model)?.kind != openidea_plugin::FileKind::Directory {
                                return Err(Error::Stale);
                            }
                            if d.target != "empty" && stat(&project, &d.target)?.kind != openidea_plugin::FileKind::File {
                                return Err(Error::Stale);
                            }
                            d.recipe = Some(body);
                        }
                    }
                    _ => return Err(Error::NotFound("comparison control".into())),
                },
                _ => {}
            }
            Ok(())
        })();
        // Error paths can invalidate selection/recipes; render that state too.
        ui_kit::web::refresh_view(VIEW, include_str!("ui_config.json"));
        self.watch();
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    const REPORT: &str = r#"{"differences":[{"id":"add-1","differenceType":"Add","objectType":"Table","from":{"schema":"app","name":"notes","definition":"CREATE TABLE app.notes (body text);"},"to":null},{"id":"delete-2","differenceType":"Delete","objectType":"View","from":null,"to":{"schema":"app","name":"old_view","definition":"CREATE VIEW app.old_view AS SELECT 1;"}}]}"#;
    fn host() -> MockHost {
        let h = MockHost::install().with_capabilities(&["project.read", "storage"]);
        h.add_dir("review");
        h.add_dir("schema-model");
        h.add_file("review/diff-text.json", REPORT);
        h.add_file("schema-model/notes.sql", "CREATE TABLE app.notes (body text);");
        h
    }
    fn id(r: &Review, key: &str) -> String {
        format!("{key}@{}", r.draft.as_ref().unwrap().token)
    }
    fn button(r: &mut Review, h: &MockHost, key: &str) -> Result<(), Error> {
        let event = ViewEvent::Button(id(r, key));
        r.handle(event, &h.invocation())
    }
    fn field(r: &mut Review, h: &MockHost, key: &str, value: &str) -> Result<(), Error> {
        let event = ViewEvent::Field(id(r, key), value.into());
        r.handle(event, &h.invocation())
    }
    fn ready(r: &mut Review, h: &MockHost) {
        r.begin(&h.invocation()).unwrap();
        button(r, h, "load").unwrap();
        h.answer_next_prompt(Some(&["add-1", "delete-2"]));
        button(r, h, "select").unwrap();
    }
    #[test]
    fn parses_supported_sides_without_inventing_delete_or_migration_sql() {
        let rows = parse(REPORT).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows[0].from.as_ref().unwrap().sql.as_ref().unwrap().contains("CREATE TABLE"));
        assert!(rows[1].from.is_none());
        let h = host();
        let mut r = Review::default();
        ready(&mut r, &h);
        let view = r.view(&h.invocation()).unwrap();
        let Body::Form(fields) = view.body else { panic!("form") };
        let sql: Vec<_> = fields.iter().filter(|f| f.key.starts_with("sql-")).collect();
        assert_eq!(sql.len(), 2);
        assert!(sql[1].label.contains("TARGET object definition"));
        assert!(h.process_runs().is_empty());
        assert!(h.requests().is_empty());
    }
    #[test]
    fn toolbar_keeps_secondary_actions_behind_more() {
        let h = host();
        let mut r = Review::default();
        ready(&mut r, &h);
        let view = r.view(&h.invocation()).unwrap();
        assert!(view.toolbar.len() <= 5 && view.toolbar.iter().all(|b| b.icon.is_some()));
        assert!(view.toolbar.iter().any(|b| b.label == "More…"));
        h.answer_next_prompt(None);
        button(&mut r, &h, "more").unwrap();
        assert!(r.draft.as_ref().unwrap().loaded.is_some(), "a cancelled pick does nothing");
        h.answer_next_prompt(Some(&["clear"]));
        button(&mut r, &h, "more").unwrap();
        assert!(r.draft.as_ref().unwrap().loaded.is_none());
    }
    #[test]
    fn malformed_duplicate_unsafe_and_unknown_report_shapes_are_refused() {
        for report in [
            "{",
            r#"{"differences":[],"differences":[]}"#,
            r#"{"differences":[],"error":{"message":"failed"}}"#,
            &REPORT.replace("\"id\":\"add-1\"", "\"id\":\"add-1\",\"id\":\"other\""),
            &REPORT.replace("delete-2", "add-1"),
            &REPORT.replace("add-1", "-changes=all"),
            &REPORT.replace("add-1", "-"),
            &REPORT.replace("\"Delete\"", "\"Unknown\""),
            &REPORT.replace("\"differenceType\":\"Add\"", "\"differenceType\":\"Delete\""),
            &format!("banner\n{REPORT}"),
        ] {
            assert!(parse(report).is_err(), "{report}");
        }
        assert!(parse(&" ".repeat(MAX_REPORT + 1)).is_err());
        let mut v: Value = serde_json::from_str(REPORT).unwrap();
        v["differences"][0]["from"]["definition"] = Value::String("x".repeat(MAX_SQL + 1));
        assert!(parse(&v.to_string()).is_err());
        let one = v["differences"][0].clone();
        v["differences"] = Value::Array(vec![one; 501]);
        assert!(parse(&v.to_string()).is_err());
    }
    #[test]
    fn session_project_and_readonly_guards_reject_cross_window_events() {
        let h = host();
        let mut r = Review::default();
        r.begin(&h.invocation()).unwrap();
        let old = id(&r, "load");
        r.begin(&h.invocation()).unwrap();
        assert!(matches!(r.handle(ViewEvent::Button(old), &h.invocation()), Err(Error::Stale)));
        assert!(
            r.handle(ViewEvent::Field("provenance".into(), "forged".into()), &h.invocation())
                .is_err()
        );
        for p in [
            "../outside.json",
            "/tmp/report.json",
            "https://host/report.json",
            "-output=x",
            "review/*.json",
            "review//x",
        ] {
            assert!(field(&mut r, &h, "report", p).is_err());
        }
        h.set_project_root(Some("/other"));
        assert!(matches!(button(&mut r, &h, "load"), Err(Error::Stale)));
        h.set_project_root(None);
        assert!(r.begin(&h.invocation()).is_err());
        assert!(h.process_runs().is_empty());
    }
    #[test]
    fn stale_reload_cancel_selection_and_unknown_ids_do_not_mix_reports() {
        let h = host();
        let mut r = Review::default();
        ready(&mut r, &h);
        let selected = r.draft.as_ref().unwrap().selected.clone();
        h.answer_next_prompt(None);
        button(&mut r, &h, "select").unwrap();
        assert_eq!(r.draft.as_ref().unwrap().selected, selected);
        h.open_editor("/project/review/diff-text.json", "json", "{", &[]);
        let refreshes = h.refreshed_views().len();
        assert!(matches!(button(&mut r, &h, "select"), Err(Error::Stale)));
        assert!(
            h.refreshed_views().len() > refreshes,
            "cleared stale state must reach the visible pane"
        );
        let Body::Form(fields) = r.view(&h.invocation()).unwrap().body else {
            panic!("form")
        };
        assert!(fields.iter().all(|f| !f.key.starts_with("sql-") && f.key != "recipe"));
        assert!(button(&mut r, &h, "load").is_err());
        assert!(r.draft.as_ref().unwrap().loaded.is_none());
        assert!(r.draft.as_ref().unwrap().selected.is_empty());
        assert!(h.process_runs().is_empty());
    }
    #[test]
    fn recipe_requires_reported_profile_fresh_distinct_vacant_paths_and_exact_review() {
        let h = host();
        let mut r = Review::default();
        ready(&mut r, &h);
        assert!(button(&mut r, &h, "plan").is_err());
        field(&mut r, &h, "version", "13.12.1").unwrap();
        field(&mut r, &h, "edition", "Teams").unwrap();
        assert!(button(&mut r, &h, "plan").is_err());
        field(&mut r, &h, "edition", "Enterprise").unwrap();
        h.answer_next_prompt(None);
        button(&mut r, &h, "plan").unwrap();
        assert!(r.draft.as_ref().unwrap().recipe.is_none());
        h.answer_next_prompt(Some(&[]));
        button(&mut r, &h, "plan").unwrap();
        let recipe = r.draft.as_ref().unwrap().recipe.as_ref().unwrap();
        for expected in [
            "-snapshot.source=schemaModel",
            "-diff.target=empty",
            "-diffText.changes=add-1,delete-2",
            "-workingDirectory=/project",
        ] {
            assert!(recipe.contains(expected), "{recipe}");
        }
        assert!(!recipe.contains("migrate"));
        assert!(h.process_runs().is_empty());
        assert!(h.requests().is_empty());
        h.add_file("review/source.snapshot.json", "{}");
        assert!(button(&mut r, &h, "plan").is_err());
        assert!(r.draft.as_ref().unwrap().recipe.is_none());
        field(&mut r, &h, "source", "review/fresh.snapshot.json").unwrap();
        field(&mut r, &h, "target", "review/fresh.snapshot.json").unwrap();
        assert!(button(&mut r, &h, "plan").is_err());
    }
    #[test]
    fn source_and_target_definitions_remain_separate_and_large_selections_are_refused() {
        let h = host();
        let mut v: Value = serde_json::from_str(REPORT).unwrap();
        v["differences"][0]["differenceType"] = "Edit".into();
        v["differences"][0]["to"] = v["differences"][0]["from"].clone();
        v["differences"][0]["from"]["definition"] = Value::String("a".repeat(40 << 10));
        v["differences"][0]["to"]["definition"] = Value::String("b".repeat(40 << 10));
        h.add_file("review/diff-text.json", &v.to_string());
        let mut r = Review::default();
        r.begin(&h.invocation()).unwrap();
        button(&mut r, &h, "load").unwrap();
        h.answer_next_prompt(Some(&["add-1"]));
        assert!(button(&mut r, &h, "select").is_err());
        assert!(r.draft.as_ref().unwrap().selected.is_empty());
        assert!(h.process_runs().is_empty());
    }
    #[test]
    fn observed_report_change_retires_sql_and_old_controls_without_render_io_or_repeat_notifications() {
        let h = host();
        let mut plugin = Flyway::default();
        ready(&mut plugin.comparison, &h);
        let old = id(&plugin.comparison, "select");
        let notifications = || h.view_messages().iter().filter(|(view,_)| view == "migrations").count();
        let before = notifications();
        plugin.handle_events(&[Event::FileSaved("/project/review/diff-text.json".into())], &h.invocation());
        assert_eq!(notifications(), before + 1);
        plugin.handle_events(&[Event::FileSaved("/project/review/diff-text.json".into())], &h.invocation());
        assert_eq!(notifications(), before + 1);
        let d = plugin.comparison.draft.as_ref().unwrap();
        assert!(d.loaded.is_none() && d.selected.is_empty() && d.recipe.is_none());
        assert!(matches!(plugin.comparison.handle(ViewEvent::Button(old), &h.invocation()), Err(Error::Stale)));
        let view = plugin.comparison.view(&h.invocation()).unwrap();
        let Body::Form(fields) = view.body else { panic!("form") };
        assert!(fields.iter().all(|f| !f.key.starts_with("sql-")));
        assert_eq!(notifications(), before + 1, "render must not invalidate itself");
        assert!(h.process_runs().is_empty() && h.requests().is_empty());
    }
    #[test]
    fn foreign_or_missing_root_replaces_sensitive_body_without_destroying_owned_cache() {
        let h = host();
        let mut r = Review::default();
        ready(&mut r, &h);
        h.set_project_root(Some("/other"));
        assert!(matches!(r.view(&h.invocation()).unwrap().body, Body::Message(_)));
        assert!(r.draft.as_ref().unwrap().loaded.is_some());
        h.set_project_root(None);
        assert!(matches!(r.view(&h.invocation()).unwrap().body, Body::Message(_)));
        h.set_project_root(Some("/project"));
        h.add_project("/project/review");
        assert!(matches!(r.view(&h.invocation()).unwrap().body, Body::Message(_)));
        assert!(button(&mut r, &h, "load").is_err());
        assert!(read(&h.invocation().project().unwrap(), "review/diff-text.json").is_err());
    }
    #[test]
    fn recipe_source_changes_retire_only_recipe_and_foreign_changes_are_quiet() {
        let h = host();
        let mut r = Review::default();
        ready(&mut r, &h);
        r.draft.as_mut().unwrap().recipe = Some("reviewed recipe".into());
        let old = id(&r, "plan");
        r.invalidate(&[Event::FileSaved("/other/schema-model/notes.sql".into())]);
        assert!(r.draft.as_ref().unwrap().recipe.is_some());
        r.invalidate(&[Event::FilesChanged(openidea_plugin::FilesChanged { removed: vec!["/project/schema-model".into()], ..Default::default() })]);
        let d = r.draft.as_ref().unwrap();
        assert!(d.recipe.is_none());
        assert!(d.loaded.is_some() && !d.selected.is_empty());
        assert!(matches!(r.handle(ViewEvent::Button(old), &h.invocation()), Err(Error::Stale)));
    }
    #[test]
    fn bounded_draft_watches_keep_migration_filters_and_support_custom_report_extensions() {
        let h = host().with_capabilities(&["project.read", "events", "storage"]);
        let mut r = Review::default();
        r.begin(&h.invocation()).unwrap();
        field(&mut r, &h, "report", "custom/deep/report.data").unwrap();
        let filters = &h.path_subscriptions()["files-changed"];
        assert!(filters.iter().any(|g| g == "**/*.sql"));
        assert!(filters.iter().any(|g| g == "custom"));
        assert!(filters.iter().any(|g| g == "custom/**"));
        assert!(filters.len() <= 14);
        field(&mut r, &h, "report", "empty").unwrap();
        assert!(h.path_subscriptions()["files-changed"].iter().any(|g| g == "empty"), "only target empty is a sentinel");
        h.add_project("/project/schema-model");
        assert!(scoped_path(&h.invocation().project().unwrap(), "schema-model/new.sql").is_err());
        assert!(h.process_runs().is_empty());
    }
    #[test]
    fn native_supporting_view_does_not_run_or_replace_existing_generation_and_managed_state() {
        let h = host();
        let mut plugin = Flyway::default();
        plugin.run_command("review-comparison", &h.invocation(), None).unwrap();
        let view = plugin.render(VIEW, None, &h.invocation()).unwrap();
        assert!(matches!(view.body, Body::Form(_)));
        assert!(plugin.generation.is_none());
        assert!(h.process_runs().is_empty());
        assert!(h.revealed_views().contains(&"migrations".to_owned()));
    }
}
