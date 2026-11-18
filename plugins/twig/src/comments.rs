//! Versioned current-editor comment toggle for configured Twig syntax.
use crate::*;
pub fn toggle(ctx: &Invocation) -> Result<Option<String>, Error> {
    let _scope = delimiters::Syntax::scope().ok_or_else(|| {
        Error::Invalid("Correct the Twig delimiter settings before toggling comments.".into())
    })?;
    let syntax = delimiters::Syntax::current();
    let editor = ctx
        .editor()
        .ok_or_else(|| Error::Invalid("Open a Twig template first.".into()))?;
    if editor.language() != "twig" || !editor.path().ends_with(".twig") {
        return Err(Error::Invalid(
            "The current editor is not a Twig template.".into(),
        ));
    }
    let denied = |e: Error| ui_kit::denied::map(e, "Toggle Comment", "editor.read", "Twig");
    let current = editor.document().map_err(denied)?;
    if current.text.len() > 128 << 10 {
        return Err(Error::Invalid(
            "Twig comment toggle is limited to 128 KiB documents.".into(),
        ));
    }
    let selections = editor.selections().map_err(denied)?;
    if selections.len() != 1 {
        return Err(Error::Invalid(
            "Select one Twig range or place a single caret on a line.".into(),
        ));
    }
    let selection = selections[0];
    let mut start = offset(&current.text, selection.start).ok_or(Error::Stale)?;
    let mut end = offset(&current.text, selection.end).ok_or(Error::Stale)?;
    if start > end {
        std::mem::swap(&mut start, &mut end);
    }
    let parsed = lex(&current.text);
    let (replacement, inner_start, inner_len) = if let Some(comment) = parsed
        .comments
        .iter()
        .find(|r| r.start <= start && end <= r.end)
    {
        let raw = &current.text[comment.clone()];
        let body = raw
            .strip_prefix(&syntax.values[4])
            .and_then(|s| s.strip_suffix(&syntax.values[5]))
            .ok_or_else(|| {
                Error::Invalid("The Twig comment is incomplete; close it before toggling.".into())
            })?;
        let body = body
            .strip_prefix(' ')
            .and_then(|s| s.strip_suffix(' '))
            .unwrap_or(body);
        start = comment.start;
        end = comment.end;
        (body.to_owned(), 0, body.len())
    } else {
        if start == end {
            start = current.text[..start].rfind('\n').map_or(0, |i| i + 1);
            end = current.text[end..]
                .find('\n')
                .map_or(current.text.len(), |i| end + i);
            if current.text.as_bytes().get(end.wrapping_sub(1)) == Some(&b'\r') {
                end -= 1;
            }
        }
        if parsed.tags.windows(2).any(|pair| {
            matches!(pair[0].name.as_str(), "raw" | "verbatim")
                && pair[1].name == format!("end{}", pair[0].name)
                && start < pair[1].range.start
                && end > pair[0].range.end
        }) {
            return Err(Error::Invalid("Twig comments are literal text inside raw/verbatim; select a range outside that body.".into()));
        }
        let body = &current.text[start..end];
        if body.contains(&syntax.values[5]) {
            return Err(Error::Invalid("The selection contains a Twig comment closer; choose one comment or a range without nested comments.".into()));
        }
        (
            format!("{} {} {}", syntax.values[4], body, syntax.values[5]),
            syntax.values[4].len() + 1,
            body.len(),
        )
    };
    let mut after = current.text.clone();
    after.replace_range(start..end, &replacement);
    editor.apply_edits(
        current.version,
        &[openidea_plugin::TextEdit {
            range: EditorRange::new(position(&current.text, start), position(&current.text, end)),
            text: replacement,
        }],
    )
    .map_err(|e| ui_kit::denied::map(e, "Toggle Comment", "editor.write", "Twig"))?;
    editor.set_selections(&[EditorRange::new(
        position(&after, start + inner_start),
        position(&after, start + inner_start + inner_len),
    )])?;
    Ok(None)
}
#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    #[test]
    fn custom_comment_toggle_roundtrips_current_unicode_line_and_crlf() {
        let host = MockHost::install().with_capabilities(&["editor.read", "editor.write"]);
        host.set_setting("commentOpen", r#""<#""#);
        host.set_setting("commentClose", "\"#>\"");
        host.open_editor("/p/a.twig", "twig", "😀 text\r\nnext", &[(0, 3, 0, 3)]);
        toggle(&host.invocation()).unwrap();
        assert_eq!(host.editor_text(), "<# 😀 text #>\r\nnext");
        toggle(&host.invocation()).unwrap();
        assert_eq!(host.editor_text(), "😀 text\r\nnext");
    }
    #[test]
    fn denied_editor_access_names_the_permission() {
        let host = MockHost::install().with_capabilities(&[]);
        host.open_editor("/p/a.twig", "twig", "text", &[(0, 1, 0, 1)]);
        let err = toggle(&host.invocation()).unwrap_err();
        assert!(matches!(err, Error::Invalid(m) if m.contains("editor.read permission") && m.contains("Plugins › Twig")));
        let host = host.with_capabilities(&["editor.read"]);
        let err = toggle(&host.invocation()).unwrap_err();
        assert!(matches!(err, Error::Invalid(m) if m.contains("editor.write permission")));
    }
    #[test]
    fn incomplete_nested_and_raw_comment_contexts_are_refused_without_edits() {
        let host = MockHost::install().with_capabilities(&["editor.read", "editor.write"]);
        for source in [
            "{# incomplete",
            "text {# nested #}",
            "{% verbatim %}literal{% endverbatim %}",
        ] {
            host.open_editor("/p/a.twig", "twig", source, &[(0, 1, 0, 1)]);
            assert!(toggle(&host.invocation()).is_err(), "{source}");
            assert_eq!(host.editor_text(), source);
        }
    }
}
