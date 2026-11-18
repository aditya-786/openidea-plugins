//! Project-wide bean navigation for the host's document index: the model and
//! the string sites of a file, published as keys (`bean#name`, `type#Simple`).

use crate::Navigator;
use crate::lib_hits;
use crate::model::{self, FileModel};
use crate::sites::Site;
use openidea_plugin::indexing::{Alias, Anchor, Document, DocumentIndexer, Edges};
use openidea_plugin::text::offset;
use spring_text::java::{Annotation, JavaFile};
use spring_text::{config, range, xml};

const MAX_ANCHORS: usize = 4096;
const MAX_ALIASES: usize = 256;
const LISTENER_ANNOTATIONS: [&str; 2] = ["EventListener", "TransactionalEventListener"];

fn bean_key(name: &str) -> String {
    format!("bean#{name}")
}

fn type_key(ty: &str) -> String {
    format!("type:{ty}#bean")
}

fn listener_key(event: &str) -> String {
    format!("event:{event}#listener")
}

fn publisher_key(event: &str) -> String {
    format!("publish:{event}#call")
}

fn override_key(name: &str) -> String {
    format!("override#{name}")
}

fn class_key(ty: &str) -> String {
    format!("class:{ty}#decl")
}

fn xml_key(ty: &str) -> String {
    format!("xml:{ty}#bean")
}

fn definitions(model: &FileModel) -> Vec<Anchor> {
    let mut out = Vec::new();
    for bean in &model.beans {
        let names = std::iter::once(&bean.name).chain(&bean.aliases).chain(&bean.qualifiers);
        out.extend(names.filter(|n| !n.is_empty()).map(|n| Anchor { key: bean_key(n), range: bean.range }));
        if bean.kind == "@Bean" && !bean.name.is_empty() {
            out.push(Anchor { key: override_key(&bean.name), range: bean.range });
        }
        if bean.ty.is_empty() {
            continue;
        }
        out.push(Anchor { key: type_key(&bean.ty), range: bean.range });
        if bean.kind.starts_with('<') {
            out.push(Anchor { key: xml_key(&bean.ty), range: bean.range });
        }
    }
    out.extend(model.types.iter().map(|t| Anchor { key: class_key(&t.name), range: t.range }));
    out
}

/// Lookups of a supertype, of a published event and of a listened-to event follow the
/// project's hierarchy across files, which per-file definitions cannot.
fn aliases(model: &FileModel) -> Vec<Alias> {
    let pairs = model.types.iter().flat_map(|t| t.supers.iter().map(move |s| (t.name.as_str(), s.as_str())));
    pairs
        .flat_map(|(sub, sup)| {
            [
                Alias { prefix: format!("type:{sup}#"), target: format!("type:{sub}#") },
                Alias { prefix: format!("event:{sub}#"), target: format!("event:{sup}#") },
                Alias { prefix: format!("publish:{sup}#"), target: format!("publish:{sub}#") },
            ]
        })
        .take(MAX_ALIASES)
        .collect()
}

/// Markers from classes to the XML beans of that class, and from those beans back to the class.
fn class_xml_markers(model: &FileModel) -> Vec<Anchor> {
    let classes = model.types.iter().map(|t| Anchor { key: xml_key(&t.name), range: t.range });
    let xml_beans = model.beans.iter().filter(|b| b.kind.starts_with('<') && !b.ty.is_empty());
    classes.chain(xml_beans.map(|b| Anchor { key: class_key(&b.ty), range: b.range })).collect()
}

fn class_literals(text: &str) -> impl Iterator<Item = &str> {
    let word = |c: char| !(c.is_alphanumeric() || c == '_' || c == '.' || c == '$');
    text.split(word).filter_map(|w| w.strip_suffix(".class")).filter_map(|w| w.rsplit('.').next())
}

/// Event publishers (`publishEvent(new X(...))`) and listeners, as (definitions, markers):
/// a publisher's marker goes to the listeners of its event, a listener's to the publishers.
fn events(path: &str, text: &str) -> (Vec<Anchor>, Vec<Anchor>) {
    let (mut definitions, mut markers) = (Vec::new(), Vec::new());
    let kotlin = spring_text::is_kotlin(path);
    let file = spring_text::parse_source(path, text);
    let mut listen = |event: &str, range| {
        definitions.push(Anchor { key: listener_key(event), range });
        markers.push(Anchor { key: publisher_key(event), range });
    };
    for ty in &file.types {
        for method in ty.methods() {
            let Some(annotation) = method.annotations.iter().find(|a| LISTENER_ANNOTATIONS.contains(&a.name.as_str())) else { continue };
            let literals: Vec<String> = annotation
                .args
                .iter()
                .filter(|a| a.name == "value" || a.name == "classes")
                .map(|a| a.text.replace("::class", ".class"))
                .collect();
            let declared: Vec<&str> = literals.iter().flat_map(|text| class_literals(text)).collect();
            let from_parameter = method.params.first().map(|p| p.ty.name.as_str());
            let events = if declared.is_empty() { from_parameter.into_iter().collect() } else { declared };
            let at = range(text, method.at, method.at + method.name.len());
            events.into_iter().for_each(|event| listen(event, at));
        }
        let at = range(text, ty.at, ty.at + ty.name.len());
        for event in ty.implements.iter().filter(|i| i.name == "ApplicationListener").filter_map(|i| i.args.first()) {
            listen(&event.name, at);
        }
    }
    for call in spring_text::source_calls(path, text).iter().filter(|c| c.name == "publishEvent") {
        let Some(first) = call.args.first() else { continue };
        let created =
            if kotlin { Some(first.text.as_str()).filter(|t| t.starts_with(char::is_uppercase)) } else { first.text.strip_prefix("new ") };
        let Some(created) = created else { continue };
        let name: String = created.trim_start().chars().take_while(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '$')).collect();
        let Some(event) = name.rsplit('.').next().filter(|e| !e.is_empty()) else { continue };
        let at = range(text, call.at, call.at + call.name.len());
        markers.push(Anchor { key: listener_key(event), range: at });
        definitions.push(Anchor { key: publisher_key(event), range: at });
    }
    (definitions, markers)
}

fn injections(model: &FileModel) -> Vec<Anchor> {
    model
        .injections
        .iter()
        .filter(|i| !i.ty.is_empty() && i.qualifier.is_none())
        .map(|i| Anchor { key: type_key(&i.ty), range: i.range })
        .collect()
}

fn named_references(path: &str, text: &str) -> Vec<Anchor> {
    let is_xml = path.ends_with(".xml");
    let mut out = Vec::new();
    for hit in lib_hits(path, text) {
        if hit.site != Site::BeanRef {
            continue;
        }
        let Some(value) = text.get(hit.from..hit.to) else { continue };
        if value.contains("${") || value.contains("#{") {
            continue;
        }
        let names: Vec<(usize, &str)> = if is_xml { model::name_tokens(value).collect() } else { vec![(0, value)] };
        for (at, name) in names.into_iter().filter(|(_, n)| !n.is_empty()) {
            out.push(Anchor { key: bean_key(name), range: range(text, hit.from + at, hit.from + at + name.len()) });
        }
    }
    out
}

const RESOURCE_EXTENSIONS: [&str; 5] = ["xml", "properties", "yml", "yaml", "sql"];
const RESOURCE_ANNOTATIONS: [(&str, &[&str]); 5] = [
    ("ImportResource", &["locations", "value"]),
    ("PropertySource", &["value"]),
    ("TestPropertySource", &["locations", "value"]),
    ("ContextConfiguration", &["locations", "value"]),
    ("Sql", &["scripts", "value"]),
];

fn profile_key(name: &str) -> String {
    format!("profile#{name}")
}

fn resource_key(file: &str) -> String {
    format!("resource#{file}")
}

fn package_key(package: &str) -> String {
    format!("package:{package}#decl")
}

fn placeholder_key(key: &str) -> String {
    format!("placeholder#{}", config::canonical(key))
}

fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '-' | '_' | '.')
}

/// The file name of a resource location, unless it is a pattern or a placeholder.
fn resource_file(location: &str) -> Option<&str> {
    if location.contains(['*', '?', '$', '{', '#']) {
        return None;
    }
    location.rsplit(['/', ':']).next().filter(|n| n.contains('.'))
}

/// The `${key}` placeholders of `value` with the byte offsets of their keys: `${a:b}` is `a`; nested ones are skipped.
fn placeholders(value: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(open) = value[from..].find("${").map(|p| from + p) {
        let Some(close) = value[open..].find('}').map(|p| open + p) else { break };
        let inner = &value[open + 2..close];
        let key = inner.split(':').next().unwrap_or(inner);
        if !key.is_empty() && !key.contains(['$', '{']) {
            out.push((open + 2, key));
        }
        from = close + 1;
    }
    out
}

/// The anchors an annotation's strings give: profile names, packages, resources and `@Value` placeholders.
fn annotation_references(text: &str, annotation: &Annotation, out: &mut Vec<Anchor>) {
    let mut push = |key: String, from: usize, to: usize| out.push(Anchor { key, range: range(text, from, to) });
    let names = |keys: &[&str]| annotation.strings(keys);
    match annotation.name.as_str() {
        "Profile" | "ActiveProfiles" => {
            for s in names(&["value", "profiles"]) {
                for (at, name) in word_spans(&s.value) {
                    push(profile_key(name), s.at + at, s.at + at + name.len());
                }
            }
        }
        "ComponentScan" | "SpringBootApplication" => {
            for s in names(&["basePackages", "scanBasePackages", "value"]).into_iter().filter(|s| !s.value.contains(['$', '*', '{'])) {
                push(package_key(s.value.trim()), s.at, s.end);
            }
        }
        "Value" => {
            for s in names(&["value"]) {
                for (at, key) in placeholders(&s.value) {
                    push(placeholder_key(key), s.at + at, s.at + at + key.len());
                }
            }
        }
        name => {
            let Some((_, keys)) = RESOURCE_ANNOTATIONS.iter().find(|(n, _)| *n == name) else { return };
            for s in names(keys) {
                if let Some(file) = resource_file(&s.value) {
                    push(resource_key(file), s.end - file.len(), s.end);
                }
            }
        }
    }
}

/// Profile names in an expression such as `dev & !cloud`, with their byte offsets.
fn word_spans(expression: &str) -> impl Iterator<Item = (usize, &str)> {
    expression
        .split(|c: char| !is_name_char(c))
        .filter(|w| !w.is_empty())
        .map(move |w| (w.as_ptr() as usize - expression.as_ptr() as usize, w))
}

fn code_references(file: &JavaFile, text: &str, kotlin: bool) -> Vec<Anchor> {
    let mut out = Vec::new();
    let owners = crate::annotation_owners::Owners::new(file,text,kotlin);
    for ty in &file.types {
        let on_members = ty.members.iter().flat_map(|m| m.annotations.iter().chain(m.params.iter().flat_map(|p| p.annotations.iter())));
        let on_components = ty.components.iter().flat_map(|p| p.annotations.iter());
        for annotation in ty.annotations.iter().chain(on_members).chain(on_components) {
            if owners.keep_definition(text,annotation) { annotation_references(text, annotation, &mut out); }
        }
    }
    out
}

fn xml_references(text: &str) -> Vec<Anchor> {
    let els = xml::elements(text);
    if !crate::xml_namespace::beans_document(text) {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (i,e) in els.iter().enumerate() {
        let beans = crate::xml_namespace::beans_element(&els,i);
        let context = xml::namespace(&els,i)==Some("http://www.springframework.org/schema/context");
        for attr in e.attrs.iter().filter(|a| !a.name.starts_with("xmlns")) {
            let mut push = |key: String, from: usize, len: usize| out.push(Anchor { key, range: range(text, from, from + len) });
            match (e.local(), attr.name.as_str()) {
                ("beans", "profile") if beans => model::name_tokens(&attr.value)
                    .for_each(|(at, n)| push(profile_key(n.trim_start_matches('!')), attr.value_at + at, n.len())),
                ("component-scan", "base-package") if context => {
                    for (at, n) in model::name_tokens(&attr.value).filter(|(_, n)| !n.contains(['$', '*', '{'])) {
                        push(package_key(n), attr.value_at + at, n.len());
                    }
                }
                ("import", "resource") if beans => {
                    for (at, n) in model::name_tokens(&attr.value) {
                        if let Some(file) = resource_file(n) {
                            push(resource_key(file), attr.value_at + at + n.len() - file.len(), file.len());
                        }
                    }
                }
                ("property-placeholder", "location") if context => {
                    for (at, n) in model::name_tokens(&attr.value) {
                        if let Some(file) = resource_file(n) {
                            push(resource_key(file), attr.value_at + at + n.len() - file.len(), file.len());
                        }
                    }
                }
                _ => {}
            }
        }
    }
    out
}

/// Byte span of the first non-blank line, trimmed: the host rejects zero-width anchors, so an empty file gets none.
fn first_line(text: &str) -> Option<(usize, usize)> {
    let line = text.split_inclusive('\n').scan(0, |at, l| { let s = *at; *at += l.len(); Some((s, l)) })
        .find(|(_, l)| !l.trim().is_empty())?;
    let lead = line.1.len() - line.1.trim_start().len();
    Some((line.0 + lead, line.0 + lead + line.1.trim().len()))
}

/// `(definitions, references, aliases)` of the keys that link profiles, resource files, packages and placeholders.
fn link_keys(path: &str, text: &str) -> (Vec<Anchor>, Vec<Anchor>, Vec<Alias>) {
    let (mut definitions, mut references, mut aliases) = (Vec::new(), Vec::new(), Vec::new());
    let extension = path.rsplit('.').next().unwrap_or_default();
    if RESOURCE_EXTENSIONS.contains(&extension) {
        let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
        if let Some((from, to)) = first_line(text) {
            definitions.push(Anchor { key: resource_key(name), range: range(text, from, to) });
        }
    }
    if config::is_config_file(path) {
        let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
        let stem = name.rsplit_once('.').map_or(name, |(s, _)| s);
        if let Some((_, profile)) = stem.split_once('-') {
            if let Some((from, to)) = first_line(text) {
                definitions.push(Anchor { key: profile_key(profile), range: range(text, from, to) });
            }
        }
        definitions
            .extend(config::keys(path, text).into_iter().map(|k| Anchor { key: placeholder_key(&k.key), range: range(text, k.at, k.end) }));
    }
    if matches!(extension, "java" | "kt") {
        let file = spring_text::parse_source(path, text);
        if !file.package.is_empty() {
            if let Some(at) = text.find(&format!("package {}", file.package)) {
                let from = at + "package ".len();
                definitions.push(Anchor { key: package_key(&file.package), range: range(text, from, from + file.package.len()) });
            }
            let mut ancestor = file.package.as_str();
            while let Some((parent, _)) = ancestor.rsplit_once('.') {
                aliases.push(Alias { prefix: format!("package:{parent}#"), target: format!("package:{}#", file.package) });
                ancestor = parent;
            }
        }
        references.extend(code_references(&file, text, extension == "kt"));
    }
    if extension == "xml" {
        references.extend(xml_references(text));
    }
    (definitions, references, aliases)
}

pub(crate) fn navigation_of(path: &str, text: &str, model: &FileModel) -> Edges {
    let mut references = named_references(path, text);
    let markers = injections(&model);
    references.extend(markers.iter().cloned());
    let mut definitions = definitions(&model);
    let (event_definitions, event_markers) = events(path, text);
    definitions.extend(event_definitions);
    let mut markers = markers;
    markers.extend(event_markers);
    markers.extend(class_xml_markers(&model));
    markers.extend(
        model
            .beans
            .iter()
            .filter(|b| b.kind == "@Bean" && !b.name.is_empty())
            .map(|b| Anchor { key: override_key(&b.name), range: b.range }),
    );
    let (linked, linked_references, linked_aliases) = link_keys(path, text);
    definitions.extend(linked);
    markers.extend(linked_references.iter().filter(|a| a.key.starts_with("package:")).cloned());
    references.extend(linked_references);
    definitions.truncate(MAX_ANCHORS);
    references.truncate(MAX_ANCHORS);
    markers.truncate(MAX_ANCHORS);
    let mut aliases = aliases(&model);
    aliases.extend(linked_aliases);
    aliases.truncate(MAX_ALIASES);
    Edges { definitions, references, aliases, markers }
}

/// Whether host Rename may rewrite `range` to a new spelling of `key`'s name:
/// a profile or resource-file reference whose text is exactly that name.
/// Class keys are simple names, which other packages share, and the
/// synthetic first-line definitions of configuration files are not names;
/// providers keep resolving both.
fn renamable(text: &str, key: &str, range: openidea_plugin::Range) -> bool {
    let Some(name) = key.strip_prefix("profile#").or_else(|| key.strip_prefix("resource#")) else { return false };
    let (Some(from), Some(to)) = (offset(text, range.start), offset(text, range.end)) else { return false };
    text.get(from..to) == Some(name)
}

impl DocumentIndexer for Navigator {
    /// Only references whose anchors cover exactly a renamable name; no
    /// definitions, aliases or markers (the line-markers provider shows those).
    fn index(&mut self, doc: &Document) -> Edges {
        if doc.text.len() > crate::cache::MAX_TEXT || crate::cache::build_output(&doc.path) {
            return Edges::default();
        }
        let Some(model) = model::parse(&doc.path, &doc.text) else { return Edges::default() };
        let references = model
            .navigation
            .references
            .iter()
            .filter(|(key, range)| renamable(&doc.text, key, *range))
            .map(|(key, range)| Anchor { key: key.clone(), range: *range })
            .collect();
        Edges { references, ..Edges::default() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(path: &str, text: &str) -> Edges {
        let _host = openidea_plugin::mock::MockHost::install();
        let model = model::parse(path, text).unwrap_or_default();
        navigation_of(path, text, &model)
    }

    fn keys(anchors: &[Anchor]) -> Vec<&str> {
        anchors.iter().map(|a| a.key.as_str()).collect()
    }

    const SERVICE: &str = "@org.springframework.stereotype.Service(\"users\")\nclass UserServiceImpl implements UserService {\n    UserServiceImpl(UserRepository repo, @org.springframework.beans.factory.annotation.Qualifier(\"sms\") Notifier n) {}\n}\n";

    #[test]
    fn stereotype_class_defines_its_name_and_types() {
        let edges = index("A.java", SERVICE);
        let defined = keys(&edges.definitions);
        assert!(defined.contains(&"bean#users"));
        assert!(defined.contains(&"type:UserServiceImpl#bean"));
        assert!(!defined.contains(&"type:UserService#bean"));
        assert!(edges.aliases.iter().any(|a| a.prefix == "type:UserService#" && a.target == "type:UserServiceImpl#"));
    }

    #[test]
    fn injection_points_and_qualifiers_are_references() {
        let edges = index("A.java", SERVICE);
        let referenced = keys(&edges.references);
        assert!(referenced.contains(&"type:UserRepository#bean"));
        assert!(referenced.contains(&"bean#sms"));
        assert!(!referenced.contains(&"type:Notifier#bean"));
        assert_eq!(keys(&edges.markers), ["type:UserRepository#bean", "xml:UserServiceImpl#bean"]);
    }

    #[test]
    fn bean_method_defines_by_method_name() {
        let edges = index("C.java", "@org.springframework.context.annotation.Configuration\nclass C {\n    @org.springframework.context.annotation.Bean\n    Clock clock() { return null; }\n}\n");
        assert!(keys(&edges.definitions).contains(&"bean#clock"));
        assert!(keys(&edges.definitions).contains(&"type:Clock#bean"));
    }

    #[test]
    fn xml_beans_and_reference_lists() {
        let xml = "<beans>\n  <bean id=\"a\" class=\"com.acme.A\" depends-on=\"b, c\">\n    <property name=\"x\" ref=\"d\"/>\n  </bean>\n</beans>\n";
        let edges = index("beans.xml", xml);
        assert!(keys(&edges.definitions).contains(&"bean#a"));
        let referenced = keys(&edges.references);
        for name in ["bean#b", "bean#c", "bean#d"] {
            assert!(referenced.contains(&name), "{name}");
        }
        let b = edges.references.iter().find(|r| r.key == "bean#c").unwrap();
        assert_eq!((b.range.start.line, b.range.start.character), (1, 49));
    }

    #[test]
    fn unrelated_file_publishes_nothing() {
        let edges = index("Plain.java", "class Plain { int x; }\n");
        assert!(edges.references.is_empty() && edges.aliases.is_empty());
        assert_eq!(keys(&edges.definitions), ["class:Plain#decl"]);
    }

    const PUBLISHER: &str = "class Orders {\n    void place() {\n        events.publishEvent(new OrderPlaced(this));\n    }\n}\n";
    const LISTENERS: &str = "class Mails implements ApplicationListener<OrderShipped> {\n    @EventListener\n    void on(OrderPlaced e) {}\n    @TransactionalEventListener(classes = {OrderCancelled.class, Other.class})\n    void gone() {}\n}\n";

    #[test]
    fn publisher_marker_goes_to_listeners_and_back() {
        let published = index("Orders.java", PUBLISHER);
        assert_eq!(keys(&published.markers), ["event:OrderPlaced#listener", "xml:Orders#bean"]);
        assert_eq!(keys(&published.definitions), ["class:Orders#decl", "publish:OrderPlaced#call"]);
        assert_eq!(published.markers[0].range.start.line, 2);

        let listening = index("Mails.java", LISTENERS);
        let defined = keys(&listening.definitions);
        for key in ["event:OrderPlaced#listener", "event:OrderCancelled#listener", "event:Other#listener", "event:OrderShipped#listener"] {
            assert!(defined.contains(&key), "{key}");
        }
        assert!(keys(&listening.markers).contains(&"publish:OrderPlaced#call"));
    }

    #[test]
    fn unknown_event_payloads_are_ignored() {
        assert!(
            !keys(&index("A.kt", "class A { fun f(o: Any) { publisher.publishEvent(o) } }").markers)
                .iter()
                .any(|k| k.starts_with("event:"))
        );
        let edges = index("B.java", "class B { void f(Object o) { events.publishEvent(o); events.publishEvent(\"x\"); } }");
        assert!(!keys(&edges.markers).iter().any(|k| k.starts_with("event:")));
    }

    #[test]
    fn subtypes_alias_in_each_direction_of_their_lookup() {
        let edges = index("Admin.java", "class Admin extends User {}\n");
        let pairs: Vec<(&str, &str)> = edges.aliases.iter().map(|a| (a.prefix.as_str(), a.target.as_str())).collect();
        assert!(pairs.contains(&("type:User#", "type:Admin#")));
        assert!(pairs.contains(&("event:Admin#", "event:User#")));
        assert!(pairs.contains(&("publish:User#", "publish:Admin#")));
    }

    #[test]
    fn classes_and_their_xml_beans_mark_each_other() {
        let java = index("A.java", "class A {}\n");
        assert_eq!(keys(&java.markers), ["xml:A#bean"]);

        let xml =
            index("beans.xml", "<beans>\n  <bean id=\"a\" class=\"com.acme.A\"/>\n  <bean id=\"b\" class=\"com.acme.A\"/>\n</beans>\n");
        assert_eq!(keys(&xml.markers), ["class:A#decl", "class:A#decl"]);
        assert_eq!(keys(&xml.definitions).iter().filter(|k| **k == "xml:A#bean").count(), 2);
    }

    fn has(anchors: &[Anchor], key: &str, covers: &str, text: &str) -> bool {
        anchors.iter().any(|a| {
            let from = openidea_plugin::text::offset(text, a.range.start).unwrap();
            let to = openidea_plugin::text::offset(text, a.range.end).unwrap();
            a.key == key && &text[from..to] == covers
        })
    }

    #[test]
    fn profiles_packages_resources_and_placeholders_link_code_to_files() {
        let java = "package com.acme.web;\n@org.springframework.context.annotation.Profile({\"dev\", \"!cloud & test\"})\n@ComponentScan(basePackages = \"com.acme.billing\")\n@PropertySource(\"classpath:conf/app.properties\")\nclass A {\n    @org.springframework.beans.factory.annotation.Value(\"${app.name:x}-${app.port}\") String n;\n}\n";
        let edges = index("A.java", java);
        for (key, covers) in [
            ("profile#dev", "dev"),
            ("profile#cloud", "cloud"),
            ("profile#test", "test"),
            ("package:com.acme.billing#decl", "com.acme.billing"),
            ("resource#app.properties", "app.properties"),
            ("placeholder#app.name", "app.name"),
            ("placeholder#app.port", "app.port"),
        ] {
            assert!(has(&edges.references, key, covers, java), "{key}");
        }
        assert!(has(&edges.definitions, "package:com.acme.web#decl", "com.acme.web", java));
        let pairs: Vec<(&str, &str)> = edges.aliases.iter().map(|a| (a.prefix.as_str(), a.target.as_str())).collect();
        assert!(pairs.contains(&("package:com.acme#", "package:com.acme.web#")));
        assert!(pairs.contains(&("package:com#", "package:com.acme.web#")));
    }

    #[test]
    fn config_files_define_profiles_resources_and_placeholders() {
        let yaml = "app:\n  name: x\nserver:\n  max-port: 1\n";
        let edges = index("/p/src/main/resources/application-dev.yml", yaml);
        assert!(has(&edges.definitions, "resource#application-dev.yml", "app:", yaml));
        assert!(has(&edges.definitions, "profile#dev", "app:", yaml));
        assert!(has(&edges.definitions, "placeholder#app.name", "name", yaml));
        assert!(has(&edges.definitions, "placeholder#server.maxport", "max-port", yaml));
        let other = index("/p/src/main/resources/messages.properties", "a=b\n");
        assert_eq!(keys(&other.definitions), ["resource#messages.properties"]);
    }

    #[test]
    fn xml_links_profiles_packages_and_resources() {
        let xml = "<beans xmlns=\"http://www.springframework.org/schema/beans\" xmlns:context=\"http://www.springframework.org/schema/context\" profile=\"a, b\">\n  <import resource=\"classpath:x/other.xml\"/>\n  <context:component-scan base-package=\"com.acme, com.beta\"/>\n  <import resource=\"${p}.xml\"/>\n</beans>\n";
        let edges = index("beans.xml", xml);
        for (key, covers) in [
            ("profile#a", "a"),
            ("profile#b", "b"),
            ("resource#other.xml", "other.xml"),
            ("package:com.acme#decl", "com.acme"),
            ("package:com.beta#decl", "com.beta"),
        ] {
            assert!(has(&edges.references, key, covers, xml), "{key}");
        }
        assert!(!keys(&edges.references).iter().any(|k| k.contains("${")));
    }

    #[test]
    fn component_scan_and_bean_overrides_are_markers() {
        let scan = index("A.java", "@ComponentScan(\"com.acme\")\nclass A {}\n");
        assert!(keys(&scan.markers).contains(&"package:com.acme#decl"));
        let beans = index("C.java", "@org.springframework.context.annotation.Configuration\nclass C {\n    @org.springframework.context.annotation.Bean\n    Clock clock() { return null; }\n}\n");
        assert!(keys(&beans.markers).contains(&"override#clock"));
        assert!(keys(&beans.definitions).contains(&"override#clock"));
        let same = |key: &str| {
            beans.markers.iter().find(|a| a.key == key).map(|a| a.range) == beans.definitions.iter().find(|a| a.key == key).map(|a| a.range)
        };
        assert!(same("override#clock"), "the host drops a marker whose target is itself");
    }

    #[test]
    fn kotlin_publishers_and_listeners_are_linked() {
        let listener = index(
            "L.kt",
            "class L {\n    @EventListener\n    fun on(e: OrderPlaced) {}\n    @EventListener(classes = [Gone::class])\n    fun gone() {}\n}\n",
        );
        assert!(keys(&listener.definitions).contains(&"event:OrderPlaced#listener"));
        assert!(keys(&listener.definitions).contains(&"event:Gone#listener"));
        let publisher = index("P.kt", "class P {\n    fun place() {\n        events.publishEvent(OrderPlaced(this))\n    }\n}\n");
        assert!(keys(&publisher.markers).contains(&"event:OrderPlaced#listener"));
        assert!(keys(&publisher.definitions).contains(&"publish:OrderPlaced#call"));
    }
}

#[cfg(test)]
mod anchor_tests {
    use super::*;

    fn nonempty(r: &openidea_plugin::Range) -> bool {
        (r.start.line, r.start.character) < (r.end.line, r.end.character)
    }

    #[test]
    fn link_definitions_have_nonempty_anchors() {
        for (path, text) in [
            ("src/main/resources/application.yml", "spring:\n  application:\n    name: demo\n"),
            ("src/main/resources/application-dev.yml", "\n\n  caf\u{e9}\u{1F600}: 1\n"),
            ("src/main/resources/data.sql", "select 1;"),
        ] {
            let (definitions, ..) = link_keys(path, text);
            assert!(!definitions.is_empty(), "{path}");
            assert!(definitions.iter().all(|d| nonempty(&d.range)), "{path}");
        }
    }

    #[test]
    fn empty_resource_files_get_no_synthetic_definition() {
        for text in ["", "\n  \n"] {
            let (definitions, ..) = link_keys("src/main/resources/application-dev.yml", text);
            assert!(definitions.iter().all(|d| nonempty(&d.range)));
        }
    }
    fn published(path: &str, text: &str) -> Edges {
        let _host = openidea_plugin::mock::MockHost::install();
        Navigator::default().index(&Document { path: path.into(), text: text.into(), symbols: Vec::new() })
    }

    const DEV: &str = "package a;\n@org.springframework.context.annotation.Profile(\"dev & !cloud\")\n@org.springframework.context.annotation.ImportResource(\"classpath:config/beans.xml\")\n@org.springframework.stereotype.Service(\"devService\")\nclass Dev implements Api {\n /* é😀 */ @org.springframework.beans.factory.annotation.Autowired Api api;\n}\n";
    const BEANS: &str = "<beans xmlns=\"http://www.springframework.org/schema/beans\" profile=\"dev,qa\">\n  <import resource=\"classpath:other-beans.xml\"/>\n  <bean id=\"a\" class=\"a.A\"/>\n</beans>\n";

    #[test]
    fn indexer_publishes_profile_and_resource_references() {
        let java = published("/project/src/main/java/a/Dev.java", DEV);
        let keys: Vec<&str> = java.references.iter().map(|a| a.key.as_str()).collect();
        assert!(keys.contains(&"profile#dev") && keys.contains(&"profile#cloud") && keys.contains(&"resource#beans.xml"), "{keys:?}");
        let xml = published("/project/src/main/resources/beans.xml", BEANS);
        let keys: Vec<&str> = xml.references.iter().map(|a| a.key.as_str()).collect();
        assert!(keys.contains(&"profile#qa") && keys.contains(&"resource#other-beans.xml"), "{keys:?}");
        let config = published("/project/src/main/resources/application-dev.yml", "server:\n  port: 1\n");
        for edges in [&java, &xml, &config] {
            assert!(edges.definitions.is_empty() && edges.markers.is_empty() && edges.aliases.is_empty());
            assert!(edges.references.iter().all(|a| a.key.starts_with("resource#") || a.key.starts_with("profile#")));
        }
    }

    #[test]
    fn published_anchor_ranges_cover_exactly_the_name() {
        for (path, text) in [
            ("/project/src/main/java/a/Dev.java", DEV),
            ("/project/src/main/resources/beans.xml", BEANS),
            ("/project/src/main/resources/application-dev.yml", "app:\n  name: x\n"),
        ] {
            let edges = published(path, text);
            for anchor in edges.definitions.iter().chain(&edges.references).chain(&edges.markers) {
                let from = openidea_plugin::text::offset(text, anchor.range.start).unwrap();
                let to = openidea_plugin::text::offset(text, anchor.range.end).unwrap();
                let name = anchor.key.split_once('#').map_or(anchor.key.as_str(), |(_, name)| name);
                assert_eq!(&text[from..to], name, "{path}: {}", anchor.key);
            }
        }
    }
}
