//! Native Services presentation. Typed keys retain the original scoped payload.
use super::*;
use openidea_plugin::PickItem;
use openidea_plugin::details::{Pane, ProcessLog, Tab};

impl KubeExplorer {
    fn service_class(&self, payload: &str) -> &'static str {
        let bare = payload
            .strip_prefix("node:")
            .or_else(|| payload.strip_prefix("choice:"))
            .unwrap_or(payload);
        let bare = if self.process_scope.root().is_some() {
            bare.rsplit_once('~').map_or(bare, |(key, _)| key)
        } else {
            bare
        };
        if let Some((kind, _)) = resource(bare) {
            return match kind.as_str() {
                "pods" => "pod",
                "deployments" | "statefulsets" => "workload",
                "daemonsets" | "jobs" => "log-workload",
                "services" => "service",
                "secrets" => "secret",
                _ => "resource",
            };
        }
        if let Some(name) = bare.strip_prefix("ctx/") {
            return if self
                .context
                .as_ref()
                .or(self.current_context.as_ref())
                .map(String::as_str)
                == Some(name)
            {
                "active-context"
            } else {
                "context"
            };
        }
        if let Some(name) = bare.strip_prefix("ns/") {
            return if self.namespace.as_deref() == Some(name) {
                "active-namespace"
            } else {
                "namespace"
            };
        }
        if bare == "kube:cluster" {
            if self.process_scope.root().is_some() && self.all_namespaces() {
                return if self.cluster_collapsed {
                    "cluster-all"
                } else {
                    "cluster-all-open"
                };
            }
            return match (self.process_scope.root().is_some(), self.cluster_collapsed) {
                (true, true) => "cluster",
                (true, false) => "cluster-open",
                (false, true) => "cluster-global",
                (false, false) => "cluster-global-open",
            };
        }
        if bare == "kube:status" {
            return "offline";
        }
        if bare.ends_with('!') {
            return "status";
        }
        if bare == "kind:secrets" && !self.all_namespaces() && self.rich_owner.is_some() {
            return if self.expanded.contains(bare) {
                "secrets-open"
            } else {
                "secrets"
            };
        }
        if bare == "ctx"
            || bare == "ns"
            || bare == "crds"
            || bare.starts_with("kind:")
            || bare.starts_with("crd:")
        {
            return if self.expanded.contains(bare) {
                "group-open"
            } else {
                "group"
            };
        }
        "status"
    }
    pub(super) fn service_payload<'a>(&self, key: &'a str) -> Result<&'a str, Error> {
        if !key.starts_with("svc-") {
            return Ok(key);
        }
        let (class, payload) = key.split_once(':').ok_or(Error::Stale)?;
        if class != format!("svc-{}", self.service_class(payload)) {
            return Err(Error::Stale);
        }
        Ok(payload)
    }
    pub(super) fn service_tree(&self, mut view: View) -> View {
        if let Body::Tree(items) = &mut view.body {
            for item in items {
                item.key = format!("svc-{}:{}", self.service_class(&item.key), item.key);
                if let Some(parent) = &mut item.parent {
                    *parent = format!("svc-{}:{parent}", self.service_class(parent));
                }
            }
        }
        // Services renders these as native icon buttons with the labels as tooltips.
        // Keep frequent actions visible and move secondary workflows into a picker.
        let more = self.service_more_items();
        view.toolbar.retain(|button| {
            let action = button.id.rsplit(':').next().unwrap_or(&button.id);
            !matches!(action, "attach" | "debug" | "log-view")
                && !(action == "create-secret" && self.selected.is_some())
        });
        if let Some((kind, row)) = &self.selected {
            if !more.is_empty() {
                view.toolbar.push(
                    Button::new(
                        &self.service_action_key(&self.resource_key(kind, row), "more"),
                        "More Resource Actions…",
                    )
                    .icon("more-horizontal"),
                );
            }
        }
        for button in &mut view.toolbar {
            let action = button.id.rsplit(':').next().unwrap_or(&button.id);
            match action {
                "yaml" => button.label = "Edit YAML…".into(),
                "logs" => button.label = "Follow Logs (Terminal)".into(),
                "log-view" => button.label = "Log Snapshot…".into(),
                "shell" => button.label = "Open Shell".into(),
                "port-forward" => button.label = "Port Forward…".into(),
                "create-secret" => button.icon = Some("plus".into()),
                _ => {}
            }
            let resource_action = matches!(
                action,
                "describe"
                    | "yaml"
                    | "logs"
                    | "log-view"
                    | "shell"
                    | "attach"
                    | "show-secret"
                    | "edit-secret"
                    | "debug"
                    | "port-forward"
                    | "delete"
            );
            let scope = if resource_action {
                self.selected
                    .as_ref()
                    .map(|(kind, row)| self.resource_key(kind, row))
            } else if matches!(action, "create-secret" | "forward-status") {
                Some(self.node_key("kube:cluster"))
            } else {
                None
            };
            if let Some(scope) = scope {
                button.id = self.service_action_key(&scope, action);
            }
        }
        view
    }
    fn service_more_items(&self) -> Vec<PickItem> {
        let Some((kind, _)) = &self.selected else {
            return Vec::new();
        };
        let mut items = Vec::new();
        if kind == "pods" || WORKLOADS.contains(&kind.as_str()) {
            items.push(PickItem::new("log-view", "Log Snapshot…"));
            items.push(PickItem::new("log-sources", "Follow Logs from Selected Sources…"));
        }
        if kind == "pods" {
            items.push(PickItem::new("attach", "Attach"));
            items.push(PickItem::new("debug", "Debug…"));
        }
        items
    }

    /// Services reselects its row before sending a toolbar action. A scoped
    /// resource identity survives that reselect; mutable form callbacks do not.
    pub(super) fn service_action(&mut self, encoded: &str, ctx: &Invocation) -> Result<(), Error> {
        let (_, action) = encoded.rsplit_once(':').ok_or(Error::Stale)?;
        let key = if matches!(action, "create-secret" | "forward-status") {
            self.node_key("kube:cluster")
        } else {
            let (kind, row) = self.selected.as_ref().ok_or(Error::Stale)?;
            self.resource_key(kind, row)
        };
        // Native button IDs are clipped to 64 bytes. A fixed-size scope proof
        // survives host reselect without embedding a potentially long row name.
        if encoded != self.service_action_key(&key, action) {
            return Err(Error::Stale);
        }
        if matches!(action, "create-secret" | "forward-status") {
            if self.checked_tree_key(&key)? != "kube:cluster" {
                return Err(Error::Stale);
            }
            self.button_in(&action, Some(ctx))?;
            crate::ui_group::reveal_view(PANEL)?;
            crate::refresh_panel();
            return Ok(());
        }
        let selected = self.checked_resource(&key)?.ok_or(Error::Stale)?;
        if self.selected.as_ref() != Some(&selected) {
            return Err(Error::Stale);
        }
        if action == "more" {
            let items = self.service_more_items();
            let Some(action) = host::pick("Resource actions", &items)? else {
                return Ok(());
            };
            if !items.iter().any(|item| item.key == action)
                || self.checked_resource(&key)?.as_ref() != self.selected.as_ref()
                || self.selected.as_ref() != Some(&selected)
            {
                return Err(Error::Stale);
            }
            return self.resource_command(&action, Some(&key), ctx);
        }
        self.resource_command(&action, Some(&key), ctx)
    }
    pub(super) fn native_details(&mut self, key: &str) -> Result<Vec<Tab>, Error> {
        let normalized = self.checked_tree_key(key)?;
        let key = normalized.as_str();
        if key == "kube:status" {
            return Ok(vec![Tab::new("status","Kubernetes",Pane::Message(format!("{} Private kubeconfig import/review needs no connection; imported profiles are not activated automatically.", ui_kit::denied::message("Browsing clusters", "process:kubectl", "Kube Explorer"))))]);
        }
        let Some((kind, row)) = self.checked_resource(key)? else {
            let context = self
                .context
                .as_ref()
                .or(self.current_context.as_ref())
                .map(String::as_str)
                .unwrap_or("current");
            let namespace = self
                .namespace
                .as_deref()
                .map(|ns| {
                    if ns == ALL_NAMESPACES {
                        "All namespaces"
                    } else {
                        ns
                    }
                })
                .unwrap_or("kubeconfig default");
            return Ok(vec![Tab::new(
                "overview",
                "Overview",
                Pane::Text(format!(
                    "Context: {context}\nNamespace: {namespace}\n\nRight-click contexts or namespaces to switch. Resource groups offer refresh and expand/collapse. Cluster actions include watch setup and kubeconfig management. Select a resource for details, logs and type-specific actions."
                )),
            )]);
        };
        let (namespace, name) = self.split_scope(&kind, &row)?;
        let namespace_label = if CLUSTER_SCOPED.contains(&kind.as_str()) {
            "Cluster-scoped"
        } else {
            namespace.as_deref().unwrap_or("kubeconfig default")
        };
        let mut tabs = vec![Tab::new(
            "resource",
            "Resource",
            Pane::Text(format!(
                "{kind}/{name}\nContext: {}\nNamespace: {namespace_label}\n\nRight-click this resource for available actions. Describe, YAML and Events execute read-only queries when their tabs are opened.",
                self.context
                    .as_deref()
                    .or(self.current_context.as_deref())
                    .unwrap_or("current")
            )),
        )];
        let log = |rest: &[&str]| {
            let args = self.args_in(namespace.as_deref(), rest);
            let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
            ProcessLog::new(KUBECTL, &refs)
        };
        let target = format!("{kind}/{name}");
        if kind == "pods" || WORKLOADS.contains(&kind.as_str()) {
            let mut args = vec![
                "logs",
                "-f",
                target.as_str(),
                "--tail=1000",
                "--prefix=true",
            ];
            if kind != "pods" {
                args.extend([
                    "--all-pods=true",
                    "--all-containers=true",
                    "--max-log-requests=20",
                ]);
            }
            let mut stream = log(&args).toggle("timestamps", "Timestamps", &["--timestamps"]);
            if kind == "pods" {
                stream = stream
                    .toggle(
                        "all-containers",
                        "All Containers",
                        &["--all-containers=true"],
                    )
                    .toggle("previous", "Previous Container", &["--previous"]);
            }
            tabs.push(Tab::new("logs", "Logs", stream));
        }
        tabs.push(Tab::new(
            "describe",
            "Describe",
            log(&["describe", &kind, &name, REQUEST_TIMEOUT]),
        ));
        // Secret data stays behind the existing explicit reveal/edit confirmation.
        if kind != "secrets" {
            tabs.push(Tab::new(
                "yaml",
                "YAML",
                log(&["get", &kind, &name, "-o", "yaml", REQUEST_TIMEOUT]),
            ));
        }
        if let Some(api_kind) = event_kind(&kind) {
            let selector = format!("involvedObject.kind={api_kind},involvedObject.name={name}");
            let mut rest = vec![
                "get",
                "events",
                "--field-selector",
                selector.as_str(),
                "--sort-by=.metadata.creationTimestamp",
                REQUEST_TIMEOUT,
            ];
            if CLUSTER_SCOPED.contains(&kind.as_str()) {
                rest.push("--all-namespaces");
            }
            tabs.push(Tab::new("events", "Events", log(&rest)));
        }
        Ok(tabs)
    }
}
fn event_kind(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "pods" => "Pod",
        "deployments" => "Deployment",
        "statefulsets" => "StatefulSet",
        "daemonsets" => "DaemonSet",
        "jobs" => "Job",
        "cronjobs" => "CronJob",
        "services" => "Service",
        "ingresses" => "Ingress",
        "configmaps" => "ConfigMap",
        "secrets" => "Secret",
        "persistentvolumeclaims" => "PersistentVolumeClaim",
        "persistentvolumes" => "PersistentVolume",
        "nodes" => "Node",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{details::Details as _, mock::MockHost};
    fn fixture(namespace: &str) -> (MockHost, KubeExplorer) {
        let host =
            MockHost::install().with_capabilities(&["project.read", "storage", "process:kubectl"]);
        let mut kube = KubeExplorer::default();
        kube.context = Some("selected-context".into());
        kube.namespace = Some(namespace.into());
        kube.render(TREE, None, &host.invocation()).unwrap();
        for kind in [
            "pods",
            "deployments",
            "daemonsets",
            "services",
            "secrets",
            "nodes",
        ] {
            kube.expanded.insert(format!("kind:{kind}"));
            let name = if namespace == ALL_NAMESPACES && kind != "nodes" {
                "team-a/web"
            } else {
                "web"
            };
            kube.children
                .insert(format!("kind:{kind}"), Ok(vec![name.into()]));
        }
        (host, kube)
    }
    fn row(kube: &mut KubeExplorer, host: &MockHost, kind: &str) -> String {
        let Body::Tree(items) = kube.render(TREE, None, &host.invocation()).unwrap().body else {
            panic!()
        };
        items
            .iter()
            .find(|item| test_resource(&item.key).is_some_and(|(k, _)| k == kind))
            .unwrap()
            .key
            .clone()
    }
    fn stream(tabs: &[Tab], id: &str) -> ProcessLog {
        let Pane::Log(log) = &tabs.iter().find(|tab| tab.id == id).unwrap().body else {
            panic!("native log pane")
        };
        log.clone()
    }
    #[test]
    fn service_resource_classes_retain_proofs_and_parent_links() {
        let (host, mut kube) = fixture("team-a");
        for (kind, class) in [
            ("pods", "pod"),
            ("deployments", "workload"),
            ("daemonsets", "log-workload"),
            ("services", "service"),
            ("secrets", "secret"),
            ("nodes", "resource"),
        ] {
            let key = row(&mut kube, &host, kind);
            assert!(key.starts_with(&format!("svc-{class}:res:{kind}/")));
            assert!(key.contains('~'));
            kube.handle(TREE, ViewEvent::Select(key.clone()), &host.invocation())
                .unwrap();
            assert_eq!(kube.selected.as_ref().unwrap().0, kind);
            let forged = key.replacen(&format!("svc-{class}:"), "svc-false:", 1);
            assert!(
                kube.handle(TREE, ViewEvent::Select(forged), &host.invocation())
                    .is_err()
            );
        }
        let Body::Tree(items) = kube.render(TREE, None, &host.invocation()).unwrap().body else {
            panic!()
        };
        let keys = items
            .iter()
            .map(|item| &item.key)
            .collect::<std::collections::HashSet<_>>();
        assert!(
            items
                .iter()
                .filter_map(|item| item.parent.as_ref())
                .all(|parent| keys.contains(parent))
        );
        assert!(host.process_runs().is_empty());
    }
    #[test]
    fn native_resource_tabs_are_lazy_scoped_and_type_appropriate() {
        let (host, mut kube) = fixture("team-a");
        let key = row(&mut kube, &host, "pods");
        let tabs = kube.detail(TREE, &key, &host.invocation()).unwrap();
        assert_eq!(
            tabs.iter().map(|tab| tab.id.as_str()).collect::<Vec<_>>(),
            ["resource", "logs", "describe", "yaml", "events"]
        );
        let logs = stream(&tabs, "logs");
        assert!(logs.args.contains(&"--namespace=team-a".into()));
        assert!(logs.args.contains(&"--context=selected-context".into()));
        assert_eq!(
            logs.toggles
                .iter()
                .map(|toggle| toggle.id.as_str())
                .collect::<Vec<_>>(),
            ["timestamps", "all-containers", "previous"]
        );
        let events = stream(&tabs, "events");
        assert!(
            events
                .args
                .contains(&"involvedObject.kind=Pod,involvedObject.name=web".into())
        );
        let secret = row(&mut kube, &host, "secrets");
        let tabs = kube.detail(TREE, &secret, &host.invocation()).unwrap();
        assert_eq!(
            tabs.iter().map(|tab| tab.id.as_str()).collect::<Vec<_>>(),
            ["resource", "describe", "events"]
        );
        assert!(
            host.process_runs().is_empty(),
            "selecting a resource must not execute the read-only tabs"
        );
    }
    #[test]
    fn all_namespace_logs_and_workloads_use_the_selected_rows_namespace() {
        let (host, mut kube) = fixture(ALL_NAMESPACES);
        for kind in ["pods", "deployments", "daemonsets"] {
            let key = row(&mut kube, &host, kind);
            let tabs = kube.detail(TREE, &key, &host.invocation()).unwrap();
            let logs = stream(&tabs, "logs");
            assert!(logs.args.contains(&"--namespace=team-a".into()));
            assert!(!logs.args.contains(&"--all-namespaces".into()));
            if kind != "pods" {
                for flag in [
                    "--all-pods=true",
                    "--all-containers=true",
                    "--max-log-requests=20",
                ] {
                    assert!(logs.args.iter().any(|arg| arg == flag));
                }
            }
        }
        assert!(host.process_runs().is_empty());
    }
    #[test]
    fn native_service_keys_cannot_cross_project_or_kind_boundaries() {
        let (host, mut kube) = fixture("team-a");
        let key = row(&mut kube, &host, "secrets");
        let forged = key.replacen("svc-secret:", "svc-pod:", 1);
        assert!(
            kube.run_command("resource-shell", &host.invocation(), Some(&forged))
                .is_err()
        );
        host.set_project_root(Some("/other"));
        assert!(kube.detail(TREE, &key, &host.invocation()).is_err());
        assert!(host.terminals().is_empty() && host.process_runs().is_empty());
    }
    #[test]
    fn native_toolbar_actions_survive_host_reselect_without_weakening_rich_forms() {
        let (host, mut kube) = fixture("team-a");
        let key = row(&mut kube, &host, "pods");
        kube.handle(TREE, ViewEvent::Select(key.clone()), &host.invocation())
            .unwrap();
        let view = kube.render(TREE, None, &host.invocation()).unwrap();
        let shell = view
            .toolbar
            .iter()
            .find(|button| button.label == "Open Shell")
            .unwrap()
            .id
            .clone();
        assert!(shell.starts_with("service-action:"));
        kube.handle(TREE, ViewEvent::Select(key.clone()), &host.invocation())
            .unwrap();
        kube.handle(TREE, ViewEvent::Button(shell.clone()), &host.invocation())
            .unwrap();
        let session = kube.shell_terminal_key(Some("team-a"), "web");
        assert!(host.finish_terminal(&session).is_some());
        let secret = row(&mut kube, &host, "secrets");
        kube.handle(TREE, ViewEvent::Select(secret), &host.invocation())
            .unwrap();
        assert!(
            kube.handle(TREE, ViewEvent::Button(shell.clone()), &host.invocation())
                .is_err()
        );
        assert!(
            kube.handle(PANEL, ViewEvent::Button(shell), &host.invocation())
                .is_err()
        );
        assert!(host.finish_terminal(&session).is_none());
        kube.handle(TREE, ViewEvent::Select(key.clone()), &host.invocation())
            .unwrap();
        let view = kube.render(TREE, None, &host.invocation()).unwrap();
        let delete = view
            .toolbar
            .iter()
            .find(|button| button.label == "Delete")
            .unwrap()
            .id
            .clone();
        kube.handle(TREE, ViewEvent::Select(key), &host.invocation())
            .unwrap();
        kube.handle(TREE, ViewEvent::Button(delete), &host.invocation())
            .unwrap(); // native confirmation cancelled
        assert!(host.process_runs().is_empty());
    }
    #[test]
    fn native_toolbar_controls_survive_host_id_clipping_for_long_resource_names() {
        let (host, mut kube) = fixture("team-a");
        let name = "a".repeat(200);
        kube.children
            .insert("kind:pods".into(), Ok(vec![name.clone()]));
        let pod = row(&mut kube, &host, "pods");
        kube.handle(TREE, ViewEvent::Select(pod.clone()), &host.invocation())
            .unwrap();
        let view = kube.render(TREE, None, &host.invocation()).unwrap();
        assert!(view.toolbar.iter().all(|button| button.id.len() <= 64));
        let shell = view
            .toolbar
            .iter()
            .find(|button| button.label == "Open Shell")
            .unwrap()
            .id
            .clone();
        assert!(!shell.contains(&name));
        let mut clipped = shell.clone();
        clipped.truncate(64);
        kube.handle(TREE, ViewEvent::Select(pod), &host.invocation())
            .unwrap();
        kube.handle(TREE, ViewEvent::Button(clipped), &host.invocation())
            .unwrap();
        assert!(host.finish_terminal(&kube.shell_terminal_key(Some("team-a"), &name)).is_some());
        // Altering an action must invalidate its proof, even on the same row.
        let forged = shell.strip_suffix("shell").unwrap().to_owned() + "delete";
        assert!(
            kube.handle(TREE, ViewEvent::Button(forged), &host.invocation())
                .is_err()
        );
        assert!(host.process_runs().is_empty());
    }

    #[test]
    fn native_top_toolbar_is_compact_kind_specific_and_overflow_is_bound_to_its_row() {
        let (host, mut kube) = fixture("team-a");
        let pod = row(&mut kube, &host, "pods");
        kube.handle(TREE, ViewEvent::Select(pod.clone()), &host.invocation())
            .unwrap();
        let view = kube.render(TREE, None, &host.invocation()).unwrap();
        assert_eq!(
            view.toolbar
                .iter()
                .map(|button| button.label.as_str())
                .collect::<Vec<_>>(),
            [
                "Refresh",
                "Describe",
                "Edit YAML…",
                "Follow Logs (Terminal)",
                "Open Shell",
                "Port Forward…",
                "Delete",
                "More Resource Actions…"
            ]
        );
        assert!(
            view.toolbar
                .iter()
                .all(|button| button.enabled && button.icon.is_some() && button.id.len() <= 64)
        );
        assert_eq!(
            kube.service_more_items()
                .iter()
                .map(|item| item.key.as_str())
                .collect::<Vec<_>>(),
            ["log-view", "log-sources", "attach", "debug"]
        );
        let more = view.toolbar.last().unwrap().id.clone();
        kube.handle(TREE, ViewEvent::Select(pod.clone()), &host.invocation())
            .unwrap();
        host.answer_next_prompt(None);
        kube.handle(TREE, ViewEvent::Button(more.clone()), &host.invocation())
            .unwrap();
        assert!(host.process_runs().is_empty() && host.terminals().is_empty());
        // A picker reply must be a secondary action actually offered for this row.
        host.answer_next_prompt(Some(&["delete"]));
        assert!(
            kube.handle(TREE, ViewEvent::Button(more.clone()), &host.invocation())
                .is_err()
        );
        assert!(host.process_runs().is_empty());
        let service = row(&mut kube, &host, "services");
        kube.handle(TREE, ViewEvent::Select(service), &host.invocation())
            .unwrap();
        let view = kube.render(TREE, None, &host.invocation()).unwrap();
        assert_eq!(
            view.toolbar
                .iter()
                .map(|button| button.label.as_str())
                .collect::<Vec<_>>(),
            [
                "Refresh",
                "Describe",
                "Edit YAML…",
                "Port Forward…",
                "Delete"
            ]
        );
        assert!(
            kube.handle(TREE, ViewEvent::Button(more), &host.invocation())
                .is_err()
        );
        assert!(host.process_runs().is_empty() && host.terminals().is_empty());
    }

    #[test]
    fn native_create_secret_toolbar_uses_its_own_view_and_opens_the_guarded_form() {
        let (host, mut kube) = fixture("team-a");
        let view = kube.render(TREE, None, &host.invocation()).unwrap();
        let create = view
            .toolbar
            .iter()
            .find(|button| button.label == "Create Secret")
            .unwrap()
            .id
            .clone();
        let Body::Tree(items) = view.body else {
            panic!()
        };
        let cluster = items[0].key.clone();
        kube.handle(TREE, ViewEvent::Select(cluster), &host.invocation())
            .unwrap();
        kube.handle(TREE, ViewEvent::Button(create), &host.invocation())
            .unwrap();
        assert!(kube.draft.is_some());
        assert_eq!(host.revealed_views(), [PANEL]);
        assert!(host.process_runs().is_empty());
    }
}
