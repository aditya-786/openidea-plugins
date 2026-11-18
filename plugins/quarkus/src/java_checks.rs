//! Checks of Java sources that need no type information: `@ConfigProperty` keys that no config file sets,
//! `@RestClient` injections of interfaces without `@RegisterRestClient`, and `@Scheduled` expressions.
//! Written from the Quarkus guides (config reference, REST client, scheduler).

use crate::checks::squash;
use crate::java::{Injection, injections};
use spring_text::finding::Finding;
use spring_text::java::{self, Annotation, JavaFile};
use spring_text::metadata::{Lookup, strip_profile};
use std::collections::{BTreeMap, BTreeSet};

pub enum Fix {
    DefaultValue {
        at: usize,
        value: &'static str,
    },
    AddKey {
        key: String,
    },
    RegisterRestClient {
        name: String,
    },
    /// Byte-range edits of the checked document.
    Edits {
        title: String,
        edits: Vec<(usize, usize, String)>,
    },
}

const HEALTH_QUALIFIERS: [&str; 4] = ["Liveness", "Readiness", "Wellness", "Health"];
const SCOPES: [&str; 4] = ["ApplicationScoped", "RequestScoped", "SessionScoped", "Dependent"];
pub const REGISTER_REST_CLIENT: &str = "org.eclipse.microprofile.rest.client.inject.RegisterRestClient";

fn import_edit(text: &str, fqcn: &str) -> Option<(usize, usize, String)> {
    if text.contains(&format!("import {fqcn};")) {
        return None;
    }
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        offset += line.len();
        if line.starts_with("package ") {
            let prefix = if line.ends_with('\n') { "" } else { "\n" };
            return Some((offset, offset, format!("{prefix}import {fqcn};\n")));
        }
    }
    Some((0, 0, format!("import {fqcn};\n")))
}

/// Add the annotation at its declaration; fully qualified spellings need no import.
pub fn annotate_edits(text: &str, start: usize, annotation: &str, import: &str) -> Vec<(usize, usize, String)> {
    let line_start = text[..start].rfind('\n').map_or(0, |i| i + 1);
    let indent = &text[line_start..start];
    let (at, prefix) = if indent.trim().is_empty() { (line_start, indent) } else { (start, "") };
    let mut edits = vec![(at, at, format!("{prefix}@{annotation}\n"))];
    if !annotation.contains('.') && let Some(import) = import_edit(text, import) { edits.push(import); }
    merge(edits)
}

fn merge(mut edits: Vec<(usize, usize, String)>) -> Vec<(usize, usize, String)> {
    edits.sort_by_key(|(a, b, _)| (*a, *b));
    let mut out: Vec<(usize, usize, String)> = Vec::new();
    for edit in edits {
        match out.last_mut() {
            Some(last) if last.1 == edit.0 && last.0 == last.1 && edit.0 == edit.1 => last.2.push_str(&edit.2),
            _ => out.push(edit),
        }
    }
    out
}


pub struct Problem {
    pub finding: Finding,
    pub fixes: Vec<Fix>,
}

pub struct Context<'a> {
    pub lookup: &'a Lookup<'a>,
    /// The squashed keys every config file and `.env` sets; `None` while the project is not fully read.
    pub configured: Option<&'a BTreeSet<String>>,
    /// Interface name to whether it carries `@RegisterRestClient`; `None` while the project is not fully read.
    pub rest_clients: Option<&'a BTreeMap<String, bool>>,
    pub qualified_rest_clients: bool,
    pub unassigned: bool,
    pub annotations: bool,
    pub health_metrics: bool,
    /// Resource methods and scheduler identities of the module's other files; `None` while the project is not fully read.
    pub module: Option<&'a crate::jaxrs::Facts>,
}

pub fn check(text: &str, ctx: &Context) -> Vec<Problem> {
    let mentions = |needle: &str| text.contains(needle);
    let mut out = Vec::new();
    let health_metrics = ctx.health_metrics
        && (mentions("HealthCheck") || mentions("@org.eclipse.microprofile.health.") || mentions("@Health") || mentions("@Liveness") || mentions("@Readiness") || mentions("@Wellness") || mentions("@Gauge") || mentions("@org.eclipse.microprofile.metrics.annotation.Gauge"));
    if !(mentions("@ConfigProperty") || mentions("@RestClient") || mentions("@org.eclipse.microprofile.rest.client.inject.RestClient") || mentions("@Scheduled") || mentions("@io.quarkus.scheduler.Scheduled") || mentions("ws.rs") || mentions("RestPath") || health_metrics) {
        return out;
    }
    let file = java::parse(text);
    if ctx.unassigned && mentions("@ConfigProperty") {
        unassigned(&file, text, ctx, &mut out);
    }
    if ctx.annotations {
        rest_clients(&file, text, ctx, &mut out);
        scheduled(&file, text, ctx, &mut out);
        let mut found = Vec::new();
        crate::jaxrs::check(&file, text, ctx.module, &mut found);
        out.extend(found.into_iter().map(|finding| Problem { finding, fixes: Vec::new() }));
    }
    if health_metrics {
        health(&file, text, &mut out);
        gauges(&file, text, &mut out);
    }
    out
}

fn default_for(ty: &str) -> &'static str {
    match ty {
        "int" | "long" | "short" | "byte" | "double" | "float" | "Integer" | "Long" | "Short" | "Byte" | "Double" | "Float" => "0",
        "boolean" | "Boolean" => "false",
        _ => "",
    }
}

fn unassigned(file: &JavaFile, text: &str, ctx: &Context, out: &mut Vec<Problem>) {
    let Some(configured) = ctx.configured.filter(|set| !set.is_empty()) else { return };
    for Injection { key, at, end, optional, has_default, ty, insert_at } in injections(file, text) {
        let provided = key.contains("${")
            || key.starts_with("quarkus.")
            || ctx.lookup.prop(&key).is_some_and(|p| !p.default.is_empty())
            || configured.contains(&squash(strip_profile(&key)));
        if optional || has_default || provided || key.is_empty() {
            continue;
        }
        let message = format!(
            "`{key}` has no default and no application config file or .env sets it. Quarkus fails at startup unless a system property, the environment or another source supplies it."
        );
        out.push(Problem {
            finding: Finding::new(at, end, "unassigned-property", message),
            fixes: vec![Fix::DefaultValue { at: insert_at, value: default_for(&ty) }, Fix::AddKey { key }],
        });
    }
}

fn rest_clients(file: &JavaFile, text: &str, ctx: &Context, out: &mut Vec<Problem>) {
    let Some(known) = ctx.rest_clients else { return };
    let mut injected = Vec::new();
    for t in &file.types {
        for m in &t.members {
            if let (Some(_), Some(ty)) = (m.annotation("RestClient").filter(|a| crate::java::annotation_owned(file, text, a, "org.eclipse.microprofile.rest.client.inject.RestClient")), &m.ty) {
                injected.push((ty, m.at, m.name.len()));
            }
            for p in &m.params {
                if p.annotation("RestClient").is_some_and(|a| crate::java::annotation_owned(file, text, a, "org.eclipse.microprofile.rest.client.inject.RestClient")) {
                    injected.push((&p.ty, p.at, p.name.len()));
                }
            }
        }
    }
    for (ty, at, len) in injected {
        let imported: Vec<_> =
            file.imports.iter().filter(|import| !import.is_static && import.path.rsplit('.').next() == Some(ty.name.as_str())).collect();
        let qualified = if ty.text.contains('.') && !ty.text.contains('<') {
            Some(ty.text.clone())
        } else if imported.len() == 1 {
            Some(imported[0].path.clone())
        } else if imported.is_empty() {
            Some(if file.package.is_empty() { ty.name.clone() } else { format!("{}.{}", file.package, ty.name) })
        } else {
            None
        };
        let registered = if ctx.qualified_rest_clients { qualified.as_ref().and_then(|name| known.get(name)) } else { known.get(&ty.name) };
        if registered != Some(&false) {
            continue;
        }
        let message = format!("`{}` is injected with @RestClient but is not annotated with @RegisterRestClient.", ty.name);
        out.push(Problem {
            finding: Finding::new(at, at + len, "rest-client-not-registered", message),
            fixes: vec![Fix::RegisterRestClient { name: if ctx.qualified_rest_clients { qualified.unwrap_or_else(|| ty.name.clone()) } else { ty.name.clone() } }],
        });
    }
}

fn health(file: &JavaFile, text: &str, out: &mut Vec<Problem>) {
    for t in file.types.iter().filter(|t| t.kind == "class" && t.outer.is_none() && !t.modifiers.iter().any(|m| m == "abstract")) {
        let owned_check = |ty: &java::TypeRef| ty.text == "org.eclipse.microprofile.health.HealthCheck"
            || ty.text == "HealthCheck" && crate::java::source_type_owned(file, "org.eclipse.microprofile.health.HealthCheck");
        let implements = t.implements.iter().any(owned_check);
        let owned_qualifier = |a: &Annotation| ["Liveness", "Readiness", "Health"].contains(&a.name.as_str())
            && crate::java::annotation_owned(file, text, a, &format!("org.eclipse.microprofile.health.{}", a.name));
        if t.annotations.iter().any(|a| HEALTH_QUALIFIERS.contains(&a.name.as_str()) && !owned_qualifier(a)) { continue; }
        let qualifier = t.annotations.iter().find(|a| owned_qualifier(a));

        if let Some(qualifier) = qualifier.filter(|_| !implements && t.extends.is_empty() && t.implements.is_empty()) {
            let message = format!(
                "`{}` declares @{} without a direct HealthCheck interface; inherited or runtime registration is not inferred.",
                t.name, qualifier.name
            );
            if let Some(brace) = text[t.at..].find('{').map(|i| t.at + i) {
                let header_end = text[..brace].trim_end().len();
                let edits = vec![(header_end, header_end, " implements org.eclipse.microprofile.health.HealthCheck".to_owned())];
                out.push(Problem {
                    finding: Finding::new(qualifier.at, qualifier.end, "health-not-implemented", message),
                    fixes: vec![Fix::Edits { title: format!("Let {} implement HealthCheck", t.name), edits }],
                });
            }
        }

        let Some(check) = t.implements.iter().find(|i| owned_check(i)).filter(|_| qualifier.is_none()) else { continue };
        let message = format!(
            "`{}` implements HealthCheck without @Liveness, @Readiness or @Health, so Quarkus may not register it.",
            t.name
        );
        let fixes = ["Liveness", "Readiness"]
            .iter()
            .map(|q| Fix::Edits {
                title: format!("Annotate {} with @{q}", t.name),
                edits: annotate_edits(text, t.start, &format!("org.eclipse.microprofile.health.{q}"), &format!("org.eclipse.microprofile.health.{q}")),
            })
            .collect();
        out.push(Problem { finding: Finding::new(check.at, check.at + check.name.len(), "health-without-qualifier", message), fixes });
    }
}

fn gauges(file: &JavaFile, text: &str, out: &mut Vec<Problem>) {
    for t in file.types.iter().filter(|t| t.kind == "class") {
        let scopes: Vec<_> = t.annotations.iter().filter(|a| SCOPES.contains(&a.name.as_str()) && ["javax", "jakarta"].iter().any(|namespace| crate::java::annotation_owned(file, text, a, &format!("{namespace}.enterprise.context.{}", a.name)))).collect();
        let scoped = scopes.iter().any(|a| a.name == "ApplicationScoped");

        for (member, gauge) in t.members.iter().filter_map(|m| m.annotation("Gauge").filter(|a| crate::java::annotation_owned(file, text, a, "org.eclipse.microprofile.metrics.annotation.Gauge")).map(|a| (m, a))) {
            if !scoped && !scopes.is_empty() {
                let message = format!("@Gauge needs an @ApplicationScoped bean, `{}` has another scope.", t.name);
                let edits = scopes.iter().take(1).map(|a| (a.at, a.end, "@jakarta.enterprise.context.ApplicationScoped".to_owned())).collect();
                out.push(Problem {
                    finding: Finding::new(gauge.at, gauge.end, "gauge-scope", message),
                    fixes: vec![Fix::Edits { title: "Use @ApplicationScoped".into(), edits }],
                });
            }

            if matches!(
                member.ty.as_ref().map(|ty| ty.name.as_str()),
                Some("void" | "String" | "boolean" | "Boolean" | "char" | "Character")
            ) {
                let message = format!("`{}` is annotated with @Gauge but does not return a number.", member.name);
                out.push(Problem { finding: Finding::new(gauge.at, gauge.end, "gauge-return-type", message), fixes: Vec::new() });
            }
        }
    }
}

fn cron_fields_are_free(ctx: &Context) -> bool {
    ctx.configured.is_some_and(|set| !set.contains(&squash("quarkus.scheduler.cron-type")))
}

/// Keys of `${key}` references without default or nesting, and of a whole-value `{key}` (older style).
fn config_references(value: &str) -> Vec<&str> {
    let plain = |key: &str| !key.is_empty() && !key.contains(['{', '}', '$', ':', ' ']);
    if let Some(key) = value.strip_prefix('{').and_then(|v| v.strip_suffix('}')) {
        return Some(key).filter(|k| plain(k)).into_iter().collect();
    }
    if value.contains("${") && value.split("${").skip(1).any(|rest| !rest.contains('}') || rest.split('}').next().is_some_and(|key| key.contains(['{', '$']))) {
        return Vec::new();
    }
    value.split("${").skip(1).filter_map(|rest| rest.split_once('}').map(|(key, _)| key)).filter(|k| plain(k)).collect()
}

/// `Duration.parse` after the scheduler's prefixing: a leading digit gets `PT`, or `P` when the value ends in `d`.
/// Quarkus scheduler reference: "If the value starts with a digit and ends with 'd', 'P' is prepended; if it starts with a digit, 'PT'."
fn scheduler_interval(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    let prefixed = match lower.chars().next() {
        Some(c) if c.is_ascii_digit() && lower.ends_with('d') => format!("p{lower}"),
        Some(c) if c.is_ascii_digit() => format!("pt{lower}"),
        _ => lower,
    };
    let rest = prefixed.strip_prefix(['+', '-']).unwrap_or(&prefixed);
    let Some(rest) = rest.strip_prefix('p') else { return false };
    let (days, time) = match rest.split_once('t') {
        Some((days, time)) => (days, Some(time)),
        None => (rest, None),
    };
    let take = |input: &str, unit: char, fractional: bool| -> Option<(bool, String)> {
        let end = input.find(|c: char| !(c.is_ascii_digit() || fractional && c == '.' || c == '-' || c == '+'))?;
        let (number, tail) = input.split_at(end);
        let tail = tail.strip_prefix(unit)?;
        let digits = number.trim_start_matches(['+', '-']);
        let ok = !digits.is_empty() && digits.chars().next().is_some_and(|c| c.is_ascii_digit()) && (fractional || digits.chars().all(|c| c.is_ascii_digit()));
        Some((ok, tail.to_owned()))
    };
    let mut found = false;
    if !days.is_empty() {
        match take(days, 'd', false) {
            Some((true, tail)) if tail.is_empty() => found = true,
            _ => return false,
        }
    }
    if let Some(time) = time {
        let mut rest = time.to_owned();
        if time.is_empty() {
            return false;
        }
        for (unit, fractional) in [('h', false), ('m', false), ('s', true)] {
            if let Some((ok, tail)) = take(&rest, unit, fractional) {
                if !ok {
                    return false;
                }
                found = true;
                rest = tail;
            }
        }
        return found && rest.is_empty();
    }
    found
}

fn scheduled(file: &JavaFile, text: &str, ctx: &Context, out: &mut Vec<Problem>) {
    let annotations: Vec<&Annotation> = file.types.iter().flat_map(|t| &t.members).filter_map(|m| m.annotation("Scheduled")).filter(|a| crate::java::annotation_owned(file, text, a, "io.quarkus.scheduler.Scheduled")).collect();
    for a in annotations {
        let literal = |name: &str| {
            let arg = a.arg(name)?;
            let [value] = arg.strings.as_slice() else { return None };
            (arg.text.matches('"').count() == 2
                && arg.text.starts_with('"')
                && arg.text.ends_with('"')
                && !value.value.contains(['{', '$']))
            .then_some((arg, value))
        };
        for name in ["cron", "every", "timeZone", "delayed"] {
            let Some(arg) = a.arg(name) else { continue };
            let [value] = arg.strings.as_slice() else { continue };
            let plain = arg.text.matches('"').count() == 2 && arg.text.starts_with('"') && arg.text.ends_with('"');
            let Some(configured) = ctx.configured.filter(|_| ctx.unassigned && plain) else { continue };
            for key in config_references(&value.value) {
                if configured.contains(&squash(key)) || key.starts_with("quarkus.") {
                    continue;
                }
                let message = format!("`{key}` has no default and no config file sets it; the scheduler fails at startup unless an environment variable or system property provides it. Use `${{{key}:default}}` for an optional value.");
                out.push(Problem { finding: Finding::new(value.at, value.end, "unassigned-schedule-property", message), fixes: Vec::new() });
            }
        }
        if let Some((_, cron)) = literal("cron").filter(|(_, value)| !value.value.is_empty()) {
            let fields = cron.value.split_whitespace().count();
            let off = matches!(cron.value.as_str(), "off" | "disabled");
            if cron_fields_are_free(ctx) && !off && !(6..=7).contains(&fields) {
                let message = format!(
                    "A Quartz cron expression has 6 or 7 fields, `{}` has {fields}. Set quarkus.scheduler.cron-type for another syntax.",
                    cron.value
                );
                out.push(Problem { finding: Finding::new(cron.at, cron.end, "invalid-cron", message), fixes: Vec::new() });
            }
        }
        if (a.arg("cron").is_none() || literal("cron").is_some_and(|(_, value)| value.value.is_empty()))
            && let Some((_, every)) = literal("every") {
            let value = every.value.trim();
            if !matches!(value, "off" | "disabled") && !scheduler_interval(value) {
                let message = format!("`{value}` is not a duration such as `10s`, `5m`, `2d` or `PT1H`; the scheduler parses it with Duration.parse after adding a `PT` (or `P` for a trailing `d`) prefix to a value starting with a digit.");
                out.push(Problem { finding: Finding::new(every.at, every.end, "invalid-every", message), fixes: Vec::new() });
            }
        }
        if a.arg("cron").is_none() && a.arg("every").is_none() {
            let message = "@Scheduled needs `cron` or `every`.";
            out.push(Problem {
                finding: Finding::new(a.at, a.name_at + a.name.len(), "scheduled-without-trigger", message),
                fixes: Vec::new(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bundled;

    fn run(text: &str, configured: &[&str], clients: &[(&str, bool)]) -> Vec<(&'static str, String)> {
        let configured: BTreeSet<String> = configured.iter().map(|k| squash(k)).collect();
        let clients: BTreeMap<String, bool> = clients.iter().map(|(n, r)| ((*n).to_owned(), *r)).collect();
        let lookup = Lookup(vec![bundled()]);
        let ctx = Context {
            lookup: &lookup,
            configured: Some(&configured),
            qualified_rest_clients: false,
            rest_clients: Some(&clients),
            unassigned: true,
            annotations: true,
            health_metrics: true,
            module: None,
        };
        check(text, &ctx).iter().map(|p| (p.finding.code, text[p.finding.start..p.finding.end].to_owned())).collect()
    }

    #[test]
    fn scheduler_interval_follows_prefix_rules() {
        for ok in ["10s", "5m", "1h30m", "2d", "1.5s", "PT1H", "P1D", "PT0.5S"] {
            assert!(scheduler_interval(ok), "{ok}");
        }
        for bad in ["10", "30ms", "1h 30m", "soon", "P", "PT", "1d2h", "-"] {
            assert!(!scheduler_interval(bad), "{bad}");
        }
    }

    #[test]
    fn schedule_config_references_need_a_configured_key_or_default() {
        let text = r#"import io.quarkus.scheduler.Scheduled; class J {
            @Scheduled(every = "${job.every}") void a() {}
            @Scheduled(every = "${job.other:10s}") void b() {}
            @Scheduled(cron = "{job.cron}") void c() {}
            @Scheduled(cron = "${job.known}") void d() {}
            @Scheduled(every = "${a.${b}}") void e() {}
            @Scheduled(every = "${quarkus.x}") void f() {}
        }"#;
        let found = run(text, &["job.known"], &[]);
        let refs: Vec<_> = found.iter().filter(|(c, _)| *c == "unassigned-schedule-property").map(|(_, t)| t.as_str()).collect();
        assert_eq!(refs, ["${job.every}", "{job.cron}"]);
    }

    #[test]
    fn unassigned_property_is_reported_only_when_nothing_provides_it() {
        let text = r#"import org.eclipse.microprofile.config.inject.ConfigProperty; class A {
            @ConfigProperty(name = "app.a") String a;
            @ConfigProperty(name = "app.b", defaultValue = "x") String b;
            @ConfigProperty(name = "app.c") Optional<String> c;
            @ConfigProperty(name = "app.d") String d;
            @ConfigProperty(name = "quarkus.http.port") int port;
            @ConfigProperty(name = "quarkus.acme") String acme;
            @ConfigProperty(name = "${x}") String e;
            @ConfigProperty(name = "app." + SUFFIX) String f;
            @ConfigProperty String g;
        }"#;
        assert_eq!(run(text, &["app.d", "other"], &[]), [("unassigned-property", "app.a".into())]);
        assert!(run(text, &[], &[]).is_empty());
    }

    #[test]
    fn rest_client_must_be_registered() {
        let text = "import org.eclipse.microprofile.rest.client.inject.RestClient; class A { @Inject @RestClient Foo foo; @Inject @RestClient Bar bar; @Inject @RestClient Lib lib; }";
        assert_eq!(run(text, &[], &[("Foo", false), ("Bar", true)]), [("rest-client-not-registered", "foo".into())]);
    }

    #[test]
    fn scheduled_expressions_are_checked() {
        let text = r#"import io.quarkus.scheduler.Scheduled; class J {
            @Scheduled(cron = "0 0/5 * * * ?") void ok() {}
            @Scheduled(cron = "0/5 * * * *") void five() {}
            @Scheduled(cron = "{job.cron}") void configured() {}
            @Scheduled(every = "10s") void every() {}
            @Scheduled(every = "soon") void bad() {}
            @Scheduled(identity = "x") void none() {}
            @Scheduled(cron = "off") void off() {}
        }"#;
        let found = run(text, &["other"], &[]);
        let codes: Vec<_> = found.iter().map(|(c, _)| *c).collect();
        assert_eq!(codes, ["invalid-cron", "unassigned-schedule-property", "invalid-every", "scheduled-without-trigger"]);
        assert_eq!(found[0].1, "0/5 * * * *");
        let other_syntax = run(text, &["quarkus.scheduler.cron-type"], &[]);
        assert!(!other_syntax.iter().any(|(c, _)| *c == "invalid-cron"));
    }

    fn apply(text: &str, edits: &[(usize, usize, String)]) -> String {
        let mut out = text.to_owned();
        for (a, b, new) in edits.iter().rev() {
            out.replace_range(*a..*b, new);
        }
        out
    }

    fn fixed(text: &str) -> Vec<(&'static str, Vec<String>)> {
        let lookup = Lookup(vec![bundled()]);
        let ctx = Context {
            lookup: &lookup,
            configured: None,
            rest_clients: None,
            qualified_rest_clients: false,
            unassigned: false,
            annotations: false,
            health_metrics: true,
            module: None,
        };
        check(text, &ctx)
            .into_iter()
            .map(|p| {
                let results =
                    p.fixes.iter().filter_map(|f| if let Fix::Edits { edits, .. } = f { Some(apply(text, edits)) } else { None }).collect();
                (p.finding.code, results)
            })
            .collect()
    }

    #[test]
    fn a_health_qualifier_without_the_interface_offers_to_implement_it() {
        let text = "package a;\nimport org.eclipse.microprofile.health.Liveness;\n\n@Liveness\npublic class Ping {\n}\n";
        let found = fixed(text);
        assert_eq!(found[0].0, "health-not-implemented");
        assert_eq!(
            found[0].1[0],
            "package a;\nimport org.eclipse.microprofile.health.Liveness;\n\n@Liveness\npublic class Ping implements org.eclipse.microprofile.health.HealthCheck {\n}\n"
        );
        assert!(fixed("import org.eclipse.microprofile.health.Liveness; import org.eclipse.microprofile.health.HealthCheck; @Liveness class Ping implements HealthCheck {}").is_empty());
        assert!(fixed("import org.eclipse.microprofile.health.Liveness; @Liveness class Ping extends Base {}").is_empty());
    }

    #[test]
    fn a_health_check_without_a_qualifier_offers_liveness_and_readiness() {
        let text = "import x.Y;\nimport org.eclipse.microprofile.health.HealthCheck;\n\n@ApplicationScoped\npublic class Ping implements HealthCheck {\n}\n";
        let found = fixed(text);
        assert_eq!(found[0].0, "health-without-qualifier");
        assert_eq!(found[0].1.len(), 2);
        assert!(found[0].1[0].contains("@Liveness\n@ApplicationScoped") || found[0].1[0].contains("@ApplicationScoped"));
        assert!(found[0].1[0].contains("@org.eclipse.microprofile.health.Liveness\n"));
        assert!(found[0].1[1].contains("@org.eclipse.microprofile.health.Readiness\n"));
        assert!(fixed("import org.eclipse.microprofile.health.Readiness; import org.eclipse.microprofile.health.HealthCheck; @Readiness class Ping implements HealthCheck {}").is_empty());
        assert!(fixed("import org.eclipse.microprofile.health.HealthCheck; abstract class Base implements HealthCheck {}").is_empty());
    }

    #[test]
    fn gauges_need_an_application_scoped_bean_and_a_number() {
        let text = "import org.eclipse.microprofile.metrics.annotation.Gauge;\nimport jakarta.enterprise.context.RequestScoped;\n@RequestScoped\nclass M {\n  @Gauge(name = \"a\") public int ok() { return 1; }\n  @Gauge(name = \"b\") public String bad() { return \"\"; }\n}\n";
        let found = fixed(text);
        assert_eq!(found.iter().map(|f| f.0).collect::<Vec<_>>(), ["gauge-scope", "gauge-scope", "gauge-return-type"]);
        assert!(found[0].1[0].contains("@jakarta.enterprise.context.ApplicationScoped\nclass M"));
        assert!(fixed("import org.eclipse.microprofile.metrics.annotation.Gauge; import jakarta.enterprise.context.ApplicationScoped; @ApplicationScoped class M { @Gauge(name = \"a\") int ok() { return 1; } }").is_empty());
        assert!(fixed("import org.eclipse.microprofile.metrics.annotation.Gauge; class M { @Gauge(name = \"a\") int ok() { return 1; } }").is_empty());
    }
    #[test]
    fn scheduler_cron_takes_precedence_over_every_as_quarkus_specifies() {
        let both = "import io.quarkus.scheduler.Scheduled; class Job { @Scheduled(cron=\"0 0/5 * * * ?\", every=\"not a duration\") void run() {} }";
        assert!(run(both, &["other"], &[]).is_empty());
        let fallback = "import io.quarkus.scheduler.Scheduled; class Job { @Scheduled(cron=\"\", every=\"10s\") void run() {} }";
        assert!(run(fallback, &["other"], &[]).is_empty());
    }

}
