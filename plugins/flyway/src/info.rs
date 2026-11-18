//! `flyway info -outputType=json`: one row per migration with its state.

use serde_json::Value;

use crate::naming::Kind;

pub struct Migration {
    pub label: String,
    pub state: &'static str,
    pub icon: &'static str,
    pub kind: Kind,
    pub version: String,
    pub description: String,
    pub filepath: Option<String>,
    /// `YYYY-MM-DD` from `installedOnUTC` / `installedOn`, when Flyway printed one.
    pub installed_on: Option<String>,
}

impl Migration {
    /// A short project-tree note for the migration's file; `None` when the state is unknown.
    pub fn note(&self) -> Option<String> {
        match self.state {
            "Applied" => Some(self.installed_on.as_ref().map_or("applied".into(), |date| format!("applied {date}"))),
            "Superseded" => Some("outdated".into()),
            "Unknown" => None,
            state => Some(state.to_lowercase()),
        }
    }
}

fn state(raw: &str) -> (&'static str, &'static str) {
    let raw = raw.to_lowercase();
    match raw.as_str() {
        "success" | "baseline" => ("Applied", "success"),
        "pending" => ("Pending", "ci-pending"),
        "ignored" => ("Ignored", "ci-skipped"),
        "baseline ignored" => ("Baseline ignored", "ci-skipped"),
        "available" => ("Available", "history"),
        "deleted" => ("Deleted", "trash"),
        "undone" => ("Undone", "history"),
        "outdated" | "superseded" => ("Superseded", "history"),
        _ if raw.contains("missing") => ("Missing", "warning"),
        _ if raw.contains("failed") => ("Failed", "error"),
        _ if raw.contains("out of order") => ("Out of order", "warning"),
        _ if raw.contains("above target") || raw.contains("below baseline") || raw.contains("future") => ("Not applicable", "clock"),
        _ => ("Unknown", "info"),
    }
}

/// Flyway 10 and 11 print `{"migrations": [...]}` with more fields around it; `mvn` wraps it in log lines.
pub fn parse(stdout: &str) -> Result<Vec<Migration>, String> {
    const INVALID: &str = "Flyway info requires one complete, unambiguous migrations JSON object; no result was accepted.";
    if stdout.len()>1<<20 {return Err(INVALID.into());}
    let mut value=None;let mut at=0;let mut attempts=0;
    while let Some(relative)=stdout[at..].find('{') {
        at+=relative;attempts+=1;
        if attempts>256 {return Err(INVALID.into());}
        let mut stream=serde_json::Deserializer::from_str(&stdout[at..]).into_iter::<Value>();
        if let Some(Ok(candidate))=stream.next() {
            let consumed=stream.byte_offset();
            if candidate.get("migrations").is_some() {
                if value.is_some() {return Err(INVALID.into());}
                let crate::comparison::Unique(candidate)=serde_json::from_str::<crate::comparison::Unique>(&stdout[at..at+consumed]).map_err(|_|INVALID.to_owned())?;
                if candidate.get("error").is_some_and(|error|!error.is_null()) {return Err(INVALID.into());}
                value=Some(candidate);
            }
            at+=consumed;
        } else {at+=1;}
    }
    let value=value.ok_or_else(||"Flyway info did not return JSON. Check the arguments setting and the Flyway version (10 or newer).".to_owned())?;
    let rows = value.get("migrations").and_then(Value::as_array).ok_or("Flyway info returned no migrations list.")?;

    if rows.len()>10_000 || rows.iter().any(|row|!row.is_object()) {return Err(INVALID.into());}
    let text = |row: &Value, key: &str| row.get(key).and_then(Value::as_str).unwrap_or_default().to_owned();
    Ok(rows
        .iter()
        .map(|row| {
            let (kind, prefix) = match text(row, "category").to_lowercase().as_str() {
                "repeatable" => (Kind::Repeatable, "R"),
                "undo" => (Kind::Undo, "U"),
                "baseline" => (Kind::Baseline, "B"),
                _ => (Kind::Versioned, "V"),
            };
            let version = text(row, "version");
            let description = text(row, "description");
            let (state, icon) = state(&text(row, "state"));
            Migration {
                label: format!("{prefix}{version} {description}").trim_end().to_owned(),
                state,
                icon,
                kind,
                version,
                description,
                filepath: Some(text(row, "filepath")).filter(|path| !path.is_empty()),
                installed_on: ["installedOnUTC", "installedOn"].iter().find_map(|key| {
                    let date = text(row, key).get(..10)?.to_owned();
                    let bytes = date.as_bytes();
                    (bytes.iter().enumerate().all(|(i, b)| if i == 4 || i == 7 { *b == b'-' } else { b.is_ascii_digit() })).then_some(date)
                }),
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    pub const HOST_ICONS: [&str; 9] = ["success", "ci-pending", "error", "warning", "ci-skipped", "trash", "history", "clock", "info"];

    #[test]
    fn states_map_to_labels_and_host_icons() {
        let cases = [
            ("Success", "Applied", "success"),
            ("Baseline", "Applied", "success"),
            ("Pending", "Pending", "ci-pending"),
            ("Failed", "Failed", "error"),
            ("Future Failed", "Failed", "error"),
            ("Missing (Success)", "Missing", "warning"),
            ("Missing (Failed)", "Missing", "warning"),
            ("Out of Order", "Out of order", "warning"),
            ("Ignored", "Ignored", "ci-skipped"),
            ("Undone", "Undone", "history"),
            ("Superseded", "Superseded", "history"),
            ("Deleted", "Deleted", "trash"),
            ("Above Target", "Not applicable", "clock"),
            ("Whatever", "Unknown", "info"),
        ];
        for (raw, label, icon) in cases {
            assert_eq!(state(raw), (label, icon), "{raw}");
            assert!(HOST_ICONS.contains(&icon), "{icon} is not a host icon");
        }
    }

    #[test]
    fn info_state_available_and_baseline_ignored() {
        assert_eq!(state("Available"), ("Available", "history"));
        assert_eq!(state("Baseline Ignored"), ("Baseline ignored", "ci-skipped"));
    }

    #[test]
    fn noisy_stdout_with_braces_still_parses() {
        let out = "[INFO] banner {not json} here\n{\"migrations\":[{\"category\":\"Undo\",\"version\":\"2\",\"description\":\"x\",\"state\":\"Pending\"}]}\n[INFO] done }";
        let migrations = parse(out).unwrap();
        assert_eq!(migrations[0].label, "U2 x");
        assert_eq!(migrations[0].kind, Kind::Undo);
    }

    #[test]
    fn notes_show_state_and_install_date() {
        let out = r#"{"migrations":[
            {"version":"1","description":"a","state":"Success","installedOnUTC":"2026-11-10 08:00:00.000"},
            {"version":"2","description":"b","state":"Success","installedOn":"bogus"},
            {"version":"3","description":"c","state":"Pending"},
            {"category":"Repeatable","description":"v","state":"Outdated"},
            {"version":"4","description":"d","state":"Failed"},
            {"version":"5","description":"e","state":"Whatever"}]}"#;
        let notes: Vec<_> = parse(out).unwrap().iter().map(Migration::note).collect();
        let expected = [Some("applied 2026-11-10"), Some("applied"), Some("pending"), Some("outdated"), Some("failed"), None];
        assert_eq!(notes, expected.map(|note| note.map(str::to_owned)));
    }
}
