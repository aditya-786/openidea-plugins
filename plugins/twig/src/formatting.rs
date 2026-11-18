//! Explicitly enabled, pinned official formatter; edits never touch external files.
use crate::*;
use openidea_plugin::language_features::{FormatOptions, LanguageFeatures};
const FORMATTER_PHAR_TITLE: &str = "Absolute path to official Twig-CS-Fixer 4.1.1 PHAR (exact release hash required)";
const SHA256: &str = "4d099a3be74ea41b0c8f1b2a7033c53a77e7d0dfba36725c2bbaaa19453ed2e5";
const ADAPTER: &str = include_str!("formatter.php");
/// Compare all literal data and string contents; only whitespace between Twig
/// tokens or directly around a comment body may change.
fn structure(text: &str) -> Option<Vec<Vec<String>>> {
    let parsed = lex(text);
    if !parsed.unterminated.is_empty() || parsed.tokens.len() > 16384 {
        return None;
    }
    let mut regions = vec![];
    let mut open = None;
    for (range, is_open) in &parsed.boundaries {
        if *is_open {
            if open.replace(range.start).is_some() {
                return None;
            }
        } else {
            regions.push((open.take()?..range.end, false));
        }
    }
    if open.is_some() {
        return None;
    }
    regions.extend(parsed.comments.iter().cloned().map(|range| (range, true)));
    regions.sort_by_key(|(range, _)| range.start);
    let mut previous = 0;
    let mut result = vec![];
    for (range, comment) in regions {
        if range.start < previous {
            return None;
        }
        result.push(vec!["data".into(), text[previous..range.start].into()]);
        if comment {
            result.push(vec![
                "comment".into(),
                text[range.clone()]
                    .strip_prefix("{#")?
                    .strip_suffix("#}")?
                    .trim()
                    .into(),
            ]);
        } else {
            let mut tokens = vec!["code".into()];
            tokens.extend(
                parsed
                    .tokens
                    .iter()
                    .filter(|t| range.start <= t.range.start && t.range.end <= range.end)
                    .map(|t| text[t.range.clone()].into()),
            );
            result.push(tokens);
        }
        previous = range.end;
    }
    result.push(vec!["data".into(), text[previous..].into()]);
    Some(result)
}
fn source_current(doc: &Document) -> Result<Project, Error> {
    let project = host::project_of(&doc.path).ok_or(Error::Stale)?;
    if !host::open_editors()?
        .iter()
        .any(|e| e.active && e.path == doc.path && e.language == "twig")
        || project.read_file(&doc.path)? != doc.text
    {
        return Err(Error::Stale);
    }
    Ok(project)
}
impl LanguageFeatures for Twig {
    fn document_highlights(&mut self,doc:&Document,at:openidea_plugin::Position)->Vec<openidea_plugin::language_features::Highlight>{crate::reviewed_symbols::highlights(doc,at)}
    fn code_lenses(&mut self,doc:&Document)->Vec<openidea_plugin::language_features::CodeLens>{crate::reviewed_symbols::lenses(&mut self.reviewed_symbols,doc)}
    fn signature_help(
        &mut self,
        doc: &Document,
        at: openidea_plugin::Position,
    ) -> Option<openidea_plugin::language_features::SignatureHelp> {
        crate::signatures::help(doc, at)
    }
    fn format(
        &mut self,
        doc: &Document,
        range: Option<EditorRange>,
        options: FormatOptions,
    ) -> Result<Vec<openidea_plugin::TextEdit>, Error> {
        if doc.language != "twig"
            || !doc.path.ends_with(".twig")
            || !host::setting_bool("formatterEnabled").unwrap_or(false)
        {
            return Ok(vec![]);
        }
        if range.is_some() {
            return Err(Error::Invalid("Twig-CS-Fixer formats a complete current document; range formatting is unavailable.".into()));
        }
        if doc.text.len() > 128 << 10 || !(1..=16).contains(&options.tab_size) {
            return Err(Error::Invalid(
                "Twig formatting document/options exceed their supported bounds.".into(),
            ));
        }
        let _scope = delimiters::Syntax::scope()
            .ok_or_else(|| Error::Invalid("Correct the Twig delimiter settings first.".into()))?;
        if delimiters::Syntax::current().values != delimiters::Syntax::default().values {
            return Err(Error::Invalid(
                "The pinned official formatter supports standard Twig delimiters only.".into(),
            ));
        }
        let raw = host::setting("formatterPhar").ok_or_else(|| {
            Error::Invalid(
                "Set the path to the official Twig-CS-Fixer 4.1.1 PHAR in Settings › Plugins › Twig before formatting."
                    .into(),
            )
        })?;
        if raw.len() > 8192 {
            return Err(Error::Invalid("Formatter path exceeds its bound.".into()));
        }
        let path = serde_json::from_str::<String>(&raw)
            .map_err(|_| Error::Invalid("Formatter PHAR path must be a string.".into()))?;
        if !std::path::Path::new(&path).is_absolute()
            || path.len() > 4096
            || path.chars().any(char::is_control)
            || !path.ends_with(".phar")
        {
            return Err(Error::Invalid(
                "The Twig-CS-Fixer path must be an absolute path to a .phar file.".into(),
            ));
        }
        let before = structure(&doc.text).ok_or_else(|| {
            Error::Invalid("Close incomplete Twig delimiters before formatting.".into())
        })?;
        let project = source_current(doc)?;
        let root = project.root()?;
        let command = host::Command::new("php").args([
            "-n",
            "-d",
            "memory_limit=128M",
            "-d",
            "max_execution_time=2",
            "-r",
            ADAPTER,
            "--",
            &path,
            SHA256,
        ]);
        let request = serde_json::json!({"source":doc.text}).to_string();
        let output = host::run_command(&command, None, Some(&request)).map_err(|e| match e {
            Error::Denied(_) => Error::Invalid(format!(
                "Formatting needs the process:php permission. Allow it in Settings › Plugins › Twig, and set \"{FORMATTER_PHAR_TITLE}\" there."
            )),
            other => other,
        })?;
        if output.status != Some(0) || output.stdout.len() > 1 << 20 {
            return Err(Error::Invalid("Formatting failed: approve PHP and the exact official Twig-CS-Fixer 4.1.1 PHAR; unsupported or malformed templates remain unchanged.".into()));
        }
        let response: serde_json::Value = serde_json::from_str(&output.stdout)
            .map_err(|_| Error::Invalid("Invalid formatter response.".into()))?;
        let formatted = response
            .get("result")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Invalid("Invalid formatter response.".into()))?;
        if response.get("version").and_then(|v| v.as_u64()) != Some(1)
            || response.get("source").and_then(|v| v.as_str()) != Some(doc.text.as_str())
            || formatted.len() > 256 << 10
            || structure(formatted).as_ref() != Some(&before)
        {
            return Err(Error::Invalid(
                "Formatter changed literal data or Twig tokens; no edit was returned.".into(),
            ));
        }
        let after = source_current(doc)?;
        let _fresh_syntax = delimiters::Syntax::scope().ok_or(Error::Stale)?;
        if delimiters::Syntax::current().values != delimiters::Syntax::default().values {
            return Err(Error::Stale);
        }
        if after.root()? != root
            || host::setting("formatterPhar").as_deref() != Some(raw.as_str())
            || !host::setting_bool("formatterEnabled").unwrap_or(false)
        {
            return Err(Error::Stale);
        }
        if formatted == doc.text {
            return Ok(vec![]);
        }
        Ok(vec![openidea_plugin::TextEdit {
            range: EditorRange::new(position(&doc.text, 0), position(&doc.text, doc.text.len())),
            text: formatted.into(),
        }])
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    fn doc(text: &str) -> Document {
        Document {
            path: "/project/a.twig".into(),
            language: "twig".into(),
            text: text.into(),
            version: 1,
        }
    }
    fn options() -> FormatOptions {
        FormatOptions {
            tab_size: 4,
            insert_spaces: true,
        }
    }
    #[test]
    fn formatter_returns_one_exact_current_document_edit_and_never_writes_files() {
        let host =
            MockHost::install().with_capabilities(&["project.read", "editor.read", "process:php"]);
        let source = "😀 {{foo|upper}}\r\n";
        let formatted = "😀 {{ foo|upper }}\r\n";
        host.add_file("a.twig", source);
        host.open_editor("/project/a.twig", "twig", source, &[(0, 0, 0, 0)]);
        host.set_setting("formatterEnabled", "true");
        host.set_setting("formatterPhar", r#""/tools/twig-cs-fixer.phar""#);
        host.on_process("php", move |args, stdin| {
            assert!(args.iter().any(|a| a == SHA256));
            assert!(args.iter().any(|a| a.contains("fixFile")));
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(stdin.unwrap()).unwrap()["source"],
                source
            );
            Ok(openidea_plugin::ProcessOutput {
                status: Some(0),
                stdout: serde_json::json!({"version":1,"source":source,"result":formatted})
                    .to_string(),
                stderr: String::new(),
            })
        });
        let edits = Twig::default()
            .format(&doc(source), None, options())
            .unwrap();
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].text, formatted);
        assert_eq!(edits[0].range.end, position(source, source.len()));
        assert_eq!(host.editor_text(), source);
    }
    #[test]
    fn formatting_denied_names_permission() {
        let host = MockHost::install().with_capabilities(&["project.read", "editor.read"]);
        let source = "{{foo}}";
        host.add_file("a.twig", source);
        host.open_editor("/project/a.twig", "twig", source, &[(0, 0, 0, 0)]);
        host.set_setting("formatterEnabled", "true");
        host.set_setting("formatterPhar", r#""/tools/twig-cs-fixer.phar""#);
        let error = Twig::default().format(&doc(source), None, options()).unwrap_err().to_string();
        assert!(error.contains("needs the process:php permission"), "{error}");
        assert!(error.contains("Settings › Plugins › Twig"), "{error}");
        assert!(error.contains("Absolute path to official Twig-CS-Fixer 4.1.1 PHAR (exact release hash required)"), "{error}");
    }
    #[test]
    fn provider_refuses_denied_execution_and_semantically_changed_output_without_edits() {
        let host = MockHost::install().with_capabilities(&["project.read", "editor.read"]);
        let source = "{{foo}}";
        host.add_file("a.twig", source);
        host.open_editor("/project/a.twig", "twig", source, &[(0, 0, 0, 0)]);
        host.set_setting("formatterEnabled", "true");
        host.set_setting("formatterPhar", r#""/tools/twig-cs-fixer.phar""#);
        assert!(matches!(
            Twig::default().format(&doc(source), None, options()),
            Err(Error::Invalid(_))
        ));
        assert_eq!(host.editor_text(), source);
        let host = host.with_capabilities(&["project.read", "editor.read", "process:php"]);
        host.on_process("php", move |_, _| {
            Ok(openidea_plugin::ProcessOutput {
                status: Some(0),
                stdout: serde_json::json!({"version":1,"source":source,"result":"{{ evil }}"})
                    .to_string(),
                stderr: String::new(),
            })
        });
        assert!(matches!(
            Twig::default().format(&doc(source), None, options()),
            Err(Error::Invalid(_))
        ));
        assert_eq!(host.editor_text(), source);
    }
    #[test]
    fn semantic_validation_rejects_literal_html_variable_and_string_changes() {
        assert_eq!(
            structure("<b>{{foo|upper}}</b>"),
            structure("<b>{{ foo|upper }}</b>")
        );
        for changed in [
            "<b> {{ foo|upper }}</b>",
            "<b>{{ other|upper }}</b>",
            "<b>{{ 'foo'|upper }}</b>",
        ] {
            assert_ne!(structure("<b>{{foo|upper}}</b>"), structure(changed));
        }
        assert_ne!(
            structure("{{ is not empty }}"),
            structure("{{ isnotempty }}")
        );
    }
}
