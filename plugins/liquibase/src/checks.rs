//! Problems in a changelog: duplicate or incomplete changeSets, unknown change types,
//! includes that point nowhere, and a missing formatted SQL header.

use crate::catalog::is_change_type;
use crate::changelog::{Changelog, Format, Include, Target};
use openidea_plugin::{Diagnostic, Location, RelatedInformation, Severity};
use openidea_plugin::{Position, Range};
use std::collections::HashMap;

/// The problems of `log` (parsed from `text`), sorted by where they start.
/// `defined` says whether the project can supply a property the changelog does not declare (`true` when unsure); `None` turns the property check off.
pub fn problems(
    path: &str,
    text: &str,
    log: &Changelog,
    target: &dyn Fn(&Include) -> Target,
    defined: Option<&dyn Fn(&str) -> bool>,
) -> Vec<Diagnostic> {
    let coordinates = Coordinates::new(text);
    let range = |_: &str, a: usize, b: usize| Range::new(coordinates.position(a), coordinates.position(b));
    let mut out: Vec<(usize, Diagnostic)> = Vec::new();

    if log.format == Format::Sql && !log.header {
        let end = text.find('\n').unwrap_or(text.len());
        let message = "Formatted SQL changelogs start with `--liquibase formatted sql`.";
        out.push((0, Diagnostic::new(range(text, 0, end), Severity::Warning, message).code("missing-header")));
    }

    let mut first: HashMap<(&str, &str), (usize, usize)> = HashMap::new();
    for change_set in &log.change_sets {
        let span = range(text, change_set.at, change_set.end);
        let (Some(id), Some(author)) = (&change_set.id, &change_set.author) else {
            let mut message = match (&change_set.id, &change_set.author) {
                (None, Some(_)) => "Changeset has no id.",
                (Some(_), None) => "Changeset has no author.",
                _ => "Changeset has no id and no author.",
            }
            .to_owned();
            if log.format == Format::Sql {
                message.push_str(" Write `--changeset author:id`.");
            }
            let diagnostic = Diagnostic::new(span, Severity::Error, message).code("incomplete-changeset");
            out.push((change_set.at, diagnostic));
            continue;
        };
        let Some(&(from, to)) = first.get(&(id.as_str(), author.as_str())) else {
            first.insert((id, author), (change_set.at, change_set.end));
            continue;
        };
        let message = format!("Changeset `{id}` by `{author}` is already defined in this file.");
        let mut diagnostic = Diagnostic::new(span, Severity::Error, message).code("duplicate-changeset");
        diagnostic.related.push(RelatedInformation {
            location: Location { path: path.to_owned(), range: range(text, from, to) },
            message: "First definition".into(),
        });
        out.push((change_set.at, diagnostic));
    }

    for change in &log.changes {
        if is_change_type(&change.name) {
            continue;
        }
        let message = format!("`{}` is not a built-in Liquibase change type.", change.name);
        let diagnostic = Diagnostic::new(range(text, change.at, change.end), Severity::Warning, message);
        out.push((change.at, diagnostic.code("unknown-change-type")));
    }

    for include in &log.includes {
        if include.optional || target(include) != Target::Missing {
            continue;
        }
        let message =
            if include.dir { format!("Cannot find directory `{}`.", include.path) } else { format!("Cannot find `{}`.", include.path) };
        let diagnostic = Diagnostic::new(range(text, include.at, include.end), Severity::Error, message);
        out.push((include.at, diagnostic.code("unresolved-include")));
    }

    for issue in &log.issues {
        let severity = if issue.error { Severity::Error } else { Severity::Warning };
        out.push((issue.at, Diagnostic::new(range(text, issue.at, issue.end), severity, issue.message.clone()).code(issue.code)));
    }

    for file in &log.sql_files {
        if target(file) != Target::Missing {
            continue;
        }
        let message = format!("Cannot find SQL file `{}`.", file.path);
        out.push((file.at, Diagnostic::new(range(text, file.at, file.end), Severity::Error, message).code("unresolved-sql-file")));
    }

    if let Some(defined) = defined {
        for (start, end, name) in placeholders(text, log.format) {
            if name.starts_with("database.") || log.properties.iter().any(|p| p == name) || defined(name) {
                continue;
            }
            let message = format!(
                "Property `{name}` is not declared in this changelog or the changelogs it includes, nor set as a parameter in the project's Liquibase configuration."
            );
            out.push((start, Diagnostic::new(range(text, start, end), Severity::Warning, message).code("unresolved-property")));
        }
    }

    out.sort_by_key(|(start, _)| *start);
    out.into_iter().map(|(_, diagnostic)| diagnostic).collect()
}

/// `${name}` placeholders as `(start, end, name)`. Skipped: names that are not plain identifiers (`${:name}`, `${1}`),
/// escaped ones (`$${name}`, `\${name}`), and text in XML comments, YAML `#` lines and SQL `--` lines (but not `--property`).
pub(crate) fn placeholders(text: &str, format: Format) -> Vec<(usize, usize, &str)> {
    let comments = if format == Format::Xml { xml_comments(text) } else { Vec::new() };
    let mut out = Vec::new();
    for (at, _) in text.match_indices("${") {
        let Some(close) = text[at..].find('}') else {
            continue;
        };
        let name = &text[at + 2..at + close];
        let starts_well = name.starts_with(|c: char| c.is_alphabetic() || c == '_');
        let plain = starts_well && name.chars().all(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '-'));
        let line_start = text[..at].rfind('\n').map_or(0, |i| i + 1);
        let before_on_line = text[line_start..at].trim_start();
        let commented = match format {
            Format::Yaml => before_on_line.starts_with('#'),
            Format::Json => false,
            Format::Sql => before_on_line.starts_with("--") && !before_on_line[2..].trim_start().to_lowercase().starts_with("property"),
            Format::Xml => comments.iter().any(|&(from, to)| from <= at && at < to),
        };
        let escaped = text[..at].ends_with(['$', '\\']);
        if !plain || commented || escaped {
            continue;
        }
        out.push((at, at + close + 1, name));
    }
    out
}

fn xml_comments(text: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(start) = text[from..].find("<!--").map(|i| i + from) {
        let end = text[start..].find("-->").map_or(text.len(), |i| start + i + 3);
        out.push((start, end));
        from = end;
    }
    out
}

/// UTF-16 positions by binary search: line starts, plus the running byte-minus-UTF-16 surplus of the non-ASCII characters.
struct Coordinates<'a> {
    text: &'a str,
    line_starts: Vec<usize>,
    wide: Vec<(usize, usize)>,
}

impl<'a> Coordinates<'a> {
    fn new(text: &'a str) -> Self {
        let mut line_starts = vec![0];
        let mut wide = Vec::new();
        let mut surplus = 0;
        for (at, ch) in text.char_indices() {
            if ch == '\n' {
                line_starts.push(at + 1);
            }
            if !ch.is_ascii() {
                surplus += ch.len_utf8() - ch.len_utf16();
                wide.push((at, surplus));
            }
        }
        Self { text, line_starts, wide }
    }

    fn surplus_before(&self, at: usize) -> usize {
        match self.wide.partition_point(|(byte, _)| *byte < at) {
            0 => 0,
            n => self.wide[n - 1].1,
        }
    }

    /// An offset inside a character, or past the end, snaps to the closest boundary before it.
    fn position(&self, at: usize) -> Position {
        let mut at = at.min(self.text.len());
        while !self.text.is_char_boundary(at) {
            at -= 1;
        }
        let line = self.line_starts.partition_point(|start| *start <= at) - 1;
        let start = self.line_starts[line];
        let column = (at - start) - (self.surplus_before(at) - self.surplus_before(start));
        Position::new(line as u32, column as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::text::position;

    #[test]
    fn position_non_boundary_offset_snaps_nearby() {
        let text = "é<a/>\n😀x";
        let coordinates = Coordinates::new(text);
        assert_eq!(coordinates.position(1), Position::new(0, 0));
        assert_eq!(coordinates.position(text.find('😀').unwrap() + 2), position(text, text.find('😀').unwrap()));
        assert_eq!(coordinates.position(10_000), position(text, text.len()));
    }

    #[test]
    fn positions_match_the_sdk_at_every_char_boundary() {
        let text = "a😀é\n\n<x y='ü'/>\r\n😀";
        let coordinates = Coordinates::new(text);
        for (at, _) in text.char_indices().chain([(text.len(), ' ')]) {
            assert_eq!(coordinates.position(at), position(text, at), "{at}");
        }
    }
}
