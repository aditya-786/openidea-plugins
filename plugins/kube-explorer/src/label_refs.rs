//! Exact read-only label/selector and ConfigMap/Secret data-key associations.
//! These are source candidates, never indexed Rename anchors: a project scan
//! does not prove runtime deployment/reference closure.
use openidea_plugin::providers::{CompletionItem, CompletionKind, Document, LineMarker};
use openidea_plugin::{Location, Position, Range, host, text};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use yaml_rust2::{
    parser::{Event, MarkedEventReceiver, Parser},
    scanner::{Marker, TScalarStyle},
};
const MAX_TEXT: usize = 128 << 10;
const MAX_FILES: usize = 64;
const MAX_LINE_TARGETS: usize = 200;
#[derive(Clone, Debug)]
pub(crate) struct Scalar {
    pub(crate) value: String,
    pub(crate) range: Range,
    string: bool,
    exact: bool,
    escaped: bool,
}
#[derive(Debug)]
pub(crate) enum Node {
    Scalar(Scalar),
    Map(Vec<(Scalar, Node)>),
    Seq(Vec<Node>),
}
impl Node {
    pub(crate) fn get(&self, key: &str) -> Option<&Node> {
        if let Self::Map(pairs) = self {
            pairs.iter().find(|(k, _)| k.value == key).map(|(_, v)| v)
        } else {
            None
        }
    }
    pub(crate) fn at(&self, path: &[&str]) -> Option<&Node> {
        path.iter().try_fold(self, |node, key| node.get(key))
    }
    pub(crate) fn scalar(&self) -> Option<&Scalar> {
        if let Self::Scalar(s) = self {
            Some(s)
        } else {
            None
        }
    }
    pub(crate) fn pairs(&self) -> Option<&[(Scalar, Node)]> {
        if let Self::Map(p) = self {
            Some(p)
        } else {
            None
        }
    }
    pub(crate) fn items(&self) -> Option<&[Node]> {
        if let Self::Seq(p) = self {
            Some(p)
        } else {
            None
        }
    }
}
enum Frame {
    Map(Vec<(Scalar, Node)>, Option<Scalar>),
    Seq(Vec<Node>),
}
/// Byte offsets of every line start, so a position costs one binary search.
pub(crate) struct LineStarts(Vec<usize>);
impl LineStarts {
    pub(crate) fn new(source: &str) -> Self {
        Self(std::iter::once(0).chain(source.match_indices('\n').map(|(at, _)| at + 1)).collect())
    }
    pub(crate) fn position(&self, source: &str, at: usize) -> Position {
        let at = at.min(source.len());
        let line = self.0.partition_point(|start| *start <= at) - 1;
        let character = source.get(self.0[line]..at).map_or(0, |text| text.chars().map(|c| c.len_utf16() as u32).sum());
        Position { line: line as u32, character }
    }
}
struct Reader<'a> {
    source: &'a str,
    lines: LineStarts,
    offsets: Vec<usize>,
    frames: Vec<Frame>,
    docs: Vec<Node>,
    bad: bool,
}
impl Reader<'_> {
    fn push(&mut self, node: Node) {
        match self.frames.last_mut() {
            Some(Frame::Map(pairs, key)) => {
                if let Some(key) = key.take() {
                    pairs.push((key, node))
                } else if let Node::Scalar(s) = node {
                    *key = Some(s)
                } else {
                    self.bad = true
                }
            }
            Some(Frame::Seq(items)) => items.push(node),
            None => self.docs.push(node),
        }
    }
}
impl MarkedEventReceiver for Reader<'_> {
    fn on_event(&mut self, event: Event, mark: Marker) {
        if self.bad {
            return;
        }
        match event {
            Event::Scalar(value, style, anchor, tag) => {
                if anchor != 0 || tag.is_some() {
                    self.bad = true;
                    return;
                }
                let start = self
                    .offsets
                    .get(mark.index())
                    .copied()
                    .unwrap_or(self.source.len());
                let Some(source) = self.source.get(start..) else {
                    self.bad = true;
                    return;
                };
                // Exact physical spelling only; escaped/multiline scalars are
                // not reinterpreted as replaceable label or key ranges.
                let quoted = matches!(
                    style,
                    TScalarStyle::SingleQuoted | TScalarStyle::DoubleQuoted
                );
                let start = start + usize::from(quoted);
                let end = start + value.len();
                let raw_exact = self.source.get(start..end) == Some(value.as_str())
                    && if quoted {
                        source.starts_with(['\'', '"'])
                            && self
                                .source
                                .get(end..)
                                .is_some_and(|s| s.starts_with(source.chars().next().unwrap()))
                    } else {
                        style == TScalarStyle::Plain
                    };
                let scanned = (!raw_exact && quoted)
                    .then(|| scan_quoted(self.source, start - 1, &value))
                    .flatten();
                let exact = raw_exact || scanned.is_some();
                let end = scanned.unwrap_or(end);
                let string = quoted
                    || matches!(
                        yaml_rust2::Yaml::from_str(&value),
                        yaml_rust2::Yaml::String(_)
                    );
                let end = if exact { end } else { start };
                let scalar = Scalar {
                    value: if exact { value } else { String::new() },
                    string,
                    exact,
                    escaped: scanned.is_some(),
                    range: Range::new(
                        self.lines.position(self.source, start),
                        self.lines.position(self.source, end),
                    ),
                };
                self.push(Node::Scalar(scalar));
            }
            Event::MappingStart(anchor, tag) => {
                if anchor != 0 || tag.is_some() {
                    self.bad = true;
                } else {
                    self.frames.push(Frame::Map(Vec::new(), None));
                }
            }
            Event::SequenceStart(anchor, tag) => {
                if anchor != 0 || tag.is_some() {
                    self.bad = true;
                } else {
                    self.frames.push(Frame::Seq(Vec::new()));
                }
            }
            Event::MappingEnd => {
                if let Some(Frame::Map(pairs, None)) = self.frames.pop() {
                    self.push(Node::Map(pairs))
                } else {
                    self.bad = true
                }
            }
            Event::SequenceEnd => {
                if let Some(Frame::Seq(items)) = self.frames.pop() {
                    self.push(Node::Seq(items))
                } else {
                    self.bad = true
                }
            }
            Event::Alias(_) => self.bad = true,
            _ => {}
        }
    }
}
/// The unescaped text of a single-line quoted scalar opening at `open`, and
/// the offset of its closing quote.
pub(crate) fn unquote(source: &str, open: usize) -> Option<(String, usize)> {
    let quote = *source.as_bytes().get(open)?;
    let rest = source.get(open + 1..)?;
    let mut raw = String::new();
    let mut chars = rest.char_indices();
    while let Some((i, c)) = chars.next() {
        match (quote, c) {
            (_, '\n' | '\r') => return None,
            (b'\'', '\'') => match chars.clone().next() {
                Some((_, '\'')) => {
                    chars.next();
                    raw.push('\'');
                }
                _ => return Some((raw, open + 1 + i)),
            },
            (b'"', '"') => return Some((raw, open + 1 + i)),
            (b'"', '\\') => raw.push(match chars.next()?.1 {
                '\\' => '\\',
                '"' => '"',
                '/' => '/',
                'n' => '\n',
                't' => '\t',
                'r' => '\r',
                _ => return None,
            }),
            _ => raw.push(c),
        }
    }
    None
}
/// Inner end offset of a single-line quoted scalar opening at `open`, only
/// when its unescaped text equals the parser's `value`.
fn scan_quoted(source: &str, open: usize, value: &str) -> Option<usize> {
    unquote(source, open).filter(|(raw, _)| raw == value).map(|(_, end)| end)
}
fn spells(source: &str, start: usize, end: usize, key: &str) -> bool {
    let Some(raw) = source.get(start..end) else {
        return false;
    };
    raw == key
        || start > 0
            && source.as_bytes().get(end) == source.as_bytes().get(start - 1)
            && scan_quoted(source, start - 1, key) == Some(end)
}
pub(crate) fn parse(source: &str) -> Option<Vec<Node>> {
    if source.len() > MAX_TEXT || source.contains("{{") || source.contains("${") {
        return None;
    }
    let mut checked = Checked {
        guard: crate::convert::ValueGuard::default(),
        reader: Reader {
            source,
            lines: LineStarts::new(source),
            offsets: source
                .char_indices()
                .map(|(i, _)| i)
                .chain(std::iter::once(source.len()))
                .collect(),
            frames: Vec::new(),
            docs: Vec::new(),
            bad: false,
        },
    };
    Parser::new_from_str(source).load(&mut checked, true).ok()?;
    (!checked.guard.rejected() && !checked.reader.bad).then_some(checked.reader.docs)
}
/// Feeds one parse pass to the value guard and to the node reader.
struct Checked<'a> {
    guard: crate::convert::ValueGuard,
    reader: Reader<'a>,
}
impl MarkedEventReceiver for Checked<'_> {
    fn on_event(&mut self, event: Event, mark: Marker) {
        yaml_rust2::parser::EventReceiver::on_event(&mut self.guard, event.clone());
        self.reader.on_event(event, mark);
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Resource {
    path: String,
    group: String,
    kind: String,
    namespace: String,
    name: String,
    document: usize,
    range: Range,
}
#[derive(Clone)]
struct Label {
    owner: Resource,
    key: Scalar,
    value: Scalar,
}
#[derive(Clone)]
struct Selector {
    owner: Resource,
    own_template: bool,
    labels: Vec<(Scalar, Scalar)>,
    expressions: Vec<Expression>,
    target: Target,
    namespace_terms: Option<Terms>,
    pod_terms: Option<Terms>,
}
#[derive(Clone, Copy)]
enum Target {
    Pod,
    Namespace,
    Node,
}
#[derive(Clone)]
struct Terms {
    labels: Vec<(Scalar, Scalar)>,
    expressions: Vec<Expression>,
}
#[derive(Clone, Copy)]
enum Operator {
    In,
    NotIn,
    Exists,
    DoesNotExist,
}
#[derive(Clone)]
struct Expression {
    key: Scalar,
    operator: Operator,
    values: Vec<Scalar>,
}
#[derive(Clone)]
struct LabelUse {
    owner: Resource,
    key: Scalar,
}
#[derive(Clone)]
struct Key {
    owner: Resource,
    key: Scalar,
}
#[derive(Clone)]
struct KeyRef {
    namespace: String,
    kind: String,
    name: String,
    key: Scalar,
}
#[derive(Clone, Default)]
struct Facts {
    pods: Vec<Resource>,
    namespaces: Vec<Resource>,
    nodes: Vec<Resource>,
    labels: Vec<Label>,
    selectors: Vec<Selector>,
    label_uses: Vec<LabelUse>,
    keys: Vec<Key>,
    references: Vec<KeyRef>,
    data_resources: Vec<Resource>,
}
fn string(node: Option<&Node>) -> Option<String> {
    Some(node?.scalar().filter(|s| s.string)?.value.clone()).filter(|s| !s.is_empty())
}
fn pod_path(group: &str, kind: &str) -> Option<&'static [&'static str]> {
    match (group, kind) {
        ("", "Pod") => Some(&["spec"]),
        ("", "ReplicationController") => Some(&["spec", "template", "spec"]),
        ("apps", "Deployment" | "ReplicaSet" | "DaemonSet" | "StatefulSet") | ("batch", "Job") => {
            Some(&["spec", "template", "spec"])
        }
        ("batch", "CronJob") => Some(&["spec", "jobTemplate", "spec", "template", "spec"]),
        _ => None,
    }
}
fn key_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 253
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}
fn label_part(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && value
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric)
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}
fn label_key(value: &str) -> bool {
    if let Some((prefix, name)) = value.split_once('/') {
        crate::is_dns_subdomain(prefix) && !name.contains('/') && label_part(name)
    } else {
        label_part(value)
    }
}
fn label_map(node: Option<&Node>) -> Option<Vec<(Scalar, Scalar)>> {
    let mut out = Vec::new();
    for (key, value) in node?.pairs()? {
        let value = value.scalar()?;
        if !key.string
            || !key.exact
            || !value.string
            || !value.exact
            || !label_key(&key.value)
            || (!value.value.is_empty() && !label_part(&value.value))
        {
            return None;
        }
        out.push((key.clone(), value.clone()));
    }
    Some(out)
}
fn selector_parts(node: &Node) -> Option<(Vec<(Scalar, Scalar)>, Vec<Expression>)> {
    if node
        .pairs()?
        .iter()
        .any(|(key, _)| !matches!(key.value.as_str(), "matchLabels" | "matchExpressions"))
    {
        return None;
    }
    let labels = if let Some(labels) = node.get("matchLabels") {
        label_map(Some(labels))?
    } else {
        Vec::new()
    };
    let mut expressions = Vec::new();
    if let Some(items) = node.get("matchExpressions") {
        for item in items.items()?.iter().take(65) {
            if item
                .pairs()?
                .iter()
                .any(|(key, _)| !matches!(key.value.as_str(), "key" | "operator" | "values"))
            {
                return None;
            }
            if expressions.len() >= 64 {
                return None;
            }
            let key = item.get("key")?.scalar()?.clone();
            if !key.string || !key.exact || !label_key(&key.value) {
                return None;
            }
            let operator = match string(item.get("operator"))?.as_str() {
                "In" => Operator::In,
                "NotIn" => Operator::NotIn,
                "Exists" => Operator::Exists,
                "DoesNotExist" => Operator::DoesNotExist,
                _ => return None,
            };
            let mut values = Vec::new();
            if let Some(list) = item.get("values") {
                for value in list.items()?.iter().take(257) {
                    let value = value.scalar()?;
                    if values.len() >= 256
                        || !value.string
                        || !value.exact
                        || (!value.value.is_empty() && !label_part(&value.value))
                    {
                        return None;
                    }
                    values.push(value.clone());
                }
            }
            if matches!(operator, Operator::In | Operator::NotIn) && values.is_empty()
                || matches!(operator, Operator::Exists | Operator::DoesNotExist)
                    && !values.is_empty()
            {
                return None;
            }
            expressions.push(Expression {
                key,
                operator,
                values,
            });
        }
    }
    Some((labels, expressions))
}
fn scheduling_selector(
    owner: &Resource,
    target: Target,
    parts: (Vec<(Scalar, Scalar)>, Vec<Expression>),
) -> Selector {
    Selector {
        owner: owner.clone(),
        own_template: false,
        labels: parts.0,
        expressions: parts.1,
        target,
        namespace_terms: None,
        pod_terms: None,
    }
}
fn only_keys(node: &Node, allowed: &[&str]) -> bool {
    node.pairs()
        .is_some_and(|pairs| pairs.iter().all(|(k, _)| allowed.contains(&k.value.as_str())))
}
fn spread_selectors(pod: &Node, owner: &Resource, out: &mut Vec<Selector>) {
    for item in pod
        .get("topologySpreadConstraints")
        .and_then(Node::items)
        .into_iter()
        .flatten()
    {
        if !only_keys(
            item,
            &[
                "maxSkew",
                "topologyKey",
                "whenUnsatisfiable",
                "labelSelector",
                "minDomains",
                "nodeAffinityPolicy",
                "nodeTaintsPolicy",
            ],
        ) {
            continue;
        }
        if let Some(parts) = item.get("labelSelector").and_then(selector_parts) {
            out.push(scheduling_selector(owner, Target::Pod, parts));
        }
    }
}
/// Exact scheduling selectors only: node affinity terms with plain
/// matchExpressions (no matchFields/Gt/Lt), and pod (anti)affinity or topology
/// spread terms with only a labelSelector (default own-namespace scope). Terms
/// using namespaces, namespaceSelector or matchLabelKeys are refused.
fn collect_scheduling_selectors(pod: &Node, owner: &Resource, out: &mut Vec<Selector>) {
    spread_selectors(pod, owner, out);
    let Some(affinity) = pod.get("affinity") else {
        return;
    };
    let node_term = |term: &Node, out: &mut Vec<Selector>| {
        if !only_keys(term, &["matchExpressions"]) {
            return;
        }
        if let Some(parts) = selector_parts(term).filter(|p| !p.1.is_empty()) {
            out.push(scheduling_selector(owner, Target::Node, parts));
        }
    };
    let pod_term = |term: &Node, out: &mut Vec<Selector>| {
        if !only_keys(term, &["labelSelector", "topologyKey"]) {
            return;
        }
        if let Some(parts) = term.get("labelSelector").and_then(selector_parts) {
            out.push(scheduling_selector(owner, Target::Pod, parts));
        }
    };
    let required = "requiredDuringSchedulingIgnoredDuringExecution";
    let preferred = "preferredDuringSchedulingIgnoredDuringExecution";
    if let Some(node_affinity) = affinity.get("nodeAffinity") {
        for term in node_affinity
            .at(&[required, "nodeSelectorTerms"])
            .and_then(Node::items)
            .into_iter()
            .flatten()
        {
            node_term(term, out);
        }
        for item in node_affinity
            .get(preferred)
            .and_then(Node::items)
            .into_iter()
            .flatten()
        {
            if only_keys(item, &["weight", "preference"])
                && let Some(term) = item.get("preference")
            {
                node_term(term, out);
            }
        }
    }
    for family in ["podAffinity", "podAntiAffinity"] {
        let Some(block) = affinity.get(family) else {
            continue;
        };
        for term in block
            .get(required)
            .and_then(Node::items)
            .into_iter()
            .flatten()
        {
            pod_term(term, out);
        }
        for item in block
            .get(preferred)
            .and_then(Node::items)
            .into_iter()
            .flatten()
        {
            if only_keys(item, &["weight", "podAffinityTerm"])
                && let Some(term) = item.get("podAffinityTerm")
            {
                pod_term(term, out);
            }
        }
    }
}
fn facts(path: &str, source: &str) -> Option<Facts> {
    let mut out = Facts::default();
    for (document, node) in parse(source)?.into_iter().enumerate() {
        let Some(api) = string(node.get("apiVersion")) else {
            continue;
        };
        let group = if api == "v1" {
            ""
        } else {
            let Some((group, _)) = api.split_once('/') else {
                continue;
            };
            group
        };
        let Some(kind) = string(node.get("kind")) else {
            continue;
        };
        // Namespace defaults and overlay transforms do not establish shared
        // runtime ownership; cross-file associations require an explicit ns.
        let cluster = group.is_empty() && matches!(kind.as_str(), "Namespace" | "Node");
        let namespace = if cluster {
            if node.at(&["metadata", "namespace"]).is_some() {
                continue;
            }
            String::new()
        } else {
            let Some(namespace) = string(node.at(&["metadata", "namespace"])) else {
                continue;
            };
            namespace
        };
        let Some(name) = string(node.at(&["metadata", "name"])) else {
            continue;
        };
        if (!cluster && !crate::is_dns_label(&namespace))
            || !crate::is_dns_subdomain(&name)
            || (kind == "Namespace" && !crate::is_dns_label(&name))
        {
            continue;
        }
        let owner = Resource {
            path: path.into(),
            group: group.into(),
            kind: kind.clone(),
            namespace: namespace.clone(),
            name,
            document,
            range: node.at(&["metadata", "name"])?.scalar()?.range,
        };
        if cluster {
            let labels_node = node.at(&["metadata", "labels"]);
            if let Some(labels) = if labels_node.is_none() {
                Some(Vec::new())
            } else {
                label_map(labels_node)
            } {
                if kind == "Namespace" {
                    out.namespaces.push(owner.clone());
                } else {
                    out.nodes.push(owner.clone());
                }
                out.labels
                    .extend(labels.into_iter().map(|(key, value)| Label {
                        owner: owner.clone(),
                        key,
                        value,
                    }));
            }
        }
        if let Some(base) =
            pod_path(group, &kind).filter(|base| node.at(base).and_then(Node::pairs).is_some())
        {
            let mut path = base.to_vec();
            path.pop();
            path.extend(["metadata", "labels"]);
            let labels_node = node.at(&path);
            if let Some(labels) = if labels_node.is_none() {
                Some(Vec::new())
            } else {
                label_map(labels_node)
            } {
                out.pods.push(owner.clone());
                out.labels
                    .extend(labels.into_iter().map(|(key, value)| Label {
                        owner: owner.clone(),
                        key,
                        value,
                    }));
            }
            if let Some(pod) = node.at(base) {
                collect_key_refs(pod, &namespace, &mut out.references);
                collect_label_uses(pod, &owner, &mut out.label_uses);
                collect_scheduling_selectors(pod, &owner, &mut out.selectors);
                if let Some(labels) = label_map(pod.get("nodeSelector")).filter(|l| !l.is_empty()) {
                    out.selectors.push(Selector {
                        owner: owner.clone(),
                        own_template: false,
                        labels,
                        expressions: Vec::new(),
                        target: Target::Node,
                        namespace_terms: None,
                        pod_terms: None,
                    });
                }
            }
        }
        let selector = match (group, kind.as_str()) {
            ("", "Service" | "ReplicationController") => node
                .at(&["spec", "selector"])
                .and_then(|n| label_map(Some(n)))
                .filter(|l| !l.is_empty())
                .map(|labels| (labels, Vec::new())),
            ("apps", "Deployment" | "ReplicaSet" | "DaemonSet" | "StatefulSet")
            | ("batch", "Job")
            | ("policy", "PodDisruptionBudget") => {
                node.at(&["spec", "selector"]).and_then(selector_parts)
            }
            ("networking.k8s.io", "NetworkPolicy") => {
                node.at(&["spec", "podSelector"]).and_then(selector_parts)
            }
            _ => None,
        };
        if let Some((labels, expressions)) = selector {
            out.selectors.push(Selector {
                owner: owner.clone(),
                own_template: group == "apps"
                    || (group == "batch" && kind == "Job")
                    || (group.is_empty() && kind == "ReplicationController"),
                labels,
                expressions,
                target: Target::Pod,
                namespace_terms: None,
                pod_terms: None,
            });
        }
        if group == "networking.k8s.io" && kind == "NetworkPolicy" {
            for (rules, peers) in [("ingress", "from"), ("egress", "to")] {
                for rule in node
                    .at(&["spec", rules])
                    .and_then(Node::items)
                    .into_iter()
                    .flatten()
                {
                    for peer in rule.get(peers).and_then(Node::items).into_iter().flatten() {
                        if peer.pairs().is_none_or(|pairs| {
                            pairs.iter().any(|(key, _)| {
                                !matches!(key.value.as_str(), "podSelector" | "namespaceSelector")
                            })
                        }) {
                            continue;
                        }
                        let pod = peer.get("podSelector");
                        let namespace = peer.get("namespaceSelector");
                        if pod.is_none() && namespace.is_none() {
                            continue;
                        }
                        let pod_terms = if let Some(pod) = pod {
                            let Some((labels, expressions)) = selector_parts(pod) else {
                                continue;
                            };
                            Some(Terms {
                                labels,
                                expressions,
                            })
                        } else {
                            None
                        };
                        let namespace_terms = if let Some(namespace) = namespace {
                            let Some((labels, expressions)) = selector_parts(namespace) else {
                                continue;
                            };
                            Some(Terms {
                                labels,
                                expressions,
                            })
                        } else {
                            None
                        };
                        let terms = pod_terms.clone().unwrap_or(Terms {
                            labels: Vec::new(),
                            expressions: Vec::new(),
                        });
                        out.selectors.push(Selector {
                            owner: owner.clone(),
                            own_template: false,
                            labels: terms.labels,
                            expressions: terms.expressions,
                            target: Target::Pod,
                            namespace_terms: namespace_terms.clone(),
                            pod_terms: None,
                        });
                        if let Some(terms) = namespace_terms {
                            out.selectors.push(Selector {
                                owner: owner.clone(),
                                own_template: false,
                                labels: terms.labels,
                                expressions: terms.expressions,
                                target: Target::Namespace,
                                namespace_terms: None,
                                pod_terms,
                            });
                        }
                    }
                }
            }
        }
        if group.is_empty() && matches!(kind.as_str(), "ConfigMap" | "Secret") {
            out.data_resources.push(owner.clone());
            for field in if kind == "Secret" {
                &["data", "stringData"][..]
            } else {
                &["data", "binaryData"][..]
            } {
                if let Some(pairs) = node.get(field).and_then(Node::pairs) {
                    for (key, _) in pairs {
                        if key.string && key_name(&key.value) {
                            out.keys.push(Key {
                                owner: owner.clone(),
                                key: key.clone(),
                            });
                        }
                    }
                }
            }
        }
    }
    Some(out)
}
fn downward_label(reference: &Node, owner: &Resource, out: &mut Vec<LabelUse>) {
    let Some(path) = reference
        .get("fieldPath")
        .and_then(Node::scalar)
        .filter(|s| s.string && s.exact && !s.escaped)
    else {
        return;
    };
    let prefix = "metadata.labels['";
    let Some(key) = path
        .value
        .strip_prefix(prefix)
        .and_then(|s| s.strip_suffix("']"))
    else {
        return;
    };
    if !label_key(key) {
        return;
    }
    let mut start = path.range.start;
    start.character += prefix.encode_utf16().count() as u32;
    let mut end = start;
    end.character += key.encode_utf16().count() as u32;
    out.push(LabelUse {
        owner: owner.clone(),
        key: Scalar {
            value: key.into(),
            range: Range::new(start, end),
            string: true,
            exact: true,
            escaped: false,
        },
    });
}
fn collect_label_uses(pod: &Node, owner: &Resource, out: &mut Vec<LabelUse>) {
    for kind in ["containers", "initContainers", "ephemeralContainers"] {
        for container in pod.get(kind).and_then(Node::items).into_iter().flatten() {
            for env in container
                .get("env")
                .and_then(Node::items)
                .into_iter()
                .flatten()
            {
                if let Some(reference) = env.at(&["valueFrom", "fieldRef"]) {
                    downward_label(reference, owner, out);
                }
            }
        }
    }
    for volume in pod
        .get("volumes")
        .and_then(Node::items)
        .into_iter()
        .flatten()
    {
        for item in volume
            .at(&["downwardAPI", "items"])
            .and_then(Node::items)
            .into_iter()
            .flatten()
        {
            if let Some(reference) = item.get("fieldRef") {
                downward_label(reference, owner, out);
            }
        }
        for source in volume
            .at(&["projected", "sources"])
            .and_then(Node::items)
            .into_iter()
            .flatten()
        {
            for item in source
                .at(&["downwardAPI", "items"])
                .and_then(Node::items)
                .into_iter()
                .flatten()
            {
                if let Some(reference) = item.get("fieldRef") {
                    downward_label(reference, owner, out);
                }
            }
        }
    }
}
fn collect_key_refs(pod: &Node, namespace: &str, out: &mut Vec<KeyRef>) {
    for container in ["containers", "initContainers", "ephemeralContainers"] {
        for node in pod
            .get(container)
            .and_then(Node::items)
            .into_iter()
            .flatten()
        {
            for env in node.get("env").and_then(Node::items).into_iter().flatten() {
                for (field, kind) in [("configMapKeyRef", "ConfigMap"), ("secretKeyRef", "Secret")]
                {
                    if let Some(reference) = env.at(&["valueFrom", field]) {
                        key_reference(reference, "name", "key", namespace, kind, out);
                    }
                }
            }
        }
    }
    for volume in pod
        .get("volumes")
        .and_then(Node::items)
        .into_iter()
        .flatten()
    {
        for (field, kind, name) in [
            ("configMap", "ConfigMap", "name"),
            ("secret", "Secret", "secretName"),
        ] {
            if let Some(reference) = volume.get(field) {
                volume_items(reference, name, namespace, kind, out);
            }
        }
        for source in volume
            .at(&["projected", "sources"])
            .and_then(Node::items)
            .into_iter()
            .flatten()
        {
            for (field, kind) in [("configMap", "ConfigMap"), ("secret", "Secret")] {
                if let Some(reference) = source.get(field) {
                    volume_items(reference, "name", namespace, kind, out);
                }
            }
        }
    }
}
fn volume_items(reference: &Node, name: &str, namespace: &str, kind: &str, out: &mut Vec<KeyRef>) {
    let Some(resource) = string(reference.get(name)).filter(|n| crate::is_dns_subdomain(n)) else {
        return;
    };
    for item in reference
        .get("items")
        .and_then(Node::items)
        .into_iter()
        .flatten()
    {
        if let Some(key) = item
            .get("key")
            .and_then(Node::scalar)
            .filter(|s| s.string && key_name(&s.value))
        {
            out.push(KeyRef {
                namespace: namespace.into(),
                kind: kind.into(),
                name: resource.clone(),
                key: key.clone(),
            });
        }
    }
}
fn key_reference(
    reference: &Node,
    name: &str,
    key: &str,
    namespace: &str,
    kind: &str,
    out: &mut Vec<KeyRef>,
) {
    let Some(name) = string(reference.get(name)).filter(|n| crate::is_dns_subdomain(n)) else {
        return;
    };
    let Some(key) = reference
        .get(key)
        .and_then(Node::scalar)
        .filter(|s| s.string && key_name(&s.value))
    else {
        return;
    };
    out.push(KeyRef {
        namespace: namespace.into(),
        kind: kind.into(),
        name,
        key: key.clone(),
    });
}
fn contains(range: Range, at: Position) -> bool {
    (range.start.line, range.start.character) <= (at.line, at.character)
        && (at.line, at.character) < (range.end.line, range.end.character)
}
fn loc(path: &str, range: Range) -> Location {
    Location {
        path: path.into(),
        range,
    }
}
fn terms_match(
    labels: &[(Scalar, Scalar)],
    expressions: &[Expression],
    owner: &Resource,
    facts: &Facts,
) -> bool {
    let actual: Vec<_> = facts.labels.iter().filter(|l| &l.owner == owner).collect();
    labels.iter().all(|(key, value)| {
        actual
            .iter()
            .any(|l| l.key.value == key.value && l.value.value == value.value)
    }) && expressions.iter().all(|expression| {
        let value = actual
            .iter()
            .find(|l| l.key.value == expression.key.value)
            .map(|l| l.value.value.as_str());
        match expression.operator {
            Operator::In => {
                value.is_some_and(|v| expression.values.iter().any(|item| item.value == v))
            }
            Operator::NotIn => {
                value.is_none_or(|v| !expression.values.iter().any(|item| item.value == v))
            }
            Operator::Exists => value.is_some(),
            Operator::DoesNotExist => value.is_none(),
        }
    })
}
fn matches(selector: &Selector, owner: &Resource, facts: &Facts, all: &[(String, Facts)]) -> bool {
    let kind_ok = match selector.target {
        Target::Pod => facts.pods.contains(owner),
        Target::Namespace => facts.namespaces.contains(owner),
        Target::Node => facts.nodes.contains(owner),
    };
    if !kind_ok || selector.own_template && owner != &selector.owner {
        return false;
    }
    if matches!(selector.target, Target::Pod) {
        if let Some(terms) = &selector.namespace_terms {
            let namespaces: Vec<_> = all
                .iter()
                .flat_map(|(_, facts)| {
                    facts
                        .namespaces
                        .iter()
                        .filter(|ns| ns.name == owner.namespace)
                        .map(move |ns| (ns, facts))
                })
                .collect();
            if namespaces.len() != 1
                || !terms_match(
                    &terms.labels,
                    &terms.expressions,
                    namespaces[0].0,
                    namespaces[0].1,
                )
            {
                return false;
            }
        } else if owner.namespace != selector.owner.namespace {
            return false;
        }
    }
    if matches!(selector.target, Target::Node)
        && all
            .iter()
            .flat_map(|(_, facts)| &facts.nodes)
            .filter(|node| node.name == owner.name)
            .count()
            != 1
    {
        return false;
    }
    if matches!(selector.target, Target::Namespace)
        && all
            .iter()
            .flat_map(|(_, facts)| &facts.namespaces)
            .filter(|ns| ns.name == owner.name)
            .count()
            != 1
    {
        return false;
    }
    if matches!(selector.target, Target::Namespace)
        && let Some(terms) = &selector.pod_terms
    {
        if !all.iter().any(|(_, facts)| {
            facts.pods.iter().any(|pod| {
                pod.namespace == owner.name
                    && terms_match(&terms.labels, &terms.expressions, pod, facts)
            })
        }) {
            return false;
        }
    }
    terms_match(&selector.labels, &selector.expressions, owner, facts)
}
fn candidates(facts: &Facts, target: Target) -> &[Resource] {
    match target {
        Target::Pod => &facts.pods,
        Target::Namespace => &facts.namespaces,
        Target::Node => &facts.nodes,
    }
}
fn matched<'a>(selector: &Selector, facts: &'a Facts, all: &[(String, Facts)]) -> Vec<&'a Label> {
    facts
        .labels
        .iter()
        .filter(|label| matches(selector, &label.owner, facts, all))
        .collect()
}

#[derive(Clone)]
struct LabelSubject {
    owner: Resource,
    key: String,
}
fn label_subject(doc: &Document, at: Position) -> Option<LabelSubject> {
    if !matches!(doc.language.as_str(), "yaml" | "json") {
        return None;
    }
    let parsed = facts(&doc.path, &doc.text)?;
    let label = parsed
        .labels
        .into_iter()
        .find(|label| contains(label.key.range, at))?;
    Some(LabelSubject {
        owner: label.owner,
        key: label.key.value,
    })
}
fn label_action(
    doc: &Document,
    at: Position,
    root: &str,
) -> Vec<openidea_plugin::file_actions::FileAction> {
    let Some(subject) = label_subject(doc, at) else {
        return vec![];
    };
    vec![openidea_plugin::file_actions::FileAction {
        title: format!(
            "Rename label key '{}' in connected static sources…",
            subject.key
        ),
        kind: Some("refactor.rename".into()),
        data: Some(
            serde_json::json!([
                "kube-source-rename",
                root,
                doc.path,
                doc.version,
                source_digest(&doc.text),
                at.line,
                at.character,
                "label-key",
                subject.owner.document,
                subject.owner.group,
                subject.owner.kind,
                subject.owner.namespace,
                subject.owner.name,
                subject.key
            ])
            .to_string(),
        ),
        ..Default::default()
    }]
}
fn family(owner: &Resource) -> Target {
    if owner.kind == "Namespace" && owner.group.is_empty() {
        Target::Namespace
    } else if owner.kind == "Node" && owner.group.is_empty() {
        Target::Node
    } else {
        Target::Pod
    }
}
fn same_target(left: Target, right: Target) -> bool {
    std::mem::discriminant(&left) == std::mem::discriminant(&right)
}
fn condition_key(selector: &Selector, key: &str) -> bool {
    selector.labels.iter().any(|(k, _)| k.value == key)
        || selector.expressions.iter().any(|e| e.key.value == key)
}
fn in_selector_scope(
    selector: &Selector,
    owner: &Resource,
    facts: &Facts,
    all: &[(String, Facts)],
) -> bool {
    let mut scope = selector.clone();
    scope.labels.clear();
    scope.expressions.clear();
    if matches!(scope.target, Target::Namespace) {
        scope.pod_terms = None;
    }
    matches(&scope, owner, facts, all)
}
fn label_component(
    all: &[(String, Facts)],
    subject: &LabelSubject,
) -> Result<(Vec<Resource>, BTreeSet<(usize, usize)>), openidea_plugin::Error> {
    let domain = family(&subject.owner);
    let graph_size: usize = all
        .iter()
        .map(|(_, facts)| {
            facts.labels.len()
                + facts
                    .selectors
                    .iter()
                    .map(|s| s.labels.len() + s.expressions.len())
                    .sum::<usize>()
        })
        .sum();
    if graph_size > 4096 {
        return Err(
            "Too many labels and selectors to rename safely (over 4096). Nothing was renamed."
                .into(),
        );
    }
    if matches!(domain, Target::Pod)
        && all.iter().any(|(_, facts)| {
            facts.selectors.iter().any(|selector| {
                same_target(selector.target, domain)
                    && condition_key(selector, &subject.key)
                    && selector.namespace_terms.is_some()
            })
        })
    {
        for owner in all.iter().flat_map(|(_, facts)| &facts.pods) {
            if all
                .iter()
                .flat_map(|(_, facts)| &facts.namespaces)
                .filter(|ns| ns.name == owner.namespace)
                .count()
                != 1
            {
                return Err("A selector depends on a namespace that is not set in exactly one place, so Rename cannot check every match. Nothing was renamed.".into());
            }
        }
    }
    let candidates_in_domain: Vec<_> = all
        .iter()
        .flat_map(|(_, facts)| candidates(facts, domain))
        .collect();
    for owner in &candidates_in_domain {
        if candidates_in_domain
            .iter()
            .filter(|other| {
                other.group == owner.group
                    && other.kind == owner.kind
                    && other.namespace == owner.namespace
                    && other.name == owner.name
            })
            .count()
            != 1
        {
            return Err("Two resources share the same kind, namespace and name, so Rename cannot tell them apart. Nothing was renamed.".into());
        }
    }
    let mut owners = vec![subject.owner.clone()];
    let mut selectors = BTreeSet::new();
    for _ in 0..128 {
        let before = (owners.len(), selectors.len());
        for (file, (_, facts)) in all.iter().enumerate() {
            for (index, selector) in facts.selectors.iter().enumerate() {
                if !same_target(selector.target, domain) || !condition_key(selector, &subject.key) {
                    continue;
                }
                let connected = selectors.contains(&(file, index))
                    || all.iter().any(|(_, facts)| {
                        candidates(facts, domain).iter().any(|owner| {
                            owners.contains(owner) && in_selector_scope(selector, owner, facts, all)
                        })
                    });
                if !connected {
                    continue;
                }
                selectors.insert((file, index));
                for (_, facts) in all {
                    for owner in candidates(facts, domain) {
                        if !owners.contains(owner)
                            && facts.labels.iter().any(|label| {
                                &label.owner == owner && label.key.value == subject.key
                            })
                            && in_selector_scope(selector, owner, facts, all)
                        {
                            owners.push(owner.clone());
                        }
                    }
                }
            }
        }
        if before == (owners.len(), selectors.len()) {
            return Ok((owners, selectors));
        }
    }
    Err("Too many connected labels and selectors to rename safely. Nothing was renamed.".into())
}
fn association_matrix(
    all: &[(String, Facts)],
) -> Result<Vec<(usize, usize, usize, usize)>, openidea_plugin::Error> {
    let mut result = Vec::new();
    let mut inspected = 0usize;
    for (source, (_, facts)) in all.iter().enumerate() {
        for (selector_index, selector) in facts.selectors.iter().enumerate() {
            for (file, (_, facts)) in all.iter().enumerate() {
                for (owner_index, owner) in candidates(facts, selector.target).iter().enumerate() {
                    inspected += 1;
                    if inspected > 65536 {
                        return Err("Too many selectors to compare (over 65536 checks). Nothing was renamed.".into());
                    }
                    if matches(selector, owner, facts, all) {
                        result.push((source, selector_index, file, owner_index));
                    }
                }
            }
        }
    }
    Ok(result)
}
fn replacement_for(
    source: &str,
    range: Range,
    value: &str,
) -> Result<String, openidea_plugin::Error> {
    let start = text::offset(source, range.start).ok_or(openidea_plugin::Error::Stale)?;
    let end = text::offset(source, range.end).ok_or(openidea_plugin::Error::Stale)?;
    let quoted = start > 0
        && matches!(source.as_bytes().get(start - 1), Some(b'\'' | b'\"'))
        && source.as_bytes().get(end) == source.as_bytes().get(start - 1);
    if quoted
        || matches!(
            yaml_rust2::Yaml::from_str(value),
            yaml_rust2::Yaml::String(_)
        )
    {
        Ok(value.into())
    } else {
        serde_json::to_string(value).map_err(|_| openidea_plugin::Error::Stale)
    }
}
fn resolve_label_action(
    doc: &Document,
    data: &serde_json::Value,
) -> Result<openidea_plugin::file_actions::FileAction, openidea_plugin::Error> {
    use openidea_plugin::Error;
    if data[2].as_str() != Some(&doc.path)
        || data[3].as_u64() != Some(doc.version)
        || data[4].as_str() != Some(&source_digest(&doc.text))
    {
        return Err(Error::Stale);
    }
    let at = Position::new(
        u32::try_from(data[5].as_u64().ok_or(Error::Stale)?).map_err(|_| Error::Stale)?,
        u32::try_from(data[6].as_u64().ok_or(Error::Stale)?).map_err(|_| Error::Stale)?,
    );
    let subject = label_subject(doc, at).ok_or(Error::Stale)?;
    if data[8].as_u64() != Some(subject.owner.document as u64)
        || data[9].as_str() != Some(&subject.owner.group)
        || data[10].as_str() != Some(&subject.owner.kind)
        || data[11].as_str() != Some(&subject.owner.namespace)
        || data[12].as_str() != Some(&subject.owner.name)
        || data[13].as_str() != Some(&subject.key)
    {
        return Err(Error::Stale);
    }
    let snapshot = rename_snapshot(doc)?;
    if data[1].as_str() != Some(&snapshot.root) {
        return Err(Error::Stale);
    }
    let all: Vec<_> = snapshot
        .files
        .iter()
        .map(|file| (file.path.clone(), file.facts.clone()))
        .collect();
    let domain = family(&subject.owner);
    let (owners, selectors) = label_component(&all, &subject)?;
    let new_key = host::input(
        "New label key for the connected static source component",
        Some(&subject.key),
    )?
    .ok_or(Error::Cancelled)?;
    if !label_key(&new_key) || new_key == subject.key {
        return Err("Use a different valid Kubernetes label key (optional DNS prefix/name; name at most 63 characters).".into());
    }
    if all.iter().any(|(_, facts)| {
        facts
            .labels
            .iter()
            .any(|label| same_target(family(&label.owner), domain) && label.key.value == new_key)
            || facts.selectors.iter().any(|selector| {
                same_target(selector.target, domain) && condition_key(selector, &new_key)
            })
    }) {
        return Err("The replacement key already appears in this label family; collision-free association preservation cannot be proved.".into());
    }
    let before = association_matrix(&all)?;
    let mut edits = Vec::new();
    let mut after = Vec::new();
    let mut review = format!(
        "Label key {} → {}\n{} resources use it; {} selectors match them. Every selector keeps matching the same resources after this edit.\n\nChanges the {} project YAML/JSON files and open buffers. Ignored files, cluster resources, custom-controller fields, Helm or Kustomize sources and opaque strings are not changed; ambiguous or unreadable files stop the rename. Only files change, not the cluster.\n\n",
        subject.key,
        new_key,
        owners.len(),
        selectors.len(),
        snapshot.files.len()
    );
    for (file_index, file) in snapshot.files.iter().enumerate() {
        let mut ranges: Vec<_> = file
            .facts
            .labels
            .iter()
            .filter(|label| owners.contains(&label.owner) && label.key.value == subject.key)
            .map(|label| label.key.range)
            .chain(
                file.facts
                    .label_uses
                    .iter()
                    .filter(|reference| {
                        owners.contains(&reference.owner) && reference.key.value == subject.key
                    })
                    .map(|reference| reference.key.range),
            )
            .collect();
        for (selector_index, selector) in file.facts.selectors.iter().enumerate() {
            if selectors.contains(&(file_index, selector_index)) {
                ranges.extend(
                    selector
                        .labels
                        .iter()
                        .filter(|(key, _)| key.value == subject.key)
                        .map(|(key, _)| key.range),
                );
                ranges.extend(
                    selector
                        .expressions
                        .iter()
                        .filter(|expression| expression.key.value == subject.key)
                        .map(|expression| expression.key.range),
                );
            }
        }
        ranges.sort_by_key(|range| {
            (
                range.start.line,
                range.start.character,
                range.end.line,
                range.end.character,
            )
        });
        ranges.dedup();
        let mut changes = Vec::new();
        for range in ranges {
            let start = text::offset(&file.source, range.start).ok_or(Error::Stale)?;
            let end = text::offset(&file.source, range.end).ok_or(Error::Stale)?;
            if !spells(&file.source, start, end, &subject.key) {
                return Err(Error::Stale);
            }
            let replacement = replacement_for(&file.source, range, &new_key)?;
            review.push_str(&format!(
                "{}:{}:{}\nBefore key: {}\nAfter key: {}\n\n",
                serde_json::to_string(&file.path).unwrap_or_default(),
                range.start.line + 1,
                range.start.character + 1,
                subject.key,
                replacement
            ));
            changes.push(openidea_plugin::TextEdit {
                range,
                text: replacement,
            });
        }
        let mut rewritten = file.source.clone();
        for change in changes.iter().rev() {
            let start = text::offset(&file.source, change.range.start).ok_or(Error::Stale)?;
            let end = text::offset(&file.source, change.range.end).ok_or(Error::Stale)?;
            rewritten.replace_range(start..end, &change.text);
        }
        after.push((
            file.path.clone(),
            facts(&file.path, &rewritten).ok_or(
                "The new name would change which resources a selector matches. Nothing was renamed.",
            )?,
        ));
        if !changes.is_empty() {
            edits.push((file.path.clone(), changes));
        }
    }
    if association_matrix(&after)? != before {
        return Err(
            "The proposed key would change a modeled selector association; no edits were prepared."
                .into(),
        );
    }
    if review.len() > ui_kit::prompts::MAX_CONFIRM_BYTES {
        return Err(
            "The complete label Rename review exceeds 16KiB; no partial edits were prepared.".into(),
        );
    }
    if !ui_kit::prompts::confirm_exact("Rename connected static Kubernetes label keys?", &review)? {
        return Err(Error::Cancelled);
    }
    rename_unchanged(doc, &snapshot)?;
    Ok(openidea_plugin::file_actions::FileAction {
        title: "Rename connected static Kubernetes label keys".into(),
        kind: Some("refactor.rename".into()),
        edits,
        ..Default::default()
    })
}
struct DataSubject {
    kind: String,
    namespace: String,
    name: String,
    key: String,
}
fn data_subject(doc: &Document, at: Position) -> Option<DataSubject> {
    if !matches!(doc.language.as_str(), "yaml" | "json") {
        return None;
    }
    let source = facts(&doc.path, &doc.text)?;
    if let Some(key) = source.keys.iter().find(|key| contains(key.key.range, at)) {
        return Some(DataSubject {
            kind: key.owner.kind.clone(),
            namespace: key.owner.namespace.clone(),
            name: key.owner.name.clone(),
            key: key.key.value.clone(),
        });
    }
    source
        .references
        .iter()
        .find(|reference| contains(reference.key.range, at))
        .map(|reference| DataSubject {
            kind: reference.kind.clone(),
            namespace: reference.namespace.clone(),
            name: reference.name.clone(),
            key: reference.key.value.clone(),
        })
}
fn same_data_key(owner: &Resource, subject: &DataSubject) -> bool {
    owner.group.is_empty()
        && owner.kind == subject.kind
        && owner.namespace == subject.namespace
        && owner.name == subject.name
}
fn source_digest(source: &str) -> String {
    format!("{:x}", Sha256::digest(source.as_bytes()))
}
struct RenameSource {
    path: String,
    source: String,
    facts: Facts,
}
struct RenameSnapshot {
    root: String,
    files: Vec<RenameSource>,
}
fn exact_tree(node: &Node) -> bool {
    match node {
        Node::Scalar(s) => s.exact,
        Node::Map(pairs) => pairs
            .iter()
            .all(|(key, value)| key.exact && exact_tree(value)),
        Node::Seq(items) => items.iter().all(exact_tree),
    }
}
fn rename_unchanged(doc: &Document, before: &RenameSnapshot) -> Result<(), openidea_plugin::Error> {
    let current = rename_snapshot(doc)?;
    if current.root != before.root
        || current
            .files
            .iter()
            .map(|f| (&f.path, &f.source))
            .collect::<Vec<_>>()
            != before
                .files
                .iter()
                .map(|f| (&f.path, &f.source))
                .collect::<Vec<_>>()
    {
        return Err(openidea_plugin::Error::Stale);
    }
    if host::project_of(&doc.path)
        .ok_or(openidea_plugin::Error::Stale)?
        .read_file(&doc.path)?
        != doc.text
    {
        return Err(openidea_plugin::Error::Stale);
    }
    Ok(())
}
fn rename_snapshot(doc: &Document) -> Result<RenameSnapshot, openidea_plugin::Error> {
    use openidea_plugin::{Error, FileKind};
    let project = host::project_of(&doc.path).ok_or(Error::Stale)?;
    let root = project.root()?;
    if project.read_file(&doc.path)? != doc.text {
        return Err(Error::Stale);
    }
    let mut files = BTreeSet::new();
    for glob in ["**/*.yaml", "**/*.yml", "**/*.json", "**/Kustomization"] {
        let found = project.find_files(glob, 129)?;
        if found.len() >= 129 {
            return Err(
                "The project has too many YAML/JSON files to rename safely. Nothing was renamed."
                    .into(),
            );
        }
        files.extend(found);
    }
    for editor in host::open_editors()? {
        if (editor.path.ends_with(".yaml")
            || editor.path.ends_with(".yml")
            || editor.path.ends_with(".json"))
            && host::project_of(&editor.path)
                .and_then(|p| p.root().ok())
                .as_deref()
                == Some(&root)
        {
            files.insert(editor.path);
        }
    }
    if !files.contains(&doc.path) || files.len() > 128 {
        return Err(
            "Rename works in projects with at most 128 YAML/JSON files.".into(),
        );
    }
    let mut total = 0usize;
    let mut sources = Vec::new();
    for path in files {
        if host::project_of(&path)
            .and_then(|p| p.root().ok())
            .as_deref()
            != Some(&root)
        {
            return Err("Nested/foreign project sources make this inventory ambiguous.".into());
        }
        let name = path.rsplit('/').next().unwrap_or("");
        if matches!(
            name,
            "Chart.yaml" | "kustomization.yaml" | "kustomization.yml" | "Kustomization"
        ) {
            return Err(
                "Helm and Kustomize sources cannot be renamed safely. Nothing was renamed.".into(),
            );
        }
        let stat = project.stat(&path)?;
        if stat.kind != FileKind::File
            || stat.size > MAX_TEXT as u64
            || stat.size > (2 * 1024 * 1024 - total) as u64
        {
            return Err(
                "Rename inventory exceeds static file/byte limits or contains a symlink.".into(),
            );
        }
        let source = project.read_file(&path)?;
        if source.len() > MAX_TEXT || source.len() > 2 * 1024 * 1024 - total {
            return Err("Rename inventory exceeds static byte limits.".into());
        }
        total += source.len();
        let nodes=parse(&source).ok_or("A YAML/JSON file is generated, ambiguous or unreadable. Nothing was renamed.")?;
        if nodes.iter().any(|node| !exact_tree(node)) {
            return Err(
                "Escaped/multiline source spellings are outside this exact static Rename scope."
                    .into(),
            );
        }
        for node in &nodes {
            let kind = string(node.get("kind")).unwrap_or_default();
            if kind == "List" {
                return Err(
                    "Resources inside a Kubernetes List cannot be renamed.".into(),
                );
            }
            let api = string(node.get("apiVersion")).unwrap_or_default();
            let group = if api == "v1" {
                ""
            } else {
                api.split_once('/').map_or("", |(group, _)| group)
            };
            if (pod_path(group, &kind).is_some()
                || group.is_empty() && matches!(kind.as_str(), "ConfigMap" | "Secret"))
                && string(node.at(&["metadata", "namespace"])).is_none()
            {
                return Err("A supported namespaced resource has no explicit namespace; runtime defaults are not safe Rename scope.".into());
            }
        }
        let facts = facts(&path, &source).ok_or(Error::Stale)?;
        sources.push(RenameSource {
            path,
            source,
            facts,
        });
    }
    Ok(RenameSnapshot {
        root,
        files: sources,
    })
}
struct Cached {
    stamp: Option<(u64, Option<u64>)>,
    facts: Facts,
}
#[derive(Default)]
pub(crate) struct Cache {
    files: crate::lru::Lru<(String, String), Cached>,
}
impl Cache {
    pub(crate) fn file_actions(
        &self,
        doc: &Document,
        selection: Range,
    ) -> Vec<openidea_plugin::file_actions::FileAction> {
        let Some(project) = host::project_of(&doc.path) else {
            return vec![];
        };
        let Ok(root) = project.root() else {
            return vec![];
        };
        let Some(subject) = data_subject(doc, selection.start) else {
            return label_action(doc, selection.start, &root);
        };
        vec![openidea_plugin::file_actions::FileAction {
            title: format!(
                "Rename {} data key '{}' in static project sources…",
                subject.kind, subject.key
            ),
            kind: Some("refactor.rename".into()),
            data: Some(
                serde_json::json!([
                    "kube-source-rename",
                    root,
                    doc.path,
                    doc.version,
                    source_digest(&doc.text),
                    selection.start.line,
                    selection.start.character,
                    subject.kind,
                    subject.namespace,
                    subject.name,
                    subject.key
                ])
                .to_string(),
            ),
            ..Default::default()
        }]
    }
    pub(crate) fn resolve_file_action(
        &self,
        doc: &Document,
        action: openidea_plugin::file_actions::FileAction,
    ) -> Result<openidea_plugin::file_actions::FileAction, openidea_plugin::Error> {
        let data: serde_json::Value = serde_json::from_str(
            action
                .data
                .as_deref()
                .ok_or(openidea_plugin::Error::Stale)?,
        )
        .map_err(|_| openidea_plugin::Error::Stale)?;
        if data[0].as_str() == Some("kube-source-rename") && data[7].as_str() == Some("label-key") {
            return resolve_label_action(doc, &data);
        }
        if data[0].as_str() != Some("kube-source-rename")
            || data[2].as_str() != Some(&doc.path)
            || data[3].as_u64() != Some(doc.version)
            || data[4].as_str() != Some(&source_digest(&doc.text))
        {
            return Err(openidea_plugin::Error::Stale);
        }
        let line = u32::try_from(data[5].as_u64().ok_or(openidea_plugin::Error::Stale)?)
            .map_err(|_| openidea_plugin::Error::Stale)?;
        let character = u32::try_from(data[6].as_u64().ok_or(openidea_plugin::Error::Stale)?)
            .map_err(|_| openidea_plugin::Error::Stale)?;
        let subject = data_subject(doc, Position::new(line, character))
            .ok_or(openidea_plugin::Error::Stale)?;
        if data[7].as_str() != Some(&subject.kind)
            || data[8].as_str() != Some(&subject.namespace)
            || data[9].as_str() != Some(&subject.name)
            || data[10].as_str() != Some(&subject.key)
        {
            return Err(openidea_plugin::Error::Stale);
        }
        let snapshot = rename_snapshot(doc)?;
        if data[1].as_str() != Some(&snapshot.root) {
            return Err(openidea_plugin::Error::Stale);
        }
        let declarations: Vec<_> = snapshot
            .files
            .iter()
            .flat_map(|file| file.facts.data_resources.iter())
            .filter(|owner| {
                owner.kind == subject.kind
                    && owner.namespace == subject.namespace
                    && owner.name == subject.name
            })
            .collect();
        if declarations.len() != 1 {
            return Err("The data resource has missing or duplicate source declarations; no Rename was prepared.".into());
        }
        let definitions: Vec<_> = snapshot
            .files
            .iter()
            .flat_map(|file| file.facts.keys.iter())
            .filter(|key| same_data_key(&key.owner, &subject) && key.key.value == subject.key)
            .collect();
        if definitions.len() != 1 {
            return Err(
                "The data key has missing or duplicate definitions; no Rename was prepared.".into(),
            );
        }
        let new_key = host::input(
            &format!(
                "New {} data key for {}/{} (static project sources only)",
                subject.kind, subject.namespace, subject.name
            ),
            Some(&subject.key),
        )?
        .ok_or(openidea_plugin::Error::Cancelled)?;
        if !key_name(&new_key) || new_key == subject.key {
            return Err("Use a different data key containing only letters, digits, '.', '_' or '-' (at most 253 bytes).".into());
        }
        if snapshot
            .files
            .iter()
            .flat_map(|file| &file.facts.keys)
            .any(|key| same_data_key(&key.owner, &subject) && key.key.value == new_key)
        {
            return Err(
                "The replacement data key already exists in this resource; nothing was changed."
                    .into(),
            );
        }
        let mut edits = Vec::new();
        let mut review = format!(
            "{} {}/{}: {} → {}\n\nChanges the {} project YAML/JSON files plus open YAML/JSON buffers. Ignored files, cluster resources, custom-controller fields and opaque strings are not changed; Helm, Kustomize, ambiguous or unreadable files stop the rename. Only files change, not the cluster.\n\n",
            subject.kind,
            subject.namespace,
            subject.name,
            subject.key,
            new_key,
            snapshot.files.len()
        );
        for file in &snapshot.files {
            let mut ranges: Vec<_> = file
                .facts
                .keys
                .iter()
                .filter(|key| same_data_key(&key.owner, &subject) && key.key.value == subject.key)
                .map(|key| key.key.range)
                .chain(
                    file.facts
                        .references
                        .iter()
                        .filter(|reference| {
                            reference.kind == subject.kind
                                && reference.namespace == subject.namespace
                                && reference.name == subject.name
                                && reference.key.value == subject.key
                        })
                        .map(|reference| reference.key.range),
                )
                .collect();
            ranges.sort_by_key(|r| (r.start.line, r.start.character, r.end.line, r.end.character));
            ranges.dedup();
            if ranges.is_empty() {
                continue;
            }
            let mut changes = Vec::new();
            for range in ranges {
                let start =
                    text::offset(&file.source, range.start).ok_or(openidea_plugin::Error::Stale)?;
                let end =
                    text::offset(&file.source, range.end).ok_or(openidea_plugin::Error::Stale)?;
                if !spells(&file.source, start, end, &subject.key) {
                    return Err(openidea_plugin::Error::Stale);
                }
                let replacement = replacement_for(&file.source, range, &new_key)?;
                review.push_str(&format!(
                    "{}:{}:{}\nBefore key: {}\nAfter key: {}\n\n",
                    serde_json::to_string(&file.path).unwrap_or_default(),
                    range.start.line + 1,
                    range.start.character + 1,
                    subject.key,
                    replacement
                ));
                changes.push(openidea_plugin::TextEdit {
                    range,
                    text: replacement,
                });
            }
            edits.push((file.path.clone(), changes));
        }
        if review.len() > ui_kit::prompts::MAX_CONFIRM_BYTES {
            return Err(
                "The complete Rename review exceeds 16KiB; no partial edits were prepared.".into(),
            );
        }
        if !ui_kit::prompts::confirm_exact("Rename static Kubernetes data-key sources?", &review)? {
            return Err(openidea_plugin::Error::Cancelled);
        }
        rename_unchanged(doc, &snapshot)?;
        Ok(openidea_plugin::file_actions::FileAction {
            title: format!("Rename {} data key in static project sources", subject.kind),
            kind: Some("refactor.rename".into()),
            edits,
            ..Default::default()
        })
    }
    pub(crate) fn clear(&mut self) {
        self.files.clear();
    }
    pub(crate) fn evict(&mut self, path: &str) {
        self.files.retain(|(_, p), _| p != path);
    }
    pub(crate) fn warm_file(&mut self, path: &str) {
        if let Some(project) = host::project_of(path)
            && let Ok(source) = project.read_file(path)
        {
            self.warm(path, &source);
        }
    }
    pub(crate) fn warm(&mut self, path: &str, source: &str) {
        self.files.retain(|(_, p), _| p != path);
        if source.len() > MAX_TEXT
            || !(path.ends_with(".yaml") || path.ends_with(".yml") || path.ends_with(".json"))
        {
            return;
        }
        let Some(project) = host::project_of(path) else {
            return;
        };
        let Ok(root) = project.root() else {
            return;
        };
        let Some(facts) = facts(path, source) else {
            return;
        };
        self.files.insert(
            (root, path.into()),
            Cached {
                stamp: project.stat(path).ok().map(|stat| (stat.size, stat.modified)),
                facts,
            },
            MAX_FILES,
        );
    }
    fn current(&self, doc: &Document) -> Option<Vec<(String, Facts)>> {
        if !matches!(doc.language.as_str(), "yaml" | "json") || doc.text.len() > MAX_TEXT {
            return None;
        }
        let project = host::project_of(&doc.path)?;
        let root = project.root().ok()?;
        project.read_file(&doc.path).ok()?;
        let mut all = vec![(doc.path.clone(), facts(&doc.path, &doc.text)?)];
        for ((owner, path), cache) in &self.files {
            if owner != &root
                || path == &doc.path
                || host::project_of(path)
                    .and_then(|p| p.root().ok())
                    .as_deref()
                    != Some(&root)
            {
                continue;
            }
            let current = project.stat(path).ok().map(|stat| (stat.size, stat.modified));
            if current.is_none() || current != cache.stamp {
                continue;
            }
            all.push((path.clone(), cache.facts.clone()));
        }
        Some(all)
    }
    pub(crate) fn definition(&self, doc: &Document, at: Position) -> Vec<Location> {
        let Some(all) = self.current(doc) else {
            return vec![];
        };
        Self::definition_in(&all, at)
    }
    fn definition_in(all: &[(String, Facts)], at: Position) -> Vec<Location> {
        let src = &all[0].1;
        let mut result = Vec::new();
        for selector in &src.selectors {
            for (key, value) in &selector.labels {
                let value_at = contains(value.range, at);
                if !value_at && !contains(key.range, at) {
                    continue;
                }
                for (path, facts) in all {
                    for label in matched(selector, facts, all) {
                        if label.key.value == key.value {
                            result.push(loc(
                                path,
                                if value_at {
                                    label.value.range
                                } else {
                                    label.key.range
                                },
                            ));
                        }
                    }
                }
            }
            for expression in &selector.expressions {
                let value_at = expression.values.iter().find(|v| contains(v.range, at));
                if !contains(expression.key.range, at) && value_at.is_none() {
                    continue;
                }
                for (path, facts) in all {
                    for owner in candidates(facts, selector.target)
                        .iter()
                        .filter(|owner| matches(selector, owner, facts, all))
                    {
                        let target = facts
                            .labels
                            .iter()
                            .find(|l| &l.owner == owner && l.key.value == expression.key.value);
                        let range = match (expression.operator, target, value_at) {
                            (Operator::In, Some(label), Some(value))
                                if label.value.value == value.value =>
                            {
                                label.value.range
                            }
                            (Operator::In, _, Some(_)) => continue,
                            (Operator::NotIn, _, Some(_)) => owner.range,
                            (_, Some(label), _) => label.key.range,
                            (_, None, _) => owner.range,
                        };
                        result.push(loc(path, range));
                    }
                }
            }
        }
        for reference in src.label_uses.iter().filter(|r| contains(r.key.range, at)) {
            result.extend(
                src.labels
                    .iter()
                    .filter(|l| l.owner == reference.owner && l.key.value == reference.key.value)
                    .map(|label| loc(&all[0].0, label.key.range)),
            );
        }
        for reference in src.references.iter().filter(|r| contains(r.key.range, at)) {
            let targets: Vec<_> = all
                .iter()
                .flat_map(|(path, facts)| {
                    facts
                        .keys
                        .iter()
                        .filter(move |k| {
                            k.owner.kind == reference.kind
                                && k.owner.namespace == reference.namespace
                                && k.owner.name == reference.name
                                && k.key.value == reference.key.value
                        })
                        .map(move |k| loc(path, k.key.range))
                })
                .collect();
            if targets.len() == 1 {
                result.extend(targets);
            }
        }
        unique(result)
    }
    pub(crate) fn occurrences(&self, doc: &Document, at: Position) -> Vec<Location> {
        let Some(all) = self.current(doc) else {
            return vec![];
        };
        let src = &all[0].1;
        let mut out = Vec::new();
        for label in &src.labels {
            let value_at = contains(label.value.range, at);
            if !value_at && !contains(label.key.range, at) {
                continue;
            }
            out.extend(
                src.label_uses
                    .iter()
                    .filter(|r| r.owner == label.owner && r.key.value == label.key.value)
                    .map(|r| loc(&doc.path, r.key.range)),
            );
            for (path, facts) in &all {
                for selector in &facts.selectors {
                    if !matches(selector, &label.owner, src, &all) {
                        continue;
                    }
                    for (key, value) in &selector.labels {
                        if key.value == label.key.value {
                            out.push(loc(path, if value_at { value.range } else { key.range }));
                        }
                    }
                    for expression in &selector.expressions {
                        if expression.key.value != label.key.value {
                            continue;
                        }
                        if value_at && matches!(expression.operator, Operator::In) {
                            out.extend(
                                expression
                                    .values
                                    .iter()
                                    .filter(|v| v.value == label.value.value)
                                    .map(|v| loc(path, v.range)),
                            );
                        } else {
                            out.push(loc(path, expression.key.range));
                        }
                    }
                }
            }
        }
        for owner in src
            .pods
            .iter()
            .chain(&src.namespaces)
            .chain(&src.nodes)
            .filter(|p| contains(p.range, at))
        {
            for (path, facts) in &all {
                for selector in &facts.selectors {
                    if matches(selector, owner, src, &all) {
                        out.extend(selector.labels.iter().map(|(key, _)| loc(path, key.range)));
                        out.extend(selector.expressions.iter().map(|e| loc(path, e.key.range)));
                    }
                }
            }
        }
        for reference in src.label_uses.iter().filter(|r| contains(r.key.range, at)) {
            out.extend(
                src.label_uses
                    .iter()
                    .filter(|r| r.owner == reference.owner && r.key.value == reference.key.value)
                    .map(|r| loc(&doc.path, r.key.range)),
            );
        }
        for reference in src.references.iter().filter(|r| contains(r.key.range, at)) {
            let targets = all
                .iter()
                .flat_map(|(_, f)| &f.keys)
                .filter(|k| {
                    k.owner.namespace == reference.namespace
                        && k.owner.kind == reference.kind
                        && k.owner.name == reference.name
                        && k.key.value == reference.key.value
                })
                .count();
            if targets != 1 {
                continue;
            }
            for (path, facts) in &all {
                for candidate in &facts.references {
                    if candidate.namespace == reference.namespace
                        && candidate.kind == reference.kind
                        && candidate.name == reference.name
                        && candidate.key.value == reference.key.value
                    {
                        out.push(loc(path, candidate.key.range));
                    }
                }
            }
        }
        for key in src.keys.iter().filter(|k| contains(k.key.range, at)) {
            let targets = all
                .iter()
                .flat_map(|(_, f)| &f.keys)
                .filter(|k| {
                    k.owner.namespace == key.owner.namespace
                        && k.owner.kind == key.owner.kind
                        && k.owner.name == key.owner.name
                        && k.key.value == key.key.value
                })
                .count();
            if targets != 1 {
                continue;
            }
            for (path, facts) in &all {
                for reference in &facts.references {
                    if reference.namespace == key.owner.namespace
                        && reference.kind == key.owner.kind
                        && reference.name == key.owner.name
                        && reference.key.value == key.key.value
                    {
                        out.push(loc(path, reference.key.range));
                    }
                }
            }
        }
        unique(out)
    }
    /// Explicit read-only source-occurrence picker. Indexing is deliberately
    /// not used: the host would also offer automatic Rename without closure.
    pub(crate) fn show_occurrences(
        &self,
        ctx: &openidea_plugin::Invocation,
    ) -> Result<Option<String>, openidea_plugin::Error> {
        let editor = ctx
            .editor()
            .ok_or("Open a Kubernetes manifest and place the caret on a label or data key.")?;
        let raw = editor.document()?;
        let doc = Document {
            path: editor.path(),
            language: editor.language(),
            version: raw.version,
            text: raw.text,
        };
        let at = editor
            .selections()?
            .first()
            .map(|r| r.start)
            .ok_or("Place the caret on a label or data key.")?;
        let found = self.occurrences(&doc, at);
        if found.is_empty() {
            return Ok(Some("No unambiguous local source occurrences in indexed/opened manifests. Dynamic/generated or namespace-ambiguous sources are not included.".into()));
        }
        let items: Vec<_> = found
            .iter()
            .enumerate()
            .map(|(i, location)| {
                openidea_plugin::PickItem::new(
                    &i.to_string(),
                    &format!(
                        "{}:{}:{}",
                        location.path,
                        location.range.start.line + 1,
                        location.range.start.character + 1
                    ),
                )
            })
            .collect();
        let Some(chosen) = host::pick("Read-only Kubernetes source occurrences", &items)? else {
            return Ok(None);
        };
        let index: usize = chosen.parse().map_err(|_| openidea_plugin::Error::Stale)?;
        let target = found.get(index).ok_or(openidea_plugin::Error::Stale)?;
        let raw = editor.document()?;
        let current = Document {
            path: editor.path(),
            language: editor.language(),
            version: raw.version,
            text: raw.text,
        };
        if current.version != doc.version
            || current.text != doc.text
            || !self.occurrences(&current, at).contains(target)
        {
            return Err(openidea_plugin::Error::Stale);
        }
        host::open_location(
            &target.path,
            target.range.start.line,
            target.range.start.character,
        )?;
        Ok(None)
    }
    pub(crate) fn markers(&self, doc: &Document) -> Vec<LineMarker> {
        let Some(all) = self.current(doc) else {
            return vec![];
        };
        let facts = &all[0].1;
        let ranges: Vec<_> = facts
            .selectors
            .iter()
            .flat_map(|s| {
                s.labels
                    .iter()
                    .map(|(key, _)| key.range)
                    .chain(s.expressions.iter().map(|e| e.key.range))
            })
            .chain(facts.references.iter().map(|r| r.key.range))
            .chain(facts.label_uses.iter().map(|r| r.key.range))
            .collect();
        // One marker per target with the same icon: the IDE merges a line's markers into one button with a chooser.
        let mut lines: BTreeMap<u32, Vec<Location>> = BTreeMap::new();
        for range in ranges {
            let targets = lines.entry(range.start.line).or_default();
            for target in Self::definition_in(&all, range.start) {
                if target.range.start != target.range.end && !targets.contains(&target) {
                    targets.push(target);
                }
            }
        }
        lines
            .into_iter()
            .flat_map(|(line, targets)| {
                let total = targets.len();
                let tooltip = if total > MAX_LINE_TARGETS {
                    format!("Read-only exact label/key source candidate (first {MAX_LINE_TARGETS} of {total} shown)")
                } else {
                    "Read-only exact label/key source candidate".to_owned()
                };
                targets.into_iter().take(MAX_LINE_TARGETS).map(move |target| LineMarker {
                    line,
                    icon: Some(crate::LABEL_MARKER.into()),
                    tooltip: tooltip.clone(),
                    target: Some(target),
                })
            })
            .collect()
    }
    pub(crate) fn completion(&self, doc: &Document, at: Position) -> Vec<CompletionItem> {
        let Some(all) = self.current(doc) else {
            return vec![];
        };
        let src = &all[0].1;
        if let Some(reference) = src.label_uses.iter().find(|r| contains(r.key.range, at)) {
            return src
                .labels
                .iter()
                .filter(|l| l.owner == reference.owner)
                .map(|label| {
                    let mut item = CompletionItem::new(&label.key.value, CompletionKind::Property);
                    item.insert_text = label.key.value.clone();
                    item.replace = Some(reference.key.range);
                    item.detail = Some("Current Pod/template label key".into());
                    item
                })
                .collect();
        }
        let Some(reference) = src.references.iter().find(|r| contains(r.key.range, at)) else {
            return vec![];
        };
        let mut keys = BTreeSet::new();
        for (_, facts) in &all {
            for key in &facts.keys {
                if key.owner.namespace == reference.namespace
                    && key.owner.kind == reference.kind
                    && key.owner.name == reference.name
                {
                    keys.insert(key.key.value.clone());
                }
            }
        }
        keys.into_iter()
            .map(|key| {
                let mut item = CompletionItem::new(&key, CompletionKind::Property);
                item.insert_text = key;
                item.replace = Some(reference.key.range);
                item.detail = Some("Local ConfigMap/Secret key (source candidate)".into());
                item
            })
            .collect()
    }
}
fn unique(mut locations: Vec<Location>) -> Vec<Location> {
    locations.sort_by(|a, b| {
        (&a.path, a.range.start.line, a.range.start.character).cmp(&(
            &b.path,
            b.range.start.line,
            b.range.start.character,
        ))
    });
    locations.dedup();
    locations
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    const POD: &str = "apiVersion: v1\nkind: Pod\nmetadata:\n  name: web\n  namespace: app\n  labels: {app: web, tier: frontend}\nspec:\n  containers:\n  - name: web\n    env:\n    - name: CONFIG\n      valueFrom:\n        configMapKeyRef: {name: settings, key: server}\n    - name: PASSWORD\n      valueFrom:\n        secretKeyRef: {name: settings, key: server}\n";
    const SERVICE: &str = "apiVersion: v1\nkind: Service\nmetadata: {name: web, namespace: app}\nspec:\n  selector: {app: web, tier: frontend}\n";
    const CM: &str = "apiVersion: v1\nkind: ConfigMap\nmetadata: {name: settings, namespace: app}\ndata:\n  server: http://localhost\n  port: '80'\n";
    const SECRET: &str = "apiVersion: v1\nkind: Secret\nmetadata: {name: settings, namespace: app}\ndata:\n  server: do-not-cache-this-secret-value\n";
    fn document(path: &str, source: &str) -> Document {
        Document {
            path: format!("/p/{path}"),
            language: "yaml".into(),
            version: 1,
            text: source.into(),
        }
    }
    fn setup(files: &[(&str, &str)]) -> (MockHost, Cache) {
        let host = MockHost::install().with_capabilities(&["project.read"]);
        host.set_project_root(Some("/p"));
        let mut cache = Cache::default();
        for (path, source) in files {
            host.add_file(path, source);
            cache.warm(&format!("/p/{path}"), source);
        }
        (host, cache)
    }
    fn caret(source: &str, needle: &str) -> Position {
        text::position(source, source.find(needle).unwrap())
    }
    fn policy(selector: &str) -> String {
        format!(
            "apiVersion: networking.k8s.io/v1\nkind: NetworkPolicy\nmetadata: {{name: policy, namespace: app}}\nspec:\n  podSelector:\n{selector}"
        )
    }
    #[test]
    fn match_expressions_apply_complete_conjunction_and_set_semantics() {
        let source = policy(
            "    matchLabels: {app: web}\n    matchExpressions:\n    - {key: tier, operator: In, values: [frontend, backend]}\n    - {key: region, operator: NotIn, values: [us]}\n    - {key: retired, operator: DoesNotExist}\n    - {key: app, operator: Exists}\n",
        );
        let (_host, cache) = setup(&[("pod.yaml", POD), ("policy.yaml", &source)]);
        let doc = document("policy.yaml", &source);
        let found = cache.definition(&doc, caret(&source, "tier, operator"));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].range.start, caret(POD, "tier: frontend"));
        let found = cache.definition(&doc, caret(&source, "frontend, backend"));
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].range.start,
            text::position(POD, POD.find("tier: frontend").unwrap() + 6)
        );
        let rejected = source
            .replace("values: [us]", "values: [frontend]")
            .replace("key: region", "key: tier");
        let rejected = document("policy.yaml", &rejected);
        assert!(
            cache
                .definition(&rejected, caret(&rejected.text, "app: web"))
                .is_empty()
        );
        let missing = source.replace(
            "key: app, operator: Exists",
            "key: missing, operator: Exists",
        );
        let missing = document("policy.yaml", &missing);
        assert!(
            cache
                .definition(&missing, caret(&missing.text, "tier, operator"))
                .is_empty()
        );
    }
    #[test]
    fn absent_label_conditions_match_known_empty_pod_labels_without_inventing_a_definition() {
        let source = policy(
            "    matchExpressions:\n    - {key: disabled, operator: DoesNotExist}\n    - {key: region, operator: NotIn, values: [us]}\n",
        );
        let pod = "apiVersion: v1\nkind: Pod\nmetadata: {name: empty, namespace: app}\nspec: {containers: []}\n";
        let (_host, cache) = setup(&[("pod.yaml", pod), ("policy.yaml", &source)]);
        let doc = document("policy.yaml", &source);
        let found = cache.definition(&doc, caret(&source, "disabled, operator"));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].range.start, caret(pod, "empty, namespace"));
        assert_eq!(
            cache
                .definition(&doc, caret(&source, "region, operator"))
                .len(),
            1
        );
    }
    #[test]
    fn invalid_expression_shapes_withhold_the_whole_selector() {
        for expression in [
            "{key: tier, operator: In, values: []}",
            "{key: tier, operator: Exists, values: [frontend]}",
            "{key: tier, operator: Unknown}",
            "{key: tier, operator: NotIn}",
            "{key: tier, operator: In, values: [true]}",
            "{key: tier, operator: Exists, invalid: true}",
        ] {
            let source = policy(&format!(
                "    matchLabels: {{app: web}}\n    matchExpressions: [{expression}]\n"
            ));
            let (_host, cache) = setup(&[("pod.yaml", POD), ("policy.yaml", &source)]);
            let doc = document("policy.yaml", &source);
            assert!(
                cache
                    .definition(&doc, caret(&source, "app: web"))
                    .is_empty(),
                "{expression}"
            );
        }
    }
    #[test]
    fn own_template_expressions_do_not_link_other_workload_templates() {
        let source = "apiVersion: apps/v1\nkind: Deployment\nmetadata: {name: web, namespace: app}\nspec:\n  selector:\n    matchExpressions: [{key: tier, operator: In, values: [frontend]}]\n  template:\n    metadata: {labels: {tier: frontend}}\n    spec: {containers: []}\n";
        let (_host, cache) = setup(&[
            ("deployment.yaml", source),
            ("copy.yaml", source),
            ("pod.yaml", POD),
        ]);
        let doc = document("deployment.yaml", source);
        let found = cache.definition(&doc, caret(source, "tier, operator"));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, "/p/deployment.yaml");
        let uses = cache.occurrences(
            &doc,
            text::position(source, source.rfind("tier: frontend").unwrap()),
        );
        assert_eq!(uses.len(), 1);
        assert_eq!(uses[0].range.start, caret(source, "tier, operator"));
    }

    #[test]
    fn projected_and_direct_volume_item_keys_keep_resource_kind_and_namespace_scope() {
        let source = "apiVersion: v1\nkind: Pod\nmetadata: {name: web, namespace: app}\nspec:\n  volumes:\n  - name: direct\n    configMap: {name: settings, items: [{key: server, path: direct}]}\n  - name: projected\n    projected:\n      sources:\n      - configMap: {name: settings, items: [{key: server, path: config}]}\n      - secret: {name: settings, items: [{key: server, path: secret}]}\n";
        let (_host, cache) = setup(&[
            ("pod.yaml", source),
            ("cm.yaml", CM),
            ("secret.yaml", SECRET),
        ]);
        let doc = document("pod.yaml", source);
        let refs: Vec<_> = source
            .match_indices("key: server")
            .map(|(i, _)| text::position(source, i + 5))
            .collect();
        for (at, expected) in refs
            .into_iter()
            .zip(["/p/cm.yaml", "/p/cm.yaml", "/p/secret.yaml"])
        {
            let found = cache.definition(&doc, at);
            assert_eq!(found.len(), 1);
            assert_eq!(found[0].path, expected);
        }
    }
    #[test]
    fn downward_api_label_subscripts_use_exact_source_keys_and_support_readonly_usages() {
        let source = "apiVersion: v1\nkind: Pod\nmetadata: {name: web, namespace: app, labels: {app: web, tier: frontend}}\nspec:\n  containers:\n  - name: web\n    env:\n    - name: APP\n      valueFrom:\n        fieldRef: {fieldPath: \"metadata.labels['app']\"}\n  volumes:\n  - name: labels\n    downwardAPI:\n      items:\n      - path: app\n        fieldRef: {fieldPath: \"metadata.labels['app']\"}\n";
        let (_host, cache) = setup(&[("pod.yaml", source)]);
        let doc = document("pod.yaml", source);
        let at = text::position(source, source.find("labels['app']").unwrap() + 8);
        let found = cache.definition(&doc, at);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].range.start, caret(source, "app: web, tier"));
        let uses = cache.occurrences(&doc, caret(source, "app: web, tier"));
        assert_eq!(uses.len(), 2);
        assert_eq!(
            cache
                .completion(&doc, at)
                .iter()
                .map(|i| i.label.as_str())
                .collect::<Vec<_>>(),
            ["app", "tier"]
        );
        let range = cache.completion(&doc, at)[0].replace.unwrap();
        assert_eq!(range.end.character - range.start.character, 3);
    }
    #[test]
    fn negative_expression_usages_are_attached_to_the_proven_empty_resource_not_a_fake_key() {
        let source = policy("    matchExpressions: [{key: disabled, operator: DoesNotExist}]\n");
        let pod = "apiVersion: v1\nkind: Pod\nmetadata: {name: empty, namespace: app}\nspec: {containers: []}\n";
        let (_host, cache) = setup(&[("pod.yaml", pod), ("policy.yaml", &source)]);
        let doc = document("pod.yaml", pod);
        let found = cache.occurrences(&doc, caret(pod, "empty, namespace"));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].range.start, caret(&source, "disabled, operator"));
    }

    fn namespace(name: &str, labels: &str) -> String {
        format!("apiVersion: v1\nkind: Namespace\nmetadata:\n  name: {name}\n  labels: {labels}\n")
    }
    fn peer_policy(peers: &str) -> String {
        format!(
            "apiVersion: networking.k8s.io/v1\nkind: NetworkPolicy\nmetadata: {{name: policy, namespace: app}}\nspec:\n  podSelector: {{}}\n  ingress:\n  - from:\n{peers}"
        )
    }
    #[test]
    fn combined_peer_namespace_and_pod_selectors_are_conjunctive_and_source_owned() {
        let source = peer_policy(
            "    - namespaceSelector:\n        matchLabels: {team: ops}\n      podSelector:\n        matchLabels: {app: web}\n",
        );
        let ns = namespace("remote", "{team: ops}");
        let local_ns = namespace("app", "{team: dev}");
        let pod = POD.replace("namespace: app", "namespace: remote");
        let (_host, cache) = setup(&[
            ("policy.yaml", &source),
            ("namespace.yaml", &ns),
            ("local-ns.yaml", &local_ns),
            ("pod.yaml", &pod),
            ("local-pod.yaml", POD),
        ]);
        let doc = document("policy.yaml", &source);
        let found = cache.definition(&doc, caret(&source, "app: web"));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, "/p/pod.yaml");
        let found = cache.definition(&doc, caret(&source, "team: ops"));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, "/p/namespace.yaml");
        let wrong = document("policy.yaml", &source.replace("app: web", "app: absent"));
        assert!(
            cache
                .definition(&wrong, caret(&wrong.text, "team: ops"))
                .is_empty()
        );
        let uses = cache.occurrences(&document("namespace.yaml", &ns), caret(&ns, "team: ops"));
        assert_eq!(uses.len(), 1);
        assert_eq!(uses[0].path, "/p/policy.yaml");
    }
    #[test]
    fn separate_network_policy_peers_remain_or_and_pod_only_peer_stays_same_namespace() {
        let source = peer_policy(
            "    - namespaceSelector: {matchLabels: {team: ops}}\n    - podSelector: {matchLabels: {app: web}}\n",
        );
        let ns = namespace("remote", "{team: ops}");
        let remote = POD.replace("namespace: app", "namespace: remote");
        let (_host, cache) = setup(&[
            ("policy.yaml", &source),
            ("namespace.yaml", &ns),
            ("remote-pod.yaml", &remote),
            ("pod.yaml", POD),
        ]);
        let doc = document("policy.yaml", &source);
        assert_eq!(
            cache.definition(&doc, caret(&source, "app: web"))[0].path,
            "/p/pod.yaml"
        );
        assert_eq!(
            cache.definition(&doc, caret(&source, "team: ops"))[0].path,
            "/p/namespace.yaml"
        );
    }
    #[test]
    fn namespace_expressions_support_absence_and_revoke_on_stale_or_duplicate_namespace_sources() {
        let source = peer_policy(
            "    - namespaceSelector:\n        matchExpressions: [{key: retired, operator: DoesNotExist}]\n      podSelector: {matchLabels: {app: web}}\n",
        );
        let ns = namespace("remote", "{}");
        let pod = POD.replace("namespace: app", "namespace: remote");
        let (host, mut cache) = setup(&[
            ("policy.yaml", &source),
            ("namespace.yaml", &ns),
            ("pod.yaml", &pod),
        ]);
        let doc = document("policy.yaml", &source);
        let at = caret(&source, "app: web");
        assert_eq!(cache.definition(&doc, at).len(), 1);
        let target = cache.definition(&doc, caret(&source, "retired, operator"));
        assert_eq!(target.len(), 1);
        assert_eq!(target[0].range.start, caret(&ns, "remote\n"));
        host.add_file("namespace.yaml", &namespace("remote", "{retired: 'true'}"));
        assert!(cache.definition(&doc, at).is_empty());
        host.add_file("namespace.yaml", &ns);
        host.add_file("duplicate.yaml", &ns);
        cache.warm("/p/duplicate.yaml", &ns);
        assert!(cache.definition(&doc, at).is_empty());
        assert!(
            cache
                .definition(&doc, caret(&source, "retired, operator"))
                .is_empty()
        );
    }
    #[test]
    fn empty_namespace_selector_requires_a_known_explicit_namespace_declaration() {
        let source = peer_policy(
            "    - namespaceSelector: {}\n      podSelector: {matchLabels: {app: web}}\n",
        );
        let remote = POD.replace("namespace: app", "namespace: remote");
        let (host, mut cache) = setup(&[("policy.yaml", &source), ("pod.yaml", &remote)]);
        let doc = document("policy.yaml", &source);
        let at = caret(&source, "app: web");
        assert!(cache.definition(&doc, at).is_empty());
        let ns = namespace("remote", "{}");
        host.add_file("namespace.yaml", &ns);
        cache.warm("/p/namespace.yaml", &ns);
        assert_eq!(cache.definition(&doc, at).len(), 1);
    }
    #[test]
    fn affinity_and_spread_selectors_link_exact_terms_and_refuse_extended_scope() {
        let source = "apiVersion: v1\nkind: Pod\nmetadata: {name: web, namespace: app}\nspec:\n  affinity:\n    nodeAffinity:\n      requiredDuringSchedulingIgnoredDuringExecution:\n        nodeSelectorTerms:\n          - matchExpressions: [{key: disk, operator: In, values: [ssd]}]\n          - matchExpressions: [{key: gpu, operator: Gt, values: ['1']}]\n    podAntiAffinity:\n      requiredDuringSchedulingIgnoredDuringExecution:\n        - topologyKey: zone\n          labelSelector: {matchLabels: {role: db}}\n        - topologyKey: zone\n          namespaces: [other]\n          labelSelector: {matchLabels: {role: cache}}\n  topologySpreadConstraints:\n    - maxSkew: 1\n      topologyKey: zone\n      whenUnsatisfiable: DoNotSchedule\n      labelSelector: {matchLabels: {role: web}}\n  containers: []\n";
        let n = node("worker-a", "{disk: ssd, gpu: '2'}");
        let db = "apiVersion: v1\nkind: Pod\nmetadata: {name: db, namespace: app, labels: {role: db}}\nspec: {containers: []}\n";
        let foreign = "apiVersion: v1\nkind: Pod\nmetadata: {name: db2, namespace: other, labels: {role: db}}\nspec: {containers: []}\n";
        let cache_pod = "apiVersion: v1\nkind: Pod\nmetadata: {name: c, namespace: other, labels: {role: cache}}\nspec: {containers: []}\n";
        let (_host, cache) = setup(&[
            ("pod.yaml", source),
            ("node.yaml", &n),
            ("db.yaml", db),
            ("foreign.yaml", foreign),
            ("cache.yaml", cache_pod),
        ]);
        let doc = document("pod.yaml", source);
        let found = cache.definition(&doc, caret(source, "disk, operator"));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, "/p/node.yaml");
        let found = cache.definition(&doc, caret(source, "role: db"));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, "/p/db.yaml");
        assert!(cache.definition(&doc, caret(source, "gpu, operator")).is_empty());
        assert!(cache.definition(&doc, caret(source, "role: cache")).is_empty());
    }
    #[test]
    fn node_affinity_with_matchfields_is_not_modeled() {
        let source = "apiVersion: v1\nkind: Pod\nmetadata: {name: web, namespace: app}\nspec:\n  affinity:\n    nodeAffinity:\n      requiredDuringSchedulingIgnoredDuringExecution:\n        nodeSelectorTerms:\n          - matchExpressions: [{key: disk, operator: Exists}]\n            matchFields: [{key: metadata.name, operator: In, values: [a]}]\n  containers: []\n";
        let n = node("worker-a", "{disk: ssd}");
        let (_host, cache) = setup(&[("pod.yaml", source), ("node.yaml", &n)]);
        let doc = document("pod.yaml", source);
        assert!(cache.definition(&doc, caret(source, "disk, operator")).is_empty());
    }
    fn node(name: &str, labels: &str) -> String {
        format!("apiVersion: v1\nkind: Node\nmetadata: {{name: {name}, labels: {labels}}}\n")
    }
    #[test]
    fn pod_node_selector_targets_only_node_labels_and_checks_the_full_map() {
        let source = "apiVersion: v1\nkind: Pod\nmetadata: {name: web, namespace: app, labels: {disk: ssd}}\nspec:\n  nodeSelector: {disk: ssd, arch: amd64}\n  containers: []\n";
        let n = node("worker-a", "{disk: ssd, arch: amd64}");
        let other = node("worker-b", "{disk: ssd, arch: arm64}");
        let (_host, cache) = setup(&[
            ("pod.yaml", source),
            ("node.yaml", &n),
            ("other.yaml", &other),
        ]);
        let doc = document("pod.yaml", source);
        let at = text::position(source, source.rfind("disk: ssd").unwrap());
        let found = cache.definition(&doc, at);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, "/p/node.yaml");
        assert_eq!(found[0].range.start, caret(&n, "disk: ssd"));
        let uses = cache.occurrences(&document("node.yaml", &n), caret(&n, "disk: ssd"));
        assert_eq!(uses.len(), 1);
        assert_eq!(uses[0].path, "/p/pod.yaml");
    }
    #[test]
    fn workload_node_selector_uses_template_spec_and_ignores_namespace_for_cluster_nodes() {
        let source = "apiVersion: apps/v1\nkind: Deployment\nmetadata: {name: web, namespace: other}\nspec:\n  template:\n    metadata: {labels: {app: web}}\n    spec:\n      nodeSelector: {disk: ssd}\n      containers: []\n";
        let n = node("worker", "{disk: ssd}");
        let (_host, cache) = setup(&[("deployment.yaml", source), ("node.yaml", &n)]);
        let doc = document("deployment.yaml", source);
        assert_eq!(
            cache.definition(&doc, caret(source, "disk: ssd"))[0].path,
            "/p/node.yaml"
        );
    }
    #[test]
    fn namespace_peer_ipblocks_and_unknown_namespace_source_identity_are_not_approximated() {
        let source = peer_policy(
            "    - ipBlock: {cidr: 10.0.0.0/8}\n      namespaceSelector: {matchLabels: {team: ops}}\n      podSelector: {matchLabels: {app: web}}\n",
        );
        let ns = namespace("remote", "{team: ops}");
        let remote = POD.replace("namespace: app", "namespace: remote");
        let (_host, cache) = setup(&[
            ("policy.yaml", &source),
            ("namespace.yaml", &ns),
            ("pod.yaml", &remote),
        ]);
        let doc = document("policy.yaml", &source);
        assert!(
            cache
                .definition(&doc, caret(&source, "app: web"))
                .is_empty()
        );
        assert!(
            cache
                .definition(&doc, caret(&source, "team: ops"))
                .is_empty()
        );
    }

    #[test]
    fn malformed_namespace_selectors_and_duplicate_node_authorities_are_withheld() {
        let ns = namespace("remote", "{team: ops}");
        let remote = POD.replace("namespace: app", "namespace: remote");
        for selector in [
            "string",
            "{matchlabels: {team: ops}}",
            "{matchExpressions: null}",
            "{matchExpressions: [{key: team, operator: Exists, unexpected: true}]}",
        ] {
            let source = peer_policy(&format!(
                "    - namespaceSelector: {selector}\n      podSelector: {{matchLabels: {{app: web}}}}\n"
            ));
            let (_host, cache) = setup(&[
                ("policy.yaml", &source),
                ("namespace.yaml", &ns),
                ("pod.yaml", &remote),
            ]);
            let doc = document("policy.yaml", &source);
            assert!(
                cache
                    .definition(&doc, caret(&source, "app: web"))
                    .is_empty(),
                "{selector}"
            );
        }
        let source = "apiVersion: v1\nkind: Pod\nmetadata: {name: web, namespace: app}\nspec: {nodeSelector: {disk: ssd}, containers: []}\n";
        let n = node("worker", "{disk: ssd}");
        let (_host, cache) = setup(&[
            ("pod.yaml", source),
            ("node.yaml", &n),
            ("duplicate.yaml", &n),
        ]);
        let doc = document("pod.yaml", source);
        assert!(
            cache
                .definition(&doc, caret(source, "disk: ssd"))
                .is_empty()
        );
    }

    fn rename_setup() -> (
        MockHost,
        Cache,
        Document,
        openidea_plugin::file_actions::FileAction,
    ) {
        let (host, cache) = setup(&[("pod.yaml", POD), ("cm.yaml", CM), ("secret.yaml", SECRET)]);
        let host = host.with_capabilities(&["project.read", "editor.read"]);
        let doc = document("cm.yaml", CM);
        let at = caret(CM, "server: http");
        let action = cache.file_actions(&doc, Range::new(at, at)).remove(0);
        (host, cache, doc, action)
    }
    fn applied(source: &str, edits: &[openidea_plugin::TextEdit]) -> String {
        let mut result = source.to_owned();
        let mut edits = edits.to_vec();
        edits.sort_by_key(|edit| {
            std::cmp::Reverse((edit.range.start.line, edit.range.start.character))
        });
        for edit in edits {
            let start = text::offset(source, edit.range.start).unwrap();
            let end = text::offset(source, edit.range.end).unwrap();
            result.replace_range(start..end, &edit.text);
        }
        result
    }
    #[test]
    fn reviewed_data_key_rename_returns_atomic_exact_kind_scoped_edits_without_writing() {
        let (host, cache, doc, action) = rename_setup();
        host.answer_next_prompt(Some(&["endpoint"]));
        host.answer_next_prompt(Some(&["yes"]));
        let ready = cache.resolve_file_action(&doc, action).unwrap();
        assert!(ready.operations.is_empty());
        assert!(ready.data.is_none());
        assert_eq!(ready.edits.len(), 2);
        let cm = ready
            .edits
            .iter()
            .find(|(path, _)| path == "/p/cm.yaml")
            .unwrap();
        assert_eq!(cm.1.len(), 1);
        let after = applied(CM, &cm.1);
        assert!(after.contains("endpoint: http://localhost"));
        let pod = ready
            .edits
            .iter()
            .find(|(path, _)| path == "/p/pod.yaml")
            .unwrap();
        assert_eq!(pod.1.len(), 1);
        let after = applied(POD, &pod.1);
        assert!(after.contains("configMapKeyRef: {name: settings, key: endpoint}"));
        assert!(after.contains("secretKeyRef: {name: settings, key: server}"));
        assert_eq!(
            host::project_of("/p/cm.yaml")
                .unwrap()
                .read_file("/p/cm.yaml")
                .unwrap(),
            CM
        );
    }
    #[test]
    fn renamed_numeric_and_boolean_yaml_keys_are_quoted_without_breaking_json_strings() {
        for new_key in ["123", "true"] {
            let (host, cache, doc, action) = rename_setup();
            host.answer_next_prompt(Some(&[new_key]));
            host.answer_next_prompt(Some(&["yes"]));
            let ready = cache.resolve_file_action(&doc, action).unwrap();
            for (path, changes) in &ready.edits {
                let before = if path.ends_with("cm.yaml") { CM } else { POD };
                let after = applied(before, changes);
                assert!(after.contains(&format!("\"{new_key}\"")));
                assert!(parse(&after).is_some());
            }
        }
        let json = r#"{"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":"settings","namespace":"app"},"data":{"server":"value"}}"#;
        let (host, cache) = setup(&[("cm.json", json)]);
        let host = host.with_capabilities(&["project.read", "editor.read"]);
        let mut doc = document("cm.json", json);
        doc.language = "json".into();
        let at = text::position(json, json.find("server").unwrap());
        let action = cache.file_actions(&doc, Range::new(at, at)).remove(0);
        host.answer_next_prompt(Some(&["true"]));
        host.answer_next_prompt(Some(&["yes"]));
        let ready = cache.resolve_file_action(&doc, action).unwrap();
        let after = applied(json, &ready.edits[0].1);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&after).unwrap()["data"]["true"],
            "value"
        );
    }
    #[test]
    fn rename_cancellation_collisions_and_duplicate_resources_do_not_produce_partial_edits() {
        let (host, cache, doc, action) = rename_setup();
        host.answer_next_prompt(None);
        assert!(matches!(
            cache.resolve_file_action(&doc, action),
            Err(openidea_plugin::Error::Cancelled)
        ));
        let (host, cache, doc, action) = rename_setup();
        host.answer_next_prompt(Some(&["endpoint"]));
        host.answer_next_prompt(None);
        assert!(matches!(
            cache.resolve_file_action(&doc, action),
            Err(openidea_plugin::Error::Cancelled)
        ));
        let (host, cache, doc, action) = rename_setup();
        host.answer_next_prompt(Some(&["port"]));
        assert!(cache.resolve_file_action(&doc, action).is_err());
        let (host, cache, doc, action) = rename_setup();
        host.add_file("duplicate.yaml", &CM.replace("server:", "other:"));
        assert!(cache.resolve_file_action(&doc, action).is_err());
    }
    #[test]
    fn rename_refuses_generated_unparseable_escaped_ambiguous_and_over_limit_inventory() {
        for (path, source) in [
            ("Chart.yaml", "apiVersion: v2\nname: chart\n"),
            ("kustomization.yaml", "resources: []\n"),
            ("bad.yaml", "x: ["),
            ("escaped.yaml", "x: \"esca\\u0070ed\"\n"),
            (
                "unknown-ns.yaml",
                "apiVersion: v1\nkind: Pod\nmetadata: {name: outside}\nspec: {}\n",
            ),
        ] {
            let (host, cache, doc, action) = rename_setup();
            host.add_file(path, source);
            assert!(cache.resolve_file_action(&doc, action).is_err(), "{path}");
        }
        let (host, cache, doc, action) = rename_setup();
        for i in 0..129 {
            host.add_file(&format!("extra-{i}.yaml"), "{}\n");
        }
        assert!(cache.resolve_file_action(&doc, action).is_err());
    }
    #[test]
    fn post_review_source_and_membership_revalidation_rejects_stale_authority() {
        let (host, _cache, doc, _action) = rename_setup();
        let before = rename_snapshot(&doc).unwrap();
        host.add_file("pod.yaml", &POD.replace("name: CONFIG", "name: OTHER"));
        assert!(rename_unchanged(&doc, &before).is_err());
        let (host, _cache, doc, _action) = rename_setup();
        let before = rename_snapshot(&doc).unwrap();
        host.add_file("new.yaml", "{}\n");
        assert!(rename_unchanged(&doc, &before).is_err());
        let (host, cache, doc, action) = rename_setup();
        host.add_file("cm.yaml", &CM.replace("server:", "other:"));
        assert!(matches!(
            cache.resolve_file_action(&doc, action),
            Err(openidea_plugin::Error::Stale)
        ));
    }

    fn label_rename(
        files: &[(&str, &str)],
        path: &str,
        source: &str,
        needle: &str,
        new_key: &str,
    ) -> openidea_plugin::file_actions::FileAction {
        let (host, cache) = setup(files);
        let _host = host.with_capabilities(&["project.read", "editor.read"]);
        let doc = document(path, source);
        let at = caret(source, needle);
        let action = cache.file_actions(&doc, Range::new(at, at)).remove(0);
        _host.answer_next_prompt(Some(&[new_key]));
        _host.answer_next_prompt(Some(&["yes"]));
        cache.resolve_file_action(&doc, action).unwrap()
    }
    #[test]
    fn reviewed_label_rename_closes_selector_scope_even_for_nonmatching_values() {
        let other = POD
            .replace("name: web\n", "name: other\n")
            .replace("app: web", "app: api")
            .replace("tier: frontend", "tier: backend");
        let ready = label_rename(
            &[
                ("pod.yaml", POD),
                ("other.yaml", &other),
                ("service.yaml", SERVICE),
            ],
            "pod.yaml",
            POD,
            "app: web",
            "application",
        );
        assert_eq!(ready.edits.len(), 3);
        assert!(
            ready
                .edits
                .iter()
                .all(|(_, edits)| edits.iter().all(|edit| edit.text == "application"))
        );
        let source = applied(
            &other,
            &ready
                .edits
                .iter()
                .find(|(p, _)| p == "/p/other.yaml")
                .unwrap()
                .1,
        );
        assert!(source.contains("application: api"));
    }
    #[test]
    fn negative_selector_rename_preserves_exclusion_and_absence_associations() {
        let other = POD
            .replace("name: web\n", "name: other\n")
            .replace("app: web", "app: api");
        for operator in ["NotIn, values: [web]", "DoesNotExist"] {
            let policy = policy(&format!(
                "    matchExpressions: [{{key: app, operator: {operator}}}]\n"
            ));
            let ready = label_rename(
                &[
                    ("pod.yaml", POD),
                    ("other.yaml", &other),
                    ("policy.yaml", &policy),
                ],
                "pod.yaml",
                POD,
                "app: web",
                "application",
            );
            assert_eq!(ready.edits.len(), 3);
            let changes = &ready
                .edits
                .iter()
                .find(|(p, _)| p == "/p/policy.yaml")
                .unwrap()
                .1;
            assert!(applied(&policy, changes).contains("key: application"));
        }
    }
    #[test]
    fn own_template_label_rename_does_not_touch_unrelated_same_namespace_pod_labels() {
        let deployment = "apiVersion: apps/v1\nkind: Deployment\nmetadata: {name: deploy, namespace: app}\nspec:\n  selector: {matchLabels: {app: web}}\n  template:\n    metadata: {labels: {app: web}}\n    spec: {containers: []}\n";
        let ready = label_rename(
            &[("deployment.yaml", deployment), ("pod.yaml", POD)],
            "deployment.yaml",
            deployment,
            "app: web}}\n    spec",
            "application",
        );
        assert_eq!(ready.edits.len(), 1);
        assert_eq!(ready.edits[0].0, "/p/deployment.yaml");
        assert_eq!(ready.edits[0].1.len(), 2);
    }
    #[test]
    fn namespace_and_node_label_rename_preserves_cross_scope_source_associations() {
        let ns = namespace("remote", "{team: ops}");
        let other_ns = namespace("other", "{team: dev}");
        let remote = POD.replace("namespace: app", "namespace: remote");
        let source = peer_policy(
            "    - namespaceSelector: {matchLabels: {team: ops}}\n      podSelector: {matchLabels: {app: web}}\n",
        );
        let ready = label_rename(
            &[
                ("namespace.yaml", &ns),
                ("other-ns.yaml", &other_ns),
                ("pod.yaml", &remote),
                ("policy.yaml", &source),
            ],
            "namespace.yaml",
            &ns,
            "team: ops",
            "group",
        );
        assert_eq!(ready.edits.len(), 3);
        let n = node("worker", "{disk: ssd}");
        let other = node("other", "{disk: hdd}");
        let pod = "apiVersion: v1\nkind: Pod\nmetadata: {name: web, namespace: app}\nspec: {nodeSelector: {disk: ssd}, containers: []}\n";
        let ready = label_rename(
            &[("node.yaml", &n), ("other.yaml", &other), ("pod.yaml", pod)],
            "node.yaml",
            &n,
            "disk: ssd",
            "storage",
        );
        assert_eq!(ready.edits.len(), 3);
    }
    #[test]
    fn label_rename_updates_exact_downward_api_keys_and_withholds_collision_or_ambiguous_graphs() {
        let pod=POD.replace("    - name: CONFIG","    - name: APP\n      valueFrom:\n        fieldRef: {fieldPath: \"metadata.labels['app']\"}\n    - name: CONFIG");
        let ready = label_rename(
            &[("pod.yaml", &pod), ("service.yaml", SERVICE)],
            "pod.yaml",
            &pod,
            "app: web",
            "application",
        );
        let after = applied(
            &pod,
            &ready
                .edits
                .iter()
                .find(|(p, _)| p == "/p/pod.yaml")
                .unwrap()
                .1,
        );
        assert!(after.contains("metadata.labels['application']"));
        let (host, cache) = setup(&[("pod.yaml", POD), ("service.yaml", SERVICE)]);
        let host = host.with_capabilities(&["project.read", "editor.read"]);
        let doc = document("pod.yaml", POD);
        let at = caret(POD, "app: web");
        let action = cache.file_actions(&doc, Range::new(at, at)).remove(0);
        host.answer_next_prompt(Some(&["tier"]));
        assert!(cache.resolve_file_action(&doc, action).is_err());
        let (host, cache) = setup(&[
            ("pod.yaml", POD),
            ("copy.yaml", POD),
            ("service.yaml", SERVICE),
        ]);
        let _host = host.with_capabilities(&["project.read", "editor.read"]);
        let action = cache.file_actions(&doc, Range::new(at, at)).remove(0);
        assert!(cache.resolve_file_action(&doc, action).is_err());
    }

    #[test]
    fn flow_label_selectors_require_the_full_selector_and_exact_namespace() {
        let (host, cache) = setup(&[("pod.yaml", POD), ("service.yaml", SERVICE)]);
        let service = document("service.yaml", SERVICE);
        let at = caret(SERVICE, "app: web");
        let found = cache.definition(&service, at);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, "/p/pod.yaml");
        assert_eq!(found[0].range.start, caret(POD, "app: web"));
        host.add_file("pod.yaml", &POD.replace("tier: frontend", "tier: backend"));
        assert!(
            cache.definition(&service, at).is_empty(),
            "changed target must expire cached facts"
        );
        let (_host, cache) = setup(&[("pod.yaml", POD), ("service.yaml", SERVICE)]);
        let wrong = document(
            "service.yaml",
            &SERVICE.replace("namespace: app", "namespace: other"),
        );
        assert!(
            cache
                .definition(&wrong, caret(&wrong.text, "app: web"))
                .is_empty()
        );
    }
    #[test]
    fn label_values_navigate_to_values_and_readonly_occurrences_return_selectors() {
        let (_host, cache) = setup(&[("pod.yaml", POD), ("service.yaml", SERVICE)]);
        let source = document("service.yaml", SERVICE);
        let at = text::position(SERVICE, SERVICE.find("app: web").unwrap() + 5);
        let found = cache.definition(&source, at);
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].range.start,
            text::position(POD, POD.find("app: web").unwrap() + 5)
        );
        let pod = document("pod.yaml", POD);
        let uses = cache.occurrences(&pod, caret(POD, "app: web"));
        assert_eq!(uses.len(), 1);
        assert_eq!(uses[0].path, "/p/service.yaml");
    }
    #[test]
    fn key_references_are_scoped_by_namespace_resource_kind_and_name() {
        let (_host, cache) = setup(&[("pod.yaml", POD), ("cm.yaml", CM), ("secret.yaml", SECRET)]);
        let doc = document("pod.yaml", POD);
        for (needle, target) in [
            (
                "configMapKeyRef: {name: settings, key: server}",
                "/p/cm.yaml",
            ),
            (
                "secretKeyRef: {name: settings, key: server}",
                "/p/secret.yaml",
            ),
        ] {
            let at = text::position(
                POD,
                POD.find(needle).unwrap() + needle.find("server").unwrap(),
            );
            let found = cache.definition(&doc, at);
            assert_eq!(found.len(), 1);
            assert_eq!(found[0].path, target);
        }
        let cm = document("cm.yaml", CM);
        assert_eq!(
            cache.occurrences(&cm, caret(CM, "server: http"))[0].path,
            "/p/pod.yaml"
        );
    }
    #[test]
    fn duplicate_key_declarations_and_foreign_nested_projects_are_not_authority() {
        let (host, mut cache) = setup(&[("pod.yaml", POD), ("cm.yaml", CM)]);
        let doc = document("pod.yaml", POD);
        let at = caret(POD, "server}");
        assert_eq!(cache.definition(&doc, at).len(), 1);
        host.add_file("duplicate.yaml", CM);
        cache.warm("/p/duplicate.yaml", CM);
        assert!(cache.definition(&doc, at).is_empty());
        cache.clear();
        cache.warm("/p/cm.yaml", CM);
        host.add_project("/p/cm.yaml");
        assert!(cache.definition(&doc, at).is_empty());
    }
    #[test]
    fn a_selector_matching_several_sources_gets_one_same_icon_marker_per_target() {
        let (_host, cache) = setup(&[("pod.yaml", POD), ("service.yaml", SERVICE)]);
        let doc = document("service.yaml", SERVICE);
        let single = cache.markers(&doc);
        assert_eq!(single.len(), 2);
        let pod2 = POD.replace("name: web\n  namespace", "name: web2\n  namespace");
        let (_host, cache) = setup(&[("pod.yaml", POD), ("pod2.yaml", &pod2), ("service.yaml", SERVICE)]);
        let markers = cache.markers(&doc);
        assert_eq!(markers.len(), 4);
        assert!(markers.iter().all(|m| m.line == 4 && m.icon.as_deref() == Some(crate::LABEL_MARKER)));
        let targets: BTreeSet<_> = markers.iter().map(|m| {
            let target = m.target.as_ref().unwrap();
            assert!(target.range.start != target.range.end);
            (target.path.clone(), target.range.start.line, target.range.start.character)
        }).collect();
        assert_eq!(targets.len(), 4);
        assert!(targets.iter().any(|(path, ..)| path == "/p/pod2.yaml"));
    }
    #[test]
    fn absent_namespaces_aliases_tags_and_generated_expressions_withhold_models() {
        let (_host, cache) = setup(&[("pod.yaml", POD), ("service.yaml", SERVICE)]);
        for source in [
            SERVICE.replace("namespace: app", "x-namespace: app"),
            SERVICE.replace("selector:", "selector: &selector"),
            SERVICE.replace("app: web", "app: !!str web"),
            SERVICE.replace("app: web", "app: '{{ value }}'"),
        ] {
            let doc = document("service.yaml", &source);
            assert!(
                cache
                    .definition(&doc, caret(&source, "selector"))
                    .is_empty()
            );
            assert!(cache.markers(&doc).is_empty());
        }
    }
    #[test]
    fn escaped_spelling_is_not_a_source_anchor_and_utf16_ranges_are_exact() {
        let source = SERVICE.replace("app: web", "'app': 'web'");
        let (_host, cache) = setup(&[("pod.yaml", POD), ("service.yaml", &source)]);
        let doc = document("service.yaml", &source);
        let at = text::position(&source, source.find("'app'").unwrap() + 1);
        assert_eq!(cache.definition(&doc, at).len(), 1);
        let source = SERVICE.replace("app: web", "app: \"w\\u0065b\"");
        let doc = document("service.yaml", &source);
        assert!(cache.markers(&doc).is_empty());
        let source = CM.replace("data:\n", "# 🐳 Unicode before exact spans\ndata:\n");
        let facts = facts("/p/cm.yaml", &source).unwrap();
        assert_eq!(
            facts.keys[0].key.range.start,
            caret(&source, "server: http")
        );
    }
    #[test]
    fn quoted_scalar_scanner_proves_round_trip_and_refuses_the_rest() {
        let scan = |src: &str, value: &str| scan_quoted(src, 0, value);
        assert_eq!(scan("'it''s' x", "it's"), Some(6));
        assert_eq!(scan("\"a\\\"b\\/c\\n\"", "a\"b/c\n"), Some(10));
        assert_eq!(scan("'a''b'", "a'b"), Some(5));
        assert_eq!(scan("\"a\\u0041\"", "aA"), None);
        assert_eq!(scan("\"a\\x\"", "ax"), None);
        assert_eq!(scan("'a\nb'", "a b"), None);
        assert_eq!(scan("\"ab\"", "abc"), None);
        assert_eq!(scan("'ab", "ab"), None);
    }
    #[test]
    fn escaped_quoted_key_gets_exact_inner_range_and_multiline_stays_refused() {
        let source = POD.replace("app: web", "\"example.com\\/app\": web");
        let facts = facts("/p/pod.yaml", &source).unwrap();
        let key = &facts
            .labels
            .iter()
            .find(|l| l.key.value == "example.com/app")
            .unwrap()
            .key;
        let (start, end) = (
            text::offset(&source, key.range.start).unwrap(),
            text::offset(&source, key.range.end).unwrap(),
        );
        assert_eq!(&source[start..end], "example.com\\/app");
        assert!(spells(&source, start, end, "example.com/app"));
        assert!(!spells(&source, start, end, "example.com/other"));
        assert_eq!(
            replacement_for(&source, key.range, "example.com/new").unwrap(),
            "example.com/new"
        );
        let folded = CM.replace("port: '80'", "port: \"8\n  0\"");
        assert!(parse(&folded).is_none_or(|nodes| nodes.iter().any(|n| !exact_tree(n))));
    }
    #[test]
    fn key_completion_uses_current_owned_resource_definitions() {
        let (_host, cache) = setup(&[("pod.yaml", POD), ("cm.yaml", CM)]);
        let doc = document("pod.yaml", POD);
        let items = cache.completion(&doc, caret(POD, "server}"));
        assert_eq!(
            items.iter().map(|i| i.label.as_str()).collect::<Vec<_>>(),
            ["port", "server"]
        );
        assert_eq!(
            items[0].replace,
            Some(Range::new(
                caret(POD, "server}"),
                text::position(POD, POD.find("server}").unwrap() + 6)
            ))
        );
    }
    #[test]
    fn slashless_api_version_keeps_other_docs() {
        let source = format!("apiVersion: weird\nkind: Thing\nmetadata: {{name: a, namespace: app}}\n---\n{POD}");
        let found = facts("/p/a.yaml", &source).unwrap();
        assert_eq!(found.labels.len(), 2);
    }
    #[test]
    fn secret_plaintext_is_not_retained_in_cached_facts() {
        let (_host, cache) = setup(&[("secret.yaml", SECRET)]);
        let cached = cache.files.values().next().unwrap();
        assert_eq!(cached.facts.keys.len(), 1);
        assert_eq!(cached.facts.keys[0].key.value, "server");
        assert_eq!(cached.stamp.map(|(size, _)| size), Some(SECRET.len() as u64));
    }
    #[test]
    fn position_table_matches_naive() {
        for source in ["", "a: 1\n", "a: é\n😀: x\r\nb: ü😀z\n\n\nlast", "\n\n", "key: \"é\"\n  - café: 😀"] {
            let lines = LineStarts::new(source);
            for at in (0..=source.len()).filter(|at| source.is_char_boundary(*at)) {
                assert_eq!(lines.position(source, at), text::position(source, at), "{source:?} at {at}");
            }
            assert_eq!(lines.position(source, source.len() + 9), text::position(source, source.len()));
        }
    }
    #[test]
    fn label_cache_no_rehash() {
        let (host, cache) = setup(&[("pod.yaml", POD), ("service.yaml", SERVICE)]);
        let service = Document {
            path: "/p/service.yaml".into(),
            language: "yaml".into(),
            version: 1,
            text: SERVICE.into(),
        };
        let at = caret(SERVICE, "tier");
        let before = cache.definition(&service, at);
        assert!(!before.is_empty());

        host.add_file("pod.yaml", &POD.replace("frontend", "frontenx"));
        assert_eq!(cache.definition(&service, at), before);

        host.add_file("pod.yaml", &format!("{POD}# longer\n"));
        assert!(cache.definition(&service, at).is_empty());
    }
    #[test]
    fn deployment_selector_links_only_its_own_template_labels() {
        let deployment = "apiVersion: apps/v1\nkind: Deployment\nmetadata: {name: web, namespace: app}\nspec:\n  selector: {matchLabels: {app: web}}\n  template:\n    metadata: {labels: {app: web}}\n    spec: {containers: []}\n";
        let (_host, cache) = setup(&[
            ("deployment.yaml", deployment),
            ("copy.yaml", deployment),
            ("pod.yaml", POD),
        ]);
        let doc = document("deployment.yaml", deployment);
        let found = cache.definition(&doc, caret(deployment, "app: web"));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, "/p/deployment.yaml");
        assert_eq!(
            found[0].range.start,
            text::position(deployment, deployment.rfind("app: web").unwrap())
        );
    }
}
