//! Positive current-file Java facts only; no effective bean/type owner model.
use crate::*;
use openidea_plugin::hover::Hover;
use spring_text::java::{self, JavaFile};
struct Token {
    name: String,
    range: Range,
    declaration: bool,
}
use spring_text::provenance::{literal, source_type_owned as owned};
fn annotation_owned(file:&JavaFile,doc:&Document,annotation:&Annotation,qualified:&str)->bool {
    owned(file,&doc.text[annotation.at+1..annotation.name_at+annotation.name.len()],qualified)
}
fn literal_argument(doc:&Document, annotation:&Annotation, keys:&[&str])->Option<Option<Token>> {
    let arguments:Vec<_>=annotation.args.iter().filter(|a|keys.contains(&a.name.as_str())).collect();
    match arguments.as_slice() {
        []=>Some(None),
        [arg]=>{
            let tokens=java::tokens(&arg.text);let [one]=tokens.as_slice() else{return None;};
            let string=one.string()?;
            if !literal(&string.value) || arg.strings.len()!=1 {return None;}
            let source=&arg.strings[0];
            if doc.text.get(source.at..source.end)!=Some(source.value.as_str()) {return None;}
            Some(Some(Token{name:source.value.clone(),range:range(&doc.text,source.at,source.end),declaration:false}))
        }
        _=>None,
    }
}
fn model(doc:&Document)->Option<Vec<Token>> {
    if doc.language!="java" {return None;}
    crate::channel_generation::context(doc)?;
    let file=spring_text::parse_source(&doc.path,&doc.text);
    let deadline=std::time::Instant::now()+std::time::Duration::from_millis(10);
    let mut out=Vec::new();let mut occupied=BTreeMap::<String,usize>::new();
    for owner in &file.types {
        if !owner.extends.is_empty() || !owner.implements.is_empty() || doc.text[owner.at+owner.name.len()..].trim_start().starts_with('<') {return None;}
        for annotation in owner.annotations.iter().filter(|a|java::STEREOTYPES.contains(&a.name.as_str())) {
            let name=literal_argument(doc,annotation,&["value","name"])?;
            let name=name.map(|t|t.name).unwrap_or_else(||spring_text::decapitalize(&owner.name));
            *occupied.entry(name).or_default()+=1;
        }
        for method in owner.methods() {
            if std::time::Instant::now()>=deadline || out.len()>128 {return None;}
            let beans:Vec<_>=method.annotations.iter().filter(|a|a.name=="Bean").collect();
            if beans.len()>1 {return None;}
            if !beans.is_empty() && java::tokens(&doc.text[method.start..method.at]).iter().any(|t|t.s=="<") {return None;}
            if let Some(annotation)=beans.first() {
                if !annotation_owned(&file,doc,annotation,"org.springframework.context.annotation.Bean") {return None;}
                let name=literal_argument(doc,annotation,&["value","name"])?;
                let mut token=name.unwrap_or_else(||Token{name:method.name.clone(),range:range(&doc.text,method.at,method.at+method.name.len()),declaration:true});
                if !literal(&token.name) {return None;}
                *occupied.entry(token.name.clone()).or_default()+=1;
                if method.ty.as_ref().is_some_and(|ty| !ty.array && ty.args.is_empty() && owned(&file,&ty.text,"org.springframework.messaging.MessageChannel")) {
                    token.declaration=true;out.push(token);
                }
            }
            for annotation in method.annotations.iter().filter(|a|a.name=="ServiceActivator") {
                if !annotation_owned(&file,doc,annotation,"org.springframework.integration.annotation.ServiceActivator") {continue;}
                for key in ["inputChannel","outputChannel"] {
                    if let Some(token)=literal_argument(doc,annotation,&[key])? {out.push(token);}
                }
            }
        }
    }
    // Only positive supported declarations are shown. Competing literal bean/
    // stereotype registrations suppress a source fact; global absence is never inferred.
    if out.len()>128 {return None;}
    out.retain(|t|occupied.get(&t.name)==Some(&1));
    Some(out)
}
pub(crate) fn definition(doc:&Document,at:Position)->Option<Vec<Location>> {
    let tokens=model(doc)?;let selected=tokens.iter().find(|t|t.range.start<=at&&at<t.range.end)?;
    let declarations:Vec<_>=tokens.iter().filter(|t|t.declaration&&t.name==selected.name).collect();
    Some(match declarations.as_slice() { [declaration] if !selected.declaration=>vec![Location{path:doc.path.clone(),range:declaration.range}],_=>vec![] })
}
pub(crate) fn occurrences(ctx:&Invocation)->Result<Option<String>,Error> {
    let root=ctx.project().ok_or("Open a project first.")?.root()?;let editor=ctx.editor().ok_or_else(||format!("Open a Java or XML file with channels first. {}",ui_kit::denied::message("Find Channel Usages in This File","editor.read","Spring Structure Tools")))?;
    let source=editor.document()?;let doc=Document{path:editor.path(),language:editor.language(),text:source.text,version:source.version};
    if spring_text::index::owning_root(&doc.path).is_none()||host::project_of(&doc.path).and_then(|p|p.root().ok()).as_deref()!=Some(root.as_str()) {return Err(Error::Stale);}
    let selections=editor.selections()?;let [selection]=selections.as_slice() else{return Err(Error::Invalid("Use a single caret.".into()));};
    if selection.start!=selection.end {return Err(Error::Invalid("Use a collapsed caret on a supported Java channel name.".into()));}
    let tokens=model(&doc).ok_or("Use literal Spring Bean/ServiceActivator names with explicit supported MessageChannel ownership.")?;
    let selected=tokens.iter().find(|t|t.range.start<=selection.start&&selection.start<t.range.end).ok_or("Place the caret on a supported channel name.")?;
    if tokens.iter().filter(|t|t.declaration&&t.name==selected.name).count()!=1 {return Err(Error::Invalid("Channel declaration is ambiguous in this source.".into()));}
    let sites:Vec<_>=tokens.iter().filter(|t|t.name==selected.name).collect();
    let items:Vec<_>=sites.iter().enumerate().map(|(i,t)|openidea_plugin::PickItem::new(&format!("site:{i}"),&format!("{} · {}:{}",if t.declaration {"Bean declaration"}else{"ServiceActivator channel"},t.range.start.line+1,t.range.start.character+1)).description(&doc.path)).collect();
    let Some(choice)=host::pick(&format!("Java channel {}: {} current-file occurrences",selected.name,sites.len()),&items)? else{return Ok(None);};
    let target=choice.strip_prefix("site:").and_then(|v|v.parse::<usize>().ok()).and_then(|i|sites.get(i)).ok_or(Error::Stale)?;
    let current=editor.document()?;if current.text!=doc.text||current.version!=doc.version||editor.path()!=doc.path||host::project_of(&doc.path).and_then(|p|p.root().ok()).as_deref()!=Some(root.as_str()) {return Err(Error::Stale);}
    host::open_location(&doc.path,target.range.start.line,target.range.start.character)?;
    Ok(Some("Navigated to a proved current-file Java channel occurrence. Only the supported Bean declaration and ServiceActivator channel literals are included; other annotations, files and runtime contexts are excluded.".into()))
}
pub(crate) fn hover(doc:&Document,at:Position)->Option<Hover> {
    let tokens=model(doc)?;
    let selected=tokens.iter().find(|t|t.range.start<=at&&at<t.range.end)?;
    let declarations:Vec<_>=tokens.iter().filter(|t|t.declaration&&t.name==selected.name).collect();
    let [declaration]=declarations.as_slice() else{return None;};
    let references=tokens.iter().filter(|t|!t.declaration&&t.name==selected.name).count();
    Some(Hover::new(format!("**Spring Integration channel** `{}`\n\nDeclared by a `@Bean` returning `MessageChannel` at line {}.\n\n{references} reference(s) in this file.",selected.name,declaration.range.start.line+1)).range(selected.range))
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{hover::HoverProvider,mock::MockHost};
    fn doc(body:&str)->Document {
        Document{path:"/project/src/app/Channels.java".into(),language:"java".into(),version:7,text:format!("package app;\r\nimport org.springframework.context.annotation.Configuration;\r\nimport org.springframework.context.annotation.Bean;\r\nimport org.springframework.messaging.MessageChannel;\r\nimport org.springframework.integration.annotation.ServiceActivator;\r\n@Configuration class Channels {{\r\n{body}\r\n}}")}
    }
    fn at(d:&Document,value:&str)->Position {openidea_plugin::text::position(&d.text,d.text.rfind(value).unwrap())}
    #[test]
    fn registered_java_definition_uses_current_channel_source_and_exact_ranges() {
        let host=MockHost::install().with_capabilities(&[]);let d=doc(r#"@Bean(name="orders") MessageChannel input(){return null;} /*😀*/ @ServiceActivator(inputChannel="orders") void consume(){}"#);
        let mut owner=structure::Structure::default();let destination=owner.definition(&d,at(&d,"orders"));assert_eq!(destination.len(),1);assert_eq!(destination[0].path,d.path);
        let from=openidea_plugin::text::offset(&d.text,destination[0].range.start).unwrap();let to=openidea_plugin::text::offset(&d.text,destination[0].range.end).unwrap();assert_eq!(&d.text[from..to],"orders");
        let changed=Document{text:d.text.replacen(r#"name="orders""#,r#"name="other""#,1),..d.clone()};assert!(definition(&changed,at(&changed,"orders")).is_none());
        assert!(host.process_runs().is_empty()&&host.requests().is_empty());
    }
    #[test]
    fn registered_java_occurrences_command_navigates_without_network_or_source_mutation() {
        let host=MockHost::install();let d=doc(r#"@Bean MessageChannel orders(){return null;} @ServiceActivator(inputChannel="orders", outputChannel="orders") void consume(){}"#);
        let p=at(&d,"orders");host.open_editor(&d.path,"java",&d.text,&[(p.line,p.character,p.line,p.character)]);host.answer_next_prompt(Some(&["site:0"]));
        let mut owner=structure::Structure::default();let result=owner.run_command("integration.current-file-channel-occurrences",&host.invocation(),None).unwrap().unwrap();assert!(result.contains("Java channel occurrence"));assert_eq!(host.opened_locations().len(),1);assert_eq!(host.editor_text(),d.text);
        assert!(host.process_runs().is_empty()&&host.requests().is_empty());
    }
    #[test]
    fn registered_positive_java_hover_is_current_source_only_with_exact_utf16_ranges() {
        let h=MockHost::install().with_capabilities(&[]);let mut owner=structure::Structure::default();
        let d=doc("@Bean MessageChannel orders(){return null;}\r\n/* 😀 café */ @ServiceActivator(inputChannel=\"orders\", outputChannel=\"orders\") void consume(){}");
        let position=at(&d,"orders");let hover=owner.hover(&d,position).unwrap();
        assert!(hover.markdown.contains("2 reference"));assert!(hover.markdown.contains("Declared by a `@Bean`"));
        let r=hover.range.unwrap();assert_eq!(r.start,position);assert_eq!(r.end.character-r.start.character,6);
        let declaration=openidea_plugin::text::position(&d.text,d.text.find("orders()").unwrap());
        assert!(owner.hover(&d,declaration).is_some());
        let changed=Document{text:d.text.replace("MessageChannel orders()","MessageChannel changed()"),..d};
        assert!(owner.hover(&changed,at(&changed,"orders")).is_none());
        assert!(h.requests().is_empty()&&h.process_runs().is_empty());
    }
    #[test]
    fn full_qualified_annotation_type_and_explicit_bean_name_show_only_exact_written_source() {
        let _h=MockHost::install().with_capabilities(&[]);let mut owner=structure::Structure::default();
        let d=doc("@org.springframework.context.annotation.Bean(\"orders\") org.springframework.messaging.MessageChannel method(){return null;}\r\n@org.springframework.integration.annotation.ServiceActivator(inputChannel=\"orders\") void consume(){}");
        assert!(owner.hover(&d,at(&d,"orders")).unwrap().markdown.contains("1 reference"));
        let method=openidea_plugin::text::position(&d.text,d.text.find("method()").unwrap());
        assert!(owner.hover(&d,method).is_none(),"explicit bean name replaces the default method spelling");
    }
    #[test]
    fn dynamic_escaped_array_alias_custom_annotation_and_type_shadow_shapes_stay_quiet() {
        let _h=MockHost::install();let mut owner=structure::Structure::default();
        let body="@Bean MessageChannel orders(){return null;} @ServiceActivator(inputChannel=\"orders\") void consume(){}";
        let original=doc(body);
        for text in [
            original.text.replace("@Bean MessageChannel", "@Bean(\"ord\"+\"ers\") MessageChannel"),
            original.text.replace("@Bean MessageChannel", "@Bean({\"orders\",\"alias\"}) MessageChannel"),
            original.text.replace("@Bean MessageChannel", "@Bean(\"ord\\u0065rs\") MessageChannel"),
            original.text.replace("org.springframework.context.annotation.Bean;", "other.Bean;"),
            original.text.replace("org.springframework.messaging.MessageChannel;", "other.MessageChannel;"),
            original.text.replace("org.springframework.integration.annotation.ServiceActivator;", "other.ServiceActivator;"),
            original.text.replace("@Configuration class Channels", "class MessageChannel {} @Configuration class Channels"),
            original.text.replace("@Bean MessageChannel", "@Bean <MessageChannel> MessageChannel"),
            original.text.replace("class Channels", "class Channels<T>"),
            original.text.replace("class Channels", "class Channels extends Parent"),
        ] {
            let d=Document{text,..original.clone()};assert!(owner.hover(&d,at(&d,"orders")).is_none(),"{}",d.text);
        }
    }
    #[test]
    fn competing_literal_registrations_and_missing_channels_never_claim_positive_owner() {
        let _h=MockHost::install();let mut owner=structure::Structure::default();
        for body in [
            "@Bean MessageChannel orders(){return null;} @Bean(name=\"orders\") String other(){return null;} @ServiceActivator(inputChannel=\"orders\") void consume(){}",
            "@Bean(\"orders\") MessageChannel one(){return null;} @Bean(\"orders\") MessageChannel two(){return null;} @ServiceActivator(inputChannel=\"orders\") void consume(){}",
            "@ServiceActivator(inputChannel=\"orders\") void consume(){}",
        ] {
            let d=doc(body);assert!(owner.hover(&d,at(&d,"orders")).is_none());
        }
        let original=doc("@Bean MessageChannel orders(){return null;} @ServiceActivator(inputChannel=\"orders\") void consume(){}");
        let d=Document{text:original.text.replace("@Configuration class", "@Configuration(\"orders\") class"),..original};
        assert!(owner.hover(&d,at(&d,"orders")).is_none(),"configuration bean literal also occupies the spelling");
    }
    #[test]
    fn unsupported_paths_languages_unclosed_source_and_bounds_return_no_positive_hover() {
        let _h=MockHost::install();let mut owner=structure::Structure::default();let original=doc("@Bean MessageChannel orders(){return null;} @ServiceActivator(inputChannel=\"orders\") void consume(){}");
        for d in [
            Document{path:"/project/target/app/Channels.java".into(),..original.clone()},
            Document{language:"kotlin".into(),..original.clone()},
            Document{text:original.text.trim_end_matches('}').into(),..original.clone()},
            Document{text:format!("{}{}",original.text," ".repeat(13<<10)),..original.clone()},
        ] { assert!(owner.hover(&d,at(&d,"orders")).is_none()); }
        let token=hover(&original,at(&original,"orders")).unwrap().range.unwrap();assert!(owner.hover(&original,token.end).is_none());
    }
}
