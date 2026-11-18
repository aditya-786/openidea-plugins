//! Positive written DTO shapes, without assuming an application's serializer.
use openidea_plugin::{Error, Project};
use serde_json::{Value, json};
use spring_text::java::{JavaFile, TypeRef};
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

pub struct Sources {
    files: BTreeMap<String, (String, JavaFile)>,
    uncertain_packages: BTreeSet<String>,
}
const MAX_FILE: usize = 256 << 10;
const MAX_BYTES: usize = 8 << 20;
const MAX_FILES: usize = 2000;
const BUDGET: Duration = Duration::from_secs(30);

impl Sources {
    /// The `controllers` and, from them, the source files of the types they name: through
    /// imports, and through same-directory files whose name appears in the text. Files that cannot
    /// be read, are too large or fall outside the budget are listed in `skipped`.
    pub fn collect(
        project: &Project,
        controllers: &[String],
        skipped: &mut Vec<String>,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Self, Error> {
        let start = Instant::now();
        let mut by_dir: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
        for glob in ["**/*.java", "**/*.kt"] {
            let found = project.find_files(glob, 10_000)?;
            if found.len() >= 10_000 {
                skipped.push("some source files (more than 10 000 found)".into());
            }
            for path in found {
                let (dir, name) = path.rsplit_once('/').unwrap_or(("", path.as_str()));
                let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
                by_dir.entry(dir.to_owned()).or_default().push((stem.to_owned(), path.clone()));
            }
        }
        let mut queue = controllers.to_vec();
        let mut seen: BTreeSet<String> = queue.iter().cloned().collect();
        let mut files = BTreeMap::new();
        let mut bytes = 0usize;
        while let Some(path) = queue.pop() {
            if cancelled() {
                return Err(Error::Cancelled);
            }
            if start.elapsed() > BUDGET || files.len() >= MAX_FILES || bytes > MAX_BYTES {
                skipped.push("the remaining files (export budget reached)".into());
                break;
            }
            let name = path.rsplit('/').next().unwrap_or(&path);
            let text = match project.read_file(&path) {
                Ok(text) if text.len() <= MAX_FILE => text,
                Ok(_) => {
                    skipped.push(format!("{name} (over 256 KiB)"));
                    continue;
                }
                Err(_) => {
                    skipped.push(format!("{name} (unreadable)"));
                    continue;
                }
            };
            bytes += text.len();
            let parsed = spring_text::parse_source(&path, &text);
            for related in related_files(&path, &text, &parsed, &by_dir) {
                if seen.insert(related.clone()) {
                    queue.push(related);
                }
            }
            files.insert(path, (text, parsed));
        }
        Ok(Self::new(files))
    }
    fn new(files: BTreeMap<String, (String, JavaFile)>) -> Self {
        let uncertain_packages = files
            .iter()
            .filter(|(path, _)| path.ends_with(".kt"))
            .filter(|(_, (text, file))| {
                spring_text::java::tokens(text)
                    .iter()
                    .any(|t| t.s == "typealias")
                    || file.imports.iter().any(|i| {
                        spring_text::java::tokens(
                            text.get(i.end..)
                                .unwrap_or("")
                                .split('\n')
                                .next()
                                .unwrap_or(""),
                        )
                        .first()
                        .is_some_and(|t| t.s == "as")
                    })
            })
            .map(|(_, (_, file))| file.package.clone())
            .collect();
        Self {
            files,
            uncertain_packages,
        }
    }
    #[cfg(test)]
    fn from_files(files: &[(&str, &str)]) -> Self {
        Self::new(
            files
                .iter()
                .map(|(path, text)| {
                    (
                        path.to_string(),
                        (text.to_string(), spring_text::parse_source(path, text)),
                    )
                })
                .collect(),
        )
    }
    pub fn text(&self, path: &str) -> Option<String> {
        self.files.get(path).map(|(text, _)| text.clone())
    }
    pub fn schemas(&self) -> Schemas<'_> {
        Schemas {
            sources: self,
            components: serde_json::Map::new(),
        }
    }
}
fn related_files(
    path: &str,
    text: &str,
    file: &JavaFile,
    by_dir: &BTreeMap<String, Vec<(String, String)>>,
) -> Vec<String> {
    let words: BTreeSet<&str> = spring_text::java::tokens(text).into_iter().map(|t| t.s).collect();
    fn dir_of<'a>(
        by_dir: &'a BTreeMap<String, Vec<(String, String)>>,
        suffix: &'a str,
    ) -> impl Iterator<Item = &'a (String, String)> {
        by_dir.iter().filter(move |(dir, _)| dir.ends_with(suffix)).flat_map(|(_, files)| files)
    }
    let mut out = Vec::new();
    let own_dir = path.rsplit_once('/').map_or("", |(dir, _)| dir);
    if let Some(siblings) = by_dir.get(own_dir) {
        out.extend(siblings.iter().filter(|(stem, _)| words.contains(stem.as_str())).map(|(_, path)| path.clone()));
    }
    for import in file.imports.iter().filter(|i| !i.is_static) {
        let slashed = import.path.replace('.', "/");
        if let Some(package) = slashed.strip_suffix("/*") {
            let suffix = format!("/{package}");
            out.extend(
                dir_of(by_dir, &suffix).filter(|(stem, _)| words.contains(stem.as_str())).map(|(_, path)| path.clone()),
            );
            continue;
        }
        let mut class = slashed.as_str();
        for _ in 0..3 {
            let Some((package, stem)) = class.rsplit_once('/') else { break };
            let suffix = format!("/{package}");
            let found: Vec<_> = dir_of(by_dir, &suffix).filter(|(candidate, _)| candidate == stem).map(|(_, path)| path.clone()).collect();
            if !found.is_empty() {
                out.extend(found);
                break;
            }
            class = package;
        }
    }
    out
}
pub struct Schemas<'a> {
    sources: &'a Sources,
    pub components: serde_json::Map<String, Value>,
}
impl Schemas<'_> {
    pub fn schema(&mut self, ty: &TypeRef, context: &JavaFile) -> Option<Value> {
        self.resolve(ty, context, 0)
    }
    fn kotlin(&self, file: &JavaFile) -> bool {
        self.sources
            .files
            .iter()
            .any(|(path, (_, parsed))| path.ends_with(".kt") && parsed == file)
    }
    fn owner(&self, ty: &TypeRef, file: &JavaFile, expected: &str) -> bool {
        let head = ty.text.split(['<', '[', '?']).next().unwrap_or("").trim();
        if head.contains('.') {
            return head == expected;
        }
        if file.types.iter().any(|t| t.name == ty.name)
            || self.sources.files.values().any(|(_, other)| {
                other.package == file.package
                    && other
                        .types
                        .iter()
                        .any(|t| t.outer.is_none() && t.name == ty.name)
            })
        {
            return false;
        }
        let named = file
            .imports
            .iter()
            .filter(|i| !i.is_static && i.path.rsplit('.').next() == Some(ty.name.as_str()))
            .collect::<Vec<_>>();
        if !named.is_empty() {
            return named.len() == 1 && named[0].path == expected;
        }
        expected.starts_with("java.lang.")
            || expected.starts_with("kotlin.")
            || self.kotlin(file)
                && [
                    "java.util.List",
                    "java.util.Set",
                    "java.util.Collection",
                    "java.util.Iterable",
                    "java.util.Map",
                ]
                .contains(&expected)
            || file
                .imports
                .iter()
                .any(|i| i.path == format!("{}.*", expected.rsplit_once('.').unwrap().0))
    }
    fn resolve(&mut self, ty: &TypeRef, file: &JavaFile, depth: usize) -> Option<Value> {
        if self.sources.uncertain_packages.contains(&file.package) {
            return None;
        }
        if depth > 8 || ty.text.len() > 256 || ty.text.contains(['\\', '$']) {
            return None;
        }
        let simple = ty.name.as_str();
        let primitive = match simple {
            "int" | "long" | "short" | "byte" => Some("integer"),
            "double" | "float" => Some("number"),
            "boolean" => Some("boolean"),
            "char" => Some("string"),
            "String" | "Character" if self.owner(ty, file, &format!("java.lang.{simple}")) => {
                Some("string")
            }
            "Integer" | "Long" | "Short" | "Byte"
                if self.owner(ty, file, &format!("java.lang.{simple}")) =>
            {
                Some("integer")
            }
            "Double" | "Float" if self.owner(ty, file, &format!("java.lang.{simple}")) => {
                Some("number")
            }
            "Boolean" if self.owner(ty, file, "java.lang.Boolean") => Some("boolean"),
            "Int" | "Char" if self.owner(ty, file, &format!("kotlin.{simple}")) => {
                Some(if simple == "Int" { "integer" } else { "string" })
            }
            _ => None,
        };
        let mut schema = if let Some(kind) = primitive {
            json!({"type":kind})
        } else if ["List", "Set", "Collection", "Iterable"].contains(&simple)
            && ty.args.len() == 1
            && self.owner(
                ty,
                file,
                &format!(
                    "{}.{simple}",
                    if simple == "Iterable" {
                        "java.lang"
                    } else {
                        "java.util"
                    }
                ),
            )
        {
            json!({"type":"array","items":self.resolve(&ty.args[0],file,depth+1).unwrap_or_else(||json!({"x-java-type":ty.args[0].text}))})
        } else if simple == "Map"
            && ty.args.len() == 2
            && self.owner(ty, file, "java.util.Map")
            && ty.args[0].name == "String"
            && self.owner(&ty.args[0], file, "java.lang.String")
        {
            json!({"type":"object","additionalProperties":self.resolve(&ty.args[1],file,depth+1).unwrap_or_else(||json!({"x-java-type":ty.args[1].text}))})
        } else {
            if !ty.args.is_empty() {
                return None;
            }
            self.object(ty, file, depth + 1)?
        };
        if ty.array {
            let dimensions = ty.text.matches('[').count().max(1);
            if dimensions > 4 {
                return None;
            }
            for _ in 0..dimensions {
                schema = json!({"type":"array","items":schema})
            }
        }
        if ty.text.trim().ends_with('?') {
            if schema.get("$ref").is_some() {
                schema = json!({"type":"object","allOf":[schema],"nullable":true})
            } else {
                schema["nullable"] = json!(true)
            }
        }
        Some(schema)
    }
    fn object(&mut self, ty: &TypeRef, context: &JavaFile, depth: usize) -> Option<Value> {
        let head = ty.text.split(['<', '[', '?']).next()?.trim();
        let named = context
            .imports
            .iter()
            .filter(|i| !i.is_static && i.path.rsplit('.').next() == Some(ty.name.as_str()))
            .map(|i| i.path.clone())
            .collect::<Vec<_>>();
        if named.len() > 1 {
            return None;
        }
        let target = if head.contains('.') {
            head.to_owned()
        } else if let Some(import) = named.first() {
            import.clone()
        } else {
            format!(
                "{}{}{}",
                context.package,
                if context.package.is_empty() { "" } else { "." },
                ty.name
            )
        };
        if target.len() > 256
            || !target
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_'))
        {
            return None;
        }
        let found = self
            .sources
            .files
            .iter()
            .flat_map(|(path, (text, file))| {
                file.types
                    .iter()
                    .filter(move |t| t.outer.is_none())
                    .map(move |t| (path, text, file, t))
            })
            .filter(|(_, _, file, t)| {
                format!(
                    "{}{}{}",
                    file.package,
                    if file.package.is_empty() { "" } else { "." },
                    t.name
                ) == target
            })
            .collect::<Vec<_>>();
        let [(path, text, file, decl)] = found.as_slice() else {
            return None;
        };
        if !matches!(decl.kind.as_str(), "record" | "class" | "enum")
            || !decl.annotations.is_empty()
            || decl
                .methods()
                .any(|m| m.annotations.iter().any(|a| a.name.starts_with("Json")))
            || decl.supers().next().is_some()
            || text.contains("\\u")
            || decl.end > text.len()
        {
            return None;
        }
        let declaration = text.get(decl.start..decl.end)?;
        let mut braces = 0usize;
        for token in spring_text::java::tokens(declaration) {
            match token.s {
                "{" => braces += 1,
                "}" => {
                    if braces == 0 {
                        return None;
                    }
                    braces -= 1
                }
                _ => {}
            }
        }
        let kotlin = path.ends_with(".kt");
        if braces != 0 || !kotlin && !declaration.trim_end().ends_with('}') {
            return None;
        }
        if kotlin
            && (!decl.modifiers.iter().any(|m| m == "data")
                || declaration.contains('@')
                || spring_text::java::tokens(declaration)
                    .iter()
                    .any(|t| matches!(t.s, "private" | "protected" | "internal" | "typealias")))
        {
            return None;
        }
        if decl.kind == "enum" {
            let tokens = spring_text::java::tokens(declaration);
            let start = tokens.iter().position(|t| t.s == "{")? + 1;
            let body = tokens.get(start..tokens.len().checked_sub(1)?)?;
            let mut values = Vec::new();
            let mut need_name = true;
            for (i, token) in body.iter().enumerate() {
                if token.s == ";" && i + 1 == body.len() {
                    break;
                }
                if need_name {
                    if !token.is_ident() {
                        return None;
                    }
                    if values.contains(&token.s) {
                        return None;
                    }
                    values.push(token.s);
                    need_name = false
                } else if token.s == "," {
                    need_name = true
                } else {
                    return None;
                }
            }
            if values.is_empty() || values.len() > 256 {
                return None;
            }
            let key = target.clone();
            if self.components.len() >= 128 && !self.components.contains_key(&key) {
                return None;
            }
            self.components.insert(key.clone(),json!({"type":"string","enum":values,"x-openidea-source":{"path":path,"type":target},"description":"Written enum constant names; custom enum serialization is not evaluated."}));
            return Some(json!({"$ref":format!("#/components/schemas/{key}")}));
        }
        let header = declaration.split('{').next()?;
        let tokens = spring_text::java::tokens(header);
        if tokens
            .windows(2)
            .any(|p| p[0].at + decl.start == decl.at && p[1].s == "<")
            || header.contains("typealias")
        {
            return None;
        }
        if decl.kind == "record" {
            if decl.components.len() > 256
                || decl.components.is_empty()
                || decl.components.iter().any(|p| !validation_only(&p.annotations, file))
            {
                return None;
            }
        } else {
            let fields = decl.fields().filter(|f| {
                (kotlin || f.modifiers.iter().any(|m| m == "public"))
                    && !f
                        .modifiers
                        .iter()
                        .any(|m| matches!(m.as_str(), "static" | "transient"))
            });
            let fields = fields.collect::<Vec<_>>();
            if fields
                .iter()
                .any(|f| !validation_only(&f.annotations, file) || f.ty.is_none())
            {
                return None;
            }
            if fields.len() + decl.methods().filter(|m| getter(m).is_some()).count() > 256 {
                return None;
            }
            if fields.is_empty() && !decl.methods().any(|m| getter(m).is_some()) {
                return None;
            }
        }
        let key = target.clone();
        let reference = json!({"$ref":format!("#/components/schemas/{key}")});
        if self.components.contains_key(&key) {
            return Some(reference);
        }
        if self.components.len() >= 128 {
            return None;
        }
        // Placeholder makes recursive source objects terminate without dropping their identity.
        self.components.insert(key.clone(), json!({}));
        let mut properties = serde_json::Map::new();
        let mut required = Vec::new();
        if decl.kind == "record" {
            for param in &decl.components {
                let mut shape = self
                    .resolve(&param.ty, file, depth)
                    .unwrap_or_else(|| json!({"x-java-type":param.ty.text}));
                if apply_validation(&param.annotations, file, &mut shape) {
                    required.push(param.name.clone());
                }
                properties.insert(param.name.clone(), shape);
            }
        } else {
            for field in decl.fields().filter(|f| {
                (kotlin || f.modifiers.iter().any(|m| m == "public"))
                    && !f
                        .modifiers
                        .iter()
                        .any(|m| matches!(m.as_str(), "static" | "transient"))
            }) {
                let ty = field.ty.as_ref()?;
                let mut shape = self
                    .resolve(ty, file, depth)
                    .unwrap_or_else(|| json!({"x-java-type":ty.text}));
                if apply_validation(&field.annotations, file, &mut shape) {
                    required.push(field.name.clone());
                }
                properties.insert(field.name.clone(), shape);
            }
            for method in decl.methods() {
                if let Some((name, ty)) = getter(method) {
                    let shape = self
                        .resolve(ty, file, depth)
                        .unwrap_or_else(|| json!({"x-java-type":ty.text}));
                    if properties.get(&name).is_some_and(|before| before != &shape) {
                        properties.insert(
                            name,
                            json!({"x-openidea-unresolved":"Conflicting written property types"}),
                        );
                    } else {
                        properties.insert(name, shape);
                    }
                }
            }
        }
        let mut object = json!({"type":"object","properties":properties,"description":"Positive written DTO shape; serializer configuration and runtime inclusion are not evaluated; only literal Bean Validation constraints are mapped.","x-openidea-source":{"path":path,"type":target}});
        if !required.is_empty() {
            object["required"] = json!(required);
        }
        self.components.insert(key, object);
        Some(reference)
    }
}

const VALIDATION: &[&str] = &[
    "NotNull", "NotBlank", "NotEmpty", "Size", "Min", "Max", "DecimalMin", "DecimalMax",
    "Positive", "PositiveOrZero", "Negative", "NegativeOrZero", "Pattern", "Email",
];

fn is_validation(a: &spring_text::java::Annotation, file: &JavaFile) -> bool {
    VALIDATION.contains(&a.name.as_str())
        && ["jakarta", "javax"]
            .iter()
            .any(|p| file.imports.iter().any(|i| !i.is_static && i.path == format!("{p}.validation.constraints.{}", a.name)))
}

fn validation_only(annotations: &[spring_text::java::Annotation], file: &JavaFile) -> bool {
    annotations.iter().all(|a| is_validation(a, file))
}

fn literal_number(a: &spring_text::java::Annotation, name: &str) -> Option<Value> {
    let text = a.arg(name)?.text.trim();
    let text = match a.arg(name)?.strings.as_slice() {
        [s] if text.starts_with('"') => s.value.trim(),
        _ => text.trim_end_matches(['L', 'l']),
    };
    if let Ok(n) = text.parse::<i64>() {
        return Some(json!(n));
    }
    text.parse::<f64>().ok().filter(|f| f.is_finite()).map(|f| json!(f))
}

/// Maps literal Bean Validation constraints onto `shape`; returns whether the property is required.
fn apply_validation(annotations: &[spring_text::java::Annotation], file: &JavaFile, shape: &mut Value) -> bool {
    let kind = shape["type"].as_str().unwrap_or("").to_owned();
    let (lo, hi) = match kind.as_str() {
        "string" => ("minLength", "maxLength"),
        "array" => ("minItems", "maxItems"),
        "object" if shape.get("additionalProperties").is_some() => ("minProperties", "maxProperties"),
        _ => ("", ""),
    };
    let numeric = matches!(kind.as_str(), "integer" | "number");
    let mut required = false;
    let set = |shape: &mut Value, key: &str, value: Value| {
        if !key.is_empty() {
            shape[key] = value;
        }
    };
    for a in annotations.iter().filter(|a| is_validation(a, file)) {
        match a.name.as_str() {
            "NotNull" => required = true,
            "NotBlank" | "NotEmpty" => {
                required = true;
                if !(a.name == "NotBlank" && kind != "string") {
                    set(shape, lo, json!(1));
                }
            }
            "Size" => {
                for (arg, key) in [("min", lo), ("max", hi)] {
                    if let Some(n) = literal_number(a, arg).filter(|n| n.is_u64()) {
                        set(shape, key, n);
                    }
                }
            }
            "Pattern" => {
                if let [s] = a.arg("regexp").map(|x| x.strings.as_slice()).unwrap_or(&[]) {
                    if kind == "string" && a.arg("regexp").is_some_and(|x| x.text.trim().starts_with('"')) {
                        shape["pattern"] = json!(s.value);
                    }
                }
            }
            "Email" if kind == "string" => shape["format"] = json!("email"),
            _ if !numeric => {}
            "Min" | "Max" | "DecimalMin" | "DecimalMax" => {
                let upper = a.name.ends_with("Max");
                if let Some(n) = literal_number(a, "value") {
                    shape[if upper { "maximum" } else { "minimum" }] = n;
                    if a.arg("inclusive").is_some_and(|x| x.text.trim() == "false") && a.name.starts_with("Decimal") {
                        shape[if upper { "exclusiveMaximum" } else { "exclusiveMinimum" }] = json!(true);
                    }
                }
            }
            "Positive" => {
                shape["minimum"] = json!(0);
                shape["exclusiveMinimum"] = json!(true);
            }
            "PositiveOrZero" => shape["minimum"] = json!(0),
            "Negative" => {
                shape["maximum"] = json!(0);
                shape["exclusiveMaximum"] = json!(true);
            }
            "NegativeOrZero" => shape["maximum"] = json!(0),
            _ => {}
        }
    }
    required
}

fn getter(method: &spring_text::java::Member) -> Option<(String, &TypeRef)> {
    if !method.params.is_empty()
        || !method.modifiers.iter().any(|m| m == "public")
        || method.modifiers.iter().any(|m| m == "static")
        || !method.annotations.is_empty()
    {
        return None;
    }
    let ty = method.ty.as_ref()?;
    let suffix = if let Some(name) = method.name.strip_prefix("get") {
        if ty.name == "void" {
            return None;
        }
        name
    } else if let Some(name) = method.name.strip_prefix("is") {
        if !matches!(ty.name.as_str(), "boolean" | "Boolean") {
            return None;
        }
        name
    } else {
        return None;
    };
    if !suffix.starts_with(|c: char| c.is_ascii_uppercase()) {
        return None;
    }
    let mut chars = suffix.chars();
    let first = chars.next()?;
    let second = chars.next();
    if second.is_some_and(|c| c.is_ascii_uppercase()) {
        return None;
    }
    let name = {
        format!(
            "{}{}",
            first.to_ascii_lowercase(),
            &suffix[first.len_utf8()..]
        )
    };
    Some((name, ty))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn named(name: &str) -> TypeRef {
        TypeRef {
            name: name.into(),
            text: name.into(),
            ..Default::default()
        }
    }
    #[test]
    fn record_shapes_preserve_arrays_maps_recursion_and_source_identity() {
        let source = "package demo; import java.util.List; import java.util.Map; record Person(String name,int age,Person parent,List<Person> children,Map<String,Integer> counts,String[][] tags) {}";
        let sources = Sources::from_files(&[("/p/Person.java", source)]);
        let context = spring_text::java::parse("package demo;");
        let mut schemas = sources.schemas();
        assert_eq!(
            schemas.schema(&named("Person"), &context),
            Some(json!({"$ref":"#/components/schemas/demo.Person"}))
        );
        let object = &schemas.components["demo.Person"];
        let p = &object["properties"];
        assert_eq!(p["name"]["type"], "string");
        assert_eq!(p["age"]["type"], "integer");
        assert_eq!(p["parent"]["$ref"], "#/components/schemas/demo.Person");
        assert_eq!(
            p["children"]["items"]["$ref"],
            "#/components/schemas/demo.Person"
        );
        assert_eq!(p["counts"]["additionalProperties"]["type"], "integer");
        assert_eq!(p["tags"]["items"]["items"]["type"], "string");
        assert!(object.get("required").is_none());
        assert_eq!(object["x-openidea-source"]["path"], "/p/Person.java");
    }
    #[test]
    fn public_fields_are_positive_and_opaque_properties_do_not_invent_library_shapes() {
        let sources = Sources::from_files(&[(
            "/p/Dto.java",
            "package demo; class Dto {public String title; public External other; private String hidden; public static int shared; public transient int transientValue;}",
        )]);
        let mut schemas = sources.schemas();
        assert!(
            schemas
                .schema(&named("Dto"), &spring_text::java::parse("package demo;"))
                .is_some()
        );
        let p = &schemas.components["demo.Dto"]["properties"];
        assert_eq!(p["title"]["type"], "string");
        assert_eq!(p["other"], json!({"x-java-type":"External"}));
        assert_eq!(p.as_object().unwrap().len(), 2);
    }
    #[test]
    fn ambiguous_generic_annotated_inherited_and_custom_accessor_shapes_stay_unknown() {
        for source in [
            "package demo; @JsonSerialize class Dto {public String x;}",
            "package demo; class Dto extends Base {public String x;}",
            "package demo; class Dto<T> {public T x;}",
            "package demo; record Dto(@JsonProperty(\"other\") String x) {}",
            "package demo; record Dto(String x) {@JsonValue public String value(){return x;}}",
        ] {
            let sources = Sources::from_files(&[("/p/Dto.java", source)]);
            let mut schemas = sources.schemas();
            assert!(
                schemas
                    .schema(&named("Dto"), &spring_text::java::parse("package demo;"))
                    .is_none(),
                "{source}"
            );
            assert!(schemas.components.is_empty());
        }
        let sources = Sources::from_files(&[
            ("/a/Dto.java", "package demo; record Dto(String x) {}"),
            ("/b/Dto.java", "package demo; record Dto(int x) {}"),
        ]);
        assert!(
            sources
                .schemas()
                .schema(&named("Dto"), &spring_text::java::parse("package demo;"))
                .is_none()
        );
        let sources = Sources::from_files(&[
            ("/a/Dto.java", "package a; record Dto(String x) {}"),
            ("/b/Dto.java", "package b; record Dto(int x) {}"),
        ]);
        let context = spring_text::java::parse("import a.Dto; import b.Dto;");
        assert!(sources.schemas().schema(&named("Dto"), &context).is_none());
    }
    #[test]
    fn bean_getters_and_kotlin_data_properties_have_positive_written_shapes() {
        let sources = Sources::from_files(&[
            (
                "/p/Bean.java",
                "package demo; class Bean {private String name;public String getName(){return name;} public boolean isReady(){return true;} public String getURL(){return null;} public static int getGlobal(){return 1;} public int getBad(int x){return x;} }",
            ),
            (
                "/p/Data.kt",
                "package demo\ndata class Data(val name:String,val counts:List<Int>,val note:String?)",
            ),
        ]);
        let mut schemas = sources.schemas();
        assert!(
            schemas
                .schema(&named("Bean"), &spring_text::java::parse("package demo;"))
                .is_some()
        );
        let p = &schemas.components["demo.Bean"]["properties"];
        assert_eq!(p["name"]["type"], "string");
        assert_eq!(p["ready"]["type"], "boolean");
        assert!(p.get("URL").is_none());
        assert_eq!(p.as_object().unwrap().len(), 2);
        assert!(
            schemas
                .schema(&named("Data"), &spring_text::java::parse("package demo;"))
                .is_some()
        );
        let p = &schemas.components["demo.Data"]["properties"];
        assert_eq!(p["counts"]["items"]["type"], "integer");
        assert_eq!(p["note"]["nullable"], true);
        let sources = Sources::from_files(&[(
            "/p/Data.kt",
            "package demo\ndata class Data(private val secret:String)",
        )]);
        assert!(
            sources
                .schemas()
                .schema(&named("Data"), &spring_text::java::parse("package demo;"))
                .is_none()
        );
    }
    #[test]
    fn malformed_types_and_local_shadowed_primitives_are_not_fabricated() {
        let sources =
            Sources::from_files(&[("/p/Dto.java", "package demo; record Dto(String name) {")]);
        assert!(
            sources
                .schemas()
                .schema(&named("Dto"), &spring_text::java::parse("package demo;"))
                .is_none()
        );
        let sources = Sources::from_files(&[
            ("/p/String.java", "package demo; record String(int code) {}"),
            ("/p/Dto.java", "package demo; record Dto(String name) {}"),
        ]);
        let mut schemas = sources.schemas();
        assert!(
            schemas
                .schema(&named("Dto"), &spring_text::java::parse("package demo;"))
                .is_some()
        );
        assert_eq!(
            schemas.components["demo.Dto"]["properties"]["name"]["$ref"],
            "#/components/schemas/demo.String"
        );
    }
    #[test]
    fn enum_constants_are_source_names_and_kotlin_alias_context_stays_unknown() {
        let sources = Sources::from_files(&[
            ("/p/Status.java", "package demo; enum Status {OPEN,CLOSED;}"),
            ("/p/Dto.java", "package demo; record Dto(Status status) {}"),
        ]);
        let mut schemas = sources.schemas();
        assert!(
            schemas
                .schema(&named("Dto"), &spring_text::java::parse("package demo;"))
                .is_some()
        );
        assert_eq!(
            schemas.components["demo.Status"]["enum"],
            json!(["OPEN", "CLOSED"])
        );
        let sources = Sources::from_files(&[(
            "/p/Data.kt",
            "package demo\nimport external.Value as String\ndata class Data(val value:String)",
        )]);
        assert!(
            sources
                .schemas()
                .schema(&named("Data"), &spring_text::java::parse("package demo;"))
                .is_none()
        );
        let sources = Sources::from_files(&[(
            "/p/Data.kt",
            "package demo\ntypealias String = Int\ndata class Data(val value:String)",
        )]);
        assert!(
            sources
                .schemas()
                .schema(&named("Data"), &spring_text::java::parse("package demo;"))
                .is_none()
        );
    }
    #[test]
    fn media_unknown_shapes_are_extensions_and_unreferenced_oversized_file_is_not_read() {
        use openidea_plugin::{Plugin, mock::MockHost};
        let host = MockHost::install();
        host.add_file(
            "src/demo/Dto.java",
            "package demo; record Dto(String name) {}",
        );
        host.add_file("src/demo/Api.java","package demo; @RestController class Api {@PostMapping(\"/items\") Dto save(@RequestBody Dto body){return body;}}");
        crate::WebTools::default()
            .run_command("copy-endpoints-openapi", &host.invocation(), None)
            .unwrap();
        let exported: Value = serde_json::from_str(&host.clipboard().unwrap()).unwrap();
        let op = &exported["paths"]["/items"]["post"];
        assert_eq!(
            op["x-spring-source-request-schema"]["$ref"],
            "#/components/schemas/demo.Dto"
        );
        assert_eq!(
            op["x-spring-source-response-schema"]["$ref"],
            "#/components/schemas/demo.Dto"
        );
        assert!(op.get("requestBody").is_none());
        assert!(op["responses"]["default"].get("content").is_none());
        host.add_file("src/TooLarge.java", &" ".repeat((256 << 10) + 1));
        crate::WebTools::default()
            .run_command("copy-endpoints-openapi", &host.invocation(), None)
            .unwrap();
        assert!(host.clipboard().unwrap().contains("/items"), "a file no endpoint refers to is never read");
    }
    #[test]
    fn openapi_export_skips_unreadable_file() {
        use openidea_plugin::{Plugin, mock::MockHost};
        let host = MockHost::install();
        host.add_file("src/demo/Dto.java", "package demo; public record Dto(String name) {}");
        host.add_file("src/demo/Api.java","package demo; @RestController class Api {@PostMapping(\"/items\") Dto save(@RequestBody Dto body){return body;}}");
        let big = format!("package demo; @RestController class Big {{@GetMapping(\"/big\") String big(){{return \"\";}}}}{}", " ".repeat((256 << 10) + 1));
        host.add_file("src/demo/Big.java", &big);
        crate::WebTools::default()
            .run_command("copy-endpoints-openapi", &host.invocation(), None)
            .unwrap();
        assert!(!host.tasks().is_empty(), "the long work runs inside a task");
        let exported: Value = serde_json::from_str(&host.clipboard().unwrap()).unwrap();
        assert_eq!(exported["paths"]["/items"]["post"]["x-spring-source-request-schema"]["$ref"], "#/components/schemas/demo.Dto");
        let notes: Vec<_> = host.notifications().into_iter().map(|n| n.body).collect();
        assert!(notes.iter().any(|m| m.contains("Big.java (over 256 KiB)")), "{notes:?}");
    }
    #[test]
    fn fresh_registered_openapi_command_attaches_declared_dto_media_shapes() {
        use openidea_plugin::{Plugin, mock::MockHost};
        let host = MockHost::install();
        host.add_file(
            "src/demo/Dto.java",
            "package demo; public record Dto(String name,int count) {}",
        );
        host.add_file("src/demo/Api.java","package demo; @RestController class Api { @PostMapping(value=\"/items\",consumes=\"application/json\",produces=\"application/json\") Dto save(@RequestBody Dto body){return body;} }");
        crate::WebTools::default()
            .run_command("copy-endpoints-openapi", &host.invocation(), None)
            .unwrap();
        let exported: Value = serde_json::from_str(&host.clipboard().unwrap()).unwrap();
        let operation = &exported["paths"]["/items"]["post"];
        assert_eq!(
            operation["requestBody"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/demo.Dto"
        );
        assert_eq!(
            operation["responses"]["default"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/demo.Dto"
        );
        assert_eq!(
            exported["components"]["schemas"]["demo.Dto"]["properties"]["count"]["type"],
            "integer"
        );
        host.add_file(
            "src/demo/Dto.java",
            "package demo; public record Dto(boolean ready) {}",
        );
        crate::WebTools::default()
            .run_command("copy-endpoints-openapi", &host.invocation(), None)
            .unwrap();
        let exported: Value = serde_json::from_str(&host.clipboard().unwrap()).unwrap();
        let p = &exported["components"]["schemas"]["demo.Dto"]["properties"];
        assert_eq!(p["ready"]["type"], "boolean");
        assert!(p.get("count").is_none());
    }

    #[test]
    fn bean_validation_literals_map_to_schema_constraints_for_exact_imports_only() {
        let source = "package demo; import jakarta.validation.constraints.*; import jakarta.validation.constraints.NotBlank; import jakarta.validation.constraints.Size; import jakarta.validation.constraints.Min; import jakarta.validation.constraints.Positive; import jakarta.validation.constraints.Pattern; import jakarta.validation.constraints.Email; import jakarta.validation.constraints.NotNull; import jakarta.validation.constraints.DecimalMax; import java.util.List;
            record Dto(@NotBlank @Size(min=2,max=9) @Pattern(regexp=\"[a-z]+\") String name, @Email String mail, @Min(3) @Positive int n, @DecimalMax(value=\"1.5\",inclusive=false) double d, @NotNull @Size(max=4) List<String> tags) {}";
        let sources = Sources::from_files(&[("/p/Dto.java", source)]);
        let mut schemas = sources.schemas();
        assert!(schemas.schema(&named("Dto"), &spring_text::java::parse("package demo;")).is_some());
        let o = &schemas.components["demo.Dto"];
        let p = &o["properties"];
        assert_eq!(o["required"], json!(["name", "tags"]));
        assert_eq!(p["name"]["minLength"], 2);
        assert_eq!(p["name"]["maxLength"], 9);
        assert_eq!(p["name"]["pattern"], "[a-z]+");
        assert_eq!(p["mail"]["format"], "email");
        assert_eq!(p["n"]["minimum"], 0);
        assert_eq!(p["n"]["exclusiveMinimum"], true);
        assert_eq!(p["d"]["maximum"], 1.5);
        assert_eq!(p["d"]["exclusiveMaximum"], true);
        assert_eq!(p["tags"]["maxItems"], 4);
        let foreign = Sources::from_files(&[("/p/Dto.java", "package demo; record Dto(@NotNull String x) {}")]);
        assert!(foreign.schemas().schema(&named("Dto"), &spring_text::java::parse("package demo;")).is_none());
    }
}
