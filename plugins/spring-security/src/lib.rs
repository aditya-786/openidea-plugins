//! Spring Security Tools for OpenIDEA: flags configuration that Spring
//! Security 6 removed or deprecated, role-prefix source advisories and plain-text
//! password encoders. Reviewed local mutations use guarded FileActions,
//! and completes SpEL in method security annotations. Text-based: Java and
//! Kotlin sources, and the security namespace of Spring XML files.

use openidea_plugin::Location;
use openidea_plugin::providers::{CodeAction, CompletionItem, CompletionKind, Document, LineMarker, Providers};
use openidea_plugin::text::offset;
use openidea_plugin::{Diagnostic, Error, Invocation, Plugin, Position, Range, Severity, TextEdit, host, register_plugin};
use spring_text::index::{DIAGNOSTICS_WARM, Indexes, PROVIDER_BUDGET};
use spring_text::java::{self, Annotation, JavaFile};
use spring_text::sibling::sibling_calls;
use spring_text::{SOURCE_GLOBS, beans, range, xml};
use std::collections::BTreeSet;
use std::rc::Rc;

mod actions;
mod endpoints;
mod live;
mod refs;
mod ownership;
mod path_matcher;
mod modern_matcher;
mod debug_call;
mod injections;

pub(crate) const METHOD_SECURITY: [&str; 4] = ["PreAuthorize", "PostAuthorize", "PreFilter", "PostFilter"];
const ROLE_PREFIX: &str = "ROLE_";
const SECURITY_NAMESPACE: &str = "http://www.springframework.org/schema/security";
const WEB_EXPRESSION_FUNCTIONS: [&str; 12] = [
    "hasRole",
    "hasAnyRole",
    "hasAuthority",
    "hasAnyAuthority",
    "hasIpAddress",
    "hasPermission",
    "isAnonymous",
    "isAuthenticated",
    "isFullyAuthenticated",
    "isRememberMe",
    "permitAll",
    "denyAll",
];
const ARGUMENT_FUNCTIONS: [&str; 5] = ["hasRole", "hasAnyRole", "hasAuthority", "hasAnyAuthority", "hasIpAddress"];
/// SpEL completions: label, snippet, the annotations it applies to (empty: all).
const SPEL: [(&str, &str, &[&str]); 15] = [
    ("hasRole", "hasRole('$1')", &[]),
    ("hasAnyRole", "hasAnyRole($1)", &[]),
    ("hasAuthority", "hasAuthority('$1')", &[]),
    ("hasAnyAuthority", "hasAnyAuthority($1)", &[]),
    ("hasPermission", "hasPermission($1, '$2')", &[]),
    ("isAuthenticated", "isAuthenticated()", &[]),
    ("isAnonymous", "isAnonymous()", &[]),
    ("isFullyAuthenticated", "isFullyAuthenticated()", &[]),
    ("isRememberMe", "isRememberMe()", &[]),
    ("permitAll", "permitAll", &[]),
    ("denyAll", "denyAll", &[]),
    ("principal", "principal", &[]),
    ("authentication", "authentication", &[]),
    ("returnObject", "returnObject", &["PostAuthorize", "PostFilter"]),
    ("filterObject", "filterObject", &["PreFilter", "PostFilter"]),
];

/// A source diagnostic. Mutations use the separately guarded FileActions resolver.
struct Problem {
    diagnostic: Diagnostic,
}

/// The quoted arguments of `hasRole(…)`/`hasAnyRole(…)` in a SpEL expression: byte offset and value.
fn spel_roles(expr: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    for call in ["hasRole(", "hasAnyRole("] {
        for (i, _) in expr.match_indices(call) {
            let from = i + call.len();
            let to = expr[from..].find(')').map_or(expr.len(), |e| from + e);
            let mut at = from;
            while let Some(q) = expr[at..to].find(['\'', '"']) {
                let start = at + q + 1;
                let Some(len) = expr[start..to].find(&expr[at + q..start]) else {
                    break;
                };
                out.push((start, &expr[start..start + len]));
                at = start + len + 1;
            }
        }
    }
    out.sort();
    out
}

fn annotations(file: &JavaFile) -> impl Iterator<Item = &Annotation> {
    file.types.iter().flat_map(|t| t.annotations.iter().chain(t.members.iter().flat_map(|m| &m.annotations)))
}

/// The role names a Java source uses, `ROLE_` stripped.
fn roles(_path: &str, text: &str) -> Option<Vec<String>> {
    if !text.contains("Role") && !text.contains("Secured") {
        return None;
    }
    let file = java::parse(text);
    let mut out = BTreeSet::new();
    for a in annotations(&file) {
        if a.name == "Secured" || a.name == "RolesAllowed" {
            out.extend(a.strings(&["value"]).iter().map(|s| s.value.clone()));
        }
        if METHOD_SECURITY.contains(&a.name.as_str()) {
            for s in a.strings(&["value"]) {
                out.extend(spel_roles(&s.value).into_iter().map(|(_, r)| r.to_owned()));
            }
        }
    }
    for c in java::calls(text).iter().filter(|c| c.name == "hasRole" || c.name == "hasAnyRole") {
        out.extend(c.args.iter().filter_map(|a| a.string.as_ref()).map(|s| s.value.clone()));
    }
    let out: Vec<String> = out
        .into_iter()
        .map(|r| r.strip_prefix(ROLE_PREFIX).map(str::to_owned).unwrap_or(r))
        .filter(|r| !r.is_empty() && !r.contains(['$', '#', '{']))
        .collect();
    (!out.is_empty()).then_some(out)
}

fn problem(text: &str, from: usize, to: usize, severity: Severity, code: &str, message: String) -> Diagnostic {
    Diagnostic::new(range(text, from, to), severity, message).code(code)
}

fn edit(text: &str, from: usize, to: usize, new: &str) -> TextEdit {
    TextEdit { range: range(text, from, to), text: new.to_owned() }
}

fn role_prefix(text: &str, at: usize, role: &str) -> Problem {
    let message = format!(
        "`hasRole`/`hasAnyRole` throw IllegalArgumentException (`ROLE_{role} should not start with ROLE_ …`) when the default `ROLE_` prefix applies: write `{role}`, or use `hasAuthority(\"ROLE_{role}\")`."
    );
    Problem {
        diagnostic: problem(text, at, at + ROLE_PREFIX.len() + role.len(), Severity::Warning, "role-prefix", message),
    }
}

fn spel_role_prefix_message(role: &str) -> String {
    format!("The SpEL `hasRole`/`hasAnyRole` strip a leading `ROLE_` with the default prefix, so this works, but `{role}` is the documented spelling.")
}

/// Quotes or parentheses left open in a SpEL expression.
fn is_unbalanced(expr: &str) -> bool {
    let mut quote = None;
    let mut depth = 0i32;
    for c in expr.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '\'' | '"') => quote = Some(c),
            (None, '(') => depth += 1,
            (None, ')') => {
                depth -= 1;
                if depth < 0 {
                    return true;
                }
            }
            _ => {}
        }
    }
    quote.is_some() || depth != 0
}

/// Problems of a SpEL web security expression: byte span, code, severity, message.
fn expression_problems(expr: &str) -> Vec<(usize, usize, &'static str, Severity, String)> {
    if is_unbalanced(expr) {
        return vec![(
            0,
            expr.len(),
            "unbalanced-expression",
            Severity::Error,
            "Unbalanced quotes or parentheses in this expression.".into(),
        )];
    }
    let mut out = Vec::new();
    let bytes = expr.as_bytes();
    let mut quote = None;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        if c == b'\'' || c == b'"' {
            quote = Some(c);
        }
        let starts_word =
            c.is_ascii_alphabetic() && (i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || b"_.@#$".contains(&bytes[i - 1])));
        if !starts_word {
            i += 1;
            continue;
        }
        let end = expr[i..].find(|ch: char| !(ch.is_alphanumeric() || ch == '_')).map_or(expr.len(), |n| i + n);
        let name = &expr[i..end];
        let after = expr[end..].trim_start();
        if after.starts_with('(') {
            let open = expr.len() - after.len();
            let known = WEB_EXPRESSION_FUNCTIONS.contains(&name);
            let close = expr[open..].find(')').map_or(expr.len(), |n| open + n);
            if known && ARGUMENT_FUNCTIONS.contains(&name) && expr[open + 1..close].trim().is_empty() {
                out.push((i, close + 1, "empty-security-function", Severity::Error, format!("`{name}` needs an argument.")));
            }
            let looks_like_check =
                ["has", "is"].iter().any(|p| name.strip_prefix(p).is_some_and(|rest| rest.starts_with(|ch: char| ch.is_uppercase())));
            if !known && looks_like_check {
                let message = format!("`{name}` is not a Spring Security web expression function.");
                out.push((i, end, "unknown-security-function", Severity::Warning, message));
            }
        }
        i = end;
    }
    for (at, role) in spel_roles(expr) {
        if let Some(bare) = role.strip_prefix(ROLE_PREFIX) {
            out.push((at, at + role.len(), "role-prefix", Severity::Info, spel_role_prefix_message(bare)));
        }
    }
    out.sort_by_key(|p| p.0);
    out
}

/// `text` with comments and string literals blanked out (same byte offsets).
/// Only Kotlin block comments nest.
fn code_only(text: &str, kotlin: bool) -> String {
    let b = text.as_bytes();
    let mut out = b.to_vec();
    let blank = |out: &mut [u8], from: usize, to: usize| {
        for byte in &mut out[from..to] {
            if *byte != b'\n' && byte.is_ascii() {
                *byte = b' ';
            }
        }
    };
    let mut i = 0;
    while i < b.len() {
        let start = i;
        if b[i..].starts_with(b"//") {
            i = text[i..].find('\n').map_or(b.len(), |n| i + n);
        } else if b[i..].starts_with(b"/*") {
            let mut depth = 0;
            while i < b.len() {
                if depth == 0 || kotlin && b[i..].starts_with(b"/*") {
                    depth += 1;
                    i += 2;
                } else if b[i..].starts_with(b"*/") {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
        } else if b[i..].starts_with(b"\"\"\"") {
            i = text[i + 3..].find("\"\"\"").map_or(b.len(), |n| i + 3 + n + 3);
        } else if b[i] == b'\'' {
            let mut j = i + 1;
            while j < b.len() && j <= i + 8 && b[j] != b'\'' && b[j] != b'\n' {
                j += if b[j] == b'\\' { 2 } else { 1 };
            }
            if j >= b.len() || j > i + 8 || b[j] != b'\'' {
                i += 1;
                continue;
            }
            i = j + 1;
        } else if b[i] == b'"' {
            i += 1;
            while i < b.len() && b[i] != b'"' && b[i] != b'\n' {
                i += if b[i] == b'\\' { 2 } else { 1 };
            }
            i = (i + 1).min(b.len());
        } else {
            i += 1;
            continue;
        }
        blank(&mut out, start, i.min(b.len()));
    }
    String::from_utf8(out).unwrap_or_default()
}

fn xml_problems(text: &str) -> Vec<Problem> {
    let mut out = Vec::new();
    if !text.contains(SECURITY_NAMESPACE) {
        return out;
    }
    let elements = xml::elements(text);
    let diagnostic = |from: usize, to: usize, severity: Severity, code: &str, message: String| Problem {
        diagnostic: problem(text, from, to, severity, code, message),
    };
    for (i, e) in elements.iter().enumerate() {
        if xml::namespace(&elements, i) != Some(SECURITY_NAMESPACE) {
            continue;
        }
        if e.local() == "global-method-security" {
            let message = "<global-method-security> is deprecated: use <method-security>.".to_owned();
            let mut found = diagnostic(e.at, e.at + e.name.len(), Severity::Warning, "global-method-security", message);
            found.diagnostic.deprecated = true;
            out.push(found);
        }
        if e.local() == "debug" && host::setting_bool("springSecurity.checkDebug").unwrap_or(true) {
            let message = "Security debug logging is on: it logs request details and headers. Turn it off in production.".to_owned();
            out.push(diagnostic(e.at, e.at + e.name.len(), Severity::Warning, "debug-enabled", message));
        }
        if e.local() != "http" {
            continue;
        }
        let expressions = e.value("use-expressions") != Some("false");
        if let Some(a) = e.attr("use-expressions").filter(|_| !expressions) {
            let message = "`use-expressions=\"false\"` is deprecated: write access rules as expressions (hasRole('ADMIN')).".to_owned();
            let mut found = diagnostic(a.at, a.value_end + 1, Severity::Warning, "use-expressions-false", message);
            found.diagnostic.deprecated = true;
            out.push(found);
        }
        let urls: Vec<&xml::Element> = elements
            .iter()
            .enumerate()
            .filter(|(j, u)| {
                u.parent == Some(i) && u.local() == "intercept-url" && xml::namespace(&elements, *j) == Some(SECURITY_NAMESPACE)
            })
            .map(|(_, u)| u)
            .collect();
        let mut seen: Vec<(&str, &str, &str)> = Vec::new();
        let ant_patterns =
            e.attr("request-matcher").is_none_or(|a| a.value == "ant" || a.value == "mvc") && e.attr("request-matcher-ref").is_none();
        for (n, u) in urls.iter().enumerate() {
            if let Some(access) = u.attr("access").filter(|_| expressions) {
                let exact = !access.value.contains('&');
                for (from, to, code, severity, message) in expression_problems(&access.value) {
                    let (from, to) =
                        if exact { (access.value_at + from, access.value_at + to) } else { (access.value_at, access.value_end) };
                    out.push(diagnostic(from, to, severity, code, message));
                }
            }
            let Some(pattern) = u.attr("pattern") else {
                continue;
            };
            let key = (pattern.value.as_str(), u.value("method").unwrap_or(""), u.value("servlet-path").unwrap_or(""));
            if seen.contains(&key) {
                let message = format!("The pattern `{}` is already matched by an earlier <intercept-url>.", pattern.value);
                out.push(diagnostic(pattern.value_at, pattern.value_end, Severity::Warning, "duplicate-pattern", message));
            }
            seen.push(key);
            let catch_all = ant_patterns
                && matches!(pattern.value.as_str(), "/**" | "**")
                && u.attr("method").is_none()
                && u.attr("servlet-path").is_none();
            if catch_all && urls[n + 1..].iter().any(|later| later.value("pattern").is_some_and(|p| p != pattern.value)) {
                let message = "This catch-all pattern comes before more specific ones, which can never match: move it last.".to_owned();
                out.push(diagnostic(pattern.value_at, pattern.value_end, Severity::Warning, "unreachable-patterns", message));
            }
        }
    }
    out
}

const MATCHER_CALLS: [&str; 4] = ["requestMatchers", "antMatchers", "mvcMatchers", "regexMatchers"];
const RULE_BLOCKS: [&str; 2] = ["authorizeHttpRequests", "authorizeRequests"];

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

/// The index of the bracket closing the one at `open`, in comment- and string-free code.
fn closing(code: &str, open: usize) -> usize {
    let mut depth = 0;
    for (i, b) in code.bytes().enumerate().skip(open) {
        match b {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth -= 1;
                if depth == 0 {
                    return i;
                }
            }
            _ => {}
        }
    }
    code.len()
}

/// Where a legacy `authorizeRequests().a().b();` chain ends.
fn statement_end(code: &str, from: usize) -> usize {
    let mut depth = 0;
    for (i, b) in code.bytes().enumerate().skip(from) {
        match b {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' if depth == 0 => return i,
            b')' | b']' | b'}' => depth -= 1,
            b';' if depth == 0 => return i,
            _ => {}
        }
    }
    code.len()
}

/// Byte ranges of the top-level arguments between the brackets `open..close`.
fn argument_ranges(code: &str, open: usize, close: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let (mut depth, mut from) = (0, open + 1);
    for (i, b) in code.bytes().enumerate().take(close).skip(open + 1) {
        match b {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b',' if depth == 0 => {
                out.push((from, i));
                from = i + 1;
            }
            _ => {}
        }
    }
    if from < close {
        out.push((from, close));
    }
    out
}

fn trimmed(text: &str, (from, to): (usize, usize)) -> (usize, usize) {
    let slice = &text[from..to];
    let start = from + (slice.len() - slice.trim_start().len());
    (start, start + slice.trim().len())
}

struct Rule {
    at: usize,
    name_end: usize,
    any_request: bool,
    method: Option<String>,
    /// Ant-style literal patterns: start, end (inside the quotes), value.
    patterns: Vec<(usize, usize, String)>,
    access: Option<String>,
}

/// The `HttpMethod` and the literal patterns that start an argument list; stops at the first dynamic argument.
fn leading_patterns(text: &str, args: &[(usize, usize)]) -> (Option<String>, Vec<(usize, usize, String)>) {
    let mut method = None;
    let mut patterns = Vec::new();
    for (n, range) in args.iter().enumerate() {
        let (from, to) = trimmed(text, *range);
        let raw = &text[from..to];
        if n == 0
            && let Some(name) = raw.strip_prefix("HttpMethod.")
        {
            method = Some(name.to_owned());
            continue;
        }
        let Some(inner) = raw.strip_prefix('"').and_then(|r| r.strip_suffix('"')) else {
            break;
        };
        if inner.contains(['"', '\\', '$']) {
            break;
        }
        patterns.push((from + 1, to - 1, inner.to_owned()));
    }
    (method, patterns)
}

fn pattern_covers(broad: &str, narrow: &str) -> bool {
    if broad == narrow || broad == "/**" || broad == "**" {
        return true;
    }
    let Some(prefix) = broad.strip_suffix("**") else {
        return false;
    };
    prefix.ends_with('/') && !prefix.contains(['*', '?', '{']) && (narrow.starts_with(prefix) || narrow == prefix.trim_end_matches('/'))
}

/// The access call chained right after a matcher call, such as `permitAll()` or `hasRole("X")`.
fn chained_access(text: &str, code: &str, from: usize) -> Option<String> {
    let rest = code.get(from..)?.trim_start().strip_prefix('.')?;
    let start = code.len() - rest.len();
    let name_end = start + rest.find(|c: char| !(c.is_alphanumeric() || c == '_'))?;
    let after = code[name_end..].trim_start();
    let open = code.len() - after.len();
    let end = if after.starts_with('(') { closing(code, open) + 1 } else { name_end };
    Some(text[start..end].split_whitespace().collect::<Vec<_>>().join(" "))
}

/// The rules of one `authorizeHttpRequests`/`authorizeRequests` configuration, in source order.
fn rules_in(text: &str, code: &str, kotlin: bool, scope: (usize, usize)) -> Vec<Rule> {
    let mut out = Vec::new();
    let bytes = code.as_bytes();
    let mut i = scope.0;
    while i < scope.1 {
        if !is_ident_byte(bytes[i]) || (i > 0 && is_ident_byte(bytes[i - 1])) {
            i += 1;
            continue;
        }
        let end = (i..scope.1).find(|&j| !is_ident_byte(bytes[j])).unwrap_or(scope.1);
        let name = &code[i..end];
        let open = end + code[end..].len() - code[end..].trim_start().len();
        let has_args = bytes.get(open) == Some(&b'(');
        let args = || {
            let close = closing(code, open);
            (close, argument_ranges(code, open, close))
        };
        let rule = |any_request: bool, method, patterns| Rule { at: i, name_end: end, any_request, method, patterns, access: None };
        if name == "anyRequest" {
            out.push(rule(true, None, Vec::new()));
        } else if has_args && MATCHER_CALLS.contains(&name) {
            let (close, ranges) = args();
            let (method, patterns) = if name == "regexMatchers" { (None, Vec::new()) } else { leading_patterns(text, &ranges) };
            let mut matcher = rule(false, method, patterns);
            matcher.access = chained_access(text, code, close + 1);
            out.push(matcher);
        } else if kotlin && has_args && name == "authorize" {
            let (close, ranges) = args();
            if ranges.first().is_some_and(|r| text[trimmed(text, *r).0..trimmed(text, *r).1] == *"anyRequest") {
                out.push(rule(true, None, Vec::new()));
            } else {
                let (method, patterns) = leading_patterns(text, &ranges);
                let mut matcher = rule(false, method, patterns);
                matcher.access = ranges.get(1).map(|r| text[trimmed(text, *r).0..trimmed(text, *r).1].to_owned());
                out.push(matcher);
            }
            i = close;
            continue;
        }
        i = end;
    }
    out
}

fn matcher_problems(text: &str, code: &str, kotlin: bool) -> Vec<Problem> {
    let mut out = Vec::new();
    let mut warn = |from: usize, to: usize, code: &str, message: String| {
        out.push(Problem { diagnostic: problem(text, from, to, Severity::Warning, code, message) });
    };
    for block in RULE_BLOCKS {
        for (at, _) in code.match_indices(block) {
            let after = at + block.len();
            if at > 0 && is_ident_byte(code.as_bytes()[at - 1]) || code.as_bytes().get(after).is_some_and(|b| is_ident_byte(*b)) {
                continue;
            }
            let open = after + code[after..].len() - code[after..].trim_start().len();
            let scope = match code.as_bytes().get(open) {
                Some(b'{') => (open + 1, closing(code, open)),
                Some(b'(') => {
                    let close = closing(code, open);
                    if code[open + 1..close].trim().is_empty() { (close + 1, statement_end(code, close + 1)) } else { (open + 1, close) }
                }
                _ => continue,
            };
            let mut any_request = false;
            let mut earlier: Vec<&Rule> = Vec::new();
            let rules = rules_in(text, code, kotlin, scope);
            for rule in &rules {
                if any_request {
                    let message = "Spring Security throws IllegalStateException at startup (`Can't configure requestMatchers after anyRequest`): `anyRequest()` must be the last rule.";
                    warn(rule.at, rule.name_end, "matchers-after-any-request", message.to_owned());
                    continue;
                }
                any_request = rule.any_request;
                for (from, to, pattern) in &rule.patterns {
                    let covering = earlier.iter().find(|e| {
                        (e.method.is_none() || e.method == rule.method)
                            && e.patterns.iter().any(|(_, _, broad)| pattern_covers(broad, pattern))
                    });
                    if let Some(first) = covering.and_then(|e| e.patterns.iter().find(|(_, _, broad)| pattern_covers(broad, pattern))) {
                        let earlier_access = covering.and_then(|e| e.access.as_deref());
                        let message = match (earlier_access, rule.access.as_deref()) {
                            (Some(first_access), Some(later)) if first.2 == *pattern && first_access != later => format!(
                                "`{pattern}` is matched twice with conflicting access: the earlier `{first_access}` wins and the later `{later}` is unreachable."
                            ),
                            _ => format!(
                                "`{pattern}` is never reached: the earlier rule for `{}` already matches it. Put the narrower pattern first.",
                                first.2
                            ),
                        };
                        warn(*from, *to, "matcher-order", message);
                    }
                }
                earlier.push(rule);
            }
        }
    }
    out
}

fn debug_problems(text: &str, code: &str) -> Vec<Problem> {
    let mut out = Vec::new();
    let mut warn = |from: usize, to: usize| {
        let message = "Security debug logging is on: it logs request details and headers. Turn it off in production.";
        out.push(Problem { diagnostic: problem(text, from, to, Severity::Warning, "debug-enabled", message.into()) });
    };
    for (at, _) in code.match_indices("@EnableWebSecurity") {
        let after = at + "@EnableWebSecurity".len();
        let open = after + code[after..].len() - code[after..].trim_start().len();
        if code.as_bytes().get(open) != Some(&b'(') {
            continue;
        }
        let close = closing(code, open);
        let debug_on = argument_ranges(code, open, close).into_iter().any(|(from, to)| {
            code[from..to].trim().strip_prefix("debug").and_then(|r| r.trim_start().strip_prefix('=')).is_some_and(|v| v.trim() == "true")
        });
        if debug_on {
            warn(at, (close + 1).min(text.len()));
        }
    }
    if code.contains("WebSecurityCustomizer") {
        for (at, _) in code.match_indices(".debug(") {
            let open = at + ".debug".len();
            let close = closing(code, open);
            if code[open + 1..close].trim() == "true" {
                warn(at + 1, close + 1);
            }
        }
    }
    out
}

fn problems(path: &str, text: &str) -> Vec<Problem> {
    if path.ends_with(".xml") {
        return xml_problems(text);
    }
    let file = spring_text::parse_source(path, text);
    let mut out = Vec::new();
    for e in file.types.iter().flat_map(|t| &t.extends).filter(|e| e.name == "WebSecurityConfigurerAdapter") {
        let message = "WebSecurityConfigurerAdapter was removed in Spring Security 6: declare a SecurityFilterChain @Bean instead.";
        out.push(Problem {
            diagnostic: problem(text, e.at, e.at + e.name.len(), Severity::Warning, "web-security-configurer-adapter", message.into()),
        });
    }
    for a in annotations(&file) {
        if a.name == "EnableGlobalMethodSecurity" {
            let message = "@EnableGlobalMethodSecurity is deprecated: use @EnableMethodSecurity (pre/post annotations are on by default).";
            let mut diagnostic = problem(text, a.at, a.end, Severity::Warning, "enable-global-method-security", message.into());
            diagnostic.deprecated = true;
            out.push(Problem { diagnostic });
        }
        if METHOD_SECURITY.contains(&a.name.as_str()) {
            for s in a.strings(&["value"]).into_iter().filter(|s| s.value.len() == s.end - s.at) {
                for (at, role) in spel_roles(&s.value) {
                    if let Some(bare) = role.strip_prefix(ROLE_PREFIX) {
                        let mut found = role_prefix(text, s.at + at, bare);
                        found.diagnostic.severity = Severity::Info;
                        found.diagnostic.message = spel_role_prefix_message(bare);
                        out.push(found);
                    }
                }
            }
        }
    }
    out.extend(disabled_method_annotations(text, &file));
    let security = file.imports.iter().any(|i| i.path.starts_with("org.springframework.security"));
    out.extend(method_expression_problems(text, &file));
    for c in spring_text::source_calls(path, text) {
        let name_end = c.at + c.name.len();
        let renamed = match c.name.as_str() {
            "antMatchers" | "mvcMatchers" | "regexMatchers" if security => Some("requestMatchers"),
            "authorizeRequests" if security => Some("authorizeHttpRequests"),
            _ => None,
        };
        if renamed.is_some() {
            let (message, deprecated) = match c.name.as_str() {
                "authorizeRequests" => ("`authorizeRequests` is deprecated: use `authorizeHttpRequests`.".to_owned(), true),
                "regexMatchers" => (
                    "`regexMatchers` was removed in Spring Security 6: use `requestMatchers(RegexRequestMatcher.regexMatcher(…))`.".into(),
                    false,
                ),
                old => (format!("`{old}` was removed in Spring Security 6: use `requestMatchers`."), false),
            };
            let code = if deprecated { "authorize-requests" } else { "removed-request-matcher" };
            let mut diagnostic = problem(text, c.at, name_end, Severity::Warning, code, message);
            diagnostic.deprecated = deprecated;
            out.push(Problem { diagnostic });
        }
        if security && (c.name == "hasRole" || c.name == "hasAnyRole") {
            for s in c.args.iter().filter_map(|a| a.string.as_ref()) {
                if let Some(bare) = s.value.strip_prefix(ROLE_PREFIX)
                    && s.value.len() == s.end - s.at
                {
                    out.push(role_prefix(text, s.at, bare));
                }
            }
        }
        if c.name == "getInstance" && c.receiver.as_deref() == Some("NoOpPasswordEncoder") {
            let message = "NoOpPasswordEncoder keeps passwords in plain text: use PasswordEncoderFactories.createDelegatingPasswordEncoder() or BCryptPasswordEncoder.";
            out.push(Problem {
                diagnostic: problem(
                    text,
                    text[..c.at].rfind("NoOpPasswordEncoder").unwrap_or(c.at),
                    c.end,
                    Severity::Warning,
                    "no-op-password-encoder",
                    message.into(),
                ),
                });
        }
        if c.name == "password"
            && let Some(s) = c.args.first().and_then(|a| a.string.as_ref())
            && s.value.starts_with("{noop}")
        {
            let message = "This `{noop}` password is stored and compared in plain text.".to_owned();
            out.push(Problem { diagnostic: problem(text, s.at, s.end, Severity::Info, "plain-text-password", message) });
        }
    }
    let kotlin = spring_text::is_kotlin(path);
    if security && !kotlin && host::setting_bool("springSecurity.checkSelfInvocation").unwrap_or(false) {
        out.extend(self_invocations(text, &file));
    }
    let code = if security { code_only(text, kotlin) } else { String::new() };
    if security {
        if host::setting_bool("springSecurity.checkMatchers").unwrap_or(true) {
            out.extend(matcher_problems(text, &code, kotlin));
        }
        if host::setting_bool("springSecurity.checkDebug").unwrap_or(true) {
            out.extend(debug_problems(text, &code));
        }
    }
    if security && kotlin {
        for (at, _) in code.match_indices("authorizeRequests") {
            let after = at + "authorizeRequests".len();
            let standalone = !code[..at].ends_with(|c: char| c.is_alphanumeric() || c == '_');
            if standalone && code[after..].trim_start().starts_with('{') {
                let message = "`authorizeRequests` is deprecated: use `authorizeHttpRequests`.".to_owned();
                let mut diagnostic = problem(text, at, after, Severity::Warning, "authorize-requests", message);
                diagnostic.deprecated = true;
                out.push(Problem { diagnostic });
            }
        }
        out.sort_by_key(|p| p.diagnostic.range.start);
    }
    out
}

/// `#name` variables of a SpEL expression: byte offset of the name and the name; skips string literals.
fn spel_variables(expr: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let bytes = expr.as_bytes();
    let mut quote = None;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
            }
            None if c == b'\'' || c == b'"' => quote = Some(c),
            None if c == b'#' && bytes.get(i + 1).is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_') => {
                let end = (i + 1..bytes.len()).find(|&j| !is_ident_byte(bytes[j])).unwrap_or(bytes.len());
                out.push((i + 1, &expr[i + 1..end]));
                i = end;
                continue;
            }
            None => {}
        }
        i += 1;
    }
    out
}

/// `@Secured`/`@RolesAllowed` next to an `@EnableMethodSecurity` of the same file that leaves their switch at its default `false`.
fn disabled_method_annotations(text: &str, file: &JavaFile) -> Vec<Problem> {
    let methods: Vec<&Annotation> = annotations(file).filter(|a| a.name == "EnableMethodSecurity").collect();
    let switched_on = |switch: &str| methods.iter().any(|e| e.arg(switch).is_some_and(|v| v.text.trim() != "false"));
    let mut out = Vec::new();
    for a in annotations(file) {
        let (switch, label) = match a.name.as_str() {
            "Secured" => ("securedEnabled", "@Secured"),
            "RolesAllowed" => ("jsr250Enabled", "@RolesAllowed"),
            _ => continue,
        };
        if methods.is_empty() || switched_on(switch) {
            continue;
        }
        let message = format!("{label} is ignored unless `@EnableMethodSecurity({switch} = true)`: it defaults to false.");
        out.push(Problem { diagnostic: problem(text, a.at, a.end, Severity::Info, "method-security-disabled", message) });
    }
    out
}

/// Literal SpEL of `@PreAuthorize`/`@PostAuthorize`: balanced syntax, argument-less role/authority functions
/// and `#name` references that no parameter of the method (nor its `@P`/`@Param`) declares.
fn method_expression_problems(text: &str, file: &JavaFile) -> Vec<Problem> {
    let mut out = Vec::new();
    for member in file.types.iter().flat_map(|t| &t.members) {
        let mut names: BTreeSet<&str> = member.params.iter().map(|p| p.name.as_str()).collect();
        let mut names_known = true;
        for param in &member.params {
            for alias in param.annotations.iter().filter(|a| a.name == "P" || a.name == "Param") {
                let literals = alias.strings(&["value"]);
                names_known &= !literals.is_empty();
                names.extend(literals.iter().map(|s| s.value.as_str()));
            }
        }
        for a in member.annotations.iter().filter(|a| a.name == "PreAuthorize" || a.name == "PostAuthorize") {
            for s in a.strings(&["value"]).into_iter().filter(|s| s.value.len() == s.end - s.at) {
                for (from, to, code, severity, message) in expression_problems(&s.value) {
                    if code == "unbalanced-expression" || code == "empty-security-function" {
                        out.push(Problem { diagnostic: problem(text, s.at + from, s.at + to, severity, code, message) });
                    }
                }
                if is_unbalanced(&s.value) || !names_known {
                    continue;
                }
                for (at, name) in spel_variables(&s.value) {
                    let assigned = s.value[at + name.len()..].trim_start().strip_prefix('=').is_some_and(|rest| !rest.starts_with('='));
                    if matches!(name, "this" | "root") || names.contains(name) || assigned {
                        continue;
                    }
                    let message = format!("`#{name}` does not match a parameter of this method (Spring Security resolves parameter names, or `@P`/`@Param` values).");
                    let diagnostic = problem(text, s.at + at - 1, s.at + at + name.len(), Severity::Warning, "unknown-method-parameter", message);
                    out.push(Problem { diagnostic });
                }
            }
        }
    }
    out
}

const BEAN_CLASSES: [&str; 6] = ["Component", "Service", "Repository", "Controller", "RestController", "Configuration"];

/// Calls of a bean's method protected by method security from another method of the same class, which skip the proxy.
fn self_invocations(text: &str, file: &JavaFile) -> Vec<Problem> {
    let mut out = Vec::new();
    for ty in file.types.iter().filter(|t| t.kind == "class" && t.has(&BEAN_CLASSES)) {
        for call in sibling_calls(ty, text, |m| m.has(&METHOD_SECURITY), |_, _| false) {
            let annotation =
                call.callee.annotations.iter().find(|a| METHOD_SECURITY.contains(&a.name.as_str())).map_or("", |a| a.name.as_str());
            let message = format!(
                "`{}` is protected by @{annotation}, but this call stays inside the class and skips the security proxy.",
                call.callee.name
            );
            out.push(Problem {
                diagnostic: problem(text, call.at, call.end, Severity::Warning, "security-self-invocation", message),
                });
        }
    }
    out
}

struct BeanNames {
    key: (u64, u64),
    incomplete: bool,
    names: Rc<BTreeSet<String>>,
}

pub struct SecurityTools {
    actions: actions::State,
    watching: bool,
    events_notice: std::sync::atomic::AtomicBool,
    roles: Indexes<Vec<String>>,
    beans: Indexes<Vec<String>>,
    mappings: Indexes<Vec<endpoints::Mapping>>,
    bean_names: Option<BeanNames>,
}

impl Default for SecurityTools {
    fn default() -> Self {
        Self {
            actions: actions::State::default(),
            watching: false,
            events_notice: Default::default(),
            roles: Indexes::new(&["**/*.java"], roles),
            beans: Indexes::new(&beans::GLOBS, |path, text| {
                if text.len() > 128 << 10 { Some(vec!["\0incomplete".into()]) } else { refs::bean_names(path, text) }
            }),
            mappings: Indexes::new(SOURCE_GLOBS, endpoints::mappings),
            bean_names: None,
        }
    }
}

impl SecurityTools {
    fn project_bean_names(&mut self, doc: &Document, check_beans: bool) -> Option<Rc<BTreeSet<String>>> {
        if !check_beans || !self.watching {
            return None;
        }
        let index = self.beans.for_cached_document_warm(&doc.path, &doc.text, DIAGNOSTICS_WARM)?;
        if !index.fresh() {
            return None;
        }
        let key = (index.id(), index.generation());
        if self.bean_names.as_ref().is_none_or(|cached| cached.key != key) {
            self.bean_names = Some(BeanNames {
                key,
                incomplete: index.iter().any(|(_, names)| names.iter().any(|n| n == "\0incomplete")),
                names: Rc::new(index.iter().flat_map(|(_, n)| n.clone()).collect()),
            });
        }
        let cached = self.bean_names.as_ref()?;
        if cached.incomplete {
            return None;
        }
        Some(Rc::clone(&cached.names))
    }

    /// Table checks of the security XML and, once every bean of the project was read, unknown bean references.
    fn reference_findings(&mut self, doc: &Document) -> Vec<Diagnostic> {
        let xml = doc.path.ends_with(".xml");
        if !xml && !doc.text.contains("WithUserDetails") && !doc.text.contains('@') {
            return Vec::new();
        }
        let check_beans = host::setting_bool("springSecurity.checkBeanReferences").unwrap_or(true);
        let names = self.project_bean_names(doc, check_beans);
        let is_bean = |name: &str| names.as_ref().is_some_and(|n| n.contains(name));
        let found = match (xml, names.is_some()) {
            (true, true) => refs::xml_rules(&doc.text, Some(&is_bean)),
            (true, false) => refs::xml_rules(&doc.text, None),
            (false, true) => refs::source_references(&doc.path, &doc.text, &is_bean),
            (false, false) => Vec::new(),
        };
        found.iter().map(|f| f.diagnostic(&doc.text)).collect()
    }


}

fn overlaps(a: Range, b: Range) -> bool {
    a.start <= b.end && b.start <= a.end
}

impl Providers for SecurityTools {
    fn line_markers(&mut self, doc: &Document) -> Vec<LineMarker> {
        if doc.text.len() <= 128 << 10 {
            let _ = self.beans.for_cached_document_warm(&doc.path, &doc.text, DIAGNOSTICS_WARM);
            let _ = self.mappings.for_cached_document_warm(&doc.path, &doc.text, DIAGNOSTICS_WARM);
        }
        Vec::new()
    }

    fn definition(&mut self, doc: &Document, at: Position) -> Vec<Location> {
        if doc.text.len() > 128 << 10 { return Vec::new(); }
        if doc.language == "xml" && !spring_text::xml::analysis_valid(&doc.text) { return vec![]; }
        if let Some(targets) = ownership::definition(doc,at) {return targets;}
        let Some(off) = offset(&doc.text, at) else {
            return Vec::new();
        };
        // A modern Java String requestMatchers call does not establish Ant/MVC/
        // PathPattern strategy. Do not run the Ant approximation over that spelling.
        // Only explicit XML Ant declarations with proved namespace/parent ownership
        // and a deliberately small pattern subset supply navigation candidates.
        if doc.language != "xml" || !doc.path.ends_with(".xml") || doc.text.len() > 128 << 10 {
            return Vec::new();
        }
        let elements = xml::elements(&doc.text);
        let patterns: Vec<(Option<String>, String)> = elements.iter().enumerate().filter_map(|(i,e)| {
            if e.local() != "intercept-url" || xml::namespace(&elements,i) != Some(SECURITY_NAMESPACE) { return None; }
            let parent_index = e.parent?;
            let parent = elements.get(parent_index)?;
            if parent.local() != "http" || xml::namespace(&elements,parent_index) != Some(SECURITY_NAMESPACE)
                || parent.value("request-matcher") != Some("ant")
                || [parent,e].iter().any(|e| e.attr("request-matcher-ref").is_some() || e.attr("servlet-path").is_some())
            { return None; }
            let pattern = e.attr("pattern").filter(|a| a.holds(off))?;
            let parts:Vec<_> = pattern.value.split('/').collect();
            if !pattern.value.starts_with('/') || pattern.value.len() > 256
                || pattern.value.contains("//") || pattern.value.len() > 1 && pattern.value.ends_with('/')
                || !parts.iter().enumerate().all(|(i,p)| {
                    *p == "*" || *p == "**" && i + 1 == parts.len()
                        || !matches!(*p,"."|"..") && p.bytes().all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
                })
            { return None; }
            let method = e.value("method");
            if method.is_some_and(|m| !matches!(m,"GET"|"POST"|"PUT"|"DELETE"|"HEAD"|"OPTIONS"|"TRACE"|"PATCH")) { return None; }
            Some((method.map(str::to_owned),pattern.value.clone()))
        }).collect();
        if patterns.is_empty() {return Vec::new();}
        let Some(index) = self.mappings.for_document(&doc.path, &doc.text, PROVIDER_BUDGET) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (verb, pattern) in patterns {
            for (path, mappings) in index.iter() {
                for m in mappings
                    .iter()
                    .filter(|m| verb.as_deref().is_none_or(|v| m.verb.is_empty() || m.verb == v) && endpoints::covers(&pattern, &m.path))
                {
                    out.push(Location { path: path.to_owned(), range: m.range });
                }
            }
        }
        out
    }

    fn code_actions(&mut self, _doc: &Document, _selection: Range) -> Vec<CodeAction> {
        // Legacy lexical migration/role suggestions cannot establish receiver,
        // prefix, defaults or version semantics. Never bypass guarded FileActions.
        Vec::new()
    }

    fn completion(&mut self, doc: &Document, at: Position, _trigger: Option<&str>) -> Vec<CompletionItem> {
        let Some(off) = offset(&doc.text, at) else {
            return Vec::new();
        };
        if spring_text::is_kotlin(&doc.path) {
            return Vec::new();
        }
        let file = java::parse(&doc.text);
        let Some((annotation, s)) = annotations(&file)
            .filter(|a| METHOD_SECURITY.contains(&a.name.as_str()))
            .find_map(|a| a.strings(&["value"]).into_iter().find(|s| s.holds(off)).map(|s| (a.name.clone(), s.clone())))
        else {
            return Vec::new();
        };
        let before = &doc.text[s.at..off];
        let open_call = ["hasRole(", "hasAnyRole("].iter().filter_map(|c| before.rfind(c)).max();
        let in_role = open_call.is_some_and(|p| !before[p..].contains(')') && before[p..].matches('\'').count() % 2 == 1);
        if in_role {
            let Some(index) = self.roles.for_document(&doc.path, &doc.text, PROVIDER_BUDGET) else {
                return Vec::new();
            };
            let roles: BTreeSet<&String> = index.iter().flat_map(|(_, r)| r).collect();
            return roles
                .into_iter()
                .map(|r| CompletionItem { detail: Some("role".into()), ..CompletionItem::new(r, CompletionKind::Value) })
                .collect();
        }
        SPEL.iter()
            .filter(|(_, _, only)| only.is_empty() || only.contains(&annotation.as_str()))
            .map(|(label, snippet, _)| CompletionItem {
                insert_text: (*snippet).into(),
                snippet: snippet.contains('$'),
                filter_text: Some((*label).into()),
                ..CompletionItem::new(snippet.replace("$1", "…").replace("$2", "…").as_str(), CompletionKind::Function)
            })
            .collect()
    }
}

impl Plugin for SecurityTools {
    fn activate(&mut self, _ctx: &openidea_plugin::Context) -> Result<(), Error> {
        self.activate_live();
        Ok(())
    }
    fn handle_events(&mut self, events: &[openidea_plugin::Event], _ctx: &Invocation) {
        self.live_events(events);
    }
    fn run_command(&mut self, id: &str, _ctx: &Invocation, _arg: Option<&str>) -> Result<Option<String>, Error> {
        Err(Error::NotFound(format!("command {id}")))
    }
}

register_plugin!(SecurityTools, providers, diagnostics, hover, inlay_hints, file_actions, injections);

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;

    fn inspect(host: &MockHost, tools: &mut SecurityTools, doc: &Document) -> Vec<LineMarker> {
        tools.activate_live();
        host.open_editor(&doc.path, &doc.language, &doc.text, &[]);
        host.refresh_diagnostics(tools);
        assert!(host.diagnostics().is_empty(), "no manual duplicate publication");
        tools.line_markers(doc)
    }

    const CONFIG: &str = r#"package com.acme;

import org.springframework.security.config.annotation.method.configuration.EnableGlobalMethodSecurity;
import org.springframework.security.config.annotation.web.builders.HttpSecurity;

@Configuration
@EnableGlobalMethodSecurity(prePostEnabled = true)
public class SecurityConfig extends WebSecurityConfigurerAdapter {
    @Override
    protected void configure(HttpSecurity http) throws Exception {
        http.authorizeRequests()
            .antMatchers("/admin/**").hasRole("ROLE_ADMIN")
            .mvcMatchers("/api/**").hasAnyRole("USER", "ROLE_STAFF")
            .regexMatchers("/x.*").permitAll();
    }

    @Bean
    PasswordEncoder encoder() { return NoOpPasswordEncoder.getInstance(); }

    @Bean
    UserDetailsService users() {
        return new InMemoryUserDetailsManager(User.withUsername("u").password("{noop}secret").roles("USER").build());
    }
}
"#;
    const SERVICE: &str = r#"package com.acme;

@Service
class Orders {
    @PreAuthorize("hasRole('ROLE_ADMIN') or hasAnyRole('MANAGER', 'ROLE_AUDITOR')")
    void cancel() {}

    @Secured({"ROLE_BILLING"})
    void bill() {}

    @PostAuthorize("returnObject.owner == authentication.name")
    Order get() { return null; }
}
"#;

    fn doc(name: &str, text: &str) -> Document {
        Document { path: format!("/project/src/main/java/com/acme/{name}"), language: "java".into(), version: 1, text: text.into() }
    }

    fn found(host: &MockHost, name: &str, text: &str) -> Vec<(String, String)> {
        host.provider_diagnostics()[&format!("/project/src/main/java/com/acme/{name}")]
            .iter()
            .map(|d| {
                let span = &text[offset(text, d.range.start).unwrap()..offset(text, d.range.end).unwrap()];
                (d.code.clone().unwrap(), span.to_owned())
            })
            .collect()
    }

    #[test]
    fn flags_removed_and_risky_configuration() {
        let host = MockHost::install();
        let mut tools = SecurityTools::default();
        assert!(inspect(&host, &mut tools, &doc("SecurityConfig.java", CONFIG)).is_empty());
        let codes = found(&host, "SecurityConfig.java", CONFIG);
        let codes: Vec<(&str, &str)> = codes.iter().map(|(c, s)| (c.as_str(), s.as_str())).collect();
        assert_eq!(
            codes,
            [
                ("web-security-configurer-adapter", "WebSecurityConfigurerAdapter"),
                ("enable-global-method-security", "@EnableGlobalMethodSecurity(prePostEnabled = true)"),
                ("authorize-requests", "authorizeRequests"),
                ("removed-request-matcher", "antMatchers"),
                ("role-prefix", "ROLE_ADMIN"),
                ("removed-request-matcher", "mvcMatchers"),
                ("role-prefix", "ROLE_STAFF"),
                ("removed-request-matcher", "regexMatchers"),
                ("no-op-password-encoder", "NoOpPasswordEncoder.getInstance()"),
                ("plain-text-password", "{noop}secret"),
            ]
        );
        assert!(host.provider_diagnostics()["/project/src/main/java/com/acme/SecurityConfig.java"][1].deprecated);

        inspect(&host, &mut tools, &doc("Orders.java", SERVICE));
        let codes = found(&host, "Orders.java", SERVICE);
        assert_eq!(codes, [("role-prefix".to_owned(), "ROLE_ADMIN".to_owned()), ("role-prefix".into(), "ROLE_AUDITOR".into())]);
    }

    #[test]
    fn legacy_annotation_defaults_and_foreign_receivers_never_offer_unreviewed_edits() {
        let _h=MockHost::install();let mut tools=SecurityTools::default();
        for text in [
            "import org.springframework.security.config.annotation.method.configuration.EnableGlobalMethodSecurity; @EnableGlobalMethodSecurity class S {}",
            "import foreign.EnableGlobalMethodSecurity; @EnableGlobalMethodSecurity class S {}",
            "import org.springframework.security.config.annotation.web.configuration.EnableWebSecurity; class S {void f(Custom c){c.antMatchers(\"/api\");c.hasRole(\"ROLE_ADMIN\");}}",
        ] {
            let d=doc("Security.java",text);
            assert!(tools.code_actions(&d,Range::new(Position::new(0,0),Position::new(u32::MAX,u32::MAX))).is_empty(),"{text}");
        }
    }
    #[test]
    fn legacy_diagnostics_remain_without_automatic_semantic_migration() {
        let _host=MockHost::install();let mut tools=SecurityTools::default();
        let d=doc("SecurityConfig.java",CONFIG);
        for needle in ["@EnableGlobal","antMatchers","authorizeRequests","ROLE_STAFF","regexMatchers"] {
            let at=openidea_plugin::text::position(CONFIG,CONFIG.find(needle).unwrap()+1);
            assert!(tools.code_actions(&d,Range::new(at,at)).is_empty(),"{needle}");
        }
        let codes: Vec<_>=problems(&d.path,&d.text).into_iter().filter_map(|p|p.diagnostic.code).collect();
        for code in ["enable-global-method-security","removed-request-matcher","authorize-requests","role-prefix"] {assert!(codes.iter().any(|c|c==code));}
    }

    #[test]
    fn completes_security_expressions_and_known_roles() {
        let host = MockHost::install();
        host.add_file("src/main/java/com/acme/Orders.java", SERVICE);
        let mut tools = SecurityTools::default();
        let text = "class A {\n    @PostFilter(\"\")\n    List<Order> all() { return null; }\n}\n";
        let items = tools.completion(&doc("A.java", text), Position::new(1, 17), None);
        let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"hasRole('…')"));
        assert!(labels.contains(&"filterObject"));
        assert!(labels.contains(&"returnObject"));
        let has_role = items.iter().find(|i| i.label == "hasRole('…')").unwrap();
        assert_eq!((has_role.insert_text.as_str(), has_role.snippet), ("hasRole('$1')", true));

        let text = "class A {\n    @PreAuthorize(\"isAuthenticated() and hasRole('\")\n    void f() {}\n}\n";
        let items = tools.completion(&doc("A.java", text), Position::new(1, 50), None);
        let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, ["ADMIN", "AUDITOR", "BILLING", "MANAGER"]);

        assert!(tools.completion(&doc("A.java", "class A { String s = \"\"; }"), Position::new(0, 22), None).is_empty());
    }

    const SECURITY_XML: &str = r#"<beans xmlns="http://www.springframework.org/schema/beans"
       xmlns:sec="http://www.springframework.org/schema/security">
    <sec:http pattern="/api/**">
        <sec:intercept-url pattern="/api/**" access="hasRole('ROLE_API')"/>
        <sec:intercept-url pattern="/api/**" access="permitAll"/>
    </sec:http>
    <sec:http>
        <sec:intercept-url pattern="/admin/**" access="hasRole('ADMIN') and hasRoles('X')"/>
        <sec:intercept-url pattern="/a" access="hasRole()"/>
        <sec:intercept-url pattern="/b" access="hasAnyRole('A', 'ROLE_B"/>
        <sec:intercept-url pattern="/c" access="(isAuthenticated()"/>
        <sec:intercept-url pattern="/ok" access="hasAuthority(&apos;x&apos;) and authentication.name.isEmpty() and hasIpAddress('10.0.0.0/8')"/>
        <sec:intercept-url pattern="/**" access="permitAll"/>
        <sec:intercept-url pattern="/late" access="denyAll"/>
    </sec:http>
    <sec:http use-expressions="false">
        <sec:intercept-url pattern="/x" access="ROLE_X,ROLE_Y"/>
    </sec:http>
    <sec:global-method-security pre-post-annotations="enabled"/>
</beans>
"#;

    #[test]
    fn intercept_url_pattern_and_access_are_checked() {
        let host = MockHost::install();
        let mut tools = SecurityTools::default();
        let d = Document {
            path: "/project/src/main/resources/security.xml".into(),
            language: "xml".into(),
            version: 1,
            text: SECURITY_XML.into(),
        };
        inspect(&host, &mut tools, &d);
        let found: Vec<(String, String)> = host.provider_diagnostics()[&d.path]
            .iter()
            .map(|x| {
                let span = &SECURITY_XML[offset(SECURITY_XML, x.range.start).unwrap()..offset(SECURITY_XML, x.range.end).unwrap()];
                (x.code.clone().unwrap(), span.to_owned())
            })
            .collect();
        let found: Vec<(&str, &str)> = found.iter().map(|(c, s)| (c.as_str(), s.as_str())).collect();
        assert_eq!(
            found,
            [
                ("role-prefix", "ROLE_API"),
                ("duplicate-pattern", "/api/**"),
                ("unknown-security-function", "hasRoles"),
                ("empty-security-function", "hasRole()"),
                ("unbalanced-expression", "hasAnyRole('A', 'ROLE_B"),
                ("unbalanced-expression", "(isAuthenticated()"),
                ("unreachable-patterns", "/**"),
                ("use-expressions-false", "use-expressions=\"false\""),
                ("global-method-security", "sec:global-method-security"),
            ]
        );
        assert!(host.provider_diagnostics()[&d.path][8].deprecated);

        let plain = "<beans xmlns=\"http://www.springframework.org/schema/beans\"><http xmlns=\"http://example.com/other\"><intercept-url pattern=\"/a\" access=\"hasRoles(\"/></http></beans>";
        let d = Document { path: "/project/other.xml".into(), language: "xml".into(), version: 1, text: plain.into() };
        inspect(&host, &mut tools, &d);
        assert!(host.provider_diagnostics().get(&d.path).is_none_or(Vec::is_empty));
    }

    #[test]
    fn kotlin_http_dsl_flags_deprecated_matchers() {
        let host = MockHost::install();
        let mut tools = SecurityTools::default();
        let text = r#"package com.acme

import org.springframework.security.config.annotation.web.invoke
import org.springframework.security.config.annotation.method.configuration.EnableGlobalMethodSecurity

@EnableGlobalMethodSecurity(prePostEnabled = true)
class SecurityConfig : WebSecurityConfigurerAdapter() {
    fun chain(http: HttpSecurity): SecurityFilterChain {
        http {
            authorizeRequests {
                authorize("/x", permitAll)
                authorize("/y", hasRole("ROLE_ADMIN"))
            }
        }
        http.authorizeRequests().antMatchers("/a").permitAll().mvcMatchers("/b").permitAll().regexMatchers("/c.*").denyAll()
        return http.build()
    }
}
"#;
        let d = Document {
            path: "/project/src/main/kotlin/com/acme/SecurityConfig.kt".into(),
            language: "kotlin".into(),
            version: 1,
            text: text.into(),
        };
        inspect(&host, &mut tools, &d);
        let found: Vec<(String, String)> = host.provider_diagnostics()[&d.path]
            .iter()
            .map(|x| (x.code.clone().unwrap(), text[offset(text, x.range.start).unwrap()..offset(text, x.range.end).unwrap()].to_owned()))
            .collect();
        let found: Vec<(&str, &str)> = found.iter().map(|(c, s)| (c.as_str(), s.as_str())).collect();
        assert_eq!(
            found,
            [
                ("enable-global-method-security", "@EnableGlobalMethodSecurity(prePostEnabled = true)"),
                ("web-security-configurer-adapter", "WebSecurityConfigurerAdapter"),
                ("authorize-requests", "authorizeRequests"),
                ("role-prefix", "ROLE_ADMIN"),
                ("authorize-requests", "authorizeRequests"),
                ("removed-request-matcher", "antMatchers"),
                ("removed-request-matcher", "mvcMatchers"),
                ("removed-request-matcher", "regexMatchers"),
            ]
        );
        let p = openidea_plugin::text::position(text, text.find("antMatchers").unwrap());
        assert!(tools.code_actions(&d, Range::new(p, p)).is_empty());
    }

    #[test]
    fn kotlin_authorize_requests_in_comments_and_strings_is_quiet() {
        let host = MockHost::install();
        let mut tools = SecurityTools::default();
        let import = "import org.springframework.security.config.annotation.web.invoke\n";
        let body = "// authorizeRequests { }\n/* a /* nested */ authorizeRequests { } */\nval s = \"authorizeRequests { } .antMatchers(\\\"/x\\\")\"\nval r = \"\"\"\nauthorizeRequests {\n\"\"\"\n// http.antMatchers(\"/a\")\nval ok = myauthorizeRequests { }\n";
        let mut quiet = |name: &str, text: String| {
            let d = Document { path: format!("/project/{name}.kt"), language: "kotlin".into(), version: 1, text };
            inspect(&host, &mut tools, &d);
            host.provider_diagnostics().get(&d.path).is_none_or(Vec::is_empty)
        };
        assert!(quiet("A", format!("{import}{body}")));
        assert!(!quiet("P", format!("{import}http.authorizeRequests {{ }}\n{body}")));
        assert!(quiet("B", "val x = http.authorizeRequests { }.antMatchers(\"/a\")\n".into()));
    }

    fn xml_codes(xml: &str) -> Vec<String> {
        let host = MockHost::install();
        let d = Document { path: "/project/s.xml".into(), language: "xml".into(), version: 1, text: xml.into() };
        inspect(&host, &mut SecurityTools::default(), &d);
        host.provider_diagnostics().get(&d.path).map(|v| v.iter().map(|x| x.code.clone().unwrap()).collect()).unwrap_or_default()
    }

    #[test]
    fn xml_namespaces_and_legitimate_patterns() {
        let ns = "http://www.springframework.org/schema/security";
        let default_ns = format!("<beans xmlns=\"{ns}\"><http><intercept-url pattern=\"/a\" access=\"hasRoles('X')\"/></http></beans>");
        assert_eq!(xml_codes(&default_ns), ["unknown-security-function"]);
        let prefixed = format!(
            "<beans xmlns:security=\"{ns}\"><security:http><security:intercept-url pattern=\"/a\" access=\"hasRoles('X')\"/></security:http></beans>"
        );
        assert_eq!(xml_codes(&prefixed), ["unknown-security-function"]);
        let quiet = |inner: &str, http: &str| xml_codes(&format!("<beans xmlns=\"{ns}\"><http{http}>{inner}</http></beans>"));
        let url = |extra: &str| format!("<intercept-url pattern=\"/a\" access=\"permitAll\"{extra}/>");
        assert!(quiet(&format!("{}{}", url(" method=\"GET\""), url(" method=\"POST\"")), "").is_empty());
        assert!(quiet(&format!("{}{}", url(" servlet-path=\"/x\""), url(" servlet-path=\"/y\"")), "").is_empty());
        assert_eq!(quiet(&format!("{}{}", url(""), url(" servlet-path=\"/y\"")), ""), Vec::<String>::new());
        let catch_all = |extra: &str| {
            format!("<intercept-url pattern=\"/**\" access=\"permitAll\"{extra}/><intercept-url pattern=\"/b\" access=\"denyAll\"/>")
        };
        assert!(quiet(&catch_all(" method=\"GET\""), "").is_empty());
        assert!(quiet(&catch_all(""), " request-matcher=\"regex\"").is_empty());
        assert!(quiet(&catch_all(""), " request-matcher-ref=\"m\"").is_empty());
        assert_eq!(quiet(&catch_all(""), ""), ["unreachable-patterns"]);
    }

    #[test]
    fn kotlin_char_literal_quote_does_not_open_a_string() {
        let host = MockHost::install();
        let mut tools = SecurityTools::default();
        let import = "import org.springframework.security.config.annotation.web.invoke\n";
        let mut codes = |name: &str, body: &str| {
            let d =
                Document { path: format!("/project/{name}.kt"), language: "kotlin".into(), version: 1, text: format!("{import}{body}") };
            inspect(&host, &mut tools, &d);
            host.provider_diagnostics().get(&d.path).map(|v| v.len()).unwrap_or(0)
        };
        assert_eq!(codes("A", "val q = '\"'; http.authorizeRequests { }\n"), 1);
        assert_eq!(codes("B", "'\"'.also { println(\"authorizeRequests {\") }\n"), 0);
        assert_eq!(codes("C", "val a = '\\n'; val b = '\\''; val c = '\\\\'; http.authorizeRequests { }\n"), 1);
        assert_eq!(codes("D", "val s = \"it's\"; don't; http.authorizeRequests { }\n"), 1);
        assert_eq!(codes("E", "x = '\n"), 0);
    }

    const SECURITY_IMPORT: &str = "import org.springframework.security.config.annotation.web.builders.HttpSecurity;\n";

    fn spans(path: &str, text: &str, code: &str) -> Vec<String> {
        let _host = MockHost::install();
        problems(path, text)
            .into_iter()
            .filter(|p| p.diagnostic.code.as_deref() == Some(code))
            .map(|p| {
                let r = p.diagnostic.range;
                text[offset(text, r.start).unwrap()..offset(text, r.end).unwrap()].to_owned()
            })
            .collect()
    }

    fn java_spans(body: &str, code: &str) -> Vec<String> {
        spans("/project/C.java", &format!("{SECURITY_IMPORT}class C {{ void f(HttpSecurity http) throws Exception {{\n{body}\n}} }}"), code)
    }

    #[test]
    fn request_matchers_after_any_request_warn() {
        let lambda = "http.authorizeHttpRequests(a -> a.requestMatchers(\"/a\").permitAll().anyRequest().authenticated().requestMatchers(\"/b\").denyAll().anyRequest().denyAll());";
        assert_eq!(java_spans(lambda, "matchers-after-any-request"), ["requestMatchers", "anyRequest"]);
        let chained = "http.authorizeHttpRequests().anyRequest().authenticated().antMatchers(\"/b\").denyAll();\nhttp.authorizeHttpRequests().requestMatchers(\"/ok\").permitAll();";
        assert_eq!(java_spans(chained, "matchers-after-any-request"), ["antMatchers"]);
    }

    #[test]
    fn method_security_literals_are_checked() {
        let src = |m: &str| format!("import org.springframework.security.access.prepost.PreAuthorize;\nclass C {{\n{m}\n}}");
        let run = |m: &str, code: &str| spans("/project/C.java", &src(m), code);
        let bad = "@PreAuthorize(\"#id == authentication.name and hasRole('A')\")\nvoid f(String name) {}";
        assert_eq!(run(bad, "unknown-method-parameter"), ["#id"]);
        let good = "@PreAuthorize(\"#id == authentication.name and #this != null and #n.isEmpty()\")\nvoid f(String id, @P(\"n\") String x) {}";
        assert!(run(good, "unknown-method-parameter").is_empty());
        let unknown_alias = "@PreAuthorize(\"#zzz == 1\")\nvoid f(@P(NAME) String x) {}";
        assert!(run(unknown_alias, "unknown-method-parameter").is_empty());
        let literal = "@PreAuthorize(\"'#nope' == 'x'\")\nvoid f() {}";
        assert!(run(literal, "unknown-method-parameter").is_empty());
        assert_eq!(run("@PreAuthorize(\"hasRole('A'\")\nvoid f() {}", "unbalanced-expression"), ["hasRole('A'"]);
        assert_eq!(run("@PreAuthorize(\"hasRole()\")\nvoid f() {}", "empty-security-function"), ["hasRole()"]);
        assert!(run("@PreAuthorize(\"hasAuthority('x') or permitAll\")\nvoid f() {}", "unbalanced-expression").is_empty());
    }

    #[test]
    fn identical_matchers_with_conflicting_access_name_both_rules() {
        let _host = MockHost::install();
        let body = "http.authorizeHttpRequests(a -> a.requestMatchers(\"/a\").permitAll().requestMatchers(\"/a\").hasRole(\"X\").anyRequest().denyAll());";
        let order = |body: &str| -> Vec<String> {
            let text = format!("{SECURITY_IMPORT}class C {{ void f(HttpSecurity http) {{\n{body}\n}} }}");
            problems("/project/C.java", &text)
                .into_iter()
                .filter(|p| p.diagnostic.code.as_deref() == Some("matcher-order"))
                .map(|p| p.diagnostic.message)
                .collect()
        };
        let found = order(body);
        assert_eq!(found.len(), 1);
        assert!(found[0].contains("permitAll()") && found[0].contains("hasRole(\"X\")") && found[0].contains("unreachable"), "{}", found[0]);
        let same = order(&body.replace("hasRole(\"X\")", "permitAll()"));
        assert!(same.len() == 1 && !same[0].contains("conflicting"));
    }

    #[test]
    fn role_prefix_is_a_startup_error_in_the_dsl_and_tolerated_in_spel() {
        let _host = MockHost::install();
        let find = |text: &str| {
            problems("/project/C.java", text).into_iter().find(|p| p.diagnostic.code.as_deref() == Some("role-prefix")).unwrap().diagnostic
        };
        let dsl = find(&format!("{SECURITY_IMPORT}class C {{ void f(HttpSecurity http) {{ http.authorizeHttpRequests(a -> a.anyRequest().hasRole(\"ROLE_A\")); }} }}"));
        assert!(dsl.message.contains("IllegalArgumentException") && dsl.severity == Severity::Warning);
        let spel = find("import org.springframework.security.access.prepost.PreAuthorize;\nclass C {\n@PreAuthorize(\"hasRole('ROLE_A')\")\nvoid f() {}\n}");
        assert!(spel.message.contains("strip") && spel.severity == Severity::Info);
    }

    #[test]
    fn secured_and_roles_allowed_need_their_switch_on_the_same_enable_annotation() {
        let src = |enable: &str| format!("import org.springframework.security.access.annotation.Secured;\n{enable}\nclass C {{\n@Secured(\"ROLE_A\")\nvoid f() {{}}\n@RolesAllowed(\"A\")\nvoid g() {{}}\n}}");
        let run = |enable: &str| spans("/project/C.java", &src(enable), "method-security-disabled");
        assert_eq!(run("@EnableMethodSecurity"), ["@Secured(\"ROLE_A\")", "@RolesAllowed(\"A\")"]);
        assert_eq!(run("@EnableMethodSecurity(securedEnabled = true)"), ["@RolesAllowed(\"A\")"]);
        assert!(run("@EnableMethodSecurity(securedEnabled = true, jsr250Enabled = true)").is_empty());
        assert!(run("").is_empty());
        assert!(run("@EnableGlobalMethodSecurity(securedEnabled = true)").is_empty());
    }

    #[test]
    fn narrow_matcher_after_broader_matcher_warns() {
        let body =
            "http.authorizeHttpRequests(a -> a.requestMatchers(\"/**\").permitAll().requestMatchers(\"/admin/**\").hasRole(\"ADMIN\"));";
        assert_eq!(java_spans(body, "matcher-order"), ["/admin/**"]);
        let nested = "http.authorizeHttpRequests(a -> a.requestMatchers(\"/api/**\").authenticated().requestMatchers(\"/api/v1/users\", \"/other\").permitAll().requestMatchers(\"/api\").denyAll());";
        assert_eq!(java_spans(nested, "matcher-order"), ["/api/v1/users", "/api"]);
        let method = "http.authorizeHttpRequests(a -> a.requestMatchers(HttpMethod.GET, \"/api/**\").permitAll().requestMatchers(\"/api/x\").denyAll().requestMatchers(HttpMethod.POST, \"/api/y\").denyAll());";
        assert!(java_spans(method, "matcher-order").is_empty());
    }

    #[test]
    fn ordered_matchers_are_quiet() {
        let body = "http.authorizeHttpRequests(a -> a.requestMatchers(\"/admin/**\").hasRole(\"ADMIN\").requestMatchers(\"/api/**\").authenticated().requestMatchers(\"/**\").permitAll().anyRequest().denyAll());";
        assert!(java_spans(body, "matcher-order").is_empty());
        assert!(java_spans(body, "matchers-after-any-request").is_empty());
    }

    #[test]
    fn matcher_checks_are_quiet_for_library_or_dynamic_name() {
        let wrong_order = "http.authorizeHttpRequests(a -> a.anyRequest().authenticated().requestMatchers(\"/a\").denyAll().requestMatchers(\"/**\").permitAll().requestMatchers(\"/b\").denyAll());";
        let library = format!("class C {{ void f(Lib http) {{\n{wrong_order}\n}} }}");
        assert!(spans("/project/C.java", &library, "matchers-after-any-request").is_empty());
        assert!(spans("/project/C.java", &library, "matcher-order").is_empty());
        let dynamic = "http.authorizeHttpRequests(a -> a.requestMatchers(PUBLIC).permitAll().requestMatchers(\"/x\").denyAll().requestMatchers(regexMatcher(\"/.*\")).denyAll().regexMatchers(\"/.*\").denyAll().requestMatchers(\"/y\").denyAll());";
        assert!(java_spans(dynamic, "matcher-order").is_empty());
        let masked = "// http.authorizeHttpRequests(a -> a.anyRequest().denyAll().requestMatchers(\"/a\"))\nString s = \"authorizeHttpRequests(a -> a.anyRequest().requestMatchers(1))\";\nhttp.authorizeHttpRequests(a -> a.requestMatchers(\"/a\").permitAll()/* .anyRequest() */.requestMatchers(\"/b\").denyAll().anyRequest().authenticated());";
        assert!(java_spans(masked, "matchers-after-any-request").is_empty());
        assert!(java_spans(masked, "matcher-order").is_empty());
    }

    #[test]
    fn enable_web_security_debug_true_warns() {
        let on = format!("{SECURITY_IMPORT}@EnableWebSecurity(debug = true)\nclass C {{}}");
        assert_eq!(spans("/project/C.java", &on, "debug-enabled"), ["@EnableWebSecurity(debug = true)"]);
        let customizer = format!("{SECURITY_IMPORT}class C {{ WebSecurityCustomizer c() {{ return web -> web.debug(true); }} }}");
        assert_eq!(spans("/project/C.java", &customizer, "debug-enabled"), ["debug(true)"]);
        let xml = "<beans xmlns:sec=\"http://www.springframework.org/schema/security\"><sec:debug/></beans>";
        assert_eq!(xml_codes(xml), ["debug-enabled"]);
    }

    #[test]
    fn debug_enabled_is_quiet_for_library_or_dynamic_name() {
        for text in [
            "@EnableWebSecurity(debug = false)\nclass C {}",
            "@EnableWebSecurity(debug = DEBUG)\nclass C {}",
            "@EnableWebSecurity\nclass C {}",
            "// @EnableWebSecurity(debug = true)\nString s = \"@EnableWebSecurity(debug = true)\";\nclass C { void f() { logger.debug(true); } }",
        ] {
            assert!(spans("/project/C.java", &format!("{SECURITY_IMPORT}{text}"), "debug-enabled").is_empty(), "{text}");
        }
        assert!(spans("/project/C.java", "@EnableWebSecurity(debug = true)\nclass C {}", "debug-enabled").is_empty());
        let other_namespace = "<beans xmlns:x=\"urn:other\"><x:debug/></beans>";
        assert!(xml_codes(other_namespace).is_empty());
    }

    #[test]
    fn lambda_dsl_and_kotlin_dsl_are_both_read() {
        let kotlin = "import org.springframework.security.config.annotation.web.invoke\n@EnableWebSecurity(debug = true)\nclass C {\n    fun f(http: HttpSecurity) {\n        http {\n            authorizeHttpRequests {\n                authorize(\"/**\", permitAll)\n                authorize(\"/admin/**\", hasRole(\"ADMIN\"))\n                authorize(anyRequest, authenticated)\n                authorize(\"/late\", denyAll)\n            }\n        }\n    }\n}\n";
        assert_eq!(spans("/project/C.kt", kotlin, "matcher-order"), ["/admin/**"]);
        assert_eq!(spans("/project/C.kt", kotlin, "matchers-after-any-request"), ["authorize"]);
        assert_eq!(spans("/project/C.kt", kotlin, "debug-enabled"), ["@EnableWebSecurity(debug = true)"]);
        let java = "http.authorizeHttpRequests(a -> a.anyRequest().authenticated().requestMatchers(\"/late\").denyAll());";
        assert_eq!(java_spans(java, "matchers-after-any-request"), ["requestMatchers"]);
    }

    #[test]
    fn fixed_file_publishes_empty_list() {
        let host = MockHost::install();
        let mut tools = SecurityTools::default();
        let bad = format!(
            "{SECURITY_IMPORT}@EnableWebSecurity(debug = true)\nclass C {{ void f(HttpSecurity http) {{ http.authorizeHttpRequests(a -> a.anyRequest().denyAll().requestMatchers(\"/a\").denyAll()); }} }}"
        );
        let d = Document { path: "/project/C.java".into(), language: "java".into(), version: 1, text: bad };
        inspect(&host, &mut tools, &d);
        assert_eq!(host.provider_diagnostics()[&d.path].len(), 2);
        let fixed = Document {
            text: format!(
                "{SECURITY_IMPORT}class C {{ void f(HttpSecurity http) {{ http.authorizeHttpRequests(a -> a.requestMatchers(\"/a\").denyAll().anyRequest().denyAll()); }} }}"
            ),
            ..d
        };
        inspect(&host, &mut tools, &fixed);
        assert!(host.provider_diagnostics().get(&fixed.path).is_none_or(Vec::is_empty));
    }

    #[test]
    fn matcher_and_debug_groups_can_be_switched_off() {
        let host = MockHost::install();
        let mut tools = SecurityTools::default();
        host.set_setting("springSecurity.checkMatchers", "false");
        host.set_setting("springSecurity.checkDebug", "false");
        let text = format!(
            "{SECURITY_IMPORT}@EnableWebSecurity(debug = true)\nclass C {{ void f(HttpSecurity http) {{ http.authorizeHttpRequests(a -> a.anyRequest().denyAll().requestMatchers(\"/a\").denyAll()); }} }}"
        );
        let d = Document { path: "/project/C.java".into(), language: "java".into(), version: 1, text };
        inspect(&host, &mut tools, &d);
        assert!(host.provider_diagnostics().get(&d.path).is_none_or(Vec::is_empty));
    }

    #[test]
    fn matcher_ranges_are_utf16_columns() {
        let text = format!(
            "{SECURITY_IMPORT}class C {{ void f(HttpSecurity http) {{ /* \u{e9}\u{1F600} */ http.authorizeHttpRequests(a -> a.anyRequest().denyAll().requestMatchers(\"/a\").denyAll()); }} }}"
        );
        let _host = MockHost::install();
        let found = problems("/project/C.java", &text);
        let d = &found[0].diagnostic;
        let line = text.lines().nth(1).unwrap();
        let expected = line.encode_utf16().count() - line[line.find("requestMatchers").unwrap()..].encode_utf16().count();
        assert_eq!(d.range.start.character as usize, expected);
    }


    #[test]
    fn unknown_bean_references_are_info_once_the_project_is_read() {
        let host = MockHost::install();
        host.add_file("src/main/java/com/acme/Authz.java", "@Component class Authz {}");
        let src = "@Service\nclass S {\n    @PreAuthorize(\"@authz.ok() and @ghost.ok()\")\n    void m() {}\n    @WithUserDetails(userDetailsServiceBeanName = \"myUsers\")\n    void t() {}\n}\n";
        inspect(&host, &mut SecurityTools::default(), &doc("S.java", src));
        let all = found(&host, "S.java", src);
        assert_eq!(all, [("unresolved-bean-reference".into(), "ghost".into()), ("unresolved-bean-reference".into(), "myUsers".into())]);
        let d = &host.provider_diagnostics()["/project/src/main/java/com/acme/S.java"];
        assert!(d.iter().all(|x| x.severity == Severity::Info));

        host.set_setting("springSecurity.checkBeanReferences", "false");
        inspect(&host, &mut SecurityTools::default(), &doc("S.java", src));
        assert!(host.provider_diagnostics().get("/project/src/main/java/com/acme/S.java").is_none_or(|d| d.is_empty()));
    }

    #[test]
    fn security_xml_tables_and_bean_refs_are_reported() {
        let xml = r#"<b:beans xmlns:b="http://www.springframework.org/schema/beans" xmlns="http://www.springframework.org/schema/security">
<http create-session="sometimes"><intercept-url access="x"/></http>
<authentication-manager><authentication-provider user-service-ref="svc"/></authentication-manager></b:beans>"#;
        let codes = xml_codes(xml);
        assert_eq!(codes, ["invalid-attribute-value", "missing-attribute", "unresolved-reference"]);
    }

    #[test]
    fn self_invocation_of_protected_methods_is_opt_in() {
        let host = MockHost::install();
        let src = "import org.springframework.security.access.prepost.PreAuthorize;\n@Service\nclass S {\n    @PreAuthorize(\"hasRole('A')\") public void guarded() {}\n    public void run() { guarded(); }\n}\n";
        inspect(&host, &mut SecurityTools::default(), &doc("S.java", src));
        assert!(host.provider_diagnostics().get("/project/src/main/java/com/acme/S.java").is_none_or(|d| d.is_empty()));
        host.set_setting("springSecurity.checkSelfInvocation", "true");
        inspect(&host, &mut SecurityTools::default(), &doc("S.java", src));
        assert_eq!(found(&host, "S.java", src), [("security-self-invocation".to_owned(), "guarded".to_owned())]);
    }

    #[test]
    fn endpoint_navigation_withholds_unowned_namespace_and_unknown_strategy() {
        let host = MockHost::install();
        host.add_file("src/main/java/Api.java", "@RestController class Api { @GetMapping(\"/api/users\") void users() {} }");
        let xml = "<s:http xmlns:s=\"http://www.springframework.org/schema/security\" request-matcher=\"ant\"><s:intercept-url pattern=\"/api/**\" method=\"GET\"/></s:http>";
        let mut tools = SecurityTools::default();
        for text in [
            xml.replace(SECURITY_NAMESPACE,"urn:foreign"),
            xml.replace(" request-matcher=\"ant\"", ""),
            xml.replace("request-matcher=\"ant\"", "request-matcher=\"mvc\""),
            xml.replace("request-matcher=\"ant\"", "request-matcher=\"ant\" request-matcher-ref=\"custom\""),
            xml.replace("request-matcher=\"ant\"", "request-matcher=\"ant\" servlet-path=\"/app\""),
            xml.replace("method=\"GET\"", "method=\"DYNAMIC\""),
            xml.replace("/api/**", "/api/?sers"),
            xml.replace("/api/**", "/api/**/users"),
            xml.replace("/api/**", "/api//**"),
            xml.replace("/api/**", "/api/../**"),
            xml.replace("/api/**", "/api/"),
        ] {
            let d = Document{path:"/project/security.xml".into(),language:"xml".into(),version:1,text};
            let at = openidea_plugin::text::position(&d.text,d.text.find("/api/").unwrap()+2);
            assert!(tools.definition(&d,at).is_empty(),"{}",d.text);
        }
        for body in ["other.requestMatchers(\"/api/**\")", "http.authorizeHttpRequests(a -> a.requestMatchers(\"/api/**\"))"] {
            let d = doc("C.java",&format!("import org.springframework.security.config.annotation.web.builders.HttpSecurity; class C {{void f(HttpSecurity http) {{ {body}; }} }}"));
            let at = openidea_plugin::text::position(&d.text,d.text.find("/api/").unwrap()+2);
            assert!(tools.definition(&d,at).is_empty());
        }
    }

    #[test]
    fn explicit_xml_ant_matchers_offer_method_filtered_mapping_candidates() {
        let host = MockHost::install();
        host.add_file(
            "src/main/java/com/acme/Api.java",
            "@RestController @RequestMapping(\"/api\")\nclass Api {\n    @GetMapping(\"/users/{id}\") void get() {}\n    @PostMapping(\"/users\") void post() {}\n}\n",
        );
        let src = "<s:http xmlns:s=\"http://www.springframework.org/schema/security\" request-matcher=\"ant\"><s:intercept-url pattern=\"/api/**\" method=\"GET\"/><s:intercept-url pattern=\"/admin/**\"/></s:http>";
        let d = Document{path:"/project/security.xml".into(),language:"xml".into(),version:1,text:src.into()};
        let mut tools = SecurityTools::default();
        let at = |needle: &str| openidea_plugin::text::position(src, src.find(needle).unwrap()+2);
        let targets = tools.definition(&d, at("/api"));
        assert_eq!(targets.len(), 1, "GET matcher skips the POST mapping: {targets:?}");
        assert_eq!(targets[0].range.start.line, 2);
        assert!(tools.definition(&d, at("/admin")).is_empty());
    }

    #[test]
    fn java_comment_with_glob_does_not_blank_file() {
        let code = code_only("/** Secures /api/** endpoints */\nint x = 1;", false);
        assert!(code.contains("int x = 1;"));
        let kotlin = code_only("/* a /* b */ c */\nval x = 1", true);
        assert!(kotlin.contains("val x = 1") && !kotlin.contains('c'));
    }
}
