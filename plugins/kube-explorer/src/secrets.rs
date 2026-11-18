//! Secret handling: decoding `data` from `kubectl get secret -o json`, and
//! building the manifest of a new Opaque Secret. Values only ever live in
//! these functions' arguments and results; nothing here logs or stores them.

use openidea_plugin::views::{Body, Button, Field, FieldValue, View};
use openidea_plugin::Error;

use crate::{KUBECTL, KubeExplorer, REQUEST_TIMEOUT, config};

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn b64_encode(bytes: &[u8]) -> String {
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

pub fn b64_decode(text: &str) -> Option<Vec<u8>> {
    // Kubernetes JSON uses padded standard base64. Only CR/LF may be ignored;
    // round-trip identity below rejects misplaced padding and leftover bits.
    let normalized:String=text.chars().filter(|c|!matches!(c,'\r'|'\n')).collect();
    if normalized.len()%4!=0 {return None;}
    let mut out = Vec::new();
    let (mut bits, mut count) = (0u32, 0);
    for c in text.bytes().filter(|c| !c.is_ascii_whitespace() && *c != b'=') {
        bits = bits << 6 | ALPHABET.iter().position(|a| *a == c)? as u32;
        count += 6;
        if count >= 8 {
            count -= 8;
            out.push((bits >> count) as u8);
            bits &= (1 << count) - 1;
        }
    }
    (b64_encode(&out)==normalized).then_some(out)
}

/// Text when the bytes are UTF-8, otherwise their base64.
pub fn display(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_owned(),
        Err(_) => format!("(binary, base64) {}", b64_encode(bytes)),
    }
}

/// Bound and validate the Secret JSON; with `preserve_order` its members keep the order kubectl printed.
/// serde_json's default recursion bound prevents unbounded unknown-field descent.
/// JSON naming no key twice in any object; `serde_json::Value` would silently keep only the last.
struct NoDuplicateKeys;

impl<'de> serde::Deserialize<'de> for NoDuplicateKeys {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(NoDuplicateKeys)
    }
}

impl<'de> serde::de::Visitor<'de> for NoDuplicateKeys {
    type Value = Self;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("JSON without duplicate keys")
    }

    fn visit_bool<E>(self, _: bool) -> Result<Self, E> { Ok(self) }
    fn visit_i64<E>(self, _: i64) -> Result<Self, E> { Ok(self) }
    fn visit_u64<E>(self, _: u64) -> Result<Self, E> { Ok(self) }
    fn visit_f64<E>(self, _: f64) -> Result<Self, E> { Ok(self) }
    fn visit_str<E>(self, _: &str) -> Result<Self, E> { Ok(self) }
    fn visit_unit<E>(self) -> Result<Self, E> { Ok(self) }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self, A::Error> {
        while seq.next_element::<Self>()?.is_some() {}
        Ok(self)
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Self, A::Error> {
        let mut seen = std::collections::HashSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !seen.insert(key) {
                return Err(serde::de::Error::custom("duplicate key"));
            }
            map.next_value::<Self>()?;
        }
        Ok(self)
    }
}

fn validate_object(json:&str)->Result<serde_json::Value,String> {
    if json.len()>2<<20 {return Err("Secret JSON exceeds the 2 MiB read bound".into());}
    let value:serde_json::Value=serde_json::from_str(json).map_err(|_|"Malformed or excessively nested Secret JSON".to_owned())?;
    serde_json::from_str::<NoDuplicateKeys>(json).map_err(|_|"Secret JSON names a key twice".to_owned())?;
    let object=value.as_object().ok_or("The Secret is not a JSON object")?;
    if let Some(data)=object.get("data").filter(|v|!v.is_null()) {
        let data=data.as_object().ok_or("Secret data is not a JSON object")?;
        if data.len()>4096||data.iter().any(|(key,value)|!valid_key(key)||!value.is_string()) {return Err("Secret data has unsupported keys or values".into());}
    }
    Ok(value)
}

pub(crate) fn validate_response(json:&str,name:&str,namespace:Option<&str>,editing:bool)->Result<(),String> {
    let value=validate_object(json)?;
    let actual_namespace=value["metadata"]["namespace"].as_str();
    if value["apiVersion"].as_str()!=Some("v1")||value["kind"].as_str()!=Some("Secret")
        ||value["metadata"]["name"].as_str()!=Some(name)
        ||!actual_namespace.is_some_and(crate::is_dns_label)
        ||namespace.is_some_and(|expected|actual_namespace!=Some(expected)) {
        return Err("Secret response does not match the selected resource and namespace".into());
    }
    if editing&&!value["metadata"]["resourceVersion"].as_str().is_some_and(|version|!version.is_empty()&&!version.chars().any(char::is_control)) {
        return Err("Secret response has no usable resourceVersion replacement guard".into());
    }
    Ok(())
}

/// The decoded top-level `data` of a Secret, in the order kubectl printed it.
pub fn data_entries(json: &str) -> Result<Vec<(String, Vec<u8>)>, String> {
    let value = validate_object(json)?;
    let Some(data) = value["data"].as_object() else {
        return Ok(Vec::new());
    };
    data.iter()
        .map(|(name, encoded)| {
            let bytes = b64_decode(encoded.as_str().unwrap_or_default()).ok_or_else(|| "Secret data contains invalid base64".to_owned())?;
            Ok((name.clone(), bytes))
        })
        .collect()
}

/// A DNS subdomain: what Kubernetes accepts as a Secret name.
pub fn valid_name(name: &str) -> bool {
    let edge = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit();
    name.len() <= 253
        && name.chars().next().is_some_and(edge)
        && name.chars().last().is_some_and(edge)
        && name.chars().all(|c| edge(c) || c == '-' || c == '.')
}

pub fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 253
        && key != "."
        && key != ".."
        && key.chars().all(|c| c.is_ascii_alphanumeric() || "-._".contains(c))
}

/// An Opaque Secret manifest. Names and keys are restricted to characters that
/// need no escaping and values are base64, so any value round-trips.
pub fn manifest(name: &str, entries: &[(String, String)]) -> Result<String, String> {
    if !valid_name(name) {
        return Err("The Secret name must be lowercase letters, digits, '-' and '.', starting and ending with a letter or digit.".into());
    }
    if entries.is_empty() {
        return Err("Add at least one key.".into());
    }
    let mut out = format!("apiVersion: v1\nkind: Secret\nmetadata:\n  name: \"{name}\"\ntype: Opaque\ndata:\n");
    for (i, (key, value)) in entries.iter().enumerate() {
        if !valid_key(key) {
            return Err("A key is invalid: use letters, digits, '-', '_' and '.' only.".into());
        }
        if entries[..i].iter().any(|(k, _)| k == key) {
            return Err(format!("The key \"{key}\" is given twice."));
        }
        out.push_str(&format!("  \"{key}\": {}\n", b64_encode(value.as_bytes())));
    }
    Ok(out)
}

pub(crate) fn command_failure(operation:&str,status:Option<i32>)->Error {
    let status=status.map(|code|format!(" (kubectl status {code})")).unwrap_or_default();
    Error::Failed(format!("Could not {operation} Secret{status}; command diagnostics withheld to protect Secret data."))
}
pub(crate) fn command_error(operation:&str,error:Error)->Error {
    match error {Error::Stale=>Error::Stale,Error::Cancelled=>Error::Cancelled,Error::Denied(_)=>Error::Denied("process:kubectl".into()),_=>command_failure(operation,None)}
}

/// `json` (a whole Secret) with its `data` replaced; everything else, the resourceVersion included, stays.
pub fn replace_data(json: &str, data: &[(String, Vec<u8>)]) -> Result<String, String> {
    let secret = validate_object(json)?;
    let replacement: serde_json::Map<String, serde_json::Value> =
        data.iter().map(|(key, bytes)| (key.clone(), serde_json::Value::String(b64_encode(bytes)))).collect();
    let replacement = serde_json::Value::Object(replacement);
    let mut members = serde_json::Map::new();
    if secret.get("data").is_none() {
        members.insert("data".into(), replacement.clone());
    }
    for (key, value) in secret.as_object().into_iter().flatten() {
        let value = if key == "data" { replacement.clone() } else { value.clone() };
        members.insert(key.clone(), value);
    }
    serde_json::to_string(&members).map_err(|_| "Could not write the Secret".to_owned())
}

const FIELD_PREFIX: &str = "secret-edit:";

/// A Secret open for editing: its object as read, so a save replaces it only if nobody changed it since.
pub struct SecretEdit {
    row: String,
    json: String,
    entries: Vec<(String, String)>,
    original_entries: Vec<(String, String)>,
    binary: Vec<(String, Vec<u8>)>,
    new_key: String,
    new_value: String,
    remove: String,
}

impl SecretEdit {
    pub(crate) fn is_dirty(&self)->bool {
        self.entries!=self.original_entries||!self.new_key.is_empty()||!self.new_value.is_empty()||!self.remove.is_empty()
    }
    pub fn set_field(&mut self, key: &str, value: String) {
        match key {
            "secret-new-key" => self.new_key = value,
            "secret-new-value" => self.new_value = value,
            "secret-remove" => self.remove = value,
            _ => {
                if let Some(entry) = key.strip_prefix(FIELD_PREFIX).and_then(|k| self.entries.iter_mut().find(|(name, _)| name == k)) {
                    entry.1 = value;
                }
            }
        }
    }

    fn data(&self) -> Result<Vec<(String, Vec<u8>)>, String> {
        let mut entries = self.entries.clone();
        if !self.new_key.is_empty() || !self.new_value.is_empty() {
            entries.push((self.new_key.clone(), self.new_value.clone()));
        }
        let removed: Vec<&str> = self.remove.split_whitespace().collect();
        entries.retain(|(key, _)| !removed.contains(&key.as_str()));
        let mut data: Vec<(String, Vec<u8>)> = self.binary.iter().filter(|(key, _)| !removed.contains(&key.as_str())).cloned().collect();
        for (key, value) in entries {
            if !valid_key(&key) {
                return Err("A key is invalid: use letters, digits, '-', '_' and '.' only.".into());
            }
            if data.iter().any(|(existing, _)| *existing == key) {
                return Err(format!("The key \"{key}\" is given twice."));
            }
            data.push((key, value.into_bytes()));
        }
        Ok(data)
    }
}

impl KubeExplorer {
    pub fn edit_secret(&mut self, row: &str) -> Result<(), Error> {
        if !Self::secret_gate("secrets") {
            return Ok(());
        }
        let (namespace, name) = self.split_scope("secrets", row)?;
        let json = self.kubectl_in(namespace.as_deref(), &["get", "secret", &name, "-o", "json", REQUEST_TIMEOUT], None).map_err(|error|command_error("read",error))?;
        validate_response(&json,&name,namespace.as_deref(),true)?;
        let mut entries = Vec::new();
        let mut binary = Vec::new();
        for (key, bytes) in data_entries(&json)? {
            match String::from_utf8(bytes) {
                Ok(text) => entries.push((key, text)),
                Err(error) => binary.push((key, error.into_bytes())),
            }
        }
        self.begin_panel()?;
        self.editing_secret = Some(SecretEdit {
            row: row.to_owned(),
            json,
            original_entries: entries.clone(),
            entries,
            binary,
            new_key: String::new(),
            new_value: String::new(),
            remove: String::new(),
        });
        self.details = None;
        crate::refresh_views();
        Ok(())
    }

    pub fn render_secret_edit(&self, edit: &SecretEdit) -> View {
        let mut fields: Vec<Field> = edit
            .entries
            .iter()
            .map(|(key, value)| Field::new(&format!("{FIELD_PREFIX}{key}"), key, FieldValue::Multiline(value.clone())))
            .collect();
        fields.push(Field::new("secret-new-key", "New key", FieldValue::Text(edit.new_key.clone())));
        fields.push(Field::new("secret-new-value", "New value", FieldValue::Multiline(edit.new_value.clone())));
        fields.push(Field::new("secret-remove", "Keys to remove (separated by spaces)", FieldValue::Text(edit.remove.clone())));
        let binary = match edit.binary.is_empty() {
            true => String::new(),
            false => format!("; binary values kept unchanged: {}", edit.binary.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join(", ")),
        };
        View::new(Body::Form(fields))
            .status(&format!("Editing secret {}{binary}; values are shown here only", edit.row))
            .button(Button::new("back", "Back").icon("arrow-left"))
            .button(Button::new("submit-secret-edit", "Save").icon("check"))
    }

    pub fn save_secret_edit(&mut self) -> Result<(), Error> {
        let Some(edit) = &self.editing_secret else { return Ok(()) };
        if !Self::reveal_allowed() {
            self.editing_secret = None;
            return Ok(());
        }
        let data = edit.data()?;
        let (namespace, _) = self.split_scope("secrets", &edit.row)?;
        let manifest = replace_data(&edit.json, &data)?;
        self.confirm_replace("secrets", &edit.row)?;
        let args = self.args_in(namespace.as_deref(), &["replace", "-f", "-"]);
        let out = self.process(KUBECTL, &args, Some(&manifest)).map_err(|error|command_error("save",error))?;
        if out.status != Some(0) {
            if out.stderr.contains("Conflict") || out.stderr.contains("has been modified") {
                return Err("The secret changed in the cluster since it was opened. Click Back and Edit Secret again; nothing was saved.".into());
            }
            return Err(command_failure("save",out.status));
        }
        let row = edit.row.clone();
        self.editing_secret = None;
        config::notify_success("Secret saved", &row);
        self.reload();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use openidea_plugin::mock::MockHost;
    use openidea_plugin::ProcessOutput;
    use openidea_plugin::views::ViewEvent;

    use super::*;

    const JSON: &str = r#"{"apiVersion":"v1","data":{"user":"YWRtaW4=","blob":"/+8="},"kind":"Secret","metadata":{"name":"db","namespace":"shop","resourceVersion":"41"},"type":"Opaque"}"#;

    type Runs = std::rc::Rc<std::cell::RefCell<Vec<(Vec<String>, Option<String>)>>>;

    fn host(allow: bool, replace_stderr: &'static str) -> (MockHost, Runs) {
        let host = MockHost::install().with_capabilities(&["process:kubectl", "storage"]);
        if allow {
            host.set_setting("allowRevealSecrets", "true");
        }
        let runs: Runs = Default::default();
        let seen = runs.clone();
        host.on_process(KUBECTL, move |args, stdin| {
            seen.borrow_mut().push((args.to_vec(), stdin.map(str::to_owned)));
            match args.iter().find(|a| !a.starts_with("--")).map(String::as_str) {
                Some("get") => Ok(ProcessOutput { status: Some(0), stdout: JSON.into(), stderr: String::new() }),
                Some("replace") if !replace_stderr.is_empty() => {
                    Ok(ProcessOutput { status: Some(1), stdout: String::new(), stderr: replace_stderr.into() })
                }
                _ => Ok(ProcessOutput { status: Some(0), stdout: String::new(), stderr: String::new() }),
            }
        });
        (host, runs)
    }

    fn editing() -> KubeExplorer {
        let mut kube = KubeExplorer { namespace: Some("shop".into()), ..Default::default() };
        kube.edit_secret("db").unwrap();
        kube
    }

    fn type_into(kube: &mut KubeExplorer, key: &str, value: &str) {
        kube.editing_secret.as_mut().unwrap().set_field(key, value.into());
    }

    #[test]
    fn duplicate_data_key_is_rejected() {
        let error = data_entries(r#"{"kind":"Secret","data":{"user":"YQ==","user":"Yg=="}}"#).unwrap_err();
        assert!(error.contains("twice"));
        assert_eq!(data_entries(r#"{"kind":"Secret","data":{"user":"YQ==","pw":"Yg=="}}"#).unwrap().len(), 2);
    }

    #[test]
    fn replace_data_keeps_everything_but_data() {
        let replaced = replace_data(JSON, &[("k".into(), b"v".to_vec())]).unwrap();
        assert_eq!(replaced, JSON.replace(r#"{"user":"YWRtaW4=","blob":"/+8="}"#, r#"{"k":"dg=="}"#));
        let bare = replace_data(r#"{"kind":"Secret"}"#, &[("k".into(), b"v".to_vec())]).unwrap();
        assert_eq!(bare, r#"{"data":{"k":"dg=="},"kind":"Secret"}"#);
        assert!(replace_data("[1]", &[]).is_err());
    }

    #[test]
    fn the_form_is_prefilled_and_binary_values_are_left_out() {
        let (_host, _) = host(true, "");
        let kube = editing();
        let Body::Form(fields) = kube.render_secret_edit(kube.editing_secret.as_ref().unwrap()).body else { panic!() };
        assert_eq!(fields[0].key, "secret-edit:user");
        assert_eq!(fields[0].value, FieldValue::Multiline("admin".into()));
        assert!(fields.iter().all(|f| f.key != "secret-edit:blob"));
    }

    #[test]
    fn save_replaces_the_object_with_its_resource_version_through_stdin() {
        let (_host, runs) = host(true, "");
        let mut kube = editing();
        type_into(&mut kube, "secret-edit:user", "root");
        type_into(&mut kube, "secret-new-key", "token");
        type_into(&mut kube, "secret-new-value", "t0p");
        _host.answer_next_prompt(Some(&[]));
        kube.button("submit-secret-edit").unwrap();
        let (args, stdin) = runs.borrow().last().cloned().unwrap();
        assert_eq!(args, ["--namespace=shop", "replace", "-f", "-"]);
        let manifest = stdin.unwrap();
        assert!(manifest.contains(r#""resourceVersion":"41""#));
        for (key, value) in [("user", "root"), ("token", "t0p")] {
            assert!(manifest.contains(&format!("\"{key}\":\"{}\"", b64_encode(value.as_bytes()))));
        }
        assert!(manifest.contains(r#""blob":"/+8=""#));
        assert!(!args.iter().any(|a| a.contains("t0p") || a.contains("root")));
        assert!(kube.editing_secret.is_none());
    }

    #[test]
    fn declining_the_save_confirm_keeps_the_form_and_runs_nothing() {
        let (host, runs) = host(true, "");
        let mut kube = editing();
        runs.borrow_mut().clear();
        type_into(&mut kube, "secret-edit:user", "root");
        host.answer_next_prompt(None);
        assert!(matches!(kube.button("submit-secret-edit"), Err(Error::Cancelled)));
        assert!(runs.borrow().is_empty());
        assert!(kube.editing_secret.is_some());
    }

    #[test]
    fn removed_keys_are_dropped() {
        let (_host, runs) = host(true, "");
        let mut kube = editing();
        type_into(&mut kube, "secret-remove", "user blob");
        _host.answer_next_prompt(Some(&[]));
        kube.button("submit-secret-edit").unwrap();
        let manifest = runs.borrow().last().unwrap().1.clone().unwrap();
        assert!(manifest.contains(r#""data":{}"#));
    }

    #[test]
    fn a_conflict_keeps_the_form_and_leaks_no_value() {
        let (_host, _) = host(true, "Operation cannot be fulfilled on secrets \"db\": the object has been modified; root");
        let mut kube = editing();
        type_into(&mut kube, "secret-edit:user", "root");
        _host.answer_next_prompt(Some(&[]));
        let error = format!("{:?}", kube.button("submit-secret-edit").unwrap_err());
        assert!(error.contains("changed in the cluster") && !error.contains("root"));
        assert!(kube.editing_secret.is_some());
    }

    #[test]
    fn an_invalid_new_key_is_refused_before_kubectl_runs() {
        let (_host, runs) = host(true, "");
        let mut kube = editing();
        runs.borrow_mut().clear();
        type_into(&mut kube, "secret-new-key", "bad key!");
        type_into(&mut kube, "secret-new-value", "x");
        assert!(kube.button("submit-secret-edit").is_err());
        assert!(runs.borrow().is_empty());
    }

    #[test]
    fn editing_needs_the_reveal_setting_and_back_discards_the_form() {
        let (host, runs) = host(false, "");
        let mut kube = KubeExplorer::default();
        kube.edit_secret("db").unwrap();
        assert!(kube.editing_secret.is_none() && runs.borrow().is_empty());
        assert!(host.notifications()[0].body.contains("Allow Show Secret and YAML on a secret"));

        host.set_setting("allowRevealSecrets", "true");
        let mut kube = editing();
        kube.button("back").unwrap();
        assert!(kube.editing_secret.is_none());
    }

    #[test]
    fn field_events_reach_the_form() {
        let (_host, _) = host(true, "");
        let mut kube = editing();
        kube.handle_tree(ViewEvent::Field("secret-edit:user".into(), "x".into()), &_host.invocation()).unwrap();
        assert_eq!(kube.editing_secret.as_ref().unwrap().entries[0].1, "x");
    }
}
