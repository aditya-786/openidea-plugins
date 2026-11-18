//! Read-only physical QName occurrences. Kept out of the host index because
//! its implicit Rename would require a complete XML QName reference model.
use super::*;
use openidea_plugin::indexing::{Anchor, Document as IndexedDocument, Edges};

const FILES_PER_TYPE: u32 = 128;

fn key(namespace: &str, local: &str) -> String {
    format!("spring-ws:qname:{}:{namespace}:{local}", namespace.len())
}
fn raw(text: &str, span: Range, value: &str) -> bool {
    let Some(start) = openidea_plugin::text::offset(text, span.start) else {
        return false;
    };
    let Some(end) = openidea_plugin::text::offset(text, span.end) else {
        return false;
    };
    text.get(start..end) == Some(value) && start < end
}
pub(crate) fn index_source(doc: &IndexedDocument) -> Edges {
    let mut edges = Edges::default();
    if doc.text.len() > MAX_SCHEMA_BYTES {
        return edges;
    }
    if doc.path.ends_with(".java") || doc.path.ends_with(".kt") {
        let source = Document {path:doc.path.clone(),text:doc.text.clone(),language:if doc.path.ends_with(".kt"){ "kotlin" } else { "java" }.into(),version:0};
        for (namespace,local,range) in xpath::references(&source) {
            edges.references.push(Anchor{key:key(&namespace,&local),range});
        }
        for endpoint in endpoints_with_syntax(
            &parse_source(&doc.path, &doc.text),
            &doc.text,
            doc.path.ends_with(".kt"),
        ) {
            let Some(namespace) = endpoint.namespace else {
                continue;
            };
            if raw(&doc.text, endpoint.local_range, &endpoint.local) {
                edges.references.push(Anchor {
                    key: key(&namespace, &endpoint.local),
                    range: endpoint.local_range,
                });
            }
        }
    } else if doc.path.ends_with(".xml") {
        let source = Document {path:doc.path.clone(),text:doc.text.clone(),language:"xml".into(),version:0};
        for (namespace,local,range) in xml_refs::payload_qnames(&source) {
            edges.references.push(Anchor{key:key(&namespace,&local),range});
        }
    } else if doc.path.ends_with(".xsd") || doc.path.ends_with(".wsdl") {
        let (schemas, complete) = schema_qnames::read(&doc.text);
        if !complete {
            return edges;
        }
        for schema in schemas {
            for (name, span) in schema.elements {
                if raw(&doc.text, span, &name) {
                    edges.definitions.push(Anchor {
                        key: key(&schema.namespace, &name),
                        range: span,
                    });
                }
            }
        }
    }
    if edges.definitions.len() + edges.references.len() > 4096 {
        return Edges::default();
    }
    edges
}
#[cfg(test)]
mod tests {
    use super::*;
    fn doc(path: &str, text: &str) -> IndexedDocument {
        IndexedDocument {
            path: path.into(),
            text: text.into(),
            symbols: vec![],
        }
    }
    #[test]
    fn qnames_match_only_exact_namespaces_and_source_ranges() {
        let schema = "<xs:schema xmlns:xs='http://www.w3.org/2001/XMLSchema' targetNamespace='urn:a'><xs:element name='Request'/></xs:schema>";
        let defs = index_source(&doc("/p/a.xsd", schema));
        assert_eq!(defs.definitions.len(), 1);
        let source = "import org.springframework.ws.server.endpoint.annotation.Endpoint; import org.springframework.ws.server.endpoint.annotation.PayloadRoot; @Endpoint class E { @PayloadRoot(namespace=\"urn:a\", localPart=\"Request\") public void handle() {} }";
        let refs = index_source(&doc("/p/E.java", source));
        assert_eq!(refs.references.len(), 1);
        assert_eq!(refs.references[0].key, defs.definitions[0].key);
        assert!(raw(source, refs.references[0].range, "Request"));
        let other = index_source(&doc("/p/E.java", &source.replace("urn:a", "urn:b")));
        assert_ne!(other.references[0].key, defs.definitions[0].key);
        assert!(
            index_source(&doc("/p/E.java", &source.replace("\"urn:a\"", "dynamic()")))
                .references
                .is_empty()
        );
        assert!(
            index_source(&doc(
                "/p/E.java",
                &source.replace("Request", "Req\\u0075est")
            ))
            .references
            .is_empty()
        );
        assert!(
            index_source(&doc(
                "/p/a.xsd",
                &schema.replace("http://www.w3.org/2001/XMLSchema", "urn:foreign")
            ))
            .definitions
            .is_empty()
        );
    }
}

fn observed_edges(_nav:&mut WsTools,doc:&IndexedDocument)->Edges {
    let mut edges=index_source(doc);
    let language=if doc.path.ends_with(".java"){"java"}else if doc.path.ends_with(".kt"){"kotlin"}else{"xml"};
    let source=Document{path:doc.path.clone(),text:doc.text.clone(),language:language.into(),version:0};
    let package=(|| {
        if !matches!(language,"java"|"kotlin"){return None;}
        let path=format!("{}/package-info.java",doc.path.rsplit_once('/')?.0);
        let project=project_of(&doc.path)?;
        if project_of(&path)?.root().ok()!=project.root().ok() {return None;}
        if project.stat(&path).ok()?.size>32768 {return None;}
        Some((path.clone(),project.read_file(&path).ok()?))
    })();
    let refs=match package {Some((path,text))=>xpath::references_with_package_source(&source,&path,&text),None=>xpath::references(&source)};
    for (namespace,local,range) in refs {
        let key=key(&namespace,&local);
        if !edges.references.iter().any(|a|a.key==key&&a.range==range){edges.references.push(Anchor{key,range});}
    }
    edges
}
pub(crate) fn occurrences(nav:&mut WsTools,ctx: &Invocation) -> Result<Option<String>, Error> {
    let project = ctx.project().ok_or("Open a project first.")?;
    let root = project.root()?;
    let editor = ctx
        .editor()
        .ok_or("Open an endpoint or schema and place the caret on its QName local name.")?;
    let snapshot = editor.document()?;
    let path = editor.path();
    let caret = editor
        .selections()?
        .first()
        .map(|r| r.start)
        .ok_or(Error::Stale)?;
    let owned = |path: &str| {
        path.starts_with(&format!("{root}/"))
            && host::project_of(path)
                .and_then(|p| p.root().ok())
                .as_deref()
                == Some(root.as_str())
    };
    if root == "/" || !owned(&path) {
        return Err(Error::Stale);
    }
    let doc = IndexedDocument {
        path: path.clone(),
        text: snapshot.text.clone(),
        symbols: vec![],
    };
    let edges = observed_edges(nav,&doc);
    let selected = edges
        .definitions
        .iter()
        .chain(&edges.references)
        .find(|a| a.range.start <= caret && caret < a.range.end)
        .ok_or("Put the caret on an element name written out in the file, then try again.")?;
    let key = selected.key.clone();
    let selected_span=selected.range;
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut files = std::collections::BTreeSet::from([path.clone()]);
    let mut capped = false;
    for glob in ["**/*.java", "**/*.kt", "**/*.xsd", "**/*.wsdl", "**/*.xml"] {
        if Instant::now() >= deadline {
            break;
        }
        let found = project.find_files(glob, FILES_PER_TYPE + 1).unwrap_or_default();
        capped |= found.len() > FILES_PER_TYPE as usize;
        files.extend(found.into_iter().filter(|p| owned(p)));
    }
    if capped {
        host::notify(
            openidea_plugin::NotificationLevel::Info,
            "Spring WS",
            &format!("Searched only the first {FILES_PER_TYPE} files of each type, so some occurrences may be missing."),
        );
    }
    let mut hits = Vec::new();
    let mut bytes = 0;
    for file in std::iter::once(&path)
        .chain(files.iter().filter(|p| *p != &path))
        .take(256)
    {
        if Instant::now() >= deadline || bytes >= 4 << 20 || hits.len() >= 512 {
            break;
        }
        if !owned(file) {
            continue;
        }
        let source = if file == &path {
            snapshot.text.clone()
        } else {
            if !project
                .stat(file)
                .is_ok_and(|s| s.size <= MAX_SCHEMA_BYTES as u64)
            {
                continue;
            }
            let Ok(text) = project.read_file(file) else {
                continue;
            };
            text
        };
        if source.len() > MAX_SCHEMA_BYTES {
            continue;
        }
        bytes += source.len();
        let edges = observed_edges(nav,&IndexedDocument {
            path: file.clone(),
            text: source.clone(),
            symbols: vec![],
        });
        for a in edges
            .definitions
            .iter()
            .chain(&edges.references)
            .filter(|a| a.key == key)
            .take(512 - hits.len())
        {
            hits.push((file.clone(), a.range, source.clone()));
        }
    }
    let items: Vec<_> = hits
        .iter()
        .enumerate()
        .map(|(i, (file, span, _))| {
            openidea_plugin::PickItem::new(
                &i.to_string(),
                &format!(
                    "{}:{}:{}",
                    file.strip_prefix(&format!("{root}/")).unwrap_or(file),
                    span.start.line + 1,
                    span.start.character + 1
                ),
            )
            .description(
                "Observed exact QName; bounded source search, no global usages or Rename claim",
            )
        })
        .collect();
    let Some(choice) = host::pick("Observed Spring WS QName occurrences", &items)? else {
        return Ok(None);
    };
    let (destination, span, source) = hits
        .get(choice.parse::<usize>().map_err(|_| Error::Stale)?)
        .ok_or(Error::Stale)?;
    let current = ctx.editor().ok_or(Error::Stale)?;
    let current_snapshot = current.document()?;
    if ctx.project().ok_or(Error::Stale)?.root()? != root
        || current.path() != path
        || current_snapshot.version != snapshot.version
        || current_snapshot.text != snapshot.text
        || !owned(destination)
    {
        return Err(Error::Stale);
    }
    let fresh = if destination == &path {
        current_snapshot.text
    } else {
        project.read_file(destination)?
    };
    if fresh != *source {
        return Err(Error::Stale);
    }
    // Package annotations are external source authority. Re-read the admitted
    // sibling package-info and re-establish both QName identities after the picker.
    let current_edges=observed_edges(nav,&IndexedDocument{path:path.clone(),text:snapshot.text.clone(),symbols:vec![]});
    if !current_edges.definitions.iter().chain(&current_edges.references).any(|a|a.key==key&&a.range==selected_span){return Err(Error::Stale);}
    let target_edges=observed_edges(nav,&IndexedDocument{path:destination.clone(),text:fresh,symbols:vec![]});
    if !target_edges.definitions.iter().chain(&target_edges.references).any(|a|a.key==key&&a.range==*span){return Err(Error::Stale);}
    host::open_location(destination, span.start.line, span.start.character)?;
    Ok(None)
}
#[cfg(test)]
mod command_tests {
    use super::*;
    use openidea_plugin::{mock::MockHost, text::position};
    #[test]
    fn registered_picker_revalidates_exact_namespace_and_never_exports_rename() {
        let h = MockHost::install();
        let source = "import org.springframework.ws.server.endpoint.annotation.Endpoint; import org.springframework.ws.server.endpoint.annotation.PayloadRoot; @Endpoint class E { @PayloadRoot(namespace=\"urn:a\",localPart=\"Request\") void handle(){} }";
        let schema = "<xs:schema xmlns:xs='http://www.w3.org/2001/XMLSchema' targetNamespace='urn:a'><xs:element name='Request'/></xs:schema>";
        h.add_file("/project/a.xsd", schema);
        h.add_file("/project/other.xsd", &schema.replace("urn:a", "urn:b"));
        let at = position(source, source.find("Request").unwrap());
        h.open_editor(
            "/project/E.java",
            "java",
            source,
            &[(at.line, at.character, at.line, at.character)],
        );
        h.answer_next_prompt(Some(&["1"]));
        WsTools::default()
            .run_command("qname-occurrences", &h.invocation(), None)
            .unwrap();
        assert_eq!(h.opened_locations().last().unwrap().0, "/project/a.xsd");
        assert!(!include_str!("../plugin.toml").contains("contributes.documentIndexers"));
        assert!(h.requests().is_empty() && h.process_runs().is_empty());
    }
    #[test]
    fn occurrences_report_cap() {
        let h = MockHost::install();
        let source = "import org.springframework.ws.server.endpoint.annotation.Endpoint; import org.springframework.ws.server.endpoint.annotation.PayloadRoot; @Endpoint class E { @PayloadRoot(namespace=\"urn:a\",localPart=\"Request\") void handle(){} }";
        h.add_file("/project/a.xsd", "<xs:schema xmlns:xs='http://www.w3.org/2001/XMLSchema' targetNamespace='urn:a'><xs:element name='Request'/></xs:schema>");
        for n in 0..129 {
            h.add_file(&format!("/project/c{n}.xml"), "<a/>");
        }
        let at = position(source, source.find("Request").unwrap());
        h.open_editor("/project/E.java", "java", source, &[(at.line, at.character, at.line, at.character)]);
        h.answer_next_prompt(Some(&["0"]));
        WsTools::default().run_command("qname-occurrences", &h.invocation(), None).unwrap();
        assert!(h.notifications().iter().any(|n| n.body.contains("128")), "{:?}", h.notifications());
    }

    #[test]
    fn occurrences_do_not_report_cap_at_exactly_128_files() {
        let h = MockHost::install();
        let source = "import org.springframework.ws.server.endpoint.annotation.Endpoint; import org.springframework.ws.server.endpoint.annotation.PayloadRoot; @Endpoint class E { @PayloadRoot(namespace=\"urn:a\",localPart=\"Request\") void handle(){} }";
        h.add_file("/project/a.xsd", "<xs:schema xmlns:xs='http://www.w3.org/2001/XMLSchema' targetNamespace='urn:a'><xs:element name='Request'/></xs:schema>");
        for n in 0..128 {
            h.add_file(&format!("/project/c{n}.xml"), "<a/>");
        }
        let at = position(source, source.find("Request").unwrap());
        h.open_editor("/project/E.java", "java", source, &[(at.line, at.character, at.line, at.character)]);
        h.answer_next_prompt(Some(&["0"]));
        WsTools::default().run_command("qname-occurrences", &h.invocation(), None).unwrap();
        assert!(h.notifications().is_empty(), "{:?}", h.notifications());
    }
}
