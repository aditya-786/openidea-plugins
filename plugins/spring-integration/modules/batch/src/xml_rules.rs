use crate::NAMESPACE;
use openidea_plugin::{Diagnostic,Severity};
use spring_text::xml::{self, Element};
use spring_text::range;
use spring_text::xml_check::{Rule, check};

const BOOLEAN: &[&str] = &["true", "false"];
const XSD_BOOLEAN:&[&str]=&["true","false","1","0"];
const ISOLATION: &[&str] = &["DEFAULT", "READ_UNCOMMITTED", "READ_COMMITTED", "REPEATABLE_READ", "SERIALIZABLE"];
const PROPAGATION: &[&str] = &["REQUIRED", "REQUIRES_NEW", "SUPPORTS", "NOT_SUPPORTED", "MANDATORY", "NEVER", "NESTED"];

/// Written by hand from the Spring Batch XSD: only elements listed completely.
const RULES: &[Rule] = &[
    Rule { element: "job", required: &[], enums: &[("restartable", BOOLEAN), ("abstract", XSD_BOOLEAN)], refs: &[] },
    Rule { element: "step", required: &[], enums: &[("abstract", XSD_BOOLEAN), ("allow-start-if-complete", BOOLEAN)], refs: &[] },
    Rule { element: "flow", required: &[], enums: &[("abstract", XSD_BOOLEAN)], refs: &[] },
    Rule { element: "listeners", required: &[], enums: &[("merge", XSD_BOOLEAN)], refs: &[] },
    Rule { element: "retry-listeners", required: &[], enums: &[("merge", XSD_BOOLEAN)], refs: &[] },
    Rule { element: "streams", required: &[], enums: &[("merge", XSD_BOOLEAN)], refs: &[] },
    Rule { element: "skippable-exception-classes", required: &[], enums: &[("merge", XSD_BOOLEAN)], refs: &[] },
    Rule { element: "retryable-exception-classes", required: &[], enums: &[("merge", XSD_BOOLEAN)], refs: &[] },
    Rule { element: "no-rollback-exception-classes", required: &[], enums: &[("merge", XSD_BOOLEAN)], refs: &[] },
    Rule { element: "transaction-attributes", required: &[], enums: &[("isolation", ISOLATION), ("propagation", PROPAGATION)], refs: &[] },
    Rule { element: "include", required: &["class"], enums: &[], refs: &[] },
    Rule { element: "exclude", required: &["class"], enums: &[], refs: &[] },
    Rule { element: "stream", required: &["ref"], enums: &[], refs: &[] },
    Rule { element: "split", required: &["id"], enums: &[], refs: &[] },
    Rule { element: "decision", required: &["id", "decider"], enums: &[], refs: &[] },
    Rule { element: "next", required: &["on", "to"], enums: &[], refs: &[] },
    Rule { element: "end", required: &["on"], enums: &[], refs: &[] },
    Rule { element: "fail", required: &["on"], enums: &[], refs: &[] },
    Rule { element: "stop", required: &["on", "restart"], enums: &[], refs: &[] },
    Rule { element: "tasklet", required: &[], enums: &[("allow-start-if-complete", BOOLEAN)], refs: &[] },
    Rule {
        element: "chunk",
        required: &[],
        enums: &[("reader-transactional-queue", BOOLEAN), ("processor-transactional", BOOLEAN)],
        refs: &[],
    },
];

#[derive(Clone,Copy,PartialEq,Eq)]
enum Schema {Five,Six}
fn namespace_of_attribute<'a>(all:&'a [Element],mut i:usize,name:&str)->Option<&'a str> {
    let (prefix,_)=name.split_once(':')?;let declaration=format!("xmlns:{prefix}");
    loop {if let Some(a)=all[i].attrs.iter().find(|a|a.name==declaration) {return Some(&a.value);};i=all[i].parent?;}
}
fn schema(all:&[Element],mut i:usize,compatibility:Option<Schema>)->Option<Schema> {
    loop {
        for attr in all[i].attrs.iter().filter(|a|a.local()=="schemaLocation") {
            if namespace_of_attribute(all,i,&attr.name)!=Some("http://www.w3.org/2001/XMLSchema-instance") {continue;}
            let words:Vec<_>=attr.value.split_whitespace().collect();if words.len()%2!=0 {return None;}
            let locations:Vec<_>=words.chunks_exact(2).filter(|pair|pair[0]==NAMESPACE).map(|pair|pair[1]).collect();
            if let [location]=locations.as_slice() {
                let tail=location.strip_prefix("https://www.springframework.org/schema/batch/").or_else(||location.strip_prefix("http://www.springframework.org/schema/batch/"))?;
                return (tail=="spring-batch.xsd").then_some(compatibility).flatten();
            }
            if !locations.is_empty() {return None;}
        }
        let Some(parent)=all[i].parent else{return compatibility;};i=parent;
    }
}
fn required(all:&[Element],i:usize)->&'static [&'static str] {
    let element=&all[i];let parent=element.parent.map(|p|(all[p].local(),xml::namespace(all,p)));
    let top=parent.is_none()||parent==Some(("beans",Some("http://www.springframework.org/schema/beans")));
    match element.local() {
        "job" if top=>&["id"],
        "step" if top||matches!(parent,Some(("job"|"flow",Some(NAMESPACE))))=>&["id"],
        "flow" if top=>&["id"],
        "flow" if parent==Some(("step",Some(NAMESPACE)))=>&["parent"],
        "flow" if matches!(parent,Some(("job"|"flow",Some(NAMESPACE))))=>&["id","parent"],
        _=>&[],
    }
}
fn filled<'a>(element:&'a Element,name:&str)->Option<&'a spring_text::xml::Attr> {element.attrs.iter().find(|a|a.name==name&&!a.value.trim().is_empty())}
/// Mirrors Spring Batch's ChunkElementParser and TaskletParser errors (XSD documents "but not both" for chunk; tasklet is a one-of).
/// A `#{}` commit-interval becomes a policy bean in the parser, and `${}` values are unresolved, so both stay quiet.
fn exclusive_choice_problems(text:&str,all:&[Element],i:usize)->Vec<Diagnostic> {
    let element=&all[i];let mut out=Vec::new();
    match element.local() {
        "chunk" => {
            let (Some(interval),Some(_))=(filled(element,"commit-interval"),filled(element,"chunk-completion-policy")) else{return out;};
            let mut at=Some(i);
            while let Some(e)=at.map(|a|&all[a]) {if e.local()=="step"&&(e.attr("parent").is_some()||e.attr("abstract").is_some()) {return out;}at=e.parent;}
            if interval.value.trim_start().starts_with('#')||interval.value.contains("${") {return out;}
            out.push(Diagnostic::new(range(text,element.at,element.at+element.name.len()),Severity::Warning,"A chunk must contain either commit-interval or chunk-completion-policy, but not both.").code("chunk-commit-interval-and-policy"));
        }
        "tasklet" => {
            let beans=Some("http://www.springframework.org/schema/beans");
            let children=all.iter().enumerate().filter(|(j,c)|c.parent==Some(i)&&(c.local()=="chunk"&&xml::namespace(all,*j)==Some(NAMESPACE)||matches!(c.local(),"bean"|"ref")&&xml::namespace(all,*j)==beans)).count();
            if usize::from(filled(element,"ref").is_some())+children>1 {
                out.push(Diagnostic::new(range(text,element.at,element.at+element.name.len()),Severity::Warning,"A tasklet may have only one of: ref attribute, chunk element, bean element or ref element.").code("tasklet-multiple-implementations"));
            }
        }
        _=>{}
    }
    out
}
/// Context-aware invariant rules; version-specific differences are emitted only
/// for an explicit project compatibility setting (5/6). Modern official XSD
/// URLs are unversioned and cannot establish the installed runtime version.
/// Unknown explicit schema URLs suppress version-specific assumptions.
#[cfg(test)]
pub fn problems(text:&str)->Vec<Diagnostic> {configured_problems(text,None)}
pub fn configured_problems(text:&str,compatibility:Option<&str>)->Vec<Diagnostic> {
    if !spring_text::xml::analysis_valid(text) {return Vec::new();}
    let compatibility=match compatibility {Some("5")=>Some(Schema::Five),Some("6")=>Some(Schema::Six),_=>None};
    let all=xml::elements(text);let mut out=Vec::new();let mut deprecated=false;
    for (i,element) in all.iter().enumerate().filter(|(i,_)|xml::namespace(&all,*i)==Some(NAMESPACE)) {
        if !crate::owned_batch_context(&all,i) {continue;}
        if let Some(mut rule)=RULES.iter().find(|r|r.element==element.local()).copied() {
            if matches!(element.local(),"job"|"step"|"flow") {rule.required=required(&all,i);}
            let mut source=element.clone();source.attrs.retain(|a|!a.name.contains(':'));
            out.extend(check(std::slice::from_ref(&source),&[rule],|_|true).iter().map(|f|f.diagnostic(text)));
            if rule.required.contains(&"id") {
                if let Some(id)=source.attrs.iter().find(|a|a.name=="id"&&a.value.trim().is_empty()) {
                    out.push(Diagnostic::new(range(text,id.value_at,id.value_end),Severity::Warning,"A Batch declaration ID must not be empty.").code("invalid-attribute-value"));
                }
            }
        }
        out.extend(exclusive_choice_problems(text,&all,i));
        let version=schema(&all,i,compatibility);
        if version==Some(Schema::Six)&&!deprecated {
            deprecated=true;out.push(Diagnostic::new(range(text,element.at,element.at+element.name.len()),Severity::Hint,"With selected Spring Batch 6 compatibility, the XML namespace is deprecated; plan a Java configuration migration before Batch 7.").code("batch-xml-deprecated"));
        }
        for attr in &element.attrs {
            let subjob=element.local()=="job"&&element.parent.is_some_and(|p|all[p].local()=="step"&&xml::namespace(&all,p)==Some(NAMESPACE));
            let removed=version==Some(Schema::Six)&&(element.local()=="tasklet"&&attr.name=="throttle-limit"||subjob&&attr.name=="job-launcher");
            let introduced=version==Some(Schema::Five)&&subjob&&attr.name=="job-operator";
            if removed||introduced {
                let message=if removed {format!("Selected Batch 6 compatibility does not support @{}{}.",attr.name,if attr.name=="job-launcher" {"; use job-operator with an appropriate JobOperator bean"}else{""})}else {"Selected Batch 5 compatibility does not support @job-operator; it uses job-launcher.".into()};
                out.push(Diagnostic::new(range(text,attr.value_at,attr.value_end),Severity::Warning,message).code("unsupported-batch-schema-attribute"));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn official(body:&str)->String {
        format!("<beans xmlns='http://www.springframework.org/schema/beans' xmlns:b='{NAMESPACE}' xmlns:s='http://www.w3.org/2001/XMLSchema-instance' s:schemaLocation='{NAMESPACE} https://www.springframework.org/schema/batch/spring-batch.xsd'>{body}</beans>")
    }
    #[test]
    fn schema_major_differences_require_explicit_project_compatibility() {
        let body="<b:job id='job'><b:step id='sub'><b:job ref='other' job-launcher='launcher' job-operator='operator'/></b:step><b:step id='work'><b:tasklet ref='worker' throttle-limit='4'/></b:step></b:job>";
        let five=configured_problems(&official(body),Some("5"));assert_eq!(five.len(),1);assert!(five[0].message.contains("@job-operator"));
        let six=configured_problems(&official(body),Some("6"));assert_eq!(six.len(),3);assert_eq!(six.iter().filter(|d|d.code.as_deref()==Some("batch-xml-deprecated")).count(),1);assert!(six.iter().any(|d|d.message.contains("@throttle-limit")));assert!(six.iter().any(|d|d.message.contains("@job-launcher")));
        let unknown=official(body).replace("https://www.springframework.org/schema/batch/","https://example.com/schema/batch/");assert!(configured_problems(&unknown,Some("6")).is_empty());
        assert!(configured_problems(&official(body),Some("auto")).is_empty());
        let false_version=official(body).replace("spring-batch.xsd","spring-batch-6.0.xsd");assert!(problems(&false_version).is_empty());
        let foreign=official(body).replace("xmlns:s='http://www.w3.org/2001/XMLSchema-instance'","xmlns:s='urn:foreign'");assert!(problems(&foreign).is_empty());
    }
    #[test]
    fn inline_subjobs_partition_steps_and_split_flows_have_contextual_identity_rules() {
        let good=format!("<beans xmlns='http://www.springframework.org/schema/beans' xmlns:b='{NAMESPACE}'><b:flow id='template' abstract='1'/><b:job id='job'><b:split id='parallel'><b:flow><b:step id='one'><b:job ref='subjob'/></b:step></b:flow></b:split><b:step id='partitioned'><b:partition><b:step><b:tasklet ref='worker'/></b:step></b:partition></b:step><b:step id='flowStep'><b:flow parent='template'/></b:step><b:flow id='reference' parent='template'/></b:job></beans>");
        assert!(problems(&good).is_empty(),"{:?}",problems(&good));
        let bad=good.replace("<b:flow id='template' abstract='1'/>","<b:flow abstract='maybe'/>").replace("<b:flow id='reference' parent='template'/>","<b:flow/>").replace("<b:flow parent='template'/>","<b:flow/>");
        let diagnostics=problems(&bad);assert_eq!(diagnostics.iter().filter(|d|d.code.as_deref()==Some("missing-attribute")).count(),4);assert_eq!(diagnostics.iter().filter(|d|d.code.as_deref()==Some("invalid-attribute-value")).count(),1);
    }
    #[test]
    fn merge_values_are_schema_booleans_and_foreign_attributes_do_not_supply_ids() {
        let body="<b:job id='j'><b:listeners merge='1'/><b:step id='s'><b:tasklet><b:chunk commit-interval='1'><b:streams merge='0'/><b:retry-listeners merge='sometimes'/></b:chunk></b:tasklet></b:step></b:job>";
        let text=official(body);let found=problems(&text);assert_eq!(found.len(),1);assert_eq!(found[0].code.as_deref(),Some("invalid-attribute-value"));
        let text=format!("<b:job xmlns:b='{NAMESPACE}' xmlns:f='urn:other' f:id='foreign' f:restartable='maybe'/>");let found=problems(&text);assert_eq!(found.len(),1);assert_eq!(found[0].code.as_deref(),Some("missing-attribute"));
        assert!(problems(&text.replace("/>",">")).is_empty());
    }
    #[test]
    fn required_ids_cannot_be_blank_but_runtime_values_do_not_invent_source_requirements() {
        let text=official("<b:job id=' '><b:step id='s'><b:job ref=''/></b:step><b:split id='parallel'><b:flow><b:step id=' '/></b:flow></b:split></b:job>");
        let found=problems(&text);assert_eq!(found.len(),2);assert!(found.iter().all(|d|d.code.as_deref()==Some("invalid-attribute-value")));
    }
    #[test]
    fn chunk_and_tasklet_exclusive_choices_are_reported_only_when_provable() {
        let codes=|body:&str|problems(&official(body)).iter().filter_map(|d|d.code.clone()).collect::<Vec<_>>();
        assert_eq!(codes("<b:job id='j'><b:step id='s'><b:tasklet><b:chunk commit-interval='5' chunk-completion-policy='p'/></b:tasklet></b:step></b:job>"),["chunk-commit-interval-and-policy"]);
        assert!(codes("<b:job id='j'><b:step id='s'><b:tasklet><b:chunk commit-interval='#{jobParameters[n]}' chunk-completion-policy='p'/></b:tasklet></b:step></b:job>").is_empty());
        assert!(codes("<b:job id='j'><b:step id='s'><b:tasklet><b:chunk commit-interval='${n}' chunk-completion-policy='p'/></b:tasklet></b:step></b:job>").is_empty());
        assert!(codes("<b:job id='j'><b:step id='s' parent='base'><b:tasklet><b:chunk commit-interval='5' chunk-completion-policy='p'/></b:tasklet></b:step></b:job>").is_empty());
        assert_eq!(codes("<b:job id='j'><b:step id='s'><b:tasklet ref='t'><b:chunk commit-interval='1'/></b:tasklet></b:step></b:job>"),["tasklet-multiple-implementations"]);
        assert_eq!(codes("<b:job id='j'><b:step id='s'><b:tasklet><b:chunk commit-interval='1'/><b:chunk commit-interval='1'/></b:tasklet></b:step></b:job>"),["tasklet-multiple-implementations"]);
        assert!(codes("<b:job id='j'><b:step id='s'><b:tasklet ref='t'/></b:step><b:step id='u'><b:tasklet ref=' '><b:chunk commit-interval='1'/></b:tasklet></b:step></b:job>").is_empty());
    }
    #[test]
    fn checks_transaction_attributes_and_exception_classes() {
        let xml = r#"<job id="j" xmlns="http://www.springframework.org/schema/batch"><step id="s"><tasklet><transaction-attributes isolation="SERIAL" propagation="NESTED"/><chunk commit-interval="1"><skippable-exception-classes><include/><exclude class="E"/></skippable-exception-classes><streams><stream/></streams></chunk></tasklet></step></job>"#;
        let found: Vec<_> = problems(xml).iter().map(|d| (d.code.clone().unwrap(), xml[..].lines().next().map(|l| l[d.range.start.character as usize..d.range.end.character as usize].to_owned()).unwrap())).collect();
        assert_eq!(
            found,
            [
                ("invalid-attribute-value".to_owned(), "SERIAL".to_owned()),
                ("missing-attribute".into(), "include".into()),
                ("missing-attribute".into(), "stream".into()),
            ]
        );
    }
}
