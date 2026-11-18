//! Read-only resolved physical edges from npm package-lock v2/v3. This does not
//! evaluate platform/peer constraints or run npm, and linked workspaces stay unknown.
use crate::*;
use serde_json::Value;

pub(crate) const VIEW: &str = "npm-dependency-graph";
pub(crate) const DEFAULT_MAX_PACKAGES: usize = 2000;

// ponytail: every package and edge becomes a native tree item; 20000 packages (~100k items) is the
// ceiling before the 5 s command budget and view refresh get risky. Page the tree if users need more.
fn max_packages() -> usize {
    host::setting_number("npmGraphMaxPackages")
        .filter(|n| n.is_finite())
        .map_or(DEFAULT_MAX_PACKAGES, |n| n.clamp(100.0, 20000.0) as usize)
}
pub(crate) struct Snapshot {
    root: String,
    path: String,
    text: String,
    items: Vec<TreeItem>,
    targets: HashMap<String, usize>,
    status: String,
    expansion: Expansion,
}

pub(crate) fn resolve(packages: &serde_json::Map<String, Value>, parent: &str, name: &str) -> Option<String> {
    locate(packages, parent, name, false)
}

/// Like `resolve`, but a `link: true` entry resolves to its `resolved` workspace member record.
pub(crate) fn resolve_linked(packages: &serde_json::Map<String, Value>, parent: &str, name: &str) -> Option<String> {
    locate(packages, parent, name, true)
}

fn locate(packages: &serde_json::Map<String, Value>, parent: &str, name: &str, follow_links: bool) -> Option<String> {
    if name.is_empty()
        || name.contains(['\\', ':'])
        || name
            .split('/')
            .any(|part| part.is_empty() || part == ".." || part == ".")
    {
        return None;
    }
    let mut base = parent;
    loop {
        if !base.ends_with("node_modules") {
            let key = if base.is_empty() {
                format!("node_modules/{name}")
            } else {
                format!("{base}/node_modules/{name}")
            };
            if follow_links {
                let target = packages.get(&key).filter(|e| e.get("link").and_then(Value::as_bool) == Some(true));
                if let Some(target) = target.and_then(|e| e.get("resolved")?.as_str()) {
                    return packages.contains_key(target).then(|| target.to_owned());
                }
            }
            if packages.get(&key).is_some_and(|entry| {
                entry.get("link").and_then(Value::as_bool) != Some(true)
                    && entry.get("version").and_then(Value::as_str).is_some()
            }) {
                return Some(key);
            }
        }
        if base.is_empty() {
            return None;
        }
        base = base
            .rsplit_once('/')
            .map(|(parent, _)| parent)
            .unwrap_or("");
    }
}

fn model(text: &str, max_packages: usize) -> Result<(Vec<TreeItem>, HashMap<String, usize>, String), Error> {
    let raise = "Raise Most packages in the npm graph in Settings › Plugins › Dependency Audit.";
    let max_bytes = max_packages << 10;
    if text.len() > max_bytes {
        return Err(format!("This lockfile is larger than {} KiB, the npm graph limit. {raise}", max_bytes >> 10).into());
    }
    let root: Value = serde_json::from_str(text)
        .map_err(|_| Error::Invalid("Invalid package lock JSON.".into()))?;
    if !matches!(
        root.get("lockfileVersion").and_then(Value::as_u64),
        Some(2 | 3)
    ) {
        return Err("Graph inspection currently supports package-lock v2/v3.".into());
    }
    let packages = root
        .get("packages")
        .and_then(Value::as_object)
        .ok_or("The lockfile has no packages map.")?;
    if packages.len() > max_packages {
        return Err(format!("This lockfile has {} packages, more than the npm graph limit ({max_packages}). {raise}", packages.len()).into());
    }
    let root_members =
        manifest::object_members(text, 0).ok_or("Ambiguous lockfile root ownership.")?;
    let package_at = root_members
        .iter()
        .find(|(name, _, _)| name == "packages")
        .map(|(_, _, at)| *at)
        .ok_or("Missing packages source.")?;
    let source_packages = manifest::object_members(text, package_at)
        .ok_or("Duplicate or unknown package source ownership.")?;
    let mut positions = HashMap::new();
    for (name, span, at) in source_packages {
        let fields =
            manifest::object_members(text, at).ok_or("Ambiguous package record ownership.")?;
        for (field, _, at) in fields {
            if [
                "dependencies",
                "optionalDependencies",
                "peerDependencies",
                "devDependencies",
            ]
            .contains(&field.as_str())
                && manifest::object_members(text, at).is_none()
            {
                return Err("Ambiguous dependency edge ownership.".into());
            }
        }
        positions.insert(name, span.0);
    }
    let mut items = Vec::new();
    let mut targets = HashMap::new();
    let mut unknown = 0usize;
    let mut edges = 0usize;
    for (index, (path, entry)) in packages.iter().enumerate() {
        let node = format!("npm-node:{index}");
        let label = if path.is_empty() {
            "Project root"
        } else {
            path
        };
        items.push(
            TreeItem::new(&node, label)
                .expandable(path.is_empty())
                .icon("package")
                .detail(
                    entry
                        .get("version")
                        .and_then(Value::as_str)
                        .unwrap_or("No resolved version"),
                ),
        );
        if let Some(at) = positions.get(path.as_str()) {
            targets.insert(node.clone(), *at);
        }
        let mut dependencies = std::collections::BTreeMap::new();
        for kind in [
            "dependencies",
            "optionalDependencies",
            "peerDependencies",
            "devDependencies",
        ] {
            if kind == "devDependencies" && !path.is_empty() {
                continue;
            }
            for (name, requirement) in entry
                .get(kind)
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
            {
                dependencies.insert(
                    (name, kind),
                    requirement.as_str().unwrap_or("Unknown requirement"),
                );
            }
        }
        for ((name, kind), requirement) in dependencies {
            edges += 1;
            if edges > max_packages * 4 {
                return Err(format!("This lockfile has more than {} dependency edges, the npm graph limit. {raise}", max_packages * 4).into());
            }
            let key = format!("npm-edge:{index}:{edges}");
            let target = resolve(packages, path, name);
            let detail = match &target {
                Some(target) => format!(
                    "{kind} {requirement} → {target} ({})",
                    packages[target]
                        .get("version")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                ),
                None => {
                    unknown += 1;
                    format!("{kind} {requirement} → unresolved or linked workspace")
                }
            };
            items.push(
                TreeItem::new(&key, name)
                    .parent(&node)
                    .icon(if target.is_some() {
                        "package"
                    } else {
                        "warning"
                    })
                    .detail(&detail),
            );
            if let Some(at) = target.as_deref().and_then(|target| positions.get(target)) {
                targets.insert(key, *at);
            }
        }
    }
    Ok((
        items,
        targets,
        format!(
            "{} package records · {edges} edges · {unknown} unresolved/linked. Physical lockfile lookup only; optional/peer/platform activation is not evaluated.",
            packages.len()
        ),
    ))
}
fn nearest_lockfile(project: &Project, root: &str) -> Result<String, Error> {
    let mut found: Vec<String> = project
        .find_files("**/package-lock.json", 50)?
        .into_iter()
        .filter(|path| !path.contains("/node_modules/"))
        .map(|path| if path.starts_with('/') { path } else { format!("{}/{path}", root.trim_end_matches('/')) })
        .collect();
    found.sort_by_key(|path| path.matches('/').count());
    let depth = |path: &String| path.matches('/').count();
    match found.as_slice() {
        [] => Err("No package-lock.json in the project.".into()),
        [only] => Ok(only.clone()),
        [first, second, ..] if depth(first) < depth(second) => Ok(first.clone()),
        _ => Err("Several package-lock.json files; focus the one to inspect.".into()),
    }
}
impl Snapshot {
    pub(crate) fn capture(ctx: &Invocation) -> Result<Self, Error> {
        let project = ctx.project().ok_or(Error::Stale)?;
        let root = project.root()?;
        let (path, text) = match ctx.editor().filter(|editor| editor.path().ends_with("/package-lock.json")) {
            Some(editor) => (editor.path(), editor.document()?.text),
            None => {
                let path = nearest_lockfile(&project, &root)?;
                let text = project.read_file(&path)?;
                (path, text)
            }
        };
        if !owned_source_path(&root, &path) {
            return Err(Error::Stale);
        }
        let (items, targets, status) = model(&text, max_packages())?;
        Ok(Self {
            root,
            path,
            text,
            items,
            targets,
            status,
            expansion: Expansion::default(),
        })
    }
    fn current(&self, ctx: &Invocation) -> bool {
        ctx.project().is_some_and(|p| {
            p.root().ok().as_deref() == Some(self.root.as_str())
                && owned_source_path(&self.root, &self.path)
                && p.read_file(&self.path).is_ok_and(|text| text == self.text)
        })
    }
    pub(crate) fn render(&self, ctx: &Invocation) -> View {
        if !self.current(ctx) {
            View::message(
                "Dependency graph source changed or belongs to another project. Inspect the lockfile again.",
            )
        } else {
            {
                let mut items = self.items.clone();
                self.expansion.apply(&mut items);
                View::new(Body::Tree(items)).status(&self.status)
            }
        }
    }
    pub(crate) fn handle(&mut self, event: ViewEvent, ctx: &Invocation) -> Result<(), Error> {
        if !self.current(ctx) {
            return Err(Error::Stale);
        }
        match &event {
            ViewEvent::Expand(key) | ViewEvent::Collapse(key)
                if self
                    .items
                    .iter()
                    .any(|item| item.parent.as_deref() == Some(key)) =>
            {
                self.expansion.handle(&event);
                crate::refresh(VIEW);
                Ok(())
            }
            ViewEvent::Activate(key) => self.activate(key, ctx),
            _ => Ok(()),
        }
    }
    pub(crate) fn activate(&self, key: &str, ctx: &Invocation) -> Result<(), Error> {
        if !self.current(ctx) {
            return Err(Error::Stale);
        }
        let Some(at) = self.targets.get(key) else {
            return Ok(());
        };
        let p = openidea_plugin::text::position(&self.text, *at);
        host::open_location(&self.path, p.line, p.character)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nested_lookup_resolves_nearest_physical_version_and_leaves_links_unknown() {
        let text = r#"{"lockfileVersion":3,"packages":{"":{"dependencies":{"a":"*","linked":"*"}},"node_modules/a":{"version":"1","dependencies":{"b":"*"}},"node_modules/b":{"version":"2"},"node_modules/a/node_modules/b":{"version":"1"},"node_modules/linked":{"link":true,"resolved":"packages/linked"}}}"#;
        let (items, targets, status) = model(text, DEFAULT_MAX_PACKAGES).unwrap();
        assert!(items.iter().any(|item| {
            item.detail
                .as_deref()
                .is_some_and(|detail| detail.contains("→ node_modules/a/node_modules/b (1)"))
        }));
        assert!(status.contains("1 unresolved/linked"));
        assert!(!targets.is_empty());
        assert!(model(r#"{"lockfileVersion":1}"#, DEFAULT_MAX_PACKAGES).is_err());
    }
    #[test]
    fn package_limit_comes_from_the_setting_and_errors_name_it() {
        let h = openidea_plugin::mock::MockHost::install();
        let packages: String = (0..150).map(|i| format!(r#","node_modules/p{i}":{{"version":"1"}}"#)).collect();
        let text = format!(r#"{{"lockfileVersion":3,"packages":{{"":{{}}{packages}}}}}"#);
        h.open_editor("/project/package-lock.json", "json", &text, &[(0, 0, 0, 0)]);
        h.set_setting("npmGraphMaxPackages", "100");
        let error = Snapshot::capture(&h.invocation()).err().unwrap().to_string();
        assert!(error.contains("151 packages") && error.contains("Settings › Plugins › Dependency Audit"));
        h.set_setting("npmGraphMaxPackages", "1");
        assert!(Snapshot::capture(&h.invocation()).is_err());
        h.set_setting("npmGraphMaxPackages", "200");
        assert!(Snapshot::capture(&h.invocation()).is_ok());
    }
    #[test]
    fn navigation_uses_package_owner_not_equal_nested_metadata_key() {
        let text = r#"{"lockfileVersion":3,"packages":{"":{"dependencies":{"b":"*"},"metadata":{"node_modules/b":{}}},"node_modules/b":{"version":"1"}}}"#;
        let (_, targets, _) = model(text, DEFAULT_MAX_PACKAGES).unwrap();
        let expected = text.rfind("node_modules/b").unwrap();
        assert!(
            targets
                .values()
                .all(|at| *at == expected || *at == text.find("\"\":{").unwrap() + 1)
        );
        assert!(model(r#"{"lockfileVersion":3,"packages":{"node_modules/b":{"version":"1"},"node_modules/b":{"version":"2"}}}"#, DEFAULT_MAX_PACKAGES).is_err());
    }
    #[test]
    fn native_expansion_survives_rerender_and_leaf_nodes_do_not_show_spinners() {
        let h = openidea_plugin::mock::MockHost::install();
        let text = r#"{"lockfileVersion":3,"packages":{"":{"dependencies":{"a":"*"}},"node_modules/a":{"version":"1","dependencies":{"b":"*"}},"node_modules/b":{"version":"2"}}}"#;
        h.open_editor("/project/package-lock.json", "json", text, &[(0, 0, 0, 0)]);
        let mut s = Snapshot::capture(&h.invocation()).unwrap();
        let view = s.render(&h.invocation());
        let Body::Tree(items) = view.body else {
            panic!()
        };
        let node = items
            .iter()
            .find(|item| item.label == "node_modules/a")
            .unwrap();
        assert!(node.expandable && !node.expanded);
        let key = node.key.clone();
        s.handle(ViewEvent::Expand(key.clone()), &h.invocation())
            .unwrap();
        let Body::Tree(items) = s.render(&h.invocation()).body else {
            panic!()
        };
        assert!(items.iter().find(|item| item.key == key).unwrap().expanded);
        assert!(
            items
                .iter()
                .filter(|item| item.parent.is_some())
                .all(|item| !item.expandable)
        );
    }
    #[test]
    fn capture_falls_back_to_the_project_root_lockfile_without_a_focused_lockfile() {
        let h = openidea_plugin::mock::MockHost::install();
        h.set_project_root(Some("/project"));
        h.add_file("/project/package-lock.json", r#"{"lockfileVersion":3,"packages":{"":{"dependencies":{"a":"*"}},"node_modules/a":{"version":"1"}}}"#);
        assert!(Snapshot::capture(&h.invocation()).is_ok());
    }
    #[test]
    fn capture_finds_a_single_subfolder_lockfile_and_refuses_ties() {
        let h = openidea_plugin::mock::MockHost::install();
        h.set_project_root(Some("/project"));
        let lock = r#"{"lockfileVersion":3,"packages":{"":{"dependencies":{"a":"*"}},"node_modules/a":{"version":"1"}}}"#;
        h.add_file("web/package-lock.json", lock);
        let snapshot = Snapshot::capture(&h.invocation()).unwrap();
        assert_eq!(snapshot.path, "/project/web/package-lock.json");
        h.add_file("api/package-lock.json", lock);
        assert!(Snapshot::capture(&h.invocation()).is_err());
    }
    #[test]
    fn snapshot_uses_current_buffer_and_withholds_stale_navigation() {
        let h = openidea_plugin::mock::MockHost::install();
        let text = r#"{"lockfileVersion":3,"packages":{"":{"dependencies":{"a":"*"}},"node_modules/a":{"version":"1"}}}"#;
        h.open_editor("/project/package-lock.json", "json", text, &[(0, 0, 0, 0)]);
        let s = Snapshot::capture(&h.invocation()).unwrap();
        let key = s.targets.keys().next().unwrap();
        s.activate(key, &h.invocation()).unwrap();
        h.type_in_editor("{}");
        assert!(matches!(
            s.activate(key, &h.invocation()),
            Err(Error::Stale)
        ));
        assert!(h.requests().is_empty() && h.process_runs().is_empty());
    }
}
