//! Reviewed defaults-only service config creation from project Composer context.
use crate::*;
use openidea_plugin::file_actions::{FileAction, FileOperation};
#[derive(Clone)]
struct Pending {
    root: String,
    doc: Document,
}
#[derive(Default)]
pub(crate) struct Creation {
    next: u64,
    pending: BTreeMap<String, Pending>,
}
fn valid_source(root: &str, doc: &Document) -> bool {
    doc.language == "json"
        && doc.path == format!("{root}/composer.json")
        && crate::file_actions::normalized(root)
        && !root.contains("//")
        && crate::strict_json::parse(&doc.text, 64 << 10).is_ok_and(|json| json.is_object())
}
pub(crate) fn actions(owner: &mut Main, doc: &Document) -> Vec<FileAction> {
    let Some(root) = host::project_of(&doc.path).and_then(|p| p.root().ok()) else {
        return Vec::new();
    };
    if !valid_source(&root, doc) || !owner.indexes.get(&root).is_some_and(|index|index.settings_current()) {
        return Vec::new();
    }
    if owner.config_creation.pending.len() >= 8 {
        owner.config_creation.pending.clear();
    }
    let Some(next) = owner.config_creation.next.checked_add(1) else {
        return Vec::new();
    };
    owner.config_creation.next = next;
    let key = format!("config-create:{next}:{}", doc.version);
    owner.config_creation.pending.insert(
        key.clone(),
        Pending {
            root,
            doc: doc.clone(),
        },
    );
    vec![FileAction {
        title: "Create Symfony service configuration…".into(),
        kind: Some("refactor.rewrite".into()),
        data: Some(key),
        ..FileAction::default()
    }]
}
fn current(owner: &Main, pending: &Pending, doc: &Document) -> Result<Project, Error> {
    if doc.path != pending.doc.path
        || doc.language != pending.doc.language
        || doc.version != pending.doc.version
        || doc.text != pending.doc.text
        || !valid_source(&pending.root, doc)
        || !owner.indexes.get(&pending.root).is_some_and(|index|index.settings_current())
    {
        return Err(Error::Stale);
    }
    let project = host::project_of(&doc.path).ok_or(Error::Stale)?;
    if project.root()? != pending.root
        || project.read_file(&doc.path)? != doc.text
        || !index::is_symfony(&project)
    {
        return Err(Error::Stale);
    }
    Ok(project)
}
fn content(format: &str) -> Result<&'static str, Error> {
    match format {
        "yaml" => {
            Ok("services:\n    _defaults:\n        autowire: true\n        autoconfigure: true\n")
        }
        "xml" => Ok(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<container xmlns=\"http://symfony.com/schema/dic/services\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:schemaLocation=\"http://symfony.com/schema/dic/services https://symfony.com/schema/dic/services/services-1.0.xsd\">\n    <services>\n        <defaults autowire=\"true\" autoconfigure=\"true\"/>\n    </services>\n</container>\n",
        ),
        "php" => Ok(
            "<?php\n\ndeclare(strict_types=1);\n\nnamespace Symfony\\Component\\DependencyInjection\\Loader\\Configurator;\n\nreturn static function (ContainerConfigurator $container): void {\n    $container->services()->defaults()->autowire()->autoconfigure();\n};\n",
        ),
        _ => Err(Error::Stale),
    }
}
fn yaml_string(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}
fn php_string(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
}
fn xml_attr(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\"', "&quot;")
        .replace('\'', "&apos;")
}
fn resource_content(format: &str, mapping: &crate::psr4::Mapping) -> Result<String, Error> {
    let prefix = format!("{}\\", mapping.namespace);
    let resource = format!("../{}/", mapping.directory);
    let exclude = format!("../{}/Kernel.php", mapping.directory);
    match format {
        "yaml" => Ok(format!(
            "{}\n    {prefix}:\n        resource: {}\n        exclude:\n            - {}\n",
            content("yaml")?,
            yaml_string(&resource),
            yaml_string(&exclude)
        )),
        "xml" => Ok(format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<container xmlns=\"http://symfony.com/schema/dic/services\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:schemaLocation=\"http://symfony.com/schema/dic/services https://symfony.com/schema/dic/services/services-1.0.xsd\">\n    <services>\n        <defaults autowire=\"true\" autoconfigure=\"true\"/>\n        <prototype namespace=\"{}\" resource=\"{}\" exclude=\"{}\"/>\n    </services>\n</container>\n",
            xml_attr(&prefix),
            xml_attr(&resource),
            xml_attr(&exclude)
        )),
        "php" => Ok(format!(
            "<?php\n\ndeclare(strict_types=1);\n\nnamespace Symfony\\Component\\DependencyInjection\\Loader\\Configurator;\n\nreturn static function (ContainerConfigurator $container): void {{\n    $services = $container->services();\n    $services->defaults()->autowire()->autoconfigure();\n    $services->load({}, {})\n        ->exclude({});\n}};\n",
            php_string(&prefix),
            php_string(&resource),
            php_string(&exclude)
        )),
        _ => Err(Error::Stale),
    }
}
pub(crate) fn resolve(
    owner: &mut Main,
    doc: &Document,
    action: FileAction,
) -> Result<FileAction, Error> {
    let pending = owner
        .config_creation
        .pending
        .remove(action.data.as_deref().ok_or(Error::Stale)?)
        .ok_or(Error::Stale)?;
    let project = current(owner, &pending, doc)?;
    let Some(format) = host::pick(
        "Service configuration format",
        &[
            PickItem::new("yaml", "YAML"),
            PickItem::new("xml", "XML"),
            PickItem::new("php", "PHP DI configuration"),
        ],
    )?
    else {
        return Err(Error::Cancelled);
    };
    let body = content(&format)?;
    let Some(name) = host::input(
        "New filename directly under config/",
        Some(&format!("services.{format}")),
    )?
    else {
        return Err(Error::Cancelled);
    };
    if name.len() > 128
        || name.starts_with('.')
        || !name.ends_with(&format!(".{format}"))
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        || name[..name.len() - format.len() - 1].is_empty()
    {
        return Err("Use a plain filename with the selected format extension.".into());
    }
    let target = format!("{}/config/{name}", pending.root);
    if !crate::file_actions::normalized(&target) {
        return Err(Error::Stale);
    }
    current(owner, &pending, doc)?;
    crate::file_actions::vacant(&project, &target, &pending.root)?;
    let Some(mode) = host::pick(
        "Service definitions in the new file",
        &[
            PickItem::new("defaults", "Defaults only (no resource discovery)"),
            PickItem::new(
                "resources",
                "Load services from a PSR-4 source folder (skips Kernel.php)",
            ),
        ],
    )?
    else {
        return Err(Error::Cancelled);
    };
    let mut selected_mapping = None;
    let body = match mode.as_str() {
        "defaults" => body.to_owned(),
        "resources" => {
            let mappings = crate::psr4::mappings(&doc.text)?;
            let items: Vec<_> = mappings
                .iter()
                .enumerate()
                .map(|(i, m)| {
                    PickItem::new(
                        &i.to_string(),
                        &format!("{} → {}", m.namespace, m.directory),
                    )
                })
                .collect();
            let Some(selected) = host::pick("Source folder to load services from", &items)?
            else {
                return Err(Error::Cancelled);
            };
            let mapping = mappings
                .get(selected.parse::<usize>().map_err(|_| Error::Stale)?)
                .ok_or(Error::Stale)?;
            if !crate::psr4::unique(&mappings, &mapping.namespace, &mapping.directory) {
                return Err(Error::Stale);
            }
            crate::psr4::directory(&project, &pending.root, &mapping.directory)?;
            let generated = resource_content(&format, mapping)?;
            selected_mapping = Some(mapping.clone());
            generated
        }
        _ => return Err(Error::Stale),
    };
    let text = body.replace('\n', intentions::eol_of(&doc.text));
    let preview = format!(
        "Creates {target} and opens it.\nSymfony loads it only if your configuration imports it.\nDelete the file to undo.\n\n{text}"
    );
    if !ui_kit::prompts::confirm_exact("Create service configuration?", &preview)? {
        return Err(Error::Cancelled);
    }
    current(owner, &pending, doc)?;
    if let Some(mapping) = selected_mapping {
        crate::psr4::directory(&project, &pending.root, &mapping.directory)?;
    }
    crate::file_actions::vacant(&project, &target, &pending.root)?;
    Ok(FileAction {
        title: action.title,
        kind: action.kind,
        operations: vec![FileOperation::Create {
            path: target.clone(),
            text,
            overwrite: false,
        }],
        open: Some(target),
        ..FileAction::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{file_actions::FileActions, mock::MockHost};
    fn setup(crlf: bool) -> (MockHost, Main, Document) {
        let host =
            MockHost::install().with_capabilities(&["project.read", "events", "editor.read"]);
        let text = format!(
            "{{\"require\":{{\"symfony/framework-bundle\":\"^7.0\"}}}}{}",
            if crlf { "\r\n" } else { "\n" }
        );
        host.add_file("composer.json", &text);
        let mut owner = Main::default();
        owner.activate(&host.context("aritti.symfony")).unwrap();
        (
            host,
            owner,
            Document {
                path: "/project/composer.json".into(),
                language: "json".into(),
                version: 2,
                text,
            },
        )
    }
    fn action(owner: &mut Main, doc: &Document) -> FileAction {
        let mut actions = owner
            .file_actions(doc, Range::default())
            .into_iter()
            .filter(|a| {
                a.data
                    .as_deref()
                    .is_some_and(|d| d.starts_with("config-create:"))
            });
        let action = actions.next().unwrap();
        assert!(actions.next().is_none());
        action
    }
    fn answer(host: &MockHost, format: &str, name: &str, accept: bool) {
        host.answer_next_prompt(Some(&[format]));
        host.answer_next_prompt(Some(&[name]));
        host.answer_next_prompt(Some(&["defaults"]));
        host.answer_next_prompt(accept.then_some(&[]));
    }
    #[test]
    fn contextual_config_formats_are_reviewed_atomic_nonoverwriting_and_keep_source_eol() {
        for format in ["yaml", "xml", "php"] {
            for crlf in [false, true] {
                let (host, mut owner, doc) = setup(crlf);
                let pending = action(&mut owner, &doc);
                let replay = pending.clone();
                let name = format!("services.local.{format}");
                answer(&host, format, &name, true);
                let resolved = owner.resolve_file_action(&doc, pending).unwrap();
                let [
                    FileOperation::Create {
                        path,
                        text,
                        overwrite: false,
                    },
                ] = resolved.operations.as_slice()
                else {
                    panic!("one nonoverwriting create")
                };
                assert_eq!(path, &format!("/project/config/{name}"));
                assert_eq!(resolved.open.as_ref(), Some(path));
                assert!(resolved.edits.is_empty());
                assert!(
                    !text.contains("App\\") && !text.contains("../src") && !text.contains("$END$")
                );
                assert_eq!(text.contains("\r\n"), crlf);
                if crlf {
                    assert!(!text.replace("\r\n", "").contains('\n'));
                }
                assert!(matches!(
                    host.invocation().project().unwrap().read_file(path),
                    Err(Error::NotFound(_))
                ));
                assert!(matches!(
                    owner.resolve_file_action(&doc, replay),
                    Err(Error::Stale)
                ));
            }
        }
    }
    #[test]
    fn explicit_resources_preserve_service_template_behavior_and_escape_each_format() {
        for format in ["yaml", "xml", "php"] {
            for (label, namespace, directory) in [
                ("basic", "App", "src"),
                ("escaped", "Acme\\Domain", "packages/a&'\"$x/lib"),
            ] {
                let (host, mut owner, mut doc) = setup(false);
                doc.text=serde_json::json!({"require":{"symfony/framework-bundle":"^7.0"},"autoload":{"psr-4":{format!("{namespace}\\"):format!("{directory}/")}}}).to_string();
                host.add_file("composer.json", &doc.text);
                let mut path = String::new();
                for part in directory.split('/') {
                    if !path.is_empty() {
                        path.push('/');
                    }
                    path.push_str(part);
                    host.add_dir(&path);
                }
                let pending = action(&mut owner, &doc);
                let name = format!("services.{format}");
                host.answer_next_prompt(Some(&[format]));
                host.answer_next_prompt(Some(&[&name]));
                host.answer_next_prompt(Some(&["resources"]));
                host.answer_next_prompt(Some(&["0"]));
                host.answer_next_prompt(Some(&[]));
                let resolved = owner.resolve_file_action(&doc, pending).unwrap();
                let [
                    FileOperation::Create {
                        path,
                        text,
                        overwrite: false,
                    },
                ] = resolved.operations.as_slice()
                else {
                    panic!("one create")
                };
                assert_eq!(path, &format!("/project/config/{name}"));
                assert!(
                    text.contains("autowire")
                        && text.contains("autoconfigure")
                        && text.contains("Kernel.php")
                );
                assert!(!text.contains("$END$"));
                if label == "basic" {
                    assert!(text.contains("../src/") && text.contains("App"));
                }
                if label == "escaped" {
                    match format {
                        "yaml" => assert!(text.contains("a&''\"$x")),
                        "xml" => assert!(text.contains("a&amp;&apos;&quot;$x")),
                        "php" => assert!(text.contains("a&\\'\"$x")),
                        _ => unreachable!(),
                    }
                }
                let fixture = std::path::Path::new(
                    "/tmp/symfony-resource-config-fixtures-closure478-20261109",
                );
                std::fs::create_dir_all(fixture).unwrap();
                std::fs::write(fixture.join(format!("{label}.{format}")), text).unwrap();
            }
        }
    }
    #[test]
    fn resource_mode_requires_explicit_valid_existing_production_root() {
        for mapping in [
            r#"{"App\\":["src/"]}"#,
            r#"{"App\\":"../foreign/"}"#,
            r#"{"":"src/"}"#,
            r#"{"App\\":"src/"}"#,
        ] {
            let (host, mut owner, mut doc) = setup(false);
            doc.text = format!(
                r#"{{"require":{{"symfony/framework-bundle":"^7.0"}},"autoload":{{"psr-4":{mapping}}}}}"#
            );
            host.add_file("composer.json", &doc.text);
            let pending = action(&mut owner, &doc);
            host.answer_next_prompt(Some(&["php"]));
            host.answer_next_prompt(Some(&["services.php"]));
            host.answer_next_prompt(Some(&["resources"]));
            host.answer_next_prompt(Some(&["0"]));
            host.answer_next_prompt(Some(&[]));
            assert!(
                owner.resolve_file_action(&doc, pending).is_err(),
                "{mapping}"
            );
        }
    }

    #[test]
    fn config_creation_refuses_existing_dirty_invalid_names_and_cancellation() {
        for name in [
            "../escape.php",
            "services.yaml",
            ".hidden.php",
            "services.php\n",
        ] {
            let (host, mut owner, doc) = setup(false);
            let pending = action(&mut owner, &doc);
            answer(&host, "php", name, true);
            assert!(owner.resolve_file_action(&doc, pending).is_err(), "{name}");
        }
        let (host, mut owner, doc) = setup(false);
        let pending = action(&mut owner, &doc);
        host.add_file("config/services.php", "existing");
        answer(&host, "php", "services.php", true);
        assert!(owner.resolve_file_action(&doc, pending).is_err());
        let (host, mut owner, doc) = setup(false);
        let pending = action(&mut owner, &doc);
        host.open_editor("/project/config/services.php", "php", "unsaved", &[]);
        answer(&host, "php", "services.php", true);
        assert!(owner.resolve_file_action(&doc, pending).is_err());
        let (host, mut owner, doc) = setup(false);
        let pending = action(&mut owner, &doc);
        answer(&host, "yaml", "services.yaml", false);
        assert!(matches!(
            owner.resolve_file_action(&doc, pending),
            Err(Error::Cancelled)
        ));
    }
    #[test]
    fn config_creation_requires_current_root_source_version_and_unambiguous_json_context() {
        let (host, mut owner, doc) = setup(false);
        let pending = action(&mut owner, &doc);
        assert!(matches!(
            owner.resolve_file_action(
                &Document {
                    version: 3,
                    ..doc.clone()
                },
                pending
            ),
            Err(Error::Stale)
        ));
        let pending = action(&mut owner, &doc);
        host.set_project_root(Some("/other"));
        assert!(matches!(
            owner.resolve_file_action(&doc, pending),
            Err(Error::Stale)
        ));
        assert!(owner.file_actions(&doc, Range::default()).is_empty());
        let (host, mut owner, doc) = setup(false);
        let pending = action(&mut owner, &doc);
        host.add_file("composer.json", "{}");
        assert!(matches!(
            owner.resolve_file_action(&doc, pending),
            Err(Error::Stale)
        ));
        let (_host, mut owner, doc) = setup(false);
        for other in [
            Document {
                path: "/project/package.json".into(),
                ..doc.clone()
            },
            Document {
                language: "jsonc".into(),
                ..doc.clone()
            },
            Document {
                text: r#"{"require":{},"require":{}}"#.into(),
                ..doc.clone()
            },
        ] {
            assert!(owner.file_actions(&other, Range::default()).is_empty());
        }
    }
}
