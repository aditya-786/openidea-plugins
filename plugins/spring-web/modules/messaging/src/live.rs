use super::*;
impl Messaging {
    pub fn live_diagnostics(&mut self, doc: &Document, allow_absence: bool) -> Vec<openidea_plugin::Diagnostic> {
        let _ = self.index.for_cached_document(&doc.path, &doc.text);
        if doc.language == "java" {
            return self.unbound_routing_keys(doc, allow_absence);
        }
        if doc.language != "xml" {
            return Vec::new();
        }
        let Some(index) = self.index.for_cached_document(&doc.path, &doc.text) else {
            return Vec::new();
        };
        let mut out = listener_xml::problems(&doc.text, index);
        if !allow_absence || !index.fresh() {
            out.retain(|d| !matches!(d.code.as_deref(), Some("unresolved-reference" | "unresolved-listener-method")));
        }
        let elements = xml::elements(&doc.text);
        out.retain(|d| {
            openidea_plugin::text::offset(&doc.text, d.range.start).and_then(|at| xml::element_at(&elements, at)).is_some_and(|i| {
                matches!(
                    xml::namespace(&elements, i),
                    Some("http://www.springframework.org/schema/jms" | "http://www.springframework.org/schema/rabbit")
                )
            })
        });
        out
    }
    /// Info for a literal key sent to an exchange this project declares (direct or topic) that no binding of the project routes.
    fn unbound_routing_keys(&mut self, doc: &Document, allow_absence: bool) -> Vec<openidea_plugin::Diagnostic> {
        if !allow_absence {
            return Vec::new();
        }
        let Some(index) = self.index.for_cached_document(&doc.path, &doc.text) else { return Vec::new() };
        if !index.fresh() {
            return Vec::new();
        }
        let all: Vec<&Endpoint> = index.iter().flat_map(|(_, f)| &f.endpoints).collect();
        let unresolved = all.iter().any(|e| e.space == UNRESOLVED_BINDING);
        let keyed = |e: &&&Endpoint| e.space == ROUTING_KEY && e.role == Role::Declaration;
        let routable = |x: &str| {
            let declared: Vec<&str> = all.iter().filter(|e| e.space == "exchange" && e.role == Role::Declaration && !e.reference && e.name == x).map(|e| e.via.as_str()).collect();
            let keyless = all.iter().any(|e| e.space == "exchange" && e.role == Role::Consumer && e.name == x);
            !keyless && !declared.is_empty() && declared.iter().all(|v| matches!(*v, "new DirectExchange" | "new TopicExchange" | "ExchangeBuilder.directExchange" | "ExchangeBuilder.topicExchange"))
        };
        let Some(own) = index.files().get(&doc.path).map(|f| &f.endpoints) else { return Vec::new() };
        if unresolved {
            return Vec::new();
        }
        own.iter()
            .filter(|e| e.space == ROUTING_KEY && e.role == Role::Producer)
            .filter(|e| e.name.split_once('/').is_some_and(|(x, _)| routable(x)))
            .filter(|e| !all.iter().filter(keyed).any(|b| b.binds_key_of(e) || b.name == e.name))
            .map(|e| {
                let (x, key) = e.name.split_once('/').unwrap_or_default();
                openidea_plugin::Diagnostic::new(e.range, openidea_plugin::Severity::Info, format!("No binding with routing key \"{key}\" for exchange \"{x}\" found in this project."))
                    .code("unbound-routing-key")
            })
            .collect()
    }

    pub fn live_hover(&mut self, doc: &Document, at: Position) -> Option<openidea_plugin::hover::Hover> {
        if doc.language=="java" {return super::destination_docs::hover(doc,at);}
        // Namespace-qualified XML declarations are source facts; template receiver names alone are not broker proof.
        if doc.language != "xml" {
            return None;
        }
        let offset = offset(&doc.text, at)?;
        let elements = xml::elements(&doc.text);
        let element = xml::element_at(&elements, offset)?;
        if !matches!(
            xml::namespace(&elements, element),
            Some("http://www.springframework.org/schema/jms" | "http://www.springframework.org/schema/rabbit")
        ) {
            return None;
        }
        let endpoints = parse(&doc.path, &doc.text)?;
        let hits: Vec<_> = endpoints
            .iter()
            .filter(|e| e.holds(offset) && !e.name.contains(['$', '#', '{', '}']) && !e.name.starts_with(CONSTANT))
            .collect();
        if hits.len() != 1 {
            return None;
        }
        let e = hits[0];
        if e.name.len() > 256 || e.name.contains(['`', '\n', '\r']) {
            return None;
        }
        Some(openidea_plugin::hover::Hover::new(format!("{} {} declaration\n\nDestination: `{}`.\n\nSource XML declaration only; broker availability, delivery and runtime routing are not inferred.",e.broker.label(),e.space,e.name)).range(e.range))
    }
}
