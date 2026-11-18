//! Concrete-root task persistence. Invalid/corrupt records are never reset implicitly.
use crate::model::{DEFAULT_TASK_ID, TaskList};
use openidea_plugin::{Error, host};
use serde::{Deserialize, Serialize};
thread_local! {static LAST_WRITE: std::cell::RefCell<Option<(String,String)>>=const {std::cell::RefCell::new(None)};}
pub fn clear_receipt() {
    LAST_WRITE.with(|r| {
        r.borrow_mut().take();
    });
}
pub fn receipt(owner: &str) -> Option<String> {
    LAST_WRITE.with(|r| r.borrow().as_ref().filter(|(root, _)| root == owner).map(|(_, raw)| raw.clone()))
}
const MAX_BYTES: usize = 2 * 1024 * 1024;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    schema: u32,
    owner: String,
    revision: u64,
    data: TaskList,
}
fn invalid(s: &str) -> Error {
    Error::Invalid(format!("Task state: {s} Existing storage was not changed."))
}
pub fn root(root: &str) -> Result<(), Error> {
    if root.is_empty()
        || root.len() > 4096
        || root.chars().any(char::is_control)
        || !(root.starts_with('/') || (root.as_bytes().get(1) == Some(&b':') && root.as_bytes().get(2) == Some(&b'/')))
        || root.split('/').any(|p| matches!(p, "." | ".."))
    {
        return Err(invalid("a concrete absolute project identity is required."));
    }
    Ok(())
}
pub fn owns(owner: &str, path: &str) -> bool {
    root(owner).is_ok()
        && path.len() <= 4096
        && !path.chars().any(char::is_control)
        && !path.contains('\\')
        && !path.split('/').any(|p| matches!(p, "." | ".."))
        && path.strip_prefix(owner.trim_end_matches('/')).is_some_and(|tail| tail.starts_with('/'))
}
pub fn key(root: &str) -> String {
    format!("tasks:{root}")
}
pub fn raw(owner: &str) -> Result<Option<String>, Error> {
    root(owner)?;
    let raw = host::storage::get(&key(owner))?;
    if raw.as_ref().is_some_and(|s| s.len() > MAX_BYTES) {
        return Err(invalid("record exceeds 2 MiB; use explicit project-state recovery."));
    }
    Ok(raw)
}
fn string(s: &str, max: usize) -> bool {
    s.len() <= max && !s.chars().any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
}
fn validate(owner: &str, tasks: &TaskList) -> Result<(), Error> {
    if tasks.local_tasks_counter == u32::MAX
        || tasks.tasks.is_empty()
        || tasks.tasks.len() > 512
        || tasks.issues.len() > 1000
        || tasks.contexts.len() > 64
        || tasks.repositories.len() > 16
        || tasks.find(&tasks.active).is_none()
        || tasks.find(DEFAULT_TASK_ID).is_none()
    {
        return Err(invalid("invalid task counts/default/active identity."));
    }
    for list in [&tasks.tasks, &tasks.issues] {
        let mut ids = std::collections::BTreeSet::new();
        for t in list {
            if (matches!(t.tracker.as_deref(),Some("redmine"|"mantis"|"generic")) && !crate::self_hosted::identity(t))
                || (!matches!(t.tracker.as_deref(),Some("redmine"|"mantis"|"generic")) && t.self_hosted.is_some())
                || (t.tracker.as_deref()==Some("fogbugz") && !crate::fogbugz::identity(t))
                || (t.tracker.as_deref()!=Some("fogbugz") && t.fogbugz.is_some())
                || t.seconds_spent > u32::MAX as u64
                || t.seconds_posted > u32::MAX as u64
                || t.id.is_empty()
                || t.id.len() > 128
                || t.id.chars().any(char::is_control)
                || !ids.insert(&t.id)
                || !string(&t.summary, 4096)
                || !string(&t.description, 32 * 1024)
                || t.files.len() > 10
                || t.files.iter().any(|f| !owns(owner, f))
                || [t.url.as_deref(), t.api.as_deref()].into_iter().flatten().any(|s| s.len() > 2048 || s.chars().any(char::is_control))
                || [t.branch.as_deref(), t.base_branch.as_deref(), t.tracker.as_deref()]
                    .into_iter()
                    .flatten()
                    .any(|s| s.len() > 256 || s.chars().any(char::is_control))
            {
                return Err(invalid("invalid or foreign task data."));
            }
        }
    }
    let mut names = std::collections::BTreeSet::new();
    for c in &tasks.contexts {
        if c.name.trim().is_empty()
            || c.name.len() > 128
            || c.name.chars().any(char::is_control)
            || !names.insert(&c.name)
            || c.files.len() > 10
            || c.files.iter().any(|f| !owns(owner, f))
            || c.selection.as_ref().is_some_and(|(path, range)| !c.files.contains(path) || range.start > range.end)
        {
            return Err(invalid("invalid or foreign context data."));
        }
    }
    if tasks.repositories.iter().any(|(k, v)| {
        k.len() > 64
            || k.chars().any(char::is_control)
            || v.len() > 64
            || v.iter().any(|s| s.len() > 512 || s.chars().any(char::is_control))
    }) {
        return Err(invalid("repository scope exceeds supported bounds."));
    }
    Ok(())
}
fn decode(owner: &str, raw: Option<&str>) -> Result<(TaskList, u64), Error> {
    root(owner)?;
    let Some(raw) = raw else {
        return Ok((TaskList::new(crate::now()), 0));
    };
    if raw.len() > MAX_BYTES {
        return Err(invalid("record exceeds 2 MiB."));
    }
    let value: serde_json::Value = serde_json::from_str(raw).map_err(|_| invalid("corrupt JSON; use explicit project-state recovery."))?;
    let (data, revision) = if value.get("schema").is_some() || value.get("owner").is_some() || value.get("data").is_some() {
        let stored: Stored = serde_json::from_str(raw).map_err(|_| invalid("malformed ownership envelope."))?;
        if stored.schema != 1 || stored.owner != owner {
            return Err(invalid("unsupported version or different project owner."));
        }
        (stored.data, stored.revision)
    } else {
        (serde_json::from_str(raw).map_err(|_| invalid("corrupt legacy task record."))?, 0)
    };
    validate(owner, &data)?;
    Ok((data, revision))
}
pub fn snapshot(owner: &str) -> Result<(TaskList, Option<String>), Error> {
    let raw = raw(owner)?;
    let data = decode(owner, raw.as_deref())?.0;
    Ok((data, raw))
}
pub fn load(owner: &str) -> Result<TaskList, Error> {
    snapshot(owner).map(|(data, _)| data)
}

pub fn unchanged(owner: &str, expected: &Option<String>) -> Result<(), Error> {
    if raw(owner)? != *expected { Err(Error::Stale) } else { Ok(()) }
}
pub fn update<R>(owner: &str, change: impl FnOnce(&mut TaskList, u64) -> Result<R, Error>) -> Result<R, Error> {
    let before = raw(owner)?;
    let (mut data, revision) = decode(owner, before.as_deref())?;
    let result = change(&mut data, crate::now())?;
    validate(owner, &data)?;
    let revision = revision.checked_add(1).ok_or_else(|| invalid("revision exhausted."))?;
    let json =
        serde_json::to_string(&Stored { schema: 1, owner: owner.into(), revision, data }).map_err(|_| invalid("serialization failed."))?;
    if json.len() > MAX_BYTES {
        return Err(invalid("new record exceeds 2 MiB; previous state retained."));
    }
    unchanged(owner, &before)?;
    host::storage::set(&key(owner), &json)?;
    LAST_WRITE.with(|receipt| *receipt.borrow_mut() = Some((owner.into(), json)));
    Ok(result)
}
fn reset_prompt(owner: &str, before: Option<&str>) -> (String, String) {
    let name = owner.trim_end_matches('/').rsplit('/').next().unwrap_or(owner);
    let saved = match before.map(serde_json::from_str::<Stored>) {
        None => "Nothing is saved yet.".to_owned(),
        Some(Ok(Stored { data, .. })) => format!(
            "Saved now: {} tasks, {} contexts, {} repository overrides.",
            data.tasks.len(),
            data.contexts.len(),
            data.repositories.len()
        ),
        Some(Err(_)) => "The saved task list can't be read.".to_owned(),
    };
    (
        format!("Reset Tasks for {name}?"),
        format!("Deletes the saved tasks, time tracking, contexts and repository overrides of this project. It can't be undone.\n{saved}\nOther projects, Git branches, tracker issues and files stay unchanged."),
    )
}
pub fn recover(owner: &str) -> Result<(), Error> {
    root(owner)?;
    let before = host::storage::get(&key(owner))?;
    let (title, body) = reset_prompt(owner, before.as_deref());
    if !ui_kit::prompts::confirm_exact(&title, &body)? {
        return Ok(());
    }
    if host::storage::get(&key(owner))? != before {
        return Err(Error::Stale);
    }
    let data = TaskList::new(crate::now());
    let json = serde_json::to_string(&Stored { schema: 1, owner: owner.into(), revision: 1, data })
        .map_err(|_| invalid("serialization failed."))?;
    host::storage::set(&key(owner), &json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    #[test]
    fn restart_keeps_exact_owner_active_time_context_and_choices() {
        let _h = MockHost::install().with_capabilities(&["storage"]);
        update("/project", |t, _| {
            let id = t.add_local("Work", "notes");
            t.activate(&id, 100)?;
            t.active_mut().seconds_spent = 25;
            t.active_mut().files = vec!["/project/src/a.rs".into()];
            t.contexts.push(crate::model::NamedContext { name: "Focus".into(), files: t.active().files.clone(), selection: None });
            t.choices.create_branch = false;
            Ok(())
        })
        .unwrap();
        let reloaded = load("/project").unwrap();
        assert_eq!(reloaded.active, "LOCAL-00001");
        assert_eq!(reloaded.active().seconds_spent, 25);
        assert_eq!(reloaded.active_since, 100);
        assert_eq!(reloaded.contexts[0].files, ["/project/src/a.rs"]);
        assert!(!reloaded.choices.create_branch);
        let json = host::storage::get(&key("/project")).unwrap().unwrap();
        assert!(json.contains("\"owner\":\"/project\"") && json.contains("\"schema\":1"));
        assert_eq!(load("/other").unwrap().active, DEFAULT_TASK_ID);
    }
    #[test]
    fn corrupt_empty_foreign_and_oversized_records_are_retained_not_reset() {
        let _h = MockHost::install().with_capabilities(&["storage"]);
        let empty = serde_json::to_string(&TaskList { tasks: vec![], ..TaskList::new(1) }).unwrap();
        let foreign = serde_json::to_string(&Stored { schema: 1, owner: "/other".into(), revision: 1, data: TaskList::new(1) }).unwrap();
        for raw in ["{broken".to_string(), empty, foreign, "x".repeat(MAX_BYTES + 1)] {
            host::storage::set(&key("/project"), &raw).unwrap();
            assert!(load("/project").is_err());
            assert!(
                update("/project", |t, _| {
                    t.active = "changed".into();
                    Ok(())
                })
                .is_err()
            );
            assert_eq!(host::storage::get(&key("/project")).unwrap(), Some(raw));
        }
    }
    #[test]
    fn validated_legacy_migrates_on_write_and_foreign_files_never_migrate() {
        let _h = MockHost::install().with_capabilities(&["storage"]);
        let legacy = TaskList::new(1);
        host::storage::set(&key("/project"), &serde_json::to_string(&legacy).unwrap()).unwrap();
        assert_eq!(load("/project").unwrap().active, DEFAULT_TASK_ID);
        update("/project", |_, _| Ok(())).unwrap();
        let value: serde_json::Value = serde_json::from_str(&raw("/project").unwrap().unwrap()).unwrap();
        assert_eq!(value["owner"], "/project");
        let mut bad = TaskList::new(1);
        bad.active_mut().files = vec!["/project-sibling/a.rs".into()];
        host::storage::set(&key("/project"), &serde_json::to_string(&bad).unwrap()).unwrap();
        assert!(load("/project").is_err());
        assert!(!owns("/project", "/project/../other/a.rs"));
        assert!(root("").is_err());
    }
    #[test]
    fn stale_write_does_not_replace_concurrent_record_and_large_writer_preserves_last_good() {
        let _h = MockHost::install().with_capabilities(&["storage"]);
        update("/project", |_, _| Ok(())).unwrap();
        let previous = raw("/project").unwrap();
        assert!(
            update("/project", |t, _| {
                t.active_mut().description = "x".repeat(32 * 1024 + 1);
                Ok(())
            })
            .is_err()
        );
        assert_eq!(raw("/project").unwrap(), previous);
        let concurrent =
            serde_json::to_string(&Stored { schema: 1, owner: "/project".into(), revision: 99, data: TaskList::new(5) }).unwrap();
        assert!(matches!(
            update("/project", |t, _| {
                t.add_local("Old", "old");
                host::storage::set(&key("/project"), &concurrent)?;
                Ok(())
            }),
            Err(Error::Stale)
        ));
        assert_eq!(raw("/project").unwrap(), Some(concurrent));
    }
    #[test]
    fn reset_prompt_names_the_project_and_counts_what_is_deleted() {
        let _h = MockHost::install().with_capabilities(&["storage"]);
        update("/work/shop", |t, _| {
            t.add_local("Work", "");
            Ok(())
        })
        .unwrap();
        let saved = raw("/work/shop").unwrap();
        let (title, body) = reset_prompt("/work/shop", saved.as_deref());
        assert_eq!(title, "Reset Tasks for shop?");
        assert!(body.contains("\nSaved now: 2 tasks, 0 contexts, 0 repository overrides.\n"), "{body}");
        assert!(!body.contains("tasks:/work/shop"));
        assert!(reset_prompt("/work/shop", Some("bad")).1.contains("The saved task list can't be read."));
    }
    #[test]
    fn denial_and_cancelled_recovery_never_create_default_state() {
        let h = MockHost::install().with_capabilities(&["storage"]);
        host::storage::set(&key("/project"), "bad").unwrap();
        h.answer_next_prompt(None);
        recover("/project").unwrap();
        assert_eq!(raw("/project").unwrap(), Some("bad".into()));
        h.answer_next_prompt(Some(&["ok"]));
        recover("/project").unwrap();
        assert_eq!(load("/project").unwrap().active, DEFAULT_TASK_ID);
        let _h = h.with_capabilities(&[]);
        assert!(matches!(load("/project"), Err(Error::Denied(_))));
        assert!(update("/project", |_, _| Ok(())).is_err());
    }
    #[test]
    fn actual_reader_writer_byte_budget_keeps_previous_valid_record() {
        let _h = MockHost::install().with_capabilities(&["storage"]);
        update("/project", |_, _| Ok(())).unwrap();
        let good = raw("/project").unwrap().unwrap();
        let boundary = format!("{good}{}", " ".repeat(MAX_BYTES - good.len()));
        host::storage::set(&key("/project"), &boundary).unwrap();
        assert!(load("/project").is_ok());
        assert!(
            update("/project", |tasks, _| {
                for _ in 0..128 {
                    tasks.add_local("Bounded task", &"x".repeat(16 * 1024));
                }
                Ok(())
            })
            .is_err()
        );
        assert_eq!(raw("/project").unwrap(), Some(boundary));
    }
}
