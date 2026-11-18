//! Current-document transition candidates; no discovery, source reads or Rename graph.
use crate::*;
use openidea_plugin::hover::Hover;
const NS: &str = "http://www.springframework.org/schema/batch";
const MAX_TEXT: usize = 32 * 1024;
const STATES: &[&str] = &["step", "flow", "split", "decision"];
struct Candidate {
    name: String,
    range: Range,
}
struct Model {
    value: String,
    range: Range,
    scope: String,
    candidates: Vec<Candidate>,
    eligible: bool,
}
use spring_text::provenance::literal;
fn model(doc: &Document, at: Position) -> Option<Model> {
    if doc.language != "xml"
        || !doc.path.ends_with(".xml")
        || !doc.text.contains(NS)
        || beans::ignored(&doc.path)
    {
        return None;
    }
    let empty = || Model {
        value: String::new(),
        range: Range::new(at, at),
        scope: String::new(),
        candidates: Vec::new(),
        eligible: false,
    };
    if doc.text.len() > MAX_TEXT || doc.text.contains('&') {
        return Some(empty());
    }
    let unprefixed = doc.text.strip_prefix('\u{feff}').unwrap_or(&doc.text);
    let declaration_misplaced = unprefixed.contains("<?xml") && !unprefixed.starts_with("<?xml");
    if !spring_text::xml::analysis_valid(&doc.text) || doc.text.to_ascii_lowercase().contains("<!doctype") || declaration_misplaced {
        return Some(empty());
    }
    let elements = xml::elements(&doc.text);
    if elements.len() > 512 || elements.iter().map(|e| e.attrs.len()).sum::<usize>() > 256 {
        return Some(empty());
    }
    let offset = openidea_plugin::text::offset(&doc.text, at)?;
    let (index, attribute) = elements
        .iter()
        .enumerate()
        .filter(|(i, _)| xml::namespace(&elements, *i) == Some(NS))
        .find_map(|(i, e)| {
            e.attrs
                .iter()
                .find(|a| {
                    a.value_at <= offset
                        && offset <= a.value_end
                        && matches!(a.local(), "next" | "to" | "restart")
                })
                .map(|a| (i, a))
        })?;
    let mut result = Model {
        value: attribute.value.clone(),
        range: range(&doc.text, attribute.value_at, attribute.value_end),
        ..empty()
    };
    let element = &elements[index];
    if attribute.name != attribute.local()
        || !(attribute.name == "next" && STATES.contains(&element.local())
            || attribute.name == "to" && element.local() == "next"
            || attribute.name == "restart" && element.local() == "stop")
    {
        return Some(result);
    }
    let mut owner = Some(index);
    while let Some(i) = owner {
        if xml::namespace(&elements, i) != Some(NS) {
            return Some(result);
        }
        if STATES.contains(&elements[i].local()) {
            break;
        }
        owner = elements[i].parent;
    }
    let owner = owner?;
    let Some(scope) = elements[owner].parent else {
        return Some(result);
    };
    if xml::namespace(&elements, scope) != Some(NS)
        || !matches!(elements[scope].local(), "job" | "flow")
    {
        return Some(result);
    }
    result.scope = format!(
        "{} at source line {}",
        elements[scope].local(),
        range(&doc.text, elements[scope].start, elements[scope].start)
            .start
            .line
            + 1
    );
    let mut counts = BTreeMap::<String, usize>::new();
    for (i, e) in elements
        .iter()
        .enumerate()
        .filter(|(_, e)| e.parent == Some(scope) && STATES.contains(&e.local()))
    {
        if xml::namespace(&elements, i) != Some(NS) {
            continue;
        }
        if let Some(id) = e
            .attrs
            .iter()
            .find(|a| a.name == "id")
            .filter(|a| literal(&a.value))
        {
            *counts.entry(id.value.clone()).or_default() += 1;
        }
    }
    for (i, e) in elements
        .iter()
        .enumerate()
        .filter(|(_, e)| e.parent == Some(scope) && STATES.contains(&e.local()))
    {
        if xml::namespace(&elements, i) != Some(NS) {
            continue;
        }
        if let Some(id) = e
            .attrs
            .iter()
            .find(|a| a.name == "id")
            .filter(|a| counts.get(&a.value) == Some(&1))
        {
            result.candidates.push(Candidate {
                name: id.value.clone(),
                range: range(&doc.text, id.value_at, id.value_end),
            });
        }
    }
    result.eligible = result.value.is_empty() || literal(&result.value);
    Some(result)
}
/// Strict source-action subset: replace one existing direct step next value.
/// Inherited and conditional transition configurations are not rewritten.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct LinkContext {
    pub range: Range,
    pub insert: bool,
    pub value: String,
    pub scope: String,
    pub owner: String,
    pub candidates: Vec<String>,
}
pub(crate) fn link_context(doc: &Document, at: Position) -> Option<LinkContext> {
    let model = model(doc, at)?;
    if !model.eligible || model.candidates.is_empty() { return None; }
    let offset = openidea_plugin::text::offset(&doc.text, at)?;
    let elements = xml::elements(&doc.text);
    let (index, step) = elements.iter().enumerate().find(|(_, e)| e.attrs.iter().any(|a| a.name == "next" && a.value_at <= offset && offset <= a.value_end))?;
    if step.local() != "step" || xml::namespace(&elements, index) != Some(NS)
        || step.attrs.iter().any(|a| matches!(a.local(), "parent" | "abstract"))
        || elements.iter().enumerate().any(|(i,e)| e.parent == Some(index) && xml::namespace(&elements,i) == Some(NS) && matches!(e.local(), "next" | "end" | "fail" | "stop")) { return None; }
    let scope = step.parent?;
    if elements[scope].attrs.iter().any(|a| matches!(a.local(), "parent" | "abstract")) { return None; }
    let owner = step.attrs.iter().find(|a| a.name == "id" && literal(&a.value))?.value.clone();
    // A duplicate physical source-state owner cannot authorize a mutation.
    if !model.candidates.iter().any(|c| c.name == owner) { return None; }
    let candidates: Vec<_> = model.candidates.into_iter().filter(|c| c.name != owner && c.name != model.value)
        .filter(|c| elements.iter().enumerate().any(|(i,e)| e.parent == Some(scope) && xml::namespace(&elements,i) == Some(NS) && STATES.contains(&e.local()) && e.value("id") == Some(c.name.as_str()) && !e.attrs.iter().any(|a| matches!(a.local(), "parent" | "abstract"))))
        .map(|c| c.name).collect();
    if candidates.is_empty() { return None; }
    Some(LinkContext { range: model.range, insert: false, value: model.value, scope: model.scope, owner, candidates })
}
/// Offer absent-next insertion only at the selected step opening tag. Validate
/// the would-be attribute with the same strict model, then retain an insertion
/// range in the original source rather than publishing synthetic coordinates.
pub(crate) fn action_context(doc: &Document, at: Position) -> Option<LinkContext> {
    if let Some(context) = link_context(doc, at) { return Some(context); }
    if doc.language != "xml" || !doc.path.ends_with(".xml") || doc.text.len() > MAX_TEXT.saturating_sub(8) { return None; }
    let offset = openidea_plugin::text::offset(&doc.text, at)?;
    let elements = xml::elements(&doc.text);
    let (index, step) = elements.iter().enumerate().find(|(_, e)| e.start <= offset && offset < e.tag_end)?;
    if step.local() != "step" || xml::namespace(&elements, index) != Some(NS)
        || step.attrs.iter().any(|a| a.local() == "next") { return None; }
    let close = step.tag_end.checked_sub(if doc.text[..step.tag_end].ends_with("/>") { 2 } else { 1 })?;
    if close <= step.start || doc.text.as_bytes().get(step.tag_end - 1) != Some(&b'>') { return None; }
    let mut synthetic = doc.clone(); synthetic.text.insert_str(close, " next=''");
    let synthetic_at = openidea_plugin::text::position(&synthetic.text, close + 7);
    let context = link_context(&synthetic, synthetic_at)?;
    Some(LinkContext { range: range(&doc.text, close, close), insert: true, ..context })
}
pub(crate) fn definition(doc: &Document, at: Position) -> Option<Vec<Location>> {
    let model = model(doc, at)?;
    Some(if model.eligible {
        model
            .candidates
            .iter()
            .find(|c| c.name == model.value)
            .map(|c| {
                vec![Location {
                    path: doc.path.clone(),
                    range: c.range,
                }]
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    })
}
pub(crate) fn completion(doc: &Document, at: Position) -> Option<Vec<CompletionItem>> {
    let model = model(doc, at)?;
    Some(if model.eligible {
        model
            .candidates
            .into_iter()
            .map(|c| CompletionItem {
                detail: Some("unique direct state in this source job/flow".into()),
                replace: Some(model.range),
                ..CompletionItem::new(&c.name, CompletionKind::Reference)
            })
            .collect()
    } else {
        Vec::new()
    })
}
pub(crate) fn hover(doc: &Document, at: Position) -> Option<Hover> {
    let model = model(doc, at)?;
    if !model.eligible || !literal(&model.value) || at >= model.range.end {
        return None;
    }
    let target=model.candidates.iter().find(|c|c.name==model.value).map(|c|format!("One supported direct state declaration at line {}.",c.range.start.line+1)).unwrap_or_else(||"No unique supported direct state declaration; inherited/imported/nested contexts remain unresolved.".into());
    Some(Hover::new(format!("**Spring Batch transition** `{}`\n\nSource scope: {}. {target}\n\nCurrent XML source candidates only; not runtime flow reachability or complete Find Usages/Rename.",model.value,model.scope)).range(model.range))
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{hover::HoverProvider, mock::MockHost};
    fn doc(body: &str) -> Document {
        Document {
            path: "/project/jobs.xml".into(),
            language: "xml".into(),
            version: 3,
            text: format!(
                "<beans xmlns='http://www.springframework.org/schema/beans' xmlns:b='{NS}'>{body}</beans>"
            ),
        }
    }
    fn at(d: &Document, value: &str) -> Position {
        openidea_plugin::text::position(&d.text, d.text.find(value).unwrap())
    }
    #[test]
    fn cold_registered_owner_resolves_only_the_current_job_and_utf16_source() {
        let _h = MockHost::install().with_capabilities(&[]);
        let mut owner = structure::Structure::default();
        let d = doc(
            "<!-- 😀 --><b:job id='one'><b:step id='a' next='finish'/><b:step id='finish'/></b:job><b:job id='two'><b:step id='finish'/></b:job>",
        );
        let pos = at(&d, "finish");
        let found = owner.definition(&d, pos);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, d.path);
        assert_eq!(
            found[0].range.start,
            openidea_plugin::text::position(&d.text, d.text.find("id='finish'").unwrap() + 4)
        );
        let hover = owner.hover(&d, pos).unwrap();
        assert_eq!(hover.range.unwrap().start, pos);
        let items = owner.completion(&d, pos, None);
        assert_eq!(items.iter().filter(|i| i.label == "finish").count(), 1);
        assert!(items.iter().all(
            |i| i.replace == Some(Range::new(pos, Position::new(pos.line, pos.character + 6)))
        ));
    }
    #[test]
    fn lowercase_doctype_disables_navigation() {
        let _h = MockHost::install();
        let mut owner = structure::Structure::default();
        let d = doc(
            "<!doctype b:job []><b:job id='one'><b:step id='a' next='finish'/><b:step id='finish'/></b:job>",
        );
        assert!(owner.definition(&d, at(&d, "finish")).is_empty());
    }
    #[test]
    fn missing_target_never_falls_back_to_other_jobs_or_project_sources() {
        let h = MockHost::install();
        h.add_file(
            "other.xml",
            &format!("<job xmlns='{NS}' id='elsewhere'><step id='finish'/></job>"),
        );
        let mut owner = structure::Structure::default();
        let d = doc(
            "<b:job id='one'><b:step id='a' next='finish'/></b:job><b:job id='two'><b:step id='finish'/></b:job>",
        );
        assert!(owner.definition(&d, at(&d, "finish")).is_empty());
        let items = owner.completion(&d, at(&d, "finish"), None);
        assert!(items.iter().all(|i| i.label != "finish"));
        assert!(
            owner
                .hover(&d, at(&d, "finish"))
                .unwrap()
                .markdown
                .contains("No unique supported")
        );
    }
    #[test]
    fn nested_flow_namespace_and_duplicate_state_owners_are_not_conflated() {
        let _h = MockHost::install();
        let mut owner = structure::Structure::default();
        let d = doc(
            "<b:job id='j'><b:flow id='f'><b:step id='a' next='done'/><b:step id='done'/></b:flow><b:step id='done'/></b:job>",
        );
        let found = owner.definition(&d, at(&d, "done"));
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].range.start,
            openidea_plugin::text::position(&d.text, d.text.find("id='done'").unwrap() + 4)
        );
        let dup = doc(
            "<b:job id='j'><b:step id='a' next='done'/><b:step id='done'/><b:step id='done'/></b:job>",
        );
        assert!(owner.definition(&dup, at(&dup, "done")).is_empty());
        let foreign = Document {
            text: d.text.replace(NS, "urn:foreign"),
            ..d.clone()
        };
        assert!(owner.definition(&foreign, at(&foreign, "done")).is_empty());
        let qualified = Document {
            text: d.text.replace("next='done'", "b:next='done'"),
            ..d
        };
        assert!(
            owner
                .definition(&qualified, at(&qualified, "done"))
                .is_empty()
        );
    }
    #[test]
    fn unsaved_transitions_update_without_cache_and_malformed_dynamic_or_large_input_is_quiet() {
        let _h = MockHost::install();
        let mut owner = structure::Structure::default();
        let d = doc("<b:job id='j'><b:step id='a' next='done'/><b:step id='done'/></b:job>");
        assert_eq!(owner.definition(&d, at(&d, "done")).len(), 1);
        let changed = Document {
            version: 4,
            text: d.text.replace("id='done'", "id='renamed'"),
            ..d.clone()
        };
        assert!(owner.definition(&changed, at(&changed, "done")).is_empty());
        for text in [
            d.text.replace("</beans>", ""),
            d.text.replace("next='done'", "next='${target}'"),
            format!("{}{}", d.text, " ".repeat(MAX_TEXT)),
        ] {
            let invalid = Document { text, ..d.clone() };
            let position = at(
                &invalid,
                if invalid.text.contains("${target}") {
                    "${target}"
                } else {
                    "done"
                },
            );
            assert!(owner.definition(&invalid, position).is_empty());
            assert!(owner.hover(&invalid, position).is_none());
        }
    }
}
