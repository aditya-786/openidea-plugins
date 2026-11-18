//! Source-local contracts checked against Spring Shell 3.4.0. Unknown expression
//! values and annotation/type ownership do not become guessed diagnostics.
use super::*;
use std::collections::HashSet;
const LEGACY: &str = "org.springframework.shell.standard.ShellMethod";
const AVAILABILITY: &str = "org.springframework.shell.standard.ShellMethodAvailability";
const MODERN: &str = "org.springframework.shell.command.annotation.Command";
const CORE_COMMAND: &str = "org.springframework.shell.core.command.annotation.Command";
const OPTION: &str = "org.springframework.shell.command.annotation.Option";

fn import_name<'a>(text: &'a str, import: &'a java::Import) -> (&'a str, bool) {
    let tail = text
        .get(import.end..)
        .unwrap_or("")
        .split('\n')
        .next()
        .unwrap_or("");
    let tokens = java::tokens(tail);
    if let [alias, name, ..] = tokens.as_slice() {
        if alias.s == "as" && name.is_ident() {
            return (name.s, true);
        }
    }
    (import.path.rsplit('.').next().unwrap_or(""), false)
}
pub(super) fn import_aliased(text: &str, import: &java::Import) -> bool {
    import_name(text, import).1
}
fn generic_shadow(code: &str, name: &str) -> bool {
    let mut depth = 0usize;
    for token in java::tokens(code) {
        match token.s {
            "<" => depth += 1,
            ">" => depth = depth.saturating_sub(1),
            _ if depth > 0 && token.s == name => return true,
            _ => {}
        }
    }
    false
}
fn generic_or_alias_shadowed(f:&JavaFile,text:&str,name:&str)->bool {
    f.types.iter().any(|t| generic_shadow(text.get(t.at+t.name.len()..t.end)
        .and_then(|s|s.split('{').next()).unwrap_or(""),name)
        || t.methods().any(|m|generic_shadow(text.get(m.start..m.at).unwrap_or(""),name)))
        || (text.contains("typealias") && java::tokens(text).windows(2).any(|p|p[0].s=="typealias"&&p[1].s==name))
}
fn shadowed(f:&JavaFile,text:&str,name:&str)->bool {
    f.types.iter().any(|t|t.name==name) || generic_or_alias_shadowed(f,text,name)
}
/// Only an unambiguous written reference to this top-level source declaration.
pub(super) fn local_type_owned(f:&JavaFile,text:&str,ty:&java::TypeRef,owner:usize)->bool {
    let declaration=&f.types[owner];
    if ty.array || declaration.outer.is_some() || f.types.iter().filter(|t|t.name==declaration.name).count()!=1{return false;}
    if ty.text==declaration.name {
        return !generic_or_alias_shadowed(f,text,&declaration.name)
            && !f.imports.iter().any(|i|import_name(text,i).0==declaration.name);
    }
    let qualified=f.qualified(owner);
    let root=qualified.split('.').next().unwrap_or("");
    ty.text==qualified && !shadowed(f,text,root) && !f.imports.iter().any(|i|import_name(text,i).0==root)
}
pub(super) fn written_owner(f:&JavaFile,text:&str,written:&str,qualified:&str)->bool {
    let short=qualified.rsplit('.').next().unwrap_or("");
    if written!=short && written!=qualified{return false;}
    if written.contains('.') {
        let root = qualified.split('.').next().unwrap_or("");
        return written == qualified
            && !shadowed(f, text, root)
            && !f.imports.iter().any(|i| import_name(text, i).0 == root);
    }
    if shadowed(f, text, short) {
        return false;
    }
    let imports: Vec<_> = f
        .imports
        .iter()
        .filter(|i| import_name(text, i).0 == short)
        .collect();
    matches!(imports.as_slice(),[i] if i.path==qualified && !i.is_static && !import_name(text,i).1)
}
pub(super) fn owned(f: &JavaFile, text: &str, annotation: &Annotation, qualified: &str) -> bool {
    let written=text.get(annotation.at+1..annotation.name_at+annotation.name.len()).unwrap_or("").trim();
    written_owner(f,text,written,qualified)
}
pub(super) fn literals<'a>(a: &'a Annotation, name: &str) -> Option<Vec<&'a Str>> {
    let arg = a.arg(name)?;
    (super::hover::literal_values(arg)
        && !arg.text.contains('\\')
        && arg.strings.iter().all(|s| !s.value.contains('$')))
    .then(|| arg.strings.iter().collect())
}
pub(super) fn modern_field(f: &JavaFile, text: &str, a: &Annotation) -> Option<&'static str> {
    if a.name != "Command" { return None; }
    if owned(f, text, a, MODERN) { Some("command") }
    else if owned(f, text, a, CORE_COMMAND) { Some("name") }
    else { None }
}
pub(super) fn supported(f: &JavaFile, text: &str, a: &Annotation) -> bool {
    if a.name == "Command" { return modern_field(f, text, a).is_some(); }
    let qualified = match a.name.as_str() {
        "ShellMethod" => LEGACY,
        "ShellMethodAvailability" => AVAILABILITY,
        "ShellComponent" => "org.springframework.shell.standard.ShellComponent",
        "ShellCommandGroup" => "org.springframework.shell.standard.ShellCommandGroup",
        "Command" => MODERN,
        "CommandAvailability" => "org.springframework.shell.command.annotation.CommandAvailability",
        "CommandGroup" => "org.springframework.shell.core.command.annotation.CommandGroup",
        "Option" => OPTION,
        _ => return false,
    };
    owned(f, text, a, qualified)
}
pub(super) fn literal_range(text: &str, s: &Str) -> Range {
    if s.at == s.end
        && s.at > 0
        && text.as_bytes().get(s.at - 1) == Some(&b'"')
        && text.as_bytes().get(s.end) == Some(&b'"')
    {
        range(text, s.at - 1, s.end + 1)
    } else {
        range(text, s.at, s.end)
    }
}
pub(super) fn type_owned(f:&JavaFile,text:&str,ty:&java::TypeRef,qualified:&str)->bool {
    !ty.array && written_owner(f,text,&ty.text,qualified)
}
pub(super) fn availability_type(f: &JavaFile, text:&str, ty: &java::TypeRef) -> bool {
    type_owned(f,text,ty,"org.springframework.shell.Availability")
}
fn known_wrong_return(f: &JavaFile, text:&str, kotlin: bool, m: &Member) -> bool {
    let Some(ty) = &m.ty else { return false }; // Kotlin inferred return type is unknown.
    if ty.text==ty.name && generic_or_alias_shadowed(f,text,&ty.name) {return false;}
    if ty.name == "Availability" {
        return ty.array
            || (ty.text.contains('.')
                && ty.text.trim() != "org.springframework.shell.Availability")
            || f.imports.iter().any(|i| {
                !i.is_static
                    && i.path.ends_with(".Availability")
                    && i.path != "org.springframework.shell.Availability"
            });
    }
    if kotlin {
        return false;
    } // Kotlin aliases are not resolved by this scanner.
    matches!(
        ty.name.as_str(),
        "void"
            | "boolean"
            | "Boolean"
            | "String"
            | "int"
            | "Int"
            | "long"
            | "Long"
            | "double"
            | "Double"
            | "float"
            | "Float"
            | "byte"
            | "Byte"
            | "short"
            | "Short"
            | "char"
            | "Char"
            | "Unit"
    )
}

pub(super) fn check(doc: &Document, f: &JavaFile, extended: &dyn Fn(&str) -> bool) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    let text = &doc.text;
    for (ti, t) in f.types.iter().enumerate() {
        let legacy: Vec<_> = t
            .methods()
            .filter(|m| {
                m.annotation("ShellMethod")
                    .is_some_and(|a| owned(f, text, a, LEGACY))
            })
            .collect();
        let mut indicators = HashSet::new();
        for command in &legacy {
            let a = command.annotation("ShellMethod").unwrap();
            if t.annotations.is_empty() && t.supers().next().is_none() && t.kind == "class" && !t.modifiers.iter().any(|m| m == "abstract") && !extended(&f.qualified(ti)) {
                out.push(problem(range(text,command.at,command.at+command.name.len()),Severity::Warning,"missing-shell-component","This class has no @ShellComponent, so Spring Shell may not register its @ShellMethod commands. Ignore this if the class is registered another way.".into()));
            }
            let missing = a.arg("value").is_none()
                || literals(a, "value").is_some_and(|s| s.len() == 1 && s[0].value.is_empty());
            if missing {
                out.push(problem(
                    range(text, command.at, command.at + command.name.len()),
                    Severity::Warning,
                    "missing-command-description",
                    "Legacy ShellMethod has no description.".into(),
                ));
            }
            if let Some(a) = command
                .annotation("ShellMethodAvailability")
                .filter(|a| owned(f, text, a, AVAILABILITY))
            {
                if let Some(values) = literals(a, "value") {
                    if values.len() != 1 {
                        out.push(problem(
                            range(text, a.name_at, a.name_at + a.name.len()),
                            Severity::Error,
                            "availability-method-count",
                            "On a command, ShellMethodAvailability must name exactly one indicator method.".into(),
                        ));
                    } else {
                        if t.supers().next().is_none()
                            && ![
                                "getClass",
                                "hashCode",
                                "toString",
                                "clone",
                                "finalize",
                                "wait",
                                "notify",
                                "notifyAll",
                            ]
                            .contains(&values[0].value.as_str())
                            && !t
                                .methods()
                                .any(|m| m.name == values[0].value && m.params.is_empty())
                        {
                            out.push(problem(
                                literal_range(text, values[0]),
                                Severity::Warning,
                                "availability-method-reference",
                                "This class has no method with this name and no parameters."
                                    .into(),
                            ));
                        }
                        for m in t
                            .methods()
                            .filter(|m| m.name == values[0].value && m.params.is_empty())
                        {
                            indicators.insert(m.at);
                        }
                    }
                }
            } else {
                for m in t.methods().filter(|m| {
                    m.name == format!("{}Availability", command.name) && m.params.is_empty()
                }) {
                    indicators.insert(m.at);
                }
            }
        }
        if t.supers().next().is_none() {
            for command in &legacy {
                if command.annotation("ShellMethodAvailability").is_some()
                    || t.methods().any(|m| {
                        m.name == format!("{}Availability", command.name) && m.params.is_empty()
                    })
                {
                    continue;
                }
                let a = command.annotation("ShellMethod").unwrap();
                let keys = if a.arg("key").is_none() {
                    command.name.is_ascii().then(|| vec![kebab(&command.name)])
                } else {
                    literals(a, "key")
                        .map(|values| values.into_iter().map(|s| s.value.clone()).collect())
                };
                let Some(keys) = keys else {
                    continue;
                };
                let candidates = t
                    .methods()
                    .filter(|m| !legacy.iter().any(|c| c.at == m.at))
                    .filter(|m| {
                        m.annotation("ShellMethodAvailability")
                            .filter(|a| owned(f, text, a, AVAILABILITY))
                            .and_then(|a| literals(a, "value"))
                            .is_some_and(|values| {
                                !values.iter().any(|s| s.value == "*")
                                    && values.iter().any(|s| keys.contains(&s.value))
                            })
                    })
                    .count();
                if candidates > 1 {
                    out.push(problem(range(text,command.at,command.at+command.name.len()),Severity::Error,"availability-ambiguous","Several @ShellMethodAvailability methods name this command. Keep only one.".into()));
                }
            }
        }
        let mut keys = HashSet::new();
        let closed = t.supers().next().is_none()
            && t.methods()
                .filter_map(|m| m.annotation("ShellMethod"))
                .all(|a| owned(f, text, a, LEGACY))
            && legacy.iter().all(|m| {
                let a = m.annotation("ShellMethod").unwrap();
                if a.arg("key").is_none() {
                    if !m.name.is_ascii() {
                        return false;
                    }
                    keys.insert(kebab(&m.name));
                    true
                } else if let Some(values) = literals(a, "key") {
                    if values.is_empty() {
                        if !m.name.is_ascii() {
                            return false;
                        }
                        keys.insert(kebab(&m.name));
                    }
                    for s in values {
                        keys.insert(s.value.clone());
                    }
                    true
                } else {
                    false
                }
            });
        for m in t.methods() {
            let command = legacy.iter().any(|c| c.at == m.at);
            if !command
                && m.annotation("ShellMethodAvailability")
                    .is_some_and(|a| owned(f, text, a, AVAILABILITY))
            {
                indicators.insert(m.at);
                if let Some(values) = m
                    .annotation("ShellMethodAvailability")
                    .and_then(|a| literals(a, "value"))
                {
                    if closed {
                        for s in &values {
                            if s.value != "*" && !keys.contains(&s.value) {
                                out.push(problem(
                                    literal_range(text, s),
                                    Severity::Error,
                                    "unknown-command",
                                    format!("No @ShellMethod command in this class is named `{}`.", s.value),
                                ));
                            }
                        }
                    }
                    if values.len() > 1 && values.iter().any(|s| s.value == "*") {
                        let a = m.annotation("ShellMethodAvailability").unwrap();
                        out.push(problem(
                            range(text, a.name_at, a.name_at + a.name.len()),
                            Severity::Error,
                            "availability-wildcard",
                            "The '*' availability wildcard must be the only value.".into(),
                        ));
                    }
                }
            }
            if m.annotation("Command")
                .is_some_and(|a| owned(f, text, a, MODERN))
            {
                for p in &m.params {
                    let Some(a) = p.annotation("Option").filter(|a| owned(f, text, a, OPTION))
                    else {
                        continue;
                    };
                    let integer =
                        |name| a.arg(name).and_then(|a| a.text.trim().parse::<i32>().ok());
                    if let (Some(min), Some(max)) = (integer("arityMin"), integer("arityMax")) {
                        if min >= 0 && max >= 0 && max < min {
                            out.push(problem(
                                range(text, a.name_at, a.name_at + a.name.len()),
                                Severity::Warning,
                                "option-arity-range",
                                "arityMax is lower than arityMin. Spring Shell raises arityMax to arityMin."
                                    .into(),
                            ));
                        }
                    }
                }
            }
        }
        for m in t.methods().filter(|m| indicators.contains(&m.at)) {
            let at = range(text, m.at, m.at + m.name.len());
            if known_wrong_return(f, text, doc.path.ends_with(".kt"), m) {
                out.push(problem(
                    at,
                    Severity::Error,
                    "availability-return-type",
                    format!(
                        "Availability method `{}` must return `Availability`.",
                        m.name
                    ),
                ));
            }
            if !m.params.is_empty() {
                out.push(problem(
                    at,
                    Severity::Error,
                    "availability-parameters",
                    format!("Availability method `{}` must take no arguments.", m.name),
                ));
            }
        }
    }
    out.extend(options::check(doc, f));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    fn doc(text: &str, kotlin: bool) -> Document {
        Document {
            path: if kotlin {
                "/project/C.kt"
            } else {
                "/project/C.java"
            }
            .into(),
            language: if kotlin { "kotlin" } else { "java" }.into(),
            version: 1,
            text: text.into(),
        }
    }
    fn codes(text: &str) -> Vec<String> {
        let doc = doc(text, false);
        let mut tools = ShellTools::default();
        tools.line_markers(&doc);
        tools
            .diagnostics(&doc)
            .into_iter()
            .map(|d| d.code.unwrap())
            .collect()
    }
    const IMPORTS: &str = "import org.springframework.shell.standard.ShellMethod; import org.springframework.shell.standard.ShellMethodAvailability; import org.springframework.shell.Availability; import org.springframework.shell.standard.ShellComponent;\n";
    #[test]
    fn annotation_on_command_names_indicator_and_does_not_constrain_command_return() {
        let _host = MockHost::install().with_capabilities(&[]);
        let text = format!(
            "{IMPORTS}@ShellComponent class C {{ @ShellMethod(\"Run\") @ShellMethodAvailability(\"gate\") String run(){{return \"ok\";}} Availability gate(){{return null;}} }}"
        );
        assert!(codes(&text).is_empty());
        let bad = text.replace("Availability gate()", "boolean gate()");
        assert_eq!(codes(&bad), ["availability-return-type"]);
    }
    #[test]
    fn legacy_indicator_arguments_wildcard_and_command_selector_count_are_checked() {
        let _host = MockHost::install().with_capabilities(&[]);
        let text = format!(
            "{IMPORTS}@ShellComponent class C {{ @ShellMethod(\"Run\") void run(){{}} @ShellMethodAvailability({{\"*\",\"run\"}}) Availability gate(String input){{return null;}} }}"
        );
        let found = codes(&text);
        assert!(found.contains(&"availability-wildcard".into()));
        assert!(found.contains(&"availability-parameters".into()));
        let command = format!(
            "{IMPORTS}@ShellComponent class C {{ @ShellMethod(\"Run\") @ShellMethodAvailability({{\"a\",\"b\"}}) String run(){{return null;}} }}"
        );
        assert_eq!(codes(&command), ["availability-method-count"]);
        let dynamic = command.replace("{\"a\",\"b\"}", "NAMES");
        assert!(codes(&dynamic).is_empty());
    }
    #[test]
    fn missing_legacy_description_is_literal_only_and_foreign_annotations_stay_quiet() {
        let _host = MockHost::install().with_capabilities(&[]);
        let text = format!(
            "{IMPORTS}@ShellComponent class C {{ @ShellMethod void empty(){{}} @ShellMethod(value=\"\") void blank(){{}} @ShellMethod(value=DESC) void dynamic(){{}} @ShellMethod(value=\" \" ) void whitespace(){{}} }}"
        );
        assert_eq!(
            codes(&text),
            ["missing-command-description", "missing-command-description"]
        );
        let foreign = "import custom.ShellMethod; class C { @ShellMethod void empty(){} }";
        assert!(codes(foreign).is_empty());
        let qualified = "@org.springframework.shell.standard.ShellComponent class C { @org.springframework.shell.standard.ShellMethod void empty(){} }";
        assert_eq!(codes(qualified), ["missing-command-description"]);
    }
    #[test]
    fn modern_arity_check_requires_owned_annotations_and_two_known_nonnegative_literals() {
        let _host = MockHost::install().with_capabilities(&[]);
        let text = "import org.springframework.shell.command.annotation.Command; import org.springframework.shell.command.annotation.Option; class C { @Command void run(@Option(arityMin=2,arityMax=1) String value){} }";
        assert_eq!(codes(text), ["option-arity-range"]);
        for quiet in [
            text.replace("arityMax=1", "arityMax=2"),
            text.replace("arityMax=1", "arityMax=-1"),
            text.replace("arityMin=2", "arityMin=MIN"),
            text.replace(
                "org.springframework.shell.command.annotation.Option",
                "foreign.Option",
            ),
            text.replace("@Command ", ""),
            text.replace("arityMin=2", "arityMin=1+1"),
        ] {
            assert!(codes(&quiet).is_empty(), "{quiet}");
        }
    }
    #[test]
    fn conventional_overloads_and_unknown_return_types_do_not_produce_guesses() {
        let _host = MockHost::install().with_capabilities(&[]);
        let text = format!(
            "{IMPORTS}@ShellComponent class C {{ @ShellMethod(\"Run\") void run(){{}} Availability runAvailability(){{return null;}} String runAvailability(String arg){{return arg;}} @ShellMethodAvailability(\"run\") Unresolved gate(){{return null;}} }}"
        );
        assert!(codes(&text).is_empty());
        let kotlin = "import org.springframework.shell.standard.ShellMethod\nimport org.springframework.shell.standard.ShellMethodAvailability\n@org.springframework.shell.standard.ShellComponent class C { @ShellMethod(\"Run\") fun run() {}\n@ShellMethodAvailability(\"run\") fun gate() = Availability.available()\n}";
        let d = doc(kotlin, true);
        assert!(ShellTools::default().diagnostics(&d).is_empty());
    }
    #[test]
    fn live_owner_reports_once_and_clears_modern_rule_on_unsaved_fix() {
        let host = MockHost::install().with_capabilities(&[]);
        let text = "import org.springframework.shell.command.annotation.Command; import org.springframework.shell.command.annotation.Option; class C { @Command void run(@Option(arityMin=2,arityMax=1) String value){} }";
        host.open_editor("/project/C.java", "java", text, &[]);
        let mut tools = ShellTools::default();
        host.refresh_diagnostics(&mut tools);
        let problems = host.provider_diagnostics();
        assert_eq!(
            problems
                .values()
                .flatten()
                .filter(|d| d.code.as_deref() == Some("option-arity-range"))
                .count(),
            1
        );
        host.open_editor(
            "/project/C.java",
            "java",
            &text.replace("arityMax=1", "arityMax=2"),
            &[],
        );
        host.refresh_diagnostics(&mut tools);
        assert!(host.provider_diagnostics().is_empty());
        assert!(host.diagnostics().is_empty());
    }
    #[test]
    fn command_side_reference_navigation_and_completion_use_local_indicators() {
        let _host = MockHost::install().with_capabilities(&[]);
        let text = format!(
            "{IMPORTS}@ShellComponent class C {{ @ShellMethod(\"Run\") @ShellMethodAvailability(\"gate\") String run(){{return null;}} Availability gate(){{return null;}} }}"
        );
        let d = doc(&text, false);
        let mut tools = ShellTools::default();
        let at = position(&text, text.find("\"gate\"").unwrap() + 2);
        let locations = tools.definition(&d, at);
        assert_eq!(locations.len(), 1);
        assert_eq!(
            locations[0].range.start,
            position(
                &text,
                text.find("gate(){}")
                    .unwrap_or_else(|| text.find("gate(){").unwrap())
            )
        );
        let items = tools.completion(&d, at, None);
        assert_eq!(
            items.iter().map(|i| i.label.as_str()).collect::<Vec<_>>(),
            ["gate"]
        );
    }
    #[test]
    fn modern_provider_arrays_navigate_established_beans_without_absence_claims() {
        let host = MockHost::install();
        let text = "import org.springframework.shell.command.annotation.Command; import org.springframework.shell.command.annotation.CommandAvailability; class C { @Command(command=\"run\") @CommandAvailability(provider={\"missing\",\"known\"}) void run(){} }";
        host.add_file("C.java", text);
        host.add_file("Config.java", "import org.springframework.context.annotation.Bean; import org.springframework.shell.AvailabilityProvider; class Config { @Bean AvailabilityProvider known(){return null;} }");
        let d = doc(text, false);
        let mut tools = ShellTools::default();
        let markers = tools.line_markers(&d);
        assert_eq!(markers.len(), 1);
        assert!(markers[0].tooltip.contains("known"));
        let found = tools.diagnostics(&d);
        let missing: Vec<_> = found
            .iter()
            .filter(|d| d.code.as_deref() == Some("unknown-availability-provider"))
            .collect();
        assert!(
            missing.is_empty(),
            "External/runtime beans cannot be disproved by source cache."
        );
    }
    #[test]
    fn v4_or_picocli_commands_do_not_receive_v3_arity_or_index_rules() {
        let _host = MockHost::install().with_capabilities(&[]);
        for package in [
            "org.springframework.shell.core.command.annotation",
            "picocli.CommandLine",
        ] {
            let text = format!(
                "import {package}.Command; import {package}.Option; import org.springframework.shell.Availability; class C {{ @Command void run(@Option(arityMin=2,arityMax=1) String value){{}} }}"
            );
            let d = doc(&text, false);
            let parsed = parse_source(&d.path, &text);
            assert_eq!(commands(&parsed, &text).len(), usize::from(package.starts_with("org.springframework.shell.core")));
            assert!(ShellTools::default().diagnostics(&d).is_empty());
        }
    }
    #[test]
    fn kotlin_literal_arity_uses_same_live_owner_without_return_type_inference() {
        let _host = MockHost::install().with_capabilities(&[]);
        let text = "import org.springframework.shell.command.annotation.Command\nimport org.springframework.shell.command.annotation.Option\nclass C { @Command fun run(@Option(arityMin=3,arityMax=1) input: String) {} }";
        for source in [
            text.to_owned(),
            text.replace("input: String", "value: String"),
        ] {
            let d = doc(&source, true);
            let found = ShellTools::default().diagnostics(&d);
            assert_eq!(
                found
                    .iter()
                    .filter(|d| d.code.as_deref() == Some("option-arity-range"))
                    .count(),
                1,
                "{source}"
            );
        }
    }
    #[test]
    fn unrelated_class_commands_do_not_satisfy_availability_and_unknown_supers_stay_quiet() {
        let host = MockHost::install();
        let text = format!(
            "{IMPORTS}@ShellComponent class C {{ @ShellMethod(\"Run\") void run(){{}} @ShellMethodAvailability(\"foreign\") Availability gate(){{return null;}} }}"
        );
        host.add_file("C.java", &text);
        host.add_file(
            "Other.java",
            &format!("{IMPORTS}class Other {{ @ShellMethod(\"Foreign\") void foreign(){{}} }}"),
        );
        let mut tools = ShellTools::default();
        let d = doc(&text, false);
        tools.line_markers(&d);
        assert_eq!(
            tools
                .diagnostics(&d)
                .iter()
                .filter(|d| d.code.as_deref() == Some("unknown-command"))
                .count(),
            1
        );
        let d = doc(
            &text.replace("class C {", "class C extends ExternalBase {"),
            false,
        );
        assert!(
            !tools
                .diagnostics(&d)
                .iter()
                .any(|d| d.code.as_deref() == Some("unknown-command"))
        );
    }
    #[test]
    fn component_placement_warns_only_for_unannotated_closed_classes() {
        let _host = MockHost::install();
        let bare = "import org.springframework.shell.standard.ShellMethod; class C { @ShellMethod(\"Run\") void run(){} }";
        assert_eq!(codes(bare), ["missing-shell-component"]);
        for quiet in [
            bare.replace("class C", "@Custom class C"),
            bare.replace("class C", "class C extends Unknown"),
            bare.replace(
                "class C",
                "@org.springframework.shell.standard.ShellComponent class C",
            ),
        ] {
            assert!(codes(&quiet).is_empty(), "{quiet}");
        }
    }
    #[test]
    fn local_indicator_reference_uses_utf16_and_unsaved_fix_clears_without_io() {
        let host = MockHost::install().with_capabilities(&[]);
        let text = format!(
            "{IMPORTS}@ShellComponent class C {{ /* 😀 */ @ShellMethod(\"Run\") @ShellMethodAvailability(\"missing\") void run(){{}} }}"
        );
        host.open_editor("/project/C.java", "java", &text, &[]);
        let mut tools = ShellTools::default();
        host.refresh_diagnostics(&mut tools);
        let diagnostics = host.provider_diagnostics();
        let d = &diagnostics["/project/C.java"][0];
        assert_eq!(d.code.as_deref(), Some("availability-method-reference"));
        assert_eq!(
            d.range.start,
            position(&text, text.find("\"missing\"").unwrap() + 1)
        );
        assert_eq!(
            d.range.end,
            position(&text, text.find("\"missing\"").unwrap() + 8)
        );
        let fixed = text.replacen(
            "void run(){}",
            "Availability missing(){return null;} void run(){}",
            1,
        );
        host.open_editor("/project/C.java", "java", &fixed, &[]);
        host.refresh_diagnostics(&mut tools);
        assert!(host.provider_diagnostics().is_empty());
        assert!(host.diagnostics().is_empty());
        assert_eq!(host.fetches(), 0);
        let inherited = text.replace("class C", "class C extends Unknown");
        assert!(!codes(&inherited).contains(&"availability-method-reference".into()));
    }
    #[test]
    fn conflicting_named_indicators_obey_local_explicit_and_conventional_precedence() {
        let _host = MockHost::install().with_capabilities(&[]);
        let text = format!(
            "{IMPORTS}@ShellComponent class C {{ @ShellMethod(\"Run\") void run(){{}} @ShellMethodAvailability(\"run\") Availability first(){{return null;}} @ShellMethodAvailability(\"run\") Availability second(){{return null;}} }}"
        );
        assert_eq!(codes(&text), ["availability-ambiguous"]);
        for quiet in [
            text.replace(
                "void run(){}",
                "Availability runAvailability(){return null;} void run(){}",
            ),
            text.replace(
                "@ShellMethod(\"Run\")",
                "@ShellMethod(\"Run\") @ShellMethodAvailability(\"first\")",
            ),
            text.replace("class C", "class C extends Unknown"),
        ] {
            assert!(
                !codes(&quiet).contains(&"availability-ambiguous".into()),
                "{quiet}"
            );
        }
    }
    #[test]
    fn wildcard_foreign_and_dynamic_annotation_ownership_stay_quiet() {
        let _host = MockHost::install().with_capabilities(&[]);
        for text in [
            "import org.springframework.shell.standard.*; class C { @ShellMethod void run(){} }",
            "import custom.ShellMethod; import custom.ShellMethodAvailability; class C { @ShellMethod void run(){} @ShellMethodAvailability(\"missing\") String gate(){return null;} }",
        ] {
            let d = doc(text, false);
            let parsed = parse_source(&d.path, text);
            assert!(commands(&parsed, text).is_empty());
            assert!(ShellTools::default().diagnostics(&d).is_empty());
        }
        let text = format!(
            "{IMPORTS}@ShellComponent class C {{ @ShellMethod(key=PREFIX+\"partial\",value=\"Run\") void run(){{}} @ShellMethodAvailability(\"possiblyDynamic\") Availability gate(){{return null;}} }}"
        );
        assert!(codes(&text).is_empty());
    }
    #[test]
    fn closed_local_command_selectors_work_cold_and_empty_reference_highlights_quotes() {
        let _host = MockHost::install().with_capabilities(&[]);
        let text = format!(
            "{IMPORTS}@ShellComponent class C {{ @ShellMethod(\"Run\") void run(){{}} @ShellMethodAvailability(\"\") Availability gate(){{return null;}} }}"
        );
        let d = doc(&text, false);
        let diagnostics = ShellTools::default().diagnostics(&d);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code.as_deref(), Some("unknown-command"));
        let start = offset(&text, diagnostics[0].range.start).unwrap();
        let end = offset(&text, diagnostics[0].range.end).unwrap();
        assert_eq!(&text[start..end], "\"\"");
        let known = text.replace(
            "ShellMethodAvailability(\"\")",
            "ShellMethodAvailability(\"run\")",
        );
        assert!(codes(&known).is_empty());
    }
    #[test]
    fn malformed_wrong_language_and_capped_documents_clear_local_diagnostics() {
        let _host = MockHost::install().with_capabilities(&[]);
        let base = "import org.springframework.shell.standard.ShellMethod; class C { @ShellMethod void run(){} }";
        for text in [
            base.trim_end_matches('}').to_owned(),
            format!("{base}{}", " ".repeat(128 << 10)),
            format!(
                "import org.springframework.shell.standard.ShellMethod; class C {{{}}}",
                (0..129)
                    .map(|i| format!("@ShellMethod void run{i}(){{}} "))
                    .collect::<String>()
            ),
        ] {
            assert!(codes(&text).is_empty());
        }
        let mut d = doc(base, false);
        d.language = "xml".into();
        assert!(ShellTools::default().diagnostics(&d).is_empty());
    }
}
