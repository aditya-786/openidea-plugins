//! Composer-owned contextual replacement for the native compiler-pass template path.
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
fn valid(root: &str, doc: &Document) -> bool {
    doc.language == "json"
        && doc.path == format!("{root}/composer.json")
        && crate::file_actions::normalized(root)
        && !root.contains("//")
        && crate::psr4::mappings(&doc.text).is_ok()
}
pub(crate) fn actions(owner: &mut Main, doc: &Document) -> Vec<FileAction> {
    let Some(root) = host::project_of(&doc.path).and_then(|p| p.root().ok()) else {
        return Vec::new();
    };
    if !valid(&root, doc) || !owner.indexes.get(&root).is_some_and(|index|index.settings_current()) {
        return Vec::new();
    }
    if owner.compiler_creation.pending.len() >= 8 {
        owner.compiler_creation.pending.clear();
    }
    let Some(next) = owner.compiler_creation.next.checked_add(1) else {
        return Vec::new();
    };
    owner.compiler_creation.next = next;
    let key = format!("composer-compiler:{next}:{}", doc.version);
    owner.compiler_creation.pending.insert(
        key.clone(),
        Pending {
            root,
            doc: doc.clone(),
        },
    );
    vec![FileAction {
        title: "Create Compiler Pass Class…".into(),
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
        || !valid(&pending.root, doc)
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
pub(crate) fn resolve(
    owner: &mut Main,
    doc: &Document,
    action: FileAction,
) -> Result<FileAction, Error> {
    let pending = owner
        .compiler_creation
        .pending
        .remove(action.data.as_deref().ok_or(Error::Stale)?)
        .ok_or(Error::Stale)?;
    let project = current(owner, &pending, doc)?;
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
    let Some(chosen) = host::pick("Production PSR4 source root for the compiler pass", &items)?
    else {
        return Err(Error::Cancelled);
    };
    let mapping = mappings
        .get(chosen.parse::<usize>().map_err(|_| Error::Stale)?)
        .ok_or(Error::Stale)?;
    crate::psr4::directory(&project, &pending.root, &mapping.directory)?;
    let Some(suffix) = host::input(
        "Namespace suffix (existing directories; empty for source root)",
        Some("DependencyInjection\\Compiler"),
    )?
    else {
        return Err(Error::Cancelled);
    };
    if suffix.len() > 256
        || (!suffix.is_empty()
            && (!suffix
                .split('\\')
                .all(crate::file_actions::valid_class_name)
                || suffix.split('\\').count() > 8))
    {
        return Err("Use an empty suffix or plain namespace components.".into());
    }
    let namespace = if suffix.is_empty() {
        mapping.namespace.clone()
    } else {
        format!("{}\\{suffix}", mapping.namespace)
    };
    let directory = if suffix.is_empty() {
        mapping.directory.clone()
    } else {
        format!("{}/{}", mapping.directory, suffix.replace('\\', "/"))
    };
    if !crate::psr4::unique(&mappings, &namespace, &directory) {
        return Err("The selected namespace/directory has ambiguous production mappings.".into());
    }
    crate::psr4::directory(&project, &pending.root, &directory)?;
    let Some(name) = host::input("Compiler-pass class name", Some("AppCompilerPass"))? else {
        return Err(Error::Cancelled);
    };
    if !crate::file_actions::valid_class_name(&name) {
        return Err("Use a plain non-keyword PHP class name.".into());
    }
    let fqcn = format!("{namespace}\\{name}");
    if owner
        .indexes
        .get(&pending.root)
        .is_some_and(|i| i.classes.keys().any(|c| c.eq_ignore_ascii_case(&fqcn)))
    {
        return Err("A class with this name already exists. Choose another name.".into());
    }
    let target = format!("{}/{directory}/{name}.php", pending.root);
    crate::file_actions::vacant(&project, &target, &pending.root)?;
    let text = include_str!("../templates/CompilerPass.php")
        .replace(
            "namespace App\\DependencyInjection\\Compiler;",
            &format!("namespace {namespace};"),
        )
        .replace("$NAME$", &name)
        .replace("$END$", "")
        .replace('\n', intentions::eol_of(&doc.text));
    let preview = format!(
        "Creates {target} (class {fqcn}) and opens it.\nRegister the pass in your Kernel yourself; this doesn't.\nDelete the file to undo.\n\n{text}"
    );
    if !ui_kit::prompts::confirm_exact(&format!("Create compiler pass {name}?"), &preview)? {
        return Err(Error::Cancelled);
    }
    current(owner, &pending, doc)?;
    crate::psr4::directory(&project, &pending.root, &directory)?;
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
    fn setup(namespace: &str, directory: &str, crlf: bool) -> (MockHost, Main, Document) {
        let host = MockHost::install();
        let mut text=serde_json::json!({"require":{"symfony/framework-bundle":"^7.0"},"autoload":{"psr-4":{format!("{namespace}\\"):format!("{directory}/")}}}).to_string();
        text.push_str(if crlf { "\r\n" } else { "\n" });
        host.add_file("composer.json", &text);
        host.add_file(
            &format!("{directory}/DependencyInjection/Compiler/.keep"),
            "",
        );
        let mut path = String::new();
        for part in format!("{directory}/DependencyInjection/Compiler").split('/') {
            if !path.is_empty() {
                path.push('/');
            }
            path.push_str(part);
            host.add_dir(&path);
        }
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
        owner
            .file_actions(doc, Range::default())
            .into_iter()
            .find(|a| {
                a.data
                    .as_deref()
                    .is_some_and(|d| d.starts_with("composer-compiler:"))
            })
            .unwrap()
    }
    fn answers(host: &MockHost, suffix: &str, name: &str, accept: bool) {
        host.answer_next_prompt(Some(&["0"]));
        host.answer_next_prompt(Some(&[suffix]));
        host.answer_next_prompt(Some(&[name]));
        host.answer_next_prompt(accept.then_some(&[]));
    }
    #[test]
    fn composer_owned_compiler_preserves_template_behavior_with_reviewed_namespace_and_create() {
        for (namespace, directory, crlf) in [
            ("App", "src", false),
            ("Acme\\Module", "packages/module/lib", true),
        ] {
            let (host, mut owner, doc) = setup(namespace, directory, crlf);
            let pending = action(&mut owner, &doc);
            let replay = pending.clone();
            answers(&host, "DependencyInjection\\Compiler", "OwnedPass", true);
            let out = owner.resolve_file_action(&doc, pending).unwrap();
            let [
                FileOperation::Create {
                    path,
                    text,
                    overwrite: false,
                },
            ] = out.operations.as_slice()
            else {
                panic!("one create")
            };
            assert_eq!(
                path,
                &format!("/project/{directory}/DependencyInjection/Compiler/OwnedPass.php")
            );
            assert!(text.contains(&format!(
                "namespace {namespace}\\DependencyInjection\\Compiler;"
            )));
            assert!(text.contains("class OwnedPass implements CompilerPassInterface"));
            assert!(text.contains("public function process(ContainerBuilder $container): void"));
            assert!(
                !text.contains("$NAME$")
                    && !text.contains("$END$")
                    && !text.contains("getDefinition(")
            );
            assert_eq!(text.contains("\r\n"), crlf);
            assert!(out.edits.is_empty());
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
    #[test]
    fn compiler_creation_refuses_missing_or_ambiguous_directories_collision_reserved_name_and_cancel()
     {
        for (suffix, name) in [
            ("../escape", "Pass"),
            ("Missing", "Pass"),
            ("DependencyInjection\\Compiler", "Class"),
            ("DependencyInjection\\Compiler", "__halt_compiler"),
        ] {
            let (host, mut owner, doc) = setup("App", "src", false);
            let pending = action(&mut owner, &doc);
            answers(&host, suffix, name, true);
            assert!(owner.resolve_file_action(&doc, pending).is_err());
        }
        let (host, mut owner, doc) = setup("App", "src", false);
        let pending = action(&mut owner, &doc);
        host.add_file("src/DependencyInjection/Compiler/Exists.php", "existing");
        answers(&host, "DependencyInjection\\Compiler", "Exists", true);
        assert!(owner.resolve_file_action(&doc, pending).is_err());
        let (host, mut owner, doc) = setup("App", "src", false);
        let pending = action(&mut owner, &doc);
        answers(&host, "DependencyInjection\\Compiler", "Pass", false);
        assert!(matches!(
            owner.resolve_file_action(&doc, pending),
            Err(Error::Cancelled)
        ));
        let (host, mut owner, mut doc) = setup("App", "src", false);
        doc.text=r#"{"require":{"symfony/framework-bundle":"^7.0"},"autoload":{"psr-4":{"App\\":"src/","App\\DependencyInjection\\":"src/DependencyInjection/"}}}"#.into();
        host.add_file("composer.json", &doc.text);
        let pending = action(&mut owner, &doc);
        answers(&host, "DependencyInjection\\Compiler", "Pass", true);
        assert!(owner.resolve_file_action(&doc, pending).is_err());
    }
    #[test]
    fn compiler_tokens_are_owned_by_exact_composer_source_version_and_project() {
        let (host, mut owner, doc) = setup("App", "src", false);
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
        host.add_file("composer.json", "{}");
        assert!(matches!(
            owner.resolve_file_action(&doc, pending),
            Err(Error::Stale)
        ));
        let (host, mut owner, doc) = setup("App", "src", false);
        let pending = action(&mut owner, &doc);
        host.set_project_root(Some("/other"));
        assert!(matches!(
            owner.resolve_file_action(&doc, pending),
            Err(Error::Stale)
        ));
    }
}
