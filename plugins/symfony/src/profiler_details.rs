//! Explicit local profile adapter. Only allowlisted numeric summaries cross into UI.
use crate::*;
use serde::Deserialize;
const ADAPTER: &str = include_str!("profiler_decode.php");
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Details {
    version: u8,
    token: String,
    collectors: Vec<Collector>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Collector {
    name: String,
    class: String,
    metrics: BTreeMap<String, f64>,
    #[serde(default)]
    events: Vec<TimeEvent>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TimeEvent {
    name: String,
    category: Option<String>,
    origin: Option<f64>,
    duration: Option<f64>,
    periods: Option<u32>,
}
fn printable(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && s.bytes().all(|b| (0x20..=0x7e).contains(&b))
}
impl TimeEvent {
    fn valid(&self) -> bool {
        printable(&self.name, 120)
            && self.category.as_deref().is_none_or(|c| printable(c, 80))
            && [self.origin, self.duration].iter().flatten().all(|n| n.is_finite() && (0.0..=1e15).contains(n))
            && self.periods.is_none_or(|n| n <= 1000)
    }
}
fn parse(text: &str, token: &str) -> Result<Details, Error> {
    if text.len() > 128 << 10 {
        return Err(Error::Invalid("Profile summary exceeds its bound.".into()));
    }
    let parsed: Details = serde_json::from_str(text)
        .map_err(|_| Error::Invalid("Unsupported profile summary.".into()))?;
    if parsed.version != 2
        || parsed.token != token
        || parsed.collectors.len() > 128
        || parsed.collectors.iter().any(|c| {
            c.name.is_empty()
                || c.name.len() > 80
                || !c
                    .name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
                || c.class.is_empty()
                || c.class.len() > 200
                || !c
                    .class
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_\\".contains(&b))
                || c.metrics.len() > 32
                || c.events.len() > 50
                || (!c.events.is_empty() && c.name != "time")
                || c.events.iter().any(|e| !e.valid())
                || c.metrics.iter().any(|(key, n)| {
                    ![
                        "time",
                        "duration",
                        "memory",
                        "memory_limit",
                        "peak_memory",
                        "query_count",
                        "query_time",
                        "error_count",
                        "exception_count",
                        "status_code",
                        "stored_logs",
                        "stored_exceptions",
                        "stored_templates",
                        "stored_events",
                        "start_time",
                        "deprecation_count",
                        "warning_count",
                        "scream_count",
                        "stopwatch_installed",
                        "cache_calls",
                        "cache_time",
                        "cache_reads",
                        "cache_writes",
                        "cache_deletes",
                        "cache_hits",
                        "cache_misses",
                    ]
                    .contains(&key.as_str())
                        || !n.is_finite()
                        || !(0.0..=1e15).contains(n)
                })
        })
    {
        return Err(Error::Invalid("Unsupported profile summary.".into()));
    }
    Ok(parsed)
}
pub fn load(project: &Project, root: &str, source: &str, token: &str) -> Result<Details, Error> {
    if token.is_empty()
        || token.len() > 64
        || !token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
    {
        return Err(Error::Stale);
    }
    let parent = source.strip_suffix("/index.csv").ok_or_else(|| {
        Error::Invalid("Profile details require a Symfony profiler/index.csv source.".into())
    })?;
    let a = token.len().saturating_sub(2);
    let b = token.len().saturating_sub(4);
    let path = format!(
        "{root}/{parent}/{}/{}/{token}",
        &token[a..],
        &token[b..(b + 2).min(token.len())]
    );
    if host::project_of(&path)
        .and_then(|p| p.root().ok())
        .as_deref()
        != Some(root)
    {
        return Err(Error::Stale);
    }
    let stat = project.stat(&path)?;
    if stat.kind != openidea_plugin::FileKind::File || stat.size > 1 << 20 {
        return Err(Error::Invalid(
            "Profile is not a file within the 1 MiB input bound.".into(),
        ));
    }
    let command = host::Command::new("php").args([
        "-n",
        "-d",
        "memory_limit=64M",
        "-d",
        "max_execution_time=2",
        "-r",
        ADAPTER,
        "--",
        root,
        &path,
        token,
    ]);
    let result = host::run_command(&command, Some(project), None)
        .map_err(|e| ui_kit::denied::map(e, "Inspect Profile", "process:php", "Symfony"))?;
    if result.status != Some(0) {
        return Err(Error::Invalid("Couldn't read this profile. Inspect Profile needs PHP 8 or newer and Symfony's standard profiler storage.".into()));
    }
    if project.root()? != root
        || host::project_of(&path)
            .and_then(|p| p.root().ok())
            .as_deref()
            != Some(root)
    {
        return Err(Error::Stale);
    }
    parse(&result.stdout, token)
}
impl Details {
    pub fn view(&self) -> View {
        let mut rows = vec![];
        for c in &self.collectors {
            rows.push(vec![
                c.name.clone(),
                c.class.clone(),
                "Collector".into(),
                "Present".into(),
            ]);
            rows.extend(
                c.metrics.iter().map(|(key, n)| {
                    vec![c.name.clone(), c.class.clone(), key.clone(), n.to_string()]
                }),
            );
            rows.extend(c.events.iter().map(|e| {
                let mut value = vec![];
                value.extend(e.duration.map(|n| format!("{n} ms")));
                value.extend(e.category.clone());
                value.extend(e.origin.map(|n| format!("origin {n}")));
                value.extend(e.periods.map(|n| format!("{n} periods")));
                vec![c.name.clone(), c.class.clone(), format!("event: {}", e.name), value.join(" · ")]
            }));
        }
        View::new(Body::Table(Table { columns: ["Collector", "Type", "Metric", "Value"].map(String::from).to_vec(), total_rows: rows.len() as u32, first_row: 0, rows, editable: false }))
            .button(Button::new("close-profile-details", "Back to Requests").icon("arrow-left"))
            .status(&format!("Profile {} · {} collectors · numbers and Stopwatch event names only; request data and secrets are not shown", self.token, self.collectors.len()))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strict_summary_rejects_identity_secrets_and_unbounded_metrics() {
        let valid = r#"{"version":2,"token":"abcdef","collectors":[{"name":"time","class":"TimeDataCollector","metrics":{"duration":2.5}}]}"#;
        let result = parse(valid, "abcdef").unwrap();
        assert_eq!(result.collectors.len(), 1);
        assert!(parse(valid, "123abc").is_err());
        for value in [
            valid.replace("duration", "password"),
            valid.replace("2.5", "-1"),
            valid.replace("TimeDataCollector", "secret@email"),
            valid.replace("\"version\":2", "\"version\":1"),
            valid.replace("\"version\":2", "\"version\":3"),
            valid.replace("duration", "cache_secret"),
        ] {
            assert!(parse(&value, "abcdef").is_err());
        }
        let view = result.view();
        let Body::Table(t) = view.body else { panic!() };
        assert_eq!(t.rows.len(), 2);
        assert_eq!(t.rows[1][3], "2.5");
    }
    #[test]
    fn time_events_are_bounded_allowlisted_and_rendered_as_rows() {
        let event = r#"{"name":"kernel.request","category":"core","origin":3.5,"duration":12.25,"periods":2}"#;
        let json = |events: &str, name: &str| format!(r#"{{"version":2,"token":"abcdef","collectors":[{{"name":"{name}","class":"TimeDataCollector","metrics":{{}},"events":[{events}]}}]}}"#);
        let result = parse(&json(event, "time"), "abcdef").unwrap();
        let Body::Table(t) = result.view().body else { panic!() };
        assert_eq!(t.rows[1], ["time", "TimeDataCollector", "event: kernel.request", "12.25 ms · core · origin 3.5 · 2 periods"]);
        assert!(parse(&json(r#"{"name":"bare"}"#, "time"), "abcdef").is_ok());
        for bad in [
            event.replace("kernel.request", "bad\\u0000name"),
            event.replace("12.25", "-1"),
            event.replace("\"periods\":2", "\"periods\":5000"),
            event.replace("\"core\"", "\"c\\u00e9\""),
            event.replace("}", ",\"memory\":1}"),
        ] {
            assert!(parse(&json(&bad, "time"), "abcdef").is_err(), "{bad}");
        }
        assert!(parse(&json(event, "cache"), "abcdef").is_err());
        assert!(parse(&json(&vec![event; 51].join(","), "time"), "abcdef").is_err());
        assert!(parse(&json(&vec![event; 50].join(","), "time"), "abcdef").is_ok());
    }
}
