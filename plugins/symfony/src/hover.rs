//! Positive cached declaration facts only: no discovery, reads or runtime/type claims.
use crate::*;
use openidea_plugin::hover::{Hover, HoverProvider};
fn escaped(value: &str) -> String {
    value
        .chars()
        .take(512)
        .flat_map(|c| {
            if c.is_control() {
                vec![' ']
            } else if "\\`*_{}[]<>#|!".contains(c) {
                vec!['\\', c]
            } else {
                vec![c]
            }
        })
        .collect()
}
impl HoverProvider for Main {
    fn hover(&mut self, doc: &Document, at: Position) -> Option<Hover> {
        if language_of(&doc.path) != Some(doc.language.as_str()) {
            return None;
        }
        let at = text::offset(&doc.text, at)?;
        if let Some(index) = self.index_for(&doc.path) {
            if let Some(hover) = type_hover(index, doc, at) {
                return Some(hover);
            }
        }
        let r = ownership::reference(doc, at)?;
        let name = &doc.text[r.start..r.end];
        if is_dynamic(name) {
            return None;
        }
        let (root, index) =
            self.indexes.iter().filter(|(root, _)| !root.is_empty() && under(&doc.path, root)).max_by_key(|(root, _)| root.len())?;
        if !index.settings_current() {return None;}
        // Current logical ownership is metadata only; never reuse a parent cache for a cold child.
        if host::project_of(&doc.path).and_then(|project|project.root().ok()).as_deref()!=Some(root.as_str()) {return None;}
        let mut domain = None;
        let (kind, entry) = match r.kind {
            Kind::Route => ("Route", index.routes.get(name)?),
            Kind::Service => ("Service", index.services.get(name)?),
            Kind::Template => {
                if index.ambiguous_templates.contains(name) {
                    return None;
                }
                ("Template", index.templates.get(name)?)
            }
            Kind::Translation => {
                let d = ownership::domain(doc, &r)?;
                let entry = index.translations.get(&d)?.get(name)?;
                domain = Some(d);
                ("Translation key", entry)
            }
            _ => return None,
        };
        // A current source document may invalidate its own cached declaration.
        if entry.path == doc.path || entry.path.is_empty() || !under(&entry.path, root)
            || host::project_of(&entry.path).and_then(|project|project.root().ok()).as_deref()!=Some(root.as_str()) {
            return None;
        }
        let mut body = format!("**{kind}** {}", escaped(name));
        if let Some(domain) = domain {
            body.push_str(&format!("\n\nDomain: {}", escaped(&domain)));
        }
        let relative = entry.path.strip_prefix(&format!("{root}/"))?;
        body.push_str(&format!("\n\nDeclared in: {}:{}", escaped(relative), entry.at.line + 1));
        if r.kind == Kind::Route {
            let routes: Vec<_> = index.route_defs.iter().filter(|route| route.name.as_deref() == Some(name)).take(9).collect();
            if routes.len() == 1 {
                let route = routes[0];
                body.push_str(&format!("\n\nDeclared path: {}", escaped(&route.path)));
                if !route.methods.is_empty() {
                    body.push_str(&format!("\n\nDeclared methods: {}", escaped(&route.methods.join(", "))));
                }
            }
        }
        body.push_str(if !index.settings_current() || index.partial {
            "\n\nFrom an incomplete project scan; it may be out of date."
        } else {
            "\n\nFrom the last project scan; it may be out of date."
        });
        Some(Hover::new(body).range(range_of(&doc.text, r.start, r.end)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    fn entry(path: &str) -> Entry {
        Entry { path: path.into(), at: Position::new(4, 2), detail: Some("do-not-display-values".into()) }
    }
    fn model() -> Main {
        let mut m = Main::default();
        let mut i = Index::default();
        i.routes.insert("home".into(), entry("/p/config/routes.yaml"));
        i.services.insert("mailer".into(), entry("/p/config/services.yaml"));
        i.templates.insert("@Ui/page.twig".into(), entry("/p/custom/page.twig"));
        i.translations.entry("admin".into()).or_default().insert("menu".into(), entry("/p/translations/admin.en.yaml"));
        i.translations.entry("messages".into()).or_default().insert("menu".into(), entry("/p/translations/messages.en.yaml"));
        i.route_defs.push(route_defs::RouteDef {
            name: Some("home".into()),
            path: "/home".into(),
            methods: vec!["GET".into()],
            class: "Controller".into(),
            member: Some("index".into()),
            file: "/p/config/routes.yaml".into(),
            at: Position::new(4, 2),
        });
        m.indexes.insert("/p".into(), i);
        m
    }
    fn at(marked: &str, lang: &str) -> (Document, Position) {
        let index = marked.find('^').unwrap();
        let text = marked.replacen('^', "", 1);
        let pos = text::position(&text, index);
        let ext = if lang == "yaml" { "yaml" } else { lang };
        (Document { path: format!("/p/config/current.{ext}"), language: lang.into(), version: 9, text }, pos)
    }
    #[test]
    fn real_php_reference_after_heredoc_remains_hoverable_without_source_reads() {
        let host=MockHost::install().with_capabilities(&["project.read"]);host.set_project_root(Some("/p"));let mut m=model();
        for header in ["<<<TEXT","<<<'TEXT'","<<<\"TEXT\""] {
            let marked=format!("<?php\n$value = {header}\n$this->generateUrl('ghost');\nTEXT;\n$this->generateUrl('ho^me');");
            let (doc,position)=at(&marked,"php");assert!(m.hover(&doc,position).is_some(),"{header}");
        }
        assert!(host.requests().is_empty() && host.process_runs().is_empty());
    }

    #[test]
    fn php_literal_body_reference_cannot_borrow_cached_hover() {
        let host=MockHost::install().with_capabilities(&["project.read"]);host.set_project_root(Some("/p"));let mut m=model();
        for marked in ["<?php\n$value = <<<TEXT\n$this->generateUrl('ho^me');\nTEXT;", "<?php\n$value = <<<'TEXT'\n$this->generateUrl('ho^me');\nTEXT;", "<?php\n$value = `echo $this->generateUrl('ho^me')`;", "<?php\n$value = <<<TEXT\n$this->generateUrl('ho^me');"] {
            let (doc,position)=at(marked,"php");assert!(m.hover(&doc,position).is_none(),"{marked}");
        }
    }

    #[test]
    fn denied_metadata_grant_never_reuses_cached_hover_without_ownership_proof() {
        let host=MockHost::install().with_capabilities(&[]);host.set_project_root(Some("/p"));
        let mut m=model();let (doc,position)=at("{{ path('ho^me') }}","twig");
        assert!(m.hover(&doc,position).is_none());
        assert!(host.requests().is_empty() && host.process_runs().is_empty());
    }

    #[test]
    fn cold_nested_invoking_owner_cannot_reuse_cached_parent_hover() {
        let host=MockHost::install().with_capabilities(&["project.read"]);host.set_project_root(Some("/p"));
        let mut m=model();let (doc,position)=at("{{ path('ho^me') }}","twig");
        assert!(m.hover(&doc,position).is_some());
        host.add_project("/p/config");
        assert!(m.hover(&doc,position).is_none(),"nested owner has no cache; parent facts are not its facts");
        assert!(host.requests().is_empty() && host.process_runs().is_empty());
    }

    #[test]
    fn cached_declaration_owner_is_rechecked_separately_from_invoking_document() {
        let host=MockHost::install().with_capabilities(&["project.read"]);host.set_project_root(Some("/p"));
        let mut m=model();let (doc,position)=at("{{ path('ho^me') }}","twig");
        let doc=Document{path:"/p/templates/current.twig".into(),..doc};
        assert!(m.hover(&doc,position).is_some());
        host.add_project("/p/config");
        assert!(m.hover(&doc,position).is_none(),"declaration now belongs to another root");
        host.set_project_root(None);
        assert!(m.hover(&doc,position).is_none(),"closed invoking project cannot reuse cached prefix");
        assert!(host.requests().is_empty() && host.process_runs().is_empty());
    }

    #[test]
    fn positive_cached_facts_need_only_declared_metadata_grant_and_no_source_files() {
        let host = MockHost::install().with_capabilities(&["project.read"]);
        host.set_project_root(Some("/p"));
        let mut m = model();
        let (d, p) = at("😀 {{ path('ho^me') }}", "twig");
        let h = m.hover(&d, p).unwrap();
        assert!(
            h.markdown.contains("Route")
                && h.markdown.contains("GET")
                && h.markdown.contains("/home")
                && h.markdown.contains("may be out of date")
        );
        assert_eq!(h.range, Some(range_of(&d.text, d.text.find("home").unwrap(), d.text.find("home").unwrap() + 4)));
        let (d, p) = at("<?php $container->get('mai^ler');", "php");
        assert!(m.hover(&d, p).unwrap().markdown.contains("Service"));
        let (d, p) = at("{% include '@Ui/pa^ge.twig' %}", "twig");
        let h = m.hover(&d, p).unwrap();
        assert!(h.markdown.contains("custom/page.twig"));
        assert!(!h.markdown.contains("do-not-display-values"));
        assert_eq!(host.fetches(), 0);
    }
    #[test]
    fn literal_translation_domain_is_exact_and_dynamic_or_missing_domains_stay_quiet() {
        let _host = MockHost::install().with_capabilities(&["project.read"]);
        _host.set_project_root(Some("/p"));
        let mut m = model();
        let (d, p) = at("{{ 'me^nu'|trans({},'admin') }}", "twig");
        assert!(m.hover(&d, p).unwrap().markdown.contains("Domain: admin"));
        let (d, p) = at("{% trans_default_domain 'admin' %}{{ 'me^nu'|trans }}", "twig");
        assert!(m.hover(&d, p).unwrap().markdown.contains("Domain: admin"));
        let (d, p) = at("<?php $translator->trans('me^nu',domain:'admin');", "php");
        assert!(m.hover(&d, p).unwrap().markdown.contains("Domain: admin"));
        for source in [
            "{{ 'me^nu'|trans({},'missing') }}",
            "{{ 'me^nu'|trans({},domain) }}",
            "{{ 'me^nu'|translatable }}",
            "{{ 'me^nu'|trans({},'admin' ~ 'extra') }}",
            "{% block title %}{% trans_default_domain 'admin' %}{% endblock %}{{ 'me^nu'|trans }}",
            "{{ 'me^nu'|trans({},domain:",
            "{{ 'me^nu'|trans(domain:'admin',domain:'messages') }}",
            "{% trans_default_domain dynamic %}{{ 'me^nu'|trans }}",
            "{% trans_default_domain 'admin' %}{% trans_default_domain 'messages' %}{{ 'me^nu'|trans }}",
        ] {
            let (d, p) = at(source, "twig");
            assert!(m.hover(&d, p).is_none(), "{source}");
        }
    }
    #[test]
    fn comments_raw_html_nested_strings_and_foreign_languages_never_create_hovers() {
        let _host = MockHost::install().with_capabilities(&["project.read"]);
        _host.set_project_root(Some("/p"));
        let mut m = model();
        for source in [
            "{# {{ path('ho^me') }} #}",
            "<!-- {{ path('ho^me') }} -->",
            "{% verbatim %}{{ path('ho^me') }}{% endverbatim %}",
            "<p>path('ho^me')</p>",
            "{{ object.path('ho^me') }}",
            "{{ \"path('ho^me')\" }}",
        ] {
            let (d, p) = at(source, "twig");
            assert!(m.hover(&d, p).is_none(), "{source}");
        }
        for source in
            ["<?php // $container->get('mai^ler');", "<?php /* $container->get('mai^ler'); */", "<?php $x=\"$container->get('mai^ler');\";"]
        {
            let (d, p) = at(source, "php");
            assert!(m.hover(&d, p).is_none(), "{source}");
        }
        let (d, p) = at("# arguments: ['@mai^ler']", "yaml");
        assert!(m.hover(&d, p).is_none());
        let (d, p) = at("arguments: ['@mai^ler']", "yaml");
        assert!(m.hover(&d, p).is_some());
        let (d, p) = at("{{ path('ho^me') }}", "html");
        assert!(m.hover(&d, p).is_none());
    }
    #[test]
    fn cache_provenance_bounds_ambiguity_and_nested_project_ownership_are_explicit() {
        let _host = MockHost::install().with_capabilities(&["project.read"]);
        _host.set_project_root(Some("/p"));
        let mut m = model();
        let (d, p) = at("{{ path('ho^me') }}", "twig");
        m.indexes.get_mut("/p").unwrap().partial = true;
        assert!(m.hover(&d, p).unwrap().markdown.contains("incomplete project scan"));
        let unknown = Document { text: d.text.replace("home", "gone"), version: 10, ..d.clone() };
        assert!(m.hover(&unknown, p).is_none());
        let outside = Document { path: "/p2/file.twig".into(), ..d.clone() };
        assert!(m.hover(&outside, p).is_none());
        m.indexes.insert("/p/config".into(), Index::default());
        assert!(m.hover(&d, p).is_none());
        m.indexes.remove("/p/config");
        let (d, p) = at("{% include '@Ui/pa^ge.twig' %}", "twig");
        m.indexes.get_mut("/p").unwrap().ambiguous_templates.insert("@Ui/page.twig".into());
        assert!(m.hover(&d, p).is_none());
        let huge = Document { text: format!("{}{}", " ".repeat(128 << 10), d.text), ..d };
        assert!(m.hover(&huge, p).is_none());
    }
    #[test]
    fn requested_domain_never_falls_back_to_other_catalogues_for_existing_navigation() {
        let m = model();
        let i = &m.indexes["/p"];
        let (d, p) = at("{{ 'me^nu'|trans({},'missing') }}", "twig");
        let r = refs::reference_at("twig", &d.text, text::offset(&d.text, p).unwrap()).unwrap();
        assert!(candidates(i, &d.path, "twig", &d.text, &r).is_empty());
    }
}
