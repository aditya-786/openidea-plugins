//! Bounded private source navigation; never exported as indexed Rename symbols.
use crate::indexer::{deployment_isolation, edges_in};
use openidea_plugin::providers::LineMarker;
use openidea_plugin::{Location, Position, Project, host};
use ui_kit::navigation::{Graph, Resolver};
const MAX_TEXT: usize = 128 << 10;
const MAX_BYTES: usize = 8 << 20;
const MAX_FILES: usize = 64;
const MAX_TARGET_READS: usize = 16;
fn yaml(path: &str) -> bool {
    path.ends_with(".yaml") || path.ends_with(".yml")
}
fn root(path: &str) -> Option<(Project, String)> {
    if path.len() > 4096
        || !path.starts_with('/')
        || path.contains('\\')
        || path.chars().any(char::is_control)
        || path.split('/').any(|p| matches!(p, "." | ".."))
    {
        return None;
    }
    let project = host::project_of(path)?;
    let root = project.root().ok()?;
    (!root.is_empty() && path.strip_prefix(root.trim_end_matches('/')).is_some_and(|s| s.starts_with('/'))).then_some((project, root))
}
struct Cached {
    graph: Graph,
    isolated: bool,
    bytes: usize,
}
#[derive(Default)]
pub(crate) struct Cache {
    files: crate::lru::Lru<(String, String), Cached>,
    bytes: usize,
    transformers: crate::indexer::TransformerFlags,
}
impl Cache {
    pub(crate) fn clear(&mut self) {
        self.files.clear();
        self.bytes = 0;
        self.transformers.clear();
    }
    pub(crate) fn forget_transformers(&mut self) {
        self.transformers.clear();
    }
    pub(crate) fn evict(&mut self, path: &str) {
        let keys: Vec<_> = self.files.keys().filter(|(_, p)| p == path).cloned().collect();
        for key in keys {
            self.remove(&key);
        }
    }
    fn remove(&mut self, key: &(String, String)) {
        if let Some(old) = self.files.get(key) {
            self.bytes = self.bytes.saturating_sub(old.bytes);
        }
        self.files.remove(key);
    }
    pub(crate) fn warm_file(&mut self, path: &str) {
        if !yaml(path) {
            return;
        }
        let Some((project, _)) = root(path) else {
            return;
        };
        if let Ok(text) = project.read_file(path) {
            self.warm(path, &text);
        } else {
            self.evict(path);
        }
    }
    pub(crate) fn warm(&mut self, path: &str, text: &str) {
        if !yaml(path) {
            return;
        }
        let Some((project, owner)) = root(path) else {
            return;
        };
        let key = (owner, path.into());
        self.remove(&key);
        if text.len() > MAX_TEXT || project.read_file(path).is_err() {
            return;
        }
        // Discovery runs only while warming (index, open, save, file changes), never in provider calls.
        let isolated = deployment_isolation(path, &mut self.transformers).unwrap_or(true);
        let graph: Graph = edges_in(path, text, isolated).into();
        let bytes = graph.definitions.iter().chain(&graph.references).chain(&graph.markers).map(|(key, _)| key.len() + 128).sum::<usize>()
            + path.len()
            + key.0.len();
        if bytes > MAX_BYTES || graph.definitions.is_empty() && graph.references.is_empty() {
            return;
        }
        while self.files.len() >= MAX_FILES || self.bytes + bytes > MAX_BYTES {
            let Some((_, old)) = self.files.pop_oldest() else { break };
            self.bytes = self.bytes.saturating_sub(old.bytes);
        }
        self.bytes += bytes;
        self.files.insert(key, Cached { graph, isolated, bytes }, usize::MAX);
    }
    fn source(&self, doc: &openidea_plugin::providers::Document) -> Option<(Project, String, Graph)> {
        if doc.language != "yaml" || !yaml(&doc.path) || doc.text.len() > MAX_TEXT {
            return None;
        }
        let (project, owner) = root(&doc.path)?;
        // Also enforces the optional current grant after a cache was warmed.
        // Project reads prefer visible editor text; current provider text below
        // remains authoritative for the calling document's unsaved ranges.
        project.read_file(&doc.path).ok()?;
        let isolated = self.files.get(&(owner.clone(), doc.path.clone())).map_or(true, |c| c.isolated);
        Some((project, owner, edges_in(&doc.path, &doc.text, isolated).into()))
    }
    pub(crate) fn definition(&self, doc: &openidea_plugin::providers::Document, at: Position) -> Vec<Location> {
        let Some((project, owner, source)) = self.source(doc) else {
            return Vec::new();
        };
        let files = std::iter::once((doc.path.as_str(), &source)).chain(
            self.files
                .iter()
                .filter(|((root, path), _)| root == &owner && path != &doc.path)
                .map(|((_, path), c)| (path.as_str(), &c.graph)),
        );
        let resolver = Resolver::new(files);
        let mut reads = 0;
        resolver
            .checked(|path| {
                if path == doc.path {
                    return Some(source.clone());
                }
                let cached = self.files.get(&(owner.clone(), path.into()))?;
                if reads == MAX_TARGET_READS {
                    return None;
                }
                reads += 1;
                let text = project.read_file(path).ok()?;
                if text.len() > MAX_TEXT {
                    return None;
                }
                Some(edges_in(path, &text, cached.isolated).into())
            })
            .definition(&source, at)
    }
    pub(crate) fn markers(&self, doc: &openidea_plugin::providers::Document) -> Vec<LineMarker> {
        let Some((project, owner, source)) = self.source(doc) else {
            return Vec::new();
        };
        let files = std::iter::once((doc.path.as_str(), &source)).chain(
            self.files
                .iter()
                .filter(|((root, path), _)| root == &owner && path != &doc.path)
                .map(|((_, path), c)| (path.as_str(), &c.graph)),
        );
        let resolver = Resolver::new(files);
        let mut reads = 0;
        let mut markers = resolver
            .checked(|path| {
                if path == doc.path {
                    return Some(source.clone());
                }
                let cached = self.files.get(&(owner.clone(), path.into()))?;
                if reads == MAX_TARGET_READS {
                    return None;
                }
                reads += 1;
                let text = project.read_file(path).ok()?;
                if text.len() > MAX_TEXT {
                    return None;
                }
                Some(edges_in(path, &text, cached.isolated).into())
            })
            .markers(&source);
        for marker in &mut markers {
            marker.tooltip = format!("{} · in project files; the cluster may differ", marker.tooltip);
        }
        markers
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::KubeExplorer;
    use openidea_plugin::{
        Event, Plugin,
        mock::MockHost,
        providers::{Document, Providers},
        text,
    };
    const SECRET: &str =
        "apiVersion: v1\nkind: Secret\nmetadata:\n  name: cert\n  namespace: app\ndata:\n  ignored: dont-cache-secret-values\n";
    const INGRESS: &str = "apiVersion: networking.k8s.io/v1\nkind: Ingress\nmetadata:\n  name: gateway\n  namespace: app\nspec:\n  tls:\n    - secretName: cert\n";
    const NS: &str = "apiVersion: v1\nkind: Namespace\nmetadata:\n  name: app\n";
    const CONFIG: &str = "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: config\n  namespace: app\ndata:\n  namespace: not-a-link\n";
    fn doc(path: &str, source: &str) -> Document {
        Document { path: format!("/project/{path}"), language: "yaml".into(), version: 4, text: source.into() }
    }
    fn warm(kube: &mut KubeExplorer, path: &str, source: &str) {
        kube.navigation.warm(&format!("/project/{path}"), source);
    }
    fn host() -> MockHost {
        MockHost::install().with_capabilities(&["project.read"])
    }
    #[test]
    fn actual_export_is_empty_for_the_audited_namespace_and_ingress_secret_fixtures() {
        let h = host();
        let mut kube = KubeExplorer::default();
        for (path, source) in [("secret.yaml", SECRET), ("ingress.yaml", INGRESS), ("namespace.yaml", NS), ("config.yaml", CONFIG)] {
            h.add_file(path, source);
            warm(&mut kube, path, source);
        }
        assert_eq!(kube.navigation.files.len(), 4);
        let src = doc("ingress.yaml", INGRESS);
        let p = text::position(INGRESS, INGRESS.rfind("cert").unwrap() + 1);
        let found = kube.definition(&src, p);
        assert_eq!(found.len(), 1);
        assert!(found[0].path.ends_with("secret.yaml"));
        let namespace = text::position(INGRESS, INGRESS.find("namespace: app").unwrap() + 12);
        let found = kube.definition(&src, namespace);
        assert_eq!(found.len(), 1);
        assert!(found[0].path.ends_with("namespace.yaml"));
        assert!(kube.line_markers(&src).iter().all(|m| m.tooltip.contains("the cluster may differ")));
        let config = doc("config.yaml", CONFIG);
        let pos = text::position(CONFIG, CONFIG.find("namespace: app").unwrap() + 12);
        assert_eq!(kube.definition(&config, pos).len(), 1);
        let data = text::position(CONFIG, CONFIG.find("not-a-link").unwrap() + 1);
        assert!(kube.definition(&config, data).is_empty());
        assert!(
            kube.navigation
                .files
                .values()
                .flat_map(|c| c.graph.definitions.iter().chain(&c.graph.references).chain(&c.graph.markers))
                .all(|(key, _)| !key.contains("dont-cache-secret-values"))
        );

        assert!(h.process_runs().is_empty());
        assert!(h.requests().is_empty());
    }
    #[test]
    fn cold_same_stream_navigation_and_reverse_users_keep_exact_utf16_source_ranges() {
        let h = host();
        let stream = format!("# 😀\r\n{NS}---\n{SECRET}---\n{INGRESS}");
        h.add_file("all.yaml", &stream);
        let mut kube = KubeExplorer::default();
        let src = doc("all.yaml", &stream);
        let p = text::position(&stream, stream.rfind("cert").unwrap() + 1);
        let found = kube.definition(&src, p);
        assert_eq!(found.len(), 1);
        let span = found[0].range;
        assert_eq!(&stream[text::offset(&stream, span.start).unwrap()..text::offset(&stream, span.end).unwrap()], "cert");
        let markers = kube.line_markers(&src);
        let target = text::position(&stream, stream.rfind("cert").unwrap());
        assert!(markers.iter().any(|m| m.target.as_ref().is_some_and(|t| t.range.start == target)));
        assert!(kube.navigation.files.is_empty(), "editor requests do not discover or populate caches");
    }
    #[test]
    fn cached_targets_are_revalidated_and_current_unsaved_source_is_authoritative() {
        let h = host();
        h.add_file("secret.yaml", SECRET);
        h.add_file("ingress.yaml", INGRESS);
        let mut kube = KubeExplorer::default();
        warm(&mut kube, "secret.yaml", SECRET);
        warm(&mut kube, "ingress.yaml", INGRESS);
        let src = doc("ingress.yaml", INGRESS);
        let p = text::position(INGRESS, INGRESS.rfind("cert").unwrap() + 1);
        assert_eq!(kube.definition(&src, p).len(), 1);
        h.open_editor("/project/secret.yaml", "yaml", &SECRET.replace("name: cert", "name: other"), &[]);
        assert!(kube.definition(&src, p).is_empty());
        let moved = format!("# 😀 moved\n{SECRET}");
        h.open_editor("/project/secret.yaml", "yaml", &moved, &[]);
        let found = kube.definition(&src, p);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].range.start.line, 4);
        let edited = INGRESS.replace("secretName: cert", "secretName: missing");
        let current = Document { text: edited.clone(), version: 5, ..src };
        let p = text::position(&edited, edited.rfind("missing").unwrap() + 1);
        assert!(kube.definition(&current, p).is_empty());
    }
    #[test]
    fn project_denial_other_roots_overlay_and_implicit_namespace_stay_isolated() {
        let h = host();
        h.add_file("overlays/prod/kustomization.yaml", "namespace: prod\n");
        h.add_file("secret.yaml", SECRET);
        h.add_file("ingress.yaml", INGRESS);
        let mut kube = KubeExplorer::default();
        warm(&mut kube, "secret.yaml", SECRET);
        warm(&mut kube, "ingress.yaml", INGRESS);
        let src = doc("ingress.yaml", INGRESS);
        let at = text::position(INGRESS, INGRESS.rfind("cert").unwrap() + 1);
        assert!(kube.definition(&src, at).is_empty());
        let other = Document { path: "/elsewhere/ingress.yaml".into(), ..src.clone() };
        assert!(kube.definition(&other, at).is_empty());
        let _h = h.with_capabilities(&[]);
        assert!(kube.definition(&src, at).is_empty());
        assert!(kube.line_markers(&src).is_empty());
    }
    #[test]
    fn cache_limits_invalidation_and_unknown_or_oversized_documents_are_conservative() {
        let h = host();
        let mut kube = KubeExplorer::default();
        for n in 0..MAX_FILES + 3 {
            let path = format!("s{n}.yaml");
            h.add_file(&path, SECRET);
            warm(&mut kube, &path, SECRET);
        }
        assert!(kube.navigation.files.len() <= MAX_FILES && kube.navigation.bytes <= MAX_BYTES);
        let removed = (0..MAX_FILES + 3).map(|n| format!("/project/s{n}.yaml")).collect();
        kube.handle_events(&[Event::FilesChanged(openidea_plugin::FilesChanged { removed, ..Default::default() })], &h.invocation());
        assert!(kube.navigation.files.is_empty());
        let huge = format!("{}{SECRET}", " ".repeat(MAX_TEXT));
        h.add_file("huge.yaml", &huge);
        warm(&mut kube, "huge.yaml", &huge);
        assert!(kube.navigation.files.is_empty());
        assert!(kube.definition(&doc("huge.yaml", &huge), Position::new(0, 1)).is_empty());
        let custom = INGRESS.replace("networking.k8s.io/v1", "unknown.example/v1");
        h.add_file("unknown.yaml", &custom);
        warm(&mut kube, "unknown.yaml", &custom);
        assert!(kube.navigation.files.is_empty());
        assert!(h.process_runs().is_empty());
    }
    #[test]
    fn files_changed_evicts_only_named_paths() {
        let h = host();
        let mut kube = KubeExplorer::default();
        for path in ["a.yaml", "b.yaml"] {
            h.add_file(path, SECRET);
            warm(&mut kube, path, SECRET);
        }
        assert_eq!(kube.navigation.files.len(), 2);
        let changed = openidea_plugin::FilesChanged { removed: vec!["/project/a.yaml".into()], ..Default::default() };
        kube.handle_events(&[Event::FilesChanged(changed)], &h.invocation());
        let left: Vec<_> = kube.navigation.files.keys().map(|(_, path)| path.as_str()).collect();
        assert_eq!(left, ["/project/b.yaml"]);
    }
    #[test]
    fn definition_into_a_never_opened_file_works_after_index() {
        use openidea_plugin::indexing::{Document as Indexed, DocumentIndexer};
        let h = host();
        h.add_file("secret.yaml", SECRET);
        h.add_file("ingress.yaml", INGRESS);
        let mut kube = KubeExplorer::default();
        let edges = kube.index(&Indexed { path: "/project/secret.yaml".into(), text: SECRET.into(), symbols: Vec::new() });
        assert!(edges.definitions.is_empty() && edges.references.is_empty() && edges.markers.is_empty());
        kube.handle_events(&[Event::FileOpened("/project/ingress.yaml".into())], &h.invocation());
        let at = text::position(INGRESS, INGRESS.rfind("cert").unwrap() + 1);
        let found = kube.definition(&doc("ingress.yaml", INGRESS), at);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, "/project/secret.yaml");
    }
    #[test]
    fn transformer_flag_cached_until_files_changed() {
        let h = host();
        let mut kube = KubeExplorer::default();
        let isolated = |kube: &KubeExplorer, path: &str| kube.navigation.files.get(&("/project".to_owned(), format!("/project/{path}"))).map(|c| c.isolated);
        h.add_file("a.yaml", SECRET);
        warm(&mut kube, "a.yaml", SECRET);
        assert_eq!(isolated(&kube, "a.yaml"), Some(false));

        h.add_file("overlays/kustomization.yaml", "namespace: prod\n");
        h.add_file("b.yaml", SECRET);
        warm(&mut kube, "b.yaml", SECRET);
        assert_eq!(isolated(&kube, "b.yaml"), Some(false), "the per-root flag is not searched again");

        let changed = openidea_plugin::FilesChanged { created: vec!["/project/overlays/kustomization.yaml".into()], ..Default::default() };
        kube.handle_events(&[Event::FilesChanged(changed)], &h.invocation());
        h.add_file("c.yaml", SECRET);
        warm(&mut kube, "c.yaml", SECRET);
        assert_eq!(isolated(&kube, "c.yaml"), Some(true));
    }
    #[test]
    fn many_equal_source_candidates_do_not_turn_target_validation_into_an_unbounded_read() {
        let h = host();
        let mut kube = KubeExplorer::default();
        for n in 0..33 {
            let path = format!("targets/secret-{n}.yaml");
            h.add_file(&path, SECRET);
            warm(&mut kube, &path, SECRET);
        }
        h.add_file("ingress.yaml", INGRESS);
        warm(&mut kube, "ingress.yaml", INGRESS);
        let src = doc("ingress.yaml", INGRESS);
        let at = text::position(INGRESS, INGRESS.rfind("cert").unwrap() + 1);
        let found = kube.definition(&src, at);
        assert!(!found.is_empty());
        assert!(found.len() <= MAX_TARGET_READS);
        assert!(found.iter().all(|l| l.path.contains("/targets/secret-")));
        assert!(h.process_runs().is_empty());
    }
}
