//! Local Helm values-schema and dependency assistance. Providers never run Helm
//! or fetch remote schemas; indexed sources must still match their project owner.
use openidea_plugin::{
    Diagnostic, Position, Range, Severity, host,
    providers::{CompletionItem, CompletionKind, Document},
    text,
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use yaml_rust2::Yaml;
const CAP: usize = 128 << 10;
#[derive(Clone)]
struct Chart {
    owner: String,
    directory: String,
    chart: String,
    schema: Option<(String, Value)>,
    values: Option<String>,
}
#[derive(Default)]
pub(crate) struct Cache {
    charts: crate::lru::Lru<String, Chart>,
    catalogs: BTreeMap<String, Catalog>,
}
struct Catalog {
    owner: String,
    identity: String,
    entries: BTreeMap<usize, Vec<String>>,
    at: std::time::Instant,
    captured: String,
}
fn yaml(source: &str) -> Option<Yaml> {
    if source.len() > CAP {
        return None;
    }
    let mut docs = crate::convert::source_documents(source)?;
    (docs.len() == 1).then(|| docs.remove(0))
}
fn json(source: &str) -> Option<Value> {
    // The YAML guard rejects duplicate decoded keys before serde_json can replace
    // them; serde_json separately restricts the schema to actual JSON syntax.
    yaml(source)?;
    serde_json::from_str(source).ok()
}
fn simple(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 128
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-./".contains(&b))
}
fn owned(owner: &str, path: &str) -> bool {
    path.starts_with(&format!("{}/", owner.trim_end_matches('/')))
        && !path.split('/').any(|part| matches!(part, "." | ".."))
        && host::project_of(path)
            .and_then(|project| project.root().ok())
            .as_deref()
            == Some(owner)
}
fn resolve<'a>(schema: &'a Value, root: &'a Value, depth: usize) -> Option<&'a Value> {
    if depth > 16
        || ["anyOf", "oneOf", "not", "if", "then", "else"]
            .iter()
            .any(|key| schema.get(key).is_some())
    {
        return None;
    }
    if let Some(reference) = schema.get("$ref") {
        let pointer = reference.as_str()?.strip_prefix('#')?;
        return resolve(root.pointer(pointer)?, root, depth + 1);
    }
    schema.is_object().then_some(schema)
}
fn properties<'a>(
    schema: &'a Value,
    root: &'a Value,
    depth: usize,
    out: &mut BTreeMap<String, &'a Value>,
) -> Option<()> {
    let schema = resolve(schema, root, depth)?;
    if let Some(parts) = schema.get("allOf") {
        for part in parts.as_array()?.iter().take(32) {
            properties(part, root, depth + 1, out)?;
        }
    }
    if let Some(props) = schema.get("properties").and_then(Value::as_object) {
        if props.len() + out.len() > 256 {
            return None;
        }
        for (key, value) in props {
            if out.get(key).is_some_and(|previous| **previous != *value) {
                return None;
            }
            out.insert(key.clone(), value);
        }
    }
    Some(())
}
#[derive(Clone)]
enum Segment {
    Key(String),
    Index(usize),
}
fn at_path<'a>(root: &'a Value, path: &[Segment]) -> Option<&'a Value> {
    let mut schema = root;
    for part in path {
        let current = resolve(schema, root, 0)?;
        schema = match part {
            Segment::Key(key) => {
                let mut props = BTreeMap::new();
                properties(current, root, 0, &mut props)?;
                props.get(key).copied().or_else(|| {
                    current
                        .get("additionalProperties")
                        .filter(|value| value.is_object())
                })?
            }
            Segment::Index(index) => match current.get("items")? {
                Value::Array(items) => items.get(*index)?,
                value => value,
            },
        };
    }
    resolve(schema, root, 0)
}
struct Cursor {
    path: Vec<Segment>,
    value: bool,
    from: usize,
    to: usize,
    has_colon: bool,
    siblings: BTreeSet<String>,
    parsed: Yaml,
}
fn find_marker(
    node: &Yaml,
    marker: &str,
    path: &mut Vec<Segment>,
    value: bool,
    depth: usize,
) -> Option<Vec<Segment>> {
    if depth > 32 {
        return None;
    }
    if value && node.as_str() == Some(marker) {
        return Some(path.clone());
    }
    if let Some(map) = node.as_hash() {
        for (key, child) in map.iter().take(1024) {
            let key = key.as_str()?;
            if !value && key == marker {
                return Some(path.clone());
            }
            path.push(Segment::Key(key.into()));
            if let Some(found) = find_marker(child, marker, path, value, depth + 1) {
                return Some(found);
            }
            path.pop();
        }
    } else if let Some(list) = node.as_vec() {
        for (index, child) in list.iter().enumerate().take(1024) {
            path.push(Segment::Index(index));
            if let Some(found) = find_marker(child, marker, path, value, depth + 1) {
                return Some(found);
            }
            path.pop();
        }
    }
    None
}
fn cursor(source: &str, at: Position) -> Option<Cursor> {
    const MARKER: &str = "__openidea_helm_cursor_927b__";
    if source.contains(MARKER) || source.len() > CAP {
        return None;
    }
    let caret = text::offset(source, at)?;
    let start = source[..caret].rfind('\n').map_or(0, |at| at + 1);
    let end = source[caret..]
        .find(['\r', '\n'])
        .map_or(source.len(), |len| caret + len);
    let line = &source[start..end];
    let indentation = line.len() - line.trim_start_matches(' ').len();
    let sequence = line[indentation..].starts_with("- ");
    let leading = indentation + if sequence { 2 } else { 0 };
    let content = &line[leading..];
    if content.contains(['\t', '{', '}', '[', ']', '#']) || content.starts_with('-') {
        return None;
    }
    let prefix = &line[..leading];
    let colon = content.find(':');
    let value = colon.is_some_and(|colon| caret > start + leading + colon);
    if !value && content.contains(['"', '\'']) {
        return None;
    }
    let (from, to, probe_line) = if value {
        let colon = colon?;
        let raw = &content[colon + 1..];
        let trimmed = raw.trim();
        if trimmed.starts_with(['\'', '"']) {
            let quote = trimmed.chars().next()?;
            let inner = &trimmed[1..];
            let inner = inner.strip_suffix(quote).unwrap_or(inner);
            if inner.contains(['\\', '\'', '"']) || inner.chars().any(char::is_control) {
                return None;
            }
        } else if !trimmed
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-+^~<>= ".contains(&b))
        {
            return None;
        }
        let from = start + leading + colon + 1 + raw.len() - raw.trim_start().len();
        (
            from,
            end,
            format!("{}{}: {MARKER}", prefix, &content[..colon]),
        )
    } else {
        let stop = colon.unwrap_or(content.len());
        if !content[..stop]
            .trim()
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
        {
            return None;
        }
        (
            start + leading,
            start + leading + stop,
            format!("{prefix}{MARKER}: null"),
        )
    };
    let probe = format!("{}{}{}", &source[..start], probe_line, &source[end..]);
    let parsed = yaml(&probe)?;
    let path = find_marker(&parsed, MARKER, &mut Vec::new(), value, 0)?;
    let mut parent = &parsed;
    for segment in &path {
        parent = match segment {
            Segment::Key(key) => &parent[key.as_str()],
            Segment::Index(index) => &parent[*index],
        };
    }
    let siblings = parent
        .as_hash()
        .map(|values| {
            values
                .keys()
                .filter_map(Yaml::as_str)
                .filter(|key| *key != MARKER)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    Some(Cursor {
        path,
        value,
        from,
        to,
        has_colon: colon.is_some(),
        siblings,
        parsed,
    })
}
fn json_value(value: &Yaml) -> Option<Value> {
    match value {
        Yaml::String(value) => Some(Value::String(value.clone())),
        Yaml::Integer(value) => Some((*value).into()),
        Yaml::Boolean(value) => Some((*value).into()),
        Yaml::Null => Some(Value::Null),
        Yaml::Real(value) => serde_json::from_str(value).ok(),
        Yaml::Array(values) => values
            .iter()
            .map(json_value)
            .collect::<Option<Vec<_>>>()
            .map(Value::Array),
        Yaml::Hash(values) => values
            .iter()
            .map(|(key, value)| Some((key.as_str()?.into(), json_value(value)?)))
            .collect::<Option<serde_json::Map<_, _>>>()
            .map(Value::Object),
        _ => None,
    }
}
fn schema_equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(left), Value::Number(right)) => {
            left == right || left.as_f64() == right.as_f64()
        }
        (Value::Array(left), Value::Array(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(left, right)| schema_equal(left, right))
        }
        (Value::Object(left), Value::Object(right)) => {
            left.len() == right.len()
                && left.iter().all(|(key, value)| {
                    right
                        .get(key)
                        .is_some_and(|right| schema_equal(value, right))
                })
        }
        _ => left == right,
    }
}
fn validate(
    value: &Value,
    schema: &Value,
    root: &Value,
    path: &str,
    depth: usize,
    out: &mut Vec<String>,
) {
    if depth > 16 || out.len() >= 64 {
        return;
    }
    if schema == &Value::Bool(false) {
        out.push(format!(
            "{path}: this value is forbidden by the local Helm schema"
        ));
        return;
    }
    let Some(schema) = resolve(schema, root, depth) else {
        return;
    };
    if let Some(parts) = schema.get("allOf").and_then(Value::as_array) {
        for part in parts.iter().take(32) {
            validate(value, part, root, path, depth + 1, out);
        }
    }
    let matches_type = |kind: &str| match kind {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "integer" => {
            value.as_i64().is_some()
                || value.as_u64().is_some()
                || value.as_f64().is_some_and(|number| number.fract() == 0.0)
        }
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        _ => true,
    };
    if let Some(ty) = schema.get("type") {
        let correct = ty
            .as_str()
            .map(matches_type)
            .or_else(|| {
                ty.as_array()
                    .map(|types| types.iter().filter_map(Value::as_str).any(matches_type))
            })
            .unwrap_or(true);
        if !correct {
            out.push(format!("{path}: expected {ty} (local Helm values schema)"));
            return;
        }
    }
    if schema
        .get("enum")
        .and_then(Value::as_array)
        .is_some_and(|values| !values.iter().any(|choice| schema_equal(choice, value)))
        || schema
            .get("const")
            .is_some_and(|constant| !schema_equal(constant, value))
    {
        out.push(format!(
            "{path}: value is outside the local Helm schema choices"
        ));
    }
    if let Some(object) = value.as_object() {
        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            for key in required.iter().filter_map(Value::as_str).take(64) {
                if !object.contains_key(key) {
                    out.push(format!(
                        "{path}: missing required value {key} (local Helm schema)"
                    ));
                }
            }
        }
        let props = schema.get("properties").and_then(Value::as_object);
        for (key, child) in object.iter().take(256) {
            let path = format!("{path}/{key}");
            if let Some(property) = props.and_then(|props| props.get(key)) {
                validate(child, property, root, &path, depth + 1, out);
            } else if schema.get("patternProperties").is_none() {
                if let Some(additional) = schema.get("additionalProperties") {
                    validate(child, additional, root, &path, depth + 1, out);
                }
            }
        }
    }
    if let Some(array) = value.as_array() {
        for (index, child) in array.iter().enumerate().take(256) {
            if let Some(items) = schema.get("items") {
                let items = if let Value::Array(items) = items {
                    items.get(index)
                } else {
                    Some(items)
                };
                if let Some(items) = items {
                    validate(
                        child,
                        items,
                        root,
                        &format!("{path}/{index}"),
                        depth + 1,
                        out,
                    );
                }
            }
        }
    }
    if let Some(number) = value.as_f64() {
        for (key, valid) in [("minimum", true), ("maximum", false)] {
            if let Some(limit) = schema.get(key).and_then(Value::as_f64) {
                if valid && number < limit || !valid && number > limit {
                    out.push(format!("{path}: outside {key} {limit} (local Helm schema)"));
                }
            }
        }
    }
}
impl Cache {
    pub(crate) fn clear(&mut self) {
        self.charts.clear();
        self.catalogs.clear();
    }
    pub(crate) fn evict(&mut self, path: &str) {
        let Some((directory, file)) = path.rsplit_once('/') else {
            return;
        };
        if matches!(file, "Chart.yaml" | "values.yaml" | "values.schema.json") {
            self.charts.remove(directory);
            self.catalogs.remove(directory);
        }
    }
    pub(crate) fn warm_file(&mut self, path: &str) {
        if let Some(project) = host::project_of(path) {
            if let Ok(source) = project.read_file(path) {
                self.warm(path, &source);
            }
        }
    }
    pub(crate) fn warm(&mut self, path: &str, source: &str) {
        let Some((directory, file)) = path.rsplit_once('/') else {
            return;
        };
        if !matches!(file, "Chart.yaml" | "values.yaml" | "values.schema.json") {
            return;
        }
        if source.len() > CAP {
            return;
        }
        let Some(project) = host::project_of(path) else {
            return;
        };
        let Ok(owner) = project.root() else {
            return;
        };
        let read = |name: &str| -> Option<String> {
            let full = format!("{directory}/{name}");
            if !owned(&owner, &full) {
                return None;
            }
            let value = if name == file {
                source.to_owned()
            } else {
                project.read_file(&full).ok()?
            };
            (value.len() <= CAP).then_some(value)
        };
        let Some(chart) = read("Chart.yaml") else {
            return;
        };
        let Some(metadata) = yaml(&chart) else {
            return;
        };
        if metadata["apiVersion"].as_str() != Some("v2") || metadata["name"].as_str().is_none() {
            self.charts.remove(directory);
            return;
        }
        self.charts.remove(directory);
        let schema = match read("values.schema.json") {
            Some(source) => match json(&source) {
                Some(schema) => Some((source, schema)),
                None => return,
            },
            None => None,
        };
        let values = read("values.yaml");
        self.charts.insert(
            directory.into(),
            Chart {
                owner,
                directory: directory.into(),
                chart,
                schema,
                values,
            },
            64,
        );
    }
    fn chart(&self, doc: &Document) -> Option<&Chart> {
        if doc.text.len() > CAP || doc.language != "yaml" {
            return None;
        }
        let directory = doc.path.rsplit_once('/')?.0;
        let chart = self.charts.get(directory)?;
        if !owned(&chart.owner, &doc.path) {
            return None;
        }
        let project = host::project_of(&doc.path)?;
        for (name, expected) in [
            ("Chart.yaml", Some(chart.chart.as_str())),
            (
                "values.schema.json",
                chart.schema.as_ref().map(|(source, _)| source.as_str()),
            ),
            ("values.yaml", chart.values.as_deref()),
        ] {
            let path = format!("{}/{name}", chart.directory);
            if !owned(&chart.owner, &path) {
                return None;
            }
            if let Some(expected) = expected {
                if project.read_file(&path).ok()?.as_str() != expected && path != doc.path {
                    return None;
                }
            }
        }
        Some(chart)
    }
    pub(crate) fn completion(&self, doc: &Document, at: Position) -> Vec<CompletionItem> {
        let Some(chart) = self.chart(doc) else {
            return vec![];
        };
        let Some(cursor) = cursor(&doc.text, at) else {
            return vec![];
        };
        if doc.path.ends_with("/Chart.yaml") {
            return self.dependency_completion(doc, &cursor, chart);
        }
        if !doc.path.ends_with("/values.yaml") {
            return vec![];
        }
        let Some((_, root)) = &chart.schema else {
            return vec![];
        };
        let Some(schema) = at_path(root, &cursor.path) else {
            return vec![];
        };
        let range = Range::new(
            text::position(&doc.text, cursor.from),
            text::position(&doc.text, cursor.to),
        );
        if cursor.value {
            let values = schema
                .get("enum")
                .and_then(Value::as_array)
                .cloned()
                .or_else(|| schema.get("const").map(|value| vec![value.clone()]))
                .unwrap_or_else(|| {
                    if schema.get("type").and_then(Value::as_str) == Some("boolean") {
                        vec![true.into(), false.into()]
                    } else {
                        vec![]
                    }
                });
            return values
                .into_iter()
                .take(128)
                .filter(|value| !value.is_array() && !value.is_object())
                .filter_map(|value| {
                    let insert = serde_json::to_string(&value).ok()?;
                    let mut item = CompletionItem::new(
                        value.as_str().unwrap_or(&insert),
                        CompletionKind::EnumMember,
                    );
                    item.insert_text = insert;
                    item.replace = Some(range);
                    Some(item)
                })
                .collect();
        }
        let mut props = BTreeMap::new();
        if properties(schema, root, 0, &mut props).is_none() {
            return vec![];
        }
        props
            .into_iter()
            .filter(|(key, _)| simple(key) && !cursor.siblings.contains(key))
            .map(|(key, property)| {
                let mut item = CompletionItem::new(&key, CompletionKind::Property);
                let key = if matches!(key.as_str(), "true" | "false" | "null")
                    || key.starts_with(|ch: char| ch.is_ascii_digit())
                {
                    serde_json::to_string(&key).unwrap_or_default()
                } else {
                    key
                };
                item.insert_text = if cursor.has_colon {
                    key
                } else {
                    format!("{key}: $0")
                };
                item.snippet = !cursor.has_colon;
                item.replace = Some(range);
                item.detail = Some(format!(
                    "{} — {} — values.schema.json",
                    property
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or("schema"),
                    property
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .chars()
                        .take(256)
                        .collect::<String>()
                ));
                item
            })
            .collect()
    }
    pub(crate) fn refresh_dependency_catalog(
        &mut self,
        ctx: &openidea_plugin::Invocation,
    ) -> Result<usize, openidea_plugin::Error> {
        let project = ctx
            .project()
            .ok_or("Open the owning chart project first.")?;
        let owner = project.root()?;
        let editor = ctx
            .editor()
            .ok_or("Open Chart.yaml, values.yaml or a chart template first.")?;
        let path = editor.path();
        self.warm_file(&path);
        let directory = self
            .charts
            .keys()
            .filter(|directory| path.starts_with(&format!("{directory}/")))
            .max_by_key(|directory| directory.len())
            .cloned()
            .ok_or("Open or index the chart metadata first.")?;
        let chart = self
            .charts
            .get(&directory)
            .filter(|chart| chart.owner == owner)
            .ok_or(openidea_plugin::Error::Stale)?
            .clone();
        self.catalogs.remove(&directory);
        if project.read_file(&format!("{directory}/Chart.yaml"))? != chart.chart {
            return Err(openidea_plugin::Error::Stale);
        }
        let metadata = yaml(&chart.chart).ok_or("Chart.yaml must be valid unambiguous YAML.")?;
        let dependencies = metadata["dependencies"]
            .as_vec()
            .ok_or("This chart has no dependency list.")?;
        if dependencies.len() > 32 {
            return Err("Dependency catalog refresh supports at most 32 dependencies.".into());
        }
        let output = host::run_process(
            "helm",
            &[
                "repo".into(),
                "list".into(),
                "--output".into(),
                "json".into(),
            ],
            None,
        )?;
        if output.status != Some(0) {
            return Err(
                "Could not read the Helm repository list; configure repositories first.".into(),
            );
        }
        let repos = json(&output.stdout)
            .and_then(|value| value.as_array().cloned())
            .filter(|repos| repos.len() <= 256)
            .ok_or("Unsupported or oversized Helm repository list.")?;
        let mut repositories = BTreeMap::new();
        for repo in repos {
            let name = repo
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| simple(name) && !name.contains('/'))
                .ok_or(openidea_plugin::Error::Stale)?;
            let url = repo
                .get("url")
                .and_then(Value::as_str)
                .filter(|url| url.len() <= 2048 && !url.chars().any(char::is_control))
                .ok_or(openidea_plugin::Error::Stale)?;
            if repositories
                .insert(name.to_owned(), url.trim_end_matches('/').to_owned())
                .is_some()
            {
                return Err(
                    "Helm repository aliases are ambiguous; repair the repository list first."
                        .into(),
                );
            }
        }
        let mut entries = BTreeMap::new();
        let mut count = 0;
        for (index, dependency) in dependencies.iter().enumerate() {
            let Some(name) = metadata_scalar(&dependency["name"])
                .filter(|name| simple(name) && !name.contains('/'))
            else {
                continue;
            };
            let Some(repository) = metadata_scalar(&dependency["repository"]) else {
                continue;
            };
            let alias = repository
                .strip_prefix('@')
                .or_else(|| repository.strip_prefix("alias:"))
                .filter(|alias| repositories.contains_key(*alias))
                .map(str::to_owned)
                .or_else(|| {
                    let matching: Vec<_> = repositories
                        .iter()
                        .filter(|(_, url)| **url == repository.trim_end_matches('/'))
                        .collect();
                    (matching.len() == 1).then(|| matching[0].0.clone())
                });
            let Some(alias) = alias else {
                continue;
            };
            let lookup = format!("{alias}/{name}");
            let output = host::run_process(
                "helm",
                &[
                    "search".into(),
                    "repo".into(),
                    lookup.clone(),
                    "--versions".into(),
                    "--output".into(),
                    "json".into(),
                ],
                None,
            )?;
            if output.status != Some(0) {
                return Err("Helm dependency search failed; run Update Helm Repositories if its local index is missing.".into());
            }
            let rows = json(&output.stdout)
                .and_then(|value| value.as_array().cloned())
                .filter(|rows| rows.len() <= 4096)
                .ok_or("Unsupported or oversized Helm dependency search output.")?;
            let versions: BTreeSet<_> = rows
                .iter()
                .filter(|row| row.get("name").and_then(Value::as_str) == Some(&lookup))
                .filter_map(|row| row.get("version").and_then(Value::as_str))
                .filter(|version| {
                    version.len() <= 128
                        && !version.is_empty()
                        && version
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || b".-+_".contains(&byte))
                })
                .map(str::to_owned)
                .collect();
            count += versions.len();
            entries.insert(index, versions.into_iter().collect());
        }
        let final_repos = host::run_process(
            "helm",
            &[
                "repo".into(),
                "list".into(),
                "--output".into(),
                "json".into(),
            ],
            None,
        )?;
        if final_repos.status != Some(0) {
            return Err(openidea_plugin::Error::Stale);
        }
        let final_repos = json(&final_repos.stdout)
            .and_then(|value| value.as_array().cloned())
            .filter(|repos| repos.len() <= 256)
            .ok_or(openidea_plugin::Error::Stale)?;
        let mut final_identity = BTreeMap::new();
        for repo in final_repos {
            let name = repo
                .get("name")
                .and_then(Value::as_str)
                .ok_or(openidea_plugin::Error::Stale)?;
            let url = repo
                .get("url")
                .and_then(Value::as_str)
                .ok_or(openidea_plugin::Error::Stale)?;
            if final_identity
                .insert(name.to_owned(), url.trim_end_matches('/').to_owned())
                .is_some()
            {
                return Err(openidea_plugin::Error::Stale);
            }
        }
        if final_identity != repositories {
            return Err(
                "Helm repository configuration changed during refresh; retry the command.".into(),
            );
        }
        if project.root()? != owner
            || project.read_file(&format!("{directory}/Chart.yaml"))? != chart.chart
            || !owned(&owner, &format!("{directory}/Chart.yaml"))
        {
            return Err(openidea_plugin::Error::Stale);
        }
        if self.catalogs.len() >= 8 {
            self.catalogs.clear();
        }
        self.catalogs.insert(
            directory,
            Catalog {
                owner,
                identity: dependency_identity(&metadata),
                entries,
                at: std::time::Instant::now(),
                captured: captured(),
            },
        );
        Ok(count)
    }
    fn dependency_completion(
        &self,
        doc: &Document,
        cursor: &Cursor,
        chart: &Chart,
    ) -> Vec<CompletionItem> {
        if cursor.value
            && let [
                Segment::Key(dependencies),
                Segment::Index(index),
                Segment::Key(key),
            ] = cursor.path.as_slice()
        {
            if dependencies == "dependencies" && key == "version" {
                let Some(catalog) = self.catalogs.get(&chart.directory).filter(|catalog| {
                    catalog.owner == chart.owner
                        && catalog.identity == dependency_identity(&cursor.parsed)
                        && catalog.at.elapsed() < std::time::Duration::from_secs(600)
                }) else {
                    return vec![];
                };
                let range = Range::new(
                    text::position(&doc.text, cursor.from),
                    text::position(&doc.text, cursor.to),
                );
                return catalog
                    .entries
                    .get(index)
                    .into_iter()
                    .flatten()
                    .take(128)
                    .map(|version| {
                        let mut item = CompletionItem::new(version, CompletionKind::Value);
                        item.insert_text = serde_json::to_string(version).unwrap_or_default();
                        item.replace = Some(range);
                        item.detail = Some(format!(
                            "Helm repository cache captured {} ({}s ago); refresh to update",
                            catalog.captured,
                            catalog.at.elapsed().as_secs()
                        ));
                        item
                    })
                    .collect();
            }
        }
        dependency_completion(doc, cursor, chart)
    }
    pub(crate) fn code_actions(
        &self,
        doc: &Document,
    ) -> Vec<openidea_plugin::providers::CodeAction> {
        if !doc.path.ends_with("/values.yaml")
            || doc.text.contains("yaml-language-server:")
            || doc.text.contains("$schema")
        {
            return vec![];
        }
        let Some(chart) = self.chart(doc) else {
            return vec![];
        };
        if chart.schema.is_none() {
            return vec![];
        }
        let eol = if doc.text.contains("\r\n") {
            "\r\n"
        } else {
            "\n"
        };
        let at = Position::new(0, 0);
        vec![openidea_plugin::providers::CodeAction {
            title: "Use local Helm values schema in YAML language server".into(),
            kind: Some("source".into()),
            preferred: false,
            edits: vec![(
                doc.path.clone(),
                vec![openidea_plugin::TextEdit {
                    range: Range::new(at, at),
                    text: format!("# yaml-language-server: $schema=./values.schema.json{eol}"),
                }],
            )],
            data: None,
        }]
    }
    pub(crate) fn diagnostics(&self, doc: &Document) -> Vec<Diagnostic> {
        let Some(chart) = self.chart(doc) else {
            return vec![];
        };
        let Some(parsed) = yaml(&doc.text) else {
            return vec![];
        };
        let values_file = doc.path.ends_with("/values.yaml");
        let mut messages = Vec::new();
        if values_file {
            if let Some((_, schema)) = &chart.schema {
                if let Some(value) = json_value(&parsed) {
                    validate(&value, schema, schema, "values", 0, &mut messages);
                }
            }
        } else if doc.path.ends_with("/Chart.yaml") {
            dependency_diagnostics(&parsed, chart, &mut messages);
        }
        messages
            .into_iter()
            .take(64)
            .map(|message| {
                let range = if values_file {
                    values_finding_range(&doc.text, &message)
                } else {
                    crate::helm_values::find_key(&doc.text, &["dependencies".to_owned()]).unwrap_or_default()
                };
                Diagnostic::new(range, Severity::Warning, message).code("helm-local-schema")
            })
            .collect()
    }
}
/// The deepest key of the finding's `values/a/b` path that the file spells out.
fn values_finding_range(values: &str, message: &str) -> Range {
    let path = message.split_once(": ").map_or("", |(path, _)| path);
    let mut keys: Vec<String> = path.split('/').skip(1).map(str::to_owned).collect();
    let array_at = keys.iter().position(|key| key.parse::<usize>().is_ok());
    keys.truncate(array_at.unwrap_or(keys.len()));
    while !keys.is_empty() {
        if let Some(range) = crate::helm_values::find_key(values, &keys) {
            return range;
        }
        keys.pop();
    }
    Range::default()
}
fn dependency_identity(metadata: &Yaml) -> String {
    let identities: Vec<_> = metadata["dependencies"]
        .as_vec()
        .into_iter()
        .flatten()
        .map(|dependency| {
            [
                metadata_scalar(&dependency["name"]),
                metadata_scalar(&dependency["repository"]),
                metadata_scalar(&dependency["alias"]),
            ]
        })
        .collect();
    serde_json::to_string(&(
        metadata_scalar(&metadata["apiVersion"]),
        metadata_scalar(&metadata["name"]),
        identities,
    ))
    .unwrap_or_default()
}
fn captured() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    let z = seconds as i64 / 86400 + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        seconds % 86400 / 3600,
        seconds % 3600 / 60
    )
}
fn dependency_completion(doc: &Document, cursor: &Cursor, chart: &Chart) -> Vec<CompletionItem> {
    let context = if cursor.value {
        &cursor.path[..cursor.path.len().saturating_sub(1)]
    } else {
        cursor.path.as_slice()
    };
    let dependency =
        matches!(context, [Segment::Key(key), Segment::Index(_)] if key == "dependencies");
    let import = matches!(context, [Segment::Key(key), Segment::Index(_), Segment::Key(imports), Segment::Index(_)] if key == "dependencies" && imports == "import-values");
    if !dependency && !import {
        return vec![];
    }
    let range = Range::new(
        text::position(&doc.text, cursor.from),
        text::position(&doc.text, cursor.to),
    );
    if cursor.value {
        if matches!(cursor.path.last(), Some(Segment::Key(key)) if key == "condition") {
            let Some(source) = &chart.values else {
                return vec![];
            };
            let Some(values) = yaml(source) else {
                return vec![];
            };
            let mut paths = Vec::new();
            boolean_paths(&values, "", 0, &mut paths);
            return paths
                .into_iter()
                .take(128)
                .map(|path| {
                    let mut item = CompletionItem::new(&path, CompletionKind::Value);
                    item.insert_text = path;
                    item.replace = Some(range);
                    item
                })
                .collect();
        }
        return vec![];
    }
    let keys: &[&str] = if import {
        &["child", "parent"]
    } else {
        &[
            "name",
            "version",
            "repository",
            "alias",
            "condition",
            "tags",
            "import-values",
        ]
    };
    keys.iter()
        .filter(|key| !cursor.siblings.contains(**key))
        .map(|key| {
            let mut item = CompletionItem::new(*key, CompletionKind::Property);
            item.insert_text = if cursor.has_colon {
                (*key).into()
            } else {
                format!("{key}: $0")
            };
            item.snippet = !cursor.has_colon;
            item.replace = Some(range);
            item
        })
        .collect()
}
fn boolean_paths(value: &Yaml, prefix: &str, depth: usize, out: &mut Vec<String>) {
    if depth > 16 || out.len() >= 128 {
        return;
    }
    if value.as_bool().is_some() && !prefix.is_empty() {
        out.push(prefix.into());
    }
    if let Some(map) = value.as_hash() {
        for (key, child) in map {
            if let Some(key) = key.as_str().filter(|key| simple(key) && !key.contains('.')) {
                boolean_paths(
                    child,
                    &if prefix.is_empty() {
                        key.into()
                    } else {
                        format!("{prefix}.{key}")
                    },
                    depth + 1,
                    out,
                );
            }
        }
    }
}
fn metadata_scalar(value: &Yaml) -> Option<String> {
    // Helm's typed Chart.yaml decoding accepts scalar-to-string conversion.
    match value {
        Yaml::String(value) | Yaml::Real(value) => Some(value.clone()),
        Yaml::Integer(value) => Some(value.to_string()),
        Yaml::Boolean(value) => Some(value.to_string()),
        _ => None,
    }
}
fn dependency_diagnostics(metadata: &Yaml, chart: &Chart, messages: &mut Vec<String>) {
    let dependencies = &metadata["dependencies"];
    if matches!(dependencies, Yaml::BadValue) {
        return;
    }
    let Some(dependencies) = dependencies.as_vec() else {
        messages.push("Chart dependencies must be a sequence".into());
        return;
    };
    let mut names = BTreeSet::new();
    for (index, dependency) in dependencies.iter().enumerate().take(256) {
        let label = format!("dependencies/{index}");
        if dependency.as_hash().is_none() {
            messages.push(format!("{label}: expected a dependency mapping"));
            continue;
        }
        for key in ["name", "version"] {
            if !metadata_scalar(&dependency[key]).is_some_and(|value| !value.trim().is_empty()) {
                messages.push(format!("{label}: missing literal {key}"));
            }
        }
        let name = metadata_scalar(&dependency["alias"])
            .filter(|name| !name.is_empty())
            .or_else(|| metadata_scalar(&dependency["name"]));
        if let Some(name) = name {
            if !names.insert(name.clone()) {
                messages.push(format!(
                    "{label}: duplicate effective dependency name/alias {name}"
                ));
            }
        }
        for key in ["alias", "condition", "repository"] {
            if !matches!(dependency[key], Yaml::BadValue | Yaml::Null)
                && metadata_scalar(&dependency[key]).is_none()
            {
                messages.push(format!("{label}: {key} must be a string"));
            }
        }
        if !matches!(dependency["tags"], Yaml::BadValue | Yaml::Null)
            && !dependency["tags"]
                .as_vec()
                .is_some_and(|tags| tags.iter().all(|tag| metadata_scalar(tag).is_some()))
        {
            messages.push(format!("{label}: tags must be a string sequence"));
        }
        if !matches!(dependency["import-values"], Yaml::BadValue | Yaml::Null)
            && dependency["import-values"].as_vec().is_none()
        {
            messages.push(format!("{label}: import-values must be a sequence"));
        }
        if let Some(imports) = dependency["import-values"].as_vec() {
            for import in imports {
                if import.as_str().is_none()
                    && !(import["child"].as_str().is_some() && import["parent"].as_str().is_some())
                {
                    messages.push(format!("{label}: import-values entries require an export string or child/parent mapping"));
                }
            }
        }
    }
    let _ = chart; // Dependency enabling uses final Helm-merged values; do not guess it here.
}
#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    fn schema() -> &'static str {
        r##"{"$schema":"http://json-schema.org/draft-07/schema#","type":"object","additionalProperties":false,"required":["image"],"properties":{"image":{"$ref":"#/definitions/image"},"replicas":{"type":"integer","minimum":1},"enabled":{"type":"boolean"}},"definitions":{"image":{"type":"object","properties":{"repository":{"type":"string"},"pullPolicy":{"type":"string","enum":["Always","IfNotPresent"]}},"required":["repository"]}}}"##
    }
    fn setup() -> (MockHost, Cache) {
        let host = MockHost::install();
        host.add_file(
            "chart/Chart.yaml",
            "apiVersion: v2\nname: example\nversion: 1.0.0\n",
        );
        host.add_file("chart/values.schema.json", schema());
        host.add_file(
            "chart/values.yaml",
            "image:\n  repository: example\nenabled: true\nreplicas: 2\n",
        );
        let mut cache = Cache::default();
        cache.warm_file("/project/chart/Chart.yaml");
        (host, cache)
    }
    fn document(source: &str) -> Document {
        Document {
            path: "/project/chart/values.yaml".into(),
            language: "yaml".into(),
            version: 1,
            text: source.into(),
        }
    }
    fn end(source: &str) -> Position {
        text::position(source, source.len())
    }
    #[test]
    fn schema_completion_follows_local_refs_preserves_colons_and_offers_enums() {
        let (_host, cache) = setup();
        let doc = document("image:\n  pu");
        let items = cache.completion(&doc, end(&doc.text));
        let policy = items
            .iter()
            .find(|item| item.label == "pullPolicy")
            .unwrap();
        assert_eq!(policy.insert_text, "pullPolicy: $0");
        assert_eq!(
            policy.replace,
            Some(Range::new(Position::new(1, 2), Position::new(1, 4)))
        );
        let doc = document("image:\n  pullPolicy: A");
        let items = cache.completion(&doc, end(&doc.text));
        assert!(items.iter().any(|item| item.insert_text == "\"Always\""));
        let doc = document("image:\n  repository: example\n  pu: IfNotPresent");
        let items = cache.completion(&doc, Position::new(2, 4));
        assert_eq!(
            items
                .iter()
                .find(|item| item.label == "pullPolicy")
                .unwrap()
                .insert_text,
            "pullPolicy"
        );
        assert!(!items.iter().any(|item| item.label == "repository"));
        assert!(
            cache
                .completion(&document("text: |\n  pu"), Position::new(1, 4))
                .is_empty()
        );
    }
    #[test]
    fn values_schema_finding_has_key_range() {
        let (_host, cache) = setup();
        let source = "image: {}\nreplicas: 0\n";
        let findings = cache.diagnostics(&document(source));
        let at = |needle: &str| findings.iter().find(|finding| finding.message.contains(needle)).unwrap().range;
        assert_eq!(at("missing required value repository"), Range::new(Position::new(0, 0), Position::new(0, 5)));
        assert_eq!(at("minimum"), Range::new(Position::new(1, 0), Position::new(1, 8)));
    }
    #[test]
    fn schema_validation_matches_known_constraints_and_withdraws_stale_sources() {
        let (host, cache) = setup();
        let findings = cache.diagnostics(&document("image: {}\nreplicas: 0\nunknown: true\n"));
        assert!(findings.iter().any(|finding| {
            finding
                .message
                .contains("missing required value repository")
        }));
        assert!(
            findings
                .iter()
                .any(|finding| finding.message.contains("minimum"))
        );
        assert!(
            findings
                .iter()
                .any(|finding| finding.message.contains("forbidden"))
        );
        assert!(
            cache
                .diagnostics(&document("image:\n  repository: demo\nreplicas: 2\n"))
                .is_empty()
        );
        host.add_file("chart/values.schema.json", "{}");
        assert!(cache.diagnostics(&document("replicas: wrong")).is_empty());
        assert!(
            cache
                .completion(&document("rep"), Position::new(0, 3))
                .is_empty()
        );
    }
    #[test]
    fn duplicates_aliases_foreign_projects_and_remote_refs_do_not_guess() {
        let (host, mut cache) = setup();
        host.add_file(
            "chart/values.schema.json",
            r#"{"type":"object","type":"array"}"#,
        );
        cache.warm_file("/project/chart/values.schema.json");
        assert!(
            cache
                .completion(&document("rep"), Position::new(0, 3))
                .is_empty()
        );
        host.add_file(
            "chart/values.schema.json",
            r#"{"$ref":"https://example.test/schema.json"}"#,
        );
        cache.warm_file("/project/chart/values.schema.json");
        assert!(cache.diagnostics(&document("anything: 1")).is_empty());
        assert!(yaml("a: &shared {x: 1}\nb: *shared").is_none());
        let foreign = Document {
            path: "/elsewhere/chart/values.yaml".into(),
            ..document("rep")
        };
        assert!(cache.completion(&foreign, Position::new(0, 3)).is_empty());
    }
    #[test]
    fn dependency_keys_aliases_and_import_shapes_use_parsed_yaml_scope() {
        let (host, mut cache) = setup();
        let source = "apiVersion: v2\nname: example\nversion: 1.0.0\ndependencies:\n  - name: child\n    version: 1.0.0\n    rep";
        host.add_file("chart/Chart.yaml", source);
        cache.warm_file("/project/chart/Chart.yaml");
        let doc = Document {
            path: "/project/chart/Chart.yaml".into(),
            ..document(source)
        };
        let items = cache.completion(&doc, end(source));
        // An incomplete YAML key is probed as a real mapping without inventing indentation parents.
        assert!(items.iter().any(|item| item.label == "repository"));
        assert!(!items.iter().any(|item| item.label == "name"));
        let source = "apiVersion: v2\nname: example\nversion: 1.0.0\ndependencies:\n  - name: child\n    version: 1.0.0\n    condition: en";
        let doc = Document {
            path: "/project/chart/Chart.yaml".into(),
            ..document(source)
        };
        assert!(
            cache
                .completion(&doc, end(source))
                .iter()
                .any(|item| item.label == "enabled")
        );
        let source = "apiVersion: v2\nname: example\nversion: 1.0.0\ndependencies:\n  - na";
        let doc = Document {
            path: "/project/chart/Chart.yaml".into(),
            ..document(source)
        };
        assert!(
            cache
                .completion(&doc, end(source))
                .iter()
                .any(|item| item.label == "name")
        );
        let source = "apiVersion: v2\nname: example\nversion: 1.0.0\ndependencies:\n  - name: child\n    version: 1.0.0\n    alias: shared\n  - name: another\n    version: 2.0.0\n    alias: shared\n    tags: wrong\n    import-values: [4]\n";
        host.add_file("chart/Chart.yaml", source);
        cache.warm_file("/project/chart/Chart.yaml");
        let doc = Document {
            path: "/project/chart/Chart.yaml".into(),
            ..document(source)
        };
        let diagnostics = cache.diagnostics(&doc);
        assert!(
            diagnostics
                .iter()
                .any(|finding| finding.message.contains("duplicate effective"))
        );
        assert!(
            diagnostics
                .iter()
                .any(|finding| finding.message.contains("tags"))
        );
        assert!(
            diagnostics
                .iter()
                .any(|finding| finding.message.contains("import-values"))
        );
    }
    #[test]
    fn full_yaml_server_schema_opt_in_is_relative_scoped_and_preserves_crlf() {
        let (_host, cache) = setup();
        let doc = document("image:\r\n  repository: example\r\n");
        let actions = cache.code_actions(&doc);
        assert_eq!(actions.len(), 1);
        assert_eq!(
            actions[0].edits[0].1[0].text,
            "# yaml-language-server: $schema=./values.schema.json\r\n"
        );
        assert!(
            cache
                .code_actions(&document(
                    "# yaml-language-server: $schema=other.json\nimage: {}\n"
                ))
                .is_empty()
        );
    }

    #[test]
    fn explicit_dependency_catalog_refresh_is_project_scoped_dated_and_provider_only_reads_cache() {
        let (host, mut cache) = setup();
        let host = host.with_capabilities(&["project.read", "process:helm"]);
        let source = "apiVersion: v2\nname: example\nversion: 1.0.0\ndependencies:\n  - name: child\n    repository: \"@team\"\n    version: \"1\"\n";
        host.add_file("chart/Chart.yaml", source);
        host.open_editor("/project/chart/Chart.yaml", "yaml", source, &[]);
        host.on_process("helm", |args, _| Ok(openidea_plugin::ProcessOutput { status: Some(0), stderr: String::new(), stdout: if args[0] == "repo" {
            r#"[{"name":"team","url":"https://charts.example.test"}]"#.into()
        } else {
            assert_eq!(args, ["search", "repo", "team/child", "--versions", "--output", "json"]);
            r#"[{"name":"team/child","version":"2.0.0"},{"name":"team/child","version":"1.0.0"},{"name":"other/child","version":"9.9.9"}]"#.into()
        } }));
        assert_eq!(
            cache
                .refresh_dependency_catalog(&host.invocation())
                .unwrap(),
            2
        );
        let doc = Document {
            path: "/project/chart/Chart.yaml".into(),
            ..document(source)
        };
        let at = text::position(source, source.rfind('"').unwrap());
        let items = cache.completion(&doc, at);
        assert!(
            items
                .iter()
                .any(|item| item.label == "2.0.0" && item.insert_text == "\"2.0.0\"")
        );
        assert!(
            items
                .iter()
                .all(|item| item.detail.as_deref().unwrap().contains("UTC"))
        );
        assert!(!items.iter().any(|item| item.label == "9.9.9"));
        assert_eq!(
            host.process_runs().len(),
            3,
            "providers must not spawn helm"
        );
        let other = Document {
            text: source.replace("name: child", "name: changed"),
            ..doc
        };
        assert!(
            cache
                .completion(
                    &other,
                    text::position(&other.text, other.text.rfind('"').unwrap())
                )
                .is_empty()
        );
    }
    #[test]
    fn integer_json_schema_accepts_mathematically_integral_yaml_numbers() {
        let schema = serde_json::json!({"type":"integer","enum":[2]});
        let mut messages = Vec::new();
        validate(
            &serde_json::json!(2.0),
            &schema,
            &schema,
            "replicas",
            0,
            &mut messages,
        );
        assert!(messages.is_empty());
        validate(
            &serde_json::json!(2.5),
            &schema,
            &schema,
            "replicas",
            0,
            &mut messages,
        );
        assert!(!messages.is_empty());
    }
}
