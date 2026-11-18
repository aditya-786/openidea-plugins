//! Project-scoped helper preferences in plugin storage, never application files.
use openidea_plugin::{Error, Invocation, PickItem, Project, host};
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Parameters {
    pub version: u32,
    pub storage: String,
    pub relation_counts: bool,
    pub date_class: String,
    #[serde(default = "migration_source")]
    pub source: String,
    #[serde(default)]
    pub collection_proxies: bool,
    #[serde(default)]
    pub on_save: bool,
    #[serde(default)]
    pub post_commands: Vec<Vec<String>>,
}
impl Default for Parameters {
    fn default() -> Self {
        Self {
            version: 2,
            storage: "helpers".into(),
            relation_counts: false,
            date_class: "Illuminate\\Support\\Carbon".into(),
            source: "migrations".into(),
            collection_proxies: false,
            on_save: false,
            post_commands: vec![],
        }
    }
}
fn migration_source() -> String {
    "migrations".into()
}
fn key(project: &Project) -> Result<String, Error> {
    Ok(format!("laravel.helper-parameters:{}", project.root()?))
}
fn validate(value: &Parameters) -> Result<(), Error> {
    if value.version != 2
        || !matches!(value.storage.as_str(), "helpers" | "preserve" | "overwrite")
        || !crate::generator_plan::qualified(&value.date_class)
        || value.date_class.starts_with('\\')
        || value.date_class.len() > 256
    {
        return Err(Error::Invalid(
            crate::ui_text::text(
                "native.model_helper_parameters.614eaf8afa",
                "Invalid persisted helper parameters",
            )
            .into(),
        ));
    }
    if !matches!(value.source.as_str(), "migrations" | "database" | "both")
        || value.on_save && value.storage != "helpers"
        || value
            .post_commands
            .iter()
            .flatten()
            .map(String::len)
            .sum::<usize>()
            > 32 * 1024
        || value.post_commands.len() > 8
        || value.post_commands.iter().any(|args| {
            args.is_empty()
                || args.len() > 64
                || args[0].starts_with('-')
                || args
                    .iter()
                    .any(|arg| arg.len() > 4096 || arg.chars().any(char::is_control))
        })
    {
        return Err(Error::Invalid(
            crate::ui_text::text(
                "native.model_helper_parameters.0f6b4cfa99",
                "Invalid helper source or automation parameters",
            )
            .into(),
        ));
    }
    Ok(())
}
pub(crate) fn load(project: &Project) -> Result<Parameters, Error> {
    let raw = match host::storage::get(&key(project)?) {
        Ok(value) => value,
        Err(Error::Denied(_)) => return Ok(Parameters::default()),
        Err(error) => return Err(error),
    };
    let Some(raw) = raw else {
        return Ok(Parameters::default());
    };
    if raw.len() > 64 * 1024 {
        return Err(Error::Invalid(
            crate::ui_text::text(
                "native.model_helper_parameters.9d66db23b4",
                "Persisted helper parameters exceed the supported size",
            )
            .into(),
        ));
    }
    let mut value: Parameters = serde_json::from_str(&raw).map_err(|_| {
        Error::Invalid(
            crate::ui_text::text(
                "native.model_helper_parameters.b4468a1735",
                "Persisted helper parameters are malformed",
            )
            .into(),
        )
    })?;
    if value.version == 1 {
        value.version = 2;
    }
    validate(&value)?;
    Ok(value)
}
fn owner(ctx: &Invocation) -> Result<Project, Error> {
    let project = ctx.project().ok_or(crate::ui_text::text(
        "native.model_actions.a74a0baad4",
        "Open a Laravel project first",
    ))?;
    let root = project.root()?;
    if !host::projects()?
        .iter()
        .any(|p| p.root().ok().as_deref() == Some(&root))
    {
        return Err(Error::Stale);
    }
    Ok(project)
}
pub(crate) fn configure(ctx: &Invocation) -> Result<(), Error> {
    let project = owner(ctx)?;
    let root = project.root()?;
    let storage_key = key(&project)?;
    let before = host::storage::get(&storage_key)?;
    let current = load(&project)?;
    let storage = host::pick(
        &crate::ui_text::format(
            "helperPreferences.storageSaved",
            "Helper storage (saved: {value})",
            &[("value", &current.storage)],
        ),
        &[
            PickItem::new(
                "helpers",
                crate::ui_text::text(
                    "native.model_actions.1ae24650fe",
                    "Only in plugin-owned helper files",
                ),
            ),
            PickItem::new(
                "preserve",
                crate::ui_text::text(
                    "native.model_helper_parameters.8207907b5f",
                    "In model PHPDoc; preserve handwritten documentation",
                ),
            ),
            PickItem::new(
                "overwrite",
                crate::ui_text::text(
                    "native.model_helper_parameters.c5701eb1cb",
                    "In model PHPDoc; replace attached PHPDoc after review",
                ),
            ),
        ],
    )?
    .ok_or(Error::Cancelled)?;
    let counts = host::pick(
        &crate::ui_text::format(
            "helperPreferences.countSaved",
            "Relationship count annotations (saved: {value})",
            &[("value", &current.relation_counts.to_string())],
        ),
        &[
            PickItem::new(
                "off",
                crate::ui_text::text(
                    "native.model_helper_parameters.cbc6a87425",
                    "Do not synthesize relation count attributes",
                ),
            ),
            PickItem::new(
                "on",
                crate::ui_text::text(
                    "native.model_helper_parameters.a4b8b64301",
                    "Include optional withCount attributes for proven many relations",
                ),
            ),
        ],
    )?
    .ok_or(Error::Cancelled)?;
    if !matches!(counts.as_str(), "on" | "off") {
        return Err(Error::Stale);
    }
    let date_class = host::input(
        crate::ui_text::text(
            "native.model_helper_parameters.0004a950b4",
            "Declared default date annotation class (type hint only; no PHP runs)",
        ),
        Some(&current.date_class),
    )?
    .ok_or(Error::Cancelled)?
    .trim()
    .trim_start_matches('\\')
    .to_owned();
    let value = Parameters {
        version: 2,
        storage,
        relation_counts: counts == "on",
        date_class,
        ..current
    };
    validate(&value)?;
    let raw = serde_json::to_string_pretty(&value).map_err(|_| Error::Stale)?;
    let preview = crate::ui_text::format(
        "helperPreferences.parameterReview",
        "Project: {root}\nSave these project-scoped plugin preferences:\n{raw}\n\nStorage modes still require review before files are changed. The date class is an authored annotation assumption; it does not change runtime dates. Save parameters?",
        &[("root", &root), ("raw", &raw)],
    );
    if !ui_kit::prompts::confirm_exact(
        crate::ui_text::text(
            "helperPreferences.parametersConfirm",
            "Save helper parameters?",
        ),
        &preview,
    )? {
        return Err(Error::Cancelled);
    }
    if owner(ctx)?.root()? != root || host::storage::get(&storage_key)? != before {
        return Err(Error::Stale);
    }
    host::storage::set(&storage_key, &raw)
}

pub(crate) fn configure_advanced(ctx: &Invocation) -> Result<(), Error> {
    let project = owner(ctx)?;
    let root = project.root()?;
    let storage_key = key(&project)?;
    let before = host::storage::get(&storage_key)?;
    let current = load(&project)?;
    let source = host::pick(
        &crate::ui_text::format(
            "helperPreferences.fieldsSaved",
            "Helper fields (saved: {value})",
            &[("value", &current.source)],
        ),
        &[
            PickItem::new(
                "migrations",
                crate::ui_text::text(
                    "native.model_helper_parameters.7bdd94b9e9",
                    "Migration/schema-file facts",
                ),
            ),
            PickItem::new(
                "database",
                crate::ui_text::text(
                    "native.model_helper_parameters.58e36d565a",
                    "Explicit captured database facts",
                ),
            ),
            PickItem::new(
                "both",
                crate::ui_text::text(
                    "native.model_helper_parameters.6538743031",
                    "Database facts plus missing migration fields",
                ),
            ),
        ],
    )?
    .ok_or(Error::Cancelled)?;
    let enabled = host::pick_many(
        crate::ui_text::text(
            "native.model_helper_parameters.462a76a0cd",
            "Helper automation",
        ),
        &[
            PickItem::new(
                "proxies",
                crate::ui_text::text(
                    "native.model_helper_parameters.28ef2ea4c1",
                    "Generate higher-order collection proxy helpers",
                ),
            ),
            PickItem::new(
                "save",
                crate::ui_text::text(
                    "native.model_helper_parameters.65bbdb980b",
                    "Refresh plugin-owned helpers after saving a model (helper-only storage)",
                ),
            ),
        ],
    )?
    .ok_or(Error::Cancelled)?;
    if enabled.len() > 2
        || enabled
            .iter()
            .any(|item| !matches!(item.as_str(), "proxies" | "save"))
    {
        return Err(Error::Stale);
    }
    let raw_commands=host::input(crate::ui_text::text("native.model_helper_parameters.d5a212bc4c", "After successful generation: JSON argv arrays without php/artisan prefix; no shell expansion"),Some(&serde_json::to_string(&current.post_commands).map_err(|_|Error::Stale)?))?.ok_or(Error::Cancelled)?;
    if raw_commands.len() > 64 * 1024 {
        return Err(Error::Invalid(
            crate::ui_text::text(
                "native.model_helper_parameters.0e58c29116",
                "Post-generation commands exceed 64KiB",
            )
            .into(),
        ));
    }
    let post_commands = serde_json::from_str(&raw_commands).map_err(|_| {
        Error::Invalid(
            crate::ui_text::text(
                "native.model_helper_parameters.4640e0994c",
                "Use an array of Artisan argv arrays",
            )
            .into(),
        )
    })?;
    let value = Parameters {
        source,
        collection_proxies: enabled.iter().any(|item| item == "proxies"),
        on_save: enabled.iter().any(|item| item == "save"),
        post_commands,
        ..current
    };
    validate(&value)?;
    let raw = serde_json::to_string_pretty(&value).map_err(|_| Error::Stale)?;
    if !ui_kit::prompts::confirm_exact(
        crate::ui_text::text(
            "helperPreferences.automationConfirm",
            "Save helper source and automation?",
        ),
        &crate::ui_text::format(
            "helperPreferences.automationReview",
            "Project: {root}\n{raw}\n\nDatabase modes only use explicitly refreshed captures. On-save writes only plugin-owned helper output. Post-generation argv run only after the resulting helper files exist exactly as reviewed. Save?",
            &[("root", &root), ("raw", &raw)],
        ),
    )? {
        return Err(Error::Cancelled);
    }
    if owner(ctx)?.root()? != root || host::storage::get(&storage_key)? != before {
        return Err(Error::Stale);
    }
    host::storage::set(&storage_key, &raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    #[test]
    fn reviewed_parameters_persist_only_after_confirmation_and_cancel_preserves_them() {
        let mock = MockHost::install();
        mock.add_file("artisan", "<?php");
        let project = host::projects().unwrap().remove(0);
        assert_eq!(load(&project).unwrap(), Parameters::default());
        for answer in ["preserve", "on", "App\\Support\\ProjectDate", "yes"] {
            mock.answer_next_prompt(Some(&[answer]));
        }
        configure(&mock.invocation()).unwrap();
        let saved = load(&project).unwrap();
        assert_eq!(saved.storage, "preserve");
        assert!(saved.relation_counts);
        assert_eq!(saved.date_class, "App\\Support\\ProjectDate");
        let raw = host::storage::get(&key(&project).unwrap()).unwrap();
        for answer in ["overwrite", "off", "Other\\Date"] {
            mock.answer_next_prompt(Some(&[answer]));
        }
        mock.answer_next_prompt(None);
        assert_eq!(configure(&mock.invocation()), Err(Error::Cancelled));
        assert_eq!(host::storage::get(&key(&project).unwrap()).unwrap(), raw);
        assert!(mock.process_runs().is_empty());
    }
    #[test]
    fn legacy_preferences_migrate_and_advanced_settings_are_reviewed() {
        let mock = MockHost::install();
        mock.add_file("artisan", "<?php");
        let project = mock.invocation().project().unwrap();
        host::storage::set(
            &key(&project).unwrap(),
            r#"{"version":1,"storage":"helpers","relationCounts":true,"dateClass":"App\\Date"}"#,
        )
        .unwrap();
        let migrated = load(&project).unwrap();
        assert_eq!(migrated.version, 2);
        assert_eq!(migrated.source, "migrations");
        assert!(migrated.relation_counts);
        assert!(!migrated.on_save);
        for answer in [
            vec!["both"],
            vec!["proxies", "save"],
            vec!["[[\"cache:clear\"]]"],
            vec!["yes"],
        ] {
            mock.answer_next_prompt(Some(&answer));
        }
        configure_advanced(&mock.invocation()).unwrap();
        let saved = load(&project).unwrap();
        assert_eq!(saved.source, "both");
        assert!(saved.collection_proxies && saved.on_save);
        assert_eq!(saved.post_commands, vec![vec!["cache:clear".to_owned()]]);
        assert!(mock.process_runs().is_empty());
        let mut invalid = saved;
        invalid.storage = "preserve".into();
        assert!(validate(&invalid).is_err());
        invalid.on_save = false;
        invalid.post_commands = vec![vec!["-r".into(), "arbitrary".into()]];
        assert!(validate(&invalid).is_err());
    }

    #[test]
    fn persisted_parameters_validate_schema_and_refuse_unknown_settings() {
        let mock = MockHost::install();
        let project = host::projects().unwrap().remove(0);
        for raw in [
            r#"{"version":3,"storage":"helpers","relationCounts":false,"dateClass":"Carbon"}"#,
            r#"{"version":1,"storage":"helpers","relationCounts":false,"dateClass":"Carbon","unexpected":true}"#,
            "invalid",
        ] {
            host::storage::set(&key(&project).unwrap(), raw).unwrap();
            assert!(load(&project).is_err());
        }
        assert!(mock.process_runs().is_empty());
    }
}
