//! API 1.4 execution target and project-scoped terminal operations.
use crate::{CONFIG, Main, database, model::Description, runtime, support};
use openidea_plugin::{
    Error, Invocation, NotificationLevel, PluginCommandOverride, PluginRunTarget,
    PluginRunTargetWithOverrides, Project, host,
};

pub const TARGET: &str = "ddev";

pub fn command(args: &[String]) -> host::Command {
    host::Command::new("ddev")
        .args(args)
        .env("DDEV_NONINTERACTIVE", "true")
}

pub fn terminal_key(project: &Project, action: &str, args: &[String]) -> Result<String, Error> {
    Ok(format!(
        "{}:{action}:{}",
        database::source_id(project)?,
        support::fingerprint(&serde_json::to_string(args).unwrap())
    ))
}

impl Main {
    pub(crate) fn check_target_preferences(&mut self, project: &Project, prefs: &runtime::Prefs) {
        let Ok(root) = project.root() else { return };
        let container = prefs.string("containerRoot", "/var/www/html");
        let changed = self
            .run_targets
            .get(&root)
            .is_some_and(|target| target.container_root.as_deref() != Some(container.as_str()));
        if !prefs.enabled("autoConfigureRunTarget")
            || !support::valid_container_root(&container)
            || changed
        {
            self.sync_target_with(project, None, prefs);
        }
    }

    pub(crate) fn terminal(
        &mut self,
        project: &Project,
        action: &str,
        args: &[String],
    ) -> Result<String, Error> {
        let command = command(args);
        let operation = host::open_terminal_session(
            &terminal_key(project, action, args)?,
            &format!("DDEV: {action}"),
            &command,
        )?;
        #[cfg(test)]
        self.terminal_launches.push(command);
        Ok(operation)
    }

    pub(crate) fn sync_target(&mut self, project: &Project, desc: Option<&Description>) {
        self.sync_target_with(project, desc, &runtime::Prefs::load(project));
    }

    pub(crate) fn sync_target_with(
        &mut self,
        project: &Project,
        desc: Option<&Description>,
        prefs: &runtime::Prefs,
    ) {
        let Ok(root) = project.root() else { return };
        if desc.is_none() || !prefs.enabled("autoConfigureRunTarget") {
            host::unregister_run_target(project, TARGET);
            self.run_targets.remove(&root);
            return;
        }
        let desc = desc.unwrap();
        let container_root = prefs.string("containerRoot", "/var/www/html");
        if !support::valid_container_root(&container_root) {
            host::unregister_run_target(project, TARGET);
            self.run_targets.remove(&root);
            host::log(
                openidea_plugin::LogLevel::Warn,
                "DDEV target withdrawn: invalid containerRoot preference. Use an absolute path without parent traversal.",
            );
            return;
        }
        let target = PluginRunTarget {
            id: TARGET.into(),
            name: format!("DDEV: {}", desc.name),
            program: "ddev".into(),
            // DDEV's raw mode preserves literal argv (including $, quotes and
            // empty strings). The host maps absolute project paths and cwd.
            args: vec!["exec".into(), "--raw".into()],
            workdir_args: vec!["--dir={cwd}".into(), "--".into()],
            container_root: Some(container_root),
            commands: vec![],
        };
        if self.run_targets.get(&root) == Some(&target) {
            return;
        }
        let mut with_overrides = PluginRunTargetWithOverrides::from(target.clone());
        with_overrides.commands = vec![PluginCommandOverride {
            name: "composer".into(),
            args: vec!["composer".into()],
            workdir_args: Some(vec![]),
        }];
        match host::register_run_target_with_overrides(project, &with_overrides) {
            Ok(()) => {
                self.run_targets.insert(root, target);
            }
            Err(Error::Denied(_) | Error::Cancelled) => {}
            Err(error) => host::log(
                openidea_plugin::LogLevel::Warn,
                &format!("DDEV execution target: {error}"),
            ),
        }
    }

    pub(crate) fn terminal_events(
        &mut self,
        events: &[openidea_plugin::Event],
        ctx: &Invocation,
    ) -> bool {
        let Some(project) = ctx.project() else {
            return false;
        };
        let Ok(prefix) = database::source_id(&project).map(|id| format!("{id}:")) else {
            return false;
        };
        let mut changed = false;
        for exit in events.iter().filter_map(|e| e.terminal_exited()) {
            let Some(action) = exit
                .key
                .strip_prefix(&prefix)
                .and_then(|k| k.split_once(':').map(|(action, _)| action))
            else {
                continue;
            };
            if !matches!(
                action,
                "config"
                    | "start"
                    | "restart"
                    | "stop"
                    | "delete"
                    | "poweroff"
                    | "share"
                    | "shell"
                    | "php"
                    | "node"
                    | "composer"
            ) {
                continue;
            }
            changed = true;
            if action == "config" {
                self.finish_config(
                    &project,
                    &exit.key,
                    &exit.id,
                    exit.status == Some(0) && !exit.closed,
                );
            }
            if !exit.closed && exit.status.is_some_and(|status| status != 0) {
                host::notify(
                    NotificationLevel::Warning,
                    "DDEV command failed",
                    &format!(
                        "ddev {action} exited with status {}. See its terminal output.",
                        exit.status.unwrap()
                    ),
                );
            }
        }
        if changed {
            let _task = host::begin_task("Refreshing DDEV after terminal exit", false);
            self.invalidate();
            if project.exists(CONFIG).unwrap_or(false) {
                if let Ok(desc) = self.description(&project).cloned() {
                    self.integrations(&project, &desc, true);
                } else {
                    self.publish_unavailable(&project, true);
                }
            } else {
                self.sync_target(&project, None);
                runtime::publish(None, false);
            }
        }
        changed
    }
}
