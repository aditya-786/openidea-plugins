//! devcontainer.json is JSONC (comments and trailing commas are legal), so the
//! file is read with a small tolerant scanner instead of a strict JSON parser.

use std::ops::Range;

use openidea_plugin::Severity;

pub const UNKNOWN_PROPERTY: &str = "unknown-property";
pub const INVALID_PROPERTY_VALUE: &str = "invalid-property-value";

pub const COMPOSE_REFERENCE: &str = "compose-reference";
pub const FEATURES_MALFORMED: &str = "features-malformed";
pub const FOLDER_STRUCTURE: &str = "folder-structure";

/// The properties of the Development Containers specification (containers.dev/implementors/json_reference).
const KNOWN: &[&str] = &[
    "image", "build", "dockerFile", "context", "dockerComposeFile", "service", "runServices", "workspaceFolder", "workspaceMount",
    "shutdownAction", "overrideCommand", "name", "forwardPorts", "portsAttributes", "otherPortsAttributes", "appPort", "containerEnv",
    "remoteEnv", "containerUser", "remoteUser", "updateRemoteUserUID", "userEnvProbe", "features", "overrideFeatureInstallOrder",
    "customizations", "mounts", "init", "privileged", "capAdd", "securityOpt", "runArgs", "onCreateCommand", "updateContentCommand",
    "postCreateCommand", "postStartCommand", "postAttachCommand", "waitFor", "initializeCommand", "hostRequirements", "secrets",
];

pub struct Finding {
    pub start: usize,
    pub end: usize,
    pub code: &'static str,
    pub message: String,
    pub severity: Severity,
}

impl Finding {
    pub(crate) fn warning(range: Range<usize>, code: &'static str, message: String) -> Self {
        Self { start: range.start, end: range.end, code, message, severity: Severity::Warning }
    }
}

pub enum Context {
    Key,
    Value { key: String },
}

pub(crate) struct Member {
    pub key: String,
    pub key_range: Range<usize>,
    pub value_range: Range<usize>,
}

/// Entries of the top-level `features` object with their option members (empty unless the value is an object).
pub(crate) fn feature_members(text: &str) -> Vec<(Member, Vec<Member>)> {
    let bytes = text.as_bytes();
    let start = skip_trivia(bytes, if bytes.starts_with(b"\xEF\xBB\xBF") { 3 } else { 0 });
    let Some(features) = members(bytes, start).into_iter().find(|m| m.key == "features" && bytes.get(m.value_range.start) == Some(&b'{')) else {
        return vec![];
    };
    members(bytes, features.value_range.start)
        .into_iter()
        .map(|f| {
            let options = if bytes.get(f.value_range.start) == Some(&b'{') { members(bytes, f.value_range.start) } else { vec![] };
            (f, options)
        })
        .collect()
}

/// OCI references need a lowercase repository path (distribution spec) and a non-empty tag. Quiet for local, URL and legacy `owner/repo/feature@v1` ids.
fn feature_id_problem(id: &str) -> Option<&'static str> {
    if id.starts_with('.') || id.contains("://") || id.contains('$') || !id.contains('/') {
        return None;
    }
    let (name, digest) = match id.split_once('@') {
        Some((name, digest)) if digest.starts_with("sha256:") => (name, true),
        Some(_) => return None,
        None => (id, false),
    };
    let last = name.rsplit('/').next().unwrap_or(name);
    let (path, tag) = match last.split_once(':') {
        Some((_, tag)) if !digest => (&name[..name.len() - tag.len() - 1], Some(tag)),
        _ => (name, None),
    };
    if tag == Some("") {
        return Some("has an empty tag after `:`");
    }
    // a registry host with a port keeps its `:`, so only the path after the first segment is case-checked
    let repository = path.split_once('/').map_or(path, |(_, rest)| rest);
    repository.chars().any(|c| c.is_ascii_uppercase()).then_some("has uppercase letters in its repository path (OCI repository names are lowercase)")
}

pub fn in_valid_location(path: &str) -> bool {
    let path = path.replace('\\', "/");
    let mut segments = path.rsplit('/');

    match segments.next() {
        Some(".devcontainer.json") => true,
        Some("devcontainer.json") => segments.take(2).any(|folder| folder == ".devcontainer"),
        _ => false,
    }
}

pub fn folder_structure(path: &str) -> Option<Finding> {
    if !path.replace('\\', "/").ends_with("/devcontainer.json") && path != "devcontainer.json" || in_valid_location(path) {
        return None;
    }

    Some(Finding {
        start: 0,
        end: 0,
        code: FOLDER_STRUCTURE,
        message: "Dev Container tools only read devcontainer.json from .devcontainer/, from one folder below it (.devcontainer/<name>/), or as .devcontainer.json at the project root.".into(),
        severity: Severity::Info,
    })
}

pub fn first_token(text: &str) -> Range<usize> {
    let bytes = text.as_bytes();
    let start = skip_trivia(bytes, if bytes.starts_with(b"\xEF\xBB\xBF") { 3 } else { 0 });
    let width = text[start..].chars().next().map_or(0, char::len_utf8);

    start..start + width
}

const SHUTDOWN_ACTIONS: &[&str] = &["none", "stopContainer", "stopCompose"];
const ENV_PROBES: &[&str] = &["none", "loginShell", "loginInteractiveShell", "interactiveShell"];
const WAIT_FOR: &[&str] =
    &["initializeCommand", "onCreateCommand", "updateContentCommand", "postCreateCommand", "postStartCommand", "postAttachCommand"];

/// Each value with whether it is written as a JSON string.
pub fn value_choices(key: &str) -> Vec<(&'static str, bool)> {
    let strings: &[&'static str] = match key {
        "shutdownAction" => SHUTDOWN_ACTIONS,
        "userEnvProbe" => ENV_PROBES,
        "waitFor" => WAIT_FOR,
        "updateRemoteUserUID" | "overrideCommand" | "init" | "privileged" => return vec![("true", false), ("false", false)],
        _ => &[],
    };

    strings.iter().map(|value| (*value, true)).collect()
}

/// The properties not yet present at the top level (the one being typed at `offset` does not count as present).
pub fn missing_properties(text: &str, offset: usize) -> Vec<&'static str> {
    let bytes = text.as_bytes();
    let start = skip_trivia(bytes, if bytes.starts_with(b"\xEF\xBB\xBF") { 3 } else { 0 });
    let present: Vec<String> = members(bytes, start)
        .into_iter()
        .filter(|m| !(m.key_range.start < offset && offset <= m.key_range.end))
        .map(|m| m.key)
        .collect();

    KNOWN.iter().copied().filter(|name| !present.iter().any(|key| key == name)).collect()
}

/// Schema-backed completion in known nested objects. Arbitrary extension/env keys
/// remain quiet. Reuse the JSONC scanner so comments and unfinished values obey
/// the same caret rules as top-level completion.
pub fn nested_choices(text: &str, offset: usize) -> Option<(bool, Vec<(&'static str, bool)>)> {
    let bytes = text.as_bytes();
    let root = first_token(text).start;
    let owner = members(bytes, root).into_iter().find(|m|
        bytes.get(m.value_range.start) == Some(&b'{') && m.value_range.start < offset && offset <= m.value_range.end)?;
    let mut start = owner.value_range.start;
    let properties: &[&str] = match owner.key.as_str() {
        "build" => &["dockerfile", "context", "args", "target", "cacheFrom", "options"],
        "hostRequirements" => &["cpus", "memory", "storage", "gpu"],
        "otherPortsAttributes" => PORT_ATTRIBUTES,
        "portsAttributes" => {
            let port = members(bytes, start).into_iter().find(|m|
                bytes.get(m.value_range.start) == Some(&b'{') && m.value_range.start < offset && offset <= m.value_range.end)?;
            start = port.value_range.start;
            PORT_ATTRIBUTES
        }
        _ => return None,
    };
    match context_at(&text[start..], offset - start)? {
        Context::Key => {
            let present = members(bytes, start);
            Some((true, properties.iter().copied().filter(|name|
                !present.iter().any(|m| m.key == *name && !(m.key_range.start < offset && offset <= m.key_range.end)))
                .map(|name| (name, false)).collect()))
        }
        Context::Value { key } => {
            let values: &[(&str, bool)] = if properties == PORT_ATTRIBUTES {
                match key.as_str() {
                    "protocol" => &[("http", true), ("https", true)],
                    "onAutoForward" => &[("notify", true), ("openBrowser", true), ("openBrowserOnce", true), ("openPreview", true), ("silent", true), ("ignore", true)],
                    "requireLocalPort" | "elevateIfNeeded" => &[("true", false), ("false", false)],
                    _ => &[],
                }
            } else { &[] };
            Some((false, values.to_vec()))
        }
    }
}

/// Known feature option object and JSONC-aware context at the caret.
pub fn feature_option_context(text: &str, offset: usize) -> Option<(String, Context, Vec<String>)> {
    let bytes = text.as_bytes();
    let root = first_token(text).start;
    let features = members(bytes, root).into_iter().find(|m| m.key == "features" && bytes.get(m.value_range.start)==Some(&b'{'))?;
    let feature = members(bytes, features.value_range.start).into_iter().find(|m|
        bytes.get(m.value_range.start)==Some(&b'{') && m.value_range.start < offset && offset <= m.value_range.end)?;
    let present = members(bytes,feature.value_range.start).into_iter()
        .filter(|m| !(m.key_range.start < offset && offset <= m.key_range.end)).map(|m|m.key).collect();
    Some((feature.key, context_at(&text[feature.value_range.start..],offset-feature.value_range.start)?, present))
}

const PORT_ATTRIBUTES: &[&str] = &["label", "protocol", "onAutoForward", "requireLocalPort", "elevateIfNeeded"];

/// The string (quotes included) holding `offset` when it is a key of the top-level `features` object.
pub fn feature_key_at(text: &str, offset: usize) -> Option<Range<usize>> {
    let bytes = text.as_bytes();
    let bom = if bytes.starts_with(b"\xEF\xBB\xBF") { 3 } else { 0 };
    let features = members(bytes, skip_trivia(bytes, bom))
        .into_iter()
        .find(|m| m.key == "features" && bytes.get(m.value_range.start) == Some(&b'{') && m.value_range.start < offset && offset <= m.value_range.end)?;

    let line_start = text.get(..offset)?.rfind('\n').map_or(0, |i| i + 1);
    let before = &text[line_start..offset];
    if before.matches('"').count() % 2 == 0 {
        return None;
    }
    let quote = line_start + before.rfind('"')?;

    let after_opening_brace = quote == skip_trivia(bytes, features.value_range.start + 1);
    let after_comma = members(bytes, features.value_range.start).iter().any(|m| {
        let comma = skip_trivia(bytes, m.value_range.end);
        bytes.get(comma) == Some(&b',') && skip_trivia(bytes, comma + 1) == quote
    });
    let at_key = after_opening_brace || after_comma;
    if !at_key {
        return None;
    }

    let end = match text[offset..].find(['"', '\n']) {
        Some(i) if text.as_bytes()[offset + i] == b'"' => offset + i + 1,
        _ => offset,
    };

    Some(quote..end)
}

/// The word (with its quotes) around `offset`, replaced when a completion is accepted.
pub fn word_range(text: &str, offset: usize) -> Range<usize> {
    let bytes = text.as_bytes();
    let part = |b: &u8| b.is_ascii_alphanumeric() || matches!(b, b'"' | b'_' | b'-' | b'.');
    let mut start = offset;
    let mut end = offset;

    while start > 0 && part(&bytes[start - 1]) {
        start -= 1;
    }
    while end < bytes.len() && part(&bytes[end]) {
        end += 1;
    }

    start..end
}

/// Where `offset` sits in the top-level object: a key, or the value of a key.
/// Quiet (`None`) inside comments, nested values and unreadable text.
pub fn context_at(text: &str, offset: usize) -> Option<Context> {
    let bytes = text.as_bytes();
    if offset > bytes.len() {
        return None;
    }

    let bom = if bytes.starts_with(b"\xEF\xBB\xBF") { 3 } else { 0 };
    let open = trivia_outside_comment(bytes, bom, offset)?;
    if bytes.get(open) != Some(&b'{') || offset <= open {
        return None;
    }

    let mut i = open + 1;
    loop {
        let j = trivia_outside_comment(bytes, i, offset)?;
        let quoted = bytes.get(j) == Some(&b'"');

        if offset < j || (offset == j && !quoted) {
            return Some(Context::Key);
        }
        if !quoted {
            return None;
        }

        let key_end = string_end(bytes, j);
        if offset <= key_end && (offset < key_end || bytes.get(key_end - 1) != Some(&b'"') || key_end - j < 2) {
            return Some(Context::Key);
        }

        let key = String::from_utf8_lossy(&bytes[j + 1..key_end.saturating_sub(1).max(j + 1)]).into_owned();
        let colon = trivia_outside_comment(bytes, key_end, offset)?;
        if offset <= colon || bytes.get(colon) != Some(&b':') {
            return None;
        }

        let value = trivia_outside_comment(bytes, colon + 1, offset)?;
        if offset <= value {
            return Some(Context::Value { key });
        }

        let value_stop = match bytes.get(value) {
            None | Some(b',' | b'}' | b']') => value,
            Some(_) => value_end(bytes, value),
        };
        if offset <= value_stop {
            return match bytes[value] {
                b'{' | b'[' => None,
                _ => Some(Context::Value { key }),
            };
        }

        let after = trivia_outside_comment(bytes, value_stop, offset)?;
        if offset <= after {
            return None;
        }

        i = after + usize::from(bytes.get(after) == Some(&b','));
    }
}

/// Like `skip_trivia`, but `None` when `offset` lies inside one of the comments skipped.
fn trivia_outside_comment(bytes: &[u8], mut at: usize, offset: usize) -> Option<usize> {
    loop {
        let next = skip_one_trivia(bytes, at);
        if next == at {
            return Some(at);
        }

        let inside = match (bytes[at], bytes.get(at + 1)) {
            (b'/', Some(b'/')) => at < offset && offset <= next,
            (b'/', Some(b'*')) => at < offset && (offset < next || !bytes[..next].ends_with(b"*/")),
            _ => false,
        };
        if inside {
            return None;
        }

        at = next;
    }
}

fn skip_one_trivia(bytes: &[u8], at: usize) -> usize {
    match bytes.get(at) {
        Some(b' ' | b'\t' | b'\r' | b'\n') => at + 1,
        Some(b'/') if matches!(bytes.get(at + 1), Some(b'/' | b'*')) => skip_trivia_comment(bytes, at),
        _ => at,
    }
}

fn skip_trivia_comment(bytes: &[u8], at: usize) -> usize {
    let mut i = at;
    if bytes[at + 1] == b'/' {
        while i < bytes.len() && bytes[i] != b'\n' {
            i += 1;
        }
        return i;
    }

    i += 2;
    while i < bytes.len() && !bytes[i..].starts_with(b"*/") {
        i += 1;
    }

    (i + 2).min(bytes.len())
}

pub fn check(text: &str) -> Vec<Finding> {
    let mut findings = vec![];
    let bytes = text.as_bytes();
    let bom = if bytes.starts_with(b"\xEF\xBB\xBF") { 3 } else { 0 };
    let start = skip_trivia(bytes, bom);

    for member in members(bytes, start) {
        if member.key == "features" {
            check_features(text, &member, &mut findings);
            continue;
        }

        check_property_value(text,&member,&mut findings);
        if let Some(known) = typo_of(&member.key) {
            findings.push(Finding::warning(
                member.key_range.clone(),
                UNKNOWN_PROPERTY,
                format!("Unknown property `{}`; did you mean `{known}`?", member.key),
            ));
        }
    }

    findings
}

fn check_property_value(text:&str,member:&Member,findings:&mut Vec<Finding>) {
    let raw=&text[member.value_range.clone()];
    if let Ok(value)=serde_json::from_str::<serde_json::Value>(raw) {
        let choices=value_choices(&member.key);
        if !choices.is_empty() {
            let valid=choices.iter().any(|(choice,quoted)|if *quoted{value.as_str()==Some(*choice)}else{value.as_bool()==choice.parse::<bool>().ok()});
            if !valid {findings.push(Finding::warning(member.value_range.clone(),INVALID_PROPERTY_VALUE,format!("Invalid value for `{}`; choose {}.",member.key,choices.iter().map(|(v,_)|*v).collect::<Vec<_>>().join(", "))));}
            return;
        }
        let kind=match member.key.as_str() {
            "image"|"dockerFile"|"context"|"service"|"workspaceFolder"|"workspaceMount"|"name"|"containerUser"|"remoteUser"=>Some((value.is_string(),"string")),
            "build"|"containerEnv"|"remoteEnv"|"customizations"|"portsAttributes"|"otherPortsAttributes"|"hostRequirements"|"secrets"=>Some((value.is_object(),"object")),
            "runServices"|"forwardPorts"|"overrideFeatureInstallOrder"|"mounts"|"capAdd"|"securityOpt"|"runArgs"=>Some((value.is_array(),"array")),
            "dockerComposeFile"=>Some((value.is_string()||value.is_array(),"string or array")),
            "initializeCommand"|"onCreateCommand"|"updateContentCommand"|"postCreateCommand"|"postStartCommand"|"postAttachCommand"=>Some((value.is_string()||value.is_array()||value.is_object(),"string, array or object")),
            _=>None,
        };
        if let Some((false,expected))=kind {findings.push(Finding::warning(member.value_range.clone(),INVALID_PROPERTY_VALUE,format!("`{}` expects a {expected}.",member.key)));}
    }
    if text.as_bytes().get(member.value_range.start)!=Some(&b'{'){return;}
    let children=members(text.as_bytes(),member.value_range.start);
    if member.key=="portsAttributes" {
        for child in children {
            if text.as_bytes().get(child.value_range.start)==Some(&b'{'){check_port_values(text,child.value_range.start,findings);}
        }
    }else if member.key=="otherPortsAttributes" {check_port_values(text,member.value_range.start,findings);}
    else if member.key=="hostRequirements" {check_host_requirements(text,member.value_range.start,findings);}
}

/// Types from the devcontainers/spec schema: `cpus` integer >= 1, `memory`/`storage` `^\d+([tgmk]b)?$`, `gpu` bool, "optional" or {cores, memory}.
fn check_host_requirements(text:&str,start:usize,findings:&mut Vec<Finding>) {
    let size=|v:&serde_json::Value|v.as_str().is_some_and(|s|{let digits=s.trim_end_matches(|c:char|c.is_ascii_alphabetic());!digits.is_empty()&&digits.bytes().all(|b|b.is_ascii_digit())&&matches!(&s[digits.len()..],""|"tb"|"gb"|"mb"|"kb")});
    let count=|v:&serde_json::Value|v.as_u64().is_some_and(|n|n>=1);
    for member in members(text.as_bytes(),start) {
        let Ok(value)=serde_json::from_str::<serde_json::Value>(&text[member.value_range.clone()])else{continue;};
        let (valid,expected)=match member.key.as_str() {
            "cpus"=>(count(&value),"an integer of at least 1"),
            "memory"|"storage"=>(size(&value),"a size such as 8gb"),
            "gpu"=>(value.is_boolean()||value.as_str()==Some("optional")||value.as_object().is_some_and(|o|o.iter().all(|(k,v)|match k.as_str(){"cores"=>count(v),"memory"=>size(v),_=>false})),"true, false, \"optional\" or an object with cores and memory"),
            _=>continue,
        };
        if !valid {findings.push(Finding::warning(member.value_range.clone(),INVALID_PROPERTY_VALUE,format!("`hostRequirements.{}` expects {expected}.",member.key)));}
    }
}

/// Literal `dockerComposeFile` entries and `service`, each with its source range. Dynamic (`${...}`) or malformed values are left out.
pub fn compose_references(text:&str)->(Vec<(String,Range<usize>)>,Option<(String,Range<usize>)>) {
    let bytes=text.as_bytes();
    let start=skip_trivia(bytes,if bytes.starts_with(b"\xEF\xBB\xBF"){3}else{0});
    let (mut files,mut service)=(vec![],None);
    for member in members(bytes,start) {
        let Ok(value)=serde_json::from_str::<serde_json::Value>(&text[member.value_range.clone()])else{continue;};
        let literal=|v:&serde_json::Value|v.as_str().filter(|s|!s.is_empty()&&!s.contains('$')).map(str::to_owned);
        match member.key.as_str() {
            "dockerComposeFile"=>{
                let items=match &value{serde_json::Value::Array(a)=>a.iter().map(literal).collect::<Option<Vec<_>>>(),v=>literal(v).map(|s|vec![s])};
                if let Some(items)=items{files=items.into_iter().map(|s|(s,member.value_range.clone())).collect();}
            }
            "service"=>service=literal(&value).map(|s|(s,member.value_range.clone())),
            _=>{}
        }
    }
    (files,service)
}
/// Literal `dockerComposeFile` entries, `build.dockerfile` and legacy `dockerFile`, each with its own string's range.
pub fn file_references(text:&str)->Vec<(String,Range<usize>)> {
    let bytes=text.as_bytes();
    let start=skip_trivia(bytes,if bytes.starts_with(b"\xEF\xBB\xBF"){3}else{0});
    let literal=|r:Range<usize>|serde_json::from_str::<String>(&text[r.clone()]).ok().filter(|s|!s.is_empty()&&!s.contains('$')).map(|s|(s,r));
    let mut found=vec![];
    for member in members(bytes,start) {
        match member.key.as_str() {
            "dockerComposeFile" if bytes[member.value_range.start]==b'['=>found.extend(elements(bytes,member.value_range.start).into_iter().filter_map(literal)),
            "dockerComposeFile"|"dockerFile"=>found.extend(literal(member.value_range)),
            "build"=>found.extend(members(bytes,member.value_range.start).into_iter().filter(|m|m.key=="dockerfile").filter_map(|m|literal(m.value_range))),
            _=>{}
        }
    }
    found
}
/// The value ranges of the array starting at `at`; stops quietly at anything malformed.
fn elements(bytes:&[u8],at:usize)->Vec<Range<usize>> {
    let mut found=vec![];
    let mut i=at+1;
    loop {
        i=skip_trivia(bytes,i);
        if i>=bytes.len()||matches!(bytes[i],b']'|b','|b'}'){return found;}
        let end=value_end(bytes,i);
        found.push(i..end);
        i=skip_trivia(bytes,end);
        if bytes.get(i)!=Some(&b','){return found;}
        i+=1;
    }
}
fn check_port_values(text:&str,start:usize,findings:&mut Vec<Finding>) {
    for member in members(text.as_bytes(),start) {
        let Ok(value)=serde_json::from_str::<serde_json::Value>(&text[member.value_range.clone()])else{continue;};
        let valid=match member.key.as_str() {
            "protocol"=>value.as_str().is_some_and(|s|["http","https"].contains(&s)),
            "onAutoForward"=>value.as_str().is_some_and(|s|["notify","openBrowser","openBrowserOnce","openPreview","silent","ignore"].contains(&s)),
            "label"=>value.is_string(),"requireLocalPort"|"elevateIfNeeded"=>value.is_boolean(),_=>true,
        };
        if !valid {findings.push(Finding::warning(member.value_range.clone(),INVALID_PROPERTY_VALUE,format!("Invalid port attribute `{}`.",member.key)));}
    }
}

fn check_features(text: &str, member: &Member, findings: &mut Vec<Finding>) {
    let bytes = text.as_bytes();

    if bytes[member.value_range.start] != b'{' {
        findings.push(Finding::warning(
            member.value_range.clone(),
            FEATURES_MALFORMED,
            "`features` must be an object mapping feature ids to options".into(),
        ));
        return;
    }

    for feature in members(bytes, member.value_range.start) {
        if feature.key.is_empty() || feature.key.chars().any(char::is_whitespace) {
            findings.push(Finding::warning(
                feature.key_range.clone(),
                FEATURES_MALFORMED,
                "A feature id must not be empty or contain whitespace".into(),
            ));
        }

        if let Some(problem) = feature_id_problem(&feature.key) {
            findings.push(Finding::warning(feature.key_range.clone(), FEATURES_MALFORMED, format!("Feature id `{}` {problem}.", feature.key)));
        }

        let value_ok = match bytes[feature.value_range.start] {
            b'{' | b'"' => true,
            b't' | b'f' => matches!(&text[feature.value_range.clone()], "true" | "false"),
            b'n' => true,
            _ => false,
        };
        if !value_ok {
            findings.push(Finding::warning(
                feature.value_range.clone(),
                FEATURES_MALFORMED,
                format!("The options of feature `{}` must be an object, a version string or a boolean", feature.key),
            ));
        }
    }
}

fn typo_of(key: &str) -> Option<&'static str> {
    if key.starts_with("x-") || key.starts_with('$') || KNOWN.contains(&key) {
        return None;
    }

    KNOWN
        .iter()
        .map(|known| (distance(key, known), *known))
        .filter(|(distance, _)| *distance <= 2)
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, known)| known)
}

fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();

    for (i, ca) in a.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (above + 1).min(row[j] + 1).min(diagonal + usize::from(ca != *cb));
            diagonal = above;
        }
    }

    row[b.len()]
}

fn skip_trivia(bytes: &[u8], mut at: usize) -> usize {
    loop {
        match bytes.get(at) {
            Some(b' ' | b'\t' | b'\r' | b'\n') => at += 1,
            Some(b'/') if bytes.get(at + 1) == Some(&b'/') => {
                while at < bytes.len() && bytes[at] != b'\n' {
                    at += 1;
                }
            }
            Some(b'/') if bytes.get(at + 1) == Some(&b'*') => {
                at += 2;
                while at < bytes.len() && !bytes[at..].starts_with(b"*/") {
                    at += 1;
                }
                at = (at + 2).min(bytes.len());
            }
            _ => return at,
        }
    }
}

/// End offset (after the closing quote) of the string starting at `at`.
fn string_end(bytes: &[u8], at: usize) -> usize {
    let mut i = at + 1;

    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'"' => return i + 1,
            b'\n' => return i,
            _ => i += 1,
        }
    }

    bytes.len()
}

/// End offset of the value starting at `at` (a scalar, string, object or array).
fn value_end(bytes: &[u8], at: usize) -> usize {
    match bytes[at] {
        b'"' => string_end(bytes, at),
        b'{' | b'[' => {
            let mut depth = 0;
            let mut i = at;

            while i < bytes.len() {
                match bytes[i] {
                    b'"' => {
                        i = string_end(bytes, i);
                        continue;
                    }
                    b'/' if matches!(bytes.get(i + 1), Some(b'/' | b'*')) => {
                        i = skip_trivia(bytes, i);
                        continue;
                    }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth -= 1;
                        if depth == 0 {
                            return i + 1;
                        }
                    }
                    _ => {}
                }
                i += 1;
            }

            bytes.len()
        }
        _ => {
            let mut i = at;
            while i < bytes.len() && !matches!(bytes[i], b',' | b'}' | b']' | b' ' | b'\t' | b'\r' | b'\n' | b'/') {
                i += 1;
            }
            i.max(at + 1)
        }
    }
}

/// The members of the object starting at `at`; stops quietly at anything malformed.
fn members(bytes: &[u8], at: usize) -> Vec<Member> {
    let mut found = vec![];
    if bytes.get(at) != Some(&b'{') {
        return found;
    }

    let mut i = at + 1;
    loop {
        i = skip_trivia(bytes, i);
        if bytes.get(i) != Some(&b'"') {
            return found;
        }

        let key_end = string_end(bytes, i);
        if key_end < i + 2 || bytes[key_end - 1] != b'"' {
            return found;
        }
        let raw = String::from_utf8_lossy(&bytes[i..key_end]);
        let key = serde_json::from_str::<String>(&raw).unwrap_or_else(|_| raw[1..raw.len() - 1].to_owned());
        let key_range = i..key_end;

        i = skip_trivia(bytes, key_end);
        if bytes.get(i) != Some(&b':') {
            return found;
        }
        i = skip_trivia(bytes, i + 1);
        if i >= bytes.len() || matches!(bytes[i], b',' | b'}' | b']') {
            return found;
        }

        let end = value_end(bytes, i);
        found.push(Member { key, key_range, value_range: i..end });

        i = skip_trivia(bytes, end);
        if bytes.get(i) == Some(&b',') {
            i += 1;
        }
    }
}
