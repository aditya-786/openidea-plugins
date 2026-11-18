//! Source-proven console entry points. Opening a command selects captured metadata;
//! editor callbacks and resolution never execute project PHP.
use openidea_plugin::language_features::{CodeLens, GutterAction};
use openidea_plugin::providers::Document;
use openidea_plugin::{Error, Invocation, Position, Range, text};
use std::collections::BTreeMap;

const MAX_SOURCE: usize = 128 * 1024;
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Name,
    Var,
    String,
    Symbol,
}
#[derive(Clone)]
struct Token<'a> {
    value: &'a str,
    start: usize,
    kind: Kind,
    depth: usize,
}
fn tokens(source: &str) -> Option<Vec<Token<'_>>> {
    if source.len() > MAX_SOURCE
        || !source.trim_start().starts_with("<?php")
        || source.contains("?>")
    {
        return None;
    }
    let bytes = source.as_bytes();
    let mut at = 0;
    let mut depth = 0usize;
    let mut tokens = Vec::new();
    while at < bytes.len() {
        if tokens.len() >= 8192 {
            return None;
        }
        if bytes[at].is_ascii_whitespace() {
            at += 1;
            continue;
        }
        if bytes[at..].starts_with(b"<<<") {
            return None;
        }
        if bytes[at..].starts_with(b"//") || bytes[at] == b'#' && bytes.get(at + 1) != Some(&b'[') {
            at = source[at..].find('\n').map_or(bytes.len(), |n| at + n);
            continue;
        }
        if bytes[at..].starts_with(b"/*") {
            at += 2;
            at += source[at..].find("*/")? + 2;
            continue;
        }
        let start = at;
        let kind = if matches!(bytes[at], b'\'' | b'"' | b'`') {
            let quote = bytes[at];
            at = crate::php::string_end(bytes, at);
            if at > bytes.len() || at <= start + 1 || bytes.get(at - 1) != Some(&quote) {
                return None;
            }
            Kind::String
        } else if bytes[at] == b'$' {
            at += 1;
            while at < bytes.len() && (bytes[at].is_ascii_alphanumeric() || bytes[at] == b'_') {
                at += 1;
            }
            Kind::Var
        } else if bytes[at].is_ascii_alphabetic() || matches!(bytes[at], b'_' | b'\\') {
            at += 1;
            while at < bytes.len()
                && (bytes[at].is_ascii_alphanumeric() || matches!(bytes[at], b'_' | b'\\'))
            {
                at += 1;
            }
            Kind::Name
        } else {
            at += source[at..].chars().next()?.len_utf8();
            Kind::Symbol
        };
        let value = &source[start..at];
        if value == "}" {
            depth = depth.checked_sub(1)?;
        }
        tokens.push(Token {
            value,
            start,
            kind,
            depth,
        });
        if value == "{" {
            depth += 1;
            if depth > 64 {
                return None;
            }
        }
    }
    (depth == 0).then_some(tokens)
}
fn literal(token: &Token<'_>) -> Option<String> {
    if token.kind != Kind::String || token.value.starts_with('`') {
        return None;
    }
    let value = &token.value[1..token.value.len() - 1];
    (!value.contains(['\\', '$'])).then(|| value.to_owned())
}
fn command_name(signature: &str) -> Option<String> {
    let name = signature.split_whitespace().next()?;
    (!name.is_empty()
        && name.len() <= 200
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '_' | '-' | '.'))
        && !name.starts_with('-'))
    .then(|| name.to_owned())
}
#[derive(Clone, Debug, PartialEq)]
struct Candidate {
    class: String,
    command: String,
    at: Position,
}
fn candidate(source: &str) -> Option<Candidate> {
    let tokens = tokens(source)?;
    let class_positions: Vec<_> = tokens
        .iter()
        .enumerate()
        .filter(|(at, t)| {
            t.depth == 0
                && t.value == "class"
                && tokens
                    .get(at.saturating_sub(1))
                    .is_none_or(|prior| prior.value != "::")
        })
        .map(|(at, _)| at)
        .collect();
    let [class_at] = class_positions.as_slice() else {
        return None;
    };
    let class_at = *class_at;
    let class_token = tokens.get(class_at + 1)?;
    if class_token.kind != Kind::Name || tokens.get(class_at + 2)?.value != "extends" {
        return None;
    }
    if (0..class_at)
        .rev()
        .take_while(|at| tokens[*at].kind == Kind::Name)
        .any(|at| tokens[at].value == "abstract")
    {
        return None;
    }
    let parent = tokens.get(class_at + 3)?;
    if parent.kind != Kind::Name {
        return None;
    }
    let mut body = class_at + 4;
    if tokens.get(body)?.value == "implements" {
        body += 1;
        let mut expect_name = true;
        let mut count = 0;
        while tokens.get(body)?.value != "{" {
            let token = &tokens[body];
            if expect_name && token.kind != Kind::Name || !expect_name && token.value != "," {
                return None;
            }
            expect_name = !expect_name;
            body += 1;
            count += 1;
            if count > 32 {
                return None;
            }
        }
        if expect_name {
            return None;
        }
    }
    if tokens.get(body)?.value != "{" {
        return None;
    }
    let mut namespace = String::new();
    let mut namespace_seen = false;
    let mut imports = BTreeMap::<String, String>::new();
    let mut at = 0;
    while at < class_at {
        let token = &tokens[at];
        if token.depth != 0 {
            at += 1;
            continue;
        }
        if token.value == "namespace" {
            if namespace_seen
                || tokens.get(at + 1)?.kind != Kind::Name
                || tokens.get(at + 2)?.value != ";"
            {
                return None;
            }
            namespace = tokens[at + 1].value.trim_start_matches('\\').into();
            namespace_seen = true;
            at += 3;
            continue;
        }
        if token.value == "use" {
            let end = (at + 1..class_at).find(|i| tokens[*i].value == ";")?;
            let fields = &tokens[at + 1..end];
            if fields
                .first()
                .is_some_and(|t| matches!(t.value, "function" | "const"))
            {
                at = end + 1;
                continue;
            }
            if fields.len() != 1 && !(fields.len() == 3 && fields[1].value == "as") {
                return None;
            }
            if fields[0].kind != Kind::Name {
                return None;
            }
            let full = fields[0].value.trim_start_matches('\\');
            let short = if fields.len() == 3 {
                fields[2].value
            } else {
                full.rsplit('\\').next()?
            };
            if imports
                .insert(short.to_ascii_lowercase(), full.into())
                .is_some()
            {
                return None;
            }
            at = end + 1;
            continue;
        }
        at += 1;
    }
    let resolve = |name: &str| -> String {
        if name.starts_with('\\') {
            return name.trim_start_matches('\\').into();
        }
        let (first, tail) = name.split_once('\\').unwrap_or((name, ""));
        if let Some(import) = imports.get(&first.to_ascii_lowercase()) {
            return if tail.is_empty() {
                import.clone()
            } else {
                format!("{import}\\{tail}")
            };
        }
        if namespace.is_empty() {
            name.into()
        } else {
            format!("{namespace}\\{name}")
        }
    };
    let parent = resolve(parent.value);
    if ![
        "Illuminate\\Console\\Command",
        "Symfony\\Component\\Console\\Command\\Command",
    ]
    .iter()
    .any(|known| parent.eq_ignore_ascii_case(known))
    {
        return None;
    }
    let mut names = Vec::new();
    for (at, token) in tokens.iter().enumerate().skip(body + 1) {
        if token.depth != 1 || token.value != "$signature" {
            continue;
        }
        let start = (body + 1..at)
            .rfind(|i| tokens[*i].depth == 1 && matches!(tokens[*i].value, ";" | "}"))
            .map_or(body + 1, |i| i + 1);
        let modifiers = &tokens[start..at];
        if modifiers.is_empty()
            || !modifiers
                .iter()
                .any(|t| matches!(t.value, "protected" | "public"))
            || modifiers
                .iter()
                .any(|t| !matches!(t.value, "protected" | "public" | "string"))
        {
            return None;
        }
        if tokens.get(at + 1)?.value != "=" || tokens.get(at + 3)?.value != ";" {
            return None;
        }
        names.push(command_name(&literal(tokens.get(at + 2)?)?)?);
    }
    // Attributes immediately preceding this class, after the final declaration terminator.
    let attribute_start = (0..class_at)
        .rfind(|i| tokens[*i].depth == 0 && matches!(tokens[*i].value, ";" | "}"))
        .map_or(0, |i| i + 1);
    for at in attribute_start..class_at {
        if tokens[at].value != "#" || tokens.get(at + 1).is_none_or(|t| t.value != "[") {
            continue;
        }
        let attribute = tokens.get(at + 2)?;
        if !resolve(attribute.value)
            .eq_ignore_ascii_case("Symfony\\Component\\Console\\Attribute\\AsCommand")
        {
            continue;
        }
        if tokens.get(at + 3)?.value != "(" {
            return None;
        }
        names.push(attribute_name(&tokens, at + 3)?);
    }
    let command = names.first()?.clone();
    if names.iter().any(|name| *name != command) {
        return None;
    }
    let class = if namespace.is_empty() {
        class_token.value.into()
    } else {
        format!("{namespace}\\{}", class_token.value)
    };
    Some(Candidate {
        class,
        command,
        at: text::position(source, class_token.start),
    })
}
fn attribute_name(tokens: &[Token<'_>], open: usize) -> Option<String> {
    let mut cursor = open + 1;
    let mut ordinal = 0;
    let mut command = None;
    loop {
        let begin = cursor;
        let mut nested = Vec::new();
        while let Some(token) = tokens.get(cursor) {
            match token.value {
                "," | ")" if nested.is_empty() => break,
                "(" => nested.push(")"),
                "[" => nested.push("]"),
                ")" | "]" => {
                    if nested.pop() != Some(token.value) {
                        return None;
                    }
                }
                "{" | "}" | ";" => return None,
                _ => {}
            }
            cursor += 1;
        }
        let delimiter = tokens.get(cursor)?.value;
        let argument = &tokens[begin..cursor];
        let value = if argument.len() == 1 && ordinal == 0 && argument[0].kind == Kind::String {
            Some(&argument[0])
        } else if argument.first().is_some_and(|token| token.value == "name") {
            if argument.len() != 3 || argument[1].value != ":" {
                return None;
            }
            Some(&argument[2])
        } else {
            None
        };
        if let Some(value) = value {
            if command.is_some() {
                return None;
            }
            command = Some(command_name(&literal(value)?)?);
        }
        if delimiter == ")" {
            return command;
        }
        cursor += 1;
        ordinal += 1;
    }
}

fn target(doc: &Document, candidate: &Candidate) -> String {
    serde_json::json!({"path":doc.path,"hash":crate::artisan_metadata::hash(&doc.text),"class":candidate.class,"command":candidate.command}).to_string()
}
pub(crate) fn lenses(doc: &Document, owns: &impl Fn(&str) -> bool) -> Vec<CodeLens> {
    if doc.language != "php" || !owns(&doc.path) {
        return Vec::new();
    }
    let Some(candidate) = candidate(&doc.text) else {
        return Vec::new();
    };
    vec![CodeLens {
        range: Range::new(candidate.at, candidate.at),
        title: format!("Run {}…", candidate.command),
        command: "console-command-open".into(),
        argument: Some(target(doc, &candidate)),
    }]
}
pub(crate) fn gutters(doc: &Document, owns: &impl Fn(&str) -> bool) -> Vec<GutterAction> {
    lenses(doc, owns)
        .into_iter()
        .map(|lens| GutterAction {
            line: lens.range.start.line,
            icon: Some("run".into()),
            tooltip: lens.title,
            command: lens.command,
            argument: lens.argument,
        })
        .collect()
}
pub(crate) fn resolve(ctx: &Invocation, raw: &str) -> Result<String, Error> {
    let value = crate::strict_json::parse(raw, 8192)?;
    let path = value["path"].as_str().ok_or(Error::Stale)?;
    let project = ctx.project().ok_or(Error::Stale)?;
    if !crate::owns_current_path(&project, path) {
        return Err(Error::Stale);
    }
    let source = project.read_file(path)?;
    let candidate = candidate(&source).ok_or(Error::Stale)?;
    let doc = Document {
        path: path.into(),
        language: "php".into(),
        version: 0,
        text: source,
    };
    if target(&doc, &candidate) != raw || !crate::owns_current_path(&project, path) {
        return Err(Error::Stale);
    }
    Ok(candidate.command)
}
/// The console command class open in the editor, for the command run from the palette.
pub(crate) fn editor_target(ctx: &Invocation) -> Option<String> {
    let path = ctx.editor()?.path();
    let project = ctx.project()?;
    let doc = Document {
        path: path.clone(),
        language: "php".into(),
        version: 0,
        text: project.read_file(&path).ok()?,
    };
    Some(target(&doc, &candidate(&doc.text)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn source() -> String {
        "<?php\nnamespace App\\Console\\Commands;\nuse Illuminate\\Console\\Command as ArtisanCommand;\nclass SyncUsers extends ArtisanCommand {\n protected $signature = 'users:sync {user?} {--force}';\n public function handle() {}\n}\n".into()
    }
    fn doc(source: String) -> Document {
        Document {
            path: "/project/app/Console/Commands/SyncUsers.php".into(),
            language: "php".into(),
            version: 1,
            text: source,
        }
    }
    #[test]
    fn signature_alias_is_a_source_proven_entrypoint_without_execution() {
        let h = openidea_plugin::mock::MockHost::install();
        let d = doc(source());
        h.add_file(&d.path, &d.text);
        let lenses = lenses(&d, &|_| true);
        assert_eq!(lenses.len(), 1);
        assert_eq!(lenses[0].title, "Run users:sync…");
        assert_eq!(lenses[0].command, "console-command-open");
        assert_eq!(
            resolve(&h.invocation(), lenses[0].argument.as_deref().unwrap()).unwrap(),
            "users:sync"
        );
        assert_eq!(gutters(&d, &|_| true).len(), 1);
        assert!(
            candidate(&d.text.replace(
                "extends ArtisanCommand {",
                "extends ArtisanCommand implements \\Illuminate\\Contracts\\Console\\Isolatable {"
            ))
            .is_some()
        );
        assert!(h.process_runs().is_empty());
    }
    #[test]
    fn actual_as_command_alias_and_namespace_alias_are_recognized() {
        let s = "<?php\nnamespace App;\nuse Symfony\\Component\\Console\\Command as Commands;\nuse Symfony\\Component\\Console\\Attribute\\AsCommand as ConsoleName;\n#[ConsoleName(name: 'app:sync', description: 'Sync')]\nfinal class Sync extends Commands\\Command {}\n";
        assert_eq!(candidate(s).unwrap().command, "app:sync");
        assert_eq!(
            candidate(&s.replace(
                "name: 'app:sync', description: 'Sync'",
                "description: 'Sync', name: 'app:sync'"
            ))
            .unwrap()
            .command,
            "app:sync"
        );
        assert!(
            candidate(&s.replace(
                "Symfony\\Component\\Console\\Attribute\\AsCommand",
                "App\\AsCommand"
            ))
            .is_none()
        );
    }
    #[test]
    fn dynamic_nested_shadowed_and_ambiguous_sources_are_withheld() {
        for s in [
            source().replace("'users:sync {user?} {--force}'", "$dynamic"),
            source().replace("protected $signature", "private $signature"),
            source().replace("extends ArtisanCommand", "extends Unknown"),
            source().replace("class SyncUsers", "abstract class SyncUsers"),
            source() + "class Other extends ArtisanCommand {}",
            source().replace(
                "protected $signature = 'users:sync {user?} {--force}';",
                "public function helper() { $signature = 'fake:name'; }",
            ),
        ] {
            assert!(candidate(&s).is_none(), "{s}");
        }
        assert!(lenses(&doc(source()), &|_| false).is_empty());
    }
    #[test]
    fn resolution_checks_unsaved_source_and_innermost_project() {
        let h = openidea_plugin::mock::MockHost::install();
        let d = doc(source());
        h.add_file(&d.path, &d.text);
        let raw = lenses(&d, &|_| true)[0].argument.clone().unwrap();
        h.open_editor(
            &d.path,
            "php",
            &d.text.replace("users:sync", "users:changed"),
            &[],
        );
        assert!(resolve(&h.invocation(), &raw).is_err());
        h.open_editor(&d.path, "php", &d.text, &[]);
        h.add_project("/project/app/Console");
        assert!(resolve(&h.invocation(), &raw).is_err());
        assert!(h.process_runs().is_empty());
    }
}
