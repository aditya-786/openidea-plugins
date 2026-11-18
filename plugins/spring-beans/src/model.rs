//! What one source file says about beans: the beans it declares, the
//! injection points it has, its types' supertypes and its named pointcuts.

use crate::xml_namespace::CoreAttributes;
use crate::kotlin;
use openidea_plugin::Range;
use spring_text::constants::Constants;
use spring_text::java::{self, Annotation, MemberKind, TypeRef};
use spring_text::{decapitalize, range, xml};

const P_NAMESPACE: &str = "http://www.springframework.org/schema/p";
const C_NAMESPACE: &str = "http://www.springframework.org/schema/c";

/// Whether the attribute of `els[i]` names a bean: `ref=`-style attributes, and
/// `p:x-ref` / `c:x-ref` when the prefix is bound to the `p`/`c` namespaces.
pub fn is_xml_ref(els: &[xml::Element], i: usize, a: &xml::Attr) -> bool {
    let e=&els[i];
    if e.local()=="aspect" && a.name=="ref" && xml::namespace(els,i)==Some("http://www.springframework.org/schema/aop")
        && e.parent.is_some_and(|p|els[p].local()=="config"&&xml::namespace(els,p)==Some("http://www.springframework.org/schema/aop")){return true;}
    if !crate::xml_namespace::beans_element(els,i){return false;}
    if match e.local() {
        "bean"=>matches!(a.name.as_str(),"parent"|"depends-on"|"factory-bean"),
        "property"|"constructor-arg"=>a.name=="ref",
        "ref"|"idref"=>a.name=="bean",
        "entry"=>matches!(a.name.as_str(),"key-ref"|"value-ref"),
        _=>false,
    } {return true;}
    is_xml_shortcut(els,i,a) && a.local().strip_suffix("-ref").is_some_and(|local|!local.is_empty())
}
/// A p/c attribute on a source-owned bean; namespace aliases are resolved.
pub(crate) fn is_xml_shortcut(els:&[xml::Element],i:usize,a:&xml::Attr)->bool {
    if els[i].local()!="bean" || !crate::xml_namespace::beans_element(els,i){return false;}
    matches!(crate::xml_namespace::attribute_namespace(els,i,a),Some(P_NAMESPACE|C_NAMESPACE))
}

/// The names of a `name`, `ref` or `profile` style list, with their byte offsets in `value`:
/// Spring splits these on `,`, `;` and whitespace and drops empty tokens.
pub fn name_tokens(value: &str) -> impl Iterator<Item = (usize, &str)> {
    value
        .split(|c: char| c == ',' || c == ';' || c.is_whitespace())
        .filter(|token| !token.is_empty())
        .map(move |token| (token.as_ptr() as usize - value.as_ptr() as usize, token))
}

pub const INJECT: [&str; 3] = ["Autowired", "Inject", "Resource"];
/// Hold one bean: `Optional<Foo>` is a `Foo`.
const SINGLE_WRAPPERS: [&str; 5] = ["Optional", "ObjectProvider", "ObjectFactory", "Provider", "Lazy"];
/// Hold every bean of their last type argument: `List<Handler>`.
const MULTI_WRAPPERS: [&str; 6] = ["List", "Set", "SortedSet", "Collection", "Iterable", "Map"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bean {
    pub name: String,
    pub aliases: Vec<String>,
    /// Shown as is: `@Service`, `@Bean`, `Spring Data repository`, `<bean>`.
    pub kind: String,
    /// Simple name of the bean's type; empty when unknown.
    pub ty: String,
    pub primary: bool,
    pub profiles: Vec<String>,
    /// `@Qualifier`/`@Named` values on the declaration.
    pub qualifiers: Vec<String>,
    /// Carries a `@Conditional*` annotation: it may not exist at run time.
    pub conditional: bool,
    /// The literal Spring Boot conditions understood from source; others only set `conditional`.
    pub conditions: Vec<Condition>,
    /// `<bean abstract="true">`: a template, never instantiated.
    pub is_abstract: bool,
    pub range: Range,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Condition {
    /// `@ConditionalOnProperty`: every full name must match.
    Property { names: Vec<String>, having: Option<String>, match_if_missing: bool },
    /// `@ConditionalOnMissingBean`: no bean of any of these simple type names may exist.
    MissingBean(Vec<String>),
}

/// What the project's `application.*` files say; `profile_scoped` files and multi-document files only count as occurrences.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConfigFacts {
    pub profile_scoped: bool,
    pub keys: Vec<(String, String)>,
}

impl Bean {
    pub fn answers_to(&self, name: &str) -> bool {
        self.name == name || self.aliases.iter().any(|a| a == name) || self.qualifiers.iter().any(|q| q == name)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Injection {
    /// Simple type name; empty for a by-name reference (XML `ref`).
    pub ty: String,
    /// The field or parameter name, Spring's last resort between candidates.
    pub name: String,
    pub qualifier: Option<String>,
    /// A collection, map or array: every candidate is injected.
    pub multiple: bool,
    pub range: Range,
    /// Index of the bean (in the same file) it is injected into.
    pub owner: Option<usize>,
    /// The `@Autowired`/`@Inject` of an injected field.
    pub field_annotation: Option<Range>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MethodInfo {
    pub name: String,
    pub params: usize,
    pub is_static: bool,
    pub is_public: bool,
    pub is_final: bool,
    pub is_private: bool,
    /// Carries `@Required`.
    pub required: bool,
    pub range: Range,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeInfo {
    pub name: String,
    pub qualified: String,
    pub supers: Vec<String>,
    pub range: Range,
    /// An abstract class or an interface.
    pub is_abstract: bool,
    /// `methods` and `ctors` list every member: false for Kotlin, enums, records and Lombok classes.
    pub members_known: bool,
    pub methods: Vec<MethodInfo>,
    /// The parameter count of each declared constructor.
    pub ctors: Vec<usize>,
    /// The parameter names of each declared constructor.
    pub ctor_names: Vec<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pointcut {
    pub name: String,
    pub class: String,
    pub range: Range,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileModel {
    pub navigation: ui_kit::navigation::Graph,
    pub beans: Vec<Bean>,
    pub injections: Vec<Injection>,
    pub types: Vec<TypeInfo>,
    pub pointcuts: Vec<Pointcut>,
    /// Declares `@SpringBootApplication` or `@EnableAutoConfiguration`.
    pub auto_configuration: bool,
    /// `Type.NAME` and the value of each `static final` string constant that resolves.
    pub constants: Vec<(String, String)>,
    pub config: Option<ConfigFacts>,
}

/// The model of a Java, Kotlin or Spring XML file (`None`: nothing to keep).
pub fn parse(path: &str, text: &str) -> Option<FileModel> {
    let mut model = if path.ends_with(".java") {
        java_model(text)
    } else if path.ends_with(".kt") {
        kotlin::model(text)
    } else if path.ends_with(".xml") {
        if !crate::xml_namespace::namespace_document(text){return None;}
        if crate::xml_namespace::beans_document(text){xml_model(text)}else{FileModel::default()}
    } else if matches!(path.rsplit('.').next(), Some("properties" | "yaml" | "yml" | "sql")) {
        FileModel { config: config_facts(path, text), ..FileModel::default() }
    } else {
        return None;
    };
    model.navigation = crate::indexer::navigation_of(path, text, &model).into();
    // Bean references and injection candidates already have a more precise
    // provider (profiles/qualifiers/primary). Never bypass it with graph aliases.
    let supplemental = |key: &str| !key.starts_with("type:") && !key.starts_with("bean#");
    model.navigation.definitions.retain(|(key, _)| supplemental(key));
    model.navigation.references.retain(|(key, _)| supplemental(key));
    model.navigation.markers.retain(|(key, _)| supplemental(key));
    (model != FileModel::default()).then_some(model)
}

pub fn strings_of(a: &Annotation, names: &[&str]) -> Vec<String> {
    a.strings(names).into_iter().map(|s| s.value.clone()).filter(|s| !s.is_empty()).collect()
}

fn values(annotations: &[Annotation], names: &[&str]) -> Vec<String> {
    annotations.iter().filter(|a| names.contains(&a.name.as_str())).flat_map(|a| strings_of(a, &["value"])).collect()
}

/// The type an injection point of type `ty` asks for, and whether it takes every candidate.
fn unwrap(ty: &TypeRef) -> (&TypeRef, bool) {
    if ty.array {
        return (ty, true);
    }
    match ty.args.last() {
        Some(last) if MULTI_WRAPPERS.contains(&ty.name.as_str()) => (unwrap(last).0, true),
        Some(last) if SINGLE_WRAPPERS.contains(&ty.name.as_str()) => unwrap(last),
        _ => (ty, false),
    }
}

/// Adds the injection point `name: ty`, unless it is a `@Value`.
pub fn push_injection(
    text: &str,
    out: &mut FileModel,
    ty: &TypeRef,
    name: &str,
    annotations: &[Annotation],
    owner: Option<usize>,
    field_annotation: Option<Range>,
) {
    if annotations.iter().any(|a| a.name == "Value") {
        return;
    }
    let (target, multiple) = unwrap(ty);
    let qualifier = annotations.iter().find_map(|a| match a.name.as_str() {
        "Qualifier" | "Named" => strings_of(a, &["value"]).pop(),
        "Resource" => strings_of(a, &["name"]).pop(),
        _ => None,
    });
    out.injections.push(Injection {
        ty: target.name.clone(),
        name: name.to_owned(),
        qualifier,
        multiple,
        range: range(text, target.at, target.at + target.name.len()),
        owner,
        field_annotation,
    });
}

fn config_facts(path: &str, text: &str) -> Option<ConfigFacts> {
    let name = path.rsplit(['/', '\\']).next()?;
    let stem = name.rsplit_once('.')?.0;
    if !spring_text::config::is_config_file(path) || !stem.starts_with("application") {
        return None;
    }
    let multi_document = text.lines().any(|l| matches!(l.trim_end(), "---" | "#---" | "!---"));
    Some(ConfigFacts {
        profile_scoped: stem != "application" || multi_document,
        keys: spring_text::config::keys(path, text).into_iter().map(|k| (k.key, k.value)).collect(),
    })
}

/// A literal-only argument: string literals, braces and commas.
fn literal_strings(a: &java::Arg) -> Option<Vec<String>> {
    let literal = java::tokens(&a.text).iter().all(|t| t.string().is_some() || matches!(t.s, "{" | "}" | ","));
    literal.then(|| a.strings.iter().map(|s| s.value.clone()).collect())
}

/// Simple type names of `Foo.class` / `{A.class, b.C.class}`.
fn class_literals(a: &java::Arg) -> Option<Vec<String>> {
    let toks = java::tokens(&a.text);
    let mut out = Vec::new();
    for part in toks.split(|t| matches!(t.s, "{" | "}" | ",")).filter(|p| !p.is_empty()) {
        let [.., name, dot, class] = part else { return None };
        if dot.s != "." || class.s != "class" || !name.is_ident() {
            return None;
        }
        out.push(name.s.to_owned());
    }
    (!out.is_empty()).then_some(out)
}

/// Literal conditions of owned (`ConditionalOnProperty`, `ConditionalOnMissingBean`) annotations; `implied` is the `@Bean` return type.
pub fn conditions(annotations: &[Annotation], implied: &str) -> Vec<Condition> {
    let mut out = Vec::new();
    for a in annotations {
        match a.name.as_str() {
            "ConditionalOnProperty" => out.extend(property_condition(a)),
            "ConditionalOnMissingBean" => {
                if a.args.is_empty() {
                    if !implied.is_empty() {
                        out.push(Condition::MissingBean(vec![implied.to_owned()]));
                    }
                    continue;
                }
                if a.args.iter().all(|x| matches!(x.name.as_str(), "value" | "type")) {
                    let types: Option<Vec<Vec<String>>> = a.args.iter().map(class_literals).collect();
                    out.extend(types.map(|t| Condition::MissingBean(t.concat())));
                }
            }
            _ => {}
        }
    }
    out
}

fn property_condition(a: &Annotation) -> Option<Condition> {
    const KNOWN: [&str; 5] = ["prefix", "name", "value", "havingValue", "matchIfMissing"];
    if a.args.iter().any(|x| !KNOWN.contains(&x.name.as_str())) || (a.arg("name").is_some() && a.arg("value").is_some()) {
        return None;
    }
    let names = literal_strings(a.arg("name").or_else(|| a.arg("value"))?)?;
    let mut prefix = a.arg("prefix").map_or(Some(Vec::new()), literal_strings)?.pop().unwrap_or_default();
    if !prefix.is_empty() && !prefix.ends_with('.') {
        prefix.push('.');
    }
    let having = match a.arg("havingValue") {
        Some(h) => literal_strings(h)?.pop().filter(|v| !v.is_empty()),
        None => None,
    };
    let match_if_missing = match a.arg("matchIfMissing").map(|m| m.text.trim()) {
        None | Some("false") => false,
        Some("true") => true,
        Some(_) => return None,
    };
    let names: Vec<String> = names.into_iter().map(|n| format!("{prefix}{n}")).collect();
    (!names.is_empty()).then_some(Condition::Property { names, having, match_if_missing })
}

pub fn is_conditional(annotations: &[Annotation]) -> bool {
    annotations.iter().any(|a| a.name.starts_with("Conditional"))
}

/// `Outer.Inner` for a nested type: Spring's default bean name derives from it.
pub fn short_name(file: &java::JavaFile, ty: &java::TypeDecl) -> String {
    let mut parts = vec![ty.name.as_str()];
    let mut outer = ty.outer;
    while let Some(i) = outer {
        let Some(t) = file.types.get(i) else { break };
        parts.push(t.name.as_str());
        outer = t.outer;
    }
    parts.reverse();
    parts.join(".")
}

/// A class bean's declaration from its annotations; `None` when it isn't one.
pub fn class_bean(text: &str, name: &str, short_name: &str, at: usize, annotations: &[Annotation], repository: bool) -> Option<Bean> {
    let stereotype = annotations.iter().find(|a| java::STEREOTYPES.contains(&a.name.as_str()));
    if stereotype.is_none() && !repository {
        return None;
    }
    let explicit = stereotype.and_then(|a| strings_of(a, &["value"]).pop());
    Some(Bean {
        name: explicit.unwrap_or_else(|| decapitalize(short_name)),
        aliases: Vec::new(),
        kind: stereotype.map_or("Spring Data repository".into(), |a| format!("@{}", a.name)),
        ty: name.to_owned(),
        primary: annotations.iter().any(|a| a.name == "Primary"),
        profiles: values(annotations, &["Profile"]),
        qualifiers: values(annotations, &["Qualifier", "Named"]),
        conditional: is_conditional(annotations),
        conditions: conditions(annotations, ""),
        is_abstract: false,
        range: range(text, at, at + name.len()),
    })
}

/// A `@Bean` method's declaration; its profiles default to the class's.
pub fn method_bean(text: &str, name: &str, at: usize, ty: &str, annotations: &[Annotation], class: &[String]) -> Bean {
    let mut names = annotations.iter().find(|a| a.name == "Bean").map(|a| strings_of(a, &["name", "value"])).unwrap_or_default();
    let first = if names.is_empty() { name.to_owned() } else { names.remove(0) };
    let profiles = values(annotations, &["Profile"]);
    Bean {
        name: first,
        aliases: names,
        kind: "@Bean".into(),
        ty: ty.to_owned(),
        primary: annotations.iter().any(|a| a.name == "Primary"),
        profiles: if profiles.is_empty() { class.to_vec() } else { profiles },
        qualifiers: values(annotations, &["Qualifier", "Named"]),
        conditional: is_conditional(annotations),
        conditions: conditions(annotations, ty),
        is_abstract: false,
        range: range(text, at, at + name.len()),
    }
}

/// Whether a type with these supertypes is a Spring Data repository interface.
pub fn is_repository<'a>(kind: &str, mut supers: impl Iterator<Item = &'a str>) -> bool {
    kind == "interface" && supers.any(|s| s.ends_with("Repository"))
}

const LOMBOK: [&str; 10] = [
    "Data",
    "Value",
    "Getter",
    "Setter",
    "Builder",
    "SuperBuilder",
    "AllArgsConstructor",
    "NoArgsConstructor",
    "RequiredArgsConstructor",
    "With",
];

fn type_info(file: &java::JavaFile, index: usize, text: &str) -> TypeInfo {
    let t = &file.types[index];
    let modifier = |m: &java::Member, name: &str| m.modifiers.iter().any(|x| x == name);
    let methods = t
        .methods()
        .map(|m| MethodInfo {
            name: m.name.clone(),
            params: m.params.len(),
            is_static: modifier(m, "static"),
            is_public: modifier(m, "public") || t.kind == "interface",
            is_final: modifier(m, "final"),
            is_private: modifier(m, "private"),
            required: m.annotations.iter().any(|a| a.name == "Required"),
            range: range(text, m.at, m.at + m.name.len()),
        })
        .collect();
    let ctors = t.members.iter().filter(|m| m.kind == MemberKind::Constructor).map(|m| m.params.len()).collect();
    let ctor_names =
        t.members.iter().filter(|m| m.kind == MemberKind::Constructor).map(|m| m.params.iter().map(|p| p.name.clone()).collect()).collect();
    TypeInfo {
        name: t.name.clone(),
        qualified: file.qualified(index),
        supers: t.supers().map(|s| s.name.clone()).collect(),
        range: range(text, t.at, t.at + t.name.len()),
        is_abstract: t.kind == "interface" || t.modifiers.iter().any(|m| m == "abstract"),
        members_known: matches!(t.kind.as_str(), "class" | "interface") && !t.has(&LOMBOK),
        methods,
        ctors,
        ctor_names,
    }
}

fn constants_of(file: &java::JavaFile, text: &str) -> Vec<(String, String)> {
    let mut known = Constants::default();
    known.add(file, text);
    let mut out = Vec::new();
    for t in &file.types {
        for field in t.fields().filter(|f| f.modifiers.iter().any(|m| m == "static") && f.modifiers.iter().any(|m| m == "final")) {
            let key = format!("{}.{}", t.name, field.name);
            if let Some(value) = known.resolve(&key, None) {
                out.push((key, value));
            }
        }
    }
    out
}

fn java_model(text: &str) -> FileModel {
    let mut file = java::parse(text);
    crate::annotation_owners::Owners::new(&file,text,false).filter_definitions(&mut file,text);
    let mut out = FileModel::default();
    out.constants = constants_of(&file, text);
    out.auto_configuration = file.types.iter().any(|t| t.has(&["SpringBootApplication", "EnableAutoConfiguration"]));
    for (index, t) in file.types.iter().enumerate() {
        let supers: Vec<String> = t.supers().map(|s| s.name.clone()).collect();
        out.types.push(type_info(&file, index, text));
        let repository = !t.has(&["NoRepositoryBean"]) && is_repository(&t.kind, supers.iter().map(String::as_str));
        let bean =
            if t.kind == "@interface" { None } else { class_bean(text, &t.name, &short_name(&file, t), t.at, &t.annotations, repository) };
        let class_profiles = bean.as_ref().map(|b| b.profiles.clone()).unwrap_or_default();
        let class_conditional = bean.as_ref().is_some_and(|b| b.conditional);
        let class_conditions = bean.as_ref().map(|b| b.conditions.clone()).unwrap_or_default();
        let owner = bean.map(|b| {
            out.beans.push(b);
            out.beans.len() - 1
        });
        if owner.is_some() {
            for p in &t.components {
                push_injection(text, &mut out, &p.ty, &p.name, &p.annotations, owner, None);
            }
        }
        let lombok = t.has(&["RequiredArgsConstructor", "AllArgsConstructor"]);
        for m in &t.members {
            match m.kind {
                MemberKind::Constructor if owner.is_some() || m.has(&["Autowired", "Inject"]) => {
                    for p in &m.params {
                        push_injection(text, &mut out, &p.ty, &p.name, &p.annotations, owner, None);
                    }
                }
                MemberKind::Method if m.has(&["Bean"]) => {
                    let ty = m.ty.as_ref().map(|t| t.name.as_str()).unwrap_or_default();
                    let mut bean = method_bean(text, &m.name, m.at, ty, &m.annotations, &class_profiles);
                    bean.conditional |= class_conditional;
                    bean.conditions.extend(class_conditions.iter().cloned());
                    out.beans.push(bean);
                    let bean = Some(out.beans.len() - 1);
                    for p in &m.params {
                        push_injection(text, &mut out, &p.ty, &p.name, &p.annotations, bean, None);
                    }
                }
                MemberKind::Method if m.has(&["Autowired", "Inject"]) => {
                    for p in &m.params {
                        push_injection(text, &mut out, &p.ty, &p.name, &p.annotations, owner, None);
                    }
                }
                MemberKind::Method if m.has(&["Pointcut"]) => out.pointcuts.push(Pointcut {
                    name: m.name.clone(),
                    class: t.name.clone(),
                    range: range(text, m.at, m.at + m.name.len()),
                }),
                MemberKind::Field => {
                    let Some(ty) = &m.ty else { continue };
                    let injected = m.annotations.iter().find(|a| INJECT.contains(&a.name.as_str()));
                    let modifier = |s: &str| m.modifiers.iter().any(|x| x == s);
                    let constructor_field = owner.is_some() && lombok && modifier("final") && !modifier("static");
                    if injected.is_some() || constructor_field {
                        let annotation = injected.filter(|a| a.name != "Resource").map(|a| range(text, a.at, a.end));
                        push_injection(text, &mut out, ty, &m.name, &m.annotations, owner, annotation);
                    }
                }
                _ => {}
            }
        }
    }
    out
}

fn simple(class: &str) -> &str {
    class.rsplit(['.', '$']).next().unwrap_or(class)
}

fn xml_model(text: &str) -> FileModel {
    let els = xml::elements(text);
    let mut out = FileModel::default();
    if xml::root(&els) != Some("beans") {
        return out;
    }
    let attr_range = |a: &xml::Attr| range(text, a.value_at, a.value_end);
    let mut owners: Vec<Option<usize>> = vec![None; els.len()];
    let mut aliases = Vec::new();
    for (i, e) in els.iter().enumerate().skip(1) {
        let parent = e.parent.and_then(|p| owners[p]);
        owners[i] = parent;
        if e.local() == "alias" {
            if let (Some(name), Some(alias)) = (e.core_value("name"), e.core_value("alias")) {
                aliases.push((name.to_owned(), alias.to_owned()));
            }
            continue;
        }
        let top = e.parent.is_some_and(|p| els[p].local() == "beans");
        let id = e.core_attr("id");
        let names: Vec<String> = name_tokens(e.core_value("name").unwrap_or("")).map(|(_, n)| n.to_owned()).collect();
        let class = e.core_value("class").unwrap_or("");
        let is_bean = e.local() == "bean" && (id.is_some() || !names.is_empty() || !class.is_empty());
        if is_bean || top && id.is_some() {
            let mut all: Vec<String> = id.map(|a| a.value.clone()).into_iter().chain(names).collect();
            if all.is_empty() {
                all.push(format!("{class}#0"));
            }
            let mut profiles = Vec::new();
            let mut up = e.parent;
            while let Some(p) = up {
                if let Some(profile) = els[p].core_value("profile") {
                    profiles.extend(name_tokens(profile).map(|(_, p)| p.to_owned()));
                }
                up = els[p].parent;
            }
            let at = id.or(e.core_attr("name")).or(e.core_attr("class")).map_or(range(text, e.at, e.at + e.name.len()), attr_range);
            out.beans.push(Bean {
                name: all.remove(0),
                aliases: all,
                kind: format!("<{}>", e.name),
                ty: simple(class).to_owned(),
                primary: e.core_value("primary") == Some("true"),
                profiles,
                qualifiers: Vec::new(),
                conditional: false,
                conditions: Vec::new(),
                is_abstract: e.core_value("abstract") == Some("true"),
                range: at,
            });
            owners[i] = Some(out.beans.len() - 1);
        }
        let by_name = e.attrs.iter().filter(|a| is_xml_ref(&els, i, a));
        for a in by_name {
            for (_, name) in name_tokens(&a.value) {
                out.injections.push(Injection {
                    ty: String::new(),
                    name: name.to_owned(),
                    qualifier: Some(name.to_owned()),
                    multiple: false,
                    range: attr_range(a),
                    owner: owners[i],
                    field_annotation: None,
                });
            }
        }
    }
    for (name, alias) in aliases {
        if let Some(b) = out.beans.iter_mut().find(|b| b.name == name) {
            b.aliases.push(alias);
        }
    }
    out
}
