//! Native browser controls; search only filters already-rendered source facts.
use crate::*;

#[derive(Clone)]
pub(crate) struct Selection {
    root: String,
    key: String,
    path: String,
    line: u32,
}

fn owner(project: &Project) -> Result<String, Error> {
    open_root(project).ok_or(Error::Stale)
}
impl Flyway {
    fn selected_source(&self, project: &Project) -> Option<(String, u32)> {
        let root = owner(project).ok()?;
        let selection = self.browser_selection.as_ref().filter(|selection| selection.root == root)?;
        let (path,line) = (self.target(&root, &selection.key)?, 0);
        (path == selection.path && line == selection.line).then_some((path,line))
    }

    /// Migration rows as real columns with Type/State facets for the packaged view; `None` for any other body.
    pub(crate) fn migration_table(&self, body: &Body) -> Option<Body> {
        let Body::Tree(items) = body else { return None };
        let prefix = if items.first()?.key.starts_with("l:") { "l:" } else { "m:" };
        if !items.iter().all(|item| item.parent.is_none() && item.key.starts_with(prefix)) {
            return None;
        }
        let (columns, facets) = match prefix {
            "l:" => (vec!["Version", "Description", "Type", "Problems"], vec![2]),
            _ => (vec!["Version", "Description", "Type", "State", "Installed"], vec![2, 3]),
        };
        let rows: Vec<_> = items.iter().map(|item| {
            let cells = self.row_cells.get(&item.key).cloned().unwrap_or_else(|| {
                let mut cells = vec![String::new(); columns.len()];
                cells[1] = item.label.clone();
                if prefix == "m:" { cells[3] = item.detail.clone().unwrap_or_default(); }
                cells
            });
            serde_json::json!({"key": item.key, "cells": cells, "icon": item.icon})
        }).collect();
        let data = serde_json::json!({"columns": columns, "facets": facets, "showRowIcons": true, "items": rows});
        Some(Body::Other { kind: "resources".into(), data: data.to_string() })
    }

    pub(crate) fn native_browser_view(&self, project: &Project, mut view: View) -> View {
        let web = ui_kit::web::is_rendering();
        let query = owner(project).ok().and_then(|root| self.browser_filter.as_ref().filter(|(saved, _)| *saved == root).map(|(_, query)| query.as_str())).unwrap_or("");
        if let Body::Tree(items) = &mut view.body {
            let total = items.len();
            if !query.is_empty() {
                let mut keep: BTreeSet<String> = items.iter().filter(|item| format!("{} {}", item.label, item.detail.as_deref().unwrap_or_default()).to_lowercase().contains(query)).map(|item| item.key.clone()).collect();
                // Parents always precede children in the native tree contract.
                for item in items.iter().rev() {
                    if keep.contains(&item.key) {
                        if let Some(parent) = &item.parent { keep.insert(parent.clone()); }
                    }
                }
                items.retain(|item| keep.contains(&item.key));
                for item in items.iter_mut() { item.expanded = true; }
                view.status = Some(format!("{} · Filter: {} · {} of {} rows", view.status.as_deref().unwrap_or_default(), query, items.len(), total));
            }
        }
        if let Body::Tree(items) = &mut view.body {
            if query.is_empty() && !web && !items.is_empty() && items.iter().all(|item| item.parent.is_none() && (item.key.starts_with("m:") || item.key.starts_with("l:"))) {
                let rows = std::mem::take(items);
                // A detail "Problems · <message>" groups under "Problems".
                let group_of = |row: &TreeItem| row.detail.as_deref().unwrap_or("Unknown").split(" · ").next().unwrap_or("Unknown").to_owned();
                let mut groups = Vec::<String>::new();
                for row in &rows { let state = group_of(row); if !groups.contains(&state) { groups.push(state); } }
                for group in groups {
                    let key = format!("state:{group}");
                    let count = rows.iter().filter(|row|group_of(row) == group).count();
                    items.push(TreeItem::new(&key, &group).icon("folder").detail(&format!("{count} {}",if count==1 {"migration"} else {"migrations"})).expandable(true));
                    for row in rows.iter().filter(|row|group_of(row) == group) { items.push(row.clone().parent(&key)); }
                }
                self.browser_expansion.apply(items);
            }
        }
        let selected = self.selected_source(project).filter(|_| match &view.body {
            Body::Tree(items) => self.browser_selection.as_ref().is_some_and(|selection| items.iter().any(|item|item.key == selection.key)),
            _ => false,
        });
        if let Some((path,line)) = &selected {
            let label = path.rsplit('/').next().unwrap_or(path);
            view.status = Some(format!("{} · Selected: {}:{}", view.status.as_deref().unwrap_or_default(), label, line + 1));
        }
        if let Some(table) = self.migration_table(&view.body).filter(|_| web) {
            view.body = table;
        }
        view.button(Button::new("browser-new", "New…").icon("plus"))
            .button(Button::new("browser-open", "Open").icon("file").enabled(selected.is_some()))
            .button(Button::new("browser-filter", "Filter…").icon("filter"))
            .button(Button::new("browser-clear-filter", "Clear").icon("close").enabled(!query.is_empty()))
            .button(Button::new("browser-actions", "More…").icon("more"))
    }
    pub(crate) fn native_browser_event(&mut self, event: &ViewEvent, ctx: &Invocation) -> Result<bool, Error> {
        let relevant = matches!(event, ViewEvent::Select(_))
            || matches!(event, ViewEvent::Button(id) if matches!(id.as_str(), "browser-filter" | "browser-clear-filter" | "browser-actions" | "browser-open" | "browser-new"))
            || matches!(event, ViewEvent::Expand(key) | ViewEvent::Collapse(key) if key.starts_with("state:"));
        if !relevant { return Ok(false); }
        let project = ctx.project().ok_or(Error::Stale)?;
        let root = owner(&project)?;
        if let ViewEvent::Select(key) = event {
            self.browser_selection = self.target(&root, key).map(|path| Selection {root, key:key.clone(), path, line: 0});
            refresh_migrations();
            return Ok(true);
        }
        if matches!(event, ViewEvent::Expand(key) | ViewEvent::Collapse(key) if key.starts_with("state:")) {
            self.browser_expansion.handle(event);
            refresh_migrations();
            return Ok(true);
        }
        let ViewEvent::Button(id) = event else { return Ok(false) };
        if !matches!(id.as_str(), "browser-filter" | "browser-clear-filter" | "browser-actions" | "browser-open" | "browser-new") { return Ok(false) }
        match id.as_str() {
            "browser-open" => {
                let (path,line) = self.selected_source(&project).ok_or(Error::Stale)?;
                project.read_file(&path)?;
                host::open_location(&path,line,0)?;
            }
            "browser-new" => {
                let source = self.selected_source(&project).map(|(path,_)|path);
                if self.browser_selection.as_ref().is_some_and(|selection| selection.root == root) && source.is_none() { return Err(Error::Stale); }
                self.begin_generation_from(&project, source.as_deref())?;
            }

            "browser-filter" => {
                let initial = self.browser_filter.as_ref().filter(|(saved, _)| *saved == root).map(|(_, value)| value.as_str());
                let Some(query) = host::input("Filter migrations by name or status", initial)? else { return Ok(true) };
                if owner(&project)? != root || ctx.project().and_then(|p| owner(&p).ok()).as_deref() != Some(&root) { return Err(Error::Stale) }
                if query.len() > 128 || query.chars().any(char::is_control) { return Err(Error::Invalid("Filter is too long or contains control characters.".into())) }
                self.browser_filter = Some((root, query.trim().to_lowercase()));
            }
            "browser-clear-filter" => { self.browser_filter = Some((root, String::new())); }
            "browser-actions" => {
                let actions = [
                        ("check", "Check migration names"),
            ("copy-next-version", "Copy next migration version"),
            ("validate", "Validate…"),
            ("info", "Run Info…"),
            ("status", "Refresh command output"),
            ("review-comparison", "Review local comparison…"),
            ("migrate", "Migrate database…"),
            ("repair", "Repair schema history…"),
            ("clean", "Clean (drops all objects)…")
                ];
                let items: Vec<_> = actions.iter().map(|(id,label)| openidea_plugin::PickItem::new(id,label)).collect();
                let Some(chosen) = host::pick("More migration actions", &items)? else { return Ok(true) };
                if !actions.iter().any(|(id,_)| *id == chosen) || owner(&project)? != root || ctx.project().and_then(|p| owner(&p).ok()).as_deref() != Some(&root) { return Err(Error::Stale) }
                self.run_command(&chosen, ctx, None)?;
            }
            _ => unreachable!(),
        }
        refresh_migrations();
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    #[test]
    fn native_filter_is_scoped_and_preserves_empty_and_source_row_states() {
        let host=MockHost::install();
        let mut plugin=Flyway::default();
        let project=host.invocation().project().unwrap();
        let view=||View::new(Body::Tree(vec![TreeItem::new("first","V1 create users").detail("Applied"),TreeItem::new("second","V2 add accounts").detail("Pending")]));
        host.answer_next_prompt(Some(&["pending"]));
        plugin.handle("migrations",ViewEvent::Button("browser-filter".into()),&host.invocation()).unwrap();
        let filtered=plugin.native_browser_view(&project,view());
        let Body::Tree(rows)=filtered.body else {panic!("native tree")};
        assert_eq!(rows.len(),1);assert_eq!(rows[0].key,"second");
        assert!(filtered.status.unwrap().contains("1 of 2 rows"));
        host.set_project_root(Some("/other"));
        let Body::Tree(rows)=plugin.native_browser_view(&host.invocation().project().unwrap(),view()).body else {panic!("native tree")};
        assert_eq!(rows.len(),2,"another project must not inherit the filter");
        assert!(host.view_messages().iter().all(|(_,text)| serde_json::from_str::<serde_json::Value>(text).ok().is_some_and(|value| value["channel"] == "ui-kit-refresh" && value.as_object().unwrap().len() <= 2)));
        assert!(host.process_runs().is_empty());
    }
    #[test]
    fn cancelled_native_action_picker_and_oversized_filter_do_no_work() {
        let host=MockHost::install();let mut plugin=Flyway::default();
        host.answer_next_prompt(None);
        plugin.handle("migrations",ViewEvent::Button("browser-actions".into()),&host.invocation()).unwrap();
        host.answer_next_prompt(Some(&[&"a".repeat(129)]));
        assert!(plugin.handle("migrations",ViewEvent::Button("browser-filter".into()),&host.invocation()).is_err());
        assert!(plugin.browser_filter.is_none());assert!(host.process_runs().is_empty());assert!(host.terminals().is_empty());
    }
}

#[cfg(test)]
mod selection_tests {
    use super::*;
    use openidea_plugin::{mock::MockHost, views::{FieldValue, ViewEvent}};
    #[test]
    fn selected_migration_drives_open_and_generation_directory_and_rejects_changed_mapping() {
        let host=MockHost::install();
        host.add_file("flyway.conf","flyway.locations=filesystem:db/migration");
        host.add_file("db/migration/V1__init.sql","select 1;");
        let project=host.invocation().project().unwrap();
        let path="/project/db/migration/V1__init.sql";
        let mut plugin=Flyway::default();
        plugin.targets_root=Some("/project".into());
        plugin.info_scope=Some(execution::info_scope(&project).unwrap());
        plugin.targets.insert("m:0".into(),path.into());
        plugin.handle("migrations",ViewEvent::Select("m:0".into()),&host.invocation()).unwrap();
        plugin.handle("migrations",ViewEvent::Button("browser-open".into()),&host.invocation()).unwrap();
        assert_eq!(host.opened_locations()[0].0,path);
        plugin.handle("migrations",ViewEvent::Button("browser-new".into()),&host.invocation()).unwrap();
        let Body::Form(fields)=plugin.generation_view(&project).unwrap().unwrap().body else {panic!("generation preview")};
        assert!(fields.iter().any(|field|field.key=="generation-directory" && matches!(&field.value,FieldValue::Text(value) if value=="db/migration")));
        plugin.targets.insert("m:0".into(),"/project/db/migration/V2__other.sql".into());
        assert!(plugin.handle("migrations",ViewEvent::Button("browser-open".into()),&host.invocation()).is_err());
        assert!(plugin.handle("migrations",ViewEvent::Button("browser-new".into()),&host.invocation()).is_err());
        assert!(host.process_runs().is_empty());
    }
    #[test]
    fn native_status_groups_expand_collapse_and_compact_icons_are_real_models() {
        let host=MockHost::install();let mut plugin=Flyway::default();
        let project=host.invocation().project().unwrap();
        let tree=||View::new(Body::Tree(vec![TreeItem::new("m:0","V1 init").detail("Applied"),TreeItem::new("m:1","V2 next").detail("Pending")]));
        let shown=plugin.native_browser_view(&project,tree());
        assert!(shown.toolbar.iter().filter(|button|button.id.starts_with("browser-")).all(|button|button.icon.is_some()));
        let Body::Tree(items)=shown.body else {panic!("tree")};
        assert_eq!(items[1].parent.as_deref(),Some("state:Applied"));
        plugin.handle("migrations",ViewEvent::Collapse("state:Applied".into()),&host.invocation()).unwrap();
        let Body::Tree(items)=plugin.native_browser_view(&project,tree()).body else {panic!("tree")};
        assert!(!items[0].expanded);
    }
    #[test]
    fn migration_rows_become_columns_with_type_and_state_facets() {
        let mut plugin=Flyway::default();
        plugin.row_cells.insert("m:0".into(),vec!["1".into(),"init".into(),"Versioned".into(),"Applied".into(),"2026-11-10".into()]);
        let tree=Body::Tree(vec![TreeItem::new("m:0","V1 init").detail("Applied").icon("success"),TreeItem::new("m:1","V2 next").detail("Pending")]);
        let Some(Body::Other{kind,data})=plugin.migration_table(&tree) else {panic!("resources")};
        assert_eq!(kind,"resources");
        let data:serde_json::Value=serde_json::from_str(&data).unwrap();
        assert_eq!(data["columns"],serde_json::json!(["Version","Description","Type","State","Installed"]));
        assert_eq!(data["facets"],serde_json::json!([2,3]));
        assert_eq!(data["items"][0]["cells"],serde_json::json!(["1","init","Versioned","Applied","2026-11-10"]));
        assert_eq!(data["items"][1]["cells"],serde_json::json!(["","V2 next","","Pending",""]));
        assert_eq!(data["items"][0]["key"],"m:0");
        let grouped=Body::Tree(vec![TreeItem::new("state:Applied","Applied"),TreeItem::new("m:0","V1").parent("state:Applied")]);
        assert!(plugin.migration_table(&grouped).is_none());
    }
}
