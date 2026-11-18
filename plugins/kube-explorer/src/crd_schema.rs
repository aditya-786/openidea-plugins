//! Local CRD structural-schema completion. Only indexed/opened project files
//! participate; providers re-read definitions and never query a cluster.
use openidea_plugin::providers::{CompletionItem, CompletionKind, Document};
use openidea_plugin::{Position, Range, host, text};
use yaml_rust2::Yaml;
const MAX_TEXT: usize = 128 << 10;
#[derive(Default)]
pub(crate) struct Cache {
    files: crate::lru::Lru<(String, String), (String, Vec<Definition>)>,
}
#[derive(Clone)]
struct Definition {
    api: String,
    kind: String,
    cluster_scoped: bool,
    schema: Yaml,
    path: String,
}
fn definitions(source: &str, path: &str) -> Vec<Definition> {
    let Some(docs) = crate::convert::source_documents(source) else {
        return vec![];
    };
    docs.into_iter()
        .filter(|d| {
            d["kind"].as_str() == Some("CustomResourceDefinition")
                && d["apiVersion"].as_str() == Some("apiextensions.k8s.io/v1")
        })
        .flat_map(|d| {
            let Some(group) = d["spec"]["group"].as_str() else {
                return vec![];
            };
            let Some(kind) = d["spec"]["names"]["kind"].as_str() else {
                return vec![];
            };
            d["spec"]["versions"]
                .as_vec()
                .into_iter()
                .flatten()
                .take(32)
                .filter_map(|v| {
                    if v["served"].as_bool() == Some(false) {
                        return None;
                    }
                    let schema = &v["schema"]["openAPIV3Schema"];
                    schema.as_hash()?;
                    Some(Definition {
                        api: format!("{group}/{}", v["name"].as_str()?),
                        kind: kind.into(),
                        cluster_scoped: d["spec"]["scope"].as_str() == Some("Cluster"),
                        schema: schema.clone(),
                        path: path.into(),
                    })
                })
                .collect::<Vec<_>>()
        })
        .collect()
}
struct Cursor {
    path: Vec<String>,
    key: Option<String>,
    start: usize,
    end: usize,
}
fn cursor(source: &str, at: Position) -> Option<Cursor> {
    let caret = text::offset(source, at)?;
    let start = source[..caret].rfind('\n').map_or(0, |i| i + 1);
    let end = source[caret..]
        .find(['\r', '\n'])
        .map_or(source.len(), |i| caret + i);
    let line = &source[start..end];
    if line.contains(['\t', '{', '}', '[', ']']) {
        return None;
    }
    let indent = line.len() - line.trim_start_matches(' ').len();
    let mut stack: Vec<(usize, String)> = Vec::new();
    for prev in source[..start].lines() {
        let content = prev.trim();
        if content.is_empty() || content.starts_with('#') || content == "---" {
            continue;
        }
        let depth = prev.len() - prev.trim_start_matches(' ').len();
        while stack.last().is_some_and(|(i, _)| *i >= depth) {
            stack.pop();
        }
        if let Some((key, value)) = content.split_once(':') {
            if value.trim().is_empty() {
                stack.push((depth, key.to_owned()));
            } else if value.trim_start().starts_with(['|', '>']) && indent > depth {
                return None;
            }
        }
    }
    while stack.last().is_some_and(|(i, _)| *i >= indent) {
        stack.pop();
    }
    let content = &line[indent..];
    if content.starts_with(['#', '-', '\'', '"']) {
        return None;
    }
    if let Some((key, _)) = content.split_once(':') {
        let value_start = start + indent + key.len() + 1;
        if caret > value_start {
            let tail = &source[value_start..end];
            let lead = tail.len() - tail.trim_start().len();
            let stop = tail.find(" #").unwrap_or(tail.len());
            if caret > value_start + stop || stop < lead {
                return None;
            }
            return Some(Cursor {
                path: stack.into_iter().map(|(_, key)| key).collect(),
                key: Some(key.trim().into()),
                start: value_start + lead,
                end: value_start + stop,
            });
        }
        return None;
    }
    if !content
        .bytes()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.'))
    {
        return None;
    }
    Some(Cursor {
        path: stack.into_iter().map(|(_, key)| key).collect(),
        key: None,
        start: start + indent,
        end,
    })
}
impl Cache {
    pub(crate) fn clear(&mut self) {
        self.files.clear();
    }
    pub(crate) fn evict(&mut self, path: &str) {
        self.files.retain(|(_, p), _| p != path);
    }
    pub(crate) fn warm(&mut self, path: &str, source: &str) {
        self.files.retain(|(_, p), _| p != path);
        if source.len() > MAX_TEXT || definitions(source, path).is_empty() {
            return;
        }
        let Some(project) = host::project_of(path) else {
            return;
        };
        let Ok(root) = project.root() else {
            return;
        };
        self.files.insert(
            (root, path.into()),
            (source.into(), definitions(source, path)),
            64,
        );
    }
    pub(crate) fn warm_file(&mut self, path: &str) {
        if let Some(project) = host::project_of(path)
            && let Ok(source) = project.read_file(path)
        {
            self.warm(path, &source);
        }
    }
    fn definition(&self, doc: &Document) -> Option<Definition> {
        if self.files.is_empty()
            || !matches!(doc.language.as_str(), "yaml" | "json" | "jsonc")
            || doc.text.len() > MAX_TEXT
        {
            return None;
        }
        let Some(project) = host::project_of(&doc.path) else {
            return None;
        };
        let Ok(root) = project.root() else {
            return None;
        };
        if project.read_file(&doc.path).is_err() {
            return None;
        }
        let (api, kind) = if matches!(doc.language.as_str(), "json" | "jsonc") {
            let value=crate::json_assistance::parsed(&doc.text)?;
            (
                value["apiVersion"].as_str()?.to_owned(),
                value["kind"].as_str()?.to_owned(),
            )
        } else {
            let facts = crate::manifest::documents(&doc.text);
            if facts.len() != 1 {
                return None;
            }
            (
                facts[0].api_version.as_ref()?.text.clone(),
                facts[0].kind.as_ref()?.text.clone(),
            )
        };
        let mut matched = Vec::new();
        for ((owner, path), (expected, definitions)) in &self.files {
            if !definitions.iter().any(|d| d.api == api && d.kind == kind) {
                continue;
            }
            if owner != &root
                || host::project_of(path)
                    .and_then(|p| p.root().ok())
                    .as_deref()
                    != Some(&root)
            {
                continue;
            }
            let Ok(source) = project.read_file(path) else {
                continue;
            };
            if source.len() > MAX_TEXT || &source != expected {
                continue;
            }
            matched.extend(
                definitions
                    .iter()
                    .filter(|d| d.api == api && d.kind == kind)
                    .cloned(),
            );
        }
        if matched.len() != 1 {
            return None;
        }
        matched.pop()
    }
    pub(crate) fn schema(&self, doc: &Document) -> Option<Yaml> {
        Some(self.definition(doc)?.schema)
    }
    pub(crate) fn diagnostics(&self, doc: &Document) -> Vec<openidea_plugin::Diagnostic> {
        if host::setting_bool("checkKindSchema") == Some(false) {
            return vec![];
        }
        let Some(def) = self.definition(doc) else {
            return vec![];
        };
        let normalized=if matches!(doc.language.as_str(),"json"|"jsonc"){crate::json_assistance::normalized(&doc.text).unwrap_or_default()}else{doc.text.clone()};
        let Some(docs) = crate::convert::source_documents(&normalized) else {
            return vec![];
        };
        if docs.len() != 1 {
            return vec![];
        }
        let mut messages = Vec::new();
        validate(&docs[0], &def.schema, "", 0, &mut messages);
        if def.cluster_scoped && !matches!(docs[0]["metadata"]["namespace"], Yaml::BadValue) {
            messages.push("metadata.namespace is set but the local CRD is cluster-scoped".into());
        }
        let facts = crate::manifest::documents(&doc.text);
        let span = facts
            .first()
            .and_then(|d| d.kind.as_ref())
            .map(|v| (v.start, v.end))
            .unwrap_or((0, 0));
        messages
            .into_iter()
            .map(|message| {
                let info = message.contains(NULL_PRUNED);
                let mut finding = crate::checks::Finding::new(span.0, span.1, "crd-schema", message);
                if info {
                    finding.severity = openidea_plugin::Severity::Info;
                }
                finding.diagnostic(&doc.text)
            })
            .collect()
    }
    pub(crate) fn items(&self, doc: &Document, at: Position) -> Vec<CompletionItem> {
        let Some(def) = self.definition(doc) else {
            return vec![];
        };
        let Some(cursor) = cursor(&doc.text, at) else {
            return vec![];
        };
        let mut schema = &def.schema;
        for key in cursor.path {
            schema = &schema["properties"][key.as_str()];
        }
        let replace = Range::new(
            text::position(&doc.text, cursor.start),
            text::position(&doc.text, cursor.end),
        );
        let detail = |schema: &Yaml| {
            let ty = schema["type"].as_str().unwrap_or("schema");
            let desc = schema["description"].as_str().unwrap_or("");
            format!(
                "{ty} — {} — {}",
                desc.chars().take(256).collect::<String>(),
                def.path
            )
        };
        if let Some(key) = cursor.key {
            let property = &schema["properties"][key.as_str()];
            let values = property["enum"].as_vec().cloned().unwrap_or_else(|| {
                if property["type"].as_str() == Some("boolean") {
                    vec![Yaml::Boolean(true), Yaml::Boolean(false)]
                } else {
                    vec![]
                }
            });
            return values
                .into_iter()
                .take(128)
                .filter_map(|value| {
                    let (label, insert) = match value {
                        Yaml::String(s) => (s.clone(), serde_json::to_string(&s).ok()?),
                        Yaml::Integer(i) => (i.to_string(), i.to_string()),
                        Yaml::Boolean(b) => (b.to_string(), b.to_string()),
                        _ => return None,
                    };
                    let mut item = CompletionItem::new(&label, CompletionKind::EnumMember);
                    item.insert_text = insert;
                    item.replace = Some(replace);
                    item.detail = Some(detail(property));
                    Some(item)
                })
                .collect();
        }
        let Some(properties) = schema["properties"].as_hash() else {
            return vec![];
        };
        let parsed = crate::convert::source_documents(&doc.text);
        let mut current = parsed.as_ref().and_then(|d| d.first());
        // At a key insertion point, use the same path again to suppress known keys.
        if let Some(c) = cursor_for_path(&doc.text, at) {
            for key in c {
                current = current.map(|v| &v[key.as_str()]);
            }
        }
        properties
            .iter()
            .take(256)
            .filter_map(|(key, value)| {
                let key = key.as_str()?;
                if current.is_some_and(|v| !matches!(v[key], Yaml::BadValue)) {
                    return None;
                }
                if !key
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.'))
                {
                    return None;
                }
                let mut item = CompletionItem::new(key, CompletionKind::Property);
                item.insert_text = format!("{key}: $0");
                item.snippet = true;
                item.replace = Some(replace);
                item.detail = Some(detail(value));
                Some(item)
            })
            .collect()
    }
}
const NULL_PRUNED: &str = "null on a non-nullable field is pruned";
fn validate(value: &Yaml, schema: &Yaml, path: &str, depth: usize, out: &mut Vec<String>) {
    let int_or_string = schema["x-kubernetes-int-or-string"].as_bool() == Some(true);
    if depth > 16
        || out.len() >= 64
        || ["$ref", "allOf", "anyOf", "oneOf", "not", "if"]
            .iter()
            .any(|key| !int_or_string && !matches!(schema[*key], Yaml::BadValue))
    {
        return;
    }
    if matches!(value, Yaml::Null) {
        return;
    }
    let ty = schema["type"].as_str();
    let correct = if int_or_string {
        matches!(value, Yaml::String(_) | Yaml::Integer(_))
    } else {
        match ty {
            Some("object") => value.as_hash().is_some(),
            Some("array") => value.as_vec().is_some(),
            Some("string") => value.as_str().is_some(),
            Some("integer") => value.as_i64().is_some(),
            Some("number") => matches!(value, Yaml::Integer(_) | Yaml::Real(_)),
            Some("boolean") => value.as_bool().is_some(),
            _ => true,
        }
    };
    let label = if path.is_empty() { "resource" } else { path };
    if !correct {
        out.push(format!(
            "{label}: expected {} (local CRD schema)",
            ty.unwrap_or("value")
        ));
        return;
    }
    if schema["readOnly"].as_bool() == Some(true) && !matches!(value, Yaml::Null) {
        out.push(format!(
            "{label}: read-only field is supplied (local CRD schema)"
        ));
    }
    if schema["deprecated"].as_bool() == Some(true) {
        out.push(format!("{label}: deprecated field (local CRD schema)"));
    }
    let number = |v: &Yaml| {
        v.as_i64()
            .map(|n| n as f64)
            .or_else(|| v.as_f64())
            .filter(|v| v.is_finite())
    };
    if let Some(n) = number(value) {
        for (key, minimum) in [("minimum", true), ("maximum", false)] {
            if let Some(bound) = number(&schema[key]) {
                let exclusive = schema[if minimum {
                    "exclusiveMinimum"
                } else {
                    "exclusiveMaximum"
                }]
                .as_bool()
                    == Some(true);
                if if minimum {
                    n < bound || exclusive && n == bound
                } else {
                    n > bound || exclusive && n == bound
                } {
                    out.push(format!("{label}: value violates {key} (local CRD schema)"));
                }
            }
        }
        for (key, minimum) in [("exclusiveMinimum", true), ("exclusiveMaximum", false)] {
            if let Some(bound) = number(&schema[key])
                && if minimum { n <= bound } else { n >= bound }
            {
                out.push(format!("{label}: value violates {key} (local CRD schema)"));
            }
        }
        if let (Some(n), Some(divisor)) = (value.as_i64(), schema["multipleOf"].as_i64())
            && divisor > 0
            && n % divisor != 0
        {
            out.push(format!(
                "{label}: value violates multipleOf (local CRD schema)"
            ));
        }
    }
    if let Some(value) = value.as_str() {
        let length = value.chars().count() as i64;
        for (key, minimum) in [("minLength", true), ("maxLength", false)] {
            if let Some(bound) = schema[key].as_i64().filter(|v| *v >= 0)
                && if minimum {
                    length < bound
                } else {
                    length > bound
                }
            {
                out.push(format!("{label}: string violates {key} (local CRD schema)"));
            }
        }
        if let Some(pattern) = schema["pattern"].as_str().filter(|s| s.len() <= 1024)
            && value.len() <= 8192
        {
            if let Ok(regex) = regex_lite::Regex::new(pattern)
                && !regex.is_match(value)
            {
                out.push(format!(
                    "{label}: string does not match pattern (local CRD schema)"
                ));
            }
        }
    }
    if let Some(items) = value.as_vec() {
        for (key, minimum) in [("minItems", true), ("maxItems", false)] {
            if let Some(bound) = schema[key].as_i64().filter(|v| *v >= 0)
                && if minimum {
                    (items.len() as i64) < bound
                } else {
                    (items.len() as i64) > bound
                }
            {
                out.push(format!("{label}: array violates {key} (local CRD schema)"));
            }
        }
        if schema["uniqueItems"].as_bool() == Some(true)
            && items.len() <= 256
            && items
                .iter()
                .enumerate()
                .any(|(i, item)| items[..i].contains(item))
        {
            out.push(format!(
                "{label}: array items must be unique (local CRD schema)"
            ));
        }
    }
    if let Some(object) = value.as_hash() {
        for (key, minimum) in [("minProperties", true), ("maxProperties", false)] {
            if let Some(bound) = schema[key].as_i64().filter(|v| *v >= 0)
                && if minimum {
                    (object.len() as i64) < bound
                } else {
                    (object.len() as i64) > bound
                }
            {
                out.push(format!("{label}: object violates {key} (local CRD schema)"));
            }
        }
    }
    if let Some(values) = schema["enum"].as_vec()
        && !values.contains(value)
    {
        out.push(format!("{label}: value is outside the local CRD enum"));
    }
    if let Some(object) = value.as_hash() {
        if let Some(required) = schema["required"].as_vec() {
            for key in required.iter().filter_map(Yaml::as_str).take(64) {
                if matches!(value[key], Yaml::BadValue) {
                    out.push(format!(
                        "{label}: missing required field {key} (local CRD schema)"
                    ));
                }
            }
        }
        for (key, child) in object.iter().take(256) {
            let Some(key) = key.as_str() else {
                continue;
            };
            let property = &schema["properties"][key];
            if !matches!(property, Yaml::BadValue) {
                if matches!(child, Yaml::Null) && property["nullable"].as_bool() != Some(true) {
                    let outcome = if matches!(property["default"], Yaml::BadValue) {
                        "dropped"
                    } else {
                        "replaced by the default"
                    };
                    out.push(format!("{path}/{key}: {NULL_PRUNED}; it is {outcome} before validation (local CRD schema)"));
                    continue;
                }
                validate(child, property, &format!("{path}/{key}"), depth + 1, out);
            } else if schema["x-kubernetes-preserve-unknown-fields"].as_bool() == Some(true) {
            } else if schema["additionalProperties"].as_bool() == Some(false)
                || (schema["properties"].as_hash().is_some()
                    && matches!(schema["additionalProperties"], Yaml::BadValue)
                    && !((depth == 0 || schema["x-kubernetes-embedded-resource"].as_bool() == Some(true))
                        && matches!(key, "apiVersion" | "kind" | "metadata")))
            {
                out.push(format!("{label}: unknown field {key} would be pruned (local CRD schema)"));
            } else if schema["additionalProperties"].as_hash().is_some() {
                validate(
                    child,
                    &schema["additionalProperties"],
                    &format!("{path}/{key}"),
                    depth + 1,
                    out,
                );
            }
            if out.len() >= 64 {
                return;
            }
        }
    }
    if let Some(items) = value.as_vec()
        && schema["items"].as_hash().is_some()
    {
        for (i, child) in items.iter().take(256).enumerate() {
            validate(
                child,
                &schema["items"],
                &format!("{path}/{i}"),
                depth + 1,
                out,
            );
            if out.len() >= 64 {
                return;
            }
        }
    }
}
fn cursor_for_path(source: &str, at: Position) -> Option<Vec<String>> {
    Some(cursor(source, at)?.path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    const CRD: &str = "apiVersion: apiextensions.k8s.io/v1\nkind: CustomResourceDefinition\nspec:\n  group: example.test\n  names: {kind: Widget}\n  versions:\n  - name: v1\n    served: true\n    schema:\n      openAPIV3Schema:\n        type: object\n        properties:\n          spec:\n            type: object\n            properties:\n              color:\n                type: string\n                description: Widget color\n                enum: [red, blue]\n              enabled: {type: boolean}\n              size: {type: integer}\n";
    fn doc(text: &str) -> Document {
        Document {
            path: "/p/widget.yaml".into(),
            language: "yaml".into(),
            version: 1,
            text: format!("apiVersion: example.test/v1\nkind: Widget\n{text}"),
        }
    }
    fn setup(d: &Document) -> (MockHost, Cache) {
        let h = MockHost::install().with_capabilities(&["project.read"]);
        h.set_project_root(Some("/p"));
        h.add_file("widget.yaml", &d.text);
        h.add_file("crd.yaml", CRD);
        let mut cache = Cache::default();
        cache.warm("/p/crd.yaml", CRD);
        (h, cache)
    }
    fn end(d: &Document) -> Position {
        text::position(&d.text, d.text.len())
    }
    #[test]
    fn crd_completion_between_spaces_no_inverted_range() {
        let source = "key:  # c";
        assert!(cursor(source, Position::new(0, 5)).is_none());
        let spaced = cursor("key: va", Position::new(0, 7)).unwrap();
        assert!(spaced.start <= spaced.end);
    }
    #[test]
    fn scalar_collection_and_read_only_constraints_are_enforced_without_claiming_cel() {
        let schema=crate::convert::source_documents("type: object\nrequired: [count]\nadditionalProperties: false\nproperties:\n  count: {type: integer, minimum: 1, maximum: 8, multipleOf: 2}\n  code: {type: string, minLength: 2, maxLength: 4, pattern: '^[a-z]+$'}\n  items: {type: array, minItems: 1, maxItems: 3, uniqueItems: true, items: {type: string}}\n  server: {type: string, readOnly: true}\n  old: {type: string, deprecated: true}\n").unwrap().remove(0);
        let valid = crate::convert::source_documents("count: 2\ncode: ab\nitems: [a, b]\n")
            .unwrap()
            .remove(0);
        let mut out = vec![];
        validate(&valid, &schema, "", 0, &mut out);
        assert!(out.is_empty());
        let invalid = crate::convert::source_documents(
            "count: 9\ncode: A\nitems: [a, a, a, a]\nserver: value\nold: value\n",
        )
        .unwrap()
        .remove(0);
        validate(&invalid, &schema, "", 0, &mut out);
        for expected in [
            "maximum",
            "multipleOf",
            "minLength",
            "pattern",
            "maxItems",
            "unique",
            "read-only",
            "deprecated",
        ] {
            assert!(
                out.iter().any(|m| m.contains(expected)),
                "{expected}: {out:?}"
            );
        }
        let composed =
            crate::convert::source_documents("oneOf: [{type: string}, {type: boolean}]\n")
                .unwrap()
                .remove(0);
        let mut out = vec![];
        validate(&Yaml::Integer(3), &composed, "", 0, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn live_schema_diagnostics_validate_types_required_enums_and_unknown_fields() {
        let d = doc("spec:\n  color: green\n  enabled: yes\n");
        let (h, mut c) = setup(&d);
        let schema=CRD.replace("type: object\n            properties:","type: object\n            required: [size]\n            additionalProperties: false\n            properties:");
        h.add_file("crd.yaml", &schema);
        c.warm("/p/crd.yaml", &schema);
        let found = c.diagnostics(&d);
        assert_eq!(found.len(), 3);
        assert!(
            found
                .iter()
                .all(|d| d.code.as_deref() == Some("crd-schema"))
        );
        assert!(
            c.diagnostics(&doc("spec:\n  color: red\n  enabled: true\n  size: 3\n"))
                .is_empty()
        );
        assert_eq!(
            c.diagnostics(&doc(
                "spec:\n  color: red\n  enabled: true\n  size: 3\n  typo: true\n"
            ))
            .len(),
            1
        );
        let mut kube = crate::KubeExplorer::default();
        kube.crd_schema = c;
        let live = openidea_plugin::diagnostics::DiagnosticsProvider::diagnostics(&mut kube, &d);
        assert!(live.iter().any(|d| d.code.as_deref() == Some("crd-schema")));
    }

    #[test]
    fn local_structural_schema_completes_nested_properties_and_typed_values() {
        let d = doc("spec:\n  ");
        let (_h, c) = setup(&d);
        let found = c.items(&d, end(&d));
        assert_eq!(
            found.iter().map(|i| i.label.as_str()).collect::<Vec<_>>(),
            ["color", "enabled", "size"]
        );
        let d = doc("spec:\n  color: ");
        let found = c.items(&d, end(&d));
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].insert_text, "\"red\"");
        assert!(found[0].detail.as_ref().unwrap().contains("Widget color"));
        let d = doc("spec:\n  enabled: ");
        let found = c.items(&d, end(&d));
        assert_eq!(
            found
                .iter()
                .map(|i| i.insert_text.as_str())
                .collect::<Vec<_>>(),
            ["true", "false"]
        );
    }
    #[test]
    fn stale_or_ambiguous_definitions_and_foreign_projects_do_not_supply_schema() {
        let d = doc("spec:\n  ");
        let (h, mut c) = setup(&d);
        h.add_file("crd.yaml", &CRD.replace("example.test", "other.test"));
        assert!(c.items(&d, end(&d)).is_empty());
        h.add_file("crd.yaml", CRD);
        h.add_file("copy.yaml", CRD);
        c.warm("/p/copy.yaml", CRD);
        assert!(c.items(&d, end(&d)).is_empty());
        c.clear();
        c.warm("/p/crd.yaml", CRD);
        h.add_project("/p/crd.yaml");
        assert!(c.items(&d, end(&d)).is_empty());
    }
    #[test]
    fn existing_keys_and_comments_are_not_replaced_with_properties() {
        let d = doc("spec:\n  color: red\n  ");
        let (_h, c) = setup(&d);
        let found = c.items(&d, end(&d));
        assert!(!found.iter().any(|i| i.label == "color"));
        for source in [
            "spec:\n  # color",
            "spec:\n  color: red # comment",
            "spec: |\n  color",
            "spec:\n  - color",
        ] {
            let d = doc(source);
            assert!(c.items(&d, end(&d)).is_empty(), "{source}");
        }
    }
    #[test]
    fn unserved_versions_aliases_and_duplicate_schema_fields_are_not_admitted() {
        for source in [
            CRD.replace("served: true", "served: false"),
            CRD.replace(
                "type: string",
                "type: string\n                type: boolean",
            ),
            CRD.replace("[red, blue]", "&colors [red, blue]"),
        ] {
            assert!(definitions(&source, "/p/crd.yaml").is_empty());
        }
    }

    #[test]
    fn pruning_int_or_string_nullable_and_scope_follow_the_structural_schema_rules() {
        let schema = crate::convert::source_documents("type: object\nproperties:\n  port:\n    x-kubernetes-int-or-string: true\n    anyOf: [{type: integer}, {type: string}]\n  name: {type: string, nullable: true}\n  free:\n    type: object\n    x-kubernetes-preserve-unknown-fields: true\n  emb:\n    type: object\n    x-kubernetes-embedded-resource: true\n    x-kubernetes-preserve-unknown-fields: true\n  map:\n    type: object\n    additionalProperties: {type: integer}\n").unwrap().remove(0);
        let ok = crate::convert::source_documents("apiVersion: v1\nkind: K\nmetadata: {name: a}\nport: 80\nname: null\nfree: {x: 1}\nemb: {apiVersion: v1, kind: Pod, metadata: {}}\nmap: {a: 1}\n").unwrap().remove(0);
        let mut out = vec![];
        validate(&ok, &schema, "", 0, &mut out);
        assert!(out.is_empty(), "{out:?}");
        let bad = crate::convert::source_documents("port: [1]\nstray: 1\nmap: {a: x}\n").unwrap().remove(0);
        validate(&bad, &schema, "", 0, &mut out);
        assert!(out.iter().any(|m| m.contains("/port")), "{out:?}");
        assert!(out.iter().any(|m| m.contains("stray")), "{out:?}");
        assert!(out.iter().any(|m| m.contains("/map/a")), "{out:?}");
    }

    #[test]
    fn null_on_non_nullable_field_is_pruned_or_defaulted_per_structural_schema_docs() {
        // kubernetes.io CRD docs, "Defaulting and Nullable": non-nullable null is pruned before defaulting,
        // default applied if present; nullable: true keeps null.
        let schema = crate::convert::source_documents("type: object\nproperties:\n  spec:\n    type: object\n    properties:\n      foo: {type: string, nullable: false, default: d}\n      bar: {type: string, nullable: true}\n      baz: {type: string}\n").unwrap().remove(0);
        let value = crate::convert::source_documents("spec: {foo: null, bar: null, baz: null}\n").unwrap().remove(0);
        let mut out = vec![];
        validate(&value, &schema, "", 0, &mut out);
        assert_eq!(out.len(), 2, "{out:?}");
        assert!(out[0].contains("/spec/foo") && out[0].contains("replaced by the default"), "{out:?}");
        assert!(out[1].contains("/spec/baz") && out[1].contains("dropped"), "{out:?}");
        assert!(out.iter().all(|m| !m.contains("expected")), "{out:?}");
    }

    #[test]
    fn cluster_scoped_crd_flags_metadata_namespace() {
        let crd = CRD.replace("spec:\n  group", "spec:\n  scope: Cluster\n  group");
        let d = doc("metadata:\n  name: a\n  namespace: ns\n");
        let h = MockHost::install().with_capabilities(&["project.read"]);
        h.set_project_root(Some("/p"));
        h.add_file("widget.yaml", &d.text);
        h.add_file("crd.yaml", &crd);
        let mut cache = Cache::default();
        cache.warm("/p/crd.yaml", &crd);
        assert!(cache.diagnostics(&d).iter().any(|x| x.message.contains("cluster-scoped")));
    }
}
