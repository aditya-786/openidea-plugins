//! Cheap cached inventory checks use current versioned text; file-reading rule families remain manual.
use crate::*;
use openidea_plugin::diagnostics::DiagnosticsProvider;

impl DiagnosticsProvider for Main {
    fn diagnostics(&mut self, doc: &Document) -> Vec<Diagnostic> {
        self.advance_document_index(&doc.path);
        let metadata_document=doc.path.ends_with("/ide.json");
        if (!metadata_document&&doc.text.len() > 32 * 1024) || !hover::canonical(&doc.path)
            || !host::setting_bool("diagnostics").unwrap_or(true)
            || ["/storage/", "/bootstrap/cache/", "/node_modules/"]
                .iter().any(|part| doc.path.contains(part)) || !metadata_document&&doc.path.contains("/vendor/") { return Vec::new(); }
        let Some(root) = project_for(&doc.path).and_then(|project| project.root().ok()) else { return Vec::new() };
        let Some(index) = self.indexes.get(&root) else { return Vec::new() };
        if metadata_document {
            return metadata::diagnostics_current(index, &doc.path, &root,&doc.text);
        }
        if index.partial{return Vec::new();}
        let Some(language) = language_of(&doc.path, &doc.language) else { return Vec::new() };
        let check_routes = host::setting_bool("reportMissingRoutes").unwrap_or(false);
        let mut current=problems(index,language,&doc.text,check_routes);
        current.extend(route_inspections::current(index,doc));
        if language=="php" {if let Some(project)=project_for(&doc.path){current.extend(eloquent_mass_problems(&project,index,doc));current.extend(enum_value_problems(&project,index,doc));}}
        if language=="blade" {if let Some(project)=project_for(&doc.path){current.extend(livewire_method_problems(&project,index,&doc.path,language,&doc.text));}}
        current.into_iter().take(200)
            .map(|mut diagnostic| {
                let mut chars = diagnostic.message.chars();
                let mut bounded: String = chars.by_ref().take(512).collect();
                if chars.next().is_some() { bounded.push('…'); }
                diagnostic.message = bounded;
                diagnostic
            }).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{activated, cursor, doc};
    use openidea_plugin::mock::MockHost;

    #[test]
    fn live_inventory_checks_use_unsaved_versioned_text_and_utf16_ranges() {
        let host = MockHost::install();
        let mut plugin = activated(&host);
        host.open_editor("/project/test.php", "php", "<?php /* 😀 */ view('gone');", &[]);
        host.refresh_diagnostics(&mut plugin);
        let found = host.provider_diagnostics();
        let d = &found["/project/test.php"][0];
        assert_eq!(d.code.as_deref(), Some("missing-view"));
        assert_eq!(d.range.start, Position::new(0, 21));
        assert!(!d.message.contains("Cached source inventory"));
        host.open_editor("/project/test.php", "php", "<?php view('welcome');", &[]);
        host.refresh_diagnostics(&mut plugin);
        assert!(host.provider_diagnostics().get("/project/test.php").is_none_or(Vec::is_empty));
        assert!(host.diagnostics().is_empty() && host.process_runs().is_empty() && host.requests().is_empty());
    }

    #[test]
    fn live_checks_refuse_partial_cold_foreign_nested_denied_and_oversized_sources() {
        let host = MockHost::install();
        let mut plugin = activated(&host);
        let d = doc("/project/test.php", "php", "<?php view('gone');");
        assert_eq!(plugin.diagnostics(&d).len(), 1);
        for path in ["/foreign/test.php", "/project/vendor/pkg/test.php", "/project/../foreign/test.php"] {
            assert!(plugin.diagnostics(&doc(path, "php", &d.text)).is_empty());
        }
        host.add_project("/project/nested");
        assert!(plugin.diagnostics(&doc("/project/nested/test.php", "php", &d.text)).is_empty());
        assert!(plugin.diagnostics(&doc(&d.path, "php", &format!("{}{}", d.text, " ".repeat(32 * 1024)))).is_empty());
        plugin.indexes.get_mut("/project").unwrap().partial = true;
        assert!(plugin.diagnostics(&d).is_empty());
        plugin.indexes.clear();
        assert!(plugin.diagnostics(&d).is_empty());
        let _host = host.with_capabilities(&[]);
        assert!(plugin.diagnostics(&d).is_empty());
    }

    #[test]
    fn live_settings_gate_route_and_tag_checks_and_bound_output() {
        let host = MockHost::install();
        let mut plugin = activated(&host);
        let d = doc("/project/test.php", "php", "<?php route('gone');");
        assert!(plugin.diagnostics(&d).is_empty());
        host.set_setting("reportMissingRoutes", "true");
        assert_eq!(plugin.diagnostics(&d)[0].code.as_deref(), Some("missing-route"));
        host.set_setting("diagnostics", "false");
        assert!(plugin.diagnostics(&d).is_empty());
        host.set_setting("diagnostics", "true");
        let d = doc("/project/resources/views/test.blade.php", "blade", "<x-missing />");
        assert!(plugin.diagnostics(&d).iter().any(|d| d.code.as_deref() == Some("laravel.unknown-component")));
        host.set_setting("reportUnknownComponents", "false");
        assert!(plugin.diagnostics(&d).is_empty());
        let d = doc("/project/test.php", "php", &format!("<?php {}", "view('gone');".repeat(250)));
        assert_eq!(plugin.diagnostics(&d).len(), 200);
        let d = doc("/project/test.php", "php", &format!("<?php route('{}');", "😀".repeat(6000)));
        let out = plugin.diagnostics(&d);
        assert_eq!(out.len(), 1);
        assert!(out[0].message.len() < 4096 && out[0].message.contains('…'));
    }

    #[test]
    fn live_and_manual_producers_do_not_duplicate_or_clear_each_other() {
        let host = MockHost::install();
        let mut plugin = activated(&host);
        let path = "/project/app/Test.php";
        let source = "<?php view('gone'); env('APP_NAME');";
        host.add_file("app/Test.php", source);
        host.open_editor(path, "php", source, &[]);
        plugin.handle_events(&[Event::FileOpened(path.into())], &host.invocation());crate::tests::finish_indexing(&mut plugin);
        let manual = host.diagnostics()[path].clone();
        assert!(manual.iter().any(|d| d.code.as_deref() == Some("env-outside-config")));
        assert!(!manual.iter().any(|d| d.code.as_deref() == Some("missing-view")));
        host.refresh_diagnostics(&mut plugin);
        assert_eq!(host.provider_diagnostics()[path].len(), 1);
        assert_eq!(host.provider_diagnostics()[path][0].code.as_deref(), Some("missing-view"));
        assert_eq!(host.diagnostics()[path], manual);
        host.open_editor(path, "php", "<?php view('welcome');", &[]);
        host.refresh_diagnostics(&mut plugin);
        assert!(host.provider_diagnostics().get(path).is_none_or(Vec::is_empty));
        assert_eq!(host.diagnostics()[path], manual);
        host.add_file("app/Test.php", "<?php view('welcome');");
        plugin.handle_events(&[Event::FileSaved(path.into())], &host.invocation());crate::tests::finish_indexing(&mut plugin);
        assert!(host.diagnostics().get(path).is_none_or(Vec::is_empty) && host.provider_diagnostics().get(path).is_none_or(Vec::is_empty));
        host.open_editor(path, "php", source, &[]);
        host.refresh_diagnostics(&mut plugin);
        host::publish_diagnostics(path, &[]).unwrap();
        assert!(host.diagnostics().get(path).is_none_or(Vec::is_empty));
        assert_eq!(host.provider_diagnostics()[path].len(), 1, "empty manual publication must retain the unsaved provider finding");
    }

    #[test]
    fn live_missing_keys_refresh_after_saved_definitions_without_reading_inside_request() {
        let host = MockHost::install();
        let mut plugin = activated(&host);
        let (source, _) = cursor("<?php config('app.new|key'); __('messages.gone');");
        let d = doc("/project/test.php", "php", &source);
        assert_eq!(plugin.diagnostics(&d).len(), 2);
        host.add_file("config/app.php", "<?php return ['newkey' => true];");
        assert_eq!(plugin.diagnostics(&d).len(), 2);
        plugin.handle_events(&[Event::FileSaved("/project/config/app.php".into())], &host.invocation());crate::tests::finish_indexing(&mut plugin);
        assert_eq!(plugin.diagnostics(&d).len(), 1);
        assert_eq!(plugin.diagnostics(&d)[0].code.as_deref(), Some("missing-translation"));
    }
    #[test]
    fn registered_live_inventory_does_not_report_foreign_namespaced_helper_literals() {
        let host=MockHost::install();let mut plugin=activated(&host);host.set_setting("reportMissingRoutes","true");
        for source in ["<?php Vendor\\view('absent');", "<?php \\Vendor\\config('absent');", "<?php namespace\\route('absent');", "<?php Vendor\\__('absent');"] {
            assert!(plugin.diagnostics(&doc("/project/test.php","php",source)).is_empty(),"{source}");
        }
    }

    #[test]
    fn registered_live_inventory_refuses_foreign_facade_classes_with_laravel_short_names() {
        let host=MockHost::install();let mut plugin=activated(&host);host.set_setting("reportMissingRoutes","true");
        let observed:Vec<_>=["<?php \\Vendor\\View::make('absent');", "<?php \\Vendor\\Config::get('app.absent');", "<?php \\Vendor\\Route::has('absent');", "<?php \\Vendor\\Lang::get('messages.absent');"].into_iter().map(|source|(source,plugin.diagnostics(&doc("/project/test.php","php",source)).len())).collect();
        assert!(observed.iter().all(|(_,count)|*count==0),"{observed:?}");
    }

    #[test]
    fn registered_live_inventory_ignores_fake_directives_and_tags_in_raw_php() {
        let host=MockHost::install();let mut plugin=activated(&host);
        let observed:Vec<_>=[r#"<?php $x = "@include('absent') <x-absent>"; ?>"#,r#"<?php $x = "?> @include('absent') <x-absent>"; ?>"#,r#"<?php /* ?> @include('absent') <x-absent> */ ?>"#].into_iter().map(|source|(source,plugin.diagnostics(&doc("/project/resources/views/test.blade.php","blade",source)).len())).collect();
        assert!(observed.iter().all(|(_,count)|*count==0),"{observed:?}");
    }

    #[test]
    fn registered_live_inventory_ignores_fake_includes_in_php_directive_argument_comments() {
        let host=MockHost::install();let mut plugin=activated(&host);let observed:Vec<_>=["@if(/* ) @include('absent') */ $real) @endif","@if(// ) @include('absent')\n$real) @endif","@if(# ) @include('absent')\n$real) @endif"].into_iter().map(|source|(source,plugin.diagnostics(&doc("/project/resources/views/test.blade.php","blade",source)).len())).collect();assert!(observed.iter().all(|(_,n)|*n==0),"{observed:?}");
    }

    #[test]
    fn metadata_diagnostics_follow_unsaved_text_and_cached_external_snapshots() {
        let host = MockHost::install();
        host.add_file("ide.json", r#"{"packages":{"passport":{"scopeFiles":["/scopes.json"]}}}"#);
        host.add_file("scopes.json", r#"{"bad":4}"#);
        let mut plugin = activated(&host);
        let original = r#"{"packages":{"passport":{"scopeFiles":["/scopes.json"]}}}"#;
        assert!(plugin.diagnostics(&doc("/project/ide.json", "json", original)).iter().any(|d| d.message.contains("scope bad")));
        // Changing the on-disk dependency does not trigger source reads from the callback.
        host.add_file("scopes.json", r#"{"good":"Description"}"#);
        let changed = format!(" {original}");
        assert!(plugin.diagnostics(&doc("/project/ide.json", "json", &changed)).iter().any(|d| d.message.contains("scope bad")));
        assert!(plugin.diagnostics(&doc("/project/ide.json", "json", "{}")).is_empty());
        assert!(plugin.diagnostics(&doc("/project/ide.json", "json", "{broken")).iter().any(|d| d.message.contains("invalid")));
        let changed = r#"{"packages":{"passport":{"scopeFiles":["/new.json"]}}}"#;
        let out = plugin.diagnostics(&doc("/project/ide.json", "json", changed));
        assert!(out.iter().any(|d| d.message.contains("new.json is not indexed; save/reindex")), "{out:?}");
        let large = format!("{}{{}}", " ".repeat(40 * 1024));
        assert!(plugin.diagnostics(&doc("/project/ide.json", "json", &large)).is_empty());
        assert!(plugin.diagnostics(&doc("/project/ide.json", "json", &" ".repeat(256 * 1024 + 1))).iter().any(|d| d.message.contains("size limit")));
        assert!(host.process_runs().is_empty() && host.requests().is_empty());
    }

}
