//! Import-level reachability for PyPI and Maven rows. OSV publishes no per-symbol data for these
//! ecosystems (only Go has `ecosystem_specific.imports`), so this is NOT call analysis: it only
//! annotates whether the package appears in an import statement of bounded project sources.
use crate::{Declaration, Row};
use openidea_plugin::Project;
use std::collections::BTreeMap;
use std::time::Instant;

const GLOBS: [&str; 3] = ["**/*.py", "**/*.java", "**/*.kt"];
const MAX_FILES_PER_GLOB: u32 = 300;
const MAX_FILE_BYTES: usize = 256 * 1024;
const MAX_TOTAL_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const LABEL: &str = "Import analysis (not call-level)";

/// Imported name -> first file importing it. Python entries are lowercase top-level modules.
#[derive(Default)]
pub(crate) struct Imports {
    python: BTreeMap<String, String>,
    jvm: BTreeMap<String, String>,
}

pub(crate) fn collect(path: &str, text: &str, into: &mut Imports) {
    let python = path.ends_with(".py");
    for line in text.lines() {
        let line = line.trim();
        if python {
            let names: Vec<&str> = if let Some(rest) = line.strip_prefix("import ") {
                rest.split(',').map(|part| part.split_whitespace().next().unwrap_or("")).collect()
            } else if let Some(rest) = line.strip_prefix("from ") {
                vec![rest.split_whitespace().next().unwrap_or("")]
            } else {
                continue;
            };
            for name in names {
                let top = name.split('.').next().unwrap_or("").to_ascii_lowercase();
                if !top.is_empty() {
                    into.python.entry(top).or_insert_with(|| path.to_owned());
                }
            }
        } else if let Some(rest) = line.strip_prefix("import ") {
            let name = rest.trim_start_matches("static ").split_whitespace().next().unwrap_or("").trim_end_matches(';');
            if !name.is_empty() {
                into.jvm.entry(name.to_owned()).or_insert_with(|| path.to_owned());
            }
        }
    }
}

fn python_module(dist: &str) -> String {
    let name = dist.to_ascii_lowercase().replace(['-', '.'], "_");
    match name.as_str() {
        "pyyaml" => "yaml",
        "pillow" => "pil",
        "beautifulsoup4" => "bs4",
        "scikit_learn" => "sklearn",
        "opencv_python" => "cv2",
        "python_dateutil" => "dateutil",
        "pyjwt" => "jwt",
        "pycryptodome" => "crypto",
        _ => &name,
    }
    .to_owned()
}

/// `Some(file)` when imported, `None` otherwise. Maven uses the groupId as the package prefix.
fn imported_in(ecosystem: &str, name: &str, imports: &Imports) -> Option<String> {
    if ecosystem == "PyPI" {
        return imports.python.get(&python_module(name)).cloned();
    }
    let group = name.split(':').next().unwrap_or("");
    let prefix = format!("{group}.");
    imports.jvm.iter().find(|(import, _)| import.starts_with(&prefix)).map(|(_, file)| file.clone())
}

pub(crate) fn annotate(rows: &mut [Row], imports: &Imports) {
    for row in rows {
        let ecosystem = row.ecosystem.as_deref().unwrap_or("");
        if ecosystem != "PyPI" && ecosystem != "Maven" {
            continue;
        }
        let basis = if ecosystem == "PyPI" { "module name derived from the distribution name" } else { "groupId used as package prefix" };
        let verdict = match imported_in(ecosystem, &row.name, imports) {
            Some(file) => format!("package imported in source ({file}); {basis}"),
            None => format!("no import found in scanned .py/.java/.kt sources; {basis}, so a differently named package or a transitive use is not excluded"),
        };
        row.sources.push(Declaration {
            path: row.path.clone(),
            line: row.line,
            evidence: format!("{LABEL}: {verdict}. Annotation only: the vulnerable symbol is not known and no finding is suppressed."),
        });
    }
}

/// Bounded read of project sources; skipped entirely when no PyPI/Maven row exists.
pub(crate) fn scan(project: &Project, rows: &mut [Row], deadline: Instant) {
    if !rows.iter().any(|r| matches!(r.ecosystem.as_deref(), Some("PyPI" | "Maven"))) {
        return;
    }
    let mut imports = Imports::default();
    let mut total = 0;
    for glob in GLOBS {
        let Ok(found) = project.find_files(glob, MAX_FILES_PER_GLOB) else { continue };
        for path in found {
            if Instant::now() >= deadline {
                break;
            }
            let Ok(text) = project.read_file(&path) else { continue };
            total += text.len();
            if text.len() <= MAX_FILE_BYTES {
                collect(&path, &text, &mut imports);
            }
            if total > MAX_TOTAL_BYTES {
                break;
            }
        }
    }
    annotate(rows, &imports);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(ecosystem: &str, name: &str) -> Row {
        Row { path: "/p/pom.xml".into(), line: 3, name: name.into(), version: "1".into(), fix: None, advisories: vec![], ecosystem: Some(ecosystem.into()), sources: vec![], details: vec![] }
    }

    // Trimmed real OSV facts: GHSA-9hjg-9r4m-mvj7 (PyPI requests) and GHSA-jfh8-c2jp-5v3q (Maven log4j-core)
    // carry no `ecosystem_specific.imports`, so only import-level evidence is possible.
    #[test]
    fn real_osv_records_have_no_symbols() {
        for record in [
            r#"{"id":"GHSA-9hjg-9r4m-mvj7","affected":[{"package":{"ecosystem":"PyPI","name":"requests"},"ranges":[{"type":"ECOSYSTEM","events":[{"introduced":"0"},{"fixed":"2.32.4"}]}],"database_specific":{"source":"x"}}]}"#,
            r#"{"id":"GHSA-jfh8-c2jp-5v3q","affected":[{"package":{"ecosystem":"Maven","name":"org.apache.logging.log4j:log4j-core"},"ranges":[{"type":"ECOSYSTEM","events":[{"introduced":"2.0-beta9"},{"fixed":"2.15.0"}]}]}]}"#,
        ] {
            let v: serde_json::Value = serde_json::from_str(record).unwrap();
            assert!(v["affected"][0].get("ecosystem_specific").is_none());
        }
    }

    #[test]
    fn python_and_jvm_imports_are_matched_never_suppressed() {
        let mut imports = Imports::default();
        collect("/p/a.py", "import os, requests.adapters as r\nfrom yaml import safe_load\n", &mut imports);
        collect("/p/A.kt", "import org.apache.logging.log4j.LogManager\nimport static org.yaml.snakeyaml.Yaml.x;\n", &mut imports);
        let mut rows = vec![row("PyPI", "Requests"), row("PyPI", "PyYAML"), row("PyPI", "jinja2"), row("Maven", "org.apache.logging.log4j:log4j-core"), row("Maven", "org.apache.commons:commons-compress"), row("npm", "x")];
        annotate(&mut rows, &imports);
        let text = |i: usize| rows[i].sources.last().map(|s| s.evidence.clone()).unwrap_or_default();
        assert!(text(0).contains("package imported in source (/p/a.py)"));
        assert!(text(1).contains("package imported in source"));
        assert!(text(2).contains("no import found") && text(2).contains("Annotation only"));
        assert!(text(3).contains("package imported in source (/p/A.kt)"));
        assert!(text(4).contains("no import found"));
        assert!(rows[5].sources.is_empty());
    }
}
