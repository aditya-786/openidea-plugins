//! Blade for OpenIDEA: highlighting, structure view (sections, pushes) and
//! folding (directive pairs, comments) from a hand-written lexer, exported as
//! the plugin's `syntax-provider`. Detection comes from `languages/blade.toml`.

mod checks;
mod injections;
mod hover;
mod navigation;

use std::ops::Range;

use openidea_plugin::syntax::{Fold, FoldKind, Highlight, Symbol, SymbolKind, SyntaxProvider, Token};
use openidea_plugin::text::position;
use openidea_plugin::{Diagnostic, Error, Invocation, Plugin, Range as EditorRange, host, register_plugin};

use checks::{Finding, UNPAIRED_DIRECTIVE, UNPAIRED_PARENTHESIS};

#[derive(Default)]
pub struct Blade;

impl Plugin for Blade {
    fn run_command(&mut self, id: &str, _ctx: &Invocation, _arg: Option<&str>) -> Result<Option<String>, Error> {
        Err(Error::NotFound(format!("command {id}")))
    }
}

impl openidea_plugin::diagnostics::DiagnosticsProvider for Blade {
    fn diagnostics(&mut self, doc: &openidea_plugin::providers::Document) -> Vec<Diagnostic> {
        if !doc.path.ends_with(".blade.php") {
            return Vec::new();
        }
        let text = &doc.text;

        enabled_findings(text)
            .iter()
            .map(|f| Diagnostic {
                code: Some(f.code.into()),
                ..Diagnostic::new(EditorRange::new(position(text, f.start), position(text, f.end)), f.severity, &f.message)
            })
            .collect()
    }
}

fn enabled_findings(text: &str) -> Vec<Finding> {
    let pairs = host::setting_bool("checkDirectivePairs").unwrap_or(true);
    let parentheses = host::setting_bool("checkParentheses").unwrap_or(true);

    checks::check(text)
        .into_iter()
        .filter(|f| match f.code {
            UNPAIRED_DIRECTIVE => pairs,
            UNPAIRED_PARENTHESIS => parentheses,
            _ => true,
        })
        .collect()
}

impl SyntaxProvider for Blade {
    fn highlights(&mut self, _language: &str, text: &str) -> Vec<Token> {
        lex(text).tokens
    }

    fn outline(&mut self, _language: &str, text: &str) -> Vec<Symbol> {
        structure(text).0
    }

    fn folding(&mut self, _language: &str, text: &str) -> Vec<Fold> {
        structure(text).1
    }
}

register_plugin!(Blade, syntax, diagnostics, hover, providers, injections);

const PHP_KEYWORDS: &[&str] = &[
    "as",
    "if",
    "else",
    "elseif",
    "for",
    "foreach",
    "while",
    "return",
    "new",
    "instanceof",
    "fn",
    "function",
    "use",
    "and",
    "or",
    "xor",
    "echo",
    "match",
    "static",
    "isset",
    "empty",
    "list",
    "array",
    "clone",
    "print",
    "throw",
    "try",
    "catch",
    "finally",
];
const PHP_CONSTANTS: &[&str] = &["true", "false", "null", "TRUE", "FALSE", "NULL"];

/// Directives that open a block, and the ones that close it.
const BLOCKS: &[(&str, &[&str])] = &[
    ("if", &["endif"]),
    ("hasSection", &["endif"]),
    ("hasStack", &["endif"]),
    ("sectionMissing", &["endif"]),
    ("unless", &["endunless"]),
    ("isset", &["endisset"]),
    ("empty", &["endempty"]),
    ("auth", &["endauth"]),
    ("guest", &["endguest"]),
    ("production", &["endproduction"]),
    ("env", &["endenv"]),
    ("section", &["endsection", "show", "stop", "append", "overwrite"]),
    ("push", &["endpush"]),
    ("pushIf", &["endPushIf"]),
    ("prepend", &["endprepend"]),
    ("pushOnce", &["endPushOnce"]),
    ("prependOnce", &["endPrependOnce"]),
    ("once", &["endonce"]),
    ("for", &["endfor"]),
    ("foreach", &["endforeach"]),
    ("forelse", &["endforelse"]),
    ("while", &["endwhile"]),
    ("switch", &["endswitch"]),
    ("php", &["endphp"]),
    ("verbatim", &["endverbatim"]),
    ("error", &["enderror"]),
    ("component", &["endcomponent"]),
    ("slot", &["endslot"]),
    ("can", &["endcan"]),
    ("cannot", &["endcannot"]),
    ("canany", &["endcanany"]),
    ("session", &["endsession"]),
    ("fragment", &["endfragment"]),
    ("persist", &["endpersist"]),
    ("teleport", &["endteleport"]),
    ("script", &["endscript"]),
    ("assets", &["endassets"]),
];

/// Directives that neither open nor close a block.
// ponytail: only Laravel's own directives (and Livewire's) are known; custom ones from Blade::directive() stay plain text.
const STANDALONE: &[&str] = &[
    "else",
    "elseif",
    "elsecan",
    "elsecannot",
    "elsecanany",
    "extends",
    "extendsFirst",
    "yield",
    "parent",
    "include",
    "includeIf",
    "includeWhen",
    "includeUnless",
    "includeFirst",
    "includeIsolated",
    "each",
    "stack",
    "continue",
    "break",
    "case",
    "default",
    "props",
    "aware",
    "class",
    "style",
    "checked",
    "selected",
    "disabled",
    "readonly",
    "required",
    "csrf",
    "method",
    "json",
    "js",
    "lang",
    "choice",
    "use",
    "inject",
    "vite",
    "viteReactRefresh",
    "dd",
    "dump",
    "livewire",
    "livewireStyles",
    "livewireScripts",
    "livewireScriptConfig",
    "bool",
    "unlessenv",
];

fn is_directive(name: &str) -> bool {
    STANDALONE.contains(&name) || BLOCKS.iter().any(|(open, closers)| *open == name || closers.contains(&name))
}

/// The closers of a directive that opens a block, if this use of it does.
fn opens(name: &str, has_arguments: bool, argument_count: usize) -> Option<&'static [&'static str]> {
    let (_, closers) = BLOCKS.iter().find(|(open, _)| *open == name)?;
    let inline = match name {
        // `@section('title', 'Home')` is complete on its own.
        "section" => argument_count >= 2,
        "php" => has_arguments,
        // Without arguments `@empty` is the separator of `@forelse`.
        "empty" => !has_arguments,
        _ => false,
    };
    (!inline).then_some(closers)
}

/// A proved local capture role. Unknown values do not establish block ownership.
fn directive_opens(text: &str, directive: &Directive) -> Result<Option<&'static [&'static str]>, ()> {
    let name=directive.name.as_str();
    if !matches!(name,"section"|"push"|"prepend") {
        let count=directive.arguments.as_ref().map_or(0,|a|argument_count(text,a));
        return Ok(opens(name,directive.arguments.is_some(),count));
    }
    let args=directive.arguments.as_ref().ok_or(())?;
    let parts=argument_parts(text,args).ok_or(())?;
    if first_string(text,args).is_none() || !(1..=2).contains(&parts.len()) {return Err(());}
    let captures=if parts.len()==1 {true} else {
        let part=&parts[1];let lexer=Lexer{text,bytes:text.as_bytes(),out:Lexed::default(),in_tag:false};
        let from=argument_trivia_end(&lexer,part.start,part.end).ok_or(())?;
        let null_end=from+4;
        if text.get(from..null_end).is_some_and(|s|s.eq_ignore_ascii_case("null"))
            && argument_trivia_end(&lexer,null_end,part.end)==Some(part.end) {
            if name=="section" {true} else {false}
        } else {
            if !matches!(lexer.bytes.get(from),Some(b'\''|b'"')) {return Err(());}
            let end=lexer.argument_atom_end(from,part.end).map_err(|_|())?.ok_or(())?;
            if argument_trivia_end(&lexer,end,part.end)!=Some(part.end) {return Err(());}
            let body=text.get(from+1..end-1).ok_or(())?;
            if body.contains('\\') || lexer.bytes[from]==b'"' && body.contains('$') {return Err(());}
            name!="section" && body.is_empty()
        }
    };
    Ok(captures.then(||BLOCKS.iter().find(|(n,_)|*n==name).unwrap().1))
}

/// Directives whose first string argument names something worth listing.
fn symbol_kind(name: &str) -> Option<SymbolKind> {
    matches!(name, "section" | "push" | "pushOnce" | "prepend" | "prependOnce" | "fragment").then_some(SymbolKind::Field)
}

#[derive(Clone, Copy)]
enum ArgumentFailure { Malformed, Opaque }

#[derive(Debug, PartialEq, Eq)]
struct Directive {
    name: String,
    /// Between the parentheses.
    arguments: Option<Range<usize>>,
    range: Range<usize>,
}

#[derive(Default)]
struct Lexed {
    tokens: Vec<Token>,
    directives: Vec<Directive>,
    comments: Vec<Range<usize>>,
    // The remaining source crosses an unsupported PHP lexical state.
    opaque_from: Option<usize>,
    argument_scanned: usize,
}

impl Lexed {
    fn push(&mut self, range: Range<usize>, kind: Highlight) {
        if range.start < range.end {
            self.tokens.push(Token { range, kind });
        }
    }
}

struct Lexer<'a> {
    text: &'a str,
    bytes: &'a [u8],
    out: Lexed,
    in_tag: bool,
}

fn is_identifier_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_identifier(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

// PHP LABEL accepts bytes >=0x80 in identifiers; do not truncate their UTF8
// spelling or give a Unicode-suffixed variable the special $this role.
fn is_php_identifier_start(byte: u8) -> bool {is_identifier_start(byte) || byte>=0x80}
fn is_php_identifier(byte: u8) -> bool {is_identifier(byte) || byte>=0x80}

fn lex(text: &str) -> Lexed {
    let mut lexer = Lexer { text, bytes: text.as_bytes(), out: Lexed::default(), in_tag: false };
    lexer.markup();
    lexer.out
}

impl Lexer<'_> {
    fn at(&self, i: usize, s: &str) -> bool {
        self.bytes[i..].starts_with(s.as_bytes())
    }

    fn find(&self, from: usize, s: &str) -> Option<usize> {
        self.text[from..].find(s).map(|i| from + i)
    }

    fn line_end(&self, from: usize) -> usize {
        self.text[from..].find('\n').map_or(self.bytes.len(), |i| from + i)
    }

    fn markup(&mut self) {
        let mut i = 0;
        while i < self.bytes.len() {
            if let Some(end) = self.construct(i) {
                i = end;
                continue;
            }

            if self.at(i, "<!--") {
                let end = self.find(i + 4, "-->").map_or(self.bytes.len(), |e| e + 3);
                self.out.push(i..end, Highlight::Comment);
                i = end;
                continue;
            }

            let opens_element =
                self.bytes[i] == b'<' && self.bytes.get(i + 1).is_some_and(|b| is_identifier_start(*b) || *b == b'/' || *b == b'!');
            if opens_element {
                i = self.element(i);
                continue;
            }

            i += 1;
        }
    }

    /// `<name attr="value" …>`, with Blade anywhere in it.
    fn element(&mut self, start: usize) -> usize {
        self.in_tag = true;
        let end = self.element_body(start);
        self.in_tag = false;
        end
    }

    fn element_body(&mut self, start: usize) -> usize {
        let mut i = start + 1;
        while i < self.bytes.len() && (is_identifier(self.bytes[i]) || matches!(self.bytes[i], b'/' | b'!' | b'-' | b':' | b'.')) {
            i += 1;
        }
        self.out.push(start..i, Highlight::Tag);

        while i < self.bytes.len() {
            let b = self.bytes[i];

            if let Some(end) = self.construct(i) {
                i = end;
            } else if b == b'>' {
                self.out.push(i..i + 1, Highlight::Tag);
                return i + 1;
            } else if self.at(i, "/>") {
                self.out.push(i..i + 2, Highlight::Tag);
                return i + 2;
            } else if b == b'"' || b == b'\'' {
                i = self.attribute_value(i);
            } else if b.is_ascii_whitespace() || b == b'=' {
                i += 1;
            } else if b == b'<' {
                return i;
            } else {
                let name_start = i;
                while i < self.bytes.len()
                    && !matches!(self.bytes[i], b' ' | b'\t' | b'\n' | b'\r' | b'=' | b'>' | b'/' | b'"' | b'\'' | b'{' | b'<')
                {
                    i += 1;
                }
                i = i.max(name_start + 1);
                self.out.push(name_start..i, Highlight::Attribute);
            }
        }

        i
    }

    fn attribute_value(&mut self, start: usize) -> usize {
        let quote = self.bytes[start];
        let mut segment = start;
        let mut i = start + 1;

        while i < self.bytes.len() {
            if self.bytes[i] == quote {
                self.out.push(segment..i + 1, Highlight::String);
                return i + 1;
            }

            let first_token = self.out.tokens.len();
            if let Some(end) = self.construct(i) {
                if segment < i {
                    self.out.tokens.insert(first_token, Token { range: segment..i, kind: Highlight::String });
                }
                i = end;
                segment = i;
                continue;
            }

            i += 1;
        }

        self.out.push(segment..i, Highlight::String);
        i
    }

    /// A Blade comment, echo, directive or PHP block starting at `i`; returns its end.
    fn construct(&mut self, i: usize) -> Option<usize> {
        match self.bytes[i] {
            b'{' if self.at(i, "{{--") => {
                let end = self.find(i + 4, "--}}").map_or(self.bytes.len(), |e| e + 4);
                self.out.push(i..end, Highlight::Comment);
                self.out.comments.push(i..end);
                Some(end)
            }
            b'{' if self.at(i, "{{") => Some(self.echo(i, 2, "}}")),
            b'{' if self.at(i, "{!!") => Some(self.echo(i, 3, "!!}")),
            // `@@if` and `@{{ x }}` print as written.
            b'@' if self.at(i, "@@") => Some(i + 2),
            b'@' if self.at(i, "@{{") => Some(self.find(i, "}}").map_or(self.bytes.len(), |e| e + 2)),
            b'@' => self.directive(i),
            b'<' if self.at(i, "<?") => Some(self.php_block(i)),
            _ => None,
        }
    }

    fn echo(&mut self, start: usize, open_length: usize, close: &str) -> usize {
        let body = start + open_length;
        self.out.push(start..body, Highlight::Punctuation);

        let Some(end) = self.find(body, close) else {
            self.php(body, self.bytes.len());
            return self.bytes.len();
        };

        self.php(body, end);
        self.out.push(end..end + close.len(), Highlight::Punctuation);
        end + close.len()
    }

    fn php_block(&mut self, start: usize) -> usize {
        let open_length = ["<?php", "<?="].iter().find(|open| self.at(start, open)).map_or(2, |open| open.len());
        let body = start + open_length;
        self.out.push(start..body, Highlight::Punctuation);

        let end=match self.php_code_close(body) {
            Ok(Some(end))=>end,
            state=> {
                if state.is_err() {self.out.opaque_from=Some(start);}
                self.php(body,self.bytes.len());return self.bytes.len();
            }
        };

        self.php(body, end);
        self.out.push(end..end + 2, Highlight::Punctuation);
        end + 2
    }

    // A PHP closing tag is active in scripting/line-comment state, not in
    // quoted strings or block comments. Unknown complex string states stay PHP.
    fn php_code_close(&self, from: usize) -> Result<Option<usize>,ArgumentFailure> {
        let mut i=from;
        while i<self.bytes.len() {
            if self.at(i,"?>") {return Ok(Some(i));}
            if matches!(self.bytes[i],b'\''|b'"'|b'`') {
                i=self.php_code_string_end(i).ok_or(ArgumentFailure::Opaque)?;
            } else if self.at(i,"/*") {
                i=self.find(i+2,"*/").ok_or(ArgumentFailure::Opaque)?+2;
            } else if self.at(i,"//") || self.bytes[i]==b'#' && self.bytes.get(i+1)!=Some(&b'[') {
                i+=if self.bytes[i]==b'#' {1} else {2};
                while i<self.bytes.len() && !matches!(self.bytes[i],b'\r'|b'\n') && !self.at(i,"?>") {i+=1;}
            } else if self.at(i,"<<<") {
                // No guessed heredoc/nowdoc terminator or interpolated PHP parser.
                return Err(ArgumentFailure::Opaque);
            } else {i+=1;}
        }
        Ok(None)
    }
    fn php_code_string_end(&self, from: usize) -> Option<usize> {
        let quote=self.bytes[from];let mut i=from+1;
        while i<self.bytes.len() {
            if self.bytes[i]==b'\\' {i+=2;continue;}
            if self.bytes[i]==quote {return Some(i+1);}
            if quote!=b'\'' && (self.at(i,"{$") || self.at(i,"${")) {return None;}
            i+=1;
        }
        None
    }

    fn directive(&mut self, at: usize) -> Option<usize> {
        // An email address, not a directive.
        if at > 0 && is_identifier(self.bytes[at - 1]) {
            return None;
        }

        let name_start = at + 1;
        let mut name_end = name_start;
        while name_end < self.bytes.len() && (self.bytes[name_end].is_ascii_alphanumeric() || self.bytes[name_end] == b'_') {
            name_end += 1;
        }

        let name = &self.text[name_start..name_end];
        let continues = self.bytes.get(name_end).is_some_and(|b| is_identifier(*b));
        if name.is_empty() || continues {
            return None;
        }

        if !is_directive(name) {
            let tail=self.text[name_end..].trim_start();
                        if tail.starts_with(['=','.',':']) || self.in_tag&&!tail.starts_with('(') {return None;}
        }

        // CSS/Tailwind at-rules share Blade's @ spelling and remain CSS text.
        if !is_directive(name) && (self.bytes.get(name_end)==Some(&b'-') || ["media","supports","keyframes","charset","import","layer","container","property","scope","page","namespace","tailwind","apply","theme","config","utility","variant","source","reference","plugin"].contains(&name)) {return None;}

        self.out.push(at..name_end, Highlight::Keyword);

        let mut open = name_end;
        while open < self.bytes.len() && matches!(self.bytes[open], b' ' | b'\t') {
            open += 1;
        }

        let mut arguments = None;
        let mut end = name_end;
        if self.bytes.get(open) == Some(&b'(') {
            let scanned=self.closing_parenthesis(open);
            let close=scanned.ok();
            if matches!(scanned,Err(ArgumentFailure::Opaque)) {self.out.opaque_from=Some(at);}
            let body_end=close.unwrap_or_else(||match scanned {
                Err(ArgumentFailure::Opaque)=>self.bytes.len(),
                _=>self.line_end(open),
            });

            self.out.push(open..open + 1, Highlight::Punctuation);
            self.php(open + 1, body_end);
            if close.is_some() {
                self.out.push(body_end..body_end + 1, Highlight::Punctuation);
            }

            arguments = Some(open + 1..body_end);
            end = close.map_or(body_end, |c| c + 1);
        }

        // Unknown syntactically valid directives receive lexical PHP coloring,
        // while pairing, navigation, hover and diagnostics retain only proven
        // core directive roles. No per-project directive cache is consulted.
        if !is_directive(name) { return Some(end); }

        let has_arguments = arguments.is_some();
        self.out.directives.push(Directive { name: name.to_owned(), arguments, range: at..end });

        if name == "verbatim" {
            return Some(self.find_directive(end, "endverbatim").unwrap_or(self.bytes.len()));
        }

        if name == "php"
            && !has_arguments
            && let Some(close) = self.find_directive(end, "endphp")
        {
            self.php(end, close);
            return Some(close);
        }

        Some(end)
    }

    /// Start of the next `@name` directive after `from`.
    fn find_directive(&self, from: usize, name: &str) -> Option<usize> {
        let needle = format!("@{name}");
        let mut at = from;
        while let Some(found) = self.find(at, &needle) {
            let after = found + needle.len();
            if !self.bytes.get(after).is_some_and(|b| is_identifier(*b)) {
                return Some(found);
            }
            at = after;
        }
        None
    }

    // PHP comments/strings cannot supply argument punctuation or Blade tokens.
    fn argument_atom_end(&self, at: usize, limit: usize) -> Result<Option<usize>,ArgumentFailure> {
        let end=if matches!(self.bytes[at],b'\''|b'"'|b'`') {
            Some(self.php_code_string_end(at).ok_or(ArgumentFailure::Opaque)?)
        } else if self.at(at,"/*") {
            Some(self.find(at+2,"*/").ok_or(ArgumentFailure::Opaque)?+2)
        } else if self.at(at,"//") || self.bytes[at]==b'#' && self.bytes.get(at+1)!=Some(&b'[') {
            let mut end=at+if self.bytes[at]==b'#' {1} else {2};
            while end<limit && !matches!(self.bytes[end],b'\r'|b'\n') {
                if self.at(end,"?>") {return Err(ArgumentFailure::Opaque);}
                end+=1;
            }
            Some(end)
        } else if self.at(at,"<<<") || self.at(at,"?>") {
            return Err(ArgumentFailure::Opaque);
        } else {None};
        if end.is_some_and(|end|end>limit) {return Err(ArgumentFailure::Opaque);}
        Ok(end)
    }
    /// The real balanced argument close, ignoring strings and PHP comments.
    fn closing_parenthesis(&mut self, open: usize) -> Result<usize,ArgumentFailure> {
        let mut stack=Vec::new();let mut i=open;
        // Recovery can revisit the remaining suffix for each malformed opener.
        // Share a linear scan budget and make exhaustion an opaque boundary.
        let limit=self.bytes.len().saturating_mul(8);
        while i<self.bytes.len() {
            if self.out.argument_scanned>=limit {return Err(ArgumentFailure::Opaque);}
            if let Some(end)=self.argument_atom_end(i,self.bytes.len())? {
                self.out.argument_scanned=self.out.argument_scanned.saturating_add(end-i);
                if self.out.argument_scanned>limit {return Err(ArgumentFailure::Opaque);}
                i=end;continue;
            }
            self.out.argument_scanned+=1;
            match self.bytes[i] {
                b'('|b'['|b'{'=>stack.push(self.bytes[i]),
                close @ (b')'|b']'|b'}')=> {
                    let expected=match close {b')'=>b'(',b']'=>b'[',_=>b'{'};
                    if stack.pop()!=Some(expected) {return Err(ArgumentFailure::Malformed);}
                    if stack.is_empty() {return Ok(i);}
                }
                _=>{}
            }
            i+=1;
        }
        Err(ArgumentFailure::Malformed)
    }

    /// The index after the string literal whose opening quote is at `start`.
    fn string_end(&self, start: usize) -> usize {
        let quote = self.bytes[start];
        let mut i = start + 1;

        while i < self.bytes.len() {
            if self.bytes[i] == quote {
                return i + 1;
            }
            i += if self.bytes[i] == b'\\' { 2 } else { 1 };
        }

        self.bytes.len()
    }

    /// PHP between `start` and `end`: strings, variables, numbers, calls and keywords.
    fn php(&mut self, start: usize, end: usize) {
        let mut i = start;

        while i < end {
            let b = self.bytes[i];
            let token_start = i;

            if b.is_ascii_whitespace() {
                i += 1;
            } else if b == b'"' || b == b'\'' {
                i = self.string_end(i).min(end);
                self.out.push(token_start..i, Highlight::String);
            } else if b == b'$' {
                i += 1;
                while i < end && is_php_identifier(self.bytes[i]) {
                    i += 1;
                }
                let kind = if &self.text[token_start..i] == "$this" { Highlight::SelfVariable } else { Highlight::Variable };
                self.out.push(token_start..i, kind);
            } else if b.is_ascii_digit() {
                while i < end && (self.bytes[i].is_ascii_digit() || self.bytes[i] == b'.' || self.bytes[i] == b'_') {
                    i += 1;
                }
                self.out.push(token_start..i, Highlight::Number);
            } else if is_php_identifier_start(b) || b == b'\\' {
                while i < end && (is_php_identifier(self.bytes[i]) || self.bytes[i] == b'\\') {
                    i += 1;
                }
                let kind = self.php_word_kind(start, token_start, i, end);
                if let Some(kind) = kind {
                    self.out.push(token_start..i, kind);
                }
            } else if b.is_ascii() {
                i += 1;
                let kind = if b"()[]{},;".contains(&b) { Highlight::Punctuation } else { Highlight::Operator };
                self.out.push(token_start..i, kind);
            } else {
                i += self.text[i..].chars().next().map_or(1, char::len_utf8);
            }
        }
    }

    fn php_word_kind(&self, region_start: usize, word_start: usize, word_end: usize, region_end: usize) -> Option<Highlight> {
        let word = &self.text[word_start..word_end];
        let before = self.text[region_start..word_start].trim_end();
        let after = self.text[word_end..region_end].trim_start();
        let is_call = after.starts_with('(');

        if before.ends_with("->") || before.ends_with("?->") {
            return Some(if is_call { Highlight::FunctionCall } else { Highlight::Property });
        }

        if before.ends_with("::") {
            return Some(if is_call { Highlight::FunctionCall } else { Highlight::Constant });
        }

        if PHP_KEYWORDS.contains(&word) {
            return Some(Highlight::Keyword);
        }

        if PHP_CONSTANTS.contains(&word) {
            return Some(Highlight::Constant);
        }

        if is_call {
            return Some(Highlight::FunctionCall);
        }

        let is_class = word.trim_start_matches('\\').bytes().next().is_some_and(|b| b.is_ascii_uppercase());
        is_class.then_some(Highlight::Type)
    }
}

fn line_of(line_starts: &[usize], at: usize) -> u32 {
    line_starts.partition_point(|start| *start <= at) as u32 - 1
}

// Split only top-level commas; PHP comments/strings cannot invent argument roles.
fn argument_parts(text: &str, arguments: &Range<usize>) -> Option<Vec<Range<usize>>> {
    let lexer=Lexer{text,bytes:text.as_bytes(),out:Lexed::default(),in_tag:false};
    let mut parts=Vec::new();let mut stack=Vec::new();let mut from=arguments.start;let mut i=from;
    while i<arguments.end {
        if let Some(end)=lexer.argument_atom_end(i,arguments.end).ok()? {i=end;continue;}
        match lexer.bytes[i] {
            b'('|b'['|b'{'=>stack.push(lexer.bytes[i]),
            close @ (b')'|b']'|b'}')=> {
                let expected=match close {b')'=>b'(',b']'=>b'[',_=>b'{'};
                if stack.pop()!=Some(expected) {return None;}
            }
            b',' if stack.is_empty()=>{parts.push(from..i);from=i+1;}
            _=>{}
        }
        i+=1;
    }
    if !stack.is_empty() {return None;}
    parts.push(from..arguments.end);Some(parts)
}
fn argument_trivia_end(lexer: &Lexer<'_>, mut at: usize, limit: usize) -> Option<usize> {
    while at<limit {
        if lexer.bytes[at].is_ascii_whitespace() {at+=1;continue;}
        if lexer.at(at,"/*") || lexer.at(at,"//") || lexer.bytes[at]==b'#' && lexer.bytes.get(at+1)!=Some(&b'[') {
            at=lexer.argument_atom_end(at,limit).ok()??;continue;
        }
        break;
    }
    Some(at)
}
/// One complete ordinary literal first argument, with an exact source span.
fn first_string(text: &str, arguments: &Range<usize>) -> Option<Range<usize>> {
    let parts=argument_parts(text,arguments)?;let first=parts.first()?;
    let lexer=Lexer{text,bytes:text.as_bytes(),out:Lexed::default(),in_tag:false};
    let from=argument_trivia_end(&lexer,first.start,first.end)?;
    if !matches!(lexer.bytes.get(from),Some(b'\''|b'"')) {return None;}
    let end=lexer.argument_atom_end(from,first.end).ok()??;
    if argument_trivia_end(&lexer,end,first.end)?!=first.end {return None;}
    let body=text.get(from+1..end-1)?;
    if body.is_empty() || body.len()>256 || body.contains('\\') || body.chars().any(char::is_control)
        || lexer.bytes[from]==b'"' && body.contains('$') {return None;}
    Some(from+1..end-1)
}
/// Commas outside PHP strings/comments/brackets, plus one.
fn argument_count(text: &str, arguments: &Range<usize>) -> usize {
    argument_parts(text,arguments).map_or(0,|parts|parts.len())
}

struct Open {
    closers: &'static [&'static str],
    start: usize,
    symbol: Option<usize>,
}

/// Structure-view symbols (sections, pushes) and folds (directive pairs, comments).
fn structure(text: &str) -> (Vec<Symbol>, Vec<Fold>) {
    let lexed = lex(text);
    let line_starts: Vec<usize> = std::iter::once(0).chain(text.match_indices('\n').map(|(at, _)| at + 1)).collect();
    let mut symbols: Vec<Symbol> = Vec::new();
    let mut folds = Vec::new();
    let mut open: Vec<Open> = Vec::new();

    for directive in &lexed.directives {
        let name = directive.name.as_str();
        if directive.arguments.as_ref().is_some_and(|arguments|directive.range.end<=arguments.end) {open.clear();continue;}

        if let Some(at) = open.iter().rposition(|o| o.closers.contains(&name)) {
            let opened = open.remove(at);
            open.truncate(at);

            let (start_line, end_line) = (line_of(&line_starts, opened.start), line_of(&line_starts, directive.range.start));
            if end_line > start_line {
                folds.push(Fold { start_line, end_line, kind: FoldKind::Region });
            }
            if let Some(symbol) = opened.symbol {
                symbols[symbol].range.end = directive.range.end;
            }
            continue;
        }

        let Ok(role)=directive_opens(text,directive) else {break;};
        let symbol = symbol_kind(name).zip(directive.arguments.as_ref().and_then(|a| first_string(text, a))).map(|(kind, name_range)| {
            let parent = open.iter().rev().find_map(|o| o.symbol);
            symbols.push(Symbol {
                name: text[name_range.clone()].to_owned(),
                detail: Some(format!("@{name}")),
                kind,
                range: directive.range.clone(),
                name_range,
                parent,
            });
            symbols.len() - 1
        });

        if let Some(closers) = role {
            open.push(Open { closers, start: directive.range.start, symbol });
        }
    }

    for comment in &lexed.comments {
        let (start_line, end_line) = (line_of(&line_starts, comment.start), line_of(&line_starts, comment.end));
        if end_line > start_line {
            folds.push(Fold { start_line, end_line, kind: FoldKind::Comment });
        }
    }

    (symbols, folds)
}

#[cfg(test)]
mod tests {
    use openidea_plugin::Event;
    use openidea_plugin::mock::MockHost;

    use super::*;

    fn project(path: &str, text: &str) -> MockHost {
        let host = MockHost::install();
        host.set_project_root(Some("/p"));
        host.add_file(path, text);
        host
    }

    fn publish(host: &MockHost, event: Event) {
        if let Event::FileOpened(path) | Event::FileSaved(path) = event {
            let project = host::project_of(&path).unwrap();
            let text = project.read_file(&path).unwrap();
            host.open_editor(&path, "blade", &text, &[]);
            host.refresh_diagnostics(&mut Blade);
        }
    }

    #[test]
    fn registered_php_highlights_keep_complete_unicode_identifier_ranges() {
        let _h=openidea_plugin::mock::MockHost::install().with_capabilities(&[]);
        for source in ["😀 {{ $café }}","@if($café) yes @endif","<?php $café=1; ?>","{{ $🦀 }}"] {
            let tokens=Blade.highlights("blade",source);
            assert!(tokens.iter().all(|token|source.is_char_boundary(token.range.start) && source.is_char_boundary(token.range.end)),"{source}");
            let variable=tokens.iter().find(|token|token.kind==Highlight::Variable).unwrap();
            let expected=if source.contains("$🦀") {"$🦀"} else {"$café"};
            assert_eq!(&source[variable.range.clone()],expected);
        }
    }
    #[test]
    fn registered_php_function_and_this_prefix_use_complete_unicode_source_spelling() {
        let _h=openidea_plugin::mock::MockHost::install().with_capabilities(&[]);
        let source="😀 {{ café($thisé) }}";let tokens=Blade.highlights("blade",source);
        let function=tokens.iter().find(|token|token.kind==Highlight::FunctionCall).expect("whole PHP function spelling");
        assert_eq!(source.get(function.range.clone()),Some("café"));
        let variable=tokens.iter().find(|token|token.kind==Highlight::Variable).expect("this prefix is not the special variable");
        assert_eq!(source.get(variable.range.clone()),Some("$thisé"));
        assert!(tokens.iter().all(|token|token.kind!=Highlight::SelfVariable));
    }
    #[test]
    fn registered_argument_facts_keep_exact_literals_and_refuse_dynamic_or_opaque_names() {
        let _h=openidea_plugin::mock::MockHost::install().with_capabilities(&[]);
        let source="😀 @section(/* 'fake', ) */ 'réal')\r\n@endsection";
        let symbols=Blade.outline("blade",source);assert_eq!(symbols.len(),1);assert_eq!(symbols[0].name,"réal");
        assert_eq!(&source[symbols[0].name_range.clone()],"réal");
        for source in [
            "@section($name ?? 'fallback') @endsection",
            "@section('prefix'.$name) @endsection",
            "@section(\"$name\") @endsection",
            "@section('escaped\\'name') @endsection",
        ] {assert!(Blade.outline("blade",source).is_empty(),"{source}");}
        for source in ["@if([$x)\n@endif","@if({$x])\n@endif"] {
            let doc=openidea_plugin::providers::Document{path:"/project/view.blade.php".into(),language:"blade".into(),version:8,text:source.into()};
            assert!(openidea_plugin::providers::Providers::definition(&mut Blade,&doc,position(source,source.find("@endif").unwrap()+2)).is_empty());
            assert!(openidea_plugin::hover::HoverProvider::hover(&mut Blade,&doc,openidea_plugin::Position::new(0,1)).is_none());
        }
        for source in [
            "@section(<<<TXT\n@if($fake)\nTXT;\n)\n@endsection",
            "@if($real) <?php $x=<<<TXT\n?> @if($fake)\nTXT;\n?> @endif",
        ] {
            let doc=openidea_plugin::providers::Document{path:"/project/view.blade.php".into(),language:"blade".into(),version:8,text:source.into()};
            assert!(openidea_plugin::hover::HoverProvider::hover(&mut Blade,&doc,position(source,source.find("@if($fake)").unwrap()+2)).is_none());
            assert!(openidea_plugin::diagnostics::DiagnosticsProvider::diagnostics(&mut Blade,&doc).is_empty());
            assert!(Blade.outline("blade",source).is_empty());
        }
        let source="@section( // 'fake', )\n'real'\n)\n@endsection";
        assert_eq!(Blade.outline("blade",source)[0].name,"real");
    }
    #[test]
    fn registered_diagnostics_ignore_parenthesis_and_directive_text_in_argument_comment() {
        let _h=openidea_plugin::mock::MockHost::install().with_capabilities(&[]);
        let doc=openidea_plugin::providers::Document{path:"/project/view.blade.php".into(),language:"blade".into(),version:7,text:"@if(/* ) @if($fake) */ $real) @endif".into()};
        assert!(openidea_plugin::diagnostics::DiagnosticsProvider::diagnostics(&mut Blade,&doc).is_empty());
    }
    #[test]
    fn registered_outline_requires_actual_first_literal_argument_not_comment_text() {
        let _h=openidea_plugin::mock::MockHost::install().with_capabilities(&[]);
        let source="@section(/* 'fake' */ 'real')\n@endsection";
        let symbols=Blade.outline("blade",source);assert_eq!(symbols.len(),1);assert_eq!(symbols[0].name,"real");
    }
    #[test]
    fn registered_diagnostics_ignore_directive_text_after_quoted_php_close() {
        let h=openidea_plugin::mock::MockHost::install().with_capabilities(&[]);
        let source="<?php $x='?> @if($fake)'; ?> @if($real) @endif";
        let doc=openidea_plugin::providers::Document{path:"/project/view.blade.php".into(),language:"blade".into(),version:7,text:source.into()};
        assert!(openidea_plugin::diagnostics::DiagnosticsProvider::diagnostics(&mut Blade,&doc).is_empty());
        assert!(h.requests().is_empty() && h.process_runs().is_empty());
    }
    #[test]
    fn publishes_findings_for_a_blade_file() {
        let host = project("v.blade.php", "<p>é</p>\n@endif\n");
        publish(&host, Event::FileOpened("/p/v.blade.php".into()));

        let published = &host.provider_diagnostics()["/p/v.blade.php"];
        assert_eq!(published.len(), 1);
        assert_eq!(published[0].code.as_deref(), Some("unpaired-directive"));
        assert_eq!(published[0].range.start.line, 1);
    }

    #[test]
    fn fixed_file_publishes_empty_list() {
        let host = project("v.blade.php", "@endif\n");
        publish(&host, Event::FileOpened("/p/v.blade.php".into()));
        assert_eq!(host.provider_diagnostics().len(), 1);

        host.add_file("v.blade.php", "@if($a)\n@endif\n");
        host.open_editor("/p/v.blade.php", "blade", "@if($a)\n@endif\n", &[]);
        publish(&host, Event::FileSaved("/p/v.blade.php".into()));
        assert!(host.provider_diagnostics().is_empty());
    }

    #[test]
    fn non_blade_files_are_ignored() {
        let host = project("v.php", "@endif\n");
        publish(&host, Event::FileOpened("/p/v.php".into()));
        assert!(host.provider_diagnostics().is_empty());
    }

    #[test]
    fn each_group_can_be_switched_off() {
        let host = project("v.blade.php", "@endif\n@if(\n");
        host.set_setting("checkDirectivePairs", "false");
        publish(&host, Event::FileOpened("/p/v.blade.php".into()));
        let codes: Vec<_> = host.provider_diagnostics()["/p/v.blade.php"].iter().map(|d| d.code.clone().unwrap()).collect();
        assert_eq!(codes, ["unpaired-parenthesis-in-string"]);

        host.set_setting("checkParentheses", "false");
        publish(&host, Event::FileOpened("/p/v.blade.php".into()));
        assert!(host.provider_diagnostics().is_empty());
    }

    #[test]
    fn current_document_diagnostics_work_without_file_events() {
        let host = MockHost::install().with_capabilities(&["project.read"]);
        let mut plugin = Blade;
        assert!(plugin.activate(&host.context("aritti.blade")).is_ok());
        let doc = openidea_plugin::providers::Document { path: "/p/test.blade.php".into(), language: "blade".into(), version: 7, text: "@endif".into() };
        assert!(!openidea_plugin::diagnostics::DiagnosticsProvider::diagnostics(&mut plugin, &doc).is_empty());
        let fixed = openidea_plugin::providers::Document { text: String::new(), version: 8, ..doc };
        assert!(openidea_plugin::diagnostics::DiagnosticsProvider::diagnostics(&mut plugin, &fixed).is_empty());
    }

    fn kinds(text: &str) -> Vec<(&str, &'static str)> {
        lex(text).tokens.iter().map(|t| (&text[t.range.clone()], t.kind.name())).collect()
    }

    #[test]
    fn highlights_echoes_in_html() {
        let text = r#"<a href="{{ route('home') }}" class="x">{{-- note --}}{!! $user->name !!}</a>"#;
        assert_eq!(
            kinds(text),
            [
                ("<a", "tag"),
                ("href", "attribute"),
                ("\"", "string"),
                ("{{", "punctuation"),
                ("route", "function-call"),
                ("(", "punctuation"),
                ("'home'", "string"),
                (")", "punctuation"),
                ("}}", "punctuation"),
                ("\"", "string"),
                ("class", "attribute"),
                ("\"x\"", "string"),
                (">", "tag"),
                ("{{-- note --}}", "comment"),
                ("{!!", "punctuation"),
                ("$user", "variable"),
                ("-", "operator"),
                (">", "operator"),
                ("name", "property"),
                ("!!}", "punctuation"),
                ("</a", "tag"),
                (">", "tag"),
            ]
        );
    }

    #[test]
    fn highlights_directives_and_their_arguments() {
        assert_eq!(
            kinds("@foreach ($posts as $post)\n@include('partials.post', ['post' => $post])"),
            [
                ("@foreach", "keyword"),
                ("(", "punctuation"),
                ("$posts", "variable"),
                ("as", "keyword"),
                ("$post", "variable"),
                (")", "punctuation"),
                ("@include", "keyword"),
                ("(", "punctuation"),
                ("'partials.post'", "string"),
                (",", "punctuation"),
                ("[", "punctuation"),
                ("'post'", "string"),
                ("=", "operator"),
                (">", "operator"),
                ("$post", "variable"),
                ("]", "punctuation"),
                (")", "punctuation"),
            ]
        );
    }

    #[test]
    fn leaves_what_blade_leaves_alone() {
        assert!(kinds("mail me at john@if.com").is_empty());
        assert!(kinds("@media (min-width: 1px) { a { color: red } }").is_empty());
        assert!(kinds("@@if(x) @{{ raw }}").is_empty());
        assert!(kinds("<button @click=\"open = !open\">").contains(&("@click", "attribute")));
    }

    #[test]
    fn reads_php_blocks_and_skips_verbatim() {
        let names = |text| kinds(text).into_iter().map(|(t, _)| t).collect::<Vec<_>>();
        assert_eq!(names("@verbatim{{ not blade }}@endverbatim"), ["@verbatim", "@endverbatim"]);
        assert_eq!(names("@php $x = 1; @endphp"), ["@php", "$x", "=", "1", ";", "@endphp"]);
        assert_eq!(names("@php($x = 1)"), ["@php", "(", "$x", "=", "1", ")"]);
    }

    #[test]
    fn outlines_and_folds_directives() {
        let text = "@extends('layouts.app')\n@section('content')\n  @if ($a)\n    x\n  @endif\n  @push('scripts')\n  @endpush\n@endsection\n@section('title', 'Home')\n{{--\n long\n--}}\n@forelse ($xs as $x)\n  y\n@empty\n  z\n@endforelse\n";
        let (symbols, folds) = structure(text);

        let outline: Vec<_> = symbols.iter().map(|s| (s.name.as_str(), s.kind, s.parent)).collect();
        assert_eq!(
            outline,
            [("content", SymbolKind::Field, None), ("scripts", SymbolKind::Field, Some(0)), ("title", SymbolKind::Field, None)]
        );
        assert!(text[symbols[0].range.clone()].starts_with("@section('content')"));
        assert!(text[symbols[0].range.clone()].ends_with("@endsection"));
        assert!(text[symbols[2].range.clone()].ends_with("'Home')"));

        let lines: Vec<_> = folds.iter().map(|f| (f.start_line, f.end_line, f.kind)).collect();
        assert_eq!(
            lines,
            [
                (2, 4, FoldKind::Region),
                (5, 6, FoldKind::Region),
                (1, 7, FoldKind::Region),
                (12, 16, FoldKind::Region),
                (9, 11, FoldKind::Comment),
            ]
        );
    }

    #[test]
    fn survives_every_prefix() {
        let text = "<div class=\"{{ $a ? 'é' : \"b\" }}\" @if($x)\n@section('é', {{ 'ü' }})\n@php $é = 1;\n<?php echo \"x\" ?>\n{{-- é --}}@verbatim ü @endverbatim\n@foreach ($ys as $y) {!! $y->é !!}";
        for (end, _) in text.char_indices().chain([(text.len(), ' ')]) {
            let prefix = &text[..end];
            let lexed = lex(prefix);
            for token in &lexed.tokens {
                assert!(prefix.is_char_boundary(token.range.start) && prefix.is_char_boundary(token.range.end), "{prefix:?}");
            }
            structure(prefix);
        }
    }
}

#[cfg(test)]
mod form_role_repro {
 use super::*;
 fn registered_quiet(source:&str) {
  let _host=openidea_plugin::mock::MockHost::install().with_capabilities(&[]);
  let doc=openidea_plugin::providers::Document{path:"/p/view.blade.php".into(),language:"blade".into(),version:1,text:source.into()};
  let diagnostics=openidea_plugin::diagnostics::DiagnosticsProvider::diagnostics(&mut Blade,&doc);
  assert!(diagnostics.is_empty(), "{source}: {diagnostics:?}");
 }
 #[test] fn registered_section_literal_null_captures(){registered_quiet("@section('title', null)\nbody\n@endsection");}
 #[test] fn registered_push_literal_content_is_inline(){registered_quiet("@push('scripts', 'asset')");}
 #[test] fn registered_prepend_literal_content_is_inline(){registered_quiet("@prepend('scripts', 'asset')");}
 #[test] fn registered_unknown_content_has_no_guessed_absence_or_navigation(){
  for source in ["@section('title', $content) @endsection", "@push('s', $content) @endpush", "@prepend('s', ...$contents) @endprepend", "@section(section: 's', content: 'x') @endsection"] {
   registered_quiet(source);
   let doc=openidea_plugin::providers::Document{path:"/p/view.blade.php".into(),language:"blade".into(),version:1,text:source.into()};
   let at=source.rfind('@').unwrap()+2;
   assert!(openidea_plugin::providers::Providers::definition(&mut Blade,&doc,openidea_plugin::text::position(source,at)).is_empty());
   assert!(structure(source).1.is_empty());
  }
 }
 #[test] fn exact_literal_roles_share_navigation_and_structure(){
  let source="😀 @section('s', /* content */ NULL)\nbody\n@endsection";
  registered_quiet(source);
  let doc=openidea_plugin::providers::Document{path:"/p/view.blade.php".into(),language:"blade".into(),version:1,text:source.into()};
  let links=openidea_plugin::providers::Providers::definition(&mut Blade,&doc,openidea_plugin::text::position(source,source.rfind('@').unwrap()+2));
  assert_eq!(links.len(),1);assert_eq!(links[0].range.start.character,3);
  assert_eq!(structure(source).1.len(),1);
  for source in ["@push('s', '')\nbody\n@endpush","@prepend('s', /* empty */ \"\")\nbody\n@endprepend","@section('s', '')"]{registered_quiet(source);}
 }

 #[test] fn registered_hover_uses_same_literal_role_and_unknown_boundary(){
  use openidea_plugin::hover::HoverProvider;
  for (source,expected,forbidden) in [
   ("@section('s', null)","Block form","Inline form"),
   ("@push('s', 'x')","Inline form","Block form"),
   ("@prepend('s', '')","Block form","Inline form"),
   ("@section('s', $content)","Capture role is unknown","Inline form"),
  ] {
   let _h=openidea_plugin::mock::MockHost::install().with_capabilities(&[]);
   let doc=openidea_plugin::hover::Document{path:"/p/view.blade.php".into(),language:"blade".into(),version:1,text:source.into()};
   let result=Blade.hover(&doc,openidea_plugin::Position::new(0,2)).unwrap();
   let rendered=format!("{result:?}"); assert!(rendered.contains(expected),"{rendered}");assert!(!rendered.contains(forbidden),"{rendered}");
  }
 }

 #[test] fn registered_malformed_argument_recovery_keeps_uncertain_suffix_bounded(){
  let _h=openidea_plugin::mock::MockHost::install().with_capabilities(&[]);
  let source=format!("{}@if($tail) @endif", "@if(\n".repeat(4000));
  let doc=openidea_plugin::providers::Document{path:"/p/view.blade.php".into(),language:"blade".into(),version:1,text:source.clone()};
  let diagnostics=openidea_plugin::diagnostics::DiagnosticsProvider::diagnostics(&mut Blade,&doc);
  assert!(!diagnostics.is_empty() && diagnostics.len()<64,"unknown suffix must not flood absence facts");
  assert!(openidea_plugin::providers::Providers::definition(&mut Blade,&doc,openidea_plugin::text::position(&source,source.rfind("@endif").unwrap()+2)).is_empty());
 }
 #[test] fn registered_large_balanced_template_preserves_successor_navigation(){
  let _h=openidea_plugin::mock::MockHost::install().with_capabilities(&[]);
  let source=format!("{}😀 @if($tail)\nbody\n@endif", "@if(/* ) */ $ok) @endif\n".repeat(1500));
  let doc=openidea_plugin::providers::Document{path:"/p/view.blade.php".into(),language:"blade".into(),version:1,text:source.clone()};
  assert!(openidea_plugin::diagnostics::DiagnosticsProvider::diagnostics(&mut Blade,&doc).is_empty());
  let found=openidea_plugin::providers::Providers::definition(&mut Blade,&doc,openidea_plugin::text::position(&source,source.rfind("@endif").unwrap()+2));
  assert_eq!(found.len(),1);assert_eq!(found[0].range.start,openidea_plugin::text::position(&source,source.find("@if($tail)").unwrap()));
 }

}

#[cfg(test)]mod custom_lexical_tests{use super::*;
 #[test]fn custom_directives_have_lexical_php_colors_without_inventing_pairing_or_scope(){let source="@money_2($amount, 'USD') @endmoney_2";let lexed=lex(source);assert!(lexed.directives.is_empty());assert!(lexed.tokens.iter().any(|token|token.kind==Highlight::Keyword));assert!(lexed.tokens.iter().any(|token|token.kind==Highlight::Variable));}
}

#[cfg(test)]mod stack_tests{use super::*;
 #[test]fn has_stack_uses_actual_laravel_if_closer_without_inventing_a_custom_closer(){let text="@hasStack('scripts')\n<script></script>\n@endif";let parsed=lex(text);assert_eq!(parsed.directives.iter().map(|d|d.name.as_str()).collect::<Vec<_>>(),["hasStack","endif"]);assert!(crate::checks::check(text).is_empty());}
}
