//! Explicit, historical positive Git observations; never native effective ignored status.
use openidea_plugin::{Error, Event, FileDecoration, FileKind, Invocation, host};
use std::time::{Duration, Instant};
pub(crate) const SET: &str = "partial-git-observation";
pub(crate) const MAX_FILES: usize = 16;
const BUDGET: Duration = Duration::from_secs(1);
const TOOLTIP: &str = "Ignored by Git in the last scan; may be stale and is not the current Git status. Run Ignore: Scan Git for Ignored Files… to refresh or Ignore: Clear Ignored-File Marks to remove.";
#[derive(Default)]
pub(crate) struct State {
    root: Option<String>,
    watching: bool,
}
impl State {
    pub(crate) fn configure(&mut self, watching: bool) {
        self.watching = watching;
    }
    pub(crate) fn clear(&mut self) -> Result<(), Error> {
        self.root = None;
        host::clear_file_decorations(SET)
    }
    pub(crate) fn changed(&mut self, events: &[Event], ctx: &Invocation) {
        let switched = self
            .root
            .as_ref()
            .is_some_and(|owner| ctx.project().and_then(|p| p.root().ok()).as_ref() != Some(owner));
        if switched
            || events.iter().any(|event| {
                matches!(
                    event,
                    Event::VcsChanged(_) | Event::FileSaved(_) | Event::FilesChanged(_)
                ) || matches!(event, Event::Other{kind,..} if kind=="settings-changed")
            })
        {
            let _ = self.clear();
        }
    }
    pub(crate) fn publish(
        &mut self,
        inventory: &crate::git_inventory::State,
        ctx: &Invocation,
    ) -> Result<Option<String>, Error> {
        self.clear()?; // Refusal, root switch and refresh never preserve older hints.
        if !self.watching {
            return Err(Error::Invalid(
                "Hint invalidation subscriptions are unavailable; nothing published.".into(),
            ));
        }
        let started = Instant::now();
        let sample = inventory.positive_samples(ctx)?;
        let project = ctx.project().ok_or(Error::Stale)?;
        let mut paths = Vec::new();
        for relative in sample.paths.iter().take(MAX_FILES) {
            if started.elapsed() > BUDGET {
                return Err(Error::Stale);
            }
            if relative.is_empty()
                || relative.len() > 1024
                || relative.starts_with('/')
                || relative.contains('\\')
                || relative.chars().any(char::is_control)
                || relative
                    .split('/')
                    .any(|part| part.is_empty() || matches!(part, "." | ".."))
            {
                return Err(Error::Stale);
            }
            let full = format!("{}/{relative}", sample.root);
            if !project.exists(&full)? {
                continue;
            } // Missing positives do not become absence warnings.
            if host::project_of(&full).ok_or(Error::Stale)?.root()? != sample.root {
                return Err(Error::Stale);
            }
            if project.stat(&full)?.kind == FileKind::File {
                paths.push(full);
            }
        }
        sample.validate(&project)?;
        if started.elapsed() > BUDGET || ctx.project().ok_or(Error::Stale)?.root()? != sample.root {
            return Err(Error::Stale);
        }
        let decoration = FileDecoration {
            badge: Some("I".into()),
            style: "muted".into(),
            tooltip: Some(TOOLTIP.into()),
        };
        for full in &paths {
            if started.elapsed() > BUDGET {
                self.clear()?;
                return Err(Error::Stale);
            }
            if let Err(error) = host::set_file_decoration(full, SET, &decoration) {
                let _ = self.clear();
                return Err(error);
            }
        }
        let validate = (|| {
            sample.validate(&project)?;
            for full in &paths {
                if started.elapsed() > BUDGET
                    || !project.exists(full)?
                    || project.stat(full)?.kind != FileKind::File
                    || host::project_of(full).ok_or(Error::Stale)?.root()? != sample.root
                {
                    return Err(Error::Stale);
                }
            }
            if started.elapsed() > BUDGET {
                return Err(Error::Stale);
            }
            Ok(())
        })();
        if let Err(error) = validate {
            let _ = self.clear();
            return Err(error);
        }
        self.root = Some(sample.root);
        Ok(Some(format!(
            "{} historical positive sample hints published (up to 16 of{} partial observations). Not current/effective Git status; no absence inference.",
            paths.len(),
            sample.observed
        )))
    }
}
