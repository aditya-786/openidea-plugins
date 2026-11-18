//! Bounded physical-source injections for raw-text bodies and bound component attributes.
use openidea_plugin::{
    Range,
    injections::{Document, Injection, Injections},
    text::position,
};

impl Injections for crate::Blade {
    fn injections(&mut self, doc: &Document) -> Vec<Injection> {
        if doc.language != "blade"
            || !doc.path.ends_with(".blade.php")
            || doc.text.len() > 2 * 1024 * 1024
        {
            return Vec::new();
        }
        let declarations = declarations::for_path(&doc.path);
        bodies_with(&doc.text, &declarations)
            .into_iter()
            .map(|(body, language)| Injection {
                range: Range::new(
                    position(&doc.text, body.start),
                    position(&doc.text, body.end),
                ),
                language: language.into(),
            })
            .collect()
    }
}

#[path = "injection_declarations.rs"]
mod declarations;
#[cfg(test)]
fn bodies(text: &str) -> Vec<(std::ops::Range<usize>, &'static str)> {
    bodies_with(text, &std::collections::BTreeSet::new())
}
fn bodies_with(
    text: &str,
    declared: &std::collections::BTreeSet<String>,
) -> Vec<(std::ops::Range<usize>, &'static str)> {
    let bytes = text.as_bytes();
    let mut lexer = crate::Lexer {
        text,
        bytes,
        out: crate::Lexed::default(),
        in_tag: false,
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() && out.len() < 1000 {
        if bytes[i] == b'@' && (i == 0 || !crate::is_identifier(bytes[i - 1])) {
            let start = i + 1;
            let mut end = start;
            while bytes
                .get(end)
                .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
            {
                end += 1;
            }
            if declared.contains(&text[start..end]) {
                let mut open = end;
                while bytes.get(open).is_some_and(u8::is_ascii_whitespace) {
                    open += 1;
                }
                if bytes.get(open) == Some(&b'(') {
                    if let Ok(close) = lexer.closing_parenthesis(open) {
                        if !text[open + 1..close].trim().is_empty() {
                            out.push((open + 1..close, "php"));
                        }
                        i = close + 1;
                        continue;
                    }
                }
            }
        }
        // The existing Blade lexer owns comments, echos, PHP and verbatim regions.
        if let Some(end) = lexer.construct(i) {
            if lexer
                .out
                .directives
                .last()
                .is_some_and(|d| d.name == "php" && d.arguments.is_none())
                && lexer.find_directive(end, "endphp").is_none()
            {
                break;
            }
            i = end.max(i + 1);
            continue;
        }
        if bytes[i..].starts_with(b"<!--") {
            let Some(end) = text[i + 4..].find("-->") else {
                break;
            };
            i += 4 + end + 3;
            continue;
        }
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        let Some(tag_end) = tag_end(bytes, i) else {
            i += 1;
            continue;
        };
        let header = &text[i + 1..tag_end];
        let name_end = header.bytes().take_while(u8::is_ascii_alphabetic).count();
        let name = &header[..name_end];
        let raw = ["script", "style", "textarea", "title"]
            .iter()
            .find(|n| name.eq_ignore_ascii_case(n));
        let Some(raw) = raw else {
            component_attributes(text, i + 1, tag_end, &mut out);
            i = tag_end + 1;
            continue;
        };
        let Some((close, end)) = closing(bytes, tag_end + 1, raw) else {
            break;
        };
        let body = &text[tag_end + 1..close];
        // Attribute expressions/types/src, Blade holes/directives and nested raw tags are unknown.
        let simple = header[name_end..].trim().is_empty();
        let clean = !body.contains('@')
            && !body.contains("{{")
            && !body.contains("{!!")
            && !body.contains("<?")
            && !body.contains("<!--")
            && !body.to_ascii_lowercase().contains("<script")
            && !body.to_ascii_lowercase().contains("<style");
        if simple && clean && !body.trim().is_empty() {
            let language = if raw.eq_ignore_ascii_case("script") {
                Some("javascript")
            } else if raw.eq_ignore_ascii_case("style") {
                Some("css")
            } else {
                None
            };
            if let Some(language) = language {
                out.push((tag_end + 1..close, language));
            }
        }
        i = end;
    }
    out
}

// Blade evaluates single-colon bindings on x-/livewire: tags as PHP. Escaped
// double-colon bindings and ordinary HTML/Alpine attributes retain HTML ownership.
fn component_attributes(
    text: &str,
    start: usize,
    end: usize,
    out: &mut Vec<(std::ops::Range<usize>, &'static str)>,
) {
    let bytes = text.as_bytes();
    let mut i = start;
    while i < end && !bytes[i].is_ascii_whitespace() && bytes[i] != b'/' {
        i += 1;
    }
    let tag = &text[start..i];
    if !(tag.starts_with("x-") || tag.starts_with("livewire:")) || tag.len() < 3 {
        return;
    }
    while i < end && out.len() < 1000 {
        while i < end && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let name_start = i;
        while i < end && !bytes[i].is_ascii_whitespace() && !matches!(bytes[i], b'=' | b'/') {
            i += 1;
        }
        if i == name_start {
            break;
        }
        let name = &text[name_start..i];
        while i < end && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if bytes.get(i) != Some(&b'=') {
            continue;
        }
        i += 1;
        while i < end && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let Some(&quote @ (b'\'' | b'"')) = bytes.get(i) else {
            break;
        };
        i += 1;
        let value_start = i;
        while i < end && bytes[i] != quote {
            i += 1;
        }
        if i >= end {
            break;
        }
        let value = &text[value_start..i];
        if name.starts_with(':')
            && !name.starts_with("::")
            && name.len() > 1
            && name[1..]
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
            && !value.trim().is_empty()
            && !["{{", "{!!", "<?", "@"]
                .iter()
                .any(|hole| value.contains(hole))
        {
            out.push((value_start..i, "php"));
        }
        i += 1;
    }
}

fn tag_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut quote = None;
    for (i, &b) in bytes.iter().enumerate().skip(start + 1) {
        if let Some(q) = quote {
            if b == q {
                quote = None;
            }
        } else if b == b'\'' || b == b'"' {
            quote = Some(b);
        } else if b == b'>' {
            return Some(i);
        } else if b == b'<' {
            return None;
        }
    }
    None
}
fn closing(bytes: &[u8], start: usize, name: &str) -> Option<(usize, usize)> {
    let prefix = format!("</{name}");
    for i in start..bytes.len() {
        if bytes[i..]
            .get(..prefix.len())
            .is_some_and(|s| s.eq_ignore_ascii_case(prefix.as_bytes()))
        {
            let mut j = i + prefix.len();
            while bytes.get(j).is_some_and(u8::is_ascii_whitespace) {
                j += 1;
            }
            if bytes.get(j) == Some(&b'>') {
                return Some((i, j + 1));
            }
            // A malformed first raw-text closer must not extend ownership into later markup.
            if bytes
                .get(i + prefix.len())
                .is_some_and(|b| b.is_ascii_whitespace() || *b == b'/')
            {
                return None;
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn declared_custom_directive_arguments_are_physical_current_and_scoped() {
        use openidea_plugin::mock::MockHost;
        let host = MockHost::install().with_capabilities(&["project.read"]);
        host.add_file(
            "ide.json",
            r#"{"blade":{"directives":[{"name":"money_2"},{"name":"virtual","prefix":"<?php "}]}}"#,
        );
        let mut plugin = crate::Blade;
        let mut doc = Document {
            path: "/project/a.blade.php".into(),
            language: "blade".into(),
            version: 1,
            text: "@money_2($amount, 'é😀') @unknown($x) @virtual($x)".into(),
        };
        let found = plugin.injections(&doc);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].language, "php");
        assert_eq!(
            found[0].range,
            Range::new(
                position(&doc.text, 9),
                position(&doc.text, doc.text.find(')').unwrap())
            )
        );
        doc.text = "{{-- @money_2($x) --}} @money_2($x".into();
        assert!(plugin.injections(&doc).is_empty());
        host.add_file("ide.json", "{}");
        doc.text = "@money_2($x)".into();
        assert!(plugin.injections(&doc).is_empty());
    }
    #[test]
    fn physical_bound_component_attributes_only() {
        let source = "😀<x-ui::card :count=\"$count + 1\" ::class=\"active\" title=\"plain\"/><livewire:blog::counter :user='$user'/>";
        let found = bodies(source);
        assert_eq!(
            found
                .iter()
                .map(|(r, l)| (&source[r.clone()], *l))
                .collect::<Vec<_>>(),
            vec![("$count + 1", "php"), ("$user", "php")]
        );
        for source in [
            "<div :count=\"$x\">",
            "<x-card :x=\"{{ $x }}\"/>",
            "{{-- <x-card :x=\"$x\"/> --}}",
            "@verbatim <x-card :x=\"$x\"/> @endverbatim",
            "<x-card :x=\"$x",
            "<textarea><x-card :x=\"$x\"/></textarea>",
        ] {
            assert!(bodies(source).is_empty(), "{source}");
        }
    }
    #[test]
    fn unquoted_lt_before_a_script_does_not_hide_it() {
        let found = bodies("a < b <script>x</script>");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].1, "javascript");
    }
    #[test]
    fn closed_static_bodies_and_utf16() {
        let text = "😀\r\n<SCRIPT>let x = 'é😀';</SCRIPT>\n<style>p { color: red; }</style>";
        let ranges = bodies(text);
        assert_eq!(ranges.len(), 2);
        assert_eq!(&text[ranges[0].0.clone()], "let x = 'é😀';");
        assert_eq!(
            position(text, ranges[0].0.start),
            openidea_plugin::Position::new(1, 8)
        );
        assert_eq!(ranges[0].1, "javascript");
        assert_eq!(ranges[1].1, "css");
    }
    #[test]
    fn suppresses_template_dynamic_nested_unclosed_and_unsupported_modes() {
        for text in [
            "<script>let x = {{ $x }};</script>",
            "<style>@media x {}</style>",
            "<script src='x'>x</script>",
            "<script type='application/json'>{}</script>",
            "<script>const a = '<?php x ?>';</script>",
            "<script><script>x</script></script>",
            "<script>x",
            "<script>x</script bad><div>x</div></script>",
            "@php '<script>x</script>'",
            "<style>{{ $css }}</style>",
            "<script>{!! $x !!}</script>",
        ] {
            assert!(bodies(text).is_empty(), "{text}");
        }
    }
    #[test]
    fn skips_masked_regions_and_quoted_attributes() {
        for text in [
            "{{-- <script>x</script> --}}",
            "<!-- <script>x</script> -->",
            "@verbatim <script>x</script> @endverbatim",
            "<?php '<script>x</script>' ?>",
            "{{ '<script>x</script>' }}",
            "<div title='<script>x</script>'></div>",
            "<textarea><script>x</script></textarea>",
            "<title><style>x</style></title>",
        ] {
            assert!(bodies(text).is_empty(), "{text}");
        }
    }
    #[test]
    fn fresh_text_controls_results_and_path_guard() {
        let _host = openidea_plugin::mock::MockHost::install();
        let mut plugin = crate::Blade;
        let mut doc = Document {
            path: "/p/a.blade.php".into(),
            language: "blade".into(),
            text: "<script>x</script>".into(),
            version: 1,
        };
        assert_eq!(plugin.injections(&doc).len(), 1);
        doc.text = "<script>{{ $x }}</script>".into();
        doc.version += 1;
        assert!(plugin.injections(&doc).is_empty());
        doc.text = "<script>x</script>".into();
        doc.language = "php".into();
        assert!(plugin.injections(&doc).is_empty());
    }
    #[test]
    fn every_unicode_prefix_is_safe_and_ranges_stay_current_and_bounded() {
        let text = "😀<script>let x = '😀';</script>\n<!--<style>x</style>-->@php '<script>x</script>' @endphp";
        for end in text
            .char_indices()
            .map(|(i, _)| i)
            .chain(std::iter::once(text.len()))
        {
            for (range, _) in bodies(&text[..end]) {
                assert!(range.start < range.end && range.end <= end);
                assert!(text.is_char_boundary(range.start) && text.is_char_boundary(range.end));
            }
        }
        let range = &bodies(text)[0].0;
        assert_eq!(
            position(text, range.start),
            openidea_plugin::Position::new(0, 10)
        );
        assert_eq!(
            position(text, range.end),
            openidea_plugin::Position::new(0, 23)
        );
        assert_eq!(bodies(&"<script>x</script>".repeat(1001)).len(), 1000);
    }

    fn doc(text: &str) -> Document {
        Document {
            path: "/project/a.blade.php".into(),
            language: "blade".into(),
            version: 1,
            text: text.into(),
        }
    }

    #[test]
    fn injection_declarations_cached() {
        use openidea_plugin::mock::MockHost;
        let host = MockHost::install().with_capabilities(&["project.read"]);
        host.add_file("ide.json", r#"{"blade":{"directives":[{"name":"aaa"}]}}"#);
        let mut plugin = crate::Blade;
        assert_eq!(plugin.injections(&doc("@aaa($x)")).len(), 1);
        host.add_file("ide.json", r#"{"blade":{"directives":[{"name":"bbb"}]}}"#);
        assert_eq!(plugin.injections(&doc("@aaa($x)")).len(), 1);
        assert!(plugin.injections(&doc("@bbb($x)")).is_empty());
    }

    #[test]
    fn cache_refreshes_when_installed_json_mtime_changes() {
        use openidea_plugin::mock::MockHost;
        let host = MockHost::install().with_capabilities(&["project.read"]);
        host.add_file("ide.json", r#"{"blade":{"directives":[{"name":"aaa"}]}}"#);
        let mut plugin = crate::Blade;
        assert_eq!(plugin.injections(&doc("@aaa($x)")).len(), 1);
        host.add_file(
            "ide.json",
            r#"{"blade":{"directives":[{"name":"aaa"},{"name":"bbb"}]}}"#,
        );
        assert_eq!(plugin.injections(&doc("@bbb($x)")).len(), 1);
    }

    #[test]
    fn stray_lt_does_not_stop_injections() {
        use openidea_plugin::mock::MockHost;
        let _host = MockHost::install().with_capabilities(&["project.read"]);
        let mut plugin = crate::Blade;
        let found = plugin.injections(&doc("<p>a < b</p><script>let x=1</script>"));
        assert!(found.iter().any(|i| i.language == "javascript"));
    }

    #[test]
    fn injections_work_without_project_read() {
        use openidea_plugin::mock::MockHost;
        let _host = MockHost::install().with_capabilities(&[]);
        let mut plugin = crate::Blade;
        let found = plugin.injections(&doc("<script>let x=1</script>@x($y)"));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].language, "javascript");
    }
}
