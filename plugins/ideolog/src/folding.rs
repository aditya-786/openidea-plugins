//! API1.4 built-in Log folding uses warmed source models only.
use crate::*;
use openidea_plugin::language_features::{FoldRange, LanguageFeatures};
use std::time::{Duration, Instant};
impl LanguageFeatures for Ideolog {
    fn selection_ranges(&mut self, doc: &Document, at: Position) -> Vec<openidea_plugin::Range> {
        if !matches!(doc.language.as_str(), "log" | "ideolog")
            || doc.text.len() > MAX_DOCUMENT_BYTES
        {
            return Vec::new();
        }
        let Some(offset) =
            text::offset(&doc.text, at).filter(|offset| text::position(&doc.text, *offset) == at)
        else {
            return Vec::new();
        };
        let Some((_, config)) = &self.config else {
            return Vec::new();
        };
        let deadline = Instant::now() + Duration::from_millis(15);
        let mut check = || {
            if Instant::now() < deadline {
                Ok(())
            } else {
                Err(())
            }
        };
        let Ok(format) = config.detect_checked(&doc.text, &mut check) else {
            return Vec::new();
        };
        let Ok(events) = log::events_checked(format, &doc.text, &mut check) else {
            return Vec::new();
        };
        let Some(event) = events
            .iter()
            .find(|event| event.offset <= offset && offset <= event.offset + event.text.len())
        else {
            return Vec::new();
        };
        let local = offset - event.offset;
        let line_start = doc.text[..offset].rfind('\n').map_or(0, |n| n + 1);
        let line_end = doc.text[offset..]
            .find('\n')
            .map_or(doc.text.len(), |n| offset + n);
        let line_end = if doc.text.as_bytes().get(line_end.wrapping_sub(1)) == Some(&b'\r') {
            line_end - 1
        } else {
            line_end
        };
        let mut ranges = vec![
            line_start..line_end,
            event.offset..event.offset + event.text.trim_end_matches(['\r', '\n']).len(),
            0..doc.text.len(),
        ];
        let parsed = Parsed::new(format, event.text);
        if let Some(token) = parsed
            .tokens
            .iter()
            .find(|token| !token.separator && token.range.start <= local && local < token.range.end)
        {
            ranges.push(
                (event.offset + token.range.start).max(line_start)
                    ..(event.offset + token.range.end).min(line_end),
            );
        }
        ranges.retain(|range| {
            range.start < range.end && range.start <= offset && offset <= range.end
        });
        ranges.sort_by_key(|range| (range.end - range.start, range.start));
        ranges.dedup();
        let mut inner: Option<std::ops::Range<usize>> = None;
        ranges
            .into_iter()
            .filter_map(|range| {
                if inner
                    .as_ref()
                    .is_some_and(|old| range.start > old.start || range.end < old.end)
                {
                    return None;
                }
                inner = Some(range.clone());
                Some(openidea_plugin::Range::new(
                    text::position(&doc.text, range.start),
                    text::position(&doc.text, range.end),
                ))
            })
            .collect()
    }

    fn folding(&mut self, doc: &Document) -> Vec<FoldRange> {
        if doc.language != "log"
            || doc.text.len() > MAX_DOCUMENT_BYTES
        {
            return Vec::new();
        }
        if let Some(folds) = self.cached_jvm_folds(doc) {
            return folds;
        }
        let Some((_, config)) = &self.config else {
            return Vec::new();
        };
        let deadline = Instant::now() + Duration::from_millis(15);
        let mut check = || {
            if Instant::now() < deadline {
                Ok(())
            } else {
                Err(())
            }
        };
        let Ok(format) = config.detect_checked(&doc.text, &mut check) else {
            return Vec::new();
        };
        let events = log::events_partial(format, &doc.text, &mut check);
        events
            .into_iter()
            .filter(|event| event.lines.len() > 1)
            .take(1024)
            .map(|event| FoldRange {
                start_line: event.lines.start,
                end_line: event.lines.end - 1,
                kind: "region".into(),
            })
            .collect()
    }
}
impl Ideolog {
    pub(crate) fn extend_log_selection(&mut self, ctx: &Invocation) -> Result<(), Error> {
        let editor = ctx
            .editor()
            .filter(|editor| matches!(editor.language().as_str(), "log" | "ideolog"))
            .ok_or("Focus a Log editor to extend its selection.")?;
        let buffer = editor.document()?;
        let selection = editor.selections()?.first().copied().ok_or(Error::Stale)?;
        if selection.start > selection.end {
            return Err(Error::Stale);
        }
        for at in [selection.start, selection.end] {
            if text::offset(&buffer.text, at)
                .is_none_or(|offset| text::position(&buffer.text, offset) != at)
            {
                return Err(Error::Stale);
            }
        }
        self.config();
        let doc = Document {
            path: editor.path(),
            language: editor.language(),
            version: buffer.version,
            text: buffer.text.clone(),
        };
        let ranges = LanguageFeatures::selection_ranges(self, &doc, selection.start);
        let next = ranges
            .into_iter()
            .find(|range| {
                range.start <= selection.start && range.end >= selection.end && *range != selection
            })
            .ok_or("The entire admitted log selection is already selected.")?;
        let latest = editor.document()?;
        if latest.version != buffer.version || latest.text != buffer.text {
            return Err(Error::Stale);
        }
        editor.set_selections(&[next])?;
        editor.reveal(next)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    #[test]
    fn selection_ranges_refuse_surrogate_interior_and_foreign_languages() {
        let h = MockHost::install().with_capabilities(&["editor.read"]);
        let mut p = Ideolog::default();
        p.config();
        let source = "😀 ERROR message\n";
        let mut doc = Document {
            path: "/outside/log.txt".into(),
            language: "log".into(),
            text: source.into(),
            version: 1,
        };
        assert!(LanguageFeatures::selection_ranges(&mut p, &doc, Position::new(0, 1)).is_empty());
        h.open_editor("/outside/log.txt", "log", source, &[(0, 1, 0, 1)]);
        assert!(matches!(
            p.extend_log_selection(&h.invocation()),
            Err(Error::Stale)
        ));
        doc.language = "java".into();
        assert!(LanguageFeatures::selection_ranges(&mut p, &doc, Position::new(0, 3)).is_empty());
    }
    #[test]
    fn generic_selection_ranges_and_command_expand_field_line_event_without_writes() {
        let h = MockHost::install().with_capabilities(&["editor.read"]);
        let source = "2024-05-01 10:00:00,123 [   1] ERROR - #a.B - 😀 message\n    at app.A.one(A.java:3)\n2024-05-01 10:00:01,123 [   1] INFO - #a.B - ready\n";
        let at = text::position(source, source.find("ERROR").unwrap() + 1);
        h.open_editor(
            "/outside/app.log",
            "log",
            source,
            &[(at.line, at.character, at.line, at.character)],
        );
        let mut p = Ideolog::default();
        p.extend_log_selection(&h.invocation()).unwrap();
        let selection = h.invocation().editor().unwrap().selections().unwrap()[0];
        assert_eq!(
            &source[text::offset(source, selection.start).unwrap()
                ..text::offset(source, selection.end).unwrap()],
            "ERROR"
        );
        p.extend_log_selection(&h.invocation()).unwrap();
        let selection = h.invocation().editor().unwrap().selections().unwrap()[0];
        assert_eq!(selection.start.line, 0);
        assert_eq!(selection.end.line, 0);
        p.extend_log_selection(&h.invocation()).unwrap();
        let selection = h.invocation().editor().unwrap().selections().unwrap()[0];
        assert_eq!(selection.end.line, 1);
        assert!(h.process_runs().is_empty());
    }
    #[test]
    fn built_in_log_stacktraces_fold_current_unsaved_text_without_processes() {
        let h = MockHost::install().with_capabilities(&[]);
        let mut p = Ideolog::default();
        p.config();
        let mut d=Document{path:"/project/app.log".into(),language:"log".into(),version:2,text:"2024-05-01 10:00:00 ERROR message\n    at app.A.one(A.java:3)\n    at app.B.two(B.java:4)\n2024-05-01 10:00:01 INFO ready\n".into()};
        let ranges = LanguageFeatures::folding(&mut p, &d);
        assert_eq!(ranges.len(), 1);
        assert_eq!((ranges[0].start_line, ranges[0].end_line), (0, 2));
        d.text = "2024-05-01 10:00:01 INFO fixed\n".into();
        assert!(LanguageFeatures::folding(&mut p, &d).is_empty());
        d.language = "java".into();
        assert!(LanguageFeatures::folding(&mut p, &d).is_empty());
        assert!(h.process_runs().is_empty());
        d.language = "log".into();
        d.text = "x".repeat(MAX_DOCUMENT_BYTES + 1);
        assert!(LanguageFeatures::folding(&mut p, &d).is_empty());
    }

    #[test]
    fn folding_returns_partial_folds_when_the_deadline_expires() {
        let config = Config::default();
        let text = "2024-05-01 10:00:00,123 [   1] ERROR - #a.B - boom\n    at a.B.c(B.java:1)\n".repeat(3000);
        let format = config.detect(&text);
        let mut checks = 0;
        let events = log::events_partial(format, &text, || {
            checks += 1;
            if checks > 2000 { Err(()) } else { Ok(()) }
        });
        assert!(!events.is_empty() && events.len() < 3000);
        assert!(events.iter().all(|e| e.lines.len() == 2), "the event being extended is dropped");
    }
}
