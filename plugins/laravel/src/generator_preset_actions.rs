//! Reviewed, atomic project preset CRUD. The workspace submits a complete next
//! config plus explicit template replacements/removals; no direct writes occur.
use crate::{generator_plan, generator_presets};
use openidea_plugin::{
    Error, FileKind, Invocation, Position, Project, Range, TextEdit,
    file_actions::{FileAction, FileOperation},
    host,
    providers::Document,
    text,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
const CONFIG: &str = ".openidea/laravel-generators.json";
const TEMPLATES: &str = ".openidea/laravel-templates/";
const LIMIT: usize = 128 << 10;

#[derive(Clone)]
struct Input {
    config: Value,
    templates: BTreeMap<String, String>,
    remove: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Change {
    path: String,
    before: Option<String>,
    after: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Plan {
    changes: Vec<Change>,
    dependencies: BTreeMap<String, Option<String>>,
}
struct Pending {
    key: String,
    input: Input,
    plan: Plan,
    anchor: String,
    anchor_text: String,
}
#[derive(Default)]
pub(crate) struct State {
    next: u64,
    pending: BTreeMap<String, Pending>,
}

fn owner(ctx: &Invocation) -> Result<(Project, String), Error> {
    let project = ctx.project().ok_or(Error::Invalid(
        "Open a project before editing project generators".into(),
    ))?;
    let root = project.root()?;
    if root.is_empty()
        || root.chars().any(char::is_control)
        || !host::projects()?
            .iter()
            .any(|p| p.root().ok().as_deref() == Some(&root))
    {
        return Err(Error::Stale);
    }
    Ok((project, root))
}
fn owned(root: &str, path: &str) -> bool {
    host::project_of(path)
        .and_then(|p| p.root().ok())
        .as_deref()
        == Some(root)
}
fn read(project: &Project, root: &str, relative: &str) -> Result<Option<String>, Error> {
    let absolute = format!("{}/{relative}", root.trim_end_matches('/'));
    if !generator_plan::relative(relative) || !owned(root, &absolute) {
        return Err(Error::Stale);
    }
    match project.stat(relative) {
        Err(Error::NotFound(_)) => Ok(None),
        Err(error) => Err(error),
        Ok(stat) if stat.kind == FileKind::File => {
            let value = project.read_file(relative)?;
            if value.len() > LIMIT {
                return Err(Error::Invalid("Preset files are limited to 128 KiB".into()));
            }
            Ok(Some(value))
        }
        Ok(_) => Err(Error::Invalid(format!(
            "{relative} must be a regular file, not a directory or symlink"
        ))),
    }
}
fn build(project: &Project, root: &str, input: &Input) -> Result<Plan, Error> {
    let config_text = serde_json::to_string_pretty(&input.config)
        .map_err(|e| Error::Invalid(e.to_string()))?
        + "\n";
    let config = generator_presets::parse(&config_text).map_err(Error::Invalid)?;
    let references = generator_presets::template_names(&config);
    if input.templates.len() > 256
        || input.remove.len() > 256
        || input.remove.iter().collect::<BTreeSet<_>>().len() != input.remove.len()
    {
        return Err(Error::Invalid(
            "Too many or duplicate template changes".into(),
        ));
    }
    if input
        .templates
        .keys()
        .chain(input.remove.iter())
        .any(|name| !generator_plan::relative(name))
        || input
            .remove
            .iter()
            .any(|name| input.templates.contains_key(name) || references.contains(name))
    {
        return Err(Error::Invalid("Templates need relative names; a referenced or replacement template cannot also be deleted".into()));
    }
    let mut dependencies = BTreeMap::new();
    let before = read(project, root, CONFIG)?;
    dependencies.insert(CONFIG.into(), before.clone());
    let mut changes = Vec::new();
    if before.as_deref() != Some(&config_text) {
        changes.push(Change {
            path: CONFIG.into(),
            before,
            after: Some(config_text),
        });
    }
    let mut templates = BTreeMap::new();
    let names: BTreeSet<_> = references
        .iter()
        .chain(input.templates.keys())
        .chain(input.remove.iter())
        .cloned()
        .collect();
    for name in names {
        let relative = format!("{TEMPLATES}{name}");
        let before = read(project, root, &relative)?;
        dependencies.insert(relative.clone(), before.clone());
        if input.remove.contains(&name) {
            if before.is_none() {
                return Err(Error::Invalid(format!(
                    "Cannot remove missing template {name}"
                )));
            }
            changes.push(Change {
                path: relative,
                before,
                after: None,
            });
        } else {
            let next = input
                .templates
                .get(&name)
                .cloned()
                .or_else(|| before.clone())
                .ok_or_else(|| Error::Invalid(format!("Missing template {name}")))?;
            if input.templates.contains_key(&name) && before.as_deref() != Some(&next) {
                changes.push(Change {
                    path: relative,
                    before,
                    after: Some(next.clone()),
                });
            }
            templates.insert(name, next);
        }
    }
    generator_presets::from_parts(input.config.clone(), templates)?;
    if changes.is_empty() {
        return Err(Error::Invalid(
            "The generator definitions are unchanged".into(),
        ));
    }
    let mut paths = BTreeSet::<String>::new();
    for change in &changes {
        let path = change.path.to_ascii_lowercase();
        if paths.iter().any(|other| {
            path.starts_with(&format!("{other}/")) || other.starts_with(&format!("{path}/"))
        }) || !paths.insert(path)
        {
            return Err(Error::Invalid("Preset output paths conflict".into()));
        }
    }
    Ok(Plan {
        changes,
        dependencies,
    })
}
fn preview(plan: &Plan) -> Value {
    json!({"files":plan.changes.iter().map(|change|json!({"path":change.path,"before":change.before,"after":change.after,"operation":if change.before.is_none(){"create"}else if change.after.is_none(){"delete"}else{"edit"}})).collect::<Vec<_>>()})
}

pub(crate) fn inspect(ctx: &Invocation) -> Result<Value, Error> {
    let (project, root) = owner(ctx)?;
    let text = read(&project, &root, CONFIG)?
        .unwrap_or_else(|| "{\"version\":1,\"presets\":[],\"modules\":[]}".into());
    let config = generator_presets::parse(&text).map_err(Error::Invalid)?;
    let mut templates = BTreeMap::new();
    for name in generator_presets::template_names(&config) {
        let value = read(&project, &root, &format!("{TEMPLATES}{name}"))?
            .ok_or_else(|| Error::Invalid(format!("Missing template {name}")))?;
        templates.insert(name, value);
    }
    let parsed: Value = serde_json::from_str(&text).map_err(|e| Error::Invalid(e.to_string()))?;
    Ok(json!({"scope":"project","config":parsed,"templates":templates}))
}
impl State {
    pub(crate) fn discard(&mut self, ctx: &Invocation) -> Result<(), Error> {
        let (_, root) = owner(ctx)?;
        self.pending.remove(&root);
        Ok(())
    }
    pub(crate) fn preview(
        &self,
        ctx: &Invocation,
        config: Value,
        templates: BTreeMap<String, String>,
        remove_templates: Vec<String>,
    ) -> Result<Value, Error> {
        let (project, root) = owner(ctx)?;
        Ok(preview(&build(
            &project,
            &root,
            &Input {
                config,
                templates,
                remove: remove_templates,
            },
        )?))
    }
    pub(crate) fn prepare(
        &mut self,
        ctx: &Invocation,
        config: Value,
        templates: BTreeMap<String, String>,
        remove_templates: Vec<String>,
    ) -> Result<Value, Error> {
        let (project, root) = owner(ctx)?;
        if self.pending.len() >= 16 && !self.pending.contains_key(&root) {
            return Err(Error::Invalid("Finish a prepared preset edit first".into()));
        }
        let input = Input {
            config,
            templates,
            remove: remove_templates,
        };
        let plan = build(&project, &root, &input)?;
        let editors = host::open_editors()?;
        if plan.changes.iter().any(|change| {
            editors
                .iter()
                .any(|editor| editor.path == format!("{root}/{}", change.path) && editor.dirty)
        }) {
            return Err(Error::Invalid(
                "Save dirty generator config/template editors before preparing changes".into(),
            ));
        }
        let anchor = editors
            .iter()
            .find(|editor| {
                editor.active
                    && !editor.dirty
                    && editor.language == "php"
                    && owned(&root, &editor.path)
            })
            .or_else(|| {
                editors.iter().find(|editor| {
                    !editor.dirty && editor.language == "php" && owned(&root, &editor.path)
                })
            })
            .map(|editor| editor.path.clone())
            .unwrap_or_else(|| format!("{root}/routes/web.php"));
        if !owned(&root, &anchor) {
            return Err(Error::Stale);
        }
        let anchor_text = project.read_file(&anchor)?;
        if anchor_text.len() > LIMIT {
            return Err(Error::Invalid(
                "Choose a smaller existing PHP file as the editor-action anchor".into(),
            ));
        }
        self.next = self.next.checked_add(1).ok_or(Error::Stale)?;
        let key = format!("generator-presets:{}", self.next);
        host::open_editor(&anchor, None)?;
        let mut response = preview(&plan);
        response["prepared"] = true.into();
        response["instruction"]=crate::ui_text::t("generator.presets.prepared","In the opened PHP editor, choose Apply prepared Laravel preset changes…; one undo reverses the complete config/template edit.").into();
        self.pending.insert(
            root,
            Pending {
                key,
                input,
                plan,
                anchor,
                anchor_text,
            },
        );
        Ok(response)
    }
    pub(crate) fn prepared(&self, root: &str) -> Option<(String, String)> {
        self.pending.get(root).map(|pending| (pending.key.clone(), pending.anchor.clone()))
    }
    pub(crate) fn actions(&self, doc: &Document) -> Vec<FileAction> {
        self.pending
            .iter()
            .find_map(|(root, pending)| {
                (pending.anchor == doc.path
                    && pending.anchor_text == doc.text
                    && owned(root, &doc.path))
                .then(|| FileAction {
                    title: crate::ui_text::t(
                        "generator.presets.apply",
                        "Apply prepared Laravel preset changes…",
                    ),
                    kind: Some("refactor".into()),
                    data: Some(pending.key.clone()),
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
        let pending = self.pending.get(&root).ok_or(Error::Stale)?;
        if action.data.as_deref() != Some(&pending.key)
            || pending.anchor != doc.path
            || pending.anchor_text != doc.text
            || project.read_file(&doc.path)? != doc.text
            || build(&project, &root, &pending.input)? != pending.plan
        {
            return Err(Error::Stale);
        }
        let exact = pending
            .plan
            .changes
            .iter()
            .map(|change| {
                crate::ui_text::format(
                    "generator.review.change",
                    "{path}\nBefore:\n{before}\nAfter:\n{after}",
                    &[
                        ("path", &change.path),
                        (
                            "before",
                            change.before.as_deref().unwrap_or(&crate::ui_text::t(
                                "generator.review.absent",
                                "(absent)",
                            )),
                        ),
                        (
                            "after",
                            change.after.as_deref().unwrap_or(&crate::ui_text::t(
                                "generator.review.deleted",
                                "(delete)",
                            )),
                        ),
                    ],
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        if !ui_kit::prompts::confirm_exact(
            &crate::ui_text::t(
                "generator.presets.confirm",
                "Apply Laravel generator definitions?",
            ),
            &exact,
        )? {
            return Err(Error::Cancelled);
        }
        if build(&project, &root, &pending.input)? != pending.plan
            || project.read_file(&doc.path)? != doc.text
        {
            return Err(Error::Stale);
        }
        let pending = self.pending.remove(&root).ok_or(Error::Stale)?;
        let mut result = FileAction {
            title: action.title,
            open: Some(format!("{root}/{CONFIG}")),
            ..Default::default()
        };
        for change in pending.plan.changes {
            let path = format!("{root}/{}", change.path);
            match (change.before, change.after) {
                (None, Some(text)) => result.operations.push(FileOperation::Create {
                    path,
                    text,
                    overwrite: false,
                }),
                (Some(_), None) => result.operations.push(FileOperation::Delete { path }),
                (Some(before), Some(after)) => result.edits.push((
                    path,
                    vec![TextEdit {
                        range: Range::new(
                            Position::new(0, 0),
                            text::position(&before, before.len()),
                        ),
                        text: after,
                    }],
                )),
                (None, None) => return Err(Error::Stale),
            }
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    fn fixture() -> (MockHost, State, Document) {
        let h = MockHost::install();
        h.add_file("routes/web.php", "<?php // anchor");
        h.open_editor("/project/routes/web.php", "php", "<?php // anchor", &[]);
        let doc = Document {
            path: "/project/routes/web.php".into(),
            language: "php".into(),
            text: "<?php // anchor".into(),
            version: 1,
        };
        (h, State::default(), doc)
    }
    fn config() -> Value {
        json!({"version":1,"presets":[{"id":"service","label":"Service","files":[{"path":"app/Service.php","template":"service.php"}]}]})
    }
    fn templates() -> BTreeMap<String, String> {
        BTreeMap::from([("service.php".into(), "<?php class Service {}".into())])
    }
    #[test]
    fn creation_is_one_reviewed_undoable_action_without_direct_writes() {
        let (h, mut state, doc) = fixture();
        state
            .prepare(&h.invocation(), config(), templates(), vec![])
            .unwrap();
        assert!(h.invocation().project().unwrap().read_file(CONFIG).is_err());
        let lazy = state.actions(&doc).remove(0);
        h.answer_next_prompt(Some(&["yes"]));
        let action = state.resolve(&doc, lazy.clone()).unwrap();
        assert_eq!(action.operations.len(), 2);
        assert!(action.operations.iter().all(|op| matches!(
            op,
            FileOperation::Create {
                overwrite: false,
                ..
            }
        )));
        assert!(action.edits.is_empty());
        assert!(state.resolve(&doc, lazy).is_err());
        assert!(h.process_runs().is_empty());
    }
    #[test]
    fn editing_and_deleting_use_one_transaction_and_reject_stale_config() {
        let (h, mut state, doc) = fixture();
        h.add_file(CONFIG, &config().to_string());
        h.add_file(&format!("{TEMPLATES}service.php"), "old");
        let empty = json!({"version":1});
        state
            .prepare(
                &h.invocation(),
                empty,
                BTreeMap::new(),
                vec!["service.php".into()],
            )
            .unwrap();
        let lazy = state.actions(&doc).remove(0);
        h.answer_next_prompt(Some(&["yes"]));
        let action = state.resolve(&doc, lazy).unwrap();
        assert_eq!(action.edits.len(), 1);
        assert!(matches!(
            action.operations.as_slice(),
            [FileOperation::Delete { .. }]
        ));
        state
            .prepare(&h.invocation(), config(), templates(), vec![])
            .unwrap();
        let lazy = state.actions(&doc).remove(0);
        h.add_file(CONFIG, "changed");
        assert!(state.resolve(&doc, lazy).is_err());
    }
    #[test]
    fn referenced_deletion_traversal_missing_templates_and_foreign_owners_are_refused() {
        let (h, state, _) = fixture();
        assert!(
            state
                .preview(
                    &h.invocation(),
                    config(),
                    templates(),
                    vec!["service.php".into()]
                )
                .is_err()
        );
        assert!(
            state
                .preview(&h.invocation(), config(), BTreeMap::new(), vec![])
                .is_err()
        );
        assert!(
            state
                .preview(
                    &h.invocation(),
                    config(),
                    BTreeMap::from([("../escape".into(), "x".into())]),
                    vec![]
                )
                .is_err()
        );
        h.add_project("/project/.openidea");
        assert!(
            state
                .preview(&h.invocation(), config(), templates(), vec![])
                .is_err()
        );
    }
    #[test]
    fn cancel_keeps_prepared_plan_and_never_returns_operations() {
        let (h, mut state, doc) = fixture();
        state
            .prepare(&h.invocation(), config(), templates(), vec![])
            .unwrap();
        let lazy = state.actions(&doc).remove(0);
        assert_eq!(state.resolve(&doc, lazy), Err(Error::Cancelled));
        assert_eq!(state.actions(&doc).len(), 1);
    }
}
