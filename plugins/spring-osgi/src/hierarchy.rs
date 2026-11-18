//! Bounded project-source hierarchy. Unknown/ambiguous edges never prove absence.
use super::*;
use std::collections::{BTreeMap, BTreeSet};
const MAX_CLASSES: usize = 4096;
const MAX_ANCESTORS: usize = 128;
const MAX_EDGES: usize = 512;
pub(super) struct Graph<'a> {
    nodes: BTreeMap<&'a str, Vec<(&'a str, &'a Class)>>,
    complete: bool,
    limited: bool,
}
pub(super) struct Report {
    pub types: Vec<String>,
    pub unknown: Vec<String>,
    pub ambiguous: Vec<String>,
    pub cycle: bool,
    pub limited: bool,
    pub complete: bool,
}
impl<'a> Graph<'a> {
    pub fn new(index: &'a impl Facts) -> Self {
        let mut nodes: BTreeMap<&str, Vec<(&str, &Class)>> = BTreeMap::new();
        let mut count = 0;
        let mut complete = index.fresh();
        let mut limited = false;
        'files: for (path, info) in index.iter() {
            for class in &info.classes {
                if count >= MAX_CLASSES {
                    complete = false;
                    limited = true;
                    break 'files;
                }
                nodes.entry(&class.fqn).or_default().push((path, class));
                count += 1;
            }
        }
        Self { nodes, complete, limited }
    }
    pub fn class(&self, name: &str) -> Option<(&'a str, &'a Class)> {
        let rows = self.nodes.get(name)?;
        (rows.len() == 1).then(|| rows[0])
    }
    pub fn report(&self, name: &str) -> Report {
        let mut types = Vec::new();
        let mut unknown = BTreeSet::new();
        let mut ambiguous = BTreeSet::new();
        let mut active = BTreeSet::new();
        let mut finished = BTreeSet::new();
        let mut todo = vec![(name.to_owned(), false)];
        let mut edges = 0;
        let mut cycle = false;
        let mut limited = self.limited;
        while let Some((next, exit)) = todo.pop() {
            if exit {
                active.remove(&next);
                finished.insert(next);
                continue;
            }
            if active.contains(&next) {
                cycle = true;
                continue;
            }
            if finished.contains(&next) {
                continue;
            }
            if types.len() >= MAX_ANCESTORS || edges >= MAX_EDGES {
                limited = true;
                break;
            }
            active.insert(next.clone());
            types.push(next.clone());
            todo.push((next.clone(), true));
            match self.nodes.get(next.as_str()) {
                Some(rows) if rows.len() == 1 => {
                    let class = rows[0].1;
                    unknown.extend(class.unknown_supers.iter().cloned());
                    for parent in class.supers.iter().rev() {
                        edges += 1;
                        if edges > MAX_EDGES {
                            limited = true;
                            break;
                        }
                        todo.push((parent.clone(), false));
                    }
                }
                Some(_) => {
                    ambiguous.insert(next);
                }
                None if next == "java.lang.Object" => {}
                None => {
                    unknown.insert(next);
                }
            }
        }
        let complete = self.complete && !cycle && !limited && unknown.is_empty() && ambiguous.is_empty();
        Report { types, unknown: unknown.into_iter().collect(), ambiguous: ambiguous.into_iter().collect(), cycle, limited, complete }
    }
    pub fn methods(&self, name: &str, method: Option<&str>) -> Vec<(&'a str, &'a Method)> {
        self.report(name)
            .types
            .iter()
            .filter_map(|name| self.class(name))
            .flat_map(|(path, class)| class.methods.iter().map(move |m| (path, m)))
            .filter(|(_, m)| method.is_none_or(|name| m.name == name))
            .collect()
    }
}

/// Resolve only lexical source declarations and explicit imports. Missing package candidates
/// remain unknown in Graph; wildcard imports never choose an arbitrary external package.
pub(super) fn qualified(file: &java::JavaFile, owner: usize, ty: &java::TypeRef) -> Result<String, String> {
    let head: String = ty.text.split('<').next().unwrap_or("").split_whitespace().collect();
    if head.is_empty() {
        return Err("incomplete supertype".into());
    }
    let (first, tail) = head.split_once('.').map(|(a, b)| (a, Some(b))).unwrap_or((&head, None));
    if tail.is_some() && first.starts_with(char::is_lowercase) {
        return Ok(head.replace('$', "."));
    }
    let imports: BTreeSet<_> =
        file.imports.iter().filter(|i| !i.is_static && i.path.rsplit('.').next() == Some(first)).map(|i| i.path.as_str()).collect();
    if imports.len() > 1 {
        return Err(format!("{head} (conflicting explicit imports)"));
    }
    let mut scopes = vec![Some(owner)];
    let mut scope = file.types[owner].outer;
    while let Some(i) = scope {
        scopes.push(Some(i));
        scope = file.types[i].outer;
    }
    scopes.push(None);
    let mut local = None;
    for scope in scopes {
        let hits: Vec<_> = file
            .types
            .iter()
            .enumerate()
            .filter(|(i, t)| t.name == first && (Some(*i) == scope || t.outer == scope))
            .map(|(i, _)| file.qualified(i))
            .collect();
        if hits.len() > 1 {
            return Err(format!("{head} (ambiguous lexical declarations)"));
        }
        if let Some(hit) = hits.first() {
            local = Some(hit.clone());
            break;
        }
    }
    if let Some(local) = &local
        && imports.iter().any(|import| **import != *local)
    {
        return Err(format!("{head} (source/import name conflict)"));
    }
    let base = if let Some(local) = local {
        local
    } else if let Some(import) = imports.first() {
        (*import).to_owned()
    } else {
        if file.imports.iter().any(|i| i.is_static && (i.path.ends_with(".*") || i.path.rsplit('.').next() == Some(first))) {
            return Err(format!("{head} (unresolved static type import)"));
        }
        if file.package.is_empty() { first.to_owned() } else { format!("{}.{first}", file.package) }
    };
    Ok(tail.map_or(base.clone(), |tail| format!("{base}.{tail}")))
}

fn literal(value: &str) -> String {
    value.chars().take(256).map(|c| if c == '`' || c.is_control() { ' ' } else { c }).collect()
}
impl openidea_plugin::hover::HoverProvider for SpringOsgi {
    fn hover(&mut self, doc: &Document, at: Position) -> Option<openidea_plugin::hover::Hover> {
        if doc.text.len() > 512 * 1024 || doc.language != "xml" || !doc.path.ends_with(".xml") {
            return None;
        }
        xml::analysis_valid(&doc.text).then_some(())?;
        let osgi = OsgiXml::new(&doc.text)?;
        if osgi.els.len() > 2048 {
            return None;
        }
        let offset = offset(&doc.text, at)?;
        let scope=self.source_scope(&doc.path, &doc.text);
        let mut local = Index::new(GLOBS, parse);
        let index = match self.indexes.for_cached_document(&doc.path,&doc.text) {
            Some(index)=>index,
            None=>{local.update(&doc.path,&doc.text);&mut local}
        };
        let index=ownership::Scoped::new(index,&scope);
        let (target, value, from, to) = target_at(&osgi, &index, offset)?;
        let owner = match target {
            Target::Class => value.replace('$', "."),
            Target::Bean => {
                let count = named(&index, &value).take(2).count();
                if count > 1 {
                    return Some(
                        openidea_plugin::hover::Hover::new(format!(
                            "Bean `{}` has multiple cached source declarations. Its hierarchy owner is unresolved.",
                            literal(&value)
                        ))
                        .range(range(&doc.text, from, to)),
                    );
                }
                bean_class(&index, &value)?
            }
            Target::Method(Some(owner), _) => owner,
            Target::Method(None, _) => return None,
        };
        let graph = Graph::new(&index);
        let report = graph.report(&owner);
        let mut markdown = format!(
            "Cached source hierarchy for `{}`

{}

",
            literal(&owner),
            if report.complete {
                "Visited hierarchy edges are complete within the source model; java.lang.Object is a known terminal."
            } else {
                "Source hierarchy is incomplete; unresolved edges do not prove missing interfaces or methods."
            }
        );
        if graph.class(&owner).is_some() {
            let types = report
                .types
                .iter()
                .filter(|name| graph.class(name).is_some())
                .take(32)
                .map(|name| format!("- `{}`", literal(name)))
                .collect::<Vec<_>>()
                .join(
                    "
",
                );
            markdown.push_str(&format!(
                "Project declarations:

{types}

"
            ));
        }
        if !report.unknown.is_empty() {
            markdown.push_str(&format!(
                "Uncached/external or unresolved source candidates: {}.

",
                report.unknown.iter().take(16).map(|s| format!("`{}`", literal(s))).collect::<Vec<_>>().join(", ")
            ));
        }
        if !report.ambiguous.is_empty() {
            markdown.push_str(&format!(
                "Ambiguous cached class identities: {}.

",
                report.ambiguous.iter().take(16).map(|s| format!("`{}`", literal(s))).collect::<Vec<_>>().join(", ")
            ));
        }
        if report.cycle {
            markdown.push_str(
                "A source inheritance cycle was detected.

",
            );
        }
        if report.limited {
            markdown.push_str(
                "Hierarchy traversal reached its bound.

",
            );
        }
        markdown.push_str("Project source facts only; classpath, effective Spring bean types and runtime service types are not resolved.");
        Some(openidea_plugin::hover::Hover::new(markdown).range(range(&doc.text, from, to)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::hover::HoverProvider;
    use openidea_plugin::mock::MockHost;
    use std::time::Duration;
    fn index(files: &[(&str, &str)]) -> (MockHost, Index<Info>) {
        let host = MockHost::install();
        for (path, text) in files {
            host.add_file(path, text);
        }
        let project = host::projects().unwrap().remove(0);
        let mut index = Index::new(GLOBS, parse);
        index.scan(&project, Duration::from_secs(1));
        (host, index)
    }
    fn document(body: &str) -> Document {
        Document {
            path: "/project/context.xml".into(),
            language: "xml".into(),
            version: 4,
            text: format!(r#"<beans xmlns="http://www.springframework.org/schema/beans" xmlns:o="{OSGI}">{body}</beans>"#),
        }
    }
    #[test]
    fn qualified_imports_diamond_and_inherited_methods_are_source_complete() {
        let (_host, index) = index(&[
            ("Api.java", "package api; public interface Api {void bound(Object s, java.util.Map p);}"),
            ("Base.java", "package app; import api.Api; interface Base extends Api {}"),
            ("Other.java", "package app; import api.Api; interface Other extends Api {}"),
            ("Impl.java", "package app; class Impl implements Base, Other {}"),
        ]);
        let graph = Graph::new(&index);
        let report = graph.report("app.Impl");
        assert!(report.complete);
        assert!(!report.cycle);
        assert_eq!(report.types.iter().filter(|name| *name == "api.Api").count(), 1);
        assert_eq!(graph.methods("app.Impl", Some("bound")).len(), 1);
    }
    #[test]
    fn nested_source_scopes_resolve_nearest_owner_and_qualified_imported_outer() {
        let (_host, index) = index(&[
            (
                "Outers.java",
                "package app; class First {interface Api {} class Impl implements Api {}} class Second {interface Api {} class Impl implements Api {}}",
            ),
            ("Outer.java", "package api; class Outer {public interface Api {}}"),
            ("Imported.java", "package app; import api.Outer; class Imported implements Outer.Api {}"),
        ]);
        let graph = Graph::new(&index);
        for (owner, expected, other) in [
            ("app.First.Impl", "app.First.Api", "app.Second.Api"),
            ("app.Second.Impl", "app.Second.Api", "app.First.Api"),
            ("app.Imported", "api.Outer.Api", "app.Outer.Api"),
        ] {
            let report = graph.report(owner);
            assert!(report.complete, "{owner}: {:?}", report.unknown);
            assert!(report.types.iter().any(|name| name == expected));
            assert!(!report.types.iter().any(|name| name == other));
        }
    }
    #[test]
    fn cycles_duplicate_fqns_and_conflicting_imports_never_prove_absence() {
        let (_host, mut index) = index(&[
            ("A.java", "package app; class A extends B {}"),
            ("B.java", "package app; class B extends A {}"),
            ("D1.java", "package app; class Duplicate {}"),
            ("D2.java", "package app; class Duplicate implements A {}"),
            ("Ambiguous.java", "package app; import a.Api; import b.Api; class Ambiguous implements Api {}"),
        ]);
        let graph = Graph::new(&index);
        let cycle = graph.report("app.A");
        assert!(cycle.cycle);
        assert!(!cycle.complete);
        let duplicate = graph.report("app.Duplicate");
        assert_eq!(duplicate.ambiguous, ["app.Duplicate"]);
        assert!(!duplicate.complete);
        assert!(graph.class("app.Duplicate").is_none());
        let ambiguous = graph.report("app.Ambiguous");
        assert!(ambiguous.unknown.iter().any(|name| name.contains("conflicting")));
        assert!(!ambiguous.complete);
        let doc = document(
            r#"<bean id="a" class="app.A"/><o:service ref="a" interface="other.Api"/><bean id="d" class="app.Duplicate"/><o:service ref="d" interface="other.Api"/>"#,
        );
        index.update(&doc.path, &doc.text);
        let osgi = OsgiXml::new(&doc.text).unwrap();
        assert!(check(&osgi, &index).is_empty());
    }
    #[test]
    fn unknown_external_or_wildcard_edges_and_duplicate_bean_owners_stay_quiet() {
        let (_host, mut index) = index(&[
            ("External.java", "package app; import vendor.*; class External extends Base {public String bound(int x){return null;}}"),
            ("A.java", "package app; class A {}"),
            ("B.java", "package app; class B {}"),
        ]);
        let graph = Graph::new(&index);
        let report = graph.report("app.External");
        assert!(!report.complete);
        assert_eq!(report.unknown, ["app.Base"]);
        let doc = document(
            r#"<bean id="same" class="app.A"/><bean id="same" class="app.B"/><o:service ref="same" interface="other.Api"/><bean id="external" class="app.External"/><o:reference interface="other.Api"><o:listener ref="external" bind-method="bound"/></o:reference>"#,
        );
        index.update(&doc.path, &doc.text);
        assert!(bean_class(&index, "same").is_none());
        assert!(check(&OsgiXml::new(&doc.text).unwrap(), &index).is_empty());
    }
    #[test]
    fn traversal_bound_and_malformed_source_are_incomplete() {
        let host = MockHost::install();
        let mut index = Index::new(&["**/*.java"], parse);
        for n in 0..140 {
            let source = format!("package app; class A{n} extends A{} {{}}", n + 1);
            host.add_file(&format!("A{n}.java"), &source);
        }
        index.scan(&host::projects().unwrap().remove(0), Duration::from_secs(1));
        let report = Graph::new(&index).report("app.A0");
        assert!(report.limited);
        assert!(!report.complete);
        assert!(report.types.len() <= MAX_ANCESTORS);
        index.update("/project/Broken.java", "package app; class Broken {");
        assert!(!Graph::new(&index).report("app.A139").complete);
    }
    #[test]
    fn cached_hover_explains_unknowns_and_exact_utf16_span_without_discovery() {
        let (host, index) = index(&[("External.java", "package app; class External extends vendor.Base {}")]);
        let mut plugin = SpringOsgi::default();
        let project = host::projects().unwrap().remove(0);
        plugin.indexes.get(&project, Duration::from_secs(1));
        let doc = document("<!-- 😀 --><o:reference interface=\"app.External\"/>");
        let from = doc.text.find("app.External").unwrap();
        let at = openidea_plugin::text::position(&doc.text, from);
        let hover = plugin.hover(&doc, at).unwrap();
        assert_eq!(hover.range.unwrap(), range(&doc.text, from, from + "app.External".len()));
        assert!(hover.markdown.contains("vendor.Base"));
        assert!(hover.markdown.contains("incomplete"));
        let cold = SpringOsgi::default().hover(&doc, at).unwrap();
        assert!(cold.markdown.contains("incomplete"));
        assert!(host.diagnostics().is_empty());
        drop(index);
    }
    #[test]
    fn cached_hierarchies_are_scoped_to_the_owning_project() {
        let host = MockHost::install();
        host.add_project("/other");
        host.add_file("/project/Impl.java", "package app; class Impl implements Api {} interface Api {}");
        host.add_file("/other/Impl.java", "package app; class Impl {}");
        host.add_file("/other/Api.java", "package app; interface Api {}");
        let mut plugin = SpringOsgi::default();
        let projects = host::projects().unwrap();
        plugin.indexes.get(&projects[0], Duration::from_secs(1));
        let mut other = document(r#"<bean id="impl" class="app.Impl"/><o:service ref="impl" interface="app.Api"/>"#);
        other.path = "/other/context.xml".into();
        plugin.indexes.get(&projects[1], Duration::from_secs(1));
        assert!(
            plugin.diagnostics(&other).iter().any(|d| d.code.as_deref() == Some("osgi-service-common")),
            "the owning root's plain source class does not implement Api"
        );
    }
    #[test]
    fn opening_nested_project_removes_cached_positive_class_and_hierarchy_authority() {
        let host=MockHost::install();
        host.add_file("nested/Impl.java","package app; class Impl {} ");
        host.add_file("Api.java","package app; interface Api {} ");
        let mut plugin=SpringOsgi::default();
        let d=document("<o:reference interface='app.Impl'/>");
        let at=openidea_plugin::text::position(&d.text,d.text.find("app.Impl").unwrap()+4);
        assert_eq!(plugin.definition(&d,at).len(),1);
        let service=document("<bean id='impl' class='app.Impl'/><o:service ref='impl' interface='app.Api'/>");
        assert!(plugin.diagnostics(&service).iter().any(|d|d.code.as_deref()==Some("osgi-service-common")));
        host.add_project("/project/nested");
        assert!(plugin.definition(&d,at).is_empty());
        assert!(plugin.completion(&d,at,None).iter().all(|item|item.label!="app.Impl"));
        assert!(plugin.diagnostics(&service).iter().all(|d|d.code.as_deref()!=Some("osgi-service-common")));
        let hover=plugin.hover(&d,at).unwrap();
        assert!(!hover.markdown.contains("Project declarations:")); assert!(hover.markdown.contains("Uncached/external"));
    }
    #[test]
    fn root_inventory_changes_and_denial_cold_cache_until_explicit_navigation_warms_it() {
        let host=MockHost::install(); host.add_file("Impl.java","package app; class Impl {} ");
        let mut plugin=SpringOsgi::default(); let d=document("<o:reference interface='app.Impl'/>");
        let at=openidea_plugin::text::position(&d.text,d.text.find("app.Impl").unwrap()+4);
        assert_eq!(plugin.definition(&d,at).len(),1);
        host.add_project("/unrelated"); host.add_file("Impl.java","package app; class Impl extends vendor.Base {} ");
        assert!(!plugin.hover(&d,at).unwrap().markdown.contains("Project declarations:"),"provider does not reread after root changes");
        assert_eq!(plugin.definition(&d,at).len(),1);
        assert!(plugin.hover(&d,at).unwrap().markdown.contains("vendor.Base"));
        let host=host.with_capabilities(&[]);
        assert!(!plugin.hover(&d,at).unwrap().markdown.contains("Project declarations:"));
        let local=document("<o:service auto-export='wrong'/>");
        assert!(plugin.diagnostics(&local).iter().any(|d|d.code.as_deref()==Some("osgi-attribute-value")));
        let host=host.with_capabilities(&["project.read","editor.read","editor.write"]);
        assert!(!plugin.hover(&d,at).unwrap().markdown.contains("Project declarations:"),"restoring a grant does not revive discarded old facts");
        assert_eq!(plugin.definition(&d,at).len(),1);
        host.set_project_root(None); assert!(plugin.definition(&d,at).is_empty());
        host.set_project_root(Some("/project"));
        assert!(!plugin.hover(&d,at).unwrap().markdown.contains("Project declarations:"));
        assert!(host.process_runs().is_empty() && host.requests().is_empty());
    }
    #[test]
    fn initial_nested_scope_filters_bean_class_and_method_candidates_and_caps_roots() {
        let host=MockHost::install(); host.add_project("/project/nested");
        host.add_file("Outer.java","package app; class Outer {public void bound(Object service, java.util.Map properties) {}}");
        host.add_file("nested/Outer.java","package app; class Outer {public void wrong(Object service, java.util.Map properties) {}}");
        host.add_file("outer.xml",&format!("<beans xmlns='{BEANS}'><bean id='outer' class='app.Outer'/></beans>"));
        host.add_file("nested/beans.xml",&format!("<beans xmlns='{BEANS}'><bean id='nestedBean' class='app.Outer'/></beans>"));
        let mut plugin=SpringOsgi::default();
        let d=document("<o:service ref=''/>"); let at=openidea_plugin::text::position(&d.text,d.text.find("ref='").unwrap()+5);
        let items=plugin.completion(&d,at,None); assert!(items.iter().any(|i|i.label=="outer")); assert!(items.iter().all(|i|i.label!="nestedBean"));
        let d=document("<o:reference interface='external.Api'><o:listener ref='outer' bind-method='b'/></o:reference>");
        let at=openidea_plugin::text::position(&d.text,d.text.find("bind-method='b").unwrap()+13);
        let items=plugin.completion(&d,at,None); assert!(items.iter().any(|i|i.label=="bound")); assert!(items.iter().all(|i|i.label!="wrong"));
        let d=document("<o:reference interface='app.Outer'/>"); let at=openidea_plugin::text::position(&d.text,d.text.find("app.Outer").unwrap()+4);
        let target=plugin.definition(&d,at); assert_eq!(target.len(),1); assert_eq!(target[0].path,"/project/Outer.java");
        assert!(plugin.hover(&d,at).unwrap().markdown.contains("Visited hierarchy edges are complete"),"known foreign owners do not poison an established outer source hierarchy");
        for i in 0..65 {host.add_project(&format!("/other-{i}"));}
        assert!(plugin.definition(&d,at).is_empty()); assert!(!plugin.hover(&d,at).unwrap().markdown.contains("Project declarations:"));
    }
    #[test]
    fn implicit_object_is_known_but_record_library_ancestry_is_not_invented() {
        let (_host, mut index) =
            index(&[("Plain.java", "package app; class Plain {}"), ("Row.java", "package app; record Row(String value) {}")]);
        let graph = Graph::new(&index);
        let plain = graph.report("app.Plain");
        assert!(plain.complete);
        assert!(plain.types.iter().any(|name| name == "java.lang.Object"));
        assert!(!graph.report("app.Row").complete);
        let doc = document(r#"<bean id="plain" class="app.Plain"/><o:service ref="plain" interface="java.lang.Object"/>"#);
        index.update(&doc.path, &doc.text);
        assert!(check(&OsgiXml::new(&doc.text).unwrap(), &index).is_empty());
    }
    #[test]
    fn external_parameter_types_are_unknown_and_known_qualified_listener_types_remain_supported() {
        let (_host, mut index) = index(&[
            ("Listener.java", "package app; import vendor.ServiceReference; class Listener {public void bound(ServiceReference ref){}}"),
            (
                "Known.java",
                "package app; import org.osgi.framework.ServiceReference; class Known {public void bound(ServiceReference ref){}}",
            ),
            ("Wrong.java", "package app; class Wrong {public void bound(int ref){}}"),
        ]);
        let unknown = Graph::new(&index).methods("app.Listener", Some("bound"));
        assert_eq!(proper_parameters(unknown[0].1, false), None);
        let known = Graph::new(&index).methods("app.Known", Some("bound"));
        assert_eq!(proper_parameters(known[0].1, false), Some(true));
        let doc = document(
            r#"<bean id="listener" class="app.Listener"/><o:reference interface="external.Api"><o:listener ref="listener" bind-method="bound" unbind-method="bound"/></o:reference>"#,
        );
        index.update(&doc.path, &doc.text);
        assert!(check(&OsgiXml::new(&doc.text).unwrap(), &index).is_empty());
        let wrong = doc.text.replace("app.Listener", "app.Wrong");
        index.update(&doc.path, &wrong);
        assert!(check(&OsgiXml::new(&wrong).unwrap(), &index).iter().any(|d| d.message == PARAMETERS));
    }
}
