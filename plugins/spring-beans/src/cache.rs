//! Per-project source caches on the shared spring-text index, with byte limits,
//! an inventory of paths kept across sessions, and lookups rebuilt once per change.
use crate::{FileModel, GLOBS, Lookups, model};
use openidea_plugin::{Error, Project, host};
use spring_text::index::Index;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::time::Duration;
const MAX_FILES: usize = 4096;
const INVENTORY_KEY: &str = "beans.source-inventory.v1";
const MAX_INVENTORY_BYTES: usize = 256 << 10;
// Only paths are persisted. Restored paths are hints, never trusted model facts.
fn inventory_paths(value: &str, root: &str) -> Option<Vec<String>> {
    if value.len() > MAX_INVENTORY_BYTES { return None; }
    let value: serde_json::Value = serde_json::from_str(value).ok()?;
    if value.get("version")?.as_u64()? != 1 { return None; }
    let roots = value.get("roots")?.as_object()?;
    if roots.len() > MAX_ROOTS { return None; }
    let paths = roots.get(root)?.as_array()?;
    if paths.len() > MAX_FILES { return None; }
    let mut result = BTreeSet::new();
    for path in paths {
        let path = path.as_str()?;
        if !owned(path, root) || !source(path) || !result.insert(path.to_owned()) { return None; }
    }
    Some(result.into_iter().collect())
}
pub(crate) const MAX_TEXT: usize = 512 << 10;
const MAX_ROOTS: usize = 8;
const MAX_CACHE_BYTES: usize = 32 << 20;
pub(crate) fn source(path: &str) -> bool {
    [".java", ".kt", ".xml", ".properties", ".yml", ".yaml", ".sql"].iter().any(|suffix| path.ends_with(suffix))
}
pub(crate) fn build(path: &str) -> bool {
    matches!(
        path.rsplit('/').next().unwrap_or_default(),
        "pom.xml" | "build.gradle" | "build.gradle.kts" | "settings.gradle" | "settings.gradle.kts" | "gradle.properties"
    )
}
pub(crate) fn owned(path: &str, root: &str) -> bool {
    path.len() <= 4096
        && !path.chars().any(char::is_control)
        && !path.contains('\\')
        && !path.split('/').any(|part| part == "." || part == "..")
        && path.strip_prefix(root).is_some_and(|tail| tail.starts_with('/'))
}
/// A copy under a module's `target/` or `build/` output, outside any `src/` tree.
pub(crate) fn build_output(path: &str) -> bool {
    if !path.contains("/target/") && !path.contains("/build/") {
        return false;
    }
    let root = host::project_of(path).and_then(|project| project.root().ok()).unwrap_or_default();
    let relative = path.strip_prefix(root.trim_end_matches('/')).unwrap_or(path);
    for part in relative.split('/') {
        match part {
            "src" => return false,
            "target" | "build" => return true,
            _ => {}
        }
    }
    false
}
fn parse(path: &str, text: &str) -> Option<FileModel> {
    if build_output(path) {
        return None;
    }
    model::parse(path, text)
}
/// Only unreadable Java, Kotlin and Spring XML sources leave the cache incomplete;
/// other XML, configuration and SQL files that cannot be kept are skipped.
fn required(path: &str, text: Option<&str>) -> bool {
    if build_output(path) {
        return false;
    }
    match path.rsplit('.').next() {
        Some("java" | "kt") => true,
        Some("xml") => text.is_none_or(|text| text.contains("springframework.org")),
        _ => false,
    }
}
/// A Spring beans file the namespace check refused, as opposed to an unrelated XML file.
fn broken_beans(path: &str, text: &str) -> bool {
    path.ends_with(".xml") && !build_output(path) && text.contains(crate::xml_checks::BEANS_NS)
}

/// One project's cached models, the build modules looked up for them, and
/// the lookups derived from both.
pub(crate) struct Cache {
    index: Index<FileModel>,
    pub(crate) modules: RefCell<BTreeMap<String, Option<String>>>,
    lookups: RefCell<Option<((u64, bool), Rc<Lookups>)>>,
}
impl Cache {
    pub fn new() -> Self {
        let index = Index::new(GLOBS, parse).with_limits(MAX_TEXT, MAX_CACHE_BYTES).requiring(required)
            .rejecting(broken_beans)
            .excluding_nested_projects();
        Self { index, modules: RefCell::default(), lookups: RefCell::default() }
    }
    pub fn files(&self) -> &BTreeMap<String, FileModel> {
        self.index.files()
    }
    pub fn complete(&self) -> bool {
        self.index.complete()
    }
    pub fn scanning(&self) -> bool {
        self.index.scanning()
    }
    pub fn generation(&self) -> u64 {
        self.index.generation()
    }
    pub fn update(&mut self, path: &str, text: &str) {
        if source(path) {
            self.index.update(path, text);
        }
    }
    pub fn remove(&mut self, path: &str) {
        self.index.remove(path);
    }
    pub fn fail(&mut self, path: &str) {
        self.index.fail(path);
    }
    pub fn remove_tree(&mut self, path: &str) {
        let prefix = format!("{path}/");
        let doomed: Vec<String> =
            self.index.inventory().into_iter().filter(|file| *file == path || file.starts_with(&prefix)).map(str::to_owned).collect();
        for file in doomed {
            self.index.remove(&file);
        }
    }
    pub fn reset(&mut self) {
        self.index.reset();
        self.modules.borrow_mut().clear();
    }
    pub fn state(&self) -> String {
        if self.index.scanning() {
            "Scan not finished. Click Refresh to finish it.".into()
        } else if !self.index.walked() {
            "Not scanned yet. Click Refresh to find this project's Spring beans.".into()
        } else if self.index.unreadable() > 0 {
            format!(
                "{} source files couldn't be read (too large or not valid Spring XML); the other beans still show. Click Refresh to retry.",
                self.index.unreadable()
            )
        } else if !self.index.complete() {
            "Not every source file was read (the project has too many files). Click Refresh to retry.".into()
        } else {
            "Source cache ready.".into()
        }
    }
    /// The lookups of the current models, rebuilt only after they change.
    pub fn lookups(&self, build: impl FnOnce() -> Lookups) -> Rc<Lookups> {
        let key = (self.generation(), self.complete());
        if let Some((built, lookups)) = self.lookups.borrow().as_ref()
            && *built == key
        {
            return lookups.clone();
        }
        let lookups = Rc::new(build());
        *self.lookups.borrow_mut() = Some((key, lookups.clone()));
        lookups
    }
}
pub(crate) struct Indexes {
    roots: BTreeMap<String, Cache>,
    unidentified: Cache,
    restored: BTreeSet<String>,
    inventory: Option<String>,
    inventory_loaded: bool,
    /// The roots' generations and the value at the last checkpoint.
    checkpointed: Option<(Vec<(String, u64)>, usize, String)>,
}
impl Default for Indexes {
    fn default() -> Self {
        Self {
            roots: BTreeMap::new(),
            unidentified: Cache::new(),
            restored: BTreeSet::new(),
            inventory: None,
            inventory_loaded: false,
            checkpointed: None,
        }
    }
}
impl Indexes {
    pub fn prune(&mut self) {
        let roots: BTreeSet<_> = host::projects().unwrap_or_default().iter().filter_map(|project| project.root().ok()).collect();
        self.roots.retain(|root, _| roots.contains(root));
        self.restored.retain(|root| roots.contains(root));
    }
    fn ensure(&mut self, root: &str) -> &mut Cache {
        if !self.roots.contains_key(root) && self.roots.len() >= MAX_ROOTS {
            if let Some(old) = self.roots.keys().next().cloned() {
                self.roots.remove(&old);
            }
        }
        self.roots.entry(root.into()).or_insert_with(Cache::new)
    }
    /// The cache of `project`, read for at most `budget` more. A zero budget
    /// (providers) never touches storage, restores or discovers.
    pub fn get(&mut self, project: &Project, budget: Duration) -> &mut Cache {
        self.prune();
        let Ok(root) = project.root().and_then(|root| if root.is_empty() { Err(Error::Stale) } else { Ok(root) }) else {
            self.unidentified.reset();
            return &mut self.unidentified;
        };
        if !host::projects().unwrap_or_default().iter().any(|project| project.root().ok().as_deref() == Some(root.as_str())) {
            self.unidentified.reset();
            return &mut self.unidentified;
        }
        if budget.is_zero() {
            return self.ensure(&root);
        }
        if !self.inventory_loaded {
            self.inventory = host::storage::get(INVENTORY_KEY).ok().flatten().filter(|value| value.len() <= MAX_INVENTORY_BYTES);
            self.inventory_loaded = true;
        }
        if self.restored.insert(root.clone()) {
            if let Some(paths) = self.inventory.as_deref().and_then(|value| inventory_paths(value, &root)) {
                let paths = paths
                    .into_iter()
                    .filter(|path| host::project_of(path).and_then(|project| project.root().ok()).as_deref() == Some(root.as_str()))
                    .collect();
                self.ensure(&root).index.seed(paths);
            }
        }
        self.ensure(&root).index.scan(project, budget);
        self.checkpoint();
        self.ensure(&root)
    }
    pub fn cached(&mut self, project: &Project) -> Option<&Cache> {
        self.prune();
        let root = project.root().ok()?;
        self.roots.get(&root)
    }
    /// The cache of the project owning `path`, without applying any text.
    pub fn document(&mut self, path: &str) -> Option<&mut Cache> {
        let project = host::project_of(path)?;
        let root = project.root().ok()?;
        if root.is_empty() || !owned(path, &root) {
            return None;
        }
        Some(self.get(&project, Duration::ZERO))
    }
    pub fn for_document(&mut self, path: &str, text: &str, budget: Duration) -> Option<&mut Cache> {
        let project = host::project_of(path)?;
        let root = project.root().ok()?;
        if root.is_empty() || !owned(path, &root) {
            return None;
        }
        let cache = self.get(&project, budget);
        cache.update(path, text);
        Some(cache)
    }
    #[cfg(test)]
    pub fn for_cached_document(&mut self, path: &str, text: &str) -> Option<&mut Cache> {
        self.for_document(path, text, Duration::ZERO)
    }
    pub fn reset_project(&mut self, project: &Project) {
        if let Ok(root) = project.root() {
            self.restored.insert(root.clone());
            self.ensure(&root).reset();
            self.checkpoint();
        }
    }
    pub fn changed(&mut self, project: &Project, path: &str, removed: bool) -> bool {
        let Ok(root) = project.root() else {
            return false;
        };
        if !owned(path, &root) {
            return false;
        }
        let directory = self.roots.get(&root).is_some_and(|cache| cache.index.inventory().iter().any(|file| file.starts_with(&format!("{path}/"))));
        if !source(path) && !directory && !build(path) {
            return false;
        }
        if build(path) {
            self.reset_project(project);
            return true;
        }
        self.restored.insert(root.clone());
        let cache = self.ensure(&root);
        if removed {
            cache.remove_tree(path);
        } else if source(path) {
            match project.read_file(path) {
                Ok(text) => cache.update(path, &text),
                // A file that exists but is not text may be reported as missing.
                Err(Error::NotFound(_)) if project.read_bytes(path).is_ok() => cache.fail(path),
                Err(Error::NotFound(_)) => cache.remove(path),
                Err(_) => cache.fail(path),
            }
        }
        true
    }
    /// Lifecycle/command only; writes storage only when the inventoried paths
    /// changed, and optional storage denial leaves the in-memory cache usable.
    pub fn checkpoint(&mut self) {
        let generations: Vec<(String, u64)> = self.roots.iter().map(|(root, cache)| (root.clone(), cache.generation())).collect();
        if self.checkpointed.as_ref().is_some_and(|(seen, restored, _)| *seen == generations && *restored == self.restored.len()) {
            return;
        }
        let value = self.inventory_value();
        let unchanged = self.checkpointed.as_ref().is_some_and(|(_, _, saved)| *saved == value);
        if !unchanged {
            let _ = host::storage::set(INVENTORY_KEY, &value);
        }
        self.checkpointed = Some((generations, self.restored.len(), value));
    }
    fn inventory_value(&self) -> String {
        let mut roots = BTreeMap::new();
        // A first root's checkpoint must not erase other open roots before their warm turn.
        if let Some(value) = self.inventory.as_deref() {
            for project in host::projects().unwrap_or_default().into_iter().take(MAX_ROOTS) {
                if let Ok(root) = project.root() {
                    if !self.restored.contains(&root) {
                        if let Some(paths) = inventory_paths(value, &root) {
                            roots.insert(root, paths.into_iter().collect::<BTreeSet<_>>());
                        }
                    }
                }
            }
        }
        let json_len = |text: &str| serde_json::to_string(text).map_or(MAX_INVENTORY_BYTES + 1, |s| s.len());
        let mut size = 32 + roots.iter().map(|(root, paths)| json_len(root) + 4 + paths.iter().map(|p| json_len(p) + 1).sum::<usize>()).sum::<usize>();
        for (root, cache) in &self.roots {
            if roots.contains_key(root) {
                continue;
            }
            let mut root_size = json_len(root) + 4;
            let mut paths = BTreeSet::new();
            let mut overflow = root.len() > 4096;
            for path in cache.index.inventory() {
                if overflow {
                    break;
                }
                if !source(path) || !owned(path, root) { continue; }
                root_size += json_len(path) + 1;
                overflow = size + root_size > MAX_INVENTORY_BYTES || paths.len() >= MAX_FILES;
                paths.insert(path.to_owned());
            }
            // A root over the bounded format is left out, so no older inventory of it survives; the others keep theirs.
            if !overflow && size + root_size <= MAX_INVENTORY_BYTES {
                size += root_size;
                roots.insert(root.clone(), paths);
            }
        }
        if roots.len() > MAX_ROOTS { roots.clear(); }
        let value = serde_json::json!({"version": 1, "roots": roots}).to_string();
        if value.len() <= MAX_INVENTORY_BYTES { value } else { r#"{"version":1,"roots":{}}"#.into() }
    }
    pub fn clear(&mut self) {
        self.roots.clear();
        self.unidentified.reset();
        self.restored.clear();
        self.inventory = None;
        self.inventory_loaded = true;
        self.checkpoint();
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::*;
    use openidea_plugin::mock::MockHost;
    use openidea_plugin::views::{Body, ViewEvent, Views};
    fn doc(path: &str, text: &str) -> Document {
        Document { path: path.into(), language: "java".into(), version: 1, text: text.into() }
    }
    fn batch(path: &str, removed: bool) -> Event {
        Event::FilesChanged(openidea_plugin::FilesChanged {
            modified: if removed { vec![] } else { vec![path.into()] },
            removed: if removed { vec![path.into()] } else { vec![] },
            ..Default::default()
        })
    }
    #[test]
    fn providers_and_render_stay_cold_until_explicit_lifecycle_warm() {
        let host = MockHost::install();
        host.add_file("src/Hidden.java", "@org.springframework.stereotype.Component class Hidden {}");
        let mut nav = Navigator::default();
        let source = doc("/project/src/Consumer.java", "@org.springframework.stereotype.Component class Consumer { @org.springframework.beans.factory.annotation.Autowired Hidden hidden; }");
        assert!(
            nav.line_markers(&source)
                .iter()
                .all(|marker| marker.target.as_ref().is_none_or(|target| !target.path.ends_with("Hidden.java")))
        );
        let shown = nav.render(BEANS_VIEW, None, &host.invocation()).unwrap();
        assert!(shown.status.as_deref().unwrap().contains("Not scanned yet"));
        assert!(!nav.indexes.cached(&host.invocation().project().unwrap()).unwrap().files().contains_key("/project/src/Hidden.java"));
        nav.handle_events(&[Event::FileOpened("/project/src/Hidden.java".into())], &host.invocation());
        assert!(nav.indexes.cached(&host.invocation().project().unwrap()).unwrap().files().contains_key("/project/src/Hidden.java"));
        host.add_file("src/Later.java", "@org.springframework.stereotype.Component class Later {}");
        nav.render(BEANS_VIEW, None, &host.invocation()).unwrap();
        nav.line_markers(&source);
        assert!(!nav.indexes.cached(&host.invocation().project().unwrap()).unwrap().files().contains_key("/project/src/Later.java"));
        nav.run_command("reindex", &host.invocation(), None).unwrap();
        assert!(nav.indexes.cached(&host.invocation().project().unwrap()).unwrap().files().contains_key("/project/src/Later.java"));
    }
    #[test]
    fn one_reindex_scans_the_whole_project_in_a_task_and_says_so() {
        let host = MockHost::install();
        for i in 0..40 {
            host.add_file(&format!("src/C{i}.java"), &format!("@org.springframework.stereotype.Component class C{i} {{}}"));
        }
        let mut nav = Navigator::default();
        let message = nav.run_command("reindex", &host.invocation(), None).unwrap();
        assert_eq!(message.as_deref(), Some("Spring Beans finished reindexing this project."));
        let project = host.invocation().project().unwrap();
        assert!(nav.indexes.cached(&project).unwrap().complete());
        assert_eq!(nav.indexes.cached(&project).unwrap().files().len(), 40);
        assert!(host.tasks().iter().any(|task| task.title == "Scanning Spring beans" && task.ended));
    }
    #[test]
    fn unrelated_events_do_not_cold_scan_and_updates_removals_are_owned() {
        let host = MockHost::install();
        host.add_file("src/A.java", "@org.springframework.stereotype.Component class A {}");
        let mut nav = Navigator::default();
        nav.handle_events(&[batch("/project/README.md", false)], &host.invocation());
        assert!(nav.indexes.cached(&host.invocation().project().unwrap()).is_none());
        nav.run_command("reindex", &host.invocation(), None).unwrap();
        host.add_file("src/A.java", "@org.springframework.stereotype.Component(\"renamed\") class A {}");
        nav.handle_events(&[batch("/project/src/A.java", false)], &host.invocation());
        let project = host.invocation().project().unwrap();
        assert_eq!(nav.indexes.cached(&project).unwrap().files()["/project/src/A.java"].beans[0].name, "renamed");
        nav.handle_events(&[batch("/project/src", true)], &host.invocation());
        assert!(nav.indexes.cached(&project).unwrap().files().is_empty());
        assert!(!nav.indexes.changed(&project, "/project/../outside.java", false));
        assert!(!nav.indexes.changed(&project, "/project-other/A.java", false));
    }
    #[test]
    fn unreadable_or_oversized_change_drops_stale_file_retains_other_positives() {
        let host = MockHost::install();
        host.add_file("src/A.java", "@org.springframework.stereotype.Component class A {}");
        host.add_file("src/B.java", "@org.springframework.stereotype.Component class B {}");
        let project = host.invocation().project().unwrap();
        let mut indexes = Indexes::default();
        let index = indexes.get(&project, COMMAND_BUDGET);
        assert!(index.complete());
        index.update("/project/src/Gone.java", "@org.springframework.stereotype.Component class Gone {}");
        host.add_file_bytes("src/Gone.java", b"class Gone { String s = \"caf\xe9\"; }");
        assert!(indexes.changed(&project, "/project/src/Gone.java", false));
        let index = indexes.cached(&project).unwrap();
        assert!(!index.complete());
        assert!(!index.files().contains_key("/project/src/Gone.java"));
        assert!(index.files().contains_key("/project/src/B.java"));
        host.add_file("src/A.java", &"x".repeat(MAX_TEXT + 1));
        indexes.changed(&project, "/project/src/A.java", false);
        let index = indexes.cached(&project).unwrap();
        assert!(!index.files().contains_key("/project/src/A.java"));
        assert!(index.files().contains_key("/project/src/B.java"));
        assert!(index.state().contains("couldn't be read"));
    }
    #[test]
    fn caps_stale_and_denied_discovery_never_prove_absence() {
        let host = MockHost::install();
        for n in 0..10_000 {
            host.add_file(&format!("src/C{n}.java"), "");
        }
        let project = host.invocation().project().unwrap();
        let mut index = Cache::new();
        index.index.scan(&project, COMMAND_BUDGET);
        assert!(!index.complete());
        assert!(index.state().contains("too many files"));
        let host = MockHost::install().with_capabilities(&[]);
        let project = host.invocation().project().unwrap();
        let mut index = Cache::new();
        index.update("/project/src/Positive.java", "@org.springframework.stereotype.Component class Positive {}");
        index.index.scan(&project, COMMAND_BUDGET);
        assert!(!index.complete());
        assert!(index.files().contains_key("/project/src/Positive.java"));
    }
    #[test]
    fn nested_project_and_closed_root_lifecycle_do_not_mix_sources_or_reports() {
        let host = MockHost::install();
        host.add_project("/project/inner");
        host.add_file("/project/src/Outer.java", "@org.springframework.stereotype.Component class Outer {}");
        host.add_file("/project/inner/src/Inner.java", "@org.springframework.stereotype.Component class Inner {}");
        let project = host.invocation().project().unwrap();
        let mut nav = Navigator::default();
        nav.indexes.get(&project, COMMAND_BUDGET);
        assert!(!nav.indexes.cached(&project).unwrap().files().contains_key("/project/inner/src/Inner.java"));
        nav.report = Report::Dump("outer-only-secret-report".into());
        nav.report_root = Some("/project".into());
        nav.sections.set(REPORT_SECTION);
        host.set_project_root(Some("/other"));
        let shown = nav.render(BEANS_VIEW, None, &host.invocation()).unwrap();
        assert!(!format!("{:?}", shown.body).contains("outer-only-secret-report"));
        nav.handle_events(&[], &host.invocation());
        assert!(!nav.indexes.roots.contains_key("/project"));
        assert!(nav.report_root.is_none());
        let stale = nav.indexes.get(&project, COMMAND_BUDGET);
        assert!(stale.files().is_empty() && !stale.complete());
        assert!(!nav.indexes.roots.contains_key("/project"));
    }
    #[test]
    fn fabricated_or_foreign_ui_locations_are_not_action_members() {
        let host = MockHost::install();
        host.add_file("src/A.java", "@org.springframework.stereotype.Component class A {}");
        let mut nav = Navigator::default();
        nav.run_command("reindex", &host.invocation(), None).unwrap();
        for key in ["b:0:0:/other/A.java", "b:0:0:/project/unknown.java", "b:no:0:/project/src/A.java"] {
            assert!(nav.handle(BEANS_VIEW, ViewEvent::Activate(key.into()), &host.invocation()).is_err());
        }
        assert!(host.opened_locations().is_empty());
    }
    #[test]
    fn overflow_batches_drop_cache_and_never_report_empty_complete() {
        let host = MockHost::install();
        host.add_file("src/A.java", "@org.springframework.stereotype.Component class A {}");
        let mut nav = Navigator::default();
        nav.run_command("reindex", &host.invocation(), None).unwrap();
        nav.handle_events(&vec![Event::FileSaved("/project/src/A.java".into()); 257], &host.invocation());
        assert!(nav.indexes.cached(&host.invocation().project().unwrap()).is_none());
        let shown = nav.render(BEANS_VIEW, None, &host.invocation()).unwrap();
        assert!(matches!(shown.body,Body::Message(message) if message.contains("Not scanned yet")));
    }
    #[test]
    fn partial_report_never_renders_a_clean_bill_and_build_changes_invalidate() {
        let host = MockHost::install();
        host.add_file("src/A.java", "@org.springframework.stereotype.Component class A {}");
        let project = host.invocation().project().unwrap();
        let mut nav = Navigator::default();
        nav.run_command("reindex", &host.invocation(), None).unwrap();
        nav.report = Report::Problems(vec![]);
        nav.report_root = Some("/project".into());
        nav.report_incomplete = true;
        nav.sections.set(REPORT_SECTION);
        let shown = nav.render(BEANS_VIEW, None, &host.invocation()).unwrap();
        assert!(matches!(shown.body,Body::Message(message) if message.contains("the scan isn't finished")));
        assert!(shown.toolbar.iter().any(|button| button.id == "check"), "partial results can be checked again");
        assert!(nav.indexes.changed(&project, "/project/build.gradle.kts", false));
        assert!(!nav.indexes.cached(&project).unwrap().complete());
    }
    #[test]
    fn retained_source_budget_is_bounded_and_recovery_is_accounted() {
        let _host = MockHost::install();
        let mut index = Cache::new();
        let source = format!("@org.springframework.stereotype.Component class C {{}} /*{}*/", "x".repeat(MAX_TEXT - 61));
        let fit = MAX_CACHE_BYTES / source.len();
        for n in 0..fit + 2 {
            index.update(&format!("/project/src/C{n}.java"), &source);
        }
        assert_eq!(index.files().len(), fit);
        assert_eq!(index.index.unreadable(), 2);
        assert!(!index.complete());
        index.remove("/project/src/C0.java");
        index.update(&format!("/project/src/C{fit}.java"), &source);
        assert!(index.files().contains_key(&format!("/project/src/C{fit}.java")), "a removal frees its bytes");
        index.remove_tree("/project/src");
        assert_eq!(index.index.unreadable(), 0);
        assert!(index.files().is_empty());
    }
}

#[cfg(test)]
mod persistence_tests {
    use super::*;
    use crate::COMMAND_BUDGET;
    use openidea_plugin::mock::MockHost;
    fn stored(root: &str, paths: &[&str]) -> String {
        serde_json::json!({"version": 1, "roots": {root: paths}}).to_string()
    }
    #[test]
    fn restart_restores_only_reread_positives_and_requires_fresh_discovery() {
        let host = MockHost::install();
        host.add_file("src/A.java", "@org.springframework.stereotype.Component class A {}");
        let project = host.invocation().project().unwrap();
        let mut first = Indexes::default();
        assert!(first.get(&project, COMMAND_BUDGET).complete());
        let saved = host.storage()[INVENTORY_KEY].clone();
        assert!(saved.contains("/project/src/A.java"));
        assert!(!saved.contains("@org.springframework.stereotype.Component"));
        host.add_file("src/A.java", "@org.springframework.stereotype.Component(\"fresh\") class A {}");
        host.add_file("src/New.java", "@org.springframework.stereotype.Component class New {}");
        let mut restarted = Indexes::default();
        let index = restarted.get(&project, COMMAND_BUDGET);
        assert_eq!(index.files()["/project/src/A.java"].beans[0].name, "fresh");
        assert!(!index.files().contains_key("/project/src/New.java"));
        assert!(!index.complete());
        assert!(index.state().contains("Not every source file"));
        let index = restarted.get(&project, COMMAND_BUDGET);
        assert!(index.complete());
        assert!(index.files().contains_key("/project/src/New.java"));
    }
    #[test]
    fn zero_budget_requests_never_restore_or_write_storage() {
        let host = MockHost::install();
        host.add_file("src/A.java", "@org.springframework.stereotype.Component class A {}");
        let saved = stored("/project", &["/project/src/A.java"]);
        host::storage::set(INVENTORY_KEY, &saved).unwrap();
        let project = host.invocation().project().unwrap();
        let mut indexes = Indexes::default();
        assert!(indexes.get(&project, Duration::ZERO).files().is_empty());
        assert!(indexes.cached(&project).unwrap().files().is_empty());
        assert_eq!(host.storage()[INVENTORY_KEY], saved);
        assert!(indexes.get(&project, COMMAND_BUDGET).files().contains_key("/project/src/A.java"));
    }
    #[test]
    fn corrupt_version_traversal_duplicates_and_caps_are_rejected() {
        for value in ["{".into(), stored("/project", &["/project/../A.java"]),
            stored("/project", &["/other/A.java"]), stored("/project", &["/project/A.java", "/project/A.java"]),
            stored("/project", &["/project/readme.md"]), "x".repeat(MAX_INVENTORY_BYTES + 1),
            r#"{"version":2,"roots":{"/project":[]}}"#.into()] {
            assert!(inventory_paths(&value, "/project").is_none());
        }
        assert!(inventory_paths(&stored("/other", &["/other/A.java"]), "/project").is_none());
    }
    #[test]
    fn nested_root_and_missing_paths_cannot_restore_stale_facts() {
        let host = MockHost::install();
        host.add_project("/project/inner");
        host.add_file("/project/inner/A.java", "@org.springframework.stereotype.Component class Inner {}");
        host::storage::set(INVENTORY_KEY, &stored("/project", &["/project/inner/A.java", "/project/Gone.java"])).unwrap();
        let project = host.invocation().project().unwrap();
        let mut indexes = Indexes::default();
        let index = indexes.get(&project, COMMAND_BUDGET);
        assert!(index.files().is_empty());
        assert!(!index.complete());
    }
    #[test]
    fn filtered_removal_build_reset_and_closed_root_checkpoint_invalidate_inventory() {
        let host = MockHost::install();
        host.add_file("src/A.java", "@org.springframework.stereotype.Component class A {}");
        let project = host.invocation().project().unwrap();
        let mut indexes = Indexes::default();
        indexes.get(&project, COMMAND_BUDGET);
        assert!(!indexes.changed(&project, "/project/readme.md", false));
        indexes.changed(&project, "/project/src", true);
        indexes.checkpoint();
        assert_eq!(inventory_paths(&host.storage()[INVENTORY_KEY], "/project"), Some(vec![]));
        indexes.get(&project, COMMAND_BUDGET).update("/project/src/A.java", "@org.springframework.stereotype.Component class A {}");
        indexes.changed(&project, "/project/pom.xml", false);
        assert_eq!(inventory_paths(&host.storage()[INVENTORY_KEY], "/project"), Some(vec![]));
        host.set_project_root(Some("/other"));
        indexes.prune();
        indexes.checkpoint();
        assert!(inventory_paths(&host.storage()[INVENTORY_KEY], "/project").is_none());
    }
    #[test]
    fn oversized_checkpoint_discards_old_inventory_and_clear_persists_invalidation() {
        let host = MockHost::install();
        let project = host.invocation().project().unwrap();
        host::storage::set(INVENTORY_KEY, &stored("/project", &["/project/Old.java"])).unwrap();
        let mut indexes = Indexes::default();
        let index = indexes.ensure("/project");
        for n in 0..80 {
            index.update(&format!("/project/{}{}.java", "a".repeat(3500), n), "@org.springframework.stereotype.Component class A {}");
        }
        indexes.checkpoint();
        let value = &host.storage()[INVENTORY_KEY];
        assert!(value.len() <= MAX_INVENTORY_BYTES);
        assert!(inventory_paths(value, "/project").is_none());
        indexes.clear();
        assert!(inventory_paths(&host.storage()[INVENTORY_KEY], &project.root().unwrap()).is_none());
    }
    #[test]
    fn one_oversized_root_does_not_erase_the_other_roots_inventory() {
        let host = MockHost::install();
        host.add_project("/second");
        let mut indexes = Indexes::default();
        indexes.ensure("/second").update("/second/B.java", "@org.springframework.stereotype.Component class B {}");
        let index = indexes.ensure("/project");
        for n in 0..80 {
            index.update(&format!("/project/{}{}.java", "a".repeat(3500), n), "@org.springframework.stereotype.Component class A {}");
        }
        indexes.checkpoint();
        let value = &host.storage()[INVENTORY_KEY];
        assert!(inventory_paths(value, "/project").is_none());
        assert_eq!(inventory_paths(value, "/second"), Some(vec!["/second/B.java".into()]));
    }
    #[test]
    fn first_warm_checkpoint_preserves_another_open_roots_pending_inventory() {
        let host = MockHost::install();
        host.add_project("/second");
        host.add_file("/project/A.java", "@org.springframework.stereotype.Component class A {}");
        host.add_file("/second/B.java", "@org.springframework.stereotype.Component class B {}");
        host::storage::set(INVENTORY_KEY, &serde_json::json!({"version":1,"roots":{
            "/project":["/project/A.java"], "/second":["/second/B.java"]}}).to_string()).unwrap();
        let mut indexes = Indexes::default();
        let first = host.invocation().project().unwrap();
        assert!(!indexes.get(&first, COMMAND_BUDGET).complete());
        assert_eq!(inventory_paths(&host.storage()[INVENTORY_KEY], "/second"), Some(vec!["/second/B.java".into()]));
        let second = host::projects().unwrap().into_iter().find(|project| project.root().unwrap() == "/second").unwrap();
        let index = indexes.get(&second, COMMAND_BUDGET);
        assert!(index.files().contains_key("/second/B.java"));
        assert!(!index.complete());
    }
    #[test]
    fn storage_denial_quietly_preserves_in_memory_warming() {
        let host = MockHost::install().with_capabilities(&["project.read", "editor.read"]);
        host.add_file("src/A.java", "@org.springframework.stereotype.Component class A {}");
        let project = host.invocation().project().unwrap();
        let mut indexes = Indexes::default();
        let index = indexes.get(&project, COMMAND_BUDGET);
        assert!(index.complete());
        assert!(index.files().contains_key("/project/src/A.java"));
        assert!(host.storage().is_empty());
    }
}
