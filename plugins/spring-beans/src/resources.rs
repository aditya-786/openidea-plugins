//! Source-model details for the Beans resource page, not a runtime context model.
use crate::cache::Cache;
use crate::{World, location_key};
use openidea_plugin::views::{Body, Button, View};
use serde_json::{Value, json};

const MAX_ROWS: usize = 500;
const MAX_FIELDS: usize = 20;
const MAX_VALUE: usize = 1024;
const MAX_BODY: usize = 4 * 1024 * 1024;

fn clip(value: &str) -> String {
    if value.chars().count() <= MAX_VALUE {
        value.into()
    } else {
        format!("{}… (truncated)", value.chars().take(MAX_VALUE).collect::<String>())
    }
}
fn fact(label: &str, value: &str) -> Value {
    json!({"label":label,"value":clip(value)})
}

pub(crate) fn view(index: &Cache, empty: &str) -> View {
    let world = World::new(index);
    let mut beans: Vec<_> = world
        .files
        .iter()
        .flat_map(|(path, model)| model.beans.iter().enumerate().map(move |(owner, bean)| (path, model, owner, bean)))
        .collect();
    beans.sort_by(|a, b| a.3.name.cmp(&b.3.name).then_with(|| a.0.cmp(b.0)).then_with(|| a.3.range.start.cmp(&b.3.range.start)));
    let total = beans.len();
    if total == 0 {
        let text = if index.complete() { empty.to_owned() } else { index.state() };
        return View::message(&text)
            .button(Button::new("refresh", "Refresh").icon("refresh"));
    }
    let mut rows = Vec::new();
    for (path, model, owner, bean) in beans.into_iter().take(MAX_ROWS) {
        let profiles = if bean.profiles.is_empty() { "No explicit profile".into() } else { bean.profiles.join(", ") };
        let inputs: Vec<_> = model
            .injections
            .iter()
            .filter(|i| i.owner == Some(owner))
            .take(MAX_FIELDS)
            .map(|i| {
                format!(
                    "{}: {}{}",
                    i.name,
                    if i.ty.is_empty() { "by name" } else { &i.ty },
                    i.qualifier.as_ref().map(|q| format!(" (qualifier {q})")).unwrap_or_default()
                )
            })
            .collect();
        let input_count = model.injections.iter().filter(|i| i.owner == Some(owner)).count();
        let wiring = if input_count == 0 {
            "No local wiring inputs recorded".into()
        } else {
            format!(
                "{}{}",
                inputs.join("\n"),
                if input_count > MAX_FIELDS { format!("\nShowing {MAX_FIELDS} of {input_count} inputs") } else { String::new() }
            )
        };
        let details = vec![
            fact("Declaration", &format!("{}:{}:{}", path, bean.range.start.line + 1, bean.range.start.character + 1)),
            fact("Aliases", &if bean.aliases.is_empty() { "None recorded".into() } else { bean.aliases.join(", ") }),
            fact("Qualifiers", &if bean.qualifiers.is_empty() { "None recorded".into() } else { bean.qualifiers.join(", ") }),
            fact("Primary", if bean.primary { "Declared primary" } else { "No primary flag recorded" }),
            fact("Conditional", &match world.inactive_reason(path, bean) {
                Some(reason) => format!("inactive (condition): {reason}"),
                None if bean.conditional => "Conditional annotation recorded".into(),
                None => "No conditional annotation recorded".into(),
            }),
            fact("Abstract template", if bean.is_abstract { "Yes" } else { "No" }),
            fact("Declared wiring inputs", &wiring),
            fact(
                "Model provenance",
                if index.complete() {
                    "Cached project source declarations; effective runtime context and library types are not resolved"
                } else {
                    "Partial cached project sources; discovery is still in progress; absence is not proof of a missing bean"
                },
            ),
        ];
        let profiles = if world.inactive_reason(path, bean).is_some() { format!("{profiles} (inactive)") } else { profiles };
        let cells = [&bean.name, &bean.ty, &bean.kind, &profiles, path].into_iter().map(|s| clip(s)).collect::<Vec<_>>();
        let icon = match bean.kind.as_str() {
            "Spring Data repository" => "interface",
            "@Bean" => "method",
            _ => "class",
        };
        rows.push(json!({"key":location_key(path,bean.range),"label":clip(&bean.name),"cells":cells,"icon":icon,"details":details}));
    }
    let mut data = json!({"columns":["Name","Type","Kind","Profiles","Source"],"facets":[3,4],"groupBy":2,"displayColumns":[0,1],"showRowIcons":true,"items":[],"totalRows":total});
    let rows = fit(&data, rows, MAX_BODY);
    let shown = rows.len();
    data["items"] = Value::Array(rows);
    let mut status = format!("{total} source beans · {shown} loaded · source model, not runtime state");
    if shown < total {
        status.push_str(" · presentation capped");
    }
    if !index.complete() {
        status.push_str(" · partial source index · ");
        status.push_str(&index.state());
    }
    View::new(Body::Other { kind: "resources".into(), data: data.to_string() })
        .button(Button::new("refresh", "Refresh").icon("refresh"))
        .status(&status)
}

/// The leading `rows` that fit in `max` bytes of JSON inside `envelope`'s empty
/// `items`; each row is serialized once.
fn fit(envelope: &Value, rows: Vec<Value>, max: usize) -> Vec<Value> {
    let mut size = envelope.to_string().len();
    let mut kept = Vec::new();
    for row in rows {
        let added = row.to_string().len() + usize::from(!kept.is_empty());
        if size.saturating_add(added) > max {
            break;
        }
        size += added;
        kept.push(row);
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::Indexes;
    use openidea_plugin::mock::MockHost;
    use spring_text::index::COMMAND_BUDGET;
    fn data(view: View) -> Value {
        let Body::Other { kind, data } = view.body else { panic!("resources") };
        assert_eq!(kind, "resources");
        serde_json::from_str(&data).unwrap()
    }
    #[test]
    fn bean_rows_expose_source_identity_and_actual_metadata() {
        let host = MockHost::install();
        let source = "@org.springframework.stereotype.Service(\"orders\") @org.springframework.context.annotation.Profile(\"dev\") class Orders { @org.springframework.beans.factory.annotation.Autowired Repo repo; }";
        host.add_file("src/Orders.java", source);
        let mut indexes = Indexes::default();
        let project = host.invocation().project().unwrap();
        let model = data(view(indexes.get(&project, COMMAND_BUDGET), "No Spring beans found."));
        let row = &model["items"][0];
        assert_eq!(row["cells"][0], "orders");
        assert_eq!(row["cells"][1], "Orders");
        assert_eq!(row["cells"][3], "dev");
        assert!(row["key"].as_str().unwrap().starts_with("b:"));
        let details = row["details"].as_array().unwrap();
        assert!(details.iter().any(|f| f["label"] == "Declared wiring inputs" && f["value"].as_str().unwrap().contains("repo: Repo")));
        assert!(details.iter().any(|f| f["value"].as_str().unwrap().contains("effective runtime context")));
    }
    #[test]
    fn cold_source_rows_do_not_claim_absence_or_runtime_resolution() {
        let host = MockHost::install();
        let mut indexes = Indexes::default();
        let index = indexes.for_cached_document("/project/src/C.java", "@org.springframework.stereotype.Component class C {}").unwrap();
        let shown = view(index, "No Spring beans found.");
        assert!(shown.status.as_deref().unwrap().contains("partial source index"));
        assert!(
            data(shown)["items"][0]["details"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f["value"].as_str().unwrap().contains("absence is not proof"))
        );
        assert!(host.process_runs().is_empty());
    }
    #[test]
    fn resources_payload_truncated_under_max() {
        let rows: Vec<Value> = (0..50).map(|n| json!({"key": n, "label": "x".repeat(90)})).collect();
        let envelope = json!({"columns": ["Name"], "items": []});
        let kept = fit(&envelope, rows.clone(), 1000);
        let size = |items: &[Value]| {
            let mut data = envelope.clone();
            data["items"] = Value::Array(items.to_vec());
            data.to_string().len()
        };
        assert!(!kept.is_empty() && kept.len() < rows.len());
        assert!(size(&kept) <= 1000);
        assert!(size(&rows[..kept.len() + 1]) > 1000, "as many rows as fit");
        assert_eq!(fit(&envelope, rows.clone(), usize::MAX).len(), rows.len());
    }
}
