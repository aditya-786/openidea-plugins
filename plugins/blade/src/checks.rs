//! Inspections for Blade templates: unpaired directives and unbalanced
//! parentheses in a directive's arguments. Written from the Laravel Blade
//! documentation; unknown (custom) directives are never reported.

use openidea_plugin::Severity;

use super::{BLOCKS, Directive, directive_opens, lex};

pub const UNPAIRED_DIRECTIVE: &str = "unpaired-directive";
pub const UNPAIRED_PARENTHESIS: &str = "unpaired-parenthesis-in-string";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub start: usize,
    pub end: usize,
    pub code: &'static str,
    pub message: String,
    pub severity: Severity,
    pub fix: Option<Fix>,
}

/// Inserts `text` at byte offset `at` of the checked document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fix {
    pub title: String,
    pub at: usize,
    pub text: String,
}

impl Finding {
    fn warning(start: usize, end: usize, code: &'static str, message: String) -> Self {
        Self { start, end, code, message, severity: Severity::Warning, fix: None }
    }
}

struct Open {
    name: String,
    start: usize,
    closers: &'static [&'static str],
}

pub fn check(text: &str) -> Vec<Finding> {
    let lexed = lex(text);
    let mut found = Vec::new();
    let mut open: Vec<Open> = Vec::new();

    for directive in &lexed.directives {
        let name = directive.name.as_str();
        if lexed.opaque_from==Some(directive.range.start) {continue;}

        if let Some(finding) = unbalanced_parenthesis(text, directive) {
            found.push(finding);
        }

        if let Some(at) = open.iter().rposition(|o| o.closers.contains(&name)) {
            let parent_end = Some(directive.range.start);
            found.extend(open.drain(at..).skip(1).filter_map(|o| unclosed(text, o, parent_end)));
            continue;
        }

        if is_closer(name) {
            let end = directive.range.start + 1 + name.len();
            let message = format!("`@{name}` has no matching opening directive.");
            found.push(Finding::warning(directive.range.start, end, UNPAIRED_DIRECTIVE, message));
            continue;
        }

        let Ok(role)=directive_opens(text,directive) else {return found;};
        if let Some(closers) = role {
            open.push(Open { name: name.to_owned(), start: directive.range.start, closers });
        }
    }

    if lexed.opaque_from.is_none() {found.extend(open.into_iter().filter_map(|o| unclosed(text, o, None)));}
    found.sort_by_key(|f| f.start);

    found
}

/// `@show`, `@stop`, `@append` and `@overwrite` end a section but are also fine alone, so only `@end…` strays are reported.
fn is_closer(name: &str) -> bool {
    name.starts_with("end") && BLOCKS.iter().any(|(_, closers)| closers.contains(&name))
}

/// `parent_end` is where the enclosing block's closing directive starts; `None` when it is unknown.
fn unclosed(text: &str, open: Open, parent_end: Option<usize>) -> Option<Finding> {
    // A bare `@empty` is the separator of `@forelse`, not a block.
    if open.name == "empty" {
        return None;
    }

    let end = open.start + 1 + open.name.len();
    let message = format!("`@{}` is never closed: add `@{}`.", open.name, open.closers[0]);

    let fix = closing_fix(text, open.start, &format!("@{}", open.closers[0]), parent_end);

    Some(Finding { fix: Some(fix), ..Finding::warning(open.start, end, UNPAIRED_DIRECTIVE, message) })
}

/// Puts `closer` on its own line with the opener's indentation, before the parent's closer or at the end of the file.
/// An opener on the parent closer's line gets its closer inline instead, followed by a space: Blade does not
/// read `@endif@endforeach` as two directives.
fn closing_fix(text: &str, opener: usize, closer: &str, parent_end: Option<usize>) -> Fix {
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let line_start = |at: usize| text[..at].rfind('\n').map_or(0, |i| i + 1);
    let indent_of = |at: usize| {
        let line = &text[line_start(at)..];
        &line[..line.len() - line.trim_start_matches([' ', '\t']).len()]
    };
    let indent = indent_of(opener);

    let title = format!("Insert {closer}");

    let Some(at) = parent_end else {
        let insert = if text.ends_with('\n') { format!("{indent}{closer}{newline}") } else { format!("{newline}{indent}{closer}") };
        return Fix { title: format!("{title} at end of file"), at: text.len(), text: insert };
    };

    if line_start(opener) == line_start(at) {
        return Fix { title, at, text: format!("{closer} ") };
    }

    if text[line_start(at)..at].trim().is_empty() {
        return Fix { title, at: line_start(at), text: format!("{indent}{closer}{newline}") };
    }

    Fix { title, at, text: format!("{newline}{indent}{closer}{newline}{}", indent_of(at)) }
}

fn unbalanced_parenthesis(text: &str, directive: &Directive) -> Option<Finding> {
    let arguments = directive.arguments.as_ref()?;
    let closed = directive.range.end > arguments.end;
    if closed {
        return None;
    }

    let open = arguments.start - 1;
    let end = open + text[open..arguments.end].trim_end().len();
    let message = format!(
        "The parentheses of `@{}(` are unbalanced: no `)` closes this `(` (parentheses inside strings are not counted).",
        directive.name,
    );

    Some(Finding::warning(open, end, UNPAIRED_PARENTHESIS, message))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codes(text: &str) -> Vec<(&'static str, &str)> {
        check(text).iter().map(|f| (f.code, &text[f.start..f.end])).collect()
    }

    fn quiet(text: &str) {
        assert_eq!(codes(text), [], "{text}");
    }

    fn fixed(text: &str) -> Vec<(String, String)> {
        check(text)
            .into_iter()
            .filter_map(|f| f.fix)
            .map(|fix| (fix.title, format!("{}{}{}", &text[..fix.at], fix.text, &text[fix.at..])))
            .collect()
    }

    #[test]
    fn unclosed_block_at_end_of_file_gets_its_closer_there() {
        assert_eq!(fixed("@if($a)\n<p></p>\n"), [("Insert @endif at end of file".into(), "@if($a)\n<p></p>\n@endif\n".into())]);
        assert_eq!(fixed("  @foreach($a as $b)\r\n  x"), [("Insert @endforeach at end of file".into(), "  @foreach($a as $b)\r\n  x\r\n  @endforeach".into())]);
    }

    #[test]
    fn nested_unclosed_block_closes_before_its_parent_with_its_own_indentation() {
        let text = "@section('a')\n    @if($a)\n        x\n@endsection\n";
        assert_eq!(fixed(text), [("Insert @endif".into(), "@section('a')\n    @if($a)\n        x\n    @endif\n@endsection\n".into())]);
    }

    #[test]
    fn closer_goes_on_its_own_line_or_inline_when_the_parent_ends_on_the_openers_line() {
        assert_eq!(fixed("@foreach($a as $b)\n  @if($b)\n  x </p>@endforeach")[0].1, "@foreach($a as $b)\n  @if($b)\n  x </p>\n  @endif\n  @endforeach");
        assert_eq!(fixed("@foreach($a as $b) @if($b) x @endforeach")[0].1, "@foreach($a as $b) @if($b) x @endif @endforeach");
    }

    #[test]
    fn both_unclosed_blocks_close_at_end_of_file_and_the_result_is_quiet() {
        let text = "@if($a)\n  @isset($b)\n";
        let fixes = fixed(text);
        assert_eq!(fixes.iter().map(|(title, _)| title.as_str()).collect::<Vec<_>>(), ["Insert @endif at end of file", "Insert @endisset at end of file"]);
        let once = &fixes[0].1;
        assert_eq!(fixed(once), [("Insert @endisset".into(), "@if($a)\n  @isset($b)\n  @endisset\n@endif\n".into())]);
        quiet(&fixed(once)[0].1);
    }

    #[test]
    fn other_findings_have_no_fix() {
        assert!(fixed("@endif\n@if($a == ')'\n@endif\n@forelse($a as $b)\n@empty\n@endforelse").is_empty());
    }

    #[test]
    fn endif_without_if_warns() {
        assert_eq!(codes("<p></p>\n@endif\n"), [(UNPAIRED_DIRECTIVE, "@endif")]);
    }

    #[test]
    fn if_without_endif_warns() {
        let found = check("@if($a)\n<p></p>\n");
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].start, found[0].end), (0, 3));
        assert!(found[0].message.contains("@endif"));
    }

    #[test]
    fn inner_block_left_open_is_reported_at_its_opener() {
        assert_eq!(codes("@if($a)\n@foreach($b as $c)\n@endif"), [(UNPAIRED_DIRECTIVE, "@foreach")]);
    }

    #[test]
    fn elseif_inside_if_is_quiet() {
        quiet("@if($a)\n@elseif($b)\n@else\n@endif\n@switch($x)\n@case(1)\n@break\n@default\n@endswitch");
    }

    #[test]
    fn verbatim_content_is_not_checked() {
        quiet("@verbatim\n@if($a)\n@endforeach\n@endverbatim");
    }

    #[test]
    fn comments_and_escaped_directives_are_not_checked() {
        quiet("{{-- @if($a) --}}\n@@if($a)\n@@endforeach\n{{ '@endif' }}\njane@endif.com");
    }

    #[test]
    fn php_blocks_are_not_checked() {
        quiet("@php\n$x = '@if';\n@endphp\n@php($y = 1)");
    }

    #[test]
    fn unclosed_php_block_warns() {
        assert_eq!(codes("@php\n$x = 1;\n"), [(UNPAIRED_DIRECTIVE, "@php")]);
    }

    #[test]
    fn inline_section_has_no_closer() {
        quiet("@section('title', 'Home')\n@section('a')\nx\n@endsection\n@section('b')\ny\n@show\n@section('c')\n@stop\n@section('d')\n@append\n@section('e')\n@overwrite");
    }

    #[test]
    fn forelse_empty_is_quiet() {
        quiet("@forelse($a as $b)\n{{ $b }}\n@empty\nnone\n@endforelse\n@empty($c)\nx\n@endempty\n@isset($d)\n@endisset");
    }

    #[test]
    fn documented_blocks_pair_quietly() {
        quiet("@push('s')\n@endpush\n@prepend('s')\n@endprepend\n@once\n@endonce\n@error('e')\n@enderror\n@env('local')\n@endenv\n@production\n@endproduction\n@unless($a)\n@endunless\n@can('x')\n@endcan\n@cannot('x')\n@endcannot\n@canany(['a'])\n@endcanany\n@auth\n@endauth\n@guest\n@endguest\n@for($i = 0; $i < 3; $i++)\n@endfor\n@while($a)\n@endwhile\n@hasSection('a')\n@endif\n@pushIf($a, 's')\n@endPushIf");
    }

    #[test]
    fn custom_directives_are_quiet_for_library_or_dynamic_name() {
        quiet("@datetime($x)\n@endmycustom\n@mycustom\n@include('a')\n@csrf");
    }

    #[test]
    fn unbalanced_parenthesis_in_string_argument_warns() {
        assert_eq!(codes("@if($a == ')'\n<p></p>\n@endif"), [(UNPAIRED_PARENTHESIS, "($a == ')'")]);
    }

    #[test]
    fn balanced_parentheses_with_parentheses_in_strings_are_quiet() {
        quiet("@if(str_contains($a, '(') && f(')'))\n@endif\n@include('a', ['b' => \"(\"])\n@if(\n  $a\n)\n@endif");
    }

    #[test]
    fn ranges_use_utf16_columns_with_multibyte_text() {
        let text = "é😀 @endif";
        let found = &check(text)[0];
        let range = openidea_plugin::text::position(text, found.start);
        assert_eq!((range.line, range.character), (0, 4));
    }

    #[test]
    fn crlf_files_report_correct_lines() {
        let text = "<p></p>\r\n@if($a)\r\n@endif\r\n@endif\r\n@if(\r\n";
        let found = check(text);
        let lines: Vec<_> = found.iter().map(|f| openidea_plugin::text::position(text, f.start).line).collect();
        assert_eq!(lines, [3, 4, 4]);
        assert_eq!(&text[found[2].start..found[2].end], "(");
    }
}
