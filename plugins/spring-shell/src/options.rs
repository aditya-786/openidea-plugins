//! Literal declaration checks, before runtime naming modifiers or custom conversion.
use super::*;
use std::collections::{BTreeMap, HashSet, hash_map::DefaultHasher};
use std::hash::Hasher;
use std::rc::Rc;
fn java_space(c: char) -> bool {
    matches!(c,'\u{0009}'..='\u{000d}'|'\u{001c}'..='\u{0020}'|'\u{1680}'|'\u{2000}'..='\u{2006}'|'\u{2008}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{205f}'|'\u{3000}')
}
pub(super) fn normalized(value: &str) -> String {
    value.split(' ').map(|s| s.trim_matches(java_space)).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ")
}
fn diag(out: &mut Vec<Diagnostic>, at: Range, code: &str, message: &str) {
    if out.len() < 500 {
        out.push(problem(at, Severity::Warning, code, message.into()));
    }
}

fn primitive<'a>(
    doc: &Document,
    f: &JavaFile,
    p: &'a java::Param,
    unknown: &HashSet<String>,
    aliases: &BTreeMap<String, String>,
) -> Option<&'static str> {
    if p.ty.array || !p.ty.args.is_empty() {
        return None;
    }
    let mut text = p.ty.text.trim();
    let via_alias = aliases.contains_key(text);
    if unknown.contains(text) {
        return None;
    }
    let mut visited = HashSet::new();
    while let Some(target) = aliases.get(text) {
        if !visited.insert(text)
            || f.types.iter().any(|t| t.outer.is_none() && t.name == text)
            || f.imports.iter().any(|i| i.path.rsplit('.').next() == Some(text))
        {
            return None;
        }
        text = target;
    }
    let kotlin = doc.language == "kotlin";
    if !kotlin {
        match text {
            "boolean" => return Some("boolean"),
            "byte" => return Some("byte"),
            "short" => return Some("short"),
            "int" => return Some("int"),
            "long" => return Some("long"),
            "float" => return Some("float"),
            "double" => return Some("double"),
            "char" => return Some("char"),
            _ => {}
        }
    }
    let families = [
        (
            "java.lang",
            [
                ("Boolean", "boolean"),
                ("Byte", "byte"),
                ("Short", "short"),
                ("Integer", "int"),
                ("Long", "long"),
                ("Float", "float"),
                ("Double", "double"),
                ("Character", "char"),
            ],
        ),
        (
            "kotlin",
            [
                ("Boolean", "boolean"),
                ("Byte", "byte"),
                ("Short", "short"),
                ("Int", "int"),
                ("Long", "long"),
                ("Float", "float"),
                ("Double", "double"),
                ("Char", "char"),
            ],
        ),
    ];
    for (package, pairs) in families {
        if package == "kotlin" && !kotlin {
            continue;
        }
        for (name, kind) in pairs {
            let qualified = format!("{package}.{name}");
            if text == qualified {
                let root = package.split('.').next().unwrap();
                if !via_alias && unknown.contains(root)
                    || f.types.iter().any(|t| t.name == root)
                    || f.imports.iter().any(|i| i.path.rsplit('.').next() == Some(root))
                    || !rules::written_owner(f,&doc.text,text,&qualified)
                {
                    return None;
                }
                return Some(kind);
            }
            if text == name && !f.types.iter().any(|t| t.name == name) && (via_alias || !unknown.contains(name)) {
                if rules::written_owner(f,&doc.text,text,&qualified) {
                    return Some(kind);
                }
            }
        }
    }

    None
}
fn jdk_digit(c: char, radix: u32) -> Option<u32> {
    let code = c as u32;
    let starts = [
        0x30, 0x660, 0x6f0, 0x7c0, 0x966, 0x9e6, 0xa66, 0xae6, 0xb66, 0xbe6, 0xc66, 0xce6, 0xd66, 0xde6, 0xe50, 0xed0, 0xf20, 0x1040,
        0x1090, 0x17e0, 0x1810, 0x1946, 0x19d0, 0x1a80, 0x1a90, 0x1b50, 0x1bb0, 0x1c40, 0x1c50, 0xa620, 0xa8d0, 0xa900, 0xa9d0, 0xa9f0,
        0xaa50, 0xabf0, 0xff10,
    ];
    let value = starts.into_iter().find_map(|start| (code >= start && code < start + 10).then(|| code - start)).or_else(|| match code {
        0x41..=0x5a => Some(code - 0x41 + 10),
        0x61..=0x7a => Some(code - 0x61 + 10),
        0xff21..=0xff3a => Some(code - 0xff21 + 10),
        0xff41..=0xff5a => Some(code - 0xff41 + 10),
        _ => None,
    })?;
    (value < radix).then_some(value)
}
fn valid_integer(value: &str, kind: &str) -> bool {
    let compact: String = value.chars().filter(|c| !java_space(*c)).collect();
    let (negative, rest) = compact.strip_prefix('-').map_or((false, compact.as_str()), |s| (true, s));
    let (hex, digits) =
        rest.strip_prefix("0x").or_else(|| rest.strip_prefix("0X")).or_else(|| rest.strip_prefix('#')).map_or((false, rest), |s| (true, s));
    let digits = if !hex && !negative { digits.strip_prefix('+').unwrap_or(digits) } else { digits };
    let radix = if hex { 16 } else { 10 };
    let mut number = 0i128;
    if digits.is_empty() {
        return false;
    }
    for c in digits.chars() {
        let Some(digit) = jdk_digit(c, radix) else {
            return false;
        };
        let Some(next) = number.checked_mul(radix as i128).and_then(|n| n.checked_add(digit as i128)) else {
            return false;
        };
        number = next;
    }
    if negative {
        number = -number;
    }
    let (min, max) = match kind {
        "byte" => (i8::MIN as i128, i8::MAX as i128),
        "short" => (i16::MIN as i128, i16::MAX as i128),
        "int" => (i32::MIN as i128, i32::MAX as i128),
        _ => (i64::MIN as i128, i64::MAX as i128),
    };
    number >= min && number <= max
}

fn exponent(value: &str) -> bool {
    let value = value.strip_prefix(['+', '-']).unwrap_or(value);
    !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit())
}
fn float_format(value: &str) -> bool {
    let compact: String = value.chars().filter(|c| !java_space(*c)).collect();
    let value = compact.strip_prefix(['+', '-']).unwrap_or(&compact);
    if matches!(value, "NaN" | "Infinity") {
        return true;
    }
    let value = value.strip_suffix(['f', 'F', 'd', 'D']).unwrap_or(value);
    let (hex, mantissa) = value.strip_prefix("0x").or_else(|| value.strip_prefix("0X")).map_or((false, value), |v| (true, v));
    let exp = if hex { mantissa.find(['p', 'P']) } else { mantissa.find(['e', 'E']) };
    let body = if let Some(at) = exp {
        if !exponent(&mantissa[at + 1..]) {
            return false;
        }
        &mantissa[..at]
    } else {
        if hex {
            return false;
        }
        mantissa
    };
    body.chars().filter(|&c| c == '.').count() <= 1
        && body.chars().any(|c| if hex { c.is_ascii_hexdigit() } else { c.is_ascii_digit() })
        && body.chars().all(|c| c == '.' || if hex { c.is_ascii_hexdigit() } else { c.is_ascii_digit() })
}
fn default_valid(value: &str, kind: &str) -> Option<bool> {
    if kind == "char" {
        return Some(value.encode_utf16().count() == 1);
    }
    Some(match kind {
        "boolean" => matches!(
            value.trim_matches(|c| c as u32 <= 32).to_ascii_lowercase().as_str(),
            "true" | "on" | "yes" | "1" | "false" | "off" | "no" | "0"
        ),
        "byte" | "short" | "int" | "long" => valid_integer(value, kind),
        "float" | "double" => float_format(value),
        _ => return None,
    })
}
fn names(doc: &Document, f: &JavaFile, m: &Member, modern: bool, out: &mut Vec<Diagnostic>, aliases: &BTreeMap<String, String>) {
    let mut unknown = HashSet::new();
    let mut collect = |tokens: &[java::Tok]| {
        let mut depth = 0usize;
        let mut names = false;
        for token in tokens {
            match token.s {
                "<" => {
                    depth += 1;
                    if depth == 1 {
                        names = true;
                    }
                }
                ">" => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        break;
                    }
                }
                "," if depth == 1 => names = true,
                "extends" | "super" | ":" if depth == 1 => names = false,
                _ if depth == 1 && names && token.is_ident() => {
                    if !matches!(token.s, "in" | "out" | "reified") {
                        unknown.insert(token.s.to_owned());
                    }
                }
                _ => {}
            }
        }
    };
    for t in f.types.iter().filter(|t| t.start <= m.start && m.end <= t.end) {
        let tokens = java::tokens(&doc.text[t.at + t.name.len()..t.end]);
        if tokens.first().is_some_and(|t| t.s == "<") {
            collect(&tokens);
        }
    }
    let header = java::tokens(&doc.text[m.start..m.at]);
    if let Some(at) = header.iter().position(|t| t.s == "<") {
        if at == 0
            || matches!(
                header[at - 1].s,
                "public"
                    | "private"
                    | "protected"
                    | "static"
                    | "final"
                    | "abstract"
                    | "synchronized"
                    | "native"
                    | "strictfp"
                    | "default"
                    | "fun"
                    | "inline"
                    | ")"
            )
        {
            collect(&header[at..]);
        }
    }
    let mut named: BTreeMap<(bool, String), Vec<Range>> = BTreeMap::new();
    let prefix = if modern {
        Some("--".to_owned())
    } else {
        m.annotation("ShellMethod").and_then(|a| {
            a.arg("prefix")
                .map(|_| literals::values(doc, a, "prefix").and_then(|s| (s.len() == 1).then(|| s[0].value.clone())))
                .unwrap_or(Some("--".into()))
        })
    };
    for p in &m.params {
        let name = if modern { "Option" } else { "ShellOption" };
        let Some(a) = p.annotation(name).filter(|a| {
            rules::owned(
                f,
                &doc.text,
                a,
                if modern {
                    "org.springframework.shell.command.annotation.Option"
                } else {
                    "org.springframework.shell.standard.ShellOption"
                },
            )
        }) else {
            continue;
        };
        let mut add = |short: bool, value: String, at: Range| {
            named.entry((short, value)).or_default().push(at);
        };
        if modern {
            if let Some(values) = literals::values(doc, a, "longNames") {
                for s in values {
                    add(false, s.value.clone(), rules::literal_range(&doc.text, &s));
                }
            }
            if let Some(values) = literals::characters(doc, a, "shortNames") {
                for (value, raw) in values {
                    let start = raw.start;
                    let end = raw.end;
                    add(true, value, range(&doc.text, start, end));
                }
            }
        } else if let Some(values) = literals::values(doc, a, "value") {
            for s in values {
                let stripped = s.value.trim_start_matches('-');
                let count = s.value.len() - stripped.len();
                let kind = match count {
                    2 => Some(false),
                    1 if stripped.encode_utf16().count() == 1 => Some(true),
                    0 => prefix.as_deref().and_then(|prefix| match prefix {
                        "--" => Some(false),
                        "-" if stripped.encode_utf16().count() == 1 => Some(true),
                        _ => None,
                    }),
                    _ => None,
                };
                if let Some(short) = kind {
                    add(short, stripped.into(), rules::literal_range(&doc.text, &s));
                } else if prefix.is_some() {
                    diag(
                        out,
                        rules::literal_range(&doc.text, &s),
                        "ignored-option-name",
                        "Spring Shell ignores this option name. Use --name for a long option or -x (one character) for a short one.",
                    );
                }
            }
        }
        let Some(values) = literals::values(doc, a, "defaultValue") else {
            continue;
        };
        let [value] = values.as_slice() else {
            continue;
        };
        if value.value.trim_matches(java_space).is_empty() || !modern && matches!(value.value.as_str(), "__NONE__" | "__NULL__") {
            continue;
        }
        if let Some(kind) = primitive(doc, f, p, &unknown, aliases) {
            if default_valid(&value.value, kind) == Some(false) {
                diag(
                    out,
                    rules::literal_range(&doc.text, value),
                    "option-default-format",
                    "This default value can't be converted to the parameter's type. Custom converters are not checked.",
                );
            }
        }
    }
    for ((_, name), ranges) in named {
        if name.is_empty() {
            for &at in &ranges {
                diag(out, at, "empty-option-name", "The option name is empty.");
            }
        }
        if ranges.len() > 1 {
            for at in ranges {
                diag(
                    out,
                    at,
                    "duplicate-option-name",
                    "This option name is used more than once in this command.",
                );
            }
        }
    }
}
struct Prefix {
    text: String,
    first: DefaultHasher,
    second: DefaultHasher,
}
impl Prefix {
    fn new(text: String) -> Self {
        let mut first = DefaultHasher::new();
        let mut second = DefaultHasher::new();
        second.write_u8(1);
        first.write(text.as_bytes());
        second.write(text.as_bytes());
        Self { text, first, second }
    }
}
struct Route {
    prefix: Rc<Prefix>,
    own: String,
    locations: Vec<(Range, bool)>,
}
impl Route {
    fn hash(&self) -> (u64, u64) {
        let (mut a, mut b) = (self.prefix.first.clone(), self.prefix.second.clone());
        if !self.prefix.text.is_empty() && !self.own.is_empty() {
            a.write(b" ");
            b.write(b" ");
        }
        a.write(self.own.as_bytes());
        b.write(self.own.as_bytes());
        (a.finish(), b.finish())
    }
    fn bytes(&self) -> impl Iterator<Item = u8> + '_ {
        self.prefix.text.bytes().chain((!self.prefix.text.is_empty() && !self.own.is_empty()).then_some(b' ')).chain(self.own.bytes())
    }
    fn same(&self, other: &Self) -> bool {
        if Rc::ptr_eq(&self.prefix, &other.prefix) { self.own == other.own } else { self.bytes().eq(other.bytes()) }
    }
}
fn aliases(doc: &Document, f: &JavaFile, out: &mut Vec<Diagnostic>) {
    let empty = Rc::new(Prefix::new(String::new()));
    let mut routes: BTreeMap<(u64, u64), Vec<Route>> = BTreeMap::new();
    let mut add = |route: Route| {
        let bucket = routes.entry(route.hash()).or_default();
        if let Some(same) = bucket.iter_mut().find(|r| r.same(&route)) {
            same.locations.extend(route.locations);
        } else {
            bucket.push(route);
        }
    };

    for t in &f.types {
        let class = t.annotation("Command").filter(|a| rules::modern_field(f, &doc.text, a) == Some("command"));
        let unknown_prefix = class.is_none() && (t.supers().next().is_some() || !t.annotations.is_empty());
        let class_command = match class {
            Some(a) if a.arg("command").is_some() => {
                literals::values(doc, a, "command").map(|v| normalized(&v.iter().map(|s| s.value.as_str()).collect::<Vec<_>>().join(" ")))
            }
            _ if unknown_prefix => None,
            _ => Some(String::new()),
        };
        for m in t.methods() {
            if let Some(a) = m.annotation("ShellMethod").filter(|a| rules::supported(f, &doc.text, a)) {
                let keys = if a.arg("key").is_none() { Some(Vec::new()) } else { literals::values(doc, a, "key") };
                if let Some(keys) = keys {
                    if keys.is_empty() {
                        if m.name.is_ascii() {
                            add(Route {
                                prefix: empty.clone(),
                                own: kebab(&m.name),
                                locations: vec![(range(&doc.text, m.at, m.at + m.name.len()), false)],
                            });
                        }
                    } else {
                        for s in keys {
                            add(Route {
                                prefix: empty.clone(),
                                own: normalized(&s.value),
                                locations: vec![(rules::literal_range(&doc.text, &s), false)],
                            });
                        }
                    }
                }
            } else if let (Some(prefix), Some(a)) = (&class_command, m.annotation("Command").filter(|a| rules::modern_field(f, &doc.text, a) == Some("command")))
            {
                let values = if a.arg("command").is_none() { Some(Vec::new()) } else { literals::values(doc, a, "command") };
                if let Some(values) = values {
                    let own = normalized(&values.iter().map(|s| s.value.as_str()).collect::<Vec<_>>().join(" "));
                    let combined = normalized(&format!("{prefix} {own}"));
                    let key = if combined.is_empty() {
                        if !m.name.is_ascii() {
                            continue;
                        }
                        kebab(&m.name)
                    } else {
                        combined
                    };
                    let at = values.first().map_or(range(&doc.text, m.at, m.at + m.name.len()), |s| rules::literal_range(&doc.text, s));
                    add(Route { prefix: empty.clone(), own: key, locations: vec![(at, false)] });
                }
            }
        }
        if unknown_prefix {
            continue;
        }
        let class_alias = match class {
            Some(a) if a.arg("alias").is_some() => {
                let Some(values) = literals::values(doc, a, "alias") else {
                    continue;
                };
                normalized(&values.iter().map(|s| s.value.as_str()).collect::<Vec<_>>().join(" "))
            }
            _ => String::new(),
        };
        let prefix = Rc::new(Prefix::new(class_alias));
        for m in t.methods() {
            let Some(a) = m.annotation("Command").filter(|a| rules::modern_field(f, &doc.text, a) == Some("command")) else {
                continue;
            };
            let Some(values) = literals::values(doc, a, "alias") else {
                continue;
            };
            for s in values {
                let own = normalized(&s.value);
                if prefix.text.is_empty() && own.is_empty() {
                    diag(
                        out,
                        rules::literal_range(&doc.text, &s),
                        "empty-command-alias",
                        "The alias is empty and the class has no alias prefix, so the command has no name.",
                    );
                    continue;
                }
                add(Route { prefix: prefix.clone(), own, locations: vec![(rules::literal_range(&doc.text, &s), true)] });
            }
        }
    }
    for bucket in routes.values() {
        for route in bucket {
            if route.locations.len() > 1 && route.locations.iter().any(|(_, alias)| *alias) {
                for (at, _) in &route.locations {
                    diag(
                        out,
                        *at,
                        "duplicate-command-alias",
                        "Another command in this file already uses this name.",
                    );
                }
            }
        }
    }
}
pub(super) fn check(doc: &Document, f: &JavaFile) -> Vec<Diagnostic> {
    let tokens = java::tokens(&doc.text);
    let mut type_aliases = BTreeMap::new();
    let mut depth = 0usize;
    for (i, token) in tokens.iter().enumerate() {
        match token.s {
            "{" => depth += 1,
            "}" => depth = depth.saturating_sub(1),
            "typealias" if depth == 0 && doc.language == "kotlin" => {
                let Some(name) = tokens.get(i + 1).filter(|t| t.is_ident()) else {
                    continue;
                };
                if tokens.get(i + 2).is_none_or(|t| t.s != "=") {
                    continue;
                }
                let mut at = i + 3;
                let Some(first) = tokens.get(at).filter(|t| t.is_ident()) else {
                    continue;
                };
                let mut target = first.s.to_owned();
                at += 1;
                while tokens.get(at).is_some_and(|t| t.s == ".") {
                    let Some(next) = tokens.get(at + 1).filter(|t| t.is_ident()) else {
                        break;
                    };
                    target.push('.');
                    target.push_str(next.s);
                    at += 2;
                }
                if tokens.get(at).is_some_and(|t| matches!(t.s, "?" | "<" | "&" | ".")) {
                    continue;
                }
                if type_aliases.insert(name.s.to_owned(), target).is_some() {
                    type_aliases.insert(name.s.to_owned(), name.s.to_owned());
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    for t in &f.types {
        for m in t.methods() {
            if m.annotation("Command").is_some_and(|a| rules::modern_field(f, &doc.text, a) == Some("command")) {
                names(doc, f, m, true, &mut out, &type_aliases);
            } else if m.annotation("ShellMethod").is_some_and(|a| rules::supported(f, &doc.text, a)) {
                names(doc, f, m, false, &mut out, &type_aliases);
            }
        }
    }
    aliases(doc, f, &mut out);
    core_options(doc,f,&mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    const MODERN: &str =
        "import org.springframework.shell.command.annotation.Command; import org.springframework.shell.command.annotation.Option; ";
    const LEGACY: &str = "import org.springframework.shell.standard.ShellMethod; import org.springframework.shell.standard.ShellOption; ";
    fn doc(text: &str, kotlin: bool) -> Document {
        Document {
            path: if kotlin { "/project/C.kt" } else { "/project/C.java" }.into(),
            language: if kotlin { "kotlin" } else { "java" }.into(),
            version: 7,
            text: text.into(),
        }
    }
    fn codes(text: &str) -> Vec<String> {
        ShellTools::default().diagnostics(&doc(text, false)).into_iter().filter_map(|d| d.code).collect()
    }
    #[test]
    fn explicit_modern_names_and_legacy_prefix_mapping_find_duplicate_declarations() {
        let _host = MockHost::install().with_capabilities(&[]);
        let modern = format!(
            "{MODERN}@Command class C {{ @Command(command=\"run\") void run(@Option(longNames={{\"name\",\"alias\"}},shortNames={{'n'}}) String one,@Option(longNames=\"name\",shortNames='n') String two){{}} }}"
        );
        assert_eq!(codes(&modern).iter().filter(|c| c.as_str() == "duplicate-option-name").count(), 4);
        let legacy = format!(
            "{LEGACY}@org.springframework.shell.standard.ShellComponent class C {{ @ShellMethod(\"Run\") void run(@ShellOption({{\"--name\",\"name\"}}) String one,@ShellOption(\"-n\") String two){{}} }}"
        );
        assert_eq!(codes(&legacy).iter().filter(|c| c.as_str() == "duplicate-option-name").count(), 2);
        let distinct = legacy.replace("@ShellMethod(\"Run\")", "@ShellMethod(value=\"Run\",prefix=\"-\")");
        assert!(!codes(&distinct).contains(&"duplicate-option-name".into()));
        let ignored = legacy.replace("\"--name\",\"name\"", "\"---name\",\"-long\"");
        assert_eq!(codes(&ignored).iter().filter(|c| c.as_str() == "ignored-option-name").count(), 2);
    }
    #[test]
    fn standard_primitive_formats_handle_ranges_hex_booleans_and_java_float_grammar() {
        for (kind, good, bad) in [
            ("byte", "-0x80", "128"),
            ("short", "32767", "32768"),
            ("int", "2 147483647", "2147483648"),
            ("long", "-9223372036854775808", "9223372036854775808"),
            ("boolean", " YES ", "maybe"),
            ("float", "0x1.fp+3F", "0x1.0"),
            ("double", "-Infinity", "1e+"),
            ("char", "é", "😀"),
        ] {
            assert_eq!(default_valid(good, kind), Some(true), "{kind}:{good}");
            assert_eq!(default_valid(bad, kind), Some(false), "{kind}:{bad}");
        }
        assert_eq!(default_valid("+0x1", "int"), Some(false));
        assert_eq!(default_valid("0x-1", "int"), Some(false));
        assert_eq!(default_valid("09", "int"), Some(true));
        assert_eq!(default_valid("NaN", "double"), Some(true));
        let _host = MockHost::install().with_capabilities(&[]);
        let text = format!(
            "{MODERN}@Command class C {{ @Command(command=\"run\") void run(@Option(defaultValue=\"abc\") int count,@Option(defaultValue=\"maybe\") boolean flag,@Option(defaultValue=\"letters\") Unknown other){{}} }}"
        );
        assert_eq!(codes(&text).iter().filter(|c| c.as_str() == "option-default-format").count(), 2);
        assert!(codes(&text.replace("\"abc\"", "\"10\"").replace("\"maybe\"", "\"true\"")).is_empty());
    }
    #[test]
    fn aliases_follow_flattened_class_prefix_and_independent_method_array_semantics() {
        let _host = MockHost::install().with_capabilities(&[]);
        let text = format!(
            "{MODERN}@Command(command=\"db\",alias={{\"short\",\"db\"}}) class C {{ @Command(command=\"one\",alias={{\"x\",\"y\"}}) void one(){{}} @Command(command=\"two\",alias=\"x\") void two(){{}} @Command(command=\"short db y\") void other(){{}} }}"
        );
        let found = codes(&text);
        assert_eq!(found.iter().filter(|c| c.as_str() == "duplicate-command-alias").count(), 2, "{found:?}");
        let collision = format!(
            "{MODERN}@Command(alias={{\"short\",\"db\"}}) class C {{ @Command(command=\"one\",alias=\"x\") void one(){{}} }} @Command(command=\"short db\") class D {{ @Command(command=\"x\") void two(){{}} }}"
        );
        assert_eq!(codes(&collision).iter().filter(|c| c.as_str() == "duplicate-command-alias").count(), 2);
        let class_only = format!(
            "{MODERN}@Command(alias=\"same\") class C {{ @Command(command=\"one\") void one(){{}} @Command(command=\"two\") void two(){{}} }}"
        );
        assert!(!codes(&class_only).contains(&"duplicate-command-alias".into()));
    }
    #[test]
    fn unknown_owners_values_modifiers_and_types_do_not_turn_into_format_assertions() {
        let _host = MockHost::install().with_capabilities(&[]);
        for text in [format!("{MODERN}@Command class C {{ @Command void run(@Option(longNames=NAME,shortNames=SHORT,defaultValue=VALUE) int n){{}} }}"),"import other.Command; import other.Option; class C { @Command void run(@Option(longNames={\"x\",\"x\"},defaultValue=\"bad\") int x){} }".into(),"import org.springframework.shell.core.command.annotation.Command; import org.springframework.shell.core.command.annotation.Option; class C { @Command void run(@Option(defaultValue=VALUE) int x){} }".into(),format!("{LEGACY}@org.springframework.shell.standard.ShellComponent class C {{ @ShellMethod(\"Run\") void run(@ShellOption(defaultValue=\"__NULL__\") int n){{}} }}")] {assert!(codes(&text).is_empty(),"{text}");}
        let kotlin = "import org.springframework.shell.command.annotation.Command\nimport org.springframework.shell.command.annotation.Option\nimport kotlin.Int\n@Command class C { @Command(command=\"run\") fun run(@Option(defaultValue=\"bad\") x:Int) {} }";
        assert!(ShellTools::default().diagnostics(&doc(kotlin, true)).iter().any(|d| d.code.as_deref() == Some("option-default-format")));
    }
    #[test]
    fn current_text_fix_clears_names_and_arity_is_an_annotation_normalization_advisory() {
        let host = MockHost::install().with_capabilities(&[]);
        let text =
            format!("{MODERN}@Command class C {{ @Command void run(@Option(longNames={{\"x\",\"x\"}},arityMin=2,arityMax=1) int x){{}} }}");
        host.open_editor("/project/C.java", "java", &text, &[]);
        let mut tools = ShellTools::default();
        host.refresh_diagnostics(&mut tools);
        let diagnostics = host.provider_diagnostics();
        let d = diagnostics.values().flatten().find(|d| d.code.as_deref() == Some("option-arity-range")).unwrap();
        assert_eq!(d.severity, Severity::Warning);
        host.open_editor("/project/C.java", "java", &text.replace("\"x\",\"x\"", "\"x\",\"y\"").replace("arityMax=1", "arityMax=2"), &[]);
        host.refresh_diagnostics(&mut tools);
        assert!(host.provider_diagnostics().is_empty());
        assert_eq!(host.fetches(), 0);
    }
    #[test]
    fn unicode_integral_defaults_and_plain_kotlin_aliases_are_not_arbitrarily_dropped() {
        let _host = MockHost::install().with_capabilities(&[]);
        assert_eq!(default_valid("１２３", "int"), Some(true));
        assert_eq!(default_valid("١٢٣", "int"), Some(true));
        assert_eq!(default_valid("0xＦＦ", "short"), Some(true));
        assert_eq!(default_valid("𝟙", "int"), Some(false));
        let text = "import org.springframework.shell.command.annotation.Command\nimport org.springframework.shell.command.annotation.Option\ntypealias Count = kotlin.Int\ntypealias Other = Count\n@Command class C { @Command(command=\"run\") fun run(@Option(defaultValue=\"bad\") x:Other) {} }";
        assert!(ShellTools::default().diagnostics(&doc(text, true)).iter().any(|d| d.code.as_deref() == Some("option-default-format")));
        let shadow = text.replace("class C", "class C<Other>");
        assert!(!ShellTools::default().diagnostics(&doc(&shadow, true)).iter().any(|d| d.code.as_deref() == Some("option-default-format")));
        let cycle = text.replace("Count = kotlin.Int", "Count = Other");
        assert!(!ShellTools::default().diagnostics(&doc(&cycle, true)).iter().any(|d| d.code.as_deref() == Some("option-default-format")));
    }
    #[test]
    fn known_primary_route_survives_unknown_aliases_and_empty_alias_spans_are_visible() {
        let _host = MockHost::install().with_capabilities(&[]);
        let text = format!(
            "{MODERN}@Command(alias=PREFIX) class C {{ @Command(command=\"a x\") void one(){{}} }} @Command(alias=\"a\") class D {{ @Command(command=\"two\",alias=\"x\") void two(){{}} }}"
        );
        assert_eq!(codes(&text).iter().filter(|c| c.as_str() == "duplicate-command-alias").count(), 2);
        let empty = format!("{MODERN}@Command class C {{ @Command(command=\"run\",alias=\"\") void run(){{}} }}");
        let d = doc(&empty, false);
        let diagnostics = ShellTools::default().diagnostics(&d);
        let at = diagnostics.iter().find(|d| d.code.as_deref() == Some("empty-command-alias")).unwrap().range;
        assert_eq!(&empty[offset(&empty, at.start).unwrap()..offset(&empty, at.end).unwrap()], "\"\"");
    }
    #[test]
    fn escaped_names_aliases_and_defaults_keep_raw_utf16_spans() {
        let _host = MockHost::install().with_capabilities(&[]);
        let text = format!(
            r#"{MODERN}// 😀
@Command class C {{ @Command(command="first",alias="\u0078") void one(@Option(longNames={{"a","\u0061"}},defaultValue="\u0062ad") int n){{}} @Command(command="x") void two(){{}} }}"#
        );
        let d = doc(&text, false);
        let ds = ShellTools::default().diagnostics(&d);
        assert_eq!(ds.iter().filter(|d| d.code.as_deref() == Some("duplicate-command-alias")).count(), 2);
        assert!(ds.iter().any(|d| d.code.as_deref() == Some("duplicate-option-name")), "{ds:?}");
        let default = ds.iter().find(|d| d.code.as_deref() == Some("option-default-format")).unwrap();
        assert_eq!(&text[offset(&text, default.range.start).unwrap()..offset(&text, default.range.end).unwrap()], r"\u0062ad");
    }
    #[test]
    fn modern_alias_collides_with_known_legacy_component_route() {
        let _host = MockHost::install().with_capabilities(&[]);
        let text = format!(
            r#"{MODERN}{LEGACY}@org.springframework.shell.standard.ShellComponent class Legacy {{ @ShellMethod(value="Run",key="same") void run(){{}} }} @Command class Modern {{ @Command(command="other",alias="same") void other(){{}} }}"#
        );
        assert_eq!(codes(&text).iter().filter(|c| c.as_str() == "duplicate-command-alias").count(), 2);
    }
}

/// Spring Shell 4.x annotation contracts use singular option names, not 3.x
/// longNames/shortNames/arity. Keep these profiles separate.
fn core_options(doc:&Document,f:&JavaFile,out:&mut Vec<Diagnostic>) {
    for method in f.types.iter().flat_map(|t|t.methods()).filter(|m|m.annotation("Command").is_some_and(|a|rules::modern_field(f,&doc.text,a)==Some("name"))) {
        let mut names=BTreeMap::<(bool,String),Vec<Range>>::new();
        for parameter in &method.params {
            for a in &parameter.annotations {
                let option=a.name=="Option" && rules::owned(f,&doc.text,a,"org.springframework.shell.core.command.annotation.Option");
                let argument=a.name=="Argument" && rules::owned(f,&doc.text,a,"org.springframework.shell.core.command.annotation.Argument");
                if !option && !argument {continue;}
                if option {
                    if let Some(values)=literals::values(doc,a,"longName").filter(|v|v.len()==1) {
                        let value=&values[0];
                        // Empty longName means runtime parameter-name fallback.
                        if !value.value.is_empty() {names.entry((false,value.value.clone())).or_default().push(rules::literal_range(&doc.text,value));}
                    }
                    if let Some(values)=literals::characters(doc,a,"shortName").filter(|v|v.len()==1) {
                        let (value,raw)=&values[0];
                        if value!=" " {names.entry((true,value.clone())).or_default().push(range(&doc.text,raw.start,raw.end));}
                    }
                }
                let Some(values)=literals::values(doc,a,"defaultValue").filter(|v|v.len()==1) else {continue;};
                let value=&values[0];if value.value.is_empty(){continue;}
                let Some(kind)=primitive(doc,f,parameter,&HashSet::new(),&BTreeMap::new()) else {continue;};
                if default_valid(&value.value,kind)==Some(false) {
                    diag(out,rules::literal_range(&doc.text,value),if option{"option-default-format"}else{"argument-default-format"},"This default value can't be converted to the parameter's type. Custom converters are not checked.");
                }
            }
        }
        for (_,spans) in names.into_iter().filter(|(_,spans)|spans.len()>1) {
            for span in spans {diag(out,span,"duplicate-option-name","This option name is used more than once in this command.");}
        }
    }
}
#[cfg(test)]
mod core_option_tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    const IMPORTS:&str="import org.springframework.shell.core.command.annotation.Command; import org.springframework.shell.core.command.annotation.Option; import org.springframework.shell.core.command.annotation.Argument; ";
    fn problems(source:&str)->Vec<Diagnostic> {
        let d=Document{path:"/project/C.java".into(),language:"java".into(),text:source.into(),version:1};
        ShellTools::default().diagnostics(&d)
    }
    #[test]
    fn core_singular_names_and_option_argument_defaults_are_checked_without_v3_arity_rules() {
        let _h=MockHost::install().with_capabilities(&[]);
        let source=format!("{IMPORTS} class C {{ @Command(name=\"run\") void run(@Option(longName=\"count\",shortName='c',defaultValue=\"bad\") int count,@Option(longName=\"count\",shortName='c') int other,@Argument(index=0,defaultValue=\"huge\") long number){{}} }}");
        let problems=problems(&source);
        assert_eq!(problems.iter().filter(|d|d.code.as_deref()==Some("duplicate-option-name")).count(),4);
        assert_eq!(problems.iter().filter(|d|d.code.as_deref()==Some("option-default-format")).count(),1);
        assert_eq!(problems.iter().filter(|d|d.code.as_deref()==Some("argument-default-format")).count(),1);
        assert!(!problems.iter().any(|d|d.code.as_deref()==Some("option-arity-range")));
    }
    #[test]
    fn core_dynamic_foreign_empty_defaults_and_generic_types_remain_unknown() {
        let _h=MockHost::install().with_capabilities(&[]);
        for source in [
            format!("{IMPORTS} class C {{ @Command void run(@Option(longName=NAME,shortName=SHORT,defaultValue=VALUE) int n){{}} }}"),
            format!("{IMPORTS} class C {{ @Command void run(@Option(defaultValue=\"\") int n,@Argument(index=0,defaultValue=\"\") int a){{}} }}"),
            format!("{IMPORTS} class C<Integer> {{ @Command void run(@Option(defaultValue=\"bad\") Integer n){{}} }}"),
            format!("{} class C {{ @Command void run(@Option(defaultValue=\"bad\") int n){{}} }}",IMPORTS.replace("org.springframework.shell.core.command.annotation.Option;","foreign.Option;")),
        ] {assert!(problems(&source).is_empty(),"{source}");}
    }
}
