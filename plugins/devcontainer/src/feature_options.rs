//! Optional cached Boolean/enum/string form. Metadata is not a registry descriptor or
//! runtime verification; creation only serializes explicitly selected values.
use openidea_plugin::{Error, PickItem, host};
use serde_json::{Map, Value};
pub(crate) const CACHE_KEY: &str = "feature-options-v1";
const MAX_CACHE: usize = 512 * 1024;
const MAX_BODY: usize = 3 * 1024 * 1024;

fn supported(raw: &Value) -> Map<String, Value> {
    let mut out = Map::new();
    let Some(options) = raw.as_object() else {
        return out;
    };
    for (id, description) in options.iter().take(64) {
        if id.is_empty()
            || id.len() > 64
            || !id
                .as_bytes()
                .first()
                .is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_')
            || !id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
        {
            continue;
        }
        let Some(ty) = description.get("type").and_then(Value::as_str) else {
            continue;
        };
        let mut option = Map::new();
        option.insert("type".into(), ty.into());
        match ty {
            "boolean" => {
                if description.get("enum").is_some()
                    || description.get("proposals").is_some()
                    || description.get("default").is_some_and(|v| !v.is_boolean())
                {
                    continue;
                }
            }
            "string" => {
                if description.get("enum").is_some() && description.get("proposals").is_some() {continue;}
                let field=if description.get("enum").is_some(){"enum"}else{"proposals"};
                if let Some(raw)=description.get(field) {
                    let Some(values)=raw.as_array() else{continue;};
                    let mut seen=std::collections::BTreeSet::new();
                    if values.is_empty() || values.len()>16 || !values.iter().all(|v|v.as_str().is_some_and(|s|s.len()<=128 && !s.chars().any(char::is_control) && seen.insert(s))){continue;}
                    if field=="enum" && description.get("default").is_some_and(|v|!values.contains(v)){continue;}
                    option.insert(field.into(),values.clone().into());
                }
                if description.get("default").is_some_and(|v|!v.as_str().is_some_and(|s|s.len()<=4096&&!s.contains('\0'))){continue;}
            }
            _ => continue,
        }
        if let Some(default) = description.get("default") {
            option.insert("default".into(), default.clone());
        }
        if let Some(text) = description.get("description").and_then(Value::as_str) {
            option.insert(
                "description".into(),
                text.chars()
                    .take(128)
                    .map(|c| if c.is_control() { ' ' } else { c })
                    .collect::<String>()
                    .into(),
            );
        }
        out.insert(id.clone(), Value::Object(option));
        if out.len() == 8 {
            break;
        }
    }
    out
}

pub(crate) fn cache_from_index(body: &str, index: &str) -> Option<String> {
    if body.len() > MAX_BODY {
        return None;
    }
    let eligible: std::collections::BTreeSet<_> = crate::features::creation_entries(index)
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    let value: Value = serde_json::from_str(body).ok()?;
    let mut features = Map::new();
    let mut examined = 0;
    for collection in value.get("collections")?.as_array()?.iter().take(512) {
        for feature in collection
            .get("features")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            examined += 1;
            if examined > 4096 {
                break;
            }
            let (Some(id), Some(major)) = (
                feature.get("id").and_then(Value::as_str),
                feature.get("majorVersion").and_then(Value::as_str),
            ) else {
                continue;
            };
            let reference = format!("{id}:{major}");
            if !eligible.contains(&reference) || features.contains_key(&reference) {
                continue;
            }
            let options = supported(feature.get("options").unwrap_or(&Value::Null));
            if !options.is_empty() {
                features.insert(reference, options.into());
            }
        }
        if examined > 4096 {
            break;
        }
    }
    if features.is_empty() {
        return None;
    }
    let cache = serde_json::json!({"index":index,"features":features}).to_string();
    (cache.len() <= MAX_CACHE).then_some(cache)
}

pub(crate) fn cached(reference: &str) -> Map<String, Value> {
    let Ok(Some(index)) = host::storage::get("feature-index") else {
        return Map::new();
    };
    if index.len() > 900 * 1024 {
        return Map::new();
    }
    let Ok(Some(cache)) = host::storage::get(CACHE_KEY) else {
        return Map::new();
    };
    if cache.len() > MAX_CACHE {
        return Map::new();
    }
    let Ok(value) = serde_json::from_str::<Value>(&cache) else {
        return Map::new();
    };
    if value.get("index").and_then(Value::as_str) != Some(index.as_str()) {
        return Map::new();
    }
    supported(
        value
            .get("features")
            .and_then(|v| v.get(reference))
            .unwrap_or(&Value::Null),
    )
}

pub(crate) fn choose(reference: &str) -> Result<Value, Error> {
    let options = cached(reference);
    let mut selected = Map::new();
    if options.is_empty() {
        return Ok(selected.into());
    }
    let mode=host::pick("Cached feature options — unverified metadata",&[
        PickItem::new("defaults","Use feature defaults (omit options)"),
        PickItem::new("configure","Review cached feature options").description("Boolean, enum and string options are supported. Metadata availability/compatibility is unverified."),
    ])?.ok_or(Error::Cancelled)?;
    match mode.as_str() {
        "defaults" => return Ok(selected.into()),
        "configure" => {}
        _ => return Err(Error::Stale),
    }
    for (id, option) in options {
        let values = if option["type"] == "boolean" {
            vec![Value::Bool(true), Value::Bool(false)]
        } else {
            option.get("enum").or_else(||option.get("proposals")).and_then(Value::as_array).cloned().unwrap_or_default()
        };
        let default = option
            .get("default")
            .map(Value::to_string)
            .unwrap_or_else(|| "not declared in this cache".into());
        let mut items = vec![
            PickItem::new("omit", "Omit — use the feature's own default").description(&format!(
                "Cached declared default: {default}. Current descriptor/runtime not verified."
            )),
        ];
        items.extend(
            values
                .iter()
                .enumerate()
                .map(|(n, value)| PickItem::new(&format!("value-{n}"), &value.to_string())),
        );
        let freeform=option["type"]=="string" && option.get("enum").is_none();
        if freeform{items.push(PickItem::new("custom","Enter a string value…"));}
        let key = host::pick(&format!("Cached option {id} for {reference}"), &items)?
            .ok_or(Error::Cancelled)?;
        if key == "omit" {
            continue;
        }
        if freeform && key=="custom" {
            let value=host::input(&format!("Value for feature option {id}"),option.get("default").and_then(Value::as_str))?.ok_or(Error::Cancelled)?;
            if value.len()>4096 || value.contains('\0'){return Err("Feature option values must be at most 4096 bytes and contain no NUL.".into());}
            selected.insert(id,Value::String(value));continue;
        }
        let n = key
            .strip_prefix("value-")
            .and_then(|v| v.parse::<usize>().ok())
            .ok_or(Error::Stale)?;
        selected.insert(id, values.get(n).ok_or(Error::Stale)?.clone());
    }
    Ok(selected.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{Plugin, mock::MockHost};
    const REFERENCE: &str = "ghcr.io/acme/features/tool:1";
    const INDEX: &str = "ghcr.io/acme/features/tool:1\tTool\n";
    fn setup() -> MockHost {
        let host = MockHost::install().with_capabilities(&["storage"]);
        host::storage::set("feature-index", INDEX).unwrap();
        host::storage::set(CACHE_KEY,&serde_json::json!({"index":INDEX,"features":{REFERENCE:{"enabled":{"type":"boolean","default":true},"version":{"type":"string","enum":["stable","next"],"default":"stable"}}}}).to_string()).unwrap();
        host
    }
    #[test]
    fn string_proposals_and_custom_values_are_explicit_and_cancelable() {
        let host=setup();
        host::storage::set(CACHE_KEY,&serde_json::json!({"index":INDEX,"features":{REFERENCE:{"version":{"type":"string","proposals":["latest","stable"],"default":"latest"}}}}).to_string()).unwrap();
        for answer in ["configure","custom","my version with spaces"]{host.answer_next_prompt(Some(&[answer]));}
        assert_eq!(choose(REFERENCE).unwrap(),serde_json::json!({"version":"my version with spaces"}));
        for answer in ["configure","value-1"]{host.answer_next_prompt(Some(&[answer]));}
        assert_eq!(choose(REFERENCE).unwrap(),serde_json::json!({"version":"stable"}));
        for answer in ["configure","custom"]{host.answer_next_prompt(Some(&[answer]));}host.answer_next_prompt(None);
        assert!(matches!(choose(REFERENCE),Err(Error::Cancelled)));
        assert!(host.process_runs().is_empty()&&host.requests().is_empty());
    }

    #[test]
    fn public_refresh_retains_metadata_from_same_index_request_without_fetching_descriptors() {
        let host = MockHost::install().with_capabilities(&["project.read", "storage", "net:containers.dev"]);
        host.on_fetch(|request| {assert_eq!(request.url,crate::features::INDEX_URL);Ok(openidea_plugin::HttpResponse{status:200,headers:vec![],body:serde_json::json!({"collections":[{"features":[{"id":"ghcr.io/acme/features/tool","majorVersion":"1","name":"Tool","options":{"enabled":{"type":"boolean","default":true},"version":{"type":"string","enum":["stable","next"],"default":"stable"}}}]}]}).to_string().into_bytes()})});
        crate::Main::default()
            .run_command("refresh-features", &host.invocation(), None)
            .unwrap();
        assert_eq!(cached(REFERENCE).len(), 2);
        assert_eq!(host.requests().len(), 1);
        assert!(host.process_runs().is_empty() && host.terminals().is_empty());
        host.answer_next_prompt(Some(&["defaults"]));
        assert_eq!(choose(REFERENCE).unwrap(), serde_json::json!({}));
        assert_eq!(host.requests().len(), 1);
    }
    #[test]
    fn supported_descriptor_subset_is_bounded_and_type_checked() {
        let raw = serde_json::json!({"bool":{"type":"boolean","default":true,"description":"a\u{1b}b"},"enum":{"type":"string","enum":["one","two"],"default":"one"},"free":{"type":"string","proposals":["latest"]},"badBool":{"type":"boolean","default":"true"},"badDefault":{"type":"string","enum":["a"],"default":"b"},"duplicate":{"type":"string","enum":["a","a"]},"bad id":{"type":"boolean"},"badControl":{"type":"string","enum":["a\n"]}});
        let admitted = supported(&raw);
        assert_eq!(admitted.len(), 3);
        assert_eq!(admitted["bool"]["description"], "a b");
        let mut many = Map::new();
        for n in 0..100 {
            many.insert(format!("option{n}"), serde_json::json!({"type":"boolean"}));
        }
        assert_eq!(supported(&many.into()).len(), 8);
        assert!(cache_from_index(&"x".repeat(MAX_BODY + 1), INDEX).is_none());
        let body=serde_json::json!({"collections":[{"features":[{"id":"ghcr.io/other/features/unselected","majorVersion":"1","options":{"x":{"type":"boolean"}}}]}]}).to_string();
        assert!(cache_from_index(&body, INDEX).is_none());
    }
    #[test]
    fn missing_denied_corrupt_oversize_or_mismatched_cache_preserves_defaults_without_prompts() {
        for cache in [None,Some("invalid".into()),Some("x".repeat(MAX_CACHE+1)),Some(serde_json::json!({"index":"different index","features":{REFERENCE:{"enabled":{"type":"boolean"}}}}).to_string())] {
            let host=MockHost::install().with_capabilities(&["storage"]);host::storage::set("feature-index",INDEX).unwrap();if let Some(cache)=cache {host::storage::set(CACHE_KEY,&cache).unwrap();}
            assert_eq!(choose(REFERENCE).unwrap(),serde_json::json!({}));assert!(host.requests().is_empty() && host.process_runs().is_empty());
        }
        let host = setup();
        let _host = host.with_capabilities(&[]);
        assert_eq!(choose(REFERENCE).unwrap(), serde_json::json!({}));
    }
    #[test]
    fn explicit_omit_defaults_cancel_and_forged_option_keys_are_respected() {
        let host = setup();
        host.answer_next_prompt(Some(&["configure"]));
        host.answer_next_prompt(Some(&["omit"]));
        host.answer_next_prompt(Some(&["omit"]));
        assert_eq!(choose(REFERENCE).unwrap(), serde_json::json!({}));
        for stage in 0..3 {
            let host = setup();
            for (n, answer) in ["configure", "value-0", "value-0"]
                .iter()
                .enumerate()
                .take(stage + 1)
            {
                let values = [*answer];
                host.answer_next_prompt(if n == stage { None } else { Some(&values) });
            }
            assert!(matches!(choose(REFERENCE), Err(Error::Cancelled)));
            assert!(host.requests().is_empty() && host.process_runs().is_empty());
        }
        let host = setup();
        host.answer_next_prompt(Some(&["configure"]));
        host.answer_next_prompt(Some(&["value-999"]));
        assert!(choose(REFERENCE).is_err());
    }
}
