//! Previewed source-local edits; effective matcher/authorization policy stays unresolved.
use super::*;
use openidea_plugin::file_actions::{FileAction, FileActions};
use std::collections::BTreeMap;
const MAX_TEXT: usize = 128 << 10;
const BEANS_NAMESPACE: &str = "http://www.springframework.org/schema/beans";
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Debug,
    DebugFlag,
    DebugCall,
    LeadingSlash,
}
impl Kind {
    fn title(self) -> &'static str {
        match self {
            Self::Debug => "Remove explicit Security debug element",
            Self::DebugFlag => "Disable explicit EnableWebSecurity debug flag",
            Self::DebugCall => "Disable explicit WebSecurity debug call",
            Self::LeadingSlash => "Add leading / to explicit PathPattern matcher literal",
        }
    }
    fn replacement(self, before: &str) -> String {
        match self {
            Self::Debug => String::new(),
            Self::DebugFlag | Self::DebugCall => "false".into(),
            Self::LeadingSlash => format!("/{before}"),
        }
    }
    fn confirmation(self) -> &'static str {
        match self {
            Self::Debug => "Remove Security debug declaration?",
            Self::DebugFlag => "Disable explicit Security debug flag?",
            Self::DebugCall => "Disable explicit WebSecurity debug call?",
            Self::LeadingSlash => "Add leading / to explicit PathPattern matcher?",
        }
    }
}
#[derive(Clone)]
struct Pending {
    doc: Document,
    root: String,
    from: usize,
    to: usize,
    kind: Kind,
}
#[derive(Default)]
pub(crate) struct State {
    next: u64,
    pending: BTreeMap<String, Pending>,
}
fn root(doc: &Document) -> Option<String> {
    spring_text::index::owning_root(&doc.path)
}
fn debug_candidates(doc: &Document, selection: Range) -> Vec<(usize, usize)> {
    if doc.language != "xml"
        || !doc.path.ends_with(".xml")
        || doc.text.len() > MAX_TEXT
        || !host::setting_bool("springSecurity.checkDebug").unwrap_or(true)
        || !spring_text::xml::generation_valid(&doc.text)
    {
        return Vec::new();
    }
    let elements = xml::elements(&doc.text);
    elements
        .iter()
        .enumerate()
        .filter_map(|(i, e)| {
            let parent = e.parent?;
            let owner = elements.get(parent)?;
            if e.local() != "debug"
                || xml::namespace(&elements, i) != Some(SECURITY_NAMESPACE)
                || owner.parent.is_some()
                || owner.local() != "beans"
                || xml::namespace(&elements, parent) != Some(BEANS_NAMESPACE)
                || e.attrs.iter().any(|a| a.name != "xmlns" && !a.name.starts_with("xmlns:"))
                || e.end != e.tag_end
                || !doc.text[e.start..e.tag_end].ends_with("/>")
                || !overlaps(range(&doc.text, e.start, e.tag_end), selection)
            {
                return None;
            }
            Some((e.start, e.tag_end))
        })
        .collect()
}
fn java_debug_candidates(doc: &Document, selection: Range) -> Vec<(usize, usize)> {
    const ANNOTATION: &str = "org.springframework.security.config.annotation.web.configuration.EnableWebSecurity";
    if doc.language != "java" || !doc.path.ends_with(".java") || doc.text.len() > 64 << 10
        || !host::setting_bool("springSecurity.checkDebug").unwrap_or(true)
        || !super::path_matcher::closed(&doc.text) { return Vec::new(); }
    let tokens=java::tokens(&doc.text);
    if tokens.len()>8192 || tokens.iter().any(|t|t.s=="<") { return Vec::new(); }
    let file=java::parse(&doc.text);
    if file.imports.iter().any(|i| i.path.ends_with(".*") || i.is_static && i.path.rsplit('.').next()==Some("EnableWebSecurity"))
        || file.imports.iter().filter(|i| !i.is_static && i.path.rsplit('.').next()==Some("EnableWebSecurity")).count()!=1
        || !file.imports.iter().any(|i| !i.is_static && i.path==ANNOTATION)
        || file.types.iter().any(|t| t.name=="EnableWebSecurity" || t.supers().next().is_some())
    { return Vec::new(); }
    file.types.iter().filter(|t| t.outer.is_none() && t.kind=="class").flat_map(|t| &t.annotations)
        .filter_map(|a| {
            if a.name!="EnableWebSecurity" || a.args.len()!=1 || a.args[0].name!="debug" { return None; }
            let tokens=java::tokens(doc.text.get(a.at..a.end)?);
            let names:Vec<_>=tokens.iter().map(|t|t.s).collect();
            if names.as_slice()!=["@","EnableWebSecurity","(","debug","=","true",")"] { return None; }
            let from=a.at+tokens[5].at; let to=from+4;
            overlaps(range(&doc.text,from,to),selection).then_some((from,to))
        }).take(4).collect()
}
fn candidates(doc: &Document, selection: Range) -> Vec<(usize, usize, Kind)> {
    let mut out: Vec<_> = debug_candidates(doc, selection).into_iter().map(|(from, to)| (from, to, Kind::Debug)).collect();
    out.extend(java_debug_candidates(doc,selection).into_iter().map(|(from,to)|(from,to,Kind::DebugFlag)));
    out.extend(super::debug_call::candidates(doc).into_iter().filter(|(from,to)|overlaps(range(&doc.text,*from,*to),selection)).map(|(from,to)|(from,to,Kind::DebugCall)));
    out.extend(
        super::path_matcher::issues(doc)
            .into_iter()
            .filter(|i| overlaps(range(&doc.text, i.from, i.to), selection))
            .map(|i| (i.from, i.to, Kind::LeadingSlash)),
    );
    out
}
impl FileActions for SecurityTools {
    fn file_actions(&mut self, doc: &Document, selection: Range) -> Vec<FileAction> {
        let Some(root) = root(doc) else {
            return Vec::new();
        };
        let candidates = candidates(doc, selection);
        if self.actions.pending.len() + candidates.len().min(4) > 8 {
            self.actions.pending.clear();
        }
        let mut out = Vec::new();
        for (from, to, kind) in candidates.into_iter().take(4) {
            let Some(next) = self.actions.next.checked_add(1) else {
                break;
            };
            self.actions.next = next;
            let token = format!("security-local:{next}:{}", doc.version);
            self.actions.pending.insert(token.clone(), Pending { doc: doc.clone(), root: root.clone(), from, to, kind });
            out.push(FileAction {
                title: format!("{}…", kind.title()),
                kind: Some("quickfix".into()),
                data: Some(token),
                ..FileAction::default()
            });
        }
        out
    }
    fn resolve_file_action(&mut self, doc: &Document, action: FileAction) -> Result<FileAction, Error> {
        let token = action.data.as_deref().ok_or(Error::Stale)?;
        let pending = self.actions.pending.get(token).cloned().ok_or(Error::Stale)?;
        if doc.path != pending.doc.path
            || doc.language != pending.doc.language
            || doc.version != pending.doc.version
            || doc.text != pending.doc.text
            || root(doc).as_deref() != Some(&pending.root)
        {
            return Err(Error::Stale);
        }
        let selection = range(&doc.text, pending.from, pending.to);
        if !candidates(doc, selection).contains(&(pending.from, pending.to, pending.kind)) {
            return Err(Error::Stale);
        }
        let project = spring_text::index::project_of(&doc.path).ok_or(Error::Stale)?;
        if project.read_file(&doc.path)? != doc.text {
            return Err(Error::Stale);
        }
        let before = &doc.text[pending.from..pending.to];
        let replacement = pending.kind.replacement(before);
        let preview = match pending.kind {
            Kind::Debug => format!(
                "File: {}\nDocument version: {}\n\nRemove exactly:\n{before}\n\nReplacement: empty text. All surrounding bytes stay unchanged.\n\nThis removes this explicit debug declaration only; other configuration and effective runtime logging remain unresolved.",
                doc.path, doc.version
            ),
            Kind::DebugFlag => format!(
                "File: {}\nDocument version: {}\n\nReplace exactly this explicit EnableWebSecurity debug value:\n{before}\n\nWith:\n{replacement}\n\nOnly this Java annotation Boolean literal changes. This disables the source declaration's debug flag; other configuration and effective runtime logging remain unresolved.", doc.path, doc.version
            ),
            Kind::DebugCall => format!(
                "File: {}\nDocument version: {}\n\nReplace exactly this explicit WebSecurity.debug argument:\n{before}\n\nWith:\n{replacement}\n\nOnly this Boolean literal changes on the directly declared Spring WebSecurity parameter. Other configuration and effective runtime logging remain unresolved.", doc.path, doc.version
            ),
            Kind::LeadingSlash => format!(
                "File: {}\nDocument version: {}\n\nReplace exactly this literal content:\n{before}\n\nWith:\n{replacement}\n\nOnly a leading / is added; any HTTP-method argument, quotes and all surrounding bytes stay unchanged. The explicit PathPatternRequestMatcher factory requires it (builder since Spring Security 6.5; pathPattern factory since 7.0). This does not infer the project's implicit matcher strategy, authorization policy or other configuration.",
                doc.path, doc.version
            ),
        };
        if !ui_kit::prompts::confirm_exact(pending.kind.confirmation(), &preview)? {
            return Err(Error::Cancelled);
        }
        if root(doc).as_deref() != Some(&pending.root)
            || project.read_file(&doc.path)? != doc.text
            || !candidates(doc, selection).contains(&(pending.from, pending.to, pending.kind))
        {
            return Err(Error::Stale);
        }
        self.actions.pending.remove(token);
        Ok(FileAction {
            title: pending.kind.title().into(),
            kind: Some("quickfix".into()),
            edits: vec![(doc.path.clone(), vec![edit(&doc.text, pending.from, pending.to, &replacement)])],
            ..FileAction::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{mock::MockHost, text::position};
    const XML: &str = "<beans xmlns=\"http://www.springframework.org/schema/beans\" xmlns:s=\"http://www.springframework.org/schema/security\">\n<!-- 😀 retained --> <s:debug/> <bean id=\"keep\"/>\n</beans>";
    fn doc(text: &str) -> Document {
        Document { path: "/project/security.xml".into(), language: "xml".into(), version: 9, text: text.into() }
    }
    fn cursor(d: &Document) -> Range {
        let p = position(&d.text, d.text.find("debug").unwrap());
        Range::new(p, p)
    }
    fn setup() -> (MockHost, SecurityTools, Document) {
        let host = MockHost::install();
        host.add_file("security.xml", XML);
        (host, SecurityTools::default(), doc(XML))
    }
    #[test]
    fn versioned_preview_resolves_only_exact_utf16_edit_without_writing() {
        let (host, mut tools, d) = setup();
        let action = tools.file_actions(&d, cursor(&d)).remove(0);
        assert!(action.edits.is_empty());
        assert!(action.operations.is_empty());
        let replay = action.clone();
        host.answer_next_prompt(Some(&["yes"]));
        let resolved = tools.resolve_file_action(&d, action).unwrap();
        assert!(resolved.operations.is_empty());
        assert_eq!(resolved.edits.len(), 1);
        let (path, edits) = &resolved.edits[0];
        assert_eq!(path, &d.path);
        assert_eq!(edits.len(), 1);
        let e = &edits[0];
        let from = offset(&d.text, e.range.start).unwrap();
        let to = offset(&d.text, e.range.end).unwrap();
        assert_eq!(&d.text[from..to], "<s:debug/>");
        assert!(e.text.is_empty());
        let mut after = d.text.clone();
        after.replace_range(from..to, &e.text);
        assert_eq!(after, XML.replace("<s:debug/>", ""));
        assert_eq!(spring_text::index::project_of(&d.path).unwrap().read_file(&d.path).unwrap(), XML);
        assert!(tools.resolve_file_action(&d, replay).is_err());
        assert!(host.process_runs().is_empty());
        assert_eq!(host.fetches(), 0);
    }
    #[test]
    fn current_unsaved_project_buffer_is_not_a_disk_saved_requirement() {
        let (host, mut tools, d) = setup();
        host.add_file("security.xml", &XML.replace("<s:debug/>", ""));
        host.open_editor(&d.path, "xml", &d.text, &[]);
        let action = tools.file_actions(&d, cursor(&d)).remove(0);
        host.answer_next_prompt(Some(&["yes"]));
        assert_eq!(tools.resolve_file_action(&d, action).unwrap().edits.len(), 1);
        assert!(host.process_runs().is_empty());
    }
    #[test]
    fn debug_removal_works_under_a_windows_drive_root() {
        let host = MockHost::install();
        host.set_project_root(Some(r"C:\work\app"));
        host.add_file("C:/work/app/security.xml", XML);
        let d = Document { path: "C:/work/app/security.xml".into(), ..doc(XML) };
        let mut tools = SecurityTools::default();
        let action = tools.file_actions(&d, cursor(&d)).pop().expect("offered under C:");
        host.answer_next_prompt(Some(&["yes"]));
        assert_eq!(tools.resolve_file_action(&d, action).unwrap().edits.len(), 1);
    }
    #[test]
    fn cancellation_produces_no_edit() {
        let (host, mut tools, d) = setup();
        let action = tools.file_actions(&d, cursor(&d)).remove(0);
        host.answer_next_prompt(None);
        assert!(matches!(tools.resolve_file_action(&d, action), Err(Error::Cancelled)));
    }
    #[test]
    fn stale_snapshot_version_path_language_root_or_visible_source_refuses() {
        for mode in 0..6 {
            let (host, mut tools, mut d) = setup();
            let action = tools.file_actions(&d, cursor(&d)).remove(0);
            match mode {
                0 => d.version += 1,
                1 => d.text.push(' '),
                2 => d.path = "/project/other.xml".into(),
                3 => d.language = "java".into(),
                4 => host.set_project_root(Some("/other")),
                _ => host.add_file("security.xml", "changed"),
            }
            host.answer_next_prompt(Some(&["yes"]));
            assert!(tools.resolve_file_action(&d, action).is_err());
        }
    }
    #[test]
    fn malformed_dynamic_or_unowned_debug_is_quiet() {
        let _host = MockHost::install();
        let mut tools = SecurityTools::default();
        for text in [
            XML.replace(SECURITY_NAMESPACE, "urn:other"),
            XML.replace(BEANS_NAMESPACE, "urn:other"),
            XML.replace("<s:debug/>", "<s:debug enabled=\"true\"/>"),
            XML.replace("<s:debug/>", "<s:debug></s:debug>"),
            XML.replace("<s:debug/>", "<s:debug>${dynamic}</s:debug>"),
            XML.replace("<s:debug/>", "<s:http><s:debug/></s:http>"),
            XML.replace("</beans>", "</wrong>"),
            XML.replace("</beans>", ""),
            XML.replace("<s:debug/>", "<!-- <s:debug/> -->"),
            XML.replace("<s:debug/>", "<s:debug xmlns:s=\"urn:other\"/>"),
            format!("<!DOCTYPE beans [<!ENTITY x 'debug'>]>{XML}"),
        ] {
            let d = doc(&text);
            assert!(tools.file_actions(&d, cursor(&d)).is_empty(), "{text}");
        }
    }
    #[test]
    fn setting_selection_size_and_missing_project_gate_actions() {
        let (host, mut tools, d) = setup();
        assert!(tools.file_actions(&d, Range::new(Position::new(0, 0), Position::new(0, 0))).is_empty());
        host.set_setting("springSecurity.checkDebug", "false");
        assert!(tools.file_actions(&d, cursor(&d)).is_empty());
        host.set_setting("springSecurity.checkDebug", "true");
        let huge = doc(&format!("{}{}", XML, " ".repeat(MAX_TEXT)));
        assert!(tools.file_actions(&huge, cursor(&huge)).is_empty());
        host.set_project_root(None);
        assert!(tools.file_actions(&d, cursor(&d)).is_empty());
    }
    #[test]
    fn selection_supports_multiple_independent_debug_elements_and_forged_tokens_refuse() {
        let (host, mut tools, _) = setup();
        let text = XML.replace("<s:debug/>", "<s:debug/><s:debug/>");
        let d = doc(&text);
        host.add_file("security.xml", &text);
        let range = range(&text, 0, text.len());
        let actions = tools.file_actions(&d, range);
        assert_eq!(actions.len(), 2);
        assert_ne!(actions[0].data, actions[1].data);
        let forged = FileAction { data: Some("disable-debug:999:9".into()), ..FileAction::default() };
        assert!(tools.resolve_file_action(&d, forged).is_err());
    }
    #[test]
    fn reserved_namespace_debug_action_refuses_but_legal_xml_prefix_survives() {
        let (_host, mut owner, _) = setup();
        for text in [
            XML.replace("xmlns:s=", "xmlns:xml=").replace("<s:debug", "<xml:debug"),
            XML.replace("<beans ", "<beans xmlns:q='http://www.w3.org/XML/1998/namespace' "),
        ] {
            let d = doc(&text);
            assert!(owner.file_actions(&d, cursor(&d)).is_empty());
        }
        let d = doc(&XML.replace("<beans ", "<beans xmlns:xml='http://www.w3.org/XML/1998/namespace' xml:lang='en' "));
        assert_eq!(owner.file_actions(&d, cursor(&d)).len(), 1);
    }
}

#[cfg(test)]
mod matcher_tests {
    use super::*;
    use openidea_plugin::{diagnostics::DiagnosticsProvider, mock::MockHost, text::position};
    const IMPORT: &str = "import org.springframework.security.web.servlet.util.matcher.PathPatternRequestMatcher;";
    fn doc(factory: &str) -> Document {
        Document {
            path: "/project/Security.java".into(),
            language: "java".into(),
            version: 17,
            text: format!("{IMPORT}\nclass Security {{ Object f() {{ /* 😀 retained */ return {factory}; }} }}"),
        }
    }
    fn setup(factory: &str) -> (MockHost, SecurityTools, Document) {
        let h = MockHost::install();
        let d = doc(factory);
        h.add_file("Security.java", &d.text);
        (h, SecurityTools::default(), d)
    }
    fn cursor(d: &Document) -> Range {
        let p = position(&d.text, d.text.find("api/users").unwrap());
        Range::new(p, p)
    }
    #[test]
    fn exact_preview_returns_one_undoable_literal_edit_for_65_and_70_forms() {
        for factory in
            ["PathPatternRequestMatcher.withDefaults().matcher(\"api/users\")", "PathPatternRequestMatcher.pathPattern(\"api/users\")",
             "PathPatternRequestMatcher.withDefaults().matcher(null, /* method retained */ \"api/users\")",
             "PathPatternRequestMatcher.pathPattern(org.springframework.http.HttpMethod.GET, \"api/users\")"]
        {
            let (h, mut tools, d) = setup(factory);
            let a = tools.file_actions(&d, cursor(&d)).remove(0);
            let replay = a.clone();
            assert!(a.edits.is_empty() && a.operations.is_empty());
            h.answer_next_prompt(Some(&["yes"]));
            let fixed = tools.resolve_file_action(&d, a).unwrap();
            assert_eq!(fixed.edits.len(), 1);
            assert!(fixed.operations.is_empty());
            let (path, edits) = &fixed.edits[0];
            assert_eq!(path, &d.path);
            assert_eq!(edits.len(), 1);
            let e = &edits[0];
            let from = offset(&d.text, e.range.start).unwrap();
            let to = offset(&d.text, e.range.end).unwrap();
            assert_eq!(&d.text[from..to], "api/users");
            assert_eq!(e.text, "/api/users");
            let mut after = d.text.clone();
            after.replace_range(from..to, &e.text);
            assert_eq!(after, d.text.replace("\"api/users\"", "\"/api/users\""));
            assert_eq!(spring_text::index::project_of(path).unwrap().read_file(path).unwrap(), d.text);
            assert!(tools.resolve_file_action(&d, replay).is_err());
            assert!(h.process_runs().is_empty());
            assert_eq!(h.fetches(), 0);
        }
    }
    #[test]
    fn overloaded_unsaved_crlf_preview_edits_only_the_path_and_can_be_cancelled() {
        let (h,mut tools,mut d)=setup("PathPatternRequestMatcher.withDefaults().matcher(null, /* 😀 */ \"api/users\")");
        d.text=d.text.replace('\n',"\r\n");
        h.open_editor(&d.path,"java",&d.text,&[]);
        let action=tools.file_actions(&d,cursor(&d)).remove(0);
        h.answer_next_prompt(None);
        assert!(matches!(tools.resolve_file_action(&d,action.clone()),Err(Error::Cancelled)));
        h.answer_next_prompt(Some(&["yes"]));
        let resolved=tools.resolve_file_action(&d,action).unwrap();
        let e=&resolved.edits[0].1[0];
        let mut applied=d.text.clone();
        applied.replace_range(offset(&d.text,e.range.start).unwrap()..offset(&d.text,e.range.end).unwrap(),&e.text);
        assert_eq!(applied,d.text.replace("\"api/users\"","\"/api/users\""));
        assert!(applied.contains("null, /* 😀 */"));
        assert!(h.process_runs().is_empty()); assert_eq!(h.fetches(),0);
    }
    #[test]
    fn live_current_text_finding_disappears_after_exact_fix_without_project_discovery() {
        let (h, mut tools, mut d) = setup("PathPatternRequestMatcher.pathPattern(\"api/users\")");
        let findings = tools.diagnostics(&d);
        assert_eq!(findings.iter().filter(|f| f.code.as_deref() == Some(super::super::path_matcher::CODE)).count(), 1);
        let f = findings.iter().find(|f| f.code.as_deref() == Some(super::super::path_matcher::CODE)).unwrap();
        assert_eq!(&d.text[offset(&d.text, f.range.start).unwrap()..offset(&d.text, f.range.end).unwrap()], "api/users");
        d.text = d.text.replace("\"api/users\"", "\"/api/users\"");
        d.version += 1;
        assert!(tools.diagnostics(&d).iter().all(|f| f.code.as_deref() != Some(super::super::path_matcher::CODE)));
        assert!(h.process_runs().is_empty());
        assert_eq!(h.fetches(), 0);
    }
    #[test]
    fn stale_version_text_language_path_root_and_project_visible_source_refuse() {
        for mode in 0..6 {
            let (h, mut tools, mut d) = setup("PathPatternRequestMatcher.pathPattern(\"api/users\")");
            let a = tools.file_actions(&d, cursor(&d)).remove(0);
            match mode {
                0 => d.version += 1,
                1 => d.text.push(' '),
                2 => d.language = "kotlin".into(),
                3 => d.path = "/project/Other.java".into(),
                4 => h.set_project_root(Some("/other")),
                _ => h.add_file("Security.java", "changed"),
            };
            h.answer_next_prompt(Some(&["yes"]));
            assert!(tools.resolve_file_action(&d, a).is_err());
            assert!(h.process_runs().is_empty());
        }
    }
    #[test]
    fn current_unsaved_buffer_is_supported_but_not_claimed_saved_on_disk() {
        let (h, mut tools, d) = setup("PathPatternRequestMatcher.pathPattern(\"api/users\")");
        h.add_file("Security.java", "different disk");
        h.open_editor(&d.path, "java", &d.text, &[]);
        let a = tools.file_actions(&d, cursor(&d)).remove(0);
        h.answer_next_prompt(Some(&["yes"]));
        assert_eq!(tools.resolve_file_action(&d, a).unwrap().edits.len(), 1);
    }
    #[test]
    fn cancellation_setting_and_reselection_do_not_create_edits() {
        let (h, mut tools, d) = setup("PathPatternRequestMatcher.pathPattern(\"api/users\")");
        let a = tools.file_actions(&d, cursor(&d)).remove(0);
        h.answer_next_prompt(None);
        assert!(matches!(tools.resolve_file_action(&d, a.clone()), Err(Error::Cancelled)));
        h.set_setting("springSecurity.checkPathPatterns", "false");
        assert!(tools.file_actions(&d, cursor(&d)).is_empty());
        h.answer_next_prompt(Some(&["yes"]));
        assert!(tools.resolve_file_action(&d, a).is_err());
        h.set_setting("springSecurity.checkPathPatterns", "true");
        assert!(tools.file_actions(&d, Range::new(Position::new(0, 0), Position::new(0, 0))).is_empty());
    }
    #[test]
    fn foreign_implicit_matcher_and_malformed_source_do_not_get_this_fix() {
        for factory in [
            "http.requestMatchers(\"api/users\")",
            "AntPathRequestMatcher.antMatcher(\"api/users\")",
            "PathPatternRequestMatcher.pathPattern(\"api/users\" + dynamic)",
        ] {
            let (_h, mut tools, d) = setup(factory);
            assert!(tools.file_actions(&d, cursor(&d)).is_empty());
        }
    }
}

#[cfg(test)]
mod java_debug_tests {
    use super::*;
    use openidea_plugin::{mock::MockHost, text::position};
    const IMPORT: &str="import org.springframework.security.config.annotation.web.configuration.EnableWebSecurity;";
    fn doc() -> Document {
        Document{path:"/project/S.java".into(),language:"java".into(),version:6,text:format!("{IMPORT}\r\n/* 😀 retained */ @EnableWebSecurity(debug = /* keep */ true)\r\nclass S {{}}")}
    }
    fn cursor(d:&Document)->Range {let at=position(&d.text,d.text.find("true").unwrap());Range::new(at,at)}
    #[test]
    fn explicit_imported_class_debug_flag_returns_exact_previewed_utf16_edit() {
        let h=MockHost::install(); let mut tools=SecurityTools::default(); let d=doc();h.add_file("S.java",&d.text);
        let a=tools.file_actions(&d,cursor(&d)).remove(0);
        assert!(a.edits.is_empty()&&a.operations.is_empty());let replay=a.clone();
        h.answer_next_prompt(Some(&["yes"]));
        let fixed=tools.resolve_file_action(&d,a).unwrap();
        assert_eq!(fixed.edits.len(),1);assert!(fixed.operations.is_empty());
        let (path,edits)=&fixed.edits[0];assert_eq!(path,&d.path);assert_eq!(edits.len(),1);
        let e=&edits[0];let from=offset(&d.text,e.range.start).unwrap();let to=offset(&d.text,e.range.end).unwrap();
        assert_eq!(&d.text[from..to],"true");assert_eq!(e.text,"false");
        let mut after=d.text.clone();after.replace_range(from..to,&e.text);
        assert_eq!(after,d.text.replace("/* keep */ true","/* keep */ false"));
        let changed=Document{text:after,version:d.version+1,..d.clone()};
        assert!(openidea_plugin::diagnostics::DiagnosticsProvider::diagnostics(&mut tools,&changed).iter().all(|d|d.code.as_deref()!=Some("debug-enabled")));
        assert_eq!(spring_text::index::project_of(path).unwrap().read_file(path).unwrap(),d.text);
        assert!(tools.resolve_file_action(&d,replay).is_err());assert!(h.process_runs().is_empty());assert_eq!(h.fetches(),0);
    }
    #[test]
    fn debug_flag_cancel_stale_and_disabled_setting_return_no_mutation() {
        for mode in 0..5 {
            let h=MockHost::install();let mut tools=SecurityTools::default();let mut d=doc();h.add_file("S.java",&d.text);
            let a=tools.file_actions(&d,cursor(&d)).remove(0);
            match mode {
                0=>h.answer_next_prompt(None),
                1=>{d.version+=1;h.answer_next_prompt(Some(&["yes"]));},
                2=>{h.add_file("S.java","changed");h.answer_next_prompt(Some(&["yes"]));},
                3=>{h.set_setting("springSecurity.checkDebug","false");h.answer_next_prompt(Some(&["yes"]));},
                _=>{h.set_project_root(Some("/other"));h.answer_next_prompt(Some(&["yes"]));},
            }
            assert!(tools.resolve_file_action(&d,a).is_err());
        }
    }
    #[test]
    fn debug_flag_foreign_dynamic_shadowed_nested_and_inherited_sources_stay_quiet() {
        let _h=MockHost::install(); let mut tools=SecurityTools::default();let d=doc();
        for text in [
            d.text.replace(IMPORT,"import other.EnableWebSecurity;"),
            d.text.replace(IMPORT,""),
            format!("import other.*;\n{}",d.text),
            format!("import static other.Owner.EnableWebSecurity;\n{}",d.text),
            d.text.replace("class S","class S extends Base"),
            d.text.replace("class S","class EnableWebSecurity {} class S"),
            d.text.replace("debug = /* keep */ true","debug = FLAG"),
            d.text.replace("debug = /* keep */ true","debug = (true)"),
            d.text.replace("debug = /* keep */ true","debug = true, debug = true"),
            d.text.replace("debug = /* keep */ true","debug = false"),
            format!("{IMPORT} class Outer {{ @EnableWebSecurity(debug=true) class Nested {{}} }}"),
            d.text.replace("@EnableWebSecurity","// @EnableWebSecurity"),
        ] {
            let changed=Document{text,..d.clone()};
            assert!(tools.file_actions(&changed,range(&changed.text,0,changed.text.len())).is_empty(),"{}",changed.text);
        }
    }
    #[test]
    fn debug_flag_current_unsaved_buffer_and_feature_setting_are_respected() {
        let h=MockHost::install();let mut tools=SecurityTools::default();let d=doc();h.add_file("S.java","different disk");h.open_editor(&d.path,"java",&d.text,&[]);
        let a=tools.file_actions(&d,cursor(&d)).remove(0);h.answer_next_prompt(Some(&["yes"]));
        assert_eq!(tools.resolve_file_action(&d,a).unwrap().edits[0].1[0].text,"false");
        h.set_setting("springSecurity.checkDebug","false");assert!(tools.file_actions(&d,cursor(&d)).is_empty());
    }
}
