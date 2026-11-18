//! Bounded current-text XML candidates, deliberately separate from host Rename.
use crate::*;
use openidea_plugin::hover::{Hover, HoverProvider};
const MAX_TEXT: usize = 32 << 10;
const MAX_TOKENS: usize = 256;
const BEANS: &str = "http://www.springframework.org/schema/beans";
#[derive(Clone)]
struct Token {
    name: String,
    range: Range,
    declaration: bool,
    collides: bool,
}
use spring_text::provenance::literal;
fn tokens(doc: &Document) -> Option<Vec<Token>> {
    if doc.language != "xml"
        || !doc.path.ends_with(".xml")
        || doc.text.len() > MAX_TEXT
        || beans::ignored(&doc.path)
    {
        return None;
    }
    if !spring_text::xml::analysis_valid(&doc.text) {
        return None;
    }
    let elements = xml::elements(&doc.text);
    let root = elements.first()?;
    if root.local() != "beans" || root.parent.is_some() || xml::namespace(&elements, 0) != Some(BEANS) {
        return None;
    }
    // Bean IDs, bean aliases and explicit alias registrations share the same
    // local name space. A channel token is not uniquely owned merely because
    // only one Integration channel element spells that name.
    let mut occupied = BTreeSet::new();
    for (i, element) in elements.iter().enumerate().skip(1) {
        let namespace = xml::namespace(&elements, i);
        if namespace == Some(NAMESPACE) {
            // Endpoint IDs are bean registrations too. Only the direct channel
            // declarations counted below can establish this local target.
            if !(element.parent == Some(0) && matches!(element.local(), "channel" | "publish-subscribe-channel")) {
                if let Some(id) = element.attr("id") { occupied.insert(id.value.as_str()); }
            }
        }
        if namespace != Some(BEANS) { continue; }
        match element.local() {
            "bean" => {
                if let Some(id) = element.attr("id") { occupied.insert(id.value.as_str()); }
                if let Some(names) = element.attr("name") {
                    occupied.extend(names.value.split(|c: char| c.is_ascii_whitespace() || matches!(c, ',' | ';')).filter(|name| !name.is_empty()));
                }
            }
            "alias" => {
                if let Some(alias) = element.attr("alias") { occupied.insert(alias.value.as_str()); }
            }
            _ => {}
        }
    }
    let mut result = Vec::new();
    for (i, e) in elements.iter().enumerate().filter(|(_, e)| e.parent == Some(0)) {
        if e.name.matches(':').count() > 1 {
            return None;
        }
        if xml::namespace(&elements, i) != Some(NAMESPACE) {
            continue;
        }
        for a in &e.attrs {
            let declaration = a.name == "id" && matches!(e.local(), "channel" | "publish-subscribe-channel");
            let endpoint = matches!(
                e.local(),
                "bridge"
                    | "service-activator"
                    | "transformer"
                    | "filter"
                    | "splitter"
                    | "aggregator"
                    | "router"
                    | "gateway"
                    | "inbound-channel-adapter"
                    | "outbound-channel-adapter"
                    | "chain"
            );
            let reference = endpoint
                && matches!(
                    a.name.as_str(),
                    "input-channel"
                        | "output-channel"
                        | "error-channel"
                        | "discard-channel"
                        | "default-output-channel"
                        | "default-reply-channel"
                        | "request-channel"
                        | "reply-channel"
                );
            if (!declaration && !reference) || !literal(&a.value) {
                continue;
            }
            if result.len() == MAX_TOKENS {
                return None;
            }
            result.push(Token { name: a.value.clone(), range: range(&doc.text, a.value_at, a.value_end), declaration, collides: occupied.contains(a.value.as_str()) });
        }
    }
    Some(result)
}
fn selected(tokens: &[Token], at: Position) -> Option<&Token> {
    tokens.iter().find(|t| t.range.start <= at && at < t.range.end)
}
pub(crate) fn definition(doc: &Document, at: Position) -> Option<Vec<Location>> {
    let tokens = tokens(doc)?;
    let token = selected(&tokens, at)?;
    let declarations: Vec<_> = tokens.iter().filter(|t| t.declaration && t.name == token.name).collect();
    if declarations.is_empty() && !token.collides {
        return None;
    }
    Some(if declarations.len() == 1 && !token.declaration && !token.collides {
        vec![Location { path: doc.path.clone(), range: declarations[0].range }]
    } else {
        Vec::new()
    })
}
fn integration_hover(doc:&Document,at:Position)->Option<Hover>{
        let tokens = tokens(doc)?;
        let token = selected(&tokens, at)?;
        let declarations: Vec<_> = tokens.iter().filter(|t| t.declaration && t.name == token.name).collect();
        let references = tokens.iter().filter(|t| !t.declaration && t.name == token.name).count();
        let ownership = if token.collides {
            "Another bean, endpoint ID or alias uses this name, so Go to Declaration is off.".into()
        } else { match declarations.as_slice() {
            [one] => format!("Declared in this file at line {}.", one.range.start.line + 1),
            [] => "Not declared in this file; it may be declared elsewhere in the project.".into(),
            _ => "Declared more than once in this file, so Go to Declaration is off.".into(),
        }};
        Some(Hover::new(format!("**Spring Integration channel** `{}`\n\n{ownership}\n\n{references} reference(s) in this file.",token.name)).range(token.range))
}
impl HoverProvider for crate::structure::Structure {
    fn hover(&mut self,doc:&Document,at:Position)->Option<Hover>{integration_hover(doc,at).or_else(||crate::batch_navigation::hover(doc,at)).or_else(||crate::java_hover::hover(doc,at))}
}

fn current_source(ctx: &Invocation) -> Result<(String, Document, Position), Error> {
    let root = ctx.project().ok_or("Open a project first.")?.root()?;
    let editor = ctx.editor().ok_or_else(|| format!("Open a Java or XML file and place the caret on a channel name. {}", ui_kit::denied::message("Find Channel Usages in This File", "editor.read", "Spring Structure Tools")))?;
    let current = editor.document()?;
    let at = editor.selections()?.first().map(|selection| selection.start).ok_or(Error::Stale)?;
    let doc = Document { path: editor.path(), language: editor.language(), text: current.text, version: current.version };
    if spring_text::index::owning_root(&doc.path).as_deref() != Some(root.as_str()) {
        return Err(Error::Stale);
    }
    Ok((root, doc, at))
}
fn unchanged_source(ctx: &Invocation, root: &str, doc: &Document) -> Result<(), Error> {
    let (current_root, current, _) = current_source(ctx)?;
    if current_root != root || current != *doc { return Err(Error::Stale); }
    Ok(())
}
/// Physical current-file occurrences, never a global usages or Rename graph.
pub(crate) fn occurrences(ctx: &Invocation) -> Result<Option<String>, Error> {
    let (root, doc, at) = current_source(ctx)?;
    let tokens = tokens(&doc).ok_or("Channel usages work in Spring XML and Java files. This file isn't one, can't be parsed, or is too large.")?;
    let token = selected(&tokens, at).ok_or("Place the caret on a channel name.")?;
    if token.collides || tokens.iter().filter(|t| t.declaration && t.name == token.name).count() != 1 {
        return Err("Needs exactly one declaration in this file and no other bean with the same name.".into());
    }
    let locations: Vec<_> = tokens.iter().filter(|t| t.name == token.name).collect();
    let items: Vec<_> = locations.iter().enumerate().map(|(i, t)| {
        openidea_plugin::PickItem::new(&i.to_string(), &format!("{} · line {}", if t.declaration { "Channel declaration" } else { "Channel reference" }, t.range.start.line + 1))
            .description(&format!("Current file · UTF-16 column {}", t.range.start.character + 1))
    }).collect();
    let Some(choice) = host::pick(&format!("Current file only: {} · {} literal occurrences", token.name, items.len()), &items)? else { return Ok(None); };
    let destination = choice.parse::<usize>().ok().and_then(|i| locations.get(i)).ok_or(Error::Stale)?.range.start;
    unchanged_source(ctx, &root, &doc)?;
    host::open_location(&doc.path, destination.line, destination.character)?;
    Ok(Some("Opened the occurrence in this file.".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{hover::HoverProvider, mock::MockHost};
    fn doc(body: &str) -> Document {
        Document {
            path: "/project/channels.xml".into(),
            language: "xml".into(),
            text: format!("<beans xmlns=\"{BEANS}\" xmlns:i=\"{NAMESPACE}\">{body}</beans>"),
            version: 1,
        }
    }
    fn at(doc: &Document, name: &str) -> Position {
        spring_text::range(&doc.text, doc.text.rfind(name).unwrap(), doc.text.rfind(name).unwrap() + name.len()).start
    }
    #[test]
    fn unsaved_utf16_exact_local_declaration_without_project_or_provider_io() {
        let host = MockHost::install();
        host.set_project_root(None);
        let doc = doc("<!--😀--><i:channel id=\"orders\"/><i:bridge input-channel=\"orders\" output-channel=\"done\"/>");
        let found = definition(&doc, at(&doc, "orders")).unwrap();
        assert_eq!(found.len(), 1);
        let a = spring_text::xml::elements(&doc.text)[1].attr("id").unwrap().clone();
        assert_eq!(found[0].range, range(&doc.text, a.value_at, a.value_end));
        assert_eq!(found[0].range.start.character, doc.text[..a.value_at].encode_utf16().count() as u32);
        assert_eq!(found[0].range.end.character - found[0].range.start.character, 6);
        let mut owner = crate::structure::Structure::default();
        let hover = owner.hover(&doc, at(&doc, "orders")).unwrap();
        assert!(hover.markdown.contains("1 reference"));
        assert!(host.requests().is_empty());
        let changed = Document { text: doc.text.replace("id=\"orders\"", "id=\"renamed\""), ..doc };
        assert!(definition(&changed, at(&changed, "orders")).is_none());
    }
    #[test]
    fn ambiguous_and_absent_local_declarations_never_choose_global_name() {
        let d = doc("<i:channel id=\"x\"/><i:channel id=\"x\"/><i:bridge input-channel=\"x\"/>");
        assert!(definition(&d, at(&d, "x")).unwrap().is_empty());
        let d = doc("<i:bridge input-channel=\"x\"/>");
        assert!(definition(&d, at(&d, "x")).is_none(), "a project-wide declaration may exist");
    }
    #[test]
    fn unknown_namespaces_escaped_values_nested_profiles_and_malformed_are_quiet() {
        for body in [
            "<i:channel id=\"x\"/><i:bridge input-channel=\"${x}\"/>",
            "<beans profile=\"a\"><i:channel id=\"x\"/><i:bridge input-channel=\"x\"/></beans>",
            "<i:channel id=\"x\"><i:bridge input-channel=\"x\"/>",
        ] {
            let d = doc(body);
            assert!(definition(&d, at(&d, "x")).is_none(), "{body}");
        }
        let d = doc("<i:channel id=\"x\"/><i:bridge input-channel=\"&#120;\"/>");
        assert_eq!(definition(&d, at(&d, "x")), Some(Vec::new()), "the lone declaration has no other place to go to");
        let d = doc("<i:channel id=\"x\"/><i:bridge input-channel=\"x\"/>");
        let foreign = Document { text: d.text.replace(NAMESPACE, &format!("{NAMESPACE}-fake")), ..d };
        assert!(definition(&foreign, at(&foreign, "x")).is_none());
    }
    #[test]
    fn registered_navigation_refuses_bean_ids_names_and_alias_collisions() {
        let _host = MockHost::install().with_capabilities(&[]);
        let mut owner = crate::structure::Structure::default();
        for collision in [
            "<bean id='orders' class='app.Other'/>",
            "<bean name='one, orders; two' class='app.Other'/>",
            "<alias name='another' alias='orders'/>",
            "<i:service-activator id='orders' input-channel='in'/>",
            "<beans profile='dev'><i:channel id='orders'/></beans>",
            "<beans profile='dev'><bean id='orders' class='app.Other'/></beans>",
        ] {
            let d = doc(&format!("<!--😀--><i:channel id='orders'/>{collision}<i:bridge input-channel='orders'/>"));
            let at = at(&d, "orders");
            assert!(owner.definition(&d, at).is_empty(), "{collision}");
            assert!(owner.hover(&d, at).unwrap().markdown.contains("uses this name"));
            let fixed = Document { text: d.text.replace(collision, ""), ..d.clone() };
            assert_eq!(owner.definition(&fixed, self::at(&fixed, "orders")).len(), 1);
        }
        let d = doc("<i:channel id='orders'/><foreign:bean xmlns:foreign='urn:other' id='orders'/><bean name='preorders, ordersExtra'/><alias name='orders' alias='different'/><i:bridge input-channel='orders'/>");
        assert_eq!(owner.definition(&d, at(&d, "orders")).len(), 1, "foreign names, substrings and alias sources do not register this spelling");
    }
    fn command_host(d: &Document, at: Position) -> MockHost {
        let h = MockHost::install().with_capabilities(&["project.read", "editor.read"]);
        h.open_editor(&d.path, "xml", &d.text, &[(at.line, at.character, at.line, at.character)]);
        h
    }
    #[test]
    fn registered_current_file_occurrence_picker_uses_unsaved_utf16_tokens_in_source_order() {
        let d = doc("<!--😀--><i:channel id='orders'/><i:bridge input-channel='orders' output-channel='orders'/>");
        let at = at(&d, "orders");
        let h = command_host(&d, at);
        h.add_file("other.xml", &d.text);
        h.type_in_editor(" ");
        h.answer_next_prompt(Some(&["0"]));
        let mut owner = crate::structure::Structure::default();
        let message = owner.run_command("integration.current-file-channel-occurrences", &h.invocation(), None).unwrap().unwrap();
        let expected = tokens(&d).unwrap()[0].range.start;
        assert_eq!(h.opened_locations(), [(d.path.clone(), expected.line, expected.character)]);
        assert!(message.contains("Opened the occurrence"));
        assert!(h.requests().is_empty() && h.process_runs().is_empty());
        let h = command_host(&d, tokens(&d).unwrap()[0].range.start);
        h.answer_next_prompt(Some(&["2"]));
        owner.run_command("integration.current-file-channel-occurrences", &h.invocation(), None).unwrap();
        assert_eq!(h.opened_locations(), [(d.path.clone(), at.line, at.character)]);
    }
    #[test]
    fn occurrence_picker_refuses_ambiguous_unknown_cancelled_forged_and_denied_contexts() {
        for body in ["<i:bridge input-channel='orders'/>", "<i:channel id='orders'/><bean name='orders'/><i:bridge input-channel='orders'/>", "<i:channel id='orders'/><i:channel id='orders'/><i:bridge input-channel='orders'/>"] {
            let d = doc(body); let h = command_host(&d, at(&d, "orders"));
            assert!(crate::structure::Structure::default().run_command("integration.current-file-channel-occurrences", &h.invocation(), None).is_err());
            assert!(h.opened_locations().is_empty());
        }
        let d = doc("<i:channel id='orders'/><i:bridge input-channel='orders'/>");
        for answer in [None, Some(&["999"][..])] {
            let h = command_host(&d, at(&d, "orders")); h.answer_next_prompt(answer);
            let result = crate::structure::Structure::default().run_command("integration.current-file-channel-occurrences", &h.invocation(), None);
            if answer.is_none() { assert_eq!(result.unwrap(), None); } else { assert!(result.is_err()); }
            assert!(h.opened_locations().is_empty());
        }
        let h = command_host(&d, at(&d, "orders")).with_capabilities(&["project.read"]);
        assert!(occurrences(&h.invocation()).is_err());
        let h = command_host(&d, at(&d, "orders")); h.set_project_root(Some("/other"));
        assert!(occurrences(&h.invocation()).is_err()); assert!(h.opened_locations().is_empty());
    }
    #[test]
    fn picked_occurrence_source_guard_rejects_version_text_path_and_root_drift() {
        let d = doc("<i:channel id='orders'/><i:bridge input-channel='orders'/>");
        for change in 0..3 {
            let h = command_host(&d, at(&d, "orders"));
            let (root, captured, _) = current_source(&h.invocation()).unwrap();
            match change {
                0 => h.type_in_editor(" "),
                1 => h.open_editor("/project/other.xml", "xml", &d.text, &[(0,0,0,0)]),
                _ => h.set_project_root(Some("/other")),
            }
            assert!(matches!(unchanged_source(&h.invocation(), &root, &captured), Err(Error::Stale)));
            assert!(h.opened_locations().is_empty());
        }
    }
    #[test]
    fn bounded_and_half_open_ranges() {
        let d = doc("<i:channel id=\"x\"/><i:bridge input-channel=\"x\"/>");
        let token = tokens(&d).unwrap().pop().unwrap();
        assert!(definition(&d, token.range.end).is_none());
        let large = Document { text: " ".repeat(MAX_TEXT + 1), ..d };
        assert!(tokens(&large).is_none());
        let capped = doc(&"<i:channel id=\"x\"/>".repeat(MAX_TOKENS + 1));
        assert!(tokens(&capped).is_none());
    }
    #[test]
    fn reserved_namespace_source_is_quiet_through_registered_owner_fallback() {
        let host = MockHost::install();
        let d = doc("<i:channel id='orders'/><i:bridge input-channel='orders'/>");
        let bad = Document { text: d.text.replace("xmlns:i=", "xmlns:xml=").replace("<i:", "<xml:"), ..d.clone() };
        host.add_file("channels.xml", &bad.text);
        let mut owner = crate::structure::Structure::default();
        assert!(owner.definition(&bad, at(&bad, "orders")).is_empty());
        assert!(owner.hover(&bad, at(&bad, "orders")).is_none());
        let legal =
            Document { text: d.text.replace("<beans ", "<beans xmlns:xml='http://www.w3.org/XML/1998/namespace' xml:lang='en' "), ..d };
        assert_eq!(owner.definition(&legal, at(&legal, "orders")).len(), 1);
    }
}

/// Explicit same-document edits for a closed channel/bridge source subset.
/// Imports, profiles, other bean registrations and nested endpoints are excluded.
#[derive(Default)]
pub(crate) struct ChannelRenames {
    next: u64,
    pending: BTreeMap<String, (String, Document, String, Vec<Range>)>,
}
fn rename_sites(doc: &Document, at: Position) -> Option<(String, Vec<Range>)> {
    let tokens = tokens(doc)?;
    let elements = xml::elements(&doc.text);
    if doc.text.contains("${") || doc.text.contains("#{") { return None; }
    for (i,e) in elements.iter().enumerate() {
        if i == 0 {
            if e.attrs.iter().any(|a| !a.name.starts_with("xmlns") && a.name != "xsi:schemaLocation") { return None; }
        } else {
            if e.parent != Some(0) || xml::namespace(&elements,i) != Some(NAMESPACE) { return None; }
            let allowed: &[&str] = match e.local() {
                "channel" | "publish-subscribe-channel" => &["id"],
                "bridge" => &["id", "input-channel", "output-channel", "error-channel"],
                // A flat endpoint's literal channel-valued attributes are owned
                // by this same local bean namespace. Expressions, nested
                // endpoints and other namespaces remain outside this inventory.
                "service-activator" | "transformer" | "filter" | "splitter" | "aggregator" => &[
                    "id", "input-channel", "output-channel", "error-channel", "discard-channel", "ref", "method",
                ],
                _ => return None,
            };
            if e.attrs.iter().any(|a| !allowed.contains(&a.name.as_str()) || !literal(&a.value)) { return None; }
        }
    }
    let selected = selected(&tokens, at)?;
    if selected.collides || matches!(selected.name.as_str(), "nullChannel" | "errorChannel")
        || tokens.iter().filter(|t|t.declaration && t.name == selected.name).count()!=1 { return None; }
    // A handler ref with the same spelling is a bean reference too. This
    // channel-only action cannot prove that handler contract, so fail closed.
    if elements.iter().flat_map(|e| &e.attrs).any(|a| a.name=="ref" && a.value==selected.name) { return None; }
    let sites: Vec<_> = tokens.iter().filter(|t|t.name==selected.name).map(|t|t.range).collect();
    Some((selected.name.clone(), sites))
}
fn current_rename(root: &str, doc: &Document) -> Result<(), Error> {
    let project = host::project_of(&doc.path).ok_or(Error::Stale)?;
    if project.root()? != root { return Err(Error::Stale); }
    let current = project.read_file(&doc.path)?;
    if current != doc.text { return Err(Error::Stale); }
    Ok(())
}
impl ChannelRenames {
    pub(crate) fn actions(&mut self, doc: &Document, selection: Range) -> Vec<openidea_plugin::file_actions::FileAction> {
        if selection.start != selection.end { return vec![]; }
        let Some((name,sites)) = rename_sites(doc,selection.start) else { return vec![] };
        let Some(root) = host::project_of(&doc.path).and_then(|p|p.root().ok()) else { return vec![] };
        if root == "/" || !doc.path.starts_with(&format!("{root}/")) { return vec![]; }
        if self.pending.len()>=8 { self.pending.clear(); }
        self.next=self.next.wrapping_add(1);
        let id=format!("integration-channel-rename:{}",self.next);
        self.pending.insert(id.clone(),(root,doc.clone(),name,sites));
        vec![openidea_plugin::file_actions::FileAction {title:"Rename literal channel in this document…".into(),kind:Some("refactor".into()),data:Some(id),..Default::default()}]
    }
    pub(crate) fn resolve(&mut self, doc: &Document, action: openidea_plugin::file_actions::FileAction) -> Result<openidea_plugin::file_actions::FileAction,Error> {
        use openidea_plugin::{file_actions::FileAction,TextEdit,text::offset};
        let (root,original,old,sites)=self.pending.remove(action.data.as_deref().ok_or(Error::Stale)?).ok_or(Error::Stale)?;
        if original != *doc { return Err(Error::Stale); }
        current_rename(&root,doc)?;
        let Some(new)=host::input("New channel ID in this document",Some(&old))? else { return Err(Error::Cancelled) };
        if !literal(&new) || new==old || matches!(new.as_str(),"nullChannel"|"errorChannel")
            || xml::elements(&doc.text).iter().flat_map(|e|&e.attrs).any(|a|a.value==new) {
            return Err(Error::Invalid("Use a different unique literal channel ID (letter/underscore first, then letters/digits/_.-).".into()));
        }
        let mut after=doc.text.clone();
        for span in sites.iter().rev() {
            let from=offset(&doc.text,span.start).ok_or(Error::Stale)?;
            let to=offset(&doc.text,span.end).ok_or(Error::Stale)?;
            if doc.text.get(from..to)!=Some(old.as_str()) { return Err(Error::Stale); }
            after.replace_range(from..to,&new);
        }
        let from=offset(&doc.text,sites[0].start).ok_or(Error::Stale)?;
        let at=openidea_plugin::text::position(&after,from);
        let changed=Document {text:after.clone(),..doc.clone()};
        if !rename_sites(&changed,at).is_some_and(|(name,spans)|name==new && spans.len()==sites.len()) { return Err(Error::Stale); }
        let review=format!("Source: {}\nVersion: {}\nChannel: {old} → {new}\nExact edits: {}\n\nThis changes only this physical XML document's literal channel and supported flat endpoint references. External references, runtime contexts, imports and global Rename are excluded.\n\nBEFORE:\n{}\n\nAFTER:\n{after}",doc.path,doc.version,sites.len(),doc.text);
        current_rename(&root,doc)?;
        if !ui_kit::prompts::confirm_exact("Rename channel in this document?",&review)? { return Err(Error::Cancelled); }
        current_rename(&root,doc)?;
        Ok(FileAction {title:format!("Rename local channel {old} to {new}"),kind:Some("refactor".into()),edits:vec![(doc.path.clone(),sites.into_iter().map(|range|TextEdit {range,text:new.clone()}).collect())],..Default::default()})
    }
}
#[cfg(test)]
mod channel_rename_tests {
    use super::*;
    use openidea_plugin::{file_actions::FileActions,mock::MockHost};
    fn doc() -> Document {Document {path:"/project/channels.xml".into(),language:"xml".into(),version:1,text:format!("<beans xmlns='{BEANS}' xmlns:i='{NAMESPACE}'><!-- 😀 --><i:channel id='in'/><i:channel id='out'/><i:bridge input-channel='in' output-channel='out'/></beans>")}}
    fn selection(d:&Document)->Range {let p=openidea_plugin::text::position(&d.text,d.text.find("id='in'").unwrap()+4);Range::new(p,p)}
    #[test]
    fn registered_owner_reviews_exact_edits_and_consumes_action() {
        let d=doc();let h=MockHost::install();h.add_file(&d.path,&d.text);let mut owner=crate::structure::Structure::default();
        let a=owner.file_actions(&d,selection(&d)).into_iter().find(|a|a.title=="Rename literal channel in this document…").unwrap();
        let replay=a.clone();h.answer_next_prompt(Some(&["incoming"]));h.answer_next_prompt(Some(&["yes"]));
        let result=owner.resolve_file_action(&d,a).unwrap();assert_eq!(result.edits[0].1.len(),2);
        assert!(result.edits[0].1.iter().all(|e|e.text=="incoming"));
        assert!(owner.resolve_file_action(&d,replay).is_err());assert!(h.process_runs().is_empty() && h.requests().is_empty());
    }
    #[test]
    fn flat_endpoint_channels_are_renamed_without_changing_handler_or_method() {
        let mut d=doc();d.text=d.text.replace("</beans>","<i:service-activator id='service' input-channel='in' output-channel='out' error-channel='in' ref='handler' method='in'/><i:filter input-channel='out' discard-channel='in' ref='filter'/></beans>");
        let h=MockHost::install();h.add_file(&d.path,&d.text);let mut owner=crate::structure::Structure::default();
        let at=openidea_plugin::text::position(&d.text,d.text.find("error-channel='in'").unwrap()+15);
        let a=owner.file_actions(&d,Range::new(at,at)).into_iter().find(|a|a.title=="Rename literal channel in this document…").unwrap();
        h.answer_next_prompt(Some(&["incoming"]));h.answer_next_prompt(Some(&["yes"]));
        let result=owner.resolve_file_action(&d,a).unwrap();assert_eq!(result.edits[0].1.len(),5);
        let mut after=d.text.clone();for edit in result.edits[0].1.iter().rev() {
            let from=openidea_plugin::text::offset(&d.text,edit.range.start).unwrap();let to=openidea_plugin::text::offset(&d.text,edit.range.end).unwrap();after.replace_range(from..to,&edit.text);
        }
        assert!(after.contains("method='in'"));assert!(after.contains("ref='handler'"));
        assert!(after.contains("discard-channel='incoming'"));assert!(after.contains("output-channel='out'"));
        let same_handler=Document{text:d.text.replace("ref='handler'","ref='in'"),..d.clone()};assert!(rename_sites(&same_handler,selection(&same_handler).start).is_none());
        let dynamic=Document{text:d.text.replace("method='in'","expression='payload.in'"),..d.clone()};assert!(rename_sites(&dynamic,selection(&dynamic).start).is_none());
    }
    #[test]
    fn ambiguous_unknown_scope_collision_and_stale_source_refuse() {
        let d=doc();let h=MockHost::install();h.add_file(&d.path,&d.text);
        for text in [d.text.replace("</beans>","<import resource='other.xml'/></beans>"),d.text.replace("<i:channel id='out'/>","<i:channel id='in'/>"),d.text.replace("id='out'","id='${out}'")] {
            let changed=Document{text,..d.clone()};assert!(rename_sites(&changed,selection(&changed).start).is_none());
        }
        let mut renames=ChannelRenames::default();let a=renames.actions(&d,selection(&d)).remove(0);
        h.answer_next_prompt(Some(&["out"]));assert!(renames.resolve(&d,a).is_err());
        let a=renames.actions(&d,selection(&d)).remove(0);h.add_file(&d.path,"<changed/>");assert!(renames.resolve(&d,a).is_err());
    }
}
