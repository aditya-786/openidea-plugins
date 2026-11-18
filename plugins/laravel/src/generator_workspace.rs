//! Project-owned previews bridged to the host's atomic editor FileAction API.
use crate::generator_plan::{self, Draft, Plan};
use openidea_plugin::{
    file_actions::{FileAction, FileOperation},
    host,
    providers::Document,
    Error, Invocation, Project,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;

#[path = "generator_factory_defaults.rs"]
pub(crate) mod factory_defaults;

pub(crate) const VIEW: &str = "laravel-generators";
pub(crate) fn html() -> String {
    let revision = crate::artisan_metadata::hash(include_str!(
        "../../../crates/ui-kit/web/command-workspace.css"
    ));
    let html = include_str!("../web/generators.html").replace(
        "./command-workspace.css",
        &format!("./command-workspace.css?v={revision}"),
    );
    format!(
        "<link rel=\"stylesheet\" href=\"./components.css\"><style>{}</style>{}",
        include_str!("../web/generators.css"),
        html
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    fn setup() -> (MockHost, State, Document) {
        let h = MockHost::install();
        h.add_dir("app");
        h.add_file(
            "composer.json",
            r#"{"autoload":{"psr-4":{"App\\":"app/","Database\\Factories\\":"database/factories/","Database\\Seeders\\":"database/seeders/"}}}"#,
        );
        h.add_file("routes/web.php", "<?php // anchor");
        let doc = Document {
            path: "/project/routes/web.php".into(),
            text: "<?php // anchor".into(),
            language: "php".into(),
            version: 1,
        };
        h.open_editor(&doc.path, "php", &doc.text, &[]);
        (h, State::default(), doc)
    }
    #[test]
    fn pending_index_preserves_draft_and_context_then_hydrates_without_prepared_actions() {
        let (h, mut state, doc) = setup();
        inferred_factory_fixture(&h, &mut state);
        let mut initial = inferred_factory_request("prepare");
        initial["draft"]["timestamps"] = json!(false);
        state.handle(&h.invocation(), &initial.to_string()).unwrap();
        let retired = state.actions(&doc).remove(0);
        state.set_models_pending(&h.invocation()).unwrap();
        state.set_models_pending(&h.invocation()).unwrap();
        let reply = state.handle(&h.invocation(), r#"{"op":"state"}"#).unwrap();
        assert_eq!(reply["indexPending"], true);
        assert_eq!(reply["modelsComplete"], false);
        assert!(reply["models"].as_array().unwrap().is_empty());
        assert_eq!(reply["draft"]["timestamps"], false);
        assert!(state.projects["/project"].input.is_some());
        assert!(state.actions(&doc).is_empty());
        assert!(
            matches!(state.resolve(&doc, retired.clone()), Err(Error::Invalid(message)) if message.contains("Indexing"))
        );
        let mut edited = initial.clone();
        edited["draft"]["name"] = json!("EditedPost");
        edited["draft"]["timezone"] = json!(true);
        edited["module"] = json!("blog");
        edited["packageOptions"] = json!({"novaLabelPrefix":"fields.custom."});
        assert!(
            matches!(state.handle(&h.invocation(), &edited.to_string()), Err(Error::Invalid(message)) if message.contains("Indexing"))
        );
        let pending = state.handle(&h.invocation(), r#"{"op":"state"}"#).unwrap();
        assert_eq!(pending["draft"]["name"], "EditedPost");
        assert_eq!(pending["draft"]["timezone"], true);
        assert_eq!(pending["prepared"], false);
        assert!(matches!(state.projects["/project"].input.as_ref(),
            Some(Input::Model { module: Some(module), package_options, .. })
                if module == "blog" && package_options.nova_label_prefix.as_deref() == Some("fields.custom.")));
        assert!(state.actions(&doc).is_empty());
        let project = h.invocation().project().unwrap();
        state
            .set_models(&h.invocation(), &crate::index::Index::build(&project))
            .unwrap();
        let ready = state.handle(&h.invocation(), r#"{"op":"state"}"#).unwrap();
        assert_eq!(ready["indexPending"], false);
        assert_eq!(ready["modelsComplete"], true);
        assert_eq!(ready["draft"], pending["draft"]);
        assert!(!ready["models"].as_array().unwrap().is_empty());
        assert!(state.resolve(&doc, retired).is_err());
        edited["op"] = json!("preview");
        edited["module"] = Value::Null;
        assert!(state.handle(&h.invocation(), &edited.to_string()).is_ok());
        assert!(h.process_runs().is_empty());
    }
    #[test]
    fn completed_snapshot_refreshes_on_settings_only_changes_and_withholds_changed_sources() {
        let (h, mut state, _) = setup();
        let project = h.invocation().project().unwrap();
        let first = crate::index::Index::build(&project);
        state.set_models(&h.invocation(), &first).unwrap();
        assert!(crate::index::index_sources_current(
            &project,
            state.projects["/project"]
                .captured_index
                .as_deref()
                .unwrap()
        ));
        h.set_setting("locale", "\"fr\"");
        let mut changed = first.clone();
        changed
            .source_settings
            .insert("locale".into(), host::setting_string("locale"));
        assert_eq!(changed.source_evidence, first.source_evidence);
        assert_eq!(changed.source_queries, first.source_queries);
        assert!(!crate::index::index_settings_current(
            state.projects["/project"]
                .captured_index
                .as_deref()
                .unwrap()
        ));
        state.set_models(&h.invocation(), &changed).unwrap();
        let captured = state.projects["/project"]
            .captured_index
            .as_deref()
            .unwrap();
        assert_eq!(captured.source_settings, changed.source_settings);
        assert!(crate::index::index_sources_current(&project, captured));
        h.add_file("routes/web.php", "<?php // changed captured route source");
        assert!(!crate::index::index_sources_current(&project, captured));
        let mut pending = changed;
        pending.partial = true;
        state.set_models(&h.invocation(), &pending).unwrap();
        assert!(state.projects["/project"].captured_index.is_none());
        assert!(!state.projects["/project"].models_complete);
    }
    #[test]
    fn composer_parent_prefix_and_owned_package_manifest_resolve_generated_classes() {
        let (h, _, _) = setup();
        let mut draft: Draft = serde_json::from_value(
            json!({"name":"Post","table":"posts","migrationStamp":"2026_11_10_120000"}),
        )
        .unwrap();
        draft.name = "Post".into();
        draft.namespace = "App\\Domain".into();
        draft.source_root = "app/Domain".into();
        let project = h.invocation().project().unwrap();
        assert!(mapping(&project, &draft).is_ok());
        h.add_dir("packages");
        h.add_dir("packages/billing");
        h.add_dir("packages/billing/src");
        h.add_file(
            "packages/billing/composer.json",
            r#"{"autoload":{"psr-4":{"Vendor\\Billing\\":"src/"}}}"#,
        );
        draft.namespace = "Vendor\\Billing".into();
        draft.source_root = "packages/billing/src".into();
        assert!(mapping(&project, &draft).is_ok());
        let before = mapping(&project, &draft).unwrap();
        h.add_file(
            "packages/billing/src/composer.json",
            r#"{"name":"owned/nested"}"#,
        );
        assert_ne!(
            mapping(&project, &draft).unwrap(),
            before,
            "new manifests invalidate prepared evidence"
        );
        h.add_project("/project/packages/billing");
        assert!(mapping(&project, &draft).is_err());
    }
    #[test]
    fn composer_conflicts_and_escaping_relative_roots_are_rejected() {
        assert_eq!(
            composer_relative("packages/billing", "../shared/src/").unwrap(),
            "packages/shared/src"
        );
        assert!(composer_relative("packages/billing", "../../../escape").is_err());
        let (h, _, _) = setup();
        h.add_dir("app/Domain");
        h.add_file(
            "app/composer.json",
            r#"{"autoload":{"psr-4":{"App\\":"different/"}}}"#,
        );
        let mut draft: Draft = serde_json::from_value(
            json!({"name":"Post","table":"posts","migrationStamp":"2026_11_10_120000"}),
        )
        .unwrap();
        draft.name = "Post".into();
        assert!(mapping(&h.invocation().project().unwrap(), &draft).is_err());
    }
    #[test]
    fn preview_uses_configured_destinations_composer_proof_and_availability() {
        let (h, mut state, _) = setup();
        h.add_file("composer.json",r#"{"autoload":{"psr-4":{"App\\":"app/","Domain\\":"src/","Database\\Factories\\":"database/factories/"}}}"#);
        h.add_file(".openidea/laravel-generators.json",r#"{"version":1,"generators":{"model":{"directory":"src/Entities","namespace":"Domain\\Entities"},"controller":{"availableInApplication":false}}}"#);
        let preview = state.handle(&h.invocation(), &request("preview")).unwrap();
        assert!(preview["files"].as_array().unwrap().iter().any(|file| {
            file["path"] == "src/Entities/Post.php"
                && file["text"]
                    .as_str()
                    .unwrap()
                    .contains("namespace Domain\\Entities;")
        }));
        let mut unavailable: Value = serde_json::from_str(&request("preview")).unwrap();
        unavailable["draft"]["related"] = json!(["controller"]);
        assert!(state
            .handle(&h.invocation(), &unavailable.to_string())
            .is_err());
        h.add_file(
            "composer.json",
            r#"{"autoload":{"psr-4":{"Domain\\":"wrong/"}}}"#,
        );
        assert!(state.handle(&h.invocation(), &request("preview")).is_err());
    }
    fn reference_fixture(h: &MockHost, state: &mut State) {
        for path in [
            "app/Http",
            "app/Http/Resources",
            "app/Factories",
            "database",
            "database/factories",
        ] {
            h.add_dir(path);
        }
        h.add_file("app/Factories/BaseFactory.php","<?php namespace App\\Factories; abstract class BaseFactory extends \\Illuminate\\Database\\Eloquent\\Factories\\Factory {}");
        h.add_file("database/factories/UserFactory.php","<?php namespace Database\\Factories; class UserFactory extends \\App\\Factories\\BaseFactory {}");
        h.add_file("app/Http/Resources/UserResource.php","<?php namespace App\\Http\\Resources; class UserResource extends \\Illuminate\\Http\\Resources\\Json\\JsonResource {}");
        let index = crate::index::Index::build(&h.invocation().project().unwrap());
        state.set_models(&h.invocation(), &index).unwrap();
    }
    fn reference_request(op: &str) -> String {
        json!({"op":op,"draft":{"name":"Post","table":"posts","migrationStamp":"2026_11_10_120000","fields":[{"name":"user_id","kind":"unsignedBigInteger"}],"relations":[{"name":"author","kind":"belongsTo","related":"App\\Models\\User","arguments":[]}],"related":["factory","resource"],"factoryMappings":{"user_id":{"kind":"factory","class":"Database\\Factories\\UserFactory"}},"resourceWrappers":{"author":"App\\Http\\Resources\\UserResource"}}}).to_string()
    }
    fn inferred_factory_fixture(h: &MockHost, state: &mut State) {
        reference_fixture(h, state);
        h.add_file(
            "composer.lock",
            r#"{"packages":[{"name":"laravel/framework","version":"v12.72.0"}]}"#,
        );
        h.add_file("app/Models/User.php","<?php namespace App\\Models;class User extends \\Illuminate\\Database\\Eloquent\\Model{}");
        h.add_file("app/Models/Article.php","<?php namespace App\\Models;class Article extends \\Illuminate\\Database\\Eloquent\\Model{public function user(){return $this->belongsTo(User::class);}}");
        h.add_file("database/factories/UserFactory.php","<?php namespace Database\\Factories;class UserFactory extends \\App\\Factories\\BaseFactory{protected $model=\\App\\Models\\User::class;}");
        h.add_file("database/migrations/2026_01_01_000000_articles.php","<?php Schema::create('articles',function(Blueprint $table){$table->id();$table->unsignedBigInteger('user_id');});");
        let index = crate::index::Index::build(&h.invocation().project().unwrap());
        state.set_models(&h.invocation(), &index).unwrap();
    }
    fn inferred_factory_request(op: &str) -> Value {
        json!({"op":op,"draft":{"name":"Post","table":"posts","migrationStamp":"2026_11_10_120000","fields":[{"name":"user_id","kind":"unsignedBigInteger"}],"relations":[{"name":"user","kind":"belongsTo","related":"App\\Models\\User","arguments":[]}],"related":["factory"]}})
    }
    #[test]
    fn inferred_factory_defaults_apply_allow_explicit_clear_and_override_and_reject_stale_bindings()
    {
        let (h, mut state, doc) = setup();
        inferred_factory_fixture(&h, &mut state);
        let preview = state
            .handle(
                &h.invocation(),
                &inferred_factory_request("preview").to_string(),
            )
            .unwrap();
        assert!(preview["files"].as_array().unwrap().iter().any(|file| {
            file["text"]
                .as_str()
                .unwrap()
                .contains("'user_id' => \\Database\\Factories\\UserFactory::new()")
        }));
        let mut clear = inferred_factory_request("preview");
        clear["draft"]["factoryMappings"] = json!({"user_id":{"kind":"generated"}});
        let preview = state.handle(&h.invocation(), &clear.to_string()).unwrap();
        assert!(!preview["files"].as_array().unwrap().iter().any(|file| {
            file["text"]
                .as_str()
                .unwrap()
                .contains("UserFactory::new()")
        }));
        clear["draft"]["factoryMappings"] = json!({"user_id":{"kind":"literal","value":7}});
        let preview = state.handle(&h.invocation(), &clear.to_string()).unwrap();
        assert!(preview["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|file| file["text"].as_str().unwrap().contains("'user_id' => 7")));
        let mut prepared = inferred_factory_request("prepare");
        prepared["draft"]["factoryMappings"] = json!({"user_id":{"kind":"factory","class":"Database\\Factories\\UserFactory","inferred":true}});
        state
            .handle(&h.invocation(), &prepared.to_string())
            .unwrap();
        let action = state.actions(&doc).remove(0);
        h.add_file("database/factories/UserFactory.php","<?php namespace Database\\Factories;class UserFactory extends \\App\\Factories\\BaseFactory{protected $model=\\App\\Models\\Article::class;}");
        assert_eq!(state.resolve(&doc, action), Err(Error::Stale));
        assert!(state
            .handle(&h.invocation(), &prepared.to_string())
            .is_err());
        assert!(h.process_runs().is_empty());
    }
    #[test]
    fn refreshed_completed_index_does_not_reuse_a_prepared_plan_from_an_old_source_manifest() {
        let (h, mut state, doc) = setup();
        inferred_factory_fixture(&h, &mut state);
        state
            .handle(
                &h.invocation(),
                &inferred_factory_request("prepare").to_string(),
            )
            .unwrap();
        let action = state.actions(&doc).remove(0);
        h.add_file("routes/other.php", "<?php // new completed source input");
        let index = crate::index::Index::build(&h.invocation().project().unwrap());
        state.set_models(&h.invocation(), &index).unwrap();
        assert_eq!(state.resolve(&doc, action), Err(Error::Stale));
    }
    #[test]
    fn existing_native_factory_catalog_preselects_only_proven_defaults_and_clearing_is_explicit() {
        for convention in [false, true] {
            let (h, mut state, _) = setup();
            inferred_factory_fixture(&h, &mut state);
            if convention {
                factory_defaults::install_convention_fixture(&h);
                let index = crate::index::Index::build(&h.invocation().project().unwrap());
                state.set_models(&h.invocation(), &index).unwrap();
            }
            let catalog = state
                .handle(
                    &h.invocation(),
                    r#"{"op":"inspect-related","model":"App\\Models\\Article"}"#,
                )
                .unwrap();
            assert_eq!(
                catalog["factoryDefaults"]["user_id"]["class"],
                "Database\\Factories\\UserFactory"
            );
            let mut input = json!({"op":"preview-native","request":{"kind":"factory","name":"Article","model":"App\\Models\\Article"},"choices":{"fields":["user_id"]}});
            let preview = state.handle(&h.invocation(), &input.to_string()).unwrap();
            assert!(preview["files"][0]["text"]
                .as_str()
                .unwrap()
                .contains("UserFactory::new()"));
            input["choices"]["factories"] = json!({});
            let preview = state.handle(&h.invocation(), &input.to_string()).unwrap();
            assert!(!preview["files"][0]["text"]
                .as_str()
                .unwrap()
                .contains("UserFactory::new()"));
            input["choices"]["factories"] = json!({"user_id":"Database\\Factories\\UserFactory"});
            input["choices"]["inferredFactories"] = json!(["user_id"]);
            input["op"] = json!("prepare-native");
            state.handle(&h.invocation(), &input.to_string()).unwrap();
            h.add_file("app/Models/User.php","<?php namespace App\\Models;class User extends \\Illuminate\\Database\\Eloquent\\Model{protected $primaryKey='uuid';}");
            assert!(state.handle(&h.invocation(), &input.to_string()).is_err());
        }
    }
    #[test]
    fn cached_reference_choices_exclude_abstracts_and_apply_proves_current_source_ancestry() {
        let (h, mut state, doc) = setup();
        reference_fixture(&h, &mut state);
        let catalog = state.handle(&h.invocation(), r#"{"op":"state"}"#).unwrap();
        assert_eq!(
            catalog["references"]["factories"].as_array().unwrap().len(),
            1
        );
        assert_eq!(
            catalog["references"]["factories"][0]["class"],
            "Database\\Factories\\UserFactory"
        );
        let preview = state
            .handle(&h.invocation(), &reference_request("preview"))
            .unwrap();
        assert!(preview["files"].as_array().unwrap().iter().any(|file| {
            file["text"]
                .as_str()
                .unwrap()
                .contains("UserFactory::new()")
        }));
        state
            .handle(&h.invocation(), &reference_request("prepare"))
            .unwrap();
        let action = state.actions(&doc).remove(0);
        h.add_file("app/Factories/BaseFactory.php","<?php namespace App\\Factories; abstract class BaseFactory extends \\Other\\Unproven {}");
        assert!(state.resolve(&doc, action).is_err());
        assert!(state
            .handle(&h.invocation(), &reference_request("preview"))
            .is_err());
        assert!(h.process_runs().is_empty());
    }
    #[test]
    fn reference_choices_refuse_unknown_foreign_and_changed_abstract_sources() {
        let (h, mut state, _) = setup();
        reference_fixture(&h, &mut state);
        let mut request: Value = serde_json::from_str(&reference_request("preview")).unwrap();
        request["draft"]["factoryMappings"]["user_id"]["class"] =
            json!("database\\factories\\userfactory");
        assert!(state.handle(&h.invocation(), &request.to_string()).is_err());
        request["draft"]["factoryMappings"]["user_id"]["class"] = json!("Unproved\\Factory");
        assert!(state.handle(&h.invocation(), &request.to_string()).is_err());
        h.add_file("database/factories/UserFactory.php","<?php namespace Database\\Factories; abstract class UserFactory extends \\App\\Factories\\BaseFactory {}");
        assert!(state
            .handle(&h.invocation(), &reference_request("preview"))
            .is_err());
        h.add_project("/project/app/Http/Resources");
        assert!(state
            .handle(&h.invocation(), &reference_request("preview"))
            .is_err());
    }
    fn request(op: &str) -> String {
        json!({"op":op,"draft":{"name":"Post","table":"posts","migrationStamp":"2026_11_10_120000","fields":[{"name":"title","kind":"string"}],"related":["migration","factory"]}}).to_string()
    }
    #[test]
    fn new_model_package_outputs_share_one_plan_and_revalidate_package_and_saved_defaults() {
        let (h, mut state, doc) = setup();
        h.add_file("composer.lock", r#"{"packages":[{"name":"laravel/framework","version":"v12.72.0"},{"name":"filament/filament","version":"v4.16.2"}]}"#);
        let base = "<?php namespace Filament\\Resources; abstract class Resource {}";
        h.add_file("vendor/filament/filament/src/Resources/Resource.php", base);
        h.add_file(
            ".openidea/laravel-generators.json",
            r#"{"version":1,"generators":{"filament-resource":{"separate":false,"template":"filament.php"}}}"#,
        );
        h.add_file(
            ".openidea/laravel-templates/filament.php",
            "${generated}\n// reviewed principal template",
        );
        let index = crate::index::Index::build(&h.invocation().project().unwrap());
        state.set_models(&h.invocation(), &index).unwrap();
        let mut message: Value = serde_json::from_str(&request("preview")).unwrap();
        message["packages"] = json!(["filament-resource"]);
        let preview = state.handle(&h.invocation(), &message.to_string()).unwrap();
        let files = preview["files"].as_array().unwrap();
        assert_eq!(
            files.len(),
            7,
            "model, migration, factory and four package files"
        );
        assert!(files
            .iter()
            .any(|file| file["path"] == "app/Filament/Resources/Posts/PostResource.php"));
        assert!(!files
            .iter()
            .any(|file| file["path"].as_str().unwrap().contains("/Schemas/")));
        assert!(files.iter().any(|file| {
            file["text"]
                .as_str()
                .unwrap()
                .contains("reviewed principal template")
        }));
        message["op"] = json!("prepare");
        state.handle(&h.invocation(), &message.to_string()).unwrap();
        let action = state
            .actions(&doc)
            .into_iter()
            .find(|action| {
                action
                    .data
                    .as_deref()
                    .is_some_and(|data| data.starts_with("generator-apply:"))
            })
            .unwrap();
        h.add_file(
            "vendor/filament/filament/src/Resources/Resource.php",
            &format!("{base}\n// changed installed source"),
        );
        assert_eq!(state.resolve(&doc, action), Err(Error::Stale));
        let index = crate::index::Index::build(&h.invocation().project().unwrap());
        state.set_models(&h.invocation(), &index).unwrap();
        state.handle(&h.invocation(), &message.to_string()).unwrap();
        let action = state.actions(&doc).remove(0);
        h.add_file(
            ".openidea/laravel-templates/filament.php",
            "${generated}\n// changed template after preparation",
        );
        assert_eq!(state.resolve(&doc, action), Err(Error::Stale));
        assert!(h.process_runs().is_empty());
        assert!(h
            .invocation()
            .project()
            .unwrap()
            .stat("app/Models/Post.php")
            .is_err());
    }
    #[test]
    fn preset_preview_prepare_and_resolve_reuse_one_server_captured_clock() {
        let (h, mut state, doc) = setup();
        h.add_file(".openidea/laravel-generators.json",r#"{"version":1,"presets":[{"id":"dated","label":"Dated","calculated":[{"name":"stamp","expression":"${CURRENT_TIME|format:yyyyMMdd_HHmmss}"}],"files":[{"path":"app/Generated_${stamp}.php","template":"dated.php"}]}]}"#);
        h.add_file(
            ".openidea/laravel-templates/dated.php",
            "<?php // ${CURRENT_TIME|format:yyyy-MM-dd} ${stamp}",
        );
        state
            .projects
            .entry("/project".into())
            .or_default()
            .template_time = Some("2024-02-29 23:59:58".into());
        let preview = state
            .handle(
                &h.invocation(),
                r#"{"op":"preview-preset","preset":"dated","values":{}}"#,
            )
            .unwrap();
        assert_eq!(
            preview["files"][0]["path"],
            "app/Generated_20240229_235958.php"
        );
        state
            .handle(
                &h.invocation(),
                r#"{"op":"prepare-preset","preset":"dated","values":{}}"#,
            )
            .unwrap();
        // A later mutable UI session cache cannot change the prepared immutable
        // input or cause the final source render to consult today's clock.
        state.projects.get_mut("/project").unwrap().template_time =
            Some("2024-03-01 00:00:00".into());
        h.answer_next_prompt(Some(&["yes"]));
        let action = state.actions(&doc).remove(0);
        let resolved = state.resolve(&doc, action).unwrap();
        assert_eq!(resolved.operations.len(), 1);
        assert!(format!("{:?}", resolved.operations).contains("Generated_20240229_235958.php"));
        assert!(h.process_runs().is_empty());
        state
            .handle(&h.invocation(), r#"{"op":"discard"}"#)
            .unwrap();
        assert!(state.projects["/project"].template_time.is_none());
    }
    fn model_preset(h: &MockHost, state: &mut State) {
        h.add_file("app/Models/UserProfile.php","<?php namespace App\\Models; use Illuminate\\Database\\Eloquent\\Model; class UserProfile extends Model {}");
        h.add_file(".openidea/laravel-generators.json",r#"{"version":1,"presets":[{"id":"model_repo","label":"Model repository","parameters":[{"name":"model","label":"Model","type":"model"}],"calculated":[{"name":"table","label":"Table name","expression":"${model|className|snake|plural}"}],"files":[{"path":"app/Repositories/${table|studly}.php","template":"model_repo.php"}]}]}"#);
        h.add_file(
            ".openidea/laravel-templates/model_repo.php",
            "<?php // ${table|php}",
        );
        let index = crate::index::Index::build(&h.invocation().project().unwrap());
        state.set_models(&h.invocation(), &index).unwrap();
    }
    fn model_request(op: &str, model: &str) -> String {
        json!({"op":op,"preset":"model_repo","values":{"model":model},"overrides":{"table":"manual_records"}}).to_string()
    }
    #[test]
    fn indexed_model_choices_and_live_values_use_owned_inventory_and_explicit_overrides() {
        let (h, mut state, _) = setup();
        model_preset(&h, &mut state);
        let response = state.handle(&h.invocation(), r#"{"op":"state"}"#).unwrap();
        assert_eq!(response["models"][0]["class"], "App\\Models\\UserProfile");
        assert_eq!(response["modelsComplete"], true);
        let live = state
            .handle(
                &h.invocation(),
                &model_request("evaluate-preset", "App\\Models\\UserProfile"),
            )
            .unwrap();
        assert_eq!(live["values"]["table"], "manual_records");
        let preview = state
            .handle(
                &h.invocation(),
                &model_request("preview-preset", "App\\Models\\UserProfile"),
            )
            .unwrap();
        assert_eq!(
            preview["files"][0]["path"],
            "app/Repositories/ManualRecords.php"
        );
        assert!(h.process_runs().is_empty());
        assert!(h.terminals().is_empty());
        assert!(state
            .handle(
                &h.invocation(),
                &model_request("evaluate-preset", "App\\Models\\Unknown")
            )
            .is_err());
        h.add_project("/project/app/Models");
        assert!(state
            .handle(
                &h.invocation(),
                &model_request("preview-preset", "App\\Models\\UserProfile")
            )
            .is_err());
    }
    #[test]
    fn live_evaluation_does_not_read_or_execute_model_sources_but_preparation_revalidates_them() {
        let (h, mut state, _) = setup();
        model_preset(&h, &mut state);
        h.add_file(
            "app/Models/UserProfile.php",
            "<?php namespace App\\Models; class Renamed {}",
        );
        let live = state
            .handle(
                &h.invocation(),
                &model_request("evaluate-preset", "App\\Models\\UserProfile"),
            )
            .unwrap();
        assert_eq!(live["values"]["table"], "manual_records");
        assert!(state
            .handle(
                &h.invocation(),
                &model_request("prepare-preset", "App\\Models\\UserProfile")
            )
            .is_err());
        assert!(h.process_runs().is_empty());
    }
    #[test]
    fn changing_selected_model_source_invalidates_prepared_calculated_generation() {
        let (h, mut state, doc) = setup();
        model_preset(&h, &mut state);
        state
            .handle(
                &h.invocation(),
                &model_request("prepare-preset", "App\\Models\\UserProfile"),
            )
            .unwrap();
        let action = state.actions(&doc).remove(0);
        h.add_file("app/Models/UserProfile.php","<?php namespace App\\Models; use Illuminate\\Database\\Eloquent\\Model; class UserProfile extends Model { protected $table = 'profiles'; }");
        assert!(state.resolve(&doc, action).is_err());
    }
    #[test]
    fn preparation_returns_one_undoable_nonoverwriting_transaction_without_writes() {
        let (h, mut state, doc) = setup();
        let response = state.handle(&h.invocation(), &request("prepare")).unwrap();
        assert_eq!(response["prepared"], true);
        let lazy = state.actions(&doc).remove(0);
        assert!(lazy.operations.is_empty());
        h.answer_next_prompt(Some(&["yes"]));
        let ready = state.resolve(&doc, lazy.clone()).unwrap();
        assert_eq!(ready.operations.len(), 3);
        assert!(ready.operations.iter().all(|o| matches!(
            o,
            FileOperation::Create {
                overwrite: false,
                ..
            }
        )));
        assert!(h.process_runs().is_empty());
        assert!(h.terminals().is_empty());
        assert!(h
            .invocation()
            .project()
            .unwrap()
            .read_file("app/Models/Post.php")
            .is_err());
        assert!(state.resolve(&doc, lazy).is_err());
    }
    #[test]
    fn collision_and_composer_changes_invalidate_prepared_generation() {
        let (h, mut state, doc) = setup();
        state.handle(&h.invocation(), &request("prepare")).unwrap();
        let lazy = state.actions(&doc).remove(0);
        h.add_file("app/Models/Post.php", "existing");
        assert!(state.resolve(&doc, lazy).is_err());
        let (h, mut state, doc) = setup();
        state.handle(&h.invocation(), &request("prepare")).unwrap();
        let lazy = state.actions(&doc).remove(0);
        h.add_file(
            "composer.json",
            r#"{"autoload":{"psr-4":{"App\\":"other/"}}}"#,
        );
        assert!(state.resolve(&doc, lazy).is_err());
    }
    #[test]
    fn cancel_stale_anchor_and_discard_never_write() {
        let (h, mut state, doc) = setup();
        state.handle(&h.invocation(), &request("prepare")).unwrap();
        let lazy = state.actions(&doc).remove(0);
        let mut changed = doc.clone();
        changed.text.push(' ');
        assert!(state.resolve(&changed, lazy.clone()).is_err());
        assert_eq!(state.resolve(&doc, lazy), Err(Error::Cancelled));
        state
            .handle(&h.invocation(), r#"{"op":"discard"}"#)
            .unwrap();
        assert!(state.actions(&doc).is_empty());
        assert!(h.process_runs().is_empty());
    }
    #[test]
    fn foreign_project_cannot_apply_another_project_plan() {
        let (h, mut state, doc) = setup();
        state.handle(&h.invocation(), &request("prepare")).unwrap();
        h.add_project("/project/routes");
        assert!(state.actions(&doc).is_empty());
    }
    #[test]
    fn custom_template_changes_cannot_apply_an_old_preview() {
        let (h, mut state, doc) = setup();
        h.add_file(".openidea/laravel-generators.json",r#"{"version":1,"presets":[{"id":"simple","label":"Simple","files":[{"path":"app/Generated.php","template":"simple.php"}]}]}"#);
        h.add_file(
            ".openidea/laravel-templates/simple.php",
            "<?php // reviewed",
        );
        let reply = state
            .handle(
                &h.invocation(),
                r#"{"op":"prepare-preset","preset":"simple","values":{}}"#,
            )
            .unwrap();
        assert_eq!(reply["prepared"], true);
        let lazy = state.actions(&doc).remove(0);
        h.add_file(".openidea/laravel-templates/simple.php", "<?php // changed");
        assert!(state.resolve(&doc, lazy).is_err());
        assert!(h
            .invocation()
            .project()
            .unwrap()
            .read_file("app/Generated.php")
            .is_err());
    }
}
#[derive(Default)]
pub(crate) struct State {
    next: u64,
    projects: BTreeMap<String, Session>,
    presets: crate::generator_preset_actions::State,
    existing: crate::existing_model_fields::State,
    scaffolds: crate::native_module_scaffold::State,
}
#[derive(Default)]
struct Session {
    draft: Option<Draft>,
    input: Option<Input>,
    pending: Option<Pending>,
    models: crate::eloquent_members::Models,
    models_complete: bool,
    index_pending: bool,
    allowed_fields: BTreeMap<String, std::collections::BTreeSet<String>>,
    model_relations: BTreeMap<String, Vec<generator_plan::Relation>>,
    model_schemas: BTreeMap<String, Value>,
    model_casts: BTreeMap<String, BTreeMap<String, String>>,
    model_types: BTreeMap<String, BTreeMap<String, String>>,
    template_time: Option<String>,
    captured_index: Option<Box<crate::index::Index>>,
}
#[derive(Clone, Default, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NewModelPackageOptions {
    #[serde(default)]
    nova_resources: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    nova_label_mode: Option<crate::package_generators::LabelMode>,
    #[serde(default)]
    nova_label_prefix: Option<String>,
    #[serde(default)]
    filament_separate: Option<bool>,
}
#[derive(Clone)]
enum Input {
    Native {
        request: crate::native_catalog::Request,
        module: Option<String>,
        choices: Value,
    },
    Package {
        request: crate::package_generators::Request,
        module: Option<String>,
    },
    Model {
        draft: Draft,
        module: Option<String>,
        packages: Vec<String>,
        package_options: NewModelPackageOptions,
    },
    Preset {
        current_time: String,
        preset: String,
        values: BTreeMap<String, Value>,
        module: Option<String>,
        overrides: BTreeMap<String, Value>,
    },
}
struct Pending {
    key: String,
    plan: Plan,
    evidence: String,
    anchor: String,
}
#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case", deny_unknown_fields)]
enum Request {
    PreviewNative {
        request: crate::native_catalog::Request,
        #[serde(default)]
        module: Option<String>,
        #[serde(default)]
        choices: Value,
    },
    PrepareNative {
        request: crate::native_catalog::Request,
        #[serde(default)]
        module: Option<String>,
        #[serde(default)]
        choices: Value,
    },
    PreviewPackage {
        request: crate::package_generators::Request,
        #[serde(default)]
        module: Option<String>,
    },
    PreparePackage {
        request: crate::package_generators::Request,
        #[serde(default)]
        module: Option<String>,
    },
    State,
    InspectFactoryDefaults {
        draft: Draft,
        signature: String,
    },
    InspectRelated {
        model: String,
    },
    InspectPackageModel {
        model: String,
    },
    Artisan,
    NewModel,
    PreviewFields {
        draft: crate::existing_model_fields::Draft,
    },
    PrepareFields {
        draft: crate::existing_model_fields::Draft,
    },
    InspectConfig {
        scope: String,
    },
    PreviewConfig {
        scope: String,
        config: Value,
        templates: BTreeMap<String, String>,
        #[serde(default, rename = "removeTemplates")]
        remove_templates: Vec<String>,
    },
    PrepareConfig {
        scope: String,
        config: Value,
        templates: BTreeMap<String, String>,
        #[serde(default, rename = "removeTemplates")]
        remove_templates: Vec<String>,
    },
    Preview {
        draft: Draft,
        #[serde(default)]
        module: Option<String>,
        #[serde(default)]
        packages: Vec<String>,
        #[serde(default, rename = "packageOptions")]
        package_options: NewModelPackageOptions,
    },
    Prepare {
        draft: Draft,
        #[serde(default)]
        module: Option<String>,
        #[serde(default)]
        packages: Vec<String>,
        #[serde(default, rename = "packageOptions")]
        package_options: NewModelPackageOptions,
    },
    PreviewPreset {
        preset: String,
        values: BTreeMap<String, Value>,
        #[serde(default)]
        module: Option<String>,
        #[serde(default)]
        overrides: BTreeMap<String, Value>,
    },
    PreparePreset {
        preset: String,
        values: BTreeMap<String, Value>,
        #[serde(default)]
        module: Option<String>,
        #[serde(default)]
        overrides: BTreeMap<String, Value>,
    },
    EvaluatePreset {
        preset: String,
        values: BTreeMap<String, Value>,
        #[serde(default)]
        module: Option<String>,
        #[serde(default)]
        overrides: BTreeMap<String, Value>,
    },
    Discard,
}

fn owner(ctx: &Invocation) -> Result<(Project, String), Error> {
    let p = ctx
        .project()
        .ok_or_else(|| Error::Invalid("Open a Laravel project first".into()))?;
    let root = p.root()?;
    if !host::projects()?
        .iter()
        .any(|p| p.root().ok().as_deref() == Some(&root))
    {
        return Err(Error::Stale);
    }
    Ok((p, root))
}
fn owned(root: &str, path: &str) -> bool {
    host::project_of(path)
        .and_then(|p| p.root().ok())
        .as_deref()
        == Some(root)
}
#[derive(Clone)]
struct ComposerContext {
    path: String,
    directory: String,
    value: Value,
}
fn composer_relative(base: &str, path: &str) -> Result<String, Error> {
    if path.starts_with('/')
        || path.contains(['\\', ':', '\0'])
        || path.chars().any(char::is_control)
    {
        return Err(Error::Invalid(
            "Composer source roots must remain slash-normalized project paths".into(),
        ));
    }
    let mut parts = base
        .split('/')
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Err(Error::Invalid(
                        "Composer source root escapes this project".into(),
                    ));
                }
            }
            part => {
                if !generator_plan::relative(part) {
                    return Err(Error::Invalid(
                        "Invalid Composer source-root segment".into(),
                    ));
                }
                parts.push(part.into());
            }
        }
    }
    Ok(parts.join("/"))
}
fn composer_candidates(roots: &[&str]) -> Result<Vec<String>, Error> {
    let mut paths = std::collections::BTreeSet::from(["composer.json".to_owned()]);
    for root in roots {
        if !generator_plan::relative(root) {
            return Err(Error::Invalid(
                "Generator source roots must be project-relative paths".into(),
            ));
        }
        let mut parts = root.split('/').collect::<Vec<_>>();
        if parts.len() > 16 {
            return Err(Error::Invalid(
                "Generator source root exceeds Composer-discovery depth".into(),
            ));
        }
        while !parts.is_empty() {
            paths.insert(format!("{}/composer.json", parts.join("/")));
            parts.pop();
        }
    }
    if paths.len() > 64 {
        return Err(Error::Invalid(
            "Too many Composer namespace contexts".into(),
        ));
    }
    Ok(paths.into_iter().collect())
}
fn composer_contexts(
    project: &Project,
    roots: &[&str],
) -> Result<(Vec<ComposerContext>, String), Error> {
    let root = project.root()?;
    let mut contexts = Vec::new();
    let mut evidence = BTreeMap::<String, Option<String>>::new();
    let mut bytes = 0usize;
    for path in composer_candidates(roots)? {
        let absolute = format!("{root}/{path}");
        if !owned(&root, &absolute) {
            return Err(Error::Stale);
        }
        match project.stat(&path) {
            Err(Error::NotFound(_)) => {
                evidence.insert(path, None);
                continue;
            }
            Ok(stat) if stat.kind == openidea_plugin::FileKind::File => {}
            Ok(_) => {
                return Err(Error::Invalid(format!(
                    "Composer metadata must be an ordinary file: {path}"
                )));
            }
            Err(error) => return Err(error),
        }
        let directory = path
            .rsplit_once('/')
            .map(|(parent, _)| parent)
            .unwrap_or("")
            .to_owned();
        let mut parts = directory
            .split('/')
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>();
        while !parts.is_empty() {
            let parent = parts.join("/");
            if project.stat(&parent)?.kind != openidea_plugin::FileKind::Directory {
                return Err(Error::Invalid(
                    "Composer metadata cannot cross a symlink or non-directory".into(),
                ));
            }
            parts.pop();
        }
        let source = project.read_file(&path)?;
        bytes += source.len();
        if bytes > 2 << 20 {
            return Err(Error::Invalid(
                "Composer namespace evidence exceeds 2 MiB".into(),
            ));
        }
        let value = crate::strict_json::parse(&source, 128 << 10)?;
        if !value.is_object() {
            return Err(Error::Invalid(format!(
                "Composer metadata must be an object: {path}"
            )));
        }
        evidence.insert(path.clone(), Some(source));
        contexts.push(ComposerContext {
            path,
            directory,
            value,
        });
    }
    Ok((
        contexts,
        serde_json::to_string(&evidence).map_err(|_| Error::Stale)?,
    ))
}
fn mapped_class(
    project: &Project,
    contexts: &[ComposerContext],
    class: &str,
    path: &str,
    allow_dev: bool,
) -> Result<(), Error> {
    if !generator_plan::qualified(class) || !generator_plan::relative(path) {
        return Err(Error::Invalid("Invalid generated class or path".into()));
    }
    let mut matches = Vec::<(usize, String, std::collections::BTreeSet<String>)>::new();
    for context in contexts {
        for section in if allow_dev {
            vec!["autoload", "autoload-dev"]
        } else {
            vec!["autoload"]
        } {
            let Some(mappings) = context.value[section]["psr-4"].as_object() else {
                continue;
            };
            for (prefix, roots) in mappings {
                if !prefix.is_empty()
                    && (!prefix.ends_with('\\')
                        || !generator_plan::qualified(prefix.trim_end_matches('\\')))
                {
                    continue;
                }
                let Some(suffix) = class.strip_prefix(prefix) else {
                    continue;
                };
                let roots = match roots {
                    Value::String(root) => vec![root.as_str()],
                    Value::Array(roots) => roots
                        .iter()
                        .map(|root| {
                            root.as_str().ok_or_else(|| {
                                Error::Invalid(format!("Invalid PSR-4 root in {}", context.path))
                            })
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                    _ => {
                        return Err(Error::Invalid(format!(
                            "Invalid PSR-4 mapping for {prefix}"
                        )));
                    }
                };
                if roots.is_empty() || roots.len() > 32 {
                    return Err(Error::Invalid("Choose 1–32 declared PSR-4 roots".into()));
                }
                let mut destinations = std::collections::BTreeSet::new();
                for root in roots {
                    let directory = composer_relative(&context.directory, root)?;
                    let file = format!(
                        "{}{}.php",
                        if directory.is_empty() {
                            String::new()
                        } else {
                            format!("{directory}/")
                        },
                        suffix.replace('\\', "/")
                    );
                    if !generator_plan::relative(&file) {
                        return Err(Error::Invalid(
                            "Composer mapping produced an invalid destination".into(),
                        ));
                    }
                    destinations.insert(file);
                }
                matches.push((prefix.len(), context.path.clone(), destinations));
            }
        }
    }
    let longest = matches
        .iter()
        .map(|(length, _, _)| *length)
        .max()
        .ok_or_else(|| {
            Error::Invalid(format!("No owned Composer PSR-4 declaration maps {class}"))
        })?;
    let matching = matches
        .into_iter()
        .filter(|(length, _, _)| *length == longest)
        .collect::<Vec<_>>();
    for (left, (_, manifest, roots)) in matching.iter().enumerate() {
        for (_, other_manifest, other_roots) in matching.iter().skip(left + 1) {
            if manifest != other_manifest && roots.is_disjoint(other_roots) {
                return Err(Error::Invalid(format!(
                    "Conflicting Composer package mappings for {class}; resolve their namespaces before generating"
                )));
            }
        }
    }
    let destinations = matching
        .into_iter()
        .flat_map(|(_, _, roots)| roots)
        .collect::<std::collections::BTreeSet<_>>();
    if !destinations.contains(path) {
        return Err(Error::Invalid(format!(
            "Composer maps {class} to a different source root; expected generated path {path}"
        )));
    }
    let project_root = project.root()?;
    for other in destinations.iter().filter(|other| other.as_str() != path) {
        let absolute = format!("{project_root}/{other}");
        if !owned(&project_root, &absolute) {
            return Err(Error::Stale);
        }
        match project.stat(other) {
            Err(Error::NotFound(_)) => {}
            Ok(_) => {
                return Err(Error::Invalid(format!(
                    "Another Composer source root already contains {class}: {other}"
                )));
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}
/// Reuse the wizard's owned ancestor/package PSR-4 proof for related actions.
pub(crate) fn related_mapping(
    project: &Project,
    class: &str,
    path: &str,
    allow_dev: bool,
) -> Result<String, Error> {
    let directory = path
        .rsplit_once('/')
        .map(|(directory, _)| directory)
        .ok_or(Error::Stale)?;
    let (contexts, evidence) = composer_contexts(project, &[directory])?;
    mapped_class(project, &contexts, class, path, allow_dev)?;
    Ok(evidence)
}
fn mapping(project: &Project, draft: &Draft) -> Result<String, Error> {
    let kinds = std::iter::once("model")
        .chain(draft.related.iter().map(String::as_str))
        .filter(|kind| *kind != "migration")
        .collect::<Vec<_>>();
    let roots = kinds
        .iter()
        .map(|kind| draft.directory(kind))
        .collect::<Vec<_>>();
    let references = roots.iter().map(String::as_str).collect::<Vec<_>>();
    let (contexts, evidence) = composer_contexts(project, &references)?;
    for kind in kinds {
        let name = draft.class_name(kind);
        mapped_class(
            project,
            &contexts,
            &format!("{}\\{name}", draft.namespace_for(kind)),
            &format!("{}/{name}.php", draft.directory(kind)),
            matches!(kind, "factory" | "seeder"),
        )?;
    }
    Ok(evidence)
}

fn validate(project: &Project, root: &str, plan: &Plan) -> Result<(), Error> {
    if plan.files.is_empty()
        || plan.files.len() > 128
        || plan.files.iter().any(|file| file.text.len() > 128 << 10)
        || plan.files.iter().map(|file| file.text.len()).sum::<usize>() > 2 << 20
    {
        return Err(Error::Invalid(
            "Generated files exceed 128 KiB/file, 128 files or 2 MiB transaction limits".into(),
        ));
    }
    let mut paths = std::collections::BTreeSet::new();
    let mut classes = std::collections::BTreeSet::new();
    for file in &plan.files {
        let folded = file.path.to_lowercase();
        if paths.iter().any(|other: &String| {
            folded == *other
                || folded.starts_with(&format!("{other}/"))
                || other.starts_with(&format!("{folded}/"))
        }) {
            return Err(Error::Invalid(
                "Generated files have conflicting paths".into(),
            ));
        }
        paths.insert(folded);
        if let Some(class) = output_declaration(&file.text) {
            if !classes.insert(class.to_ascii_lowercase()) {
                return Err(Error::Invalid("Generated PHP classes conflict".into()));
            }
        }
    }
    for file in &plan.files {
        let absolute = format!("{root}/{}", file.path);
        if !generator_plan::relative(&file.path) || !owned(root, &absolute) {
            return Err(Error::Stale);
        }
        match project.stat(&file.path) {
            Err(Error::NotFound(_)) => {}
            Ok(_) => {
                return Err(Error::Invalid(format!(
                    "Target already exists: {}",
                    file.path
                )));
            }
            Err(e) => return Err(e),
        }
        // Require every existing ancestor to be an ordinary directory. This also
        // excludes symlink roots even on hosts whose project ownership follows them.
        let mut parts = file.path.split('/').collect::<Vec<_>>();
        parts.pop();
        while !parts.is_empty() {
            let parent = parts.join("/");
            match project.stat(&parent) {
                Ok(stat) if stat.kind == openidea_plugin::FileKind::Directory => {}
                Ok(_) => {
                    return Err(Error::Invalid(format!(
                        "Target parent is not a directory: {parent}"
                    )));
                }
                Err(Error::NotFound(_)) => {}
                Err(e) => return Err(e),
            }
            parts.pop();
        }
    }
    Ok(())
}
const FACTORY_BASE: &str = "Illuminate\\Database\\Eloquent\\Factories\\Factory";
const RESOURCE_BASE: &str = "Illuminate\\Http\\Resources\\Json\\JsonResource";
fn reference_chain<'a>(
    inventory: &'a [crate::eloquent_members::ClassIdentity],
    class: &str,
    target: &str,
    root: &str,
) -> Option<Vec<&'a crate::eloquent_members::ClassIdentity>> {
    let mut name = class.trim_start_matches('\\');
    let mut seen = std::collections::BTreeSet::new();
    let mut chain = Vec::new();
    for _ in 0..16 {
        if name.eq_ignore_ascii_case(target) {
            return (!chain.is_empty()).then_some(chain);
        }
        if !seen.insert(name.to_ascii_lowercase()) {
            return None;
        }
        let identity = inventory
            .iter()
            .find(|identity| identity.full.eq_ignore_ascii_case(name))?;
        if identity.ambiguous
            || chain.is_empty() && identity.full != name
            || identity.is_trait
            || chain.is_empty() && identity.is_abstract
            || !owned(root, &identity.path)
        {
            return None;
        }
        chain.push(identity);
        name = &identity.parent;
    }
    None
}
fn reference_catalog(models: &crate::eloquent_members::Models, root: &str) -> Value {
    let inventory = crate::eloquent_members::class_inventory(models);
    if inventory.len() > 4096 {
        return json!({"factories":[],"resources":[],"complete":false});
    }
    let candidates = |target| {
        inventory
            .iter()
            .filter(|identity| reference_chain(&inventory, &identity.full, target, root).is_some())
            .take(256)
            .map(|identity| json!({"class":identity.full,"path":identity.path}))
            .collect::<Vec<_>>()
    };
    json!({"factories":candidates(FACTORY_BASE),"resources":candidates(RESOURCE_BASE),"novaResources":candidates("Laravel\\Nova\\Resource"),"filamentResources":candidates("Filament\\Resources\\Resource"),"complete":true})
}
fn fresh_references(
    project: &Project,
    models: &crate::eloquent_members::Models,
    draft: &Draft,
) -> Result<(generator_plan::References, String), Error> {
    let inventory = crate::eloquent_members::class_inventory(models);
    let root = project.root()?;
    let mut wanted = Vec::new();
    for mapping in draft.factory_mappings.values() {
        if let generator_plan::FactoryMapping::Factory { class, .. } = mapping {
            wanted.push((class.as_str(), FACTORY_BASE));
        }
    }
    wanted.extend(
        draft
            .resource_wrappers
            .values()
            .map(|class| (class.as_str(), RESOURCE_BASE)),
    );
    if wanted.is_empty() {
        return Ok((Default::default(), String::new()));
    }
    if inventory.len() > 4096 || wanted.len() > 192 {
        return Err(Error::Invalid(
            "Reference inventory exceeds generation proof limits".into(),
        ));
    }
    let mut sources = BTreeMap::new();
    let mut total = 0;
    let mut fresh = crate::eloquent_members::Models::default();
    for (class, target) in &wanted {
        let chain = reference_chain(&inventory, class, target, &root).ok_or_else(|| {
            Error::Invalid(format!("Choose an owned indexed subclass for {class}"))
        })?;
        for identity in chain {
            if sources.contains_key(&identity.path) {
                continue;
            }
            if sources.len() >= 512
                || project.stat(&identity.path)?.kind != openidea_plugin::FileKind::File
            {
                return Err(Error::Stale);
            }
            let relative = identity
                .path
                .strip_prefix(&format!("{root}/"))
                .ok_or(Error::Stale)?;
            if !generator_plan::relative(relative) {
                return Err(Error::Stale);
            }
            let mut parent = relative.rsplit_once('/').map(|(parent, _)| parent);
            while let Some(path) = parent {
                if project.stat(path)?.kind != openidea_plugin::FileKind::Directory
                    || !owned(&root, &format!("{root}/{path}"))
                {
                    return Err(Error::Stale);
                }
                parent = path.rsplit_once('/').map(|(parent, _)| parent);
            }
            let source = project.read_file(&identity.path)?;
            total += source.len();
            if source.len() > 128 << 10 || total > 8 << 20 {
                return Err(Error::Invalid(
                    "Reference source proof exceeds 128 KiB/file or 8 MiB total".into(),
                ));
            }
            crate::eloquent_members::cache(&mut fresh, &source, &identity.path);
            sources.insert(identity.path.clone(), source);
        }
    }
    let current = crate::eloquent_members::class_inventory(&fresh);
    let mut proof = generator_plan::References::default();
    for (class, target) in wanted {
        let old = reference_chain(&inventory, class, target, &root).ok_or(Error::Stale)?;
        let new = reference_chain(&current, class, target, &root).ok_or(Error::Stale)?;
        if old.len() != new.len()
            || old.iter().zip(&new).any(|(old, new)| {
                old.full != new.full
                    || old.path != new.path
                    || old.parent != new.parent
                    || old.is_abstract != new.is_abstract
            })
        {
            return Err(Error::Stale);
        }
        let selected = new.first().ok_or(Error::Stale)?;
        if target == FACTORY_BASE {
            proof.factories.insert(selected.full.clone());
        } else {
            proof.resources.insert(selected.full.clone());
        }
    }
    Ok((
        proof,
        serde_json::to_string(&sources).map_err(|_| Error::Stale)?,
    ))
}
fn owned_class_evidence(
    project: &Project,
    models: &crate::eloquent_members::Models,
    names: &[String],
) -> Result<String, Error> {
    let root = project.root()?;
    let inventory = crate::eloquent_members::class_inventory(models);
    let mut sources = BTreeMap::new();
    let mut total = 0;
    if names.len() > 64 || inventory.len() > 4096 {
        return Err(Error::Invalid(
            "Class selection exceeds exact generation proof budget".into(),
        ));
    }
    for name in names {
        let identity = inventory
            .iter()
            .find(|identity| {
                identity
                    .full
                    .eq_ignore_ascii_case(name.trim_start_matches('\\'))
                    && !identity.ambiguous
                    && !identity.is_trait
                    && owned(&root, &identity.path)
            })
            .ok_or_else(|| Error::Invalid(format!("Choose an owned indexed class: {name}")))?;
        if sources.contains_key(&identity.path) {
            continue;
        }
        if project.stat(&identity.path)?.kind != openidea_plugin::FileKind::File {
            return Err(Error::Stale);
        }
        let source = project.read_file(&identity.path)?;
        total += source.len();
        if source.len() > 128 << 10 || total > 4 << 20 {
            return Err(Error::Invalid(
                "Selected source exceeds class proof limits".into(),
            ));
        }
        if crate::eloquent_members::declaration(&source).as_deref() != Some(identity.full.as_str())
        {
            return Err(Error::Stale);
        }
        sources.insert(identity.path.clone(), source);
    }
    serde_json::to_string(&sources).map_err(|_| Error::Stale)
}
fn output_declaration(text: &str) -> Option<String> {
    let class = crate::eloquent_members::declaration(text)?;
    if !generator_plan::qualified(&class) {
        let masked = crate::php::mask_comments(text);
        if masked
            .split_whitespace()
            .collect::<Vec<_>>()
            .windows(2)
            .any(|tokens| tokens == ["new", "class"])
        {
            return None;
        }
    }
    Some(class)
}
fn plan_mapping(project: &Project, plan: &Plan, allow_dev: bool) -> Result<String, Error> {
    let declared = plan
        .files
        .iter()
        .filter_map(|file| output_declaration(&file.text).map(|class| (file, class)))
        .collect::<Vec<_>>();
    let roots = declared
        .iter()
        .filter_map(|(file, _)| file.path.rsplit_once('/').map(|(root, _)| root))
        .collect::<Vec<_>>();
    if roots.is_empty() {
        return Ok(String::new());
    }
    let (contexts, evidence) = composer_contexts(project, &roots)?;
    let mut classes = std::collections::BTreeSet::new();
    for (file, class) in declared {
        if !classes.insert(class.to_ascii_lowercase()) {
            return Err(Error::Invalid("Generated PHP declarations conflict".into()));
        }
        mapped_class(project, &contexts, &class, &file.path, allow_dev)?;
    }
    Ok(evidence)
}
fn package_versions(project: &Project) -> BTreeMap<String, String> {
    let metadata = crate::metadata::Metadata::load(project);
    ["laravel/nova", "filament/filament", "laravel/framework"]
        .into_iter()
        .filter_map(|name| {
            metadata
                .package_version(name)
                .map(|version| (name.into(), version.into()))
        })
        .collect()
}
fn model_catalog(models: &crate::eloquent_members::Models, root: &str) -> Vec<Value> {
    crate::eloquent_members::model_inventory(models)
        .into_iter()
        .filter(|(_, path, _)| owned(root, path))
        .take(512)
        .map(|(class, path, table)| json!({"class":class,"path":path,"table":table}))
        .collect()
}
fn indexed_model_inputs(
    bundle: &crate::generator_presets::Bundle,
    models: &crate::eloquent_members::Models,
    preset: &str,
    values: &BTreeMap<String, Value>,
    root: &str,
) -> Result<Vec<String>, Error> {
    let chosen = bundle
        .model_inputs(preset, values)
        .map_err(Error::Invalid)?;
    let catalog = model_catalog(models, root);
    for model in &chosen {
        if !catalog
            .iter()
            .any(|entry| entry["class"].as_str() == Some(model.trim_start_matches('\\')))
        {
            return Err(Error::Invalid(format!(
                "Choose a currently indexed model: {model}"
            )));
        }
    }
    Ok(chosen)
}
fn fresh_model_evidence(
    project: &Project,
    models: &crate::eloquent_members::Models,
    chosen: &[String],
) -> Result<String, Error> {
    if chosen.is_empty() {
        return Ok(String::new());
    }
    let root = project.root()?;
    let inventory = crate::eloquent_members::model_inventory(models);
    if inventory.len() > 512 {
        return Err(Error::Invalid(
            "Model inventory exceeds the exact generation snapshot budget".into(),
        ));
    }
    let mut files = BTreeMap::new();
    let mut fresh = crate::eloquent_members::Models::default();
    let mut total = 0usize;
    for (_, path, _) in inventory {
        if !owned(&root, &path) {
            continue;
        }
        if files.contains_key(&path) {
            continue;
        }
        let source = project.read_file(&path)?;
        total += source.len();
        if source.len() > 128 << 10 || total > 8 << 20 {
            return Err(Error::Invalid(
                "Model sources exceed generation snapshot limits".into(),
            ));
        }
        crate::eloquent_members::cache(&mut fresh, &source, &path);
        files.insert(path, source);
    }
    let current = crate::eloquent_members::model_inventory(&fresh);
    for name in chosen {
        if !current
            .iter()
            .any(|(full, path, _)| full == name.trim_start_matches('\\') && owned(&root, path))
        {
            return Err(Error::Stale);
        }
    }
    serde_json::to_string(&files).map_err(|_| Error::Stale)
}
fn fresh_ancestor_evidence(
    project: &Project,
    models: &crate::eloquent_members::Models,
    class: &str,
    target: &str,
) -> Result<String, Error> {
    let root = project.root()?;
    let inventory = crate::eloquent_members::class_inventory(models);
    let chain = reference_chain(&inventory, class, target, &root)
        .ok_or_else(|| Error::Invalid(format!("Choose an owned indexed {target} subclass")))?;
    let mut fresh = crate::eloquent_members::Models::default();
    let mut files = BTreeMap::new();
    let mut total = 0;
    for identity in chain {
        if project.stat(&identity.path)?.kind != openidea_plugin::FileKind::File {
            return Err(Error::Stale);
        }
        let source = project.read_file(&identity.path)?;
        total += source.len();
        if source.len() > 128 << 10 || total > 2 << 20 {
            return Err(Error::Invalid("Package class proof exceeds limits".into()));
        }
        crate::eloquent_members::cache(&mut fresh, &source, &identity.path);
        files.insert(identity.path.clone(), source);
    }
    let current = crate::eloquent_members::class_inventory(&fresh);
    reference_chain(&current, class, target, &root).ok_or(Error::Stale)?;
    serde_json::to_string(&files).map_err(|_| Error::Stale)
}
fn index_identity(index: &crate::index::Index) -> String {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    index.source_root.hash(&mut hash);
    index.source_evidence.hash(&mut hash);
    index.source_queries.hash(&mut hash);
    index.source_settings.hash(&mut hash);
    format!("{:016x}", hash.finish())
}
fn build_package(
    project: &Project,
    request: &crate::package_generators::Request,
    module: Option<&str>,
    models: &crate::eloquent_members::Models,
    bundle: &crate::generator_presets::Bundle,
    allowed_fields: &BTreeMap<String, std::collections::BTreeSet<String>>,
    _model_relations: &BTreeMap<String, Vec<generator_plan::Relation>>,
    model_schemas: &BTreeMap<String, Value>,
    model_casts: &BTreeMap<String, BTreeMap<String, String>>,
    new_model: Option<&Draft>,
    current_index: &crate::index::Index,
) -> Result<(Plan, String), Error> {
    if !bundle.available(&request.kind, module) {
        return Err(Error::Invalid(
            "Package generator is unavailable in the selected application/module".into(),
        ));
    }
    let mut captured_request = request.clone();
    for field in &mut captured_request.fields {
        if new_model.is_some() {
            continue;
        }
        field.cast = model_casts
            .get(&request.model)
            .and_then(|casts| casts.get(&field.name))
            .cloned();
    }
    let defaults = bundle
        .package_defaults(&request.kind, module)
        .map_err(Error::Invalid)?;
    if captured_request.label_mode.is_none() {
        captured_request.label_mode =
            Some(serde_json::from_value(defaults["labelMode"].clone()).map_err(|_| Error::Stale)?);
    }
    if captured_request.label_prefix.is_none() {
        captured_request.label_prefix = defaults["labelPrefix"].as_str().map(str::to_owned);
    }
    if captured_request.kind == "filament-resource" && captured_request.separate.is_none() {
        captured_request.separate = defaults["separate"].as_bool();
    }
    let request = &captured_request;
    let versions = package_versions(project);
    let package = if request.kind == "nova-resource" {
        "laravel/nova"
    } else {
        "filament/filament"
    };
    let version = versions.get(package).ok_or_else(|| {
        Error::Invalid(format!(
            "Enable and install a supported {package} version before generating"
        ))
    })?;
    let major = crate::package_generators::major(version).ok_or(Error::Stale)?;
    if current_index.partial {
        return Err(Error::Stale);
    }
    let root = project.root()?;
    if new_model.is_none()
        && !model_catalog(models, &root)
            .iter()
            .any(|item| item["class"].as_str() == Some(request.model.trim_start_matches('\\')))
    {
        return Err(Error::Invalid("Choose an indexed Eloquent model".into()));
    }
    let model_source = if let Some(draft) = new_model {
        if request.model
            != format!(
                "{}\\{}",
                draft.namespace_for("model"),
                draft.class_name("model")
            )
        {
            return Err(Error::Stale);
        }
        serde_json::to_string(draft).map_err(|_| Error::Stale)?
    } else {
        fresh_model_evidence(project, models, &[request.model.clone()])?
    };
    let allowed = if let Some(draft) = new_model {
        draft
            .fields
            .iter()
            .map(|field| field.name.clone())
            .chain(std::iter::once(draft.primary_key.name.clone()))
            .collect()
    } else {
        allowed_fields
            .get(&request.model)
            .cloned()
            .unwrap_or_default()
    };
    let schema = model_schemas.get(&request.model);
    let captured = schema
        .and_then(|schema| schema["columns"].as_array())
        .into_iter()
        .flatten()
        .filter_map(|column| column["name"].as_str())
        .collect::<std::collections::BTreeSet<_>>();
    if request
        .fields
        .iter()
        .any(|field| !allowed.contains(&field.name) && !captured.contains(field.name.as_str()))
        || !allowed.contains(&request.primary_key.name)
            && !captured.contains(request.primary_key.name.as_str())
    {
        return Err(Error::Invalid("Package fields and primary key must be declared by indexed/captured schema metadata; capture schema or review an indexed field mapping".into()));
    }
    let relations = if let Some(draft) = new_model {
        draft.relations.clone()
    } else {
        crate::eloquent_members::helper_facts::facts(&current_index, &request.model, &|path| {
            owned(&root, path)
        })
        .map(|facts| {
            facts
                .relations
                .into_iter()
                .map(|relation| generator_plan::Relation {
                    name: relation.name,
                    kind: relation.kind,
                    related: relation.target,
                    through: None,
                    arguments: Vec::new(),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
    };
    if request.relations.iter().any(|relation| {
        !relations.iter().any(|known| {
            known.name == relation.name
                && known.related == relation.related
                && known.kind == relation.kind
        })
    }) {
        return Err(Error::Invalid(
            "Choose currently indexed package model relationships".into(),
        ));
    }

    let native = bundle.native_settings(module).map_err(Error::Invalid)?;
    let options = native.kinds.get(&request.kind);
    let plural = crate::generator_presets::inflect(&request.name, true).map_err(Error::Invalid)?;
    let (directory, namespace, postfix) = match request.kind.as_str() {
        "nova-resource" => (
            format!("{}/Nova", native.source_root),
            format!("{}\\Nova", native.namespace),
            "",
        ),
        "filament-resource" if major >= 4 => (
            format!("{}/Filament/Resources/{plural}", native.source_root),
            format!("{}\\Filament\\Resources\\{plural}", native.namespace),
            "Resource",
        ),
        "filament-resource" => (
            format!("{}/Filament/Resources", native.source_root),
            format!("{}\\Filament\\Resources", native.namespace),
            "Resource",
        ),
        "filament-schema" => (
            format!("{}/Filament/Schemas", native.source_root),
            format!("{}\\Filament\\Schemas", native.namespace),
            "Form",
        ),
        "filament-table" => (
            format!("{}/Filament/Tables", native.source_root),
            format!("{}\\Filament\\Tables", native.namespace),
            "Table",
        ),
        "filament-relation-manager" => (
            format!("{}/Filament/RelationManagers", native.source_root),
            format!("{}\\Filament\\RelationManagers", native.namespace),
            "RelationManager",
        ),
        _ => return Err(Error::Invalid("Unknown package generator".into())),
    };
    let base = options.and_then(|option| option.base_class.clone());
    let target = if package == "laravel/nova" {
        "Laravel\\Nova\\Resource"
    } else if request.kind == "filament-relation-manager" {
        "Filament\\Resources\\RelationManagers\\RelationManager"
    } else {
        "Filament\\Resources\\Resource"
    };
    let mut proof = String::new();
    let mut owner_relation_proven = false;
    if request.kind == "filament-relation-manager" {
        let owner = request
            .owner_model
            .as_deref()
            .ok_or_else(|| Error::Invalid("Choose the indexed owner model".into()))?;
        let relationship = request.relationship.as_deref().ok_or(Error::Stale)?;
        let facts = crate::eloquent_members::helper_facts::facts(&current_index, owner, &|path| {
            owned(&root, path)
        })
        .ok_or(Error::Stale)?;
        if !facts.relations.iter().any(|relation| {
            relation.name == relationship
                && relation.target == request.model
                && Some(relation.kind.as_str()) == request.relation_kind.as_deref()
                && relation.many
        }) {
            return Err(Error::Invalid(
                "Choose a current collection relation on the selected owner model".into(),
            ));
        }
        proof.push_str(&fresh_model_evidence(
            project,
            &current_index.eloquent_models,
            &[owner.into()],
        )?);
        owner_relation_proven = true;
    }

    let proven_base = if let Some(base) = base.as_deref().filter(|base| *base != target) {
        proof.push_str(&fresh_ancestor_evidence(project, models, base, target)?);
        true
    } else {
        false
    };
    let mut nova_resources = std::collections::BTreeSet::new();
    for class in request.nova_resources.values().flatten() {
        let reference_proof =
            fresh_ancestor_evidence(project, models, class, "Laravel\\Nova\\Resource")?;
        let files: BTreeMap<String, String> =
            serde_json::from_str(&reference_proof).map_err(|_| Error::Stale)?;
        let inventory = crate::eloquent_members::class_inventory(models);
        let identity = inventory
            .iter()
            .find(|identity| identity.full.eq_ignore_ascii_case(class))
            .ok_or(Error::Stale)?;
        if let Some(bound) = files
            .get(&identity.path)
            .and_then(|source| crate::eloquent_members::related_facts::declared_model_class(source))
        {
            for (name, targets) in &request.nova_resources {
                if targets.contains(class) {
                    if let Some(relation) = request
                        .relations
                        .iter()
                        .find(|relation| relation.name == *name && relation.kind != "morphTo")
                    {
                        if !relation
                            .related
                            .trim_start_matches('\\')
                            .eq_ignore_ascii_case(&bound)
                        {
                            return Err(Error::Invalid(
                                "Selected Nova resource belongs to a different related model"
                                    .into(),
                            ));
                        }
                    }
                }
            }
        }
        proof.push_str(&reference_proof);
        nova_resources.insert(class.clone());
    }
    let settings = crate::package_generators::Settings {
        directory: options
            .and_then(|option| option.directory.clone())
            .unwrap_or(directory),
        namespace: options
            .and_then(|option| option.namespace.clone())
            .unwrap_or(namespace),
        postfix: options
            .and_then(|option| option.postfix.clone())
            .unwrap_or(postfix.into()),
        base_class: base,
        proven_base,
        proven_nova_resources: nova_resources,
        owner_relation_proven,
    };
    // Current installed source is captured with the effective version; a stale
    // package replacement invalidates an already reviewed transaction.
    let base_path = if package == "laravel/nova" {
        "vendor/laravel/nova/src/Resource.php"
    } else if request.kind == "filament-relation-manager" {
        "vendor/filament/filament/src/Resources/RelationManagers/RelationManager.php"
    } else {
        "vendor/filament/filament/src/Resources/Resource.php"
    };
    let absolute = format!("{root}/{base_path}");
    if !owned(&root, &absolute) || project.stat(base_path)?.kind != openidea_plugin::FileKind::File
    {
        return Err(Error::Stale);
    }
    let package_source = project.read_file(base_path)?;
    if package_source.len() > 128 << 10
        || crate::eloquent_members::declaration(&package_source).as_deref() != Some(target)
    {
        return Err(Error::Invalid(
            "Installed package base declaration does not match the supported contract".into(),
        ));
    }
    let mut plan =
        crate::package_generators::plan(request, version, &settings).map_err(Error::Invalid)?;
    let class = format!("{}{}", request.name, settings.postfix);
    let principal = format!("{}\\{class}", settings.namespace);
    let table = new_model
        .map(|draft| draft.table.clone())
        .or_else(|| {
            model_schemas
                .get(&request.model)
                .and_then(|schema| schema["table"].as_str())
                .map(str::to_owned)
        })
        .or_else(|| {
            model_catalog(models, &root)
                .into_iter()
                .find(|identity| identity["class"].as_str() == Some(request.model.as_str()))
                .and_then(|identity| identity["table"].as_str().map(str::to_owned))
        })
        .unwrap_or_default();
    let output = plan
        .files
        .iter_mut()
        .find(|file| {
            crate::eloquent_members::declaration(&file.text).as_deref() == Some(principal.as_str())
        })
        .ok_or(Error::Stale)?;
    output.text = bundle
        .render_package_output(
            &request.kind,
            module,
            &principal,
            &request.model,
            &table,
            &output.text,
            native.type_hints,
        )
        .map_err(Error::Invalid)?;
    let composer = plan_mapping(project, &plan, false)?;
    Ok((
        plan,
        format!(
            "{}\n{model_source}\n{proof}\n{version}\n{package_source}\n{composer}\nindex:{}",
            bundle.fingerprint(),
            index_identity(current_index)
        ),
    ))
}
fn build(
    project: &Project,
    input: &Input,
    models: &crate::eloquent_members::Models,
    allowed_fields: &BTreeMap<String, std::collections::BTreeSet<String>>,
    model_relations: &BTreeMap<String, Vec<generator_plan::Relation>>,
    model_schemas: &BTreeMap<String, Value>,
    model_casts: &BTreeMap<String, BTreeMap<String, String>>,
    captured: Option<&crate::index::Index>,
) -> Result<(Plan, String), Error> {
    let bundle = crate::generator_presets::load(project)?;
    let fingerprint = bundle.fingerprint();
    let needs_index = match input {
        Input::Package { .. } => true,
        Input::Native { request, .. } => {
            request.model.is_some()
                || request.user_model.is_some()
                || request.event.is_some()
                || !request.dependencies.is_empty()
        }
        Input::Model {
            draft, packages, ..
        } => {
            !packages.is_empty()
                || !draft.resource_wrappers.is_empty()
                || draft.factory_mappings.values().any(|mapping| {
                    matches!(mapping, generator_plan::FactoryMapping::Factory { .. })
                })
                || draft.related.contains("factory")
                    && (draft
                        .relations
                        .iter()
                        .any(|relation| relation.kind == "belongsTo")
                        || draft.factory_mappings.values().any(|mapping| {
                            matches!(
                                mapping,
                                generator_plan::FactoryMapping::Factory { inferred: true, .. }
                            )
                        }))
        }
        Input::Preset { preset, values, .. } => !bundle
            .model_inputs(preset, values)
            .map_err(Error::Invalid)?
            .is_empty(),
    };
    let current_index = if needs_index {
        let index = captured.ok_or(Error::Stale)?;
        if !crate::index::index_sources_current(project, index) {
            return Err(Error::Stale);
        }
        Some(index)
    } else {
        None
    };
    let fingerprint = format!(
        "{fingerprint}\nindex:{}",
        current_index.map(index_identity).unwrap_or_default()
    );
    match input {
        Input::Native {
            request,
            module,
            choices,
        } => {
            if !bundle.available(&request.kind, module.as_deref()) {
                return Err(Error::Invalid(
                    "Generator is unavailable in the selected application/module".into(),
                ));
            }
            let mut chosen = request
                .dependencies
                .iter()
                .map(|dependency| dependency.class.clone())
                .collect::<Vec<_>>();
            chosen.extend(
                request
                    .model
                    .iter()
                    .chain(request.user_model.iter())
                    .chain(request.event.iter())
                    .cloned(),
            );
            let source = owned_class_evidence(project, models, &chosen)?;
            if matches!(
                request.kind.as_str(),
                "factory" | "policy" | "observer" | "resource-controller"
            ) {
                if let Some(model) = &request.model {
                    if !model_catalog(models, &project.root()?)
                        .iter()
                        .any(|item| item["class"].as_str() == Some(model.trim_start_matches('\\')))
                    {
                        return Err(Error::Invalid("Select an indexed Eloquent model".into()));
                    }
                }
            }
            if matches!(request.kind.as_str(), "factory" | "resource") && request.model.is_some() {
                let index = current_index.ok_or(Error::Stale)?;
                if index.partial {
                    return Err(Error::Stale);
                }
                let model = request.model.as_deref().ok_or(Error::Stale)?;
                let kind = if request.kind == "factory" {
                    "factory"
                } else {
                    "resource"
                };
                let choices = if choices.is_null() {
                    json!({})
                } else {
                    choices.clone()
                };
                let native_defaults = bundle
                    .native_settings(module.as_deref())
                    .map_err(Error::Invalid)?;
                let postfix = native_defaults
                    .kinds
                    .get(kind)
                    .and_then(|option| option.postfix.as_deref())
                    .unwrap_or(if kind == "factory" {
                        "Factory"
                    } else {
                        "Resource"
                    });
                let name = if postfix.is_empty() || request.name.ends_with(postfix) {
                    request.name.clone()
                } else {
                    format!("{}{postfix}", request.name)
                };
                let (path, text, evidence) = crate::bulk::related_preview_in_module(
                    project,
                    &index,
                    kind,
                    model,
                    &name,
                    Some(module.as_deref()),
                    &choices,
                )?;
                let root = project.root()?;
                let relative = path
                    .strip_prefix(&format!("{root}/"))
                    .ok_or(Error::Stale)?
                    .to_string();
                let plan = Plan {
                    files: vec![generator_plan::Output {
                        path: relative,
                        text,
                    }],
                };
                let composer = plan_mapping(project, &plan, kind == "factory")?;
                return Ok((
                    plan,
                    format!("{fingerprint}\n{source}\n{evidence}\n{composer}"),
                ));
            }
            let settings = bundle
                .native_settings(module.as_deref())
                .map_err(Error::Invalid)?;
            let (plan, stub) = crate::native_catalog::plan(project, request, &settings)?;
            let composer = plan_mapping(
                project,
                &plan,
                matches!(
                    request.kind.as_str(),
                    "factory" | "seeder" | "feature-test" | "unit-test"
                ),
            )?;
            Ok((plan, format!("{fingerprint}\n{source}\n{stub}\n{composer}")))
        }
        Input::Package { request, module } => build_package(
            project,
            request,
            module.as_deref(),
            models,
            &bundle,
            allowed_fields,
            model_relations,
            model_schemas,
            model_casts,
            None,
            current_index.ok_or(Error::Stale)?,
        ),
        Input::Model {
            draft,
            module,
            packages,
            package_options,
        } => {
            if packages.len() > 2
                || packages
                    .iter()
                    .any(|kind| !matches!(kind.as_str(), "nova-resource" | "filament-resource"))
                || packages
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    != packages.len()
            {
                return Err(Error::Invalid(
                    "Choose unique supported New Model package outputs".into(),
                ));
            }
            let mut draft = draft.clone();
            bundle.apply_settings(&mut draft);
            if let Some(module) = module {
                bundle
                    .apply_module(&mut draft, module)
                    .map_err(Error::Invalid)?;
            }
            bundle
                .apply_generator_options(&mut draft, module.as_deref())
                .map_err(Error::Invalid)?;
            let mut inferred_evidence = BTreeMap::new();
            let mut inferred_models = None;
            if draft.related.contains("factory")
                && (draft
                    .relations
                    .iter()
                    .any(|relation| relation.kind == "belongsTo")
                    || draft.factory_mappings.values().any(|mapping| {
                        matches!(
                            mapping,
                            generator_plan::FactoryMapping::Factory { inferred: true, .. }
                        )
                    }))
            {
                let current = current_index.ok_or(Error::Stale)?;
                let defaults = factory_defaults::for_draft(project, &current, &draft);
                if !current.partial {
                    inferred_models = Some(current.eloquent_models.clone());
                }
                for (field, mapping) in &draft.factory_mappings {
                    if let generator_plan::FactoryMapping::Factory {
                        class,
                        inferred: true,
                    } = mapping
                    {
                        let suggestion = defaults
                            .get(field)
                            .filter(|suggestion| suggestion.class == *class)
                            .ok_or(Error::Stale)?;
                        inferred_evidence.insert(field.clone(), suggestion.evidence.clone());
                    }
                }
                for (field, suggestion) in defaults {
                    if !draft.factory_mappings.contains_key(&field) {
                        draft.factory_mappings.insert(
                            field.clone(),
                            generator_plan::FactoryMapping::Factory {
                                class: suggestion.class,
                                inferred: true,
                            },
                        );
                        inferred_evidence.insert(field, suggestion.evidence);
                    }
                }
            }
            let composer = mapping(project, &draft)?;
            let (mut references, source_evidence) =
                fresh_references(project, inferred_models.as_ref().unwrap_or(models), &draft)?;
            references.templates = bundle
                .generated_templates(&draft, module.as_deref())
                .map_err(Error::Invalid)?;
            let mut plan = generator_plan::plan_with_references(&draft, &references)
                .map_err(Error::Invalid)?;
            let mut package_evidence = String::new();
            for kind in packages {
                let request:crate::package_generators::Request=serde_json::from_value(json!({"kind":kind,"name":draft.name,"model":format!("{}\\{}",draft.namespace_for("model"),draft.class_name("model")),"fields":draft.fields,"relations":draft.relations,"primaryKey":draft.primary_key,"selectedFields":draft.resource_fields,"selectedRelations":if kind=="nova-resource"{draft.resource_relations.clone()}else{Some(Vec::<String>::new())},"separate":if kind=="filament-resource"{package_options.filament_separate}else{None},"labelMode":if kind=="nova-resource"{package_options.nova_label_mode.clone()}else{None},"labelPrefix":if kind=="nova-resource"{package_options.nova_label_prefix.clone()}else{None},"novaResources":if kind=="nova-resource"{package_options.nova_resources.clone()}else{BTreeMap::new()}})).map_err(|_|Error::Stale)?;
                let (package_plan, evidence) = build_package(
                    project,
                    &request,
                    module.as_deref(),
                    models,
                    &bundle,
                    allowed_fields,
                    model_relations,
                    model_schemas,
                    model_casts,
                    Some(&draft),
                    current_index.ok_or(Error::Stale)?,
                )?;
                plan.files.extend(package_plan.files);
                package_evidence.push_str(&evidence);
            }
            plan.files.sort_by(|left, right| left.path.cmp(&right.path));
            let actual_composer = plan_mapping(project, &plan, true)?;
            Ok((
                plan,
                format!(
                    "{fingerprint}\n{composer}\n{actual_composer}\n{source_evidence}\n{package_evidence}\n{}",
                    serde_json::to_string(&inferred_evidence).map_err(|_| Error::Stale)?
                ),
            ))
        }
        Input::Preset {
            current_time,
            preset,
            values,
            module,
            overrides,
        } => {
            let bundle = bundle
                .with_current_time(current_time)
                .map_err(Error::Invalid)?;
            let fingerprint = bundle.fingerprint();
            let chosen = indexed_model_inputs(&bundle, models, preset, values, &project.root()?)?;
            let evidence = fresh_model_evidence(project, models, &chosen)?;
            Ok((
                bundle
                    .render_with_overrides(preset, values, module.as_deref(), overrides)
                    .map_err(Error::Invalid)?,
                format!("{fingerprint}\n{evidence}"),
            ))
        }
    }
}
fn snapshot(plan: &Plan) -> Value {
    json!({"files":plan.files,"canPrepare":true})
}
/// Translate only exact authored static validator messages at the delivery
/// boundary. Dynamic errors, paths and project source are never rewritten.
pub(crate) fn ui_error(error: Error) -> Error {
    let Error::Invalid(message) = error else {
        return error;
    };
    let message = match message.as_str() {
        "A generated target is open in the editor" => {
            crate::ui_text::t("generator.error.e054bb01f357", &message)
        }
        "A generator refers to a missing template" => {
            crate::ui_text::t("generator.error.14e77c580f33", &message)
        }
        "A preset needs 1–64 files and at most 64 inputs/calculations" => {
            crate::ui_text::t("generator.error.d08f2ca42abc", &message)
        }
        "Anonymous migration bases are provided by the migration template" => {
            crate::ui_text::t("generator.error.7deed39411f7", &message)
        }
        "Anonymous migrations do not have a class postfix" => {
            crate::ui_text::t("generator.error.e8399669c228", &message)
        }
        "Application namespaces and directories must be explicit qualified/relative names" => {
            crate::ui_text::t("generator.error.f867517e3cce", &message)
        }
        "Arbitrary text needs the php transform for an escaped PHP literal" => {
            crate::ui_text::t("generator.error.5351f3f1bc1a", &message)
        }
        "At most 64 presets and 64 modules are supported" => {
            crate::ui_text::t("generator.error.4b48b6c651a2", &message)
        }
        "Binary Filament fields require an explicit upload/storage mapping" => {
            crate::ui_text::t("generator.error.11eb8e46f61d", &message)
        }
        "Binary Nova fields require an explicit upload/storage mapping" => {
            crate::ui_text::t("generator.error.91b453887914", &message)
        }
        "Binary table fields require an explicit display mapping" => {
            crate::ui_text::t("generator.error.337bbc30229f", &message)
        }
        "Calculated inputs need a unique name and exactly one substitution expression" => {
            crate::ui_text::t("generator.error.ff8aacaf7df6", &message)
        }
        "Calculated transformation exceeds the bounded value limit or contains NUL" => {
            crate::ui_text::t("generator.error.fe10cbd75506", &message)
        }
        "Calculated values must be at most 4096 bytes" => {
            crate::ui_text::t("generator.error.5c7e0c383c1b", &message)
        }
        "Captured package schemas exceed the 8 MiB project budget" => {
            crate::ui_text::t("generator.error.f27b24a01b09", &message)
        }
        "Choose 1–32 declared PSR-4 roots" => {
            crate::ui_text::t("generator.error.4ae9f7441123", &message)
        }
        "Choose a current collection relation on the selected owner model" => {
            crate::ui_text::t("generator.error.cb0e7c30b955", &message)
        }
        "Choose a declared related record title field" => {
            crate::ui_text::t("generator.error.6009c1fdbe41", &message)
        }
        "Choose an indexed Eloquent model" => {
            crate::ui_text::t("generator.error.55c7b8c04364", &message)
        }
        "Choose at most 16 constructor dependencies" => {
            crate::ui_text::t("generator.error.25630508d182", &message)
        }
        "Choose currently indexed package model relationships" => {
            crate::ui_text::t("generator.error.5d709ad276dc", &message)
        }
        "Choose indexed Nova resource types for polymorphic MorphTo fields" => {
            crate::ui_text::t("generator.error.582bc0d2daf6", &message)
        }
        "Choose the indexed owner model" => {
            crate::ui_text::t("generator.error.8415102a8a1a", &message)
        }
        "Choose unique package fields/relations from the captured model" => {
            crate::ui_text::t("generator.error.ec0cfe7d9972", &message)
        }
        "Choose unique supported New Model package outputs" => {
            crate::ui_text::t("generator.error.7431cf50ffb3", &message)
        }
        "Class selection exceeds exact generation proof budget" => {
            crate::ui_text::t("generator.error.4dbfb3257ae6", &message)
        }
        "Composer mapping produced an invalid destination" => {
            crate::ui_text::t("generator.error.11d61c2ee585", &message)
        }
        "Composer metadata cannot cross a symlink or non-directory" => {
            crate::ui_text::t("generator.error.d11c6c3c5239", &message)
        }
        "Composer namespace evidence exceeds 2 MiB" => {
            crate::ui_text::t("generator.error.5f72242e3358", &message)
        }
        "Composer source root escapes this project" => {
            crate::ui_text::t("generator.error.5834998a5cd0", &message)
        }
        "Composer source roots must remain slash-normalized project paths" => {
            crate::ui_text::t("generator.error.9566da025776", &message)
        }
        "Conditional nesting limit exceeded" => {
            crate::ui_text::t("generator.error.af1fd85465ad", &message)
        }
        "Conditions require a boolean variable" => {
            crate::ui_text::t("generator.error.5e0486e138c7", &message)
        }
        "Conditions require a declared boolean value" => {
            crate::ui_text::t("generator.error.e54d23c573ff", &message)
        }
        "Configured generated class name is invalid or too long" => {
            crate::ui_text::t("generator.error.f3c63d60bb4b", &message)
        }
        "Configured package base must be a proven indexed subclass" => {
            crate::ui_text::t("generator.error.7280d6687b42", &message)
        }
        "Constructor dependencies need distinct names and fully qualified types" => {
            crate::ui_text::t("generator.error.f403f4d720a4", &message)
        }
        "Database connection names need 1–128 characters without controls" => {
            crate::ui_text::t("generator.error.c132bdcd25a2", &message)
        }
        "Date inputs must use yyyy-MM-dd HH:mm:ss in UTC" => {
            crate::ui_text::t("generator.error.36ad70b40d53", &message)
        }
        "Date patterns exceed the 1024-byte limit" => {
            crate::ui_text::t("generator.error.106255452cdc", &message)
        }
        "Defaults must be a JSON string, number, boolean or null" => {
            crate::ui_text::t("generator.error.2016f310c21b", &message)
        }
        "Duplicate else" => crate::ui_text::t("generator.error.3b5b98de6ac8", &message),
        "Duplicate generated path" => crate::ui_text::t("generator.error.e9f7bb729937", &message),
        "Duplicate package field" => crate::ui_text::t("generator.error.18f383313c5b", &message),
        "Enum/set default is not among the declared values" => {
            crate::ui_text::t("generator.error.9fe70e4a9786", &message)
        }
        "Enum/set defaults must be strings" => {
            crate::ui_text::t("generator.error.d5e207103a31", &message)
        }
        "Enum/set fields need 1–100 distinct nonempty values" => {
            crate::ui_text::t("generator.error.338780eed1ce", &message)
        }
        "Expanded template exceeds 128 KiB" => {
            crate::ui_text::t("generator.error.c12fad8ecffd", &message)
        }
        "Expressions allow at most 64 transforms" => {
            crate::ui_text::t("generator.error.f145632c38d6", &message)
        }
        "Expressions exceed 4096 bytes" => {
            crate::ui_text::t("generator.error.f27f834e1683", &message)
        }
        "Factory mapping requires a current owned indexed Factory subclass" => {
            crate::ui_text::t("generator.error.b79090a3f8ea", &message)
        }
        "Field label prefixes must be bounded text without controls" => {
            crate::ui_text::t("generator.error.06a588a08588", &message)
        }
        "Formatted date exceeds 4096 bytes" => {
            crate::ui_text::t("generator.error.a56414b33226", &message)
        }
        "Generated PHP classes conflict" => {
            crate::ui_text::t("generator.error.3502d3ca500f", &message)
        }
        "Generated PHP declarations conflict" => {
            crate::ui_text::t("generator.error.ee8ed8406d74", &message)
        }
        "Generated class requires namespace" => {
            crate::ui_text::t("generator.error.8e8f74980b3e", &message)
        }
        "Generated files exceed 128 KiB/file, 128 files or 2 MiB transaction limits" => {
            crate::ui_text::t("generator.error.a9f40547edad", &message)
        }
        "Generated files have conflicting paths" => {
            crate::ui_text::t("generator.error.3c0432c7a298", &message)
        }
        "Generated template output exceeds 128 KiB" => {
            crate::ui_text::t("generator.error.1bf3bc987d5c", &message)
        }
        "Generated template output exceeds limits" => {
            crate::ui_text::t("generator.error.e5088ac45f6e", &message)
        }
        "Generated/model identities must be qualified classes" => {
            crate::ui_text::t("generator.error.53ab5fd8a8d4", &message)
        }
        "Generation roots must be relative project paths" => {
            crate::ui_text::t("generator.error.82d43a41ba1e", &message)
        }
        "Generator base class, postfix or template is invalid" => {
            crate::ui_text::t("generator.error.002b31539468", &message)
        }
        "Generator directories require safe relative paths with optional sourceRoot/moduleRoot placeholders" => {
            crate::ui_text::t("generator.error.f52c093ddb77", &message)
        }
        "Generator is unavailable in the selected application/module" => {
            crate::ui_text::t("generator.error.685409739891", &message)
        }
        "Generator namespaces require a qualified class namespace with optional namespace placeholder" => {
            crate::ui_text::t("generator.error.24dcf8fcfc8a", &message)
        }
        "Generator postfixes must be PHP identifier suffixes" => {
            crate::ui_text::t("generator.error.b759e1b2c0e3", &message)
        }
        "Generator request is too large" => {
            crate::ui_text::t("generator.error.c4b33bee99c6", &message)
        }
        "Generator source root exceeds Composer-discovery depth" => {
            crate::ui_text::t("generator.error.58f6cd0f449c", &message)
        }
        "Generator source roots must be project-relative paths" => {
            crate::ui_text::t("generator.error.84ac2618fa13", &message)
        }
        "Include requires a registered shared template" => {
            crate::ui_text::t("generator.error.eb28b937735f", &message)
        }
        "Include requires a scoped relative template name" => {
            crate::ui_text::t("generator.error.1122752a876d", &message)
        }
        "Installed package base declaration does not match the supported contract" => {
            crate::ui_text::t("generator.error.1b5919cde195", &message)
        }
        "Intermediate classes apply only to through relations" => {
            crate::ui_text::t("generator.error.d06ce0d52bbf", &message)
        }
        "Invalid Composer source-root segment" => {
            crate::ui_text::t("generator.error.01a568674baf", &message)
        }
        "Invalid Gregorian UTC date" => crate::ui_text::t("generator.error.c4f2ecc50a69", &message),
        "Invalid class name" => crate::ui_text::t("generator.error.66dc975e8826", &message),
        "Invalid decimal precision/scale" => {
            crate::ui_text::t("modelUi.model_from_schema.ae2aa3c384", &message)
        }
        "Invalid field name" => crate::ui_text::t("generator.error.7da6876f4b1c", &message),
        "Invalid generated class or path" => {
            crate::ui_text::t("generator.error.5c0e4e9f16b5", &message)
        }
        "Invalid index name or columns" => {
            crate::ui_text::t("modelUi.existing_model_fields.5c58936c7d", &message)
        }
        "Invalid module factory/seeder layout" => {
            crate::ui_text::t("generator.error.f77d695a7c34", &message)
        }
        "Invalid package class name" => crate::ui_text::t("generator.error.7ce4596fbd74", &message),
        "Invalid package primary key" => {
            crate::ui_text::t("generator.error.f8ccdf16c289", &message)
        }
        "Invalid package relation" => crate::ui_text::t("generator.error.9716a2787336", &message),
        "Invalid package resource name, model, namespace, directory or postfix" => {
            crate::ui_text::t("generator.error.f9c1dd0bad74", &message)
        }
        "Invalid per-generator destination" => {
            crate::ui_text::t("generator.error.2977843bba67", &message)
        }
        "Invalid primary key name or strategy" => {
            crate::ui_text::t("generator.error.afbb9216c106", &message)
        }
        "Invalid qualified class name" => {
            crate::ui_text::t("generator.error.3ae10430a128", &message)
        }
        "Invalid related-file roots or namespaces" => {
            crate::ui_text::t("generator.error.5314b8af2a0c", &message)
        }
        "Label/schema options apply only to Nova and Filament generators" => {
            crate::ui_text::t("generator.error.2478757b464b", &message)
        }
        "Length applies to string/char fields and must be 1–65535" => {
            crate::ui_text::t("generator.error.f2326fc3afc2", &message)
        }
        "Migration timestamp must use YYYY_MM_DD_HHMMSS" => {
            crate::ui_text::t("generator.error.6dcf476b58ce", &message)
        }
        "Model inventory exceeds the exact generation snapshot budget" => {
            crate::ui_text::t("generator.error.005bd74e24be", &message)
        }
        "Model name, table and namespace must be valid PHP identifiers" => {
            crate::ui_text::t("generator.error.64a1f7be6c9e", &message)
        }
        "Model sources exceed generation snapshot limits" => {
            crate::ui_text::t("generator.error.511e04adfa16", &message)
        }
        "Module generator overrides cannot set Application availability" => {
            crate::ui_text::t("generator.error.b636cfcb64a6", &message)
        }
        "Modules require unique identifiers, an explicit namespace and relative source/migration/view roots" => {
            crate::ui_text::t("generator.error.6bfcd65cb994", &message)
        }
        "Name contains no words" => crate::ui_text::t("generator.error.90da4b38b32d", &message),
        "Name transformation produced no name" => {
            crate::ui_text::t("generator.error.259fdc52e29e", &message)
        }
        "Name transformations need 1–4096 bytes" => {
            crate::ui_text::t("generator.error.e771a495e649", &message)
        }
        "Name transforms accept letters, numbers, whitespace, underscores, hyphens and dots" => {
            crate::ui_text::t("generator.error.d728c4b263ae", &message)
        }
        "Name transforms require a string input" => {
            crate::ui_text::t("generator.error.9ee9f4402473", &message)
        }
        "Non-polymorphic Nova relations select one resource class" => {
            crate::ui_text::t("generator.error.f4e8debcb554", &message)
        }
        "Nova relation targets require current indexed Nova resource classes" => {
            crate::ui_text::t("generator.error.195949763d03", &message)
        }
        "Nova resource mappings require declared relationships" => {
            crate::ui_text::t("generator.error.f640595ead22", &message)
        }
        "Only string/boolean values can be substituted" => {
            crate::ui_text::t("generator.error.a91b0d1af72e", &message)
        }
        "Open a Laravel project first" => {
            crate::ui_text::t("native.model_actions.a74a0baad4", &message)
        }
        "Open an existing saved PHP file in this project to anchor the undoable generation action" => {
            crate::ui_text::t("generator.error.047a6278ab71", &message)
        }
        "Package class proof exceeds limits" => {
            crate::ui_text::t("generator.error.70c550428d71", &message)
        }
        "Package field label prefix is invalid" => {
            crate::ui_text::t("generator.error.df87e5bedd00", &message)
        }
        "Package fields and primary key must be declared by indexed/captured schema metadata; capture schema or review an indexed field mapping" => {
            crate::ui_text::t("generator.error.75d4fd6ad203", &message)
        }
        "Package fields/relations exceed generation limits" => {
            crate::ui_text::t("generator.error.62143efebf4e", &message)
        }
        "Package generation requires an exact known installed version" => {
            crate::ui_text::t("generator.error.ad3ae4fd593b", &message)
        }
        "Package generator is unavailable in the selected application/module" => {
            crate::ui_text::t("generator.error.ce2e9c466be2", &message)
        }
        "Parameter defaults must match their input type; null is not an input value" => {
            crate::ui_text::t("generator.error.d21a225e2549", &message)
        }
        "Per-generator namespaces/postfixes produce duplicate PHP classes" => {
            crate::ui_text::t("generator.error.6ec4e5747cc1", &message)
        }
        "Precision and scale apply to decimal fields" => {
            crate::ui_text::t("generator.error.d108f1d0d822", &message)
        }
        "Preset ids must be unique identifiers with a nonempty label" => {
            crate::ui_text::t("generator.error.2a698b2e82e6", &message)
        }
        "Preset is unavailable in the selected application/module" => {
            crate::ui_text::t("generator.error.b1f1a4ebbe2b", &message)
        }
        "Preset moduleIds must contain at most 64 unique module ids" => {
            crate::ui_text::t("generator.error.83073091db72", &message)
        }
        "Preset templates exceed the 128 KiB/file or 2 MiB bundle limit" => {
            crate::ui_text::t("generator.error.9d4fb685b18b", &message)
        }
        "Primary key factory literals cannot be null" => {
            crate::ui_text::t("generator.error.e1a9a52773db", &message)
        }
        "Primary key length applies only to string keys (1–65535)" => {
            crate::ui_text::t("generator.error.feb99cd839ff", &message)
        }
        "Reference inventory exceeds generation proof limits" => {
            crate::ui_text::t("generator.error.fae92a403c2e", &message)
        }
        "Reference source proof exceeds 128 KiB/file or 8 MiB total" => {
            crate::ui_text::t("generator.error.251d9be988c7", &message)
        }
        "Relation manager owner and relationship require current indexed source proof" => {
            crate::ui_text::t("generator.error.aaf81e623453", &message)
        }
        "Relation managers require a proven collection relationship" => {
            crate::ui_text::t("generator.error.98358c18bc1f", &message)
        }
        "Rendered output exceeds generation limits" => {
            crate::ui_text::t("generator.error.d7c24fe9a1f6", &message)
        }
        "Repeated date pattern fields exceed the 64-character limit" => {
            crate::ui_text::t("generator.error.c824603663b2", &message)
        }
        "Replacement exceeds value budget" => {
            crate::ui_text::t("generator.error.cd47d2f713b2", &message)
        }
        "Resolved parameter values exceed the live-preview budget" => {
            crate::ui_text::t("generator.error.9d7ff941d47b", &message)
        }
        "Resource wrapper requires a declared relation and a current owned indexed JsonResource subclass" => {
            crate::ui_text::t("generator.error.140fbff0fed0", &message)
        }
        "Review explicit nullable true/false for every package field mapping" => {
            crate::ui_text::t("generator.error.30150df88f9b", &message)
        }
        "Schema references must be bounded strings without control characters" => {
            crate::ui_text::t("generator.error.95c4577ba047", &message)
        }
        "Schema references must be strings when supplied" => {
            crate::ui_text::t("generator.error.2de0694fcda3", &message)
        }
        "Select a declared owner relationship" => {
            crate::ui_text::t("generator.error.1f156d0d77a4", &message)
        }
        "Select an indexed Eloquent model" => {
            crate::ui_text::t("generator.error.a87709653f59", &message)
        }
        "Selected Nova resource belongs to a different related model" => {
            crate::ui_text::t("generator.error.165cd0afcf5d", &message)
        }
        "Selected source exceeds class proof limits" => {
            crate::ui_text::t("generator.error.7c69a9ff9c9c", &message)
        }
        "Separated Schema classes require Filament 4 or 5" => {
            crate::ui_text::t("generator.error.a8cfce0f9182", &message)
        }
        "Separated schema/table helpers do not use resource base classes" => {
            crate::ui_text::t("generator.error.ce2e391c7a0e", &message)
        }
        "Shared template recursion or nesting limit exceeded" => {
            crate::ui_text::t("generator.error.a1c95cb86dfe", &message)
        }
        "Shared template was not loaded" => {
            crate::ui_text::t("generator.error.ab306cffcee1", &message)
        }
        "Shared templates need at most 64 unique relative names" => {
            crate::ui_text::t("generator.error.5817d88e087f", &message)
        }
        "System UTC clock exceeds year 9999" => {
            crate::ui_text::t("generator.error.192f3d834437", &message)
        }
        "Template expansion exceeds 128 KiB" => {
            crate::ui_text::t("generator.error.7421f5c27210", &message)
        }
        "Template expansion source-work limit exceeded" => {
            crate::ui_text::t("generator.error.ec0201bdce50", &message)
        }
        "Template expansion work limit exceeded" => {
            crate::ui_text::t("generator.error.4467a1780228", &message)
        }
        "Template names must be relative paths and output paths must be bounded" => {
            crate::ui_text::t("generator.error.d0fd7a19fc3d", &message)
        }
        "Templates cannot contain NUL" => {
            crate::ui_text::t("generator.error.1eacab00f132", &message)
        }
        "Temporal precision must be 0–6 with no scale" => {
            crate::ui_text::t("generator.error.a454cc6c59c7", &message)
        }
        "Too many Composer namespace contexts" => {
            crate::ui_text::t("generator.error.66a8182d575f", &message)
        }
        "Too many captured package models" => {
            crate::ui_text::t("generator.error.d4e74914129f", &message)
        }
        "Too many fields, relations or indexes" => {
            crate::ui_text::t("generator.error.f95cd95d2b44", &message)
        }
        "Too many generator project sessions" => {
            crate::ui_text::t("generator.error.0fbef076c85d", &message)
        }
        "Too many generator projects; reload to retire inactive sessions" => {
            crate::ui_text::t("generator.error.8303beee0445", &message)
        }
        "Unclosed conditional section" => {
            crate::ui_text::t("generator.error.3fc1e4d411a5", &message)
        }
        "Unclosed generated template slot" => {
            crate::ui_text::t("generator.error.42cbd37ea4d6", &message)
        }
        "Unclosed quoted date pattern literal" => {
            crate::ui_text::t("generator.error.ce765e61fdc2", &message)
        }
        "Unclosed template placeholder" => {
            crate::ui_text::t("generator.error.08967053a0e5", &message)
        }
        "Unclosed template substitution" => {
            crate::ui_text::t("generator.error.86f5fec9d3aa", &message)
        }
        "Unexpected conditional end" => crate::ui_text::t("generator.error.20f68b068124", &message),
        "Unexpected else" => crate::ui_text::t("generator.error.b76b497f02fc", &message),
        "Unknown generator preset" => crate::ui_text::t("generator.error.4860eb549922", &message),
        "Unknown module" => crate::ui_text::t("generator.error.d9f2a85001c9", &message),
        "Unknown or calculated generator input supplied" => {
            crate::ui_text::t("generator.error.74302808c1d9", &message)
        }
        "Unknown package generation kind" => {
            crate::ui_text::t("generator.error.481aee5ca19c", &message)
        }
        "Unknown package generator" => crate::ui_text::t("generator.error.8f263bd117a7", &message),
        "Unknown preset scope" => crate::ui_text::t("generator.error.1048e3f07404", &message),
        "Unknown related file selection" => {
            crate::ui_text::t("generator.error.80f59c9c7367", &message)
        }
        "Unknown template directive" => crate::ui_text::t("generator.error.40c111b44040", &message),
        "Unknown, hidden or read-only calculated override supplied" => {
            crate::ui_text::t("generator.error.7b7267f4e6f7", &message)
        }
        "Unsupported Nova relationship" => {
            crate::ui_text::t("generator.error.5a4e46cfbf28", &message)
        }
        "Unsupported Nova/Filament major; refresh installed package metadata" => {
            crate::ui_text::t("generator.error.2a3b824bf824", &message)
        }
        "Unsupported generator config version; expected version 1" => {
            crate::ui_text::t("generator.error.f87f4563c095", &message)
        }
        "Unsupported related generator" => {
            crate::ui_text::t("generator.error.3c0b26a85e66", &message)
        }
        "Unsupported relation kind" => crate::ui_text::t("generator.error.a0d51f717a68", &message),
        "Use the proven relation-manager workflow for Filament relationships" => {
            crate::ui_text::t("generator.error.85159abf1991", &message)
        }
        "User templates must be bounded relative names without NUL (128 KiB/file, 2 MiB total)" => {
            crate::ui_text::t("generator.error.8fc81091db9d", &message)
        }
        "Values apply only to enum/set fields" => {
            crate::ui_text::t("generator.error.730a6064e217", &message)
        }
        "convertToClassName replacement must be one character" => {
            crate::ui_text::t("generator.error.1c44cf9af78c", &message)
        }
        "format requires a date pattern" => {
            crate::ui_text::t("generator.error.a3a0bbd3b11d", &message)
        }
        "removePrefix requires an argument" => {
            crate::ui_text::t("generator.error.a787836b480b", &message)
        }
        "removeSuffix requires an argument" => {
            crate::ui_text::t("generator.error.5350773fab90", &message)
        }
        "replace requires old,new" => crate::ui_text::t("generator.error.7201f01cfca2", &message),
        "Separate schema/table settings apply only to Filament resource generation" => {
            crate::ui_text::t("generator.error.07f80988da23", &message)
        }
        "Standalone Filament schema/table helpers do not extend a resource base class" => {
            crate::ui_text::t("generator.error.3caa6a3a17fc", &message)
        }
        "A package template table slot requires a captured or indexed table name" => {
            crate::ui_text::t("generator.error.27fdf52faff3", &message)
        }
        "A module already uses this id, namespace or source root" => {
            crate::ui_text::t("generator.error.da10bf080503", &message)
        }
        "A resource field and relation cannot share an output key" => {
            crate::ui_text::t("generator.error.fb31f483a6db", &message)
        }
        "An owned root composer.json is required" => {
            crate::ui_text::t("generator.error.52746097a3ff", &message)
        }
        "Capture an installed nwidart/laravel-modules 12.x in composer.lock first" => {
            crate::ui_text::t("generator.error.2caf6f882418", &message)
        }
        "Choose a smaller existing PHP file as the editor-action anchor" => {
            crate::ui_text::t("generator.error.0084b5e2763f", &message)
        }
        "Choose a supported scaffold, Laravel 12/13, Studly module name, explicit namespace, package name and relative root" => {
            crate::ui_text::t("generator.error.2cbc65da7eb4", &message)
        }
        "Composer lock packages are missing" => {
            crate::ui_text::t("generator.error.02a2a1f3c390", &message)
        }
        "Duplicate selected model fields" => {
            crate::ui_text::t("generator.error.6e604b6e89b3", &message)
        }
        "Finish a prepared preset edit first" => {
            crate::ui_text::t("generator.error.08d90780c982", &message)
        }
        "Invalid related-model output selections" => {
            crate::ui_text::t("generator.error.18699a7f5d1e", &message)
        }
        "Map at most 32 foreign keys per generation" => {
            crate::ui_text::t("generator.error.bc9826101b1f", &message)
        }
        "Native Composer packages must use an explicit packages/<directory> root" => {
            crate::ui_text::t("generator.error.b5ded855f6f8", &message)
        }
        "Native nWidart v12 scaffolds support the default Modules/Name, Modules\\Name, Laravel 12 layout only" => {
            crate::ui_text::t("generator.error.e240c975f95a", &message)
        }
        "Only an explicitly required, locked nWidart 12.x package is supported natively; use captured Artisan forms for other versions" => {
            crate::ui_text::t("generator.error.a4abe2689f69", &message)
        }
        "Open a project before editing project generators" => {
            crate::ui_text::t("generator.error.8d27d28bda35", &message)
        }
        "Preset files are limited to 128 KiB" => {
            crate::ui_text::t("generator.error.adf78f1f0bf4", &message)
        }
        "Preset output paths conflict" => {
            crate::ui_text::t("generator.error.d4c6179044ba", &message)
        }
        "Published modules.php may customize discovery/stubs; use captured module:make rather than guessing its executable configuration" => {
            crate::ui_text::t("generator.error.32f12ef68f94", &message)
        }
        "Related generation evidence exceeds 4 MiB" => {
            crate::ui_text::t("generator.error.2a6822eb743d", &message)
        }
        "Root Composer PSR-4 metadata is required; Composer is never rewritten by this action" => {
            crate::ui_text::t("generator.error.ec68e16c63ce", &message)
        }
        "Save dirty generator config/template editors before preparing changes" => {
            crate::ui_text::t("generator.error.aa6ccfa945cb", &message)
        }
        "Save scaffold destination/config editors first" => {
            crate::ui_text::t("generator.error.f1d55f4bb580", &message)
        }
        "Scaffold paths must be safe project-relative paths" => {
            crate::ui_text::t("generator.error.3e114cb40061", &message)
        }
        "Select from at most 128 model fields" => {
            crate::ui_text::t("generator.error.44d5371e7b1c", &message)
        }
        "Select from at most 64 model relations" => {
            crate::ui_text::t("generator.error.3931c0ba29b9", &message)
        }
        "Templates need relative names; a referenced or replacement template cannot also be deleted" => {
            crate::ui_text::t("generator.error.bf4739fc353f", &message)
        }
        "The generator definitions are unchanged" => {
            crate::ui_text::t("generator.error.cee2a15ae7ca", &message)
        }
        "The scaffold root already contains files" => {
            crate::ui_text::t("generator.error.54d9d48d7437", &message)
        }
        "The scaffold root must not already exist" => {
            crate::ui_text::t("generator.error.fb2f4781679c", &message)
        }
        "Too many or duplicate template changes" => {
            crate::ui_text::t("generator.error.d27e5bda75a0", &message)
        }
        "Too many prepared module projects" => {
            crate::ui_text::t("generator.error.32d49c15b638", &message)
        }
        "Unknown module scaffold operation" => {
            crate::ui_text::t("generator.error.a01c15c02e9c", &message)
        }
        "Factory suggestion signature is too large" => {
            crate::ui_text::t("generator.factory.signatureLimit", &message)
        }
        "Livewire namespace generation requires an unambiguous literal registered namespace and matching Laravel view mapping" => {
            crate::ui_text::t("project.native.livewire.namespaceProof", &message)
        }
        _ => crate::ui_text::owned_error(&message, UI_ERROR_TEMPLATES).unwrap_or(message),
    };
    Error::Invalid(message)
}

const UI_ERROR_TEMPLATES: &[(&str, &str)] = &[
    (
        "generator.dynamic.61913eb84014",
        "Conflicting Composer package mappings for {class}; resolve their namespaces before generating",
    ),
    (
        "generator.dynamic.111b639b6257",
        "Invalid effective {kind} directory/namespace for selected application/module",
    ),
    (
        "generator.dynamic.fcfd09ecdf35",
        "Composer maps {class} to a different source root; expected generated path {path}",
    ),
    (
        "generator.dynamic.04b0009f9a3b",
        "{label} must select unique names from this draft's fields/relations",
    ),
    (
        "generator.dynamic.51f45558e844",
        "Generator {kind} is unavailable in the selected application/module",
    ),
    (
        "generator.dynamic.259d7b2da749",
        "Enable and install a supported {package} version before generating",
    ),
    (
        "generator.dynamic.af0dc8a156d4",
        "{relative} must be a regular file, not a directory or symlink",
    ),
    (
        "generator.dynamic.419d0901c702",
        "Another Composer source root already contains {class}: {other}",
    ),
    (
        "generator.dynamic.e9a436f1128e",
        "{value1} requires a fully qualified intermediate class",
    ),
    (
        "generator.dynamic.2ac22b9aebbe",
        "Composer metadata must be an ordinary file: {path}",
    ),
    (
        "generator.dynamic.2cb642a480e3",
        "Unsupported SimpleDateFormat pattern field {ch}",
    ),
    (
        "generator.dynamic.7a4a4b157815",
        "Unknown or automatic-key factory mapping {name}",
    ),
    (
        "generator.dynamic.76d75d191f9c",
        "No owned Composer PSR-4 declaration maps {class}",
    ),
    (
        "generator.dynamic.5bd2ed786f75",
        "Invalid or conflicting generated path {path}",
    ),
    (
        "generator.dynamic.6f70cd510ef3",
        "{value1} requires nullable for a null default",
    ),
    (
        "generator.dynamic.cdcbc952b5a0",
        "Composer metadata must be an object: {path}",
    ),
    (
        "generator.dynamic.7eda97e00b8a",
        "Choose an owned indexed subclass for {class}",
    ),
    (
        "generator.dynamic.9ea553f006f0",
        "Transform {kind} does not accept arguments",
    ),
    (
        "generator.dynamic.9572fa171c84",
        "Target parent is not a directory: {parent}",
    ),
    (
        "generator.dynamic.6249ead22c51",
        "Choose a currently indexed model: {model}",
    ),
    (
        "generator.dynamic.8a643aa7b324",
        "Choose an owned indexed {target} subclass",
    ),
    (
        "generator.dynamic.5deb79aaac8b",
        "Unknown generated template slot {key}",
    ),
    (
        "generator.dynamic.81c3ac0e5883",
        "{value1} requires an explicit morph name",
    ),
    (
        "generator.dynamic.40750334a2c2",
        "Cannot remove missing template {name}",
    ),
    (
        "generator.dynamic.78c97ef526d2",
        "Choose an owned indexed class: {name}",
    ),
    (
        "generator.dynamic.153d2e47a26f",
        "Invalid or duplicate relation {value1}",
    ),
    (
        "generator.dynamic.42b1b99f0433",
        "Unknown native generator kind {kind}",
    ),
    (
        "generator.dynamic.8c57f28ee088",
        "{prefix} must be an ordinary directory",
    ),
    (
        "generator.dynamic.b3929df37a19",
        "Invalid generator base class {base}",
    ),
    (
        "generator.dynamic.77f76be9d4b7",
        "Invalid calculated override {value1}",
    ),
    (
        "generator.dynamic.7b90b53d97ca",
        "Choose an indexed model for {value1}",
    ),
    (
        "generator.dynamic.45eb9e374349",
        "Invalid or duplicate field {value1}",
    ),
    (
        "generator.dynamic.163863b3c80c",
        "Invalid input declaration {value1}",
    ),
    (
        "generator.dynamic.0b13f7a118ec",
        "Invalid PSR-4 mapping for {prefix}",
    ),
    (
        "generator.dynamic.602d78fe77da",
        "Unsupported field type {value1}",
    ),
    (
        "generator.dynamic.12b41746b6ea",
        "Target already exists: {value1}",
    ),
    (
        "generator.dynamic.3ab0180658be",
        "{relative} must be a regular file",
    ),
    (
        "generator.dynamic.1c1c9b2c9d7d",
        "Unsupported transform {kind}",
    ),
    (
        "generator.dynamic.d3351a11af22",
        "Invalid PSR-4 root in {value1}",
    ),
    (
        "generator.dynamic.618b689db578",
        "Refusing to overwrite {path}",
    ),
    ("generator.dynamic.d320820ba856", "Missing template {name}"),
    (
        "generator.dynamic.61198fecbfa8",
        "{relative} exceeds 128 KiB",
    ),
    ("generator.dynamic.615f38af41c2", "Missing input {value1}"),
    ("generator.dynamic.becf27f703ab", "Invalid input {value1}"),
    ("generator.dynamic.9817bee7bfaa", "Unknown input {name}"),
    ("generator.dynamic.d16f2d57585c", "Invalid cast {cast}"),
];

fn index_pending_error() -> Error {
    Error::Invalid(crate::ui_text::t(
        "index.building",
        "Indexing Laravel project…",
    ))
}

impl State {
    /// Retire source-backed choices while preserving the user's form and output context.
    pub(crate) fn set_models_pending(&mut self, ctx: &Invocation) -> Result<(), Error> {
        let (_, root) = owner(ctx)?;
        if !self.projects.contains_key(&root) && self.projects.len() >= 8 {
            return Err(Error::Invalid("Too many generator project sessions".into()));
        }
        let session = self.projects.entry(root).or_default();
        session.index_pending = true;
        session.captured_index = None;
        session.models = Default::default();
        session.models_complete = false;
        session.allowed_fields.clear();
        session.model_relations.clear();
        session.model_schemas.clear();
        session.model_casts.clear();
        session.model_types.clear();
        session.pending = None;
        self.presets.discard(ctx)?;
        self.existing.discard(ctx)?;
        self.scaffolds
            .handle(ctx, r#"{"op":"module-scaffold-discard"}"#)?;
        Ok(())
    }
    /// Index-only inventory snapshot; source contents are read only for explicit
    /// preview/preparation/application, never during view rendering.
    pub(crate) fn set_models(
        &mut self,
        ctx: &Invocation,
        index: &crate::index::Index,
    ) -> Result<(), Error> {
        let (_, root) = owner(ctx)?;
        if !self.projects.contains_key(&root) && self.projects.len() >= 8 {
            return Err(Error::Invalid("Too many generator project sessions".into()));
        }
        let session = self.projects.entry(root.clone()).or_default();
        let complete = !index.partial
            && !index.source_evidence_incomplete
            && index.source_root == root
            && !index.source_evidence.is_empty();
        if complete
            && session.captured_index.as_ref().is_some_and(|previous| {
                previous.source_evidence == index.source_evidence
                    && previous.source_queries == index.source_queries
                    && previous.source_settings == index.source_settings
                    && previous.source_root == index.source_root
            })
        {
            return Ok(());
        }
        session.captured_index = complete.then(|| Box::new(index.clone()));
        session.models = if !complete {
            Default::default()
        } else {
            index.eloquent_models.clone()
        };
        session.models_complete = complete;
        session.index_pending = !complete;
        session.allowed_fields.clear();
        session.model_relations.clear();
        session.model_casts.clear();
        session.model_types.clear();
        if complete {
            for (full, _, table) in crate::eloquent_members::model_inventory(&index.eloquent_models)
            {
                session.model_types.insert(
                    full.clone(),
                    index
                        .columns
                        .get(&table)
                        .into_iter()
                        .flatten()
                        .filter(|(_, entry)| owned(&root, &entry.path))
                        .filter_map(|(name, entry)| {
                            entry
                                .detail
                                .as_ref()
                                .map(|kind| (name.clone(), kind.clone()))
                        })
                        .collect(),
                );
                session.allowed_fields.insert(
                    full.clone(),
                    index
                        .columns
                        .get(&table)
                        .into_iter()
                        .flatten()
                        .filter(|(_, entry)| owned(&root, &entry.path))
                        .map(|(name, _)| name.clone())
                        .collect(),
                );
                if let Some(facts) =
                    crate::eloquent_members::helper_facts::facts(index, &full, &|path| {
                        owned(&root, path)
                    })
                {
                    session.model_casts.insert(
                        full.clone(),
                        facts
                            .casts
                            .iter()
                            .map(|cast| (cast.name.clone(), cast.label.clone()))
                            .collect(),
                    );
                    session.model_relations.insert(
                        full,
                        facts
                            .relations
                            .into_iter()
                            .map(|relation| generator_plan::Relation {
                                name: relation.name,
                                kind: relation.kind,
                                related: relation.target,
                                through: None,
                                arguments: Vec::new(),
                            })
                            .collect(),
                    );
                }
            }
        }

        Ok(())
    }
    pub(crate) fn set_model_schema(
        &mut self,
        ctx: &Invocation,
        model: &str,
        snapshot: Option<Value>,
    ) -> Result<(), Error> {
        let (project, _) = owner(ctx)?;
        self.set_model_schema_for_project(&project, model, snapshot)
    }
    pub(crate) fn set_model_schema_for_project(
        &mut self,
        project: &Project,
        model: &str,
        snapshot: Option<Value>,
    ) -> Result<(), Error> {
        let root = project.root()?;
        if !self.projects.contains_key(&root) && self.projects.len() >= 8 {
            return Err(Error::Invalid("Too many generator project sessions".into()));
        }
        let session = self.projects.entry(root.clone()).or_default();
        if let Some(snapshot) = snapshot {
            if snapshot["model"].as_str() != Some(model)
                || snapshot["file"]
                    .as_str()
                    .is_none_or(|path| !owned(&root, path))
                || snapshot.to_string().len() > 2 << 20
            {
                return Err(Error::Stale);
            }
            if session.model_schemas.len() >= 128 && !session.model_schemas.contains_key(model) {
                return Err(Error::Invalid("Too many captured package models".into()));
            }
            let retained = session
                .model_schemas
                .iter()
                .filter(|(key, _)| key.as_str() != model)
                .map(|(_, schema)| schema.to_string().len())
                .sum::<usize>();
            if retained + snapshot.to_string().len() > 8 << 20 {
                return Err(Error::Invalid(
                    "Captured package schemas exceed the 8 MiB project budget".into(),
                ));
            }
            session.model_schemas.insert(model.into(), snapshot);
        } else {
            session.model_schemas.remove(model);
        }
        Ok(())
    }
    pub(crate) fn seed_existing(
        &mut self,
        ctx: &Invocation,
        index: &crate::index::Index,
        path: &str,
    ) -> Result<(), Error> {
        self.existing.seed(ctx, index, path)?;
        ui_kit::web::reveal_view(VIEW, include_str!("ui_config.json"))
    }

    pub(crate) fn seed_model(&mut self, ctx: &Invocation, draft: Draft) -> Result<(), Error> {
        let (_, root) = owner(ctx)?;
        generator_plan::plan(&draft).map_err(Error::Invalid)?;
        if !self.projects.contains_key(&root) && self.projects.len() >= 8 {
            return Err(Error::Invalid(
                "Too many generator projects; reload to retire inactive sessions".into(),
            ));
        }
        let session = self.projects.entry(root).or_default();
        session.pending = None;
        session.input = None;
        session.draft = Some(draft);
        ui_kit::web::reveal_view(VIEW, include_str!("ui_config.json"))
    }
    pub(crate) fn handle(&mut self, ctx: &Invocation, message: &str) -> Result<Value, Error> {
        self.handle_inner(ctx, message).map_err(ui_error)
    }
    fn handle_inner(&mut self, ctx: &Invocation, message: &str) -> Result<Value, Error> {
        if message.len() > 128 * 1024 {
            return Err(Error::Invalid("Generator request is too large".into()));
        }
        let envelope = crate::strict_json::parse(message, 128 << 10)?;
        let (_, root) = owner(ctx)?;
        if self
            .projects
            .get(&root)
            .is_some_and(|session| session.index_pending)
        {
            let op = envelope["op"].as_str().unwrap_or("");
            if op.starts_with("preview")
                || op.starts_with("prepare")
                || matches!(
                    op,
                    "module-scaffold-preview"
                        | "module-scaffold-prepare"
                        | "inspect-related"
                        | "inspect-package-model"
                        | "inspect-factory-defaults"
                )
            {
                if matches!(op, "preview" | "prepare") {
                    let retained: Request = serde_json::from_value(envelope.clone())
                        .map_err(|error| Error::Invalid(error.to_string()))?;
                    if let Request::Preview {
                        draft,
                        module,
                        packages,
                        package_options,
                    }
                    | Request::Prepare {
                        draft,
                        module,
                        packages,
                        package_options,
                    } = retained
                    {
                        let session = self.projects.get_mut(&root).unwrap();
                        session.draft = Some(draft.clone());
                        session.input = Some(Input::Model {
                            draft,
                            module,
                            packages,
                            package_options,
                        });
                    }
                }
                return Err(index_pending_error());
            }
        }
        if envelope
            .get("op")
            .and_then(Value::as_str)
            .is_some_and(|op| op.starts_with("module-scaffold-"))
        {
            return self.scaffolds.handle(ctx, message);
        }
        if envelope["op"]
            .as_str()
            .is_some_and(|op| matches!(op, "preview-package" | "prepare-package"))
            && envelope["request"]["fields"]
                .as_array()
                .is_some_and(|fields| {
                    fields
                        .iter()
                        .any(|field| !field.get("nullable").is_some_and(Value::is_boolean))
                })
        {
            return Err(Error::Invalid(
                "Review explicit nullable true/false for every package field mapping".into(),
            ));
        }
        let request: Request =
            serde_json::from_str(message).map_err(|e| Error::Invalid(e.to_string()))?;
        let (project, root) = owner(ctx)?;
        let request = match request {
            Request::NewModel => {
                self.existing.close(ctx)?;
                return Ok(json!({"existing":null}));
            }
            Request::PreviewFields { draft } => return self.existing.handle(ctx, draft, false),
            Request::PrepareFields { draft } => return self.existing.handle(ctx, draft, true),
            Request::InspectConfig { scope } => {
                return match scope.as_str() {
                    "project" => crate::generator_preset_actions::inspect(ctx),
                    "user" => crate::generator_presets::user_document(),
                    _ => Err(Error::Invalid("Unknown preset scope".into())),
                };
            }
            Request::PreviewConfig {
                scope,
                config,
                templates,
                remove_templates,
            } => {
                return match scope.as_str() {
                    "project" => self
                        .presets
                        .preview(ctx, config, templates, remove_templates),
                    "user" => {
                        crate::generator_presets::from_parts(config.clone(), templates.clone())?;
                        let before = crate::generator_presets::user_document()?;
                        Ok(
                            json!({"files":[{"path":"User-level generator settings","before":serde_json::to_string_pretty(&before).unwrap_or_default(),"after":serde_json::to_string_pretty(&json!({"config":config,"templates":templates})).unwrap_or_default(),"operation":"edit"}]}),
                        )
                    }
                    _ => Err(Error::Invalid("Unknown preset scope".into())),
                };
            }
            Request::PrepareConfig {
                scope,
                config,
                templates,
                remove_templates,
            } => {
                return match scope.as_str() {
                    "project" => self
                        .presets
                        .prepare(ctx, config, templates, remove_templates),
                    "user" => crate::generator_presets::save_user(config, templates),
                    _ => Err(Error::Invalid("Unknown preset scope".into())),
                };
            }
            request => request,
        };
        if !self.projects.contains_key(&root) && self.projects.len() >= 8 {
            return Err(Error::Invalid(
                "Too many generator projects; reload to retire inactive sessions".into(),
            ));
        }
        let session = self.projects.entry(root.clone()).or_default();
        if matches!(
            request,
            Request::EvaluatePreset { .. }
                | Request::PreviewPreset { .. }
                | Request::PreparePreset { .. }
        ) && session.template_time.is_none()
        {
            session.template_time =
                Some(crate::generator_presets::capture_current_time().map_err(Error::Invalid)?);
        }
        if let Request::EvaluatePreset {
            preset,
            values,
            module,
            overrides,
        } = &request
        {
            let bundle = crate::generator_presets::load(&project)?
                .with_current_time(session.template_time.as_deref().ok_or(Error::Stale)?)
                .map_err(Error::Invalid)?;
            if session.index_pending
                && !bundle
                    .model_inputs(preset, values)
                    .map_err(Error::Invalid)?
                    .is_empty()
            {
                return Err(index_pending_error());
            }
            indexed_model_inputs(&bundle, &session.models, preset, values, &root)?;
            let mut result = bundle
                .evaluate(preset, values, module.as_deref(), overrides)
                .map_err(Error::Invalid)?;
            result["preset"] = preset.clone().into();
            return Ok(result);
        }
        if matches!(request, Request::Artisan) {
            ui_kit::web::reveal_view("laravel-artisan", include_str!("ui_config.json"))?;
            return Ok(
                json!({"instruction":crate::ui_text::t("generator.native.49b507f95e","Choose a make:* command in Artisan; load command metadata explicitly if needed.")}),
            );
        }
        if let Request::InspectFactoryDefaults { draft, signature } = &request {
            if signature.len() > 65536 {
                return Err(Error::Invalid(
                    "Factory suggestion signature is too large".into(),
                ));
            }
            if draft.fields.is_empty()
                || !draft
                    .relations
                    .iter()
                    .any(|relation| relation.kind == "belongsTo")
            {
                return Ok(json!({"signature":signature,"factoryDefaults":{}}));
            }
            let current = session.captured_index.as_deref().ok_or(Error::Stale)?;
            if !crate::index::index_sources_current(&project, current) {
                return Err(Error::Stale);
            }
            return Ok(
                json!({"signature":signature,"factoryDefaults":factory_defaults::for_draft(&project,&current,draft),"references":reference_catalog(&current.eloquent_models,&root)}),
            );
        }
        if let Request::InspectRelated { model } = &request {
            let index = session.captured_index.as_deref().ok_or(Error::Stale)?;
            if !crate::index::index_sources_current(&project, index) {
                return Err(Error::Stale);
            }
            return crate::bulk::related_catalog(&project, &index, model);
        }
        if let Request::InspectPackageModel { model } = &request {
            let identity = model_catalog(&session.models, &root)
                .into_iter()
                .find(|item| item["class"].as_str() == Some(model.as_str()))
                .ok_or(Error::Stale)?;
            let schema = session.model_schemas.get(model);
            let name = model.rsplit('\\').next().ok_or(Error::Stale)?;
            let mut mapped=schema.and_then(|schema|{let table=schema["table"].as_str()?;let mut descriptor=schema.clone();descriptor["name"]=table.into();let snapshot=json!({"capturedAt":schema["capturedAt"],"connection":schema["connection"],"tables":[descriptor]});crate::model_from_schema::draft(&snapshot,table,name).ok()});
            if let Some(draft) = &mut mapped {
                for field in &mut draft.fields {
                    field.cast = session
                        .model_casts
                        .get(model)
                        .and_then(|casts| casts.get(&field.name))
                        .cloned();
                }
            }
            let mut available = session
                .allowed_fields
                .get(model)
                .cloned()
                .unwrap_or_default();
            if let Some(schema) = schema {
                available.extend(
                    schema["columns"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|column| column["name"].as_str().map(str::to_owned)),
                );
            }
            return Ok(
                json!({"model":model,"table":identity["table"],"fields":mapped.as_ref().map(|draft|&draft.fields),"primaryKey":mapped.as_ref().map(|draft|&draft.primary_key),"relations":session.model_relations.get(model),"availableFields":available,"fieldTypes":session.model_types.get(model),"casts":session.model_casts.get(model),"schemaCaptured":mapped.is_some(),"instruction":if mapped.is_some(){crate::ui_text::t("generator.native.b19a7fc059","Review captured schema fields before generating.")}else{crate::ui_text::t("generator.native.bed6b7a297","Capture this model's connection/schema for exact types and nullability, or supply explicit mappings for indexed field names. Unknown nullability is not defaulted.")}}),
            );
        }
        if matches!(request, Request::State) {
            let bundle = crate::generator_presets::load(&project)?;
            let mut catalog = bundle.catalog();
            let native_settings = bundle.native_settings(None).map_err(Error::Invalid)?;
            let mut native_catalog =
                crate::native_catalog::catalog_for_project(&project, &native_settings)?;
            let metadata = crate::metadata::Metadata::load(&project);
            if let Some(kinds) = native_catalog.as_array_mut() {
                kinds.retain(|kind| {
                    let package = match kind["id"].as_str().unwrap_or("") {
                        "livewire-component" | "livewire-form" => "livewire/livewire",
                        "dusk-test" | "dusk-page" | "dusk-component" => "laravel/dusk",
                        "tenant-migration" => "stancl/tenancy",
                        _ => return true,
                    };
                    metadata.package_enabled(package, true)
                });
            }
            catalog["nativeCatalog"] = native_catalog;
            catalog["frameworkVersion"] = package_versions(&project)
                .get("laravel/framework")
                .cloned()
                .map(Value::String)
                .unwrap_or(Value::Null);
            catalog["packages"] = crate::package_generators::catalog(&package_versions(&project));
            let classes = crate::eloquent_members::class_inventory(&session.models)
                .into_iter()
                .filter(|identity| {
                    !identity.ambiguous && !identity.is_trait && owned(&root, &identity.path)
                })
                .take(512)
                .map(|identity| json!({"class":identity.full,"path":identity.path}))
                .collect::<Vec<_>>();
            return Ok(
                json!({"draft":session.draft,"existing":self.existing.state(ctx)?,"catalog":catalog,"classes":classes,"models":model_catalog(&session.models,&root),"modelsComplete":session.models_complete,"indexPending":session.index_pending,"factoryDefaultsAvailable":true,"references":reference_catalog(&session.models,&root),"project":root,"prepared":session.pending.is_some(),"instruction":if session.index_pending {crate::ui_text::t("index.building","Indexing Laravel project…")} else {crate::ui_text::t("generator.native.prepareWrites","Preview the files, then choose Finish to write them after one confirmation. No PHP runs.")}}),
            );
        }
        if matches!(request, Request::Discard) {
            session.pending = None;
            session.template_time = None;
            self.presets.discard(ctx)?;
            self.existing.discard(ctx)?;
            self.scaffolds
                .handle(ctx, r#"{"op":"module-scaffold-discard"}"#)?;
            return Ok(json!({"prepared":false}));
        }
        let (input, prepare) = match request {
            Request::PreviewNative {
                request,
                module,
                choices,
            } => (
                Input::Native {
                    request,
                    module,
                    choices,
                },
                false,
            ),
            Request::PrepareNative {
                request,
                module,
                choices,
            } => (
                Input::Native {
                    request,
                    module,
                    choices,
                },
                true,
            ),
            Request::PreviewPackage { request, module } => {
                (Input::Package { request, module }, false)
            }
            Request::PreparePackage { request, module } => {
                (Input::Package { request, module }, true)
            }
            Request::Preview {
                draft,
                module,
                packages,
                package_options,
            } => (
                Input::Model {
                    draft,
                    module,
                    packages,
                    package_options,
                },
                false,
            ),
            Request::Prepare {
                draft,
                module,
                packages,
                package_options,
            } => (
                Input::Model {
                    draft,
                    module,
                    packages,
                    package_options,
                },
                true,
            ),
            Request::PreviewPreset {
                preset,
                values,
                module,
                overrides,
            } => (
                Input::Preset {
                    current_time: session.template_time.clone().ok_or(Error::Stale)?,
                    preset,
                    values,
                    module,
                    overrides,
                },
                false,
            ),
            Request::PreparePreset {
                preset,
                values,
                module,
                overrides,
            } => (
                Input::Preset {
                    current_time: session.template_time.clone().ok_or(Error::Stale)?,
                    preset,
                    values,
                    module,
                    overrides,
                },
                true,
            ),
            _ => unreachable!(),
        };
        session.pending = None;
        let (plan, evidence) = build(
            &project,
            &input,
            &session.models,
            &session.allowed_fields,
            &session.model_relations,
            &session.model_schemas,
            &session.model_casts,
            session.captured_index.as_deref(),
        )?;
        validate(&project, &root, &plan)?;
        if let Input::Model { draft, .. } = &input {
            session.draft = Some(draft.clone());
        }
        session.input = Some(input);
        if !prepare {
            return Ok(snapshot(&plan));
        }
        let editors = host::open_editors()?;
        if plan.files.iter().any(|f| {
            editors
                .iter()
                .any(|e| e.path == format!("{root}/{}", f.path))
        }) {
            return Err(Error::Invalid(
                "A generated target is open in the editor".into(),
            ));
        }
        let anchor = editors
            .iter()
            .find(|e| e.active && !e.dirty && e.language == "php" && owned(&root, &e.path))
            .or_else(|| {
                editors
                    .iter()
                    .find(|e| !e.dirty && e.language == "php" && owned(&root, &e.path))
            })
            .map(|e| e.path.clone())
            .unwrap_or_else(|| format!("{root}/routes/web.php"));
        if !owned(&root, &anchor) || project.read_file(&anchor).is_err() {
            return Err(Error::Invalid("Open an existing saved PHP file in this project to anchor the undoable generation action".into()));
        }
        self.next = self.next.checked_add(1).ok_or(Error::Stale)?;
        let key = format!("generator-apply:{}", self.next);
        host::open_editor(&anchor, None)?;
        session.pending = Some(Pending {
            key,
            plan,
            evidence,
            anchor,
        });
        Ok(
            json!({"prepared":true,"instruction":crate::ui_text::t("generator.native.88aba506f1","In the opened PHP editor, choose Apply prepared Laravel generation… from editor actions. One undo reverses all created files.")}),
        )
    }

    pub(crate) fn prepared_model(&self, doc: &Document, action: &FileAction) -> Option<String> {
        self.projects.iter().find_map(|(root, session)| {
            let pending = session.pending.as_ref()?;
            if action.data.as_deref() != Some(&pending.key)
                || pending.anchor != doc.path
                || !owned(root, &doc.path)
            {
                return None;
            }
            match session.input.as_ref()? {
                Input::Package { request, .. } => Some(request.model.clone()),
                _ => None,
            }
        })
    }
    /// Every generation prepared for this project, as (action key, anchor file).
    pub(crate) fn prepared(&self, root: &str) -> Vec<(String, String)> {
        let main = self.projects.get(root).and_then(|session| session.pending.as_ref()).map(|pending| (pending.key.clone(), pending.anchor.clone()));
        [main, self.presets.prepared(root), self.existing.prepared(root), self.scaffolds.prepared(root)].into_iter().flatten().collect()
    }
    pub(crate) fn actions(&self, doc: &Document) -> Vec<FileAction> {
        let mut actions = self.presets.actions(doc);
        actions.extend(self.existing.actions(doc));
        actions.extend(self.scaffolds.actions(doc));
        actions.extend(
            self.projects
                .iter()
                .find_map(|(root, s)| {
                    s.pending
                        .as_ref()
                        .filter(|p| p.anchor == doc.path && owned(root, &doc.path))
                        .map(|p| FileAction {
                            title: crate::ui_text::t(
                                "generator.native.applyAction",
                                "Apply prepared Laravel generation…",
                            ),
                            kind: Some("refactor".into()),
                            data: Some(p.key.clone()),
                            ..Default::default()
                        })
                })
                .into_iter()
                .collect::<Vec<_>>(),
        );
        actions
    }
    pub(crate) fn resolve(
        &mut self,
        doc: &Document,
        action: FileAction,
    ) -> Result<FileAction, Error> {
        self.resolve_inner(doc, action).map_err(ui_error)
    }
    fn resolve_inner(&mut self, doc: &Document, action: FileAction) -> Result<FileAction, Error> {
        if host::project_of(&doc.path)
            .and_then(|project| project.root().ok())
            .and_then(|root| self.projects.get(&root))
            .is_some_and(|session| session.index_pending)
        {
            return Err(index_pending_error());
        }
        if action
            .data
            .as_deref()
            .is_some_and(|key| key.starts_with("module-scaffold:"))
        {
            return self.scaffolds.resolve(doc, action);
        }
        if action
            .data
            .as_deref()
            .is_some_and(|key| key.starts_with("existing-model-fields:"))
        {
            return self.existing.resolve(doc, action);
        }
        if action
            .data
            .as_deref()
            .is_some_and(|key| key.starts_with("generator-presets:"))
        {
            return self.presets.resolve(doc, action);
        }
        let root = host::project_of(&doc.path)
            .and_then(|p| p.root().ok())
            .ok_or(Error::Stale)?;
        let project = host::project_of(&doc.path).ok_or(Error::Stale)?;
        let session = self.projects.get_mut(&root).ok_or(Error::Stale)?;
        let pending = session.pending.as_ref().ok_or(Error::Stale)?;
        if action.data.as_deref() != Some(&pending.key)
            || pending.anchor != doc.path
            || project.read_file(&doc.path)? != doc.text
        {
            return Err(Error::Stale);
        }
        let input = session.input.as_ref().ok_or(Error::Stale)?;
        let (current, evidence) = build(
            &project,
            input,
            &session.models,
            &session.allowed_fields,
            &session.model_relations,
            &session.model_schemas,
            &session.model_casts,
            session.captured_index.as_deref(),
        )?;
        if evidence != pending.evidence || current != pending.plan {
            return Err(Error::Stale);
        }
        validate(&project, &root, &pending.plan)?;
        let preview = pending
            .plan
            .files
            .iter()
            .map(|f| {
                crate::ui_text::format(
                    "generator.review.create",
                    "Create {path}\n{source}",
                    &[("path", &f.path), ("source", &f.text)],
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        if !ui_kit::prompts::confirm_exact(
            &crate::ui_text::t(
                "generator.native.e02588db0d",
                "Apply prepared Laravel generation?",
            ),
            &preview,
        )? {
            return Err(Error::Cancelled);
        }
        let (current, evidence) = build(
            &project,
            input,
            &session.models,
            &session.allowed_fields,
            &session.model_relations,
            &session.model_schemas,
            &session.model_casts,
            session.captured_index.as_deref(),
        )?;
        if evidence != pending.evidence
            || current != pending.plan
            || project.read_file(&doc.path)? != doc.text
        {
            return Err(Error::Stale);
        }
        validate(&project, &root, &pending.plan)?;
        let pending = session.pending.take().ok_or(Error::Stale)?;
        let open = pending
            .plan
            .files
            .iter()
            .find(|f| f.path.contains("/Models/"))
            .map(|f| format!("{root}/{}", f.path));
        Ok(FileAction {
            title: crate::ui_text::t(
                "generator.action.generated",
                "Generate Laravel model and related files",
            ),
            operations: pending
                .plan
                .files
                .into_iter()
                .map(|f| FileOperation::Create {
                    path: format!("{root}/{}", f.path),
                    text: f.text,
                    overwrite: false,
                })
                .collect(),
            open,
            ..Default::default()
        })
    }
}
