//! Declarative views in place of the original's Swing UI: the structure view
//! (`PropertiesStructureViewModel`, keys grouped by dotted prefix) as a tool
//! window, and the resource bundle editor (`ResourceBundleEditor`, keys ×
//! locales) as a custom editor.

use crate::checks::Settings;
use crate::props::{append_line, escape_key, escape_value, file_name, locale_name, parse, suffix, unescape, whole_lines};
use crate::{Properties, bundle_inventory, project_for, to_edits};
use openidea_plugin::text::position;
use openidea_plugin::views::{Body, Button, Table, TreeItem, View, ViewEvent, Viewport, Views};
use openidea_plugin::{Error, EventKind, Invocation, NotificationLevel, PickItem, TextEditor, host};
use openidea_plugin::providers::Document;
use std::collections::{HashMap, HashSet};
use std::ops::Range as Span;

pub const STRUCTURE: &str = "structure";
pub const BUNDLE: &str = "bundle";
const MAX_BUNDLE_TEXT: usize = 256 << 10;
const MAX_BUNDLE_TOTAL: usize = 1 << 20;
const MAX_BUNDLE_KEYS: usize = 4096;
const MAX_BUNDLE_CELLS: usize = 32768;

/// Native table events contain row indices, not source versions or opaque keys.
/// Bind their meaning to the last source/layout actually returned for this file.
#[derive(Clone)]
pub(crate) struct BundleBinding {
    root: String,
    doc: Document,
    keys: Vec<String>,
    files: Vec<String>,
    selected: Option<usize>,
}

/// One bounded, ephemeral inverse for the last reviewed native-table mutation.
/// This is deliberately independent of the host's native Undo stack.
pub(crate) struct BundleRevert {
    root: String,
    after: Document,
    inverse: Vec<openidea_plugin::TextEdit>,
}
fn inverse_bundle_edit(doc: &Document, edits: &[openidea_plugin::TextEdit]) -> Result<(String,Vec<openidea_plugin::TextEdit>),Error> {
    if doc.text.len()>MAX_BUNDLE_TEXT { return Err(Error::Stale); }
    let mut changes=edits.iter().map(|edit| {
        let start=openidea_plugin::text::offset(&doc.text,edit.range.start).ok_or(Error::Stale)?;
        let end=openidea_plugin::text::offset(&doc.text,edit.range.end).ok_or(Error::Stale)?;
        if start>end { return Err(Error::Stale); }
        Ok((start,end,edit.text.clone()))
    }).collect::<Result<Vec<_>,Error>>()?;
    changes.sort_by_key(|(start,end,_)|(*start,*end));
    if changes.windows(2).any(|pair|pair[0].1>pair[1].0) { return Err(Error::Stale); }
    let mut joined: Vec<(usize,usize,String)>=Vec::new();
    for (start,end,text) in changes {
        if let Some(previous)=joined.last_mut() {
            if previous.1==start { previous.1=end; previous.2.push_str(&text); continue; }
        }
        joined.push((start,end,text));
    }
    let changes=joined;
    let size=changes.iter().try_fold(doc.text.len(),|size,(start,end,text)|size.checked_sub(end-start).and_then(|n|n.checked_add(text.len()))).ok_or(Error::Stale)?;
    if size>MAX_BUNDLE_TEXT { return Err("Edited bundle source would exceed 256 KiB.".into()); }
    let mut after=doc.text.clone();
    for (start,end,text) in changes.iter().rev() { after.replace_range(*start..*end,text); }
    let mut delta=0isize;
    let mut inverse=Vec::new();
    for (start,end,text) in changes {
        let shifted=start.checked_add_signed(delta).ok_or(Error::Stale)?;
        inverse.push(openidea_plugin::TextEdit{range:crate::range_of(&after,&(shifted..shifted+text.len())),text:doc.text[start..end].into()});
        delta+=text.len() as isize-(end-start) as isize;
    }
    Ok((after,inverse))
}
fn revert_review(record: &BundleRevert) -> Result<String,Error> {
    let mut body=format!("Undoes the last edit made from this table, in this file only. Other files and earlier edits stay as they are.\nFile: {}\n",serde_json::to_string(&record.after.path).unwrap_or_default());
    for edit in &record.inverse {
        let start=openidea_plugin::text::offset(&record.after.text,edit.range.start).ok_or(Error::Stale)?;
        let end=openidea_plugin::text::offset(&record.after.text,edit.range.end).ok_or(Error::Stale)?;
        body.push_str(&format!("\nRange: {}:{}–{}:{}\nBefore: {}\nAfter: {}\n",edit.range.start.line+1,edit.range.start.character+1,edit.range.end.line+1,edit.range.end.character+1,serde_json::to_string(&record.after.text[start..end]).unwrap_or_default(),serde_json::to_string(&edit.text).unwrap_or_default()));
    }
    Ok(body)
}

/// Refresh the native structure view after source or bundle changes.
pub(crate) fn refresh_structure() {
    host::refresh_view(STRUCTURE);
}
fn owned_editor(ctx: &Invocation) -> Result<(TextEditor, Document, String), Error> {
    let project = ctx.project().ok_or(Error::Stale)?;
    let root = project.root()?;
    let editor = ctx.editor().ok_or(Error::Stale)?;
    let path = editor.path();
    if root.is_empty() || editor.language() != "properties" || !path.ends_with(".properties")
        || host::project_of(&path).and_then(|p|p.root().ok()).as_deref() != Some(root.as_str()) {
        return Err(Error::Stale);
    }
    let snapshot = editor.document()?;
    let doc = Document { path, language: "properties".into(), version: snapshot.version, text: snapshot.text };
    Ok((editor, doc, root))
}
fn scoped_key(root: &str, doc: &Document, key: &str) -> String {
    format!("doc:{}", serde_json::json!([root,doc.path,doc.version,key]))
}

fn bundle_edit_review(root: &str, doc: &Document, grid: &Grid, event: &ViewEvent, edits: &[openidea_plugin::TextEdit]) -> Result<String, Error> {
    let (operation, keys): (&str, Vec<&str>) = match event {
        ViewEvent::Cell {row,column:0,..} => ("Change a key spelling in this file", vec![grid.keys.get(*row as usize).ok_or(Error::Stale)?]),
        ViewEvent::Cell {row,column:1,..} => ("Set this file's property value", vec![grid.keys.get(*row as usize).ok_or(Error::Stale)?]),
        ViewEvent::DeleteRows(rows) => ("Remove keys from this file", rows.iter().map(|row|grid.keys.get(*row as usize).map(String::as_str).ok_or(Error::Stale)).collect::<Result<_,_>>()?),
        ViewEvent::InsertRow(before) => ("Insert a property before the listed key, or append if none", grid.keys.get(*before as usize).map(String::as_str).into_iter().collect()),
        _ => return Err(Error::Stale),
    };
    let mut body = format!("Project: {}\nFile: {}\nDocument version: {}\nOperation: {operation}\nCurrent key(s): {}\n\nReview every exact edit below. Only this file changes; code references and other locale files are not edited.\n",serde_json::to_string(root).unwrap_or_default(),serde_json::to_string(&doc.path).unwrap_or_default(),doc.version,serde_json::to_string(&keys).unwrap_or_default());
    for edit in edits {
        let start = openidea_plugin::text::offset(&doc.text,edit.range.start).ok_or(Error::Stale)?;
        let end = openidea_plugin::text::offset(&doc.text,edit.range.end).ok_or(Error::Stale)?;
        let before = doc.text.get(start..end).ok_or(Error::Stale)?;
        body.push_str(&format!("\nRange: {}:{}–{}:{}\nBefore: {}\nAfter: {}\n",edit.range.start.line+1,edit.range.start.character+1,edit.range.end.line+1,edit.range.end.character+1,serde_json::to_string(before).unwrap_or_default(),serde_json::to_string(&edit.text).unwrap_or_default()));
    }
    Ok(body)
}
fn checked_key(key: &str, root: &str, doc: &Document) -> Result<String, Error> {
    let raw = key.strip_prefix("doc:").ok_or(Error::Stale)?;
    let value: serde_json::Value = serde_json::from_str(raw).map_err(|_|Error::Stale)?;
    let parts = value.as_array().filter(|p|p.len()==4).ok_or(Error::Stale)?;
    if parts[0].as_str()!=Some(root) || parts[1].as_str()!=Some(doc.path.as_str()) || parts[2].as_u64()!=Some(doc.version) { return Err(Error::Stale); }
    parts[3].as_str().map(str::to_owned).ok_or(Error::Stale)
}


struct Entry {
    /// Earlier properties of the file with the same key.
    occurrence: usize,
    key: String,
    value: String,
}

fn with_parent(item: TreeItem, parent: Option<&str>) -> TreeItem {
    match parent {
        Some(parent) => item.parent(parent),
        None => item,
    }
}

fn leaf(entry: &Entry, label: &str, parent: Option<&str>) -> TreeItem {
    let item = TreeItem::new(&format!("p:{}:{}", entry.occurrence, entry.key), label).icon("symbol-property").detail(&entry.value);
    with_parent(item, parent)
}

/// `PropertiesPrefixGroup`: keys sharing a dotted prefix with at least one
/// other key go under a node for that prefix, recursively.
fn group_by_prefix(entries: &[&Entry], consumed: usize, parent: Option<&str>, collapsed: &HashSet<String>, out: &mut Vec<TreeItem>) {
    let mut buckets: Vec<(&str, Vec<&Entry>)> = Vec::new();
    for &entry in entries {
        let segment = entry.key[consumed..].split('.').next().unwrap_or_default();
        match buckets.iter_mut().find(|(s, _)| *s == segment) {
            Some((_, members)) => members.push(entry),
            None => buckets.push((segment, vec![entry])),
        }
    }

    for (segment, members) in buckets {
        let end = consumed + segment.len();
        let (deeper, here): (Vec<&Entry>, Vec<&Entry>) = members.iter().copied().partition(|e| e.key.len() > end);
        if deeper.len() < 2 {
            out.extend(members.iter().map(|e| leaf(e, &e.key[consumed..], parent)));
            continue;
        }

        let group = format!("g:{}", &deeper[0].key[..end]);
        out.extend(here.iter().map(|e| leaf(e, &e.key[consumed..], parent)));
        out.push(with_parent(TreeItem::new(&group, segment).icon("package").expandable(!collapsed.contains(&group)), parent));
        group_by_prefix(&deeper, end + 1, Some(&group), collapsed, out);
    }
}

/// The other files of a bundle, read once per editor (until a save or Reload).
pub struct BundleFiles {
    pub root: String,
    pub path: String,
    pub others: Vec<(String, String)>,
}

/// The bundle editor's table: a row per key of any locale, a column per file
/// (this one first).
pub struct Grid {
    pub columns: Vec<String>,
    pub files: Vec<String>,
    pub keys: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub missing: usize,
    pub gaps: Vec<usize>,
    exact_keys: bool,
}

pub fn grid(path: &str, text: &str, others: &[(String, String)]) -> Grid {
    let files: Vec<(&str, &str)> = std::iter::once((path, text)).chain(others.iter().map(|(p, t)| (p.as_str(), t.as_str()))).collect();

    let mut keys: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut values: Vec<HashMap<String, String>> = Vec::new();
    let mut exact_keys = true;
    for (_, file_text) in &files {
        let mut file_values = HashMap::new();
        for p in parse(file_text) {
            exact_keys &= p.exact_key(file_text).is_some();
            let key = p.unescaped_key(file_text);
            if seen.insert(key.clone()) {
                keys.push(key.clone());
            }
            file_values.entry(key).or_insert_with(|| unescape(&file_text[p.value.clone()]));
        }
        values.push(file_values);
    }

    let columns = std::iter::once("Key".to_owned())
        .chain(files.iter().enumerate().map(|(i, (file, _))| {
            let locale = locale_name(suffix(file));
            if i == 0 { format!("{locale} (this file)") } else { locale.to_string() }
        }))
        .collect();

    let rows = keys
        .iter()
        .map(|key| std::iter::once(key.clone()).chain(values.iter().map(|v| v.get(key).cloned().unwrap_or_default())).collect())
        .collect();

    let gaps: Vec<usize> = keys.iter().map(|key| values.iter().filter(|v| !v.contains_key(key)).count()).collect();
    let missing = gaps.iter().sum();

    Grid { columns, files: files.iter().map(|(p, _)| (*p).to_owned()).collect(), keys, rows, missing, gaps, exact_keys }
}

impl Grid {
    fn keep_incomplete(mut self) -> Self {
        let keep: Vec<bool> = self.gaps.iter().map(|g| *g > 0).collect();
        let mut flags = keep.iter();
        self.keys.retain(|_| *flags.next().unwrap_or(&false));
        let mut flags = keep.iter();
        self.rows.retain(|_| *flags.next().unwrap_or(&false));
        self.gaps.retain(|g| *g > 0);
        self
    }
}

/// What a table edit does to this file: cells of the key and of this file's
/// column, inserted and deleted rows. Other locales are only shown.
pub fn bundle_edits(text: &str, grid: &Grid, event: &ViewEvent, settings: &Settings) -> Result<Vec<(Span<usize>, String)>, Error> {
    if !grid.exact_keys || parse(text).iter().any(|p| p.exact_key(text).is_none()) {
        return Err("A bundle key cannot be represented losslessly; edit its source spelling directly.".into());
    }
    let props = parse(text);
    let declared = |key: &str| props.iter().filter(|p| p.unescaped_key(text) == key).collect::<Vec<_>>();
    let line = |key: &str, value: &str| format!("{}{}{}", escape_key(key), settings.delimiter_text(), escape_value(value));

    match event {
        ViewEvent::Cell { row, column, value } => {
            let key = grid.keys.get(*row as usize).ok_or("No such row.")?;
            match column {
                0 => {
                    if value.is_empty() {
                        return Err("A key can't be empty.".into());
                    }
                    let found = declared(key);
                    if found.len() > 1 { return Err("Resolve duplicate keys before editing this bundle row.".into()); }
                    if value != key && !declared(value).is_empty() { return Err("This key already exists in the current file.".into()); }
                    if found.is_empty() {
                        return Err(format!("'{key}' isn't in this file.").into());
                    }
                    Ok(found.iter().map(|p| (p.key.clone(), escape_key(value))).collect())
                }
                1 if declared(key).len() > 1 => Err("Resolve duplicate keys before editing this bundle row.".into()),
                1 => Ok(vec![match declared(key).first() {
                    Some(p) if p.sep.is_some() => (p.value.clone(), escape_value(value)),
                    Some(p) => (p.key.end..p.value.end, format!("{}{}", settings.delimiter_text(), escape_value(value))),
                    None => append_line(text, &line(key, value)),
                }]),
                column => {
                    let other = grid.files.get(*column as usize - 1).ok_or("No such column.")?;
                    Err(format!("Open {} to edit its translations.", file_name(other)).into())
                }
            }
        }
        ViewEvent::InsertRow(before) => {
            let mut key = "new.property".to_owned();
            for n in 2.. {
                if !grid.keys.contains(&key) {
                    break;
                }
                key = format!("new.property.{n}");
            }
            let at = grid.keys.get(*before as usize).and_then(|k| declared(k).first().map(|p| whole_lines(text, p).start));
            Ok(vec![match at {
                Some(at) => (at..at, format!("{}\n", line(&key, ""))),
                None => append_line(text, &line(&key, "")),
            }])
        }
        ViewEvent::DeleteRows(rows) => {
            let keys: HashSet<&String> = rows.iter().filter_map(|r| grid.keys.get(*r as usize)).collect();
            Ok(props.iter().filter(|p| keys.contains(&p.unescaped_key(text))).map(|p| (whole_lines(text, p), String::new())).collect())
        }
        _ => Ok(Vec::new()),
    }
}

impl Properties {
    fn bundle_files(&mut self, path: &str) -> Result<&BundleFiles, Error> {
        let project = project_for(path)?;
        let root = project.root()?;
        if root.is_empty() { return Err(Error::Stale); }
        if self.bundle.as_ref().is_none_or(|b| b.path != path || b.root != root) {
            let (siblings,complete) = bundle_inventory(&project,path)?;
            if !complete || siblings.len()>=199 { return Err("Bundle view requires a complete bounded source inventory.".into()); }
            let mut others=Vec::new();
            let mut bytes=0usize;
            for other in siblings {
                if host::project_of(&other).and_then(|p|p.root().ok()).as_deref()!=Some(root.as_str()) { return Err(Error::Stale); }
                let stat=project.stat(&other)?;
                if stat.kind!=openidea_plugin::FileKind::File || stat.size>MAX_BUNDLE_TEXT as u64 { return Err("Bundle locale exceeds its source byte limit.".into()); }
                let text=project.read_file(&other)?;
                bytes=bytes.saturating_add(text.len());
                if text.len()>MAX_BUNDLE_TEXT || bytes>MAX_BUNDLE_TOTAL { return Err("Bundle locales exceed their source byte limit.".into()); }
                others.push((other,text));
            }
            others.sort_by(|a, b| suffix(&a.0).cmp(suffix(&b.0)));
            self.bundle = Some(BundleFiles { root: root.clone(), path: path.to_owned(), others });
        }
        let bundle = self.bundle.as_ref().ok_or_else(|| Error::Invalid("no bundle".into()))?;
        if bundle.others.iter().any(|(other,_)| host::project_of(other).and_then(|p|p.root().ok()).as_deref()!=Some(root.as_str())) { return Err(Error::Stale); }
        Ok(bundle)
    }

    fn bundle_grid(&mut self, path: &str, text: &str) -> Result<Grid, Error> {
        if text.len()>MAX_BUNDLE_TEXT { return Err("Bundle source exceeds its source byte limit.".into()); }
        let others = &self.bundle_files(path)?.others;
        let mut total = 0usize;
        let mut names = HashSet::new();
        if others.len() >= 199 { return Err("Bundle view exceeds 199 files; use a smaller source scope.".into()); }
        for source in std::iter::once(text).chain(others.iter().map(|(_, text)| text.as_str())) {
            total = total.saturating_add(source.len());
            if source.len() > MAX_BUNDLE_TEXT || total > MAX_BUNDLE_TOTAL { return Err("Bundle view exceeds its source byte limit.".into()); }
            for prop in parse(source) {
                let key = prop.exact_key(source).ok_or("A bundle key cannot be represented losslessly; edit its source spelling directly.")?;
                names.insert(key);
                if names.len() > MAX_BUNDLE_KEYS || names.len().saturating_mul(others.len()+2) > MAX_BUNDLE_CELLS {
                    return Err("Bundle view exceeds its bounded key/cell layout; use a smaller source scope.".into());
                }
            }
        }
        let grid = grid(path, text, others);
        if !grid.exact_keys { return Err("A bundle key cannot be represented losslessly; edit its source spelling directly.".into()); }
        Ok(if self.bundle_incomplete_only { grid.keep_incomplete() } else { grid })
    }

    fn render_structure(&mut self, ctx: &Invocation) -> Result<View, Error> {
        let _ = host::subscribe(EventKind::EditorFocused);

        let group = if self.structure_flat { "Group by Prefix" } else { "Show Flat" };
        let sort = if self.structure_sorted { "Keep File Order" } else { "Sort Alphabetically" };
        let grouped = !self.structure_flat;
        let toolbar = |view: View| {
            view.button(Button::new("group", group).icon("package"))
                .button(Button::new("sort", sort).icon("sort"))
                .button(Button::new("expand-all", "Expand All").icon("expand-all").enabled(grouped))
                .button(Button::new("collapse-all", "Collapse All").icon("collapse-all").enabled(grouped))
                .button(Button::new("refresh", "Refresh").icon("refresh"))
        };

        let Ok((editor, doc, root)) = owned_editor(ctx) else {
            return Ok(toolbar(View::message("Open a project-owned .properties file to see its properties.")));
        };
        let text = &doc.text;
        let mut seen: HashMap<String, usize> = HashMap::new();
        let mut entries: Vec<Entry> = parse(&text)
            .iter()
            .map(|p| {
                let key = p.unescaped_key(&text);
                let occurrence = seen.entry(key.clone()).or_default();
                *occurrence += 1;
                Entry {
                    occurrence: *occurrence - 1,
                    key,
                    value: unescape(&text[p.value.clone()]).replace(['\n', '\r'], " ").chars().take(80).collect(),
                }
            })
            .collect();
        if entries.is_empty() {
            return Ok(toolbar(View::message("No properties in this file.")));
        }
        if self.structure_sorted {
            entries.sort_by_key(|e| e.key.to_lowercase());
        }

        let mut items = Vec::new();
        if self.structure_flat {
            items.extend(entries.iter().map(|e| leaf(e, &e.key, None)));
        } else {
            group_by_prefix(&entries.iter().collect::<Vec<_>>(), 0, None, &self.structure_collapsed, &mut items);
        }

        for item in &mut items {
            item.key = scoped_key(&root, &doc, &item.key);
            item.parent = item.parent.as_ref().map(|key|scoped_key(&root,&doc,key));
        }

        let count = match entries.len() {
            1 => "1 property".to_owned(),
            n => format!("{n} properties"),
        };
        let status = format!("{} · {count}", file_name(&editor.path()));
        Ok(toolbar(View::new(Body::Tree(items))).status(&status))
    }

    fn handle_structure(&mut self, event: ViewEvent, ctx: &Invocation) -> Result<(), Error> {
        let event = match event {
            ViewEvent::Activate(key) => { let (_,doc,root)=owned_editor(ctx)?; ViewEvent::Activate(checked_key(&key,&root,&doc)?) },
            ViewEvent::Expand(key) => { let (_,doc,root)=owned_editor(ctx)?; ViewEvent::Expand(checked_key(&key,&root,&doc)?) },
            ViewEvent::Collapse(key) => { let (_,doc,root)=owned_editor(ctx)?; ViewEvent::Collapse(checked_key(&key,&root,&doc)?) },
            event => event,
        };
        match event {
            ViewEvent::Button(id) => {
                match id.as_str() {
                    "group" => self.structure_flat = !self.structure_flat,
                    "sort" => self.structure_sorted = !self.structure_sorted,
                    "expand-all" => self.structure_collapsed.clear(),
                    "collapse-all" => {
                        self.structure_collapsed.clear();
                        if let Body::Tree(items) = self.render_structure(ctx)?.body {
                            let (_,doc,root)=owned_editor(ctx)?;
                            self.structure_collapsed = items.into_iter().filter_map(|i| checked_key(&i.key,&root,&doc).ok()).filter(|k| k.starts_with("g:")).collect();
                        }
                    }
                    _ => {}
                }
                host::refresh_view(STRUCTURE);
            }
            ViewEvent::Expand(key) => {
                self.structure_collapsed.remove(&key);
                host::refresh_view(STRUCTURE);
            }
            ViewEvent::Collapse(key) => {
                self.structure_collapsed.insert(key);
                host::refresh_view(STRUCTURE);
            }
            ViewEvent::Activate(key) => {
                let Some((occurrence, key)) = key.strip_prefix("p:").and_then(|k| k.split_once(':')) else {
                    return Ok(());
                };
                let Ok(occurrence) = occurrence.parse::<usize>() else {
                    return Ok(());
                };
                let (editor,doc,_) = owned_editor(ctx)?;
                let text = doc.text;
                let p = parse(&text)
                    .into_iter()
                    .filter(|p| p.unescaped_key(&text) == key)
                    .nth(occurrence)
                    .ok_or("The file has changed. Refresh the view.")?;
                let at = position(&text, p.key.start);
                host::open_location(&editor.path(), at.line, at.character)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn render_bundle(&mut self, viewport: Option<Viewport>, ctx: &Invocation) -> Result<View, Error> {
        let (editor,doc,root) = owned_editor(ctx)?;
        let path = editor.path();
        if let Some(draft) = self.bundle_draft.as_ref().filter(|draft| draft.root==root && draft.doc.path==path) {
            if draft.changed() && draft.present(&doc) {
                self.bundle_draft=None;
                self.bundle=None;
            } else {
                return Ok(draft.render(&doc,viewport));
            }
        }
        let previous = self.bundle_bindings.remove(&path);
        let grid = self.bundle_grid(&path, &doc.text)?;
        let can_revert=self.bundle_revert.as_ref().is_some_and(|record|record.root==root && record.after==doc);
        if self.bundle_bindings.len() >= 8 { self.bundle_bindings.clear(); }
        let selected = previous.filter(|old| old.root==root && old.doc==doc && old.keys==grid.keys && old.files==grid.files).and_then(|old| old.selected);
        self.bundle_bindings.insert(path.clone(), BundleBinding {root,doc,keys:grid.keys.clone(),files:grid.files.clone(),selected});

        let total = grid.keys.len() as u32;
        let shown = Viewport::range(viewport, total, 200);
        let table = Table {
            columns: grid.columns,
            total_rows: total,
            first_row: shown.start,
            rows: grid.rows[shown.start as usize..shown.end as usize].to_vec(),
            editable: false,
        };

        let filter = if self.bundle_incomplete_only { "Show All Keys" } else { "Show Incomplete Only" };
        let mut status = format!("{total} keys · {} locales · {} missing translations", grid.files.len(), grid.missing);
        if grid.files.len() > 1 {
            status.push_str(" · Select a property to edit this file or open a translation");
        }
        let mut view=View::new(Body::Table(table))
            .button(Button::new("bundle-stage-start", "Edit All Locales").icon("edit"))
            .button(Button::new("reload", "Reload Locales").icon("refresh"))
            .button(Button::new("incomplete", filter).icon("filter"))
            .button(Button::new("bundle-edit-value", "Edit Value…").icon("edit").enabled(selected.is_some()))
            .button(Button::new("bundle-add", "Add Property…").icon("plus"))
            .button(Button::new("bundle-row-more", "More…").icon("more").enabled(selected.is_some()))
            .status(&status);
        if can_revert { view=view.button(Button::new("revert-reviewed", "Undo Last Table Edit…").icon("undo")); }
        Ok(view)
    }

    fn open_locale(&self, grid: &Grid, row: u32, column: u32) -> Result<(), Error> {
        if !grid.exact_keys { return Err(Error::Stale); }
        let key = grid.keys.get(row as usize).ok_or("No such row.")?;
        let other = grid.files.get(column as usize - 1).ok_or("No such column.")?;
        let root = project_for(grid.files.first().ok_or(Error::Stale)?)?.root()?;
        let project = project_for(other)?;
        if root.is_empty() || project.root()? != root { return Err(Error::Stale); }
        let text = project.read_file(other)?;
        let line = parse(&text)
            .iter()
            .find(|p| p.unescaped_key(&text) == *key)
            .map_or_else(|| text.lines().count() as u32, |p| position(&text, p.key.start).line);
        host::open_location(other, line, 0)?;
        refresh_structure();
        Ok(())
    }

    fn handle_bundle(&mut self, event: ViewEvent, ctx: &Invocation) -> Result<(), Error> {
        let stage_button=matches!(&event,ViewEvent::Button(id) if matches!(id.as_str(),"bundle-stage-start"|"bundle-stage-discard"|"bundle-stage-apply"));
        if stage_button || self.bundle_draft.is_some() {
            let (_,doc,root)=owned_editor(ctx)?;
            if matches!(&event,ViewEvent::Button(id) if id=="bundle-stage-start") {
                if self.bundle_draft.is_some() { return Err("Apply or discard the current locale edits first.".into()); }
                self.bundle_serial=self.bundle_serial.checked_add(1).ok_or(Error::Stale)?;
                self.bundle_draft=Some(crate::bundle_draft::Draft::new(&doc,self.bundle_serial)?);
                host::refresh_view(BUNDLE);
                return Ok(());
            }
            if let Some(draft)=self.bundle_draft.as_ref().filter(|draft|draft.root==root&&draft.doc.path==doc.path) {
                if let ViewEvent::Button(id)=&event {
                    match id.as_str() {
                        "bundle-stage-discard" => {
                            if draft.changed() && !crate::bundle_draft::confirm_discard(&draft.doc.path)? {return Ok(());}
                            self.bundle_draft=None; self.bundle=None; self.bundle_bindings.clear();
                            host::refresh_view(BUNDLE);return Ok(());
                        }
                        "bundle-stage-apply" => {
                            draft.current(&doc)?;
                            if !draft.changed() {return Err("No staged translations to apply.".into());}
                            // Do not synchronously execute native intentions here: their
                            // provider callback would re-enter this still-running plugin.
                            host::open_editor(&doc.path,None)?;
                            let file=doc.path.rsplit('/').next().unwrap_or_default();
                            host::notify(NotificationLevel::Info,"Properties",&format!("Press ⌥⏎ in {file} and choose {} to review and save the edits. Several locale files change at once, which only ⌥⏎ can do in one undoable step.",crate::bundle_draft::APPLY_TITLE));
                            return Ok(());
                        }
                        _ => return Err("Apply or discard the locale edits first.".into()),
                    }
                }
                if event.table_select().is_some() || event.table_activate().is_some() {return Ok(());}
                draft.current(&doc)?;
                self.bundle_serial=self.bundle_serial.checked_add(1).ok_or(Error::Stale)?;
                self.bundle_draft.as_mut().ok_or(Error::Stale)?.stage(&event,self.bundle_serial)?;
                host::refresh_view(BUNDLE);return Ok(());
            }
            if stage_button {return Err(Error::Stale);}
        }

        if matches!(&event,ViewEvent::Button(id) if id=="revert-reviewed") {
            let (editor,doc,root)=owned_editor(ctx)?;
            let record=self.bundle_revert.as_ref().ok_or("There is no table edit to undo.")?;
            if record.root!=root || record.after!=doc { return Err(Error::Stale); }
            let review=revert_review(record)?;
            if !ui_kit::prompts::confirm_exact("Undo the last table edit?",&review)? { return Ok(()); }
            let (_,current,current_root)=owned_editor(ctx)?;
            if current!=doc || current_root!=root { return Err(Error::Stale); }
            editor.apply_edits(doc.version,&record.inverse)?;
            self.bundle_revert=None;
            self.bundle_bindings.remove(&doc.path);
            refresh_structure();
            host::refresh_view(BUNDLE);
            return Ok(());
        }
        if matches!(&event, ViewEvent::Button(id) if id == "reload") {
            self.bundle = None;
            self.bundle_bindings.clear();
            host::refresh_view(BUNDLE);
            return Ok(());
        }

        if matches!(&event, ViewEvent::Button(id) if id == "incomplete") {
            self.bundle_incomplete_only = !self.bundle_incomplete_only;
            self.bundle_bindings.clear();
            host::refresh_view(BUNDLE);
            return Ok(());
        }

        let (editor,doc,root) = owned_editor(ctx)?;
        let path = editor.path();
        let binding = self.bundle_bindings.get(&path).ok_or("Bundle rows are not current; refresh the view before editing.")?.clone();
        if binding.root != root || binding.doc != doc || binding.files.iter().any(|path| {
            host::project_of(path).and_then(|project|project.root().ok()).as_deref()!=Some(root.as_str())
        }) { return Err(Error::Stale); }
        let rows = binding.keys.len();
        let valid = match &event {
            ViewEvent::Cell {row,column,..} => (*row as usize) < rows && (*column as usize) <= binding.files.len(),
            ViewEvent::InsertRow(before) => (*before as usize) <= rows,
            ViewEvent::DeleteRows(indices) => indices.len() <= rows && indices.iter().collect::<HashSet<_>>().len()==indices.len() && indices.iter().all(|row|(*row as usize)<rows),
            _ => true,
        };
        if !valid { return Err(Error::Stale); }
        if let Some(row) = event.table_select().or_else(||event.table_activate()) {
            if row as usize >= rows { return Err(Error::Stale); }
            self.bundle_bindings.get_mut(&path).ok_or(Error::Stale)?.selected=Some(row as usize);
            if event.table_activate().is_some() {
                return self.handle_bundle(ViewEvent::Button("bundle-open-translation".into()),ctx);
            }
            host::refresh_view(BUNDLE);
            return Ok(());
        }
        if matches!(&event,ViewEvent::Button(id) if id=="bundle-row-more") {
            let key=binding.selected.and_then(|row|binding.keys.get(row)).ok_or("Select a property first.")?;
            let items=[
                PickItem::new("bundle-edit-key","Rename Key in This File…"),
                PickItem::new("bundle-open-translation","Open Translation…").description("Also: double-click the row"),
                PickItem::new("bundle-remove","Remove from This File…"),
            ];
            let Some(chosen)=host::pick(&format!("Property {key}"),&items)? else {return Ok(())};
            return self.handle_bundle(ViewEvent::Button(chosen),ctx);
        }
        if let ViewEvent::Button(id)=&event {
            if matches!(id.as_str(),"bundle-edit-value"|"bundle-edit-key"|"bundle-open-translation"|"bundle-add"|"bundle-remove") {
                let row=binding.selected;
                let key=row.and_then(|row|binding.keys.get(row));
                let prepared=match id.as_str() {
                    "bundle-edit-value"|"bundle-edit-key" => {
                        let row=row.ok_or("Select a property first.")?;
                        let key=key.ok_or(Error::Stale)?;
                        let initial=if id=="bundle-edit-key" {key.clone()} else {
                            parse(&doc.text).into_iter().find(|p|p.unescaped_key(&doc.text)==*key).map(|p|unescape(&doc.text[p.value])).unwrap_or_default()
                        };
                        let Some(value)=host::input(if id=="bundle-edit-key" {"Rename key in this file"} else {"Edit value in this file"},Some(&initial))? else {return Ok(())};
                        if value.len()>16384 {return Err("Property input exceeds 16 KiB.".into());}
                        ViewEvent::Cell{row:row as u32,column:if id=="bundle-edit-key"{0}else{1},value}
                    }
                    "bundle-remove" => ViewEvent::DeleteRows(vec![row.ok_or("Select a property first.")? as u32]),
                    "bundle-add" => {
                        let Some(key)=host::input("New property key",None)? else {return Ok(())};
                        if rows>=MAX_BUNDLE_KEYS || key.trim().is_empty()||key.len()>512||key.chars().any(char::is_control)||binding.keys.contains(&key) {return Err("Use a new non-empty property key of at most 512 bytes.".into());}
                        let Some(value)=host::input("New property value",None)? else {return Ok(())};
                        if value.len()>16384 {return Err("Property input exceeds 16 KiB.".into());}
                        let (_,current,current_root)=owned_editor(ctx)?;
                        if current!=doc||current_root!=root {return Err(Error::Stale);}
                        // Extend only the trusted, exact source binding for this explicit
                        // prompted operation, never a raw native numeric callback.
                        self.bundle_bindings.get_mut(&path).ok_or(Error::Stale)?.keys.push(key);
                        let result=self.handle_bundle(ViewEvent::Cell{row:rows as u32,column:1,value},ctx);
                        self.bundle_bindings.remove(&path);
                        host::refresh_view(BUNDLE);
                        return result;
                    }
                    "bundle-open-translation" => {
                        let row=row.ok_or("Select a property first.")?;
                        let choices: Vec<_>=binding.files.iter().enumerate().map(|(i,path)|openidea_plugin::PickItem::new(&i.to_string(),&format!("{} — {}",locale_name(suffix(path)),file_name(path)))).collect();
                        let Some(chosen)=host::pick("Open translation source",&choices)? else {return Ok(())};
                        let column=chosen.parse::<usize>().ok().filter(|i|*i<binding.files.len()).ok_or(Error::Stale)?;
                        let (_,current,current_root)=owned_editor(ctx)?;
                        if current!=doc||current_root!=root {return Err(Error::Stale);}
                        let grid=Grid{keys:binding.keys,files:binding.files,columns:Vec::new(),rows:Vec::new(),gaps:Vec::new(),missing:0,exact_keys:true};
                        return self.open_locale(&grid,row as u32,column as u32+1);
                    }
                    _=>unreachable!(),
                };
                let (_,current,current_root)=owned_editor(ctx)?;
                if current!=doc||current_root!=root {return Err(Error::Stale);}
                return self.handle_bundle(prepared,ctx);
            }
        }
        let grid = Grid {keys:binding.keys,files:binding.files,columns:Vec::new(),rows:Vec::new(),gaps:Vec::new(),missing:0,exact_keys:true};

        if let ViewEvent::Cell { column, .. } = &event {
            if *column >= 2 {
                return Err("Translations are read-only in this overview. Use Open Translation to edit its source, or the Set property value across selected locales intention.".into());
            }
        }

        let edits = bundle_edits(&doc.text, &grid, &event, &Settings::load())?;
        if !edits.is_empty() {
            let edits = to_edits(&doc.text, edits);
            let (expected,inverse)=inverse_bundle_edit(&doc,&edits)?;
            let review = bundle_edit_review(&root,&doc,&grid,&event,&edits)?;
            if !ui_kit::prompts::confirm_exact("Review resource bundle edit",&review)? { return Ok(()); }
            let (_,current,current_root) = owned_editor(ctx)?;
            if current!=doc || current_root!=root || editor.path()!=path || grid.files.iter().any(|path|host::project_of(path).and_then(|project|project.root().ok()).as_deref()!=Some(root.as_str())) { return Err(Error::Stale); }
            editor.apply_edits(doc.version, &edits)?;
            self.bundle_revert=None;
            if let Ok((_,after,after_root))=owned_editor(ctx) {
                if after_root==root && after.path==path && after.text==expected && after.version!=doc.version {
                    self.bundle_revert=Some(BundleRevert{root:root.clone(),after,inverse});
                }
            }
            self.bundle_bindings.remove(&path);
            refresh_structure();
        }

        if let ViewEvent::DeleteRows(rows) = &event {
            let others = &self.bundle_files(&path)?.others;
            let elsewhere = rows
                .iter()
                .filter_map(|r| grid.keys.get(*r as usize))
                .any(|key| others.iter().any(|(_, text)| parse(text).iter().any(|p| p.unescaped_key(text) == *key)));
            if elsewhere {
                host::notify(
                    NotificationLevel::Info,
                    "Properties",
                    "Removed from this file only. To delete it everywhere, use Remove property from all locales.",
                );
            }
        }
        Ok(())
    }
}

impl Views for Properties {
    fn render(&mut self, view: &str, viewport: Option<Viewport>, ctx: &Invocation) -> Result<View, Error> {
        (|| -> Result<View, Error> {
            match view {
                STRUCTURE => self.render_structure(ctx),
                BUNDLE => self.render_bundle(viewport, ctx),
                _ => Err(Error::NotFound(view.into())),
            }
        })().map(ui_kit::actions::available)
    }

    fn handle(&mut self, view: &str, event: ViewEvent, ctx: &Invocation) -> Result<(), Error> {
        match view {
            STRUCTURE => self.handle_structure(event, ctx),
            BUNDLE => self.handle_bundle(event, ctx),
            _ => Err(Error::NotFound(view.into())),
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;

    fn reviewed_cell(host: &MockHost, plugin: &mut Properties, value: &str) {
        plugin.render(BUNDLE,None,&host.invocation()).unwrap();
        host.answer_next_prompt(Some(&[]));
        plugin.handle(BUNDLE,ViewEvent::Cell{row:0,column:1,value:value.into()},&host.invocation()).unwrap();
    }
    #[test]
    fn reviewed_bundle_revert_restores_each_mutation_in_one_versioned_apply() {
        for event in [ViewEvent::Cell{row:0,column:0,value:"renamed😀".into()},ViewEvent::Cell{row:0,column:1,value:"Changed😀".into()},ViewEvent::InsertRow(0),ViewEvent::DeleteRows(vec![0,1]),ViewEvent::DeleteRows(vec![0,2])] {
            let host=MockHost::install();let mut plugin=Properties::default();
            let original="# 😀\nhello=Hello\nkeep=Keep\nlast=Last\n";
            host.open_editor("/project/messages.properties","properties",original,&[]);
            host.add_file("/project/messages_fr.properties","hello=Bonjour\n");
            plugin.render(BUNDLE,None,&host.invocation()).unwrap();host.answer_next_prompt(Some(&[]));
            plugin.handle(BUNDLE,event,&host.invocation()).unwrap();
            assert_ne!(host.editor_text(),original);let version=host.editor_version();
            let view=plugin.render(BUNDLE,None,&host.invocation()).unwrap();
            assert!(view.toolbar.iter().any(|b|b.id=="revert-reviewed"));
            host.answer_next_prompt(Some(&[]));
            plugin.handle(BUNDLE,ViewEvent::Button("revert-reviewed".into()),&host.invocation()).unwrap();
            assert_eq!(host.editor_text(),original);assert_eq!(host.editor_version(),version+1);
            assert!(plugin.bundle_revert.is_none());
            assert_eq!(project_for("/project/messages.properties").unwrap().read_file("/project/messages_fr.properties").unwrap(),"hello=Bonjour\n");
        }
    }
    #[test]
    fn reviewed_bundle_revert_cancel_retains_record_and_only_last_edit_is_reverted() {
        let host=MockHost::install();let mut plugin=Properties::default();
        host.open_editor("/project/messages.properties","properties","hello=Hello\n",&[]);
        reviewed_cell(&host,&mut plugin,"First");reviewed_cell(&host,&mut plugin,"Second");
        let version=host.editor_version();host.answer_next_prompt(None);
        plugin.handle(BUNDLE,ViewEvent::Button("revert-reviewed".into()),&host.invocation()).unwrap();
        assert_eq!(host.editor_text(),"hello=Second\n");assert_eq!(host.editor_version(),version);assert!(plugin.bundle_revert.is_some());
        host.answer_next_prompt(Some(&[]));plugin.handle(BUNDLE,ViewEvent::Button("revert-reviewed".into()),&host.invocation()).unwrap();
        assert_eq!(host.editor_text(),"hello=First\n");
        assert!(plugin.handle(BUNDLE,ViewEvent::Button("revert-reviewed".into()),&host.invocation()).is_err());
        assert!(Properties::default().handle(BUNDLE,ViewEvent::Button("revert-reviewed".into()),&host.invocation()).is_err());
    }
    #[test]
    fn reviewed_bundle_revert_rejects_unrelated_text_version_path_and_owner() {
        for change in 0..4 {
            let host=MockHost::install();let mut plugin=Properties::default();
            host.open_editor("/project/messages.properties","properties","hello=Hello\n",&[]);
            reviewed_cell(&host,&mut plugin,"Changed");
            match change {
                0=>host.type_in_editor("hello=Unrelated\n"),
                1=>host.type_in_editor("hello=Changed\n"),
                2=>host.open_editor("/project/other.properties","properties","hello=Changed\n",&[]),
                _=>host.add_project("/project/messages.properties"),
            }
            let text=host.editor_text();let version=host.editor_version();host.answer_next_prompt(Some(&[]));
            assert!(plugin.handle(BUNDLE,ViewEvent::Button("revert-reviewed".into()),&host.invocation()).is_err());
            assert_eq!(host.editor_text(),text);assert_eq!(host.editor_version(),version);
        }
    }
    #[test]
    fn reviewed_bundle_inverse_is_exact_utf16_bounded_and_reviewed_in_full() {
        let doc=Document{path:"/project/messages.properties".into(),language:"properties".into(),version:1,text:"😀=Old\nother=Keep\n".into()};
        let edits=vec![openidea_plugin::TextEdit{range:openidea_plugin::Range::new(openidea_plugin::Position::new(0,3),openidea_plugin::Position::new(0,6)),text:"New😀".into()}];
        let (after,inverse)=inverse_bundle_edit(&doc,&edits).unwrap();
        assert_eq!(after,"😀=New😀\nother=Keep\n");assert_eq!(inverse[0].range.end.character,8);
        let record=BundleRevert{root:"/project".into(),after:Document{text:after,version:2,..doc.clone()},inverse};
        let review=revert_review(&record).unwrap();
        for phrase in ["Undoes the last edit made from this table","/project/messages.properties","Before: \"New😀\"","After: \"Old\""] {assert!(review.contains(phrase));}
        let too_big=Document{text:"x".repeat(MAX_BUNDLE_TEXT+1),..doc.clone()};assert!(inverse_bundle_edit(&too_big,&[]).is_err());
        let mut overlapping=edits.clone();overlapping.push(edits[0].clone());assert!(inverse_bundle_edit(&doc,&overlapping).is_err());
    }
    #[test]
    fn reviewed_bundle_revert_oversized_review_does_not_mutate_or_consume_approval() {
        let host=MockHost::install();let mut plugin=Properties::default();
        let text=format!("hello={}\n","x".repeat(17000));host.open_editor("/project/messages.properties","properties",&text,&[]);
        let (_,doc,root)=owned_editor(&host.invocation()).unwrap();
        let edit=openidea_plugin::TextEdit{range:crate::range_of(&text,&(6..text.len()-1)),text:"Old".into()};
        plugin.bundle_revert=Some(BundleRevert{root,after:doc,inverse:vec![edit]});host.answer_next_prompt(Some(&[]));
        assert!(plugin.handle(BUNDLE,ViewEvent::Button("revert-reviewed".into()),&host.invocation()).is_err());
        assert_eq!(host.editor_text(),text);assert!(plugin.bundle_revert.is_some());
        host.open_editor("/project/messages.properties","properties","hello=New\n",&[]);
        let (_,doc,root)=owned_editor(&host.invocation()).unwrap();
        plugin.bundle_revert=Some(BundleRevert{root,after:doc,inverse:vec![openidea_plugin::TextEdit{range:openidea_plugin::Range::new(openidea_plugin::Position::new(0,6),openidea_plugin::Position::new(0,9)),text:"Old".into()}]});
        plugin.handle(BUNDLE,ViewEvent::Button("revert-reviewed".into()),&host.invocation()).unwrap();assert_eq!(host.editor_text(),"hello=Old\n");
    }
    #[test]
    fn reviewed_bundle_edit_exposes_explicit_revert_control() {
        let host=MockHost::install();let mut plugin=Properties::default();
        host.open_editor("/project/messages.properties","properties","hello=Hello\n",&[]);
        plugin.render(BUNDLE,None,&host.invocation()).unwrap();host.answer_next_prompt(Some(&[]));
        plugin.handle(BUNDLE,ViewEvent::Cell{row:0,column:1,value:"Welcome".into()},&host.invocation()).unwrap();
        let view=plugin.render(BUNDLE,None,&host.invocation()).unwrap();
        assert!(view.toolbar.iter().any(|button|button.id=="revert-reviewed"));
        host.answer_next_prompt(Some(&[]));plugin.handle(BUNDLE,ViewEvent::Button("revert-reviewed".into()),&host.invocation()).unwrap();
        assert_eq!(host.editor_text(),"hello=Hello\n");
    }

    #[test]
    fn native_bundle_rows_cannot_retarget_after_source_changes() {
        let host = MockHost::install();
        host.open_editor("/project/messages.properties", "properties", "hello=Original\n", &[]);
        let mut plugin = Properties::default();
        plugin.render(BUNDLE, None, &host.invocation()).unwrap();
        let changed = "inserted=Keep\nhello=Original\n";
        host.open_editor("/project/messages.properties", "properties", changed, &[]);
        let event = ViewEvent::Cell {row:0,column:1,value:"Wrong row".into()};
        assert!(plugin.handle(BUNDLE, event, &host.invocation()).is_err());
        assert_eq!(host.editor_text(), changed);
        plugin.render(BUNDLE, None, &host.invocation()).unwrap();
        host.answer_next_prompt(Some(&[]));
        plugin.handle(BUNDLE, ViewEvent::Cell {row:1,column:1,value:"Updated".into()}, &host.invocation()).unwrap();
        assert_eq!(host.editor_text(), "inserted=Keep\nhello=Updated\n");
    }

    #[test]
    fn native_bundle_callbacks_require_a_current_render_and_valid_rows() {
        let host = MockHost::install();
        let source = "a=One\nb=Two\n";
        host.open_editor("/project/messages.properties", "properties", source, &[]);
        let mut plugin = Properties::default();
        let change = || ViewEvent::Cell {row:0,column:1,value:"Changed".into()};
        assert!(plugin.handle(BUNDLE, change(), &host.invocation()).is_err());
        plugin.render(BUNDLE, None, &host.invocation()).unwrap();
        for event in [ViewEvent::DeleteRows(vec![0,99]), ViewEvent::DeleteRows(vec![0,0]), ViewEvent::InsertRow(99), ViewEvent::Cell {row:0,column:99,value:"x".into()}] {
            assert!(plugin.handle(BUNDLE, event, &host.invocation()).is_err());
            assert_eq!(host.editor_text(), source);
        }
        host.type_in_editor("");
        assert!(plugin.handle(BUNDLE, change(), &host.invocation()).is_err(), "same bytes/new version is not the issued source");
        plugin.render(BUNDLE, None, &host.invocation()).unwrap();
        plugin.handle(BUNDLE, ViewEvent::Button("incomplete".into()), &host.invocation()).unwrap();
        assert!(plugin.handle(BUNDLE, change(), &host.invocation()).is_err());
        plugin.handle(BUNDLE, ViewEvent::Button("incomplete".into()), &host.invocation()).unwrap();
        plugin.render(BUNDLE, None, &host.invocation()).unwrap();
        host.answer_next_prompt(Some(&[]));
        plugin.handle(BUNDLE, change(), &host.invocation()).unwrap();
        assert!(plugin.handle(BUNDLE, change(), &host.invocation()).is_err(), "mutating callbacks require a fresh model");
        plugin.render(BUNDLE, None, &host.invocation()).unwrap();
        host.set_project_root(Some("/foreign"));
        assert!(plugin.handle(BUNDLE, change(), &host.invocation()).is_err());
    }

    #[test]
    fn native_bundle_preparation_rejects_oversized_layouts_before_rows() {
        for case in 0..4 {
            let host = MockHost::install();
            let source = match case {
                0 => "x".repeat(MAX_BUNDLE_TEXT+1),
                1 => (0..MAX_BUNDLE_KEYS+1).map(|i|format!("key{i}=v\n")).collect(),
                _ => "a=v\n".into(),
            };
            if case==2 {
                for locale in ["fr","de","it","es","pt"] { host.add_file(&format!("/project/messages_{locale}.properties"), &format!("#{}", "x".repeat(MAX_BUNDLE_TEXT-1))); }
            }
            if case==3 { for i in 0..201 { host.add_file(&format!("/project/messages-other-{i:03}.properties"),"x=unrelated\n"); } }
            host.open_editor("/project/messages.properties", "properties", &source, &[]);
            let mut plugin=Properties::default();
            assert!(plugin.render(BUNDLE, None, &host.invocation()).is_err());
            if case>=2 { assert!(plugin.bundle.is_none(),"oversized/incomplete locale reads cannot become an accepted cache"); }
            assert_eq!(host.editor_text(), source);
        }
        let host = MockHost::install();
        let source:String=(0..2049).map(|i|format!("k{i}=v\n")).collect();
        for locale in ["fr","de","it","es","pt","nl","ja","ko","zh","sv","da","no","fi","pl","cs","hu"] { host.add_file(&format!("/project/messages_{locale}.properties"), "a=small\n"); }
        host.open_editor("/project/messages.properties", "properties", &source, &[]);
        assert!(Properties::default().render(BUNDLE, None, &host.invocation()).is_err(), "dense cell bound must precede row allocation");
    }

    #[test]
    fn native_bundle_cells_refuse_duplicate_or_colliding_key_edits() {
        for (source,column,value) in [("a=One\nb=Two\n",0,"b"),("a=One\na=Two\n",1,"Changed"),("a=One\na=Two\n",0,"renamed")] {
            let host = MockHost::install();
            host.open_editor("/project/messages.properties", "properties", source, &[]);
            let mut plugin = Properties::default();
            plugin.render(BUNDLE, None, &host.invocation()).unwrap();
            assert!(plugin.handle(BUNDLE, ViewEvent::Cell {row:0,column,value:value.into()}, &host.invocation()).is_err());
            assert_eq!(host.editor_text(), source);
        }
    }

    #[test]
    fn native_bundle_mutations_have_exact_review_and_cancellation() {
        for event in [ViewEvent::Cell {row:0,column:0,value:"renamed".into()},ViewEvent::Cell {row:0,column:1,value:"Changed".into()},ViewEvent::InsertRow(1),ViewEvent::DeleteRows(vec![0])] {
            let host = MockHost::install();
            let source = "emoji😀=A\nb=B\n";
            host.open_editor("/project/messages.properties", "properties", source, &[]);
            let mut plugin = Properties::default();
            plugin.render(BUNDLE, None, &host.invocation()).unwrap();
            let doc = Document {path:"/project/messages.properties".into(),language:"properties".into(),version:1,text:source.into()};
            let grid = grid(&doc.path,source,&[]);
            let edits = to_edits(source,bundle_edits(source,&grid,&event,&Settings::default()).unwrap());
            let review = bundle_edit_review("/project",&doc,&grid,&event,&edits).unwrap();
            assert!(review.contains("File: \"/project/messages.properties\"") && review.contains("Before:") && review.contains("After:"));
            if matches!(&event,ViewEvent::Cell {column:1,..}) { assert!(review.contains("Range: 1:9–1:10") && review.contains("Before: \"A\"")); }
            host.answer_next_prompt(None);
            plugin.handle(BUNDLE,event.clone(),&host.invocation()).unwrap();
            assert_eq!(host.editor_text(),source);
            assert_eq!(host.editor_version(),1);
            host.answer_next_prompt(Some(&[]));
            plugin.handle(BUNDLE,event,&host.invocation()).unwrap();
            assert_ne!(host.editor_text(),source);
            assert_eq!(host.editor_version(),2);
        }
    }

    #[test]
    fn late_old_numeric_callback_requires_review_of_its_actual_current_key() {
        let host = MockHost::install();
        host.open_editor("/project/messages.properties", "properties", "hello=Original\n", &[]);
        let mut plugin = Properties::default();
        plugin.render(BUNDLE,None,&host.invocation()).unwrap();
        let changed = "inserted=Keep\nhello=Original\n";
        host.open_editor("/project/messages.properties", "properties", changed, &[]);
        plugin.render(BUNDLE,None,&host.invocation()).unwrap();
        // The transport cannot identify that this row index came from the old UI.
        // The actual current target must therefore be shown and reaffirmed.
        let event = ViewEvent::Cell {row:0,column:1,value:"Changed".into()};
        let doc=Document {path:"/project/messages.properties".into(),language:"properties".into(),version:1,text:changed.into()};
        let grid=grid(&doc.path,changed,&[]);
        let edits=to_edits(changed,bundle_edits(changed,&grid,&event,&Settings::default()).unwrap());
        let review=bundle_edit_review("/project",&doc,&grid,&event,&edits).unwrap();
        assert!(review.contains("Current key(s): [\"inserted\"]") && review.contains("Before: \"Keep\""));
        host.answer_next_prompt(None);
        plugin.handle(BUNDLE,event.clone(),&host.invocation()).unwrap();
        assert_eq!(host.editor_text(),changed);
        host.answer_next_prompt(Some(&[]));
        plugin.handle(BUNDLE,event,&host.invocation()).unwrap();
        assert_eq!(host.editor_text(),"inserted=Changed\nhello=Original\n");
    }

    #[test]
    fn native_bundle_review_limit_does_not_consume_an_unshown_approval() {
        let host=MockHost::install();
        host.open_editor("/project/messages.properties","properties","a=One\n",&[]);
        let mut plugin=Properties::default();
        plugin.render(BUNDLE,None,&host.invocation()).unwrap();
        host.answer_next_prompt(Some(&[]));
        assert!(plugin.handle(BUNDLE,ViewEvent::Cell {row:0,column:1,value:"x".repeat(20<<10)},&host.invocation()).is_err());
        assert_eq!(host.editor_text(),"a=One\n");
        plugin.handle(BUNDLE,ViewEvent::Cell {row:0,column:1,value:"Small".into()},&host.invocation()).unwrap();
        assert_eq!(host.editor_text(),"a=Small\n");
    }

    #[test]
    fn grid_mutations_require_lossless_identity_across_locales() {
        let host = MockHost::install();
        let source = "\\uD800=one\n\\uD801=two\n�=replacement\n";
        let invalid = grid("/project/messages.properties", source, &[]);
        assert!(bundle_edits(source, &invalid, &ViewEvent::DeleteRows(vec![0]), &Settings::default()).is_err());
        let normal = "hello=World\n";
        let foreign = grid("/project/messages.properties", normal, &[("/project/messages_fr.properties".into(), source.into())]);
        assert!(bundle_edits(normal, &foreign, &ViewEvent::InsertRow(0), &Settings::default()).is_err());
        let paired = "\\uD83D\\uDE00=Paired\n";
        let valid = grid("/project/messages.properties", paired, &[]);
        assert!(bundle_edits(paired, &valid, &ViewEvent::DeleteRows(vec![0]), &Settings::default()).is_ok());
        host.open_editor("/project/messages.properties", "properties", source, &[]);
        assert!(Properties::default().render(BUNDLE, None, &host.invocation()).is_err());
    }

    #[test]
    fn groups_keys_by_prefix() {
        let host = MockHost::install();
        host.open_editor(
            "/project/m.properties",
            "properties",
            "app.title=T\napp.name=N\napp.menu.file=F\napp.menu.edit=E\nother=O\n",
            &[],
        );

        let view = Properties::default().render(STRUCTURE, None, &host.invocation()).unwrap();
        let Body::Tree(items) = view.body else { panic!("a tree") };
        let (_, doc, root) = owned_editor(&host.invocation()).unwrap();
        let shape: Vec<(String, Option<String>)> = items.iter().map(|i| (i.label.clone(), i.parent.as_ref().map(|key| checked_key(key, &root, &doc).unwrap()))).collect();
        let g = |s: &str| Some(s.to_owned());

        assert_eq!(
            shape,
            [
                ("app".into(), None),
                ("title".into(), g("g:app")),
                ("name".into(), g("g:app")),
                ("menu".into(), g("g:app")),
                ("file".into(), g("g:app.menu")),
                ("edit".into(), g("g:app.menu")),
                ("other".into(), None),
            ]
        );
    }

    #[test]
    fn opens_the_activated_property() {
        let host = MockHost::install();
        host.open_editor("/project/m.properties", "properties", "a=1\nb=2\n", &[]);

        let (_, doc, root) = owned_editor(&host.invocation()).unwrap();
        Properties::default().handle(STRUCTURE, ViewEvent::Activate(scoped_key(&root, &doc, "p:0:b")), &host.invocation()).unwrap();
        host.open_editor("/project/m.properties", "properties", "b=2\na=1\na=3\n", &[]);
        let (_, doc, root) = owned_editor(&host.invocation()).unwrap();
        Properties::default().handle(STRUCTURE, ViewEvent::Activate(scoped_key(&root, &doc, "p:1:a")), &host.invocation()).unwrap();
        assert_eq!(host.opened_locations(), [("/project/m.properties".to_owned(), 1, 0), ("/project/m.properties".to_owned(), 2, 0)]);
    }

    #[test]
    fn collapses_and_expands_prefix_groups() {
        let host = MockHost::install();
        host.open_editor("/project/m.properties", "properties", "app.a=1\napp.b=2\napp.menu.x=3\napp.menu.y=4\n", &[]);
        let mut plugin = Properties::default();
        let ctx = host.invocation();
        let expanded = |plugin: &mut Properties| -> Vec<(String, bool)> {
            let Body::Tree(items) = plugin.render(STRUCTURE, None, &ctx).unwrap().body else { panic!("a tree") };
            let (_, doc, root) = owned_editor(&ctx).unwrap();
            items.into_iter().filter(|i| i.expandable).map(|i| (checked_key(&i.key, &root, &doc).unwrap(), i.expanded)).collect()
        };
        let state = |app, menu| vec![("g:app".to_owned(), app), ("g:app.menu".to_owned(), menu)];
        assert_eq!(expanded(&mut plugin), state(true, true));

        let (_, doc, root) = owned_editor(&ctx).unwrap();
        let menu_key = scoped_key(&root, &doc, "g:app.menu");
        plugin.handle(STRUCTURE, ViewEvent::Collapse(menu_key.clone()), &ctx).unwrap();
        assert_eq!(expanded(&mut plugin), state(true, false));
        plugin.handle(STRUCTURE, ViewEvent::Expand(menu_key), &ctx).unwrap();
        assert_eq!(expanded(&mut plugin), state(true, true));
        plugin.handle(STRUCTURE, ViewEvent::Button("collapse-all".into()), &ctx).unwrap();
        assert_eq!(expanded(&mut plugin), state(false, false));
        plugin.handle(STRUCTURE, ViewEvent::Button("expand-all".into()), &ctx).unwrap();
        assert_eq!(expanded(&mut plugin), state(true, true));
        assert_eq!(host.refreshed_views().len(), 4);
    }

    #[test]
    fn structure_status_and_empty_file() {
        let host = MockHost::install();
        host.open_editor("/project/m.properties", "properties", "a=1\n", &[]);
        let view = Properties::default().render(STRUCTURE, None, &host.invocation()).unwrap();
        assert_eq!(view.status.as_deref(), Some("m.properties · 1 property"));

        host.open_editor("/project/m.properties", "properties", "# nothing yet\n", &[]);
        let view = Properties::default().render(STRUCTURE, None, &host.invocation()).unwrap();
        assert_eq!(view.body, Body::Message("No properties in this file.".into()));
        assert!(!view.toolbar.is_empty());
    }

    #[test]
    fn shows_and_edits_the_bundle() {
        let host = MockHost::install();
        host.add_file("/project/messages_fr.properties", "hello=Bonjour\nextra=En plus\n");
        host.open_editor("/project/messages.properties", "properties", "hello=Hello\nbye=Bye\n", &[]);
        let mut plugin = Properties::default();

        let view = plugin.render(BUNDLE, None, &host.invocation()).unwrap();
        let Body::Table(table) = view.body else { panic!("a table") };
        assert_eq!(table.columns, ["Key", "default (this file)", "fr"]);
        assert_eq!(table.rows[2], ["extra", "", "En plus"]);
        assert_eq!(
            view.status.as_deref(),
            Some("3 keys · 2 locales · 2 missing translations · Select a property to edit this file or open a translation")
        );

        let cell = |row, column, value: &str| ViewEvent::Cell { row, column, value: value.into() };
        let shown = |plugin: &mut Properties, event| {
            plugin.render(BUNDLE, None, &host.invocation()).unwrap();
            if !matches!(&event,ViewEvent::Cell {column,..} if *column>=2) { host.answer_next_prompt(Some(&[])); }
            plugin.handle(BUNDLE, event, &host.invocation()).unwrap();
        };
        shown(&mut plugin, cell(1, 1, "Goodbye"));
        shown(&mut plugin, cell(2, 1, "Extra"));
        shown(&mut plugin, cell(0, 0, "greeting"));
        assert_eq!(host.editor_text(), "greeting=Hello\nbye=Goodbye\nextra=Extra\n");

        plugin.render(BUNDLE, None, &host.invocation()).unwrap();
        assert!(plugin.handle(BUNDLE, cell(2,2,"x"), &host.invocation()).is_err(),"foreign edits must never discard typed text while pretending to save");
        assert!(host.opened_locations().is_empty());
        plugin.handle(BUNDLE,ViewEvent::Other{kind:"table-select".into(),data:r#"{"row":2}"#.into()},&host.invocation()).unwrap();
        host.answer_next_prompt(Some(&["1"]));
        plugin.handle(BUNDLE,ViewEvent::Button("bundle-open-translation".into()),&host.invocation()).unwrap();
        assert_eq!(host.opened_locations(), [("/project/messages_fr.properties".to_owned(), 1, 0)]);

        shown(&mut plugin, ViewEvent::InsertRow(1));
        shown(&mut plugin, ViewEvent::DeleteRows(vec![0, 3]));
        assert_eq!(host.editor_text(), "new.property=\nbye=Goodbye\n");
        assert_eq!(host.notifications().len(), 1, "extra stays in fr");

        shown(&mut plugin, ViewEvent::DeleteRows(vec![1]));
        assert_eq!(host.editor_text(), "new.property=\n");
        assert_eq!(host.notifications().len(), 1, "bye was only in this file");
    }

    #[test]
    fn filters_the_bundle_to_incomplete_keys() {
        let host = MockHost::install();
        host.add_file("/project/messages_fr.properties", "hello=Bonjour\n");
        host.open_editor("/project/messages.properties", "properties", "hello=Hello\nbye=Bye\n", &[]);
        let mut plugin = Properties::default();
        let ctx = host.invocation();

        plugin.handle(BUNDLE, ViewEvent::Button("incomplete".into()), &ctx).unwrap();
        let Body::Table(table) = plugin.render(BUNDLE, None, &ctx).unwrap().body else { panic!("a table") };
        assert_eq!(table.rows, [["bye", "Bye", ""]]);

        host.answer_next_prompt(Some(&[]));
        plugin.handle(BUNDLE, ViewEvent::Cell { row: 0, column: 1, value: "Ciao".into() }, &ctx).unwrap();
        assert_eq!(host.editor_text(), "hello=Hello\nbye=Ciao\n");
    }

    #[test]
    fn deleting_a_row_kept_by_other_locales_says_so() {
        let host = MockHost::install();
        host.add_file("/project/messages_fr.properties", "hello=Bonjour\n");
        host.open_editor("/project/messages.properties", "properties", "hello=Hello\n", &[]);
        let mut plugin = Properties::default();

        plugin.render(BUNDLE, None, &host.invocation()).unwrap();
        host.answer_next_prompt(Some(&[]));
        plugin.handle(BUNDLE, ViewEvent::DeleteRows(vec![0]), &host.invocation()).unwrap();
        assert_eq!(host.editor_text(), "");
        assert_eq!(
            host.notifications()[0].body,
            "Removed from this file only. To delete it everywhere, use Remove property from all locales."
        );
    }

    fn locale_host() -> MockHost {
        let host = MockHost::install().with_capabilities(&["project.read", "editor.read", "editor.write"]);
        host.add_file("/project/messages_fr.properties", "hello=Bonjour\n");
        host.open_editor("/project/messages.properties", "properties", "hello=Hello\nbye=Bye\n", &[]);
        host
    }

    #[test]
    fn selected_row_more_menu_runs_the_picked_row_action() {
        let host=locale_host();let mut plugin=Properties::default();let ctx=host.invocation();
        let view=plugin.render(BUNDLE,None,&ctx).unwrap();
        assert!(view.toolbar.len()<=5,"{:?}",view.toolbar.iter().map(|b|&b.label).collect::<Vec<_>>());
        plugin.handle(BUNDLE,ViewEvent::Other{kind:"table-select".into(),data:r#"{"row":1}"#.into()},&ctx).unwrap();
        let view=plugin.render(BUNDLE,None,&ctx).unwrap();
        assert!(view.toolbar.iter().any(|b|b.id=="bundle-row-more"));
        assert!(!view.toolbar.iter().any(|b|b.id=="bundle-remove"));
        host.answer_next_prompt(Some(&["bundle-remove"]));host.answer_next_prompt(Some(&[]));
        plugin.handle(BUNDLE,ViewEvent::Button("bundle-row-more".into()),&ctx).unwrap();
        assert!(!host.editor_text().contains("bye=Bye"));
    }
    #[test]
    fn apply_locale_edits_button_names_the_alt_enter_action() {
        let host=locale_host();host.add_file("/project/messages.properties","hello=Hello\nbye=Bye\n");
        let mut plugin=Properties::default();let ctx=host.invocation();
        plugin.handle(BUNDLE,ViewEvent::Button("bundle-stage-start".into()),&ctx).unwrap();
        plugin.handle(BUNDLE,ViewEvent::Cell{row:0,column:2,value:"Salut".into()},&ctx).unwrap();
        plugin.handle(BUNDLE,ViewEvent::Button("bundle-stage-apply".into()),&ctx).unwrap();
        let body=host.notifications().last().unwrap().body.clone();
        assert!(body.contains("⌥⏎ in messages.properties") && body.contains(crate::bundle_draft::APPLY_TITLE),"{body}");
    }
    #[test]
    fn readonly_bundle_overview_has_complete_reviewed_current_file_workflow() {
        let host=locale_host();
        let mut plugin=Properties::default();
        let ctx=host.invocation();
        let view=plugin.render(BUNDLE,None,&ctx).unwrap();
        let Body::Table(table)=view.body else {panic!("table")};
        assert!(!table.editable,"overview must never present sibling translations as editable");
        assert!(!view.toolbar.iter().any(|b|b.id=="bundle-edit-value"));
        plugin.handle(BUNDLE,ViewEvent::Other{kind:"table-select".into(),data:r#"{"row":0}"#.into()},&ctx).unwrap();
        let view=plugin.render(BUNDLE,None,&ctx).unwrap();
        assert!(view.toolbar.iter().find(|b|b.id=="bundle-edit-value").unwrap().enabled);
        host.answer_next_prompt(Some(&["Welcome"]));host.answer_next_prompt(Some(&[]));
        plugin.handle(BUNDLE,ViewEvent::Button("bundle-edit-value".into()),&ctx).unwrap();
        assert!(host.editor_text().starts_with("hello=Welcome"));
        assert_eq!(ctx.project().unwrap().read_file("/project/messages_fr.properties").unwrap(),"hello=Bonjour\n");
        plugin.render(BUNDLE,None,&ctx).unwrap();
        host.answer_next_prompt(Some(&["fresh"]));host.answer_next_prompt(Some(&["New value"]));host.answer_next_prompt(Some(&[]));
        plugin.handle(BUNDLE,ViewEvent::Button("bundle-add".into()),&ctx).unwrap();
        assert!(host.editor_text().contains("fresh=New value"));
        assert!(host.process_runs().is_empty());
    }

    #[test]
    fn bundle_control_cancellation_and_stale_source_leave_text_untouched() {
        let host=locale_host();let mut plugin=Properties::default();let ctx=host.invocation();
        plugin.render(BUNDLE,None,&ctx).unwrap();
        plugin.handle(BUNDLE,ViewEvent::Other{kind:"table-select".into(),data:r#"{"row":0}"#.into()},&ctx).unwrap();
        let before=host.editor_text();
        host.answer_next_prompt(None);
        plugin.handle(BUNDLE,ViewEvent::Button("bundle-edit-value".into()),&ctx).unwrap();
        assert_eq!(host.editor_text(),before);
        host.open_editor("/project/messages.properties","properties","hello=external\n",&[]);
        assert!(plugin.handle(BUNDLE,ViewEvent::Button("bundle-remove".into()),&host.invocation()).is_err());
        assert_eq!(host.editor_text(),"hello=external\n");
    }

    #[test]
    fn cancelled_add_revokes_prompt_only_row_identity() {
        let host=locale_host();let mut plugin=Properties::default();let ctx=host.invocation();
        plugin.render(BUNDLE,None,&ctx).unwrap();let before=host.editor_text();
        host.answer_next_prompt(Some(&["fresh"]));host.answer_next_prompt(Some(&["value"]));host.answer_next_prompt(None);
        plugin.handle(BUNDLE,ViewEvent::Button("bundle-add".into()),&ctx).unwrap();
        assert_eq!(host.editor_text(),before);
        assert!(plugin.handle(BUNDLE,ViewEvent::Cell{row:2,column:1,value:"unreviewed".into()},&ctx).is_err());
        assert_eq!(host.editor_text(),before);
    }

    #[test]
    fn editing_another_locale_never_runs_a_process_or_overwrites_it() {
        let host = locale_host();
        let mut plugin = Properties::default();
        plugin.render(BUNDLE, None, &host.invocation()).unwrap();
        assert!(plugin.handle(BUNDLE, ViewEvent::Cell { row: 0, column: 2, value: "Salut".into() }, &host.invocation()).is_err());
        assert!(host.process_runs().is_empty());
        assert!(host.opened_locations().is_empty());
        assert_eq!(host.invocation().project().unwrap().read_file("/project/messages_fr.properties").unwrap(), "hello=Bonjour\n");
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod refresh_tests {
    use super::*;
    use openidea_plugin::{Event, FilesChanged, Plugin, mock::MockHost};
    #[test]
    fn external_focus_save_and_file_batches_refresh_native_view() {
        let host=MockHost::install();host.add_file("/project/m.properties","a=1\n");
        host.open_editor("/project/m.properties","properties","a=1\n",&[]);
        let mut plugin=Properties::default();
        for event in [Event::EditorFocused(Some("/project/m.properties".into())),Event::FileSaved("/project/m.properties".into()),Event::FilesChanged(FilesChanged{created:vec!["/project/m_fr.properties".into()],..Default::default()})] {
            let before=host.refreshed_views().len();plugin.handle_events(&[event],&host.invocation());
            assert!(host.refreshed_views()[before..].contains(&STRUCTURE.to_owned()));
        }
        assert!(host.view_messages().is_empty(),"native views need no web transport");
    }
    #[test]
    fn native_keys_reject_foreign_editor_and_source_version_drift() {
        let host=MockHost::install();host.open_editor("/project/a.properties","properties","same=one\n",&[]);
        let mut plugin=Properties::default();
        let Body::Tree(items)=plugin.render(STRUCTURE,None,&host.invocation()).unwrap().body else {panic!()};
        let key=items[0].key.clone();assert!(key.starts_with("doc:"));
        host.open_editor("/project/b.properties","properties","same=two\n",&[]);
        assert!(plugin.handle(STRUCTURE,ViewEvent::Activate(key.clone()),&host.invocation()).is_err());
        host.open_editor("/project/a.properties","properties","same=one\n",&[]);host.type_in_editor("other=three\n");
        assert!(plugin.handle(STRUCTURE,ViewEvent::Activate(key),&host.invocation()).is_err());
        assert!(host.opened_locations().is_empty());
        let Body::Tree(items)=plugin.render(STRUCTURE,None,&host.invocation()).unwrap().body else {panic!()};
        plugin.handle(STRUCTURE,ViewEvent::Activate(items[0].key.clone()),&host.invocation()).unwrap();
        assert_eq!(host.opened_locations(),[("/project/a.properties".into(),0,0)]);
    }
    #[test]
    fn structure_and_bundle_refuse_mismatched_project_editor_and_unknown_owners() {
        let host=MockHost::install();host.add_project("/foreign");host.open_editor("/foreign/m.properties","properties","secret=foreign\n",&[]);
        let mut plugin=Properties::default();let view=plugin.render(STRUCTURE,None,&host.invocation()).unwrap();assert!(matches!(view.body,Body::Message(_)));
        assert!(plugin.render(BUNDLE,None,&host.invocation()).is_err());
        assert!(plugin.handle(STRUCTURE,ViewEvent::Activate("p:0:secret".into()),&host.invocation()).is_err());
        assert!(plugin.handle(BUNDLE,ViewEvent::Cell{row:0,column:1,value:"changed".into()},&host.invocation()).is_err());
        host.set_project_root(None);assert!(matches!(plugin.render(STRUCTURE,None,&host.invocation()).unwrap().body,Body::Message(_)));
        assert!(host.opened_locations().is_empty());assert_eq!(host.editor_text(),"secret=foreign\n");
    }
    #[test]
    fn native_locale_and_reformat_mutations_invalidate_only_structure_after_success() {
        let host=MockHost::install();host.open_editor("/project/m.properties","properties","a=1\n",&[]);
        let mut plugin=Properties::default();plugin.render(BUNDLE,None,&host.invocation()).unwrap();host.answer_next_prompt(Some(&[]));plugin.handle(BUNDLE,ViewEvent::Cell{row:0,column:1,value:"two".into()},&host.invocation()).unwrap();
        assert_eq!(host.editor_text(),"a=two\n");assert!(host.refreshed_views().contains(&STRUCTURE.to_owned()));
        host.open_editor("/project/m.properties","properties","a   =  one\n",&[]);
        let before=host.refreshed_views().len();
        plugin.run_command("reformat",&host.invocation(),None).unwrap();
        assert!(host.refreshed_views()[before..].contains(&STRUCTURE.to_owned()));
        assert!(host.view_messages().is_empty());
    }
    #[test]
    fn bundle_cache_rechecks_member_project_ownership_before_render_or_open() {
        let host=MockHost::install();host.add_file("/project/m_fr.properties","a=French\n");host.open_editor("/project/m.properties","properties","a=English\n",&[]);
        let mut plugin=Properties::default();plugin.render(BUNDLE,None,&host.invocation()).unwrap();assert_eq!(plugin.bundle.as_ref().unwrap().root,"/project");
        host.add_project("/project/m_fr.properties");
        assert!(plugin.render(BUNDLE,None,&host.invocation()).is_err());
        assert!(plugin.handle(BUNDLE,ViewEvent::Cell{row:0,column:2,value:"wrong".into()},&host.invocation()).is_err());
        assert!(host.opened_locations().is_empty());
    }
}
