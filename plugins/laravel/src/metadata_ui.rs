//! Presentation only. Metadata parsers/cache retain raw diagnostics and source identities.
pub(super) fn issue(index: &crate::Index, message: &str) -> String {
    if let Some(rendered) = render(message) {
        return rendered;
    }
    if let Some(source) = super::issue_owner(message) {
        if let Some(detail) = message
            .strip_prefix(source)
            .and_then(|tail| tail.strip_prefix(": "))
        {
            if let Some(rendered) = render(detail) {
                return format!("{source}: {rendered}");
            }
        }
    }
    if let Some((prefix, detail)) = message.rsplit_once(": ") {
        if let Some((path, offset)) = prefix.rsplit_once(':') {
            let proven = offset.parse::<usize>().ok().is_some_and(|offset| {
                index
                    .sql_sources
                    .get(path)
                    .and_then(|source| source["issues"].as_array())
                    .is_some_and(|issues| {
                        issues.iter().any(|issue| {
                            issue["offset"].as_u64() == Some(offset as u64)
                                && issue["message"].as_str() == Some(detail)
                        })
                    })
            });
            if proven {
                let rendered = crate::migrations::sql_issue_label(detail);
                if rendered != detail {
                    return format!("{prefix}: {rendered}");
                }
            }
        }
    }
    message.to_owned()
}
fn render(message: &str) -> Option<String> {
    match message {
        "Enum-backed config key inventory exceeded its 2-MiB source budget; unresolved files remain incomplete" => {
            Some(crate::ui_text::t(
                "metadata.issue.enumConfigBudget",
                message,
            ))
        }
        "Metadata file budget exceeded; only 128 source files are loaded" => {
            Some(crate::ui_text::t("metadata.issue.e91d2d16c285", message))
        }
        "Custom framework configuration bootstrap: base defaults withheld" => {
            Some(crate::ui_text::t("metadata.issue.be819cb75cd2", message))
        }
        "Framework config inventory exceeded 64-file bound; defaults withheld" => {
            Some(crate::ui_text::t("metadata.issue.2596a90d9e20", message))
        }
        "Metadata class fact budget exceeded; remaining declarations withheld" => {
            Some(crate::ui_text::t("metadata.issue.054b006a1675", message))
        }
        "Package source discovery reached its bounded descriptor/source budget" => {
            Some(crate::ui_text::t("metadata.issue.264dbaef9b4e", message))
        }
        "Blade component metadata exceeds the 128-declaration bound" => {
            Some(crate::ui_text::t("metadata.issue.22d8e3d1244c", message))
        }
        "Blade component metadata source discovery reached its 750ms / 128-source / 2MiB aggregate bound" => {
            Some(crate::ui_text::t("metadata.issue.769f5b3e04fa", message))
        }
        "Artisan metadata completions require an explicit Artisan metadata refresh followed by reindex; no project PHP runs during completion" => {
            Some(crate::ui_text::t("metadata.issue.ce45d28e29e0", message))
        }
        "Metadata type dependency scan exceeded its 256-path/64-file/4-MiB/350-ms budget; remaining class facts withheld" => {
            Some(crate::ui_text::t("metadata.issue.e15778f39d93", message))
        }
        "Parent configuration discovery reached its 64-declaration budget; remaining roots withheld" => {
            Some(crate::ui_text::t("metadata.issue.e89607115b24", message))
        }
        "Parent configuration cache reached its 2-MiB/64-link budget; remaining roots withheld" => {
            Some(crate::ui_text::t("metadata.issue.3d01c02a4aba", message))
        }
        "Framework bootstrap ownership/source unavailable; base defaults withheld" => {
            Some(crate::ui_text::t("metadata.issue.bf59d1ed55e6", message))
        }
        "Framework config inventory reached 2-MiB read bound; remaining defaults withheld" => {
            Some(crate::ui_text::t("metadata.issue.afe47eb0ebe4", message))
        }
        "Framework config inventory exceeded 32768-key bound; remaining defaults withheld" => {
            Some(crate::ui_text::t("metadata.issue.bd5154675426", message))
        }
        "Dynamic package configuration key prevents authoritative missing-key diagnostics" => {
            Some(crate::ui_text::t("metadata.issue.28ff77f12323", message))
        }
        "Config default inventory reached its 2-MiB read budget; remaining defaults withheld" => {
            Some(crate::ui_text::t("metadata.issue.113acf2916bb", message))
        }
        "Package config inventory exceeded 32768-key bound; remaining defaults withheld" => {
            Some(crate::ui_text::t("metadata.issue.8ff97fa4651f", message))
        }
        "metadata inventories exceed aggregate enumeration budget; remaining scans withheld" => {
            Some(crate::ui_text::t("metadata.issue.0c37acfa4d6f", message))
        }
        "metadata inventories exceed aggregate cache budget; remaining rules withheld" => {
            Some(crate::ui_text::t("metadata.issue.65de58d90e1a", message))
        }
        "Blade icon provider source count exceeds 128; remaining providers withheld" => {
            Some(crate::ui_text::t("metadata.issue.f64f79804286", message))
        }
        "Blade icon provider sources exceed aggregate 4MiB budget; remaining providers withheld" => {
            Some(crate::ui_text::t("metadata.issue.256300cb0ad3", message))
        }
        "Blade icon provider call candidates exceed128; remaining registrations withheld" => {
            Some(crate::ui_text::t("metadata.issue.d4f08e45ac4b", message))
        }
        "Blade icon provider registrations exceed128 calls; remaining sets withheld" => {
            Some(crate::ui_text::t("metadata.issue.18d90992aec2", message))
        }
        "config/blade-icons.php: icon sets are not a proven literal returned map; inventory is partial" => {
            Some(crate::ui_text::t("metadata.issue.cdf0663c57cb", message))
        }
        "Livewire config belongs to another project; component inventory is partial" => {
            Some(crate::ui_text::t("metadata.issue.67d9dcf911c3", message))
        }
        "Livewire config is dynamically constructed; component inventory is partial" => {
            Some(crate::ui_text::t("metadata.issue.3233a5a95f92", message))
        }
        "Livewire class_namespace is dynamic; class component inventory is partial" => {
            Some(crate::ui_text::t("metadata.issue.d04aa2fce57a", message))
        }
        "completion transformers must be an array of strings" => {
            Some(crate::ui_text::t("metadata.issue.808660fbd8f6", message))
        }
        "completion transformers exceed the 64-step/16-KiB budget" => {
            Some(crate::ui_text::t("metadata.issue.dfe2fe30fd48", message))
        }
        "Index source cache budget exceeded (8 MiB or 5000 files); remaining source reads withheld" => {
            Some(crate::ui_text::t("metadata.issue.5c8b7dc293d4", message))
        }
        _ => crate::ui_text::owned_error(&message, UI_ERROR_TEMPLATES),
    }
}
const UI_ERROR_TEMPLATES: &[(&str, &str)] = &[
    (
        "metadata.issue.daedb3ad09c5",
        "{source}: invalid {domain}.{field} name",
    ),
    (
        "metadata.issue.b36c8bfaefaf",
        "{source}: invalid helperCode ignoreClasses entry",
    ),
    (
        "metadata.issue.5ce3a32efe96",
        "{source}: classMixins budget exceeded",
    ),
    (
        "metadata.issue.cfa9dab42232",
        "{source}: invalid helperCode classMixins entry",
    ),
    (
        "metadata.issue.e4aa29675f04",
        "{source}: invalid forwarding baseFqn",
    ),
    (
        "metadata.issue.294384df6c43",
        "{source}: invalid constructor forwarding method",
    ),
    (
        "metadata.issue.4856e7ea742b",
        "{source}: invalid view.paths entry",
    ),
    (
        "metadata.issue.00d58903c53a",
        "{source}: invalid routing.rootFiles entry",
    ),
    (
        "metadata.issue.7a142d5702e4",
        "{source}: invalid database.{key} entry",
    ),
    (
        "metadata.issue.8a4f66638f6b",
        "{source}: invalid version override for {name}",
    ),
    (
        "metadata.issue.e8562e386075",
        "{source}: invalid packages.inertia.pagePaths path",
    ),
    (
        "metadata.issue.108bb15d7a22",
        "{source}: invalid Livewire namespace/path",
    ),
    (
        "metadata.issue.9bf9c403fcc6",
        "{source}: completion missing complete",
    ),
    (
        "metadata.issue.34810fdff8cb",
        "{source}: unsupported completion condition",
    ),
    (
        "metadata.issue.b6eccf9970aa",
        "{source}: malformed completion condition filter",
    ),
    (
        "metadata.issue.71b9144e8e80",
        "{source}: unsupported completion place {place}",
    ),
    (
        "metadata.issue.7efbaf9a29c5",
        "{source}: invalid parameterNames identifier",
    ),
    (
        "metadata.issue.14768bdd9348",
        "{source}: invalid completion parameter positions",
    ),
    (
        "metadata.issue.15e263dd8596",
        "{source}: unsupported completion options",
    ),
    (
        "metadata.issue.72c2679f0c7a",
        "{source}: invalid Passport scope name in {path}",
    ),
    (
        "metadata.issue.3921bde0cd9d",
        "Ambiguous constructor-forwarded class {full}",
    ),
    (
        "metadata.issue.f69b331c3c30",
        "{source}: blade.components must be an object",
    ),
    (
        "metadata.issue.e76e78693a98",
        "{source}: unsupported blade.components.{key}",
    ),
    (
        "metadata.issue.bf16828f9305",
        "{source}: blade.components.{key} must be an array",
    ),
    (
        "metadata.issue.ee88b6cdc9a8",
        "{source}: invalid blade.components.viewPaths entry",
    ),
    (
        "metadata.issue.bc0d1247a1ea",
        "{source}: invalid blade.components.phpNamespaces entry",
    ),
    (
        "metadata.issue.18c529dc41b8",
        "{source}: invalid blade.components.list entry",
    ),
    (
        "metadata.issue.997a431d5ee2",
        "{source}: invalid blade.components.fromConfig entry",
    ),
    (
        "metadata.issue.b0d9151a6c70",
        "{source}: invalid blade.components.ignoreViewPaths entry",
    ),
    (
        "metadata.issue.da70a90d7df1",
        "unresolved owned component class {full}",
    ),
    (
        "metadata.issue.677ef9650406",
        "{relative}: dynamic component class withheld",
    ),
    (
        "metadata.issue.273cf9b77a21",
        "{relative}: component class has no unambiguous owned Composer source",
    ),
    (
        "metadata.issue.e70e8402e65b",
        "{relative}: dynamic Blade component prefix/namespace withheld",
    ),
    (
        "metadata.issue.559025d77820",
        "{value0}: Livewire classNamespace {namespace} has no owned Composer PSR-4 mapping",
    ),
    (
        "metadata.issue.ed7662c04834",
        "{source}: metadata exceeds size limit",
    ),
    (
        "metadata.issue.67bb0249d174",
        "{value0}: stringsWithLocation target {value1} is missing or outside current project ownership",
    ),
    (
        "metadata.issue.791edb92e59e",
        "{source}: invalid, duplicate-key or oversized metadata JSON",
    ),
    (
        "metadata.issue.968ead8b9691",
        "{source}: invalid eloquent.modelTables table for {model}",
    ),
    (
        "metadata.issue.d37fb302a55d",
        "{source}: conflicting eloquent.modelTables override for {model}; later metadata source wins",
    ),
    (
        "metadata.issue.86957035ad97",
        "{source}: invalid routing.routeFacadeMethods httpMethod for {name}",
    ),
    (
        "metadata.issue.9fb5549c0a8c",
        "{source}: invalid helperCode ignoreNamespaces entry",
    ),
    (
        "metadata.issue.730090aa5d59",
        "{source}: constructor forwarding method budget exceeded",
    ),
    (
        "metadata.issue.6d59d4104920",
        "{source}: {name} injection prefix/suffix is not supported",
    ),
    (
        "metadata.issue.0745fcc547bf",
        "{source}: conflicting package override {name}; explicit enable supersedes an earlier source disable",
    ),
    (
        "metadata.issue.624125949a4f",
        "{source}: conflicting package override {name}; explicit disable wins in this source",
    ),
    (
        "metadata.issue.11f70b2d8a31",
        "{source}: deprecated inertia metadata; use packages.inertia",
    ),
    (
        "metadata.issue.0641fcd57f12",
        "{source}: invalid packages.livewire.namespaces entry; need classNamespace or owned viewPath and a literal tagPrefix",
    ),
    (
        "metadata.issue.7578a5d7eda2",
        "{source}: unreadable or invalid Passport scope file {path}",
    ),
    (
        "metadata.issue.85d2774d1241",
        "{source}: unsupported completion category {category}",
    ),
    (
        "metadata.issue.21663d7e400b",
        "{source}: unsupported or malformed {category} completion options",
    ),
    (
        "metadata.issue.5dd28d822a0a",
        "{source}: malformed {category} completion option shape",
    ),
    (
        "metadata.issue.dcda0a609c6b",
        "{source}: completion requires functionNames or exact classFqn + methodNames",
    ),
    (
        "metadata.issue.dd3a87b0e7d7",
        "{source}: staticStrings dataFile {file}[{position}] must be a string",
    ),
    (
        "metadata.issue.f13d6399c641",
        "{source}: invalid staticStrings dataFile {file}; expected an array of strings or a location map",
    ),
    (
        "metadata.issue.cd70b8fab322",
        "{source}: Passport scopes in {path} must be an object of scope descriptions",
    ),
    (
        "metadata.issue.837ee76b4f79",
        "{source}: Passport scope {name} in {path} must have a string description",
    ),
    (
        "metadata.issue.8aa170011560",
        "{relative}: external metadata {dependency} is not indexed; save/reindex to validate its contents",
    ),
    (
        "metadata.issue.dbafe00d6ad1",
        "Metadata type source {relative} exceeds 128-KiB declaration budget; withheld",
    ),
    (
        "metadata.issue.ce6430d09897",
        "Ambiguous Composer declarations for metadata class {full}; withheld",
    ),
    (
        "metadata.issue.018e6caa5fa6",
        "Conditional configuration defaults for {key} are not authoritative; defaults withheld",
    ),
    (
        "metadata.issue.8e97e855a2d5",
        "Config defaults for {key} have ambiguous provider order; defaults withheld",
    ),
    (
        "metadata.issue.a4a5cf911bd6",
        "Config defaults for {key} have an unresolved app override; defaults withheld",
    ),
    (
        "metadata.issue.0b3a5fb5f7d3",
        "metadata inventory {glob}: exceeds {FILES} files; withheld",
    ),
    (
        "metadata.issue.3a060d4af1ad",
        "metadata inventory {glob}: file enumeration unavailable",
    ),
    (
        "metadata.issue.0b589708cd60",
        "metadata inventory {relative}: content budget exceeded",
    ),
    (
        "metadata.issue.e73506efa0c3",
        "{source}: {category} reached {NAMES}-name cap; inventory may be incomplete",
    ),
    (
        "metadata.issue.5c83915d2f52",
        "{source}: {category} exceeds aggregate cache budget; withheld",
    ),
    (
        "metadata.issue.829f8dbd1bb1",
        "{source}: directoryFiles path escapes project or is unsupported: {directory}",
    ),
    (
        "metadata.issue.1d4184c95aad",
        "directoryFiles {relative}: directory identity changed or symlink; inventory is partial",
    ),
    (
        "metadata.issue.fc02e2d42e74",
        "directoryFiles {relative}: directory walk budget reached; inventory is partial",
    ),
    (
        "metadata.issue.66ad5dc2b416",
        "directoryFiles {relative}: scoped filesystem read grant unavailable; file-derived fallback omits empty directories",
    ),
    (
        "metadata.issue.a693171680a5",
        "directoryFiles {relative}: directory listing unavailable; inventory is partial",
    ),
    (
        "metadata.issue.0836ab0a70c3",
        "directoryFiles {relative}: directory entry budget reached; inventory is partial",
    ),
    (
        "metadata.issue.4728c695818b",
        "directoryFiles {relative}: depth limit reached; inventory is partial",
    ),
    (
        "metadata.issue.42b7f5128775",
        "{path}: Blade icon set {set} has dynamic, disk-backed or unproven options; withheld",
    ),
    (
        "metadata.issue.abbf42d086fc",
        "config/blade-icons.php: icon set {set} has dynamic, disk-backed or invalid options; withheld",
    ),
    (
        "metadata.issue.bd1861a6f1b5",
        "{source}: unresolved or cyclic helperCode mixin {target}",
    ),
    (
        "metadata.issue.48b26af970c0",
        "{source}: stringsWithLocation must be an object",
    ),
    (
        "metadata.issue.72482f2fb388",
        "{source}: invalid stringsWithLocation key",
    ),
    (
        "metadata.issue.70358a307f0b",
        "{source}: invalid stringsWithLocation line for {name}",
    ),
    (
        "metadata.issue.f0bc72d6374b",
        "{source}: invalid stringsWithLocation path for {name}",
    ),
    (
        "metadata.issue.6a6f93e55eb2",
        "{source}: stringsWithLocation {name} must be a path or null",
    ),
    (
        "metadata.issue.b9019c70a84f",
        "{source}: unsupported metadata field {name}",
    ),
    (
        "metadata.issue.547df20e0517",
        "{source}: malformed metadata field {name}",
    ),
    (
        "metadata.issue.9f3b3d9b0f8a",
        "{source}: malformed metadata field {path}[{index}] (expected string)",
    ),
    (
        "metadata.issue.6c5d15d383ae",
        "{source}: malformed metadata field {path} (expected object)",
    ),
    (
        "metadata.issue.9d17791a3c7d",
        "{source}: missing required metadata field {path}.{field}",
    ),
    (
        "metadata.issue.a56a048d5917",
        "Livewire {key} is dynamic; view component inventory is partial",
    ),
    (
        "metadata.issue.47f6aee69154",
        "Livewire {key} contains an unresolved root; view component inventory is partial",
    ),
    (
        "metadata.issue.765438e37cc9",
        "{path}: NativePHP child registration uses unresolved runtime values; the static Native component inventory is partial",
    ),
    (
        "metadata.issue.e2d30171ab4d",
        "NativePHP child alias {tag} has multiple source registrations; navigation is withheld",
    ),
    (
        "metadata.issue.9b4a7a04d0b0",
        "NativePHP auto-discovered alias {tag} is ambiguous; navigation is withheld",
    ),
    (
        "metadata.issue.ddbd5540c09e",
        "{source}: metadata exceeds the {limit}-byte size limit",
    ),
    (
        "metadata.issue.9dfd903860ff",
        "{source}: completion transformers must be an array of strings",
    ),
    (
        "metadata.issue.68083962c3d0",
        "{source}: completion transformers exceed the 64-step/16-KiB budget",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cached_metadata_and_sql_facts_localize_only_when_published_and_preserve_values() {
        let host = openidea_plugin::mock::MockHost::install();
        let raw = "ide.json: unsupported metadata field custom.{source}.日本";
        let sql = "database/schema/example.dump:14: SQL block comment is unterminated; schema inventory is incomplete.";
        let unknown = "Project-authored data {source}: values stay exactly raw";
        let mut index = crate::Index::default();
        index.metadata.issues = vec![raw.into(), sql.into(), unknown.into()];
        index.sql_sources.insert("database/schema/example.dump".into(),serde_json::json!({"issues":[{"offset":14,"message":"SQL block comment is unterminated; schema inventory is incomplete."}]}));
        let schema_before = index.sql_sources.clone();
        let before = index.metadata.issues.clone();
        for language in ["ja", "ko", "zh-CN"] {
            host.set_setting("uiLanguage", &serde_json::to_string(language).unwrap());
            let diagnostics = crate::metadata::diagnostics(&index, "/project/ide.json", "/project");
            assert_eq!(diagnostics.len(), 3);
            assert!(
                diagnostics[0].message.contains("ide.json")
                    && diagnostics[0].message.contains("custom.{source}.日本")
            );
            assert!(
                !diagnostics[0]
                    .message
                    .contains("unsupported metadata field")
            );
            assert!(
                diagnostics[1]
                    .message
                    .starts_with("database/schema/example.dump:14: ")
            );
            assert!(
                !diagnostics[1]
                    .message
                    .contains("SQL block comment is unterminated")
            );
            assert_eq!(diagnostics[2].message, unknown);
            assert_eq!(index.metadata.issues, before);
            assert_eq!(index.sql_sources, schema_before);
        }
        assert!(host.requests().is_empty() && host.process_runs().is_empty());
    }
}
