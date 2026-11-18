//! DDEV's unmodified, bundled schema. No schema or ref is fetched on the
//! editing path; the complete Draft 7 validator has network/file resolving off.
use serde_json::Value;
use std::{
    collections::BTreeMap,
    sync::{Mutex, OnceLock},
};

pub const VERSION: &str = "DDEV v1.25.5";
pub struct Schema {
    // Never mutated after init, so the pointer-keyed condition cache stays valid.
    pub root: Value,
    pub validator: jsonschema::Validator,
    conditions: Mutex<BTreeMap<usize, Option<std::sync::Arc<jsonschema::Validator>>>>,
}
static PROJECT: OnceLock<Schema> = OnceLock::new();
static GLOBAL: OnceLock<Schema> = OnceLock::new();
pub fn project() -> &'static Schema {
    PROJECT.get_or_init(|| {
        let root: Value = serde_json::from_str(include_str!("../../schemas/project-v1.25.5.json"))
            .expect("bundled DDEV schema JSON");
        let validator = jsonschema::validator_for(&root).expect("bundled DDEV schema compiles");
        Schema {
            root,
            validator,
            conditions: Mutex::default(),
        }
    })
}
pub fn global() -> &'static Schema {
    GLOBAL.get_or_init(|| {
        let root: Value = serde_json::from_str(include_str!("../../schemas/global-v1.25.5.json"))
            .expect("bundled global DDEV schema JSON");
        let validator =
            jsonschema::validator_for(&root).expect("bundled global DDEV schema compiles");
        Schema {
            root,
            validator,
            conditions: Mutex::default(),
        }
    })
}

impl Schema {
    fn condition_holds(&self, condition: &Value, instance: &Value) -> bool {
        let key = std::ptr::from_ref(condition) as usize;
        let compiled = self
            .conditions
            .lock()
            .unwrap()
            .entry(key)
            .or_insert_with(|| {
                jsonschema::validator_for(condition)
                    .ok()
                    .map(std::sync::Arc::new)
            })
            .clone();
        compiled.is_some_and(|validator| validator.is_valid(instance))
    }
    pub fn variants<'a>(&'a self, schemas: &[&'a Value], instance: &Value) -> Vec<&'a Value> {
        fn visit<'a>(
            schema: &Schema,
            root: &'a Value,
            s: &'a Value,
            instance: &Value,
            out: &mut Vec<&'a Value>,
            depth: usize,
        ) {
            if depth > 24 {
                return;
            }
            out.push(s);
            if let Some(reference) = s
                .get("$ref")
                .and_then(Value::as_str)
                .and_then(|s| s.strip_prefix('#'))
                && let Some(target) = root.pointer(reference)
            {
                visit(schema, root, target, instance, out, depth + 1);
            }
            for keyword in ["allOf", "anyOf", "oneOf"] {
                if let Some(list) = s.get(keyword).and_then(Value::as_array) {
                    for branch in list {
                        visit(schema, root, branch, instance, out, depth + 1);
                    }
                }
            }
            if let Some(condition) = s.get("if") {
                let branch = if schema.condition_holds(condition, instance) {
                    "then"
                } else {
                    "else"
                };
                if let Some(branch) = s.get(branch) {
                    visit(schema, root, branch, instance, out, depth + 1);
                }
            }
        }
        let mut out = vec![];
        for s in schemas {
            visit(self, &self.root, s, instance, &mut out, 0);
        }
        out
    }
    pub fn at<'a>(&'a self, path: &[String], document: &Value) -> Vec<&'a Value> {
        let mut schemas = vec![&self.root];
        let mut instance = document;
        for part in path {
            let mut next = vec![];
            for s in self.variants(&schemas, instance) {
                if instance.is_array() {
                    if let Some(items) = s.get("items") {
                        next.push(items);
                    }
                } else if let Some(property) = s.get("properties").and_then(|p| p.get(part)) {
                    next.push(property);
                } else if let Some(other) = s.get("additionalProperties").filter(|v| v.is_object())
                {
                    next.push(other);
                }
            }
            schemas = next;
            instance = if instance.is_array() {
                part.parse::<usize>()
                    .ok()
                    .and_then(|i| instance.get(i))
                    .unwrap_or(&Value::Null)
            } else {
                instance.get(part).unwrap_or(&Value::Null)
            };
        }
        self.variants(&schemas, instance)
    }
    pub fn properties<'a>(&'a self, schemas: &[&'a Value]) -> BTreeMap<&'a str, Vec<&'a Value>> {
        let mut props: BTreeMap<&str, Vec<&Value>> = BTreeMap::new();
        for schema in schemas {
            if let Some(obj) = schema.get("properties").and_then(Value::as_object) {
                for (key, value) in obj {
                    props.entry(key).or_default().push(value);
                }
            }
        }
        props
    }
    pub fn values(&self, schemas: &[&Value]) -> Vec<Value> {
        let mut values = BTreeMap::new();
        for s in schemas {
            let mut offered = vec![];
            if let Some(list) = s.get("enum").and_then(Value::as_array) {
                offered.extend(list.iter().cloned());
            }
            if let Some(value) = s.get("const").or_else(|| s.get("default")) {
                offered.push(value.clone());
            }
            if s.get("type").and_then(Value::as_str) == Some("boolean") {
                offered.extend([Value::Bool(true), Value::Bool(false)]);
            }
            if s.get("type").and_then(Value::as_str) == Some("null") {
                offered.push(Value::Null);
            }
            for value in offered {
                values.insert(value.to_string(), value);
            }
        }
        values.into_values().collect()
    }
    pub fn documentation(&self, name: &str, schemas: &[&Value]) -> String {
        let description = schemas
            .iter()
            .filter_map(|s| s.get("description").and_then(Value::as_str))
            .find(|s| !s.is_empty())
            .unwrap_or("DDEV configuration property.");
        let types: std::collections::BTreeSet<_> = schemas
            .iter()
            .filter_map(|s| s.get("type").and_then(Value::as_str))
            .collect();
        let mut doc = format!(
            "**{name}**\n\n{description}\n\nType: `{}`. Schema: {VERSION}.",
            types.into_iter().collect::<Vec<_>>().join(" | ")
        );
        let values = self.values(schemas);
        if !values.is_empty() {
            doc.push_str(&format!(
                "\n\nSuggested values: {}.",
                values
                    .iter()
                    .map(|v| format!("`{}`", v))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        doc.push_str("\n\n[DDEV configuration reference](https://docs.ddev.com/en/stable/users/configuration/config/)");
        doc
    }
}
