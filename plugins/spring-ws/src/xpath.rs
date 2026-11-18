//! Source-owned namespace declarations and absolute XPath root element references.
//! This is not XPath evaluation or an effective Spring context/type model.
use crate::*;
use spring_text::java::Annotation;
const PREFIX: &str = "org.springframework.ws.server.endpoint.annotation.";
#[derive(Clone)]
struct Binding {
    prefix: String,
    uri: String,
    uri_range: Range,
    prefix_range: Option<Range>,
    path: Option<String>,
}
fn literal<'a>(a: &'a Annotation, name: &str, text: &str) -> Option<&'a Str> {
    if a.args.iter().filter(|a| a.name == name).count() != 1 {
        return None;
    }
    let arg = a.arg(name)?;
    let [s] = arg.strings.as_slice() else {
        return None;
    };
    let tokens = java::tokens(&arg.text);
    if tokens.len() != 1
        || !tokens[0].s.starts_with('"')
        || tokens[0].s.starts_with("\"\"\"")
        || text.get(s.at..s.end) != Some(s.value.as_str())
        || s.value.contains(['\\', '$', '&'])
    {
        return None;
    }
    Some(s)
}
fn bindings(
    annotations: &[Annotation],
    owners: &annotation_owners::Owners,
    text: &str,
    kotlin: bool,
) -> Option<Vec<Binding>> {
    let mut declared = vec![];
    for a in annotations
        .iter()
        .filter(|a| a.name == "Namespaces")
        .chain(annotations.iter().filter(|a| a.name == "Namespace"))
    {
        if a.name == "Namespace" && owners.known(text, a, &format!("{PREFIX}Namespace")) {
            declared.push(a.clone());
        } else if a.name == "Namespaces" && owners.known(text, a, &format!("{PREFIX}Namespaces")) {
            let Some(arg) = a.arg("value") else {
                return None;
            };
            if a.args.len() != 1 {
                return None;
            }
            let mut tokens = java::tokens(text.get(arg.at..arg.end)?);
            if tokens.first().is_some_and(|t| matches!(t.s, "{" | "[")) {
                let closing = if tokens[0].s == "{" { "}" } else { "]" };
                if tokens.last().is_none_or(|t| t.s != closing) {
                    return None;
                }
                tokens = tokens[1..tokens.len() - 1].to_vec();
            }
            // Arg token offsets above are relative to the physical argument.
            for t in &mut tokens {
                t.at += arg.at;
            }
            let mut at = 0;
            while at < tokens.len() {
                if tokens[at].s == "," {
                    at += 1;
                    continue;
                }
                let synthetic = kotlin && tokens[at].s != "@";
                let mut rest = tokens[at..].to_vec();
                if synthetic {
                    rest.insert(
                        0,
                        java::Tok {
                            s: "@",
                            at: tokens[at].at,
                        },
                    );
                }
                let mut parsed = vec![];
                let end = java::annotation(text, &rest, 0, &mut parsed);
                if parsed.len() != 1
                    || end == 0
                    || parsed[0].name != "Namespace"
                    || !owners.known(text, &parsed[0], &format!("{PREFIX}Namespace"))
                {
                    return None;
                }
                at += end - usize::from(synthetic);
                declared.extend(parsed);
                if at < tokens.len() && tokens[at].s != "," {
                    return None;
                }
            }
        }
    }
    let mut result = std::collections::BTreeMap::new();
    for a in declared {
        let prefix = if a.arg("prefix").is_some() {
            literal(&a, "prefix", text)?.value.clone()
        } else {
            String::new()
        };
        let uri = literal(&a, "uri", text)?;
        if (!prefix.is_empty() && !schema_qnames::local(&prefix))
            || matches!(prefix.as_str(), "xml" | "xmlns")
            || uri.value.len() > 1024
            || uri
                .value
                .chars()
                .any(|c| c.is_control() || c.is_whitespace())
        {
            return None;
        }
        result.insert(
            prefix.clone(),
            Binding {
                prefix,
                uri: uri.value.clone(),
                uri_range: range(text, uri.at, uri.end),
                prefix_range: a
                    .arg("prefix")
                    .and_then(|_| literal(&a, "prefix", text))
                    .map(|p| range(text, p.at, p.end)),
                path: None,
            },
        );
    }
    Some(result.into_values().collect())
}
fn source(doc: &Document) -> Option<JavaFile> {
    if !matches!(doc.language.as_str(), "java" | "kotlin") || doc.text.len() > MAX_SCHEMA_BYTES {
        return None;
    }
    let tokens = java::tokens(&doc.text);
    let mut stack = vec![];
    if tokens.len() > 32768 {
        return None;
    }
    for t in tokens {
        match t.s {
            "(" => stack.push(")"),
            "{" => stack.push("}"),
            "[" => stack.push("]"),
            ")" | "}" | "]" => {
                if stack.pop() != Some(t.s) {
                    return None;
                }
            }
            _ => {}
        }
    }
    if !stack.is_empty() {
        return None;
    }
    Some(parse_source(&doc.path, &doc.text))
}
pub(crate) fn namespace_sites(doc: &Document) -> Vec<(String, Range)> {
    let Some(file) = source(doc) else {
        return vec![];
    };
    let owners = annotation_owners::Owners::new(&file, &doc.text, doc.language == "kotlin");
    let mut out = bindings(&file.package_annotations, &owners, &doc.text, false)
        .unwrap_or_default()
        .into_iter()
        .map(|b| (b.uri, b.uri_range))
        .collect::<Vec<_>>();
    for ty in &file.types {
        for annotations in
            std::iter::once(&ty.annotations).chain(ty.methods().map(|m| &m.annotations))
        {
            if let Some(bindings) =
                bindings(annotations, &owners, &doc.text, doc.language == "kotlin")
            {
                out.extend(bindings.into_iter().map(|b| (b.uri, b.uri_range)));
            }
        }
    }
    out
}
#[derive(Clone)]
pub(crate) struct PackageNamespaces {
    package: String,
    bindings: Option<Vec<Binding>>,
}
pub(crate) fn scan_package(path: &str, text: &str) -> Option<PackageNamespaces> {
    if !path.ends_with("/package-info.java") && path != "package-info.java" {
        return None;
    }
    let doc = Document {
        path: path.into(),
        text: text.into(),
        language: "java".into(),
        version: 0,
    };
    let Some(file) = source(&doc) else {
        return Some(PackageNamespaces { package: String::new(), bindings: None });
    };
    if file.package.is_empty() || !file.types.is_empty() {
        return Some(PackageNamespaces { package: String::new(), bindings: None });
    }
    let owners = annotation_owners::Owners::new(&file, text, false);
    Some(PackageNamespaces {
        package: file.package,
        bindings: bindings(&file.package_annotations, &owners, text, false).map(|mut values| {
            for binding in &mut values {
                binding.path = Some(path.into());
            }
            values
        }),
    })
}
fn cached_package(nav: &mut WsTools, doc: &Document) -> Vec<Binding> {
    (|| {
        if !nav.watching
            || !matches!(doc.language.as_str(), "java" | "kotlin")
            || doc.path.contains('\\')
            || doc.path.split('/').any(|p| matches!(p, "." | ".."))
        {
            return None;
        }
        let root = project_of(&doc.path)?.root().ok()?;
        let directory = doc.path.rsplit_once('/')?.0;
        let path = format!("{directory}/package-info.java");
        if project_of(&path)?.root().ok()? != root {
            return None;
        }
        let index = nav
            .xpath_packages
            .for_cached_document_warm(&doc.path, &doc.text, PROVIDER_BUDGET)?;
        if !index.fresh() {
            return None;
        }
        let fact = index.files().get(&path)?;
        if fact.package != parse_source(&doc.path, &doc.text).package {
            return None;
        }
        fact.bindings.clone()
    })()
    .unwrap_or_default()
}
pub(crate) fn cached_references(nav: &mut WsTools, doc: &Document) -> Vec<(String, String, Range)> {
    references_with_package(doc, &cached_package(nav, doc))
}
pub(crate) fn references_with_package_source(
    doc: &Document,
    path: &str,
    text: &str,
) -> Vec<(String, String, Range)> {
    let package = (|| {
        let directory = doc.path.rsplit_once('/')?.0;
        if path != format!("{directory}/package-info.java") {
            return None;
        }
        let fact = scan_package(path, text)?;
        if fact.package != parse_source(&doc.path, &doc.text).package {
            return None;
        }
        fact.bindings
    })()
    .unwrap_or_default();
    references_with_package(doc, &package)
}
pub(crate) fn references(doc: &Document) -> Vec<(String, String, Range)> {
    references_with_package(doc, &[])
}
fn references_with_package(doc: &Document, package: &[Binding]) -> Vec<(String, String, Range)> {
    if doc.language == "xml" {
        return xml_refs::payload_qnames(doc);
    }
    let Some(file) = source(doc) else {
        return vec![];
    };
    let owners = annotation_owners::Owners::new(&file, &doc.text, doc.language == "kotlin");
    let mut out = vec![];
    for ty in &file.types {
        let class = bindings(
            &ty.annotations,
            &owners,
            &doc.text,
            doc.language == "kotlin",
        );
        for method in ty.methods() {
            let Some(own) = bindings(
                &method.annotations,
                &owners,
                &doc.text,
                doc.language == "kotlin",
            ) else {
                continue;
            };
            // Spring NamespaceUtils binds package, then class, then method;
            // method declarations override only their own prefixes.
            let mut bindings = if let Some(class) = &class {
                let mut values = package.to_vec();
                for binding in class {
                    values.retain(|b| b.prefix != binding.prefix);
                    values.push(binding.clone());
                }
                values
            } else {
                vec![]
            };
            for binding in own {
                bindings.retain(|b| b.prefix != binding.prefix);
                bindings.push(binding);
            }
            for param in &method.params {
                for a in &param.annotations {
                    if a.name != "XPathParam"
                        || !owners.known(&doc.text, a, &format!("{PREFIX}XPathParam"))
                    {
                        continue;
                    }
                    let Some(value) = literal(a, "value", &doc.text) else {
                        continue;
                    };
                    let Some(root) = value.value.strip_prefix('/') else {
                        continue;
                    };
                    let root = root.split('/').next().unwrap_or("");
                    let Some((prefix, local)) = root.split_once(':') else {
                        continue;
                    };
                    if !schema_qnames::local(prefix) || !schema_qnames::local(local) {
                        continue;
                    }
                    let Some(binding) = bindings.iter().find(|b| b.prefix == prefix) else {
                        continue;
                    };
                    let start = value.at + 1 + prefix.len() + 1;
                    out.push((
                        binding.uri.clone(),
                        local.into(),
                        range(&doc.text, start, start + local.len()),
                    ));
                }
            }
        }
    }
    out
}
const LEGACY_RESOLVER_MARKERS: [&str; 3] =
    ["XPathParamAnnotationMethodEndpointAdapter", "XPathParamMethodArgumentResolver", "NamespaceUtils"];
pub(crate) fn scan_resolver_customization(_path: &str, text: &str) -> Option<()> {
    LEGACY_RESOLVER_MARKERS.iter().any(|marker| text.contains(marker)).then_some(())
}
fn effective_bindings(package: &[Binding], class: &[Binding], own: Vec<Binding>) -> Vec<Binding> {
    let mut values = package.to_vec();
    for binding in class.iter().cloned().chain(own) {
        values.retain(|b| b.prefix != binding.prefix);
        values.push(binding);
    }
    values
}
/// Reports an `@XPathParam` prefix only when Spring-WS's `NamespaceUtils` (package, class, method
/// `@Namespace`/`@Namespaces`; `@Namespace` is not valid on parameters) is the sole source and every
/// project file was scanned: any custom/legacy adapter or resolver mention, unparseable or ambiguous
/// `package-info.java`, or non-literal declaration keeps this quiet. `xml`/`xmlns` are always bound.
pub(crate) fn undeclared_prefixes(nav: &mut WsTools, doc: &Document) -> Vec<Diagnostic> {
    let quiet = vec![];
    if !nav.watching || doc.path.contains('\\') {
        return quiet;
    }
    let Some(file) = source(doc) else {
        return quiet;
    };
    let Some(guards) = nav.source.for_cached_document_warm(&doc.path, &doc.text, DIAGNOSTICS_WARM) else {
        return quiet;
    };
    if !guards.fresh() || guards.iter().any(|(_, facts)| facts.legacy_resolver) {
        return quiet;
    }
    let Some(packages) = nav.xpath_packages.for_cached_document_warm(&doc.path, &doc.text, DIAGNOSTICS_WARM) else {
        return quiet;
    };
    if !packages.fresh() || packages.iter().any(|(_, fact)| fact.package.is_empty() && fact.bindings.is_none()) {
        return quiet;
    }
    let directory = doc.path.rsplit_once('/').map_or("", |(directory, _)| directory);
    let package_bindings = if file.package.is_empty() {
        vec![]
    } else {
        let same: Vec<_> = packages.iter().filter(|(_, fact)| fact.package == file.package).collect();
        match same.as_slice() {
            [] => vec![],
            [(path, fact)] if path.rsplit_once('/').map(|(d, _)| d) == Some(directory) => match &fact.bindings {
                Some(bindings) => bindings.clone(),
                None => return quiet,
            },
            _ => return quiet,
        }
    };
    let owners = annotation_owners::Owners::new(&file, &doc.text, doc.language == "kotlin");
    let kotlin = doc.language == "kotlin";
    let mut out = vec![];
    for ty in &file.types {
        let Some(class) = bindings(&ty.annotations, &owners, &doc.text, kotlin) else {
            continue;
        };
        for method in ty.methods() {
            let Some(own) = bindings(&method.annotations, &owners, &doc.text, kotlin) else {
                continue;
            };
            let active = effective_bindings(&package_bindings, &class, own);
            for a in method.params.iter().flat_map(|p| &p.annotations) {
                if a.name != "XPathParam" || !owners.known(&doc.text, a, &format!("{PREFIX}XPathParam")) {
                    continue;
                }
                let Some(value) = literal(a, "value", &doc.text) else {
                    continue;
                };
                if xpath_syntax::check(&value.value).is_err() {
                    continue;
                }
                for (prefix, start, end) in prefix_ranges(&value.value) {
                    if matches!(prefix.as_str(), "xml" | "xmlns") || active.iter().any(|b| b.prefix == prefix) {
                        continue;
                    }
                    out.push(
                        Diagnostic::new(
                            range(&doc.text, value.at + start, value.at + end),
                            Severity::Warning,
                            format!("XPath prefix `{prefix}` is not declared by @Namespace on the method, class or package."),
                        )
                        .code("undeclared-xpath-prefix"),
                    );
                }
            }
        }
    }
    out
}
fn syntax_problem(doc: &Document, at: usize, value: &str) -> Option<Diagnostic> {
    let (message, from, to) = xpath_syntax::check(value).err()?;
    let (start, end) = if to > from { (at + from, at + to) } else { (at.saturating_sub(1), at + value.len() + 1) };
    Some(
        Diagnostic::new(range(&doc.text, start, end), Severity::Warning, format!("Invalid XPath 1.0 expression: {message}."))
            .code("invalid-xpath"),
    )
}
/// Syntax errors in literal `@XPathParam` values and sole-literal `XPathExpressionFactory.createXPathExpression` arguments.
pub(crate) fn syntax_diagnostics(doc: &Document) -> Vec<Diagnostic> {
    let Some(file) = source(doc) else {
        return vec![];
    };
    let owners = annotation_owners::Owners::new(&file, &doc.text, doc.language == "kotlin");
    let mut out = vec![];
    for ty in &file.types {
        for method in ty.methods() {
            for a in method.params.iter().flat_map(|p| &p.annotations) {
                if a.name != "XPathParam" || !owners.known(&doc.text, a, &format!("{PREFIX}XPathParam")) {
                    continue;
                }
                if let Some(value) = literal(a, "value", &doc.text) {
                    out.extend(syntax_problem(doc, value.at, &value.value));
                }
            }
        }
    }
    let tokens = java::tokens(&doc.text);
    for w in tokens.windows(7) {
        let [factory, dot, name, open, lit, next, _] = [w[0], w[1], w[2], w[3], w[4], w[5], w[6]];
        let literal_text = lit.s.len() >= 2 && lit.s.starts_with('"') && !lit.s.starts_with("\"\"\"") && lit.s.ends_with('"');
        if factory.s != "XPathExpressionFactory"
            || dot.s != "."
            || name.s != "createXPathExpression"
            || open.s != "("
            || !literal_text
            || !matches!(next.s, ")" | ",")
            || lit.s.contains(['\\', '$'])
            || !owners.written_known("XPathExpressionFactory", "org.springframework.xml.xpath.XPathExpressionFactory")
        {
            continue;
        }
        out.extend(syntax_problem(doc, lit.at + 1, &lit.s[1..lit.s.len() - 1]));
    }
    out
}
pub(crate) fn definition(nav: &mut WsTools, doc: &Document, at: Position) -> Option<Vec<Location>> {
    let package = cached_package(nav, doc);
    if let Some(link) = prefix_links(doc, &package)
        .into_iter()
        .find(|link| contains(link.range, at))
    {
        return Some(link.target.into_iter().collect());
    }
    let (namespace, local, _) = cached_references(nav, doc)
        .into_iter()
        .find(|(_, _, r)| contains(*r, at))?;
    let Some(index) = nav
        .schemas
        .for_document(&doc.path, &doc.text, PROVIDER_BUDGET)
    else {
        return Some(vec![]);
    };
    Some(
        elements_for(index, Some(&namespace))
            .into_iter()
            .filter(|(_, n, _)| *n == local)
            .map(|(p, _, range)| Location {
                path: p.into(),
                range,
            })
            .collect(),
    )
}
pub(crate) fn completion(
    nav: &mut WsTools,
    doc: &Document,
    at: Position,
) -> Option<Vec<CompletionItem>> {
    let package = cached_package(nav, doc);
    if let Some(link) = prefix_links(doc, &package)
        .into_iter()
        .find(|link| link.range.start <= at && at <= link.range.end)
    {
        let start = openidea_plugin::text::offset(&doc.text, link.range.start)?;
        let caret = openidea_plugin::text::offset(&doc.text, at)?;
        let prefix = doc.text.get(start..caret)?;
        return Some(
            link.names
                .into_iter()
                .filter(|name| name.starts_with(prefix))
                .take(64)
                .map(|name| CompletionItem {
                    replace: Some(link.range),
                    detail: Some("Declared XPath namespace prefix".into()),
                    ..CompletionItem::new(&name, CompletionKind::Reference)
                })
                .collect(),
        );
    }
    let (namespace, _, span) = cached_references(nav, doc)
        .into_iter()
        .find(|(_, _, r)| r.start <= at && at <= r.end)?;
    let start = openidea_plugin::text::offset(&doc.text, span.start)?;
    let caret = openidea_plugin::text::offset(&doc.text, at)?;
    let prefix = doc.text.get(start..caret)?;
    let Some(index) = nav
        .schemas
        .for_document(&doc.path, &doc.text, PROVIDER_BUDGET)
    else {
        return Some(vec![]);
    };
    Some(
        elements_for(index, Some(&namespace))
            .into_iter()
            .filter(|(_, n, _)| n.starts_with(prefix))
            .take(64)
            .map(|(_, name, _)| CompletionItem {
                replace: Some(span),
                detail: Some(
                    "Global schema element matching the declared XPath root namespace".into(),
                ),
                ..CompletionItem::new(name, CompletionKind::Reference)
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{
        mock::MockHost,
        text::{offset, position},
    };
    const IMPORTS: &str = "import org.springframework.ws.server.endpoint.annotation.Namespace; import org.springframework.ws.server.endpoint.annotation.Namespaces; import org.springframework.ws.server.endpoint.annotation.XPathParam; ";
    fn doc(text: &str, kotlin: bool) -> Document {
        Document {
            path: if kotlin {
                "/project/E.kt"
            } else {
                "/project/E.java"
            }
            .into(),
            language: if kotlin { "kotlin" } else { "java" }.into(),
            text: text.into(),
            version: 1,
        }
    }
    fn written<'a>(d: &'a Document, r: Range) -> &'a str {
        &d.text[offset(&d.text, r.start).unwrap()..offset(&d.text, r.end).unwrap()]
    }
    #[test]
    fn java_namespaces_and_kotlin_container_bind_literal_absolute_root_qnames() {
        let java = format!(
            "{IMPORTS}@Namespaces({{@Namespace(prefix=\"p\",uri=\"urn:a\"),@Namespace(prefix=\"q\",uri=\"urn:b\")}}) class E {{ void handle(@XPathParam(\"/p:Request/text()\") String value) {{}} }}"
        );
        let d = doc(&java, false);
        let refs = references(&d);
        assert_eq!(refs.len(), 1);
        assert_eq!(
            (&refs[0].0, &refs[0].1),
            (&"urn:a".to_string(), &"Request".to_string())
        );
        assert_eq!(written(&d, refs[0].2), "Request");
        assert_eq!(namespace_sites(&d).len(), 2);
        let kotlin = "import org.springframework.ws.server.endpoint.annotation.Namespace\nimport org.springframework.ws.server.endpoint.annotation.Namespaces\nimport org.springframework.ws.server.endpoint.annotation.XPathParam\n@Namespaces(Namespace(prefix=\"p\",uri=\"urn:a\")) class E { fun handle(@XPathParam(\"/p:Request/text()\") value: String) {} }";
        assert_eq!(references(&doc(kotlin, true)).len(), 1);
    }
    #[test]
    fn method_namespace_scope_merges_class_and_overrides_each_declared_prefix() {
        let base = format!(
            "{IMPORTS}@Namespace(prefix=\"p\",uri=\"urn:a\") class E {{ @Namespace(prefix=\"p\",uri=\"urn:b\") void handle(@XPathParam(\"/p:Request\") String value) {{}} }}"
        );
        assert_eq!(references(&doc(&base, false))[0].0, "urn:b");
        let extra = base.replace(
            "@Namespace(prefix=\"p\",uri=\"urn:b\")",
            "@Namespace(prefix=\"q\",uri=\"urn:b\")",
        );
        assert_eq!(references(&doc(&extra, false))[0].0, "urn:a");
        let combined=base.replace("@Namespace(prefix=\"p\",uri=\"urn:b\")", "@Namespace(prefix=\"p\",uri=\"urn:direct\") @Namespaces(@Namespace(prefix=\"p\",uri=\"urn:container\"))");
        assert_eq!(references(&doc(&combined, false))[0].0, "urn:direct");
        let invalid=base.replace("@Namespace(prefix=\"p\",uri=\"urn:b\")", "@Namespaces({@Namespace(prefix=\"p\",uri=\"urn:b\"),@Namespace(prefix=\"p\",uri=\"urn:c\")})");
        assert_eq!(references(&doc(&invalid, false))[0].0, "urn:c");
        for changed in [
            base.replace("/p:Request", "//p:Request"),
            base.replace("/p:Request", "/p:Request[@id]"),
            base.replace("/p:Request", "/x:Request"),
            base.replace(
                "import org.springframework.ws.server.endpoint.annotation.Namespace;",
                "import foreign.Namespace;",
            ),
            base.replace("uri=\"urn:b\"", "uri=URI"),
            base.replace("/p:Request", "/p:Requ\\u0065st"),
        ] {
            assert!(references(&doc(&changed, false)).is_empty(), "{changed}");
        }
    }
    #[test]
    fn providers_link_root_and_namespace_to_warm_schema_with_exact_unsaved_ranges() {
        let h = MockHost::install();
        let schema = "<xs:schema xmlns:xs='http://www.w3.org/2001/XMLSchema' targetNamespace='urn:a'><xs:element name='Request'/><xs:element name='Response'/></xs:schema>";
        h.add_file("src/main/resources/a.xsd", schema);
        let mut nav = WsTools::default();
        nav.activate(&h.context("aritti.spring-ws")).unwrap();
        let text = format!(
            "{IMPORTS}/*😀*/\r\n@Namespace(prefix=\"p\",uri=\"urn:a\") class E {{ void handle(@XPathParam(\"/p:Request\") String value) {{}} }}"
        );
        let d = doc(&text, false);
        let at = position(&text, text.find("p:Request").unwrap() + 2);
        let definitions = nav.definition(&d, at);
        assert_eq!(definitions.len(), 1);
        assert!(definitions[0].path.ends_with("a.xsd"));
        let completions = nav.completion(&d, at, None);
        assert_eq!(completions.len(), 2);
        assert!(
            completions
                .iter()
                .all(|c| written(&d, c.replace.unwrap()) == "Request")
        );
        let uri = position(&text, text.find("urn:a").unwrap());
        assert_eq!(nav.definition(&d, uri).len(), 1);
        assert_eq!(nav.completion(&d, uri, None).len(), 1);
        let edges = indexing::index_source(&openidea_plugin::indexing::Document {
            path: d.path.clone(),
            text: d.text.clone(),
            symbols: vec![],
        });
        assert_eq!(edges.references.len(), 1);
        assert_eq!(edges.references[0].key, "spring-ws:qname:5:urn:a:Request");
        assert!(h.requests().is_empty() && h.process_runs().is_empty());
    }
}

#[cfg(test)]
mod xml_selector_tests {
    use super::*;
    use openidea_plugin::{
        mock::MockHost,
        text::{offset, position},
    };
    #[test]
    fn payload_root_interceptor_selectors_share_endpoint_qname_navigation_and_occurrences() {
        let h = MockHost::install();
        h.add_file("src/main/resources/a.xsd","<xs:schema xmlns:xs='http://www.w3.org/2001/XMLSchema' targetNamespace='urn:a'><xs:element name='Request'/></xs:schema>");
        let text = "<b:beans xmlns:b='http://www.springframework.org/schema/beans' xmlns:s='http://www.springframework.org/schema/web-services'><s:interceptors><s:payloadRoot namespaceUri='urn:a' localPart='Request'><b:ref bean='audit'/></s:payloadRoot></s:interceptors></b:beans>";
        let doc = Document {
            path: "/project/context.xml".into(),
            text: text.into(),
            language: "xml".into(),
            version: 1,
        };
        let mut nav = WsTools::default();
        nav.activate(&h.context("aritti.spring-ws")).unwrap();
        let at = position(text, text.find("Request").unwrap());
        assert_eq!(nav.definition(&doc, at).len(), 1);
        let values = nav.completion(&doc, at, None);
        assert_eq!(values.len(), 1);
        assert_eq!(values[0].label, "Request");
        let span = values[0].replace.unwrap();
        assert_eq!(
            &text[offset(text, span.start).unwrap()..offset(text, span.end).unwrap()],
            "Request"
        );
        let edges = indexing::index_source(&openidea_plugin::indexing::Document {
            path: doc.path.clone(),
            text: doc.text.clone(),
            symbols: vec![],
        });
        assert_eq!(edges.references.len(), 1);
        assert_eq!(edges.references[0].key, "spring-ws:qname:5:urn:a:Request");
        for invalid in [
            text.replace("namespaceUri='urn:a'", "namespaceUri='${ns}'"),
            text.replace("s:payloadRoot", "b:payloadRoot"),
            text.replace("s:interceptors", "b:interceptors"),
            text.replace("localPart='Request'", "localPart='p:Request'"),
        ] {
            assert!(
                references(&Document {
                    text: invalid,
                    ..doc.clone()
                })
                .is_empty()
            );
        }
    }
}

#[cfg(test)]
mod package_namespace_tests {
    use super::*;
    use openidea_plugin::{FilesChanged, mock::MockHost, text::position};
    const ENDPOINT: &str = "package com.acme; import org.springframework.ws.server.endpoint.annotation.XPathParam; class E { void handle(@XPathParam(\"/p:Request\") String input){} }";
    const PACKAGE: &str = "@Namespace(prefix=\"p\",uri=\"urn:a\") package com.acme; import org.springframework.ws.server.endpoint.annotation.Namespace;";
    fn document() -> Document {
        Document {
            path: "/project/src/main/java/com/acme/E.java".into(),
            text: ENDPOINT.into(),
            language: "java".into(),
            version: 1,
        }
    }
    fn setup() -> (MockHost, WsTools) {
        let h = MockHost::install();
        for (name, uri) in [("a", "urn:a"), ("b", "urn:b")] {
            h.add_file(&format!("src/main/resources/{name}.xsd"),&format!("<xs:schema xmlns:xs='http://www.w3.org/2001/XMLSchema' targetNamespace='{uri}'><xs:element name='Request'/></xs:schema>"));
        }
        h.add_file("src/main/java/com/acme/package-info.java", PACKAGE);
        let mut nav = WsTools::default();
        nav.activate(&h.context("aritti.spring-ws")).unwrap();
        (h, nav)
    }
    #[test]
    fn watched_adjacent_package_namespace_is_cache_only_and_replaced_or_withdrawn_on_change() {
        let (h, mut nav) = setup();
        let d = document();
        let at = position(ENDPOINT, ENDPOINT.find("p:Request").unwrap() + 2);
        assert!(nav.definition(&d, at)[0].path.ends_with("a.xsd"));
        assert_eq!(nav.completion(&d, at, None).len(), 1);
        let path = "/project/src/main/java/com/acme/package-info.java";
        h.add_file(
            path.trim_start_matches("/project/"),
            &PACKAGE.replace("urn:a", "urn:b"),
        );
        nav.handle_events(&[Event::FileSaved(path.into())], &h.invocation());
        assert!(nav.definition(&d, at)[0].path.ends_with("b.xsd"));
        h.add_file(
            path.trim_start_matches("/project/"),
            &PACKAGE.replace("com.acme", "com.other"),
        );
        nav.handle_events(&[Event::FileSaved(path.into())], &h.invocation());
        assert!(nav.definition(&d, at).is_empty());
        h.add_file(path.trim_start_matches("/project/"), PACKAGE);
        nav.handle_events(&[Event::FileSaved(path.into())], &h.invocation());
        assert!(!nav.definition(&d, at).is_empty());
        nav.handle_events(
            &[Event::FilesChanged(FilesChanged {
                removed: vec![path.into()],
                ..Default::default()
            })],
            &h.invocation(),
        );
        assert!(nav.definition(&d, at).is_empty());
        assert!(h.requests().is_empty() && h.process_runs().is_empty());
    }
    #[test]
    fn cold_denied_or_foreign_package_facts_do_not_supply_a_binding_and_class_override_still_works()
    {
        let (h, mut nav) = setup();
        let d = document();
        let at = position(ENDPOINT, ENDPOINT.find("p:Request").unwrap() + 2);
        assert!(WsTools::default().definition(&d, at).is_empty());
        nav.watching = false;
        assert!(nav.definition(&d, at).is_empty());
        nav.watching = true;
        let text=ENDPOINT.replace("class E", "@org.springframework.ws.server.endpoint.annotation.Namespace(prefix=\"p\",uri=\"urn:b\") class E");
        let explicit = Document {
            text: text.clone(),
            ..d.clone()
        };
        let at_explicit = position(&text, text.find("p:Request").unwrap() + 2);
        assert!(
            nav.definition(&explicit, at_explicit)[0]
                .path
                .ends_with("b.xsd")
        );
        h.add_project("/project/src/main/java/com/acme");
        assert!(nav.definition(&d, at).is_empty());
    }
    #[test]
    fn package_annotation_uri_sites_and_missing_or_dynamic_owners_are_source_checked() {
        let d = Document {
            path: "/project/src/package-info.java".into(),
            text: PACKAGE.into(),
            language: "java".into(),
            version: 1,
        };
        assert_eq!(namespace_sites(&d).len(), 1);
        assert!(
            scan_package(&d.path, &d.text)
                .unwrap()
                .bindings
                .unwrap()
                .len()
                == 1
        );
        assert!(scan_package("/project/src/Other.java", PACKAGE).is_none());
        for text in [
            PACKAGE.replace("uri=\"urn:a\"", "uri=URI"),
            PACKAGE.replace(
                "import org.springframework.ws.server.endpoint.annotation.Namespace;",
                "import foreign.Namespace;",
            ),
        ] {
            assert!(
                scan_package(&d.path, &text)
                    .unwrap()
                    .bindings
                    .unwrap_or_default()
                    .is_empty()
            );
        }
    }
}

#[cfg(test)]
mod package_occurrence_tests {
    use super::*;
    use openidea_plugin::{mock::MockHost, text::position};
    #[test]
    fn explicit_occurrence_command_reads_current_package_authority_instead_of_promoting_cached_bindings()
     {
        let h = MockHost::install();
        let source = "package com.acme; import org.springframework.ws.server.endpoint.annotation.XPathParam; class E { void handle(@XPathParam(\"/p:Request\") String input){} }";
        let package = "@org.springframework.ws.server.endpoint.annotation.Namespace(prefix=\"p\",uri=\"urn:a\") package com.acme;";
        h.add_file("src/main/java/com/acme/package-info.java", package);
        for (name, uri) in [("a", "urn:a"), ("b", "urn:b")] {
            h.add_file(&format!("src/main/resources/{name}.xsd"),&format!("<xs:schema xmlns:xs='http://www.w3.org/2001/XMLSchema' targetNamespace='{uri}'><xs:element name='Request'/></xs:schema>"));
        }
        let mut nav = WsTools::default();
        nav.activate(&h.context("aritti.spring-ws")).unwrap();
        // Deliberately change disk authority without a delivered watcher event.
        h.add_file(
            "src/main/java/com/acme/package-info.java",
            &package.replace("urn:a", "urn:b"),
        );
        let at = position(source, source.find("p:Request").unwrap() + 2);
        h.open_editor(
            "/project/src/main/java/com/acme/E.java",
            "java",
            source,
            &[(at.line, at.character, at.line, at.character)],
        );
        h.answer_next_prompt(Some(&["1"]));
        nav.run_command("qname-occurrences", &h.invocation(), None)
            .unwrap();
        assert!(h.opened_locations().last().unwrap().0.ends_with("b.xsd"));
        assert!(h.requests().is_empty() && h.process_runs().is_empty());
    }
}

struct PrefixLink {
    range: Range,
    target: Option<Location>,
    names: Vec<String>,
}
fn prefix_ranges(value: &str) -> Vec<(String, usize, usize)> {
    let bytes = value.as_bytes();
    let mut out = vec![];
    let mut at = 0;
    while at < bytes.len() && out.len() < 512 {
        if matches!(bytes[at], b'\'' | b'"') {
            let quote = bytes[at];
            at += 1;
            while at < bytes.len() && bytes[at] != quote {
                at += 1;
            }
            at += usize::from(at < bytes.len());
            continue;
        }
        if !bytes[at].is_ascii_alphabetic() && bytes[at] != b'_' {
            at += 1;
            continue;
        }
        let start = at;
        at += 1;
        while at < bytes.len() && (bytes[at].is_ascii_alphanumeric() || b"_.-".contains(&bytes[at]))
        {
            at += 1;
        }
        if bytes.get(at) == Some(&b':') && bytes.get(at + 1) != Some(&b':') {
            out.push((value[start..at].to_owned(), start, at));
        }
    }
    out
}
fn prefix_links(doc: &Document, package: &[Binding]) -> Vec<PrefixLink> {
    let Some(file) = source(doc) else {
        return vec![];
    };
    let owners = annotation_owners::Owners::new(&file, &doc.text, doc.language == "kotlin");
    let mut out = vec![];
    for ty in &file.types {
        let class = bindings(
            &ty.annotations,
            &owners,
            &doc.text,
            doc.language == "kotlin",
        );
        for method in ty.methods() {
            let Some(own) = bindings(
                &method.annotations,
                &owners,
                &doc.text,
                doc.language == "kotlin",
            ) else {
                continue;
            };
            let mut active = if let Some(class) = &class {
                let mut values = package.to_vec();
                for b in class {
                    values.retain(|v| v.prefix != b.prefix);
                    values.push(b.clone());
                }
                values
            } else {
                vec![]
            };
            for b in own {
                active.retain(|v| v.prefix != b.prefix);
                active.push(b);
            }
            let names = active
                .iter()
                .filter(|b| b.prefix_range.is_some())
                .map(|b| b.prefix.clone())
                .collect::<Vec<_>>();
            for parameter in &method.params {
                for a in &parameter.annotations {
                    if a.name != "XPathParam"
                        || !owners.known(&doc.text, a, &format!("{PREFIX}XPathParam"))
                    {
                        continue;
                    }
                    let Some(value) = literal(a, "value", &doc.text) else {
                        continue;
                    };
                    for (prefix, start, end) in prefix_ranges(&value.value) {
                        let target =
                            active
                                .iter()
                                .find(|b| b.prefix == prefix)
                                .and_then(|binding| {
                                    binding.prefix_range.map(|range| Location {
                                        path: binding
                                            .path
                                            .clone()
                                            .unwrap_or_else(|| doc.path.clone()),
                                        range,
                                    })
                                });
                        out.push(PrefixLink {
                            range: range(&doc.text, value.at + start, value.at + end),
                            target,
                            names: names.clone(),
                        });
                        if out.len() == 512 {
                            return out;
                        }
                    }
                }
            }
        }
    }
    out
}
#[cfg(test)]
mod prefix_tests {
    use super::*;
    use openidea_plugin::{
        mock::MockHost,
        text::{offset, position},
    };
    #[test]
    fn arbitrary_xpath_steps_attributes_and_predicates_link_only_physical_declared_prefixes() {
        let _h = MockHost::install().with_capabilities(&[]);
        let source = "import org.springframework.ws.server.endpoint.annotation.Namespace; import org.springframework.ws.server.endpoint.annotation.XPathParam; @Namespace(prefix=\"p\",uri=\"urn:owned\") class E { void handle(@XPathParam(\"//p:child[@p:id='q:ignored']/child::p:item\") String input){} }";
        let doc = Document {
            path: "/project/E.java".into(),
            text: source.into(),
            language: "java".into(),
            version: 1,
        };
        let links = prefix_links(&doc, &[]);
        assert_eq!(links.len(), 3);
        for link in links {
            assert_eq!(
                &source[offset(source, link.range.start).unwrap()
                    ..offset(source, link.range.end).unwrap()],
                "p"
            );
            let values = WsTools::default().definition(&doc, link.range.start);
            assert_eq!(values.len(), 1);
            assert_eq!(Some(values[0].clone()), link.target);
            let at = position(source, offset(source, link.range.start).unwrap());
            let completion = WsTools::default().completion(&doc, at, None);
            assert_eq!(completion.len(), 1);
            assert_eq!(completion[0].label, "p");
        }
        // Prefix assistance does not turn an arbitrary descendant into a global schema QName.
        assert!(references(&doc).is_empty());
    }
    #[test]
    fn xpath_package_prefix_links_use_owned_warm_package_source_ranges() {
        let h = MockHost::install();
        let package = "@org.springframework.ws.server.endpoint.annotation.Namespace(prefix=\"p\",uri=\"urn:a\") package com.acme;";
        h.add_file("src/com/acme/package-info.java", package);
        let mut nav = WsTools::default();
        nav.activate(&h.context("aritti.spring-ws")).unwrap();
        let source = "package com.acme; import org.springframework.ws.server.endpoint.annotation.XPathParam; class E { void handle(@XPathParam(\"p:child/@p:id\") String input){} }";
        let doc = Document {
            path: "/project/src/com/acme/E.java".into(),
            language: "java".into(),
            text: source.into(),
            version: 1,
        };
        let at = position(source, source.find("p:child").unwrap());
        let locations = nav.definition(&doc, at);
        assert_eq!(locations.len(), 1);
        assert!(locations[0].path.ends_with("package-info.java"));
        assert_eq!(
            &package[offset(package, locations[0].range.start).unwrap()
                ..offset(package, locations[0].range.end).unwrap()],
            "p"
        );
        nav.watching = false;
        assert!(nav.definition(&doc, at).is_empty());
    }
}

#[cfg(test)]
mod unknown_prefix_completion_test {
    use super::*;
    use openidea_plugin::{mock::MockHost, text::position};
    #[test]
    fn unknown_xpath_prefix_can_be_replaced_by_a_declared_prefix_without_a_guessed_definition() {
        let _h = MockHost::install().with_capabilities(&[]);
        let text = "import org.springframework.ws.server.endpoint.annotation.Namespace; import org.springframework.ws.server.endpoint.annotation.XPathParam; @Namespace(prefix=\"p\",uri=\"urn:a\") class E {void handle(@XPathParam(\"//q:item\") String input){}}";
        let doc = Document {
            path: "/project/E.java".into(),
            text: text.into(),
            language: "java".into(),
            version: 1,
        };
        let at = position(text, text.find("q:item").unwrap());
        assert!(WsTools::default().definition(&doc, at).is_empty());
        let values = WsTools::default().completion(&doc, at, None);
        assert_eq!(values.len(), 1);
        assert_eq!(values[0].label, "p");
    }
}

#[cfg(test)]
mod syntax_diagnostic_tests {
    use super::*;
    const XP: &str = "import org.springframework.ws.server.endpoint.annotation.XPathParam; import org.springframework.xml.xpath.XPathExpressionFactory; ";
    fn doc(body: &str, language: &str) -> Document {
        Document {
            path: if language == "kotlin" { "/p/E.kt" } else { "/p/E.java" }.into(),
            language: language.into(),
            text: format!("{}{body}", if language == "kotlin" { XP.replace("; ", "\n") } else { XP.into() }),
            ..Default::default()
        }
    }
    #[test]
    fn invalid_literal_xpath_param_warns_on_the_token() {
        let d = doc("class E { void h(@XPathParam(\"/a/b[\") String v) {} }", "java");
        let found = syntax_diagnostics(&d);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].severity, Severity::Warning);
        let at = d.text.find("b[").unwrap() + 1;
        assert_eq!(found[0].range, range(&d.text, at, at + 1));
    }
    #[test]
    fn valid_non_literal_and_foreign_stay_quiet() {
        for body in [
            "class E { void h(@XPathParam(\"/p:a[@id='1']/text()\") String v) {} }",
            "class E { static final String X=\"[\"; void h(@XPathParam(X) String v) {} }",
            "class E { void h(@XPathParam(\"/a[\" + \"b\") String v) {} }",
            "class E { void h(@XPathParam(\"/a[$x\") String v) {} }",
            "class E { void h(@XPathParam(\"/a[\\n\") String v) {} }",
            "class E { void h(@Other(\"/a[\") String v) {} }",
            "class E { Object h() { return XPathExpressionFactory.createXPathExpression(\"//a\"); } }",
            "class E { Object h(String s) { return XPathExpressionFactory.createXPathExpression(s); } }",
            "class E { Object h(String s) { return XPathExpressionFactory.createXPathExpression(\"//a[\" + s); } }",
        ] {
            assert!(syntax_diagnostics(&doc(body, "java")).is_empty(), "{body}");
        }
        let foreign = "class XPathExpressionFactory { static void createXPathExpression(String s){} } class E { void h(){ XPathExpressionFactory.createXPathExpression(\"a[\"); } }";
        let mut d = doc(foreign, "java");
        d.text = foreign.into();
        assert!(syntax_diagnostics(&d).is_empty());
    }
    #[test]
    fn create_xpath_expression_literal_is_checked_including_namespace_overload_and_kotlin() {
        let java = doc("class E { Object h(java.util.Map m) { return XPathExpressionFactory.createXPathExpression(\"//a[\", m); } }", "java");
        assert_eq!(syntax_diagnostics(&java).len(), 1);
        let kotlin = doc("class E { fun h() = XPathExpressionFactory.createXPathExpression(\"a b\") }", "kotlin");
        assert_eq!(syntax_diagnostics(&kotlin).len(), 1);
    }
    #[test]
    fn empty_literal_is_reported_over_the_quotes() {
        let d = doc("class E { void h(@XPathParam(\"\") String v) {} }", "java");
        let found = syntax_diagnostics(&d);
        assert_eq!(found.len(), 1);
        let q = d.text.find("\"\"").unwrap();
        assert_eq!(found[0].range, range(&d.text, q, q + 2));
    }
}
#[cfg(test)]
mod undeclared_prefix_tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    const IMPORTS: &str = "import org.springframework.ws.server.endpoint.annotation.Namespace; import org.springframework.ws.server.endpoint.annotation.XPathParam; ";
    const DIR: &str = "src/main/java/com/acme";
    fn reported(files: &[(&str, &str)], body: &str) -> Vec<String> {
        let host = MockHost::install();
        for (name, text) in files {
            host.add_file(name, text);
        }
        let text = format!("package com.acme; {IMPORTS}{body}");
        host.add_file(&format!("{DIR}/E.java"), &text);
        let mut nav = WsTools::default();
        nav.activate(&host.context("aritti.spring-ws")).unwrap();
        let doc = Document { path: format!("/project/{DIR}/E.java"), text: text.clone(), language: "java".into(), version: 1 };
        nav.diagnostics(&doc)
            .into_iter()
            .filter(|d| d.code.as_deref() == Some("undeclared-xpath-prefix"))
            .map(|d| text[offset(&text, d.range.start).unwrap()..offset(&text, d.range.end).unwrap()].to_owned())
            .collect()
    }
    use openidea_plugin::text::offset;
    const PARAM: &str = "void h(@XPathParam(\"/p:a/q:b[@xml:lang='x']/@xmlns:z\") String v) {}";
    #[test]
    fn unbound_prefixes_are_reported_and_reserved_xml_prefixes_are_not() {
        let found = reported(&[], &format!("@Namespace(prefix=\"p\",uri=\"urn:a\") class E {{ {PARAM} }}"));
        assert_eq!(found, ["q"]);
    }
    #[test]
    fn package_class_and_method_declarations_all_bind() {
        let package = "@Namespace(prefix=\"q\",uri=\"urn:q\") package com.acme; import org.springframework.ws.server.endpoint.annotation.Namespace;";
        let found = reported(&[(&format!("{DIR}/package-info.java"), package)], &format!("@Namespace(prefix=\"p\",uri=\"urn:a\") class E {{ {PARAM} }}"));
        assert!(found.is_empty(), "{found:?}");
        let found = reported(&[], &format!("class E {{ @Namespace(prefix=\"p\",uri=\"urn:a\") void h(@XPathParam(\"/p:a/q:b\") String v) {{}} }}"));
        assert_eq!(found, ["q"]);
    }
    #[test]
    fn uncertain_declaration_sources_keep_diagnostics_quiet() {
        let class = format!("class E {{ {PARAM} }}");
        for (files, body) in [
            (vec![("src/main/resources/a.xml", "<bean class=\"org.springframework.ws.server.endpoint.adapter.XPathParamAnnotationMethodEndpointAdapter\"/>")], class.clone()),
            (vec![("src/main/java/com/acme/Config.java", "class Config { XPathParamAnnotationMethodEndpointAdapter a; }")], class.clone()),
            (vec![(&*format!("{DIR}/package-info.java"), "@Namespace(prefix=PREFIX,uri=\"u\") package com.acme; import org.springframework.ws.server.endpoint.annotation.Namespace;")], class.clone()),
            (vec![(&*format!("{DIR}/package-info.java"), "package com.acme; @@@ {{")], class.clone()),
            (vec![("src/test/java/com/acme/package-info.java", "package com.acme;")], class.clone()),
            (vec![], format!("@Namespace(prefix=P,uri=\"urn:a\") class E {{ {PARAM} }}")),
            (vec![], "class E { void h(@XPathParam(NAME) String v) {} }".into()),
        ] {
            assert!(reported(&files, &body).is_empty(), "{files:?} {body}");
        }
    }
}
