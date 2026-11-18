//! Domain rows and advisory details for the packaged resource page.
//! Uses persisted scan/cache facts only; rendering does not contact OSV.
use crate::{Declaration, Kind, Report, Row, ScanFailure, cached_advisory, ignored_advisories, kind, manifest, now, osv};
use openidea_plugin::views::Body;
use serde_json::{Value, json};

pub(crate) fn evidence(path: &str) -> &'static str {
    match kind(path) {
        Some(Kind::PackageLock | Kind::ComposerLock | Kind::YarnLock | Kind::PnpmLock | Kind::CargoLock | Kind::PoetryLock | Kind::UvLock | Kind::PipfileLock) => {
            "Lockfile entry"
        }
        Some(Kind::PackageJson | Kind::ComposerJson) => "Manifest declaration (uses a sibling lockfile when available)",
        _ => "Manifest declaration",
    }
}

pub(crate) fn ecosystem(row: &Row) -> &str {
    row.ecosystem.as_deref().unwrap_or_else(|| match kind(&row.path) {
        Some(Kind::Pom | Kind::Gradle | Kind::VersionCatalog) => "Maven",
        Some(Kind::PackageJson | Kind::PackageLock | Kind::YarnLock | Kind::PnpmLock) => "npm",
        Some(Kind::ComposerJson | Kind::ComposerLock) => "Packagist",
        Some(Kind::CargoLock) => "crates.io",
        Some(Kind::PoetryLock | Kind::UvLock | Kind::PipfileLock | Kind::Requirements | Kind::Pylock | Kind::Pyproject) => "PyPI",
        _ => "Unknown",
    })
}

fn dependency_key(row: &Row) -> String {
    format!("{}|{} {}", row.path, row.name, row.version)
}

fn age(at: Option<u64>) -> String {
    let Some(at) = at else {
        return "Not recorded (older report)".into();
    };
    let elapsed = now().saturating_sub(at);
    let age = if elapsed < 60 {
        format!("{elapsed}s ago")
    } else if elapsed < 3600 {
        format!("{}m ago", elapsed / 60)
    } else if elapsed < 172_800 {
        format!("{}h ago", elapsed / 3600)
    } else {
        format!("{}d ago", elapsed / 86_400)
    };
    format!("{age} ({})", utc(at))
}

/// `2026-11-17 09:30 UTC` from Unix seconds (civil-from-days, proleptic Gregorian).
pub(crate) fn utc(at: u64) -> String {
    let (days, seconds) = (at / 86_400, at % 86_400);
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year}-{month:02}-{day:02} {:02}:{:02} UTC", seconds / 3600, seconds % 3600 / 60)
}

pub(crate) fn report_status(report: &Report, failure: Option<&ScanFailure>) -> String {
    let mut status = format!("{} vulnerable dependencies · last successful scan: {}", report.rows.len(), age(report.scanned_at));
    if report.discovery_limited {
        status.push_str(" · partial: dependency-file discovery reached its limit");
    }
    if report.files_skipped > 0 {
        status.push_str(&format!(" · partial: {} files skipped", report.files_skipped));
    }
    if let Some(failure) = failure {
        status.push_str(&format!(" · last rescan failed: {} · showing previous report", failure.message));
    }
    status
}

fn row_advisories(row: &Row) -> Vec<osv::Advisory> {
    row.advisories
        .iter()
        .map(|id| {
            row.details
                .iter()
                .find(|a| a.id == *id)
                .cloned()
                .or_else(|| cached_advisory(id))
                .unwrap_or_else(|| osv::Advisory { id: id.clone(), ..osv::Advisory::default() })
        })
        .collect()
}

fn detail_known(advisory: &osv::Advisory) -> bool {
    !advisory.summary.is_empty() || !advisory.fixed.is_empty() || advisory.severity.is_some()
}

fn advisory_fixes(row: &Row, advisory: &osv::Advisory) -> String {
    if advisory.known_malicious() { return "Remove/replace after advisory review; no automatic upgrade".into(); }
    let name = if ecosystem(row) == "PyPI" { manifest::normalise_pypi(&row.name) } else { row.name.clone() };
    let fixes = advisory.fixed.get(&format!("{}:{name}", ecosystem(row))).cloned().unwrap_or_default();
    if !fixes.is_empty() {
        fixes.join(", ")
    } else if detail_known(advisory) {
        "No fixed version reported".into()
    } else {
        "Unknown (details unavailable)".into()
    }
}

const MAX_RESOURCE_ROWS: usize = 5000;
const MAX_RESOURCE_BYTES: usize = 4 << 20;

fn clip_presentation(value: &mut Value, limit: usize) {
    if let Some(text) = value.as_str().filter(|s| s.len() > limit) {
        let end = text.char_indices().map(|(at, _)| at).take_while(|at| *at <= limit).last().unwrap_or(0);
        *value = Value::String(format!("{}… (truncated)", &text[..end]));
    }
}

fn push_row(items: &mut Vec<Value>, bytes: &mut usize, mut row: Value) -> bool {
    // Preserve the full action key. Only presentation strings may be shortened.
    clip_presentation(&mut row["label"], 512);
    if let Some(cells) = row["cells"].as_array_mut() {
        for cell in cells {
            clip_presentation(cell, 1024);
        }
    }
    if let Some(details) = row["details"].as_array_mut() {
        for detail in details {
            clip_presentation(&mut detail["label"], 256);
            clip_presentation(&mut detail["value"], 4096);
        }
    }
    let size = row.to_string().len() + 1;
    if items.len() >= MAX_RESOURCE_ROWS || bytes.saturating_add(size) > MAX_RESOURCE_BYTES {
        return false;
    }
    *bytes += size;
    items.push(row);
    true
}

pub(crate) fn resource_body(root: &str, report: &Report, failure: Option<&ScanFailure>) -> Body {
    let root_prefix = format!("{}/", root.trim_end_matches('/'));
    let relative = |path: &str| path.strip_prefix(&root_prefix).unwrap_or(path).to_owned();
    let ignored = ignored_advisories();
    let ignore_scope = format!(
        "Dependency Audit setting ignoredAdvisories; {} configured IDs/aliases. Matching advisories are hidden from this report.",
        ignored.len()
    );
    let ignore_scope = format!("{ignore_scope} Ignored findings in this project: {}. Review Ignored Findings shows their reasons and restores them.",crate::scoped::load(root).len());
    let mut items = Vec::new();
    let mut bytes = 0;
    let total_rows: usize = report.rows.iter().map(|r| 1 + r.advisories.len()).sum();
    'rows: for row in &report.rows {
        let advisories = row_advisories(row);
        let severity_rank = |s: &str| match s {
            "CRITICAL" => 5,
            "HIGH" => 4,
            "MODERATE" | "MEDIUM" => 3,
            "LOW" => 2,
            _ => 1,
        };
        let mut severity =
            advisories.iter().filter_map(|a| a.severity.as_deref()).max_by_key(|s| severity_rank(s)).unwrap_or("Unknown").to_owned();
        if severity != "Unknown" && advisories.iter().any(|a| a.severity.is_none()) {
            severity.push_str(" + unknown");
        }
        if advisories.iter().any(osv::Advisory::known_malicious) { severity="Known malicious".into(); }
        let sources = if row.sources.is_empty() {
            vec![Declaration { path: row.path.clone(), line: row.line, evidence: evidence(&row.path).into() }]
        } else {
            row.sources.clone()
        };
        let source_text =
            sources.iter().map(|d| format!("{}:{} — {}", relative(&d.path), d.line + 1, d.evidence)).collect::<Vec<_>>().join("\n");
        let directory = relative(&row.path).rsplit_once('/').map(|(p, _)| p.to_owned()).unwrap_or_else(|| ".".into());
        let mut details = vec![
            json!({"label":"Directory","value":directory}),
            json!({"label":"Declarations / lockfile evidence","value":source_text}),
            json!({"label":"Source","value":"OSV.dev package/version evidence; effective-build provenance is shown per source when available; no call analysis in this report"}),
            json!({"label":"Report scan","value":age(report.scanned_at)}),
            json!({"label":"Report coverage","value":match (report.files_scanned,report.dependencies_checked) {
                (Some(files),Some(dependencies)) => format!("{files} dependency files; {dependencies} unique package/version coordinates; {} skipped files. Discovery is bounded to 200 matches per supported glob.",report.files_skipped),
                _ => "Not recorded (older report)".into(),
            }}),
            json!({"label":"Discovery coverage","value":if report.discovery_limited { "Partial: the per-glob file discovery limit was reached." } else { "No discovery limit was recorded for this scan; unsupported formats remain outside coverage." }}),
            json!({"label":"Query freshness","value":"OSV answers may be reused from the 24-hour query cache."}),
            json!({"label":"Ignore scope","value":ignore_scope}),
            json!({"label":"Suggested package upgrade (known advisories)","value":row.fix.as_deref().unwrap_or("No recommendation available")}),
        ];
        if let Some(failure) = failure {
            details.push(json!({"label":"Last rescan failed","value":format!("{} — {}", age(Some(failure.at)), failure.message)}));
        }
        for advisory in &advisories {
            details.push(json!({"label":format!("{} evidence class",advisory.id),"value":advisory.classification()}));
            if advisory.known_malicious() {
                details.push(json!({"label":"Known-malicious source","value":format!("OpenSSF malicious-packages MAL namespace, delivered by OSV.dev. Reporters: {}. Package/version match does not establish execution or absence of other malware.",if advisory.malicious_reporters.is_empty(){"Not recorded".into()}else{advisory.malicious_reporters.join(", ")})}));
            }
            details.push(json!({"label":format!("{} summary",advisory.id),"value":if advisory.summary.is_empty() { "Details unavailable; rescan to retry." } else { &advisory.summary }}));
        }
        let key = dependency_key(row);
        if !push_row(
            &mut items,
            &mut bytes,
            json!({"key":format!("dep:{key}"),"label":format!("{} {}", row.name,row.version),"cells":["Dependency",row.name,ecosystem(row),row.version,row.fix.as_deref().unwrap_or("No recommendation"),severity,row.advisories.join(", "),relative(&row.path)],"details":details}),
        ) {
            break;
        }
        for advisory in advisories {
            let mut facts = details.clone();
            facts.extend([
                json!({"label":"Advisory URL","value":format!("https://osv.dev/vulnerability/{}",advisory.id)}),
                json!({"label":"Advisory aliases","value":if advisory.aliases.is_empty() { "None recorded".into() } else { advisory.aliases.join(", ") }}),
                json!({"label":"Advisory modified","value":if advisory.modified.is_empty() { "Not recorded" } else { &advisory.modified }}),
                json!({"label":"Fixed versions reported for this package","value":advisory_fixes(row,&advisory)}),
            ]);
            if !push_row(
                &mut items,
                &mut bytes,
                json!({"key":format!("advisory:{key}|{}",advisory.id),"label":format!("{} · {}",row.name,advisory.id),"cells":["Advisory",row.name,ecosystem(row),row.version,advisory_fixes(row,&advisory),if advisory.known_malicious(){"Known malicious"}else{advisory.severity.as_deref().unwrap_or("Unknown")},advisory.id,relative(&row.path)],"details":facts}),
            ) {
                break 'rows;
            }
        }
    }
    let flagged: std::collections::HashSet<(String, String, String)> =
        report.rows.iter().map(|r| (ecosystem(r).to_owned(), r.name.clone(), r.version.clone())).collect();
    let clean = |s: &str| s.chars().filter(|c| !c.is_control()).collect::<String>();
    for [eco, name, version] in &report.coordinates {
        if flagged.contains(&(eco.clone(), name.clone(), version.clone())) {
            continue;
        }
        let (name, eco, version) = (clean(name), clean(eco), clean(version));
        let details = vec![json!({"label":"Queried coordinate","value":format!("{eco}/{name}@{version}")}),json!({"label":"Result","value":"OSV.dev returned no unignored advisories for this exact coordinate (possibly from the 24-hour cache)."})];
        if !push_row(&mut items, &mut bytes, json!({"key":format!("checked:{eco}|{name}|{version}"),"label":format!("{name} {version}"),"cells":["Checked",name,eco,version,"-","None","-","-"],"details":details})) {
            break;
        }
    }
    for path in &report.skipped_paths {
        let path = clean(&relative(path));
        if !push_row(&mut items, &mut bytes, json!({"key":format!("skipped:{path}"),"label":path,"cells":["Skipped file","-","-","-","-","Not checked","-",path],"details":[json!({"label":"Why skipped","value":"The dependency file could not be read; its coordinates were not queried."})]})) {
            break;
        }
    }
    for [label, name, eco, spec, path] in &report.notes {
        let (name, eco, spec, label) = (clean(name), clean(eco), clean(spec), clean(label));
        let path = clean(&relative(path));
        let (severity, why) = match label.as_str() {
            manifest::LOCAL => ("Not checked", "Resolved from the local workspace, not a registry package; its coordinates were never sent to OSV.dev."),
            manifest::PRIVATE => ("Not checked", "Resolved from a registry or host outside the public ones; its name and version were never sent to OSV.dev."),
            _ => ("Audited", "The requirement carries a PEP 508 environment marker that was not evaluated; the pinned version is audited regardless of whether the marker applies here."),
        };
        let details = vec![json!({"label":"Declared","value":format!("{name} {spec}")}),json!({"label":"Why","value":why})];
        if !push_row(&mut items, &mut bytes, json!({"key":format!("note:{label}|{eco}|{name}|{spec}|{path}"),"label":format!("{name} {spec}"),"cells":[label,name,eco,spec,"-",severity,"-",path],"details":details})) {
            break;
        }
    }
    let total_rows = total_rows + report.coordinates.len() + report.skipped_paths.len() + report.notes.len();
    Body::Other { kind: "resources".into(), data: json!({"columns":["Type","Dependency","Ecosystem","Installed","Fixed","Severity","Advisory","Manifest"],"facets":[0,2,5,7],"totalRows":total_rows,"items":items}).to_string() }
}
