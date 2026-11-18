use super::*;
impl GraphQlTools {
    pub fn live_diagnostics(&mut self, doc: &Document, allow_absence: bool) -> Vec<Diagnostic> {
        let _ = self.java.for_cached_document(&doc.path, &doc.text);
        let mut out = Vec::new();
        let Some(schemas) = self.schemas.for_cached_document(&doc.path, &doc.text) else {
            return out;
        };
        if is_code(doc) {
            for h in handlers_in(&doc.text, doc.language == "kotlin") {
                let ty = type_of(&h, schemas);
                let found = schema_fields(schemas, &ty, &h.field);
                if found.is_empty() {
                    let known = schemas.iter().any(|(_, s)| s.types.iter().any(|t| t.name == ty));
                    if allow_absence
                        && schemas.fresh()
                        && known
                        && host::setting_bool("springGraphql.checkUnmatchedMapping").unwrap_or(true)
                    {
                        out.push(
                            Diagnostic::new(
                                h.range,
                                Severity::Warning,
                                format!("The GraphQL schema declares no field `{}` on type `{ty}`.", h.field),
                            )
                            .code("unknown-schema-field"),
                        );
                    }
                } else if allow_absence && schemas.fresh() && found.len() == 1 {
                    for (name, at) in &h.args {
                        if !found[0].1.args.contains(name) {
                            out.push(
                                Diagnostic::new(*at, Severity::Warning, format!("`{ty}.{}` has no argument `{name}`.", h.field))
                                    .code("unknown-graphql-argument"),
                            );
                        }
                    }
                }
            }
            if host::setting_bool("springGraphql.checkBatchMappingReturn").unwrap_or(true) {
                out.extend(
                    checks::batch_mapping_return(&spring_text::parse_source(&doc.path, &doc.text), &doc.text)
                        .iter()
                        .map(|f| f.diagnostic(&doc.text)),
                );
            }
        } else {
            let Some(java) = self.java.for_cached_document(&doc.path, &doc.text) else {
                return out;
            };
            if !allow_absence || !java.fresh() || !schemas.fresh() || java.iter().any(|(_, f)| f.wired) {
                return out;
            }
            let roots: Vec<_> = ROOTS.iter().map(|r| root_type(schemas, r.1)).collect();
            for ty in parse_schema(&doc.text).types {
                if ty.kind != "type" || !roots.contains(&ty.name) {
                    continue;
                }
                for field in ty.fields {
                    if !java.iter().flat_map(|(_, f)| &f.handlers).any(|h| h.field == field.name && type_of(h, schemas) == ty.name) {
                        out.push(
                            Diagnostic::new(
                                field.range,
                                Severity::Hint,
                                format!("No cached source controller method handles `{}.{}`.", ty.name, field.name),
                            )
                            .code("unmapped-root-field"),
                        );
                    }
                }
            }
        }
        out
    }
    pub fn live_hover(&mut self, doc: &Document, at: Position) -> Option<openidea_plugin::hover::Hover> {
        if matches!(doc.language.as_str(),"java"|"kotlin") {return super::mapping_docs::hover(doc,at);}
        if doc.language != "graphql" {return None;}
        let schema = parse_schema(&doc.text);
        for ty in schema.types {
            for field in ty.fields {
                if contains(field.range, at) && !field.type_reference.is_empty() && field.name.len() <= 128 && field.type_reference.len() <= 1024 && field.argument_types.len()<=64 {
                    if field.argument_types.len()!=field.args.len()||field.argument_types.iter().any(|(name,ty)|name.len()>128||ty.is_empty()||ty.len()>1024){return None;}
                    let arguments=field.argument_types.iter().map(|(name,ty)|format!("{name}: {ty}")).collect::<Vec<_>>().join(", ");
                    let signature=if arguments.is_empty(){format!("{}: {}",field.name,field.type_reference)}else{format!("{}({arguments}): {}",field.name,field.type_reference)};
                    if signature.len()>8192{return None;}
                    return Some(openidea_plugin::hover::Hover::new(format!("GraphQL schema declaration `{}.{}`\n\nDeclared signature: `{signature}`.\n\nSource declaration only; list/non-null wrappers reproduce the written SDL. Runtime resolver behavior, schema merging and type inference are not established.",ty.name,field.name)).range(field.range));
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod declared_signature_hover_tests {
    use super::*;
    use openidea_plugin::{mock::MockHost,text::position};
    fn doc(text:&str)->Document{Document{path:"/project/schema.graphqls".into(),language:"graphql".into(),text:text.into(),version:1}}
    #[test]
    fn sdl_hover_preserves_declared_list_and_nonnull_wrappers_for_fields_and_arguments(){
        let h=MockHost::install().with_capabilities(&[]);
        let text="type Query { books(ids: [ID!]!, filter: [[String!]]): [Book!]! } type Book { title: String! }";
        let value=GraphQlTools::default().live_hover(&doc(text),position(text,text.find("books").unwrap())).unwrap();
        assert!(value.markdown.contains("books(ids: [ID!]!, filter: [[String!]]): [Book!]!"));assert!(value.markdown.contains("wrappers reproduce the written SDL"));assert!(!value.markdown.contains("wrappers and runtime resolver behavior are not inferred"));
        let simple=GraphQlTools::default().live_hover(&doc(text),position(text,text.find("title").unwrap())).unwrap();assert!(simple.markdown.contains("title: String!"));assert!(h.requests().is_empty()&&h.process_runs().is_empty());
    }
    #[test]
    fn malformed_or_excessive_declared_signatures_are_not_presented_as_complete(){
        for text in ["type Query { books: [Book!!] }", "type Query { books(ids: [ID!!]): Book }"] {assert!(GraphQlTools::default().live_hover(&doc(text),position(text,text.find("books").unwrap())).is_none(),"{text}");}
        let args=(0..65).map(|i|format!("arg{i}: ID")).collect::<Vec<_>>().join(", ");let text=format!("type Query {{ books({args}): [Book!]! }}");assert!(GraphQlTools::default().live_hover(&doc(&text),position(&text,text.find("books").unwrap())).is_none());
    }
}
