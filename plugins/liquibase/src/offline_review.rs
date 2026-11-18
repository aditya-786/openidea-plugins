//! A reviewed subset of cached CLI observations, never deployable SQL/DDL.
use super::*;
pub(super) struct Selection { identity: String, text: String, count: usize }
fn identity(state: &State, report: &Report) -> String {
    let mut hash = Sha256::new();
    hash.update(state.generation.to_le_bytes());
    for value in std::iter::once(report.root.as_str()).chain(std::iter::once(report.source.as_str()))
        .chain(report.items.iter().flat_map(|item| [item.kind.as_str(), item.object.as_str(), item.detail.as_str()])) {
        hash.update((value.len() as u64).to_le_bytes()); hash.update(value.as_bytes());
    }
    format!("{:x}", hash.finalize())
}
pub(super) fn replace_report(state: &mut State, report: Report) -> Result<(), Error> {
    let next = state.generation.checked_add(1).ok_or(Error::Stale)?;
    state.generation = next;
    state.report = Some(report);
    state.generated = None;
    state.generated_view = false;
    state.selected = None;
    state.review = None;
    state.visible = 0..0;
    state.page = 0;
    Ok(())
}
pub(super) fn body(state: &State, report: &Report) -> Option<Body> {
    let review = state.review.as_ref().filter(|review| review.identity == identity(state, report))?;
    Some(Body::Form(vec![
        Field::new("review-count", "Selected report observations", FieldValue::readonly_text(&review.count.to_string(), false)),
        Field::new("review-output", "Read-only selected output (not SQL or a changelog)", FieldValue::readonly_text(&review.text, true)),
    ]))
}
fn owned(ctx: &Invocation, root: &str) -> bool {
    ctx.project().and_then(|project| project.root().ok()).as_deref() == Some(root)
        && host::projects().is_ok_and(|projects| projects.iter().any(|p| p.root().ok().as_deref() == Some(root)))
}
impl Liquibase {
    pub(crate) fn choose_offline_observations(&mut self, ctx: &Invocation) -> Result<(), Error> {
        if self.offline.running.is_some() { return Err(Error::Stale); }
        let report = self.offline.report.as_ref().ok_or(Error::Stale)?;
        if !owned(ctx, &report.root) || report.items.is_empty() { return Err(Error::Stale); }
        let id = identity(&self.offline, report);
        let choices: Vec<_> = report.items.iter().enumerate().map(|(at, item)| {
            PickItem::new(&format!("offline-review:{id}:{at}"), &format!("{} · {}", item.kind, item.object))
                .description("Cached CLI observation only; full selected text appears in the preview.")
        }).collect();
        let Some(chosen) = host::pick_many("Choose report observations to preview (not deployable changes)", &choices)? else { return Ok(()); };
        let report = self.offline.report.as_ref().ok_or(Error::Stale)?;
        if !owned(ctx, &report.root) || identity(&self.offline, report) != id || chosen.len() > report.items.len() { return Err(Error::Stale); }
        let available: BTreeMap<_, _> = choices.iter().enumerate().map(|(index, item)| (item.key.as_str(), index)).collect();
        let mut indexes = BTreeSet::new();
        for key in &chosen {
            let index = available.get(key.as_str()).ok_or(Error::Stale)?;
            if !indexes.insert(*index) { return Err(Error::Stale); }
        }
        let mut text = format!("Project: {}\nCaptured report: {}\n\nSelected positive report observations only. Not executable changes, SQL, DDL or proof of complete schema differences.\n", report.root, report.source);
        for index in &indexes {
            let item = &report.items[*index];
            text.push_str(&format!("\n{} · {}\n{}\n", item.kind, item.object, item.detail));
            if text.len() > 128 << 10 { return Err("Selected output exceeds the 128KiB review limit; choose fewer observations.".into()); }
        }
        self.offline.generated_view = false;
        self.offline.review = (!indexes.is_empty()).then_some(Selection { identity: id, text, count: indexes.len() });
        self.offline.selected = None;
        self.offline.visible = 0..0;
        self.offline.status = format!("{} report observations selected for read-only review; no CLI, SQL or workspace edit runs on selection.", indexes.len());
        ui_kit::web::refresh_view(VIEW, include_str!("ui_config.json"));
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    fn setup() -> (MockHost, Liquibase) {
        let host = MockHost::install().with_capabilities(&["project.read"]);
        let mut plugin = Liquibase::default();
        replace_report(&mut plugin.offline, Report { root: "/project".into(), source: "referenceSHA -> targetSHA; coverage unverified".into(), items: vec![
            Item { kind: "Missing Table".into(), object: "CUSTOMER".into(), detail: "first detail".into() },
            Item { kind: "Changed Table".into(), object: "ACCOUNT".into(), detail: "second detail".into() },
        ], at: Instant::now() }).unwrap();
        (host, plugin)
    }
    fn key(plugin: &Liquibase, index: usize) -> String { format!("offline-review:{}:{index}", identity(&plugin.offline, plugin.offline.report.as_ref().unwrap())) }
    #[test]
    fn selected_observations_preview_exact_subset_without_cli_and_back_restores_list() {
        let (host, mut plugin) = setup();
        host.answer_next_prompt(Some(&[&key(&plugin, 1)]));
        plugin.handle_offline_diff(ViewEvent::Button("offline-choose".into()), &host.invocation()).unwrap();
        let shown = plugin.render_offline_diff(None, &host.invocation()).unwrap();
        let text = format!("{:?}", shown.body);
        assert!(text.contains("ACCOUNT") && text.contains("second detail") && text.contains("referenceSHA"));
        assert!(!text.contains("CUSTOMER"));
        assert!(text.contains("Not executable changes"));
        assert!(host.process_runs().is_empty() && host.terminals().is_empty());
        plugin.handle_offline_diff(ViewEvent::Button("offline-back".into()), &host.invocation()).unwrap();
        assert!(plugin.offline.review.is_none());
        assert!(matches!(plugin.render_offline_diff(None, &host.invocation()).unwrap().body, Body::Table(_)));
    }
    #[test]
    fn cancel_foreign_forged_and_duplicate_keys_preserve_prior_selection() {
        let (host, mut plugin) = setup();
        let key = key(&plugin, 0);
        host.answer_next_prompt(Some(&[&key]));
        plugin.choose_offline_observations(&host.invocation()).unwrap();
        let before = plugin.offline.review.as_ref().unwrap().text.clone();
        host.answer_next_prompt(None);
        plugin.choose_offline_observations(&host.invocation()).unwrap();
        for bad in [vec!["forged"], vec![key.as_str(), key.as_str()]] {
            host.answer_next_prompt(Some(&bad));
            assert!(plugin.choose_offline_observations(&host.invocation()).is_err());
            assert_eq!(plugin.offline.review.as_ref().unwrap().text, before);
        }
        host.set_project_root(Some("/other"));
        assert!(plugin.choose_offline_observations(&host.invocation()).is_err());
        assert_eq!(plugin.offline.review.as_ref().unwrap().text, before);
        assert!(host.process_runs().is_empty());
    }
    #[test]
    fn replacing_identical_report_changes_picker_identity_and_expires_preview() {
        let (host, mut plugin) = setup();
        let old = key(&plugin, 1);
        host.answer_next_prompt(Some(&[&old]));
        plugin.choose_offline_observations(&host.invocation()).unwrap();
        let report = plugin.offline.report.take().unwrap();
        replace_report(&mut plugin.offline, report).unwrap();
        assert!(plugin.offline.review.is_none());
        assert_ne!(old, key(&plugin, 1));
        host.answer_next_prompt(Some(&[&old]));
        assert!(plugin.choose_offline_observations(&host.invocation()).is_err());
        assert!(plugin.offline.review.is_none());
    }
}
