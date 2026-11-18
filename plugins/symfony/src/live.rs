//! Source-local checks only; project discovery and cross-file checks stay outside requests.
use crate::*;
use openidea_plugin::diagnostics::DiagnosticsProvider;
impl DiagnosticsProvider for Main {
    fn diagnostics(&mut self, doc: &Document) -> Vec<Diagnostic> {
        if doc.text.len() > 256 << 10
            || !host::setting_bool("diagnostics").unwrap_or(true)
            || !matches!(
                (doc.language.as_str(), doc.path.rsplit('.').next()),
                ("twig", Some("twig"))
                    | ("yaml", Some("yaml" | "yml"))
                    | ("php", Some("php"))
                    | ("xml", Some("xml"))
            )
        {
            return Vec::new();
        }
        let Some(root)=host::project_of(&doc.path).and_then(|project|project.root().ok()) else {return Vec::new();};
        let Some(cached)=self.indexes.get(&root) else {return Vec::new();};
        // Keep current-document checks available, without any rejected cache facts.
        let unavailable=Index{partial:true,..Index::default()};
        let index=if cached.settings_current() {cached} else {&unavailable};
        let mut groups = Groups::from_settings();
        if doc.language == "twig" && index.custom_twig_lexer() {
            groups.twig = false;
            groups.twig_syntax = false;
        }
        let mut found = if doc.language == "twig" && groups.twig_syntax {
            twig_checks::syntax(&doc.text)
        } else if doc.language == "yaml" && groups.yaml_syntax {
            config_checks::yaml_scalars(&doc.path, &doc.language, &doc.text)
        } else if doc.language == "php" {
            let mut found = Vec::new();
            if groups.controllers { checks::php_route_class(&doc.text, &mut found); }
            if groups.php_calls { found.extend(checks::source_php_resources(index, &doc.text)); }
            if groups.php_calls && !index.partial {
                if let Some(evidence) = index.deprecation_version.as_ref().filter(|e| Some(e.floor) == index.framework_version && e.floor >= (2,8)) {
                    let mut forms = Vec::new();
                    checks::form_type_strings(&doc.text, &mut forms);
                    found.extend(forms.into_iter().map(|mut finding| {
                        finding.message.push_str(&format!(" (based on composer.json constraint {})", evidence.constraint));
                        finding
                    }));
                }
            }
            if groups.commands {
                found.extend(php_checks::commands(&doc.text));
            }
            if groups.practices {
                found.extend(php_checks::practices(&doc.text));
            }
            if groups.deprecations && !index.partial {
                if let Some(evidence) = index
                    .deprecation_version
                    .as_ref()
                    .filter(|e| Some(e.floor) == index.framework_version)
                {
                    let read = |_: &str| None;
                    let ctx = checks::Context::new(index, &read);
                    found.extend(php_checks::deprecations(&ctx,&doc.text).into_iter().map(|mut f| {
                        f.message.push_str(&format!(" (based on composer.json constraint {})",evidence.constraint));
                        f
                    }));
                }
            }
            found.sort_by_key(|finding| finding.start);
            found
        } else {
            Vec::new()
        };
        if doc.language == "twig" && groups.twig {
            found.extend(twig_checks::source_names(index, &doc.text));
            found.extend(twig_checks::source_references(index,&doc.text));
            found.sort_by_key(|finding| finding.start);
        }
        if groups.controllers {
            found.extend(route_duplicates::check(&doc.path, &doc.language, &doc.text).unwrap_or_default());
            let evidence = index.deprecation_version.as_ref().filter(|e| !index.partial && Some(e.floor) == index.framework_version);
            found.extend(checks::source_route_deprecations(&doc.path, &doc.language, &doc.text, evidence).unwrap_or_default());
            found.sort_by_key(|finding| finding.start);
        }
        if groups.services {
            found.extend(checks::service_class_advisories(index,&doc.path,&doc.language,&doc.text));
            found.extend(source_service_duplicates(&doc.path, &doc.language, &doc.text).unwrap_or_default());
            found.extend(checks::source_parameter_duplicates(&doc.path, &doc.language, &doc.text));
            found.extend(config_checks::source_factory_settings(&doc.path, &doc.language, &doc.text).unwrap_or_default());
            let read = |_: &str| None;
            found.extend(config_checks::unknown_parameters(&checks::Context::new(index, &read), &doc.path, &doc.language, &doc.text));
            found.sort_by_key(|finding| finding.start);
        }
        if groups.doctrine {
            found.extend(doctrine::check(&doc.path, &doc.language, &doc.text));
            found.sort_by_key(|finding| finding.start);
        }
        found
            .into_iter()
            .filter(|f| {
                f.start < f.end
                    && f.end <= doc.text.len()
                    && doc.text.is_char_boundary(f.start)
                    && doc.text.is_char_boundary(f.end)
            })
            .take(200)
            .map(|f| Diagnostic {
                code: Some(f.code.into()),
                source: Some(SOURCE.into()),
                ..Diagnostic::new(range_of(&doc.text, f.start, f.end), f.severity, f.message)
            })
            .collect()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    fn setup() -> (MockHost, Main) {
        let host = MockHost::install().with_capabilities(&["project.read"]);
        let mut owner = Main::default();
        owner.indexes.insert("/project".into(), Index::default());
        (host, owner)
    }
    #[test]
    fn diagnostics_run_on_64kib_controller() {
        let (_host, mut owner) = setup();
        let padding = format!("// {}\n", "x".repeat(64 << 10));
        let text = format!("<?php\n#[Route('/x')]\nclass C {{}}\n{padding}");
        let doc = Document { path: "/project/src/C.php".into(), language: "php".into(), version: 1, text };
        assert!(doc.text.len() > 64 << 10);
        assert!(owner.diagnostics(&doc).iter().any(|finding| finding.code.as_deref() == Some("controller-method-missing")));
    }

    #[test]
    fn custom_twig_lexer_skips_symfony_twig_checks() {
        let missing = Document { path: "/project/templates/a.twig".into(), language: "twig".into(), version: 1, text: "{{ path('missing') }}".into() };
        let checked = |lexer: Option<&str>| {
            let (host, mut owner) = setup();
            if let Some(source) = lexer {
                host.add_file("src/Twig/Custom.php", source);
            }
            let mut index = Index::build(&host.invocation().project().unwrap());
            index.partial = false;
            index.compiled_routes = true;
            owner.indexes.insert("/project".into(), index);
            owner.diagnostics(&missing).iter().any(|finding| finding.code.as_deref() == Some("twig-missing-route"))
        };
        let twig = "<?php\nuse Twig\\Lexer;\n$twig->setLexer(new Lexer($twig, ['tag_block' => ['<%', '%>']]));\n";
        let doctrine = "<?php\nuse Doctrine\\ORM\\Query\\Lexer;\n$lexer = new Lexer($dql);\n";
        assert!(checked(None));
        assert!(checked(Some(doctrine)));
        assert!(!checked(Some(twig)));
    }

    #[test]
    fn stale_setting_cache_keeps_source_checks_without_cached_reference_facts() {
        let (host,mut owner)=setup();let mut index=Index::build(&host.invocation().project().unwrap());
        index.partial=false;index.compiled_routes=true;owner.indexes.insert("/project".into(),index);
        let doc=Document{path:"/project/templates/a.twig".into(),language:"twig".into(),version:1,
            text:"{% for x in xs if x %}{% endfor %}{{ path('missing') }}".into()};
        let before=owner.diagnostics(&doc);assert!(before.iter().any(|finding|finding.code.as_deref()==Some("twig-missing-route")));
        host.set_setting("compiledRoutes",r#""custom/routes.php""#);
        let after=owner.diagnostics(&doc);assert_eq!(after.len(),1);assert_eq!(after[0].code.as_deref(),Some("twig-for-condition"));
        host.set_setting("twigSyntaxChecks","false");assert!(owner.diagnostics(&doc).is_empty());
        assert!(host.requests().is_empty() && host.process_runs().is_empty());
    }

    #[test]
    fn stale_setting_cache_keeps_unsaved_factory_check_but_cold_child_stays_quiet() {
        let (host,mut owner)=setup();owner.indexes.insert("/project".into(),Index::build(&host.invocation().project().unwrap()));
        host.set_setting("publicDirectory",r#""web""#);
        let doc=Document{path:"/project/config/services.yaml".into(),language:"yaml".into(),version:1,
            text:"services:\n  foo:\n    factory_class: OldFactory\n".into()};
        let after=owner.diagnostics(&doc);assert_eq!(after.len(),1);assert_eq!(after[0].code.as_deref(),Some("deprecated-factory-setting"));
        host.add_project("/project/config");assert!(owner.diagnostics(&doc).is_empty());
    }

    #[test]
    fn quoted_opaque_markers_do_not_hide_real_refs_and_raw_bodies_offer_no_rewrites() {
        let (_host,mut owner)=setup();let facts=owner.indexes.get_mut("/project").unwrap();facts.compiled_routes=true;
        facts.routes.insert("known".into(),Entry{detail:Some("/known".into()),path:"/project/config/routes.yaml".into(),at:Position::new(0,0)});
        let text=r#"{{ "{# fake #}{% verbatim %}" }}{{ path('missing') }}{% raw %}{{ path('raw_ghost') }}<a href='/known'>Raw</a>{% endraw %}"#;
        let mut doc=Document{path:"/project/templates/a.twig".into(),language:"twig".into(),version:1,text:text.into()};
        let out=owner.diagnostics(&doc);assert_eq!(out.len(),1);assert_eq!(out[0].code.as_deref(),Some("twig-missing-route"));
        assert!(refs::reference_at("twig",text,text.find("raw_ghost").unwrap()).is_none());
        let at=text.find("/known").unwrap();let position=text::position(text,at);
        assert!(owner.code_actions(&doc,Range::new(position,position)).iter().all(|action|!action.title.contains("path(")));
        doc.text=r#"{{ "{# unfinished comment text" }}{{ path('missing') }}"#.into();
        assert_eq!(owner.diagnostics(&doc).len(),1,"quoted comment openers cannot swallow later actual code");
    }

    #[test]
    fn quoted_twig_looking_data_cannot_create_references_named_checks_or_path_edits() {
        let (_host,mut owner)=setup();let facts=owner.indexes.get_mut("/project").unwrap();facts.template_scan_observed=true;facts.compiled_routes=true;
        facts.routes.insert("known".into(),Entry{detail:Some("/known".into()),path:"/project/config/routes.yaml".into(),at:Position::new(0,0)});
        let text=r#"{{ "{% include 'ghost.twig' %}<a href='/known'>app.current_route == 'ghost'</a>{% for x in xs if x %}" }}"#;
        let doc=Document{path:"/project/templates/a.twig".into(),language:"twig".into(),version:1,text:text.into()};
        assert!(owner.diagnostics(&doc).is_empty());assert!(refs::references("twig",text).is_empty());
        let at=text.find("ghost.twig").unwrap();
        assert!(refs::reference_at("twig",text,at).is_none());
        let actions=owner.code_actions(&doc,Range::new(text::position(text,at),text::position(text,at)));
        assert!(actions.iter().all(|action|!action.title.contains("path(")),"HTML-like data cannot offer a path rewrite");
        let partial=Document{text:"{{ path('ghost' }}".into(),..doc};
        assert!(owner.diagnostics(&partial).is_empty(),"unfinished call is not a complete reference fact");
    }

    #[test]
    fn twig_template_absence_is_live_static_and_observation_guarded() {
        let (host,mut owner)=setup();
        let mut doc=Document{path:"/project/templates/a.twig".into(),language:"twig".into(),version:1,
            text:"{# 😀 {{ include('comment.twig') }} #}{{ include('missing.twig') }}{% include 'prefix/' ~ name ~ '.twig' %}{% include 'fallback.twig' ignore missing %}{% verbatim %}{{ include('raw.twig') }}{% endverbatim %}{{ include('@Unknown/x.twig') }}{{ include('Legacy:View:x.twig') }}".into()};
        assert!(owner.diagnostics(&doc).is_empty(),"cold/default metadata is not a discovery witness");
        let facts=owner.indexes.get_mut("/project").unwrap();facts.template_scan_observed=true;
        facts.templates.insert("known.twig".into(),Entry{detail:None,path:"/project/templates/known.twig".into(),at:Position::new(0,0)});
        let out=owner.diagnostics(&doc);assert_eq!(out.len(),2,"ignore missing has static source-file absence, not a runtime-error claim");
        let at=doc.text.find("missing.twig").unwrap();assert_eq!(out[0].range.start,Position::new(0,doc.text[..at].encode_utf16().count() as u32));
        assert!(out.iter().all(|finding|finding.code.as_deref()==Some("missing-template") && finding.message.contains("wasn't found in the project's templates")));
        owner.indexes.get_mut("/project").unwrap().template_paths_unknown=true;assert!(owner.diagnostics(&doc).is_empty());owner.indexes.get_mut("/project").unwrap().template_paths_unknown=false;
        host.set_setting("twigChecks","false");assert!(owner.diagnostics(&doc).is_empty());host.set_setting("twigChecks","true");
        host.add_file("templates/a.twig",&doc.text);owner.check(&host.invocation().project().unwrap(),&doc.path);assert!(host.diagnostics().get(&doc.path).is_none_or(Vec::is_empty));
        doc.text=doc.text.replace("missing.twig","known.twig").replace("fallback.twig","known.twig");doc.version+=1;assert!(owner.diagnostics(&doc).is_empty());
    }

    #[test]
    fn twig_key_absence_uses_known_domains_and_logical_icu_catalogs() {
        let (_host,mut owner)=setup();let facts=owner.indexes.get_mut("/project").unwrap();
        let mut icu=Names::new();icu.insert("icu.key".into(),Entry{detail:None,path:"/project/translations/messages+intl-icu.en.yaml".into(),at:Position::new(0,0)});
        facts.translations.insert("messages".into(),Names::new());facts.translations.insert("messages+intl-icu".into(),icu);facts.translations.insert("admin".into(),Names::new());
        let mut doc=Document{path:"/project/templates/a.twig".into(),language:"twig".into(),version:1,
            text:"{# 😀 #}{{ 'missing'|trans({}, domainName) }}{{ 'icu.key'|trans }}{{ 'other'|trans({}, 'messages') }}{{ 'literal'|transchoice(2, {}, 'admin') }}".into()};
        let out=owner.diagnostics(&doc);assert_eq!(out.len(),2);assert!(out.iter().all(|finding|finding.code.as_deref()==Some("missing-translation") && finding.severity==Severity::Info));
        doc.text="{% trans_default_domain domainName %}{{ 'other'|trans }}".into();assert!(owner.diagnostics(&doc).is_empty());
        doc.text="{% if flag %}{% trans_default_domain 'admin' %}{% endif %}{{ 'other'|trans }}".into();assert!(owner.diagnostics(&doc).is_empty(),"unmodelled scoped directive does not become messages");
        doc.text="{% trans_default_domain 'admin' %}{{ 'other'|trans }}".into();assert_eq!(owner.diagnostics(&doc).len(),1);
        doc.text="{% trans_default_domain domainName %}{{ 'other'|trans(domain: 'messages') }}".into();assert_eq!(owner.diagnostics(&doc).len(),1,"explicit static argument overrides unknown file scope");
        owner.indexes.get_mut("/project").unwrap().partial=true;assert!(owner.diagnostics(&doc).is_empty());
    }

    #[test]
    fn php_literal_bodies_cannot_manufacture_checks_class_routing_or_intention_anchors() {
        let (_host,mut owner)=setup();let facts=owner.indexes.get_mut("/project").unwrap();
        facts.assets.insert("img/known.png".into(),Entry {detail:None,path:"/project/public/img/known.png".into(),at:Position::new(0,0)});
        let mut doc=Document {path:"/project/src/Actual.php".into(),language:"php".into(),version:1,
            text:"<?php\n$literal = <<<'TEXT'\nnamespace Ghost;\n#[Route('/fake')]\nclass Fake {}\n/** @Route('/fake') */\n$builder->add('field', 'text');\nasset('img/ghost.png');\nTEXT;\nclass Actual {}\n".into()};
        assert!(owner.diagnostics(&doc).is_empty());assert!(checks::form_type_literals(&doc.text).is_empty());
        assert_eq!(index::php_class(&doc.text).unwrap().0,"Actual");
        doc.text=doc.text.replace("class Actual {}","/** @Route('/real') */\nclass Actual {}");doc.version+=1;
        let out=owner.diagnostics(&doc);assert_eq!(out.len(),1);assert_eq!(out[0].code.as_deref(),Some("controller-method-missing"));
        doc.text=doc.text.replace("class Actual {}","class Actual { public function __invoke() {} }");assert!(owner.diagnostics(&doc).is_empty());
        doc.text.push_str("\nasset('img/real-missing.png');");assert_eq!(owner.diagnostics(&doc).len(),1,"actual code outside literal still checked");
        let tick=char::from(96);doc.text=format!("<?php $literal = {tick}$builder->add('field','text'); asset('img/shell.png');{tick};");
        assert!(owner.diagnostics(&doc).is_empty());assert!(checks::form_type_literals(&doc.text).is_empty());
    }

    #[test]
    fn cached_service_class_advisories_are_scoped_complete_and_source_current() {
        let (host,mut owner)=setup();
        let facts=owner.indexes.get_mut("/project").unwrap();facts.psr4_prefixes.push("App\\".into());facts.psr4_source_verified=true;
        facts.classes.insert("App\\Known".into(),Entry {detail:None,path:"/project/src/Known.php".into(),at:Position::new(0,0)});
        let mut doc=Document {path:"/project/config/services.yaml".into(),language:"yaml".into(),version:1,text:"services:\n  foo:\n    class: App\\Missing\n".into()};
        owner.indexes.get_mut("/project").unwrap().psr4_source_verified=false;
        assert!(owner.diagnostics(&doc).is_empty(),"a guessed namespace is not literal production provenance");
        host.add_file("config/services.yaml",&doc.text);owner.check(&host.invocation().project().unwrap(),&doc.path);
        assert!(host.diagnostics().get(&doc.path).unwrap().iter().any(|d|d.code.as_deref()==Some("service-class-missing")),"unproven reader retains saved fallback");
        owner.indexes.get_mut("/project").unwrap().psr4_source_verified=true;
        let found=owner.diagnostics(&doc);assert_eq!(found.len(),1);assert_eq!(found[0].code.as_deref(),Some("service-class-missing"));assert!(found[0].message.contains("wasn't found in this project's source"));
        owner.indexes.get_mut("/project").unwrap().partial=true;assert!(owner.diagnostics(&doc).is_empty());owner.indexes.get_mut("/project").unwrap().partial=false;
        doc.text=doc.text.replace("Missing","Known");doc.version+=1;assert!(owner.diagnostics(&doc).is_empty());
        doc.language="xml".into();doc.path="/project/config/services.xml".into();
        doc.text="<container><!-- 😀 <service class='App\\Comment'/> --><services><service id='App\\Implicit' class='App\\Known'/><service id='outer'><argument><service class = 'App\\Missing'/></argument></service></services></container>".into();
        let found=owner.diagnostics(&doc);assert_eq!(found.len(),1,"explicit class wins over ID; nested anonymous service class is checked");
        let at=doc.text.find("App\\Missing").unwrap();assert_eq!(found[0].range.start,Position::new(0,doc.text[..at].encode_utf16().count() as u32));
        host.add_file("config/services.xml",&doc.text);owner.check(&host.invocation().project().unwrap(),&doc.path);assert!(host.diagnostics().get(&doc.path).is_none_or(Vec::is_empty),"source class warnings have one live owner, comments ignored");
        host.set_setting("serviceChecks","false");assert!(owner.diagnostics(&doc).is_empty());host.set_setting("serviceChecks","true");
        doc.text=doc.text.replace("App\\Missing","Vendor\\Unknown");assert!(owner.diagnostics(&doc).is_empty(),"unindexed vendor namespace never supplies negative evidence");
        doc.text="<!DOCTYPE container><container><services><service class='App\\Missing'/></services></container>".into();
        assert!(owner.diagnostics(&doc).is_empty(),"unsupported XML keeps saved fallback");
        host.add_file("config/services.xml",&doc.text);owner.check(&host.invocation().project().unwrap(),&doc.path);
        assert!(host.diagnostics().get(&doc.path).unwrap().iter().any(|d|d.code.as_deref()==Some("service-class-missing")));
        assert!(host.requests().is_empty() && host.process_runs().is_empty());
    }

    #[test]
    fn route_duplicates_are_live_in_supported_xml_yaml_and_resolved_php_source() {
        let (host,mut owner)=setup();
        let cases=[
            ("xml","/project/config/routes.xml","<routes><!-- 😀 --><route id='same'/><route id = 'same'/></routes>","id = 'same'","id = 'other'"),
            ("yaml","/project/config/routes.yaml","same:\n  path: /a\nwhen@test:\n  same:\n    path: /test\nsame:\n  path: /b\n","same:\n  path: /b","other:\n  path: /b"),
            ("php","/project/src/C.php","<?php\nuse Symfony\\Component\\Routing\\Attribute\\Route as R;\nclass C {\n #[R(name: 'same')]\n public function a() {}\n #[R(name : 'same')]\n public function b() {}\n}\n","name : 'same'","name : 'other'"),
        ];
        for (language,path,text,old,new) in cases {
            owner.indexes.get_mut("/project").unwrap().partial=true;
            let mut doc=Document {path:path.into(),language:language.into(),version:1,text:text.into()};
            let out=owner.diagnostics(&doc);assert_eq!(out.len(),1,"{language}: {out:?}");assert_eq!(out[0].code.as_deref(),Some("duplicate-route"));
            host.set_setting("controllerChecks","false");assert!(owner.diagnostics(&doc).is_empty());host.set_setting("controllerChecks","true");
            owner.indexes.get_mut("/project").unwrap().partial=false;
            host.add_file(path.strip_prefix("/project/").unwrap(),text);owner.check(&host.invocation().project().unwrap(),path);
            assert!(host.diagnostics().get(path).is_none_or(Vec::is_empty),"{language}: supported source has one live owner");
            doc.text=doc.text.replace(old,new);doc.version+=1;assert!(owner.diagnostics(&doc).is_empty());
        }
        let legacy="<?php\nclass C {\n #[Route(name: 'same')]\n public function a() {}\n #[Route(name: 'same')]\n public function b() {}\n}\n";
        let doc=Document {path:"/project/src/Legacy.php".into(),language:"php".into(),version:1,text:legacy.into()};
        assert!(owner.diagnostics(&doc).is_empty(),"unresolved route attribute retains saved fallback");
        host.add_file("src/Legacy.php",legacy);owner.check(&host.invocation().project().unwrap(),&doc.path);
        assert!(host.diagnostics().get(&doc.path).unwrap().iter().any(|d|d.code.as_deref()==Some("duplicate-route")));
        host.set_setting("controllerChecks","false");owner.check(&host.invocation().project().unwrap(),&doc.path);assert!(host.diagnostics().get(&doc.path).is_none_or(Vec::is_empty));
        assert!(host.requests().is_empty() && host.process_runs().is_empty());
    }

    #[test]
    fn xml_duplicate_service_ids_are_live_only_for_supported_structural_source() {
        let (host,mut owner)=setup();owner.indexes.get_mut("/project").unwrap().partial=true;
        let mut doc=Document {path:"/project/config/services.xml".into(),language:"xml".into(),version:1,
            text:"<container><!-- 😀 <service id='same'/> --><services><service id='same' note='a > b'/><service id = 'same'/><service id='outer'><argument type='service' id='same'/><service id='same'/></service></services></container>".into()};
        let found=owner.diagnostics(&doc);assert_eq!(found.len(),1);assert_eq!(found[0].code.as_deref(),Some("duplicate-service"));
        let at=doc.text.find("id = 'same'").unwrap()+6;assert_eq!(found[0].range.start,Position::new(0,doc.text[..at].encode_utf16().count() as u32));
        owner.indexes.get_mut("/project").unwrap().partial=false;host.add_file("config/services.xml",&doc.text);owner.check(&host.invocation().project().unwrap(),&doc.path);
        assert!(host.diagnostics().get(&doc.path).is_none_or(Vec::is_empty));
        doc.text=doc.text.replace("id = 'same'","id = 'different'");doc.version+=1;assert!(owner.diagnostics(&doc).is_empty());
        doc.text="<!DOCTYPE container><container><services><service id='same'/><service id='same'/></services></container>".into();
        assert!(owner.diagnostics(&doc).is_empty(),"unsupported DTD source retains saved fallback only");host.add_file("config/services.xml",&doc.text);owner.check(&host.invocation().project().unwrap(),&doc.path);
        assert!(host.diagnostics().get(&doc.path).unwrap().iter().any(|d|d.code.as_deref()==Some("duplicate-service")));
        host.set_setting("serviceChecks","false");owner.check(&host.invocation().project().unwrap(),&doc.path);
        assert!(host.diagnostics().get(&doc.path).is_none_or(Vec::is_empty));
        assert!(host.requests().is_empty() && host.process_runs().is_empty());
    }

    #[test]
    fn twig_cached_source_checks_are_live_guarded_and_avoid_named_package_guesses() {
        let (host,mut owner)=setup();
        let mut doc=Document {path:"/project/templates/a.html.twig".into(),language:"twig".into(),version:1,
            text:"{# 😀 {{ path('comment') }} #}{{ path('ghost_route') }}{{ asset('img/missing.png') }}{{ asset('img/package.png', 'cdn') }}{{ 'key'|trans({}, 'ghost_domain') }}{% if app.current_route == 'ghost_route' %}{% endif %}<a href=\"/known\">Known</a>{% verbatim %}{{ path('raw') }}{% endverbatim %}".into()};
        assert!(owner.diagnostics(&doc).is_empty());
        let facts=owner.indexes.get_mut("/project").unwrap();facts.compiled_routes=true;
        facts.routes.insert("known".into(),Entry {detail:Some("/known".into()),path:"/project/config/routes.yaml".into(),at:Position::new(0,0)});
        facts.assets.insert("img/known.png".into(),Entry {detail:None,path:"/project/public/img/known.png".into(),at:Position::new(0,0)});
        let mut messages=Names::new();messages.insert("key".into(),Entry{detail:None,path:"/project/translations/messages.en.yaml".into(),at:Position::new(0,0)});
        facts.translations.insert("messages".into(),messages);
        let found=owner.diagnostics(&doc);assert_eq!(found.len(),5,"{found:?}");
        assert!(found.iter().all(|d|d.message.contains("From the last project scan")));
        let at=doc.text.find("ghost_route").unwrap();assert_eq!(found[0].range.start,Position::new(0,doc.text[..at].encode_utf16().count() as u32));
        owner.indexes.get_mut("/project").unwrap().partial=true;assert!(owner.diagnostics(&doc).is_empty());owner.indexes.get_mut("/project").unwrap().partial=false;
        host.set_setting("twigChecks","false");assert!(owner.diagnostics(&doc).is_empty());host.set_setting("twigChecks","true");
        host.add_file("templates/a.html.twig",&doc.text);owner.check(&host.invocation().project().unwrap(),&doc.path);
        assert!(host.diagnostics().get(&doc.path).is_none_or(Vec::is_empty));
        doc.text=doc.text.replace("ghost_route","known").replace("img/missing.png","img/known.png").replace("ghost_domain","messages").replace("href=\"/known\"","href=\"{{ path('known') }}\"");doc.version+=1;assert!(owner.diagnostics(&doc).is_empty());
        assert!(host.requests().is_empty() && host.process_runs().is_empty());
    }

    #[test]
    fn php_route_class_check_is_current_source_owned_and_plain_class_bounded() {
        let (host,mut owner)=setup();owner.indexes.get_mut("/project").unwrap().partial=true;
        let mut doc=Document {path:"/project/src/Controller/Only.php".into(),language:"php".into(),version:1,
            text:"<?php /* 😀 */\n#[Route('/only')]\nclass Only {}".into()};
        let found=owner.diagnostics(&doc);assert_eq!(found.len(),1);assert_eq!(found[0].code.as_deref(),Some("controller-method-missing"));
        assert_eq!(found[0].range.start,Position::new(2,6));
        doc.text=doc.text.replace("{}","{ public function __invoke() {} }");doc.version+=1;assert!(owner.diagnostics(&doc).is_empty());
        doc.text="<?php\n#[Route('/only')]\nclass Only extends ParentController {}".into();assert!(owner.diagnostics(&doc).is_empty(),"unknown inherited method stays quiet");
        doc.text="<?php\n#[Route('/only')]\nabstract class Only {}".into();assert!(owner.diagnostics(&doc).is_empty());
        doc.text="<?php\n#[Route('/only')]\nclass Only {}".into();host.set_setting("controllerChecks","false");assert!(owner.diagnostics(&doc).is_empty());
        host.set_setting("controllerChecks","true");owner.indexes.get_mut("/project").unwrap().partial=false;
        host.add_file("src/Controller/Only.php",&doc.text);owner.check(&host.invocation().project().unwrap(),&doc.path);
        assert!(host.diagnostics().get(&doc.path).is_none_or(|items|items.iter().all(|d|d.code.as_deref()!=Some("controller-method-missing"))));
        assert!(host.requests().is_empty() && host.process_runs().is_empty());
    }

    #[test]
    fn php_resource_compound_expressions_are_not_guessed_as_literal_paths_or_domains() {
        let (_host,mut owner)=setup();
        let facts=owner.indexes.get_mut("/project").unwrap();
        facts.assets.insert("img/known.png".into(),Entry {detail:None,path:"/project/public/img/known.png".into(),at:Position::new(0,0)});
        facts.translations.insert("messages".into(),Names::new());
        let mut doc=Document {path:"/project/src/Resources.php".into(),language:"php".into(),version:1,
            text:"<?php asset('img/' . 'missing.png'); $translator->trans('key', [], 'missing' . 'domain');".into()};
        assert!(owner.diagnostics(&doc).is_empty(),"compound arguments are not one literal");
        doc.text="<?php asset('img/missing.png'); $translator->trans('key', [], 'missingdomain');".into();doc.version+=1;
        assert_eq!(owner.diagnostics(&doc).len(),2,"equivalent literal source still checked");
    }

    #[test]
    fn php_resource_warnings_use_complete_cached_facts_and_current_code_anchors() {
        let (host,mut owner)=setup();
        let mut doc=Document {path:"/project/src/Resources.php".into(),language:"php".into(),version:1,
            text:"<?php /* 😀 */ asset('img/missing.png'); $translator->trans('key', [], 'missing'); // asset('img/comment.png'); $translator->trans('x', [], 'comment');\n$quoted = \"asset('img/string.png');\"; asset('build/generated.js'); asset('unseen/quiet.png');".into()};
        assert!(owner.diagnostics(&doc).is_empty());
        let facts=owner.indexes.get_mut("/project").unwrap();
        facts.assets.insert("img/known.png".into(),Entry {detail:None,path:"/project/public/img/known.png".into(),at:Position::new(0,0)});
        facts.translations.insert("messages".into(),Names::new());
        let found=owner.diagnostics(&doc);assert_eq!(found.len(),2);
        assert!(found.iter().all(|d|d.message.contains("From the last project scan")));
        assert_eq!(found[0].range.start,Position::new(0,doc.text[..doc.text.find("img/missing.png").unwrap()].encode_utf16().count() as u32));
        owner.indexes.get_mut("/project").unwrap().partial=true;assert!(owner.diagnostics(&doc).is_empty());
        owner.indexes.get_mut("/project").unwrap().partial=false;
        host.set_setting("phpCallChecks","false");assert!(owner.diagnostics(&doc).is_empty());host.set_setting("phpCallChecks","true");
        host.add_file("src/Resources.php",&doc.text);owner.check(&host.invocation().project().unwrap(),&doc.path);
        assert!(host.diagnostics().get(&doc.path).is_none_or(|items|items.iter().all(|d|!matches!(d.code.as_deref(),Some("missing-asset"|"missing-translation-domain")))));
        doc.text=doc.text.replace("img/missing.png","img/known.png").replace("'missing'","'messages'");doc.version+=1;assert!(owner.diagnostics(&doc).is_empty());
        doc.text="<?php $service->getUrl('img/missing.png'); // Packages".into();assert!(owner.diagnostics(&doc).is_empty(),"comment-only asset receiver hint");
        assert!(host.requests().is_empty() && host.process_runs().is_empty());
    }

    #[test]
    fn form_alias_warning_is_live_version_gated_and_ignores_commented_calls() {
        let (host,mut owner)=setup();
        let mut doc=Document {path:"/project/src/Form/Legacy.php".into(),language:"php".into(),version:1,
            text:"<?php /* 😀 */ $builder->add('field', 'text'); // $builder->add('x', 'textarea');\n$quoted = \"$builder->add('x', 'text');\";".into()};
        assert!(owner.diagnostics(&doc).is_empty());
        for (constraint,count) in [("^2.7",0),("^2.8",1),("^7.0",1)] {
            let evidence=index::VersionEvidence::from_composer(&format!(r#"{{"require":{{"symfony/framework-bundle":"{constraint}"}}}}"#)).unwrap();
            let facts=owner.indexes.get_mut("/project").unwrap();facts.framework_version=Some(evidence.floor);facts.deprecation_version=Some(evidence);
            let found=owner.diagnostics(&doc);assert_eq!(found.len(),count);
            if count>0 {assert_eq!(found[0].code.as_deref(),Some("form-type-string"));assert!(found[0].message.contains("(based on composer.json constraint "));}
        }
        owner.indexes.get_mut("/project").unwrap().partial=true;assert!(owner.diagnostics(&doc).is_empty());
        owner.indexes.get_mut("/project").unwrap().partial=false;host.set_setting("phpCallChecks","false");assert!(owner.diagnostics(&doc).is_empty());
        host.set_setting("phpCallChecks","true");doc.text=doc.text.replace("'field', 'text'","'field', TextType::class");doc.version+=1;assert!(owner.diagnostics(&doc).is_empty());
        doc.text="<?php $logger->add('field', 'text'); // $builder FormBuilderInterface AbstractType".into();
        assert!(owner.diagnostics(&doc).is_empty(), "comment-only source hints do not enable warning");
        assert!(host.requests().is_empty() && host.process_runs().is_empty());
    }

    #[test]
    fn xml_legacy_requirement_keys_use_structural_unsaved_route_context() {
        let (_host, mut owner)=setup();owner.indexes.get_mut("/project").unwrap().partial=true;
        let mut doc=Document {path:"/project/config/routes.xml".into(),language:"xml".into(),version:1,
            text:"<routes><!-- 😀 <requirement key='_method'/> --><route id='blog' note='a > b'><requirement key = '_method'>GET</requirement><requirement key='_scheme'>https</requirement><default key='_method'/></route><requirement key='_method'/></routes>".into()};
        let out=owner.diagnostics(&doc);assert_eq!(out.len(),2);assert!(out.iter().all(|d|d.code.as_deref()==Some("deprecated-route-setting")));
        let at=doc.text.find("key = '_method'").unwrap()+7;
        assert_eq!(out[0].range.start,Position::new(0,doc.text[..at].encode_utf16().count() as u32));
        doc.text=doc.text.replace("<requirement key = '_method'>GET</requirement>","<requirement key = 'locale'>en</requirement>").replace("key='_scheme'","key='host'");doc.version+=1;
        assert!(owner.diagnostics(&doc).is_empty());
    }

    #[test]
    fn route_source_deprecations_are_live_with_guarded_annotation_provenance() {
        let (host, mut owner)=setup();
        owner.indexes.get_mut("/project").unwrap().partial=true;
        let mut doc=Document {path:"/project/config/routes.yaml".into(),language:"yaml".into(),version:1,
            text:"# 😀\nblog:\n  pattern: /old\n  type: annotation\n  requirements:\n    _method: GET\n".into()};
        let found=owner.diagnostics(&doc);assert_eq!(found.len(),2,"unconditional source facts only");
        assert_eq!(found[0].range.start,Position::new(2,2));
        let evidence=index::VersionEvidence::from_composer(r#"{"require":{"symfony/framework-bundle":"^6.4"}}"#).unwrap();
        let facts=owner.indexes.get_mut("/project").unwrap();facts.partial=false;facts.framework_version=Some(evidence.floor);facts.deprecation_version=Some(evidence);
        let found=owner.diagnostics(&doc);assert_eq!(found.len(),3);
        assert!(found.iter().any(|d|d.message.contains("(based on composer.json constraint ^6.4)")));
        owner.indexes.get_mut("/project").unwrap().framework_version=Some((8,1));assert_eq!(owner.diagnostics(&doc).len(),2,"mismatched loose hint cannot enable annotation warning");
        doc.text="blog:\n  path: /new\n  type: attribute\n  methods: [GET]\n".into();doc.version+=1;assert!(owner.diagnostics(&doc).is_empty());
        doc.path="/project/config/routes.xml".into();doc.language="xml".into();doc.text="<routes><!-- 😀 --><route id=\"blog\" pattern=\"/old\"/></routes>".into();
        let found=owner.diagnostics(&doc);assert_eq!(found.len(),1);
        host.set_setting("controllerChecks","false");assert!(owner.diagnostics(&doc).is_empty());
        host.set_setting("controllerChecks","true");doc.path="/foreign/config/routes.xml".into();assert!(owner.diagnostics(&doc).is_empty());
        assert!(host.requests().is_empty() && host.process_runs().is_empty());
    }

    #[test]
    fn structural_xml_route_settings_use_exact_name_and_value_anchors() {
        let (_host,mut owner)=setup();let evidence=index::VersionEvidence::from_composer(r#"{"require":{"symfony/framework-bundle":"^6.4"}}"#).unwrap();
        let facts=owner.indexes.get_mut("/project").unwrap();facts.framework_version=Some(evidence.floor);facts.deprecation_version=Some(evidence);
        let doc=Document{path:"/project/config/routes.xml".into(),language:"xml".into(),version:1,
            text:"<routes><!-- 😀 <route pattern='comment'/> --><![CDATA[<route pattern='data'/><import type='annotation'/>]]><wrong><route pattern='nested'/><import type='annotation'/></wrong><route id='real' pattern = '/' controller='Blog:Post:index'/><import resource='x' type='annotation'/></routes>".into()};
        let found=owner.diagnostics(&doc);assert_eq!(found.len(),3);
        let start=text::offset(&doc.text,found[0].range.start).unwrap();let end=text::offset(&doc.text,found[0].range.end).unwrap();
        assert_eq!(&doc.text[start..end],"pattern");assert_eq!(start,doc.text.find("pattern =").unwrap());
        let start=text::offset(&doc.text,found[1].range.start).unwrap();let end=text::offset(&doc.text,found[1].range.end).unwrap();assert_eq!(&doc.text[start..end],"Blog:Post:index");
        assert!(found[2].message.contains("(based on composer.json constraint "));
    }

    #[test]
    fn unsupported_route_xml_keeps_saved_deprecation_fallback_only() {
        let (host,mut owner)=setup();let path="/project/config/routes.xml";
        let text="<!DOCTYPE routes><routes><route id='legacy' pattern='/old'/></routes>";
        let doc=Document{path:path.into(),language:"xml".into(),version:1,text:text.into()};
        assert!(owner.diagnostics(&doc).is_empty());host.add_file("config/routes.xml",text);owner.check(&host.invocation().project().unwrap(),path);
        assert_eq!(host.diagnostics()[path].iter().filter(|finding|finding.code.as_deref()==Some("deprecated-route-setting")).count(),1);
    }

    #[test]
    fn duplicate_parameters_are_source_owned_and_clear_on_unsaved_fix() {
        let (host, mut owner) = setup();
        owner.indexes.get_mut("/project").unwrap().partial = true;
        let mut doc = Document { path:"/project/config/services.yaml".into(), language:"yaml".into(), version:1,
            text:"# 😀\r\nparameters:\r\n  first: 1\r\n  first: 2\r\n".into() };
        let found=owner.diagnostics(&doc);assert_eq!(found.len(),1);
        assert_eq!(found[0].code.as_deref(),Some("duplicate-parameter-key"));
        assert_eq!(found[0].range.start,Position::new(3,2));
        doc.text=doc.text.replace("first: 2","second: 2");doc.version+=1;
        assert!(owner.diagnostics(&doc).is_empty());
        doc.path="/project/config/services.xml".into();doc.language="xml".into();
        doc.text="<container><!-- 😀 --><parameters><parameter key=\"x\">1</parameter><parameter key=\"x\">2</parameter></parameters></container>".into();
        let found=owner.diagnostics(&doc);assert_eq!(found.len(),1);
        let at=doc.text.rfind("x\"").unwrap();assert_eq!(found[0].range.start,Position::new(0,doc.text[..at].encode_utf16().count() as u32));
        host.set_setting("serviceChecks","false");assert!(owner.diagnostics(&doc).is_empty());
        host.set_setting("serviceChecks","true");doc.path="/foreign/config/services.xml".into();assert!(owner.diagnostics(&doc).is_empty());
        assert!(host.diagnostics().is_empty());
        assert!(host.requests().is_empty() && host.process_runs().is_empty());
    }

    #[test]
    fn cached_unknown_parameters_respect_completeness_and_unsaved_local_definitions() {
        let (host, mut owner) = setup();
        let mut doc = Document { path: "/project/config/services.yaml".into(), language: "yaml".into(), version: 1,
            text: "# 😀\nservices:\n  foo:\n    arguments: ['%new.parameter%']\n".into() };
        assert!(owner.diagnostics(&doc).is_empty(), "no compiled-container negative evidence");
        owner.indexes.get_mut("/project").unwrap().compiled_container = true;
        let found = owner.diagnostics(&doc);
        assert_eq!(found.len(),1);
        assert_eq!(found[0].code.as_deref(), Some("unknown-parameter"));
        doc.version += 1;
        doc.text.push_str("parameters:\n  new.parameter: value\n");
        assert!(owner.diagnostics(&doc).is_empty(), "unsaved local parameter resolves without index adoption");
        doc.language="xml".into();doc.path="/project/config/services.xml".into();
        doc.text="<container><!-- 😀 --><services><service id=\"foo\"><argument>%new.parameter%</argument></service></services></container>".into();
        let found=owner.diagnostics(&doc);assert_eq!(found.len(),1);
        let at=doc.text.find("new.parameter").unwrap();
        assert_eq!(found[0].range.start,Position::new(0,doc.text[..at].encode_utf16().count() as u32));
        doc.text=doc.text.replace("</container>","<parameters><parameter key=\"new.parameter\">value</parameter></parameters></container>");
        assert!(owner.diagnostics(&doc).is_empty());
        doc.text=doc.text.replace("key=\"new.parameter\"", "key=\"other\"");
        owner.indexes.get_mut("/project").unwrap().partial=true;
        assert!(owner.diagnostics(&doc).is_empty());
        owner.indexes.get_mut("/project").unwrap().partial=false;
        host.set_setting("serviceChecks","false");assert!(owner.diagnostics(&doc).is_empty());
        host.set_setting("serviceChecks","true");
        host.add_file("config/services.xml",&doc.text);
        owner.check(&host.invocation().project().unwrap(),&doc.path);
        assert!(host.diagnostics().get(&doc.path).is_none_or(|items|items.iter().all(|d|d.code.as_deref()!=Some("unknown-parameter"))));
        assert!(host.requests().is_empty() && host.process_runs().is_empty());
    }

    #[test]
    fn source_xml_factory_attributes_require_real_service_structure() {
        let (_host,mut owner)=setup();
        let doc=Document{path:"/project/config/services.xml".into(),language:"xml".into(),version:1,
            text:"<container><![CDATA[<service factory-class='Fake'/>]]><parameters><service factory-method='wrong'/></parameters><services><!-- <service factory-service='comment'/> --><service id='real' factory-class = 'Real'><argument><service factory-method='nested'/></argument></service></services></container>".into()};
        let found=owner.diagnostics(&doc);assert_eq!(found.len(),2);
        for finding in found {let start=text::offset(&doc.text,finding.range.start).unwrap();assert!(start==doc.text.find("factory-class =").unwrap() || start==doc.text.find("factory-method='nested'").unwrap());}
        for text in ["<container><services><service factory-class='incomplete'/>","<!DOCTYPE container><container><services><service factory-class='unsupported'/></services></container>","<c:container><c:services><c:service factory-class='unknown'/></c:services></c:container>"] {
            assert!(owner.diagnostics(&Document{text:text.into(),..doc.clone()}).is_empty());
        }
    }

    #[test]
    fn unsupported_factory_xml_retains_saved_fallback_without_live_duplicate() {
        let (host,mut owner)=setup();
        let path="/project/config/services.xml";let text="<!DOCTYPE container><container><services><service factory-class='Legacy'/></services></container>";
        host.add_file("config/services.xml",text);
        let doc=Document{path:path.into(),language:"xml".into(),version:1,text:text.into()};
        assert!(owner.diagnostics(&doc).is_empty());owner.check(&host.invocation().project().unwrap(),path);
        assert_eq!(host.diagnostics()[path].iter().filter(|finding|finding.code.as_deref()==Some("deprecated-factory-setting")).count(),1);
    }

    #[test]
    fn deprecated_factory_settings_use_unsaved_source_without_manual_duplicates() {
        let (host, mut owner) = setup();
        owner.indexes.get_mut("/project").unwrap().partial = true;
        let mut doc = Document { path: "/project/config/services.yaml".into(), language: "yaml".into(), version: 1,
            text: "# 😀\nservices:\n  foo:\n    factory_class: OldFactory\n".into() };
        let found = owner.diagnostics(&doc);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].code.as_deref(), Some("deprecated-factory-setting"));
        assert_eq!(found[0].range.start, Position::new(3,4));
        doc.version += 1;
        doc.text = doc.text.replace("factory_class", "factory");
        assert!(owner.diagnostics(&doc).is_empty());
        doc.language = "xml".into(); doc.path = "/project/config/services.xml".into();
        doc.text = "<container><!-- 😀 --><services><service id=\"foo\" factory-method=\"create\"/></services></container>".into();
        let found = owner.diagnostics(&doc);
        assert_eq!(found.len(), 1);
        let start = doc.text.find("factory-method").unwrap();
        assert_eq!(found[0].range.start, Position::new(0,doc.text[..start].encode_utf16().count() as u32));
        host.set_setting("serviceChecks", "false");
        assert!(owner.diagnostics(&doc).is_empty());
        host.set_setting("serviceChecks", "true");
        doc.path = "/foreign/config/services.xml".into();
        assert!(owner.diagnostics(&doc).is_empty());
        doc.path = "/project/config/services.xml".into();
        owner.indexes.get_mut("/project").unwrap().partial = false;
        host.add_file("config/services.xml", &doc.text);
        owner.check(&host.invocation().project().unwrap(), &doc.path);
        assert!(host.diagnostics().get(&doc.path).is_none_or(|items|items.iter().all(|d|d.code.as_deref()!=Some("deprecated-factory-setting"))));
        assert!(host.requests().is_empty() && host.process_runs().is_empty());
    }

    #[test]
    fn unsaved_yaml_and_twig_rules_are_versioned_source_facts_with_utf16_ranges() {
        let (_host, mut owner) = setup();
        let mut doc = Document {
            path: "/project/config/services.yaml".into(),
            language: "yaml".into(),
            version: 3,
            text: "# 😀\nservices:\n  value: @missing\n".into(),
        };
        let out = owner.diagnostics(&doc);
        assert!(
            out.iter()
                .any(|f| f.code.as_deref() == Some("unquoted-yaml-scalar"))
        );
        let f = out
            .iter()
            .find(|f| f.code.as_deref() == Some("unquoted-yaml-scalar"))
            .unwrap();
        assert_eq!(f.range.start, Position::new(2, 9));
        doc.text = doc.text.replace("@missing", "'@missing'");
        doc.version += 1;
        assert!(owner.diagnostics(&doc).is_empty());
        doc.path = "/project/templates/a.twig".into();
        doc.language = "twig".into();
        doc.text = "{% for item in items if item.enabled %}{{ item }}{% endfor %}".into();
        assert!(
            owner
                .diagnostics(&doc)
                .iter()
                .any(|f| f.code.as_deref() == Some("twig-for-condition"))
        );
    }
    #[test]
    fn live_rules_quiet_for_foreign_cold_disabled_and_oversized_documents() {
        let (host, mut owner) = setup();
        let mut doc = Document {
            path: "/project/config/services.yaml".into(),
            language: "yaml".into(),
            version: 1,
            text: "x: @bad\n".into(),
        };
        host.set_setting("yamlSyntaxChecks", "false");
        assert!(owner.diagnostics(&doc).is_empty());
        host.set_setting("yamlSyntaxChecks", "true");
        doc.text = "x".repeat(32 * 1024 + 1);
        assert!(owner.diagnostics(&doc).is_empty());
        doc.text = "x: @bad\n".into();
        host.set_project_root(Some("/other"));
        assert!(owner.diagnostics(&doc).is_empty());
        host.set_project_root(Some("/project"));
        owner.indexes.clear();
        assert!(owner.diagnostics(&doc).is_empty());
    }

    #[test]
    fn php_commands_use_unsaved_owned_text_and_respect_the_group() {
        let (host, mut owner) = setup();
        let text = "<?php\n// 😀\nuse Symfony\\Component\\Console\\Attribute\\AsCommand;\n#[AsCommand(name: 'app:test')]\nclass Command { public function __invoke() {} }";
        let mut doc = Document {
            path: "/project/src/Command.php".into(),
            language: "php".into(),
            version: 1,
            text: text.into(),
        };
        host.add_file(
            "src/Command.php",
            &text.replace("__invoke()", "__invoke(): int"),
        );
        let out = owner.diagnostics(&doc);
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].code.as_deref(),
            Some("command-invokable-return-type")
        );
        assert_eq!(out[0].range.start, Position::new(4, 32));
        assert!(host.diagnostics().is_empty());
        host.set_setting("commandChecks", "false");
        assert!(owner.diagnostics(&doc).is_empty());
        host.set_setting("commandChecks", "true");
        doc.text = doc.text.replace("__invoke()", "__invoke(): int");
        doc.version += 1;
        assert!(owner.diagnostics(&doc).is_empty());
    }

    #[test]
    fn cached_version_deprecations_require_complete_supported_evidence_and_remain_advisory() {
        let (host, mut owner) = setup();
        let bad = "<?php use Symfony\\Component\\Filesystem\\Filesystem; $fs->mirror('a','b',null,['copy_on_windows' => true]);";
        let mut doc = Document {
            path: "/project/src/Fs.php".into(),
            language: "php".into(),
            version: 4,
            text: bad.into(),
        };
        assert!(owner.diagnostics(&doc).is_empty());
        let evidence = index::VersionEvidence::from_composer(
            r#"{"require":{"symfony/framework-bundle":"^8.1"}}"#,
        )
        .unwrap();
        owner.indexes.get_mut("/project").unwrap().framework_version = Some(evidence.floor);
        owner
            .indexes
            .get_mut("/project")
            .unwrap()
            .deprecation_version = Some(evidence);
        // A changed disk constraint is not read during the current-document request.
        host.add_file(
            "composer.json",
            r#"{"require":{"symfony/framework-bundle":"^6.4"}}"#,
        );
        let found = owner.diagnostics(&doc);
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].code.as_deref(),
            Some("filesystem-mirror-copy-on-windows")
        );
        assert!(found[0].message.contains("(based on composer.json constraint "));
        assert!(
            found[0]
                .message
                .contains("constraint ^8.1)")
        );
        assert!(host.diagnostics().is_empty());
        host.set_setting("deprecationChecks", "false");
        assert!(owner.diagnostics(&doc).is_empty());
        host.set_setting("deprecationChecks", "true");
        owner.indexes.get_mut("/project").unwrap().partial = true;
        assert!(owner.diagnostics(&doc).is_empty());
        owner.indexes.get_mut("/project").unwrap().partial = false;
        owner.indexes.get_mut("/project").unwrap().framework_version = Some((6, 4));
        assert!(owner.diagnostics(&doc).is_empty());
        owner.indexes.get_mut("/project").unwrap().framework_version = Some((8, 1));
        doc.version += 1;
        doc.text =
            "<?php use Symfony\\Component\\Filesystem\\Filesystem; $fs->mirror('a','b');".into();
        assert!(owner.diagnostics(&doc).is_empty());
    }

    #[test]
    fn composer_file_events_refresh_or_clear_version_evidence_outside_provider_requests() {
        let host = MockHost::install();
        host.add_file(
            "composer.json",
            r#"{"require":{"symfony/framework-bundle":"^8.1"}}"#,
        );
        let mut owner = Main::default();
        owner.activate(&host.context("aritti.symfony")).unwrap();
        let doc=Document { path:"/project/src/Fs.php".into(),language:"php".into(),version:1,
            text:"<?php use Symfony\\Component\\Filesystem\\Filesystem; $fs->mirror('a','b',null,['copy_on_windows' => true]);".into() };
        assert_eq!(owner.diagnostics(&doc).len(), 1);
        host.add_file(
            "composer.json",
            r#"{"require":{"symfony/framework-bundle":"^6.4 || ^8.1"}}"#,
        );
        owner.handle_events(
            &[Event::FileSaved("/project/composer.json".into())],
            &host.invocation(),
        );
        assert!(owner.diagnostics(&doc).is_empty());
        host.add_file(
            "composer.json",
            r#"{"require":{"symfony/framework-bundle":"8.1.*"}}"#,
        );
        owner.handle_events(
            &[Event::FileSaved("/project/composer.json".into())],
            &host.invocation(),
        );
        assert!(
            owner.diagnostics(&doc)[0]
                .message
                .contains("constraint 8.1.*)")
        );
        host.set_project_root(Some("/other"));
        assert!(owner.diagnostics(&doc).is_empty());
    }

    #[test]
    fn source_only_php_practices_are_bounded_and_quiet_after_unsaved_fix() {
        let (host, mut owner) = setup();
        owner.indexes.get_mut("/project").unwrap().partial = true;
        let mut doc = Document {
            path: "/project/src/Legacy.php".into(),
            language: "php".into(),
            version: 1,
            text: format!("<?php {}", "Twig_Environment; ".repeat(1000)),
        };
        let found = owner.diagnostics(&doc);
        assert_eq!(found.len(), 200);
        assert!(
            found
                .iter()
                .all(|f| f.code.as_deref() == Some("twig-legacy-class"))
        );
        assert!(host.diagnostics().is_empty());
        host.set_setting("practiceChecks", "false");
        assert!(owner.diagnostics(&doc).is_empty());
        host.set_setting("practiceChecks", "true");
        doc.version += 1;
        doc.text = "<?php new \\Twig\\Environment();".into();
        assert!(owner.diagnostics(&doc).is_empty());
    }

    #[test]
    fn partial_cache_allows_only_source_facts_and_settings_control_each_group() {
        let (host, mut owner) = setup();
        owner.indexes.get_mut("/project").unwrap().partial = true;
        let doc = Document {
            path: "/project/templates/a.twig".into(),
            language: "twig".into(),
            version: 7,
            text: "{% for a in b if c %}{% endfor %}{{ path('unknown') }}".into(),
        };
        assert_eq!(owner.diagnostics(&doc).len(), 1);
        host.set_setting("twigSyntaxChecks", "false");
        assert!(owner.diagnostics(&doc).is_empty());
        host.set_setting("twigSyntaxChecks", "true");
        host.set_setting("diagnostics", "false");
        assert!(owner.diagnostics(&doc).is_empty());
    }

    #[test]
    fn live_provider_does_not_publish_manual_diagnostics_or_read_saved_text() {
        let (host, mut owner) = setup();
        host.add_file("config/services.yaml", "saved: '@valid'\n");
        let doc = Document {
            path: "/project/config/services.yaml".into(),
            language: "yaml".into(),
            version: 9,
            text: "unsaved: @bad\n".into(),
        };
        assert_eq!(owner.diagnostics(&doc).len(), 1);
        assert!(host.diagnostics().is_empty());
        let foreign = Document {
            path: "/project/unrelated.yaml".into(),
            ..doc
        };
        assert!(owner.diagnostics(&foreign).is_empty());
    }
}
