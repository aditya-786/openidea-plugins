//! Explicit global plugin preferences; scalar host settings remain read-only fallback.
use super::*;
use std::collections::{BTreeMap, BTreeSet};
use std::hash::BuildHasher;
const KEY: &str = "preferences:v1";
const KEY2: &str = "preferences:v2";
pub(crate) const VIEW: &str = "templates";
const FLAGS: [(&str, &str, bool); 5] = [
    ("missingGitignore", "Warn when a Git project has no .gitignore", true),
    ("ignoredEditing", "Show Ignored in the status bar", true),
    ("insertAtCursor", "Insert tool-window templates at the caret", false),
    ("unignoreActions", "Offer literal Unignore actions", true),
    ("packageSyntax", "Colour additional ignore files (shared)", true),
];
const EXTRA: [&str;22] = [".boringignore",".mtn-ignore","ignore-glob",".gitignore_global",".ignore",".rgignore",".fdignore",".vscodeignore",".containerignore",".deployignore",".distignore",".ebignore",".eleventyignore",".flooignore",".jpmignore",".nuxtignore",".openapi-generator-ignore",".stylintignore",".swagger-codegen-ignore",".tfignore",".tokeignore",".upignore"];
fn names() -> Vec<&'static str> {
    IGNORE_FILE_NAMES.into_iter().chain(EXTRA).collect()
}
#[derive(Clone, Debug, Default, PartialEq)]
struct Model {
    overrides: BTreeMap<String, bool>,
    disabled: BTreeSet<String>,
    disabled_support: BTreeSet<String>,
    disabled_creation: BTreeSet<String>,
}
#[derive(Clone,PartialEq)]
struct Raw { legacy:Option<String>, current:Option<String> }
fn decode(text:&str,schema:u64)->Result<Model,Error> {
    let invalid = || Error::Invalid("Stored Ignore preferences are malformed or from another schema/scope; preserved without changes.".into());
    let value=crate::strict_json::parse(text,16384).map_err(|_|invalid())?;
    let obj=value.as_object().ok_or_else(invalid)?;
    if obj.len()!=if schema==1 {4}else{6} || obj.get("schema").and_then(|v|v.as_u64())!=Some(schema)
        || obj.get("scope").and_then(|v|v.as_str())!=Some("application:aritti.ignore") {return Err(invalid());}
    let mut model=Model::default();
    let allowed=if schema==1 {names().into_iter().take(23).collect::<Vec<_>>()} else {names()};
    for (key,value) in obj.get("overrides").and_then(|v|v.as_object()).ok_or_else(invalid)? {
        if !FLAGS.iter().any(|(name,_,_)|name==key) || schema==1 && key=="packageSyntax" {return Err(invalid());}
        model.overrides.insert(key.clone(),value.as_bool().ok_or_else(invalid)?);
    }
    for (key,set) in [("disabledChecks",&mut model.disabled),("disabledSupport",&mut model.disabled_support),("disabledCreation",&mut model.disabled_creation)] {
        if schema==1 && key!="disabledChecks" {continue;}
        let values=obj.get(key).and_then(|v|v.as_array()).filter(|v|v.len()<=allowed.len()).ok_or_else(invalid)?;
        for value in values {
            let name=value.as_str().filter(|s|allowed.contains(s)).ok_or_else(invalid)?;
            if !set.insert(name.into()) {return Err(invalid());}
        }
    }
    Ok(model)
}
fn read()->Result<(Raw,Model),Error> {
    let bounded=|key|->Result<Option<String>,Error>{
        let value=host::storage::get(key)?;
        if value.as_ref().is_some_and(|v|v.len()>16384) {return Err("Stored Ignore preferences exceed 16KiB; preserved without changes.".into());}
        Ok(value)
    };
    let raw=Raw{legacy:bounded(KEY)?,current:bounded(KEY2)?};
    let model=match (&raw.current,&raw.legacy) {
        (Some(text),_)=>decode(text,2)?,
        (None,Some(text))=>decode(text,1)?,
        _=>Model::default(),
    };
    Ok((raw,model))
}
fn filename(path:&str)->&str {path.rsplit(['/', '\\']).next().unwrap_or(path)}
pub(crate) fn boolean(key: &str, default: bool) -> bool {
    read().ok().and_then(|(_,m)|m.overrides.get(key).copied()).unwrap_or_else(||host::setting_bool(key).unwrap_or(default))
}
pub(crate) fn enabled(path:&str)->bool {read().ok().is_none_or(|(_,m)|!m.disabled_support.contains(filename(path)))}
pub(crate) fn can_edit(path:&str)->bool {read().ok().is_some_and(|(_,m)|!m.disabled_support.contains(filename(path)))}
pub(crate) fn checks(path:&str)->bool {
    read().ok().is_none_or(|(_,m)|!m.disabled_support.contains(filename(path))&&!m.disabled.contains(filename(path)))
}
pub(crate) fn creation(path:&str)->bool {
    read().ok().is_some_and(|(_,m)|names().contains(&filename(path))&&!m.disabled_creation.contains(filename(path)))
}
pub(crate) fn creatable_names()->Vec<&'static str> {
    read().ok().map(|(_,m)|names().into_iter().filter(|name|!m.disabled_creation.contains(*name)).collect()).unwrap_or_default()
}
#[derive(Clone)]
struct Draft {
    token: String,
    raw: Raw,
    scalars: Vec<Option<String>>,
    model: Model,
    rejected: BTreeSet<String>,
    selected:usize,
}
#[derive(Default)]
pub(crate) struct State {
    draft: Option<Draft>,
    message: Option<String>,
    next: u64,
}
impl State {
    fn begin(&mut self) -> Result<(), Error> {
        let (raw, model) = read()?;
        self.next = self.next.checked_add(1).ok_or(Error::Stale)?;
        let random = std::collections::hash_map::RandomState::new();
        let token = format!("{:016x}{:016x}", random.hash_one((self.next, 0u8)), random.hash_one((self.next, 1u8)));
        self.draft = Some(Draft {
            token,
            raw,
            scalars: FLAGS.iter().map(|(key, _, _)| host::setting(key)).collect(),
            model,
            rejected: BTreeSet::new(),
            selected:0,
        });
        Ok(())
    }
    fn id(&self, key: &str) -> String {
        format!("{key}@{}", self.draft.as_ref().unwrap().token)
    }
    fn scoped<'a>(&self, id: &'a str) -> Result<&'a str, Error> {
        if id.len() > 128 {
            return Err(Error::Stale);
        }
        let (key, token) = id.rsplit_once('@').ok_or(Error::Stale)?;
        if self.draft.as_ref().is_none_or(|d| d.token != token) {
            return Err(Error::Stale);
        }
        Ok(key)
    }
    pub(crate) fn closes(&self, event: &ViewEvent) -> bool {
        matches!(event, ViewEvent::Button(id) if matches!(self.scoped(id).ok(),Some("prefs-save" | "prefs-cancel")))
    }
    fn render(&mut self) -> Result<View, Error> {
        if self.draft.is_none() {
            if let Err(error) = self.begin() {
                return Ok(View::message(&error.to_string()).button(Button::new("prefs-reload", "Reload Preserved Settings").icon("refresh")));
            }
        }
        let draft = self.draft.as_ref().unwrap();
        let mut fields = vec![Field::new(
            "scope",
            "Scope and ownership",
            FieldValue::readonly_text(
                "On or Off overrides Settings › Plugins › .ignore for all projects; Inherit keeps the value from Settings.",
                true,
            ),
        )];
        for (key, label, default) in FLAGS {
            let inherited = host::setting_bool(key).unwrap_or(default);
            let choice = match draft.model.overrides.get(key) {
                Some(true) => "On",
                Some(false) => "Off",
                None => "Inherit",
            };
            fields.push(Field::new(
                &self.id(key),
                &format!("{label} (inherited: {})", if inherited { "on" } else { "off" }),
                FieldValue::Choice(vec!["Inherit".into(), "On".into(), "Off".into()], choice.into()),
            ));
        }
        fields.push(Field::new("dialects","File-type options",FieldValue::readonly_text("Editor assistance turns checks, completion, navigation, markers and templates on or off for the chosen file type. Changes apply the next time a file is edited or opened.",true)));
        let names=names();let i=draft.selected;let name=names[i];
        fields.push(Field::new(&self.id("dialect-select"),"Ignore file type",FieldValue::Choice(names.iter().map(|name|(*name).into()).collect(),name.into())));
        let syntax=match check::dialect(name).unwrap_or(Dialect{syntax:check::Syntax::Glob,switchable:false}) {
            Dialect{switchable:true,..}=>"regexp; syntax directives",Dialect{syntax:check::Syntax::Regexp,..}=>"regexp",_=>"glob",
        };
        fields.push(Field::new(&self.id(&format!("support:{i}")),"Editor assistance",FieldValue::Checkbox(!draft.model.disabled_support.contains(name))));
        fields.push(Field::new(&self.id(&format!("create:{i}")),"Allow Create action for this file type",FieldValue::Checkbox(!draft.model.disabled_creation.contains(name))));
        fields.push(Field::new(&self.id(&format!("dialect:{i}")),&format!("Checks ({syntax}; requires editor assistance)"),FieldValue::Checkbox(!draft.model.disabled.contains(name))));
        fields.push(Field::new("colouring","Colouring scope",FieldValue::readonly_text("Turning off editor assistance removes this plugin's markers and highlighting. The IDE's own colouring and New menu are not affected.",true)));
        Ok(View::new(Body::Form(fields))
            .button(Button::new(&self.id("prefs-save"), "Save").icon("check"))
            .button(Button::new(&self.id("prefs-cancel"), "Cancel").icon("close"))
            .button(Button::new(&self.id("prefs-reload"), "Reload / discard draft").icon("refresh"))
            .status(self.message.as_deref().unwrap_or(
                "Changes apply when you press Save; Cancel discards them.",
            )))
    }
}
impl IgnorePlugin {
    pub(crate) fn preferences_render(&mut self) -> Result<View, Error> {
        self.preferences.render()
    }
    pub(crate) fn preferences_event(&mut self, event: ViewEvent, ctx: &Invocation) -> Result<(), Error> {
        let result: Result<(), Error> = (|| {
            match event {
                ViewEvent::Field(id, value) => {
                    let key = self.preferences.scoped(&id)?.to_owned();
                    let draft = self.preferences.draft.as_mut().ok_or(Error::Stale)?;
                    if FLAGS.iter().any(|(name, _, _)| *name == key) {
                        match value.as_str() {
                            "Inherit" => {
                                draft.model.overrides.remove(&key);
                            }
                            "On" | "Off" => {
                                draft.model.overrides.insert(key.clone(), value == "On");
                            }
                            _ => {
                                draft.rejected.insert(key);
                                return Err("Choose Inherit, On or Off.".into());
                            }
                        }
                    } else if key=="dialect-select" {
                        draft.selected=names().iter().position(|name|*name==value).ok_or("Choose one supported exact ignore filename.")?;
                    } else if let Some((kind,index))=key.split_once(':').and_then(|(kind,index)|index.parse::<usize>().ok().map(|i|(kind,i))) {
                        if index>=names().len() || index!=draft.selected || !matches!(kind,"dialect"|"support"|"create") {return Err(Error::Stale);}
                        if !matches!(value.as_str(),"true"|"false") {draft.rejected.insert(key);return Err("Checkbox values must be true or false.".into());}
                        let name=names()[index];
                        let set=match kind {"support"=>&mut draft.model.disabled_support,"create"=>&mut draft.model.disabled_creation,_=>&mut draft.model.disabled};
                        if value=="true" {set.remove(name);} else {set.insert(name.into());}
                    } else {
                        return Err("Unknown preference field.".into());
                    }
                    draft.rejected.remove(&key);
                    self.preferences.message = Some("Draft changed; nothing saved.".into());
                }
                ViewEvent::Button(id) => {
                    if id == "prefs-reload" && self.preferences.draft.is_none() {
                        self.preferences.begin()?;
                        return Ok(());
                    }
                    let key = self.preferences.scoped(&id)?.to_owned();
                    match key.as_str() {
                        "prefs-cancel" | "prefs-reload" => {
                            self.preferences.draft = None;
                            self.preferences.message = Some("Draft discarded; stored preferences unchanged.".into());
                        }
                        "prefs-save" => {
                            let draft = self.preferences.draft.as_ref().ok_or(Error::Stale)?;
                            if !draft.rejected.is_empty() {
                                return Err("Correct invalid fields before saving.".into());
                            }
                            let current = read()?.0;
                            let scalars: Vec<_> = FLAGS.iter().map(|(key, _, _)| host::setting(key)).collect();
                            if current != draft.raw || scalars != draft.scalars {
                                return Err(
                                    "Preferences or settings changed elsewhere. Reload before saving; your changes are kept until then.".into()
                                );
                            }
                            let encoded=serde_json::json!({"schema":2,"scope":"application:aritti.ignore","overrides":draft.model.overrides,"disabledChecks":draft.model.disabled,"disabledSupport":draft.model.disabled_support,"disabledCreation":draft.model.disabled_creation}).to_string();
                            if encoded.len()>16384 {return Err("Ignore preferences exceed 16KiB; nothing saved.".into());}
                            host::storage::set(KEY2, &encoded)?;
                            self.preferences.draft = None;
                            self.preferences.message = Some(
                                "Saved. Other open files update their checks when next opened or saved.".into(),
                            );
                            show_ignored_status(ctx.editor().as_ref().map(|e| e.path()).as_deref());
                        }
                        _ => return Err("Unknown preferences button.".into()),
                    }
                }
                _ => {}
            }
            Ok(())
        })();
        if let Err(error) = &result {
            self.preferences.message = Some(error.to_string());
        }
        host::refresh_view(VIEW);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{Plugin, mock::MockHost};
    fn render(plugin: &mut IgnorePlugin) {
        plugin.preferences_render().unwrap();
    }
    fn field(plugin: &mut IgnorePlugin, key: &str, value: &str, ctx: &Invocation) -> Result<(), Error> {
        let id = plugin.preferences.id(key);
        plugin.preferences_event(ViewEvent::Field(id, value.into()), ctx)
    }
    fn button(plugin: &mut IgnorePlugin, key: &str, ctx: &Invocation) -> Result<(), Error> {
        let id = plugin.preferences.id(key);
        plugin.preferences_event(ViewEvent::Button(id), ctx)
    }
    #[test]
    fn toolbar_preferences_share_the_template_view_without_a_project() {
        use openidea_plugin::views::Views;
        fn send(plugin:&mut IgnorePlugin,host:&MockHost,event:serde_json::Value)->serde_json::Value {
            let request=serde_json::json!({"channel":"ui-kit","sequence":1,"project":"@ui-kit:project-independent","op":"event","event":event});
            plugin.handle(VIEW,ViewEvent::Message(request.to_string()),&host.invocation()).unwrap();
            serde_json::from_str(&host.view_messages().last().unwrap().1).unwrap()
        }
        let host=MockHost::install();host.set_project_root(None);
        let mut plugin=IgnorePlugin::default();
        let opened=send(&mut plugin,&host,serde_json::json!({"kind":"button","key":"preferences"}));
        assert_eq!(opened["model"]["body"]["kind"],"form");
        let key=plugin.preferences.id("missingGitignore");
        send(&mut plugin,&host,serde_json::json!({"kind":"field","key":key,"value":"Off"}));
        send(&mut plugin,&host,serde_json::json!({"kind":"button","key":"preferences-back"}));
        assert!(!plugin.show_preferences);assert!(host.storage().is_empty());
        send(&mut plugin,&host,serde_json::json!({"kind":"button","key":"preferences"}));
        assert_eq!(plugin.preferences.id("missingGitignore"),key,"Back retains the unsaved draft");
        let save=plugin.preferences.id("prefs-save");
        let saved=send(&mut plugin,&host,serde_json::json!({"kind":"button","key":save}));
        assert!(saved.get("error").is_none(),"{saved}");assert!(!plugin.show_preferences);
        assert!(!boolean("missingGitignore",true));
        // Project-independent preferences must not authorize template creation.
        let denied=send(&mut plugin,&host,serde_json::json!({"kind":"button","key":"new"}));
        assert!(denied.get("error").is_some());
        assert!(host.process_runs().is_empty());
    }

    #[test]
    fn global_form_works_without_project_and_defaults_inherit_all_four_flags() {
        let host = MockHost::install();
        host.set_project_root(None);
        let ctx = host.invocation();
        let mut plugin = IgnorePlugin::default();
        plugin.run_command("preferences", &ctx, None).unwrap();
        assert_eq!(host.revealed_views(), vec![VIEW]);
        let view = plugin.preferences_render().unwrap();
        let Body::Form(fields) = view.body else { panic!() };
        assert_eq!(fields.len(), 12);
        assert_eq!(names().len(), 42);
        for (key, _, default) in FLAGS {
            assert_eq!(boolean(key, !default), !default);
            assert!(
                fields.iter().any(|f| f.key.starts_with(&format!("{key}@")) && matches!(&f.value,FieldValue::Choice(_,v) if v=="Inherit"))
            );
        }
        assert!(host.storage().is_empty());
        assert!(host.process_runs().is_empty());
        assert_eq!(host.fetches(), 0);
    }
    #[test]
    fn save_cancel_and_inherit_control_actual_scalar_reader_with_reclaim() {
        let host = MockHost::install();
        host.set_setting("insertAtCursor", "true");
        let ctx = host.invocation();
        let mut plugin = IgnorePlugin::default();
        render(&mut plugin);
        field(&mut plugin, "insertAtCursor", "Off", &ctx).unwrap();
        assert!(setting("insertAtCursor", false));
        button(&mut plugin, "prefs-cancel", &ctx).unwrap();
        assert!(host.storage().is_empty());
        render(&mut plugin);
        for (key, _, _) in FLAGS {
            field(&mut plugin, key, "Off", &ctx).unwrap();
        }
        button(&mut plugin, "prefs-save", &ctx).unwrap();
        for (key, _, _) in FLAGS {
            assert!(!setting(key, true), "{key}");
        }
        let mut reclaimed = IgnorePlugin::default();
        render(&mut reclaimed);
        field(&mut reclaimed, "insertAtCursor", "Inherit", &ctx).unwrap();
        button(&mut reclaimed, "prefs-save", &ctx).unwrap();
        assert!(setting("insertAtCursor", false));
        assert_eq!(host.storage().len(), 1);
    }
    #[test]
    fn exact_dialect_toggle_clears_tracked_diagnostics_and_disables_only_check_fixes() {
        let host = MockHost::install();
        host.add_file(".gitignore", "a\na\n");
        let ctx = host.invocation();
        let mut plugin = IgnorePlugin::default();
        host.open_editor("/project/.gitignore", "ignore", "a\na\n", &[]);
        plugin.handle_events(&[Event::FileOpened("/project/.gitignore".into())], &ctx);
        host.refresh_diagnostics(&mut plugin);
        assert!(!host.provider_diagnostics()["/project/.gitignore"].is_empty());
        render(&mut plugin);
        field(&mut plugin, "dialect:0", "false", &ctx).unwrap();
        button(&mut plugin, "prefs-save", &ctx).unwrap();
        host.refresh_diagnostics(&mut plugin);
        assert!(host.provider_diagnostics().get("/project/.gitignore").is_none_or(Vec::is_empty));
        assert!(!checks("/project/.gitignore"));
        assert!(checks("/project/.hgignore"));
        assert!(checks("/project/.customignore"));
        let d = Document { path: "/project/.gitignore".into(), language: "ignore".into(), version: 1, text: "a\na\n".into() };
        assert!(plugin.code_actions(&d, Range::new(Position::new(1, 0), Position::new(1, 0))).is_empty());
        use openidea_plugin::file_actions::FileActions;
        assert!(plugin.file_actions(&d, Range::default()).iter().any(|a| a.title.contains("Create exact-name")));
        plugin.handle_events(&[Event::FileSaved(d.path.clone())], &ctx);
        host.refresh_diagnostics(&mut plugin);
        assert!(host.provider_diagnostics().get(&d.path).is_none_or(Vec::is_empty));
    }
    #[test]
    fn invalid_values_block_save_until_corrected_and_scoped_ids_expire() {
        let host = MockHost::install();
        let ctx = host.invocation();
        let mut plugin = IgnorePlugin::default();
        render(&mut plugin);
        let old = plugin.preferences.id("insertAtCursor");
        assert!(field(&mut plugin, "insertAtCursor", "true", &ctx).is_err());
        assert!(button(&mut plugin, "prefs-save", &ctx).is_err());
        assert!(host.storage().is_empty());
        field(&mut plugin, "insertAtCursor", "On", &ctx).unwrap();
        assert!(field(&mut plugin, "dialect:0", "yes", &ctx).is_err());
        assert!(button(&mut plugin, "prefs-save", &ctx).is_err());
        field(&mut plugin, "dialect:0", "true", &ctx).unwrap();
        button(&mut plugin, "prefs-save", &ctx).unwrap();
        render(&mut plugin);
        assert!(plugin.preferences_event(ViewEvent::Field(old, "Off".into()), &ctx).is_err());
        assert!(setting("insertAtCursor", false));
    }
    #[test]
    fn other_session_or_external_storage_and_scalar_changes_never_overwrite() {
        for mode in 0..3 {
            let host = MockHost::install();
            let ctx = host.invocation();
            let mut a = IgnorePlugin::default();
            let mut b = IgnorePlugin::default();
            render(&mut a);
            render(&mut b);
            assert_ne!(a.preferences.id("prefs-save"), b.preferences.id("prefs-save"));
            let other = a.preferences.id("prefs-save");
            assert!(b.preferences_event(ViewEvent::Button(other), &ctx).is_err());
            match mode {
                0 => {
                    field(&mut a, "insertAtCursor", "On", &ctx).unwrap();
                    button(&mut a, "prefs-save", &ctx).unwrap();
                }
                1 => host::storage::set(KEY, "foreign").unwrap(),
                _ => host.set_setting("insertAtCursor", "true"),
            }
            let before = host.storage();
            assert!(button(&mut b, "prefs-save", &ctx).is_err());
            assert_eq!(host.storage(), before);
            assert!(b.preferences.draft.is_some());
        }
    }
    #[test]
    fn malformed_foreign_or_oversized_preferences_are_preserved_and_never_overwritten() {
        for raw in [
            "{broken".into(),
            serde_json::json!({"schema":2,"scope":"application:aritti.ignore","overrides":{},"disabledChecks":[]}).to_string(),
            serde_json::json!({"schema":1,"scope":"project:/other","overrides":{},"disabledChecks":[]}).to_string(),
            serde_json::json!({"schema":1,"scope":"application:aritti.ignore","overrides":{"unknown":true},"disabledChecks":[]})
                .to_string(),
            serde_json::json!({"schema":1,"scope":"application:aritti.ignore","overrides":{},"disabledChecks":["unknown"]}).to_string(),
            " ".repeat(16385),
        ] {
            let host = MockHost::install();
            host::storage::set(KEY, &raw).unwrap();
            let ctx = host.invocation();
            let mut plugin = IgnorePlugin::default();
            let view = plugin.preferences_render().unwrap();
            assert!(matches!(view.body, Body::Message(_)));
            assert!(plugin.preferences.draft.is_none());
            assert!(plugin.preferences_event(ViewEvent::Button("prefs-save".into()), &ctx).is_err());
            assert_eq!(host.storage()[KEY], raw);
        }
    }
    #[test]
    fn duplicate_decoded_envelope_or_preference_keys_are_preserved_and_never_enter_a_draft() {
        for raw in [
            r#"{"schema":9,"schema":1,"scope":"application:aritti.ignore","overrides":{},"disabledChecks":[]}"#,
            r#"{"schema":1,"scope":"foreign","scope":"application:aritti.ignore","overrides":{},"disabledChecks":[]}"#,
            r#"{"schema":1,"scope":"application:aritti.ignore","overrides":{"insertAtCursor":false,"insertAtCursor":true},"disabledChecks":[]}"#,
            r#"{"schema":1,"scope":"application:aritti.ignore","overrides":{"insertAtCursor":false,"\u0069nsertAtCursor":true},"disabledChecks":[]}"#,
        ] {
            let host=MockHost::install();host::storage::set(KEY,raw).unwrap();let mut plugin=IgnorePlugin::default();
            assert!(matches!(plugin.preferences_render().unwrap().body,Body::Message(_)),"{raw}");
            assert!(plugin.preferences.draft.is_none());
            assert!(plugin.preferences_event(ViewEvent::Button("prefs-save".into()),&host.invocation()).is_err());
            assert_eq!(host.storage()[KEY],raw);
            assert!(host.process_runs().is_empty()&&host.requests().is_empty());
        }
    }
    #[test]
    fn valid_escaped_preference_key_is_decoded_without_rewriting_stored_evidence() {
        let host=MockHost::install();
        let raw=r#"{"schema":1,"scope":"application:aritti.ignore","overrides":{"\u0069nsertAtCursor":true},"disabledChecks":[".gitignore"]}"#;
        host::storage::set(KEY,raw).unwrap();let mut plugin=IgnorePlugin::default();render(&mut plugin);
        assert!(setting("insertAtCursor",false)&&!checks("/project/.gitignore"));
        assert_eq!(host.storage()[KEY],raw);
        assert!(plugin.preferences.draft.is_some());
    }

    #[test]
    fn stored_dialect_editor_disable_suppresses_registered_directory_markers() {
        let _host=MockHost::install();
        let raw=serde_json::json!({"schema":2,"scope":"application:aritti.ignore","overrides":{},"disabledChecks":[],"disabledSupport":[".gitignore"],"disabledCreation":[]}).to_string();
        host::storage::set("preferences:v2",&raw).unwrap();
        let mut plugin=IgnorePlugin::default();
        let doc=Document{path:"/project/.gitignore".into(),language:"ignore".into(),version:1,text:"build/\n".into()};
        assert!(plugin.line_markers(&doc).is_empty(),"explicit per-file editor disable must suppress this build's marker output");
    }

    fn config(_host:&MockHost,support:&[&str],creation:&[&str],overrides:serde_json::Value) {
        let raw=serde_json::json!({"schema":2,"scope":"application:aritti.ignore","overrides":overrides,"disabledChecks":[],"disabledSupport":support,"disabledCreation":creation}).to_string();
        host::storage::set(KEY2,&raw).unwrap();
    }
    fn select(plugin:&mut IgnorePlugin,name:&str,ctx:&Invocation)->usize {
        field(plugin,"dialect-select",name,ctx).unwrap();names().iter().position(|n|*n==name).unwrap()
    }
    fn source_doc(path:&str,text:&str)->Document {Document{path:path.into(),language:"ignore".into(),version:1,text:text.into()}}
    #[test]
    fn editor_assistance_gates_owned_checks_completion_navigation_markers_tokens_and_append_only_for_selected_type() {
        use openidea_plugin::{file_actions::FileActions,semantic_tokens::SemanticTokens};
        let host=MockHost::install();host.add_file(".gitignore","a\na\n");host.add_file("file.txt","target");
        let ctx=host.invocation();let mut plugin=IgnorePlugin::default();
        let duplicate=source_doc("/project/.gitignore","a\na\n");
        let completion=source_doc("/project/.gitignore","fi");let navigation=source_doc("/project/.gitignore","file.txt\n");
        let decorated=source_doc("/project/.gitignore","build/\n# note\n*.o\n");
        assert!(!plugin.completion(&completion,Position::new(0,2),None).is_empty());
        assert_eq!(plugin.definition(&navigation,Position::new(0,2)).len(),1);
        assert!(!plugin.line_markers(&decorated).is_empty()&&!plugin.semantic_tokens(&decorated).is_empty());
        host.open_editor(&duplicate.path,"ignore",&duplicate.text,&[]);plugin.handle_events(&[Event::FileOpened(duplicate.path.clone())],&ctx);host.refresh_diagnostics(&mut plugin);assert!(!host.provider_diagnostics()[&duplicate.path].is_empty());
        render(&mut plugin);field(&mut plugin,"support:0","false",&ctx).unwrap();button(&mut plugin,"prefs-save",&ctx).unwrap();
        assert!(!checks(&duplicate.path)&&!enabled(&duplicate.path)&&creation(&duplicate.path));
        assert!(plugin.completion(&completion,Position::new(0,2),None).is_empty());
        assert!(plugin.definition(&navigation,Position::new(0,2)).is_empty());
        assert!(plugin.line_markers(&decorated).is_empty()&&plugin.semantic_tokens(&decorated).is_empty());
        assert!(plugin.code_actions(&duplicate,Range::new(Position::new(1,0),Position::new(1,0))).is_empty());
        host.refresh_diagnostics(&mut plugin);assert!(host.provider_diagnostics().get(&duplicate.path).is_none_or(Vec::is_empty));
        let actions=plugin.file_actions(&decorated,Range::default());
        assert!(actions.iter().any(|a|a.title.contains("Create exact-name"))&&!actions.iter().any(|a|a.title.contains("Append ignore")));
        let other=source_doc("/project/.dockerignore","build/\n# note\n*.o\n");
        assert!(!plugin.line_markers(&other).is_empty()&&!plugin.semantic_tokens(&other).is_empty());
        assert!(host.process_runs().is_empty()&&host.requests().is_empty());
    }
    #[test]
    fn create_options_are_independent_filter_picker_and_revalidate_stale_creation_actions() {
        use openidea_plugin::file_actions::{FileActions,FileOperation};
        let host=MockHost::install();let ctx=host.invocation();let mut plugin=IgnorePlugin::default();
        let doc=Document{path:"/project/src/a.rs".into(),language:"rust".into(),version:1,text:String::new()};
        host.add_file(&doc.path,"");host.open_editor(&doc.path,"rust","",&[]);
        let lazy=plugin.file_actions(&doc,Range::default()).into_iter().find(|a|a.title.contains("Create exact-name")).unwrap();
        render(&mut plugin);let i=select(&mut plugin,".dockerignore",&ctx);
        field(&mut plugin,&format!("create:{i}"),"false",&ctx).unwrap();button(&mut plugin,"prefs-save",&ctx).unwrap();
        assert!(!creation(".dockerignore")&&creation(".gitignore")&&enabled(".dockerignore"));
        assert!(!creatable_names().contains(&".dockerignore"));
        host.answer_next_prompt(Some(&[".dockerignore"]));assert!(plugin.resolve_file_action(&doc,lazy).is_err());
        let lazy=plugin.file_actions(&doc,Range::default()).into_iter().find(|a|a.title.contains("Create exact-name")).unwrap();
        for answer in [Some(&["ignore-glob"][..]),Some(&[""][..]),Some(&["yes"][..])] {host.answer_next_prompt(answer);}
        let ready=plugin.resolve_file_action(&doc,lazy).unwrap();
        assert!(matches!(&ready.operations[..],[FileOperation::Create{path,overwrite:false,..}] if path=="/project/ignore-glob"));
        assert!(host.invocation().project().unwrap().read_file("ignore-glob").is_err());
        assert!(host.process_runs().is_empty());
    }
    #[test]
    fn stale_append_entry_and_all_disabled_creation_paths_never_return_edits() {
        use openidea_plugin::file_actions::FileActions;
        let host=MockHost::install();let mut plugin=IgnorePlugin::default();
        let doc=source_doc("/project/.gitignore","# source\n");host.add_file(&doc.path,&doc.text);host.open_editor(&doc.path,"ignore",&doc.text,&[]);
        let append=plugin.file_actions(&doc,Range::default()).into_iter().find(|a|a.title.contains("Append ignore")).unwrap();
        config(&host,&[".gitignore"],&[],serde_json::json!({}));
        assert!(matches!(plugin.resolve_file_action(&doc,append),Err(Error::Stale)));
        config(&host,&[],&[],serde_json::json!({}));
        let source=Document{path:"/project/src/a.rs".into(),language:"rust".into(),version:1,text:String::new()};host.add_file(&source.path,"");
        let entry=plugin.file_actions(&source,Range::default()).into_iter().find(|a|a.title.starts_with("Add /")).unwrap();
        config(&host,&[".gitignore"],&[],serde_json::json!({}));assert!(matches!(plugin.resolve_file_action(&source,entry),Err(Error::Stale)));
        let disabled=names();config(&host,&[],&disabled,serde_json::json!({}));
        assert!(!plugin.file_actions(&source,Range::default()).iter().any(|a|a.title.contains("Create exact-name")));
        assert_eq!(host.invocation().project().unwrap().read_file(&doc.path).unwrap(),doc.text);
        assert!(host.process_runs().is_empty());
    }
    #[test]
    fn shared_syntax_switch_has_language_scope_without_guessing_active_filename_or_changing_builtin_grammar() {
        use openidea_plugin::{syntax::SyntaxProvider,semantic_tokens::SemanticTokens};
        let host=MockHost::install();let ctx=host.invocation();let mut plugin=IgnorePlugin::default();let text="# note\n*.o\n";
        assert!(!plugin.highlights("dotignore",text).is_empty());
        config(&host,&[".chefignore"],&[],serde_json::json!({}));
        host.open_editor("/project/.chefignore","dotignore",text,&[]);
        let left=plugin.highlights("dotignore",text);
        host.open_editor("/project/.mtn-ignore","dotignore",text,&[]);
        assert_eq!(left,plugin.highlights("dotignore",text));
        render(&mut plugin);field(&mut plugin,"packageSyntax","Off",&ctx).unwrap();button(&mut plugin,"prefs-save",&ctx).unwrap();
        assert!(plugin.highlights("dotignore",text).is_empty()&&plugin.highlights("ignore",text).is_empty());
        assert!(!plugin.semantic_tokens(&source_doc("/project/.dockerignore",text)).is_empty());
        assert!(host.process_runs().is_empty()&&host.requests().is_empty());
    }
    #[test]
    fn compact_selector_retains_other_dialect_drafts_and_rejects_stale_fields_after_selection_switch() {
        let host=MockHost::install();let ctx=host.invocation();let mut plugin=IgnorePlugin::default();render(&mut plugin);
        let stale=plugin.preferences.id("support:0");let i=select(&mut plugin,".dockerignore",&ctx);
        assert!(plugin.preferences_event(ViewEvent::Field(stale,"false".into()),&ctx).is_err());
        field(&mut plugin,&format!("support:{i}"),"false",&ctx).unwrap();
        assert!(field(&mut plugin,&format!("create:{i}"),"yes",&ctx).is_err());assert!(button(&mut plugin,"prefs-save",&ctx).is_err());
        field(&mut plugin,&format!("create:{i}"),"false",&ctx).unwrap();
        select(&mut plugin,".gitignore",&ctx);field(&mut plugin,"dialect:0","false",&ctx).unwrap();
        let Body::Form(fields)=plugin.preferences_render().unwrap().body else{panic!("form")};assert_eq!(fields.len(),12);
        button(&mut plugin,"prefs-save",&ctx).unwrap();
        assert!(enabled(".gitignore")&&!checks(".gitignore")&&!enabled(".dockerignore")&&!creation(".dockerignore"));
    }
    #[test]
    fn first_v2_save_copies_legacy_settings_without_overwriting_frozen_build_storage() {
        let host=MockHost::install();host.set_project_root(None);let ctx=host.invocation();
        let legacy=r#"{"schema":1,"scope":"application:aritti.ignore","overrides":{"insertAtCursor":true},"disabledChecks":[".hgignore"]}"#;
        host::storage::set(KEY,legacy).unwrap();let mut plugin=IgnorePlugin::default();render(&mut plugin);
        assert!(setting("insertAtCursor",false)&&!checks(".hgignore"));assert_eq!(host.storage().len(),1);
        field(&mut plugin,"support:0","false",&ctx).unwrap();button(&mut plugin,"prefs-save",&ctx).unwrap();
        assert_eq!(host.storage()[KEY],legacy);assert!(host.storage().contains_key(KEY2));
        let mut reclaimed=IgnorePlugin::default();render(&mut reclaimed);
        assert!(!enabled(".gitignore")&&!checks(".hgignore")&&setting("insertAtCursor",false));
        assert_eq!(host.storage()[KEY],legacy);
    }
    #[test]
    fn both_preference_keys_and_shared_scalar_are_checked_before_saving_a_draft() {
        for mode in 0..3 {
            let host=MockHost::install();config(&host,&[],&[],serde_json::json!({}));let ctx=host.invocation();let mut plugin=IgnorePlugin::default();render(&mut plugin);
            match mode {
                0=>host::storage::set(KEY,r#"{"schema":1,"scope":"application:aritti.ignore","overrides":{},"disabledChecks":[]}"#).unwrap(),
                1=>config(&host,&[".dockerignore"],&[],serde_json::json!({})),
                _=>host.set_setting("packageSyntax","false"),
            }
            let before=host.storage();assert!(button(&mut plugin,"prefs-save",&ctx).is_err());assert_eq!(host.storage(),before);
        }
    }
    #[test]
    fn malformed_v2_preserves_both_stores_and_refuses_creation_without_falling_back_to_legacy() {
        for raw in [
            r#"{"schema":2,"scope":"foreign","overrides":{},"disabledChecks":[],"disabledSupport":[],"disabledCreation":[]}"#,
            r#"{"schema":2,"scope":"application:aritti.ignore","overrides":{},"disabledChecks":[],"disabledSupport":[".gitignore",".gitignore"],"disabledCreation":[]}"#,
            r#"{"schema":2,"scope":"application:aritti.ignore","overrides":{},"disabledChecks":[],"disabledSupport":[],"disabledCreation":["unknown"]}"#,
            r#"{"schema":2,"scope":"application:aritti.ignore","overrides":{},"disabledChecks":[],"disabledSupport":[],"disabledCreation":[],"disabledCreation":[]}"#,
        ] {
            let host=MockHost::install();let legacy=r#"{"schema":1,"scope":"application:aritti.ignore","overrides":{},"disabledChecks":[]}"#;host::storage::set(KEY,legacy).unwrap();host::storage::set(KEY2,raw).unwrap();
            let mut plugin=IgnorePlugin::default();assert!(matches!(plugin.preferences_render().unwrap().body,Body::Message(_)));
            assert!(creatable_names().is_empty()&&!creation(".gitignore"));assert_eq!(host.storage()[KEY],legacy);assert_eq!(host.storage()[KEY2],raw);
        }
    }
    #[test]
    fn exact_option_catalog_covers_shipped_filenames_and_keeps_a_small_global_form() {
        let host=MockHost::install();let mut plugin=IgnorePlugin::default();
        let filenames=include_str!("../languages/dotignore.toml").lines().find(|line|line.starts_with("filenames = ")).unwrap();
        for name in filenames.split('"').skip(1).step_by(2) {assert!(names().contains(&name),"{name}");}
        assert_eq!(names().len(),names().into_iter().collect::<BTreeSet<_>>().len());
        let Body::Form(fields)=plugin.preferences_render().unwrap().body else{panic!("form")};
        assert_eq!(fields.len(),12);assert!(fields.iter().any(|f|matches!(&f.value,FieldValue::Choice(items,_) if items.len()==42)));
        assert!(host.storage().is_empty()&&host.process_runs().is_empty());
    }

    #[test]
    fn both_raw_baselines_are_bounded_even_when_the_other_version_is_active() {
        let host=MockHost::install();config(&host,&[],&[],serde_json::json!({}));
        let modern=host.storage()[KEY2].clone();let oversized=" ".repeat(16385);host::storage::set(KEY,&oversized).unwrap();
        let mut plugin=IgnorePlugin::default();assert!(matches!(plugin.preferences_render().unwrap().body,Body::Message(_)));
        assert!(plugin.preferences.draft.is_none()&&creatable_names().is_empty());
        assert_eq!(host.storage()[KEY],oversized);assert_eq!(host.storage()[KEY2],modern);
    }

}
