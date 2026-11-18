//! Literal channel edges of Java DSL chains (`IntegrationFlow.from("in")....channel("out")`)
//! and Kotlin `integrationFlow("in") { ... }` builders. Method names follow the Spring
//! Integration reference and API: `from(String)`, `channel(String)`, `wireTap(String)`,
//! `filter(.., f -> f.discardChannel(String))`, `aggregate(a -> a.discardChannel(String))`,
//! `routeToRecipients(r -> r.recipient(String ..).recipientMessageSelector(String, ..))`.
//! Only ordinary string literals count; anything dynamic yields no reference.
use crate::*;
use spring_text::java::{Call, JavaFile, tokens};

const SPEC_CALLS: [(&str, &[&str]); 3] = [
    ("filter", &["discardChannel"]),
    ("aggregate", &["discardChannel"]),
    ("routeToRecipients", &["recipient", "recipientMessageSelector"]),
];
const KOTLIN_FLOW: &str = "org.springframework.integration.dsl.integrationFlow";

fn literal<'a>(text: &str, call: &'a Call, kotlin: bool) -> Option<&'a Str> {
    let arg = call.args.first()?;
    let s = arg.string.as_ref()?;
    let plain = !arg.text.trim_start().starts_with("\"\"\"") && !s.value.contains('\\') && !(kotlin && s.value.contains('$'));
    (plain && text.get(s.at..s.end) == Some(s.value.as_str())).then_some(s)
}

fn flow_ref(text: &str, s: &Str, role: Role, what: &str, owner: usize) -> Ref {
    Ref { owner, ..str_ref(text, s, role, what.into(), false) }
}

fn spec_names(call: &str) -> &'static [&'static str] {
    SPEC_CALLS.iter().find(|(name, _)| *name == call).map_or(&[], |(_, specs)| specs)
}

/// The text between two chained calls is only `.`, optionally with explicit type
/// arguments and calls the call scanner does not report (`.<A, B>transform(..).`).
fn continues_chain(text: &str, from: usize, at: usize) -> bool {
    let Some(gap) = text.get(from..at) else {
        return false;
    };
    let toks = tokens(gap);
    let mut i = 0;
    while i < toks.len() {
        if toks[i].s != "." {
            return false;
        }
        i += 1;
        if toks.get(i).is_some_and(|t| t.s == "<") {
            let mut depth = 0usize;
            while let Some(t) = toks.get(i) {
                depth += t.s.matches('<').count();
                depth = depth.saturating_sub(t.s.matches('>').count());
                i += 1;
                if depth == 0 {
                    break;
                }
            }
        }
        if i == toks.len() {
            return true;
        }
        if !toks[i].is_ident() || toks.get(i + 1).is_none_or(|t| t.s != "(") {
            return false;
        }
        i += 1;
        let mut depth = 0usize;
        while let Some(t) = toks.get(i) {
            depth += usize::from(t.s == "(");
            depth = depth.saturating_sub(usize::from(t.s == ")"));
            i += 1;
            if depth == 0 {
                break;
            }
        }
    }
    false
}

fn lambda_param(arg: &str) -> Option<&str> {
    let (param, _) = arg.split_once("->")?;
    let param = param.trim().trim_start_matches('(').trim_end_matches(')').trim();
    (!param.is_empty() && param.chars().all(|c| c.is_alphanumeric() || c == '_')).then_some(param)
}

/// Java (and Kotlin `IntegrationFlow.from`) chains.
pub(crate) fn java_chains(file: &JavaFile, text: &str, tokens: &[spring_text::java::Tok], calls: &[Call], kotlin: bool, out: &mut Vec<Ref>) {
    let mut ends: Vec<(usize, usize)> = Vec::new();
    let mut members: Vec<(&Call, usize)> = Vec::new();
    for c in calls {
        let starts = c.name == "from"
            && !c.is_new
            && matches!(c.receiver.as_deref(), Some("IntegrationFlow" | "IntegrationFlows"))
            && dsl_factory_owned(file, tokens, c, c.receiver.as_deref().unwrap_or_default());
        let owner = if starts {
            Some(c.at + 1)
        } else if c.chained && !c.is_new {
            ends.iter().find(|(end, _)| continues_chain(text, *end, c.at)).map(|(_, owner)| *owner)
        } else {
            None
        };
        let Some(owner) = owner else {
            continue;
        };
        ends.push((c.end, owner));
        members.push((c, owner));
        let Some(s) = literal(text, c, kotlin) else {
            continue;
        };
        match c.name.as_str() {
            "from" => out.push(flow_ref(text, s, Role::Consumes, "IntegrationFlow.from", owner)),
            "channel" => {
                out.push(flow_ref(text, s, Role::Declares, "flow .channel", owner));
                out.push(flow_ref(text, s, Role::Produces, "flow .channel", owner));
            }
            "wireTap" => out.push(flow_ref(text, s, Role::Produces, "flow .wireTap", owner)),
            _ => {}
        }
    }
    for &(member, owner) in &members {
        let one_expression = member.args.len() == 1 || member.args.len() == 2 && member.args[1].text.contains("->");
        if one_expression && matches!(member.name.as_str(), "filter" | "route" | "transform" | "split") {
            if let Some(s) = literal(text, member, kotlin) {
                out.extend(spel::refs(text, s.at, &s.value, &format!("flow .{}", member.name), owner));
            }
        }
        let specs = spec_names(&member.name);
        let Some(lambda) = member.args.last().filter(|_| !specs.is_empty()) else {
            continue;
        };
        let Some(param) = lambda_param(&lambda.text) else {
            continue;
        };
        let mut spec_ends: Vec<usize> = Vec::new();
        for c in calls.iter().filter(|c| lambda.at <= c.at && c.end <= lambda.end && specs.contains(&c.name.as_str())) {
            let on_param = c.receiver.as_deref() == Some(param) && !c.chained;
            let chained = c.chained && spec_ends.iter().any(|end| continues_chain(text, *end, c.at));
            if !on_param && !chained {
                continue;
            }
            spec_ends.push(c.end);
            if let Some(s) = literal(text, c, kotlin) {
                let role = Role::Produces;
                out.push(flow_ref(text, s, role, &format!("flow .{}", c.name), owner));
            }
        }
    }
}

fn kotlin_flow_imported(file: &JavaFile, text: &str) -> bool {
    let imported = file.imports.iter().any(|i| !i.is_static && (i.path == KOTLIN_FLOW || i.path == "org.springframework.integration.dsl.*"));
    let shadowed = tokens(text).windows(2).any(|w| w[0].s == "fun" && w[1].s == "integrationFlow");
    imported && !shadowed
}

struct Block {
    open: usize,
    close: usize,
    parent: Option<usize>,
    open_token: usize,
}

fn blocks(toks: &[spring_text::java::Tok]) -> Vec<Block> {
    let (mut out, mut stack): (Vec<Block>, Vec<usize>) = (Vec::new(), Vec::new());
    for (i, t) in toks.iter().enumerate() {
        if t.s == "{" {
            out.push(Block { open: t.at, close: usize::MAX, parent: stack.last().copied(), open_token: i });
            stack.push(out.len() - 1);
        } else if t.s == "}" {
            if let Some(b) = stack.pop() {
                out[b].close = t.at;
            }
        }
    }
    out
}

fn innermost(blocks: &[Block], at: usize) -> Option<usize> {
    (0..blocks.len()).filter(|&b| blocks[b].open < at && at < blocks[b].close).max_by_key(|&b| blocks[b].open)
}

fn statement_level(text: &str, at: usize) -> bool {
    text[..at].trim_end_matches([' ', '\t']).chars().next_back().is_none_or(|c| matches!(c, '\n' | '\r' | '{' | ';'))
}

/// The name before `X { }`, `X<T> { }`, `X(..) { }` or `X<T>(..) { }`, and where it is written.
fn block_owner<'a>(toks: &[spring_text::java::Tok<'a>], open: usize) -> Option<(&'a str, usize)> {
    let skip_back = |mut i: usize, close: &str, open_s: &str| -> Option<usize> {
        let mut depth = 0usize;
        loop {
            let s = toks.get(i)?.s;
            depth += s.matches(close).count();
            depth = depth.saturating_sub(s.matches(open_s).count());
            if depth == 0 {
                return Some(i);
            }
            i = i.checked_sub(1)?;
        }
    };
    let mut i = open.checked_sub(1)?;
    if toks[i].s == ")" {
        i = skip_back(i, ")", "(")?.checked_sub(1)?;
    }
    if toks[i].s.ends_with('>') {
        i = skip_back(i, ">", "<")?.checked_sub(1)?;
    }
    let name = toks[i];
    name.is_ident().then_some((name.s, name.at))
}

/// Kotlin `integrationFlow("in") { ... }` builders.
pub(crate) fn kotlin_flows(file: &JavaFile, text: &str, calls: &[Call], out: &mut Vec<Ref>) {
    if !kotlin_flow_imported(file, text) {
        return;
    }
    let toks = tokens(text);
    let blocks = blocks(&toks);
    for flow in calls.iter().filter(|c| c.name == "integrationFlow" && c.receiver.is_none() && !c.chained && !c.is_new) {
        let Some(body) = blocks.iter().position(|b| b.open >= flow.end && text[flow.end..b.open].trim().is_empty()) else {
            continue;
        };
        let owner = flow.at + 1;
        if let Some(s) = literal(text, flow, true) {
            out.push(flow_ref(text, s, Role::Consumes, "integrationFlow", owner));
        }
        let statement = |c: &&Call| c.receiver.is_none() && !c.chained && !c.is_new && statement_level(text, c.at);
        for c in calls.iter().filter(statement).filter(|c| innermost(&blocks, c.at) == Some(body)) {
            let Some(s) = literal(text, c, true) else {
                continue;
            };
            match c.name.as_str() {
                "channel" => {
                    out.push(flow_ref(text, s, Role::Declares, "flow channel", owner));
                    out.push(flow_ref(text, s, Role::Produces, "flow channel", owner));
                }
                "wireTap" => out.push(flow_ref(text, s, Role::Produces, "flow wireTap", owner)),
                _ => {}
            }
        }
        for (b, block) in blocks.iter().enumerate().filter(|(_, b)| b.parent == Some(body)) {
            let Some((owner_name, owner_at)) = block_owner(&toks, block.open_token) else {
                continue;
            };
            let specs = spec_names(owner_name);
            if specs.is_empty() || !statement_level(text, owner_at) {
                continue;
            }
            for c in calls.iter().filter(statement).filter(|c| innermost(&blocks, c.at) == Some(b) && specs.contains(&c.name.as_str())) {
                if let Some(s) = literal(text, c, true) {
                    out.push(flow_ref(text, s, Role::Produces, &format!("flow {}", c.name), owner));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{Ref, Role, parse};

    const JAVA_HEAD: &str = "package app; import org.springframework.integration.dsl.IntegrationFlow; class F { Object f(){ return ";
    const KT_HEAD: &str = "package app\nimport org.springframework.integration.dsl.integrationFlow\nclass F {\n  fun f() =\n";

    fn edges(refs: &[Ref]) -> Vec<(Role, String, String)> {
        refs.iter()
            .filter(|r| r.owner != 0 && matches!(r.role, Role::Consumes | Role::Produces))
            .map(|r| (r.role, r.name.clone(), r.what.clone()))
            .collect()
    }

    fn java(chain: &str) -> Vec<Ref> {
        parse("F.java", &format!("{JAVA_HEAD}{chain}; }} }}"))
    }

    fn kotlin(body: &str) -> Vec<Ref> {
        parse("F.kt", &format!("{KT_HEAD}{body}\n}}\n"))
    }

    fn produced(refs: &[Ref]) -> Vec<&str> {
        refs.iter().filter(|r| r.owner != 0 && r.role == Role::Produces).map(|r| r.name.as_str()).collect()
    }

    #[test]
    fn java_chain_links_input_to_output_through_intermediate_endpoints() {
        let source = format!("{JAVA_HEAD}IntegrationFlow.from(\"in\").<String, String>transform(String::trim).handle(h).channel(\"out\"); }} }}");
        let refs = parse("F.java", &source);
        assert_eq!(edges(&refs).iter().map(|e| (e.0, e.1.as_str())).collect::<Vec<_>>(), [(Role::Consumes, "in"), (Role::Produces, "out")]);
        let out = refs.iter().find(|r| r.role == Role::Declares).unwrap();
        assert_eq!(out.name, "out");
        let at = source.find("\"out\"").unwrap();
        assert_eq!(out.range, spring_text::range(&source, at + 1, at + 4));
    }

    #[test]
    fn java_spec_lambdas_name_discard_and_recipient_channels() {
        let refs = java("IntegrationFlow.from(\"in\").filter(p -> true, f -> f.discardChannel(\"rejected\")).aggregate(a -> a.discardChannel(\"late\")).routeToRecipients(r -> r.recipient(\"a\", \"true\").recipientMessageSelector(\"b\", m -> true).recipient(\"c\")).wireTap(\"tap\").channel(\"out\")");
        assert_eq!(produced(&refs), ["tap", "out", "rejected", "late", "a", "b", "c"]);
    }

    #[test]
    fn java_refuses_dynamic_foreign_and_non_chain_names() {
        for chain in [
            "IntegrationFlow.from(\"in\").channel(prefix + \"out\")",
            "IntegrationFlow.from(\"in\").channel(\"o\\u0075t\")",
            "IntegrationFlow.from(\"in\").filter(p -> true, f -> f.discardChannel(name))",
            "IntegrationFlow.from(\"in\").filter(p -> true, f -> other.discardChannel(\"x\"))",
            "IntegrationFlow.from(\"in\").handle(h, e -> e.discardChannel(\"x\"))",
            "IntegrationFlow.from(\"in\").handle(h).other.channel(\"x\")",
            "Other.from(\"in\").channel(\"x\")",
            "IntegrationFlow.from(\"in\").transform(t -> helper.channel(\"x\"))",
        ] {
            assert!(produced(&java(chain)).is_empty(), "{chain}");
        }
    }

    #[test]
    fn kotlin_builder_links_input_to_literal_outputs() {
        let refs = kotlin("integrationFlow(\"in\") {\n    transform<String> { it.trim() }\n    filter<String>({ it != \"x\" }) { discardChannel(\"rejected\") }\n    routeToRecipients { recipient(\"a\"); recipient(\"b\", \"true\") }\n    wireTap(\"tap\")\n    channel(\"out\")\n}");
        assert_eq!(edges(&refs).iter().map(|e| (e.0, e.1.as_str())).collect::<Vec<_>>(), [
            (Role::Consumes, "in"),
            (Role::Produces, "tap"),
            (Role::Declares, "out"),
            (Role::Produces, "out"),
            (Role::Produces, "rejected"),
            (Role::Produces, "a"),
            (Role::Produces, "b"),
        ].iter().filter(|e| e.0 != Role::Declares).copied().collect::<Vec<_>>());
    }

    #[test]
    fn kotlin_builder_refuses_dynamic_unimported_and_nested_names() {
        for body in [
            "integrationFlow(\"in\") { channel(\"o${x}\") }",
            "integrationFlow(\"in\") { channel(name) }",
            "integrationFlow(\"in\") { transform<String> { channel(\"x\") } }",
            "integrationFlow(\"in\") { val c = channel(\"x\") }",
            "integrationFlow(\"in\") { handle<String> { _, _ -> other.discardChannel(\"x\") } }",
        ] {
            assert!(produced(&kotlin(body)).is_empty(), "{body}");
        }
        let unimported = parse("F.kt", "package app\nclass F { fun f() = integrationFlow(\"in\") { channel(\"out\") } }\n");
        assert!(unimported.iter().all(|r| r.owner == 0), "{unimported:?}");
        let shadowed = parse("F.kt", "package app\nimport org.springframework.integration.dsl.integrationFlow\nfun integrationFlow(c: String, f: () -> Unit) {}\nclass F { fun f() = integrationFlow(\"in\") { channel(\"out\") } }\n");
        assert!(shadowed.iter().all(|r| r.owner == 0));
    }

    #[test]
    fn two_flows_keep_separate_endpoints() {
        let refs = parse("F.java", &format!("{JAVA_HEAD}IntegrationFlow.from(\"a\").channel(\"b\"); }} Object g(){{ return IntegrationFlow.from(\"b\").channel(\"c\"); }} }}"));
        let owners: std::collections::BTreeSet<usize> = refs.iter().filter(|r| r.owner != 0).map(|r| r.owner).collect();
        assert_eq!(owners.len(), 2);
    }
}
