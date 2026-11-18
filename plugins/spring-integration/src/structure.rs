use crate::{Integration, OWNER_WINDOW, graph};
use batch::Batch;
use modulith::ModulithTools;
use openidea_plugin::diagnostics::DiagnosticsProvider;
use openidea_plugin::providers::{CodeAction, CompletionItem, Document, LineMarker, Providers};
use openidea_plugin::views::{View, ViewEvent, Viewport, Views};
use openidea_plugin::{Diagnostic, Error, Invocation, Location, Plugin, Position, Range, host};
use spring_text::index::project_of;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};
use ui_kit::sections::{SectionDef, Switcher};

const SECTIONS: &[SectionDef] = &[
    SectionDef { id: "channels", label: "Channels" },
    SectionDef { id: "channel-table", label: "Channel Table" },
    SectionDef { id: "batch", label: "Batch" },
    SectionDef { id: "modules", label: "Modules" },
    SectionDef { id: "diagram", label: "Diagram" },
];
const SECTION_KEY: &str = "structure.section";
const APPLIES_TTL: Duration = Duration::from_secs(30);
const EVENT_BUDGET: Duration = Duration::from_millis(500);

fn is_build_file(path: &str) -> bool {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    name == "pom.xml" || name.ends_with(".gradle") || name.ends_with(".gradle.kts")
}

fn builds_changed(event: &openidea_plugin::Event) -> bool {
    match event {
        openidea_plugin::Event::FileSaved(path) => is_build_file(path),
        openidea_plugin::Event::FilesChanged(batch) => batch.created.iter().chain(&batch.modified).chain(&batch.removed).any(|path| is_build_file(path)),
        _ => false,
    }
}

#[derive(Clone, Copy, Default)]
struct Applies {
    integration: bool,
    batch: bool,
    modulith: bool,
}

fn applies_of(project: &openidea_plugin::Project) -> Applies {
    Applies {
        integration: spring_text::build::applies(project, &["-integration"]),
        batch: spring_text::build::applies(project, &["-batch"]),
        modulith: spring_text::build::applies(project, &["spring-modulith"]),
    }
}

pub struct Structure {
    integration: Integration,
    batch: Batch,
    modulith: ModulithTools,
    sections: Switcher,
    diagrams: BTreeMap<String, graph::External>,
    applies: BTreeMap<String, (Instant, Applies)>,
    batch_links: crate::batch_actions::Links,
    batch_step_rename: crate::batch_step_rename::Rename,
    channel_renames: crate::navigation::ChannelRenames,
    import_renames: crate::import_tree::Renames,
    batch_runtime:crate::batch_runtime::Runtime,
    watching: bool,
}

impl Default for Structure {
    fn default() -> Self {
        Self {
            integration: Integration::default(),
            batch: Batch::default(),
            modulith: ModulithTools::default(),
            sections: Switcher::new(SECTION_KEY, SECTIONS),
            diagrams: BTreeMap::new(),
            applies: BTreeMap::new(),
            batch_links: Default::default(),
            batch_step_rename: Default::default(),
            channel_renames: Default::default(),
            import_renames: Default::default(),
            batch_runtime:Default::default(),
            watching: false,
        }
    }
}

impl Structure {
    fn source_status(&self, mut view: View) -> View {
        if !self.watching {
            let hint = "Automatic source updates unavailable; use Refresh after source changes";
            view.status = Some(match view.status.take() { Some(status) if !status.is_empty() => format!("{status} · {hint}"), _ => hint.into() });
        }
        view
    }
    fn applicable(&mut self, doc: &Document) -> Applies {
        let Some(project) = project_of(&doc.path) else {
            return Applies::default();
        };
        let Some(root) = project.root().ok().filter(|r| !r.is_empty()) else { return Applies::default() };
        if let Some((at, applies)) = self.applies.get(&root)
            && at.elapsed() < APPLIES_TTL
        {
            return *applies;
        }

        let applies =
            applies_of(&project);
        self.applies.insert(root, (Instant::now(), applies));
        applies
    }

    fn wants_integration(&mut self, doc: &Document) -> bool {
        matches!(doc.language.as_str(), "java" | "kotlin" | "xml") && self.applicable(doc).integration
    }

    fn wants_batch(&mut self, doc: &Document) -> bool {
        self.applicable(doc).batch
    }

    fn wants_modulith(&mut self, doc: &Document) -> bool {
        matches!(doc.language.as_str(),"java"|"kotlin") && self.applicable(doc).modulith
    }
}

fn module_data(module: &str, action: CodeAction) -> CodeAction {
    CodeAction { data: action.data.map(|data| format!("{module}:{data}")), ..action }
}

impl Providers for Structure {
    fn completion(&mut self, doc: &Document, at: Position, trigger: Option<&str>) -> Vec<CompletionItem> {
        if doc.language=="xml" && !spring_text::xml::analysis_valid(&doc.text) {return Vec::new();}
        if let Some(items)=crate::batch_navigation::completion(doc,at){return items;}
        let mut items = Vec::new();
        if self.wants_integration(doc) {
            items.extend(self.integration.completion(doc, at, trigger));
        }
        if self.wants_batch(doc) {
            items.extend(self.batch.completion(doc, at, &mut self.integration.indexes));
        }
        if self.wants_modulith(doc) {
            items.extend(self.modulith.completion(doc, at, &mut self.integration.indexes));
        }
        items
    }

    fn definition(&mut self, doc: &Document, at: Position) -> Vec<Location> {
        if let Some(found)=crate::java_hover::definition(doc,at) {return found;}
        if doc.language == "xml" && !spring_text::xml::analysis_valid(&doc.text) { return vec![]; }
        if let Some(found) = crate::navigation::definition(doc, at) { return found; }
        if let Some(found)=crate::batch_navigation::definition(doc,at){return found;}
        let mut found = Vec::new();
        if self.wants_integration(doc) {
            found.extend(self.integration.definition(doc, at));
        }
        if self.wants_batch(doc) {
            found.extend(self.batch.definition(doc, at, &mut self.integration.indexes));
        }
        if self.wants_modulith(doc) {found.extend(self.modulith.definition(doc,at,&mut self.integration.indexes));}
        found
    }

    fn code_actions(&mut self, doc: &Document, selection: Range) -> Vec<CodeAction> {
        if !self.wants_modulith(doc) {
            return Vec::new();
        }
        let actions = self.modulith.code_actions(doc, selection, &mut self.integration.indexes);
        actions.into_iter().map(|action| module_data(modulith::WINDOW_SECTION, action)).collect()
    }

    fn resolve_code_action(&mut self, doc: &Document, action: CodeAction) -> Result<CodeAction, Error> {
        let Some(data) = action.data.as_deref() else {
            return Ok(action);
        };
        let Some((module, rest)) = data.split_once(':') else {
            return Err(Error::Invalid(format!("code action {data}")));
        };
        let routed = CodeAction { data: Some(rest.to_owned()), ..action };
        match module {
            modulith::WINDOW_SECTION => self.modulith.resolve_code_action(doc, routed).map(|resolved| module_data(module, resolved)),
            batch::WINDOW_SECTION => self.batch.resolve_code_action(doc, routed).map(|resolved| module_data(module, resolved)),
            crate::WINDOW_SECTION => self.integration.resolve_code_action(doc, routed).map(|resolved| module_data(module, resolved)),
            _ => Err(Error::Invalid(format!("code action {data}"))),
        }
    }

    fn line_markers(&mut self, doc: &Document) -> Vec<LineMarker> {
        if doc.language=="xml" && !spring_text::xml::analysis_valid(&doc.text) {return Vec::new();}
        // Batch/Modulith's old marker methods only published diagnostics. The
        // registered owner now returns those through one versioned producer.
        if self.wants_integration(doc) { self.integration.line_markers(doc) } else { Vec::new() }
    }
}

fn module_view(module: &str, name: &str) -> String {
    format!("{module}.{name}")
}

impl Views for Structure {
    fn render(&mut self, _view: &str, viewport: Option<Viewport>, ctx: &Invocation) -> Result<View, Error> {
        (|| -> Result<View, Error> {
            let active = self.sections.active();
            if matches!(active,"batch"|"diagram") {
                let Some(root)=ctx.project().and_then(|p|p.root().ok()).filter(|r|!r.is_empty()) else {
                    self.diagrams.clear();return Ok(self.sections.buttons(View::message("Open a project to see this diagram."),&[]));
                };
                let data=if active=="batch"{self.batch.diagram_snapshot(ctx,&mut self.integration.indexes)}else{self.modulith.diagram_snapshot(ctx,&mut self.integration.indexes)};
                let inner=match data {
                    Ok(data)=>self.diagrams.entry(active.into()).or_default().render(&root,data,if active=="batch"{"Batch source candidate"}else{"Module source candidate"},if active=="batch"{"No Spring Batch jobs or steps found."}else{"No application modules found."}),
                    Err(_)=>{self.diagrams.remove(active);View::message("Diagram source data is unavailable; refresh in its project.")},
                };
                return Ok(self.source_status(self.sections.buttons(inner,&[])));
            }
            let inner = match active {
                "modules" => self.modulith.render(&module_view(modulith::WINDOW_SECTION, active), ctx, &mut self.integration.indexes)?,
                "channel-table" => self.integration.render_table(ctx)?,
                _ => self.integration.render(&module_view(crate::WINDOW_SECTION, "diagram"), viewport, ctx)?,
            };
            Ok(self.source_status(self.sections.buttons(inner, &[])))
        })().map(ui_kit::actions::available)
    }

    fn handle(&mut self, _view: &str, event: ViewEvent, ctx: &Invocation) -> Result<(), Error> {
        if ui_kit::web::handle(self, _view, &event, ctx, include_str!("ui_config.json"))? {
            return Ok(());
        }

        if self.sections.handle(&event) {
            crate::refresh(OWNER_WINDOW);
            return Ok(());
        }

        if matches!(&event, ViewEvent::Button(button) if button == "refresh") {
            // Explicit user recovery is a new source pass, even without event grants.
            // Render-driven scan continuation never routes through this button path.
            self.reset_live();
            self.diagrams.clear();
            self.integration.graph = Default::default();
            if let Some(project) = ctx.project() { self.warm_live(&project); }
            crate::refresh(OWNER_WINDOW);
            return Ok(());
        }
        let active = self.sections.active();
        if matches!(active,"batch"|"diagram") && !matches!(&event,ViewEvent::Button(button) if button=="refresh") {
            let effect=self.diagrams.get_mut(active).ok_or(Error::Stale)?.prepare(&event,ctx)?;
            match effect {
                graph::Effect::Handled=>{crate::refresh(OWNER_WINDOW);return Ok(());},
                graph::Effect::Open(key)=>{
                    if active=="batch" {self.batch.handle(&module_view(batch::WINDOW_SECTION,"diagram"),ViewEvent::Message(key.clone()),ctx,&mut self.integration.indexes)?;}
                    else {self.modulith.handle(&module_view(modulith::WINDOW_SECTION,"diagram"),ViewEvent::Message(key.clone()),ctx,&mut self.integration.indexes)?;}
                    self.diagrams.get_mut(active).ok_or(Error::Stale)?.selected(key);crate::refresh(OWNER_WINDOW);return Ok(());
                },
                graph::Effect::Pass=>{},
            }
        }
        match active {
            "batch" => self.batch.handle(&module_view(batch::WINDOW_SECTION, "diagram"), event, ctx, &mut self.integration.indexes),
            "modules" | "diagram" => self.modulith.handle(&module_view(modulith::WINDOW_SECTION, active), event, ctx, &mut self.integration.indexes),
            _ => self.integration.handle(&module_view(crate::WINDOW_SECTION, "diagram"), event, ctx),
        }
    }
}

impl Structure {
    fn warm_live(&mut self, project: &openidea_plugin::Project) {
        let Some(root) = project.root().ok().filter(|r| !r.is_empty()) else { return };
        let cached = self.applies.get(&root).filter(|(at, _)| at.elapsed() < APPLIES_TTL).map(|(_, applies)| *applies);
        let applies = cached.unwrap_or_else(|| {
            let applies = applies_of(project);
            self.applies.insert(root, (Instant::now(), applies));
            applies
        });
        let budget = Duration::from_millis(20);
        if applies.integration {
            self.integration.warm_project(project, budget);
        }
        if applies.batch && !applies.integration {
            self.integration.warm_project(project, budget);
        }
        if applies.modulith {
            self.modulith.warm_project(project, budget, &mut self.integration.indexes);
        }
    }

    fn reset_live(&mut self) {
        self.applies.clear();
        self.integration.reset_live();
        self.modulith.reset_live();
    }
}

impl DiagnosticsProvider for Structure {
    fn diagnostics(&mut self, doc: &Document) -> Vec<Diagnostic> {
        if doc.text.len() > 256 << 10 {
            return Vec::new();
        }
        if doc.language=="xml" && !spring_text::xml::analysis_valid(&doc.text) {return Vec::new();}
        let Some(root) = project_of(&doc.path).and_then(|p| p.root().ok()).filter(|r| !r.is_empty()) else { return Vec::new() };
        // Gating itself is cached. Do not call applicable(), which reads build files.
        let Some((_, applies)) = self.applies.get(&root).copied() else { return Vec::new() };
        let mut found = Vec::new();
        if applies.integration && matches!(doc.language.as_str(), "java" | "kotlin" | "xml") {
            found.extend(self.integration.live_diagnostics(doc));
        }
        if applies.batch {
            found.extend(self.batch.live_diagnostics(doc, &mut self.integration.indexes));
        }
        if applies.modulith && matches!(doc.language.as_str(),"java"|"kotlin") {
            found.extend(self.modulith.live_diagnostics(doc, &mut self.integration.indexes));
        }
        found.truncate(5000);
        found
    }
}

impl Plugin for Structure {
    fn activate(&mut self, _ctx: &openidea_plugin::Context) -> Result<(), Error> {
        let _ = host::subscribe_paths(
            "file-opened",
            &["**/*.java", "**/*.kt", "**/*.xml", "**/application*.properties", "**/application*.yml", "**/application*.yaml"],
        );
        let saved = host::subscribe_paths(
            "file-saved",
            &[
                "**/*.java",
                "**/*.kt",
                "**/*.xml",
                "**/*.gradle",
                "**/*.gradle.kts",
                "**/application*.properties",
                "**/application*.yml",
                "**/application*.yaml",
            ],
        );
        let changed = host::subscribe_paths(
            "files-changed",
            &[
                "**/*.java",
                "**/*.kt",
                "**/*.xml",
                "**/*.gradle",
                "**/*.gradle.kts",
                "**/application*.properties",
                "**/application*.yml",
                "**/application*.yaml",
            ],
        );
        self.watching = saved.is_ok() && changed.is_ok();
        Ok(())
    }

    fn handle_events(&mut self, events: &[openidea_plugin::Event], _ctx: &Invocation) {
        let mut paths = std::collections::BTreeSet::new();
        let mut invalidate = false;
        for event in events {
            match event {
                openidea_plugin::Event::FileOpened(path) => {
                    paths.insert(path.as_str());
                }
                openidea_plugin::Event::FileSaved(path) => {
                    invalidate = true;
                    paths.insert(path.as_str());
                }
                openidea_plugin::Event::FilesChanged(batch) => {
                    invalidate = true;
                    paths.extend(batch.created.iter().chain(&batch.modified).chain(&batch.removed).take(64).map(String::as_str));
                }
                _ => {}
            }
        }
        if events.iter().any(builds_changed) {
            self.reset_live();
        } else if invalidate {
            let deadline = Instant::now() + EVENT_BUDGET;
            self.integration.on_events(events, deadline);
        }
        let mut roots = std::collections::BTreeSet::new();
        for path in paths.into_iter().take(64) {
            let Some(project) = project_of(path) else { continue };
            let Some(root) = project.root().ok().filter(|r| !r.is_empty()) else { continue };
            if roots.insert(root) {
                self.warm_live(&project);
            }
            if roots.len() >= 4 {
                break;
            }
        }
        if invalidate { crate::refresh(OWNER_WINDOW); }
    }

    fn run_command(&mut self, id: &str, ctx: &Invocation, arg: Option<&str>) -> Result<Option<String>, Error> {
        if id=="structure.refresh" {crate::refresh(OWNER_WINDOW);return Ok(None);}
        if id=="structure.show" {host::reveal_view(OWNER_WINDOW)?;return Ok(None);}
        if id.starts_with("batch.runtime-") {return self.batch_runtime.run_command(id,ctx);}
        if id=="batch.launch-command" {let project=ctx.project().ok_or(Error::Stale)?;let jobs=self.batch.job_names(&project,&mut self.integration.indexes);return crate::batch_launch::run(ctx,&jobs);}
        if id=="integration.import-tree-occurrences" {return crate::import_tree::occurrences(ctx);}
        if id=="batch.current-file-step-usages" {return crate::batch_step_rename::usages(ctx);}
        let module = id.split_once('.').map(|(module, _)| module);
        match module {
            Some(modulith::WINDOW_SECTION) => self.modulith.run_command(id, ctx, &mut self.integration.indexes),
            Some(batch::WINDOW_SECTION) => self.batch.run_command(id, ctx, arg),
            Some(crate::WINDOW_SECTION) => self.integration.run_command(id, ctx, arg),
            _ => Err(Error::NotFound(format!("command {id}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    use openidea_plugin::text::position;
    use openidea_plugin::views::Body;

    const XML_PATH: &str = "/project/src/main/resources/integration.xml";
    const SHIPPER_PATH: &str = "/project/src/main/java/com/acme/shipping/Shipper.java";
    const XML: &str =
        "<beans xmlns=\"http://www.springframework.org/schema/beans\" xmlns:int=\"http://www.springframework.org/schema/integration\">
<int:channel id=\"orders\"/>
<int:service-activator input-channel=\"orders\" output-channel=\"shipping\"/>
<int:service-activator input-channel=\"shipping\" output-channel=\"orders\"/>
</beans>";
    const SHIPPER: &str = "package com.acme.shipping;\n\nimport com.acme.order.internal.OrderRepository;\n\nclass Shipper {}\n";
    const INTEGRATION_POM: &str = "<project><artifactId>spring-boot-starter-integration</artifactId></project>";
    const WEB_POM: &str = "<project><artifactId>spring-boot-starter-web</artifactId></project>";

    fn setup(pom: &str) -> MockHost {
        let host = MockHost::install();
        host.add_file("pom.xml", pom);
        host.add_file("src/main/resources/integration.xml", XML);
        host.add_file(
            "src/main/java/com/acme/Application.java",
            "package com.acme;\n\nimport org.springframework.boot.autoconfigure.SpringBootApplication; @SpringBootApplication\npublic class Application {}\n",
        );
        host.add_file("src/main/java/com/acme/order/OrderService.java", "package com.acme.order;\n\npublic class OrderService {}\n");
        host.add_file(
            "src/main/java/com/acme/order/internal/OrderRepository.java",
            "package com.acme.order.internal;\n\nclass OrderRepository {}\n",
        );
        host.add_file("src/main/java/com/acme/order/package-info.java", "@ApplicationModule\npackage com.acme.order;\n");
        host.add_file("src/main/java/com/acme/shipping/package-info.java", "@ApplicationModule\npackage com.acme.shipping;\n");
        host.add_file("src/main/java/com/acme/shipping/Shipper.java", SHIPPER);
        host
    }

    fn doc(path: &str, language: &str, text: &str) -> Document {
        Document { path: path.into(), language: language.into(), version: 1, text: text.into() }
    }

    const GHOST_XML: &str = "<beans xmlns='http://www.springframework.org/schema/beans' xmlns:i='http://www.springframework.org/schema/integration'><i:service-activator input-channel='orders' output-channel='ghost'/></beans>";
    fn codes(found: Vec<Diagnostic>) -> Vec<String> {
        found.into_iter().filter_map(|d| d.code).collect()
    }
    #[test]
    fn xml_channel_definition_cross_file() {
        let host = setup(INTEGRATION_POM);
        let mut owner = Structure::default();
        let java = "/project/src/main/java/com/acme/Channels.java";
        host.add_file(java, "package com.acme; import org.springframework.context.annotation.Bean; import org.springframework.messaging.MessageChannel; class Channels { @Bean MessageChannel audit(){return null;} }");
        let text = "<beans xmlns='http://www.springframework.org/schema/beans' xmlns:i='http://www.springframework.org/schema/integration'><i:bridge input-channel='audit' output-channel='done'/></beans>";
        let document = doc("/project/src/main/resources/bridge.xml", "xml", text);
        host.add_file(&document.path, text);
        owner.warm_live(&host.invocation().project().unwrap());
        let found = owner.definition(&document, position(text, text.find("audit").unwrap() + 1));
        assert!(found.iter().any(|location| location.path == java), "{found:?}");
    }
    #[test]
    fn runnable_service_does_not_silence_unconsumed_channel() {
        let host = setup(INTEGRATION_POM);
        let mut owner = Structure::default();
        host.add_file("/project/src/main/java/com/acme/Svc.java", "package com.acme; import org.springframework.stereotype.Service; @Service class Svc implements Runnable { public void run(){} }");
        let document = doc("/project/src/main/resources/ghost.xml", "xml", GHOST_XML);
        host.add_file(&document.path, GHOST_XML);
        owner.warm_live(&host.invocation().project().unwrap());
        assert!(codes(owner.diagnostics(&document)).iter().any(|c| c == "unconsumed-channel"));
    }
    #[test]
    fn unresolved_source_candidates_silence_only_the_names_they_could_declare() {
        let host = setup(INTEGRATION_POM);
        let mut owner = Structure::default();
        host.add_file("/project/src/main/java/com/acme/Odd.java", "package com.acme; import org.springframework.stereotype.Service; @Service class Odd<T> { String name = \"ghost\"; String \\u0061 = \"x\"; }");
        let document = doc("/project/src/main/resources/ghost.xml", "xml", GHOST_XML);
        host.add_file(&document.path, GHOST_XML);
        owner.warm_live(&host.invocation().project().unwrap());
        assert!(codes(owner.diagnostics(&document)).is_empty(), "the file spells ghost and cannot be read");
        let other = GHOST_XML.replace("ghost", "phantom");
        assert!(codes(owner.diagnostics(&doc(&document.path, "xml", &other))).iter().any(|c| c == "unconsumed-channel"));
    }
    #[test]
    fn spel_lt_entity_keeps_features() {
        let host = setup(INTEGRATION_POM);
        let mut owner = Structure::default();
        let text = "<beans xmlns='http://www.springframework.org/schema/beans' xmlns:i='http://www.springframework.org/schema/integration'><i:channel id='orders'/><i:filter input-channel='orders' expression=\"payload.size() &lt; 5\" output-channel='ghost'/></beans>";
        let document = doc("/project/src/main/resources/lt.xml", "xml", text);
        host.add_file(&document.path, text);
        owner.warm_live(&host.invocation().project().unwrap());
        assert!(codes(owner.diagnostics(&document)).iter().any(|c| c == "unconsumed-channel"));
        let at = position(text, text.find("input-channel='orders'").unwrap() + 18);
        assert_eq!(owner.definition(&document, at).len(), 1);
        assert!(!owner.line_markers(&document).is_empty() || owner.completion(&document, at, None).iter().any(|item| item.label == "orders"));
    }
    #[test]
    fn save_keeps_absence_diagnostics() {
        let host = setup(INTEGRATION_POM);
        let mut owner = Structure::default();
        let document = doc("/project/src/main/resources/ghost.xml", "xml", GHOST_XML);
        host.add_file(&document.path, GHOST_XML);
        let consumer = "/project/src/main/resources/consumer.xml";
        let consuming = "<beans xmlns='http://www.springframework.org/schema/beans' xmlns:i='http://www.springframework.org/schema/integration'><i:bridge input-channel='ghost'/></beans>";
        host.add_file(consumer, consuming);
        owner.handle_events(&[openidea_plugin::Event::FileOpened(document.path.clone())], &host.invocation());
        assert!(!codes(owner.diagnostics(&document)).iter().any(|c| c == "unconsumed-channel"));
        host.add_file(consumer, "<beans/>");
        owner.handle_events(&[openidea_plugin::Event::FileSaved(document.path.clone())], &host.invocation());
        assert!(!codes(owner.diagnostics(&document)).iter().any(|c| c == "unconsumed-channel"), "a save re-reads only the saved file");
        owner.handle_events(&[openidea_plugin::Event::FileSaved(consumer.into())], &host.invocation());
        assert!(codes(owner.diagnostics(&document)).iter().any(|c| c == "unconsumed-channel"));
    }
    #[test]
    fn modules_share_one_index_with_a_combined_record_per_file() {
        use batch::Source as _;
        let host = setup("<project><module>app</module></project>");
        let mut structure = Structure::default();
        let project = host.invocation().project().unwrap();
        structure.warm_live(&project);
        let batch_id = structure.integration.indexes.project(&project, Duration::ZERO).id();
        let modulith_id = modulith::Source::project(&mut structure.integration.indexes, &project, Duration::ZERO).id();
        assert_eq!(batch_id, modulith_id);
        let index = structure.integration.indexes.get(&project, Duration::ZERO);
        let xml = index.files().get(XML_PATH).unwrap();
        assert!(!xml.refs.is_empty() && xml.unit.is_none());
        let shipper = index.files().get(SHIPPER_PATH).unwrap();
        assert!(shipper.unit.is_some());
    }
    #[test]
    fn gradle_subproject_dependency_detected() {
        let host = setup("<project/>");
        host.add_file("settings.gradle", "include 'app'");
        let mut structure = Structure::default();
        assert!(!structure.line_markers(&doc(XML_PATH, "xml", XML)).is_empty());
    }
    #[test]
    fn windows_root_has_scope() {
        let host = MockHost::install();
        host.set_project_root(Some("C:/project"));
        host.add_file("pom.xml", INTEGRATION_POM);
        host.add_file("src/main/resources/integration.xml", XML);
        let mut structure = Structure::default();
        let path = "C:/project/src/main/resources/integration.xml";
        assert!(!structure.line_markers(&doc(path, "xml", XML)).is_empty());
        structure.warm_live(&host.invocation().project().unwrap());
        assert!(!structure.integration.indexes.get(&host.invocation().project().unwrap(), Duration::ZERO).files().is_empty());
    }

    #[test]
    fn providers_skip_modules_that_do_not_apply() {
        let _host = setup(WEB_POM);
        let mut structure = Structure::default();
        assert!(structure.line_markers(&doc(XML_PATH, "xml", XML)).is_empty());
        let project = spring_text::index::project_of(XML_PATH).unwrap();
        assert!(structure.integration.indexes.get(&project, Duration::ZERO).files().is_empty());

        let _host = setup(INTEGRATION_POM);
        let mut structure = Structure::default();
        assert!(!structure.line_markers(&doc(XML_PATH, "xml", XML)).is_empty());
    }

    #[test]
    fn a_project_with_no_spring_dependency_does_no_module_work() {
        let host = MockHost::install();
        host.add_file("composer.json", "{}");
        host.add_file("src/Thing.java", "class Thing {}");
        let mut structure = Structure::default();
        let thing = doc("/project/src/Thing.java", "java", "class Thing {}");
        assert!(structure.line_markers(&thing).is_empty());
        assert!(structure.completion(&thing, Position::new(0, 0), None).is_empty());
        assert!(structure.definition(&thing, Position::new(0, 0)).is_empty());
        assert!(structure.code_actions(&thing, Range::default()).is_empty());
        assert!(host.diagnostics().is_empty());
    }

    #[test]
    fn one_failing_module_does_not_hide_the_others() {
        let _host = setup("<project><artifactId>spring-integration</artifactId><artifactId>spring-batch</artifactId></project>");
        let mut structure = Structure::default();
        let at = position(XML, XML.find("input-channel=\"orders\"").unwrap() + 20);
        assert!(!structure.definition(&doc(XML_PATH, "xml", XML), at).is_empty());
        assert!(structure.batch.definition(&doc(XML_PATH, "xml", XML), at, &mut structure.integration.indexes).is_empty());
    }

    #[test]
    fn diagnostics_of_every_module_have_one_live_owner() {
        let host = setup("<project><module>app</module></project>");
        let mut structure = Structure::default();
        structure.warm_live(&host.invocation().project().unwrap());
        let live = structure.diagnostics(&doc(SHIPPER_PATH, "java", SHIPPER));
        let codes: Vec<_> = live.iter().filter_map(|d| d.code.clone()).collect();
        assert_eq!(codes, ["module-internal"]);

        structure.line_markers(&doc(XML_PATH, "xml", XML));
        assert!(host.diagnostics().is_empty(), "markers must not publish a second diagnostics producer");
    }
    #[test]
    fn registered_providers_refuse_foreign_integration_annotation_spelling() {
        let host=setup(INTEGRATION_POM);let mut owner=Structure::default();
        let source="package com.acme; import other.integration.ServiceActivator; class Foreign { @ServiceActivator(inputChannel=\"orders\",outputChannel=\"ghost\") void run(){} }";
        let path="/project/src/main/java/com/acme/Foreign.java";host.add_file(path,source);
        let document=doc(path,"java",source);owner.warm_live(&host.invocation().project().unwrap());
        let at=position(source,source.find("orders").unwrap()+1);
        assert!(owner.definition(&document,at).is_empty(),"foreign same-named annotation must not navigate to an Integration channel");
        assert!(owner.line_markers(&document).is_empty());
        assert!(owner.diagnostics(&document).is_empty(),"foreign annotation must not generate missing-channel facts");
    }
    #[test]
    fn registered_annotation_navigation_requires_owned_exact_literal_arguments() {
        for (imports,annotation,tail) in [
            ("import org.springframework.integration.annotation.ServiceActivator;","@ServiceActivator(inputChannel=\"ord\"+\"ers\")",""),
            ("import org.springframework.integration.annotation.ServiceActivator;","@ServiceActivator(inputChannel=\"orders\"+suffix)",""),
            ("import org.springframework.integration.annotation.ServiceActivator;","@ServiceActivator(inputChannel=\"orders\",inputChannel=\"other\")",""),
            ("import org.springframework.integration.annotation.*;","@ServiceActivator(inputChannel=\"orders\")",""),
            ("import org.springframework.integration.annotation.ServiceActivator;","@ServiceActivator(inputChannel=\"orders\")","@interface ServiceActivator {String inputChannel();}"),
            ("import org.springframework.integration.annotation.ServiceActivator;","@ServiceActivator(inputChannel=\"orders\")","// \\u000a class Unsupported {}"),
        ] {
            let host=setup(INTEGRATION_POM);let mut owner=Structure::default();
            let source=format!("package com.acme; {imports} class Foreign {{ {annotation} void run(){{}} }} {tail}");
            let path="/project/src/main/java/com/acme/Foreign.java";host.add_file(path,&source);
            let document=doc(path,"java",&source);owner.warm_live(&host.invocation().project().unwrap());
            let start=source.find("orders").or_else(||source.find("ord")).unwrap();
            assert!(owner.definition(&document,position(&source,start+1)).is_empty(),"{source}");
            assert!(owner.line_markers(&document).is_empty(),"{source}");
        }
        let host=setup(INTEGRATION_POM);let mut owner=Structure::default();
        let source="package com.acme; class Owned { @org.springframework.integration.annotation.ServiceActivator(inputChannel=\"orders\") void run(){} }";
        let path="/project/src/main/java/com/acme/Owned.java";host.add_file(path,source);
        let document=doc(path,"java",source);let found=owner.definition(&document,position(source,source.find("orders").unwrap()+1));
        assert_eq!(found.len(),1);assert_eq!(found[0].path,XML_PATH);
    }
    #[test]
    fn registered_navigation_does_not_turn_foreign_bean_or_channel_suffix_into_declaration() {
        for producer in [
            "package com.acme; import fake.Bean; class Producer { @Bean FakeChannel orders(){return null;} }",
            "package com.acme; import org.springframework.context.annotation.Bean; import fake.MessageChannel; class Producer { @Bean MessageChannel orders(){return null;} }",
        ] {
            let host=setup(INTEGRATION_POM);let mut owner=Structure::default();
            let path="/project/src/main/java/com/acme/Producer.java";host.add_file(path,producer);
            let consumer="package com.acme; import org.springframework.integration.annotation.ServiceActivator; class Consumer { @ServiceActivator(inputChannel=\"orders\") void run(){} }";
            let document=doc("/project/src/main/java/com/acme/Consumer.java","java",consumer);
            let found=owner.definition(&document,position(consumer,consumer.find("orders").unwrap()+1));
            assert!(found.iter().all(|location|location.path!=path),"foreign bean/type must not become channel declaration: {found:?}");
            assert_eq!(found.len(),1);assert_eq!(found[0].path,XML_PATH);
        }
    }
    #[test]
    fn registered_channel_bean_navigation_checks_type_shape_and_literal_aliases() {
        for (producer,expected) in [
            ("package com.acme; import org.springframework.context.annotation.Bean; import org.springframework.messaging.MessageChannel; class Producer { @Bean MessageChannel orders(){return null;} }",true),
            ("package com.acme; class Producer { @org.springframework.context.annotation.Bean org.springframework.messaging.PollableChannel orders(){return null;} }",true),
            ("package com.acme; import org.springframework.context.annotation.Bean; import org.springframework.messaging.SubscribableChannel; class Producer { @Bean(name=\"orders\") SubscribableChannel declared(){return null;} }",true),
            ("package com.acme; import org.springframework.context.annotation.Bean; import org.springframework.messaging.MessageChannel; class Producer { @Bean MessageChannel[] orders(){return null;} }",false),
            ("package com.acme; import org.springframework.context.annotation.Bean; import org.springframework.messaging.MessageChannel; class Producer { @Bean MessageChannel<String> orders(){return null;} }",false),
            ("package com.acme; import org.springframework.context.annotation.Bean; import org.springframework.messaging.MessageChannel; class Producer { @Bean(name=\"orders\"+suffix) MessageChannel declared(){return null;} }",false),
            ("package com.acme; import org.springframework.context.annotation.Bean; import org.springframework.messaging.MessageChannel; class Producer { @Bean MessageChannel orders(){return null;} } class MessageChannel {}",false),
        ] {
            let host=setup(INTEGRATION_POM);let mut owner=Structure::default();let path="/project/src/main/java/com/acme/Producer.java";host.add_file(path,producer);
            let consumer="package com.acme; import org.springframework.integration.annotation.ServiceActivator; class Consumer { @ServiceActivator(inputChannel=\"orders\") void run(){} }";
            let found=owner.definition(&doc("/project/src/main/java/com/acme/Consumer.java","java",consumer),position(consumer,consumer.find("orders").unwrap()+1));
            assert_eq!(found.iter().any(|location|location.path==path),expected,"{producer}: {found:?}");
        }
    }
    #[test]
    fn registered_navigation_refuses_foreign_dsl_factory_spelling_and_unrelated_channel_calls() {
        for source in [
            "package com.acme; import fake.integration.IntegrationFlow; class Flow { void run(){IntegrationFlow.from(\"orders\");} }",
            "package com.acme; import org.springframework.integration.dsl.IntegrationFlow; class Flow { void run(){Object IntegrationFlow=null; IntegrationFlow.from(\"orders\");} }",
            "package com.acme; import org.springframework.integration.dsl.IntegrationFlow; class Flow { void run(){other.channel(\"orders\");} }",
        ] {
            let host=setup(INTEGRATION_POM);let mut owner=Structure::default();let path="/project/src/main/java/com/acme/Flow.java";host.add_file(path,source);
            let document=doc(path,"java",source);let at=position(source,source.find("orders").unwrap()+1);
            assert!(owner.definition(&document,at).is_empty(),"foreign/unrelated DSL literal must not navigate: {source}");
            assert!(owner.line_markers(&document).is_empty());
        }
    }
    #[test]
    fn registered_dsl_navigation_keeps_proved_static_factories_and_contiguous_channels_only() {
        for (source,expected) in [
            ("package com.acme; import org.springframework.integration.dsl.IntegrationFlow; class Flow { Object flow(){return IntegrationFlow.from(\"orders\");} }",true),
            ("package com.acme; class Flow { Object flow(){return org.springframework.integration.dsl.IntegrationFlow.from(\"orders\");} }",true),
            ("package com.acme; import org.springframework.integration.dsl.IntegrationFlow; class Flow { Object flow(){return IntegrationFlow.from(\"start\").channel(\"orders\");} }",true),
            ("package com.acme; import org.springframework.integration.dsl.MessageChannels; class Flow { Object channel(){return MessageChannels.queue(\"orders\");} }",true),
            ("package com.acme; import fake.integration.MessageChannels; class Flow { Object channel(){return MessageChannels.queue(\"orders\");} }",false),
            ("package com.acme; import org.springframework.integration.dsl.MessageChannels; class Flow { Object channel(){Object MessageChannels=null; return MessageChannels.queue(\"orders\");} }",false),
            ("package com.acme; import org.springframework.integration.dsl.IntegrationFlow; class Flow { Object flow(){return IntegrationFlow.from(\"start\").handle(handler).channel(\"orders\");} }",true),
            ("package com.acme; import org.springframework.integration.dsl.IntegrationFlow; class Flow { Object flow(){return unrelated.channel(c -> fake.queue(\"orders\"));} }",false),
            ("package com.acme; import org.springframework.integration.dsl.IntegrationFlow; class Flow { Object flow(){return IntegrationFlow.from(\"orders\"+suffix);} }",false),
        ] {
            let host=setup(INTEGRATION_POM);let mut owner=Structure::default();let path="/project/src/main/java/com/acme/Flow.java";host.add_file(path,source);
            let document=doc(path,"java",source);let at=position(source,source.find("orders").unwrap()+1);
            assert_eq!(!owner.definition(&document,at).is_empty(),expected,"{source}");
        }
    }
    #[test]
    fn registered_xml_channels_refuse_foreign_attributes_and_namespace_lookalikes() {
        for (namespace,attributes) in [
            ("http://www.springframework.org/schema/integration","f:input-channel='orders' f:output-channel='ghost'"),
            ("http://www.springframework.org/schema/integration-not-spring","input-channel='orders' output-channel='ghost'"),
        ] {
            let host=setup(INTEGRATION_POM);let mut owner=Structure::default();
            let text=format!("<beans xmlns='http://www.springframework.org/schema/beans' xmlns:i='{namespace}' xmlns:f='urn:foreign'><i:bridge {attributes}/></beans>");
            let path="/project/src/main/resources/foreign.xml";host.add_file(path,&text);let document=doc(path,"xml",&text);
            owner.warm_live(&host.invocation().project().unwrap());
            assert!(owner.definition(&document,position(&text,text.find("orders").unwrap()+1)).is_empty());
            assert!(owner.line_markers(&document).is_empty());assert!(owner.diagnostics(&document).is_empty());
        }
    }
    #[test]
    fn registered_xml_providers_refuse_incomplete_or_multiple_root_source_facts() {
        let prefix="<beans xmlns='http://www.springframework.org/schema/beans' xmlns:i='http://www.springframework.org/schema/integration'>";
        for text in [
            format!("{prefix}<i:bridge input-channel='orders' output-channel='ghost'/>"),
            format!("{prefix}<i:bridge input-channel='orders' output-channel='ghost'/></beans><other/>"),
            format!("outside{prefix}<i:bridge input-channel='orders' output-channel='ghost'/></beans>"),
        ] {
            let host=setup(INTEGRATION_POM);let mut owner=Structure::default();let path="/project/src/main/resources/bad.xml";
            host.add_file(path,&text);let document=doc(path,"xml",&text);owner.warm_live(&host.invocation().project().unwrap());
            assert!(owner.definition(&document,position(&text,text.find("orders").unwrap()+1)).is_empty());
            assert!(owner.line_markers(&document).is_empty());assert!(owner.diagnostics(&document).is_empty());
        }
    }
    #[test]
    fn registered_batch_live_diagnostics_refuse_unclosed_xml_envelope() {
        let host=setup("<project><artifactId>spring-batch</artifactId></project>");let mut owner=Structure::default();
        let text="<beans xmlns='http://www.springframework.org/schema/beans' xmlns:b='http://www.springframework.org/schema/batch'><b:job id='job'><b:step id='one' next='missing'/></b:job>";
        let path="/project/src/main/resources/bad-batch.xml";host.add_file(path,text);let document=doc(path,"xml",text);
        owner.warm_live(&host.invocation().project().unwrap());
        assert!(owner.diagnostics(&document).is_empty(),"unclosed XML must not publish Batch absence diagnostics");
    }
    #[test]
    fn registered_batch_diagnostics_do_not_borrow_foreign_transition_attributes() {
        for attribute in ["q:next='missing'","q:parent='missing'"] {
            let host=setup("<project><artifactId>spring-batch</artifactId></project>");let mut owner=Structure::default();
            let text=format!("<beans xmlns='http://www.springframework.org/schema/beans' xmlns:b='http://www.springframework.org/schema/batch' xmlns:q='urn:foreign'><b:job id='job'><b:step id='one' {attribute}/></b:job></beans>");
            let path="/project/src/main/resources/foreign-batch.xml";host.add_file(path,&text);owner.warm_live(&host.invocation().project().unwrap());
            let diagnostics=owner.diagnostics(&doc(path,"xml",&text));
            assert!(diagnostics.is_empty(),"foreign attribute is not a Batch transition/parent: {diagnostics:?}");
        }
    }
    #[test]
    fn registered_batch_source_cannot_borrow_a_foreign_job_owner() {
        let host=setup("<project><artifactId>spring-batch</artifactId></project>");let mut owner=Structure::default();
        let text="<beans xmlns='http://www.springframework.org/schema/beans' xmlns:b='http://www.springframework.org/schema/batch' xmlns:q='urn:foreign'><q:job id='foreign'><b:step id='one' next='missing'/></q:job></beans>";
        let path="/project/src/main/resources/foreign-owner.xml";host.add_file(path,text);owner.warm_live(&host.invocation().project().unwrap());
        assert!(owner.diagnostics(&doc(path,"xml",text)).is_empty(),"foreign job spelling cannot donate a Batch transition source owner");
    }
    #[test]
    fn registered_batch_diagram_withholds_malformed_project_source_nodes() {
        let host=setup("<project><artifactId>spring-batch</artifactId></project>");let mut owner=Structure::default();
        host.add_file("/project/src/main/resources/bad-batch.xml","<beans xmlns='http://www.springframework.org/schema/beans' xmlns:b='http://www.springframework.org/schema/batch'><b:job id='Phantom'><b:step id='Ghost'/></b:job>");
        owner.handle(OWNER_WINDOW,ViewEvent::Button("section:batch".into()),&host.invocation()).unwrap();
        let Body::Html(html)=owner.render(OWNER_WINDOW,None,&host.invocation()).unwrap().body else{panic!("batch source diagram")};
        assert!(!html.contains("Phantom") && !html.contains("Ghost"),"an incomplete envelope cannot establish source-owned Batch nodes");
        host.add_file("/project/src/main/resources/bad-batch.xml","<beans xmlns='http://www.springframework.org/schema/beans' xmlns:b='http://www.springframework.org/schema/batch'><b:job id='Phantom'><b:step id='Ghost'/></b:job></beans>");
        owner.handle(OWNER_WINDOW,ViewEvent::Button("refresh".into()),&host.invocation()).unwrap();
        let Body::Html(html)=owner.render(OWNER_WINDOW,None,&host.invocation()).unwrap().body else{panic!("batch source diagram")};
        assert!(html.contains("Phantom") && html.contains("Ghost"),"complete source candidates must still recover after Refresh");
    }
    #[test]
    fn registered_bean_reference_navigation_refuses_foreign_or_compound_bean_names() {
        for source in [
            "package com.acme; import fake.Bean; class Producer { @Bean String helper(){return null;} }",
            "package com.acme; import fake.Component; @Component(\"helper\") class Producer {}",
            "package com.acme; import org.springframework.context.annotation.Bean; class Producer { @Bean(name=\"helper\"+suffix) String build(){return null;} }",
            "package com.acme; import org.springframework.context.annotation.Bean; class Producer extends Base { @Bean String helper(){return null;} }",
            "package com.acme; import org.springframework.context.annotation.Bean; class Producer<T> { @Bean String helper(){return null;} }",
        ] {
            let host=setup(INTEGRATION_POM);let mut owner=Structure::default();let path="/project/src/main/java/com/acme/Producer.java";host.add_file(path,source);
            let text="<beans xmlns='http://www.springframework.org/schema/beans' xmlns:i='http://www.springframework.org/schema/integration'><i:service-activator input-channel='orders' ref='helper'/></beans>";
            let document=doc("/project/src/main/resources/ref.xml","xml",text);let at=position(text,text.find("helper").unwrap()+1);
            assert!(owner.definition(&document,at).is_empty(),"foreign/compound Bean name is not a proved registration: {source}");
            assert!(owner.completion(&document,at,None).iter().all(|item|item.label!="helper"));
        }
    }
    #[test]
    fn registered_bean_source_names_keep_explicit_scalar_and_array_aliases() {
        for source in [
            "package com.acme; import org.springframework.context.annotation.Bean; class Producer { @Bean(name={\"helper\",\"alias\"}) String build(){return null;} }",
            "package com.acme; import org.springframework.context.annotation.Bean; class Producer { @Bean(name={\"helper\",\"helper\"}) String build(){return null;} }",
            "package com.acme; @org.springframework.stereotype.Component(\"helper\") class Producer {}",
        ] {
            let host=setup(INTEGRATION_POM);let mut owner=Structure::default();let path="/project/src/main/java/com/acme/Producer.java";host.add_file(path,source);
            let text="<beans xmlns='http://www.springframework.org/schema/beans' xmlns:i='http://www.springframework.org/schema/integration'><i:service-activator input-channel='orders' ref='helper'/></beans>";
            let document=doc("/project/src/main/resources/ref.xml","xml",text);let at=position(text,text.find("helper").unwrap()+1);
            let found=owner.definition(&document,at);assert_eq!(found.len(),1);assert_eq!(found[0].path,path);
            assert!(owner.completion(&document,at,None).iter().any(|item|item.label=="helper"));
        }
    }
    #[test]
    fn uncertain_bean_source_candidates_cannot_authorize_missing_source_warnings() {
        let host=setup(INTEGRATION_POM);let mut owner=Structure::default();let path="/project/src/main/java/com/acme/Producer.java";
        host.add_file(path,"package com.acme; import org.springframework.context.annotation.*; class Producer { @Bean String helper(){return null;} }");
        let text="<beans xmlns='http://www.springframework.org/schema/beans' xmlns:i='http://www.springframework.org/schema/integration'><i:service-activator input-channel='orders' output-channel='ghost' ref='helper'/></beans>";
        let document=doc("/project/src/main/resources/ref.xml","xml",text);host.add_file(&document.path,text);owner.warm_live(&host.invocation().project().unwrap());
        let codes=|found:Vec<Diagnostic>|found.into_iter().filter_map(|d|d.code).collect::<Vec<_>>();
        assert!(!codes(owner.diagnostics(&document)).iter().any(|c|c=="unknown-bean"),"a complete file scan is not a complete supported Bean model");
        host.add_file(path,"package com.acme; import org.springframework.context.annotation.Bean; class Producer { @Bean String helper(){return null;} }");
        owner.handle_events(&[openidea_plugin::Event::FileSaved(path.into())],&host.invocation());
        let found=owner.diagnostics(&document);assert_eq!(found.len(),1);assert_eq!(found[0].code.as_deref(),Some("unconsumed-channel"));
        assert!(found[0].message.contains("No consumer or declaration for channel"));
    }
    #[test]
    fn registered_bean_alias_array_admission_remains_bounded_and_complete_for_supported_source() {
        let host=setup(INTEGRATION_POM);let mut owner=Structure::default();let path="/project/src/main/java/com/acme/Producer.java";
        let aliases=(0..1000).map(|n|format!("\"alias{n}\"")).collect::<Vec<_>>().join(",");
        host.add_file(path,&format!("package com.acme; import org.springframework.context.annotation.Bean; class Producer {{ @Bean(name={{{aliases}}}) String build(){{return null;}} }}"));
        let text="<beans xmlns='http://www.springframework.org/schema/beans' xmlns:i='http://www.springframework.org/schema/integration'><i:service-activator input-channel='orders' ref='alias500'/></beans>";
        let document=doc("/project/src/main/resources/ref.xml","xml",text);let at=position(text,text.find("alias500").unwrap()+1);owner.warm_live(&host.invocation().project().unwrap());
        let found=owner.definition(&document,at);assert_eq!(found.len(),1);assert_eq!(found[0].path,path);
        assert_eq!(owner.completion(&document,at,None).iter().filter(|item|item.label.starts_with("alias")).count(),1000);
    }
    #[test]
    fn registered_xml_bean_name_aliases_have_exact_individual_source_locations() {
        for declaration in ["<bean id='base' name='helper,other; third'/>","<bean name='helper,helper'/>","<bean id='helper' name='helper,other'/>","<bean name='helper,\r\n other;\tthird'/>"] {
            let host=setup(INTEGRATION_POM);let mut owner=Structure::default();
            let text=format!("<beans xmlns='http://www.springframework.org/schema/beans' xmlns:i='http://www.springframework.org/schema/integration'><!-- 😀 -->{declaration}<i:service-activator input-channel='orders' ref='helper'/></beans>");
            let document=doc("/project/src/main/resources/ref.xml","xml",&text);host.add_file(&document.path,&text);owner.warm_live(&host.invocation().project().unwrap());let at=position(&text,text.rfind("helper").unwrap()+1);
            let found=owner.definition(&document,at);assert_eq!(found.len(),1);assert_eq!(found[0].path,document.path);
            let range=found[0].range;assert_eq!(&text[openidea_plugin::text::offset(&text,range.start).unwrap()..openidea_plugin::text::offset(&text,range.end).unwrap()],"helper");
            assert!(owner.completion(&document,at,None).iter().any(|item|item.label=="helper"));assert!(owner.diagnostics(&document).is_empty());
        }
    }
    #[test]
    fn registered_xml_bean_facts_do_not_borrow_foreign_ids_or_inner_bean_scope() {
        for declaration in ["<bean q:id='helper'/>","<q:bean id='helper'/>","<bean id='outer'><property name='value'><bean id='helper'/></property></bean>"] {
            let _host=setup(INTEGRATION_POM);let mut owner=Structure::default();
            let text=format!("<beans xmlns='http://www.springframework.org/schema/beans' xmlns:i='http://www.springframework.org/schema/integration' xmlns:q='urn:foreign'>{declaration}<i:service-activator input-channel='orders' ref='helper'/></beans>");
            let document=doc("/project/src/main/resources/ref.xml","xml",&text);let at=position(&text,text.rfind("helper").unwrap()+1);
            assert!(owner.definition(&document,at).is_empty(),"unproved global Bean name: {declaration}");assert!(owner.completion(&document,at,None).iter().all(|item|item.label!="helper"));
        }
    }
    #[test]
    fn registered_cached_bean_facts_follow_current_innermost_project_owner() {
        let host=setup(INTEGRATION_POM);let mut owner=Structure::default();
        let source_path="/project/nested/src/main/java/ChildBeans.java";
        host.add_file(source_path,"package child; import org.springframework.context.annotation.Bean; class ChildBeans { @Bean String helper(){return null;} }");
        let text="<beans xmlns='http://www.springframework.org/schema/beans' xmlns:i='http://www.springframework.org/schema/integration'><i:service-activator input-channel='orders' ref='helper'/></beans>";
        let document=doc("/project/src/main/resources/ref.xml","xml",text);host.add_file(&document.path,text);owner.warm_live(&host.invocation().project().unwrap());let at=position(text,text.find("helper").unwrap()+1);
        assert!(owner.definition(&document,at).iter().any(|location|location.path==source_path));
        host.add_project("/project/nested");
        assert!(owner.definition(&document,at).is_empty(),"cached outer-project facts must not retain a newly nested project");
        assert!(owner.completion(&document,at,None).iter().all(|item|item.label!="helper"));
    }
    #[test]
    fn registered_cached_batch_bean_facts_follow_current_project_owner() {
        let host=setup("<project><artifactId>spring-batch</artifactId></project>");let mut owner=Structure::default();
        let source_path="/project/nested/src/main/java/ChildBeans.java";host.add_file(source_path,"package child; import org.springframework.context.annotation.Bean; class ChildBeans { @Bean String helper(){return null;} }");
        let text="<beans xmlns='http://www.springframework.org/schema/beans' xmlns:b='http://www.springframework.org/schema/batch'><b:job id='j'><b:step id='s'><b:tasklet ref='helper'/></b:step></b:job></beans>";
        let document=doc("/project/src/main/resources/ref-batch.xml","xml",text);host.add_file(&document.path,text);owner.warm_live(&host.invocation().project().unwrap());let at=position(text,text.find("helper").unwrap()+1);
        assert!(owner.definition(&document,at).iter().any(|location|location.path==source_path));host.add_project("/project/nested");
        assert!(owner.definition(&document,at).is_empty(),"Batch cannot retain a newly foreign cached Bean");
        assert!(owner.completion(&document,at,None).iter().all(|item|item.label!="helper"));
    }
    #[test]
    fn registered_channel_graph_recovers_owned_anchor_after_nested_project_opens() {
        let host=setup(INTEGRATION_POM);let mut owner=Structure::default();let child="/project/nested/src/Channels.java";
        host.add_file(child,"package child; import org.springframework.context.annotation.Bean; import org.springframework.messaging.MessageChannel; class Channels { @Bean MessageChannel orders(){return null;} }");
        owner.render(OWNER_WINDOW,None,&host.invocation()).unwrap();host.add_project("/project/nested");
        let Body::Html(html)=owner.render(OWNER_WINDOW,None,&host.invocation()).unwrap().body else{panic!("channel graph")};
        let start=XML.find("id=\"orders\"").unwrap()+4;let expected=crate::key("b",XML_PATH,crate::range(XML,start,start+6));
        assert!(html.contains(&format!("data-id=\"{expected}\"")),"an owned declaration must not stay hidden behind a cached foreign first anchor");
        assert!(!html.contains(child));
    }
    #[test]
    fn registered_batch_job_name_navigation_refuses_foreign_builder_spelling() {
        for source in ["package app; import fake.JobBuilder; class Jobs { Object job(){return new JobBuilder(\"ghost\");} }","package app; class Jobs { Object job(){return rogueJobBuilder.get(\"ghost\");} }"] {
            let host=setup("<project><artifactId>spring-batch</artifactId></project>");let mut owner=Structure::default();host.add_file("/project/src/main/java/Jobs.java",source);
            let text="spring.batch.job.name=ghost\n";let document=doc("/project/src/main/resources/application.properties","properties",text);let at=position(text,text.find("ghost").unwrap()+1);
            assert!(owner.definition(&document,at).is_empty(),"unowned Builder spelling cannot define a Batch job: {source}");
            assert!(owner.completion(&document,at,None).iter().all(|item|item.label!="ghost"));
        }
    }

    #[test]
    fn registered_batch_bean_navigation_refuses_foreign_annotation() {
        let host=setup("<project><artifactId>spring-batch</artifactId></project>");let mut owner=Structure::default();
        host.add_file("/project/src/main/java/Jobs.java","import fake.Bean; class Jobs { @Bean Object ghost(){return null;} }");
        let text="<beans xmlns=\"http://www.springframework.org/schema/beans\" xmlns:b=\"http://www.springframework.org/schema/batch\"><b:job id=\"job\"><b:step id=\"step\"><b:tasklet ref=\"ghost\"/></b:step></b:job></beans>";
        let document=doc("/project/src/main/resources/jobs.xml","xml",text);let at=position(text,text.find("ghost").unwrap()+1);
        assert!(owner.definition(&document,at).is_empty(),"foreign Bean cannot define Batch tasklet ref");
    }

    #[test]
    fn registered_batch_parameter_completion_refuses_unowned_adders() {
        let host=setup("<project><artifactId>spring-batch</artifactId></project>");let mut owner=Structure::default();
        host.add_file("/project/src/main/java/Params.java","import fake.JobParametersBuilder; class Params { void go(){new JobParametersBuilder().addString(\"ghost\",\"value\");} }");
        let text="<beans xmlns=\"http://www.springframework.org/schema/beans\" xmlns:b=\"http://www.springframework.org/schema/batch\"><b:job id=\"job\"><b:step id=\"step\"><b:tasklet ref=\"#{jobParameters['g']}\"/></b:step></b:job></beans>";
        let document=doc("/project/src/main/resources/jobs.xml","xml",text);let at=position(text,text.find("['g").unwrap()+3);
        assert!(owner.completion(&document,at,None).iter().all(|item|item.label!="ghost"),"foreign parameter builder cannot supply a Spring key");
    }

    #[test]
    fn registered_batch_parameter_completion_refuses_foreign_value_and_comments() {
        for source in ["import fake.Value; class Params { @Value(\"#{jobParameters['ghost']}\") String key; }","class Params { /* jobParameters['ghost'] */ String note=\"jobParameters['ghost']\"; }"] {
            let host=setup("<project><artifactId>spring-batch</artifactId></project>");let mut owner=Structure::default();host.add_file("/project/src/main/java/Params.java",source);
            let text="<beans xmlns=\"http://www.springframework.org/schema/beans\" xmlns:b=\"http://www.springframework.org/schema/batch\"><b:job id=\"job\"><b:step id=\"step\"><b:tasklet ref=\"#{jobParameters['g']}\"/></b:step></b:job></beans>";
            let document=doc("/project/src/main/resources/jobs.xml","xml",text);let at=position(text,text.find("['g").unwrap()+3);
            assert!(owner.completion(&document,at,None).iter().all(|item|item.label!="ghost"),"unproved parameter expression source: {source}");
        }
    }

    #[test]
    fn registered_batch_parameter_completion_refuses_foreign_xml_expression_text() {
        for source in ["<q:bean xmlns:q=\"urn:foreign\" value=\"#{jobParameters['ghost']}\"/>","<beans xmlns=\"http://www.springframework.org/schema/beans\"><!-- jobParameters['ghost'] --></beans>","<beans xmlns=\"http://www.springframework.org/schema/beans\"><property value=\"#{jobParameters['ghost']}\"/>"] {
            let host=setup("<project><artifactId>spring-batch</artifactId></project>");let mut owner=Structure::default();host.add_file("/project/src/main/resources/foreign.xml",source);
            let text="<beans xmlns=\"http://www.springframework.org/schema/beans\" xmlns:b=\"http://www.springframework.org/schema/batch\"><b:job id=\"job\"><b:step id=\"step\"><b:tasklet ref=\"#{jobParameters['g']}\"/></b:step></b:job></beans>";
            let document=doc("/project/src/main/resources/jobs.xml","xml",text);let at=position(text,text.find("['g").unwrap()+3);
            assert!(owner.completion(&document,at,None).iter().all(|item|item.label!="ghost"),"unproved XML expression: {source}");
        }
    }

    #[test]
    fn registered_batch_parameter_completion_requires_owned_current_caret_context() {
        for source in ["package app.batch; import fake.Value; class Params { @Value(\"#{jobParameters['g']}\") String key; }","package app.batch; class Params { /* jobParameters['g'] */ }"] {
            let host=setup("<project><artifactId>spring-batch</artifactId></project>");let mut owner=Structure::default();
            host.add_file("/project/src/main/java/Keys.java","import org.springframework.batch.core.JobParametersBuilder; class Keys {void go(){new JobParametersBuilder().addString(\"ghost\",\"v\");}}");
            let document=doc("/project/src/main/java/Params.java","java",source);let at=position(source,source.find("['g").unwrap()+3);
            assert!(owner.completion(&document,at,None).iter().all(|item|item.label!="ghost"),"unowned caret must not receive Batch key completion: {source}");
        }
    }

    #[test]
    fn registered_same_document_step_usage_ui_path_is_missing() {
        let host=setup("<project><artifactId>spring-batch</artifactId></project>");let mut owner=Structure::default();
        let text="<job xmlns=\"http://www.springframework.org/schema/batch\" id=\"job\"><step id=\"load\" next=\"process\"/><step id=\"process\"/></job>";
        let path="/project/src/main/resources/jobs.xml";let at=position(text,text.find("id=\"process\"").unwrap()+4);host.open_editor(path,"xml",text,&[(at.line,at.character,at.line,at.character)]);
        let result=owner.run_command("batch.current-file-step-usages",&host.invocation(),None);
        assert!(!matches!(result,Err(Error::NotFound(_))),"missing registered source-local step usages UI: {result:?}");
    }

    #[test]
    fn code_action_resolves_through_its_module() {
        let _host = setup("<project><artifactId>spring-modulith-core</artifactId></project>");
        let mut structure = Structure::default();
        let shipper = doc(SHIPPER_PATH, "java", SHIPPER);
        let at = position(SHIPPER, SHIPPER.find("order.internal").unwrap());
        let actions = structure.code_actions(&shipper, Range::new(at, at));
        assert_eq!(actions.len(), 1);

        let routed = CodeAction { data: Some("modulith:ref".into()), ..actions[0].clone() };
        let resolved = structure.resolve_code_action(&shipper, routed).unwrap();
        assert_eq!(resolved.data.as_deref(), Some("modulith:ref"));
        assert_eq!(resolved.edits, actions[0].edits);

        let unknown = CodeAction { data: Some("nowhere:ref".into()), ..actions[0].clone() };
        assert!(structure.resolve_code_action(&shipper, unknown).is_err());
    }

    #[test]
    fn registered_kotlin_integration_navigation_completion_and_live_fix_use_owned_source_model() {
        let host=MockHost::install();host.add_file("/project/pom.xml","spring-integration-core");
        let source="package app\nimport org.springframework.context.annotation.Bean\nimport org.springframework.messaging.MessageChannel\nimport org.springframework.integration.annotation.ServiceActivator\nclass Channels {\n @Bean fun orders(): MessageChannel = TODO()\n @ServiceActivator(inputChannel=\"orders\", outputChannel=\"missing\") fun consume() {}\n}";
        let document=doc("/project/src/app/Channels.kt","kotlin",source);host.add_file(&document.path,source);let mut owner=Structure::default();owner.activate(&host.context("aritti.spring-integration")).unwrap();assert!(host.path_subscriptions()["file-opened"].iter().any(|glob|glob=="**/*.kt"));owner.handle_events(&[openidea_plugin::Event::FileOpened(document.path.clone())],&host.invocation());assert!(owner.diagnostics(&document).iter().any(|d|d.code.as_deref()==Some("unconsumed-channel")));
        let at=position(source,source.find("inputChannel=\"orders\"").unwrap()+15);
        let found=owner.definition(&document,at);assert_eq!(found.len(),1);assert_eq!(found[0].path,document.path);assert!(owner.completion(&document,at,None).iter().any(|item|item.label=="orders"));
        assert!(owner.diagnostics(&document).iter().any(|d|d.code.as_deref()==Some("unconsumed-channel")));
        let fixed=Document{text:source.replace("outputChannel=\"missing\"","outputChannel=\"orders\""),..document.clone()};assert!(!owner.diagnostics(&fixed).iter().any(|d|d.code.as_deref()==Some("unconsumed-channel")));
        let foreign=Document{text:source.replace("org.springframework.integration.annotation.ServiceActivator","other.ServiceActivator"),..document};assert!(owner.definition(&foreign,at).is_empty());
    }
    #[test]
    fn kotlin_parameter_completion_uses_owned_builder_facts_without_java_only_diagnostics() {
        let host=MockHost::install();host.add_file("/project/pom.xml","spring-batch-core");
        let source="package app\nimport org.springframework.batch.core.JobParametersBuilder\nimport org.springframework.beans.factory.annotation.Value\nclass Params {\n @Value(\"#{jobParameters['r']}\") val parameter: String = \"\"\n fun parameters() { JobParametersBuilder().addString(\"runId\",\"1\") }\n}";
        let document=doc("/project/src/app/Params.kt","kotlin",source);host.add_file(&document.path,source);let mut owner=Structure::default();owner.warm_live(&host.invocation().project().unwrap());let at=position(source,source.find("['r']").unwrap()+2);
        let items=owner.completion(&document,at,None);assert!(items.iter().any(|item|item.label=="runId"),"{items:?}");assert!(owner.diagnostics(&document).is_empty());
    }
    #[test]
    fn registered_kotlin_modulith_metadata_navigation_and_live_boundary_diagnostics_are_reachable() {
        let host=MockHost::install();host.add_file("/project/pom.xml","spring-modulith");
        host.add_file("/project/src/main/kotlin/app/Main.kt","package app\nimport org.springframework.boot.autoconfigure.SpringBootApplication\n@SpringBootApplication class Main");
        host.add_file("/project/src/main/kotlin/app/orders/Api.kt","package app.orders\nclass Api");host.add_file("/project/src/main/kotlin/app/orders/internal/Repository.kt","package app.orders.internal\nclass Repository");
        let metadata="package app.shipping\nimport org.springframework.modulith.PackageInfo\nimport org.springframework.modulith.ApplicationModule\n@PackageInfo @ApplicationModule(allowedDependencies=[\"orders\"]) class Metadata";let document=doc("/project/src/main/kotlin/app/shipping/Metadata.kt","kotlin",metadata);host.add_file(&document.path,metadata);
        let mut owner=Structure::default();owner.warm_live(&host.invocation().project().unwrap());let at=position(metadata,metadata.find("\"orders\"").unwrap()+2);assert!(owner.completion(&document,at,None).iter().any(|item|item.label=="orders::*"));let locations=owner.definition(&document,at);assert_eq!(locations.len(),1);assert!(locations[0].path.ends_with("/orders/Api.kt"));
        let source="package app.shipping\nimport app.orders.internal.Repository\nclass Shipping";let shipping=doc("/project/src/main/kotlin/app/shipping/Shipping.kt","kotlin",source);assert!(owner.diagnostics(&shipping).iter().any(|d|d.code.as_deref()==Some("module-internal")));
    }
    #[test]
    fn merged_batch_live_checks_use_explicit_compatibility_without_inferring_unversioned_schema() {
        let host=MockHost::install();host.add_file("/project/pom.xml","spring-batch-core");
        let source="<beans xmlns='http://www.springframework.org/schema/beans' xmlns:b='http://www.springframework.org/schema/batch'><b:job id='j'><b:step id='s'><b:tasklet ref='worker' throttle-limit='4'/></b:step></b:job></beans>";
        let document=doc("/project/jobs.xml","xml",source);host.add_file(&document.path,source);let mut owner=Structure::default();owner.warm_live(&host.invocation().project().unwrap());
        assert!(!owner.diagnostics(&document).iter().any(|d|d.code.as_deref()==Some("unsupported-batch-schema-attribute")));
        host.set_setting("springBatch.xmlCompatibility",r#""6""#);
        let found=owner.diagnostics(&document);assert!(found.iter().any(|d|d.code.as_deref()==Some("unsupported-batch-schema-attribute")&&d.message.contains("throttle-limit")));assert_eq!(found.iter().filter(|d|d.code.as_deref()==Some("batch-xml-deprecated")).count(),1);
    }
    #[test]
    fn registered_named_interface_file_action_routes_reviewed_foreign_type_annotation() {
        use openidea_plugin::file_actions::FileActions;
        let host=MockHost::install();host.add_file("/project/pom.xml","spring-modulith");
        host.add_file("/project/src/app/Main.java","package app; @SpringBootApplication public class Main {}");
        host.add_file("/project/src/app/orders/internal/Repository.java","package app.orders.internal; public class Repository {}");
        let document=doc("/project/src/app/shipping/Shipping.java","java","package app.shipping; import app.orders.internal.Repository; public class Shipping {}");host.add_file(&document.path,&document.text);let mut owner=Structure::default();owner.warm_live(&host.invocation().project().unwrap());
        let at=position(&document.text,document.text.find("app.orders.internal.Repository").unwrap()+1);let action=owner.file_actions(&document,Range::new(at,at)).into_iter().find(|a|a.title=="Expose imported type as a named interface…").unwrap();host.answer_next_prompt(Some(&["repository"]));host.answer_next_prompt(Some(&["yes"]));let resolved=owner.resolve_file_action(&document,action).unwrap();assert_eq!(resolved.edits[0].0,"/project/src/app/orders/internal/Repository.java");assert!(resolved.edits[0].1[0].text.contains("org.springframework.modulith.NamedInterface"));assert!(resolved.operations.is_empty());
    }
    #[test]
    fn section_buttons_switch_the_structure_window() {
        let host = setup(INTEGRATION_POM);
        let mut structure = Structure::default();
        let view = structure.render("structure", None, &host.invocation()).unwrap();
        assert!(matches!(view.body, Body::Html(_)));
        let ids: Vec<_> = view.toolbar.iter().map(|b| b.id.as_str()).collect();
        assert_eq!(&ids[..5], ["section:channels", "section:channel-table", "section:batch", "section:modules", "section:diagram"]);

        structure.handle("structure", ViewEvent::Button("section:modules".into()), &host.invocation()).unwrap();
        assert_eq!(host.refreshed_views(), ["structure"]);
        let view = structure.render("structure", None, &host.invocation()).unwrap();
        assert!(matches!(view.body, Body::Tree(_) | Body::Message(_)));
        assert!(view.toolbar[3].label.starts_with('\u{25cf}'));
        assert_eq!(host.storage()[SECTION_KEY], "modules");

        structure.handle("structure", ViewEvent::Button("section:diagram".into()), &host.invocation()).unwrap();
        let view = structure.render("structure", None, &host.invocation()).unwrap();
        assert!(matches!(view.body, Body::Html(_)));
    }

    #[test]
    fn channel_table_section_lists_and_opens_channels() {
        let host = setup(INTEGRATION_POM);
        let mut structure = Structure::default();
        structure.handle("structure", ViewEvent::Button("section:channel-table".into()), &host.invocation()).unwrap();
        let view = structure.render("structure", None, &host.invocation()).unwrap();
        let Body::Table(table) = view.body else { panic!("channel table") };
        assert_eq!(table.columns, ["Channel", "Declarations", "Producers", "Consumers", "File"]);
        assert_eq!(table.rows, [["orders", "1", "1", "1", "integration.xml"], ["shipping", "0", "1", "1", "integration.xml"]]);
        let event = ViewEvent::Other { kind: "table-activate".into(), data: r#"{"row":0}"#.into() };
        structure.handle("structure", event, &host.invocation()).unwrap();
        assert_eq!(host.opened_locations()[0].0, XML_PATH);
    }
    #[test]
    fn refresh_in_a_section_refreshes_the_structure_window() {
        let host = setup(INTEGRATION_POM);
        let mut structure = Structure::default();
        structure.handle("structure", ViewEvent::Button("section:batch".into()), &host.invocation()).unwrap();
        let before = host.refreshed_views().len();
        structure.handle("structure", ViewEvent::Button("refresh".into()), &host.invocation()).unwrap();
        assert_eq!(host.refreshed_views()[before..], ["structure"]);
    }

    fn cached_channels(view: View) -> String {
        let Body::Html(html) = view.body else { panic!("channels page"); }; html
    }
    const CREATED: &str = "<beans xmlns='http://www.springframework.org/schema/beans' xmlns:i='http://www.springframework.org/schema/integration'><i:channel id='createdChannel'/></beans>";
    #[test]
    fn denied_events_graph_labels_manual_refresh_and_recovers_after_source_restore() {
        let host = setup(INTEGRATION_POM).with_capabilities(&["project.read", "storage"]);
        host.add_file("src/created.xml", CREATED);
        let mut owner = Structure::default(); owner.activate(&host.context("aritti.spring-integration")).unwrap();
        assert!(host.path_subscriptions().is_empty());
        let view = owner.render("structure",None,&host.invocation()).unwrap();
        assert!(view.status.as_deref().unwrap().contains("Automatic source updates unavailable"));
        assert!(cached_channels(view).contains("createdChannel"));
        host.add_file("src/created.xml", "");
        assert!(cached_channels(owner.render("structure",None,&host.invocation()).unwrap()).contains("createdChannel"), "cache intentionally retained until explicit recovery");
        owner.handle("structure",ViewEvent::Button("refresh".into()),&host.invocation()).unwrap();
        assert!(!cached_channels(owner.render("structure",None,&host.invocation()).unwrap()).contains("createdChannel"));
        assert!(host.view_messages().iter().any(|(view,message)|view=="structure" && message==r#"{"channel":"ui-kit-refresh"}"#));
        assert!(host.requests().is_empty() && host.process_runs().is_empty());
    }
    #[test]
    fn watched_source_change_invalidates_already_open_packaged_structure_model() {
        let host = setup(INTEGRATION_POM); host.add_file("src/created.xml",CREATED);
        let mut owner = Structure::default(); owner.activate(&host.context("aritti.spring-integration")).unwrap();
        assert!(host.path_subscriptions().contains_key("files-changed"));
        let view=owner.render("structure",None,&host.invocation()).unwrap();
        assert!(!view.status.as_deref().unwrap().contains("Automatic source updates unavailable"));
        assert!(cached_channels(view).contains("createdChannel"));
        host.add_file("src/created.xml","");
        owner.handle_events(&[openidea_plugin::Event::FilesChanged(openidea_plugin::FilesChanged { modified:vec!["/project/src/created.xml".into()],..Default::default() })],&host.invocation());
        assert!(host.view_messages().iter().any(|(view,message)|view=="structure" && message==r#"{"channel":"ui-kit-refresh"}"#));
        let request=r#"{"channel":"ui-kit","sequence":1,"frameId":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","project":"/project","op":"render"}"#;
        owner.handle("structure",ViewEvent::Message(request.into()),&host.invocation()).unwrap();
        let answer=host.view_messages().last().unwrap().1.clone();
        assert!(answer.contains("\"model\""),"{answer}");assert!(!answer.contains("createdChannel"));
        assert!(owner.diagrams.is_empty());
    }
    #[test]
    fn explicit_wrapper_refresh_rewarms_changed_dependency_gates_without_source_events() {
        let host=setup(INTEGRATION_POM).with_capabilities(&["project.read", "storage"]);let mut owner=Structure::default();
        owner.activate(&host.context("aritti.spring-integration")).unwrap();
        owner.handle("structure",ViewEvent::Button("refresh".into()),&host.invocation()).unwrap();
        assert!(owner.applies["/project"].1.integration);
        host.add_file("pom.xml",WEB_POM);
        owner.handle("structure",ViewEvent::Button("refresh".into()),&host.invocation()).unwrap();
        let applies=owner.applies["/project"].1;assert!(!applies.integration && !applies.batch && !applies.modulith);
        assert!(owner.diagnostics(&doc(XML_PATH,"xml",XML)).is_empty());
        host.set_project_root(None);
        owner.handle("structure",ViewEvent::Button("refresh".into()),&host.invocation()).unwrap();
        assert!(owner.applies.is_empty());
    }
    #[test]
    fn refresh_command_refreshes_the_structure_window() {
        let host = setup(INTEGRATION_POM);
        let mut structure = Structure::default();
        host.refreshed_views();
        structure.run_command("structure.refresh", &host.invocation(), None).unwrap();
        assert!(host.refreshed_views().iter().any(|view| view == OWNER_WINDOW));
    }
    #[test]
    fn show_command_reveals_the_structure_window() {
        let host = setup(INTEGRATION_POM);
        Structure::default().run_command("structure.show", &host.invocation(), None).unwrap();
        assert_eq!(host.revealed_views(), [OWNER_WINDOW]);
    }
    #[test]
    fn ids_are_unique_across_modules() {
        let manifest = include_str!("../plugin.toml");
        let ids: Vec<&str> =
            manifest.lines().filter_map(|line| line.strip_prefix("id = \"")).filter_map(|rest| rest.strip_suffix('"')).collect();
        let mut unique = ids.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), ids.len(), "{ids:?}");
        assert!(ids.contains(&"modulith.copy-docs"));

        let host = setup(INTEGRATION_POM);
        let mut structure = Structure::default();
        structure.run_command("modulith.copy-docs", &host.invocation(), None).unwrap();
        assert!(structure.run_command("copy-docs", &host.invocation(), None).is_err());
        assert!(structure.run_command("batch.copy-docs", &host.invocation(), None).is_err());
    }

    #[test]
    fn merged_providers_stay_under_budget_on_a_sample_project() {
        let host = MockHost::install();
        host.add_file("pom.xml", "<project><module>app</module></project>");
        for n in 0..40 {
            host.add_file(
                &format!("src/main/java/com/acme/m{n}/Service{n}.java"),
                &format!("package com.acme.m{n};\n\npublic class Service{n} {{}}\n"),
            );
            host.add_file(&format!("src/main/resources/flow{n}.xml"), &XML.replace("orders", &format!("orders{n}")));
        }
        host.add_file(
            "src/main/java/com/acme/Application.java",
            "package com.acme;\n\nimport org.springframework.boot.autoconfigure.SpringBootApplication; @SpringBootApplication\npublic class Application {}\n",
        );
        let mut structure = Structure::default();
        let document = doc(XML_PATH, "xml", XML);
        let java = doc("/project/src/main/java/com/acme/m1/Service1.java", "java", "package com.acme.m1;\n\npublic class Service1 {}\n");
        structure.line_markers(&document);

        let started = Instant::now();
        structure.completion(&document, Position::new(2, 40), None);
        structure.definition(&document, Position::new(2, 40));
        structure.code_actions(&java, Range::default());
        structure.line_markers(&document);
        assert!(started.elapsed() < Duration::from_millis(10), "{:?}", started.elapsed());
    }
    const COMBINED: &str = r#"<!-- 🦀 --><beans xmlns="http://www.springframework.org/schema/beans" xmlns:int="http://www.springframework.org/schema/integration" xmlns:b="http://www.springframework.org/schema/batch">
<int:channel id="orders"/>
<int:service-activator input-channel="orders" output-channel="lost"/>
<bean id="r"/><bean id="w"/>
<b:job id="j"><b:step id="s"><b:tasklet><b:chunk reader="r" writer="w"/></b:tasklet></b:step></b:job>
</beans>"#;

    #[test]
    fn live_results_combine_modules_and_follow_independent_unsaved_fixes() {
        let host = setup("<project><module>app</module></project>");
        host.add_file("src/main/resources/integration.xml", COMBINED);
        let mut structure = Structure::default();
        let document = doc(XML_PATH, "xml", COMBINED);
        assert!(structure.diagnostics(&document).is_empty(), "cold gating must not scan");
        structure.handle_events(&[openidea_plugin::Event::FileOpened(XML_PATH.into())], &host.invocation());
        let codes = |found: Vec<Diagnostic>| found.into_iter().filter_map(|d| d.code).collect::<Vec<_>>();
        let first = codes(structure.diagnostics(&document));
        assert!(first.iter().any(|c| c == "unconsumed-channel"), "{first:?}");
        assert!(first.iter().any(|c| c == "chunk-missing-commit-interval"), "{first:?}");
        let fixed = COMBINED.replace("output-channel=\"lost\"", "output-channel=\"orders\"");
        let after = codes(structure.diagnostics(&doc(XML_PATH, "xml", &fixed)));
        assert!(!after.iter().any(|c| c == "unconsumed-channel"));
        assert!(after.iter().any(|c| c == "chunk-missing-commit-interval"));
        let fixed = fixed.replace("<b:chunk", "<b:chunk commit-interval=\"1\"");
        assert!(structure.diagnostics(&doc(XML_PATH, "xml", &fixed)).is_empty());
        structure.line_markers(&document);
        assert!(host.diagnostics().is_empty(), "no manual duplicate producer");
    }

    #[test]
    fn live_dependency_gating_reacts_to_watched_build_changes() {
        let host = setup(INTEGRATION_POM);
        let mut structure = Structure::default();
        let document = doc(XML_PATH, "xml", COMBINED);
        structure.handle_events(&[openidea_plugin::Event::FileOpened(XML_PATH.into())], &host.invocation());
        assert!(!structure.diagnostics(&document).iter().any(|d| d.code.as_deref() == Some("chunk-missing-commit-interval")));
        host.add_file("pom.xml", "<project><artifactId>spring-batch</artifactId></project>");
        structure.handle_events(
            &[openidea_plugin::Event::FilesChanged(openidea_plugin::FilesChanged {
                modified: vec!["/project/pom.xml".into()],
                ..Default::default()
            })],
            &host.invocation(),
        );
        let found = structure.diagnostics(&document);
        assert!(found.iter().any(|d| d.code.as_deref() == Some("chunk-missing-commit-interval")));
        assert!(!found.iter().any(|d| d.code.as_deref() == Some("unconsumed-channel")));
    }

    #[test]
    fn live_cache_identity_and_bounds_do_not_leak_between_projects() {
        let host = setup("<project><module>app</module></project>");
        let mut structure = Structure::default();
        structure.handle_events(&[openidea_plugin::Event::FileOpened(XML_PATH.into())], &host.invocation());
        assert!(!structure.diagnostics(&doc(XML_PATH, "xml", COMBINED)).is_empty());
        host.set_project_root(Some("/second"));
        assert!(structure.diagnostics(&doc("/second/new.xml", "xml", COMBINED)).is_empty());
        assert!(structure.diagnostics(&doc("/second/new.xml", "xml", &"x".repeat(300 << 10))).is_empty());
        assert!(host.diagnostics().is_empty());
    }
    #[test]
    fn all_diagram_sections_use_typed_snapshots_and_preserve_guarded_source_navigation() {
        let host=setup(INTEGRATION_POM);host.add_file("src/main/resources/integration.xml",COMBINED);
        let mut owner=Structure::default();
        for (section,kind) in [("batch","Batch source candidate"),("diagram","Module source candidate")] {
            owner.handle("structure",ViewEvent::Button(format!("section:{section}")),&host.invocation()).unwrap();
            let view=owner.render("structure",None,&host.invocation()).unwrap();
            assert!(view.toolbar.iter().any(|b|b.id=="graph-zoom-in"));
            let state=owner.diagrams.get(section).unwrap();let key=state.first_key().unwrap();
            owner.handle("structure",ViewEvent::Message(key.clone()),&host.invocation()).unwrap();
            let view=owner.render("structure",None,&host.invocation()).unwrap();
            let Body::Html(html)=view.body else{panic!("typed diagram")};assert!(html.contains(kind));assert!(html.contains("aria-selected=\"true\""));
            owner.handle("structure",ViewEvent::Button("graph-zoom-in".into()),&host.invocation()).unwrap();
            assert!(owner.render("structure",None,&host.invocation()).unwrap().status.unwrap().contains("125%"));
            assert!(owner.handle("structure",ViewEvent::Message(format!("{key}:forged")),&host.invocation()).is_err());
        }
        assert_eq!(host.opened_locations().len(),2);assert!(host.requests().is_empty() && host.process_runs().is_empty());
    }
    #[test]
    fn module_diagram_packaged_zoom_root_switch_and_missing_owner_replace_model_safely() {
        let host=setup(INTEGRATION_POM);let mut owner=Structure::default();
        owner.handle("structure",ViewEvent::Button("section:diagram".into()),&host.invocation()).unwrap();
        owner.render("structure",None,&host.invocation()).unwrap();let old=owner.diagrams["diagram"].first_key().unwrap();
        let request=r#"{"channel":"ui-kit","sequence":1,"frameId":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","project":"/project","op":"event","event":{"kind":"button","key":"graph-zoom-in"}}"#;
        owner.handle("structure",ViewEvent::Message(request.into()),&host.invocation()).unwrap();
        assert!(host.view_messages().last().unwrap().1.contains("125%"));
        host.set_project_root(Some("/other"));let view=owner.render("structure",None,&host.invocation()).unwrap();assert!(view.status.unwrap().contains("100%"));
        assert!(owner.handle("structure",ViewEvent::Message(old),&host.invocation()).is_err());assert!(host.opened_locations().is_empty());
        host.set_project_root(None);let view=owner.render("structure",None,&host.invocation()).unwrap();assert!(matches!(view.body,Body::Message(_)));assert!(owner.diagrams.is_empty());
    }

}

// Root XML namespace facts authorize source creation independently of build heuristics.
impl openidea_plugin::file_actions::FileActions for Structure {
    fn file_actions(&mut self, doc: &Document, selection: Range) -> Vec<openidea_plugin::file_actions::FileAction> {
        let mut actions = self.integration.file_actions(doc, selection);
        actions.extend(self.batch_links.actions(doc, selection));
        actions.extend(self.batch_step_rename.actions(doc, selection));
        actions.extend(self.channel_renames.actions(doc, selection));
        actions.extend(self.import_renames.actions(doc, selection));
        actions.extend(self.modulith.expose_actions(doc,selection,&mut self.integration.indexes));
        actions
    }
    fn resolve_file_action(
        &mut self,
        doc: &Document,
        action: openidea_plugin::file_actions::FileAction,
    ) -> Result<openidea_plugin::file_actions::FileAction, Error> {
        if action.data.as_deref().is_some_and(|data| data.starts_with("modulith-expose-interface:")) {return self.modulith.resolve_expose(doc,action);}
        if action.data.as_deref().is_some_and(|data| data.starts_with("integration-import-tree:")) {
            return self.import_renames.resolve(doc,action);
        }
        if action.data.as_deref().is_some_and(|data| data.starts_with("integration-channel-rename:")) {
            return self.channel_renames.resolve(doc,action);
        }
        if action.data.as_deref().is_some_and(|data| data.starts_with("batch-step-rename:")) {
            return self.batch_step_rename.resolve(doc,action);
        }
        if action.data.as_deref().is_some_and(|data| data.starts_with("batch-transition:")) {
            return self.batch_links.resolve(doc, action);
        }
        self.integration.resolve_file_action(doc, action)
    }

}
