//! What configuration keys exist: Spring Boot's own metadata (bundled), the
//! project's metadata files and its `@ConfigurationProperties` classes.

use serde_json::Value;
use spring_text::java::{self, JavaFile, TypeRef};
use spring_text::metadata::{Catalog, Deprecation, Meta, Prop, parse_tsv};
use std::sync::OnceLock;

const MAX_DESCRIPTION: usize = 110;

/// Spring Boot's own properties.
pub fn bundled() -> &'static Catalog {
    static BUNDLED: OnceLock<Catalog> = OnceLock::new();
    BUNDLED.get_or_init(|| {
        let mut c = Catalog::default();
        c.add(&parse_tsv(include_str!("../data/boot-properties.tsv")));
        c
    })
}

/// Whether a key of Spring Boot's own metadata has a type of this simple name.
pub(crate) fn library_type(name: &str) -> bool {
    static TYPES: OnceLock<std::collections::BTreeSet<String>> = OnceLock::new();
    TYPES
        .get_or_init(|| spring_text::metadata::Lookup(vec![bundled()])
                .all()
                .map(|prop| prop.ty.clone())
                .collect())
        .contains(name)
}

/// Dataset origin from the packaged snapshot, never the project's effective version.
pub(crate) fn bundled_origin() -> &'static str {
    include_str!("../data/boot-properties.tsv")
        .lines()
        .next()
        .unwrap_or("Bundled Spring Boot metadata snapshot")
        .trim_start_matches("# ")
}

/// `java.util.Map<java.lang.String,Foo$Bar>` → `Map<String,Foo.Bar>`.
fn simple_type(t: &str) -> String {
    let mut out = String::new();
    let mut seg = String::new();
    for c in t.chars().chain([' ']) {
        if c.is_alphanumeric() || c == '_' {
            seg.push(c);
            continue;
        }
        if c == '.' && seg.starts_with(char::is_lowercase) {
            seg.clear();
            continue;
        }
        out.push_str(&seg);
        seg.clear();
        if !c.is_whitespace() {
            out.push(if c == '$' { '.' } else { c });
        }
    }
    out
}

fn first_sentence(text: &str) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let end = text.find(". ").unwrap_or(text.len());
    let s = text[..end].trim_end_matches('.');
    if s.chars().count() <= MAX_DESCRIPTION {
        return s.to_owned();
    }
    s.chars().take(MAX_DESCRIPTION - 1).collect::<String>() + "…"
}

fn json_text(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Array(items) => items.iter().map(json_text).collect::<Vec<_>>().join(","),
        other => other.to_string(),
    }
}

/// A `META-INF/(additional-)spring-configuration-metadata.json` of the project.
pub fn parse_json(text: &str) -> Option<Meta> {
    let v: Value = serde_json::from_str(text).ok()?;
    let props = v["properties"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| {
            let dep = &p["deprecation"];
            let deprecated =
                (dep.is_object() || p["deprecated"] == Value::Bool(true)).then(|| Deprecation {
                    removed: dep["level"] == "error",
                    replacement: dep["replacement"].as_str().unwrap_or_default().to_owned(),
                });
            Some(Prop {
                name: p["name"].as_str()?.to_owned(),
                ty: simple_type(p["type"].as_str().unwrap_or_default()),
                default: json_text(&p["defaultValue"]),
                deprecated,
                description: first_sentence(p["description"].as_str().unwrap_or_default()),
            })
        })
        .collect();
    let hints = v["hints"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|h| {
            let values: Vec<String> = h["values"]
                .as_array()?
                .iter()
                .map(|v| json_text(&v["value"]))
                .filter(|s| !s.is_empty())
                .collect();
            Some((h["name"].as_str()?.to_owned(), values)).filter(|(_, v)| !v.is_empty())
        })
        .collect();
    Some(Meta { props, hints })
}

/// Spring's dashed form of a Java property name: `maxPoolSize` → `max-pool-size`.
pub fn dashed(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        let c = if c == '_' { '-' } else { c };
        if c.is_uppercase() && !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
        out.extend(c.to_lowercase());
    }
    out
}

/// The prefix of a `@ConfigurationProperties`.
fn prefix(a: Option<&java::Annotation>) -> Option<String> {
    let annotation = a?;
    let mut aliases = annotation
        .args
        .iter()
        .filter(|arg| matches!(arg.name.as_str(), "prefix" | "value"));
    let argument = aliases.next()?;
    if aliases.next().is_some() {
        return None;
    }
    let [literal] = argument.strings.as_slice() else {
        return None;
    };
    // No partial constant-expression evaluation or incomplete Java escaping.
    if !argument.text.starts_with('"')
        || !argument.text.ends_with('"')
        || argument.text.starts_with("\"\"\"")
        || argument.text.contains('\\')
        || argument.at + 1 != literal.at
        || argument.end != literal.end + 1
    {
        return None;
    }
    let p = literal.value.clone();
    (!p.is_empty() && !p.contains("${")).then_some(p)
}

// Public source facts and edit admission require an explicit source annotation owner.
// General Java resolution (including imported/local generic shadows) is not inferred.
fn unshadowed_annotation_name(file: &JavaFile, text: &str, name: &str) -> bool {
    let tokens = java::tokens(text);
    tokens
        .iter()
        .enumerate()
        .filter(|(_, token)| token.s == name)
        .all(|(i, token)| {
            file.imports
                .iter()
                .any(|import| import.at <= token.at && token.end() <= import.end)
                || file.types.iter().any(|ty| {
                    ty.annotations
                        .iter()
                        .chain(
                            ty.members
                                .iter()
                                .flat_map(|member| member.annotations.iter()),
                        )
                        .any(|annotation| annotation.name_at == token.at)
                })
                || tokens.get(i + 1).is_some_and(|next| next.s == ".")
                || i > 0 && tokens[i - 1].s == "package"
        })
}
pub(super) fn supported_annotation<'a>(
    file: &JavaFile,
    text: &str,
    annotations: &'a [java::Annotation],
    simple: &str,
    qualified: &str,
) -> Option<&'a java::Annotation> {
    let mut named = annotations.iter().filter(|a| a.name == simple);
    let annotation = named.next()?;
    if named.next().is_some()
        || text.contains("\\u")
        || file
            .imports
            .iter()
            .any(|import| import.path.ends_with(".*"))
    {
        return None;
    }
    // Inherited member types can shadow even a qualified name's first segment.
    // This bounded model does not resolve those scopes.
    if file.types.iter().any(|ty| {
        ty.start <= annotation.at
            && annotation.end <= ty.end
            && (!ty.extends.is_empty() || !ty.implements.is_empty())
    }) {
        return None;
    }
    let spelling: String = text
        .get(annotation.at + 1..annotation.name_at + simple.len())?
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    if spelling != simple {
        let first = qualified.split('.').next()?;
        return (spelling == qualified
            && !file.types.iter().any(|ty| ty.name == first)
            && !file
                .imports
                .iter()
                .any(|import| import.path.rsplit('.').next() == Some(first))
            && unshadowed_annotation_name(file, text, first))
        .then_some(annotation);
    }
    if file.types.iter().any(|ty| ty.name == simple)
        || !unshadowed_annotation_name(file, text, simple)
    {
        return None;
    }
    let imports: Vec<_> = file
        .imports
        .iter()
        .filter(|import| import.path.rsplit('.').next() == Some(simple))
        .collect();
    (imports.len() == 1 && !imports[0].is_static && imports[0].path == qualified)
        .then_some(annotation)
}

/// The `@ConfigurationProperties` classes and `@Bean` methods of a Java source.
pub fn parse_java(text: &str) -> Option<Meta> {
    if text.len() > 128 << 10 || !text.contains("ConfigurationProperties") {
        return None;
    }
    let file = java::parse(text);
    let plain_nested_scope = !java::tokens(text).iter().any(|token| token.s == "<");
    let mut props = Vec::new();
    for (i, t) in file.types.iter().enumerate() {
        if let Some(prefix) = prefix(supported_annotation(
            &file,
            text,
            &t.annotations,
            "ConfigurationProperties",
            "org.springframework.boot.context.properties.ConfigurationProperties",
        )) {
            bind(&file, text, i, &prefix, 0, plain_nested_scope, &mut props)?;
            if !t.extends.is_empty() {
                if props.len() >= 512 {
                    return None;
                }
                props.push(Prop::open_prefix(prefix)); // inherited properties are not seen
            }
        }
        for m in t.methods().filter(|m| {
            supported_annotation(
                &file,
                text,
                &m.annotations,
                "Bean",
                "org.springframework.context.annotation.Bean",
            )
            .is_some()
        }) {
            if let Some(prefix) = prefix(supported_annotation(
                &file,
                text,
                &m.annotations,
                "ConfigurationProperties",
                "org.springframework.boot.context.properties.ConfigurationProperties",
            )) {
                if props.len() >= 512 {
                    return None;
                }
                props.push(Prop::open_prefix(prefix));
            }
        }
    }
    (!props.is_empty()).then_some(Meta {
        props,
        hints: Vec::new(),
    })
}

// Only plain same-file lexical type owners are expanded. Imported/qualified,
// generic and inherited ownership is left opaque rather than matched by name.
fn nested_declared_type(file: &JavaFile, owner: usize, ty: &TypeRef, plain: bool) -> Option<usize> {
    if !plain
        || ty.text != ty.name
        || !ty.args.is_empty()
        || ty.array
        || file
            .imports
            .iter()
            .any(|import| import.path.rsplit('.').next() == Some(ty.name.as_str()))
    {
        return None;
    }
    let mut scope = Some(owner);
    for _ in 0..32 {
        let candidates: Vec<_> = file
            .types
            .iter()
            .enumerate()
            .filter(|(_, candidate)| {
                candidate.name == ty.name
                    && candidate.outer == scope
                    && matches!(candidate.kind.as_str(), "class" | "record")
            })
            .collect();
        if let [candidate] = candidates.as_slice() {
            let (index, declaration) = candidate;
            return (*index != owner
                && declaration.extends.is_empty()
                && declaration.implements.is_empty())
            .then_some(*index);
        }
        if !candidates.is_empty() {
            return None;
        }
        match scope {
            Some(index) => {
                let current = file.types.get(index)?;
                if !current.extends.is_empty() || !current.implements.is_empty() {
                    return None;
                }
                scope = current.outer;
            }
            None => return None,
        }
    }
    None
}

/// The properties of `file.types[i]` under `prefix`: record components,
/// fields and setters; a class of the same file one level down.
fn bind(
    file: &JavaFile,
    text: &str,
    i: usize,
    prefix: &str,
    depth: usize,
    plain_nested_scope: bool,
    out: &mut Vec<Prop>,
) -> Option<()> {
    let t = &file.types[i];
    let mut found: Vec<(&str, &TypeRef, String)> = t
        .components
        .iter()
        .map(|c| (c.name.as_str(), &c.ty, String::new()))
        .collect();
    for m in &t.members {
        if m.modifiers.iter().any(|m| m == "static") {
            continue;
        }
        match m.kind {
            java::MemberKind::Field => {
                let Some(ty) = &m.ty else { continue };
                let init = m.body.map(|(a, b)| text[a..b].trim()).unwrap_or_default();
                let literal =
                    !init.is_empty() && init.len() <= 60 && !init.contains(['(', '{', ' ']);
                found.push((
                    &m.name,
                    ty,
                    if literal {
                        init.trim_matches('"').to_owned()
                    } else {
                        String::new()
                    },
                ));
            }
            java::MemberKind::Method
                if m.params.len() == 1 && m.name.len() > 3 && m.name.starts_with("set") =>
            {
                found.push((&m.name[3..], &m.params[0].ty, String::new()));
            }
            _ => {}
        }
    }
    for (name, ty, default) in found {
        let key = format!("{prefix}.{}", dashed(&spring_text::decapitalize(name)));
        if out.iter().any(|p| p.name == key) {
            continue;
        }
        if out.len() >= 512 {
            return None;
        }
        match nested_declared_type(file, i, ty, plain_nested_scope) {
            Some(j) if depth < 3 => bind(file, text, j, &key, depth + 1, plain_nested_scope, out)?,
            _ => out.push(Prop {
                name: key,
                ty: simple_type(&ty.text),
                default,
                ..Prop::default()
            }),
        }
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use spring_text::metadata::Lookup;

    #[test]
    fn unresolved_nested_type_forms_are_opaque_and_expansion_is_bounded() {
        for field in ["foreign.Pool", "Pool[]", "Pool<String>"] {
            let source = format!(
                "import org.springframework.boot.context.properties.ConfigurationProperties; @ConfigurationProperties(prefix=\"app\") class App {{{field} pool;static class Pool {{String wrong;}}}}"
            );
            let metadata = parse_java(&source).unwrap();
            assert!(
                !metadata
                    .props
                    .iter()
                    .any(|property| property.name == "app.pool.wrong")
            );
            assert!(
                metadata
                    .props
                    .iter()
                    .any(|property| property.name == "app.pool")
            );
        }
        for source in [
            "import org.springframework.boot.context.properties.ConfigurationProperties; import foreign.Pool; @ConfigurationProperties(prefix=\"app\") class App {Pool pool;} class Pool {String wrong;}",
            "import org.springframework.boot.context.properties.ConfigurationProperties; @ConfigurationProperties(prefix=\"app\") class App<Pool> {Pool pool;} class Pool {String wrong;}",
            "import org.springframework.boot.context.properties.ConfigurationProperties; @ConfigurationProperties(prefix=\"app\") class App {Pool pool; static class Pool extends Unknown {String wrong;}}",
        ] {
            assert!(
                !parse_java(source)
                    .unwrap()
                    .props
                    .iter()
                    .any(|property| property.name == "app.pool.wrong")
            );
        }
        let source = |count: usize| {
            format!(
                "import org.springframework.boot.context.properties.ConfigurationProperties; @ConfigurationProperties(prefix=\"app\") class App {{{}}}",
                (0..count)
                    .map(|i| format!("String value{i};"))
                    .collect::<String>()
            )
        };
        assert_eq!(parse_java(&source(512)).unwrap().props.len(), 512);
        assert!(
            parse_java(&source(513)).is_none(),
            "never publish a partial oversized Java metadata expansion"
        );
    }
    #[test]
    fn nested_binding_expansion_requires_lexical_declaring_type_owner() {
        let source = "import org.springframework.boot.context.properties.ConfigurationProperties; class Other {static class Pool {String wrong;}} @ConfigurationProperties(prefix=\"app\") class App {Pool pool;static class Pool {String right;}}";
        let metadata = parse_java(source).unwrap();
        assert!(
            !metadata
                .props
                .iter()
                .any(|property| property.name == "app.pool.wrong")
        );
        assert!(
            metadata
                .props
                .iter()
                .any(|property| property.name == "app.pool.right")
        );
    }
    #[test]
    fn bean_metadata_requires_both_explicit_annotation_owners() {
        let configuration =
            "import org.springframework.boot.context.properties.ConfigurationProperties;";
        let body =
            "class C {@Bean @ConfigurationProperties(prefix=\"app\") Object value(){return null;}}";
        assert!(
            parse_java(&format!("{configuration} {body}")).is_none(),
            "unimported Bean is not a supported owner"
        );
        assert!(
            parse_java(&format!(
                "import org.springframework.context.annotation.Bean; {configuration} {body}"
            ))
            .is_some()
        );
        assert!(parse_java("class C {@org.springframework.context.annotation.Bean @org.springframework.boot.context.properties.ConfigurationProperties(prefix=\"app\") Object value(){return null;}}").is_some());
        for bean in [
            "import org.springframework.context.annotation.*;",
            "import foreign.*;",
            "import org.springframework.context.annotation.Bean; class Bean {}",
        ] {
            assert!(parse_java(&format!("{bean} {configuration} {body}")).is_none());
        }
        for source in [
            "import org.springframework.boot.context.properties.ConfigurationProperties; @ConfigurationProperties(prefix=\"app\") class A<ConfigurationProperties> {String name;}",
            "import foreign.org; @org.springframework.boot.context.properties.ConfigurationProperties(prefix=\"app\") class A {String name;}",
            "import org.springframework.boot.context.properties.ConfigurationProperties; class Outer extends Unknown {@ConfigurationProperties(prefix=\"app\") class A {String name;}}",
        ] {
            assert!(parse_java(source).is_none(), "{source}");
        }
    }
    #[test]
    fn prefix_expression_fragments_and_conflicting_aliases_are_not_declarations() {
        for argument in [
            "prefix=\"app\" + SUFFIX",
            "prefix=\"app\",value=\"other\"",
            "prefix=\"app\",prefix=\"other\"",
            "prefix=\"app\\u002ename\"",
        ] {
            let text = format!(
                "import org.springframework.boot.context.properties.ConfigurationProperties; @ConfigurationProperties({argument}) class A {{String name;}}"
            );
            assert!(parse_java(&text).is_none(), "{text}");
        }
        assert!(parse_java("import org.springframework.boot.context.properties.ConfigurationProperties; @ConfigurationProperties(prefix=\"app\",ignoreUnknownFields=false) class A {String name;}").is_some());
    }
    #[test]
    fn explicit_supported_annotation_owners_survive_and_ambiguous_contexts_stay_quiet() {
        for annotation in [
            "import org.springframework.boot.context.properties.ConfigurationProperties; @ConfigurationProperties",
            "@org.springframework.boot.context.properties.ConfigurationProperties",
        ] {
            assert!(
                parse_java(&format!(
                    "{annotation}(prefix=\"app\") class A {{String name;}}"
                ))
                .is_some()
            );
        }
        for prefix in [
            "import foreign.*;",
            "import static foreign.Owner.*;",
            "import org.springframework.boot.context.properties.ConfigurationProperties; import foreign.ConfigurationProperties;",
            "import org.springframework.boot.context.properties.ConfigurationProperties; class ConfigurationProperties {}",
        ] {
            assert!(
                parse_java(&format!(
                    "{prefix} @ConfigurationProperties(prefix=\"app\") class A {{String name;}}"
                ))
                .is_none()
            );
        }
        assert!(parse_java("class org {} @org.springframework.boot.context.properties.ConfigurationProperties(prefix=\"app\") class A {String name;}").is_none());
        assert!(parse_java("import org.springframework.boot.context.properties.ConfigurationProperties; class C {@foreign.Bean @ConfigurationProperties(prefix=\"app\") Object value(){return null;}}").is_none());
    }
    #[test]
    fn explicit_foreign_configuration_properties_cannot_supply_boot_metadata() {
        for text in [
            "import foreign.ConfigurationProperties; @ConfigurationProperties(prefix=\"app\") class A {String name;}",
            "@foreign.ConfigurationProperties(prefix=\"app\") class A {String name;}",
        ] {
            assert!(parse_java(text).is_none(), "{text}");
        }
    }
    #[test]
    fn same_file_annotation_shadows_and_foreign_bean_method_cannot_supply_metadata() {
        for text in [
            "import org.springframework.boot.context.properties.ConfigurationProperties; @interface ConfigurationProperties {String prefix();} @ConfigurationProperties(prefix=\"app\") class A {String name;}",
            "import org.springframework.boot.context.properties.ConfigurationProperties; import foreign.Bean; class C {@Bean @ConfigurationProperties(prefix=\"app\") Object value(){return null;}}",
        ] {
            assert!(parse_java(text).is_none(), "{text}");
        }
    }
    #[test]
    fn reads_the_bundled_metadata() {
        let c = Lookup(vec![bundled()]);
        let port = c.prop("server.port").unwrap();
        assert_eq!(
            (port.ty.as_str(), port.default.as_str()),
            ("Integer", "8080")
        );
        assert!(c.prop("spring.jpa.showSql").is_some(), "relaxed binding");
        let redis = c
            .prop("spring.redis.host")
            .unwrap()
            .deprecated
            .as_ref()
            .unwrap();
        assert_eq!(redis.replacement, "spring.data.redis.host");
        assert_eq!(
            c.values("logging.level.com.acme"),
            ["trace", "debug", "info", "warn", "error", "fatal", "off"]
        );
        assert_eq!(c.values("spring.jpa.show-sql"), ["true", "false"]);
        assert!(
            c.values("spring.jpa.hibernate.ddl-auto")
                .contains(&"update".to_owned())
        );
        assert_eq!(c.map_keys("logging.level"), ["root", "sql", "web"]);
    }

    #[test]
    fn tells_unknown_keys_only_in_known_groups() {
        let c = Lookup(vec![bundled()]);
        assert_eq!(
            c.unknown("spring.datasource.urll"),
            Some("spring.datasource")
        );
        assert_eq!(
            c.unknown("server.servlet.context-pathh"),
            Some("server.servlet")
        );
        for known in [
            "spring.datasource.url",
            "spring.datasource.driverClassName",
            "spring.datasource",
            "logging.level.com.acme",
            "spring.jpa.properties.hibernate.format_sql",
            "spring.security.oauth2.client.registration.google.client-id",
            "management.endpoints.web.exposure.include[0]",
            "spring.datasource.druid.initial-size",
            "management.health.consul.enabled",
            "spring.cloud.gateway.routes[0].id",
            "app.greeting",
            "server.prot",
            "spring.datasource.${x}",
        ] {
            assert_eq!(c.unknown(known), None, "{known}");
        }
    }

    #[test]
    fn reads_project_metadata_files() {
        let json = r#"{"groups": [{"name": "acme"}], "properties": [
            {"name": "acme.mode", "type": "java.lang.String", "description": "How it runs. More text.", "defaultValue": "fast"},
            {"name": "acme.old", "type": "java.lang.Boolean", "deprecation": {"replacement": "acme.mode", "level": "error"}},
            {"name": "acme.tags", "type": "java.util.Map<java.lang.String,java.util.List<com.acme.Tag$Kind>>"}
        ], "hints": [{"name": "acme.mode", "values": [{"value": "fast"}, {"value": "safe"}]}]}"#;
        let meta = parse_json(json).unwrap();
        assert_eq!(meta.props[0].description, "How it runs");
        assert_eq!(meta.props[0].default, "fast");
        assert_eq!(
            meta.props[1].deprecated,
            Some(Deprecation {
                removed: true,
                replacement: "acme.mode".into()
            })
        );
        assert_eq!(meta.props[2].ty, "Map<String,List<Tag.Kind>>");
        assert_eq!(
            meta.hints,
            [(
                "acme.mode".to_owned(),
                vec!["fast".to_owned(), "safe".to_owned()]
            )]
        );
        assert_eq!(parse_json("{ nope"), None);
    }

    #[test]
    fn binds_configuration_properties_classes() {
        let src = r#"package com.acme;
import org.springframework.boot.context.properties.ConfigurationProperties;
import org.springframework.context.annotation.Bean;
@ConfigurationProperties(prefix = "acme.mail")
public class MailProperties {
    private String host = "localhost";
    private int port = 25;
    private boolean useSsl;
    private static final int MAX = 3;
    private final Pool pool = new Pool();
    public void setReplyTo(String replyTo) {}
    public static class Pool { private int maxSize = 8; }
}

@ConfigurationProperties("acme.retry")
record RetryProperties(int maxAttempts, Duration backoff) {}

@Configuration
class Config {
    @Bean
    @ConfigurationProperties("acme.pool")
    HikariDataSource pool() { return new HikariDataSource(); }
}
"#;
        let meta = parse_java(src).unwrap();
        let names: Vec<_> = meta
            .props
            .iter()
            .map(|p| (p.name.as_str(), p.ty.as_str(), p.default.as_str()))
            .collect();
        assert_eq!(
            names,
            [
                ("acme.mail.host", "String", "localhost"),
                ("acme.mail.port", "int", "25"),
                ("acme.mail.use-ssl", "boolean", ""),
                ("acme.mail.pool.max-size", "int", "8"),
                ("acme.mail.reply-to", "String", ""),
                ("acme.retry.max-attempts", "int", ""),
                ("acme.retry.backoff", "Duration", ""),
                ("acme.pool", "*", ""),
            ]
        );
        assert_eq!(parse_java("class A {}"), None);
        assert_eq!(dashed("URLPath"), "u-r-l-path");
    }
}
