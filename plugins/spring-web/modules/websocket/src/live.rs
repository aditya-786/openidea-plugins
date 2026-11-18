use super::*;
impl WebSocketTools {
    pub fn live_diagnostics(&mut self, doc: &Document, allow_absence: bool) -> Vec<Diagnostic> {
        if doc.language == "xml" {
            let names = self
                .beans
                .for_cached_document(&doc.path, &doc.text)
                .filter(|i| allow_absence && i.fresh())
                .map(|i| i.iter().flat_map(|(_, names)| names.clone()).collect::<BTreeSet<_>>());
            let known = |name: &str| names.as_ref().is_some_and(|n| n.contains(name));
            return xml_checks::check(&doc.text, names.is_some().then_some(&known as &dyn Fn(&str) -> bool))
                .iter()
                .map(|f| f.diagnostic(&doc.text))
                .collect();
        }
        if !allow_absence || !host::setting_bool("springWebsocket.checkBrokerPrefixes").unwrap_or(true) {
            return Vec::new();
        }
        let Some(index) = self.index.for_cached_document(&doc.path, &doc.text).filter(|i| i.fresh()) else {
            return Vec::new();
        };
        let snapshot = collect(index.iter(), |_| true);
        snapshot
            .of_document(doc)
            .filter(|u| u.kind == Kind::Send && !snapshot.config.brokers.is_empty() && !snapshot.config.routed(&u.dest))
            .map(|u| {
                Diagnostic::new(
                    u.range,
                    Severity::Warning,
                    format!(
                        "`{}` is not under a cached source broker prefix ({}). Runtime broker configuration may differ.",
                        u.dest,
                        snapshot.config.brokers.join(", ")
                    ),
                )
                .code("unrouted-destination")
            })
            .collect()
    }
}
