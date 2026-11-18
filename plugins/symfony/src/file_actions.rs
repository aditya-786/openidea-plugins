//! Contextual literal Twig-template creation. Does not mutate the caller or
//! infer PHP types, service registrations, Kernel edits or runtime loader truth.
use crate::*;
use openidea_plugin::FileKind;
use openidea_plugin::file_actions::{FileAction, FileActions, FileOperation};
const MAX_TEXT: usize = 128 << 10;
#[derive(Clone)]
struct Pending {
    root: String,
    doc: Document,
    name: String,
    php: bool,
}
#[derive(Default)]
pub(crate) struct Creation {
    next: u64,
    pending: BTreeMap<String, Pending>,
}

pub(crate) fn normalized(path: &str) -> bool {
    path.starts_with('/')
        && path.len() <= 4096
        && !path.contains(['\\', '*', '?', '[', ']', '{', '}'])
        && !path.chars().any(char::is_control)
        && !path.split('/').any(|part| part == "." || part == "..")
}
fn valid_name(name: &str) -> bool {
    name.len() <= 512
        && name.ends_with(".twig")
        && !name.starts_with('/')
        && name.split('/').all(|part| {
            !part.is_empty()
                && part.len() <= 255
                && part != "."
                && part != ".."
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        })
}
pub(crate) fn tag_end(text: &str, mut at: usize, closer: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut quote = None;
    while at < bytes.len() {
        match quote {
            Some(q) => {
                if bytes[at] == b'\\' {
                    at += 1;
                    if at < bytes.len() {
                        at += text[at..].chars().next()?.len_utf8();
                    }
                    continue;
                }
                if bytes[at] == q {
                    quote = None;
                }
            }
            None => {
                if bytes[at] == b'#' {
                    at = text[at..].find('\n').map_or(text.len(), |end| at + end + 1);
                    continue;
                }
                if text[at..].starts_with(closer) {
                    return Some(at);
                }
                if matches!(bytes[at], b'\'' | b'"') {
                    quote = Some(bytes[at]);
                }
            }
        }
        at += text[at..].chars().next()?.len_utf8();
    }
    None
}
/// Skip complete print strings/comments/raw bodies while locating a plain
/// first literal of a structural Twig template tag. Dynamic tails are refused.
fn literal(doc: &Document, at: usize) -> Option<String> {
    if doc.language != "twig" || !doc.path.ends_with(".twig") || doc.text.len() > MAX_TEXT {
        return None;
    }
    let text = &doc.text;
    let mut cursor = 0;
    while cursor < text.len() {
        let open = cursor + text[cursor..].find('{')?;
        if text[open..].starts_with("{#") {
            cursor = text[open + 2..]
                .find("#}")
                .map_or(text.len(), |end| open + 2 + end + 2);
            continue;
        }
        let (tag, close) = if text[open..].starts_with("{%") {
            (true, "%}")
        } else if text[open..].starts_with("{{") {
            (false, "}}")
        } else {
            cursor = open + 1;
            continue;
        };
        let end = tag_end(text, open + 2, close)?;
        cursor = end + 2;
        if !tag {
            continue;
        }
        let body = text[open + 2..end].trim_matches(['-', '~', ' ', '\t', '\r', '\n']);
        let name_end = body
            .find(|ch: char| !ch.is_ascii_alphabetic())
            .unwrap_or(body.len());
        let kind = &body[..name_end];
        if matches!(kind, "verbatim" | "raw") && body[name_end..].trim().is_empty() {
            let expected = format!("end{kind}");
            let mut from = cursor;
            let mut found = None;
            while let Some(next) = text[from..].find("{%") {
                let start = from + next;
                let end = start + 2 + text[start + 2..].find("%}")?;
                if text[start + 2..end].trim_matches(['-', '~', ' ', '\t', '\r', '\n']) == expected
                {
                    found = Some(end + 2);
                    break;
                }
                from = start + 2;
            }
            cursor = found.unwrap_or(text.len());
            continue;
        }
        if !["extends", "include", "embed", "use"].contains(&kind) {
            continue;
        }
        let rest = body[name_end..].trim_start();
        let Some(quote) = rest
            .as_bytes()
            .first()
            .copied()
            .filter(|quote| matches!(quote, b'\'' | b'"'))
        else {
            continue;
        };
        let Some(length) = rest[1..].find(quote as char) else {
            continue;
        };
        let value = &rest[1..1 + length];
        if !rest[2 + length..].trim().is_empty() || !valid_name(value) {
            continue;
        }
        let start = rest.as_ptr() as usize - text.as_ptr() as usize + 1;
        let finish = start + value.len();
        if start <= at && at < finish {
            return Some(value.into());
        }
    }
    None
}
fn current(pending: &Pending, doc: &Document) -> Result<Project, Error> {
    if doc.path != pending.doc.path
        || doc.language != pending.doc.language
        || doc.version != pending.doc.version
        || doc.text != pending.doc.text
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
fn optional(project: &Project, path: &str) -> Result<Option<String>, Error> {
    match project.read_file(path) {
        Ok(text) => Ok(Some(text)),
        Err(Error::NotFound(_)) => Ok(None),
        Err(error) => Err(error),
    }
}
fn config(project: &Project) -> Result<Vec<(String, Option<String>)>, Error> {
    if !host::setting_string("twigNamespaces")
        .unwrap_or_default()
        .trim()
        .is_empty()
    {
        return Err(
            "Extra Twig namespaces/loader paths are outside conventional template creation.".into(),
        );
    }
    let paths = project.find_files("config/**/twig.*", 33)?;
    if paths.len() > 32
        || paths.iter().any(|path| {
            !path.ends_with("/config/packages/twig.yaml")
                && !path.ends_with("/config/packages/twig.yml")
        })
    {
        return Err(
            "Layered or non-YAML Twig loader configuration is outside safe template creation."
                .into(),
        );
    }
    let mut snapshot = Vec::new();
    for path in [
        "config/packages/twig.yaml",
        "config/packages/twig.yml",
        "config/packages/twig.php",
        "config/packages/twig.xml",
    ] {
        let text = optional(project, path)?;
        if let Some(text) = &text {
            if text.len() > 64 << 10 || !path.ends_with("yaml") && !path.ends_with("yml") {
                return Err(
                    "Can't create the template: the Twig configuration is not YAML or is over 64 KiB. Create the file by hand.".into(),
                );
            }
            let keys = index::yaml_keys(text);
            if keys.iter().any(|key| {
                key.key == "imports"
                    || key.key == "paths"
                    || key.key == "<<"
                    || key.value.starts_with(['*', '&'])
                    || (key.key == "twig" && !key.value.is_empty())
            }) {
                return Err(
                    "This project's Twig configuration (imports or custom paths) isn't supported. Only templates/ is."
                        .into(),
                );
            }
            for key in keys
                .iter()
                .filter(|key| key.key == "default_path" && key.parents == ["twig"])
            {
                let raw = key.value.trim();
                let value =
                    if let Some(quote) = raw.chars().next().filter(|ch| matches!(ch, '\'' | '"')) {
                        raw.strip_prefix(quote)
                            .and_then(|value| value.strip_suffix(quote))
                            .filter(|value| !value.contains(quote))
                            .ok_or("Couldn't read twig.default_path. Check its quotes in the Twig config.")?
                    } else {
                        raw
                    };
                if value != "%kernel.project_dir%/templates"
                    && value != format!("{}/templates", project.root()?)
                {
                    return Err(
                        "Only the conventional project templates/ root is supported.".into(),
                    );
                }
            }
        }
        snapshot.push((path.into(), text));
    }
    Ok(snapshot)
}
pub(crate) fn vacant(project: &Project, target: &str, root: &str) -> Result<(), Error> {
    if project.exists(target)? {
        return Err("Target already exists; nothing is overwritten.".into());
    }
    match project.read_file(target) {
        Ok(_) => {
            return Err("Target has an unsaved buffer; nothing is overwritten.".into());
        }
        Err(Error::NotFound(_)) => {}
        Err(error) => return Err(error),
    }
    let relative = target
        .strip_prefix(&format!("{root}/"))
        .ok_or(Error::Stale)?;
    let mut parent = root.to_string();
    let parts: Vec<_> = relative.split('/').collect();
    for part in &parts[..parts.len() - 1] {
        parent.push('/');
        parent.push_str(part);
        match project.stat(&parent) {
            Ok(stat) if stat.kind == FileKind::Directory => {}
            Err(Error::NotFound(_)) => {}
            Ok(_) => {
                return Err(
                    "Target parent is not a compatible project directory.".into(),
                );
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

// Explicit project-owned PSR-4 source only. This is not a PHP type resolver.
fn php_namespace(doc: &Document) -> Option<String> {
    if doc.language != "php" || !doc.path.ends_with(".php") || doc.text.len() > 32 << 10 {
        return None;
    }
    let model = crate::php_text::class_model(&doc.text)?;
    let (namespace, name) = model.fqcn.rsplit_once('\\')?;
    if !namespace.split('\\').all(php_ident) || !php_ident(name) {
        return None;
    }
    // Reject multiple namespace/class declarations rather than guess lexical ownership.
    let masked = crate::php_text::mask(&doc.text);
    if masked
        .split_whitespace()
        .filter(|s| *s == "namespace")
        .count()
        != 1
        || masked
            .split_whitespace()
            .filter(|s| matches!(*s, "class" | "interface" | "trait" | "enum"))
            .count()
            != 1
    {
        return None;
    }
    Some(namespace.into())
}
pub(crate) fn php_ident(value: &str) -> bool {
    let mut chars = value.bytes();
    value.len() <= 64
        && chars
            .next()
            .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && chars.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}
pub(crate) fn valid_class_name(name:&str)->bool {
    !(!php_ident(name)
        || name.eq_ignore_ascii_case("class")
        || [
            "__halt_compiler",
            "__class__",
            "__dir__",
            "__file__",
            "__function__",
            "__line__",
            "__method__",
            "__namespace__",
            "__trait__",
            "self",
            "parent",
            "static",
            "trait",
            "interface",
            "enum",
            "match",
            "readonly",
            "true",
            "false",
            "null",
            "string",
            "int",
            "float",
            "bool",
            "array",
            "object",
            "callable",
            "iterable",
            "void",
            "never",
            "mixed",
            "resource",
            "namespace",
            "function",
            "abstract",
            "final",
            "extends",
            "implements",
            "new",
            "clone",
            "const",
            "echo",
            "print",
            "return",
            "throw",
            "try",
            "catch",
            "finally",
            "if",
            "else",
            "elseif",
            "while",
            "for",
            "foreach",
            "switch",
            "case",
            "default",
            "break",
            "continue",
            "public",
            "protected",
            "private",
            "use",
            "instanceof",
            "global",
            "unset",
            "isset",
            "empty",
            "eval",
            "exit",
            "die",
            "yield",
            "list",
            "fn",
            "include",
            "include_once",
            "require",
            "require_once",
            "declare",
            "do",
            "goto",
            "as",
            "insteadof",
            "var",
            "and",
            "or",
            "xor",
            "enddeclare",
            "endfor",
            "endforeach",
            "endif",
            "endswitch",
            "endwhile",
        ]
        .iter()
        .any(|word| name.eq_ignore_ascii_case(word)))
}

fn php_mapping(project: &Project, pending: &Pending) -> Result<String, Error> {
    let composer = project.read_file("composer.json")?;
    let mappings = crate::psr4::mappings(&composer)?;
    let model = crate::php_text::class_model(&pending.doc.text).ok_or(Error::Stale)?;
    if php_namespace(&pending.doc).as_deref()!=Some(&pending.name)
        || pending.doc.path.rsplit('/').next()!=Some(format!("{}.php",model.fqcn.rsplit('\\').next().ok_or(Error::Stale)?).as_str()) {
        return Err("Source class, filename and namespace must agree.".into());
    }
    let mut matches = 0usize;
    for mapping in mappings {
        let namespace = mapping.namespace.as_str();
        let directory = mapping.directory.as_str();
        let base = format!("{}/{directory}/",pending.root);
        let Some(relative) = pending.doc.path.strip_prefix(&base) else { continue };
        let parent = relative.rsplit_once('/').map(|(p,_)|p).unwrap_or("");
        let expected = if parent.is_empty() { namespace.to_owned() } else { format!("{namespace}\\{}",parent.replace('/',"\\")) };
        if expected==pending.name { matches+=1; }
    }
    if matches!=1 { return Err("This file's folder must match exactly one PSR-4 entry in composer.json.".into()); }
    Ok(composer)
}

fn resolve_php(
    owner: &mut Main,
    pending: &Pending,
    doc: &Document,
    action: FileAction,
    project: &Project,
) -> Result<FileAction, Error> {
    let mapping = php_mapping(project, pending)?;
    let Some(kind) = host::pick(
        "New PHP class next to this file",
        &[
            PickItem::new("service", "Plain PHP service class"),
            PickItem::new("compiler", "CompilerPassInterface scaffold"),
        ],
    )?
    else {
        return Err(Error::Cancelled);
    };
    if !matches!(kind.as_str(), "service" | "compiler") {
        return Err(Error::Stale);
    }
    let Some(name) = host::input(
        "New PHP class name",
        Some(if kind == "compiler" {
            "AppCompilerPass"
        } else {
            "NewService"
        }),
    )?
    else {
        return Err(Error::Cancelled);
    };
    if !valid_class_name(&name)
    {
        return Err("Use a plain non-keyword PHP class name.".into());
    }
    let parent = doc.path.rsplit_once('/').ok_or(Error::Stale)?.0;
    let target = format!("{parent}/{name}.php");
    let fqcn = format!("{}\\{name}", pending.name);
    if owner
        .indexes
        .get(&pending.root)
        .is_some_and(|i| i.classes.keys().any(|key| key.eq_ignore_ascii_case(&fqcn)))
    {
        return Err("A class with this name already exists. Choose another name.".into());
    }
    let eol = intentions::eol_of(&doc.text);
    let body = if kind == "compiler" {
        format!(
            "final class {name} implements \\Symfony\\Component\\DependencyInjection\\Compiler\\CompilerPassInterface\n{{\n    public function process(\\Symfony\\Component\\DependencyInjection\\ContainerBuilder $container): void\n    {{\n    }}\n}}\n"
        )
    } else {
        format!("final class {name}\n{{\n}}\n")
    };
    let content = format!(
        "<?php\n\ndeclare(strict_types=1);\n\nnamespace {};\n\n{body}",
        pending.name
    )
    .replace('\n', eol);
    current(pending, doc)?;
    vacant(project, &target, &pending.root)?;
    let preview = format!(
        "Creates {target} (class {fqcn}) and opens it.\nIt isn't registered as a service and the Kernel isn't edited.\nDelete the file to undo.\n\n{content}"
    );
    if !ui_kit::prompts::confirm_exact(&format!("Create class {name}?"), &preview)? {
        return Err(Error::Cancelled);
    }
    current(pending, doc)?;
    if php_mapping(project, pending)? != mapping {
        return Err(Error::Stale);
    }
    vacant(project, &target, &pending.root)?;
    Ok(FileAction {
        title: action.title,
        kind: Some("refactor.rewrite".into()),
        operations: vec![FileOperation::Create {
            path: target.clone(),
            text: content,
            overwrite: false,
        }],
        open: Some(target),
        ..FileAction::default()
    })
}
impl Main {
    fn creation_actions(&mut self, doc: &Document, selection: Range) -> Vec<FileAction> {
        let Some(at) = text::offset(&doc.text, selection.start) else {
            return Vec::new();
        };
        let php = php_namespace(doc).is_some();
        let Some(name) = php_namespace(doc).or_else(|| literal(doc, at)) else {
            return Vec::new();
        };
        let Some(root) = host::project_of(&doc.path).and_then(|project| project.root().ok()) else {
            return Vec::new();
        };
        if !normalized(&root)
            || !normalized(&doc.path)
            || !under(&doc.path, &root)
            || !self.indexes.get(&root).is_some_and(|index|index.settings_current())
            || (!php && self.indexes[&root].templates.contains_key(&name))
        {
            return Vec::new();
        }
        if self.template_creation.pending.len() >= 8 {
            self.template_creation.pending.clear();
        }
        let Some(next) = self.template_creation.next.checked_add(1) else {
            return Vec::new();
        };
        self.template_creation.next = next;
        let data = format!("twig-create:{next}:{}", doc.version);
        self.template_creation.pending.insert(
            data.clone(),
            Pending {
                root,
                doc: doc.clone(),
                name: name.clone(),
                php,
            },
        );
        vec![FileAction {
            title: if php {
                "Create sibling PHP service or compiler-pass class…".into()
            } else {
                format!("Create missing Twig template {name}…")
            },
            kind: Some("quickfix".into()),
            data: Some(data),
            ..FileAction::default()
        }]
    }
    fn resolve_creation_action(
        &mut self,
        doc: &Document,
        action: FileAction,
    ) -> Result<FileAction, Error> {
        let pending = self
            .template_creation
            .pending
            .remove(action.data.as_deref().ok_or(Error::Stale)?)
            .ok_or(Error::Stale)?;
        if !self.indexes.get(&pending.root).is_some_and(|index|index.settings_current()) {return Err(Error::Stale);}
        let project = current(&pending, doc)?;
        if pending.php {
            return resolve_php(self, &pending, doc, action, &project);
        }
        let snapshot = config(&project)?;
        let target = format!("{}/templates/{}", pending.root, pending.name);
        if !normalized(&target) {
            return Err(Error::Stale);
        }
        vacant(&project, &target, &pending.root)?;
        let modes = [
            PickItem::new("empty", "Empty Twig template"),
            PickItem::new("body", "Template with an empty body block"),
        ];
        let Some(mode) = host::pick("Initial content for the missing Twig template", &modes)?
        else {
            return Err(Error::Cancelled);
        };
        let content = match mode.as_str() {
            "empty" => String::new(),
            "body" => crate::make_commands::PLAIN_BASE_TEMPLATE
                .replace('\n', intentions::eol_of(&doc.text)),
            _ => return Err("Unknown template content selection.".into()),
        };
        current(&pending, doc)?;
        if config(&project)? != snapshot {
            return Err(Error::Stale);
        }
        vacant(&project, &target, &pending.root)?;
        let preview = format!(
            "Creates {target} and opens it.\nThe current file is unchanged.\nDelete the new file to undo.\n\n{}",
            if content.is_empty() {
                "(empty file)"
            } else {
                &content
            }
        );
        if !ui_kit::prompts::confirm_exact(&format!("Create template {}?", pending.name), &preview)? {
            return Err(Error::Cancelled);
        }
        current(&pending, doc)?;
        if config(&project)? != snapshot {
            return Err(Error::Stale);
        }
        vacant(&project, &target, &pending.root)?;
        Ok(FileAction {
            title: action.title,
            kind: Some("quickfix".into()),
            operations: vec![FileOperation::Create {
                path: target.clone(),
                text: content,
                overwrite: false,
            }],
            open: Some(target),
            ..FileAction::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    fn document(text: &str) -> Document {
        Document {
            path: "/project/templates/caller.html.twig".into(),
            language: "twig".into(),
            version: 7,
            text: text.into(),
        }
    }
    fn setup(text: &str) -> (MockHost, Main, Document) {
        let host = MockHost::install().with_capabilities(&["project.read", "editor.read"]);
        host.add_file("symfony.lock", "{}");
        host.add_file("templates/caller.html.twig", text);
        host.open_editor("/project/templates/caller.html.twig", "twig", text, &[]);
        let mut plugin = Main::default();
        plugin.indexes.insert("/project".into(), Index::default());
        (host, plugin, document(text))
    }
    fn action(plugin: &mut Main, doc: &Document) -> FileAction {
        let at = doc.text.find("new.html.twig").unwrap() + 1;
        let position = text::position(&doc.text, at);
        let actions = plugin.file_actions(doc, Range::new(position, position));
        assert_eq!(actions.len(), 1);
        let action = actions.into_iter().next().unwrap();
        assert!(action.operations.is_empty() && action.edits.is_empty());
        action
    }
    fn answers(host: &MockHost, mode: &str, confirm: bool) {
        host.answer_next_prompt(Some(&[mode]));
        host.answer_next_prompt(confirm.then_some(&[]));
    }
    #[test]
    fn reviewed_literal_create_uses_current_buffer_and_one_non_overwrite_operation() {
        let source = "😀 prefix\r\n{% include 'blog/new.html.twig' %}\r\n";
        let (host, mut plugin, doc) = setup(source);
        host.add_file(&doc.path, "older disk text");
        let pending = action(&mut plugin, &doc);
        answers(&host, "body", true);
        let resolved = plugin.resolve_file_action(&doc, pending).unwrap();
        let [
            FileOperation::Create {
                path,
                text,
                overwrite: false,
            },
        ] = resolved.operations.as_slice()
        else {
            panic!("atomic create")
        };
        assert_eq!(path, "/project/templates/blog/new.html.twig");
        assert_eq!(text, "{% block body %}\r\n{% endblock %}\r\n");
        assert_eq!(resolved.open.as_deref(), Some(path.as_str()));
        assert!(resolved.edits.is_empty());
        assert_eq!(host.editor_text(), source);
        assert!(matches!(
            host.invocation().project().unwrap().read_file(path),
            Err(Error::NotFound(_))
        ));
        assert!(host.process_runs().is_empty() && host.requests().is_empty());
    }
    #[test]
    fn only_static_structural_literal_references_survive_suppressed_and_dynamic_contexts() {
        for kind in ["extends", "include", "embed", "use"] {
            let source = format!("{{% {kind} 'new.html.twig' %}}");
            assert_eq!(
                literal(&document(&source), source.find("new").unwrap() + 1).as_deref(),
                Some("new.html.twig")
            );
        }
        for source in [
            "{# {% include 'new.html.twig' %} #}",
            "{% verbatim %}{% include 'new.html.twig' %}{% endverbatim %}",
            "{% raw %}{% include 'new.html.twig' %}{% endraw %}",
            "{{ \"{% include 'new.html.twig' %}\" }}",
            "{% set x = \"{% include 'new.html.twig' %}\" %}",
            "{% set x = 1 # %} {% include 'new.html.twig' %}\n%}",
            "{% extends 'new.html.twig' ~ name %}",
            "{% include ['new.html.twig'] %}",
            "{% include 'new.html.twig' with {a: 1} %}",
            "{% include '@Ns/new.html.twig' %}",
            "{% include '../new.html.twig' %}",
            "{% include '/new.html.twig' %}",
            "{% include 'dir\\new.html.twig' %}",
        ] {
            assert!(
                literal(&document(source), source.find("new.html.twig").unwrap() + 1).is_none(),
                "{source}"
            );
        }
        let source = "{% include variable %} {% include 'new.html.twig' %}";
        assert!(literal(&document(source), source.find("new.html.twig").unwrap() + 1).is_some());
        let source = "{{ 'escaped \\😀' }} {% include 'new.html.twig' %}";
        for end in source
            .char_indices()
            .map(|(at, _)| at)
            .chain(std::iter::once(source.len()))
        {
            let prefix = &source[..end];
            literal(&document(prefix), 0);
        }
        assert!(literal(&document(source), source.find("new.html.twig").unwrap() + 1).is_some());
    }
    #[test]
    fn cancellation_unknown_mode_and_stale_invoking_source_return_no_create() {
        let source = "{% include 'new.html.twig' %}";
        for mode in [None, Some("empty")] {
            let (host, mut plugin, doc) = setup(source);
            let pending = action(&mut plugin, &doc);
            if let Some(mode) = mode {
                host.answer_next_prompt(Some(&[mode]));
            }
            assert!(matches!(
                plugin.resolve_file_action(&doc, pending),
                Err(Error::Cancelled)
            ));
            assert_eq!(host.editor_text(), source);
        }
        let (host, mut plugin, doc) = setup(source);
        let pending = action(&mut plugin, &doc);
        answers(&host, "forged", true);
        assert!(plugin.resolve_file_action(&doc, pending).is_err());
        let (_host, mut plugin, doc) = setup(source);
        let pending = action(&mut plugin, &doc);
        let mut newer = doc.clone();newer.version += 1;
        assert!(matches!(plugin.resolve_file_action(&newer,pending),Err(Error::Stale)));
        let (host,mut plugin,doc)=setup(source);let pending=action(&mut plugin,&doc);
        host.type_in_editor("changed");assert!(matches!(plugin.resolve_file_action(&doc,pending),Err(Error::Stale)));
        let (host,mut plugin,doc)=setup(source);let pending=action(&mut plugin,&doc);
        host.set_project_root(Some("/other"));assert!(matches!(plugin.resolve_file_action(&doc,pending),Err(Error::Stale)));
    }
    #[test]
    fn twig_creation_resolution_tokens_are_one_use_even_before_apply_or_after_cancel() {
        for accepted in [false,true] {
            let (host,mut plugin,doc)=setup("{% include 'new.html.twig' %}");let pending=action(&mut plugin,&doc);let replay=pending.clone();answers(&host,"empty",accepted);
            let result=plugin.resolve_file_action(&doc,pending);if accepted {assert!(result.is_ok());}else{assert!(matches!(result,Err(Error::Cancelled)));}
            assert!(matches!(plugin.resolve_file_action(&doc,replay),Err(Error::Stale)));
            assert!(matches!(host.invocation().project().unwrap().read_file("templates/new.html.twig"),Err(Error::NotFound(_))));
        }
    }
    #[test]
    fn existing_dirty_targets_and_non_directory_parents_are_preserved() {
        let source = "{% include 'blog/new.html.twig' %}";
        let (host, mut plugin, doc) = setup(source);
        let pending = action(&mut plugin, &doc);
        host.add_file("templates/blog/new.html.twig", "existing");
        assert!(plugin.resolve_file_action(&doc, pending).is_err());
        let (host, mut plugin, doc) = setup(source);
        let pending = action(&mut plugin, &doc);
        host.open_editor(
            "/project/templates/blog/new.html.twig",
            "twig",
            "dirty target",
            &[],
        );
        assert!(plugin.resolve_file_action(&doc, pending).is_err());
        assert_eq!(host.editor_text(), "dirty target");
        let (host, mut plugin, doc) = setup(source);
        let pending = action(&mut plugin, &doc);
        host.add_file("templates/blog", "file blocks parent");
        assert!(plugin.resolve_file_action(&doc, pending).is_err());
    }
    #[test]
    fn loader_configuration_and_namespace_settings_refuse_guessed_roots() {
        for (config_path, config_text) in [
            (
                "config/packages/twig.yaml",
                "twig:\n  default_path: '%kernel.project_dir%/other'\n",
            ),
            (
                "config/packages/twig.yaml",
                "twig:\n  paths:\n    other: ~\n",
            ),
            (
                "config/packages/twig.yaml",
                "twig: { default_path: other }\n",
            ),
            (
                "config/packages/twig.yaml",
                "imports:\n  - { resource: other.yaml }\n",
            ),
            ("config/packages/dev/twig.yaml", "twig: ~"),
            ("config/packages/twig.php", "<?php return []; "),
            ("config/packages/twig.yaml", "twig:\n  <<: *loader\n"),
            (
                "config/packages/twig.yaml",
                "twig:\n  default_path: '*loader'\n",
            ),
        ] {
            let (host, mut plugin, doc) = setup("{% include 'new.html.twig' %}");
            host.add_file(config_path, config_text);
            let pending = action(&mut plugin, &doc);
            assert!(
                plugin.resolve_file_action(&doc, pending).is_err(),
                "{config_path}: {config_text}"
            );
        }
        let (host, mut plugin, doc) = setup("{% include 'new.html.twig' %}");
        host.set_setting("twigNamespaces", "\"=other\"");
        let pending = action(&mut plugin, &doc);
        assert!(plugin.resolve_file_action(&doc, pending).is_err());
        let (host, mut plugin, doc) = setup("{% include 'new.html.twig' %}");
        host.add_file(
            "config/packages/twig.yaml",
            "twig:\n  default_path: '%kernel.project_dir%/templates'\n  strict_variables: true\n",
        );
        let pending = action(&mut plugin, &doc);
        answers(&host, "empty", true);
        let resolved = plugin.resolve_file_action(&doc, pending).unwrap();
        assert!(
            matches!(resolved.operations.as_slice(),[FileOperation::Create{text,..}] if text.is_empty())
        );
    }
    #[test]
    fn cache_membership_names_and_document_bounds_are_explicit() {
        let (host, mut plugin, doc) = setup("{% include 'new.html.twig' %}");
        plugin
            .indexes
            .get_mut("/project")
            .unwrap()
            .templates
            .insert(
                "new.html.twig".into(),
                Entry {
                    detail: None,
                    path: "/project/templates/new.html.twig".into(),
                    at: Position::default(),
                },
            );
        let position = text::position(&doc.text, doc.text.find("new").unwrap() + 1);
        assert!(
            plugin
                .file_actions(&doc, Range::new(position, position))
                .is_empty()
        );
        plugin.indexes.clear();
        assert!(
            plugin
                .file_actions(&doc, Range::new(position, position))
                .is_empty()
        );
        assert!(
            literal(
                &document(&format!(
                    "{}{{% include 'new.html.twig' %}}",
                    "x".repeat(MAX_TEXT)
                )),
                0
            )
            .is_none()
        );
        for name in [
            "@N/new.twig",
            "C:/new.twig",
            "a/../new.twig",
            "a//new.twig",
            "https://x/new.twig",
            "a\0.twig",
        ] {
            assert!(!valid_name(name));
        }
        assert!(host.process_runs().is_empty());
    }
    #[test]
    fn pending_root_and_configuration_snapshot_are_checked_independently() {
        let (host, mut plugin, doc) = setup("{% include 'new.html.twig' %}");
        let pending = action(&mut plugin, &doc);
        host.set_project_root(Some("/other"));
        assert!(plugin.resolve_file_action(&doc, pending).is_err());
        let (host, _, _doc) = setup("{% include 'new.html.twig' %}");
        let project = host.invocation().project().unwrap();
        let before = config(&project).unwrap();
        host.add_file(
            "config/packages/twig.yaml",
            "twig:\n  default_path: '%kernel.project_dir%/templates'\n",
        );
        assert_ne!(
            config(&project).unwrap(),
            before,
            "even supported changed loader source invalidates a pending snapshot"
        );
        let _denied = host.with_capabilities(&[]);
        assert!(config(&project).is_err());
    }
    fn php_setup() -> (MockHost, Main, Document) {
        let text = "<?php\nnamespace App\\Service;\nclass Existing {}\n";
        let host = MockHost::install().with_capabilities(&["project.read", "editor.read"]);
        host.add_file("symfony.lock", "{}");
        host.add_file(
            "composer.json",
            r#"{"autoload":{"psr-4":{"App\\":"src/"}}}"#,
        );
        host.add_file("src/Service/Existing.php", text);
        host.open_editor("/project/src/Service/Existing.php", "php", text, &[]);
        let mut owner = Main::default();
        owner.indexes.insert("/project".into(), Index::default());
        (
            host,
            owner,
            Document {
                path: "/project/src/Service/Existing.php".into(),
                language: "php".into(),
                version: 3,
                text: text.into(),
            },
        )
    }
    fn php_action(owner: &mut Main, doc: &Document) -> FileAction {
        let mut actions = owner.file_actions(doc, Range::default());
        assert_eq!(actions.len(), 1);
        actions.remove(0)
    }
    #[test]
    fn php_service_and_compiler_form_preview_exact_atomic_creation() {
        for kind in ["service", "compiler"] {
            let (host, mut owner, doc) = php_setup();
            let action = php_action(&mut owner, &doc);
            host.answer_next_prompt(Some(&[kind]));
            host.answer_next_prompt(Some(&["OwnedService"]));
            host.answer_next_prompt(Some(&[]));
            let out = owner.resolve_file_action(&doc, action.clone()).unwrap();
            let [
                FileOperation::Create {
                    path,
                    text,
                    overwrite: false,
                },
            ] = out.operations.as_slice()
            else {
                panic!("atomic nonoverwrite")
            };
            assert_eq!(path, "/project/src/Service/OwnedService.php");
            assert!(text.contains("namespace App\\Service;"));
            assert_eq!(text.contains("CompilerPassInterface"), kind == "compiler");
            assert!(out.edits.is_empty());
            assert_eq!(out.open.as_deref(), Some(path.as_str()));
            assert!(matches!(
                owner.resolve_file_action(&doc, action),
                Err(Error::Stale)
            ));
            assert!(matches!(
                host.invocation().project().unwrap().read_file(path),
                Err(Error::NotFound(_))
            ));
        }
    }
    #[test]
    fn php_creation_accepts_one_matching_custom_production_mapping_among_unrelated_strings() {
        for kind in ["service","compiler"] {
            let (host,mut owner,mut doc)=php_setup();
            doc.path="/project/packages/domain/lib/Service/Existing.php".into();
            doc.text="<?php\nnamespace Acme\\Domain\\Service;\nclass Existing {}\n".into();
            host.add_file(&doc.path,&doc.text);
            host.open_editor(&doc.path,"php",&doc.text,&[]);
            host.add_file("composer.json",r#"{"autoload":{"psr-4":{"App\\":"src/","Acme\\Domain\\":"packages/domain/lib/","Tools\\":"tools/"}}}"#);
            let action=php_action(&mut owner,&doc);
            host.answer_next_prompt(Some(&[kind]));host.answer_next_prompt(Some(&["Created"]));host.answer_next_prompt(Some(&[]));
            let resolved=owner.resolve_file_action(&doc,action).unwrap();
            assert!(matches!(resolved.operations.as_slice(),[FileOperation::Create {path,text,overwrite:false}] if path=="/project/packages/domain/lib/Service/Created.php" && text.contains("namespace Acme\\Domain\\Service;")));
        }
    }
    #[test]
    fn php_mapping_refuses_ambiguous_arrays_duplicate_keys_traversal_dev_and_filename_mismatch() {
        for composer in [
            r#"{"autoload":{"psr-4":{"App\\":"src/","App\\Service\\":"src/Service/"}}}"#,
            r#"{"autoload":{"psr-4":{"App\\":["src/"]}}}"#,
            r#"{"autoload":{"psr-4":{"App\\":"src/","App\\":"src/"}}}"#,
            r#"{"autoload":{"psr-4":{"App\\":"src/../src/"}}}"#,
            r#"{"autoload":{"psr-4":{"App\\":"/project/src/"}}}"#,
            r#"{"autoload-dev":{"psr-4":{"App\\":"src/"}}}"#,
            r#"{"autoload":{"psr-4":{"":"src/"}}}"#,
            r#"{"autoload":{"psr-4":{"App\\":"src//"}}}"#,
        ] {
            let (host,mut owner,doc)=php_setup();host.add_file("composer.json",composer);
            let action=php_action(&mut owner,&doc);
            assert!(owner.resolve_file_action(&doc,action).is_err(),"{composer}");
        }
        let (host,mut owner,mut doc)=php_setup();
        doc.path="/project/src/Service/Wrong.php".into();host.add_file(&doc.path,&doc.text);
        let action=php_action(&mut owner,&doc);
        assert!(owner.resolve_file_action(&doc,action).is_err());
    }

    #[test]
    fn php_creation_denies_mapping_collision_stale_source_and_reserved_names() {
        let (host, mut owner, doc) = php_setup();
        let action = php_action(&mut owner, &doc);
        host.add_file(
            "composer.json",
            r#"{"autoload":{"psr-4":{"Other\\":"src/"}}}"#,
        );
        assert!(owner.resolve_file_action(&doc, action).is_err());
        let (host, mut owner, doc) = php_setup();
        let action = php_action(&mut owner, &doc);
        host.answer_next_prompt(Some(&["service"]));
        host.answer_next_prompt(Some(&["Existing"]));
        assert!(owner.resolve_file_action(&doc, action).is_err());
        let (host, mut owner, doc) = php_setup();
        let action = php_action(&mut owner, &doc);
        host.answer_next_prompt(Some(&["service"]));
        host.answer_next_prompt(Some(&["String"]));
        assert!(owner.resolve_file_action(&doc, action).is_err());
        let (_host, mut owner, doc) = php_setup();
        let action = php_action(&mut owner, &doc);
        let mut changed = doc.clone();
        changed.version += 1;
        assert!(matches!(
            owner.resolve_file_action(&changed, action),
            Err(Error::Stale)
        ));
    }
    #[test]
    fn php_creation_cancel_and_unknown_namespace_do_not_return_effects() {
        let (host, mut owner, doc) = php_setup();
        let action = php_action(&mut owner, &doc);
        host.answer_next_prompt(None);
        assert!(matches!(
            owner.resolve_file_action(&doc, action),
            Err(Error::Cancelled)
        ));
        let mut invalid = doc.clone();
        invalid
            .text
            .push_str("\nnamespace Other;\nclass Other {}\n");
        assert!(owner.file_actions(&invalid, Range::default()).is_empty());
        let (host, mut owner, doc) = php_setup();
        let action = php_action(&mut owner, &doc);
        host.answer_next_prompt(Some(&["compiler"]));
        host.answer_next_prompt(Some(&["OwnedPass"]));
        host.answer_next_prompt(None);
        assert!(matches!(
            owner.resolve_file_action(&doc, action),
            Err(Error::Cancelled)
        ));
        let _denied = host.with_capabilities(&[]);
        assert!(owner.file_actions(&doc, Range::default()).is_empty());
    }
}

impl FileActions for Main {
    fn file_actions(&mut self, doc: &Document, selection: Range) -> Vec<FileAction> {
        let mut actions = crate::config_file_actions::actions(self,doc);
        actions.extend(crate::composer_compiler::actions(self,doc));
        actions.extend(crate::service_file_actions::actions(self,doc,selection));
        actions.extend(crate::service_edit_actions::actions(self,doc,selection));
        actions.extend(self.creation_actions(doc,selection));
        actions
    }
    fn resolve_file_action(&mut self,doc:&Document,action:FileAction)->Result<FileAction,Error> {
        if action.data.as_deref().is_some_and(|d|d.starts_with("composer-compiler:")) {
            crate::composer_compiler::resolve(self,doc,action)
        } else if action.data.as_deref().is_some_and(|d|d.starts_with("service-edit:")) {
            crate::service_edit_actions::resolve(self,doc,action)
        } else if action.data.as_deref().is_some_and(|d|d.starts_with("config-create:")) {
            crate::config_file_actions::resolve(self,doc,action)
        } else if action.data.as_deref().is_some_and(|d|d.starts_with("service-register:")) {
            crate::service_file_actions::resolve(self,doc,action)
        } else { self.resolve_creation_action(doc,action) }
    }
}
