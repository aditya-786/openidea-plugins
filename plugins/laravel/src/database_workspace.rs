//! Hub "Database" and "Schema" pages: tables from migrations, compare with the live database,
//! read-only masked rows and the schema diagram. Browsing is source-only; compare and rows
//! run a bounded read-only PHP probe on explicit request, through the Artisan runner.
use crate::artisan_runner::Runner;
use crate::index::Index;
use crate::migrations::keys::Schema;
use crate::schema_compare::{self, Live, Relation};
use openidea_plugin::{Error, Invocation, Position, Project, Range, host};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const DATABASE_VIEW: &str = "laravel-database";
const MAX_MIGRATIONS: usize = 2000;
const MAX_SOURCE_BYTES: usize = 32 * 1024 * 1024;
const MAX_REPLY: usize = 4 * 1024 * 1024;
const MAX_CELLS: usize = 128 * 100;

#[derive(Default)]
pub(crate) struct State {
    projects: BTreeMap<String, Session>,
}

#[derive(Default)]
struct Session {
    compared: Option<Value>,
    migration_paths: BTreeSet<String>,
    sources: Sources,
}

/// Migration text by path, with the (modified, size) stamp it was read at.
type Sources = BTreeMap<String, (Option<(u64, u64)>, String)>;

struct Migrations {
    schema: Schema,
    paths: BTreeSet<String>,
    unreadable: Vec<String>,
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case", deny_unknown_fields)]
enum Request {
    State,
    Compare,
    Rows {
        table: String,
        #[serde(default)]
        page: u32,
        #[serde(default = "default_size")]
        size: u32,
    },
    Open {
        path: String,
        line: u32,
    },
}

fn default_size() -> u32 {
    25
}

fn owner(ctx: &Invocation) -> Result<(Project, String), Error> {
    let project = ctx
        .project()
        .ok_or_else(|| Error::Invalid("Open a Laravel project first.".into()))?;
    let root = project.root()?;
    if !crate::hover::canonical(&root) || !host::projects()?.iter().any(|p| p.root().ok().as_deref() == Some(&root)) {
        return Err(Error::Stale);
    }
    Ok((project, root))
}

fn migration_schema(project: &Project, root: &str, sources: &mut Sources) -> Result<Migrations, Error> {
    migration_schema_with(project, root, sources, &mut |path| project.read_file(path))
}

fn migration_schema_with(
    project: &Project,
    root: &str,
    sources: &mut Sources,
    read: &mut dyn FnMut(&str) -> Result<String, Error>,
) -> Result<Migrations, Error> {
    let _task = host::begin_task(crate::ui_text::text("database.task.read", "Read migrations"), false);
    let mut paths: Vec<String> = project
        .find_files("database/migrations/**/*.php", MAX_MIGRATIONS as u32 + 1)?
        .into_iter()
        .filter(|path| host::project_of(path).and_then(|owner| owner.root().ok()).as_deref() == Some(root))
        .collect();
    paths.sort();
    paths.dedup();
    if paths.len() > MAX_MIGRATIONS {
        return Err(Error::Invalid(format!("More than {MAX_MIGRATIONS} migrations; the schema cannot be drawn completely.")));
    }
    sources.retain(|path, _| paths.binary_search(path).is_ok());
    let mut schema = Schema::default();
    let mut unreadable = Vec::new();
    let mut bytes = 0;
    for path in &paths {
        // ponytail: stamps have one-second resolution; a same-size edit within that second is missed until the next one.
        let stamp = project.stat(path).ok().and_then(|stat| Some((stat.modified?, stat.size)));
        let cached = stamp.is_some() && sources.get(path).is_some_and(|(seen, _)| *seen == stamp);
        if !cached {
            match read(path) {
                Ok(text) => {
                    sources.insert(path.clone(), (stamp, text));
                }
                Err(_) => {
                    sources.remove(path);
                    unreadable.push(path.clone());
                    continue;
                }
            }
        }
        let text = &sources[path].1;
        bytes += text.len();
        if bytes > MAX_SOURCE_BYTES {
            return Err(Error::Invalid("Migration sources exceed 32 MiB.".into()));
        }
        schema.apply(path, text);
    }
    Ok(Migrations { schema, paths: paths.into_iter().collect(), unreadable })
}

fn models_and_relations(project: &Project, index: Option<&Index>) -> (Vec<(String, String)>, Vec<Relation>) {
    let Some(index) = index else {
        return (Vec::new(), Vec::new());
    };
    let owned = |path: &str| crate::owns_current_path(project, path);
    let models: Vec<(String, String)> = crate::eloquent_members::model_inventory(&index.eloquent_models)
        .into_iter()
        .filter(|(_, path, _)| owned(path))
        .map(|(full, _, table)| (full, table))
        .collect();
    let mut relations = Vec::new();
    for (full, _) in &models {
        let Some(facts) = crate::eloquent_members::helper_facts::facts(index, full, &owned) else {
            continue;
        };
        for relation in facts.relations {
            relations.push(Relation {
                model: full.clone(),
                method: relation.name,
                kind: relation.kind,
                target: relation.target,
            });
        }
    }
    (models, relations)
}

fn probe_json(raw: &str) -> Result<Value, Error> {
    let value = crate::strict_json::parse(raw, MAX_REPLY)?;
    if let Some(message) = value.get("error").and_then(Value::as_str) {
        return Err(Error::Invalid(message.chars().take(300).collect()));
    }
    Ok(value)
}

fn live_from(value: Value) -> Result<Live, Error> {
    let live: Live = serde_json::from_value(value).map_err(|error| Error::Invalid(error.to_string()))?;
    let bounded = live.tables.len() <= schema_compare::MAX_LIVE_TABLES
        && live.pending.len() <= 500
        && live.tables.iter().all(|table| table.columns.len() <= 512 && table.indexes.len() <= 64);
    if !bounded {
        return Err(Error::Invalid("The database reply exceeds its bounds.".into()));
    }
    Ok(live)
}

fn rows_reply(value: &Value) -> Result<Value, Error> {
    let columns = value["columns"].as_array().filter(|columns| columns.len() <= 128 && columns.iter().all(Value::is_string));
    let rows = value["rows"].as_array().filter(|rows| rows.len() <= 100);
    let (Some(columns), Some(rows)) = (columns, rows) else {
        return Err(Error::Invalid("Unexpected rows reply.".into()));
    };
    let shaped = rows
        .iter()
        .all(|row| row.as_array().is_some_and(|cells| cells.len() == columns.len() && cells.iter().all(|cell| !cell.is_array() && !cell.is_object())));
    if !shaped || rows.len() * columns.len() > MAX_CELLS {
        return Err(Error::Invalid("Unexpected rows reply.".into()));
    }
    let masked: Vec<&Value> = value["masked"].as_array().into_iter().flatten().filter(|name| name.is_string()).collect();
    Ok(json!({
        "table": value["table"].as_str().unwrap_or_default(),
        "columns": columns,
        "rows": rows,
        "total": value["total"].as_u64().unwrap_or(0),
        "page": value["page"].as_u64().unwrap_or(0),
        "size": value["size"].as_u64().unwrap_or(25),
        "masked": masked,
    }))
}

impl State {
    pub(crate) fn handle(&mut self, ctx: &Invocation, message: &str, index: Option<&Index>, runner: Runner) -> Result<Value, Error> {
        self.handle_with(ctx, message, index, &mut |project, mode, options| {
            crate::runtime_inspection::probe(project, runner, mode, options)
        })
    }

    fn handle_with(
        &mut self,
        ctx: &Invocation,
        message: &str,
        index: Option<&Index>,
        run: &mut dyn FnMut(&Project, &str, &Value) -> Result<String, Error>,
    ) -> Result<Value, Error> {
        if message.len() > 4096 {
            return Err(Error::Invalid("Request is too large.".into()));
        }
        let request: Request = serde_json::from_str(message).map_err(|error| Error::Invalid(error.to_string()))?;
        let (project, root) = owner(ctx)?;
        if !self.projects.contains_key(&root) && self.projects.len() >= 8 {
            return Err(Error::Invalid("Too many projects in this session; reload the plugin.".into()));
        }
        let session = self.projects.entry(root.clone()).or_default();
        match request {
            Request::State => {
                let migrations = migration_schema(&project, &root, &mut session.sources)?;
                let (models, relations) = models_and_relations(&project, index);
                session.migration_paths = migrations.paths;
                let mut diagram = schema_compare::diagram(&root, &migrations.schema, &models, &relations);
                diagram["unreadable"] = json!(migrations.unreadable);
                diagram["live"] = session.compared.clone().unwrap_or(Value::Null);
                diagram["root"] = json!(root);
                diagram["indexed"] = json!(index.is_some());
                Ok(diagram)
            }
            Request::Compare => {
                let migrations = migration_schema(&project, &root, &mut session.sources)?;
                session.migration_paths = migrations.paths;
                let live = live_from(probe_json(&run(&project, "database-compare", &json!({}))?)?)?;
                let rows: Vec<Value> = schema_compare::compare(&migrations.schema, &live).iter().map(|row| row.json(&root)).collect();
                let tables: Vec<&str> = live.tables.iter().map(|table| table.name.as_str()).collect();
                let compared = json!({"connection":live.connection,"driver":live.driver,"truncated":live.truncated,"tables":tables,"rows":rows});
                session.compared = Some(compared.clone());
                Ok(json!({"live": compared}))
            }
            Request::Rows { table, page, size } => {
                if table.is_empty() || table.len() > 512 || table.chars().any(char::is_control) {
                    return Err(Error::Invalid("Choose a table.".into()));
                }
                let options = json!({"table":table,"page":page.min(100_000),"size":size.clamp(10, 100)});
                let reply = probe_json(&run(&project, "database-rows", &options)?)?;
                Ok(json!({"rows": rows_reply(&reply)?}))
            }
            Request::Open { path, line } => {
                if !session.migration_paths.contains(&path) || !crate::owns_current_path(&project, &path) {
                    return Err(Error::Stale);
                }
                let at = Position {
                    line: line.saturating_sub(1),
                    character: 0,
                };
                host::open_editor(&path, Some(Range::new(at, at)))?;
                Ok(json!({"opened": true}))
            }
        }
    }
}

pub(crate) fn html() -> String {
    let panels = r#"<div class="ui-tabs dbw-tabs" role="tablist" aria-label="Database sections" data-i18n-aria-label="database.sections"><button type="button" role="tab" class="ui-tab" data-dbw-tab="tables" aria-selected="true" data-i18n="database.tabTables">Tables</button><button type="button" role="tab" class="ui-tab" data-dbw-tab="compare" aria-selected="false" data-i18n="database.compareTab">Compare with database</button><button type="button" role="tab" class="ui-tab" data-dbw-tab="schema" aria-selected="false" data-i18n="database.tabSchema">Schema</button><button type="button" role="tab" class="ui-tab" data-dbw-tab="rows" aria-selected="false" data-i18n="database.tabRows">Rows</button></div><div data-dbw-panel="tables"><div class="dbw-toolbar ui-toolbar"><button type="button" id="dbw-table-refresh" class="ui-button"><span data-ui-icon="refresh" aria-hidden="true"></span><span data-i18n="database.refresh">Refresh</span></button><button type="button" id="dbw-expand-all" class="ui-button ui-icon-button" aria-label="Expand all" title="Expand all" data-i18n-aria-label="database.expandAllButton" data-i18n-title="database.expandAllButton"><span data-ui-icon="expand-all" aria-hidden="true"></span></button><button type="button" id="dbw-collapse-all" class="ui-button ui-icon-button" aria-label="Collapse all" title="Collapse all" data-i18n-aria-label="database.collapseAllButton" data-i18n-title="database.collapseAllButton"><span data-ui-icon="collapse-all" aria-hidden="true"></span></button><span class="dbw-grow"></span><span id="dbw-table-count" class="dbw-muted"></span><label class="dbw-search"><span class="ui-visually-hidden" data-i18n="database.filterTables">Filter tables</span><input id="dbw-table-filter" class="ui-input" type="search" placeholder="Filter by table, model or column" data-i18n-placeholder="database.filterPlaceholder"></label></div><div id="dbw-tables" class="dbw-scroll"></div><p id="dbw-unreadable" class="dbw-muted" role="status" hidden></p></div><div data-dbw-panel="compare" hidden><div class="dbw-toolbar ui-toolbar"><button type="button" id="dbw-compare" class="ui-button"><span data-ui-icon="run" aria-hidden="true"></span><span data-i18n="database.compareTab">Compare with database</span></button><label class="dbw-inline"><span data-i18n="database.show">Show</span><select id="dbw-compare-status" class="ui-input"><option value="" data-i18n="database.allDifferences">All differences</option><option value="pending" data-i18n="database.pendingMigrations">Pending migrations</option><option value="only-migrations" data-i18n="database.statusOnlyMigrations">Only in migrations</option><option value="only-database" data-i18n="database.statusOnlyDatabase">Only in the database</option><option value="differs" data-i18n="database.missingColumns">Missing columns or indexes</option></select></label><span class="dbw-grow"></span><span id="dbw-compare-summary" class="dbw-muted"></span></div><p class="dbw-muted" data-i18n="database.compareNote">Boots the application and runs read-only queries on the default connection. Nothing is changed.</p><div id="dbw-compare-result" class="dbw-scroll"></div></div><div data-dbw-panel="schema" hidden><div class="dbw-toolbar ui-toolbar" role="toolbar" aria-label="Diagram" data-i18n-aria-label="database.diagramToolbar"><button type="button" id="dbw-zoom-out" class="ui-button ui-icon-button" aria-label="Zoom out" title="Zoom out" data-i18n-aria-label="database.zoomOut" data-i18n-title="database.zoomOut"><span data-ui-icon="remove" aria-hidden="true"></span></button><button type="button" id="dbw-zoom-in" class="ui-button ui-icon-button" aria-label="Zoom in" title="Zoom in" data-i18n-aria-label="database.zoomIn" data-i18n-title="database.zoomIn"><span data-ui-icon="add" aria-hidden="true"></span></button><button type="button" id="dbw-fit" class="ui-button"><span data-ui-icon="expand-all" aria-hidden="true"></span><span data-i18n="database.fit">Fit</span></button><button type="button" id="dbw-refresh" class="ui-button"><span data-ui-icon="refresh" aria-hidden="true"></span><span data-i18n="database.refresh">Refresh</span></button><span class="dbw-grow"></span><span id="dbw-count" class="dbw-muted"></span><label class="dbw-search"><span class="ui-visually-hidden" data-i18n="database.filterPlaceholder">Filter by table, model or column</span><input id="dbw-diagram-filter" class="ui-input" type="search" placeholder="Filter by table, model or column" data-i18n-placeholder="database.filterPlaceholder"></label></div><div id="dbw-diagram" class="dbw-diagram" tabindex="0" aria-label="Schema diagram. Drag to pan, scroll to zoom." data-i18n-aria-label="database.diagramHelp"></div><p class="dbw-legend dbw-muted"><span data-i18n="database.legendPrimary">PK primary</span><span data-i18n="database.legendForeign">FK foreign key</span><span data-i18n="database.legendUnique">UK unique</span><span data-i18n="database.legendSolid">Solid: foreign key</span><span data-i18n="database.legendDashed">Dashed: model relation</span></p></div><div data-dbw-panel="rows" hidden><div class="dbw-toolbar ui-toolbar"><label class="dbw-inline"><span data-i18n="database.table">Table</span><span class="dbw-combo"><input id="dbw-rows-table" class="ui-input" role="combobox" aria-expanded="false" aria-controls="dbw-rows-tables" aria-autocomplete="list" autocomplete="off" spellcheck="false"><ul id="dbw-rows-tables" class="dbw-suggest" role="listbox" hidden></ul></span></label><label class="dbw-inline"><span data-i18n="database.rowsPerPage">Rows per page</span><select id="dbw-rows-size" class="ui-input"><option>10</option><option selected>25</option><option>50</option><option>100</option></select></label><button type="button" id="dbw-rows-load" class="ui-button"><span data-ui-icon="run" aria-hidden="true"></span><span data-i18n="database.showRows">Show rows</span></button><button type="button" id="dbw-rows-prev" class="ui-button ui-icon-button" aria-label="Previous page" title="Previous page" data-i18n-aria-label="database.previousPage" data-i18n-title="database.previousPage" disabled><span data-ui-icon="chevron-left" aria-hidden="true"></span></button><button type="button" id="dbw-rows-next" class="ui-button ui-icon-button" aria-label="Next page" title="Next page" data-i18n-aria-label="database.nextPage" data-i18n-title="database.nextPage" disabled><span data-ui-icon="chevron-right" aria-hidden="true"></span></button><span class="dbw-grow"></span><span id="dbw-rows-summary" class="dbw-muted"></span></div><p class="dbw-muted" data-i18n="database.rowsNote">Read only. Columns named like secrets or personal data (password, token, email, phone, address, ...) are masked.</p><div id="dbw-rows-result" class="dbw-scroll"></div></div>"#;
    format!(
        "<style>{}</style><section data-database-workspace=\"database\" aria-label=\"Laravel database\" data-i18n-aria-label=\"database.region\">{panels}<p id=\"dbw-status\" class=\"dbw-muted\" role=\"status\"></p><p id=\"dbw-error\" class=\"dbw-error\" role=\"alert\"></p></section>",
        include_str!("../web/database-workspace.css")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;

    const MIGRATION: &str = "<?php\nreturn new class extends Migration {\n    public function up(): void\n    {\n        Schema::create('users', function (Blueprint $table) {\n            $table->id();\n            $table->string('email')->unique();\n        });\n        Schema::create('orders', function (Blueprint $table) {\n            $table->id();\n            $table->foreignId('user_id')->constrained();\n        });\n    }\n};\n";

    fn setup() -> MockHost {
        let host = MockHost::install().with_capabilities(&["project.read", "process:php"]);
        host.add_file("artisan", "<?php // artisan");
        host.add_file("database/migrations/2024_01_01_000000_create_shop.php", MIGRATION);
        host
    }

    fn send(state: &mut State, host: &MockHost, message: Value, reply: &str, calls: &mut Vec<(String, Value)>) -> Result<Value, Error> {
        state.handle_with(&host.invocation(), &message.to_string(), None, &mut |_, mode, options| {
            calls.push((mode.into(), options.clone()));
            Ok(reply.into())
        })
    }

    #[test]
    fn state_draws_the_migration_schema_without_running_php() {
        let host = setup();
        let mut state = State::default();
        let mut calls = vec![];
        let reply = send(&mut state, &host, json!({"op":"state"}), "", &mut calls).unwrap();
        assert!(calls.is_empty());
        assert_eq!(reply["tables"].as_array().unwrap().len(), 2);
        assert_eq!(reply["links"][0]["kind"], "foreign key");
        assert_eq!(reply["tables"][1]["indexes"][0]["kind"], "primary");
        assert_eq!(reply["tables"][1]["foreigns"][0]["on"], "users");
        assert_eq!(reply["live"], Value::Null);
    }

    #[test]
    fn unreadable_migration_flagged_not_fatal() {
        let host = setup();
        host.add_file("database/migrations/2024_02_01_000000_latin1.php", "<?php // stands in for a Latin-1 file");
        let project = host.invocation().project().unwrap();
        let mut cache = BTreeMap::new();
        let found = migration_schema_with(&project, "/project", &mut cache, &mut |path| {
            if path.ends_with("latin1.php") {
                return Err(Error::Invalid(format!("{path}: not valid UTF-8 text")));
            }
            project.read_file(path)
        })
        .unwrap();
        assert_eq!(found.unreadable, ["/project/database/migrations/2024_02_01_000000_latin1.php"]);
        assert_eq!(found.paths.len(), 2);
        let mut state = State::default();
        let mut calls = vec![];
        let reply = send(&mut state, &host, json!({"op":"state"}), "", &mut calls).unwrap();
        assert_eq!(reply["tables"].as_array().unwrap().len(), 2);
        assert_eq!(reply["unreadable"], json!([]));
        assert!(!host.tasks().is_empty());
    }

    #[test]
    fn compare_runs_the_probe_once_and_is_kept_for_the_next_state() {
        let host = setup();
        let mut state = State::default();
        let mut calls = vec![];
        let live = json!({"connection":"sqlite","driver":"sqlite","tables":[{"name":"users","columns":["id","email"],"indexes":[{"columns":["id"],"primary":true},{"columns":["email"],"primary":false}]}],"pending":["2024_03_01_000000_x"]}).to_string();
        let reply = send(&mut state, &host, json!({"op":"compare"}), &live, &mut calls).unwrap();
        assert_eq!(calls, [("database-compare".to_owned(), json!({}))]);
        let statuses: Vec<&str> = reply["live"]["rows"].as_array().unwrap().iter().map(|row| row["status"].as_str().unwrap()).collect();
        assert_eq!(statuses, ["pending", "only-migrations"]);
        assert_eq!(reply["live"]["rows"][1]["table"], "orders");
        let again = send(&mut state, &host, json!({"op":"state"}), "", &mut calls).unwrap();
        assert_eq!(again["live"]["connection"], "sqlite");
        assert_eq!(calls.len(), 1);
    }

    #[test]
    fn probe_errors_and_denied_execution_are_reported_not_swallowed() {
        let host = setup();
        let mut state = State::default();
        let mut calls = vec![];
        let failed = send(&mut state, &host, json!({"op":"compare"}), r#"{"error":"Database inspection failed (PDOException)."}"#, &mut calls);
        assert!(matches!(failed, Err(Error::Invalid(message)) if message.contains("PDOException")));
        let denied = state.handle_with(&host.invocation(), r#"{"op":"compare"}"#, None, &mut |_, _, _| Err(Error::Denied("process:php".into())));
        assert!(matches!(denied, Err(Error::Denied(_))));
    }

    #[test]
    fn rows_are_bounded_validated_and_shape_checked() {
        let host = setup();
        let mut state = State::default();
        let mut calls = vec![];
        let page = json!({"table":"users","columns":["id","email"],"rows":[[1,"•••"]],"total":41,"page":1,"size":10,"masked":["email"]}).to_string();
        let reply = send(&mut state, &host, json!({"op":"rows","table":"users","page":1,"size":5000}), &page, &mut calls).unwrap();
        assert_eq!(calls[0].1, json!({"table":"users","page":1,"size":100}));
        assert_eq!(reply["rows"]["masked"], json!(["email"]));
        assert_eq!(reply["rows"]["total"], 41);
        let ragged = json!({"table":"users","columns":["id","email"],"rows":[[1]]}).to_string();
        assert!(send(&mut state, &host, json!({"op":"rows","table":"users"}), &ragged, &mut calls).is_err());
        let nested = json!({"table":"users","columns":["id"],"rows":[[{"a":1}]]}).to_string();
        assert!(send(&mut state, &host, json!({"op":"rows","table":"users"}), &nested, &mut calls).is_err());
        assert!(send(&mut state, &host, json!({"op":"rows","table":""}), "", &mut calls).is_err());
        assert!(send(&mut state, &host, json!({"op":"rows","table":"users","sql":"drop"}), "", &mut calls).is_err());
        assert_eq!(calls.len(), 3);
    }

    #[test]
    fn open_only_accepts_migration_paths_from_the_last_state() {
        let host = setup();
        let mut state = State::default();
        let mut calls = vec![];
        let path = "/project/database/migrations/2024_01_01_000000_create_shop.php";
        assert!(matches!(send(&mut state, &host, json!({"op":"open","path":path,"line":5}), "", &mut calls), Err(Error::Stale)));
        send(&mut state, &host, json!({"op":"state"}), "", &mut calls).unwrap();
        assert!(matches!(send(&mut state, &host, json!({"op":"open","path":"/project/artisan","line":1}), "", &mut calls), Err(Error::Stale)));
        send(&mut state, &host, json!({"op":"open","path":path,"line":5}), "", &mut calls).unwrap();
    }

    #[test]
    fn unknown_operations_are_rejected() {
        let host = setup();
        let mut state = State::default();
        let mut calls = vec![];
        assert!(send(&mut state, &host, json!({"op":"sql","query":"select 1"}), "", &mut calls).is_err());
        assert!(calls.is_empty());
    }
}
