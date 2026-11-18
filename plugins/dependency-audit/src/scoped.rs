//! Plugin-owned exact-coordinate suppressions; independent of inspection profiles/settings.
use crate::*;
use openidea_plugin::PickItem;

const MAX_ENTRIES: usize = 200;
const MAX_BYTES: usize = 256 << 10;
const PROFILE_FILE: &str = ".openidea/dependency-audit-ignores.json";
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Profile {
    format: String,
    version: u32,
    ignores: Vec<Ignore>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Ignore {
    ecosystem: String,
    name: String,
    version: String,
    id: String,
    aliases: Vec<String>,
    reason: String,
    at: u64,
}
fn key(root: &str) -> Result<String, Error> {
    if !root.starts_with('/') || root.len() > 4096 || root.chars().any(char::is_control) || root.split('/').any(|p| matches!(p, "." | ".."))
    {
        return Err(Error::Invalid("Project identity is unavailable.".into()));
    }
    Ok(format!("scoped-ignores:{root}"))
}
fn text(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}
fn valid(r: &Ignore) -> bool {
    matches!(r.ecosystem.as_str(), "Maven" | "npm" | "Packagist" | "crates.io" | "PyPI")
        && text(&r.name, 512)
        && text(&r.version, 128)
        && osv::is_safe_id(&r.id)
        && r.aliases.len() <= 100
        && r.aliases.iter().all(|a| osv::is_safe_id(a))
        && text(&r.reason, 512)
}
fn read(root: &str) -> Result<(Option<String>, Vec<Ignore>), Error> {
    let raw = host::storage::get(&key(root)?)?;
    let Some(value) = &raw else {
        return Ok((None, Vec::new()));
    };
    if value.len() > MAX_BYTES {
        return Err(Error::Invalid("The ignored-findings list is too large to read (over 256 KiB); nothing is hidden.".into()));
    }
    let rows: Vec<Ignore> =
        serde_json::from_str(value).map_err(|_| Error::Invalid("The ignored-findings list is unreadable; nothing is hidden.".into()))?;
    if rows.len() > MAX_ENTRIES || rows.iter().any(|r| !valid(r)) {
        return Err(Error::Invalid("The ignored-findings list is invalid; nothing is hidden.".into()));
    }
    Ok((raw, rows))
}
pub(crate) fn load(root: &str) -> Vec<Ignore> {
    read(root).map(|(_, r)| r).unwrap_or_default()
}
fn name(eco: &str, value: &str) -> String {
    if eco == "PyPI" { manifest::normalise_pypi(value) } else { value.to_owned() }
}
pub(crate) fn matches(rows: &[Ignore], eco: &str, package: &str, version: &str, id: &str, aliases: &[String]) -> bool {
    let package = name(eco, package);
    rows.iter().any(|r| {
        r.ecosystem == eco
            && r.name == package
            && r.version == version
            && std::iter::once(r.id.as_str())
                .chain(r.aliases.iter().map(String::as_str))
                .any(|known| known.eq_ignore_ascii_case(id) || aliases.iter().any(|a| known.eq_ignore_ascii_case(a)))
    })
}
pub(crate) fn filter(rows: &[Ignore], finding: &mut Finding) -> bool {
    finding.advisories.retain(|a| !matches(rows, finding.dep.ecosystem.osv(), &finding.dep.name, &finding.dep.version, &a.id, &a.aliases));
    !finding.advisories.is_empty()
}
pub(crate) fn aliases(row: &Row, id: &str) -> Vec<String> {
    row.details
        .iter()
        .find(|a| a.id == id)
        .map(|a| a.aliases.clone())
        .or_else(|| cached_advisory(id).filter(|a| a.id == id).map(|a| a.aliases))
        .unwrap_or_default()
}
pub(crate) fn visible_count(root: &str, report: &Report) -> usize {
    let scopes = load(root);
    if scopes.is_empty() {
        return report.rows.len();
    }
    report.rows.iter().filter(|r| visible(&scopes, r)).count()
}
pub(crate) fn visible(scopes: &[Ignore], r: &Row) -> bool {
    scopes.is_empty() || r.advisories.iter().any(|id| !matches(scopes, resources::ecosystem(r), &r.name, &r.version, id, &aliases(r, id)))
}
fn selected(root: &str, report: &Report, key: &str) -> Result<Ignore, Error> {
    let key = key.strip_prefix("advisory:").unwrap_or(key);
    let mut selected = None;
    for row in &report.rows {
        for id in &row.advisories {
            if format!("{}|{id}", actions::base(row)) != key {
                continue;
            }
            if !row.path.starts_with(&format!("{}/", root.trim_end_matches('/')))
                || row.path.split('/').any(|p| matches!(p, "." | ".."))
                || selected.is_some()
                || !osv::is_safe_id(id)
            {
                return Err(Error::Stale);
            }
            let eco = resources::ecosystem(row);
            let mut aliases =
                aliases(row, id).into_iter().filter(|a| osv::is_safe_id(a)).map(|a| a.to_ascii_uppercase()).collect::<Vec<_>>();
            aliases.sort();
            aliases.dedup();
            let record = Ignore {
                ecosystem: eco.into(),
                name: name(eco, &row.name),
                version: row.version.clone(),
                id: id.to_ascii_uppercase(),
                aliases,
                reason: "pending".into(),
                at: now(),
            };
            if !valid(&record) {
                return Err(Error::Invalid("The selected package/advisory facts cannot be safely scoped.".into()));
            }
            selected = Some(record);
        }
    }
    selected.ok_or(Error::Stale)
}
fn summary(root: &str, r: &Ignore) -> String {
    format!(
        "Project: {root}\nPackage: {} / {}\nVersion: {} (exact)\nAdvisory: {}\nKnown aliases: {}\nReason: {}\nRecorded: Unix {}\n\nThis applies only to this project and this exact version. It does not mark the package safe and does not expire.",
        r.ecosystem,
        r.name,
        r.version,
        r.id,
        r.aliases.join(", "),
        r.reason,
        r.at
    )
}
fn save(root: &str, before: &Option<String>, rows: &[Ignore]) -> Result<(), Error> {
    if host::storage::get(&key(root)?)? != *before {
        return Err(Error::Stale);
    }
    let json = serde_json::to_string(rows).map_err(|e| Error::Failed(e.to_string()))?;
    if rows.len() > MAX_ENTRIES || json.len() > MAX_BYTES || rows.iter().any(|r| !valid(r)) {
        return Err(Error::Invalid("Scoped-ignore storage is full. Review and restore an entry first.".into()));
    }
    host::storage::set(&key(root)?, &json)
}
fn encode_profile(rows: &[Ignore]) -> Result<String, Error> {
    let profile = Profile { format: "openidea-dependency-audit-ignores".into(), version: 1, ignores: rows.to_vec() };
    serde_json::to_string_pretty(&profile).map_err(|e| Error::Failed(e.to_string()))
}
fn decode_profile(text: &str) -> Result<Vec<Ignore>, Error> {
    let bad = |m: &str| Error::Invalid(format!("Ignore profile rejected: {m}. Nothing was imported."));
    if text.len() > MAX_BYTES {
        return Err(bad("file exceeds the size limit"));
    }
    let profile: Profile = serde_json::from_str(text).map_err(|_| bad("not a valid profile document"))?;
    if profile.format != "openidea-dependency-audit-ignores" || profile.version != 1 {
        return Err(bad("unsupported format or version"));
    }
    if profile.ignores.len() > MAX_ENTRIES || profile.ignores.iter().any(|r| !valid(r)) {
        return Err(bad("too many entries or an invalid entry"));
    }
    Ok(profile.ignores)
}
/// Existing entries win; returns the merged list and how many incoming rows were new.
fn merge_profile(existing: &[Ignore], incoming: Vec<Ignore>) -> Result<(Vec<Ignore>, usize), Error> {
    let mut merged = existing.to_vec();
    for r in incoming {
        let same = |e: &Ignore| e.ecosystem == r.ecosystem && e.name == r.name && e.version == r.version && e.id.eq_ignore_ascii_case(&r.id);
        if !merged.iter().any(same) {
            merged.push(r);
        }
    }
    if merged.len() > MAX_ENTRIES {
        return Err(Error::Invalid("Import would exceed the scoped-ignore limit; restore entries first. Nothing was imported.".into()));
    }
    let added = merged.len() - existing.len();
    Ok((merged, added))
}
fn owner(ctx: &Invocation, root: &str) -> Result<(), Error> {
    if ctx.project().and_then(|p| p.root().ok()).as_deref() != Some(root) {
        return Err(Error::Stale);
    }
    Ok(())
}
/// Cached editor finding path, separate from stored-report selection authority.
pub(crate) fn ignore_editor(root:&str,f:&Finding,id:&str,revalidate:impl Fn()->Result<(),Error>)->Result<(),Error> {
    let (before,mut ignores)=read(root)?;
    let row=Row{path:f.dep.path.clone(),line:0,name:f.dep.name.clone(),version:f.dep.version.clone(),fix:f.fix(),advisories:f.advisories.iter().map(|a|a.id.clone()).collect(),ecosystem:Some(f.dep.ecosystem.osv().into()),sources:Vec::new(),details:f.advisories.clone()};
    let key=format!("{}|{id}",actions::base(&row));
    let mut record=selected(root,&Report{rows:vec![row],..Report::default()},&key)?;
    if ignores.len()>=MAX_ENTRIES{return Err(Error::Invalid("Scoped-ignore storage is full; restore an entry first.".into()));}
    let Some(reason)=host::input("Reason for ignoring this exact cached editor finding (required, 512 bytes max)",None)? else{return Err(Error::Cancelled)};
    record.reason=reason.trim().into();if !valid(&record){return Err(Error::Invalid("Enter a nonempty reason of at most 512 bytes without controls.".into()));}
    let review=summary(root,&record).replace("retained scan snapshot","current source and cached OSV finding");
    if !ui_kit::prompts::confirm_exact("Ignore cached editor finding?",&review)?{return Err(Error::Cancelled);}
    revalidate()?;ignores.push(record);save(root,&before,&ignores)
}
impl DependencyAudit {
    pub(crate) fn scoped_command(&mut self, id: &str, ctx: &Invocation, selection: Option<&str>) -> Result<Option<String>, Error> {
        let project = ctx.project().ok_or("Open a project first.")?;
        let root = project.root()?;
        let (before, mut ignores) = read(&root)?;
        if id == "export-ignores" {
            if ignores.is_empty() {
                return Ok(Some("No ignored findings to export.".into()));
            }
            let path = format!("{root}/{PROFILE_FILE}");
            if project.exists(&path)? {
                return Err(Error::Invalid(format!("{PROFILE_FILE} already exists; rename or delete it first. Nothing was overwritten.")));
            }
            let text = encode_profile(&ignores)?;
            let review = format!("Write {} ignored finding(s) (with reasons) to {path}.\nReview the file before committing it; it is not applied anywhere else.", ignores.len());
            if !ui_kit::prompts::confirm_exact("Export ignored findings?", &review)? {
                return Ok(None);
            }
            owner(ctx, &root)?;
            if project.exists(&path)? {
                return Err(Error::Stale);
            }
            project
                .write_file(&path, &text)
                .map_err(|e| ui_kit::denied::map(e, "Exporting ignored findings", "project.write", "Dependency Audit"))?;
            return Ok(Some(format!("Exported {} ignored finding(s) to {PROFILE_FILE}.", ignores.len())));
        }
        if id == "import-ignores" {
            let path = format!("{root}/{PROFILE_FILE}");
            if !project.exists(&path)? {
                return Err(Error::Invalid("No profile file; export one first".into()));
            }
            let incoming = decode_profile(&project.read_file(&path)?)?;
            let (merged, added) = merge_profile(&ignores, incoming)?;
            if added == 0 {
                return Ok(Some("The profile contains no new ignores; nothing changed.".into()));
            }
            let list = merged[ignores.len()..]
                .iter()
                .map(|r| format!("{} {} {} / {} - {}", r.ecosystem, r.name, r.version, r.id, r.reason))
                .collect::<Vec<_>>()
                .join("\n");
            if !ui_kit::prompts::confirm_exact("Import ignored findings?", &format!("Add {added} new ignore(s) from {path}; existing entries are kept:\n{list}"))? {
                return Ok(None);
            }
            owner(ctx, &root)?;
            save(&root, &before, &merged)?;
            self.invalidate_report();
            return Ok(Some(format!("Imported {added} ignored finding(s).")));
        }
        if id == "review-ignores" {
            if ignores.is_empty() {
                return Ok(Some("No ignored findings in this project. The Ignored advisories setting is separate.".into()));
            }
            let items: Vec<_> = ignores
                .iter()
                .enumerate()
                .map(|(i, r)| PickItem {
                    key: i.to_string(),
                    label: format!("{} {} — {} / {}", r.name, r.version, r.id, r.ecosystem),
                    description: Some(format!("{} · Unix {}", r.reason, r.at)),
                })
                .collect();
            let Some(index) = host::pick("Ignored findings — select one to restore", &items)? else { return Ok(None) };
            let index = index.parse::<usize>().ok().filter(|i| *i < ignores.len()).ok_or(Error::Stale)?;
            if !ui_kit::prompts::confirm_exact("Restore this ignored finding?", &summary(&root, &ignores[index]))? {
                return Ok(None);
            }
            owner(ctx, &root)?;
            ignores.remove(index);
            save(&root, &before, &ignores)?;
            self.invalidate_report();
            return Ok(Some("Finding restored. It shows again in the last scan results; the Ignored advisories setting can still hide it.".into()));
        }
        let report_before = host::storage::get(&rows_key(&project)?)?;
        let report = stored_report(&project)?.ok_or(Error::Stale)?;
        let mut record = selected(&root, &report, selection.ok_or("Select an advisory row first.")?)?;
        if ignores.len() >= MAX_ENTRIES {
            return Err(Error::Invalid("Scoped-ignore storage is full. Review and restore an entry first.".into()));
        }
        let Some(reason) = host::input("Reason for ignoring this exact project/package/version/advisory (required, 512 bytes max)", None)?
        else {
            return Ok(None);
        };
        record.reason = reason.trim().into();
        if !valid(&record) {
            return Err(Error::Invalid("Enter a nonempty reason of at most 512 bytes without control characters.".into()));
        }
        if !ui_kit::prompts::confirm_exact("Ignore this exact finding?", &summary(&root, &record))? {
            return Ok(None);
        }
        owner(ctx, &root)?;
        let latest=actions::current_report(&project,&report_before)?;
        selected(&root,&latest,selection.ok_or(Error::Stale)?)?;
        ignores.push(record);
        save(&root, &before, &ignores)?;
        self.invalidate_report();
        Ok(Some("Finding ignored in this project. Restore it with Dependency Audit: Review Ignored Findings…".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    fn record() -> Ignore {
        Ignore {
            ecosystem: "PyPI".into(),
            name: "example-pkg".into(),
            version: "1.0.0".into(),
            id: "GHSA-EXAMPLE".into(),
            aliases: vec!["CVE-2026-1234".into()],
            reason: "Tracked risk acceptance".into(),
            at: 1,
        }
    }
    #[test]
    fn scope_is_exact_in_project_ecosystem_name_version_and_known_alias_identity() {
        let host = MockHost::install();
        let records = vec![record()];
        save("/project", &None, &records).unwrap();
        assert!(matches(&load("/project"), "PyPI", "Example_Pkg", "1.0.0", "cve-2026-1234", &[]));
        assert!(matches(&records, "PyPI", "example-pkg", "1.0.0", "NEW-ID", &["ghsa-example".into()]));
        for (eco, package, version, id) in [
            ("npm", "example-pkg", "1.0.0", "GHSA-EXAMPLE"),
            ("PyPI", "other", "1.0.0", "GHSA-EXAMPLE"),
            ("PyPI", "example-pkg", "1.0.1", "GHSA-EXAMPLE"),
            ("PyPI", "example-pkg", "1.0.0", "UNKNOWN"),
        ] {
            assert!(!matches(&records, eco, package, version, id, &[]));
        }
        assert!(!matches(&load("/another"), "PyPI", "example-pkg", "1.0.0", "GHSA-EXAMPLE", &[]));
        let body = summary("/project", &records[0]);
        for text in ["/project", "PyPI", "example-pkg", "1.0.0", "GHSA-EXAMPLE", "CVE-2026-1234", "Tracked risk acceptance", "does not expire"] {
            assert!(body.contains(text), "{body}");
        }
        assert!(host.requests().is_empty());
    }
    #[test]
    fn corrupt_oversized_denied_and_raced_storage_never_suppresses_or_overwrites() {
        let host = MockHost::install();
        for value in ["not json".into(), "x".repeat(MAX_BYTES + 1), serde_json::to_string(&vec![record(); MAX_ENTRIES + 1]).unwrap()] {
            host::storage::set("scoped-ignores:/project", &value).unwrap();
            assert!(load("/project").is_empty());
            assert!(read("/project").is_err());
            assert_eq!(host.storage()["scoped-ignores:/project"], value);
        }
        host::storage::delete("scoped-ignores:/project").unwrap();
        let before = read("/project").unwrap().0;
        host::storage::set("scoped-ignores:/project", "[]").unwrap();
        assert!(matches!(save("/project", &before, &[record()]), Err(Error::Stale)));
        assert_eq!(host.storage()["scoped-ignores:/project"], "[]");
        for root in ["", "anonymous", "/project/../other"] {
            assert!(key(root).is_err());
        }
        let host = host.with_capabilities(&[]);
        assert!(load("/project").is_empty());
        assert!(read("/project").is_err());
        assert_eq!(host.storage()["scoped-ignores:/project"], "[]");
    }
    #[test]
    fn profile_round_trips_and_rejects_bad_input() {
        let _host = MockHost::install();
        let text = encode_profile(&[record()]).unwrap();
        assert_eq!(decode_profile(&text).unwrap(), vec![record()]);
        let mut bad_id = record();
        bad_id.id = "../x".into();
        let mut no_reason = record();
        no_reason.reason = " ".into();
        for rows in [vec![bad_id], vec![no_reason]] {
            assert!(decode_profile(&encode_profile(&rows).unwrap()).is_err());
        }
        assert!(decode_profile(&text.replace("\"version\": 1", "\"version\": 2")).is_err());
        assert!(decode_profile(&text.replace("\"ignores\"", "\"extra\": 1, \"ignores\"")).is_err());
        assert!(decode_profile(&text.replace("\"at\"", "\"x\": 1, \"at\"")).is_err());
        assert!(decode_profile("not json").is_err());
        assert!(decode_profile(&" ".repeat(MAX_BYTES + 1)).is_err());
        assert!(decode_profile(&encode_profile(&vec![record(); MAX_ENTRIES + 1]).unwrap()).is_err());
    }
    #[test]
    fn merge_keeps_existing_skips_duplicates_and_enforces_limit() {
        let mut other = record();
        other.version = "2.0.0".into();
        let mut dup = record();
        dup.reason = "different reason".into();
        let (merged, added) = merge_profile(&[record()], vec![dup, other.clone()]).unwrap();
        assert_eq!((added, merged.len(), merged[0].reason.as_str()), (1, 2, "Tracked risk acceptance"));
        let full: Vec<_> = (0..MAX_ENTRIES).map(|i| Ignore { version: format!("{i}"), ..record() }).collect();
        assert!(merge_profile(&full, vec![other]).is_err());
    }
    #[test]
    fn unsafe_or_ambiguous_snapshot_ids_cannot_create_scopes() {
        let _host = MockHost::install();
        let row: Row = serde_json::from_str(
            r#"{"path":"/project/package.json","line":0,"name":"x","version":"1.0","fix":null,"advisories":["../bad"],"ecosystem":"npm"}"#,
        )
        .unwrap();
        let key = format!("{}|../bad", actions::base(&row));
        assert!(selected("/project", &Report { rows: vec![row], ..Report::default() }, &key).is_err());
        let row:Row=serde_json::from_str(r#"{"path":"/project/package.json","line":0,"name":"x","version":"1.0","fix":null,"advisories":["GHSA-X","GHSA-X"],"ecosystem":"npm"}"#).unwrap();
        let key = format!("{}|GHSA-X", actions::base(&row));
        let report = Report { rows: vec![row], ..Report::default() };
        assert!(selected("/project", &report, &key).is_err());
        assert!(selected("/another", &report, &key).is_err());
    }
}
