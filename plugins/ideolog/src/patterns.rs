//! The custom patterns the user adds from the Patterns view, kept in plugin
//! storage as Ideolog `<LogParsingPattern/>` and `<LogHighlightingPattern/>`
//! elements, one per line, next to the `customPatterns` setting.

use crate::log::{self, Config};
use openidea_plugin::views::{Body, Button, Table, View, ViewEvent};
use openidea_plugin::{Error, NotificationLevel, PickItem, host};

pub const VIEW: &str = "patterns";
const STORAGE_KEY: &str = "patterns";

pub(super) const COLOURS: [(&str, &str, i32); 6] = [
    ("red", "Red (errors)", -39836),
    ("yellow", "Yellow (warnings)", -6329600),
    ("green", "Green (info)", -10316203),
    ("blue", "Blue", 0xFF4080FFu32 as i32),
    ("purple", "Purple", 0xFF9966CCu32 as i32),
    ("grey", "Grey", 0xFF808080u32 as i32),
];

pub(super) const ACTIONS: [(&str, &str); 3] = [
    ("HIGHLIGHT_LINE", "Highlight the whole line"),
    ("HIGHLIGHT_FIELD", "Highlight a field"),
    ("HIGHLIGHT_MATCH", "Highlight the matching text"),
];

pub fn stored() -> String {
    host::storage::get(STORAGE_KEY).ok().flatten().unwrap_or_default()
}

pub fn combined(setting: &str) -> String {
    format!("{setting}\n{}", stored())
}

fn escape(value: &str) -> String {
    value.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;").replace('>', "&gt;")
}

pub(super) fn uuid(seed: &str) -> String {
    let hash = seed.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100000001b3));
    format!("c0570000-0000-4000-8000-{:012x}", hash & 0xffff_ffff_ffff)
}

pub fn format_element(name: &str, pattern: &str, line_pattern: &str, time: &str, severity: &str, category: &str) -> String {
    format!(
        "<LogParsingPattern enabled=\"true\" name=\"{}\" pattern=\"{}\" linePattern=\"{}\" timeId=\"{}\" severityId=\"{}\" categoryId=\"{}\" uuid=\"{}\" />",
        escape(name),
        escape(pattern),
        escape(line_pattern),
        time.trim(),
        severity.trim(),
        category.trim(),
        uuid(&format!("{name}{pattern}")),
    )
}

pub fn highlight_element(pattern: &str, action: &str, argb: i32) -> String {
    format!(
        "<LogHighlightingPattern enabled=\"true\" pattern=\"{}\" captureGroup=\"-1\" action=\"{action}\" fg=\"{argb}\" uuid=\"{}\" />",
        escape(pattern),
        uuid(&format!("{pattern}{action}{argb}")),
    )
}

pub(super) fn entries() -> Vec<String> {
    stored().lines().filter(|l| !l.trim().is_empty()).map(str::to_owned).collect()
}

pub(super) fn save(entries: &[String]) -> Result<(), Error> {
    host::storage::set(STORAGE_KEY, &entries.join("\n"))
}

fn describe(entry: &str) -> Vec<String> {
    let Some((kind, attrs)) = log::elements(entry).into_iter().next() else { return vec!["?".into(), entry.into(), String::new(), "Unknown".into()] };
    let get = |name: &str| log::attr(&attrs, name).unwrap_or_default().to_owned();
    if kind == "LogParsingPattern" {
        return vec!["Format".into(), get("name"), get("pattern"), if get("enabled") == "false" { "No" } else { "Yes" }.into()];
    }
    let how = ACTIONS.iter().find(|(id, _)| *id == get("action")).map_or("", |(_, label)| *label);
    vec!["Highlighting".into(), how.into(), get("pattern"), if get("enabled") == "false" { "No" } else { "Yes" }.into()]
}

pub fn render(editor: &crate::pattern_editor::Editor) -> View {
    if let Some(view) = editor.render() {
        return view;
    }
    let all=entries();
    let visible:Vec<usize>=all.iter().enumerate().filter(|(_,entry)|editor.section.is_none_or(|formats| entry.starts_with(if formats {"<LogParsingPattern"} else {"<LogHighlightingPattern"}))).map(|(n,_)|n).collect();
    let rows:Vec<Vec<String>>=visible.iter().map(|n|describe(&all[*n])).collect();
    let selected=editor.selected(&all).ok().flatten();
    editor.bind_rows(all.clone(),visible);
    let formats=all.iter().filter(|s|s.starts_with("<LogParsingPattern")).count();
    let highlights=all.len()-formats;
    let status = match rows.len() {
        0 => "No patterns yet. Use Ideolog: Highlight Selection, or paste Ideolog XML into Custom log formats and highlighting in Settings.".to_owned(),
        1 => "1 pattern".to_owned(),
        n => format!("{n} patterns"),
    };
    let body = if rows.is_empty() {
        Body::Message(match editor.section {Some(true)=>"No custom log formats yet.",Some(false)=>"No custom highlighting rules yet.",None=>"No custom formats or highlighting patterns yet."}.into())
    } else {
        Body::Table(Table {
            columns: vec!["Kind".into(), "Name or action".into(), "Pattern".into(), "Enabled".into()],
            total_rows: rows.len() as u32,
            first_row: 0,
            rows,
            editable: false,
        })
    };
    View::new(body)
        .button(Button::new("choose-section", &format!("Log Formats ({formats}) / Highlighting ({highlights})…")).icon("list"))
        .button(Button::new("add-format", "Add Format").icon("plus"))
        .button(Button::new("add-highlight", "Add Highlighting").icon("plus"))
        .button(Button::new("edit", "Edit").icon("edit").enabled(selected.is_some()))
        .button(Button::new("toggle-enabled", "Enable / Disable Selected").icon("check").enabled(selected.is_some()))
        .button(Button::new("move-up", "Move Up").icon("arrow-up").enabled(selected.is_some() && entries().len() > 1))
        .button(Button::new("move-down", "Move Down").icon("arrow-down").enabled(selected.is_some() && entries().len() > 1))
        .button(Button::new("remove", "Remove").icon("minus").enabled(selected.is_some()))
        .status(&format!("{} · {status}",match editor.section {Some(true)=>"Log Formats",Some(false)=>"Highlighting",None=>"All Custom Patterns"}))
}

fn add(element: String) -> Result<(), Error> {
    let errors = Config::with_custom(&element).errors;
    if let Some(error) = errors.first() {
        host::notify(NotificationLevel::Warning, "Ideolog", &format!("Pattern not added: {error}"));
        return Ok(());
    }

    let mut all = entries();
    all.push(element);
    save(&all)
}

fn highlight_dialog(pattern: &str) -> Result<Option<String>, Error> {
    let items: Vec<PickItem> =
        ACTIONS.iter().map(|(id, label)| PickItem { key: (*id).into(), label: (*label).into(), description: None }).collect();
    let Some(action) = host::pick("What to highlight", &items)? else { return Ok(None) };
    let items: Vec<PickItem> =
        COLOURS.iter().map(|(id, label, _)| PickItem { key: (*id).into(), label: (*label).into(), description: None }).collect();
    let Some(colour) = host::pick("Colour", &items)? else { return Ok(None) };
    let argb = COLOURS.iter().find(|(id, ..)| *id == colour).map_or(0, |(.., argb)| *argb);
    Ok(Some(highlight_element(pattern, &action, argb)))
}

pub fn highlight_text(text: &str) -> Result<(), Error> {
    match highlight_dialog(&fancy_regex::escape(text))? {
        Some(element) => add(element),
        None => Ok(()),
    }
}

pub fn highlighting_count() -> usize {
    entries().iter().filter(|e| e.starts_with("<LogHighlightingPattern")).count()
}

pub fn remove_highlightings() -> Result<usize, Error> {
    let all = entries();
    let kept: Vec<String> = all.iter().filter(|e| !e.starts_with("<LogHighlightingPattern")).cloned().collect();
    save(&kept)?;
    Ok(all.len() - kept.len())
}

pub(super) fn pick_entry(title: &str, all: &[String]) -> Result<Option<usize>, Error> {
    let items: Vec<PickItem> = all
        .iter()
        .enumerate()
        .map(|(n, e)| {
            let row = describe(e);
            PickItem { key: n.to_string(), label: format!("{}: {}", row[0], row[2]), description: Some(row[1].clone()) }
        })
        .collect();
    let Some(chosen) = host::pick(title, &items)? else { return Ok(None) };
    chosen.parse::<usize>().ok().filter(|at| *at < all.len()).map(Some).ok_or_else(|| Error::Invalid("Unknown pattern selection".into()))
}

fn reorder(down: bool, editor: Option<&crate::pattern_editor::Editor>) -> Result<(), Error> {
    let base = stored();
    let mut all: Vec<String> = base.lines().filter(|line| !line.trim().is_empty()).map(str::to_owned).collect();
    let selected=editor.map(|e|e.selected(&all)).transpose()?.flatten();
    let Some(at) = selected.or(pick_entry_if_unselected(selected,if down { "Move pattern down" } else { "Move pattern up" }, &all)?) else { return Ok(()) };
    if stored() != base { return Err(Error::Stale); }
    if down && at + 1 < all.len() { all.swap(at, at + 1); }
    if !down && at > 0 { all.swap(at, at - 1); }
    save(&all)
}
#[cfg(test)]
fn move_up() -> Result<(), Error> { reorder(false,None) }

fn remove(editor: &crate::pattern_editor::Editor) -> Result<(), Error> {
    let base = stored();
    let all: Vec<String> = base.lines().filter(|line| !line.trim().is_empty()).map(str::to_owned).collect();
    let selected=editor.selected(&all)?;
    let Some(at) = selected.or(pick_entry_if_unselected(selected,"Remove pattern", &all)?) else { return Ok(()) };
    if stored() != base { return Err(Error::Stale); }
    save(&all.iter().enumerate().filter(|(n, _)| *n != at).map(|(_, e)| e.clone()).collect::<Vec<_>>())
}

pub(super) fn pick_entry_if_unselected(selected:Option<usize>,title:&str,all:&[String])->Result<Option<usize>,Error> {
    if selected.is_some() {Ok(None)} else {pick_entry(title,all)}
}

pub fn handle(editor: &mut crate::pattern_editor::Editor, event: ViewEvent) -> Result<(), Error> {
    if editor.handle(&event)? {
        ui_kit::web::refresh_view(VIEW, include_str!("ui_config.json"));
        return Ok(());
    }
    if let Some(row)=event.table_select().or(event.table_activate()) {
        editor.select_row(row as usize)?;
        if event.table_activate().is_some() { editor.handle(&ViewEvent::Button("edit".into()))?; }
        ui_kit::web::refresh_view(VIEW, include_str!("ui_config.json"));return Ok(());
    }
    let ViewEvent::Button(id) = event else { return Ok(()) };
    match id.as_str() {
        "choose-section"=>{
            let choices=[PickItem::new("formats","Log Formats"),PickItem::new("highlights","Highlighting"),PickItem::new("all","All Custom Patterns")];
            let Some(chosen)=host::pick("Ideolog Settings Section",&choices)? else {return Ok(())};
            editor.section=match chosen.as_str(){"formats"=>Some(true),"highlights"=>Some(false),"all"=>None,_=>return Err(Error::Stale)};
            editor.bind_rows(Vec::new(),Vec::new());
        },
        "show-formats"=>{editor.section=Some(true);editor.bind_rows(Vec::new(),Vec::new());},
        "show-highlights"=>{editor.section=Some(false);editor.bind_rows(Vec::new(),Vec::new());},
        "show-all"=>{editor.section=None;editor.bind_rows(Vec::new(),Vec::new());},
        "toggle-enabled"=>{
            let mut all=entries();let at=editor.selected(&all)?.ok_or(Error::Stale)?;
            let entry=all.get(at).ok_or(Error::Stale)?;
            let Some((name,mut attrs))=log::elements(entry).into_iter().next() else {return Err(Error::Stale)};
            let enabled=log::attr(&attrs,"enabled")!=Some("false");
            attrs.retain(|(key,_)|key!="enabled");attrs.push(("enabled".into(),(!enabled).to_string()));
            all[at]=format!("<{name}{} />",attrs.iter().map(|(key,value)|format!(" {key}=\"{}\"",escape(value))).collect::<String>());
            save(&all)?;
        },
        "move-up" => reorder(false,Some(editor))?,
        "move-down" => reorder(true,Some(editor))?,
        "remove" => remove(editor)?,
        _ => return Ok(()),
    }
    ui_kit::web::refresh_view(VIEW, include_str!("ui_config.json"));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;


    #[test]
    fn selected_row_actions_respect_filtered_layout_and_storage_freshness() {
        let host=openidea_plugin::mock::MockHost::install();
        let format=format_element("App",r"^(.*)$","^","0","-1","-1");
        let highlight=highlight_element("ERROR","HIGHLIGHT_LINE",COLOURS[0].2);
        save(&[format.clone(),highlight.clone()]).unwrap();
        let mut editor=crate::pattern_editor::Editor::default();
        host.answer_next_prompt(Some(&["highlights"]));
        handle(&mut editor,ViewEvent::Button("choose-section".into())).unwrap();
        let Body::Table(table)=render(&editor).body else {panic!("table")};
        assert_eq!(table.rows.len(),1);
        handle(&mut editor,ViewEvent::Other{kind:"table-select".into(),data:r#"{"row":0}"#.into()}).unwrap();
        handle(&mut editor,ViewEvent::Button("toggle-enabled".into())).unwrap();
        assert_eq!(entries()[0],format);
        assert!(entries()[1].contains("enabled=\"false\""));
        assert!(handle(&mut editor,ViewEvent::Button("remove".into())).is_err(),"changed storage invalidates previous controls");
        render(&editor);
        handle(&mut editor,ViewEvent::Other{kind:"table-select".into(),data:r#"{"row":0}"#.into()}).unwrap();
        handle(&mut editor,ViewEvent::Button("edit".into())).unwrap();
        assert!(matches!(render(&editor).body,Body::Form(_)));
        assert!(host.process_runs().is_empty());
    }

    #[test]
    fn highlight_colours_map_back_to_their_kind() {
        use openidea_plugin::syntax::Highlight::*;
        let kinds = [Keyword, Tag, String, Function, Property, Comment];
        for ((_, _, argb), kind) in COLOURS.iter().zip(kinds) {
            let config = Config::with_custom(&highlight_element("x", "HIGHLIGHT_LINE", *argb));
            assert!(config.errors.is_empty());
            assert_eq!(log::colour_kind_of(*argb), Some(kind));
        }
    }

    #[test]
    fn editing_replaces_a_pattern_in_place_and_move_up_reorders() {
        let host = openidea_plugin::mock::MockHost::install();
        save(&[highlight_element("one", "HIGHLIGHT_LINE", -39836), highlight_element("two", "HIGHLIGHT_LINE", -39836)]).unwrap();

        let mut editor = crate::pattern_editor::Editor::default();
        host.answer_next_prompt(Some(&["1"]));
        editor.handle(&ViewEvent::Button("edit".into())).unwrap();
        editor.handle(&ViewEvent::Field(editor.control_key("pattern-pattern"), "three".into())).unwrap();
        editor.handle(&ViewEvent::Field(editor.control_key("pattern-action"), "HIGHLIGHT_FIELD".into())).unwrap();
        editor.handle(&ViewEvent::Field(editor.control_key("pattern-fg"), COLOURS[3].2.to_string())).unwrap();
        editor.handle(&ViewEvent::Button(editor.control_key("pattern-save"))).unwrap();
        let all = entries();
        assert_eq!(all.len(), 2);
        assert!(all[1].contains("pattern=\"three\"") && all[1].contains("HIGHLIGHT_FIELD"));
        assert!(all[0].contains("pattern=\"one\""));

        host.answer_next_prompt(Some(&["1"]));
        move_up().unwrap();
        assert!(entries()[0].contains("three"));
    }

    #[test]
    fn native_ordering_preserves_enabled_state_and_cannot_mutate_during_a_draft() {
        let host = openidea_plugin::mock::MockHost::install();
        let first = highlight_element("first", "HIGHLIGHT_LINE", COLOURS[0].2);
        let second = highlight_element("second", "HIGHLIGHT_LINE", COLOURS[1].2).replace("enabled=\"true\"", "enabled=\"false\"");
        save(&[first.clone(), second.clone()]).unwrap();
        let mut editor = crate::pattern_editor::Editor::default();
        let Body::Table(table) = render(&editor).body else { panic!("native table") };
        assert_eq!(table.rows[0].last().unwrap(), "Yes");
        assert_eq!(table.rows[1].last().unwrap(), "No");
        host.answer_next_prompt(Some(&["0"]));
        handle(&mut editor, ViewEvent::Button("move-down".into())).unwrap();
        assert_eq!(entries(), vec![second, first]);
        handle(&mut editor, ViewEvent::Button("add-format".into())).unwrap();
        let stored_before = stored();
        handle(&mut editor, ViewEvent::Button("move-down".into())).unwrap();
        assert_eq!(stored(), stored_before);
        assert!(matches!(render(&editor).body, Body::Form(_)));
        let cancel = editor.control_key("pattern-cancel");
        handle(&mut editor, ViewEvent::Button(cancel)).unwrap();
        assert!(matches!(render(&editor).body, Body::Table(_)));
    }

    #[test]
    fn a_format_element_round_trips_through_the_parser() {
        let element = format_element("Mine", r#"^(\d+) "(\w+)" (.*)$"#, "^\\d", "0", "1", "-1");
        let config = Config::with_custom(&element);
        assert!(config.errors.is_empty(), "{:?}", config.errors);
        let text = "12 \"WARN\" careful\n";
        let format = config.detect(text).unwrap();
        assert_eq!(format.name, "Mine");
        assert_eq!(format.severity_column, Some(1));
    }
}
