//! Local Kustomization graph checks on the current source: directory references
//! must hold a kustomization, cycles are bounded, and inline `patches` /
//! `replacements` selectors are matched against the plain local `resources` of
//! the same kustomization. Remote, generated, regex and dynamic forms are refused.
use crate::kustomize::{Reference, local, named, owner, references, target};
use crate::label_refs::{Node, Scalar, parse};
use openidea_plugin::providers::Document;
use openidea_plugin::{Diagnostic, Error, FileKind, Location, Position, Project, Range, Severity, host, text};
use crate::lru::Lru;
use std::collections::{BTreeMap, BTreeSet};

const LIMIT: usize = 128 << 10;
const NAMES: [&str; 3] = ["kustomization.yaml", "kustomization.yml", "Kustomization"];
const MAX_DEPTH: usize = 8;
const MAX_READS: usize = 64;
const MAX_RESOURCES: usize = 256;
const MAX_KUSTOMIZATIONS: usize = 64;

fn remote(value: &str) -> bool {
    value.contains("://")
        || value.starts_with("git@")
        || value.ends_with(".git")
        || value.contains(".git/")
        || value.split_once('/').is_some_and(|(host, _)| {
            !host.starts_with('.')
                && host
                    .rsplit_once('.')
                    .is_some_and(|(_, tld)| tld.len() >= 2 && tld.chars().all(|c| c.is_ascii_lowercase()))
        })
}

fn reference_message(project: &Project, root: &str, doc_path: &str, r: &Reference) -> Option<String> {
    let path = local(root, doc_path, &r.value)?;
    let owned = host::project_of(&path).and_then(|p| p.root().ok()).as_deref() == Some(root);
    match project.stat(&path) {
        Err(Error::NotFound(_)) if !remote(&r.value) => Some(format!("kustomization not found: {} does not exist", r.value)),
        Ok(stat) if stat.kind == FileKind::Directory && owned => {
            let candidates: Vec<_> = NAMES
                .iter()
                .map(|n| format!("{path}/{n}"))
                .filter(|p| project.stat(p).is_ok_and(|s| s.kind == FileKind::File))
                .collect();
            match candidates.as_slice() {
                [] => Some(format!("kustomization not found in {}", r.value)),
                [only] => cycle(project, root, doc_path, only).map(|chain| format!("Kustomization cycle: {chain}")),
                _ => None,
            }
        }
        _ => None,
    }
}

fn diagnostic(source: &str, r: &Reference, message: String) -> Diagnostic {
    let range = Range::new(text::position(source, r.start), text::position(source, r.end));
    Diagnostic { code: Some("kustomize-reference".into()), ..Diagnostic::new(range, Severity::Warning, &message) }
}

fn cycle(project: &Project, root: &str, start: &str, first: &str) -> Option<String> {
    let mut chain = vec![start.to_owned()];
    let mut seen = BTreeSet::new();
    walk(project, root, start, first, &mut chain, &mut seen).then(|| {
        chain
            .iter()
            .map(|p| p.rsplit('/').nth(1).unwrap_or(p.as_str()).to_owned())
            .collect::<Vec<_>>()
            .join(" -> ")
    })
}

fn walk(project: &Project, root: &str, start: &str, node: &str, chain: &mut Vec<String>, seen: &mut BTreeSet<String>) -> bool {
    if node == start {
        chain.push(node.to_owned());
        return true;
    }
    if chain.len() > MAX_DEPTH || seen.len() >= MAX_READS || !seen.insert(node.to_owned()) {
        return false;
    }
    let Ok(source) = project.read_file(node) else {
        return false;
    };
    if source.len() > LIMIT {
        return false;
    }
    chain.push(node.to_owned());
    let doc = Document { path: node.into(), language: "yaml".into(), version: 0, text: source };
    for r in references(&doc).into_iter().filter(|r| r.graph) {
        if let Some(next) = target(project, root, node, &r.value).filter(|p| named(p))
            && walk(project, root, start, &next, chain, seen)
        {
            return true;
        }
    }
    chain.pop();
    false
}

struct Res {
    path: String,
    doc: usize,
    group: String,
    version: String,
    kind: String,
    name: String,
    namespace: String,
    labels: Vec<(String, String)>,
    annotations: Vec<(String, String)>,
    kind_range: Range,
    name_range: Range,
    api_range: Range,
    namespace_range: Option<Range>,
}

fn strings(node: Option<&Node>) -> Option<Vec<(String, String)>> {
    let Some(node) = node else {
        return Some(vec![]);
    };
    node.pairs()?
        .iter()
        .map(|(k, v)| {
            let value = &v.scalar()?.value;
            (!k.value.is_empty() && !value.is_empty()).then(|| (k.value.clone(), value.clone()))
        })
        .collect()
}

fn resource_files(top: &Node) -> Option<&[Node]> {
    const REFUSED: [&str; 9] = [
        "bases", "components", "generators", "transformers", "configMapGenerator", "secretGenerator", "helmCharts", "crds", "helmGlobals",
    ];
    if top.pairs()?.iter().any(|(k, _)| REFUSED.contains(&k.value.as_str())) {
        return None;
    }
    let files = top.get("resources")?.items()?;
    (!files.is_empty() && files.len() <= MAX_READS).then_some(files)
}

fn resources_key(top: &Node) -> Option<String> {
    let values: Option<Vec<&str>> = resource_files(top)?.iter().map(|file| file.scalar().map(|s| s.value.as_str())).collect();
    Some(values?.join("\n"))
}

fn gather(project: &Project, root: &str, source: &str, top: &Node) -> Option<Vec<Res>> {
    let files = resource_files(top)?;
    let mut found = Vec::new();
    for file in files {
        let value = &file.scalar()?.value;
        let path = local(root, source, value)?;
        if !project.stat(&path).is_ok_and(|s| s.kind == FileKind::File) {
            return None;
        }
        let content = project.read_file(&path).ok()?;
        if content.len() > LIMIT {
            return None;
        }
        for (doc, node) in parse(&content)?.iter().enumerate() {
            let api = node.get("apiVersion")?.scalar()?;
            let kind = node.get("kind")?.scalar()?;
            let name = node.at(&["metadata", "name"])?.scalar()?;
            if api.value.is_empty() || kind.value.is_empty() || name.value.is_empty() || kind.value.ends_with("List") {
                return None;
            }
            let (group, version) = api.value.split_once('/').unwrap_or(("", api.value.as_str()));
            let namespace = node.at(&["metadata", "namespace"]).and_then(Node::scalar);
            found.push(Res {
                path: path.clone(),
                doc,
                group: group.into(),
                version: version.into(),
                kind: kind.value.clone(),
                name: name.value.clone(),
                namespace: namespace.map(|n| n.value.clone()).unwrap_or_default(),
                labels: strings(node.at(&["metadata", "labels"]))?,
                annotations: strings(node.at(&["metadata", "annotations"]))?,
                kind_range: kind.range,
                name_range: name.range,
                api_range: api.range,
                namespace_range: namespace.map(|n| n.range),
            });
            if found.len() > MAX_RESOURCES {
                return None;
            }
        }
    }
    Some(found)
}

type Pairs = Vec<(String, Option<String>)>;

#[derive(Default)]
struct Selector {
    group: Option<String>,
    version: Option<String>,
    kind: Option<String>,
    name: Option<String>,
    namespace: Option<String>,
    labels: Pairs,
    annotations: Pairs,
}

fn word(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn pairs(raw: &str) -> Option<Pairs> {
    raw.split(',')
        .map(|item| match item.split_once("==").or_else(|| item.split_once('=')) {
            Some((k, v)) if word(k) && word(v) => Some((k.into(), Some(v.into()))),
            None if word(item) => Some((item.into(), None)),
            _ => None,
        })
        .collect()
}

impl Selector {
    fn read(node: &Node, adds_metadata: bool) -> Option<Self> {
        let mut selector = Self::default();
        for (key, value) in node.pairs()? {
            let Node::Scalar(s) = value else {
                continue;
            };
            let slot = match key.value.as_str() {
                "group" => &mut selector.group,
                "version" => &mut selector.version,
                "kind" => &mut selector.kind,
                "name" => &mut selector.name,
                "namespace" => &mut selector.namespace,
                "labelSelector" | "annotationSelector" if adds_metadata => return None,
                "labelSelector" => {
                    selector.labels = pairs(&s.value)?;
                    continue;
                }
                "annotationSelector" => {
                    selector.annotations = pairs(&s.value)?;
                    continue;
                }
                _ => continue,
            };
            if !word(&s.value) {
                return None;
            }
            *slot = Some(s.value.clone());
        }
        let empty = selector.group.is_none()
            && selector.version.is_none()
            && selector.kind.is_none()
            && selector.name.is_none()
            && selector.namespace.is_none()
            && selector.labels.is_empty()
            && selector.annotations.is_empty();
        (!empty).then_some(selector)
    }

    fn matches(&self, r: &Res) -> bool {
        let has = |have: &[(String, String)], want: &Pairs| {
            want.iter().all(|(k, v)| have.iter().any(|(hk, hv)| hk == k && v.as_ref().is_none_or(|v| v == hv)))
        };
        // Selector values are anchored regexes; only '.' is a metacharacter in `word` values.
        let same = |want: &Option<String>, have: &str| {
            want.as_ref().is_none_or(|w| w.chars().count() == have.chars().count() && w.chars().zip(have.chars()).all(|(a, b)| a == '.' || a == b))
        };
        same(&self.group, &r.group)
            && same(&self.version, &r.version)
            && same(&self.kind, &r.kind)
            && same(&self.name, &r.name)
            && (r.namespace.is_empty() || same(&self.namespace, &r.namespace))
            && has(&r.labels, &self.labels)
            && has(&r.annotations, &self.annotations)
    }
}

struct Link {
    range: Range,
    locations: Vec<Location>,
}

#[derive(Default)]
struct Analysis {
    links: Vec<Link>,
    warnings: Vec<(Range, String)>,
}

fn key<'a>(node: &'a Node, name: &str) -> Option<&'a Scalar> {
    node.pairs()?.iter().find(|(k, _)| k.value == name).map(|(k, _)| k)
}

fn literal_path(project: &Project, r: &Res, path: &str) -> Option<Location> {
    let segments: Vec<_> = path.split('.').collect();
    if segments.iter().any(|s| s.is_empty() || !s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))) {
        return None;
    }
    let docs = parse(&project.read_file(&r.path).ok()?)?;
    let mut node = docs.get(r.doc)?;
    let mut range = None;
    for segment in segments {
        let (k, v) = node.pairs()?.iter().find(|(k, _)| k.value == segment)?;
        range = Some(k.range);
        node = v;
    }
    Some(Location { path: r.path.clone(), range: range? })
}

struct Context<'a> {
    project: Option<&'a Project>,
    resources: &'a [Res],
    adds_metadata: bool,
    analysis: Analysis,
}

impl Context<'_> {
    fn select(&mut self, node: &Node, anchor: &Scalar) -> Vec<usize> {
        let Some(selector) = Selector::read(node, self.adds_metadata) else {
            return vec![];
        };
        let matched: Vec<usize> = (0..self.resources.len()).filter(|i| selector.matches(&self.resources[*i])).collect();
        if matched.is_empty() {
            self.analysis.warnings.push((
                anchor.range,
                format!("Selector matches none of the {} resources listed in this kustomization", self.resources.len()),
            ));
            return matched;
        }
        for field in ["kind", "name", "namespace", "group", "version"] {
            if self.project.is_none() {
                break;
            }
            let Some(Node::Scalar(s)) = node.get(field) else {
                continue;
            };
            let locations: Vec<_> = matched
                .iter()
                .take(8)
                .filter_map(|i| {
                    let r = &self.resources[*i];
                    let range = match field {
                        "kind" => r.kind_range,
                        "name" => r.name_range,
                        "namespace" => r.namespace_range?,
                        _ => r.api_range,
                    };
                    Some(Location { path: r.path.clone(), range })
                })
                .collect();
            if !locations.is_empty() && !s.value.is_empty() {
                self.analysis.links.push(Link { range: s.range, locations });
            }
        }
        matched
    }

    fn field_paths(&mut self, matched: &[usize], scalar: &Scalar) {
        let Some(project) = self.project else {
            return;
        };
        let locations: Vec<_> = matched
            .iter()
            .take(8)
            .filter_map(|i| literal_path(project, &self.resources[*i], &scalar.value))
            .collect();
        if !locations.is_empty() {
            self.analysis.links.push(Link { range: scalar.range, locations });
        }
    }
}

fn implicit_selector(patch: &str) -> Option<Selector> {
    let docs = crate::convert::source_documents(patch)?;
    let body = docs.first().filter(|_| docs.len() == 1)?;
    let api = body["apiVersion"].as_str()?;
    let (kind, name) = (body["kind"].as_str()?, body["metadata"]["name"].as_str()?);
    let (group, version) = api.split_once('/').unwrap_or(("", api));
    (word(kind) && word(name)).then(|| Selector {
        group: word(group).then(|| group.into()),
        version: word(version).then(|| version.into()),
        kind: Some(kind.into()),
        name: Some(name.into()),
        ..Selector::default()
    })
}

enum Mode<'a> {
    /// Reads the resource files and builds links.
    Live(&'a Project, &'a str),
    /// Matches the selectors against the resources read when the kustomization was warmed.
    Cached(&'a Facts),
}

fn analyze(doc: &Document, mode: Mode) -> Option<Analysis> {
    if doc.text.len() > LIMIT || !named(&doc.path) {
        return None;
    }
    let nodes = parse(&doc.text)?;
    let top = nodes.first().filter(|_| nodes.len() == 1)?;
    let yaml = crate::convert::source_documents(&doc.text)?;
    let gathered;
    let (resources, project) = match mode {
        Mode::Live(project, root) => {
            gathered = gather(project, root, &doc.path, top)?;
            (gathered.as_slice(), Some(project))
        }
        Mode::Cached(facts) => {
            if facts.resources_key != resources_key(top) {
                return None;
            }
            (facts.resources.as_deref()?, None)
        }
    };
    let adds_metadata = ["commonLabels", "labels", "commonAnnotations"].iter().any(|k| top.get(k).is_some());
    let mut ctx = Context { project, resources, adds_metadata, analysis: Analysis::default() };
    for field in ["patches", "patchesJson6902"] {
        for (i, item) in top.get(field).and_then(Node::items).unwrap_or_default().iter().enumerate() {
            if let (Some(target), Some(anchor)) = (item.get("target"), key(item, "target")) {
                ctx.select(target, anchor);
                continue;
            }
            let (Some(anchor), Some(patch)) = (key(item, "patch"), yaml[0][field][i]["patch"].as_str()) else {
                continue;
            };
            if let Some(selector) = implicit_selector(patch)
                && !resources.iter().any(|r| selector.matches(r))
            {
                ctx.analysis.warnings.push((
                    anchor.range,
                    format!("Patch matches none of the {} resources listed in this kustomization", resources.len()),
                ));
            }
        }
    }
    for item in top.get("replacements").and_then(Node::items).unwrap_or_default() {
        if let (Some(source), Some(anchor)) = (item.get("source"), key(item, "source")) {
            let matched = ctx.select(source, anchor);
            if let Some(Node::Scalar(path)) = source.get("fieldPath") {
                ctx.field_paths(&matched, path);
            }
        }
        for target in item.get("targets").and_then(Node::items).unwrap_or_default() {
            let (Some(select), Some(anchor)) = (target.get("select"), key(target, "select")) else {
                continue;
            };
            let matched = ctx.select(select, anchor);
            for path in target.get("fieldPaths").and_then(Node::items).unwrap_or_default() {
                if let Node::Scalar(path) = path {
                    ctx.field_paths(&matched, path);
                }
            }
        }
    }
    Some(ctx.analysis)
}

struct Facts {
    resources_key: Option<String>,
    resources: Option<Vec<Res>>,
    messages: BTreeMap<String, String>,
    dependencies: Vec<String>,
}

#[derive(Default)]
pub(crate) struct Cache {
    facts: Lru<String, Facts>,
}

impl Cache {
    pub(crate) fn clear(&mut self) {
        self.facts.clear();
    }

    pub(crate) fn evict(&mut self, path: &str) {
        self.facts.remove(path);
    }

    pub(crate) fn warm_file(&mut self, path: &str) {
        if !named(path) {
            return;
        }
        match host::project_of(path).and_then(|project| project.read_file(path).ok()) {
            Some(text) => self.warm(path, &text),
            None => self.evict(path),
        }
    }

    pub(crate) fn warm(&mut self, path: &str, text: &str) {
        let doc = Document { path: path.into(), language: "yaml".into(), version: 0, text: text.into() };
        let Some((project, root)) = owner(&doc) else {
            self.evict(path);
            return;
        };
        let mut messages = BTreeMap::new();
        let mut dependencies = Vec::new();
        for r in references(&doc).into_iter().filter(|r| r.graph).take(64) {
            dependencies.extend(local(&root, path, &r.value));
            if let Some(message) = reference_message(&project, &root, path, &r) {
                messages.insert(r.value, message);
            }
        }
        let top = parse(text).and_then(|nodes| nodes.into_iter().next());
        let resources_key = top.as_ref().and_then(resources_key);
        let resources = top.as_ref().filter(|_| resources_key.is_some()).and_then(|top| gather(&project, &root, path, top));
        if resources.is_some() {
            for file in top.as_ref().and_then(resource_files).unwrap_or_default() {
                dependencies.extend(file.scalar().and_then(|s| local(&root, path, &s.value)));
            }
        }
        self.facts.insert(path.into(), Facts { resources_key, resources, messages, dependencies }, MAX_KUSTOMIZATIONS);
    }

    /// The warmed kustomizations that read one of `changed` or sit at one of them.
    pub(crate) fn affected_by(&self, changed: &[&str]) -> Vec<String> {
        self.facts
            .iter()
            .filter(|(path, facts)| {
                changed.iter().any(|changed| {
                    changed == path
                        || facts.dependencies.iter().any(|dependency| changed == dependency || changed.starts_with(&format!("{dependency}/")))
                })
            })
            .map(|(path, _)| path.clone())
            .collect()
    }
}

/// Reads only the cache, so an edit never triggers cross-file work.
pub(crate) fn diagnostics(doc: &Document, cache: &Cache) -> Vec<Diagnostic> {
    if owner(doc).is_none() {
        return vec![];
    }
    let Some(facts) = cache.facts.get(doc.path.as_str()) else {
        return vec![];
    };
    let mut found = Vec::new();
    for r in references(doc).into_iter().filter(|r| r.graph).take(64) {
        if let Some(message) = facts.messages.get(&r.value) {
            found.push(diagnostic(&doc.text, &r, message.clone()));
        }
    }
    for (range, message) in analyze(doc, Mode::Cached(facts)).map(|a| a.warnings).unwrap_or_default() {
        found.push(Diagnostic { code: Some("kustomize-selector".into()), ..Diagnostic::new(range, Severity::Info, &message) });
    }
    found
}

pub(crate) fn definition(doc: &Document, at: Position) -> Vec<Location> {
    let Some((project, root)) = owner(doc) else {
        return vec![];
    };
    analyze(doc, Mode::Live(&project, &root))
        .into_iter()
        .flat_map(|a| a.links)
        .filter(|l| l.range.start <= at && at < l.range.end)
        .flat_map(|l| l.locations)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;

    fn setup(kustomization: &str) -> (MockHost, Document) {
        let h = MockHost::install().with_capabilities(&["project.read"]);
        h.set_project_root(Some("/p"));
        h.add_file("overlays/dev/kustomization.yaml", kustomization);
        h.add_file(
            "overlays/dev/app.yaml",
            "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: web\n  labels:\n    tier: front\nspec:\n  replicas: 2\n---\napiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: cfg\ndata:\n  k: v\n",
        );
        let doc = Document { path: "/p/overlays/dev/kustomization.yaml".into(), language: "yaml".into(), version: 1, text: kustomization.into() };
        (h, doc)
    }

    fn warmed_diagnostics(doc: &Document) -> Vec<Diagnostic> {
        let mut cache = Cache::default();
        cache.warm(&doc.path, &doc.text);
        diagnostics(doc, &cache)
    }

    fn at(doc: &Document, needle: &str) -> Position {
        text::position(&doc.text, doc.text.find(needle).unwrap() + 1)
    }

    #[test]
    fn inline_patch_targets_link_and_unmatched_selectors_are_info() {
        let (_h, d) = setup(
            "resources:\n- app.yaml\npatches:\n- target:\n    kind: Deployment\n    name: web\n    labelSelector: tier=front\n  patch: |-\n    - op: replace\n      path: /spec/replicas\n      value: 3\n- target:\n    kind: Deployment\n    name: gone\n  patch: |-\n    x: y\n- target:\n    name: \"w.*\"\n- patch: |-\n    apiVersion: v1\n    kind: ConfigMap\n    metadata:\n      name: other\n",
        );
        let found = definition(&d, at(&d, "web"));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, "/p/overlays/dev/app.yaml");
        assert_eq!(found[0].range, Range::new(Position::new(3, 8), Position::new(3, 11)));
        assert_eq!(definition(&d, at(&d, "Deployment"))[0].range.start, Position::new(1, 6));
        let lines: Vec<_> = warmed_diagnostics(&d).iter().map(|w| (w.range.start.line, w.severity)).collect();
        assert_eq!(lines, [(11, Severity::Info), (18, Severity::Info)]);
    }

    #[test]
    fn replacement_source_and_target_field_paths_link_literal_keys_only() {
        let (_h, d) = setup(
            "resources:\n- app.yaml\nreplacements:\n- source:\n    kind: ConfigMap\n    name: cfg\n    fieldPath: data.k\n  targets:\n  - select:\n      kind: Deployment\n    fieldPaths:\n    - spec.replicas\n    - spec.template.spec.containers.[name=x].image\n    - metadata.labels.tier\n",
        );
        let source = definition(&d, at(&d, "data.k"));
        assert_eq!(source[0].range, Range::new(Position::new(14, 2), Position::new(14, 3)));
        assert_eq!(definition(&d, at(&d, "spec.replicas"))[0].range, Range::new(Position::new(7, 2), Position::new(7, 10)));
        assert_eq!(definition(&d, at(&d, "metadata.labels.tier"))[0].range.start, Position::new(5, 4));
        assert!(definition(&d, at(&d, "[name=x]")).is_empty());
    }

    const UNMATCHED: &str = "resources:\n- app.yaml\npatches:\n- target:\n    kind: Deployment\n    name: gone\n  patch: |-\n    x: y\n";

    #[test]
    fn kustomize_diagnostics_use_cache() {
        let (h, d) = setup(UNMATCHED);
        let mut cache = Cache::default();
        assert!(diagnostics(&d, &cache).is_empty(), "nothing is read before the kustomization is warmed");

        cache.warm(&d.path, &d.text);
        let before = diagnostics(&d, &cache);
        assert_eq!(before.len(), 1, "{before:?}");

        h.add_file("overlays/dev/app.yaml", "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: gone\n");
        let after = diagnostics(&d, &cache);
        assert_eq!(after.iter().map(|w| (&w.message, w.range)).collect::<Vec<_>>(), before.iter().map(|w| (&w.message, w.range)).collect::<Vec<_>>());
    }

    #[test]
    fn cached_findings_follow_the_typed_text_and_changed_files_refresh_them() {
        let (h, d) = setup(UNMATCHED);
        let mut cache = Cache::default();
        cache.warm(&d.path, &d.text);
        let shifted = Document { text: format!("# note\n{}", d.text), ..d.clone() };
        let lines: Vec<_> = diagnostics(&shifted, &cache).iter().map(|w| w.range.start.line).collect();
        assert_eq!(lines, [4]);

        h.add_file("overlays/dev/app.yaml", "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: gone\n");
        assert_eq!(cache.affected_by(&["/p/overlays/dev/app.yaml"]), ["/p/overlays/dev/kustomization.yaml"]);
        assert!(cache.affected_by(&["/p/elsewhere.yaml"]).is_empty());
        cache.warm_file(&d.path);
        assert!(diagnostics(&d, &cache).is_empty());
    }

    #[test]
    fn regex_remote_generated_and_unparsable_resources_stay_quiet() {
        for source in [
            "resources:\n- app.yaml\n- https://x.io/a.yaml\npatches:\n- target:\n    name: nope\n",
            "resources:\n- app.yaml\npatches:\n- target:\n    name: \"no.*\"\n",
            "resources:\n- app.yaml\nconfigMapGenerator:\n- name: nope\npatches:\n- target:\n    name: nope\n",
            "resources:\n- app.yaml\ncommonLabels: {a: b}\npatches:\n- target:\n    labelSelector: a=b\n",
            "resources:\n- app.yaml\n- bad.yaml\npatches:\n- target:\n    name: nope\n",
        ] {
            let (h, d) = setup(source);
            h.add_file("overlays/dev/bad.yaml", "kind: [\n");
            assert!(warmed_diagnostics(&d).is_empty(), "{source}");
        }
    }

    #[test]
    fn directory_references_need_a_kustomization_and_cycles_are_found() {
        let (h, d) = setup("resources:\n- ../../base\n- ../empty\n- ../missing\n- github.com/org/repo//x\n- https://x.io/k\ncomponents:\n- ../../comp\nbases:\n- ../loop\n");
        for dir in ["base", "overlays/empty", "comp", "overlays/loop"] {
            h.add_dir(dir);
        }
        h.add_file("base/kustomization.yaml", "resources: []\n");
        h.add_file("overlays/empty/readme.md", "x\n");
        h.add_file("comp/kustomization.yaml", "resources:\n- ../overlays/dev\n");
        h.add_file("overlays/loop/kustomization.yaml", "resources:\n- ../dev\n");
        let rows: Vec<_> = warmed_diagnostics(&d).into_iter().map(|w| (w.range.start.line, w.message)).collect();
        assert_eq!(rows.len(), 4, "{rows:?}");
        assert!(rows[0].1.contains("kustomization not found in ../empty"));
        assert!(rows[1].1.starts_with("kustomization not found: ../missing"));
        assert!(rows[2].1.starts_with("Kustomization cycle: dev -> comp -> dev"));
        assert!(rows[3].1.starts_with("Kustomization cycle: dev -> loop -> dev"));
    }

    #[test]
    fn deep_chain_is_bounded() {
        let (h, d) = setup("resources:\n- ../c0\n");
        for i in 0..20 {
            h.add_dir(&format!("overlays/c{i}"));
            h.add_file(&format!("overlays/c{i}/kustomization.yaml"), &format!("resources:\n- ../c{}\n", i + 1));
        }
        h.add_file("overlays/c20/kustomization.yaml", "resources:\n- ../dev\n");
        assert!(warmed_diagnostics(&d).is_empty());
    }
}
