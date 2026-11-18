//! The Channels table: one row per channel name, searchable in the packaged view.
use crate::*;
use openidea_plugin::views::Table;

const MAX_ROWS: usize = 5000;
const COLUMNS: [&str; 5] = ["Channel", "Declarations", "Producers", "Consumers", "File"];
const KEY_PREFIX: &str = "channel:";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ChannelRow {
    pub name: String,
    pub declarations: usize,
    pub producers: usize,
    pub consumers: usize,
    pub file: String,
    pub at: Location,
}

#[derive(Default)]
struct Tally<'a> {
    declarations: usize,
    producers: usize,
    consumers: usize,
    declared: Option<(&'a str, &'a Ref)>,
    first: Option<(&'a str, &'a Ref)>,
}

pub(crate) fn rows(index: &Refs<'_>) -> Vec<ChannelRow> {
    let mut tallies: BTreeMap<&str, Tally> = BTreeMap::new();
    let channels = all(index).filter(|(_, r)| matches!(r.role, Role::Declares | Role::Produces | Role::Consumes));
    for (path, r) in channels.filter(|(_, r)| !r.name.is_empty() && !beans::is_expression(&r.name)) {
        let tally = tallies.entry(r.name.as_str()).or_default();
        tally.first.get_or_insert((path, r));
        match r.role {
            Role::Declares => {
                tally.declarations += 1;
                tally.declared.get_or_insert((path, r));
            }
            Role::Produces => tally.producers += 1,
            _ => tally.consumers += 1,
        }
    }
    tallies
        .into_iter()
        .take(MAX_ROWS)
        .filter_map(|(name, tally)| {
            let (path, r) = tally.declared.or(tally.first)?;
            Some(ChannelRow {
                name: name.into(),
                declarations: tally.declarations,
                producers: tally.producers,
                consumers: tally.consumers,
                file: file_name(path).into(),
                at: location((path, r)),
            })
        })
        .collect()
}

fn cells(row: &ChannelRow) -> Vec<String> {
    vec![row.name.clone(), row.declarations.to_string(), row.producers.to_string(), row.consumers.to_string(), row.file.clone()]
}

fn body(rows: &[ChannelRow]) -> Body {
    if ui_kit::web::is_rendering() {
        let items = rows.iter().enumerate().map(|(i, row)| serde_json::json!({"key": format!("{KEY_PREFIX}{i}"), "cells": cells(row)}));
        return Body::Other {
            kind: "resources".into(),
            data: serde_json::json!({"columns": COLUMNS, "facets": [4], "items": items.collect::<Vec<_>>()}).to_string(),
        };
    }
    Body::Table(Table {
        columns: COLUMNS.iter().map(|column| (*column).into()).collect(),
        total_rows: rows.len() as u32,
        first_row: 0,
        rows: rows.iter().map(cells).collect(),
        editable: false,
    })
}

impl Integration {
    pub(crate) fn render_table(&mut self, ctx: &Invocation) -> Result<View, Error> {
        let refresh = Button::new("refresh", "Refresh").icon("refresh");
        let Some(project) = ctx.project() else {
            return Ok(View::message("Open a project to see its message channels.").button(refresh));
        };
        let Some(root) = spring_text::index::scoped_root(&project) else {
            return Ok(View::message("This project's folder path can't be read. Open the project from a local folder.").button(refresh));
        };
        let index = self.indexes.get(&project, COMMAND_BUDGET);
        let note = spring_text::index::coverage_note(index);
        let scanning = index.scanning();
        if scanning {
            crate::refresh(&id(VIEW));
        }
        let rows = rows(&index.view(shared::refs_of));
        if rows.is_empty() {
            let text = spring_text::index::empty_message("No message channels found. Add spring-integration to the root build file, then declare channels as MessageChannel @Bean methods, Java DSL flows or <int:channel> in XML", scanning, note.as_deref());
            return Ok(View::message(&text).button(refresh));
        }
        let status = match note {
            Some(note) => format!("{} channels · {note}", rows.len()),
            None => format!("{} channels", rows.len()),
        };
        let view = View::new(body(&rows)).button(refresh).status(&status);
        self.channel_rows = (root, rows);
        Ok(view)
    }

    pub(crate) fn handle_table(&mut self, event: &ViewEvent, ctx: &Invocation) -> Result<bool, Error> {
        let row = match event {
            ViewEvent::Activate(key) => key.strip_prefix(KEY_PREFIX).and_then(|row| row.parse::<usize>().ok()),
            _ => event.table_activate().map(|row| row as usize),
        };
        let Some(row) = row else {
            return Ok(false);
        };
        let (root, rows) = &self.channel_rows;
        let project_root = ctx.project().and_then(|project| project.root().ok());
        if project_root.as_deref() != Some(root.as_str()) {
            return Err(Error::Stale);
        }
        let at = &rows.get(row).ok_or(Error::Stale)?.at;
        if spring_text::index::owning_root(&at.path).as_deref() != Some(root.as_str()) {
            return Err(Error::Stale);
        }
        host::open_location(&at.path, at.range.start.line, at.range.start.character)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channels_table_rows() {
        let mut index = spring_text::index::Index::new(&beans::GLOBS, crate::shared::record);
        index.update("/p/src/main/resources/a.xml", "<beans xmlns='http://www.springframework.org/schema/beans' xmlns:i='http://www.springframework.org/schema/integration'><i:channel id='orders'/><i:bridge input-channel='orders' output-channel='audit'/><i:filter input-channel='orders' output-channel='audit'/></beans>");
        index.update("/p/src/main/resources/b.xml", "<beans xmlns='http://www.springframework.org/schema/beans' xmlns:i='http://www.springframework.org/schema/integration'><i:bridge input-channel='audit' output-channel='${dynamic}'/></beans>");
        let found = rows(&index.view(shared::refs_of));
        let table: Vec<_> = found.iter().map(|r| (r.name.as_str(), r.declarations, r.producers, r.consumers, r.file.as_str())).collect();
        assert_eq!(table, [("audit", 0, 2, 1, "a.xml"), ("orders", 1, 0, 2, "a.xml")]);
        assert_eq!(found[1].at.path, "/p/src/main/resources/a.xml");
    }
}
