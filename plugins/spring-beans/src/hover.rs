//! Owned source declarations only: no effective bean candidate or Rename graph.
use crate::*;
use openidea_plugin::hover::{Hover, HoverProvider};
use crate::annotation_owners::Owners;
use spring_text::java::{Annotation, MemberKind, TypeDecl};
fn known(owners: &Owners, text: &str, a: &Annotation, expected: &str) -> bool {
    owners.known(text, a, expected)
}
/// A generic type header: the shared model does not settle its identifiers.
fn generic(text: &str, t: &TypeDecl) -> bool {
    text.get(t.at + t.name.len()..t.end).and_then(|s| s.split_once('{')).is_some_and(|(header, _)| header.contains('<'))
}
fn literal(text: &str, s: &spring_text::java::Str) -> Option<String> {
    (s.value.len() <= 128
        && !s.value.is_empty()
        && !s.value.contains(['\\', '\r', '\n', '$', '#'])
        && text.get(s.at..s.end) == Some(s.value.as_str()))
    .then(|| s.value.clone())
}
fn values(owners: &Owners, text: &str, annotations: &[Annotation], expected: &str) -> Vec<String> {
    annotations
        .iter()
        .filter(|a| known(owners, text, a, expected))
        .flat_map(|a| a.args.iter())
        .filter(|arg| java::tokens(&arg.text).iter().all(|t| {
            matches!(t.s, "{" | "}" | ",") || t.s.starts_with('"') && t.s.ends_with('"')
        }))
        .flat_map(|a| a.strings.iter())
        .filter_map(|s| literal(text, s))
        .take(8)
        .collect()
}
fn safe(value: &str) -> String {
    value
        .chars()
        .take(256)
        .collect::<String>()
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('`', "\\`")
}
impl HoverProvider for Navigator {
    fn hover(&mut self, doc: &Document, at: Position) -> Option<Hover> {
        if doc.language != "java" || !doc.path.ends_with(".java") || doc.text.len() > 128 * 1024 {
            return None;
        }
        let off = offset(&doc.text, at)?;
        let file = java::parse(&doc.text);
        let owners = Owners::new(&file, &doc.text, false);
        if let Some(owner) = file.types.iter().find(|t| t.at <= off && off < t.at + t.name.len()) {
            if generic(&doc.text, owner) {
                return None;
            }
            // Nested names in the shared model may be qualified; admit top-level
            // concrete classes only until their identifier ownership is established.
            if owner.outer.is_some() || owner.kind != "class" {
                return None;
            }
            let markers = ["Component", "Service", "Repository", "Controller"];
            let annotations: Vec<_> = owner.annotations.iter().filter(|a| {
                markers.iter().any(|name| known(&owners, &doc.text, a, &format!("org.springframework.stereotype.{name}")))
            }).collect();
            if annotations.len() != 1 {
                return None;
            }
            let annotation = annotations[0];
            // A concatenation/constant is not a known literal bean-name spelling.
            if !annotation.args.is_empty() && !(annotation.args.len() == 1
                && annotation.args[0].name == "value"
                && annotation.args[0].strings.len() == 1
                && literal(&doc.text, &annotation.args[0].strings[0]).is_some()
                && java::tokens(&annotation.args[0].text).len() == 1) {
                return None;
            }
            let location = range(&doc.text, owner.at, owner.at + owner.name.len());
            let bean = self.with_doc(doc, |_, model| {
                let found: Vec<_> = model.beans.iter().filter(|b| b.range == location && b.kind == format!("@{}", annotation.name)).collect();
                (found.len() == 1).then(|| found[0].clone())
            })?;
            if bean.name.is_empty() || bean.name.len() > 128 || bean.name.contains(['\\', '$', '#', '\r', '\n']) {
                return None;
            }
            let mut content = format!("**Source bean declaration** `{}`\n\nStereotype: `@{}`\nDeclared class spelling: `{}`", safe(&bean.name), safe(&annotation.name), safe(&owner.name));
            if owner.annotations.iter().any(|a| known(&owners, &doc.text, a, "org.springframework.context.annotation.Primary")) {
                content.push_str("\nSource marker: `@Primary`");
            }
            for (label, expected) in [("profile", "org.springframework.context.annotation.Profile"), ("qualifier", "org.springframework.beans.factory.annotation.Qualifier")] {
                let facts = values(&owners, &doc.text, &owner.annotations, expected);
                if !facts.is_empty() {
                    content.push_str(&format!("\nDeclared {label} spellings: `{}`", facts.iter().map(|s| safe(s)).collect::<Vec<_>>().join("`, `")));
                }
            }
            content.push_str("\n\nCurrent source declaration only; the class spelling is not a resolved type. Conditional activation, active profiles, configuration imports and effective runtime bean state remain unknown.");
            return Some(Hover::new(content).range(location));
        }
        let (owner, method) = file
            .types
            .iter()
            .flat_map(|t| t.members.iter().map(move |m| (t, m)))
            .find(|(_, m)| {
                m.kind == MemberKind::Method && m.at <= off && off < m.at + m.name.len()
            })?;
        if generic(&doc.text, owner) {
            return None;
        }
        if !method.annotations.iter().any(|a| {
            known(
                &owners,
                &doc.text,
                a,
                "org.springframework.context.annotation.Bean",
            )
        }) {
            return None;
        }
        let location = range(&doc.text, method.at, method.at + method.name.len());
        let bean = self.with_doc(doc, |_, model| {
            let found: Vec<_> = model
                .beans
                .iter()
                .filter(|b| b.kind == "@Bean" && b.range == location)
                .collect();
            (found.len() == 1).then(|| found[0].clone())
        })?;
        if bean.name.is_empty()
            || bean.name.len() > 128
            || bean.name.contains(['\\', '$', '#', '\r', '\n'])
        {
            return None;
        }
        let primary = method.annotations.iter().any(|a| {
            known(
                &owners,
                &doc.text,
                a,
                "org.springframework.context.annotation.Primary",
            )
        });
        let mut profiles = values(
            &owners,
            &doc.text,
            &method.annotations,
            "org.springframework.context.annotation.Profile",
        );
        if !method.annotations.iter().any(|a| {
            known(
                &owners,
                &doc.text,
                a,
                "org.springframework.context.annotation.Profile",
            )
        }) {
            profiles = values(
                &owners,
                &doc.text,
                &owner.annotations,
                "org.springframework.context.annotation.Profile",
            );
        }
        let qualifiers = values(
            &owners,
            &doc.text,
            &method.annotations,
            "org.springframework.beans.factory.annotation.Qualifier",
        );
        let mut content = format!(
            "**Source bean declaration** `{}`\n\nFactory method: `{}`\nDeclared return type spelling: `{}`",
            safe(&bean.name),
            safe(&method.name),
            safe(&bean.ty)
        );
        let aliases: Vec<_> = bean
            .aliases
            .iter()
            .filter(|s| s.len() <= 128 && !s.contains(['\\', '$', '#', '\r', '\n']))
            .take(8)
            .map(|s| safe(s))
            .collect();
        if !aliases.is_empty() {
            content.push_str(&format!(
                "\nDeclared alias spellings: `{}`",
                aliases.join("`, `")
            ));
        }
        if primary {
            content.push_str("\nSource marker: `@Primary`");
        }
        if !profiles.is_empty() {
            content.push_str(&format!(
                "\nDeclared profile spellings: `{}`",
                profiles
                    .iter()
                    .map(|s| safe(s))
                    .collect::<Vec<_>>()
                    .join("`, `")
            ));
        }
        if !qualifiers.is_empty() {
            content.push_str(&format!(
                "\nDeclared qualifier spellings: `{}`",
                qualifiers
                    .iter()
                    .map(|s| safe(s))
                    .collect::<Vec<_>>()
                    .join("`, `")
            ));
        }
        content.push_str("\n\nCurrent source declaration only; the declared type spelling is not resolved. Conditional activation, active profiles, configuration imports and effective runtime bean state remain unknown.");
        Some(Hover::new(content).range(location))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::indexing::DocumentIndexer;
    use openidea_plugin::mock::MockHost;
    const TEXT: &str = "import org.springframework.context.annotation.Bean; import org.springframework.context.annotation.Primary; import org.springframework.context.annotation.Profile; import org.springframework.beans.factory.annotation.Qualifier; @Profile(\"dev\") class Config { /* 😀 */ @Bean(name={\"mail\",\"mailAlias\"}) @Primary @Qualifier(\"outbound\") Mailer mailer() {return null;} }";
    fn doc(text: &str) -> Document {
        Document {
            path: "/project/Config.java".into(),
            language: "java".into(),
            version: 3,
            text: text.into(),
        }
    }
    fn at(doc: &Document) -> Position {
        range(
            &doc.text,
            doc.text.find("mailer()").unwrap(),
            doc.text.find("mailer()").unwrap(),
        )
        .start
    }
    fn class_at(d: &Document) -> Position {
        let off = d.text.find("class Mailer").unwrap() + 6;
        range(&d.text, off, off).start
    }
    const CLASS: &str = "import org.springframework.stereotype.Service; import org.springframework.context.annotation.Primary; /* 😀 */ @Service(\"mail\") @Primary class Mailer {}";
    #[test]
    fn class_declaration_hover_owns_stereotype_name_and_utf16_identifier() {
        let h = MockHost::install().with_capabilities(&["project.read"]);
        let d = doc(CLASS);
        let mut nav = Navigator::default();
        let result = nav.hover(&d, class_at(&d)).unwrap();
        assert!(result.markdown.contains("`mail`") && result.markdown.contains("`@Service`") && result.markdown.contains("`@Primary`"));
        let start = d.text.find("class Mailer").unwrap() + 6;
        assert_eq!(result.range, Some(range(&d.text, start, start + 6)));
        let implicit = doc(&CLASS.replace("@Service(\"mail\")", "@Service"));
        assert!(nav.hover(&implicit, class_at(&implicit)).unwrap().markdown.contains("`mailer`"));
        assert!(h.process_runs().is_empty() && h.requests().is_empty());
    }
    #[test]
    fn class_declaration_hover_rejects_foreign_ambiguous_dynamic_or_nested_owners() {
        let _h = MockHost::install();
        for text in [
            CLASS.replace("org.springframework.stereotype.Service", "foreign.Service"),
            format!("{CLASS} @interface Service {{}}"),
            CLASS.replace("@Service(\"mail\")", "@Service(\"mail\" + SUFFIX)"),
            CLASS.replace("@Service(\"mail\")", "@Service(NAME)"),
            CLASS.replace("class Mailer", "class Mailer<T>"),
            "import org.springframework.stereotype.Service; class Outer { @Service class Mailer {} }".into(),
            CLASS.replace("@Service(\"mail\")", "@Service(\"${name}\")"),
            CLASS.replace("@Service(\"mail\")", "@Service(\"mail\") @org.springframework.stereotype.Component"),
        ] {
            let d = doc(&text);
            assert!(Navigator::default().hover(&d, class_at(&d)).is_none(), "{text}");
        }
        let d = doc(&CLASS.replace("import org.springframework.stereotype.Service;", "").replace("@Service(", "@org.springframework.stereotype.Service("));
        assert!(Navigator::default().hover(&d, class_at(&d)).is_some());
    }
    #[test]
    fn class_hover_replaces_unsaved_facts_and_refuses_foreign_root() {
        let h = MockHost::install();
        let mut nav = Navigator::default();
        let old = doc(CLASS);
        assert!(nav.hover(&old, class_at(&old)).unwrap().markdown.contains("`mail`"));
        let changed = doc(&CLASS.replace("\"mail\"", "\"newMail\"").replace("org.springframework.context.annotation.Primary", "foreign.Primary"));
        let result = nav.hover(&changed, class_at(&changed)).unwrap();
        assert!(result.markdown.contains("`newMail`") && !result.markdown.contains("Source marker:"));
        let dynamic = doc(&CLASS.replace("import org.springframework.context.annotation.Primary;", "import org.springframework.context.annotation.Primary; import org.springframework.context.annotation.Profile; import org.springframework.beans.factory.annotation.Qualifier;")
            .replace("@Primary class", "@Primary @Profile(\"dev\" + SUFFIX) @Qualifier(NAME) class"));
        let result = nav.hover(&dynamic, class_at(&dynamic)).unwrap();
        assert!(!result.markdown.contains("Declared profile") && !result.markdown.contains("Declared qualifier"));
        h.set_project_root(Some("/other"));
        assert!(nav.hover(&old, class_at(&old)).is_none());
    }
    #[test]
    fn source_owned_factory_hover_uses_current_utf16_metadata_with_no_runtime_or_rename_claim() {
        let h = MockHost::install().with_capabilities(&["project.read"]);
        let d = doc(TEXT);
        let mut nav = Navigator::default();
        let hover = nav.hover(&d, at(&d)).unwrap();
        assert!(
            hover.markdown.contains("`mail`")
                && hover.markdown.contains("mailAlias")
                && hover.markdown.contains("outbound")
                && hover.markdown.contains("`dev`")
                && hover.markdown.contains("`@Primary`")
        );
        assert_eq!(
            hover.range,
            Some(range(
                TEXT,
                TEXT.find("mailer()").unwrap(),
                TEXT.find("mailer()").unwrap() + 6
            ))
        );
        let edges = nav.index(&openidea_plugin::indexing::Document {
            path: d.path.clone(),
            symbols: Vec::new(),
            text: d.text.clone(),
        });
        assert!(
            edges.definitions.iter().chain(&edges.references).all(|a| a.key.starts_with("class:") || a.key.starts_with("profile#"))
                && edges.aliases.is_empty(),
            "no bean-name anchor reaches the host index"
        );
        assert!(h.requests().is_empty() && h.process_runs().is_empty());
    }
    #[test]
    fn foreign_shadowed_bean_owner_is_quiet_but_explicit_fqn_keeps_its_owner() {
        let _h = MockHost::install();
        for text in [
            TEXT.replace("org.springframework.context.annotation.Bean", "other.Bean"),
            format!("{TEXT} @interface Bean {{}}"),
            TEXT.replace("import org.springframework.context.annotation.Bean;", ""),
            TEXT.replace("class Config {", "class Config<Bean> {"),
        ] {
            let d = doc(&text);
            assert!(Navigator::default().hover(&d, at(&d)).is_none(), "{text}");
        }
        let text = TEXT
            .replace("@Bean(", "@org.springframework.context.annotation.Bean(")
            .replace("import org.springframework.context.annotation.Bean;", "");
        let d = doc(&text);
        assert!(Navigator::default().hover(&d, at(&d)).is_some());
    }
    #[test]
    fn primary_qualifier_and_profile_markers_require_their_own_exact_source_owners() {
        let _h = MockHost::install();
        let text = TEXT
            .replace(
                "org.springframework.context.annotation.Primary",
                "other.Primary",
            )
            .replace(
                "org.springframework.context.annotation.Profile",
                "other.Profile",
            )
            .replace(
                "org.springframework.beans.factory.annotation.Qualifier",
                "other.Qualifier",
            );
        let d = doc(&text);
        let hover = Navigator::default().hover(&d, at(&d)).unwrap();
        assert!(
            !hover.markdown.contains("Source marker:")
                && !hover.markdown.contains("Declared qualifier")
                && !hover.markdown.contains("Declared profile")
        );
        let text = TEXT.replace("@Profile(\"dev\")", "").replace(
            "@Primary @Qualifier",
            "@Profile(\"${runtime}\") @Primary @Qualifier",
        );
        let d = doc(&text);
        let hover = Navigator::default().hover(&d, at(&d)).unwrap();
        assert!(
            !hover.markdown.contains("${runtime}") && !hover.markdown.contains("Declared profile")
        );
    }
    #[test]
    fn current_unsaved_change_foreign_root_oversize_and_outside_declaration_do_not_leak_stale_hover()
     {
        let h = MockHost::install();
        let mut nav = Navigator::default();
        let old = doc(TEXT);
        assert!(
            nav.hover(&old, at(&old))
                .unwrap()
                .markdown
                .contains("`mail`")
        );
        let changed = Document {
            text: TEXT.replace("\"mail\"", "\"other\""),
            version: 4,
            ..old.clone()
        };
        let new = nav.hover(&changed, at(&changed)).unwrap();
        assert!(
            new.markdown.contains("`other`")
                && !new.markdown.contains("**Source bean declaration** `mail`")
        );
        assert!(nav.hover(&changed, Position::new(0, 0)).is_none());
        let noncanonical = Document {
            path: "/project/../other/Config.java".into(),
            ..old.clone()
        };
        assert!(nav.hover(&noncanonical, at(&noncanonical)).is_none());
        let huge = Document {
            text: format!("{}{}", TEXT, " ".repeat(128 * 1024)),
            ..old.clone()
        };
        assert!(nav.hover(&huge, at(&huge)).is_none());
        h.set_project_root(Some("/other"));
        assert!(nav.hover(&old, at(&old)).is_none());
        h.set_project_root(Some("C:/project"));
        let windows = Document {
            path: "C:/project/Config.java".into(),
            ..old.clone()
        };
        assert!(nav.hover(&windows, at(&windows)).is_some());
        assert!(h.requests().is_empty() && h.process_runs().is_empty());
    }
    #[test]
    fn hover_works_in_file_with_generic_class() {
        let _h = MockHost::install();
        let class = doc(&format!("{CLASS} class Box<T> {{}}"));
        assert!(Navigator::default().hover(&class, class_at(&class)).unwrap().markdown.contains("`mail`"));
        let factory = doc(&format!("{TEXT} class Box<T> {{}}"));
        assert!(Navigator::default().hover(&factory, at(&factory)).unwrap().markdown.contains("`mail`"));
    }
}
