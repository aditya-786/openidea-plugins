//! The built-in Log editor's semantic overlay uses the existing Ideolog
//! event/field/match rules. It performs no project discovery or file reads.
use crate::{Ideolog, MAX_DOCUMENT_BYTES, TIME_GAP, highlights_limited};
use openidea_plugin::semantic_tokens::{Document, SemanticToken, SemanticTokens, TokenKind};
use openidea_plugin::syntax::{Highlight, Token};
use openidea_plugin::{Position, Range, host};
use std::time::{Duration, Instant};

pub(super) const DECORATION_SET: &str = "log-source-hints";

const MAX_LINE_BYTES: usize = 4096;
const MAX_TOKENS: usize = 65_536;
const BUDGET: Duration = Duration::from_millis(15);

impl SemanticTokens for Ideolog {
    fn semantic_tokens(&mut self, doc: &Document) -> Vec<SemanticToken> {
        self.semantic_tokens_within(doc, BUDGET)
    }
}

impl Ideolog {
    fn semantic_tokens_within(&mut self, doc: &Document, budget: Duration) -> Vec<SemanticToken> {
        // A new semantic request (including unsaved edits) withdraws explicit
        // snapshot hints before publishing current semantic results.
        let _ = host::clear_decorations(DECORATION_SET);
        if doc.language != "log" {
            return Vec::new();
        }
        let gap = host::setting_number(TIME_GAP).unwrap_or(0.0);
        if let Some(tokens) = self.cached_jvm_tokens(doc, gap) { return tokens; }
        let text = bounded_head(&doc.text);
        if text.is_empty() {
            return Vec::new();
        }
        let deadline = Instant::now() + budget;
        // Activation/focus/commands/settings forms compile configuration outside
        // semantic requests. A cold provider never compiles or reads storage.
        let Some((_, config)) = &self.config else { return Vec::new(); };
        let gap = host::setting_number(TIME_GAP).unwrap_or(0.0);
        convert(text, highlights_limited(config, text, gap, MAX_TOKENS, Some(deadline)))
    }
}

/// Explicit bounded snapshot only. Native decorations carry no source version,
/// so the next request/event clears them instead of leaving mapped stale hints.
pub(super) fn mark(plugin: &mut Ideolog, ctx: &openidea_plugin::Invocation) -> Result<(), openidea_plugin::Error> {
    host::clear_decorations(DECORATION_SET)?;
    // Snapshot publication requires change/lifecycle delivery, not merely an
    // activation declaration. Denied events leave the set empty.
    for kind in [openidea_plugin::EventKind::FileSaved, openidea_plugin::EventKind::FilesChanged,
        openidea_plugin::EventKind::EditorFocused, openidea_plugin::EventKind::FileClosed,
        openidea_plugin::EventKind::SettingsChanged] {
        host::subscribe(kind)?;
    }
    let editor = ctx.editor().filter(|e| e.language() == "log")
        .ok_or("Focus a built-in Log editor first.")?;
    let path = editor.path();
    let project = host::project_of(&path).ok_or("The log must belong to an open project.")?;
    let root = project.root()?;
    if root.is_empty() || !path.starts_with(&format!("{}/", root.trim_end_matches('/'))) {
        return Err(openidea_plugin::Error::Stale);
    }
    let document = editor.document()?;
    let text = bounded_head(&document.text);
    let config = plugin.config();
    let gap = host::setting_number(TIME_GAP).unwrap_or(0.0);
    let tokens = highlights_limited(&config, text, gap, MAX_TOKENS, Some(Instant::now() + BUDGET));
    let admitted = tokens.into_iter().filter_map(|mut token| {
        match token.kind {
            Highlight::Keyword | Highlight::Tag => {},
            // Preserve a distinct conversion category for gaps.
            Highlight::Constant => token.kind = Highlight::Number,
            _ => return None,
        }
        Some(token)
    }).collect();
    let doc = Document { path: path.clone(), language: "log".into(), text: document.text.clone(), version: document.version };
    let mapped = plugin.reviewed_jvm_tokens(&doc, gap).unwrap_or_else(||convert(text, admitted));
    let items: Vec<_> = mapped.into_iter().map(|mapped| {
        let style = match mapped.kind {
            TokenKind::Keyword => "error",
            TokenKind::EnumMember => "warning",
            _ => "highlight",
        };
        openidea_plugin::Decoration { range: mapped.range, style: style.into(),
            hover: Some("Ideolog source snapshot; re-run Mark Current Log Source to refresh.".into()), after: None }
    }).collect();
    let current = editor.document()?;
    if current.version != document.version || current.text != document.text
        || host::project_of(&path).and_then(|p| p.root().ok()).as_deref() != Some(root.as_str()) {
        return Err(openidea_plugin::Error::Stale);
    }
    host::set_decorations(&path, DECORATION_SET, &items)?;
    plugin.marked = Some((path, root));
    Ok(())
}

/// Stop before an overlong line rather than feeding its partial text to
/// format detection. The cooperative deadline bounds the work past that.
pub(crate) fn bounded_head(text: &str) -> &str {
    // Bound the newline search itself: split_inclusive on the whole input
    // otherwise scans an arbitrarily long line before its length can be refused.
    let prefix_end=crate::log::floor_char_boundary(text,text.len().min(MAX_DOCUMENT_BYTES));
    let prefix=&text[..prefix_end];
    let mut end = 0;
    for line in prefix.split_inclusive('\n') {
        let complete=line.ends_with('\n') || prefix_end==text.len();
        if !complete || line.len() > MAX_LINE_BYTES || end + line.len() > MAX_DOCUMENT_BYTES {
            break;
        }
        end += line.len();
    }
    &text[..end]
}

fn kind(highlight: Highlight) -> Option<TokenKind> {
    Some(match highlight {
        Highlight::Keyword => TokenKind::Keyword,
        Highlight::Tag | Highlight::Constant => TokenKind::EnumMember,
        Highlight::String => TokenKind::String,
        Highlight::Number => TokenKind::Number,
        Highlight::Comment => TokenKind::Comment,
        Highlight::Function => TokenKind::Function,
        Highlight::Property => TokenKind::Property,
        _ => return None,
    })
}

/// Walk UTF-8 once in sorted range order, counting UTF-16 columns. Calling
/// text::position from the document start per token would be quadratic.
fn convert(text: &str, mut tokens: Vec<Token>) -> Vec<SemanticToken> {
    tokens.sort_by_key(|token| (token.range.start, token.range.end));
    let mut out = Vec::new();
    let (mut cursor, mut position) = (0, Position::new(0, 0));
    for token in tokens.into_iter().take(MAX_TOKENS) {
        let Some(kind) = kind(token.kind) else {
            continue;
        };
        let span = token.range;
        if span.start < cursor
            || span.start >= span.end
            || span.end > text.len()
            || !text.is_char_boundary(span.start)
            || !text.is_char_boundary(span.end)
        {
            continue;
        }
        advance(text, &mut cursor, &mut position, span.start);
        let start = position;
        advance(text, &mut cursor, &mut position, span.end);
        if start.line == position.line && start.character < position.character {
            out.push(SemanticToken::new(Range::new(start, position), kind));
        }
    }
    out
}

fn advance(text: &str, cursor: &mut usize, position: &mut Position, to: usize) {
    for ch in text[*cursor..to].chars() {
        if ch == '\n' {
            position.line += 1;
            position.character = 0;
        } else {
            position.character += ch.len_utf16() as u32;
        }
    }
    *cursor = to;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Config, highlights_with_gaps};
    use openidea_plugin::{Plugin, mock::MockHost, text};

    fn doc(language: &str, text: &str) -> Document {
        Document { path: "/outside/log.txt".into(), language: language.into(), version: 1, text: text.into() }
    }
    fn strings<'a>(source: &'a str, tokens: &[SemanticToken]) -> Vec<(&'a str, TokenKind)> {
        tokens
            .iter()
            .map(|token| {
                let start = text::offset(source, token.range.start).unwrap();
                let end = text::offset(source, token.range.end).unwrap();
                assert!(start < end);
                assert_eq!(token.range.start.line, token.range.end.line);
                assert!(!source[start..end].contains(['\n', '\r']));
                (&source[start..end], token.kind)
            })
            .collect()
    }

    #[test]
    fn explicit_native_snapshot_is_bounded_owned_and_withdrawn_on_next_request() {
        let host = MockHost::install().with_capabilities(&["editor.read", "project.read", "events"]);
        host.set_setting("timeGapSeconds", "5");
        let source = "2024-05-01|WARN|😀main|warning\r\n2024-05-01 10:00:00 ERROR first\r\n";
        host.open_editor("/project/app.log", "log", source, &[(0, 0, 0, 0)]);
        let mut plugin = Ideolog::default();
        mark(&mut plugin, &host.invocation()).unwrap();
        let items = host.decorations();
        let marks = items.get(&(DECORATION_SET.into(), "/project/app.log".into())).unwrap();
        assert!(!marks.is_empty());
        assert!(marks.iter().all(|d| matches!(d.style.as_str(), "error" | "warning" | "highlight")));
        for item in marks {
            let a = text::offset(source, item.range.start).unwrap();
            let b = text::offset(source, item.range.end).unwrap();
            assert!(!source[a..b].contains(['\n', '\r']));
        }
        plugin.semantic_tokens(&doc("log", source));
        assert!(host.decorations().is_empty(), "new semantic request invalidates the snapshot");
        host.open_editor("/outside/app.log", "log", source, &[(0, 0, 0, 0)]);
        assert!(mark(&mut plugin, &host.invocation()).is_err());
        assert!(host.decorations().is_empty());
    }

    #[test]
    fn built_in_editor_uses_same_level_timestamp_and_stack_trace_rules() {
        let _host = MockHost::install().with_capabilities(&[]);
        let source = "2024-05-01 10:00:00,123 [   1] ERROR - #a.B - 😀 boom\r\n\tat demo.Foo.run(Foo.java:42)\r\n2024-05-01 10:00:01,123 [   2] DEBUG - #a.B - quiet\r\n";
        let mut plugin = Ideolog::default();
        plugin.config();
        let tokens = plugin.semantic_tokens(&doc("log", source));
        let values = strings(source, &tokens);
        assert!(values.contains(&("2024-05-01 10:00:00,123 ", TokenKind::Number)));
        assert!(values.contains(&("ERROR", TokenKind::Keyword)));
        assert!(values.iter().any(|(value, kind)| value.contains("😀 boom") && *kind == TokenKind::Keyword));
        assert!(values.contains(&("\tat demo.Foo.run(Foo.java:42)", TokenKind::Keyword)));
        assert!(tokens.iter().all(|t| t.range.start.line < 2), "DEBUG retains its lexical styling");
        assert!(tokens.windows(2).all(|pair| pair[0].range.end <= pair[1].range.start));
        let mut plugin = Ideolog::default();
        assert!(plugin.semantic_tokens(&doc("ideolog", source)).is_empty(), "custom language keeps syntax-provider ownership");
        assert!(plugin.semantic_tokens(&doc("rust", source)).is_empty());
    }

    #[test]
    fn registered_semantic_gap_uses_instant_not_wall_clock_offset() {
        let h=MockHost::install();h.set_setting("timeGapSeconds","10");let mut p=Ideolog::default();p.config();
        let same="2024-05-01T08:00:00Z INFO first\n2024-05-01T10:00:00+02:00 INFO same instant\n";
        assert!(!p.semantic_tokens(&doc("log",same)).iter().any(|t|t.kind==TokenKind::EnumMember),"timezone spelling alone must not manufacture gap");
        let later="2024-05-01T10:00:00+02:00 INFO first\n2024-05-01T08:00:30Z INFO thirty seconds later\n";
        assert!(p.semantic_tokens(&doc("log",later)).iter().any(|t|t.range.start.line==1 && t.kind==TokenKind::EnumMember),"actual positive instant gap remains marked");
    }
    #[test]
    fn utf16_columns_survive_non_bmp_text_before_and_inside_captures() {
        let config = Config::default();
        let source = "2024-05-01|ERROR|😀logger|é😀 message\r\n\t😀 trace\r\n";
        let lexical = highlights_with_gaps(&config, source, 0.0);
        let tokens = convert(source, lexical.clone());
        assert_eq!(tokens.len(), lexical.len());
        for (token, original) in tokens.iter().zip(lexical) {
            assert_eq!(token.range.start, text::position(source, original.range.start));
            assert_eq!(token.range.end, text::position(source, original.range.end));
        }
        assert!(strings(source, &tokens).iter().any(|(value, _)| *value == "é😀 message"));
    }

    #[test]
    fn custom_formats_match_and_field_rules_and_time_gap_setting_are_reused() {
        let host = MockHost::install();
        let xml = r#"<LogParsingPattern pattern="^(\d\d:\d\d:\d\d) (ALARM) (.*)$" linePattern="^\d" timeId="0" severityId="1" categoryId="-1" uuid="custom"/>
<LogHighlightingPattern pattern="ALARM" captureGroup="1" action="HIGHLIGHT_LINE" fg="-39836"/>
<LogHighlightingPattern pattern="user=" captureGroup="2" action="HIGHLIGHT_FIELD" fg="-65281"/>
<LogHighlightingPattern pattern="user=(\w+)" action="HIGHLIGHT_MATCH" fg="-16776961"/>"#;
        host.set_setting("customPatterns", &format!("{xml:?}"));
        host.set_setting("timeGapSeconds", "5.0");
        let source = "10:00:00 ALARM user=alice\n10:00:10 ALARM user=bob\n";
        let mut plugin = Ideolog::default();
        plugin.config();
        let tokens = plugin.semantic_tokens(&doc("log", source));
        let values = strings(source, &tokens);
        assert!(values.contains(&("ALARM", TokenKind::Keyword)));
        assert!(values.contains(&("alice", TokenKind::Function)));
        assert!(values.contains(&("user=", TokenKind::Property)));
        assert!(values.contains(&("10:00:10", TokenKind::EnumMember)));
        assert!(tokens.windows(2).all(|pair| pair[0].range.end <= pair[1].range.start));
    }

    #[test]
    fn semantic_requests_use_warmed_configuration_without_recompiling_external_changes() {
        let host=MockHost::install();
        let source="2024-05-01|ERROR|main|message\n";
        let mut plugin=Ideolog::default();
        assert!(plugin.semantic_tokens(&doc("log",source)).is_empty(),"cold provider waits for activation/focus warming");
        plugin.config();
        let before=plugin.config.as_ref().unwrap().1.clone();
        host.set_setting("customPatterns", &format!("{:?}","<LogHighlightingPattern pattern=\"message\" action=\"HIGHLIGHT_MATCH\" fg=\"-16776961\"/>"));
        assert!(!plugin.semantic_tokens(&doc("log",source)).is_empty());
        assert!(std::rc::Rc::ptr_eq(&before,&plugin.config.as_ref().unwrap().1));
        plugin.config();
        assert!(!std::rc::Rc::ptr_eq(&before,&plugin.config.as_ref().unwrap().1),"explicit out-of-provider refresh adopts settings");
        assert!(host.process_runs().is_empty() && host.requests().is_empty());
    }
    #[test]
    fn checked_semantic_preparation_stops_at_detection_and_grouping_boundaries() {
        let config=Config::default();
        let source="2024-05-01|ERROR|main|message\n".repeat(300);
        let mut checks=0;
        let empty=crate::highlights_checked(&config,&source,0.0,4096,||{checks+=1;true});
        assert!(empty.is_empty()); assert_eq!(checks,1);
        let mut checks=0;
        let empty=crate::highlights_checked(&config,&source,0.0,4096,||{checks+=1;checks==400});
        assert!(empty.is_empty()); assert_eq!(checks,400,"cancel during physical-line grouping before token parsing");
        let mut checks=0;
        let empty=crate::highlights_checked(&config,&source,0.0,0,||{checks+=1;false});
        assert!(empty.is_empty()); assert_eq!(checks,0,"zero token budget skips all preparation");
    }
    #[test]
    fn bounded_newline_search_never_admits_a_truncated_line_or_utf8_scalar() {
        let line=format!("INFO {}\n","x".repeat(4000));
        let first=line.repeat(MAX_DOCUMENT_BYTES/line.len());
        let source=format!("{first}{line}INFO tail\n");
        assert!(first.len()<=MAX_DOCUMENT_BYTES && first.len()+line.len()>MAX_DOCUMENT_BYTES);
        assert_eq!(bounded_head(&source),first);
        let first=format!("{}\n","x".repeat(127)).repeat(MAX_DOCUMENT_BYTES/128-1);
        let source=format!("{first}{}😀tail\n","a".repeat(127));
        assert!(!source.is_char_boundary(MAX_DOCUMENT_BYTES));
        assert_eq!(bounded_head(&source),first);
        let huge=format!("INFO short\n{}\nINFO tail\n","😀".repeat(MAX_DOCUMENT_BYTES));
        assert_eq!(bounded_head(&huge),"INFO short\n");
        assert_eq!(bounded_head("INFO short"),"INFO short","real end-of-document line remains complete");
    }
    #[test]
    fn request_limits_skip_oversized_lines_and_preserve_complete_utf8_lines() {
        assert!(bounded_head(&"😀".repeat(MAX_LINE_BYTES)).is_empty());
        let text = "INFO hello\r\n".repeat(10_000);
        assert_eq!(bounded_head(&text).lines().count(), 10_000, "line count is not capped");
        let line = format!("INFO {}\n", "😀".repeat(500));
        let bytes = line.repeat(400);
        assert!(bounded_head(&bytes).len() <= MAX_DOCUMENT_BYTES);
        assert!(bounded_head(&bytes).ends_with('\n'));
        let source = "INFO x\n";
        let config = Config::default();
        assert!(highlights_limited(&config, source, 0.0, MAX_TOKENS, Some(Instant::now() - Duration::from_secs(1))).is_empty());
        let many = (0..MAX_TOKENS + 100).map(|_| Token { range: 0..1, kind: Highlight::Keyword }).collect();
        assert_eq!(convert("x", many).len(), 1, "overlapping duplicate tokens rejected");
        let source = "x".repeat(MAX_TOKENS + 100);
        let distinct = (0..source.len()).map(|n| Token { range: n..n + 1, kind: Highlight::Keyword }).collect();
        assert_eq!(convert(&source, distinct).len(), MAX_TOKENS);
        let malformed = vec![
            Token { range: 1..2, kind: Highlight::Keyword },
            Token { range: 0..0, kind: Highlight::Keyword },
            Token { range: 0..99, kind: Highlight::Keyword },
        ];
        assert!(convert("😀", malformed).is_empty());
    }

    #[test]
    fn highlights_cover_line_2000() {
        let _host = MockHost::install();
        let source: String = (0..2500).map(|i| format!("2024-05-01 10:00:00,123 [   1] ERROR - #a.B - failure {i}\n")).collect();
        let text = bounded_head(&source);
        assert_eq!(text.len(), source.len(), "no line or byte cap below the document limit");
        let tokens = convert(text, highlights_limited(&Config::default(), text, 0.0, MAX_TOKENS, None));
        assert!(tokens.iter().any(|t| t.range.start.line == 2000), "colouring reaches line 2000");
        let mut plugin = Ideolog::default();
        plugin.config();
        let tokens = plugin.semantic_tokens_within(&doc("log", &source), Duration::from_secs(30));
        assert!(tokens.iter().any(|t| t.range.start.line == 2000), "provider reaches line 2000");
    }

    #[test]
    fn an_expired_deadline_returns_partial_tokens_without_panicking() {
        let line = "2024-05-01 10:00:00,123 [   1] ERROR - #a.B - failure\n";
        let source = line.repeat(2500);
        let config = Config::default();
        let mut total = 0;
        let full = crate::highlights_checked(&config, &source, 0.0, MAX_TOKENS, || {
            total += 1;
            false
        });
        let mut checks = 0;
        let partial = crate::highlights_checked(&config, &source, 0.0, MAX_TOKENS, || {
            checks += 1;
            checks > total - 100
        });
        assert!(!partial.is_empty() && partial.len() < full.len());
    }

    #[test]
    fn a_huge_log_returns_within_the_deadline_without_panicking() {
        let _host = MockHost::install();
        let line = "2024-05-01 10:00:00,123 [   1] ERROR - #a.B - something went wrong\n";
        let source = line.repeat(MAX_DOCUMENT_BYTES / line.len());
        let mut plugin = Ideolog::default();
        plugin.config();
        let started = Instant::now();
        let tokens = plugin.semantic_tokens(&doc("log", &source));
        assert!(tokens.len() <= MAX_TOKENS);
        assert!(started.elapsed() < Duration::from_millis(500), "provider stays under the host hard limit");
    }

    #[test]
    fn custom_pattern_change_applies_without_refocus() {
        let host = MockHost::install();
        let source = "2024-05-01|ERROR|main|user=alice\n";
        let mut plugin = Ideolog::default();
        plugin.config();
        let before = plugin.semantic_tokens(&doc("log", source));
        assert!(!strings(source, &before).contains(&("alice", TokenKind::Function)));
        host.set_setting("customPatterns", &format!("{:?}", r#"<LogHighlightingPattern pattern="user=(\w+)" action="HIGHLIGHT_MATCH" fg="-16776961"/>"#));
        plugin.handle_events(&[openidea_plugin::Event::Other { kind: "settings-changed".into(), data: String::new() }], &host.invocation());
        let after = plugin.semantic_tokens(&doc("log", source));
        assert!(strings(source, &after).contains(&("alice", TokenKind::Function)));
    }

    #[test]
    fn unrelated_file_change_keeps_marks() {
        let host = MockHost::install().with_capabilities(&["editor.read", "project.read", "events"]);
        let source = "2024-05-01 10:00:00 ERROR first\n";
        host.open_editor("/project/app.log", "log", source, &[(0, 0, 0, 0)]);
        let mut plugin = Ideolog::default();
        mark(&mut plugin, &host.invocation()).unwrap();
        let key = (DECORATION_SET.to_owned(), "/project/app.log".to_owned());
        let changed = |path: &str| openidea_plugin::Event::FilesChanged(openidea_plugin::FilesChanged { modified: vec![path.into()], ..Default::default() });
        plugin.handle_events(&[changed("/project/build/output.class"), openidea_plugin::Event::FileSaved("/project/Other.java".into())], &host.invocation());
        assert!(host.decorations().contains_key(&key));
        plugin.handle_events(&[changed("/project/app.log")], &host.invocation());
        assert!(!host.decorations().contains_key(&key));
        mark(&mut plugin, &host.invocation()).unwrap();
        plugin.handle_events(&[openidea_plugin::Event::VcsChanged("/project".into())], &host.invocation());
        assert!(!host.decorations().contains_key(&key));
    }

    #[test]
    fn one_huge_event_with_a_custom_pattern_stays_within_the_deadline() {
        let host = MockHost::install();
        host.set_setting("customPatterns", &format!("{:?}", r#"<LogHighlightingPattern pattern="(a+)+b" action="HIGHLIGHT_MATCH" fg="-16776961"/>"#));
        let header = "2024-05-01 10:00:00,123 [   1] ERROR - #a.B - boom\n";
        let source = format!("{header}{}", "    at some.pkg.Klass.method(Klass.java:1)\n".repeat(MAX_DOCUMENT_BYTES / 44 - 2));
        let mut plugin = Ideolog::default();
        plugin.config();
        let started = Instant::now();
        let tokens = plugin.semantic_tokens(&doc("log", &source));
        assert!(started.elapsed() < Duration::from_millis(500));
        assert!(tokens.len() <= MAX_TOKENS);
        assert!(tokens.windows(2).all(|pair| pair[0].range.end <= pair[1].range.start));
    }

    #[test]
    fn capped_event_cuts_on_a_line_boundary() {
        let event = "x\n".repeat(crate::MAX_HIGHLIGHTED_EVENT_BYTES);
        let capped = crate::capped_event(&event);
        assert!(capped.len() <= crate::MAX_HIGHLIGHTED_EVENT_BYTES && capped.ends_with('\n'));
        assert_eq!(crate::capped_event("short"), "short");
    }

    #[test]
    fn unnamed_events_and_other_files_keep_marks() {
        let host = MockHost::install().with_capabilities(&["editor.read", "project.read", "events"]);
        host.open_editor("/project/app.log", "log", "2024-05-01 10:00:00 ERROR first\n", &[(0, 0, 0, 0)]);
        let mut plugin = Ideolog::default();
        mark(&mut plugin, &host.invocation()).unwrap();
        let key = (DECORATION_SET.to_owned(), "/project/app.log".to_owned());
        let other = |kind: &str, data: &str| openidea_plugin::Event::Other { kind: kind.into(), data: data.into() };
        plugin.handle_events(&[other("file-closed", "\"/project/Other.java\""), other("settings-changed", ""), other("unknown", ""), openidea_plugin::Event::Scheduled], &host.invocation());
        assert!(host.decorations().contains_key(&key));
        plugin.handle_events(&[other("file-closed", "\"/project/app.log\"")], &host.invocation());
        assert!(!host.decorations().contains_key(&key));
    }
}
