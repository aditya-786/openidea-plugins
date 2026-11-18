//! Editable native locale grid stages source changes; FileActions commits atomically.
use crate::{Properties, atomic, views, Settings, parse, to_edits};
use openidea_plugin::{Error, TextEdit};
use openidea_plugin::providers::Document;
use openidea_plugin::file_actions::FileAction;
use openidea_plugin::views::{Body, Button, Table, View, ViewEvent, Viewport};
use std::collections::HashSet;

pub(crate) struct Draft {
    pub root: String,
    pub doc: Document,
    pub serial: u64,
    before: Vec<(String, String)>,
    proposed: Vec<(String, String)>,
}
impl Draft {
    pub fn new(doc: &Document, serial: u64) -> Result<Self, Error> {
        let (root, before) = atomic::snapshot(doc)?;
        for (_, text) in &before {
            let mut keys = HashSet::new();
            if parse(text).iter().any(|p| !keys.insert(p.exact_key(text))) {
                return Err("Resolve duplicate keys before editing the locale grid.".into());
            }
        }
        let draft = Self { root, doc: doc.clone(), serial, proposed: before.clone(), before };
        draft.check_layout()?;
        Ok(draft)
    }
    fn check_layout(&self) -> Result<(), Error> {
        let grid = self.grid();
        if grid.keys.len() > 4096 || grid.keys.len().saturating_mul(grid.files.len()+1) > 32768
            || self.proposed.iter().any(|(_,t)| t.len() > 256 << 10)
            || self.proposed.iter().map(|(_,t)|t.len()).sum::<usize>() > 1 << 20 {
            return Err("The bundle is too large to edit as a grid (over 4096 keys, 256 KiB per file or 1 MiB in total).".into());
        }
        Ok(())
    }
    fn grid(&self) -> views::Grid {
        views::grid(&self.proposed[0].0, &self.proposed[0].1, &self.proposed[1..])
    }
    pub fn changed(&self) -> bool { self.before != self.proposed }
    pub fn current(&self, doc: &Document) -> Result<(), Error> {
        if self.doc != *doc { return Err(Error::Stale); }
        atomic::unchanged(doc, &self.root, &self.before)
    }
    pub fn present(&self, doc: &Document) -> bool {
        let Ok((root, mut files)) = atomic::snapshot(doc) else { return false; };
        let mut proposed = self.proposed.clone(); files.sort(); proposed.sort();
        root == self.root && files == proposed
    }
    pub fn render(&self, doc: &Document, viewport: Option<Viewport>) -> View {
        let grid = self.grid();
        let editable = self.current(doc).is_ok();
        let shown = Viewport::range(viewport, grid.keys.len() as u32, 200);
        let mut status = format!("Editing all locales — {} keys · {} locales. Files change only when you apply the edits.", grid.keys.len(), grid.files.len());
        if !editable { status = "A locale file changed outside this grid, so these edits can't be applied. Discard them and edit again.".into(); }
        View::new(Body::Table(Table { columns: grid.columns, total_rows: grid.keys.len() as u32, first_row: shown.start,
            rows: grid.rows[shown.start as usize..shown.end as usize].to_vec(), editable }))
            .button(Button::new("bundle-stage-apply", "Apply Locale Edits…").icon("check").enabled(editable && self.changed()))
            .button(Button::new("bundle-stage-discard", "Discard Locale Edits…").icon("undo"))
            .status(&status)
    }
    pub fn stage(&mut self, event: &ViewEvent, serial: u64) -> Result<(), Error> {
        let grid = self.grid();
        let mut next = self.proposed.clone();
        let events: Vec<(usize, ViewEvent)> = match event {
            ViewEvent::Cell { row, column, value } => {
                if value.len() > 16384 || value.contains('\0') { return Err("A value can be at most 16 KiB and can't contain a NUL character.".into()); }
                let key = grid.keys.get(*row as usize).ok_or(Error::Stale)?;
                if *column == 0 {
                    if value.is_empty() || value.len() > 512 || value.chars().any(char::is_control) || value != key && grid.keys.contains(value) {
                        return Err("Use a non-empty, unique bundle key of at most 512 bytes.".into());
                    }
                    next.iter().enumerate().filter(|(_,(_,text))|parse(text).iter().any(|p|p.exact_key(text).as_ref()==Some(key)))
                        .map(|(i,_)|(i,event.clone())).collect()
                } else {
                    let target = (*column as usize).checked_sub(1).filter(|i|*i<next.len()).ok_or(Error::Stale)?;
                    vec![(target, ViewEvent::Cell { row: *row, column: 1, value: value.clone() })]
                }
            }
            ViewEvent::DeleteRows(rows) => {
                if rows.is_empty() || rows.len() > grid.keys.len() || rows.iter().collect::<HashSet<_>>().len()!=rows.len()
                    || rows.iter().any(|r|*r as usize>=grid.keys.len()) { return Err(Error::Stale); }
                (0..next.len()).map(|i|(i,event.clone())).collect()
            }
            ViewEvent::InsertRow(before) if *before as usize <= grid.keys.len() => (0..next.len()).map(|i|(i,event.clone())).collect(),
            _ => return Err(Error::Invalid("Use editable cells or native insert/remove rows to stage changes.".into())),
        };
        for (index, event) in events {
            let text = &next[index].1;
            let mut edits = views::bundle_edits(text, &grid, &event, &Settings::load())?;
            edits.sort_by_key(|(span,_)| (span.start,span.end));
            let mut updated = text.clone();
            for (span, replacement) in edits.into_iter().rev() { updated.replace_range(span, &replacement); }
            next[index].1 = updated;
        }
        let previous = std::mem::replace(&mut self.proposed, next);
        if let Err(error) = self.check_layout() { self.proposed=previous; return Err(error); }
        self.serial = serial;
        Ok(())
    }
    fn edits(&self) -> Vec<(String, Vec<TextEdit>)> {
        self.before.iter().zip(&self.proposed).filter_map(|((path,before),(_,after))| {
            if before==after { return None; }
            let mut start=before.bytes().zip(after.bytes()).take_while(|(a,b)|a==b).count();
            while !before.is_char_boundary(start) || !after.is_char_boundary(start) { start-=1; }
            let mut suffix=before[start..].bytes().rev().zip(after[start..].bytes().rev()).take_while(|(a,b)|a==b).count();
            while !before.is_char_boundary(before.len()-suffix) || !after.is_char_boundary(after.len()-suffix) { suffix-=1; }
            Some((path.clone(), to_edits(before, vec![(start..before.len()-suffix,after[start..after.len()-suffix].into())])))
        }).collect()
    }
    pub fn action(&self, doc: &Document) -> Option<FileAction> {
        if !self.changed() || self.current(doc).is_err() { return None; }
        Some(FileAction { title: APPLY_TITLE.into(), kind: Some("refactor.rewrite".into()),
            data: Some(serde_json::json!([self.root,doc.path,doc.version,format!("apply-grid\n{}",self.serial)]).to_string()), ..FileAction::default() })
    }
    pub fn resolve(&self, doc: &Document, serial: u64) -> Result<FileAction, Error> {
        if self.serial!=serial || !self.changed() { return Err(Error::Stale); }
        self.current(doc)?;
        let edits = self.edits();
        let mut review = format!("Project: {}\nOnly the files below change; code that uses the keys is not updated. One Undo reverts all of it.\n",self.root);
        for (path, changes) in &edits {
            let before = &self.before.iter().find(|(p,_)|p==path).ok_or(Error::Stale)?.1;
            review.push_str(&format!("\nFile: {}\n",serde_json::to_string(path).unwrap_or_default()));
            for change in changes {
                let start=openidea_plugin::text::offset(before,change.range.start).ok_or(Error::Stale)?;
                let end=openidea_plugin::text::offset(before,change.range.end).ok_or(Error::Stale)?;
                review.push_str(&format!("Before: {}\nAfter: {}\n",serde_json::to_string(&before[start..end]).unwrap_or_default(),serde_json::to_string(&change.text).unwrap_or_default()));
            }
        }
        if !ui_kit::prompts::confirm_exact("Apply locale edits?",&review)? { return Err(Error::Cancelled); }
        self.current(doc)?;
        // Preserve the draft until subsequent source observation proves its text is present.
        Ok(FileAction { title: "Apply locale edits".into(), edits, ..FileAction::default() })
    }
}
/// Title of the ⌥⏎ action that applies the grid; the Apply button points to it.
pub(crate) const APPLY_TITLE: &str = "Apply Locale Edits…";
pub(crate) fn confirm_discard(path: &str) -> Result<bool, Error> {
    let file = path.rsplit('/').next().unwrap_or(path);
    ui_kit::prompts::confirm_exact("Discard locale edits?", &format!("Throws away the unapplied edits of the {file} bundle. No file changes."))
}
impl Properties {
    pub(crate) fn draft_action(&self, doc: &Document) -> Option<FileAction> { self.bundle_draft.as_ref()?.action(doc) }
    pub(crate) fn discard_grid(&mut self, ctx: &openidea_plugin::Invocation) -> Result<Option<String>,Error> {
        let root=ctx.project().ok_or(Error::Stale)?.root()?;
        let draft=self.bundle_draft.as_ref().filter(|draft|draft.root==root).ok_or("There are no locale edits to discard in this project.")?;
        if !confirm_discard(&draft.doc.path)? {return Ok(None);}
        self.bundle_draft=None;self.bundle=None;self.bundle_bindings.clear();
        openidea_plugin::host::refresh_view(views::BUNDLE);Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{mock::MockHost, Invocation, Range};
    use openidea_plugin::views::Views;
    use openidea_plugin::file_actions::FileActions;
    fn setup() -> (MockHost, Properties) {
        let h=MockHost::install();
        h.open_editor("/project/messages.properties","properties","hello=Hello\nonly=One\n",&[]);
        h.add_file("/project/messages_fr.properties","hello=Bonjour\n");
        let mut p=Properties::default();let ctx=h.invocation();
        p.handle(views::BUNDLE,ViewEvent::Button("bundle-stage-start".into()),&ctx).unwrap();
        (h,p)
    }
    fn action(p:&mut Properties,doc:&Document)->FileAction {
        p.file_actions(doc,Range::default()).into_iter().find(|a|a.title==APPLY_TITLE).expect("staged action")
    }
    fn document(ctx:&Invocation)->Document {
        let editor=ctx.editor().unwrap();let d=editor.document().unwrap();
        Document{path:editor.path(),language:"properties".into(),text:d.text,version:d.version}
    }
    #[test]
    fn two_locale_cells_are_staged_then_one_atomic_reviewed_workspace_action() {
        let (h,mut p)=setup();let ctx=h.invocation();let before=h.editor_text();
        for (column,value) in [(1,"Welcome😀"),(2,"Salut😀")] {
            p.handle(views::BUNDLE,ViewEvent::Cell{row:0,column,value:value.into()},&ctx).unwrap();
        }
        assert_eq!(h.editor_text(),before);
        assert_eq!(ctx.project().unwrap().read_file("/project/messages_fr.properties").unwrap(),"hello=Bonjour\n");
        let view=p.render(views::BUNDLE,None,&ctx).unwrap();
        let Body::Table(table)=view.body else {panic!("editable native table")};
        assert!(table.editable);assert!(view.status.unwrap().contains("Editing all locales"));
        assert_eq!(&table.rows[0][1..], &["Welcome😀","Salut😀"]);
        let doc=document(&ctx);let pending=action(&mut p,&doc);
        h.answer_next_prompt(None);
        assert!(matches!(p.resolve_file_action(&doc,pending.clone()),Err(Error::Cancelled)));
        assert!(p.bundle_draft.as_ref().unwrap().changed());assert_eq!(h.editor_text(),before);
        h.answer_next_prompt(Some(&[]));
        let resolved=p.resolve_file_action(&doc,pending).unwrap();
        assert_eq!(resolved.edits.len(),2);assert!(resolved.operations.is_empty());
        assert!(resolved.edits.iter().any(|(path,_)|path.ends_with("messages_fr.properties")));
        // The resolver only returns a transaction; source remains untouched until host applies it.
        assert_eq!(h.editor_text(),before);assert!(p.bundle_draft.is_some());
        let proposed=p.bundle_draft.as_ref().unwrap().proposed.clone();
        for (path,text) in &proposed {
            if path=="/project/messages.properties" {
                let changes=&resolved.edits.iter().find(|(p,_)|p==path).unwrap().1;
                ctx.editor().unwrap().apply_edits(doc.version,changes).unwrap();
            } else {h.add_file(path,text); }
        }
        let view=p.render(views::BUNDLE,None,&ctx).unwrap();
        assert!(p.bundle_draft.is_none(),"retire draft only after observing exact proposed source");
        assert!(!matches!(view.body,Body::Table(table) if table.editable));
    }
    #[test]
    fn rename_insert_delete_and_missing_translation_update_all_staged_sources_only() {
        let (h,mut p)=setup();let ctx=h.invocation();
        p.handle(views::BUNDLE,ViewEvent::Cell{row:1,column:2,value:"Deux".into()},&ctx).unwrap();
        p.handle(views::BUNDLE,ViewEvent::Cell{row:0,column:0,value:"greeting".into()},&ctx).unwrap();
        p.handle(views::BUNDLE,ViewEvent::InsertRow(2),&ctx).unwrap();
        let draft=p.bundle_draft.as_ref().unwrap();let grid=draft.grid();
        assert!(grid.keys.iter().any(|k|k=="new.property"));
        assert_eq!(&grid.rows[0], &["greeting","Hello","Bonjour"]);
        assert_eq!(&grid.rows[1], &["only","One","Deux"]);
        p.handle(views::BUNDLE,ViewEvent::DeleteRows(vec![0]),&ctx).unwrap();
        assert!(p.bundle_draft.as_ref().unwrap().proposed.iter().all(|(_,t)|!t.contains("greeting")));
        assert_eq!(h.editor_text(),"hello=Hello\nonly=One\n");
    }
    #[test]
    fn changed_sources_and_stale_action_generation_keep_unsaved_grid_without_apply() {
        let (h,mut p)=setup();let ctx=h.invocation();
        p.handle(views::BUNDLE,ViewEvent::Cell{row:0,column:2,value:"Salut".into()},&ctx).unwrap();
        let doc=document(&ctx);let old=action(&mut p,&doc);
        p.handle(views::BUNDLE,ViewEvent::Cell{row:0,column:2,value:"Bienvenue".into()},&ctx).unwrap();
        assert!(p.resolve_file_action(&doc,old).is_err());
        let pending=action(&mut p,&doc);
        h.add_file("/project/messages_fr.properties","hello=external\n");
        assert!(p.resolve_file_action(&doc,pending).is_err());
        let view=p.render(views::BUNDLE,None,&ctx).unwrap();
        assert!(matches!(view.body,Body::Table(t) if !t.editable));assert!(view.status.unwrap().contains("changed outside this grid"));
        assert!(p.bundle_draft.is_some());
        h.answer_next_prompt(None);p.handle(views::BUNDLE,ViewEvent::Button("bundle-stage-discard".into()),&ctx).unwrap();
        assert!(p.bundle_draft.is_some());
        h.answer_next_prompt(Some(&[]));p.handle(views::BUNDLE,ViewEvent::Button("bundle-stage-discard".into()),&ctx).unwrap();
        assert!(p.bundle_draft.is_none());
    }
    #[test]
    fn duplicate_and_foreign_sources_never_enter_editable_stage() {
        let h=MockHost::install();h.open_editor("/project/messages.properties","properties","a=1\na=2\n",&[]);
        assert!(Draft::new(&document(&h.invocation()),1).is_err());
        h.open_editor("/project/messages.properties","properties","a=1\n",&[]);
        let (mut draft,doc)=(Draft::new(&document(&h.invocation()),1).unwrap(),document(&h.invocation()));
        h.add_project("/project/messages.properties");
        assert!(draft.current(&doc).is_err());assert!(draft.action(&doc).is_none());
        assert!(draft.stage(&ViewEvent::Cell{row:99,column:1,value:"x".into()},2).is_err());
    }
}
