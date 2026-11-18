//! Effective context ownership from literal `@Import`, `@ImportResource`, `@ComponentScan`
//! (and `@SpringBootApplication`) annotations and XML `<import>` elements.
//! Anything not provably literal taints the contexts containing it, and a tainted
//! or unrelated file is never narrowed.
use crate::shared::Refs;
use crate::{Ref, Role};
use openidea_plugin::Range;
use spring_text::java::{self, Arg, JavaFile};
use spring_text::xml;
use std::collections::{BTreeMap, BTreeSet};

const MAX_DEPTH: usize = 8;

fn edge(what: &str, name: String) -> Ref {
    Ref { name, role: Role::Edge, what: what.into(), range: Range::default(), checked: false, owner: 0 }
}
fn unsure() -> Ref {
    edge("unsure", String::new())
}

fn literal_strings(arg: &Arg) -> Option<Vec<String>> {
    let t = java::tokens(&arg.text);
    let body = match (t.first(), t.last()) {
        (Some(a), Some(b)) if t.len() >= 2 && a.s == "{" && b.s == "}" => &t[1..t.len() - 1],
        _ => &t[..],
    };
    let (mut out, mut want) = (Vec::new(), true);
    for tok in body {
        if want {
            let s = tok.string()?;
            if tok.s.starts_with("\"\"\"") || s.value.is_empty() || s.value.contains('\\') {
                return None;
            }
            out.push(s.value);
            want = false;
        } else if tok.s == "," {
            want = true;
        } else {
            return None;
        }
    }
    (!out.is_empty()).then_some(out)
}

fn class_literals(arg: &Arg) -> Option<Vec<String>> {
    let t = java::tokens(&arg.text);
    let body = match (t.first(), t.last()) {
        (Some(a), Some(b)) if t.len() >= 2 && a.s == "{" && b.s == "}" => &t[1..t.len() - 1],
        _ => &t[..],
    };
    let mut out = Vec::new();
    for seg in body.split(|tok| tok.s == ",") {
        let n = seg.len();
        if n < 3 || seg[n - 1].s != "class" || seg[n - 2].s != "." {
            return None;
        }
        let name = &seg[..n - 2];
        if name.iter().enumerate().any(|(i, tok)| if i % 2 == 0 { !tok.is_ident() } else { tok.s != "." }) || name.len() % 2 == 0 {
            return None;
        }
        out.push(name.iter().map(|tok| tok.s).collect::<String>());
    }
    (!out.is_empty()).then_some(out)
}

fn qualify(file: &JavaFile, written: &str) -> Option<String> {
    if written.contains('.') {
        return written.starts_with(|c: char| c.is_ascii_lowercase()).then(|| written.to_string());
    }
    let suffix = format!(".{written}");
    if let Some(i) = file.imports.iter().find(|i| !i.is_static && i.path.ends_with(&suffix)) {
        return Some(i.path.clone());
    }
    if file.imports.iter().any(|i| i.path.ends_with(".*")) {
        return None;
    }
    Some(if file.package.is_empty() { written.into() } else { format!("{}.{written}", file.package) })
}

fn packages(strings: Vec<String>, out: &mut Vec<Ref>) {
    for value in strings {
        for pkg in value.split(|c: char| matches!(c, ',' | ';') || c.is_whitespace()).filter(|p| !p.is_empty()) {
            let valid = pkg.split('.').all(|seg| !seg.is_empty() && seg.chars().all(|c| c.is_alphanumeric() || c == '_'));
            out.push(if valid { edge("scan", pkg.into()) } else { unsure() });
        }
    }
}

/// `basePackageClasses`: the scanned package is the class's package (ClassUtils.getPackageName).
/// A nested or default-package class is not provable from the literal alone.
fn class_packages(file: &JavaFile, classes: Vec<String>, out: &mut Vec<Ref>) {
    for class in classes {
        let package = qualify(file, &class)
            .and_then(|q| q.rsplit_once('.').map(|(p, _)| p.to_string()))
            .filter(|p| p.rsplit('.').next().is_some_and(|last| last.starts_with(|c: char| c.is_ascii_lowercase())));
        out.push(package.map_or_else(unsure, |p| edge("scan", p)));
    }
}

fn scan_args(file: &JavaFile, a: &java::Annotation, strings: &[&str], classes: &str, out: &mut Vec<Ref>) {
    let literal_names = a.args.iter().filter(|x| strings.contains(&x.name.as_str())).count();
    if literal_names > 1 || a.args.iter().any(|x| !strings.contains(&x.name.as_str()) && x.name != classes) {
        out.push(unsure());
        return;
    }
    for x in &a.args {
        if x.name == classes {
            match class_literals(x) {
                Some(found) => class_packages(file, found, out),
                None => out.push(unsure()),
            }
        } else {
            match literal_strings(x) {
                Some(found) => packages(found, out),
                None => out.push(unsure()),
            }
        }
    }
}

const IMPORT_HOOKS: [&str; 3] = ["ImportSelector", "DeferredImportSelector", "ImportBeanDefinitionRegistrar"];

/// Context edges and type identities of one Java/Kotlin file.
pub(crate) fn source_edges(path: &str, file: &JavaFile, text: &str) -> Vec<Ref> {
    let mut out = Vec::new();
    let kotlin = path.ends_with(".kt");
    for t in &file.types {
        if t.extends.iter().chain(&t.implements).any(|r| IMPORT_HOOKS.contains(&r.name.as_str())) {
            out.push(unsure());
        }
        for a in &t.annotations {
            let n = a.name.as_str();
            let qualified = match n {
                "Import" | "ImportResource" | "ComponentScan" | "ComponentScans" | "Profile" => format!("org.springframework.context.annotation.{n}"),
                "SpringBootApplication" => "org.springframework.boot.autoconfigure.SpringBootApplication".into(),
                _ if n.starts_with("Conditional") => {
                    out.push(unsure());
                    continue;
                }
                _ => continue,
            };
            let Some(written) = text.get(a.at + 1..a.name_at + n.len()) else {
                out.push(unsure());
                continue;
            };
            if !crate::source_type_owned(file, written, &qualified) {
                out.push(unsure());
                continue;
            }
            if kotlin || n == "Profile" || a.args.iter().any(|x| a.args.iter().filter(|y| y.name == x.name).count() > 1) {
                out.push(unsure());
                continue;
            }
            let only = |names: &[&str]| a.args.iter().all(|x| names.contains(&x.name.as_str()));
            match n {
                "Import" if only(&["value"]) && a.args.len() == 1 => match class_literals(&a.args[0]) {
                    Some(classes) => {
                        for class in classes {
                            out.push(qualify(file, &class).map_or_else(unsure, |q| edge("import-class", q)));
                        }
                    }
                    None => out.push(unsure()),
                },
                "ImportResource" if only(&["value", "locations"]) && a.args.len() == 1 => match literal_strings(&a.args[0]) {
                    Some(values) => {
                        for v in values {
                            let location = v.strip_prefix("classpath:").map(|p| p.trim_start_matches('/'));
                            out.push(match location {
                                Some(p) if resource_ok(p) => edge("import-resource", p.into()),
                                _ => unsure(),
                            });
                        }
                    }
                    None => out.push(unsure()),
                },
                "ComponentScan" | "SpringBootApplication" if a.args.is_empty() => {
                    out.push(if file.package.is_empty() { unsure() } else { edge("scan", file.package.clone()) });
                }
                "ComponentScan" => scan_args(file, a, &["basePackages", "value"], "basePackageClasses", &mut out),
                "SpringBootApplication" => scan_args(file, a, &["scanBasePackages"], "scanBasePackageClasses", &mut out),
                _ => out.push(unsure()),
            }
        }
    }
    out
}

/// Type identities, so `X.class` and package scans can find the declaring file.
pub(crate) fn source_types(file: &JavaFile) -> Vec<Ref> {
    file.types
        .iter()
        .filter(|t| t.outer.is_none())
        .map(|t| edge("type", if file.package.is_empty() { t.name.clone() } else { format!("{}.{}", file.package, t.name) }))
        .collect()
}

fn resource_ok(p: &str) -> bool {
    !p.is_empty() && !p.contains("${") && !p.contains('*') && !p.chars().any(char::is_control)
        && p.split('/').all(|s| !matches!(s, "" | "." | ".."))
}

/// `<import resource>` edges of the root-level beans of a Spring XML.
pub(crate) fn xml_edges(els: &[xml::Element], text: &str) -> Vec<Ref> {
    let _ = text;
    let mut out = Vec::new();
    for (i, e) in els.iter().enumerate() {
        if e.local() != "import" || !matches!(xml::namespace(els, i), None | Some("http://www.springframework.org/schema/beans")) {
            continue;
        }
        let at_root = e.parent == Some(0);
        let Some(a) = e.attrs.iter().find(|a| a.name == "resource") else {
            out.push(unsure());
            continue;
        };
        let v = a.value.as_str();
        out.push(match v.strip_prefix("classpath:") {
            _ if !at_root => unsure(),
            Some(p) if resource_ok(p.trim_start_matches('/')) => edge("import-resource", p.trim_start_matches('/').into()),
            None if !v.contains(':') && resource_ok(v) => edge("xml-relative", v.into()),
            _ => unsure(),
        });
    }
    out
}

fn module(path: &str) -> Option<&str> {
    let at = ["/src/main/", "/src/test/"].iter().filter_map(|m| path.find(m)).min()?;
    Some(&path[..at])
}

#[derive(Default)]
pub(crate) struct Contexts {
    member: BTreeMap<String, BTreeSet<usize>>,
    tainted: BTreeSet<usize>,
}

fn build(index: &Refs<'_>) -> Contexts {
    let mut types: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (path, refs) in index.iter() {
        for r in refs.iter().filter(|r| r.role == Role::Edge && r.what == "type") {
            types.entry(r.name.as_str()).or_default().push(path);
        }
    }
    let mut edges: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    let mut taint: BTreeSet<&str> = BTreeSet::new();
    for (path, refs) in index.iter() {
        for r in refs.iter().filter(|r| r.role == Role::Edge && r.what != "type") {
            let name = r.name.as_str();
            let targets: Option<Vec<&str>> = match r.what.as_str() {
                "import-class" => types.get(name).filter(|v| v.len() == 1).cloned(),
                "import-resource" => module(path).and_then(|m| {
                    ["main", "test"].iter().find_map(|s| index.entry(&format!("{m}/src/{s}/resources/{name}")).map(|(k, _)| vec![k]))
                }),
                "xml-relative" => path.rsplit_once('/').and_then(|(dir, _)| index.entry(&format!("{dir}/{name}")).map(|(k, _)| vec![k])),
                "scan" => module(path).map(|m| {
                    let prefix = format!("{m}/src/");
                    let dotted = format!("{name}.");
                    types.iter()
                        .filter(|(q, _)| q.rsplit_once('.').is_some_and(|(pkg, _)| pkg == name || pkg.starts_with(&dotted)))
                        .flat_map(|(_, v)| v.iter().copied())
                        .filter(|p| p.starts_with(&prefix) && *p != path)
                        .collect()
                }),
                _ => None,
            };
            match targets {
                Some(found) => edges.entry(path).or_default().extend(found),
                None => {
                    taint.insert(path);
                }
            }
        }
    }
    let mut incoming: BTreeSet<&str> = BTreeSet::new();
    for targets in edges.values() {
        incoming.extend(targets.iter().copied());
    }
    let nodes: BTreeSet<&str> = edges.keys().copied().chain(incoming.iter().copied()).chain(taint.iter().copied()).collect();
    let mut contexts = Contexts::default();
    let mut covered: BTreeSet<&str> = BTreeSet::new();
    let mut roots: Vec<&str> = nodes.iter().copied().filter(|n| !incoming.contains(n)).collect();
    let mut i = 0;
    loop {
        while i < roots.len() {
            let mut seen: BTreeSet<&str> = BTreeSet::from([roots[i]]);
            let mut frontier = vec![roots[i]];
            let mut bad = false;
            for depth in 0..=MAX_DEPTH {
                let mut next = Vec::new();
                for n in &frontier {
                    for t in edges.get(n).into_iter().flatten() {
                        if seen.insert(t) {
                            next.push(*t);
                        }
                    }
                }
                if depth == MAX_DEPTH && !next.is_empty() {
                    bad = true;
                }
                frontier = next;
            }
            if bad || seen.iter().any(|n| taint.contains(n)) {
                contexts.tainted.insert(i);
            }
            for n in seen {
                covered.insert(n);
                contexts.member.entry(n.into()).or_default().insert(i);
            }
            i += 1;
        }
        match nodes.iter().find(|n| !covered.contains(*n)) {
            Some(n) => roots.push(n),
            None => break,
        }
    }
    contexts
}

thread_local! {
    static CACHE: std::cell::RefCell<Option<((u64, u64), std::rc::Rc<Contexts>)>> = const { std::cell::RefCell::new(None) };
}

/// The contexts of `index`, built once per state of it: an index id and generation are never reused, even after a reset.
pub(crate) fn cached(index: &Refs<'_>) -> std::rc::Rc<Contexts> {
    let key = (index.id(), index.generation());
    if let Some(found) = CACHE.with_borrow(|cache| cache.as_ref().filter(|(k, _)| *k == key).map(|(_, contexts)| contexts.clone())) {
        return found;
    }
    let contexts = std::rc::Rc::new(build(index));
    CACHE.with_borrow_mut(|cache| *cache = Some((key, contexts.clone())));
    contexts
}

impl Contexts {
    /// Definitions visible from `usage`'s effective contexts plus how many contexts
    /// they span (0 when no ownership claim is made).
    pub(crate) fn scope<'a>(&self, usage: &str, defs: Vec<(&'a str, &'a Ref)>) -> (Vec<(&'a str, &'a Ref)>, usize) {
        let Some(mine) = self.member.get(usage).filter(|m| m.is_disjoint(&self.tainted)) else {
            return (defs, 0);
        };
        let kept: Vec<_> = defs
            .iter()
            .copied()
            .filter(|(p, _)| match self.member.get(*p) {
                Some(theirs) if theirs.is_disjoint(&self.tainted) => !theirs.is_disjoint(mine),
                _ => true,
            })
            .collect();
        if kept.is_empty() {
            return (defs, 0);
        }
        let scoped: Vec<_> = kept.iter().filter(|(p, _)| self.member.contains_key(*p)).collect();
        let files: BTreeSet<&str> = scoped.iter().map(|(p, _)| *p).collect();
        let owners: BTreeSet<usize> = files.iter().flat_map(|p| self.member[*p].intersection(mine).copied()).collect();
        let n = if files.len() > 1 { owners.len() } else { 0 };
        (kept, n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::{Record, refs_of};
    use spring_text::index::Index;

    fn build(index: &Index<Record>) -> Contexts {
        super::build(&index.view(refs_of))
    }

    fn index(files: &[(&str, &str)]) -> Index<Record> {
        let mut index = Index::new(&spring_text::beans::GLOBS, crate::shared::record);
        for (p, t) in files {
            index.update(p, t);
        }
        index
    }

    const R: &str = "/p/src/main/resources/";
    const J: &str = "/p/src/main/java/a/";
    fn bean_xml(id: &str) -> String {
        format!("<beans xmlns=\"http://www.springframework.org/schema/beans\"><bean id=\"{id}\" class=\"x.Y\"/></beans>")
    }
    fn xml_import(res: &str) -> String {
        format!("<beans xmlns=\"http://www.springframework.org/schema/beans\"><import resource=\"{res}\"/></beans>")
    }
    fn java(imp: &str, name: &str) -> String {
        format!("package a;\nimport org.springframework.context.annotation.Configuration;\nimport org.springframework.context.annotation.Import;\nimport org.springframework.context.annotation.ImportResource;\nimport org.springframework.context.annotation.ComponentScan;\nimport org.springframework.context.annotation.Profile;\nimport org.springframework.context.annotation.ComponentScans;\n{imp}\n@Configuration\npublic class {name} {{}}\n")
    }
    fn def<'a>(index: &'a Index<Record>, path: &str) -> (&'a str, &'a Ref) {
        index.view(refs_of).iter().find(|(p, _)| *p == path).map(|(p, r)| (p, r.iter().find(|r| r.role == Role::Bean).unwrap())).unwrap()
    }

    #[test]
    fn a_reset_index_never_reuses_cached_contexts() {
        let _host = openidea_plugin::mock::MockHost::install();
        let (a, b, shared) = (format!("{R}a.xml"), format!("{R}b.xml"), format!("{R}shared.xml"));
        let files = |import: &str, other: &str| vec![(a.clone(), xml_import(import)), (b.clone(), bean_xml("b")), (shared.clone(), bean_xml(other))];
        let mut indexes = spring_text::index::Indexes::new(&spring_text::beans::GLOBS, crate::shared::record);
        let project = openidea_plugin::host::projects().unwrap().remove(0);
        for (path, text) in files("shared.xml", "s") {
            indexes.get(&project, std::time::Duration::ZERO).update(&path, &text);
        }
        let first = cached(&indexes.get(&project, std::time::Duration::ZERO).view(refs_of));
        assert!(first.member.get(&b).is_none());
        indexes.reset();
        let imports_b = "b.xml";
        for (path, text) in files(imports_b, "s") {
            indexes.get(&project, std::time::Duration::ZERO).update(&path, &text);
        }
        let second = cached(&indexes.get(&project, std::time::Duration::ZERO).view(refs_of));
        assert!(!std::rc::Rc::ptr_eq(&first, &second));
        assert!(second.member.get(&b).is_some(), "the new import closure reaches b.xml");
    }

    #[test]
    fn contexts_are_built_once_per_generation() {
        let _host = openidea_plugin::mock::MockHost::install();
        let mut index = index(&[(&format!("{R}a.xml"), &bean_xml("a"))]);
        let first = cached(&index.view(refs_of));
        assert!(std::rc::Rc::ptr_eq(&first, &cached(&index.view(refs_of))));
        index.update(&format!("{R}b.xml"), &bean_xml("b"));
        assert!(!std::rc::Rc::ptr_eq(&first, &cached(&index.view(refs_of))));
    }

    #[test]
    fn xml_imports_scope_definitions_and_report_shared_contexts() {
        let (a, b, shared) = (format!("{R}a.xml"), format!("{R}b.xml"), format!("{R}shared.xml"));
        let (da, db) = (format!("{R}da.xml"), format!("{R}db.xml"));
        let index = index(&[
            (&a, &format!("<beans xmlns=\"http://www.springframework.org/schema/beans\"><import resource=\"shared.xml\"/><import resource=\"da.xml\"/></beans>")),
            (&b, &format!("<beans xmlns=\"http://www.springframework.org/schema/beans\"><import resource=\"classpath:/shared.xml\"/><import resource=\"db.xml\"/></beans>")),
            (&shared, &bean_xml("s")),
            (&da, &bean_xml("dup")),
            (&db, &bean_xml("dup")),
        ]);
        let ctx = build(&index);
        let defs = vec![def(&index, &da), def(&index, &db)];
        let (kept, n) = ctx.scope(&shared, defs.clone());
        assert_eq!((kept.len(), n), (2, 2));
        let (kept, n) = ctx.scope(&da, defs);
        assert_eq!((kept.iter().map(|(p, _)| *p).collect::<Vec<_>>(), n), (vec![da.as_str()], 0));
    }

    #[test]
    fn class_import_and_cycles_are_bounded() {
        let (a, b) = (format!("{J}A.java"), format!("{J}B.java"));
        let index = index(&[(&a, &java("@Import(B.class)", "A")), (&b, &java("@Import(A.class)", "B"))]);
        let ctx = build(&index);
        assert_eq!(ctx.member[&a], ctx.member[&b]);
        assert!(ctx.tainted.is_empty());
    }

    #[test]
    fn file_location_dynamic_and_profile_stay_quiet() {
        for imp in ["@ImportResource(\"file:/x.xml\")", "@ImportResource(\"classpath:${x}.xml\")", "@Import(Missing.class)", "@Profile(\"x\")", "@ComponentScan(basePackages = \"a.**\")", "@ComponentScans({@ComponentScan(\"a\")})", "@ComponentScan(basePackageClasses = A.Inner.class)", "@ImportResource(\"x.xml\")", "@ImportResource(\"classpath*:x.xml\")"] {
            let a = format!("{J}A.java");
            let index = index(&[(&a, &java(imp, "A"))]);
            let ctx = build(&index);
            assert!(!ctx.tainted.is_empty(), "{imp}");
        }
    }

    #[test]
    fn component_scan_and_import_resource_link_literal_targets() {
        let (a, c) = (format!("{J}A.java"), format!("{J}sub/C.java"));
        let src = format!("package a.sub;\nimport org.springframework.context.annotation.Configuration;\nimport org.springframework.context.annotation.Import;\nimport org.springframework.context.annotation.ImportResource;\nimport org.springframework.context.annotation.ComponentScan;\nimport org.springframework.context.annotation.Profile;\n@Configuration\npublic class C {{}}\n");
        let x = format!("{R}x.xml");
        let index = index(&[(&a, &java("@ComponentScan(\"a.sub\")\n@ImportResource(\"classpath:x.xml\")", "A")), (&c, &src), (&x, &bean_xml("b"))]);
        let ctx = build(&index);
        assert!(ctx.tainted.is_empty());
        assert_eq!(ctx.member[&c], ctx.member[&a]);
        assert_eq!(ctx.member[&x], ctx.member[&a]);
        let _ = xml_import("a");
    }

    fn scan_fixture(annotation: &str) -> (Index<Record>, String, String, String) {
        let (a, sub, other) = (format!("{J}A.java"), format!("{J}sub/C.java"), format!("/p/src/main/java/z/D.java"));
        let cfg = |pkg: &str, name: &str| format!("package {pkg};\nimport org.springframework.context.annotation.Configuration;\nimport org.springframework.context.annotation.ComponentScan;\nimport org.springframework.boot.autoconfigure.SpringBootApplication;\n@Configuration\npublic class {name} {{}}\n");
        let head = format!("package a;\nimport org.springframework.context.annotation.Configuration;\nimport org.springframework.context.annotation.ComponentScan;\nimport org.springframework.boot.autoconfigure.SpringBootApplication;\n{annotation}\n@Configuration\npublic class A {{}}\n");
        (index(&[(&a, &head), (&sub, &cfg("a.sub", "C")), (&other, &cfg("z", "D"))]), a, sub, other)
    }

    // ComponentScanAnnotationParser: empty basePackages and basePackageClasses => package of the declaring class;
    // ClassPathScanningCandidateComponentProvider scans "<pkg>/**/*.class", i.e. subpackages too.
    // SpringBootApplication meta-annotates @ComponentScan and aliases scanBasePackages/scanBasePackageClasses to it.
    #[test]
    fn bare_scan_covers_declaring_package_and_subpackages_only() {
        for annotation in ["@ComponentScan", "@SpringBootApplication"] {
            let (index, a, sub, other) = scan_fixture(annotation);
            let ctx = build(&index);
            assert!(ctx.tainted.is_empty(), "{annotation}");
            assert_eq!(ctx.member[&sub], ctx.member[&a], "{annotation}");
            assert!(!ctx.member.contains_key(&other), "{annotation}");
        }
    }

    #[test]
    fn explicit_scan_attributes_replace_the_declaring_package() {
        for (annotation, covers) in [
            ("@SpringBootApplication(scanBasePackages = \"z\")", "z"),
            ("@SpringBootApplication(scanBasePackageClasses = z.D.class)", "z"),
            ("@ComponentScan(basePackageClasses = a.sub.C.class)", "a.sub"),
            ("@ComponentScan(value = {\"z\", \"a.sub\"})", "both"),
            ("@ComponentScan(basePackages = \"z; a.sub\")", "both"),
        ] {
            let (index, a, sub, other) = scan_fixture(annotation);
            let ctx = build(&index);
            assert!(ctx.tainted.is_empty(), "{annotation}");
            assert_eq!(ctx.member.contains_key(&sub) && ctx.member[&sub] == ctx.member[&a], covers != "z", "{annotation}");
            assert_eq!(ctx.member.contains_key(&other) && ctx.member[&other] == ctx.member[&a], covers != "a.sub", "{annotation}");
        }
    }

    #[test]
    fn import_selectors_and_registrars_are_unprovable() {
        let (a, b) = (format!("{J}A.java"), format!("{J}B.java"));
        let selector = "package a;\nimport org.springframework.context.annotation.Configuration;\nimport org.springframework.context.annotation.ImportSelector;\n@Configuration\npublic class B implements ImportSelector { public String[] selectImports(Object m) { return null; } }\n";
        let index = index(&[(&a, &java("@Import(B.class)", "A")), (&b, selector)]);
        assert!(!build(&index).tainted.is_empty());
    }
}
