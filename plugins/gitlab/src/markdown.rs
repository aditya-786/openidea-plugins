//! CommonMark presentation from a bounded event stream. Raw HTML is text;
//! only explicit HTTP(S) links survive, and images never create fetchable tags.
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd, html};
const MAX_INPUT: usize = 128 << 10;
pub(crate) const STYLE: &str = r#"
.gl-markdown{font:inherit;color:var(--ui-fg);line-height:1.5;overflow-wrap:anywhere}.gl-markdown>:first-child{margin-top:0}.gl-markdown pre{overflow:auto;padding:var(--ui-space-2,8px);background:var(--ui-bg-raised,var(--ui-bg));border:1px solid var(--ui-border)}.gl-markdown code{font-family:var(--ui-font-mono,monospace)}.gl-markdown blockquote{margin-inline:0;padding-inline-start:var(--ui-space-3,12px);border-inline-start:2px solid var(--ui-border);color:var(--ui-muted)}.gl-markdown table{border-collapse:collapse;max-width:100%}.gl-markdown td,.gl-markdown th{border:1px solid var(--ui-border);padding:4px 8px}.gl-markdown a{color:var(--ui-accent);text-decoration:underline}.gl-markdown img{display:none}.gl-markdown .gl-mention{color:var(--ui-accent);font-weight:600}.gl-markdown .gl-math{font-family:var(--ui-font-mono,monospace);border:1px dashed var(--ui-border);padding:0 4px}.gl-markdown .gl-math::before,.gl-markdown .gl-math-label{content:"math (source, not rendered) ";color:var(--ui-muted);font-size:.85em}.gl-markdown .gl-math-label{display:block}
"#;
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
fn link(destination: &str, base: &str) -> Option<String> {
    if destination.len() > 4096
        || destination
            .chars()
            .any(|c| c.is_control() || c.is_whitespace())
        || destination.contains('\\')
    {
        return None;
    }
    let base = url::Url::parse(base).ok()?;
    let url = base.join(destination).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    Some(url.to_string())
}
const EMOJI: &[(&str, &str)] = &[
    ("thumbsup", "\u{1F44D}"), ("+1", "\u{1F44D}"), ("thumbsdown", "\u{1F44E}"), ("-1", "\u{1F44E}"),
    ("tada", "\u{1F389}"), ("rocket", "\u{1F680}"), ("eyes", "\u{1F440}"), ("heart", "\u{2764}\u{FE0F}"),
    ("smile", "\u{1F604}"), ("grin", "\u{1F601}"), ("joy", "\u{1F602}"), ("thinking", "\u{1F914}"),
    ("warning", "\u{26A0}\u{FE0F}"), ("white_check_mark", "\u{2705}"), ("x", "\u{274C}"), ("bug", "\u{1F41B}"),
    ("fire", "\u{1F525}"), ("sparkles", "\u{2728}"), ("pray", "\u{1F64F}"), ("clap", "\u{1F44F}"),
    ("100", "\u{1F4AF}"), ("construction", "\u{1F6A7}"), ("memo", "\u{1F4DD}"), ("lock", "\u{1F512}"),
];
fn mention_len(rest: &str) -> usize {
    let mut chars = rest.char_indices();
    if !chars.next().is_some_and(|(_, c)| c.is_ascii_alphanumeric() || c == '_') {
        return 0;
    }
    let end = chars
        .find(|(_, c)| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-' | '/')))
        .map_or(rest.len(), |(at, _)| at);
    rest[..end.min(255)].trim_end_matches(['.', '-', '/']).len()
}
/// Escaped HTML for text with `@mention` spans (never links) and known `:emoji:` shortcodes.
fn decorate(text: &str) -> Option<String> {
    if !text.contains('@') && !text.contains(':') {
        return None;
    }
    let (mut out, mut changed, mut at, mut prev) = (String::new(), false, 0, None::<char>);
    while at < text.len() {
        let rest = &text[at..];
        let c = rest.chars().next()?;
        let step = c.len_utf8();
        if c == '@' && !prev.is_some_and(|p| p.is_alphanumeric() || p == '@') {
            let n = mention_len(&rest[1..]);
            if n > 0 {
                out.push_str(&format!("<span class=\"gl-mention\">@{}</span>", escape(&rest[1..=n])));
                at += 1 + n;
                prev = Some('x');
                changed = true;
                continue;
            }
        }
        if c == ':' {
            let hit = rest[1..].find(':').and_then(|end| {
                let name = &rest[1..=end];
                EMOJI.iter().find(|(n, _)| *n == name).map(|(_, e)| (end, *e))
            });
            if let Some((end, emoji)) = hit {
                out.push_str(emoji);
                at += end + 2;
                prev = Some('x');
                changed = true;
                continue;
            }
        }
        out.push_str(&escape(&rest[..step]));
        prev = Some(c);
        at += step;
    }
    changed.then_some(out)
}
pub(crate) fn fragment(text: &str, base: &str) -> String {
    let truncated = text.len() > MAX_INPUT;
    let limit = if truncated {
        text.char_indices()
            .map(|(at, _)| at)
            .take_while(|at| *at <= MAX_INPUT)
            .last()
            .unwrap_or(0)
    } else {
        text.len()
    };
    let text = &text[..limit];
    let mut links = Vec::new();
    let mut in_code = false;
    let mut is_math = false;
    let parser=Parser::new_ext(text,Options::ENABLE_MATH|Options::ENABLE_TABLES|Options::ENABLE_STRIKETHROUGH|Options::ENABLE_TASKLISTS).map(|event|match event{
        Event::Html(raw)|Event::InlineHtml(raw)=>Event::Text(raw),
        Event::Start(Tag::Image{..})=>Event::Text("[Image: ".into()),Event::End(TagEnd::Image)=>Event::Text("]".into()),
        Event::Start(Tag::Link{dest_url,..})=>{let safe=link(&dest_url,base);links.push(safe.is_some());match safe{Some(url)=>Event::Html(format!("<a href=\"{}\" target=\"_blank\" rel=\"noopener noreferrer\" data-markdown-url=\"{}\">",escape(&url),escape(&url)).into()),None=>Event::Text("".into())}},
        Event::End(TagEnd::Link)=>if links.pop()==Some(true){Event::Html("</a>".into())}else{Event::Text("".into())},
        Event::InlineMath(src)=>Event::Html(format!("<span class=\"gl-math\">{}</span>",escape(&src)).into()),
        Event::DisplayMath(src)=>Event::Html(format!("<pre class=\"gl-math\"><span class=\"gl-math-label\"></span>{}</pre>",escape(&src)).into()),
        Event::Start(Tag::CodeBlock(pulldown_cmark::CodeBlockKind::Fenced(ref info))) if info.split_whitespace().next()==Some("math")=>{in_code=true;is_math=true;Event::Html("<pre class=\"gl-math\"><span class=\"gl-math-label\"></span><code>".into())}
        Event::End(TagEnd::CodeBlock) if in_code&&is_math=>{in_code=false;is_math=false;Event::Html("</code></pre>".into())}
        Event::Start(Tag::CodeBlock(_))=>{in_code=true;event}
        Event::End(TagEnd::CodeBlock)=>{in_code=false;event}
        Event::Text(ref text) if !in_code=>match decorate(text){Some(html)=>Event::Html(html.into()),None=>event},
        other=>other,
    });
    let mut body = String::new();
    html::push_html(&mut body, parser);
    if body.len() > 768 << 10 {
        body = format!(
            "<pre>{}</pre><p>Markdown layout exceeded its display budget; bounded source shown.</p>",
            escape(text)
        );
    }
    format!(
        "<article class=\"gl-markdown\">{body}{}</article>",
        if truncated {
            "<p>Markdown truncated at 128KiB.</p>"
        } else {
            ""
        }
    )
}
pub(crate) fn links(text: &str, base: &str) -> Vec<String> {
    Parser::new(text)
        .filter_map(|event| match event {
            Event::Start(Tag::Link { dest_url, .. }) => link(&dest_url, base),
            _ => None,
        })
        .collect()
}
const LINK_SCRIPT: &str = r#"document.addEventListener('click',function(event){const anchor=event.target.closest&&event.target.closest('a[data-markdown-url]');if(!anchor)return;event.preventDefault();openidea.postMessage(JSON.stringify({type:'markdown-link',url:anchor.dataset.markdownUrl}));});"#;
pub(crate) fn document(text: &str, base: &str) -> String {
    format!(
        "<style>{STYLE}</style>{}<script>{LINK_SCRIPT}</script>",
        fragment(text, base)
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn commonmark_tables_lists_code_and_safe_relative_links_are_rendered() {
        let html = fragment(
            "# Heading\n\n**bold** _emphasis_ ~~removed~~\n\n- [x] complete\n\n|Name|Value|\n|---|---|\n|a|b|\n\n```rust\n<code>\n```\n\n[Issue](/g/p/-/issues/7)",
            "https://gitlab.com/g/p/-/merge_requests/1",
        );
        for expected in [
            "<h1>Heading</h1>",
            "<strong>bold</strong>",
            "<em>emphasis</em>",
            "<del>removed</del>",
            "<table>",
            "&lt;code&gt;",
            "href=\"https://gitlab.com/g/p/-/issues/7\"",
            "noopener noreferrer",
        ] {
            assert!(html.contains(expected), "{expected}: {html}");
        }
    }
    #[test]
    fn untrusted_html_unsafe_links_and_remote_images_cannot_create_active_elements() {
        let html = fragment(
            "<script>alert(1)</script>\n\n<img src=https://evil.test/a onerror=alert(1)>\n\n[bad](javascript:alert%281%29) [data](data:text/html,hello) [creds](https://u:p@evil.test)\n\n![tracking](https://evil.test/pixel)\n\n`<iframe>`",
            "https://gitlab.com/",
        );
        for forbidden in [
            "<script",
            "<img",
            "<iframe",
            "href=\"javascript:",
            "href=\"data:",
            "href=\"https://u:p",
        ] {
            assert!(!html.contains(forbidden), "{html}");
        }
        assert!(html.contains("[Image: tracking]"));
        assert!(html.contains("&lt;script&gt;"));
        assert!(link("java\nscript:alert(1)", "https://gitlab.com/").is_none());
    }
    #[test]
    fn mentions_and_emoji_are_decorated_but_never_linked_or_injected() {
        let html = fragment(
            "cc @alice and @grp/sub. :tada: :nope: a@b.c `@code :tada:`\n\n```\n@x :rocket:\n```\n\n@<script>:+1:",
            "https://gitlab.com/",
        );
        assert!(html.contains("<span class=\"gl-mention\">@alice</span>"), "{html}");
        assert!(html.contains("<span class=\"gl-mention\">@grp/sub</span>."), "{html}");
        assert!(html.contains("\u{1F389} :nope:"), "{html}");
        assert!(html.contains("a@b.c"), "{html}");
        assert!(html.contains("<code>@code :tada:</code>"), "{html}");
        assert!(html.contains("@x :rocket:"), "{html}");
        assert!(html.contains("@&lt;script&gt;\u{1F44D}"), "{html}");
        assert!(!html.contains("<script") && !html.contains("href=\"https://gitlab.com/alice"));
    }
    #[test]
    fn math_is_escaped_labelled_source_and_code_is_untouched() {
        let html = fragment(
            "inline $a<b>_1$ and `$code$`\n\n$$\nx</script><b>\n$$\n\n```math\n\\sum <i>\n```\n\n```\n$not$\n```",
            "https://gitlab.com/",
        );
        assert!(html.contains("<span class=\"gl-math\">a&lt;b&gt;_1</span>"), "{html}");
        assert!(html.contains("<code>$code$</code>"), "{html}");
        assert!(html.contains("x&lt;/script&gt;&lt;b&gt;"), "{html}");
        assert!(html.contains("<code>\\sum &lt;i&gt;\n</code></pre>"), "{html}");
        assert!(html.contains("$not$"), "{html}");
        assert_eq!(html.matches("gl-math-label\"></span>").count(), 2, "{html}");
        for bad in ["<script", "<i>", "<b>"] {
            assert!(!html.contains(bad), "{html}");
        }
    }
    #[test]
    fn unicode_and_large_documents_are_bounded_without_panics() {
        let html = fragment(&"😀".repeat(100000), "https://gitlab.com/");
        assert!(html.contains("truncated"));
        assert!(html.len() < 800000);
    }
}
