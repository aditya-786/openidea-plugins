//! Lifecycle-owned physical paths only. Resource contents never establish QNames here.
use crate::*;
use openidea_plugin::Project;
use std::collections::BTreeMap;
const GLOBS: [&str; 4] = ["**/*.xsd", "**/*.wsdl", "**/*.xslt", "**/*.xml"];
const MAX_FILES: usize = 4096;
const MAX_FAMILY_BYTES: usize = 2 * 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 8 * 1024 * 1024;
const MAX_ROOTS: usize = 8;
const TTL: Duration = Duration::from_secs(120);
fn kind(path: &str) -> Option<usize> {
    [".xsd", ".wsdl", ".xslt", ".xml"]
        .iter()
        .position(|ext| path.ends_with(ext))
}
pub(crate) fn relevant(path: &str) -> bool {
    kind(path).is_some()
}
fn normalized(path: &str) -> bool {
    let bytes = path.as_bytes();
    let absolute = path.starts_with('/')
        || (bytes.first().is_some_and(u8::is_ascii_alphabetic) && bytes.get(1..3) == Some(b":/"));
    absolute
        && path.len() <= 4096
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
        && !path.split('/').any(|p| matches!(p, "." | ".."))
}
fn owned(path: &str, root: &str) -> bool {
    normalized(path) && path.starts_with(&format!("{}/", root.trim_end_matches('/')))
}
#[derive(Default)]
struct Family {
    paths: BTreeSet<String>,
    bytes: usize,
    scanned: Option<Instant>,
    complete: bool,
}
impl Family {
    fn ready(&self) -> bool {
        self.complete && self.scanned.is_some_and(|at| at.elapsed() <= TTL)
    }
    fn clear(&mut self) {
        *self = Self::default();
    }
}
struct Root {
    families: [Family; 4],
    touched: Instant,
}
impl Root {
    fn new() -> Self {
        Self {
            families: std::array::from_fn(|_| Family::default()),
            touched: Instant::now(),
        }
    }
}
#[derive(Default)]
pub(crate) struct Paths {
    roots: BTreeMap<String, Root>,
}
impl Paths {
    fn bytes(&self) -> usize {
        self.roots
            .values()
            .flat_map(|r| &r.families)
            .map(|f| f.bytes)
            .sum()
    }
    pub(crate) fn reset(&mut self) {
        self.roots.clear();
    }
    /// Discovery runs from activation, file events, providers and refresh. Each extension has independent
    /// completeness, so capped XML/policy paths cannot corrupt XSD QName authority.
    pub(crate) fn warm(&mut self, project: &Project, budget: Duration) {
        if budget.is_zero() {
            return;
        }
        let Ok(root) = project.root() else {
            return;
        };
        if !normalized(&root) {
            return;
        }
        if !self.roots.contains_key(&root) {
            if self.roots.len() >= MAX_ROOTS {
                if let Some(old) = self
                    .roots
                    .iter()
                    .min_by_key(|(_, r)| r.touched)
                    .map(|(k, _)| k.clone())
                {
                    self.roots.remove(&old);
                }
            }
            self.roots.insert(root.clone(), Root::new());
        }
        self.roots.get_mut(&root).unwrap().touched = Instant::now();
        let start = Instant::now();
        for (index, glob) in GLOBS.iter().enumerate() {
            if start.elapsed() >= budget {
                return;
            }
            if self.roots[&root].families[index].ready() {
                continue;
            }
            let mut paths = BTreeSet::new();
            let mut bytes = 0;
            let complete = match project.find_files(glob, (MAX_FILES + 1) as u32) {
                Ok(found) if found.len() <= MAX_FILES => {
                    let mut valid = true;
                    for path in found {
                        if !owned(&path, &root) {
                            valid = false;
                            break;
                        }
                        if paths.insert(path.clone()) {
                            bytes += path.len();
                        }
                        if bytes > MAX_FAMILY_BYTES {
                            valid = false;
                            break;
                        }
                    }
                    valid
                }
                _ => false,
            };
            let before = self.roots[&root].families[index].bytes;
            let within_total =
                self.bytes().saturating_sub(before).saturating_add(bytes) <= MAX_TOTAL_BYTES;
            let family = &mut self.roots.get_mut(&root).unwrap().families[index];
            if complete && within_total {
                *family = Family {
                    paths,
                    bytes,
                    scanned: Some(Instant::now()),
                    complete: true,
                };
            } else {
                family.clear();
            }
        }
    }
    pub(crate) fn observe(&mut self, project: &Project, path: &str, removed: bool) {
        let Some(index) = kind(path) else {
            return;
        };
        let Ok(root) = project.root() else {
            return;
        };
        let total = self.bytes();
        let Some(state) = self.roots.get_mut(&root) else {
            return;
        };
        state.touched = Instant::now();
        if !owned(path, &root) {
            for family in &mut state.families {
                family.clear();
            }
            return;
        }
        let family = &mut state.families[index];
        if !family.ready() {
            return;
        }
        if !removed {
            // File watcher paths can name directories or already-deleted files.
            // Only scoped metadata, never resource content, admits a positive.
            match project.stat(path) {
                Ok(stat) if stat.kind == openidea_plugin::FileKind::File => {}
                Ok(_) | Err(Error::NotFound(_)) => {
                    if family.paths.remove(path) {
                        family.bytes = family.bytes.saturating_sub(path.len());
                    }
                    return;
                }
                Err(_) => {
                    family.clear();
                    return;
                }
            }
        }
        if removed {
            if family.paths.remove(path) {
                family.bytes = family.bytes.saturating_sub(path.len());
            }
        } else if !family.paths.contains(path) {
            if family.paths.len() >= MAX_FILES
                || family.bytes + path.len() > MAX_FAMILY_BYTES
                || total + path.len() > MAX_TOTAL_BYTES
            {
                family.clear();
                return;
            }
            family.paths.insert(path.into());
            family.bytes += path.len();
        }
        // Observing one path does not renew the discovery snapshot's TTL.
    }
    pub(crate) fn ready(&self, root: &str) -> bool {
        self.roots.get(root).is_some_and(|state| state.families.iter().all(Family::ready))
    }
    #[cfg(test)]
    pub(crate) fn expire(&mut self) {
        let old = Instant::now().checked_sub(TTL * 2);
        for family in self.roots.values_mut().flat_map(|root| &mut root.families) {
            family.scanned = old;
        }
    }
    pub(crate) fn paths(&self, root: &str, relative: &str) -> Option<&BTreeSet<String>> {
        let family = &self.roots.get(root)?.families[kind(relative)?];
        family.ready().then_some(&family.paths)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    #[test]
    fn independent_path_families_ignore_content_and_keep_scope_and_freshness() {
        let h = MockHost::install();
        h.add_file("src/main/resources/policy.xml", &"not XML ".repeat(40000));
        h.add_file(
            "src/main/resources/request.xslt",
            "not an executable stylesheet",
        );
        let project = h.invocation().project().unwrap();
        let mut paths = Paths::default();
        paths.warm(&project, COMMAND_BUDGET);
        assert!(
            paths
                .paths("/project", "policy.xml")
                .unwrap()
                .contains("/project/src/main/resources/policy.xml")
        );
        assert!(paths.paths("/other", "policy.xml").is_none());
        paths.roots.get_mut("/project").unwrap().families[3].scanned =
            Some(Instant::now() - TTL - Duration::from_secs(1));
        assert!(paths.paths("/project", "policy.xml").is_none());
        assert!(
            paths.paths("/project", "request.xslt").is_some(),
            "one expired family does not invalidate another"
        );
        assert!(h.process_runs().is_empty() && h.requests().is_empty());
    }
    #[test]
    fn capped_xml_family_does_not_poison_xsd_path_authority() {
        let h = MockHost::install();
        h.add_file("src/main/resources/known.xsd", "<schema/>");
        for i in 0..=MAX_FILES {
            h.add_file(&format!("src/main/resources/p{i}.xml"), "");
        }
        let project = h.invocation().project().unwrap();
        let mut paths = Paths::default();
        paths.warm(&project, COMMAND_BUDGET);
        assert!(paths.paths("/project", "p0.xml").is_none());
        assert!(
            paths
                .paths("/project", "known.xsd")
                .unwrap()
                .contains("/project/src/main/resources/known.xsd")
        );
    }
    #[test]
    fn filtered_observed_paths_add_remove_without_renewing_discovery_or_leaking_invalid_owner() {
        let h = MockHost::install();
        let project = h.invocation().project().unwrap();
        let mut paths = Paths::default();
        paths.warm(&project, COMMAND_BUDGET);
        let before = paths.roots["/project"].families[2].scanned;
        let resource = "/project/src/main/resources/new.xslt";
        h.add_file(resource, "opaque resource text is not read");
        paths.observe(&project, resource, false);
        assert!(
            paths
                .paths("/project", "new.xslt")
                .unwrap()
                .contains(resource)
        );
        assert_eq!(paths.roots["/project"].families[2].scanned, before);
        paths.observe(&project, resource, true);
        assert!(
            !paths
                .paths("/project", "new.xslt")
                .unwrap()
                .contains(resource)
        );
        paths.observe(
            &project,
            "/project/other/../src/main/resources/new.xslt",
            false,
        );
        assert!(paths.paths("/project", "new.xslt").is_none());
    }
    #[test]
    fn root_inventory_is_bounded_and_cold_after_reset() {
        let h = MockHost::install();
        let mut paths = Paths::default();
        for i in 0..=MAX_ROOTS {
            let root = format!("/root{i}");
            h.add_project(&root);
            let project = host::project_of(&format!("{root}/context.xml")).unwrap();
            paths.warm(&project, COMMAND_BUDGET);
        }
        assert_eq!(paths.roots.len(), MAX_ROOTS);
        assert!(paths.paths("/root0", "x.xml").is_none());
        assert!(paths.paths(&format!("/root{MAX_ROOTS}"), "x.xml").is_some());
        paths.reset();
        assert!(paths.paths(&format!("/root{MAX_ROOTS}"), "x.xml").is_none());
    }
}
