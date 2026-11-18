//! Declared String request patterns in a proved source HttpSecurity lambda.
//! No matcher strategy, endpoint relation or runtime policy is inferred.
use super::*;
const HTTP: &str = "org.springframework.security.config.annotation.web.builders.HttpSecurity";
#[derive(Clone, Debug)]
pub(crate) struct Pattern {
    pub from: usize,
    pub to: usize,
    pub value: String,
    pub method: Option<String>,
}
fn ident(value: &str) -> bool {
    let mut chars = value.chars();
    value.len() <= 64
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}
// This is a declaration fact, not a resolved matcher implementation. Reject
// inheritance/wildcard/shadowing rather than assign an arbitrary expression a type.
fn declared_method(raw: &str, file: &JavaFile, tokens: &[java::Tok<'_>]) -> Option<Option<String>> {
    if raw.trim() == "null" { return Some(None); }
    if file.imports.iter().any(|i| i.path.ends_with(".*"))
        || file.types.iter().any(|t| t.supers().next().is_some())
    { return None; }
    let parts = java::tokens(raw);
    let names: Vec<_> = parts.iter().map(|t| t.s).collect();
    let simple = file.imports.iter().filter(|i| !i.is_static && i.path.rsplit('.').next() == Some("HttpMethod")).count() == 1
        && file.imports.iter().any(|i| !i.is_static && i.path == "org.springframework.http.HttpMethod")
        && !file.types.iter().any(|t| t.name == "HttpMethod");
    let (owner, verb) = match names.as_slice() {
        ["HttpMethod", ".", verb] if simple => ("HttpMethod", *verb),
        ["org", ".", "springframework", ".", "http", ".", "HttpMethod", ".", verb] => ("org", *verb),
        _ => return None,
    };
    if !matches!(verb, "GET" | "HEAD" | "POST" | "PUT" | "PATCH" | "DELETE" | "OPTIONS" | "TRACE")
        || file.types.iter().any(|t| t.name == owner)
        || file.imports.iter().any(|i| i.is_static && i.path.rsplit('.').next() == Some(owner))
        || tokens.iter().enumerate().filter(|(_, t)| t.s == owner).any(|(i,t)| {
            !file.imports.iter().any(|imp| imp.at <= t.at && t.end() <= imp.end)
                && !tokens.get(i + 1).is_some_and(|t| t.s == ".")
        })
    { return None; }
    Some(Some(verb.to_owned()))
}
pub(crate) fn declarations(doc: &Document) -> Vec<Pattern> {
    if doc.language != "java"
        || !doc.path.ends_with(".java")
        || doc.text.len() > 64 * 1024
        || !crate::path_matcher::closed(&doc.text)
    {
        return Vec::new();
    }
    let file = java::parse(&doc.text);
    let tokens = java::tokens(&doc.text);
    let imports = file
        .imports
        .iter()
        .filter(|i| !i.is_static && i.path.rsplit('.').next() == Some("HttpSecurity"))
        .collect::<Vec<_>>();
    if imports.len() != 1
        || imports[0].path != HTTP
        || file.types.iter().any(|t| t.name == "HttpSecurity")
        || file.types.len() > 64
    {
        return Vec::new();
    }
    // Type parameters can shadow imported framework names. This source subset
    // intentionally withholds generic/angle syntax rather than resolving it.
    if tokens.iter().any(|t| t.s == "<") || tokens.len() > 8192
        || file
            .types
            .iter()
            .map(|t| t.methods().count())
            .sum::<usize>()
            > 128
    {
        return Vec::new();
    }
    let calls = java::calls(&doc.text);
    if calls.len() > 256 {
        return Vec::new();
    }
    let mut out = Vec::new();
    for method in file
        .types
        .iter()
        .filter(|t| t.outer.is_none())
        .flat_map(|t| t.methods())
    {
        let params = method
            .params
            .iter()
            .filter(|p| {
                p.ty.name == "HttpSecurity"
                    && p.ty.text.trim() == "HttpSecurity"
                    && p.ty.args.is_empty()
                    && ident(&p.name)
            })
            .collect::<Vec<_>>();
        let [parameter] = params.as_slice() else {
            continue;
        };
        if tokens
            .iter()
            .enumerate()
            .filter(|(_, t)| method.at < t.at && t.at < method.end && t.s == parameter.name)
            .any(|(i, t)| {
                t.at != parameter.at && tokens.get(i + 1).is_none_or(|next| next.s != ".")
            })
        {
            continue;
        }
        // Local/anonymous types can introduce different receiver bindings. Their
        // semantics require a richer resolver; ordinary Java blocks cannot shadow a parameter.
        if tokens
            .iter()
            .filter(|t| method.at < t.at && t.at < method.end)
            .any(|t| matches!(t.s, "new" | "class" | "record" | "interface" | "enum"))
        {
            continue;
        }
        for authorize in calls.iter().filter(|c| {
            c.name == "authorizeHttpRequests"
                && c.receiver.as_deref() == Some(parameter.name.as_str())
                && !c.chained
                && !c.is_new
                && method.at < c.at
                && c.end <= method.end
                && c.args.len() == 1
        }) {
            let Some(index) = tokens.iter().position(|t| t.at == authorize.at) else {
                continue;
            };
            if index < 2
                || tokens[index - 2].s != parameter.name
                || index >= 3 && tokens[index - 3].s == "."
            {
                continue;
            }
            let argument = &authorize.args[0];
            let lambda = java::tokens(&argument.text);
            let binding = match lambda.as_slice() {
                [name, minus, greater, ..]
                    if ident(name.s) && minus.s == "-" && greater.s == ">" =>
                {
                    Some(name.s)
                }
                [open, name, close, minus, greater, ..]
                    if open.s == "("
                        && close.s == ")"
                        && ident(name.s)
                        && minus.s == "-"
                        && greater.s == ">" =>
                {
                    Some(name.s)
                }
                _ => None,
            };
            let Some(binding) =
                binding.filter(|name| !method.params.iter().any(|p| p.name == *name))
            else {
                continue;
            };
            let binder = lambda
                .iter()
                .find(|t| t.s == binding)
                .map(|t| argument.at + t.at)
                .unwrap_or(usize::MAX);
            if tokens
                .iter()
                .enumerate()
                .filter(|(_, t)| argument.at <= t.at && t.at < argument.end && t.s == binding)
                .any(|(i, t)| t.at != binder && tokens.get(i + 1).is_none_or(|next| next.s != "."))
            {
                continue;
            }
            for call in calls.iter().filter(|c| {
                c.name == "requestMatchers"
                    && c.receiver.as_deref() == Some(binding)
                    && !c.chained
                    && !c.is_new
                    && argument.at < c.at
                    && c.end <= argument.end
                    && !c.args.is_empty()
                    && c.args.len() <= 32
            }) {
                let Some(call_index) = tokens.iter().position(|t| t.at == call.at) else {
                    continue;
                };
                if call_index < 2
                    || tokens[call_index - 2].s != binding
                    || call_index >= 3 && tokens[call_index - 3].s == "."
                {
                    continue;
                }
                let (method, patterns) = if call.args[0].string.is_some() {
                    (None, call.args.as_slice())
                } else {
                    let Some(method) = declared_method(&call.args[0].text, &file, &tokens) else { continue; };
                    (method, &call.args[1..])
                };
                if patterns.is_empty() || patterns.iter().any(|a| a.string.is_none() || a.text.contains('\\')) {
                    continue;
                }
                for arg in patterns {
                    let value = arg.string.as_ref().unwrap();
                    if value.value.is_empty()
                        || value.value.len() > 512
                        || value.value.chars().any(char::is_control)
                        || value.value.contains('`')
                    {
                        continue;
                    }
                    if out.len() == 200 {
                        return out;
                    }
                    out.push(Pattern {
                        from: value.at,
                        to: value.end,
                        value: value.value.clone(),
                        method: method.clone(),
                    });
                }
            }
        }
    }
    out.sort_by_key(|p| p.from);
    out.dedup_by_key(|p| p.from);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{hover::HoverProvider, inlay_hints::InlayHints, mock::MockHost};
    fn doc(body: &str) -> Document {
        Document{path:"/project/Security.java".into(),language:"java".into(),version:7,text:format!("import org.springframework.security.config.annotation.web.builders.HttpSecurity;\r\nclass Security {{ Object filter(HttpSecurity http) {{ /* 😀 */ {body} return http.build(); }} }}")}
    }
    fn all() -> Range {
        Range::new(Position::new(0, 0), Position::new(u32::MAX, u32::MAX))
    }
    #[test]
    fn owned_modern_lambda_exposes_declared_patterns_with_unknown_strategy_and_utf16_ranges() {
        let _h = MockHost::install().with_capabilities(&[]);
        let mut tools = SecurityTools::default();
        let d=doc("http.authorizeHttpRequests(auth -> auth.requestMatchers(\"/admin/**\", \"/api/{id}\").authenticated());");
        let values = declarations(&d);
        assert_eq!(values.len(), 2);
        let at = openidea_plugin::text::position(&d.text, d.text.find("/admin").unwrap());
        let hover = tools.hover(&d, at).unwrap();
        assert_eq!(hover.range.unwrap().start, at);
        assert!(hover.markdown.contains("matcher strategy unresolved"));
        assert!(
            !hover.markdown.contains("Explicit Ant")
                && !hover.markdown.contains("Explicit PathPattern")
        );
        assert!(tools.inlay_hints(&d, all()).is_empty());
    }
    #[test]
    fn parenthesized_and_block_lambdas_are_supported_and_unsaved_changes_do_not_cache() {
        let _h = MockHost::install();
        let mut tools = SecurityTools::default();
        let d=doc("http.authorizeHttpRequests((auth) -> { auth.requestMatchers(\"/before\").permitAll(); });");
        assert_eq!(declarations(&d).len(), 1);
        let changed = Document {
            version: 8,
            text: d.text.replace("/before", "/after"),
            ..d
        };
        let at =
            openidea_plugin::text::position(&changed.text, changed.text.find("/after").unwrap());
        assert!(tools
            .hover(&changed, at)
            .unwrap()
            .markdown
            .contains("/after"));
    }
    #[test]
    fn unowned_receivers_aliases_dynamic_args_and_shadowing_produce_no_invented_patterns() {
        let _h = MockHost::install();
        for body in [
            "other.requestMatchers(\"/x\");",
            "holder.http.authorizeHttpRequests(auth -> auth.requestMatchers(\"/x\"));",
            "var alias=http; alias.authorizeHttpRequests(auth -> auth.requestMatchers(\"/x\"));",
            "http.authorizeHttpRequests(auth -> auth.requestMatchers(PATH));",
            "http.authorizeHttpRequests(auth -> auth.requestMatchers(\"/x\" + suffix));",
            "http.authorizeHttpRequests(auth -> { Object auth; auth.requestMatchers(\"/x\"); });",
            "http.authorizeHttpRequests(auth -> { Other http; http.authorizeHttpRequests(a -> a.requestMatchers(\"/x\")); });",
            "http.authorizeHttpRequests(auth -> { Runnable r=new Runnable(){ public void run(){ auth.requestMatchers(\"/x\"); } }; });",
            "http.authorizeHttpRequests(auth -> { consume((auth) -> auth.requestMatchers(\"/x\")); });",
            "http.authorizeHttpRequests(auth -> auth.requestMatchers(HttpMethod.GET, \"/x\"));",
        ] {assert!(declarations(&doc(body)).is_empty(),"{body}");}
        let d = doc("http.authorizeHttpRequests(auth -> auth.requestMatchers(\"/x\"));");
        for text in [
            d.text.replace(HTTP, "other.HttpSecurity"),
            d.text
                .replace("class Security", "class HttpSecurity {} class Security"),
            d.text.replace("class Security", "class Security<HttpSecurity>"),
            d.text
                .replace("HttpSecurity http)", "HttpSecurity http,Object auth)"),
            d.text.replace(
                "http.authorizeHttpRequests",
                "// http.authorizeHttpRequests",
            ),
        ] {
            assert!(declarations(&Document { text, ..d.clone() }).is_empty());
        }
    }
    #[test]
    fn explicit_method_overload_keeps_each_pattern_method_and_unknown_strategy() {
        let h = MockHost::install().with_capabilities(&[]);
        let mut tools = SecurityTools::default();
        for method in ["org.springframework.http.HttpMethod.GET", "HttpMethod.POST", "null"] {
            let mut d = doc(&format!("http.authorizeHttpRequests(auth -> auth.requestMatchers({method}, \"/api\", \"/admin\").authenticated());"));
            d.text = format!("import org.springframework.http.HttpMethod;\n{}", d.text);
            let found = declarations(&d);
            assert_eq!(found.len(), 2, "{method}");
            let expected = if method == "null" { None } else { method.rsplit('.').next() };
            assert!(found.iter().all(|p| p.method.as_deref() == expected));
            let at = openidea_plugin::text::position(&d.text, d.text.find("/api").unwrap());
            let hover = tools.hover(&d, at).unwrap();
            assert!(hover.markdown.contains("strategy unresolved"));
            if expected.is_none() { assert!(hover.markdown.contains("any method (no specific method declared)")); }
            if let Some(expected) = expected {
                assert!(hover.markdown.contains(&format!("HTTP method: {expected}.")));
                assert!(tools.inlay_hints(&d, all())[0].label.starts_with(expected));
            }
        }
        assert_eq!(h.fetches(), 0);
        assert!(h.process_runs().is_empty());
    }
    #[test]
    fn method_overload_refuses_dynamic_unimported_foreign_or_shadowed_types() {
        let _h = MockHost::install();
        for method in ["HttpMethod.GET", "method", "GET", "other.HttpMethod.GET", "org.springframework.http.HttpMethod.CUSTOM", "org.springframework.http.HttpMethod.GET, PATH"] {
            assert!(declarations(&doc(&format!("http.authorizeHttpRequests(auth -> auth.requestMatchers({method}, \"/api\"));"))).is_empty(), "{method}");
        }
        let mut d = doc("http.authorizeHttpRequests(auth -> auth.requestMatchers(HttpMethod.GET, \"/api\"));");
        d.text = format!("import org.springframework.http.HttpMethod;\n{}", d.text);
        assert_eq!(declarations(&d).len(), 1);
        for text in [
            d.text.replace("org.springframework.http.HttpMethod;", "other.HttpMethod;"),
            d.text.replace("class Security", "class HttpMethod {} class Security"),
            d.text.replace("class Security", "class Security extends Base"),
            d.text.replace("Object filter(", "Object HttpMethod; Object filter("),
            d.text.replace("Object filter(", "Object org; Object filter(" ).replace("requestMatchers(HttpMethod.GET", "requestMatchers(org.springframework.http.HttpMethod.GET"),
            format!("import other.*;\n{}", d.text),
        ] { assert!(declarations(&Document { text, ..d.clone() }).is_empty()); }
    }
    #[test]
    fn method_overload_unsaved_changes_and_range_filter_are_current() {
        let _h = MockHost::install();
        let mut tools = SecurityTools::default();
        let before = doc("http.authorizeHttpRequests(auth -> auth.requestMatchers(org.springframework.http.HttpMethod.GET, \"/before\"));");
        let after = Document {version:8,text:before.text.replace("GET", "POST").replace("/before", "/after"),..before};
        let found = declarations(&after);
        assert_eq!(found[0].method.as_deref(), Some("POST"));
        assert_eq!(found[0].value, "/after");
        let hint = tools.inlay_hints(&after, all()).remove(0);
        assert_eq!(hint.position, openidea_plugin::text::position(&after.text, found[0].to));
        assert!(tools.inlay_hints(&after, Range::new(Position::new(0,0), Position::new(0,0))).is_empty());
    }
    #[test]
    fn settings_and_source_limits_apply_without_changing_explicit_matcher_contracts() {
        let h = MockHost::install();
        let mut tools = SecurityTools::default();
        let mut d = doc("http.authorizeHttpRequests(auth -> auth.requestMatchers(HttpMethod.GET, \"/x\"));");
        d.text = format!("import org.springframework.http.HttpMethod;\n{}", d.text);
        assert_eq!(tools.inlay_hints(&d, all()).len(), 1);
        h.set_setting("springSecurity.checkMatchers", "false");
        assert!(tools.inlay_hints(&d, all()).is_empty());
        h.set_setting("springSecurity.checkMatchers", "true");
        h.set_setting("springSecurity.matcherHover", "false");
        let at = openidea_plugin::text::position(&d.text, d.text.find("/x").unwrap());
        assert!(tools.hover(&d, at).is_none());
        assert_eq!(tools.inlay_hints(&d, all()).len(), 1);
        h.set_setting("springSecurity.matcherInlays", "false");
        assert!(tools.inlay_hints(&d, all()).is_empty());
        let large = Document {
            text: format!("{}{}", d.text, " ".repeat(64 * 1024)),
            ..d
        };
        assert!(declarations(&large).is_empty());
    }
}
