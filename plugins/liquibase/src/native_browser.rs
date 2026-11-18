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
    selection::project_root(project)
}
impl Liquibase {
    fn selected_source(&self, project: &Project) -> Option<(String, u32)> {
        let root = owner(project).ok()?;
        let selection = self.browser_selection.as_ref().filter(|selection| selection.root == root)?;
        if !(self.targets_root.as_deref() == Some(root.as_str())) { return None; }
        let (path,line) = self.targets.get(&selection.key)?.clone();
        (path == selection.path && line == selection.line
            && host::project_of(&path).and_then(|p| p.root().ok()).as_deref() == Some(root.as_str()))
            .then_some((path,line))
    }

    pub(crate) fn native_browser_view(&self, project: &Project, mut view: View) -> View {
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
        let selected = self.selected_source(project).filter(|_| match &view.body {
            Body::Tree(items) => self.browser_selection.as_ref().is_some_and(|selection| items.iter().any(|item|item.key == selection.key)),
            _ => false,
        });
        if let Some((path,line)) = &selected {
            let label = path.rsplit('/').next().unwrap_or(path);
            view.status = Some(format!("{} · Selected: {}:{}", view.status.as_deref().unwrap_or_default(), label, line + 1));
        }
        view.button(Button::new("browser-new", "New…").icon("plus"))
            .button(Button::new("browser-open", "Open").icon("file").enabled(selected.is_some()))
            .button(Button::new("browser-status", "Pending…").icon("database"))
            .button(Button::new("browser-filter", "Filter…").icon("filter"))
            .button(Button::new("browser-clear-filter", "Clear").icon("close").enabled(!query.is_empty()))
            .button(Button::new("browser-actions", "More…").icon("more"))
    }
    pub(crate) fn native_browser_event(&mut self, event: &ViewEvent, ctx: &Invocation) -> Result<bool, Error> {
        let relevant = matches!(event, ViewEvent::Select(_))
            || matches!(event, ViewEvent::Button(id) if matches!(id.as_str(), "browser-filter" | "browser-clear-filter" | "browser-actions" | "browser-open" | "browser-new" | "browser-status"));
        if !relevant { return Ok(false); }
        let project = ctx.project().ok_or(Error::Stale)?;
        let root = owner(&project)?;
        if let ViewEvent::Select(key) = event {
            self.browser_selection = None;
            if self.targets_root.as_deref() != Some(root.as_str()) { return Err(Error::Stale); }
            if let Some((path,line)) = self.targets.get(key).cloned() {
                self.browser_selection = Some(Selection {root, key:key.clone(), path, line});
            }
            refresh_browser();
            return Ok(true);
        }
        let ViewEvent::Button(id) = event else { return Ok(false) };
        if !matches!(id.as_str(), "browser-filter" | "browser-clear-filter" | "browser-actions" | "browser-open" | "browser-new" | "browser-status") { return Ok(false) }
        match id.as_str() {
            "browser-open" => {
                let (path,line) = self.selected_source(&project).ok_or(Error::Stale)?;
                project.read_file(&path)?;
                host::open_location(&path,line,0)?;
            }
            "browser-new" => {
                if let Some((path,line)) = self.selected_source(&project) {
                    project.read_file(&path)?;
                    host::open_location(&path,line,0)?;
                    host::notify(NotificationLevel::Info,TITLE,"Choose Add Liquibase changeset… or Create Liquibase changelog… from this source editor's code actions. The editor action reviews the exact source and applies one undoable change.");
                } else {
                    host::notify(NotificationLevel::Info,TITLE,"Select a changelog, then choose New… to open its source and add a reviewed changeset through the editor's code actions.");
                }
            }

            "browser-filter" => {
                let initial = self.browser_filter.as_ref().filter(|(saved, _)| *saved == root).map(|(_, value)| value.as_str());
                let Some(query) = host::input("Filter changelogs or changesets", initial)? else { return Ok(true) };
                if owner(&project)? != root || ctx.project().and_then(|p| owner(&p).ok()).as_deref() != Some(&root) { return Err(Error::Stale) }
                if query.len() > 128 || query.chars().any(char::is_control) { return Err(Error::Invalid("Filter is too long or contains control characters.".into())) }
                self.browser_filter = Some((root, query.trim().to_lowercase()));
            }
            "browser-clear-filter" => { self.browser_filter = Some((root, String::new())); }
            "browser-status" => return self.show_pending(ctx).map(|()| true),
            "browser-actions" => {
                let actions = [
                                    ("offline-diff", "Review Offline Snapshot Diff"),
            ("choose-sources", "Choose Sources…"),
            ("clear-sources", "Browse All Changelogs"),
            ("update-confirmed", "Update Database…"),
            ("rollback-count", "Rollback Count…")
                ];
                let items: Vec<_> = actions.iter().map(|(id,label)| openidea_plugin::PickItem::new(id,label)).collect();
                let Some(chosen) = host::pick("More migration actions", &items)? else { return Ok(true) };
                if !actions.iter().any(|(id,_)| *id == chosen) || owner(&project)? != root || ctx.project().and_then(|p| owner(&p).ok()).as_deref() != Some(&root) { return Err(Error::Stale) }
                self.run_command(&chosen, ctx, None)?;
            }
            _ => unreachable!(),
        }
        refresh_browser();
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    #[test]
    fn native_filter_keeps_include_ancestors_and_does_not_change_selection() {
        let host=MockHost::install();let mut plugin=Liquibase::default();
        let view=||View::new(Body::Tree(vec![TreeItem::new("root","master.xml"),TreeItem::new("include","tables.xml").parent("root"),TreeItem::new("change","author: create-users").parent("include"),TreeItem::new("other","other.xml")]));
        host.answer_next_prompt(Some(&["create-users"]));
        plugin.handle("changesets",ViewEvent::Button("browser-filter".into()),&host.invocation()).unwrap();
        let shown=plugin.native_browser_view(&host.invocation().project().unwrap(),view());
        let Body::Tree(items)=shown.body else {panic!("native tree")};
        assert_eq!(items.iter().map(|item|item.key.as_str()).collect::<Vec<_>>(),["root","include","change"]);
        assert!(items.iter().all(|item|item.expanded));
        assert!(host.storage().is_empty(),"filter must not persist source selection");
        host.set_project_root(Some("/other"));
        let Body::Tree(items)=plugin.native_browser_view(&host.invocation().project().unwrap(),view()).body else {panic!("native tree")};
        assert_eq!(items.len(),4);
        assert!(host.requests().is_empty()&&host.process_runs().is_empty()&&host.view_messages().iter().all(|(_,text)| serde_json::from_str::<serde_json::Value>(text).ok().is_some_and(|value| value["channel"] == "ui-kit-refresh" && value.as_object().unwrap().len() <= 2)));
    }
    #[test]
    fn native_action_picker_cancellation_and_oversized_filter_are_safe() {
        let host=MockHost::install();let mut plugin=Liquibase::default();
        host.answer_next_prompt(None);
        plugin.handle("changesets",ViewEvent::Button("browser-actions".into()),&host.invocation()).unwrap();
        host.answer_next_prompt(Some(&[&"a".repeat(129)]));
        assert!(plugin.handle("changesets",ViewEvent::Button("browser-filter".into()),&host.invocation()).is_err());
        assert!(plugin.browser_filter.is_none());assert!(host.terminals().is_empty());assert!(host.process_runs().is_empty());
    }
}

#[cfg(test)]
mod selection_tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    #[test]
    fn selected_changeset_opens_its_source_and_new_never_uses_another_editor() {
        let host=MockHost::install();
        host.add_file("db/master.xml","<databaseChangeLog/>\n");
        host.open_editor("/project/other.xml","xml","<unrelated/>",&[(0,0,0,0)]);
        let mut plugin=Liquibase::default();
        plugin.targets_root=Some("/project".into());
        plugin.targets.insert("f:0/c0".into(),("/project/db/master.xml".into(),0));
        plugin.handle("changesets",ViewEvent::Select("f:0/c0".into()),&host.invocation()).unwrap();
        plugin.handle("changesets",ViewEvent::Button("browser-new".into()),&host.invocation()).unwrap();
        assert_eq!(host.opened_locations()[0].0,"/project/db/master.xml");
        assert!(host.notifications().iter().any(|notification|notification.body.contains("code actions")));
        let shown=plugin.native_browser_view(&host.invocation().project().unwrap(),View::new(Body::Tree(vec![TreeItem::new("f:0/c0","1 (author)")])));
        assert!(shown.status.unwrap().contains("Selected: master.xml:1"));
        assert!(shown.toolbar.iter().any(|button|button.id=="browser-open"&&button.enabled));
        plugin.targets.insert("f:0/c0".into(),("/project/other.xml".into(),0));
        assert!(plugin.handle("changesets",ViewEvent::Button("browser-open".into()),&host.invocation()).is_err());
        assert!(host.process_runs().is_empty()&&host.terminals().is_empty());
    }
}
