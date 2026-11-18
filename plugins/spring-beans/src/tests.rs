use super::*;
use crate::annotation_checks;
use openidea_plugin::Event;
use openidea_plugin::file_actions::FileActions;
use openidea_plugin::mock::MockHost;
use spring_text::xml;

const SERVICE: &str = r#"package com.acme;

import org.springframework.stereotype.Service;

/** A @Component in a comment is not a bean. */
@Service("users")
@org.springframework.context.annotation.Profile({"dev", "prod"})
public class UserServiceImpl implements UserService {
    private final UserRepository repo;
    private final List<Notifier> notifiers;

    public UserServiceImpl(UserRepository repo, @org.springframework.beans.factory.annotation.Qualifier("sms") Notifier main, List<Notifier> notifiers) {
        this.repo = repo;
        this.notifiers = notifiers;
    }
}
"#;

const CONTROLLER: &str = r##"package com.acme;

@org.springframework.web.bind.annotation.RestController
class UserController {
    @org.springframework.beans.factory.annotation.Autowired
    private UserService users;

    @javax.annotation.Resource(name = "clock") private Clock clock;

    @org.springframework.beans.factory.annotation.Value("${app.name}") private String name;

    private int count = 0;

    @Cacheable(value = "users", key = "#id + #r", unless = "#result == null")
    User find(long id, String region) { return users.find(id); }

    @PreAuthorize("@guard.allows(#id)")
    void delete(long id) {}
}
"##;

const CONFIG: &str = r#"package com.acme;

@org.springframework.context.annotation.Configuration
public class AppConfig {
    @org.springframework.context.annotation.Bean
    public Clock clock() { return Clock.systemUTC(); }

    @org.springframework.context.annotation.Bean(name = {"mail", "mailer"})
    @org.springframework.context.annotation.Primary
    MailNotifier mailNotifier(Clock clock) { return new MailNotifier(clock); }

    @org.springframework.context.annotation.Bean
    Guard guard() { return new Guard(); }
}

interface UserRepository extends JpaRepository<User, Long> {}

class MailNotifier extends AbstractNotifier {}

abstract class AbstractNotifier implements Notifier {}

@org.springframework.stereotype.Component("sms")
@RequiredArgsConstructor
class SmsNotifier implements Notifier {
    private final Clock clock;
    private static final int RETRIES = 3;
}
"#;

const ASPECT: &str = r#"package com.acme;

@Aspect
@org.springframework.stereotype.Component
class Auditing {
    @Pointcut("within(com.acme..*)")
    void inApp() {}

    @Before("inApp() && bean(users) && execution(* *(..))")
    void audit() {}
}
"#;

const XML: &str = r#"<?xml version="1.0"?>
<beans xmlns="http://www.springframework.org/schema/beans">
  <bean id="legacy" class="com.acme.SmsNotifier" depends-on="clock, mail">
    <property name="clock" ref="clock"/>
  </bean>
  <alias name="legacy" alias="old"/>
  <beans profile="test">
    <bean name="fake" class="com.acme.MailNotifier"/>
  </beans>
</beans>
"#;

const KOTLIN: &str = r#"package com.acme.kt

@org.springframework.stereotype.Service
class ReportService(
    private val repo: UserRepository,
    @org.springframework.beans.factory.annotation.Qualifier("sms") private val notifier: Notifier,
) : Reports, AutoCloseable {
    @org.springframework.beans.factory.annotation.Autowired
    lateinit var clock: Clock

    override fun close() {
        val x = object : Runnable { override fun run() {} }
    }
}

@org.springframework.context.annotation.Configuration
class KtConfig {
    @org.springframework.context.annotation.Bean
    fun reports(service: ReportService): ReportFacade = ReportFacade(service)
}
"#;

const ROOT: &str = "/project/src/main/java/com/acme";

fn setup() -> MockHost {
    let host = MockHost::install();
    host.add_file("src/main/java/com/acme/UserServiceImpl.java", SERVICE);
    host.add_file("src/main/java/com/acme/UserController.java", CONTROLLER);
    host.add_file("src/main/java/com/acme/AppConfig.java", CONFIG);
    host.add_file("src/main/java/com/acme/Auditing.java", ASPECT);
    host.add_file("src/main/resources/beans.xml", XML);
    host.add_file("src/main/kotlin/com/acme/kt/Reports.kt", KOTLIN);
    host.add_file("pom.xml", "<project/>");
    host
}

// Legacy full-context fixtures explicitly warm lifecycle state before cache-only editor reads.
fn warmed_navigator() -> Navigator {
    let mut navigator = Navigator::default();
    for project in host::projects().unwrap_or_default() {
        navigator.indexes.get(&project, COMMAND_BUDGET);
    }
    navigator
}

fn doc(path: &str, text: &str) -> Document {
    let language = match path.rsplit('.').next() {
        Some("kt") => "kotlin",
        Some("xml") => "xml",
        _ => "java",
    };
    Document { path: path.into(), language: language.into(), version: 1, text: text.into() }
}

fn java(name: &str, text: &str) -> Document {
    doc(&format!("{ROOT}/{name}"), text)
}

/// The position just after the `n`th occurrence of `needle` (1-based).
fn after(text: &str, needle: &str, n: usize) -> Position {
    let at = text.match_indices(needle).nth(n - 1).unwrap().0 + needle.len();
    position(text, at)
}

fn names(model: &FileModel) -> Vec<(&str, &str)> {
    model.beans.iter().map(|b| (b.kind.as_str(), b.name.as_str())).collect()
}

fn injected(model: &FileModel) -> Vec<(&str, Option<&str>, bool)> {
    model.injections.iter().map(|i| (i.ty.as_str(), i.qualifier.as_deref(), i.multiple)).collect()
}

#[test]
fn reads_java_beans_and_injection_points() {
    let service = parse("A.java", SERVICE).unwrap();
    assert_eq!(names(&service), [("@Service", "users")]);
    assert_eq!(service.beans[0].profiles, ["dev", "prod"]);
    assert_eq!(injected(&service), [("UserRepository", None, false), ("Notifier", Some("sms"), false), ("Notifier", None, true)]);

    let controller = parse("A.java", CONTROLLER).unwrap();
    assert_eq!(injected(&controller), [("UserService", None, false), ("Clock", Some("clock"), false)], "@Value is config");
    assert!(controller.injections[0].field_annotation.is_some());
    assert_eq!(controller.injections[1].field_annotation, None, "@Resource is not flagged");

    let config = parse("A.java", CONFIG).unwrap();
    assert_eq!(
        names(&config),
        [
            ("@Configuration", "appConfig"),
            ("@Bean", "clock"),
            ("@Bean", "mail"),
            ("@Bean", "guard"),
            ("Spring Data repository", "userRepository"),
            ("@Component", "sms")
        ]
    );
    assert_eq!((config.beans[2].aliases.as_slice(), config.beans[2].primary), (&["mailer".to_owned()][..], true));
    assert_eq!(injected(&config), [("Clock", None, false), ("Clock", None, false)], "@Bean parameter and Lombok field");
    assert!(parse("A.java", "class Plain {}").unwrap().beans.is_empty());
}

#[test]
fn reads_xml_beans_and_references() {
    let xml = parse("beans.xml", XML).unwrap();
    assert_eq!(names(&xml), [("<bean>", "legacy"), ("<bean>", "fake")]);
    assert_eq!(xml.beans[0].aliases, ["old"]);
    assert_eq!((xml.beans[0].ty.as_str(), xml.beans[1].profiles.as_slice()), ("SmsNotifier", &["test".to_owned()][..]));
    let refs: Vec<_> = xml.injections.iter().map(|i| (i.name.as_str(), i.owner)).collect();
    assert_eq!(refs, [("clock", Some(0)), ("mail", Some(0)), ("clock", Some(0))]);
    let pom = parse("pom.xml", "<project><bean id=\"x\"/></project>").unwrap();
    assert!(pom.beans.is_empty(), "non-Spring XML must not create bean definitions");
    assert_eq!(pom.navigation.definitions[0].0, "resource#pom.xml");
}

#[test]
fn reads_kotlin_beans_and_injection_points() {
    let kt = parse("Reports.kt", KOTLIN).unwrap();
    assert_eq!(names(&kt), [("@Service", "reportService"), ("@Configuration", "ktConfig"), ("@Bean", "reports")]);
    assert_eq!(
        injected(&kt),
        [("UserRepository", None, false), ("Notifier", Some("sms"), false), ("Clock", None, false), ("ReportService", None, false)]
    );
    assert!(kt.injections[2].field_annotation.is_some());
    assert_eq!(kt.types[0].supers, ["Reports", "AutoCloseable"]);
    assert_eq!(kt.types[0].qualified, "com.acme.kt.ReportService");
    assert_eq!(kt.beans[2].ty, "ReportFacade");
}

#[test]
fn resolves_like_spring() {
    let _host = setup();
    let mut n = warmed_navigator();
    let service = java("UserServiceImpl.java", SERVICE);
    let mut at = |needle: &str, k: usize| n_definition(&mut n, &service, after(SERVICE, needle, k));
    assert_eq!(at("(UserRepo", 1), [("AppConfig.java", 15)]);
    assert_eq!(at("@org.springframework.beans.factory.annotation.Qualifier(\"sms\") Noti", 1), [("AppConfig.java", 23)], "qualifier picks sms");
    let mut all = at("List<Noti", 2);
    all.sort();
    assert_eq!(
        all,
        [("AppConfig.java", 9), ("AppConfig.java", 23), ("beans.xml", 2), ("beans.xml", 7)],
        "every Notifier, through the hierarchy"
    );

    let controller = java("UserController.java", CONTROLLER);
    let markers = n.checked_markers(&controller);
    let tips: Vec<_> = markers.iter().map(|m| (m.line, m.tooltip.as_str())).collect();
    assert_eq!(tips, [(5, "Injected bean: users (@Service) [dev, prod]"), (7, "Injected bean: clock() (@Bean)")]);
}

fn n_definition(n: &mut Navigator, doc: &Document, at: Position) -> Vec<(&'static str, u32)> {
    n.definition(doc, at).into_iter().map(|l| (file_name(&l.path).to_owned().leak() as &'static str, l.range.start.line)).collect()
}

#[test]
fn primary_wins_between_candidates() {
    let _host = setup();
    let mut n = warmed_navigator();
    let text = "package com.acme;\n@org.springframework.stereotype.Service\nclass Alerts {\n    Alerts(Notifier notifier) {}\n}\n";
    let found = n_definition(&mut n, &java("Alerts.java", text), after(text, "(Noti", 1));
    assert_eq!(found, [("AppConfig.java", 9)], "the @Primary mail bean");
}

#[test]
fn beans_show_where_they_are_injected() {
    let _host = setup();
    let mut n = warmed_navigator();
    let doc = java("AppConfig.java", CONFIG);
    let on_bean: Vec<_> = n.checked_markers(&doc).into_iter().filter(|m| m.line == 5 && m.icon.as_deref() == Some(BEAN_MARKER)).collect();
    let into: BTreeSet<_> = on_bean.iter().map(|m| m.tooltip.as_str()).collect();
    assert_eq!(
        into,
        BTreeSet::from(["Injected into legacy", "Injected into mail", "Injected into reportService", "Injected into sms", "Injected into userController"])
    );
    assert_eq!(on_bean.len(), 6, "one marker per injection point, joined by the IDE into one popup");
    assert!(on_bean.iter().any(|m| m.target.as_ref().is_some_and(|t| t.path.ends_with("/UserController.java"))));
}

const CONSUMER: &str = "package shop;\n@org.springframework.stereotype.Service class Consumer {\n @org.springframework.beans.factory.annotation.Autowired ServiceType service;\n @org.springframework.beans.factory.annotation.Autowired Only only;\n}";

fn ambiguous_setup() -> (MockHost, Navigator, Document) {
    let host = MockHost::install();
    host.add_file("src/main/java/shop/Consumer.java", CONSUMER);
    host.add_file("src/main/java/shop/ServiceType.java", "package shop; interface ServiceType {}");
    host.add_file("src/main/java/shop/First.java", "package shop; @org.springframework.stereotype.Service(\"first\") class First implements ServiceType {}");
    host.add_file("src/main/java/shop/Second.java", "package shop; @org.springframework.stereotype.Service(\"second\") class Second implements ServiceType {}");
    host.add_file("src/main/java/shop/Only.java", "package shop; @org.springframework.stereotype.Service class Only {}");
    let navigator = warmed_navigator();
    (host, navigator, doc("/project/src/main/java/shop/Consumer.java", CONSUMER))
}

/// The text a marker's target covers in the project file it points to.
fn covered(host: &MockHost, marker: &LineMarker) -> String {
    let target = marker.target.as_ref().unwrap();
    let text = host.invocation().project().unwrap().read_file(&target.path).unwrap();
    let (from, to) = (offset(&text, target.range.start).unwrap(), offset(&text, target.range.end).unwrap());
    text[from..to].to_owned()
}

#[test]
fn ambiguous_injection_marks_every_candidate_on_its_line() {
    let (host, mut navigator, doc) = ambiguous_setup();
    let markers = navigator.line_markers(&doc);
    let line: Vec<_> = markers.iter().filter(|m| m.line == 2).collect();
    assert!(line.iter().all(|m| m.icon.as_deref() == Some(BEAN_MARKER)), "one icon, so the IDE joins them into one popup");
    let mut tips: Vec<_> = line.iter().map(|m| m.tooltip.as_str()).collect();
    tips.sort();
    assert_eq!(tips, ["Candidate bean: first (@Service)", "Candidate bean: second (@Service)"]);
    let mut targets: Vec<_> = line.iter().map(|m| covered(&host, m)).collect();
    targets.sort();
    assert_eq!(targets, ["First", "Second"], "targets sit on the bean's class name, never an empty range");
    assert!(host.requests().is_empty() && host.process_runs().is_empty());
}

#[test]
fn single_candidate_keeps_one_direct_marker() {
    let (host, mut navigator, doc) = ambiguous_setup();
    let markers = navigator.line_markers(&doc);
    let line: Vec<_> = markers.iter().filter(|m| m.line == 3).collect();
    assert_eq!(line.len(), 1);
    assert_eq!(line[0].tooltip, "Injected bean: only (@Service)");
    assert_eq!(covered(&host, line[0]), "Only");
}

#[test]
fn line_targets_are_bounded_and_say_so() {
    let marker = |n: usize| LineMarker { line: 0, icon: None, tooltip: format!("t{n}"), target: None };
    let mut markers: Vec<_> = (0..MAX_LINE_TARGETS + 1).map(marker).collect();
    bounded(&mut markers[..], MAX_LINE_TARGETS);
    assert_eq!(markers[1].tooltip, "t1");
    bounded(&mut markers[..], MAX_LINE_TARGETS + 5);
    assert_eq!(markers[1].tooltip, "t1 (1000 of 1005 shown)");
}

#[test]
fn edits_to_the_open_file_update_the_index() {
    let _host = setup();
    let mut n = warmed_navigator();
    n.checked_markers(&java("UserServiceImpl.java", &SERVICE.replace("@Service(\"users\")", "")));
    assert!(n.checked_markers(&java("UserController.java", CONTROLLER)).iter().all(|m| m.line != 5));
}

#[test]
fn xml_references_navigate_and_complete() {
    let _host = setup();
    let mut n = warmed_navigator();
    let xml = doc("/project/src/main/resources/beans.xml", XML);
    assert_eq!(n_definition(&mut n, &xml, after(XML, "ref=\"clo", 1)), [("AppConfig.java", 5)]);
    assert_eq!(n_definition(&mut n, &xml, after(XML, "clock, ma", 1)), [("AppConfig.java", 9)], "a list item");
    assert_eq!(n_definition(&mut n, &xml, after(XML, "com.acme.Sms", 1)), [("AppConfig.java", 23)], "class");

    let edited = XML.replace("ref=\"clock\"", "ref=\"\"");
    let items = n.completion(&doc("/project/src/main/resources/beans.xml", &edited), after(&edited, "ref=\"", 1), None);
    let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
    assert!(labels.contains(&"clock") && labels.contains(&"mailer") && labels.contains(&"old"), "{labels:?}");

    let edited = XML.replace("class=\"com.acme.MailNotifier\"", "class=\"com.acme.Ma\"");
    let items = n.completion(&doc("/project/src/main/resources/beans.xml", &edited), after(&edited, "com.acme.Ma", 1), None);
    assert_eq!(items.iter().map(|i| i.label.as_str()).collect::<Vec<_>>(), ["com.acme.MailNotifier"]);
}

#[test]
fn spel_completes_beans_and_variables() {
    let _host = setup();
    let mut n = warmed_navigator();
    let controller = java("UserController.java", CONTROLLER);
    assert_eq!(n_definition(&mut n, &controller, after(CONTROLLER, "@gua", 1)), [("AppConfig.java", 12)]);

    let text = CONTROLLER.replace("#id + #r", "#id + #");
    let items = n.completion(&java("UserController.java", &text), after(&text, "#id + #", 1), None);
    let labels: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(labels, ["id", "region", "root", "result"]);

    let text = CONTROLLER.replace("@guard", "@");
    let items = n.completion(&java("UserController.java", &text), after(&text, "(\"@", 1), None);
    assert!(items.iter().any(|i| i.label == "guard"));
}

#[test]
fn pointcuts_complete_and_navigate() {
    let _host = setup();
    let mut n = warmed_navigator();
    let aspect = java("Auditing.java", ASPECT);
    assert_eq!(n_definition(&mut n, &aspect, after(ASPECT, "\"inAp", 1)), [("Auditing.java", 6)]);
    assert_eq!(n_definition(&mut n, &aspect, after(ASPECT, "bean(use", 1)), [("UserServiceImpl.java", 7)]);
    assert!(n_definition(&mut n, &aspect, after(ASPECT, "execu", 1)).is_empty());

    let text = ASPECT.replace("inApp() && bean", "in && bean");
    let items = n.completion(&java("Auditing.java", &text), after(&text, "(\"in", 1), None);
    let inserts: Vec<_> = items.iter().map(|i| (i.label.as_str(), i.insert_text.as_str())).collect();
    assert!(inserts.contains(&("execution", "execution(${1:* com.example..*.*(..)})")));
    assert!(inserts.contains(&("inApp", "inApp()")));
    assert!(n.completion(&aspect, after(ASPECT, "within(com.ac", 1), None).is_empty(), "designator arguments");
}

#[test]
fn kotlin_injections_navigate() {
    let _host = setup();
    let mut n = warmed_navigator();
    let kt = doc("/project/src/main/kotlin/com/acme/kt/Reports.kt", KOTLIN);
    assert_eq!(n_definition(&mut n, &kt, after(KOTLIN, "val repo: UserRepo", 1)), [("AppConfig.java", 15)]);
    assert_eq!(n_definition(&mut n, &kt, after(KOTLIN, "(service: Report", 1)), [("Reports.kt", 3)]);
}

#[test]
fn flags_field_injection_and_fixes_it() {
    let host = setup();
    let mut n = warmed_navigator();
    let controller = java("UserController.java", CONTROLLER);
    n.checked_markers(&controller);
    let problems = &host.diagnostics()[&controller.path];
    assert_eq!(problems.len(), 1);
    assert_eq!(problems[0].code.as_deref(), Some("field-injection"));

    let actions = n.code_actions(&controller, problems[0].range);
    assert_eq!(actions[0].title, "Use constructor injection");
    let mut edits = actions[0].edits[0].1.clone();
    edits.sort_by_key(|e| std::cmp::Reverse(offset(CONTROLLER, e.range.start)));
    let mut fixed = CONTROLLER.to_owned();
    for e in edits {
        let (s, t) = (offset(&fixed, e.range.start).unwrap(), offset(&fixed, e.range.end).unwrap());
        fixed.replace_range(s..t, &e.text);
    }
    assert!(fixed.contains("    private final UserService users;\n"), "{fixed}");
    assert!(
        fixed.contains("    private int count = 0;\n\n    UserController(UserService users) {\n        this.users = users;\n    }\n"),
        "{fixed}"
    );
    assert!(n.code_actions(&java("AppConfig.java", CONFIG), Range::new(Position::new(5, 0), Position::new(5, 0))).is_empty());
}

#[test]
fn views_list_and_draw_beans() {
    let host = setup();
    let mut n = warmed_navigator();
    let tree = n.render(BEANS_VIEW, None, &host.invocation()).unwrap();
    let Body::Tree(items) = tree.body else { panic!("tree expected") };
    let groups: Vec<_> = items.iter().filter(|i| i.parent.is_none()).map(|i| i.label.as_str()).collect();
    assert_eq!(groups, ["<bean>", "@Bean", "@Component", "@Configuration", "@RestController", "@Service", "Spring Data repository"]);
    let icon = |label: &str| items.iter().find(|i| i.label == label).and_then(|i| i.icon.as_deref());
    assert_eq!(
        [icon("<bean>"), icon("@Service"), icon("Spring Data repository")],
        [Some("file-xml"), Some("annotation"), Some("database")]
    );
    assert_eq!([icon("clock"), icon("userRepository")], [Some("method"), Some("interface")]);
    assert_eq!(tree.status.as_deref(), Some(&*format!("{} beans", items.len() - groups.len())));
    let users = items.iter().find(|i| i.label == "users").unwrap();
    assert_eq!(users.detail.as_deref(), Some("UserServiceImpl.java · dev, prod"));
    assert_eq!(icon("users"), Some("class"));
    n.handle(BEANS_VIEW, ViewEvent::Activate(users.key.clone()), &host.invocation()).unwrap();
    assert_eq!(host.opened_locations(), [(format!("{ROOT}/UserServiceImpl.java"), 7, 13)]);

    n.handle(BEANS_VIEW, ViewEvent::Button("section:graph".into()), &host.invocation()).unwrap();
    let graph = n.render(BEANS_VIEW, None, &host.invocation()).unwrap();
    let Body::Html(page) = graph.body else { panic!("html expected") };
    assert!(page.contains(">users<") && page.contains("<path class=\"e\""));
    assert!(page.contains("<title>users (@Service)</title>"));
    let status = graph.status.unwrap();
    assert!(status.ends_with("dependencies · arrows point to dependencies"), "{status}");
    n.handle(BEANS_VIEW, ViewEvent::Message(users.key.clone()), &host.invocation()).unwrap();
    assert_eq!(host.opened_locations().len(), 1, "the first activation focuses the bean");
    let refreshed = host.refreshed_views().len();
    n.handle(BEANS_VIEW, ViewEvent::Message(users.key.clone()), &host.invocation()).unwrap();
    assert_eq!(host.opened_locations().len(), 2, "activating the focused bean opens it");
    assert_eq!(host.refreshed_views().len(), refreshed, "a complete scan doesn't re-render");

    n.handle(BEANS_VIEW, ViewEvent::Button("refresh".into()), &host.invocation()).unwrap();
    assert_eq!(host.refreshed_views().last().map(String::as_str), Some(BEANS_VIEW));
}

fn toolbar_ids(view: &View) -> Vec<&str> {
    view.toolbar.iter().map(|b| b.id.as_str()).collect()
}

#[test]
fn dump_report_can_run_again_from_its_toolbar() {
    let host = setup();
    let mut n = warmed_navigator();
    n.run_command("dump-spring-configuration", &host.invocation(), None).unwrap();
    n.sections.set(REPORT_SECTION);
    let dumped = n.render(BEANS_VIEW, None, &host.invocation()).unwrap();
    assert!(toolbar_ids(&dumped).contains(&"dump"));
    n.handle(BEANS_VIEW, ViewEvent::Button("dump".into()), &host.invocation()).unwrap();
}

#[test]
fn beans_window_has_three_sections() {
    let host = setup();
    let mut n = warmed_navigator();
    let beans = n.render(BEANS_VIEW, None, &host.invocation()).unwrap();
    assert_eq!(&toolbar_ids(&beans)[..3], ["section:beans", "section:graph", "section:report"]);
    assert!(matches!(beans.body, Body::Tree(_)));
    n.handle(BEANS_VIEW, ViewEvent::Button("section:report".into()), &host.invocation()).unwrap();
    let report = n.render(BEANS_VIEW, None, &host.invocation()).unwrap();
    assert!(matches!(report.body, Body::Message(_)));
    assert_eq!(toolbar_ids(&report), ["section:beans", "section:graph", "section:report", "check"]);
    n.handle(BEANS_VIEW, ViewEvent::Button("check".into()), &host.invocation()).unwrap();
    let checked = n.render(BEANS_VIEW, None, &host.invocation()).unwrap();
    assert!(toolbar_ids(&checked).contains(&"check"), "results can be checked again");
}

#[test]
fn empty_beans_view_says_when_the_project_has_no_build_file() {
    let host = MockHost::install();
    host.add_file("src/Plain.java", "class Plain {}");
    let mut n = warmed_navigator();
    let view = n.render(BEANS_VIEW, None, &host.invocation()).unwrap();
    assert!(matches!(view.body, Body::Message(ref text) if text.contains("doesn't look like a Spring project")), "{:?}", view.body);
    host.add_file("build.gradle.kts", "plugins {}");
    let view = n.render(BEANS_VIEW, None, &host.invocation()).unwrap();
    assert!(matches!(view.body, Body::Message(ref text) if text == "No Spring beans found."), "{:?}", view.body);
}

#[test]
fn check_configuration_command_opens_the_report_section() {
    let host = setup();
    let mut n = warmed_navigator();
    assert_eq!(n.sections.active(), "beans");
    n.run_command("check-spring-configuration", &host.invocation(), None).unwrap();
    assert_eq!(n.sections.active(), "report");
    assert_eq!(host.revealed_views(), [BEANS_VIEW]);
    assert_eq!(host.refreshed_views().last().map(String::as_str), Some(BEANS_VIEW));
}

#[test]
fn graph_section_renders_the_bean_graph() {
    let host = setup();
    let mut n = warmed_navigator();
    n.handle(BEANS_VIEW, ViewEvent::Button("section:graph".into()), &host.invocation()).unwrap();
    let graph = n.render(BEANS_VIEW, None, &host.invocation()).unwrap();
    assert!(matches!(graph.body, Body::Html(_)));
    assert!(toolbar_ids(&graph).contains(&"refresh"));
}

#[test]
fn graph_over_the_cap_starts_with_a_search_prompt_and_focuses_the_first_match() {
    let host = MockHost::install();
    let beans: String = (0..MAX_GRAPH + 1).map(|i| format!("    @org.springframework.context.annotation.Bean Clock clock{i}() {{ return null; }}\n")).collect();
    host.add_file("src/main/java/com/acme/Many.java", &format!("@org.springframework.context.annotation.Configuration\nclass Many {{\n{beans}}}\n"));
    let mut n = warmed_navigator();
    n.sections.set(GRAPH_SECTION);
    let total = MAX_GRAPH + 2;
    let prompt = n.render(BEANS_VIEW, None, &host.invocation()).unwrap();
    assert_eq!(prompt.status.as_deref(), Some(&*format!("0 of {total} beans shown · find a bean to focus")));
    assert!(toolbar_ids(&prompt).contains(&graph::FIND));
    let Body::Html(page) = prompt.body else { panic!("html expected") };
    assert!(page.contains("Find a bean to show its neighbours.") && page.contains(&format!("has {total} beans")), "{page}");
    assert!(!page.contains("<svg"));

    host.answer_next_prompt(Some(&["  CLOCK7 "]));
    n.handle(BEANS_VIEW, ViewEvent::Button(graph::FIND.into()), &host.invocation()).unwrap();
    let focused = n.render(BEANS_VIEW, None, &host.invocation()).unwrap();
    let Body::Html(page) = focused.body else { panic!("html expected") };
    assert!(page.contains("Focused on <strong>clock7</strong>"), "{page}");
    assert!(page.contains(&format!("11 of {total} beans match \u{201c}CLOCK7\u{201d}.")), "{page}");
    assert!(page.contains("data-id=\"graph:all\">Clear Focus<"));
    assert!(focused.status.unwrap().starts_with(&format!("1 of {total} beans shown · 0 dependencies")));

    host.answer_next_prompt(None);
    n.handle(BEANS_VIEW, ViewEvent::Button(graph::FIND.into()), &host.invocation()).unwrap();
    assert!(n.graph.query.is_empty(), "Escape clears the search");
    n.handle(BEANS_VIEW, ViewEvent::Message("graph:all".into()), &host.invocation()).unwrap();
    let Body::Html(page) = n.render(BEANS_VIEW, None, &host.invocation()).unwrap().body else { panic!("html expected") };
    assert!(page.contains("Find a bean to show its neighbours."));
}

fn graph_page(n: &mut Navigator, host: &MockHost) -> String {
    let Body::Html(page) = n.render(BEANS_VIEW, None, &host.invocation()).unwrap().body else { panic!("html expected") };
    page
}

fn bean_key(n: &mut Navigator, host: &MockHost, name: &str) -> String {
    let index = n.indexes.cached(&host.invocation().project().unwrap()).unwrap();
    World::new(index).beans().find(|b| b.1.name == name).map(|(p, b)| location_key(p, b.range)).unwrap()
}

fn drawn(page: &str) -> Vec<&str> {
    page.match_indices("<g class=\"n").map(|(i, _)| &page[i + page[i..].find("<title>").unwrap() + 7..][..page[i + page[i..].find("<title>").unwrap() + 7..].find(" (").unwrap()]).collect()
}

#[test]
fn focusing_a_bean_shows_its_dependencies_and_dependents() {
    let host = setup();
    let mut n = warmed_navigator();
    n.sections.set(GRAPH_SECTION);
    assert!(graph_page(&mut n, &host).contains("Select a bean to show only its dependencies and dependents."));
    let users = bean_key(&mut n, &host, "users");
    n.handle(BEANS_VIEW, ViewEvent::Message(users), &host.invocation()).unwrap();
    let page = graph_page(&mut n, &host);
    assert_eq!(drawn(&page), ["users", "mail", "userRepository", "sms", "userController", "legacy", "fake"]);
    assert!(page.contains("<g class=\"n a\" data-id=\"b:"), "the focused bean is accented");
    assert!(page.contains("aria-checked=\"true\">1 hop<") && page.contains("data-id=\"graph:all\">Show All<"));

    n.handle(BEANS_VIEW, ViewEvent::Message("graph:hops:2".into()), &host.invocation()).unwrap();
    let page = graph_page(&mut n, &host);
    assert!(drawn(&page).contains(&"clock"), "userController's own dependency is two hops away");
    assert!(!drawn(&page).contains(&"reports"));
    assert!(page.contains("aria-checked=\"true\">2 hops<"));

    n.handle(BEANS_VIEW, ViewEvent::Message("graph:open".into()), &host.invocation()).unwrap();
    assert_eq!(host.opened_locations(), [(format!("{ROOT}/UserServiceImpl.java"), 7, 13)]);
    n.handle(BEANS_VIEW, ViewEvent::Message("graph:all".into()), &host.invocation()).unwrap();
    assert_eq!(drawn(&graph_page(&mut n, &host)).len(), 14);
    assert!(matches!(n.handle(BEANS_VIEW, ViewEvent::Message("b:0:0:/project/Gone.java".into()), &host.invocation()), Err(Error::Stale)));
}

#[test]
fn graph_search_matches_names_and_types_ignoring_case() {
    let host = setup();
    let mut n = warmed_navigator();
    n.sections.set(GRAPH_SECTION);
    host.answer_next_prompt(Some(&["serviceIMPL"]));
    n.handle(BEANS_VIEW, ViewEvent::Button(graph::FIND.into()), &host.invocation()).unwrap();
    let page = graph_page(&mut n, &host);
    assert!(page.contains("Focused on <strong>users</strong> <span class=\"t\">UserServiceImpl · @Service</span>"), "{page}");
    assert!(page.contains("1 of 14 beans match"));
    n.handle(BEANS_VIEW, ViewEvent::Message("graph:all".into()), &host.invocation()).unwrap();

    n.graph.query = "user".into();
    let page = graph_page(&mut n, &host);
    assert_eq!(drawn(&page), ["userController", "userRepository", "users"], "unfocused, the graph shows only the matches");
    assert!(page.contains("3 of 14 beans match \u{201c}user\u{201d}. Select one to show its neighbours."));
    n.handle(BEANS_VIEW, ViewEvent::Message("graph:clear".into()), &host.invocation()).unwrap();
    assert_eq!(drawn(&graph_page(&mut n, &host)).len(), 14);

    n.graph.query = "nothing-like-this".into();
    let page = graph_page(&mut n, &host);
    assert!(page.contains("No beans match") && page.contains("data-id=\"graph:clear\""));
}

#[test]
fn focus_in_graph_row_action_opens_the_graph_on_that_bean() {
    let host = setup();
    let mut n = warmed_navigator();
    let key = bean_key(&mut n, &host, "userController");
    n.run_command("focus-in-graph", &host.invocation(), Some(&key)).unwrap();
    assert_eq!(n.sections.active(), GRAPH_SECTION);
    assert_eq!(host.revealed_views(), [BEANS_VIEW]);
    assert!(graph_page(&mut n, &host).contains("Focused on <strong>userController</strong>"));
    assert!(matches!(n.run_command("focus-in-graph", &host.invocation(), Some("b:1:1:/project/Gone.java")), Err(Error::Stale)));
    let actions: serde_json::Value = serde_json::from_str(include_str!("ui_config.json")).unwrap();
    assert_eq!(actions["actions"][0]["id"], "focus-in-graph");
}

#[test]
fn p_and_c_namespace_refs_are_followed() {
    let host = MockHost::install();
    host.add_file("src/main/java/com/acme/AppConfig.java", CONFIG);
    let text = r#"<beans xmlns="http://www.springframework.org/schema/beans"
       xmlns:p="http://www.springframework.org/schema/p"
       xmlns:c="http://www.springframework.org/schema/c"
       xmlns:x="http://example.com/x">
  <bean id="a" class="x.A" p:dao-ref="clock" c:repo-ref="guard" c:_0-ref="clock" p:name="literal" x:thing-ref="guard"/>
</beans>
"#;
    let model = parse("beans.xml", text).unwrap();
    let refs: Vec<_> = model.injections.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(refs, ["clock", "guard", "clock"]);

    let mut n = warmed_navigator();
    let xml = doc("/project/src/main/resources/beans.xml", text);
    assert_eq!(n_definition(&mut n, &xml, after(text, "dao-ref=\"clo", 1)), [("AppConfig.java", 5)]);
    assert_eq!(n_definition(&mut n, &xml, after(text, "repo-ref=\"gua", 1)), [("AppConfig.java", 12)]);
    assert_eq!(n_definition(&mut n, &xml, after(text, "_0-ref=\"clo", 1)), [("AppConfig.java", 5)]);
    assert!(n_definition(&mut n, &xml, after(text, "p:name=\"lit", 1)).is_empty());
    assert!(n_definition(&mut n, &xml, after(text, "x:thing-ref=\"gua", 1)).is_empty());
}

#[test]
fn kotlin_bean_fun_without_return_type_uses_expression_type_when_obvious() {
    let text = r#"package com.acme.kt

@org.springframework.context.annotation.Configuration
class C {
    @org.springframework.context.annotation.Bean fun a() = Foo()
    @org.springframework.context.annotation.Bean fun b(x: X) = FooImpl(x, 1)
    @org.springframework.context.annotation.Bean fun c() = someCall()
    @org.springframework.context.annotation.Bean fun d() = foo.bar()
    @org.springframework.context.annotation.Bean fun e() = Foo().with(1)
    @org.springframework.context.annotation.Bean fun f() = if (x) A() else B()
    @org.springframework.context.annotation.Bean fun g() = com.acme.Foo()
    @org.springframework.context.annotation.Bean fun h(): Typed = Other()
}
"#;
    let model = parse("C.kt", text).unwrap();
    let types: Vec<_> = model.beans.iter().skip(1).map(|b| (b.name.as_str(), b.ty.as_str())).collect();
    assert_eq!(types, [("a", "Foo"), ("b", "FooImpl"), ("c", ""), ("d", ""), ("e", ""), ("f", ""), ("g", ""), ("h", "Typed")]);
}

fn apply(text: &str, mut edits: Vec<TextEdit>) -> String {
    edits.sort_by_key(|e| std::cmp::Reverse(offset(text, e.range.start)));
    let mut fixed = text.to_owned();
    for e in edits {
        let (s, t) = (offset(&fixed, e.range.start).unwrap(), offset(&fixed, e.range.end).unwrap());
        fixed.replace_range(s..t, &e.text);
    }
    fixed
}

#[test]
fn use_constructor_injection_merges_into_existing_constructor() {
    let class = |members: &str| format!("package com.acme;\n\n@org.springframework.stereotype.Service\nclass S {{\n    @org.springframework.beans.factory.annotation.Autowired\n    private Foo foo;\n\n{members}}}\n");
    let fix = |text: &str| constructor_injection(text, 4).map(|e| apply(text, e));

    let text = class("    private int n;\n\n    S(Bar bar) {\n        this.bar = bar;\n    }\n");
    let fixed = fix(&text).unwrap();
    assert!(fixed.contains("    private final Foo foo;\n"), "{fixed}");
    assert!(fixed.contains("    S(Bar bar, Foo foo) {\n        this.bar = bar;\n        this.foo = foo;\n    }\n"), "{fixed}");

    let fixed = fix(&class("    S() {}\n")).unwrap();
    assert!(fixed.contains("    S(Foo foo) {\n        this.foo = foo;\n    }\n"), "{fixed}");

    assert_eq!(fix(&class("    S() {}\n    S(int a) {}\n")), None, "several constructors");
    assert_eq!(fix(&class("    S() {\n        if (x) {\n    }\n")), None, "braces mismatch");
    assert_eq!(fix(&class("    S() {\n        this.foo = new Foo();\n    }\n")), None, "already assigned");
    assert_eq!(fix(&class("    S() {}\n    @org.springframework.beans.factory.annotation.Autowired void setBar(Bar bar) {}\n")), None, "autowired setter");
}

#[test]
fn merge_into_multiline_parameter_list() {
    let class = |ctor: &str| format!("package com.acme;\n\n@org.springframework.stereotype.Service\nclass S {{\n    @org.springframework.beans.factory.annotation.Autowired\n    private Foo foo;\n\n{ctor}}}\n");
    let fix = |text: &str| constructor_injection(text, 4).map(|e| apply(text, e));

    let fixed = fix(&class("    S(\n        Bar bar\n    ) {\n        this.bar = bar;\n    }\n")).unwrap();
    assert!(
        fixed.contains("    S(\n        Bar bar,\n        Foo foo\n    ) {\n        this.bar = bar;\n        this.foo = foo;\n    }\n"),
        "{fixed}"
    );

    let fixed = fix(&class("    S(Bar bar) {\n        super(bar);\n    }\n")).unwrap();
    assert!(fixed.contains("    S(Bar bar, Foo foo) {\n        super(bar);\n        this.foo = foo;\n    }\n"), "{fixed}");

    assert_eq!(fix(&class("    S(Foo foo) {\n        this.foo = foo;\n    }\n")), None, "parameter name clash");
    assert_eq!(fix(&class("    S() {\n        foo = new Foo();\n    }\n")), None, "bare assignment");

    let text = "package com.acme;\n\n@org.springframework.stereotype.Service\nclass S {\n    @org.springframework.beans.factory.annotation.Autowired\n    private final Foo foo;\n\n    S() {}\n}\n";
    let fixed = fix(text).unwrap();
    assert!(fixed.contains("    private final Foo foo;\n") && !fixed.contains("final final"), "{fixed}");
    assert!(fixed.contains("    S(Foo foo) {\n        this.foo = foo;\n    }\n"), "{fixed}");
}

#[test]
fn kotlin_string_body_is_not_a_constructor_call() {
    let text = "@org.springframework.context.annotation.Configuration\nclass C {\n    @org.springframework.context.annotation.Bean fun a() = \"Foo()\"\n}\n";
    let model = parse("C.kt", text).unwrap();
    assert_eq!(model.beans[1].ty, "");
}

#[test]
fn merge_after_a_trailing_line_comment() {
    let class = |ctor: &str| format!("package com.acme;\n\n@org.springframework.stereotype.Service\nclass S {{\n    @org.springframework.beans.factory.annotation.Autowired\n    private Foo foo;\n\n{ctor}}}\n");
    let fix = |text: &str| constructor_injection(text, 4).map(|e| apply(text, e));

    let fixed = fix(&class("    S(\n        Bar bar // the bar\n    ) {\n        this.bar = bar;\n    }\n")).unwrap();
    assert!(fixed.contains("    S(\n        Bar bar, // the bar\n        Foo foo\n    ) {\n"), "{fixed}");

    let fixed = fix(&class("    S(\n        Bar bar /* the bar */\n    ) {\n        this.bar = bar;\n    }\n")).unwrap();
    assert!(fixed.contains("    S(\n        Bar bar, /* the bar */\n        Foo foo\n    ) {\n"), "{fixed}");
}

#[test]
fn parameter_name_clash_is_refused_by_the_clash_check() {
    let text = "package com.acme;\n\n@org.springframework.stereotype.Service\nclass S {\n    @org.springframework.beans.factory.annotation.Autowired\n    private Foo foo;\n\n    S(Foo foo) {}\n}\n";
    assert_eq!(constructor_injection(text, 4), None);
}

fn findings(host: &MockHost, n: &mut Navigator, name: &str, text: &str) -> Vec<(String, String)> {
    let document = java(name, text);
    n.checked_markers(&document);
    host.diagnostics()
        .get(&document.path)
        .into_iter()
        .flatten()
        .filter(|d| d.code.as_deref() != Some("field-injection"))
        .map(|d| {
            let (from, to) = (offset(text, d.range.start).unwrap(), offset(text, d.range.end).unwrap());
            (d.code.clone().unwrap(), text[from..to].to_owned())
        })
        .collect()
}

fn found(text: &str) -> Vec<(String, String)> {
    let host = MockHost::install();
    findings(&host, &mut warmed_navigator(), "Subject.java", text)
}

fn pair(code: &str, text: &str) -> (String, String) {
    (code.into(), text.into())
}

#[test]
fn transactional_on_private_method_warns() {
    let text = "@org.springframework.stereotype.Service\nclass A {\n    @Transactional\n    private void save() {}\n}\n";
    assert_eq!(found(text), [pair("non-proxyable-method", "save")]);
}

#[test]
fn async_on_final_method_warns() {
    let text = "@org.springframework.stereotype.Service\nclass A {\n    @Async\n    public final void send() {}\n}\n";
    assert_eq!(found(text), [pair("non-proxyable-method", "send")]);
}

#[test]
fn cacheable_on_static_method_warns() {
    let text = "@org.springframework.stereotype.Component\nclass A {\n    @Cacheable(\"x\")\n    public static String load() { return \"\"; }\n}\n";
    assert_eq!(found(text), [pair("non-proxyable-method", "load")]);
}

#[test]
fn transactional_on_public_method_is_quiet() {
    let text = "@org.springframework.stereotype.Service\nclass A {\n    @Transactional\n    public void save() {}\n    @Transactional\n    void other() {}\n}\n";
    assert!(found(text).is_empty());
}

#[test]
fn non_proxyable_method_is_quiet_for_library_or_dynamic_name() {
    let library = "import com.acme.Async;\n@org.springframework.stereotype.Service\nclass A {\n    @Async\n    private void send() {}\n}\n";
    assert!(found(library).is_empty());
    let spring =
        "import org.springframework.scheduling.annotation.Async;\n@org.springframework.stereotype.Service\nclass A {\n    @Async\n    private void send() {}\n}\n";
    assert_eq!(found(spring), [pair("non-proxyable-method", "send")]);
    let jakarta = "import jakarta.transaction.Transactional;\n@org.springframework.stereotype.Service\nclass A {\n    @Transactional\n    private void send() {}\n}\n";
    assert_eq!(found(jakarta), [pair("non-proxyable-method", "send")]);
}

#[test]
fn scheduled_with_parameters_warns() {
    let text = "@org.springframework.stereotype.Component\nclass A {\n    @Scheduled(fixedRate = 1000)\n    void tick(String now) {}\n}\n";
    assert_eq!(found(text), [pair("scheduled-signature", "tick")]);
}

#[test]
fn scheduled_without_parameters_is_quiet() {
    let text = "@org.springframework.stereotype.Component\nclass A {\n    @Scheduled(fixedRate = 1000)\n    void tick() {}\n}\n";
    assert!(found(text).is_empty());
}

#[test]
fn scheduled_is_quiet_for_library_or_dynamic_name() {
    let text = "import org.quartz.Scheduled;\n@org.springframework.stereotype.Component\nclass A {\n    @Scheduled(fixedRate = 1000)\n    void tick(String now) {}\n}\n";
    assert!(found(text).is_empty());
}

const DUPLICATE: &str = "@org.springframework.stereotype.Service(\"dispatcher\")\nclass SmtpMailer {}\n";

fn with_other(other: &str) -> (MockHost, Navigator) {
    let host = setup();
    host.add_file("src/main/java/com/acme/Other.java", other);
    (host, warmed_navigator())
}

#[test]
fn duplicate_bean_names_across_files_warn_on_both() {
    let other = "@org.springframework.stereotype.Component(\"dispatcher\")\nclass Other {}\n";
    let (host, mut n) = with_other(other);
    host.add_file("src/main/java/com/acme/Subject.java", DUPLICATE);
    assert_eq!(findings(&host, &mut n, "Subject.java", DUPLICATE), [pair("duplicate-bean-name", "SmtpMailer")]);
    assert_eq!(findings(&host, &mut n, "Other.java", other), [pair("duplicate-bean-name", "Other")]);
}

#[test]
fn same_name_in_different_profiles_is_quiet() {
    let (host, mut n) = with_other("@org.springframework.context.annotation.Profile(\"prod\")\n@org.springframework.stereotype.Component(\"dispatcher\")\nclass Other {}\n");
    let dev = "@org.springframework.context.annotation.Profile(\"dev\")\n@org.springframework.stereotype.Service(\"dispatcher\")\nclass SmtpMailer {}\n";
    assert!(findings(&host, &mut n, "Subject.java", dev).is_empty());
    assert_eq!(findings(&host, &mut n, "Subject.java", DUPLICATE).len(), 1, "an unprofiled bean overlaps every profile");
}

#[test]
fn duplicate_bean_name_is_quiet_for_library_or_dynamic_name() {
    let (host, mut n) = with_other("@org.springframework.stereotype.Component(\"dispatcher\")\nclass Other {}\n");
    let dynamic = "@org.springframework.stereotype.Service(Names.DISPATCHER)\nclass Mailer {}\n";
    assert!(findings(&host, &mut n, "Subject.java", dynamic).is_empty());
    let conditional = "@org.springframework.stereotype.Service(\"dispatcher\")\n@ConditionalOnMissingBean\nclass SmtpMailer {}\n";
    assert!(findings(&host, &mut n, "Subject.java", conditional).is_empty());
    let library = "@org.springframework.stereotype.Service(\"mailSender\")\nclass Sender {}\n";
    assert!(findings(&host, &mut n, "Subject.java", library).is_empty());
}

#[test]
fn conditional_bean_clash_is_quiet_on_either_side() {
    let conditional = "@org.springframework.context.annotation.Configuration(proxyBeanMethods = false)\nclass Other {\n    @org.springframework.context.annotation.Bean @ConditionalOnMissingBean\n    Stopwatch stopwatch() { return null; }\n}\n";
    let plain = "@org.springframework.context.annotation.Configuration(proxyBeanMethods = false)\nclass Subject {\n    @org.springframework.context.annotation.Bean\n    Stopwatch stopwatch() { return null; }\n}\n";
    let (host, mut n) = with_other(conditional);
    host.add_file("src/main/java/com/acme/Subject.java", plain);
    assert!(findings(&host, &mut n, "Subject.java", plain).is_empty(), "the other side is conditional");
    assert!(findings(&host, &mut n, "Other.java", conditional).is_empty(), "this side is conditional");
    let (host, mut n) = with_other(plain.replace("Subject", "Other").as_str());
    assert_eq!(findings(&host, &mut n, "Subject.java", plain), [pair("duplicate-bean-name", "stopwatch")]);
}

#[test]
fn activation_succeeds_when_file_events_are_denied() {
    let host = MockHost::install().with_capabilities(&["project.read"]);
    assert_eq!(warmed_navigator().activate(&host.context("aritti.spring-beans")), Ok(()));
    assert!(host.logs().iter().any(|(level, _)| *level == LogLevel::Warn));
}

#[test]
fn overloaded_bean_methods_are_not_duplicates() {
    let text = "@org.springframework.context.annotation.Configuration(proxyBeanMethods = false)\nclass C {\n    @org.springframework.context.annotation.Bean Clock clock() { return null; }\n    @org.springframework.context.annotation.Bean Clock clock(Zone zone) { return null; }\n}\n";
    assert!(found(text).is_empty());
}

#[test]
fn configuration_with_final_bean_method_warns() {
    let text = "@org.springframework.context.annotation.Configuration\nclass C {\n    @org.springframework.context.annotation.Bean\n    public final Clock clock() { return null; }\n}\n";
    assert_eq!(found(text), [pair("configuration-proxy-method", "clock")]);
    let private = "@org.springframework.context.annotation.Configuration(proxyBeanMethods = true)\nclass C {\n    @org.springframework.context.annotation.Bean\n    private Clock clock() { return null; }\n}\n";
    assert_eq!(found(private), [pair("configuration-proxy-method", "clock")]);
}

#[test]
fn proxy_bean_methods_false_is_quiet() {
    let text = "@org.springframework.context.annotation.Configuration(proxyBeanMethods = false)\nclass C {\n    @org.springframework.context.annotation.Bean\n    public final Clock clock() { return null; }\n}\n";
    assert!(found(text).is_empty());
    let lite = "@org.springframework.stereotype.Component\nclass C {\n    @org.springframework.context.annotation.Bean\n    public final Clock clock() { return null; }\n}\n";
    assert!(found(lite).is_empty());
}

#[test]
fn configuration_proxy_method_is_quiet_for_library_or_dynamic_name() {
    let dynamic =
        "@org.springframework.context.annotation.Configuration(proxyBeanMethods = Settings.PROXY)\nclass C {\n    @org.springframework.context.annotation.Bean\n    public final Clock clock() { return null; }\n}\n";
    assert!(found(dynamic).is_empty());
    let library =
        "import com.acme.Configuration;\n@Configuration\nclass C {\n    @org.springframework.context.annotation.Bean\n    public final Clock clock() { return null; }\n}\n";
    assert!(found(library).is_empty());
    let static_bean = "@org.springframework.context.annotation.Configuration\nclass C {\n    @org.springframework.context.annotation.Bean\n    public static Clock clock() { return null; }\n}\n";
    assert!(found(static_bean).is_empty());
}

#[test]
fn capitalised_component_name_gets_a_hint() {
    let text = "@org.springframework.stereotype.Service(\"UserService\")\nclass UserServiceImpl {}\n";
    assert_eq!(found(text), [pair("bean-name-convention", "UserService")]);
    let host = MockHost::install();
    let document = java("Subject.java", text);
    warmed_navigator().checked_markers(&document);
    assert_eq!(host.diagnostics()[&document.path][0].severity, Severity::Hint);
    let method = "@org.springframework.context.annotation.Configuration(proxyBeanMethods = false)\nclass C {\n    @org.springframework.context.annotation.Bean(\"Clock\")\n    Clock clock() { return null; }\n}\n";
    assert_eq!(found(method), [pair("bean-name-convention", "Clock")]);
}

#[test]
fn bean_name_convention_is_quiet_for_library_or_dynamic_name() {
    assert!(found("@org.springframework.stereotype.Service(\"userService\")\nclass A {}\n").is_empty());
    assert!(found("@org.springframework.stereotype.Service(\"URLMapper\")\nclass A {}\n").is_empty(), "two capitals is Spring's own default");
    assert!(found("@org.springframework.stereotype.Service(Names.User)\nclass A {}\n").is_empty());
    assert!(found("import com.acme.Service;\n@Service(\"User\")\nclass A {}\n").is_empty());
}

const SELF_CALL: &str = "@org.springframework.stereotype.Service\nclass A {\n    @Transactional\n    public void save() {}\n    public void run() {\n        save();\n        this.save();\n    }\n}\n";

fn self_invocation(host: &MockHost, text: &str) -> Vec<(String, String)> {
    findings(host, &mut warmed_navigator(), "Subject.java", text)
}

#[test]
fn self_call_to_transactional_method_warns_only_for_unique_name() {
    let host = MockHost::install();
    host.set_setting("springBeans.checkSelfInvocation", "true");
    assert_eq!(self_invocation(&host, SELF_CALL), [pair("self-invocation", "save"), pair("self-invocation", "save")]);
    let overloaded = SELF_CALL.replace("    public void run", "    void save(int x) {}\n    public void run");
    assert!(self_invocation(&host, &overloaded).is_empty());
    let inherited = SELF_CALL.replace("class A", "class A extends Base");
    assert!(self_invocation(&host, &inherited).is_empty());
}

#[test]
fn call_through_injected_self_is_quiet() {
    let host = MockHost::install();
    host.set_setting("springBeans.checkSelfInvocation", "true");
    let text = SELF_CALL.replace("        save();\n        this.save();\n", "        self.save();\n        other.save();\n");
    assert!(self_invocation(&host, &text).is_empty());
    let same_annotation = SELF_CALL.replace("    public void run", "    @Transactional\n    public void run");
    assert!(self_invocation(&host, &same_annotation).is_empty(), "a transactional caller already holds the transaction");
}

#[test]
fn self_invocation_is_off_by_default() {
    assert!(found(SELF_CALL).is_empty());
}

#[test]
fn self_invocation_is_quiet_for_library_or_dynamic_name() {
    let host = MockHost::install();
    host.set_setting("springBeans.checkSelfInvocation", "true");
    let library = SELF_CALL.replace("@org.springframework.stereotype.Service", "import com.acme.Transactional;\n@org.springframework.stereotype.Service");
    assert!(self_invocation(&host, &library).is_empty());
    let local = "@org.springframework.stereotype.Service\nclass A {\n    @Transactional\n    public void save() {}\n    public void run(Runnable save) {\n        helper.save();\n        String s = \"save()\";\n    }\n}\n";
    assert!(self_invocation(&host, local).is_empty());
}

#[test]
fn switched_off_groups_are_quiet() {
    let host = MockHost::install();
    host.set_setting("springBeans.checkProxyMethods", "false");
    let text = "@org.springframework.stereotype.Service\nclass A {\n    @Transactional\n    private void save() {}\n}\n";
    assert!(findings(&host, &mut warmed_navigator(), "Subject.java", text).is_empty());
}

#[test]
fn fixed_file_publishes_empty_list() {
    let host = MockHost::install();
    let mut n = warmed_navigator();
    let broken = "@org.springframework.stereotype.Service\nclass A {\n    @Transactional\n    private void save() {}\n}\n";
    assert_eq!(findings(&host, &mut n, "Subject.java", broken).len(), 1);
    let fixed = broken.replace("private ", "");
    assert!(findings(&host, &mut n, "Subject.java", &fixed).is_empty());
    let path = java("Subject.java", "").path;
    assert!(host.diagnostics().get(&path).is_none_or(Vec::is_empty), "no stale problem is left");
}

#[test]
fn saved_and_opened_files_are_checked() {
    let host = MockHost::install();
    host.add_file("src/main/java/com/acme/Subject.java", "@org.springframework.stereotype.Service\nclass A {\n    @Transactional\n    private void save() {}\n}\n");
    let mut n = warmed_navigator();
    let path = java("Subject.java", "").path;
    n.checked_events(&[Event::FileOpened(path.clone())], &host.invocation());
    assert_eq!(host.diagnostics()[&path].len(), 1);
    host.add_file("src/main/java/com/acme/Subject.java", "@org.springframework.stereotype.Service\nclass A {}\n");
    n.checked_events(&[Event::FileSaved(path.clone())], &host.invocation());
    assert!(host.diagnostics().get(&path).is_none_or(Vec::is_empty));
}

#[test]
fn ranges_use_utf16_columns_with_multibyte_text() {
    let host = MockHost::install();
    let text = "@org.springframework.stereotype.Service\nclass A {\n    @Transactional\n    private void caf\u{e9}\u{1F600}() {}\n}\n";
    let document = java("Subject.java", text);
    warmed_navigator().checked_markers(&document);
    let d = &host.diagnostics()[&document.path][0];
    assert_eq!((d.range.start.line, d.range.start.character, d.range.end.character), (3, 17, 23));
}

fn unresolved(text: &str, extra: &[(&str, &str)]) -> Vec<(String, String)> {
    let host = setup();
    host.set_setting("springBeans.checkUnresolvedReferences", "true");
    for (name, content) in extra {
        host.add_file(&format!("src/main/java/com/acme/{name}"), content);
    }
    findings(&host, &mut warmed_navigator(), "Subject.java", text)
}

const BOOT_APP: (&str, &str) = ("App.java", "package com.acme;\n@org.springframework.boot.autoconfigure.SpringBootApplication\nclass App {}\n");

#[test]
fn depends_on_unknown_name_warns_when_enabled() {
    let text = "@org.springframework.stereotype.Service\n@DependsOn({\"mail\", \"cache\"})\nclass A {}\n";
    assert_eq!(unresolved(text, &[]), [pair("unresolved-depends-on", "cache")]);
    assert!(found(text).is_empty(), "off by default");
}

#[test]
fn depends_on_is_quiet_when_project_uses_auto_configuration() {
    let library = "@org.springframework.stereotype.Service\n@DependsOn(\"flyway\")\nclass A {}\n";
    assert!(unresolved(library, &[BOOT_APP]).is_empty());
    let enable = ("Cfg.java", "package com.acme;\n@EnableAutoConfiguration\nclass Cfg {}\n");
    assert!(unresolved(library, &[enable]).is_empty());
    let project_shaped = "@org.springframework.stereotype.Service\n@DependsOn(\"abstractNotifier\")\nclass A {}\n";
    assert_eq!(unresolved(project_shaped, &[BOOT_APP]), [pair("unresolved-depends-on", "abstractNotifier")]);
}

#[test]
fn depends_on_known_xml_bean_is_quiet() {
    assert!(unresolved("@org.springframework.stereotype.Service\n@DependsOn({\"legacy\", \"fake\", \"mailer\", \"users\"})\nclass A {}\n", &[]).is_empty());
}

#[test]
fn depends_on_is_quiet_for_library_or_dynamic_name() {
    assert!(unresolved("@org.springframework.stereotype.Service\n@DependsOn(Names.CACHE)\nclass A {}\n", &[]).is_empty());
    assert!(unresolved("import com.acme.DependsOn;\n@org.springframework.stereotype.Service\n@DependsOn(\"cache\")\nclass A {}\n", &[]).is_empty());
}

#[test]
fn component_scan_base_package_without_matching_sources_warns() {
    let text = "@org.springframework.context.annotation.Configuration\n@ComponentScan(basePackages = {\"com.acme\", \"com.acme.missing\"})\nclass A {}\n";
    assert_eq!(unresolved(text, &[]), [pair("component-scan-package-missing", "com.acme.missing")]);
    assert_eq!(unresolved("@ComponentScan(\"com.acme.gone\")\nclass A {}\n", &[]).len(), 1);
}

#[test]
fn component_scan_with_class_reference_is_quiet() {
    let text = "@ComponentScan(basePackageClasses = App.class, basePackages = \"com.acme.missing\")\nclass A {}\n";
    assert!(unresolved(text, &[]).is_empty());
}

#[test]
fn component_scan_is_quiet_for_library_or_dynamic_name() {
    assert!(unresolved("@ComponentScan(\"org.vendor.lib\")\nclass A {}\n", &[]).is_empty(), "a package outside the project's root");
    assert!(unresolved("@ComponentScan(Packages.ROOT)\nclass A {}\n", &[]).is_empty());
    assert!(unresolved("import com.acme.ComponentScan;\n@ComponentScan(\"com.acme.missing\")\nclass A {}\n", &[]).is_empty());
}

#[test]
fn unresolved_references_fixed_file_publishes_empty_list() {
    let host = setup();
    host.set_setting("springBeans.checkUnresolvedReferences", "true");
    let mut n = warmed_navigator();
    let broken = "@org.springframework.stereotype.Service\n@DependsOn(\"cache\")\nclass A {}\n";
    assert_eq!(findings(&host, &mut n, "Subject.java", broken).len(), 1);
    assert!(findings(&host, &mut n, "Subject.java", &broken.replace("cache", "mail")).is_empty());
}

#[test]
fn unresolved_reference_ranges_use_utf16_columns() {
    let host = setup();
    host.set_setting("springBeans.checkUnresolvedReferences", "true");
    let text = "@org.springframework.stereotype.Service\n@DependsOn(\"caf\u{e9}\u{1F600}\")\nclass A {}\n";
    let document = java("Subject.java", text);
    warmed_navigator().checked_markers(&document);
    let d = &host.diagnostics()[&document.path][0];
    assert_eq!((d.range.start.character, d.range.end.character), (12, 18));
}

fn findings_in(host: &MockHost, n: &mut Navigator, relative: &str, text: &str) -> Vec<(String, String)> {
    host.add_file(relative, text);
    let document = doc(&format!("/project/{relative}"), text);
    n.checked_markers(&document);
    host.diagnostics()
        .get(&document.path)
        .into_iter()
        .flatten()
        .filter(|d| d.code.as_deref() == Some("duplicate-bean-name"))
        .map(|d| (d.code.clone().unwrap(), d.message.clone()))
        .collect()
}

#[test]
fn nested_configuration_classes_do_not_clash() {
    let host = setup();
    let mut n = warmed_navigator();
    let nested = |outer: &str| format!("class {outer} {{\n    @org.springframework.context.annotation.Configuration\n    static class Config {{}}\n}}\n");
    let a = nested("ATests");
    let b = nested("BTests");
    assert!(findings_in(&host, &mut n, "src/test/java/com/acme/ATests.java", &a).is_empty());
    assert!(findings_in(&host, &mut n, "src/test/java/com/acme/BTests.java", &b).is_empty());
    assert!(findings_in(&host, &mut n, "src/test/java/com/acme/ATests.java", &a).is_empty());
}

#[test]
fn same_simple_name_in_different_modules_is_quiet() {
    let host = setup();
    host.add_file("one/pom.xml", "<project/>");
    host.add_file("two/pom.xml", "<project/>");
    let app = "@org.springframework.boot.autoconfigure.SpringBootApplication\nclass Application {}\n";
    let mut n = warmed_navigator();
    findings_in(&host, &mut n, "two/src/main/java/org/two/Application.java", app);
    assert!(findings_in(&host, &mut n, "one/src/main/java/org/one/Application.java", app).is_empty());
    assert!(findings_in(&host, &mut n, "two/src/main/java/org/two/Application.java", app).is_empty());
}

#[test]
fn same_name_in_main_and_test_source_sets_is_quiet() {
    let host = setup();
    let service = "@org.springframework.stereotype.Service(\"dispatcher\")\nclass Dispatcher {}\n";
    let mut n = warmed_navigator();
    findings_in(&host, &mut n, "src/test/java/com/acme/Dispatcher.java", service);
    assert!(findings_in(&host, &mut n, "src/main/java/com/acme/Dispatcher.java", service).is_empty());
}

#[test]
fn same_name_in_same_module_and_source_set_still_warns() {
    let host = setup();
    host.add_file("one/pom.xml", "<project/>");
    let service = "@org.springframework.stereotype.Service(\"dispatcher\")\nclass Dispatcher {}\n";
    let other = "@org.springframework.stereotype.Service(\"dispatcher\")\nclass Other {}\n";
    let mut n = warmed_navigator();
    findings_in(&host, &mut n, "one/src/main/java/org/one/other/Other.java", other);
    assert_eq!(findings_in(&host, &mut n, "one/src/main/java/org/one/Dispatcher.java", service).len(), 1);
}

#[test]
fn unresolved_references_are_quiet_on_a_partial_index() {
    let host = setup();
    host.set_setting("springBeans.checkUnresolvedReferences", "true");
    let text = "@org.springframework.stereotype.Service\n@DependsOn(\"cache\")\nclass A {}\n";
    let mut index = cache::Cache::new();
    index.update(&format!("{ROOT}/Subject.java"), text);
    assert!(!index.complete());
    let file = java::parse(text);
    let world = World::new(&index);
    let found = checks::check_java(&format!("{ROOT}/Subject.java"), &file, text, &world, checks::Groups::from_settings());
    assert!(found.iter().all(|f| f.code != "unresolved-depends-on"));
}

const XML_PATH: &str = "/project/src/main/resources/aop.xml";

fn beans_xml(body: &str) -> String {
    format!(
        "<beans xmlns=\"http://www.springframework.org/schema/beans\"\n       xmlns:aop=\"http://www.springframework.org/schema/aop\"{}>\n{body}\n</beans>\n",
        if body.contains("<context:") {" xmlns:context=\"http://www.springframework.org/schema/context\""} else {""}
    )
}

fn xml_findings(host: &MockHost, text: &str) -> Vec<(String, String)> {
    let document = doc(XML_PATH, text);
    warmed_navigator().checked_markers(&document);
    host.diagnostics()
        .get(XML_PATH)
        .into_iter()
        .flatten()
        .map(|d| {
            let (from, to) = (offset(text, d.range.start).unwrap(), offset(text, d.range.end).unwrap());
            (d.code.clone().unwrap(), text[from..to].to_owned())
        })
        .collect()
}

fn aop_found(body: &str) -> Vec<(String, String)> {
    xml_findings(&MockHost::install(), &beans_xml(body))
}

#[test]
fn aop_pointcut_without_expression_is_reported() {
    let found = aop_found("<aop:config><aop:pointcut id=\"p\"/></aop:config>");
    assert_eq!(found, [pair("aop-structure", "aop:pointcut")]);
    assert_eq!(aop_found("<aop:config><aop:pointcut expression=\"within(a..*)\"/></aop:config>").len(), 1);
}

#[test]
fn aop_aspect_without_ref_is_reported() {
    let found = aop_found("<aop:config><aop:aspect id=\"a\"/></aop:config>");
    assert_eq!(found, [pair("aop-structure", "aop:aspect")]);
}

#[test]
fn aop_advice_without_pointcut_is_reported() {
    let found = aop_found("<aop:config><aop:aspect ref=\"a\"><aop:before method=\"m\"/></aop:aspect></aop:config>");
    assert_eq!(found, [pair("aop-structure", "aop:before")]);
    let no_method = aop_found("<aop:config><aop:aspect ref=\"a\"><aop:around pointcut-ref=\"p\"/></aop:aspect></aop:config>");
    assert_eq!(no_method, [pair("aop-structure", "aop:around")]);
}

#[test]
fn aop_advice_with_both_pointcut_and_pointcut_ref_is_reported() {
    let found = aop_found(
        "<aop:config><aop:aspect ref=\"a\"><aop:after method=\"m\" pointcut=\"x\" pointcut-ref=\"p\"/></aop:aspect></aop:config>",
    );
    assert_eq!(found, [pair("aop-structure", "aop:after")]);
}

#[test]
fn aop_advisor_without_advice_ref_is_reported() {
    let found = aop_found("<aop:config><aop:advisor pointcut-ref=\"p\"/></aop:config>");
    assert_eq!(found, [pair("aop-structure", "aop:advisor")]);
}

#[test]
fn aop_structure_is_quiet_for_library_or_dynamic_name() {
    let dynamic = "<aop:config><aop:aspect ref=\"${aspect}\"><aop:before method=\"m\" pointcut=\"${a}\" pointcut-ref=\"${b}\"/></aop:aspect></aop:config>";
    assert!(aop_found(dynamic).is_empty());
    let lookalike = "<x:config xmlns:x=\"urn:other\"><x:pointcut/><x:aspect/><x:advisor/></x:config>";
    assert!(aop_found(lookalike).is_empty());
    let outside_config = "<aop:pointcut id=\"p\"/>";
    assert!(aop_found(outside_config).is_empty());
    let dynamic_pair =
        "<aop:config><aop:aspect ref=\"a\"><aop:before method=\"m\" pointcut=\"x\" pointcut-ref=\"#{b}\"/></aop:aspect></aop:config>";
    assert!(aop_found(dynamic_pair).is_empty());
    let not_beans = "<project xmlns:aop=\"http://www.springframework.org/schema/aop\"><aop:config><aop:aspect/></aop:config></project>";
    assert!(xml_findings(&MockHost::install(), not_beans).is_empty());
}

#[test]
fn bean_with_class_and_factory_bean_is_reported() {
    let found = aop_found("<bean id=\"a\" class=\"x.Y\" factory-bean=\"f\" factory-method=\"make\"/>");
    assert_eq!(found, [pair("xml-bean-attributes", "factory-bean")]);
    let host = MockHost::install();
    xml_findings(&host, &beans_xml("<bean id=\"a\" class=\"x.Y\" factory-bean=\"f\" factory-method=\"make\"/>"));
    assert_eq!(host.diagnostics()[XML_PATH][0].severity, Severity::Warning);
    xml_findings(&host, &beans_xml("<bean id=\"a\" factory-bean=\"f\"/>"));
    assert_eq!(host.diagnostics()[XML_PATH][0].severity, Severity::Error);
}

#[test]
fn factory_bean_without_factory_method_is_reported() {
    let found = aop_found("<bean id=\"a\" factory-bean=\"f\"/>");
    assert_eq!(found, [pair("xml-bean-attributes", "factory-bean")]);
}

#[test]
fn bean_without_class_parent_or_factory_bean_is_reported() {
    let found = aop_found("<bean id=\"a\"/>");
    assert_eq!(found, [pair("xml-bean-attributes", "bean")]);
    assert!(aop_found("<bean id=\"a\" parent=\"b\"/>").is_empty());
}

#[test]
fn abstract_bean_without_class_is_quiet() {
    assert!(aop_found("<bean id=\"a\" abstract=\"true\"/>").is_empty());
}

#[test]
fn property_with_ref_and_value_is_reported() {
    let found = aop_found("<bean class=\"x.Y\"><property name=\"p\" ref=\"a\" value=\"b\"/></bean>");
    assert_eq!(found, [pair("xml-injection-value", "property")]);
    let arg = aop_found("<bean class=\"x.Y\"><constructor-arg ref=\"a\" value=\"b\"/></bean>");
    assert_eq!(arg, [pair("xml-injection-value", "constructor-arg")]);
}

#[test]
fn property_with_attribute_and_child_element_is_reported() {
    let found = aop_found("<bean class=\"x.Y\"><property name=\"p\" value=\"b\"><list/></property></bean>");
    assert_eq!(found, [pair("xml-injection-value", "property")]);
    let described = aop_found("<bean class=\"x.Y\"><property name=\"p\" value=\"b\"><description>d</description></property></bean>");
    assert!(described.is_empty());
}

#[test]
fn property_with_nothing_is_reported() {
    let found = aop_found("<bean class=\"x.Y\"><property name=\"p\"/></bean>");
    assert_eq!(found, [pair("xml-injection-value", "property")]);
    assert!(aop_found("<bean class=\"x.Y\"><property name=\"p\"><null/></property></bean>").is_empty());
}

#[test]
fn constructor_arg_without_value_is_quiet() {
    assert!(aop_found("<bean class=\"x.Y\"><constructor-arg index=\"0\"/></bean>").is_empty());
}

#[test]
fn misspelled_scope_is_reported() {
    let found = aop_found("<bean class=\"x.Y\" scope=\"Singleton\"/>");
    assert_eq!(found, [pair("xml-scope", "Singleton")]);
    let typo = aop_found("<bean class=\"x.Y\" scope=\"protoype\"/>");
    assert_eq!(typo, [pair("xml-scope", "protoype")]);
}

#[test]
fn bundle_scope_is_accepted() {
    assert!(aop_found("<bean class=\"x.Y\" scope=\"bundle\"/>").is_empty());
}

#[test]
fn custom_scope_is_quiet() {
    assert!(aop_found("<bean class=\"x.Y\" scope=\"thread\"/>").is_empty());
    assert!(aop_found("<bean class=\"x.Y\" scope=\"custom\"/>").is_empty());
}

#[test]
fn xml_attribute_checks_are_quiet_for_library_or_dynamic_name() {
    let dynamic =
        "<bean id=\"a\" class=\"${c}\" factory-bean=\"#{f}\" scope=\"${Singleton}\"><property name=\"p\" ref=\"${r}\" value=\"v\"/></bean>";
    assert!(aop_found(dynamic).is_empty());
    let other_ns = "<x:bean xmlns:x=\"urn:other\" id=\"a\" scope=\"Singleton\"><x:property name=\"p\"/></x:bean>";
    assert!(aop_found(other_ns).is_empty());
    let other_file = "<project xmlns=\"urn:other\"><bean id=\"a\" scope=\"Singleton\"><property name=\"p\"/></bean></project>";
    assert!(xml_findings(&MockHost::install(), other_file).is_empty());
}

#[test]
fn xml_attribute_checks_use_utf16_columns_and_crlf_lines() {
    let host = MockHost::install();
    let text = beans_xml("<!-- caf\u{e9}\u{1F600} --><bean id=\"a\"/>");
    xml_findings(&host, &text);
    let d = &host.diagnostics()[XML_PATH][0];
    assert_eq!((d.range.start.line, d.range.start.character, d.range.end.character), (2, 16, 20));
    let text = beans_xml("<bean class=\"x.Y\"\n scope=\"Singleton\"/>").replace('\n', "\r\n");
    xml_findings(&host, &text);
    let d = &host.diagnostics()[XML_PATH][0];
    assert_eq!((d.range.start.line, d.range.start.character, d.range.end.character), (3, 8, 17));
}

#[test]
fn xml_bean_name_with_capital_is_hinted() {
    let found = aop_found("<bean id=\"Dispatcher\" name=\"alias, Other\" class=\"x.Y\"/>");
    assert_eq!(found, [pair("bean-name-convention", "Dispatcher"), pair("bean-name-convention", "Other")]);
}

#[test]
fn duplicate_xml_ids_in_one_file_are_reported() {
    let found = aop_found("<bean id=\"a\" class=\"x.Y\"/>\n<bean id=\"b\" name=\"a\" class=\"x.Y\"/>");
    assert_eq!(found, [pair("duplicate-bean-name", "a")]);
    let host = MockHost::install();
    let text = beans_xml("<bean id=\"a\" class=\"x.Y\"/>\n<bean id=\"a\" class=\"x.Y\"/>");
    xml_findings(&host, &text);
    let d = &host.diagnostics()[XML_PATH][0];
    assert_eq!((d.range.start.line, d.range.start.character), (3, 10));
}

#[test]
fn xml_bean_clashing_with_java_bean_is_reported() {
    let host = MockHost::install();
    host.add_file("src/main/java/com/acme/Dispatcher.java", "@org.springframework.stereotype.Service(\"dispatcher\")\nclass Dispatcher {}\n");
    let found = xml_findings(&host, &beans_xml("<bean id=\"dispatcher\" class=\"x.Y\"/>"));
    assert_eq!(found, [pair("duplicate-bean-name", "dispatcher")]);
    let host = MockHost::install();
    host.add_file("src/main/java/com/acme/Dispatcher.java", "@org.springframework.stereotype.Service(\"dispatcher\")\n@org.springframework.context.annotation.Profile(\"dev\")\nclass Dispatcher {}\n");
    let other_profile = beans_xml("<beans profile=\"prod\"><bean id=\"dispatcher\" class=\"x.Y\"/></beans>");
    assert!(xml_findings(&host, &other_profile).is_empty());
}

#[test]
fn same_id_in_different_profile_blocks_is_quiet() {
    let body =
        "<beans profile=\"dev\"><bean id=\"a\" class=\"x.Y\"/></beans><beans profile=\"prod\"><bean id=\"a\" class=\"x.Z\"/></beans>";
    assert!(aop_found(body).is_empty());
}

#[test]
fn same_id_in_two_xml_files_is_quiet() {
    let host = MockHost::install();
    host.add_file("src/main/resources/other.xml", &beans_xml("<bean id=\"a\" class=\"x.Y\"/>"));
    assert!(xml_findings(&host, &beans_xml("<bean id=\"a\" class=\"x.Z\"/>")).is_empty());
}

#[test]
fn generated_names_are_quiet() {
    let body = "<bean id=\"x.Y#0\" class=\"x.Y\"/><bean id=\"x.Y#0\" class=\"x.Y\"/>";
    assert!(aop_found(body).is_empty());
}

#[test]
fn xml_names_are_quiet_for_library_or_dynamic_name() {
    let dynamic = "<bean id=\"${A}\" class=\"x.Y\"/><bean id=\"${A}\" class=\"x.Y\"/><bean id=\"A#{b}\" class=\"x.Y\"/>";
    assert!(aop_found(dynamic).is_empty());
    let other_file = "<project xmlns=\"urn:other\"><bean id=\"A\"/><bean id=\"A\"/></project>";
    assert!(xml_findings(&MockHost::install(), other_file).is_empty());
    let host = MockHost::install();
    host.set_setting("springBeans.checkNaming", "false");
    host.set_setting("springBeans.checkDuplicateNames", "false");
    assert!(xml_findings(&host, &beans_xml("<bean id=\"A\" class=\"x.Y\"/><bean id=\"A\" class=\"x.Y\"/>")).is_empty());
}

#[test]
fn ref_to_abstract_bean_is_reported() {
    let body = "<bean id=\"base\" abstract=\"true\"/>\n<bean id=\"a\" class=\"x.Y\" depends-on=\"base\"><property name=\"p\" ref=\"base\"/><property name=\"q\"><ref bean=\"base\"/></property></bean>";
    let found: Vec<_> = aop_found(body).into_iter().filter(|(code, _)| code == "xml-abstract-reference").collect();
    assert_eq!(found, vec![pair("xml-abstract-reference", "base"); 3]);
}

#[test]
fn parent_to_abstract_bean_is_quiet() {
    assert!(aop_found("<bean id=\"base\" abstract=\"true\"/><bean id=\"a\" parent=\"base\"/>").is_empty());
}

#[test]
fn ref_to_bean_with_a_concrete_definition_is_quiet() {
    let host = MockHost::install();
    host.add_file("src/main/java/com/acme/Base.java", "@org.springframework.stereotype.Service(\"base\")\nclass Base {}\n");
    let body = "<bean id=\"base\" abstract=\"true\"/><bean id=\"a\" class=\"x.Y\"><property name=\"p\" ref=\"base\"/></bean>";
    let found = xml_findings(&host, &beans_xml(body));
    assert!(found.iter().all(|(code, _)| code != "xml-abstract-reference"));
}

#[test]
fn xml_name_findings_use_utf16_columns_and_crlf_lines() {
    let host = MockHost::install();
    let text = beans_xml("<!-- caf\u{e9}\u{1F600} --><bean id=\"Ab\" class=\"x.Y\"/>");
    xml_findings(&host, &text);
    let d = &host.diagnostics()[XML_PATH][0];
    assert_eq!((d.range.start.line, d.range.start.character, d.range.end.character), (2, 25, 27));
    let text = beans_xml("<bean id=\"base\" abstract=\"true\"/>\n<bean class=\"x.Y\"\n  depends-on=\"base\"/>").replace('\n', "\r\n");
    xml_findings(&host, &text);
    let d = &host.diagnostics()[XML_PATH][0];
    assert_eq!((d.range.start.line, d.range.start.character, d.range.end.character), (4, 14, 18));
}

#[test]
fn switched_off_xml_group_is_quiet() {
    let host = MockHost::install();
    host.set_setting("springBeans.checkXml", "false");
    assert!(xml_findings(&host, &beans_xml("<bean id=\"a\"/>")).is_empty());
}

#[test]
fn xml_ranges_use_utf16_columns_with_multibyte_text() {
    let host = MockHost::install();
    let text = beans_xml("<!-- caf\u{e9}\u{1F600} --><aop:config><aop:aspect id=\"a\"/></aop:config>");
    xml_findings(&host, &text);
    let d = &host.diagnostics()[XML_PATH][0];
    assert_eq!((d.range.start.line, d.range.start.character, d.range.end.character), (2, 28, 38));
}

#[test]
fn xml_crlf_files_report_correct_lines() {
    let host = MockHost::install();
    let text = beans_xml("<aop:config>\n<aop:aspect id=\"a\"/>\n</aop:config>").replace('\n', "\r\n");
    xml_findings(&host, &text);
    let d = &host.diagnostics()[XML_PATH][0];
    assert_eq!((d.range.start.line, d.range.start.character), (3, 1));
}

#[test]
fn fixed_xml_file_publishes_empty_list() {
    let host = MockHost::install();
    let broken = beans_xml("<aop:config><aop:aspect id=\"a\"/></aop:config>");
    assert_eq!(xml_findings(&host, &broken).len(), 1);
    let fixed = broken.replace("id=\"a\"", "ref=\"a\"");
    assert!(xml_findings(&host, &fixed).is_empty());
    assert!(host.diagnostics().get(XML_PATH).is_none_or(Vec::is_empty), "no stale problem is left");
}

#[test]
fn switched_off_aop_group_is_quiet() {
    let host = MockHost::install();
    host.set_setting("springBeans.checkAop", "false");
    assert!(xml_findings(&host, &beans_xml("<aop:config><aop:aspect id=\"a\"/></aop:config>")).is_empty());
}

#[test]
fn saved_xml_is_checked_and_pom_is_not() {
    let host = MockHost::install();
    let broken = beans_xml("<aop:config><aop:aspect id=\"a\"/></aop:config>");
    host.add_file("src/main/resources/aop.xml", &broken);
    host.add_file("pom.xml", "<project><bean id=\"a\" class=\"x.Y\"/></project>");
    let mut n = warmed_navigator();
    n.checked_events(&[Event::FileSaved(XML_PATH.into())], &host.invocation());
    assert_eq!(host.diagnostics()[XML_PATH].len(), 1);
    n.checked_events(&[Event::FileSaved("/project/pom.xml".into())], &host.invocation());
    assert!(host.diagnostics().get("/project/pom.xml").is_none_or(Vec::is_empty));
}

const WEAVING_ASPECT: &str = "import org.aspectj.lang.annotation.*;\n\n@Aspect\nclass Tracing {\n    @Before(\"within(a..*) && call(* *(..))\")\n    void trace() {}\n}\n";

#[test]
fn unsupported_designator_in_java_advice_is_reported() {
    assert_eq!(found(WEAVING_ASPECT), [pair("unsupported-pointcut-designator", "call")]);
    let utf16 = "@Aspect\nclass T {\n    @Pointcut(\"within(é..*) && cflow(x())\")\n    void p() {}\n}\n";
    let host = MockHost::install();
    let document = java("Subject.java", utf16);
    warmed_navigator().checked_markers(&document);
    let diagnostic = &host.diagnostics()[&document.path][0];
    assert_eq!((diagnostic.range.start.line, diagnostic.range.start.character), (2, 31));
    assert_eq!(diagnostic.range.end.character, 36);
}

#[test]
fn unsupported_designator_in_xml_expression_is_reported() {
    let body = "<aop:config><aop:pointcut id=\"p\" expression=\"call(* *(..))\"/>\
        <aop:aspect ref=\"a\"><aop:before method=\"m\" pointcut=\"within(a..*) &amp;&amp; get(* x)\"/></aop:aspect></aop:config>";
    assert_eq!(aop_found(body), [pair("unsupported-pointcut-designator", "call"), pair("unsupported-pointcut-designator", "get")]);
    assert!(aop_found("<aop:config><aop:pointcut id=\"p\" expression=\"execution(* get(..))\"/></aop:config>").is_empty());
}

#[test]
fn unsupported_designator_is_quiet_for_library_or_dynamic_name() {
    let concatenated = WEAVING_ASPECT.replace("\"within(a..*) && call(* *(..))\"", "\"within(a..*) && \" + \"call(* *(..))\"");
    assert!(found(&concatenated).is_empty());
    let placeholder = WEAVING_ASPECT.replace("call(* *(..))", "${expr}");
    assert!(found(&placeholder).is_empty());
    let foreign = "import org.junit.Before;\n\n@Aspect\nclass T {\n    @Before(\"call(* *(..))\")\n    void p() {}\n}\n";
    assert!(found(foreign).is_empty());
    let not_an_aspect = "class T {\n    @Before(\"call(* *(..))\")\n    void p() {}\n}\n";
    assert!(found(not_an_aspect).is_empty());
    let host = MockHost::install();
    host.add_file("pom.xml", "<project><build><plugin>aspectj-maven-plugin</plugin></build></project>");
    assert!(findings(&host, &mut warmed_navigator(), "Subject.java", WEAVING_ASPECT).is_empty());
    let xml_host = MockHost::install();
    xml_host.add_file("pom.xml", "<project>aspectjtools</project>");
    let body = "<aop:config><aop:pointcut id=\"p\" expression=\"call(* *(..))\"/></aop:config>";
    assert!(xml_findings(&xml_host, &beans_xml(body)).is_empty());
    assert!(aop_found("<aop:config><aop:pointcut id=\"p\" expression=\"${expr}\"/></aop:config>").is_empty());
}

#[test]
fn crlf_files_report_correct_lines() {
    let text = "@Aspect\r\nclass T {\r\n    @Pointcut(\"within(a..*) && cflow(x())\")\r\n    void p() {}\r\n}\r\n";
    let document = java("Subject.java", text);
    let host = MockHost::install();
    warmed_navigator().checked_markers(&document);
    let range = host.diagnostics()[&document.path][0].range;
    assert_eq!((range.start.line, range.start.character, range.end.line, range.end.character), (2, 31, 2, 36));

    let xml = beans_xml("<aop:config>\r\n<aop:pointcut id=\"p\" expression=\"within(a..*) &amp;&amp; call(* *(..))\"/>\r\n</aop:config>")
        .replace('\n', "\r\n");
    let host = MockHost::install();
    warmed_navigator().checked_markers(&doc(XML_PATH, &xml));
    let range = host.diagnostics()[XML_PATH][0].range;
    assert_eq!((range.start.line, range.start.character, range.end.line, range.end.character), (3, 57, 3, 61));
}

#[test]
fn escaped_literal_is_quiet() {
    let escaped = WEAVING_ASPECT.replace("within(a..*) &&", "within(a..*) \\t&&");
    assert!(found(&escaped).is_empty());
}

fn applied_xml_fix(text: &str) -> Option<String> {
    let host = MockHost::install();
    let document = doc(XML_PATH, text);
    let mut navigator = warmed_navigator();
    navigator.checked_markers(&document);
    let hint = host.diagnostics()[XML_PATH].iter().find(|d| d.code.as_deref() == Some("xml-value-style"))?.range;
    let actions = navigator.code_actions(&document, hint);
    let edits = &actions.first()?.edits;
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].0, XML_PATH);
    let mut fixed = text.to_owned();
    for e in &edits[0].1 {
        let (s, t) = (offset(&fixed, e.range.start).unwrap(), offset(&fixed, e.range.end).unwrap());
        fixed.replace_range(s..t, &e.text);
    }
    Some(fixed)
}

#[test]
fn value_element_is_hinted_and_rewritten_to_attribute() {
    let text = beans_xml("<bean id=\"a\" class=\"x.Y\">\n<property name=\"n\">\n  <value>5 &amp; 6</value>\n</property>\n</bean>");
    let host = MockHost::install();
    assert_eq!(xml_findings(&host, &text), [pair("xml-value-style", "value")]);
    assert_eq!(host.diagnostics()[XML_PATH][0].severity, Severity::Hint);
    let constructor = beans_xml("<bean id=\"a\" class=\"x.Y\"><constructor-arg index=\"0\"><value>7</value></constructor-arg></bean>");
    assert_eq!(xml_findings(&host, &constructor), [pair("xml-value-style", "value")]);
    let fixed = applied_xml_fix(&text).unwrap();
    assert!(fixed.contains("<property name=\"n\" value=\"5 &amp; 6\"/>\n</bean>"), "{fixed}");
    let els = spring_text::xml::elements(&fixed);
    let property = els.iter().find(|e| e.name == "property").unwrap();
    assert_eq!(property.value("value"), Some("5 &amp; 6"));
    assert!(els.iter().all(|e| e.name != "value"));
}

#[test]
fn ref_element_is_hinted_and_rewritten_to_attribute() {
    let text = beans_xml("<bean id=\"a\" class=\"x.Y\">\n<property name=\"n\"><ref bean=\"other\"/></property>\n</bean>");
    let host = MockHost::install();
    assert_eq!(xml_findings(&host, &text), [pair("xml-value-style", "ref")]);
    let fixed = applied_xml_fix(&text).unwrap();
    assert!(fixed.contains("<property name=\"n\" ref=\"other\"/>\n</bean>"), "{fixed}");
    let els = spring_text::xml::elements(&fixed);
    let property = els.iter().find(|e| e.name == "property").unwrap();
    assert_eq!(property.value("ref"), Some("other"));
    assert!(els.iter().all(|e| e.name != "ref"));
    assert!(els.iter().any(|e| e.name == "bean"));
}

#[test]
fn value_style_is_quiet_for_library_or_dynamic_name() {
    let host = MockHost::install();
    let quiet = |body: &str| {
        let found = xml_findings(&host, &beans_xml(&format!("<bean id=\"a\" class=\"x.Y\">{body}</bean>")));
        assert!(found.iter().all(|(code, _)| code != "xml-value-style"), "{body}: {found:?}");
    };
    quiet("<property name=\"n\"><value>one\ntwo</value></property>");
    quiet("<property name=\"n\"><value><![CDATA[a<b]]></value></property>");
    quiet("<property name=\"n\"><value type=\"java.lang.String\">v</value></property>");
    quiet("<property name=\"n\"><value>say \"hi\"</value></property>");
    quiet("<property name=\"n\"><value>a</value><value>b</value></property>");
    quiet("<property name=\"n\"><value>a</value><description>d</description></property>");
    quiet("<property name=\"n\"><ref local=\"o\"/></property>");
    quiet("<property name=\"n\"><ref bean=\"o\" local=\"o\"/></property>");
    quiet("<property name=\"n\"><ref bean=\"${o}\"/></property>");
    quiet("<property name=\"n\"><value>${v}</value></property>");
    quiet("<property name=\"n\" value=\"x\"><value>a</value></property>");
    quiet("<property name=\"n\"/>");
    quiet("<property name=\"n\"><!-- keep --><value>a</value></property>");
    let other_ns = "<x:bean xmlns:x=\"urn:other\"><x:property name=\"n\"><x:value>a</x:value></x:property></x:bean>";
    assert!(xml_findings(&host, other_ns).is_empty());
    host.set_setting("springBeans.checkXml", "false");
    let off = beans_xml("<bean id=\"a\" class=\"x.Y\"><property name=\"n\"><value>a</value></property></bean>");
    assert!(xml_findings(&host, &off).is_empty());
}

#[test]
fn rewrite_keeps_multibyte_text() {
    let text = beans_xml(
        "<!-- \u{1F600} --><bean id=\"a\" class=\"x.Y\">\n<property name=\"n\"><value>caf\u{e9} \u{1F600}</value></property>\n</bean>",
    )
    .replace('\n', "\r\n");
    let host = MockHost::install();
    xml_findings(&host, &text);
    let hint = host.diagnostics()[XML_PATH][0].range;
    assert_eq!((hint.start.line, hint.start.character), (3, 20));
    let fixed = applied_xml_fix(&text).unwrap();
    assert!(fixed.contains("<property name=\"n\" value=\"caf\u{e9} \u{1F600}\"/>\r\n</bean>"), "{fixed:?}");
    let els = spring_text::xml::elements(&fixed);
    assert_eq!(els.iter().find(|e| e.name == "property").unwrap().value("value"), Some("caf\u{e9} \u{1F600}"));
}

#[test]
fn code_action_applies_the_edit_to_the_open_file() {
    let text = beans_xml("<bean id=\"a\" class=\"x.Y\"><property name=\"n\"><value>v</value></property></bean>");
    let host = MockHost::install();
    let document = doc(XML_PATH, &text);
    let mut navigator = warmed_navigator();
    navigator.checked_markers(&document);
    let hint = host.diagnostics()[XML_PATH][0].range;
    let actions = navigator.code_actions(&document, hint);
    assert_eq!(actions.len(), 1);
    assert_eq!(actions[0].title, "Use the value attribute");
    assert_eq!(actions[0].edits.len(), 1);
    assert_eq!(actions[0].edits[0].0, XML_PATH);
    let elsewhere = Range::new(Position::new(0, 1), Position::new(0, 1));
    assert!(navigator.code_actions(&document, elsewhere).is_empty());

    let reference = beans_xml("<bean id=\"a\" class=\"x.Y\"><property name=\"n\"><ref bean=\"o\"/></property></bean>");
    let document = doc(XML_PATH, &reference);
    navigator.checked_markers(&document);
    let hint = host.diagnostics()[XML_PATH][0].range;
    assert_eq!(navigator.code_actions(&document, hint)[0].title, "Use the ref attribute");
}

fn apply_edits(text: &str, edits: &[TextEdit]) -> String {
    let mut fixed = text.to_owned();
    for e in edits {
        let (s, t) = (offset(&fixed, e.range.start).unwrap(), offset(&fixed, e.range.end).unwrap());
        fixed.replace_range(s..t, &e.text);
    }
    fixed
}

fn caret_in(text: &str, needle: &str) -> usize {
    text.find(needle).unwrap() + 2
}

const P_NS: &str = "http://www.springframework.org/schema/p";

#[test]
fn p_namespace_moves_value_and_adds_declaration() {
    let text =
        beans_xml("<bean id=\"a\" class=\"x.Y\">\n  <property name=\"n\" value=\"5\"/>\n  <property name=\"m\" value=\"6\"/>\n</bean>");
    let edits = xml_actions::use_p_namespace(&text, caret_in(&text, "<property name=\"n\"")).unwrap();
    let fixed = apply_edits(&text, &edits);
    assert!(fixed.contains("class=\"x.Y\" p:n=\"5\">\n  <property name=\"m\" value=\"6\"/>\n</bean>"), "{fixed}");
    let els = xml::elements(&fixed);
    assert_eq!(els[0].value("xmlns:p"), Some(P_NS));
    assert_eq!(els.iter().find(|e| e.name == "bean").unwrap().value("p:n"), Some("5"));
    assert_eq!(els.iter().filter(|e| e.name == "property").count(), 1);
    assert_eq!(xml::elements(&fixed).len(), 3);
}

#[test]
fn p_namespace_reuses_bound_prefix() {
    let text = format!(
        "<beans xmlns=\"http://www.springframework.org/schema/beans\" xmlns:q='{P_NS}'>\n<bean id='a' class='x.Y'><property name='n' value='say \"hi\"'/></bean>\n</beans>"
    );
    let edits = xml_actions::use_p_namespace(&text, caret_in(&text, "<property")).unwrap();
    let fixed = apply_edits(&text, &edits);
    let els = xml::elements(&fixed);
    assert_eq!(els.iter().find(|e| e.name == "bean").unwrap().value("q:n"), Some("say \"hi\""));
    assert!(els[0].attrs.iter().all(|a| a.name != "xmlns:p"));
    assert!(els.iter().all(|e| e.name != "property"));
}

#[test]
fn p_namespace_for_ref_uses_ref_suffix() {
    let text = beans_xml("<bean id=\"a\" class=\"x.Y\"><property name=\"n\" ref=\"b\"/></bean>");
    let edits = xml_actions::use_p_namespace(&text, caret_in(&text, "<property")).unwrap();
    let els = xml::elements(&apply_edits(&text, &edits));
    assert_eq!(els.iter().find(|e| e.name == "bean").unwrap().value("p:n-ref"), Some("b"));
}

#[test]
fn p_namespace_is_quiet_for_library_or_dynamic_name() {
    let quiet = |text: &str| assert!(xml_actions::use_p_namespace(text, caret_in(text, "<property")).is_none(), "{text}");
    quiet(&beans_xml("<bean id=\"a\" class=\"x.Y\"><property name=\"my.n\" value=\"5\"/></bean>"));
    quiet(&beans_xml("<bean id=\"a\" class=\"x.Y\"><property name=\"my-n\" value=\"5\"/></bean>"));
    quiet(&beans_xml("<bean id=\"a\" class=\"x.Y\"><property name=\"${n}\" value=\"5\"/></bean>"));
    quiet(&beans_xml("<bean id=\"a\" class=\"x.Y\"><property name=\"n\" value=\"5\" ref=\"b\"/></bean>"));
    quiet(&beans_xml("<bean id=\"a\" class=\"x.Y\"><property name=\"n\"><value>5</value></property></bean>"));
    quiet(&format!(
        "<beans xmlns=\"http://www.springframework.org/schema/beans\" xmlns:p=\"{P_NS}\"><bean id=\"a\" class=\"x.Y\" p:n=\"1\"><property name=\"n\" value=\"5\"/></bean></beans>"
    ));
    quiet(
        "<beans xmlns=\"http://www.springframework.org/schema/beans\" xmlns:p=\"urn:other\"><bean id=\"a\" class=\"x.Y\"><property name=\"n\" value=\"5\"/></bean></beans>",
    );
    quiet(
        "<beans xmlns=\"http://www.springframework.org/schema/beans\"><x:bean xmlns:x=\"urn:o\"><property name=\"n\" value=\"5\"/></x:bean></beans>",
    );
    quiet("<project><bean><property name=\"n\" value=\"5\"/></bean></project>");
    let text = beans_xml("<bean id=\"a\" class=\"x.Y\"><property name=\"n\" value=\"5\"/></bean>");
    assert!(xml_actions::use_p_namespace(&text, 0).is_none());
}

const INNER: &str = "<bean id=\"a\" class=\"x.Y\">\n  <property name=\"dao\">\n    <bean class=\"com.acme.UserDao\">\n      <property name=\"k\" value=\"1\"/>\n    </bean>\n  </property>\n</bean>";

#[test]
fn introduce_bean_moves_inner_bean_and_references_it() {
    let text = beans_xml(INNER);
    let edits = xml_actions::introduce_bean(&text, caret_in(&text, "<bean class"), BTreeSet::new).unwrap();
    let fixed = apply_edits(&text, &edits);
    let els = xml::elements(&fixed);
    let names: Vec<_> = els.iter().map(|e| (e.name.as_str(), e.parent)).collect();
    assert_eq!(names, [("beans", None), ("bean", Some(0)), ("property", Some(1)), ("bean", Some(0)), ("property", Some(3))]);
    assert_eq!(els[2].value("ref"), Some("userDao"));
    assert_eq!(els[3].value("id"), Some("userDao"));
    assert_eq!(els[3].value("class"), Some("com.acme.UserDao"));
    assert!(
        fixed
            .contains("</bean>\n<bean id=\"userDao\" class=\"com.acme.UserDao\">\n  <property name=\"k\" value=\"1\"/>\n</bean>\n</beans>"),
        "{fixed}"
    );

    let list = beans_xml("<bean id=\"a\" class=\"x.Y\"><property name=\"l\"><list><bean class=\"p.Q$R\"/></list></property></bean>");
    let fixed = apply_edits(&list, &xml_actions::introduce_bean(&list, caret_in(&list, "<bean class"), BTreeSet::new).unwrap());
    let els = xml::elements(&fixed);
    let reference = els.iter().find(|e| e.name == "ref").unwrap();
    assert_eq!(reference.value("bean"), Some("r"));
    assert_eq!(els[reference.parent.unwrap()].name, "list");
    assert_eq!(els.iter().find(|e| e.value("id") == Some("r")).unwrap().parent, Some(0));
}

#[test]
fn introduce_bean_picks_an_unused_id() {
    let text = beans_xml(
        "<bean id=\"userDao\" class=\"a.B\"/><bean name=\"x, userDao2\" class=\"a.C\"/>\n<alias name=\"userDao\" alias=\"userDao3\"/>\n<bean id=\"a\" class=\"x.Y\"><constructor-arg><bean class=\"com.acme.UserDao\"/></constructor-arg></bean>",
    );
    let fixed = apply_edits(&text, &xml_actions::introduce_bean(&text, caret_in(&text, "<bean class"), BTreeSet::new).unwrap());
    let els = xml::elements(&fixed);
    assert!(els.iter().any(|e| e.name == "bean" && e.value("id") == Some("userDao4")), "{fixed}");
    let arg = els.iter().find(|e| e.name == "constructor-arg").unwrap();
    assert_eq!(arg.value("ref"), Some("userDao4"));
}

#[test]
fn introduce_bean_is_quiet_when_bean_has_id() {
    let quiet =
        |text: &str| assert!(xml_actions::introduce_bean(text, text.rfind("<bean").unwrap() + 2, BTreeSet::new).is_none(), "{text}");
    quiet(&beans_xml("<bean id=\"a\" class=\"x.Y\"><property name=\"d\"><bean class=\"c.D\" id=\"d\"/></property></bean>"));
    let top = beans_xml("<bean class=\"c.D\"/>");
    assert!(xml_actions::introduce_bean(&top, caret_in(&top, "<bean class"), BTreeSet::new).is_none());
    quiet(&beans_xml("<bean id=\"a\"><property name=\"d\"><bean class=\"${c}\"/></property></bean>"));
    quiet(&beans_xml("<bean id=\"a\"><property name=\"d\"><bean parent=\"p\"/></property></bean>"));
    quiet(&beans_xml("<bean id=\"a\"><property name=\"d\"><bean class=\"c.D\">"));
}

const DTD: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE beans PUBLIC \"-//SPRING//DTD BEAN 2.0//EN\" \"https://www.springframework.org/dtd/spring-beans-2.0.dtd\">\n<beans default-lazy-init=\"true\">\n  <bean id=\"a\" class=\"x.Y\"/>\n</beans>\n";

#[test]
fn update_schema_replaces_doctype() {
    let fixed = apply_edits(DTD, &xml_actions::update_schema(DTD).unwrap());
    assert!(!fixed.contains("DOCTYPE"), "{fixed}");
    assert!(fixed.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<beans xmlns="), "{fixed}");
    let els = xml::elements(&fixed);
    assert_eq!(xml::namespace(&els, 0), Some("http://www.springframework.org/schema/beans"));
    assert_eq!(
        els[0].value("xsi:schemaLocation"),
        Some("http://www.springframework.org/schema/beans https://www.springframework.org/schema/beans/spring-beans.xsd")
    );
    assert_eq!(els[0].value("xmlns:xsi"), Some("http://www.w3.org/2001/XMLSchema-instance"));
    assert_eq!(els[0].value("default-lazy-init"), Some("true"));
    assert_eq!(els.len(), 2);
    assert_eq!(xml::namespace(&els, 1), Some("http://www.springframework.org/schema/beans"));
}

#[test]
fn update_schema_is_quiet_for_schema_files() {
    assert!(xml_actions::update_schema(&beans_xml("<bean id=\"a\" class=\"x.Y\"/>")).is_none());
    let both = DTD.replace("<beans ", "<beans xmlns=\"http://www.springframework.org/schema/beans\" ");
    assert!(xml_actions::update_schema(&both).is_none());
    assert!(
        xml_actions::update_schema(&DTD.replace("beans PUBLIC", "html PUBLIC").replace("<beans ", "<html ").replace("</beans>", "</html>"))
            .is_none()
    );
    assert!(xml_actions::update_schema("<project><!DOCTYPE beans></project>").is_none());
}

#[test]
fn intentions_keep_multibyte_text_and_crlf() {
    let crlf = |s: &str| s.replace('\n', "\r\n");
    let text = crlf(&beans_xml(
        "<!-- \u{1F600} -->\n<bean id=\"a\" class=\"x.Y\">\n  <property name=\"n\" value=\"caf\u{e9} \u{1F600}\"/>\n  <property name=\"dao\">\n    <bean class=\"com.acme.UserDao\">\n      <property name=\"k\" value=\"\u{e9}\"/>\n    </bean>\n  </property>\n</bean>",
    ));
    let fixed = apply_edits(&text, &xml_actions::use_p_namespace(&text, caret_in(&text, "<property name=\"n\"")).unwrap());
    let els = xml::elements(&fixed);
    assert_eq!(els.iter().find(|e| e.name == "bean").unwrap().value("p:n"), Some("caf\u{e9} \u{1F600}"));
    assert!(!fixed.contains("\r\n\r\n") && !fixed.replace("\r\n", "").contains('\n'), "{fixed:?}");

    let fixed = apply_edits(&text, &xml_actions::introduce_bean(&text, caret_in(&text, "<bean class"), BTreeSet::new).unwrap());
    let els = xml::elements(&fixed);
    assert_eq!(els.iter().find(|e| e.name == "property" && e.value("name") == Some("dao")).unwrap().value("ref"), Some("userDao"));
    assert_eq!(els.iter().find(|e| e.value("id") == Some("userDao")).unwrap().parent, Some(0));
    assert!(
        fixed.contains("\r\n<bean id=\"userDao\" class=\"com.acme.UserDao\">\r\n  <property name=\"k\" value=\"\u{e9}\"/>\r\n</bean>\r\n"),
        "{fixed:?}"
    );
    assert!(!fixed.replace("\r\n", "").contains('\n'));

    let dtd = crlf(&DTD.replace("x.Y", "caf\u{e9}\u{1F600}"));
    let fixed = apply_edits(&dtd, &xml_actions::update_schema(&dtd).unwrap());
    assert!(fixed.contains("<beans xmlns=\"http://www.springframework.org/schema/beans\"\r\n       xmlns:xsi"), "{fixed:?}");
    assert!(!fixed.replace("\r\n", "").contains('\n'));
    assert_eq!(xml::elements(&fixed)[1].value("class"), Some("caf\u{e9}\u{1F600}"));
}

#[test]
fn xml_intentions_are_offered_as_code_actions() {
    let _host = MockHost::install();
    let text = beans_xml(
        "<bean id=\"a\" class=\"x.Y\"><property name=\"n\" value=\"5\"/><property name=\"d\"><bean class=\"c.D\"/></property></bean>",
    );
    let document = doc(XML_PATH, &text);
    let mut navigator = warmed_navigator();
    let at = |needle: &str| {
        let p = position(&text, caret_in(&text, needle));
        Range::new(p, p)
    };
    let mut titles = |range| navigator.code_actions(&document, range).into_iter().map(|a| a.title).collect::<Vec<_>>();
    assert_eq!(titles(at("<property name=\"n\"")), ["Use p-namespace"]);
    assert!(titles(at("<bean class")).is_empty());
    assert_eq!(navigator.file_actions(&document, at("<bean class"))[0].title, "Introduce bean…");
    let dtd = doc(XML_PATH, DTD);
    let first = Range::new(Position::new(0, 0), Position::new(0, 0));
    assert_eq!(navigator.code_actions(&dtd, first).iter().map(|a| a.title.as_str()).collect::<Vec<_>>(), ["Update XML schema"]);
}

#[test]
fn style_fix_handles_unusual_parent_tags() {
    let fix = |body: &str| {
        let text = beans_xml(body);
        let els = xml::elements(&text);
        let property = els.iter().find(|e| e.local() == "property").unwrap();
        apply_edits(&text, &[xml_checks::attribute_style_edit(&text, property, &els).unwrap()])
    };
    let fixed = fix("<bean class=\"x.Y\"><property name=\"a&gt;b\" title=\"x>y\"><value>v</value></property></bean>");
    let els = xml::elements(&fixed);
    assert_eq!(els.iter().find(|e| e.name == "property").unwrap().value("value"), Some("v"));
    assert_eq!(els.iter().find(|e| e.name == "property").unwrap().value("title"), Some("x>y"));
    assert!(els.iter().all(|e| e.name != "value"));

    let fixed = fix(
        "<b:bean xmlns:b=\"http://www.springframework.org/schema/beans\"><b:property name=\"n\"><b:ref bean=\"o\"/></b:property></b:bean>",
    );
    let els = xml::elements(&fixed);
    assert_eq!(els.iter().find(|e| e.name == "b:property").unwrap().value("ref"), Some("o"));
    assert!(els.iter().all(|e| e.name != "b:ref"));

    let fixed = fix("<bean class=\"x.Y\"><property name='n'><value>v</value></property></bean>");
    let els = xml::elements(&fixed);
    let property = els.iter().find(|e| e.name == "property").unwrap();
    assert_eq!((property.value("name"), property.value("value")), (Some("n"), Some("v")));
    assert!(els.iter().all(|e| e.name != "value"));

    let text = "<beans xmlns=\"http://www.springframework.org/schema/beans\"><bean><property name=\"n\"";
    let els = xml::elements(text);
    assert!(xml_checks::attribute_style_edit(text, &els[2], &els).is_none());
}

#[test]
fn autowired_static_field_is_reported() {
    let text = "@org.springframework.stereotype.Service\nclass A {\n    @org.springframework.beans.factory.annotation.Autowired\n    private static Repo repo;\n}\n";
    assert_eq!(found(text), [pair("static-injection", "repo")]);
    let constant = "@org.springframework.stereotype.Service\nclass A {\n    @org.springframework.beans.factory.annotation.Value(\"${a}\")\n    static final String NAME = null;\n}\n";
    assert_eq!(found(constant), [pair("static-injection", "NAME")]);
}

#[test]
fn value_spel_on_static_field_reports_the_spel_code_only() {
    let text = "@org.springframework.stereotype.Service\nclass A {\n    @org.springframework.beans.factory.annotation.Value(\"#{systemProperties['x']}\")\n    static String x;\n}\n";
    assert_eq!(found(text), [pair("static-spel-injection", "x")]);
}

#[test]
fn inject_on_static_method_is_reported() {
    let text = "import jakarta.inject.Inject;\n\n@org.springframework.stereotype.Service\nclass A {\n    @Inject\n    static void setRepo(Repo r) {}\n}\n";
    assert_eq!(found(text), [pair("static-injection", "setRepo")]);
}

#[test]
fn two_required_autowired_constructors_are_reported() {
    let text = "@org.springframework.stereotype.Service\nclass A {\n    @org.springframework.beans.factory.annotation.Autowired\n    A(X x) {}\n    @org.springframework.beans.factory.annotation.Autowired(required = true)\n    A(Y y) {}\n}\n";
    assert_eq!(found(text), [pair("autowired-constructors", "A")]);
    let host = MockHost::install();
    let document = java("Subject.java", text);
    warmed_navigator().checked_markers(&document);
    assert_eq!(host.diagnostics()[&document.path][0].range.start.line, 5);
}

#[test]
fn required_next_to_optional_constructor_is_reported() {
    let text = "@org.springframework.stereotype.Service\nclass A {\n    @org.springframework.beans.factory.annotation.Autowired\n    A(X x) {}\n    @org.springframework.beans.factory.annotation.Autowired(required = false)\n    A(Y y) {}\n}\n";
    assert_eq!(found(text), [pair("autowired-constructors", "A")]);
}

#[test]
fn optional_autowired_constructors_are_quiet() {
    let text = "@org.springframework.stereotype.Service\nclass A {\n    @org.springframework.beans.factory.annotation.Autowired(required = false)\n    A(X x) {}\n    @org.springframework.beans.factory.annotation.Autowired(required = false)\n    A(Y y) {}\n}\n";
    assert!(found(text).is_empty());
}

#[test]
fn injection_checks_are_quiet_for_library_or_dynamic_name() {
    let foreign = "import com.acme.Autowired;\n\nclass A {\n    @Autowired\n    static Repo repo;\n    @Autowired\n    A(X x) {}\n    @Autowired\n    A(Y y) {}\n}\n";
    assert!(found(foreign).is_empty());
    let dynamic = "class A {\n    @org.springframework.beans.factory.annotation.Autowired(required = FLAG)\n    A(X x) {}\n    @org.springframework.beans.factory.annotation.Autowired\n    A(Y y) {}\n}\n";
    assert!(found(dynamic).is_empty());
    let lombok = "@RequiredArgsConstructor\n@org.springframework.stereotype.Service\nclass A {\n    private final Repo repo;\n}\n";
    assert!(found(lombok).is_empty());
    let record = "record A(@Autowired static int x) {\n    @Autowired static Repo repo;\n}\n";
    assert!(found(record).is_empty());
    let instance = "@org.springframework.stereotype.Service\nclass A {\n    @org.springframework.beans.factory.annotation.Autowired\n    private Repo repo;\n    @org.springframework.beans.factory.annotation.Autowired\n    void set(Repo r) {}\n    @org.springframework.beans.factory.annotation.Value(\"#{x}\")\n    String y;\n}\n";
    assert!(found(instance).is_empty());
}

#[test]
fn switched_off_injection_group_is_quiet() {
    let host = MockHost::install();
    host.set_setting("springBeans.checkInjectionAnnotations", "false");
    let text = "@org.springframework.stereotype.Service\nclass A {\n    @org.springframework.beans.factory.annotation.Autowired\n    static Repo repo;\n}\n";
    assert!(findings(&host, &mut warmed_navigator(), "Subject.java", text).is_empty());
}

#[test]
fn injection_ranges_use_utf16_columns_with_multibyte_text() {
    let text = "@org.springframework.stereotype.Service\nclass A {\n    @Qualifier(\"é\") @org.springframework.beans.factory.annotation.Autowired static Repo repo;\n}\n";
    let host = MockHost::install();
    let document = java("Subject.java", text);
    warmed_navigator().checked_markers(&document);
    let diagnostics = &host.diagnostics()[&document.path];
    let diagnostic = diagnostics.iter().find(|d| d.code.as_deref() == Some("static-injection")).unwrap();
    assert_eq!((diagnostic.range.start.line, diagnostic.range.start.character), (2, 88));
}

#[test]
fn profile_expression_problem_cases() {
    for bad in ["", "  ", "a & b | c", "a | b & c", "!", "(a & b", "a & b)", "!!", "()"] {
        assert!(annotation_checks::profile_expression_problem(bad).is_some(), "{bad:?} should report");
    }
    for good in [
        "a | b",
        "!dev",
        "(a & b) | c",
        "a",
        " a & b ",
        "!(a | b)",
        "a & (b | c)",
        "((a))",
        "a&b&c",
        "!a & !b",
        "a &",
        "a |",
        "a && b",
        "a !b",
        "(a)(b)",
    ] {
        assert_eq!(annotation_checks::profile_expression_problem(good), None, "{good:?} should be quiet");
    }
}

#[test]
fn empty_profile_is_reported() {
    let text = "@org.springframework.context.annotation.Profile(\"\")\n@org.springframework.stereotype.Service\nclass A {\n    @org.springframework.context.annotation.Bean\n    @org.springframework.context.annotation.Profile(\" \")\n    B b() { return null; }\n}\n";
    assert_eq!(found(text), [pair("profile-expression", ""), pair("profile-expression", " ")]);
}

#[test]
fn mixed_operators_are_reported() {
    let text = "@org.springframework.stereotype.Service\nclass A {\n    @org.springframework.context.annotation.Profile({\"x\", \"a & b | c\", \"!\"})\n    void m() {}\n}\n";
    assert_eq!(found(text), [pair("profile-expression", "a & b | c"), pair("profile-expression", "!")]);
    let valid = "@org.springframework.context.annotation.Profile({\"a\", \"!b\", \"(a & b) | c\"})\n@org.springframework.stereotype.Service\nclass A {}\n";
    assert!(found(valid).is_empty());
}

#[test]
fn profile_checks_are_quiet_for_library_or_dynamic_name() {
    let constant = "@org.springframework.context.annotation.Profile(NAMES)\n@org.springframework.stereotype.Service\nclass A {}\n";
    assert!(found(constant).is_empty());
    let concatenated = "@org.springframework.context.annotation.Profile(\"a &\" + X)\n@org.springframework.stereotype.Service\nclass A {}\n";
    assert!(found(concatenated).is_empty());
    let placeholder = "@org.springframework.context.annotation.Profile(\"${p} & !\")\n@org.springframework.stereotype.Service\nclass A {}\n";
    assert!(found(placeholder).is_empty());
    let foreign = "import com.acme.Profile;\n\n@Profile(\"\")\n@org.springframework.stereotype.Service\nclass A {}\n";
    assert!(found(foreign).is_empty());
}

#[test]
fn profile_ranges_use_utf16_columns_with_multibyte_text() {
    let text = "@org.springframework.stereotype.Service\nclass A {\n    @Named(\"é\") @org.springframework.context.annotation.Profile(\"a & b | c\") void m() {}\n}\n";
    let host = MockHost::install();
    let document = java("Subject.java", text);
    warmed_navigator().checked_markers(&document);
    let diagnostic = host.diagnostics()[&document.path].iter().find(|d| d.code.as_deref() == Some("profile-expression")).unwrap().clone();
    assert_eq!((diagnostic.range.start.line, diagnostic.range.start.character), (2, 65));
}

#[test]
fn async_returning_string_is_reported() {
    let text = "@org.springframework.stereotype.Service\nclass A {\n    @Async\n    public String a() { return null; }\n    @Async\n    int b() { return 0; }\n    @Async\n    public List<String> c() { return null; }\n}\n";
    assert_eq!(found(text), [pair("async-return-type", "String"), pair("async-return-type", "int"), pair("async-return-type", "List")]);
}

#[test]
fn async_returning_future_types_is_quiet() {
    let text = "@org.springframework.stereotype.Service\nclass A {\n    @Async\n    void a() {}\n    @Async\n    Future<String> b() { return null; }\n    @Async\n    CompletableFuture<String> c() { return null; }\n    @Async\n    ListenableFuture<String> d() { return null; }\n    @Async\n    CompletionStage<String> e() { return null; }\n}\n";
    assert!(found(text).is_empty());
}

#[test]
fn async_check_is_quiet_for_library_or_dynamic_name() {
    let project = "@org.springframework.stereotype.Service\nclass A {\n    @Async\n    Result a() { return null; }\n}\n";
    assert!(found(project).is_empty());
    let foreign = "import com.acme.Async;\n\n@org.springframework.stereotype.Service\nclass A {\n    @Async\n    String a() { return null; }\n}\n";
    assert!(found(foreign).is_empty());
    let plain = "@org.springframework.stereotype.Service\nclass A {\n    String a() { return null; }\n}\n";
    assert!(found(plain).is_empty());
}

#[test]
fn switched_off_profile_and_async_groups_are_quiet() {
    let text = "@org.springframework.context.annotation.Profile(\"\")\n@org.springframework.stereotype.Service\nclass A {\n    @Async\n    String a() { return null; }\n}\n";
    let host = MockHost::install();
    host.set_setting("springBeans.checkProfiles", "false");
    assert_eq!(findings(&host, &mut warmed_navigator(), "Subject.java", text), [pair("async-return-type", "String")]);
    host.set_setting("springBeans.checkAsync", "false");
    assert!(findings(&host, &mut warmed_navigator(), "Subject.java", text).is_empty());
}

#[test]
fn async_ranges_use_utf16_columns_with_multibyte_text() {
    let text = "@org.springframework.stereotype.Service\nclass A {\n    @Named(\"é\") @Async String a() { return null; }\n}\n";
    let host = MockHost::install();
    let document = java("Subject.java", text);
    warmed_navigator().checked_markers(&document);
    let diagnostic = host.diagnostics()[&document.path].iter().find(|d| d.code.as_deref() == Some("async-return-type")).unwrap().clone();
    assert_eq!((diagnostic.range.start.line, diagnostic.range.start.character), (2, 23));
}

#[test]
fn cacheable_without_cache_name_is_reported() {
    let text = "@org.springframework.stereotype.Service\nclass A {\n    @Cacheable\n    String a() { return null; }\n    @CachePut(key = \"#id\")\n    String b() { return null; }\n    @CacheEvict(allEntries = true)\n    void c() {}\n    @Cacheable(\"x\")\n    String d() { return null; }\n}\n";
    assert_eq!(
        found(text),
        [
            pair("cache-names-missing", "@Cacheable"),
            pair("cache-names-missing", "@CachePut(key = \"#id\")"),
            pair("cache-names-missing", "@CacheEvict(allEntries = true)"),
        ]
    );
}

#[test]
fn cache_config_on_class_supplies_the_name() {
    let text = "@CacheConfig(cacheNames = \"x\")\n@org.springframework.stereotype.Service\nclass A {\n    @Cacheable\n    String a() { return null; }\n}\n";
    assert!(found(text).is_empty());
}

#[test]
fn cache_annotation_on_interface_method_is_reported() {
    let text = "@Cacheable(\"t\")\ninterface A {\n    @Cacheable(\"x\")\n    String a();\n}\n";
    assert_eq!(
        found(text),
        [pair("cache-annotation-on-interface", "@Cacheable(\"t\")"), pair("cache-annotation-on-interface", "@Cacheable(\"x\")")]
    );
    let host = MockHost::install();
    let document = java("Subject.java", text);
    warmed_navigator().checked_markers(&document);
    let d = &host.diagnostics()[&document.path][0];
    assert_eq!(d.severity, Severity::Hint);
    assert!(d.message.contains("AspectJ") && d.message.contains("concrete class"), "{}", d.message);
}

#[test]
fn cacheable_and_cache_put_together_are_reported() {
    let text = "@org.springframework.stereotype.Service\nclass A {\n    @Cacheable(\"x\") @CachePut(\"x\")\n    String a() { return null; }\n}\n";
    assert_eq!(found(text), [pair("cacheable-and-cache-put", "@CachePut(\"x\")")]);
}

#[test]
fn cache_checks_are_quiet_for_library_or_dynamic_name() {
    let constant = "@org.springframework.stereotype.Service\nclass A {\n    @Cacheable(NAME)\n    String a() { return null; }\n    @Cacheable(cacheNames = PREFIX + \"x\")\n    String b() { return null; }\n}\n";
    assert!(found(constant).is_empty());
    let foreign = "import com.acme.Cacheable;\n\n@org.springframework.stereotype.Service\nclass A {\n    @Cacheable\n    String a() { return null; }\n}\n\ninterface B {\n    @Cacheable\n    String a();\n}\n";
    assert!(found(foreign).is_empty());
    let caching =
        "@org.springframework.stereotype.Service\nclass A {\n    @Caching(cacheable = @Cacheable(\"x\"), put = @CachePut(\"x\"))\n    String a() { return null; }\n}\n";
    assert!(found(caching).is_empty());
    let dynamic_config = "@CacheConfig(cacheNames = NAMES)\n@org.springframework.stereotype.Service\nclass A {\n    @Cacheable\n    String a() { return null; }\n}\n";
    assert!(found(dynamic_config).is_empty());
    let spring = "import org.springframework.cache.annotation.Cacheable;\n\n@org.springframework.stereotype.Service\nclass A {\n    @Cacheable\n    String a() { return null; }\n}\n";
    assert_eq!(found(spring), [pair("cache-names-missing", "@Cacheable")]);
}

#[test]
fn switched_off_cache_group_is_quiet() {
    let text = "@org.springframework.stereotype.Service\nclass A {\n    @Cacheable\n    String a() { return null; }\n}\n";
    let host = MockHost::install();
    assert_eq!(findings(&host, &mut warmed_navigator(), "Subject.java", text).len(), 1);
    host.set_setting("springBeans.checkCache", "false");
    assert!(findings(&host, &mut warmed_navigator(), "Subject.java", text).is_empty());
}

#[test]
fn cache_ranges_use_utf16_columns_with_multibyte_text() {
    let text = "@org.springframework.stereotype.Service\nclass A {\n    @Named(\"é\") @Cacheable String a() { return null; }\n}\n";
    let host = MockHost::install();
    let document = java("Subject.java", text);
    warmed_navigator().checked_markers(&document);
    let diagnostic = host.diagnostics()[&document.path].iter().find(|d| d.code.as_deref() == Some("cache-names-missing")).unwrap().clone();
    assert_eq!((diagnostic.range.start.line, diagnostic.range.start.character), (2, 16));
}

const KT_TX: &str = "@org.springframework.stereotype.Service\nclass Billing {\n    @Transactional\n    fun charge() {}\n}\n";
const KTS_BUILD: &str = "plugins {\n    kotlin(\"jvm\") version \"1.9.24\"\n    id(\"org.springframework.boot\") version \"3.3.0\"\n}\n";
const KT_POM: &str = "<project><plugin>kotlin-maven-plugin</plugin></project>";
const KT_PATH: &str = "/project/src/main/kotlin/Billing.kt";

fn kt_host(build: Option<(&str, &str)>) -> MockHost {
    let host = MockHost::install();
    if let Some((name, text)) = build {
        host.add_file(name, text);
    }
    host
}

fn kt_findings(host: &MockHost, text: &str) -> Vec<(String, String)> {
    let document = doc(KT_PATH, text);
    warmed_navigator().checked_markers(&document);
    host.diagnostics()
        .get(&document.path)
        .into_iter()
        .flatten()
        .filter(|d| d.code.as_deref() == Some("kotlin-spring-plugin"))
        .map(|d| {
            let (from, to) = (offset(text, d.range.start).unwrap(), offset(text, d.range.end).unwrap());
            (d.code.clone().unwrap(), text[from..to].to_owned())
        })
        .collect()
}

#[test]
fn final_transactional_kotlin_class_without_plugin_is_reported() {
    let host = kt_host(Some(("build.gradle.kts", KTS_BUILD)));
    assert_eq!(kt_findings(&host, KT_TX), [pair("kotlin-spring-plugin", "Billing")]);
    let on_class = "@Transactional\nclass Billing\n";
    assert_eq!(kt_findings(&host, on_class), [pair("kotlin-spring-plugin", "Billing")]);
}

#[test]
fn configuration_class_without_plugin_is_reported() {
    let host = kt_host(Some(("pom.xml", KT_POM)));
    assert_eq!(kt_findings(&host, "@org.springframework.context.annotation.Configuration\nclass Config\n"), [pair("kotlin-spring-plugin", "Config")]);
    let explicit = "@org.springframework.context.annotation.Configuration(proxyBeanMethods = true)\nclass Config\n";
    assert_eq!(kt_findings(&host, explicit).len(), 1);
}

#[test]
fn plugin_in_build_file_silences_the_check() {
    for build in [
        "plugins { kotlin(\"plugin.spring\") version \"1.9.24\" }",
        "plugins { id 'org.jetbrains.kotlin.plugin.spring' version '1.9.24' }",
        "apply plugin: 'kotlin-spring'",
        "<compilerPlugins>\n  <plugin>spring</plugin>\n</compilerPlugins>",
        "compilerPlugins { \"spring\" }",
    ] {
        let host = kt_host(Some(("build.gradle.kts", &format!("kotlin(\"jvm\")\n{build}"))));
        assert!(kt_findings(&host, KT_TX).is_empty(), "{build}");
    }
    let host = kt_host(Some(("build.gradle.kts", "kotlin(\"jvm\")\ncompilerPlugins { \"jpa\" }")));
    assert_eq!(kt_findings(&host, KT_TX).len(), 1);
}

#[test]
fn open_or_abstract_class_is_quiet() {
    let host = kt_host(Some(("build.gradle.kts", KTS_BUILD)));
    for decl in ["open class", "abstract class", "sealed class", "data class", "enum class", "annotation class", "interface", "object"] {
        let text = format!("@Transactional\n{decl} Billing\n");
        assert!(kt_findings(&host, &text).is_empty(), "{decl}");
    }
    let inner = "class Outer {\n    @Transactional\n    inner class Billing\n}\n";
    assert!(kt_findings(&host, inner).is_empty());
}

#[test]
fn kotlin_spring_plugin_is_quiet_for_library_or_dynamic_name() {
    assert!(kt_findings(&kt_host(None), KT_TX).is_empty());
    let host = kt_host(Some(("build.gradle.kts", KTS_BUILD)));
    let foreign = "import com.acme.Transactional\n\nclass Billing {\n    @Transactional\n    fun charge() {}\n}\n";
    assert!(kt_findings(&host, foreign).is_empty());
    assert!(kt_findings(&host, "class Billing {\n    @Cacheable\n    fun a() {}\n}\n").len() == 1);
    assert!(kt_findings(&host, "@org.springframework.context.annotation.Configuration(proxyBeanMethods = false)\nclass Config\n").is_empty());
    assert!(kt_findings(&host, "@org.springframework.context.annotation.Configuration(proxyBeanMethods = FLAG)\nclass Config\n").is_empty());
    assert!(kt_findings(&host, "class Plain {\n    fun a() {}\n}\n").is_empty());
    let allopen = kt_host(Some(("build.gradle", "plugins { id 'org.jetbrains.kotlin.plugin.allopen' }")));
    assert!(kt_findings(&allopen, KT_TX).is_empty());
}

fn edited(text: &str, edit: &TextEdit) -> String {
    let (from, to) = (offset(text, edit.range.start).unwrap(), offset(text, edit.range.end).unwrap());
    format!("{}{}{}", &text[..from], edit.text, &text[to..])
}

#[test]
fn gradle_kts_fix_inserts_plugin_line() {
    let edit = kotlin_build::gradle_fix("/project/build.gradle.kts", KTS_BUILD).unwrap();
    assert_eq!(
        edited(KTS_BUILD, &edit),
        "plugins {\n    kotlin(\"jvm\") version \"1.9.24\"\n    id(\"org.springframework.boot\") version \"3.3.0\"\n    kotlin(\"plugin.spring\") version \"1.9.24\"\n}\n"
    );
    let host = kt_host(Some(("build.gradle.kts", KTS_BUILD)));
    let document = doc(KT_PATH, KT_TX);
    let actions = warmed_navigator().code_actions(&document, Range::new(Position::new(1, 8), Position::new(1, 8)));
    assert_eq!(actions.len(), 1);
    assert_eq!(actions[0].edits[0].0, "/project/build.gradle.kts");
    assert_eq!(edited(KTS_BUILD, &actions[0].edits[0].1[0]), edited(KTS_BUILD, &edit));
    assert!(warmed_navigator().code_actions(&document, Range::new(Position::new(3, 5), Position::new(3, 5))).is_empty());
    drop(host);
}

#[test]
fn groovy_gradle_fix_inserts_plugin_line() {
    let build = "plugins {\n  id 'org.jetbrains.kotlin.jvm' version '1.9.24'\n}\n";
    let edit = kotlin_build::gradle_fix("/project/build.gradle", build).unwrap();
    assert_eq!(
        edited(build, &edit),
        "plugins {\n  id 'org.jetbrains.kotlin.jvm' version '1.9.24'\n  id 'org.jetbrains.kotlin.plugin.spring' version '1.9.24'\n}\n"
    );
    let crlf = build.replace('\n', "\r\n");
    let edit = kotlin_build::gradle_fix("/project/build.gradle", &crlf).unwrap();
    assert!(edited(&crlf, &edit).contains("'1.9.24'\r\n  id 'org.jetbrains.kotlin.plugin.spring' version '1.9.24'\r\n}"));
}

#[test]
fn maven_build_offers_no_fix() {
    let host = kt_host(Some(("pom.xml", KT_POM)));
    let document = doc(KT_PATH, KT_TX);
    assert!(warmed_navigator().code_actions(&document, Range::new(Position::new(1, 8), Position::new(1, 8))).is_empty());
    assert_eq!(kt_findings(&host, KT_TX).len(), 1);
    assert!(kotlin_build::gradle_fix("/project/pom.xml", "<project/>").is_none());
    assert!(kotlin_build::gradle_fix("/project/build.gradle.kts", "dependencies {}\n").is_none());
    assert!(kotlin_build::gradle_fix("/project/build.gradle.kts", "plugins {\n    java\n}\n").is_none());
    assert!(kotlin_build::gradle_fix("/project/build.gradle.kts", "plugins {\n    kotlin(\"jvm\") version kv\n}\n").is_none());
    assert!(kotlin_build::gradle_fix("/project/build.gradle.kts", "plugins { kotlin(\"jvm\") version \"1.9\" }\n").is_none());
}

#[test]
fn fix_never_edits_outside_the_project() {
    // the mock refuses `..` files, so this guards the `..` segment check in build_module
    let _host = kt_host(Some(("build.gradle.kts", KTS_BUILD)));
    for path in ["/project/../outside/src/Billing.kt", "/projectile/src/Billing.kt", "/etc/Billing.kt"] {
        let document = doc(path, KT_TX);
        assert!(kotlin_build::fix_for(path).is_none(), "{path}");
        assert!(warmed_navigator().code_actions(&document, Range::new(Position::new(1, 8), Position::new(1, 8))).is_empty(), "{path}");
    }
    assert!(kotlin_build::fix_for(KT_PATH).is_some());
}

#[test]
fn switched_off_kotlin_plugin_group_is_quiet() {
    let host = kt_host(Some(("build.gradle.kts", KTS_BUILD)));
    assert_eq!(kt_findings(&host, KT_TX).len(), 1);
    host.set_setting("springBeans.checkKotlinSpringPlugin", "false");
    assert!(kt_findings(&host, KT_TX).is_empty());
    let document = doc(KT_PATH, KT_TX);
    assert!(warmed_navigator().code_actions(&document, Range::new(Position::new(1, 8), Position::new(1, 8))).is_empty());
}

#[test]
fn fixed_kotlin_file_publishes_empty_list() {
    let host = kt_host(Some(("build.gradle.kts", KTS_BUILD)));
    let mut navigator = warmed_navigator();
    navigator.checked_markers(&doc(KT_PATH, KT_TX));
    assert!(host.diagnostics()[KT_PATH].iter().any(|d| d.code.as_deref() == Some("kotlin-spring-plugin")));
    navigator.checked_markers(&doc(KT_PATH, "open class Billing\n"));
    assert!(host.diagnostics().get(KT_PATH).is_none_or(|list| list.is_empty()));
}

#[test]
fn kotlin_plugin_ranges_use_utf16_columns_with_multibyte_text() {
    let text = "@org.springframework.stereotype.Service\n@Named(\"é😀\") class Billing {\n    @Transactional fun a() {}\n}\n";
    let host = kt_host(Some(("build.gradle.kts", KTS_BUILD)));
    let document = doc(KT_PATH, text);
    warmed_navigator().checked_markers(&document);
    let d = host.diagnostics()[KT_PATH].iter().find(|d| d.code.as_deref() == Some("kotlin-spring-plugin")).unwrap().clone();
    assert_eq!((d.range.start.line, d.range.start.character, d.range.end.character), (1, 20, 27));
}

#[test]
fn kotlin_plugin_crlf_files_report_correct_lines() {
    let host = kt_host(Some(("build.gradle.kts", KTS_BUILD)));
    let text = KT_TX.replace('\n', "\r\n");
    let document = doc(KT_PATH, &text);
    warmed_navigator().checked_markers(&document);
    let d = host.diagnostics()[KT_PATH].iter().find(|d| d.code.as_deref() == Some("kotlin-spring-plugin")).unwrap().clone();
    assert_eq!((d.range.start.line, d.range.start.character), (1, 6));
}

#[test]
fn kotlin_spring_plugin_is_quiet_without_a_literal_kotlin_plugin() {
    for build in [
        "plugins {\n    alias(libs.plugins.kotlin.spring)\n    kotlin(\"jvm\") version \"1.9.24\"\n}\n",
        "plugins {\n    id(\"my.kotlin-conventions\")\n}\n",
        "plugins {\n    kotlin(\"jvm\") version \"1.9.24\" apply false\n}\n",
        "subprojects { }\n",
    ] {
        let host = kt_host(Some(("build.gradle.kts", build)));
        assert!(kt_findings(&host, KT_TX).is_empty(), "{build}");
        assert!(kotlin_build::gradle_fix("/project/build.gradle.kts", build).is_none_or(|_| build.contains("alias")));
    }
    let root_applies = kt_host(Some(("build.gradle.kts", "plugins {\n    kotlin(\"plugin.spring\") version \"1.9.24\" apply false\n}\n")));
    assert!(kt_findings(&root_applies, KT_TX).is_empty());
    let host = kt_host(Some(("pom.xml", KT_POM)));
    assert_eq!(kt_findings(&host, KT_TX).len(), 1);
}

#[test]
fn own_names_of_one_bean_are_not_duplicates() {
    assert!(aop_found("<bean id=\"a\" name=\"a\" class=\"x.Y\"/>").is_empty());
    assert!(aop_found("<bean name=\"b,b\" class=\"x.Y\"/>").is_empty());
    let found = aop_found("<bean id=\"a\" name=\"a\" class=\"x.Y\"/>\n<bean id=\"a\" class=\"x.Y\"/>");
    assert_eq!(found, [pair("duplicate-bean-name", "a")]);
    let host = MockHost::install();
    host.add_file("src/main/java/com/acme/Dispatcher.java", "@org.springframework.stereotype.Service(\"dispatcher\")\nclass Dispatcher {}\n");
    let found = xml_findings(&host, &beans_xml("<bean id=\"dispatcher\" name=\"dispatcher\" class=\"x.Y\"/>"));
    assert_eq!(found, [pair("duplicate-bean-name", "dispatcher")]);
}

#[test]
fn names_split_on_any_whitespace() {
    assert!(aop_found("<bean name=\"p,\n q\" class=\"x.Y\"/>\n<bean name=\"r,\n\ts\" class=\"x.Y\"/>").is_empty());
    assert_eq!(aop_found("<bean name=\"p,\n Q\" class=\"x.Y\"/>"), [pair("bean-name-convention", "Q")]);
    let model = parse("/project/beans.xml", &beans_xml("<bean name=\"p,\n q;\tr\" class=\"x.Y\"/>")).unwrap();
    assert_eq!((model.beans[0].name.as_str(), model.beans[0].aliases.clone()), ("p", vec!["q".to_owned(), "r".to_owned()]));
    let text = beans_xml(
        "<bean name=\"x,\n\tuserDao\" class=\"a.B\"/><bean id=\"a\" class=\"x.Y\"><constructor-arg><bean class=\"com.acme.UserDao\"/></constructor-arg></bean>",
    );
    let fixed = apply_edits(&text, &xml_actions::introduce_bean(&text, caret_in(&text, "<bean class"), BTreeSet::new).unwrap());
    assert!(xml::elements(&fixed).iter().any(|e| e.value("id") == Some("userDao2")), "{fixed}");
}

#[test]
fn placeholder_profile_keeps_xml_and_java_names_quiet() {
    let host = MockHost::install();
    host.add_file("src/main/java/com/acme/Dispatcher.java", "@org.springframework.stereotype.Service(\"dispatcher\")\nclass Dispatcher {}\n");
    for profile in ["${env}", "#{env}", "dev, ${env}"] {
        let body = format!("<beans profile=\"{profile}\"><bean id=\"dispatcher\" class=\"x.Y\"/></beans>");
        assert!(xml_findings(&host, &beans_xml(&body)).is_empty(), "{profile}");
    }
    let host = MockHost::install();
    host.add_file("src/main/resources/beans.xml", &beans_xml("<beans profile=\"${env}\"><bean id=\"dispatcher\" class=\"x.Y\"/></beans>"));
    assert!(findings(&host, &mut warmed_navigator(), "Dispatcher.java", "@org.springframework.stereotype.Service(\"dispatcher\")\nclass Dispatcher {}\n").is_empty());
}

#[test]
fn cache_name_from_a_supertype_is_quiet() {
    let extends = "@org.springframework.stereotype.Service\nclass A extends Base {\n    @Cacheable\n    String a() { return null; }\n}\n";
    assert!(found(extends).is_empty());
    let implements = "@org.springframework.stereotype.Service\nclass A implements Api {\n    @CacheEvict(allEntries = true)\n    void a() {}\n}\n";
    assert!(found(implements).is_empty());
}

#[test]
fn cacheable_on_spring_data_repository_is_quiet() {
    for parent in ["Repository<User, Long>", "CrudRepository<User, Long>", "JpaRepository<User, Long>"] {
        let text = format!("interface Users extends {parent} {{\n    @Cacheable(\"users\")\n    User findByName(String name);\n}}\n");
        assert!(found(&text).is_empty(), "{parent}");
    }
}

#[test]
fn introduce_bean_avoids_names_taken_elsewhere_in_the_project() {
    let host = MockHost::install();
    host.add_file("src/main/java/com/acme/UserDao.java", "@org.springframework.stereotype.Repository\nclass UserDao {}\n");
    host.add_file("src/main/resources/other.xml", &beans_xml("<bean id=\"userDao2\" class=\"a.B\"/>"));
    let text = beans_xml(INNER);
    let document = doc(XML_PATH, &text);
    let p = position(&text, caret_in(&text, "<bean class"));
    host.add_file(XML_PATH, &text);
    let mut navigator = warmed_navigator();
    let action = navigator.file_actions(&document, Range::new(p, p)).remove(0);
    host.answer_next_prompt(Some(&["userDao3"]));
    host.answer_next_prompt(Some(&["yes"]));
    let introduce = navigator.resolve_file_action(&document, action).unwrap();
    let fixed = apply_edits(&text, &introduce.edits[0].1);
    assert!(xml::elements(&fixed).iter().any(|e| e.value("id") == Some("userDao3")), "{fixed}");
    let taken = || BTreeSet::from(["userDao".to_owned()]);
    let fixed = apply_edits(&text, &xml_actions::introduce_bean(&text, caret_in(&text, "<bean class"), taken).unwrap());
    assert!(xml::elements(&fixed).iter().any(|e| e.value("id") == Some("userDao2")), "{fixed}");
}

#[test]
fn weaving_in_a_parent_build_or_aop_xml_keeps_designators_quiet() {
    let module = "app/src/main/java/com/acme/Tracing.java";
    let host = MockHost::install();
    host.add_file("pom.xml", "<project><build><plugin>aspectj-maven-plugin</plugin></build></project>");
    host.add_file("app/pom.xml", "<project/>");
    host.add_file(module, WEAVING_ASPECT);
    let document = doc(&format!("/project/{module}"), WEAVING_ASPECT);
    warmed_navigator().checked_markers(&document);
    assert!(host.diagnostics().get(&document.path).is_none_or(Vec::is_empty));

    let host = MockHost::install();
    host.add_file("pom.xml", "<project/>");
    host.add_file("app/pom.xml", "<project/>");
    warmed_navigator().checked_markers(&document);
    assert_eq!(host.diagnostics()[&document.path].len(), 1, "no weaving anywhere still reports");
    host.add_file("app/src/main/resources/META-INF/aop.xml", "<aspectj/>");
    warmed_navigator().checked_markers(&document);
    assert!(host.diagnostics().get(&document.path).is_none_or(Vec::is_empty));
}

#[test]
fn resource_on_static_member_says_spring_fails() {
    let host = MockHost::install();
    let text = "import jakarta.annotation.Resource;\n\n@org.springframework.stereotype.Service\nclass A {\n    @Resource\n    static Repo repo;\n    @org.springframework.beans.factory.annotation.Autowired\n    static Repo other;\n}\n";
    let document = java("Subject.java", text);
    warmed_navigator().checked_markers(&document);
    let messages: Vec<String> = host.diagnostics()[&document.path]
        .iter()
        .filter(|d| d.code.as_deref() == Some("static-injection"))
        .map(|d| d.message.clone())
        .collect();
    assert_eq!(messages.len(), 2);
    assert!(messages[0].contains("@Resource") && messages[0].contains("fails") && !messages[0].contains("ignored"), "{}", messages[0]);
    assert!(messages[1].contains("@Autowired") && messages[1].contains("ignored"), "{}", messages[1]);
}

#[test]
fn saved_kotlin_file_publishes_field_injection() {
    let host = MockHost::install();
    let path = "/project/src/main/kotlin/com/acme/kt/Reports.kt";
    host.add_file("src/main/kotlin/com/acme/kt/Reports.kt", KOTLIN);
    warmed_navigator().checked_events(&[Event::FileSaved(path.into())], &host.invocation());
    assert!(host.diagnostics()[path].iter().any(|d| d.code.as_deref() == Some("field-injection")));
}

const MAILER: &str = "package com.acme.mail;\n\n@org.springframework.stereotype.Service\npublic class Mailer {\n}\n";

fn command_host(text: &str) -> MockHost {
    let host = MockHost::install();
    host.add_file("src/main/java/com/acme/mail/Mailer.java", MAILER);
    host.open_editor("/project/src/main/java/com/acme/Orders.java", "java", text, &[(3, 0, 3, 0)]);
    host
}

fn run(host: &MockHost, id: &str, answers: &[&[&str]]) -> Result<Option<String>, Error> {
    answers.iter().for_each(|a| host.answer_next_prompt(Some(a)));
    warmed_navigator().run_command(id, &host.invocation(), None)
}

const ORDERS: &str =
    "package com.acme;\n\nimport java.util.List;\npublic class Orders {\n    private int count;\n\n    void place() {}\n}\n";

#[test]
fn autowire_adds_an_annotated_field_with_imports() {
    let host = command_host(ORDERS);
    run(&host, "generate-autowired-dependency", &[&["0"], &["ok"]]).unwrap();
    let text = host.editor_text();
    assert!(text.contains("import com.acme.mail.Mailer;"), "{text}");
    assert!(text.contains("import org.springframework.beans.factory.annotation.Autowired;"));
    assert!(text.contains("    private int count;\n    @Autowired\n    private Mailer mailer;\n"), "{text}");
}

#[test]
fn setter_dependency_adds_field_and_autowired_setter() {
    let host = command_host(ORDERS);
    run(&host, "generate-setter", &[&["0"], &["ok"]]).unwrap();
    let text = host.editor_text();
    assert!(text.contains("    private Mailer mailer;\n"), "{text}");
    assert!(text.contains("    @Autowired\n    public void setMailer(Mailer mailer) {\n        this.mailer = mailer;\n    }\n}"), "{text}");
}

#[test]
fn constructor_dependency_creates_or_extends_the_constructor() {
    let host = command_host(ORDERS);
    run(&host, "generate-constructor-dependency", &[&["0"], &["ok"]]).unwrap();
    let text = host.editor_text();
    assert!(text.contains("private final Mailer mailer;"), "{text}");
    assert!(text.contains("public Orders(Mailer mailer) {\n        this.mailer = mailer;\n    }"), "{text}");

    let with_constructor = ORDERS.replace("    void place() {}", "    Orders(int n) {\n        this.count = n;\n    }");
    let host = command_host(&with_constructor);
    run(&host, "generate-constructor-dependency", &[&["0"], &["ok"]]).unwrap();
    let text = host.editor_text();
    assert!(text.contains("Orders(int n, Mailer mailer) {\n        this.count = n;\n        this.mailer = mailer;"), "{text}");
}

#[test]
fn event_listener_asks_for_the_event_type() {
    let host = command_host("package com.acme;\n\nclass Orders {}\n");
    run(&host, "generate-event-listener", &[&["OrderPlaced"], &["ok"]]).unwrap();
    let text = host.editor_text();
    assert!(text.contains("@EventListener\n    public void onOrderPlaced(OrderPlaced event) {\n    }\n}"), "{text}");
    assert!(text.contains("import org.springframework.context.event.EventListener;"));
    assert!(!text.contains("}}"));

    let host = command_host(ORDERS);
    assert!(run(&host, "generate-event-listener", &[&["not a type"]]).is_err());
    assert_eq!(host.editor_text(), ORDERS);
}

#[test]
fn generators_edit_nothing_when_dismissed_or_duplicated() {
    for id in ["generate-autowired-dependency", "generate-setter", "generate-constructor-dependency", "generate-event-listener"] {
        let host = command_host(ORDERS);
        host.answer_next_prompt(None);
        warmed_navigator().run_command(id, &host.invocation(), None).unwrap();
        assert_eq!(host.editor_text(), ORDERS, "{id}");
    }

    let host = command_host(&ORDERS.replace("int count", "Mailer mailer"));
    assert!(run(&host, "generate-autowired-dependency", &[&["0"]]).is_err());
}

#[test]
fn generators_without_write_access_show_a_notice() {
    let host = MockHost::install().with_capabilities(&["project.read", "editor.read"]);
    host.add_file("src/main/java/com/acme/mail/Mailer.java", MAILER);
    host.open_editor("/project/src/main/java/com/acme/Orders.java", "java", ORDERS, &[(3, 0, 3, 0)]);
    run(&host, "generate-autowired-dependency", &[&["0"], &["ok"]]).unwrap();
    assert_eq!(host.editor_text(), ORDERS);
    assert!(host.notifications()[0].body.contains("editor.write permission"));
}

#[test]
fn java_generators_cancel_exact_preview_without_any_source_change() {
    for (id, choice) in [("generate-autowired-dependency", "0"), ("generate-setter", "0"), ("generate-constructor-dependency", "0"), ("generate-event-listener", "OrderPlaced")] {
        let host = command_host(ORDERS);
        host.answer_next_prompt(Some(&[choice]));
        host.answer_next_prompt(None);
        let version = host.editor_version();
        warmed_navigator().run_command(id, &host.invocation(), None).unwrap();
        assert_eq!(host.editor_text(), ORDERS, "{id}");
        assert_eq!(host.editor_version(), version, "{id}");
    }
}

#[test]
fn java_generation_refuses_oversize_preview_unknown_owner_and_invalid_event_identifiers() {
    let oversized = format!("{}{}", ORDERS, " ".repeat(17 * 1024));
    let host = command_host(&oversized);
    assert!(run(&host, "generate-event-listener", &[&["OrderPlaced"], &["ok"]]).is_err());
    assert_eq!(host.editor_text(), oversized);
    for ty in ["1Order", "Order.Event", "a-b", "", "<Event>"] {
        let host = command_host(ORDERS);
        assert!(run(&host, "generate-event-listener", &[&[ty], &["ok"]]).is_err(), "{ty}");
        assert_eq!(host.editor_text(), ORDERS);
    }
    let host = command_host(ORDERS);
    host.set_project_root(Some("/other"));
    assert!(run(&host, "generate-event-listener", &[&["OrderPlaced"], &["ok"]]).is_err());
    assert_eq!(host.editor_text(), ORDERS);
}

#[test]
fn reviewed_java_generation_preserves_crlf_unicode_and_one_atomic_version() {
    let original = ORDERS.replace("private int count;", "/* 😀 café */ private int count;").replace('\n', "\r\n");
    let host = command_host(&original);
    let version = host.editor_version();
    run(&host, "generate-setter", &[&["0"], &["ok"]]).unwrap();
    let result = host.editor_text();
    assert!(result.contains("😀 café") && result.contains("void setMailer(Mailer mailer)"));
    assert!(!result.replace("\r\n", "").contains('\n'));
    assert_eq!(host.editor_version(), version + 1);
}

#[test]
fn active_profiles_are_chosen_stored_and_narrow_duplicate_names() {
    let host = MockHost::install();
    host.add_file(
        "src/main/java/com/acme/Dev.java",
        "@org.springframework.context.annotation.Configuration\nclass Dev {\n    @org.springframework.context.annotation.Bean @org.springframework.context.annotation.Profile(\"dev\") Clock clock() { return null; }\n}\n",
    );
    host.add_file("src/main/java/com/acme/Any.java", "@org.springframework.context.annotation.Configuration\nclass Any {\n    @org.springframework.context.annotation.Bean Clock clock() { return null; }\n}\n");
    host.add_file("src/main/resources/application-prod.yml", "a: b\n");

    let profile_choices = |host: &MockHost| {
        host.answer_next_prompt(Some(&["dev", "prod"]));
        warmed_navigator().run_command("change-active-profiles", &host.invocation(), None).unwrap();
        host.storage().get("activeProfiles").cloned()
    };
    let clash = || checks::coexist(&["dev".to_owned()], &[]);
    assert!(clash());
    assert_eq!(profile_choices(&host).as_deref(), Some("dev,prod"));
    assert!(clash());

    host.answer_next_prompt(Some(&["prod"]));
    warmed_navigator().run_command("change-active-profiles", &host.invocation(), None).unwrap();
    assert_eq!(commands::active_profiles(), ["prod"]);
    assert!(!clash());

    host.answer_next_prompt(None);
    warmed_navigator().run_command("change-active-profiles", &host.invocation(), None).unwrap();
    assert_eq!(commands::active_profiles(), ["prod"]);
}

#[test]
fn overloads_all_carrying_the_annotation_are_self_invocations() {
    let host = MockHost::install();
    host.set_setting("springBeans.checkSelfInvocation", "true");
    let both = SELF_CALL.replace("    public void run", "    @Transactional\n    public void save(int x) {}\n    public void run");
    assert_eq!(self_invocation(&host, &both), [pair("self-invocation", "save"), pair("self-invocation", "save")]);
}

const LITE_CONFIG: &str = "@org.springframework.context.annotation.Configuration(proxyBeanMethods = false)\nclass C {\n    @org.springframework.context.annotation.Bean\n    Clock clock() { return null; }\n    @org.springframework.context.annotation.Bean\n    Mailer mailer() { return new Mailer(clock()); }\n    Mailer plain() { return new Mailer(clock()); }\n}\n";

#[test]
fn bean_call_without_proxying_is_reported() {
    assert_eq!(found(LITE_CONFIG), [pair("unproxied-bean-method-call", "clock")]);
    let overloaded = LITE_CONFIG
        .replace("    @org.springframework.context.annotation.Bean\n    Mailer mailer", "    @org.springframework.context.annotation.Bean\n    Clock clock(int n) { return null; }\n    @org.springframework.context.annotation.Bean\n    Mailer mailer");
    assert_eq!(found(&overloaded), [pair("unproxied-bean-method-call", "clock")]);
}

#[test]
fn bean_call_is_quiet_with_proxying_or_a_plain_callee() {
    assert!(found(&LITE_CONFIG.replace("(proxyBeanMethods = false)", "")).is_empty());
    assert!(found(&LITE_CONFIG.replace("    @org.springframework.context.annotation.Bean\n    Clock clock", "    Clock clock")).is_empty());
    let partly =
        LITE_CONFIG.replace("    @org.springframework.context.annotation.Bean\n    Mailer mailer", "    Clock clock(int n) { return null; }\n    @org.springframework.context.annotation.Bean\n    Mailer mailer");
    assert!(found(&partly).is_empty(), "an overload without @Bean may be the one called");
    assert!(found(&LITE_CONFIG.replace("@org.springframework.context.annotation.Configuration", "@org.springframework.stereotype.Component")).is_empty());
}

#[test]
fn cache_expressions_with_unbalanced_brackets_are_errors() {
    let text = "@org.springframework.stereotype.Service\nclass A {\n    @Cacheable(cacheNames = \"c\", key = \"#id.toString(\", condition = \"#id > 'x\", unless = \"#result == null\")\n    Object find(long id) { return null; }\n}\n";
    assert_eq!(found(text), [pair("cache-spel-syntax", "#id.toString("), pair("cache-spel-syntax", "#id > 'x")]);
    assert!(found(&text.replace("#id.toString(\"", "#id\"").replace("#id > 'x", "#id > 1")).is_empty());
}

#[test]
fn cache_key_and_key_generator_together_are_errors() {
    let text = "@org.springframework.stereotype.Service\nclass A {\n    @Cacheable(cacheNames = \"c\", key = \"#id\", keyGenerator = \"gen\")\n    Object find(long id) { return null; }\n}\n";
    assert_eq!(found(text), [pair("cache-attribute-conflict", "\"gen\"")]);
    let library = format!("import com.acme.Cacheable;\n{text}");
    assert!(found(&library).is_empty());
}

fn project_findings(text: &str, extra: &[(&str, &str)]) -> Vec<(String, String)> {
    let host = setup();
    host.set_setting("springBeans.checkUnresolvedReferences", "true");
    for (name, content) in extra {
        host.add_file(name, content);
    }
    findings(&host, &mut warmed_navigator(), "Subject.java", text)
}

const WORKER: (&str, &str) =
    ("src/main/java/com/acme/Worker.java", "package com.acme;\npublic class Worker {\n    public void start() {}\n}\n");

#[test]
fn init_method_missing_from_a_project_class_is_reported() {
    let text = "package com.acme;\n@org.springframework.context.annotation.Configuration\nclass C {\n    @org.springframework.context.annotation.Bean(initMethod = \"boot\", destroyMethod = \"stop\")\n    Worker worker() { return new Worker(); }\n}\n";
    assert_eq!(project_findings(text, &[WORKER]), [pair("bean-method-missing", "boot"), pair("bean-method-missing", "stop")]);
    let known = text.replace("\"boot\"", "\"start\"").replace(", destroyMethod = \"stop\"", ", destroyMethod = \"(inferred)\"");
    assert!(project_findings(&known, &[WORKER]).is_empty());
}

#[test]
fn init_method_is_quiet_for_library_abstract_or_subtyped_classes() {
    let text =
        "package com.acme;\n@org.springframework.context.annotation.Configuration\nclass C {\n    @org.springframework.context.annotation.Bean(initMethod = \"boot\")\n    Worker worker() { return new Worker(); }\n}\n";
    assert!(project_findings(text, &[]).is_empty(), "type from a library");
    let sub = ("src/main/java/com/acme/Sub.java", "package com.acme;\nclass Sub extends Worker { void boot() {} }\n");
    assert!(project_findings(text, &[WORKER, sub]).is_empty());
    let library_parent = ("src/main/java/com/acme/Worker.java", "package com.acme;\npublic class Worker extends Thread {}\n");
    assert!(project_findings(text, &[library_parent]).is_empty());
    let lombok = ("src/main/java/com/acme/Worker.java", "package com.acme;\n@Getter\npublic class Worker {}\n");
    assert!(project_findings(text, &[lombok]).is_empty());
}

#[test]
fn init_method_reads_project_constants() {
    let constants = ("src/main/java/com/acme/Names.java", "package com.acme;\nclass Names { static final String BOOT = \"boot\"; }\n");
    let text =
        "package com.acme;\n@org.springframework.context.annotation.Configuration\nclass C {\n    @org.springframework.context.annotation.Bean(initMethod = Names.BOOT)\n    Worker worker() { return null; }\n}\n";
    assert_eq!(project_findings(text, &[WORKER, constants]), [pair("bean-method-missing", "Names.BOOT")]);
}

#[test]
fn test_bean_method_must_exist_in_the_test_class() {
    let text = "class T {\n    @TestBean(methodName = \"fake\")\n    Clock clock;\n    static Clock other() { return null; }\n}\n";
    assert_eq!(found(text), [pair("bean-method-missing", "fake")]);
    assert!(found(&text.replace("other", "fake")).is_empty());
    assert!(found(&text.replace("class T", "class T extends Base")).is_empty());
}

#[test]
fn required_outside_a_setter_is_reported() {
    let text = "class A {\n    @Required\n    void setClock(Clock c) {}\n    @Required\n    void configure(Clock c) {}\n}\n";
    assert_eq!(found(text), [pair("required-not-setter", "@Required")]);
}

#[test]
fn lookup_scheduler_and_spy_names_must_be_beans() {
    let text = "package com.acme;\nclass A {\n    @Lookup(\"nope\")\n    Clock clock() { return null; }\n    @Scheduled(fixedRate = 5, scheduler = \"ticker\")\n    void run() {}\n    @Lookup(\"clock\")\n    Clock fine() { return null; }\n}\n";
    assert_eq!(project_findings(text, &[]), [pair("unresolved-bean-name", "nope"), pair("unresolved-bean-name", "ticker")]);
    let spy = "package com.acme;\nclass T {\n    @MockitoSpyBean(name = \"gone\")\n    Clock c;\n}\n";
    assert_eq!(project_findings(spy, &[]), [pair("unresolved-bean-name", "gone")]);
    assert!(found(text).is_empty(), "off by default");
    assert!(project_findings(&text.replace("@Lookup(\"nope\")", "@Lookup(\"${x}\")"), &[]).iter().all(|f| f.1 != "${x}"));
}

#[test]
fn resource_locations_are_checked_against_the_resource_folders() {
    let text = "package com.acme;\n@ImportResource(\"classpath:beans.xml\")\n@PropertySource({\"classpath:missing.properties\", \"classpath*:any.properties\", \"file:/etc/x\"})\n@ContextConfiguration(locations = \"Test-context.xml\")\nclass A {\n    @Sql(\"/db/none.sql\")\n    void t() {}\n}\n";
    assert_eq!(
        project_findings(text, &[]),
        [
            pair("resource-not-found", "classpath:missing.properties"),
            pair("resource-not-found", "Test-context.xml"),
            pair("resource-not-found", "/db/none.sql")
        ]
    );
    let present = [
        ("src/test/resources/com/acme/Test-context.xml", "<beans/>"),
        ("src/main/resources/missing.properties", "a=b"),
        ("src/test/resources/db/none.sql", ""),
    ];
    assert!(project_findings(text, &present).is_empty());
    let optional = text.replace("@PropertySource(", "@PropertySource(ignoreResourceNotFound = true, value = ");
    assert!(!project_findings(&optional, &[]).iter().any(|f| f.1.contains("missing.properties")));
}

fn xml_with_setting(body: &str, unresolved: bool, files: &[(&str, &str)]) -> Vec<(String, String)> {
    let host = MockHost::install();
    if unresolved {
        host.set_setting("springBeans.checkUnresolvedReferences", "true");
    }
    for (name, content) in files {
        host.add_file(name, content);
    }
    xml_findings(&host, &beans_xml(body))
}

const WIRED: &str = "<bean id=\"a\" class=\"x.A\" depends-on=\"b, gone\"><property name=\"p\" ref=\"nope\"/><constructor-arg ref=\"a\"/></bean><bean id=\"b\" parent=\"missing\"/>";

#[test]
fn unresolved_xml_references_are_info_and_opt_in() {
    let found = xml_with_setting(WIRED, true, &[]);
    assert_eq!(
        found,
        [pair("unresolved-reference", "nope"), pair("unresolved-reference", "missing"), pair("unresolved-reference", "gone")]
    );
    assert!(xml_with_setting(WIRED, false, &[]).is_empty());
}

#[test]
fn unresolved_xml_references_are_quiet_with_imports_dynamic_or_container_names() {
    let imported = format!("<import resource=\"other.xml\"/>{WIRED}");
    let files = [("src/main/resources/other.xml", "<beans/>")];
    assert!(xml_with_setting(&imported, true, &files).is_empty());
    let quiet = "<bean id=\"a\" class=\"x.A\" depends-on=\"${dep}\"><property name=\"p\" ref=\"environment\"/><property name=\"q\" ref=\"${r}\"/></bean>";
    assert!(xml_with_setting(quiet, true, &[]).is_empty());
}

#[test]
fn unresolved_xml_references_see_beans_of_other_files_and_code() {
    let body = "<bean id=\"a\" class=\"x.A\"><property name=\"p\" ref=\"users\"/><property name=\"q\" ref=\"clock\"/></bean>";
    let host = setup();
    host.set_setting("springBeans.checkUnresolvedReferences", "true");
    assert!(xml_findings(&host, &beans_xml(body)).is_empty(), "stereotype and @Bean beans of the project");
}

#[test]
fn component_scan_filters_are_checked() {
    let body = "<context:component-scan use-default-filters=\"false\"><context:include-filter type=\"bogus\"/></context:component-scan><context:component-scan base-package=\"a\" use-default-filters=\"false\"/>";
    assert_eq!(
        xml_with_setting(body, false, &[]),
        [
            pair("missing-attribute", "context:component-scan"),
            pair("missing-attribute", "context:include-filter"),
            pair("invalid-attribute-value", "bogus"),
            pair("xml-component-scan-filters", "context:component-scan"),
        ]
    );
    let fine =
        "<context:component-scan base-package=\"a\"><context:exclude-filter type=\"regex\" expression=\".*\"/></context:component-scan>";
    assert!(xml_with_setting(fine, false, &[]).is_empty());
}

#[test]
fn import_of_a_missing_file_is_reported() {
    let body = "<import resource=\"gone.xml\"/><import resource=\"here.xml\"/><import resource=\"sub/../here.xml\"/><import resource=\"classpath:lib.xml\"/><import resource=\"file:/x.xml\"/><import resource=\"${p}.xml\"/>";
    let files = [("src/main/resources/here.xml", "<beans/>")];
    assert_eq!(xml_with_setting(body, false, &files), [pair("xml-import-missing", "gone.xml")]);
    assert_eq!(
        xml_with_setting(body, true, &files),
        [pair("xml-import-missing", "gone.xml"), pair("xml-import-missing", "classpath:lib.xml")]
    );
}

#[test]
fn inactive_profile_blocks_are_hinted_once_profiles_are_chosen() {
    let body = "<beans profile=\"dev\"/><beans profile=\"prod\"/><beans profile=\"!prod\"/>";
    assert!(xml_with_setting(body, false, &[]).is_empty());
    let host = MockHost::install();
    openidea_plugin::host::storage::set("activeProfiles", "prod").unwrap();
    let found = xml_findings(&host, &beans_xml(body));
    assert_eq!(found, [pair("xml-inactive-profile", "dev"), pair("xml-inactive-profile", "!prod")]);
}

#[test]
fn placeholders_are_checked_against_the_property_placeholder_files() {
    let body = "<context:property-placeholder location=\"classpath:app.properties\"/><bean id=\"a\" class=\"x.A\"><property name=\"p\" value=\"${db.url}\"/><property name=\"q\" value=\"${db.user:sa}\"/><property name=\"r\" value=\"${db.nope}\"/></bean>";
    let files = [("src/main/resources/app.properties", "# c\ndb.url = jdbc:x\ndb.user: u\n")];
    assert_eq!(xml_with_setting(body, true, &files), [pair("xml-placeholder-unresolved", "db.nope")]);
    assert!(xml_with_setting(body, true, &[]).is_empty(), "an unreadable location says nothing");
    assert!(xml_with_setting(body, false, &files).is_empty());
}

const POOL: (&str, &str) = (
    "src/main/java/com/acme/Pool.java",
    "package com.acme;\npublic class Pool {\n    public Pool(int size) {}\n    @Required\n    public void setUrl(String url) {}\n    public void setMaxSize(int n) {}\n    public void open() {}\n    public static Pool create() { return null; }\n    public Pool copy() { return null; }\n}\n",
);
const SHAPE: (&str, &str) =
    ("src/main/java/com/acme/Shape.java", "package com.acme;\npublic abstract class Shape {\n    public abstract Object x();\n}\n");

fn class_findings(body: &str, files: &[(&str, &str)]) -> Vec<(String, String)> {
    let host = MockHost::install();
    for (name, content) in files {
        host.add_file(name, content);
    }
    xml_findings(&host, &beans_xml(body))
}

#[test]
fn factory_method_must_exist_and_be_static() {
    let body = "<bean class=\"com.acme.Pool\" factory-method=\"create\"/><bean class=\"com.acme.Pool\" factory-method=\"copy\"/><bean class=\"com.acme.Pool\" factory-method=\"nope\"/>";
    let found = class_findings(body, &[POOL]);
    assert_eq!(found, [pair("xml-factory-method", "copy"), pair("xml-factory-method", "nope")]);
    let with_factory_bean = "<bean class=\"com.acme.Pool\" factory-bean=\"f\" factory-method=\"nope\"/>";
    assert!(class_findings(with_factory_bean, &[POOL]).iter().all(|f| f.0 != "xml-factory-method"));
}

#[test]
fn abstract_classes_cannot_be_instantiated() {
    let body = "<bean id=\"a\" class=\"com.acme.Shape\"/><bean id=\"b\" class=\"com.acme.Shape\" abstract=\"true\"/><bean id=\"c\" class=\"com.acme.Shape\"><lookup-method name=\"x\" bean=\"y\"/></bean>";
    assert_eq!(class_findings(body, &[SHAPE]), [pair("xml-abstract-class", "com.acme.Shape")]);
}

#[test]
fn constructor_arguments_must_fit_a_constructor() {
    let two = "<bean class=\"com.acme.Pool\"><constructor-arg value=\"1\"/><constructor-arg value=\"2\"/></bean>";
    assert_eq!(class_findings(two, &[POOL]), [pair("xml-constructor-args", "com.acme.Pool")]);
    let one = "<bean class=\"com.acme.Pool\"><constructor-arg value=\"1\"/></bean>";
    assert!(class_findings(one, &[POOL]).is_empty());
    assert!(class_findings(&two.replace("<bean ", "<bean autowire=\"constructor\" "), &[POOL]).is_empty());
    let lombok = ("src/main/java/com/acme/Pool.java", "package com.acme;\n@AllArgsConstructor\npublic class Pool {}\n");
    assert!(class_findings(two, &[lombok]).is_empty());
    let implicit = ("src/main/java/com/acme/Pool.java", "package com.acme;\npublic class Pool {}\n");
    assert_eq!(class_findings(one, &[implicit]), [pair("xml-constructor-args", "com.acme.Pool")]);
}

#[test]
fn lifecycle_methods_of_xml_beans_must_exist() {
    let body = "<bean class=\"com.acme.Pool\" init-method=\"open\" destroy-method=\"close\"/>";
    assert_eq!(class_findings(body, &[POOL]), [pair("bean-method-missing", "close")]);
}

#[test]
fn required_setters_must_be_set_when_annotations_are_enforced() {
    let enforced = "<context:annotation-config/><bean class=\"com.acme.Pool\"><constructor-arg value=\"1\"/></bean>";
    assert_eq!(class_findings(enforced, &[POOL]), [pair("xml-required-property", "bean")]);
    let set = enforced.replace("</bean>", "<property name=\"url\" value=\"u\"/></bean>");
    assert!(class_findings(&set, &[POOL]).is_empty());
    let attribute = enforced.replace("<bean ", "<bean xmlns:p=\"http://www.springframework.org/schema/p\" p:url=\"u\" ");
    assert!(class_findings(&attribute, &[POOL]).is_empty());
    assert!(class_findings(&enforced.replace("<context:annotation-config/>", ""), &[POOL]).is_empty());
}

#[test]
fn classes_with_library_parents_are_skipped() {
    let child =
        ("src/main/java/com/acme/Child.java", "package com.acme;\npublic class Child extends Lib {\n    public Child(int a) {}\n}\n");
    let body = "<bean class=\"com.acme.Child\" factory-method=\"nope\" init-method=\"x\"/>";
    assert!(class_findings(body, &[child]).is_empty());
}

#[test]
fn property_names_complete_and_navigate_to_setters() {
    let host = MockHost::install();
    host.add_file(POOL.0, POOL.1);
    let xml = beans_xml("<bean class=\"com.acme.Pool\"><property name=\"max\"/></bean>");
    let document = doc(XML_PATH, &xml);
    let mut n = warmed_navigator();
    let at = after(&xml, "name=\"max", 1);
    let labels: Vec<String> = n.completion(&document, at, None).into_iter().map(|i| i.label).collect();
    assert_eq!(labels, ["url", "maxSize"]);
    let full = beans_xml("<bean class=\"com.acme.Pool\"><property name=\"maxSize\"/></bean>");
    let found = n.definition(&doc(XML_PATH, &full), after(&full, "name=\"max", 1));
    assert_eq!(found.len(), 1);
    assert_eq!((found[0].path.as_str(), found[0].range.start.line), ("/project/src/main/java/com/acme/Pool.java", 5));
}

fn meta_findings(host: &MockHost, relative: &str, text: &str) -> Vec<(String, String)> {
    host.add_file(relative, text);
    let path = format!("/project/{relative}");
    let mut navigator = warmed_navigator();
    navigator.checked_events(&[Event::FileOpened(path.clone())], &host.invocation());
    host.diagnostics()
        .get(&path)
        .into_iter()
        .flatten()
        .map(|d| {
            let (from, to) = (offset(text, d.range.start).unwrap(), offset(text, d.range.end).unwrap());
            (d.code.clone().unwrap(), text[from..to].to_owned())
        })
        .collect()
}

#[test]
fn spring_handlers_must_name_project_classes() {
    let host = setup();
    let text = "http\\://acme/ns=com.acme.UserServiceImpl\nhttp\\://acme/two=com.acme.Gone\nhttp\\://lib/ns=org.lib.Handler\n";
    assert_eq!(
        meta_findings(&host, "src/main/resources/META-INF/spring.handlers", text),
        [pair("spring-handler-class-missing", "com.acme.Gone")]
    );
}

#[test]
fn spring_schemas_must_name_existing_files() {
    let host = setup();
    host.add_file("src/main/resources/META-INF/acme.xsd", "<xs/>");
    let text = "http\\://acme/a.xsd=META-INF/acme.xsd\nhttp\\://acme/b.xsd=META-INF/gone.xsd\n";
    assert_eq!(
        meta_findings(&host, "src/main/resources/META-INF/spring.schemas", text),
        [pair("spring-schema-file-missing", "META-INF/gone.xsd")]
    );
}

const SPRING_KTS: &str = "plugins {\n    kotlin(\"jvm\") version \"1.9.24\"\n    kotlin(\"plugin.spring\") version \"1.9.24\"\n}\n";

fn kt_codes(host: &MockHost, text: &str, code: &str) -> Vec<String> {
    let document = doc(KT_PATH, text);
    warmed_navigator().checked_markers(&document);
    host.diagnostics()
        .get(&document.path)
        .into_iter()
        .flatten()
        .filter(|d| d.code.as_deref() == Some(code))
        .map(|d| text[offset(text, d.range.start).unwrap()..offset(text, d.range.end).unwrap()].to_owned())
        .collect()
}

#[test]
fn open_is_redundant_when_the_spring_plugin_opens_the_class() {
    let text = "@org.springframework.stereotype.Service\nopen class Billing {\n    @Transactional\n    open fun charge() {}\n    fun other() {}\n}\n\nopen class Plain\n\n@Named(\"open\") class Odd\n";
    let host = kt_host(Some(("build.gradle.kts", SPRING_KTS)));
    assert_eq!(kt_codes(&host, text, "kotlin-redundant-open"), ["open", "open"]);
    let without_plugin = kt_host(Some(("build.gradle.kts", KTS_BUILD)));
    assert!(kt_codes(&without_plugin, text, "kotlin-redundant-open").is_empty());
    let allopen_only = kt_host(Some(("build.gradle", "plugins { id 'org.jetbrains.kotlin.plugin.allopen' }")));
    assert!(kt_codes(&allopen_only, text, "kotlin-redundant-open").is_empty());
    assert!(kt_codes(&kt_host(None), text, "kotlin-redundant-open").is_empty());
}

const ROUTES: &str = "fun routes() = router {\n    GET(\"/users/{id}\") { req -> ok().body(req.pathVariable(\"id\")) }\n    GET(\"/orders\") { req -> ok().body(req.pathVariable(\"oid\")) }\n}\n";

#[test]
fn router_path_variables_must_appear_in_a_route_path() {
    let host = kt_host(None);
    assert_eq!(kt_codes(&host, ROUTES, "router-path-variable"), ["oid"]);
    let nested = ROUTES.replace("GET(\"/orders\")", "GET(\"/orders/{oid:[0-9]+}\")");
    assert!(kt_codes(&host, &nested, "router-path-variable").is_empty());
    let not_a_router = "fun f(req: Request) = req.pathVariable(\"oid\")\n";
    assert!(kt_codes(&host, not_a_router, "router-path-variable").is_empty());
    let computed = ROUTES.replace("\"oid\"", "name");
    assert!(kt_codes(&host, &computed, "router-path-variable").is_empty());
}

#[test]
fn class_java_arguments_get_a_reified_extension_hint() {
    let host = kt_host(None);
    let text = "fun f(ctx: ApplicationContext, rest: RestTemplate) {\n    ctx.getBean(Foo::class.java)\n    ctx.getBean(\"foo\", Foo::class.java)\n    rest.getForObject(\"/u\", User::class.java)\n    getBean(Foo::class.java)\n    ctx.getBean(Foo::class)\n}\n";
    assert_eq!(kt_codes(&host, text, "kotlin-reified-extension"), ["Foo::class.java", "Foo::class.java", "User::class.java"]);
}

#[test]
fn check_configuration_collects_problems_into_the_report_view() {
    let host = setup();
    host.add_file(
        "src/main/java/com/acme/Bad.java",
        "package com.acme;\n@org.springframework.stereotype.Service\nclass Bad {\n    @Transactional\n    private void save() {}\n}\n",
    );
    let mut n = warmed_navigator();
    n.run_command("check-spring-configuration", &host.invocation(), None).unwrap();
    assert_eq!(host.revealed_views(), [BEANS_VIEW]);
    let shown = n.render(BEANS_VIEW, None, &host.invocation()).unwrap();
    let Body::Tree(items) = shown.body else { panic!("tree expected") };
    let bad = items.iter().find(|i| i.label == "Bad.java").unwrap();
    assert!(items.iter().any(|i| i.parent.as_deref() == Some(&bad.key) && i.detail.as_deref() == Some("line 5 · non-proxyable-method")));
    let problem =
        items.iter().find(|i| i.parent.is_some() && i.detail.as_deref().is_some_and(|d| d.ends_with("non-proxyable-method"))).unwrap();
    n.handle(BEANS_VIEW, ViewEvent::Activate(problem.key.clone()), &host.invocation()).unwrap();
    assert_eq!(host.opened_locations(), [(format!("{ROOT}/Bad.java"), 4, 17)]);
    assert!(host.notifications().last().unwrap().body.contains("problems in"));
}

#[test]
fn check_configuration_says_when_nothing_is_wrong() {
    let host = MockHost::install();
    host.add_file("src/main/java/com/acme/Fine.java", "package com.acme;\n@org.springframework.stereotype.Service\nclass Fine {}\n");
    let mut n = warmed_navigator();
    n.run_command("check-spring-configuration", &host.invocation(), None).unwrap();
    let Body::Message(text) = n.render(BEANS_VIEW, None, &host.invocation()).unwrap().body else { panic!("message expected") };
    assert_eq!(text, "No problems found.");
}

#[test]
fn dump_configuration_lists_the_beans() {
    let host = setup();
    let mut n = warmed_navigator();
    n.run_command("dump-spring-configuration", &host.invocation(), None).unwrap();
    let Body::Html(page) = n.render(BEANS_VIEW, None, &host.invocation()).unwrap().body else { panic!("html expected") };
    assert!(page.contains("@Service") && page.contains("users") && page.contains("UserServiceImpl.java:8"), "{page}");
    assert!(page.contains("[profiles: dev, prod]") && page.contains("[primary]"));
}

const SCHEDULED: &str = "package com.acme;\npublic class Jobs {\n    @Scheduled(cron = \"0 0 * * * *\")\n    void run() {}\n}\n";

fn externalize_host(text: &str, line: u32, column: u32) -> MockHost {
    let host = MockHost::install();
    host.open_editor("/project/src/main/java/com/acme/Jobs.java", "java", text, &[(line, column, line, column)]);
    host
}

#[test]
fn annotation_externalization_command_cancelled_at_the_first_prompt_changes_nothing() {
    let host = externalize_host(SCHEDULED, 2, 24);
    host.add_file("src/main/resources/application.properties", "a=1\n");
    run(&host, "externalize-annotation-string", &[]).unwrap();
    assert_eq!(host.editor_text(), SCHEDULED);
    assert_eq!(host.clipboard(), None);
    assert!(host.process_runs().is_empty());
    assert_eq!(host::project_of("/project/src/main/resources/application.properties").unwrap().read_file("/project/src/main/resources/application.properties").unwrap(), "a=1\n");
    let outside = externalize_host(SCHEDULED, 3, 6);
    assert!(run(&outside, "externalize-annotation-string", &[]).is_err());
}

const FACTORY: (&str, &str) = (
    "src/main/java/com/acme/Maker.java",
    "package com.acme;\npublic class Maker {\n    static Maker hidden() { return null; }\n    public static Maker open() { return null; }\n    public Object make() { return null; }\n    private Object secret() { return null; }\n}\n",
);

#[test]
fn factory_method_visibility_is_an_info_hint() {
    let body = "<bean class=\"com.acme.Maker\" factory-method=\"hidden\"/><bean class=\"com.acme.Maker\" factory-method=\"open\"/>";
    assert_eq!(class_findings(body, &[FACTORY]), [pair("xml-factory-method-visibility", "hidden")]);
}

#[test]
fn lookup_and_replaced_methods_must_be_overridable() {
    let body = "<bean class=\"com.acme.Maker\"><lookup-method name=\"make\" bean=\"x\"/><lookup-method name=\"nope\" bean=\"x\"/><replaced-method name=\"secret\" replacer=\"r\"/></bean>";
    assert_eq!(class_findings(body, &[FACTORY]), [pair("xml-lookup-method", "nope"), pair("xml-lookup-method", "secret")]);
}

#[test]
fn import_of_a_file_that_is_not_beans_is_info() {
    let body = "<import resource=\"other.xml\"/><import resource=\"beans2.xml\"/>";
    let files = [("src/main/resources/other.xml", "<project/>"), ("src/main/resources/beans2.xml", "<beans/>")];
    assert_eq!(xml_with_setting(body, false, &files), [pair("xml-import-not-beans", "other.xml")]);
}

#[test]
fn unresolved_bean_class_needs_a_project_package_root() {
    let files = [("src/main/java/com/acme/Real.java", "package com.acme;\nclass Real {}\n")];
    let body = "<bean class=\"com.acme.Gone\"/><bean class=\"com.acme.Real\"/><bean class=\"org.lib.Other\"/>";
    let host = MockHost::install();
    host.set_setting("springBeans.checkUnresolvedReferences", "true");
    for (name, content) in files {
        host.add_file(name, content);
    }
    assert_eq!(xml_findings(&host, &beans_xml(body)), [pair("unresolved-class", "com.acme.Gone")]);
}

#[test]
fn component_scan_filter_expressions_are_checked() {
    let body = "<context:component-scan base-package=\"a\"><context:include-filter type=\"regex\" expression=\"(a.*\"/><context:exclude-filter type=\"regex\" expression=\"a.*\"/></context:component-scan>";
    assert_eq!(xml_with_setting(body, false, &[]), [pair("xml-filter-expression", "(a.*")]);
}

#[test]
fn mockito_bean_names_and_relative_resources_are_checked() {
    let text = "package com.acme;\n@ImportResource(\"rel.xml\")\n@PropertySource(\"app.properties\")\nclass T {\n    @MockitoBean(name = \"gone\")\n    Clock c;\n}\n";
    let found = project_findings(text, &[("src/main/resources/app.properties", "")]);
    assert_eq!(found, [pair("resource-not-found", "rel.xml"), pair("unresolved-bean-name", "gone")]);
    let present = [("src/main/resources/com/acme/rel.xml", "<beans/>"), ("src/main/resources/app.properties", "")];
    assert_eq!(project_findings(text, &present), [pair("unresolved-bean-name", "gone")]);
}

#[test]
fn bare_context_configuration_needs_its_default_file() {
    let text = "package com.acme;\n@ContextConfiguration\nclass FooTest {}\n";
    assert_eq!(project_findings(text, &[]), [pair("resource-not-found", "@ContextConfiguration")]);
    let present = [("src/test/resources/com/acme/FooTest-context.xml", "<beans/>")];
    assert!(project_findings(text, &present).is_empty());
    assert!(project_findings(&text.replace("class FooTest {}", "class FooTest { @org.springframework.context.annotation.Configuration static class C {} }"), &[]).is_empty());
}

#[test]
fn cache_config_without_cache_annotations_is_a_hint() {
    let text = "@CacheConfig(cacheNames = \"a\")\nclass A {\n    void m() {}\n}\n";
    assert_eq!(found(text), [pair("cache-config-unused", "@CacheConfig(cacheNames = \"a\")")]);
    assert!(found(&text.replace("void m() {}", "@Cacheable\n    void m() {}")).is_empty());
    let both = "@CacheConfig(cacheManager = \"m\", cacheResolver = \"r\")\nclass A {\n    @Cacheable\n    void m() {}\n}\n";
    assert_eq!(found(both), [pair("cache-attribute-conflict", "\"r\"")]);
}

fn completion_labels(path: &str, text: &str, cursor_after: &str, files: &[(&str, &str)]) -> Vec<String> {
    let host = MockHost::install();
    for (name, content) in files {
        host.add_file(name, content);
    }
    let mut n = warmed_navigator();
    n.completion(&doc(path, text), after(text, cursor_after, 1), None).into_iter().map(|i| i.label).collect()
}

#[test]
fn live_templates_complete_by_abbreviation_with_tab_stops() {
    let host = MockHost::install();
    let text = "class A {\n    sbe\n}\n";
    let items = warmed_navigator().completion(&java("A.java", text), after(text, "sbe", 1), None);
    assert_eq!(items.iter().map(|i| i.label.as_str()).collect::<Vec<_>>(), ["sbean"]);
    assert!(items[0].snippet && items[0].insert_text.contains("${1:Type}"));
    let kotlin = warmed_navigator().completion(&doc("A.kt", "class A {\n    sbe\n}\n"), after("class A {\n    sbe\n}\n", "sbe", 1), None);
    assert_eq!(kotlin.len(), 1);
    drop(host);
    let in_string = "class A { String s = \"sbe\"; }";
    assert!(completion_labels("A.java", in_string, "\"sbe", &[]).is_empty());
}

#[test]
fn xml_live_templates_only_in_element_content() {
    let body = beans_xml("  sbe");
    assert_eq!(completion_labels("/project/b.xml", &body, "  sbe", &[]), ["sbean"]);
    let inside = beans_xml("<bean sbe/>");
    assert!(completion_labels("/project/b.xml", &inside, "<bean sbe", &[]).is_empty());
}

#[test]
fn scope_profile_and_constructor_arg_values_complete() {
    let body = beans_xml("<bean class=\"com.acme.Pool\" scope=\"\"><constructor-arg name=\"\"/></bean>");
    let files = [POOL];
    assert!(completion_labels("/project/b.xml", &body, "scope=\"", &files).contains(&"prototype".to_owned()));
    let host_files = [POOL, ("src/main/java/com/acme/P.java", "package com.acme;\n@org.springframework.context.annotation.Profile(\"dev\")\n@org.springframework.stereotype.Service\nclass P {}\n")];
    let java_text = "@Scope(\"pr\") @org.springframework.context.annotation.Profile(\"d\")\nclass B {}\n";
    assert!(completion_labels("/project/src/main/java/B.java", java_text, "@Scope(\"", &[]).contains(&"singleton".to_owned()));
    assert_eq!(completion_labels("/project/src/main/java/B.java", java_text, "@org.springframework.context.annotation.Profile(\"", &host_files), ["dev"]);
    let xml_profile = beans_xml("<beans profile=\"\"/>");
    assert_eq!(completion_labels("/project/b.xml", &xml_profile, "<beans profile=\"", &host_files), ["dev"]);
    let args = beans_xml("<bean class=\"com.acme.Pool\"><constructor-arg name=\"\"/></bean>");
    assert_eq!(completion_labels("/project/b.xml", &args, "constructor-arg name=\"", &files), ["size"]);
}

#[test]
fn relationship_targets_are_provider_navigation_never_host_rename_anchors() {
    use openidea_plugin::indexing::{Document as IndexDocument, DocumentIndexer};
    let host = setup();
    let listener = "package com.acme;\n@org.springframework.stereotype.Component class ReadyListener { @EventListener void ready(Ready event) {} }\n";
    let publisher = "package com.acme;\n@org.springframework.stereotype.Component class Sender { void send() { bus.publishEvent(new Ready()); } }\n";
    host.add_file("src/main/java/com/acme/ReadyListener.java", listener);
    host.add_file("src/main/java/com/acme/Sender.java", publisher);
    let mut navigator = warmed_navigator();
    for (path, text) in [("A.java", SERVICE), ("L.java", listener), ("P.java", publisher), ("application-dev.yml", "app:\n  name: hello\n")]
    {
        let graph = navigator.index(&IndexDocument { path: path.into(), text: text.into(), symbols: Vec::new() });
        let mut keys = graph.definitions.iter().chain(&graph.references).chain(&graph.markers).map(|a| a.key.as_str());
        assert!(keys.all(|k| k.starts_with("class:") || k.starts_with("resource#") || k.starts_with("profile#")), "{path}");
        assert!(graph.aliases.is_empty());
    }
    let markers = navigator.checked_markers(&java("Sender.java", publisher));
    assert!(markers.iter().any(|m| m.target.as_ref().is_some_and(|t| t.path.ends_with("ReadyListener.java"))), "{markers:?}");
    let back = navigator.checked_markers(&java("ReadyListener.java", listener));
    assert!(back.iter().any(|m| m.target.as_ref().is_some_and(|t| t.path.ends_with("Sender.java"))), "{back:?}");
}

#[test]
fn source_resource_navigation_survives_without_filename_insertion_anchors() {
    let host = setup();
    host.add_file("src/main/resources/app.properties", "app.name=demo\n");
    let text = "@PropertySource(\"classpath:app.properties\") class Config {}";
    let mut navigator = warmed_navigator();
    let targets = navigator.definition(&java("Config.java", text), after(text, "app.properties", 1));
    assert!(targets.iter().any(|t| t.path.ends_with("app.properties")), "{targets:?}");
    navigator.checked_events(
        &[Event::FilesChanged(openidea_plugin::FilesChanged {
            removed: vec!["/project/src/main/resources/app.properties".into()],
            ..Default::default()
        })],
        &host.invocation(),
    );
    assert!(navigator.definition(&java("Config.java", text), after(text, "app.properties", 1)).is_empty());
}

#[test]
fn unsaved_listener_edits_cannot_leave_a_stale_event_target() {
    let host = setup();
    let listener = "@org.springframework.stereotype.Component class ReadyListener { @EventListener void ready(Ready event) {} }";
    let publisher = "@org.springframework.stereotype.Component class Sender { void send() { bus.publishEvent(new Ready()); } }";
    host.add_file("src/main/java/com/acme/ReadyListener.java", listener);
    let mut navigator = warmed_navigator();
    let source = java("Sender.java", publisher);
    assert!(navigator.checked_markers(&source).iter().any(|m| m.target.as_ref().is_some_and(|l| l.path.ends_with("ReadyListener.java"))));
    host.open_editor(
        "/project/src/main/java/com/acme/ReadyListener.java",
        "java",
        "@org.springframework.stereotype.Component class ReadyListener { @EventListener void ready(Other event) {} }",
        &[],
    );
    // The host's current-text diagnostics callback updates this unsaved document;
    // navigation then uses that cache without rereading another editor/file.
    navigator.diagnostics(&java("ReadyListener.java", "@org.springframework.stereotype.Component class ReadyListener { @EventListener void ready(Other event) {} }"));
    assert!(!navigator.checked_markers(&source).iter().any(|m| m.target.as_ref().is_some_and(|l| l.path.ends_with("ReadyListener.java"))));
}

// Existing full-context inspection fixtures deliberately exercise the explicit
// configuration-check path, rather than coupling publication to gutter requests.
impl Navigator {
    fn checked_markers(&mut self, doc: &Document) -> Vec<LineMarker> {
        self.publish_full_fixture(&doc.path, &doc.language, &doc.text);
        self.line_markers(doc)
    }
    fn publish_full_fixture(&mut self, path: &str, language: &str, text: &str) {
        let index = self.indexes.for_document(path, text, COMMAND_BUDGET);
        let problems = problems_of(path, language, text, index.as_deref().map(World::new).as_ref());
        host::publish_diagnostics(path, &problems).unwrap();
    }
    fn checked_events(&mut self, events: &[Event], ctx: &Invocation) {
        self.handle_events(events, ctx);
        for event in events {
            let path = match event {
                Event::FileOpened(p) | Event::FileSaved(p) => p,
                _ => continue,
            };
            let language = if target_checks::SPRING_META_FILES.iter().any(|f| path.ends_with(f)) {
                "spring-meta"
            } else if path.ends_with(".kt") {
                "kotlin"
            } else if path.ends_with(".xml") {
                "xml"
            } else {
                "java"
            };
            if let Some(project) = host::project_of(path) {
                if let Ok(text) = project.read_file(path) {
                    self.publish_full_fixture(path, language, &text);
                }
            }
        }
    }
}

#[test]
fn live_diagnostics_uses_unsaved_text_and_clears_after_fix_without_publication() {
    let host = MockHost::install();
    host.add_file("src/main/java/com/acme/Subject.java", "class Subject {}");
    let mut navigator = Navigator::default();
    let broken = java("Subject.java", "// 😀\n@org.springframework.stereotype.Service class Subject { @org.springframework.beans.factory.annotation.Autowired private Repo repo; }");
    let problems = navigator.diagnostics(&broken);
    assert!(problems.iter().any(|p| p.code.as_deref() == Some("field-injection")));
    let fixed = java("Subject.java", "class Subject {}");
    assert!(navigator.diagnostics(&fixed).is_empty());
    assert!(host.diagnostics().is_empty(), "only the host publishes returned diagnostics");
}

#[test]
fn live_diagnostics_denied_context_and_unknown_external_types_stay_conservative() {
    let _host = MockHost::install().with_capabilities(&[]);
    let mut navigator = Navigator::default();
    let doc = java(
        "Subject.java",
        "@org.springframework.stereotype.Service class Subject extends External { @org.springframework.beans.factory.annotation.Autowired private Repo repo; @org.springframework.context.annotation.Bean(initMethod=\"fromLibrary\") External bean() { return null; } }",
    );
    let problems = navigator.diagnostics(&doc);
    assert!(problems.iter().any(|p| p.code.as_deref() == Some("field-injection")));
    assert!(!problems.iter().any(|p| p.message.contains("fromLibrary") || p.message.contains("No bean")));
    let mut large = doc.clone();
    large.text = " ".repeat(512 * 1024 + 1);
    assert!(navigator.diagnostics(&large).is_empty());
}

#[test]
fn live_xml_checks_do_not_read_missing_imports_or_report_cold_absences() {
    let host = MockHost::install();
    host.set_setting("springBeans.checkUnresolvedReferences", "true");
    let text = beans_xml(
        "<import resource=\"missing.xml\"/><bean id=\"subject\" class=\"external.Subject\"><property name=\"repo\" ref=\"unknown\"/></bean>",
    );
    let mut navigator = Navigator::default();
    assert!(
        navigator
            .diagnostics(&doc(XML_PATH, &text))
            .iter()
            .all(|p| !p.message.contains("missing.xml") && !p.message.contains("unknown") && !p.message.contains("external.Subject"))
    );
    let index = navigator.indexes.for_cached_document(XML_PATH, &text).unwrap();
    assert!(!index.complete());
    assert!(!index.scanning());
    assert_eq!(index.files().len(), 1);
    assert!(host.diagnostics().is_empty());
}

#[test]
fn live_diagnostic_ranges_are_utf16_and_markers_do_not_publish() {
    let host = MockHost::install();
    let text = "@org.springframework.stereotype.Service class Subject { /* 😀 */ @org.springframework.beans.factory.annotation.Autowired private Repo repo; }";
    let doc = java("Subject.java", text);
    let mut navigator = Navigator::default();
    let problems = navigator.diagnostics(&doc);
    let finding = problems.iter().find(|p| p.code.as_deref() == Some("field-injection")).unwrap();
    let from = offset(text, finding.range.start).unwrap();
    let to = offset(text, finding.range.end).unwrap();
    assert_eq!(&text[from..to], "@org.springframework.beans.factory.annotation.Autowired");
    assert_eq!(finding.range.start.character as usize, text[..from].encode_utf16().count());
    navigator.line_markers(&doc);
    navigator.handle_events(&[Event::FileOpened(doc.path.clone())], &host.invocation());
    assert!(host.diagnostics().is_empty());
}

#[test]
fn live_xml_preserves_local_structure_checks() {
    let _host = MockHost::install();
    let text = beans_xml("<aop:config><aop:aspect id=\"a\"/></aop:config>");
    assert!(Navigator::default().diagnostics(&doc(XML_PATH, &text)).iter().any(|p| p.code.as_deref() == Some("aop-structure")));
}

#[test]
fn metadata_checks_remain_in_explicit_configuration_report() {
    let host = MockHost::install();
    host.add_file("src/main/resources/META-INF/spring.schemas", "http\\://acme/schema.xsd=missing.xsd\n");
    let mut navigator = Navigator::default();
    commands::check_configuration(&mut navigator, &host.invocation()).unwrap();
    let Report::Problems(files) = navigator.report else { panic!("expected explicit configuration report") };
    assert!(files.iter().any(|(path, findings)| path.ends_with("META-INF/spring.schemas")
        && findings.iter().any(|d| d.code.as_deref() == Some("spring-schema-file-missing"))));
    assert!(host.diagnostics().is_empty());
}

mod external_packaged_refresh_tests {
    use super::*;
    fn notices(host:&MockHost,after:usize)->Vec<String>{
        host.view_messages()[after..].iter().filter(|(_,text)|text==r#"{"channel":"ui-kit-refresh"}"#).map(|(view,_)|view.clone()).collect()
    }
    #[test]
    fn observed_source_change_notifies_only_beans_and_new_model_uses_updated_cache(){
        let host=setup();let mut nav=warmed_navigator();let before=host.view_messages().len();
        host.add_file("src/main/java/com/acme/Added.java","package com.acme; import org.springframework.stereotype.Component; @Component public class Added {}");
        nav.handle_events(&[Event::FileSaved("/project/src/main/java/com/acme/Added.java".into())],&host.invocation());
        assert_eq!(notices(&host,before),[BEANS_VIEW.to_owned()]);
        let model=nav.render(BEANS_VIEW,None,&host.invocation()).unwrap();
        assert!(format!("{:?}",model.body).contains("added"));assert!(host.requests().is_empty());assert!(host.process_runs().is_empty());
    }
    #[test]
    fn reindex_and_explicit_report_commands_notify_packaged_view(){
        let host=setup();let mut nav=warmed_navigator();
        for command in ["reindex","dump-spring-configuration","check-spring-configuration"] {
            let before=host.view_messages().len();nav.run_command(command,&host.invocation(),None).unwrap();
            assert!(!notices(&host,before).is_empty(),"{command}");assert!(notices(&host,before).iter().all(|view|view==BEANS_VIEW));
        }
        assert_eq!(nav.report_root.as_deref(),Some("/project"));
    }
    #[test]
    fn foreign_missing_or_denied_project_returns_replacement_without_old_report(){
        let host=setup();let mut nav=warmed_navigator();nav.report=Report::Dump("PRIVATE SOURCE REPORT".into());nav.report_root=Some("/project".into());nav.sections.set(REPORT_SECTION);
        host.set_project_root(Some("/other"));
        assert!(!format!("{:?}",nav.render(BEANS_VIEW,None,&host.invocation()).unwrap()).contains("PRIVATE SOURCE REPORT"));
        host.set_project_root(None);
        assert!(matches!(nav.render(BEANS_VIEW,None,&host.invocation()).unwrap().body,Body::Message(_)));
        host.set_project_root(Some("/project"));let host=host.with_capabilities(&["storage"]);
        let model=nav.render(BEANS_VIEW,None,&host.invocation()).unwrap();
        assert!(matches!(model.body,Body::Message(text) if text.contains("Project identity is unavailable") && !text.contains("Allow project.read")));
    }
    #[test]
    fn closed_report_owner_is_invalidated_even_without_file_event(){
        let host=setup();let mut nav=warmed_navigator();nav.report=Report::Dump("PRIVATE SOURCE REPORT".into());nav.report_root=Some("/project".into());
        host.set_project_root(None);let before=host.view_messages().len();nav.handle_events(&[],&host.invocation());
        assert!(nav.report_root.is_none());assert!(matches!(nav.report,Report::Empty));assert_eq!(notices(&host,before),[BEANS_VIEW.to_owned()]);
    }
    #[test]
    fn stable_ui_kit_render_only_replies_without_index_progress_feedback(){
        let host=setup();let mut nav=warmed_navigator();
        for sequence in [1,2] {
            let before=host.view_messages().len();
            let request=serde_json::json!({"channel":"ui-kit","frameId":"0123456789abcdef0123456789abcdef","sequence":sequence,"project":"/project","op":"render"});
            nav.handle(BEANS_VIEW,ViewEvent::Message(request.to_string()),&host.invocation()).unwrap();
            let messages=host.view_messages();assert_eq!(messages.len(),before+1);
            let reply:serde_json::Value=serde_json::from_str(&messages[before].1).unwrap();assert_eq!(reply["channel"],"ui-kit");assert!(reply.get("model").is_some());
        }
    }
    #[test]
    fn cold_foreign_cache_and_irrelevant_event_do_not_scan_or_notify(){
        let host=setup();let mut nav=warmed_navigator();host.set_project_root(Some("/other"));let before=host.view_messages().len();
        let model=nav.render(BEANS_VIEW,None,&host.invocation()).unwrap();assert!(matches!(model.body,Body::Message(text) if text.contains("Not scanned yet")));
        nav.handle_events(&[Event::EditorFocused(None)],&host.invocation());assert!(notices(&host,before).is_empty());
        assert!(host.requests().is_empty());assert!(host.process_runs().is_empty());
    }
}

#[test]
fn live_xml_diagnostics_refuse_namespace_invalid_or_foreign_root_context() {
    let _host=MockHost::install();let good=beans_xml("<bean id=\"kept\" class=\"a.Kept\"/><aop:config><aop:aspect id=\"a\"/></aop:config>");
    assert!(Navigator::default().diagnostics(&doc(XML_PATH,&good)).iter().any(|p|p.code.as_deref()==Some("aop-structure")));
    for text in [good.replacen("<beans ","<beans xmlns:xml=\"urn:foreign\" ",1),good.replacen("<beans ","<beans bad:flag=\"x\" ",1),good.replacen("http://www.springframework.org/schema/beans","urn:foreign",1)] {
        assert!(Navigator::default().diagnostics(&doc(XML_PATH,&text)).is_empty(),"{text}");
    }
}

#[test]
fn invalid_xml_current_text_drops_old_bean_facts_and_marks_context_incomplete_until_fixed() {
    let host=MockHost::install();let good=beans_xml("<bean id=\"kept\" class=\"a.Kept\"/>");
    host.add_file(XML_PATH,&good);host.add_file("Other.java","@org.springframework.stereotype.Service class Unaffected {}");let project=host.invocation().project().unwrap();let mut nav=Navigator::default();
    assert!(nav.indexes.get(&project,COMMAND_BUDGET).complete());assert!(nav.indexes.cached(&project).unwrap().files().contains_key(XML_PATH));
    let bad=good.replacen("<beans ","<beans xmlns:xml=\"urn:foreign\" ",1);assert!(nav.diagnostics(&doc(XML_PATH,&bad)).is_empty());
    let cache=nav.indexes.cached(&project).unwrap();assert!(!cache.complete()&&!cache.files().contains_key(XML_PATH));assert!(cache.files().contains_key("/project/Other.java"));assert!(cache.state().contains("not valid Spring XML"));
    assert!(parse(XML_PATH,&bad).is_none());nav.diagnostics(&doc(XML_PATH,&good));assert!(nav.indexes.cached(&project).unwrap().complete());assert!(nav.indexes.cached(&project).unwrap().files().contains_key(XML_PATH));
}

#[test]
fn namespace_context_retains_prefixed_and_legacy_dtd_beans_without_external_resolution() {
    let host=MockHost::install();let legacy="<!DOCTYPE beans SYSTEM \"https://example.invalid/spring-beans.dtd\"><beans><bean id=\"legacy\" class=\"a.Legacy\"/></beans>";
    assert_eq!(parse("legacy.xml",legacy).unwrap().beans[0].name,"legacy");
    let prefixed="<b:beans xmlns:b=\"http://www.springframework.org/schema/beans\" xmlns=\"urn:other\"><b:bean id=\"prefixed\" class=\"a.Prefixed\"/></b:beans>";
    assert_eq!(parse("prefixed.xml",prefixed).unwrap().beans[0].name,"prefixed");
    let foreign="<beans xmlns=\"urn:foreign\"><bean id=\"notSpring\" class=\"a.Foreign\"/></beans>";
    let model=parse("foreign.xml",foreign).unwrap();assert!(model.beans.is_empty());assert!(model.navigation.definitions.iter().any(|(key,_)|key=="resource#foreign.xml"));
    assert!(host.requests().is_empty()&&host.process_runs().is_empty());
}

#[test]
fn oversized_or_deep_current_source_cannot_leave_usable_old_bean_ownership() {
    for oversized in [false,true]{let host=MockHost::install();let good=beans_xml("<bean id=\"kept\" class=\"a.Kept\"/>");host.add_file(XML_PATH,&good);let project=host.invocation().project().unwrap();let mut nav=Navigator::default();assert!(nav.indexes.get(&project,COMMAND_BUDGET).complete());
        let unsupported=if oversized{format!("{good}{}"," ".repeat(512*1024))}else{beans_xml(&format!("{}{}","<bean>".repeat(129),"</bean>".repeat(129)))};
        assert!(nav.diagnostics(&doc(XML_PATH,&unsupported)).is_empty());let cache=nav.indexes.cached(&project).unwrap();assert!(!cache.complete()&&!cache.files().contains_key(XML_PATH));
    }
}

#[test]
fn foreign_prefixed_ref_attribute_does_not_navigate_as_spring_bean_reference() {
    let host=MockHost::install();
    let text="<beans xmlns='http://www.springframework.org/schema/beans' xmlns:x='urn:foreign'><bean id='target' class='a.Target'/><bean id='owner' class='a.Owner'><property name='p' x:ref='target'/></bean></beans>";
    host.add_file(XML_PATH,text);let mut nav=warmed_navigator();
    let at=position(text,text.rfind("target").unwrap());
    assert!(nav.definition(&doc(XML_PATH,text),at).is_empty());
}

#[test]
fn foreign_or_malformed_xml_envelope_cannot_borrow_cached_other_bean_navigation() {
    let host=MockHost::install();host.add_file("other.xml","<beans xmlns='http://www.springframework.org/schema/beans'><bean id='target' class='a.Target'/></beans>");
    let good="<beans xmlns='http://www.springframework.org/schema/beans'><bean id='owner'><property name='p' ref='target'/></bean></beans>";
    host.add_file(XML_PATH,good);let mut nav=warmed_navigator();let at=position(good,good.find("target").unwrap());
    assert_eq!(nav.definition(&doc(XML_PATH,good),at).len(),1);
    for text in [good.replace("http://www.springframework.org/schema/beans","urn:foreign"),good.replace("<beans ","<beans xmlns:xml='urn:foreign' "),good.replace("</beans>","</wrong>")] {
        let at=position(&text,text.find("target").unwrap());
        assert!(nav.definition(&doc(XML_PATH,&text),at).is_empty(),"{text}");
        assert!(nav.completion(&doc(XML_PATH,&text),at,None).is_empty(),"{text}");
    }
}

#[test]
fn illegal_namespace_prefix_name_cannot_prove_a_spring_xml_navigation_context() {
    let host=MockHost::install();
    let text="<beans xmlns='http://www.springframework.org/schema/beans' xmlns:1bad='urn:foreign'><bean id='target'/><bean id='owner'><property name='p' ref='target'/></bean></beans>";
    host.add_file(XML_PATH,text);let mut nav=warmed_navigator();let at=position(text,text.rfind("target").unwrap());
    assert!(nav.definition(&doc(XML_PATH,text),at).is_empty());
}

#[test]
fn duplicate_expanded_attribute_names_cannot_prove_a_spring_xml_navigation_context() {
    let host=MockHost::install();
    let text="<beans xmlns='http://www.springframework.org/schema/beans' xmlns:x='urn:foreign' xmlns:y='urn:foreign' x:flag='a' y:flag='b'><bean id='target'/><bean id='owner'><property name='p' ref='target'/></bean></beans>";
    host.add_file(XML_PATH,text);let mut nav=warmed_navigator();let at=position(text,text.rfind("target").unwrap());
    assert!(nav.definition(&doc(XML_PATH,text),at).is_empty());
}

#[test]
fn valid_unicode_namespace_names_retain_owned_spring_xml_navigation() {
    let host=MockHost::install();
    let text="<é:beans xmlns:é='http://www.springframework.org/schema/beans' xmlns:𐀀x='urn:foreign' 𐀀x:flag='a'><é:bean id='target'/><é:bean id='owner'><é:property name='p' ref='target'/></é:bean></é:beans>";
    host.add_file(XML_PATH,text);let mut nav=warmed_navigator();let at=position(text,text.rfind("target").unwrap());
    assert_eq!(nav.definition(&doc(XML_PATH,text),at).len(),1);
}

#[test]
fn foreign_core_attribute_id_is_not_a_declared_spring_bean_name() {
    let host=MockHost::install();let text="<beans xmlns='http://www.springframework.org/schema/beans' xmlns:x='urn:foreign'><bean x:id='target' class='a.Target'/><bean id='owner'><property name='p' ref='target'/></bean></beans>";
    host.add_file(XML_PATH,text);let mut nav=warmed_navigator();let at=position(text,text.rfind("target").unwrap());
    assert!(nav.definition(&doc(XML_PATH,text),at).is_empty());
}
#[test]
fn foreign_core_attribute_class_is_not_a_setter_navigation_owner() {
    let host=MockHost::install();host.add_file("Target.java","package a; class Target { public void setP(String p){} }");
    let text="<beans xmlns='http://www.springframework.org/schema/beans' xmlns:x='urn:foreign'><bean id='owner' x:class='a.Target'><property name='p'/></bean></beans>";
    host.add_file(XML_PATH,text);let mut nav=warmed_navigator();let at=position(text,text.find("name='p'").unwrap()+6);
    assert!(nav.definition(&doc(XML_PATH,text),at).is_empty());
}
#[test]
fn foreign_core_attribute_scope_does_not_trigger_spring_scope_warning() {
    let _host=MockHost::install();let text="<beans xmlns='http://www.springframework.org/schema/beans' xmlns:x='urn:foreign'><bean id='owner' class='a.Owner' x:scope='Singleton'/></beans>";
    assert!(!Navigator::default().diagnostics(&doc(XML_PATH,text)).iter().any(|d|d.code.as_deref()==Some("xml-scope")));
}
#[test]
fn foreign_core_attribute_ref_does_not_trigger_spring_missing_reference_warning() {
    let host=MockHost::install();let text="<beans xmlns='http://www.springframework.org/schema/beans' xmlns:x='urn:foreign'><bean id='owner'><property name='p' x:ref='missing'/></bean></beans>";
    host.add_file(XML_PATH,text);let mut nav=warmed_navigator();
    assert!(!nav.diagnostics(&doc(XML_PATH,text)).iter().any(|d|d.code.as_deref()==Some("unresolved-reference")));
}
#[test]
fn foreign_core_attribute_filter_type_is_not_a_spring_filter_enum_value() {
    let _host=MockHost::install();let text="<beans xmlns='http://www.springframework.org/schema/beans' xmlns:context='http://www.springframework.org/schema/context' xmlns:x='urn:foreign'><context:component-scan base-package='a'><context:include-filter x:type='bogus' expression='a.Foo'/></context:component-scan></beans>";
    assert!(!Navigator::default().diagnostics(&doc(XML_PATH,text)).iter().any(|d|d.code.as_deref()==Some("invalid-attribute-value")));
}
#[test]
fn owned_core_attributes_remain_authoritative_beside_foreign_lookalikes() {
    let host=MockHost::install();host.add_file("Target.java","package a; class Target { public void setP(String p){} }");
    let text="<beans xmlns='http://www.springframework.org/schema/beans' xmlns:x='urn:foreign'><bean x:id='foreign' id='target' x:class='foreign.Target' class='a.Target'/><bean id='owner' x:class='foreign.Owner' class='a.Target'><property name='p' ref='target'/></bean></beans>";
    host.add_file(XML_PATH,text);let mut nav=warmed_navigator();
    let at=position(text,text.rfind("target").unwrap());let targets=nav.definition(&doc(XML_PATH,text),at);assert_eq!(targets.len(),1);
    assert_eq!(targets[0].range.start,position(text,text.find(" id='target'").unwrap()+5));
    let property=position(text,text.find("name='p'").unwrap()+6);
    assert_eq!(nav.definition(&doc(XML_PATH,text),property).len(),1);
}
#[test]
fn foreign_element_ref_is_not_a_spring_reference_owner() {
    let host=MockHost::install();let text="<beans xmlns='http://www.springframework.org/schema/beans' xmlns:x='urn:foreign'><bean id='target'/><x:property ref='target'/></beans>";
    host.add_file(XML_PATH,text);let mut nav=warmed_navigator();let at=position(text,text.rfind("target").unwrap());assert!(nav.definition(&doc(XML_PATH,text),at).is_empty());
}
#[test]
fn foreign_element_class_is_not_a_spring_class_owner() {
    let host=MockHost::install();host.add_file("Target.java","package a; class Target {}");let text="<beans xmlns='http://www.springframework.org/schema/beans' xmlns:x='urn:foreign'><x:thing class='a.Target'/></beans>";
    host.add_file(XML_PATH,text);let mut nav=warmed_navigator();let at=position(text,text.find("a.Target").unwrap());assert!(nav.definition(&doc(XML_PATH,text),at).is_empty());
}
#[test]
fn foreign_prefixed_value_is_not_a_spring_expression_owner() {
    let host=MockHost::install();let text="<beans xmlns='http://www.springframework.org/schema/beans' xmlns:x='urn:foreign'><bean id='target'/><bean id='owner'><property name='p' x:value='#{@target}'/></bean></beans>";
    host.add_file(XML_PATH,text);let mut nav=warmed_navigator();let at=position(text,text.rfind("target").unwrap());assert!(nav.definition(&doc(XML_PATH,text),at).is_empty());
}
#[test]
fn owned_aop_ref_plain_value_and_namespace_bound_shortcut_expression_remain_navigable() {
    let host=MockHost::install();let text="<beans xmlns='http://www.springframework.org/schema/beans' xmlns:aop='http://www.springframework.org/schema/aop' xmlns:q='http://www.springframework.org/schema/p'><bean id='target'/><bean id='owner' q:text='#{@target}'><property name='p' value='#{@target}'/></bean><aop:config><aop:aspect ref='target'/></aop:config></beans>";
    host.add_file(XML_PATH,text);let mut nav=warmed_navigator();
    for (at,_) in text.match_indices("target").skip(1){assert_eq!(nav.definition(&doc(XML_PATH,text),position(text,at)).len(),1,"{at}");}
}
#[test]
fn xml_reference_does_not_borrow_a_foreign_java_factory_annotation() {
    for source in ["import foreign.Bean; class Config { @Bean Object fake(){return null;} }","import org.springframework.context.annotation.Bean; @interface A { int[] value(); } class Config<@A({1}) Bean> { @Bean Object fake(){return null;} }","import foreign.*; class Config { @org.springframework.context.annotation.Bean Object fake(){return null;} }"] {
        let host=MockHost::install();host.add_file("Fake.java",source);
        let text="<beans xmlns='http://www.springframework.org/schema/beans'><bean id='owner'><property name='p' ref='fake'/></bean></beans>";host.add_file(XML_PATH,text);
        let mut nav=warmed_navigator();assert!(nav.definition(&doc(XML_PATH,text),position(text,text.find("fake").unwrap())).is_empty(),"{source}");
    }
}
#[test]
fn xml_reference_does_not_borrow_a_foreign_java_stereotype_annotation() {
    let host=MockHost::install();host.add_file("Fake.java","import foreign.Service; @Service(\"fake\") class ForeignService {}");
    let text="<beans xmlns='http://www.springframework.org/schema/beans'><bean id='owner'><property name='p' ref='fake'/></bean></beans>";host.add_file(XML_PATH,text);
    let mut nav=warmed_navigator();assert!(nav.definition(&doc(XML_PATH,text),position(text,text.find("fake").unwrap())).is_empty());
}
#[test]
fn method_generic_annotation_names_do_not_prove_java_factory_hover_owner() {
    use openidea_plugin::hover::HoverProvider;
    let host=MockHost::install();let text="import org.springframework.context.annotation.Bean; class Config { @Bean <Bean> Object factory(){return null;} }";
    host.add_file("Config.java",text);let mut nav=warmed_navigator();assert!(nav.hover(&doc("/project/Config.java",text),position(text,text.find("factory").unwrap())).is_none());
}
#[test]
fn xml_reference_does_not_borrow_a_foreign_or_aliased_kotlin_factory_annotation() {
    for source in ["import foreign.Bean\nclass Config { @Bean fun fake(): Any = Any() }","import org.springframework.context.annotation.Bean as Other\nclass Config { @Bean fun fake(): Any = Any() }","import org.springframework.context.annotation.Bean as `Other`\nclass Config { @Bean fun fake(): Any = Any() }","import foreign.Custom /*alias*/ as `org`\nclass Config { @org.springframework.context.annotation.Bean fun fake(): Any = Any() }"] {
        let host=MockHost::install();host.add_file("Fake.kt",source);let text="<beans xmlns='http://www.springframework.org/schema/beans'><bean id='owner'><property name='p' ref='fake'/></bean></beans>";host.add_file(XML_PATH,text);
        let mut nav=warmed_navigator();assert!(nav.definition(&doc(XML_PATH,text),position(text,text.find("fake").unwrap())).is_empty(),"{source}");
    }
}
#[test]
fn canonical_duplicate_import_same_package_and_kotlin_fqn_factory_owners_are_retained() {
    for (path,source) in [
        ("Known.java","import org.springframework.context.annotation.Bean; import org.springframework.context.annotation.Bean; import foreign.*; class Config { @Bean Object known(){return null;} }"),
        ("Known.java","package org.springframework.context.annotation; class Config { @Bean Object known(){return null;} }"),
        ("Known.java","import org.springframework.context.annotation.Bean; class Config<T extends org.example.Marker> { @Bean java.util.List<Bean> known(){return null;} }"),
        ("Known.kt","import org.springframework.context.annotation.Bean as Other\ntypealias Bean = foreign.Other\nclass Config { @org.springframework.context.annotation.Bean fun known(): Any = Any() }"),
    ] {
        let host=MockHost::install();host.add_file(path,source);let text="<beans xmlns='http://www.springframework.org/schema/beans'><bean id='owner'><property name='p' ref='known'/></bean></beans>";host.add_file(XML_PATH,text);
        let mut nav=warmed_navigator();assert_eq!(nav.definition(&doc(XML_PATH,text),position(text,text.find("known").unwrap())).len(),1,"{source}");
    }
}

#[test]
fn foreign_parent_bean_is_not_a_setter_navigation_owner() {
    let host=MockHost::install();host.add_file("Target.java","package a; class Target { public void setP(String p){} }");
    let text="<beans xmlns='http://www.springframework.org/schema/beans' xmlns:x='urn:foreign'><x:bean class='a.Target'><property name='p'/></x:bean></beans>";
    host.add_file(XML_PATH,text);let mut nav=warmed_navigator();let at=position(text,text.find("name='p'").unwrap()+6);
    assert!(nav.definition(&doc(XML_PATH,text),at).is_empty());
}

#[test]
fn foreign_primary_does_not_choose_an_injection_candidate() {
    let host=MockHost::install();
    host.add_file("Candidates.java","package a; import foreign.Primary; interface Notifier {} @org.springframework.stereotype.Service @Primary class First implements Notifier {} @org.springframework.stereotype.Service class Second implements Notifier {}");
    let text="package a; @org.springframework.stereotype.Service class Alerts { Alerts(Notifier notifier) {} }";
    let mut nav=warmed_navigator();
    assert_eq!(nav.definition(&java("Alerts.java",text),after(text,"(Noti",1)).len(),2);
}

#[test]
fn foreign_qualifier_literal_is_not_a_bean_navigation_site() {
    let host=MockHost::install();host.add_file("Target.java","@org.springframework.stereotype.Service(\"target\") class Target {}");
    let text="import foreign.Qualifier; class Client { @Qualifier(\"target\") Object value; }";
    let mut nav=warmed_navigator();
    assert!(nav.definition(&java("Client.java",text),position(text,text.find("target").unwrap()+1)).is_empty());
}

#[test]
fn owned_qualifier_literal_retains_navigation() {
    let host=MockHost::install();host.add_file("Target.java","@org.springframework.stereotype.Service(\"target\") class Target {}");
    for text in ["import org.springframework.beans.factory.annotation.Qualifier; class Client { @Qualifier(\"target\") Object value; }", "class Client { @org.springframework.beans.factory.annotation.Qualifier(\"target\") Object value; }"] {
        let mut nav=warmed_navigator();
        assert_eq!(nav.definition(&java("Client.java",text),position(text,text.find("target").unwrap()+1)).len(),1);
    }
}

#[test]
fn foreign_constructor_qualifier_does_not_select_an_injection_candidate() {
    let host=MockHost::install();host.add_file("Candidates.java","package a; interface Notifier {} @org.springframework.stereotype.Service class First implements Notifier {} @org.springframework.stereotype.Service class Second implements Notifier {}");
    let text="package a; import foreign.Qualifier; @org.springframework.stereotype.Service class Alerts { Alerts(@Qualifier(\"first\") Notifier notifier) {} }";
    let mut nav=warmed_navigator();assert_eq!(nav.definition(&java("Alerts.java",text),position(text,text.rfind("Notifier").unwrap()+1)).len(),2);
}

#[test]
fn kotlin_parameter_qualifier_requires_canonical_owner() {
    for (source, expected) in [("import foreign.Qualifier\n@org.springframework.stereotype.Service class Client(@Qualifier(\"first\") val notifier: Notifier) {}",None), ("import org.springframework.beans.factory.annotation.Qualifier\n@org.springframework.stereotype.Service class Client(@Qualifier(\"first\") val notifier: Notifier) {}",Some("first"))] {
        let model=parse("Client.kt",source).unwrap();assert_eq!(model.injections.len(),1);assert_eq!(model.injections[0].qualifier.as_deref(),expected);
    }
}

#[test]
fn foreign_profile_does_not_hide_duplicate_bean_names() {
    let (host,mut nav)=with_other("import foreign.Profile; @Profile(\"prod\") @org.springframework.stereotype.Component(\"dispatcher\") class Other {}");
    let text="import foreign.Profile; @Profile(\"dev\") @org.springframework.stereotype.Service(\"dispatcher\") class SmtpMailer {}";
    assert!(findings(&host,&mut nav,"Subject.java",text).iter().any(|(code,_)|*code=="duplicate-bean-name"));
}

#[test]
fn foreign_annotation_config_does_not_enable_required_setter_checks() {
    let source=POOL.1.replace("@Required","@org.springframework.beans.factory.annotation.Required");
    let body="<x:annotation-config xmlns:x='urn:foreign'/><bean class='com.acme.Pool'><constructor-arg value='1'/></bean>";
    assert!(class_findings(body,&[(POOL.0,&source)]).iter().all(|(code,_)|code!="xml-required-property"));
}

#[test]
fn required_setter_shortcuts_follow_namespace_bindings() {
    let source=POOL.1.replace("@Required","@org.springframework.beans.factory.annotation.Required");
    for (binding, expected) in [("xmlns:p='urn:foreign' p:url='u'",true),("xmlns:q='http://www.springframework.org/schema/p' q:url='u'",false)] {
        let body=format!("<context:annotation-config/><bean class='com.acme.Pool' {binding}><constructor-arg value='1'/></bean>");
        assert_eq!(class_findings(&body,&[(POOL.0,&source)]).iter().any(|(code,_)|code=="xml-required-property"),expected);
    }
}
#[test]
fn foreign_c_prefix_does_not_suppress_constructor_argument_check() {
    let body="<bean class='com.acme.Pool' xmlns:c='urn:foreign' c:other='x'><constructor-arg value='1'/><constructor-arg value='2'/></bean>";
    assert!(class_findings(body,&[POOL]).iter().any(|(code,_)|code=="xml-constructor-args"));
}

#[test]
fn foreign_lookup_child_does_not_emit_a_spring_method_check() {
    let body="<bean class='com.acme.Pool'><x:lookup-method xmlns:x='urn:foreign' name='missing' bean='target'/></bean>";
    assert!(class_findings(body,&[POOL]).iter().all(|(code,_)|code!="xml-lookup-method"));
}
#[test]
fn foreign_constructor_children_do_not_count_as_spring_arguments() {
    let body="<bean class='com.acme.Pool' xmlns:x='urn:foreign'><constructor-arg value='1'/><x:constructor-arg value='2'/></bean>";
    assert!(class_findings(body,&[POOL]).iter().all(|(code,_)|code!="xml-constructor-args"));
}

#[test]
fn foreign_resource_is_not_an_injection_or_literal_site() {
    let host=MockHost::install();host.add_file("Target.java","@org.springframework.stereotype.Service(\"target\") class Target {}");
    let text="import foreign.Resource; @org.springframework.stereotype.Service class Client { @Resource(name=\"target\") Target field; }";
    let mut nav=warmed_navigator();
    for at in [text.rfind("Target").unwrap()+1,text.find("target").unwrap()+1] {
        assert!(nav.definition(&java("Client.java",text),position(text,at)).is_empty());
    }
}
#[test]
fn canonical_resource_retains_injection_and_literal_navigation() {
    let host=MockHost::install();host.add_file("Target.java","@org.springframework.stereotype.Service(\"target\") class Target {}");
    for owner in ["javax.annotation.Resource","jakarta.annotation.Resource"] {
        let text=format!("import {owner}; @org.springframework.stereotype.Service class Client {{ @Resource(name=\"target\") Target field; }}");let mut nav=warmed_navigator();
        for at in [text.rfind("Target").unwrap()+1,text.find("target").unwrap()+1] {assert_eq!(nav.definition(&java("Client.java",&text),position(&text,at)).len(),1);}
    }
}

#[test]
fn foreign_named_does_not_select_an_injection_candidate() {
    let host=MockHost::install();host.add_file("Candidates.java","package a; interface Notifier {} @org.springframework.stereotype.Service class First implements Notifier {} @org.springframework.stereotype.Service class Second implements Notifier {}");
    let text="package a; import foreign.Named; @org.springframework.stereotype.Service class Alerts { Alerts(@Named(\"first\") Notifier notifier) {} }";let mut nav=warmed_navigator();
    assert_eq!(nav.definition(&java("Alerts.java",text),position(text,text.rfind("Notifier").unwrap()+1)).len(),2);
    assert!(nav.definition(&java("Alerts.java",text),position(text,text.find("first").unwrap()+1)).is_empty());
}
#[test]
fn canonical_named_retains_injection_and_literal_navigation() {
    let host=MockHost::install();host.add_file("Candidates.java","package a; interface Notifier {} @org.springframework.stereotype.Service class First implements Notifier {} @org.springframework.stereotype.Service class Second implements Notifier {}");
    for owner in ["javax.inject.Named","jakarta.inject.Named"] {
        let text=format!("package a; import {owner}; @org.springframework.stereotype.Service class Alerts {{ Alerts(@Named(\"first\") Notifier notifier) {{}} }}");let mut nav=warmed_navigator();
        for at in [text.rfind("Notifier").unwrap()+1,text.find("first").unwrap()+1] {assert_eq!(nav.definition(&java("Alerts.java",&text),position(&text,at)).len(),1);}
    }
}

#[test]
fn foreign_autowired_does_not_offer_constructor_rewrite() {
    let _host=MockHost::install();let text="import foreign.Autowired;\n@org.springframework.stereotype.Service class Client {\n @Autowired Target target;\n}";
    let mut nav=Navigator::default();let at=position(text,text.find("@Autowired").unwrap()+1);
    assert!(nav.code_actions(&java("Client.java",text),Range{start:at,end:at}).is_empty());
}
#[test]
fn foreign_autowired_does_not_emit_a_spring_field_injection_diagnostic() {
    let _host=MockHost::install();let text="import foreign.Autowired;\n@org.springframework.stereotype.Service class Client {\n @Autowired Target target;\n}";
    assert!(Navigator::default().diagnostics(&java("Client.java",text)).iter().all(|d|d.code.as_deref()!=Some("field-injection")));
}

#[test]
fn foreign_inject_does_not_offer_constructor_rewrite() {
    let _host=MockHost::install();let text="import foreign.Inject;\n@org.springframework.stereotype.Service class Client {\n @Inject Target target;\n}";let mut nav=Navigator::default();let at=position(text,text.find("@Inject").unwrap()+1);
    assert!(nav.code_actions(&java("Client.java",text),Range{start:at,end:at}).is_empty());
}
#[test]
fn foreign_inject_does_not_create_an_injection_navigation_fact() {
    let host=MockHost::install();host.add_file("Target.java","@org.springframework.stereotype.Service class Target {}");let text="import foreign.Inject;\n@org.springframework.stereotype.Service class Client {\n @Inject Target target;\n}";let mut nav=warmed_navigator();
    assert!(nav.definition(&java("Client.java",text),position(text,text.rfind("Target").unwrap()+1)).is_empty());
}
#[test]
fn canonical_inject_retains_navigation_and_constructor_rewrite() {
    let host=MockHost::install();host.add_file("Target.java","@org.springframework.stereotype.Service class Target {}");
    for owner in ["javax.inject.Inject","jakarta.inject.Inject"] {
        let text=format!("import {owner};\n@org.springframework.stereotype.Service class Client {{\n @Inject Target target;\n}}");let mut nav=warmed_navigator();let at=position(&text,text.find("@Inject").unwrap()+1);
        assert_eq!(nav.definition(&java("Client.java",&text),position(&text,text.rfind("Target").unwrap()+1)).len(),1);
        assert_eq!(nav.code_actions(&java("Client.java",&text),Range{start:at,end:at}).len(),1);
    }
}

#[test]
fn foreign_value_does_not_hide_a_canonical_autowired_injection() {
    let host=MockHost::install();host.add_file("Target.java","@org.springframework.stereotype.Service class Target {}");let text="import foreign.Value; @org.springframework.stereotype.Service class Client { @Value(\"ignored\") @org.springframework.beans.factory.annotation.Autowired Target target; }";let mut nav=warmed_navigator();
    assert_eq!(nav.definition(&java("Client.java",text),position(text,text.rfind("Target").unwrap()+1)).len(),1);
}
#[test]
fn foreign_value_expression_is_not_a_spring_navigation_site() {
    let host=MockHost::install();host.add_file("Target.java","@org.springframework.stereotype.Service(\"target\") class Target {}");let text="import foreign.Value; class Client { @Value(\"#{@target}\") Object value; }";let mut nav=warmed_navigator();
    assert!(nav.definition(&java("Client.java",text),position(text,text.find("target").unwrap()+1)).is_empty());
}

#[test]
fn canonical_value_retains_expression_navigation_and_configuration_exclusion() {
    let host=MockHost::install();host.add_file("Target.java","@org.springframework.stereotype.Service(\"target\") class Target {}");
    for (header, annotation) in [("import org.springframework.beans.factory.annotation.Value;", "Value"),("", "org.springframework.beans.factory.annotation.Value")] {
        let text=format!("{header} class Client {{ @{annotation}(\"#{{@target}}\") Object value; }}");let mut nav=warmed_navigator();assert_eq!(nav.definition(&java("Client.java",&text),position(&text,text.find("target").unwrap()+1)).len(),1);
        let text=format!("{header} @org.springframework.stereotype.Service class Client {{ @{annotation}(\"configured\") @org.springframework.beans.factory.annotation.Autowired Target target; }}");assert!(nav.definition(&java("Client.java",&text),position(&text,text.rfind("Target").unwrap()+1)).is_empty());
    }
}

#[test]
fn foreign_value_metadata_does_not_borrow_property_navigation() {
    let host=MockHost::install();host.add_file("application.properties","app.name=x\n");let text="import foreign.Value; class Client { @Value(\"${app.name}\") String value; }";let mut nav=warmed_navigator();
    assert!(nav.definition(&java("Client.java",text),position(text,text.find("app.name").unwrap()+1)).is_empty());
}
#[test]
fn foreign_profile_metadata_does_not_borrow_config_navigation() {
    let host=MockHost::install();host.add_file("application-prod.properties","key=x\n");
    for (path,text) in [("Client.java","import foreign.Profile; @Profile(\"prod\") class Client {}"),("Client.kt","import foreign.Profile\n@Profile(\"prod\") class Client {}") ] {let mut nav=warmed_navigator();assert!(nav.definition(&java(path,text),position(text,text.find("prod").unwrap()+1)).is_empty());}
}
#[test]
fn canonical_metadata_annotation_navigation_is_retained() {
    let host=MockHost::install();host.add_file("application.properties","app.name=x\n");host.add_file("application-prod.properties","key=x\n");
    for (path,text,needle) in [("Client.java","class Client { @org.springframework.beans.factory.annotation.Value(\"${app.name}\") String value; }","app.name"),("Client.java","@org.springframework.context.annotation.Profile(\"prod\") class Client {}","prod"),("Client.kt","@org.springframework.context.annotation.Profile(\"prod\") class Client {}","prod")] {let mut nav=warmed_navigator();assert_eq!(nav.definition(&java(path,text),position(text,text.find(needle).unwrap()+1)).len(),1);}
}

#[test]
fn foreign_xml_metadata_roles_do_not_borrow_navigation() {
    let host=MockHost::install();host.add_file("application-prod.properties","key=x\n");host.add_file("extra.properties","key=x\n");host.add_file("extra.xml","<beans xmlns='http://www.springframework.org/schema/beans'/>");host.add_file("Billing.java","package com.acme.billing; class Billing {}");
    let mut bad=Vec::new();
    for (label,body,needle) in [
      ("profile-attribute","<beans xmlns:x='urn:foreign' x:profile='prod'/>","prod"),
      ("profile-element","<x:beans xmlns:x='urn:foreign' profile='prod'/>","prod"),
      ("import-attribute","<import xmlns:x='urn:foreign' x:resource='extra.xml'/>","extra.xml"),
      ("import-element","<x:import xmlns:x='urn:foreign' resource='extra.xml'/>","extra.xml"),
      ("scan-attribute","<context:component-scan xmlns:x='urn:foreign' x:base-package='com.acme.billing'/>","com.acme.billing"),
      ("scan-element","<x:component-scan xmlns:x='urn:foreign' base-package='com.acme.billing'/>","com.acme.billing"),
      ("placeholder-attribute","<context:property-placeholder xmlns:x='urn:foreign' x:location='extra.properties'/>","extra.properties"),
      ("placeholder-element","<x:property-placeholder xmlns:x='urn:foreign' location='extra.properties'/>","extra.properties")
    ] {let text=beans_xml(body);let mut nav=warmed_navigator();if !nav.definition(&doc(XML_PATH,&text),position(&text,text.find(needle).unwrap()+1)).is_empty(){bad.push(label);}}
    assert!(bad.is_empty(),"foreign metadata links: {bad:?}");
}
#[test]
fn canonical_xml_metadata_roles_retain_navigation() {
    let host=MockHost::install();host.add_file("application-prod.properties","key=x\n");host.add_file("extra.properties","key=x\n");host.add_file("extra.xml","<beans xmlns='http://www.springframework.org/schema/beans'/>");host.add_file("Billing.java","package com.acme.billing; class Billing {}");
    for (body,needle) in [("<beans profile='prod'/>","prod"),("<import resource='extra.xml'/>","extra.xml"),("<q:component-scan xmlns:q='http://www.springframework.org/schema/context' base-package='com.acme.billing'/>","com.acme.billing"),("<q:property-placeholder xmlns:q='http://www.springframework.org/schema/context' location='extra.properties'/>","extra.properties")] {let text=beans_xml(body);let mut nav=warmed_navigator();assert_eq!(nav.definition(&doc(XML_PATH,&text),position(&text,text.find(needle).unwrap()+1)).len(),1);}
}

fn inactive_beans(files: &[(&str, &str)]) -> Vec<String> {
    let host = MockHost::install();
    for (path, text) in files {
        host.add_file(path, text);
    }
    let mut navigator = Navigator::default();
    let project = host.invocation().project().unwrap();
    let index = navigator.indexes.get(&project, COMMAND_BUDGET);
    assert!(index.complete());
    let world = World::new(index);
    let mut out: Vec<String> = world.beans().filter(|(p, b)| world.inactive_reason(p, b).is_some()).map(|(_, b)| b.name.clone()).collect();
    out.sort();
    out
}

const ON_PROPERTY: &str = "import org.springframework.boot.autoconfigure.condition.ConditionalOnProperty;\nimport org.springframework.stereotype.Service;\n@Service @ConditionalOnProperty(prefix = \"app\", name = \"mail\", havingValue = \"smtp\") class Smtp {}\n";

#[test]
fn conditional_on_property_is_inactive_only_when_the_literal_differs() {
    let java = ("src/main/java/Smtp.java", ON_PROPERTY);
    assert_eq!(inactive_beans(&[java, ("src/main/resources/application.properties", "app.mail=ses\n")]), ["smtp"]);
    assert_eq!(inactive_beans(&[java, ("src/main/resources/application.yml", "app:\n  mail: ses\n")]), ["smtp"]);
    assert!(inactive_beans(&[java, ("src/main/resources/application.properties", "app.mail=SMTP\n")]).is_empty());
    assert!(inactive_beans(&[java]).is_empty());
    assert!(inactive_beans(&[java, ("src/main/resources/application.properties", "app.mail=${MAIL}\n")]).is_empty());
    let twice = ("src/test/resources/application.properties", "app.mail=smtp\n");
    assert!(inactive_beans(&[java, ("src/main/resources/application.properties", "app.mail=ses\n"), twice]).is_empty());
    let profile = ("src/main/resources/application-dev.properties", "app.mail=smtp\n");
    assert!(inactive_beans(&[java, ("src/main/resources/application.properties", "app.mail=ses\n"), profile]).is_empty());
    let imported = ("src/main/resources/application.properties", "app.mail=ses\nspring.config.import=optional:x.yml\n");
    assert!(inactive_beans(&[java, imported]).is_empty());
}

#[test]
fn conditional_on_property_without_having_value_rejects_only_false() {
    let text = ON_PROPERTY.replace(", havingValue = \"smtp\"", "");
    let java = ("src/main/java/Smtp.java", text.as_str());
    assert_eq!(inactive_beans(&[java, ("src/main/resources/application.properties", "app.mail=false\n")]), ["smtp"]);
    assert!(inactive_beans(&[java, ("src/main/resources/application.properties", "app.mail=true\n")]).is_empty());
}

#[test]
fn foreign_conditional_on_property_is_not_evaluated() {
    let text = ON_PROPERTY.replace("org.springframework.boot.autoconfigure.condition", "acme");
    assert!(inactive_beans(&[("src/main/java/Smtp.java", &text), ("src/main/resources/application.properties", "app.mail=ses\n")]).is_empty());
}

#[test]
fn profile_is_evaluated_against_a_single_literal_active_profile() {
    let java = (
        "src/main/java/Dev.java",
        "@org.springframework.stereotype.Service @org.springframework.context.annotation.Profile(\"dev\") class Dev {}\n@org.springframework.stereotype.Service @org.springframework.context.annotation.Profile(\"!prod\") class NotProd {}\n@org.springframework.stereotype.Service @org.springframework.context.annotation.Profile(\"a & b\") class Both {}\n",
    );
    let app = |v: &str| format!("spring.profiles.active={v}\n");
    assert_eq!(inactive_beans(&[java, ("src/main/resources/application.properties", &app("prod"))]), ["dev", "notProd"]);
    assert!(inactive_beans(&[java, ("src/main/resources/application.properties", &app("dev, x"))]).is_empty());
    assert!(inactive_beans(&[java]).is_empty());
    assert!(inactive_beans(&[java, ("src/main/resources/application.properties", &app("${P}"))]).is_empty());
    let group = format!("{}spring.profiles.group.prod=dev\n", app("prod"));
    assert!(inactive_beans(&[java, ("src/main/resources/application.properties", &group)]).is_empty());
}

#[test]
fn missing_bean_is_inactive_when_a_plain_component_of_the_type_exists() {
    let config = "import org.springframework.boot.autoconfigure.condition.ConditionalOnMissingBean;\nimport org.springframework.context.annotation.Bean;\n@org.springframework.context.annotation.Configuration class Cfg {\n @Bean @ConditionalOnMissingBean Clock clock() { return null; }\n @Bean @ConditionalOnMissingBean(Mailer.class) Object other() { return null; }\n @Bean @ConditionalOnMissingBean(name = \"x\") Clock named() { return null; }\n}\n";
    let component = "@org.springframework.stereotype.Component class Clock {}\n";
    let cfg = ("src/main/java/Cfg.java", config);
    assert_eq!(inactive_beans(&[cfg, ("src/main/java/Clock.java", component)]), ["clock"]);
    assert!(inactive_beans(&[cfg]).is_empty());
    let profiled = "@org.springframework.stereotype.Component @org.springframework.context.annotation.Profile(\"x\") class Clock {}\n";
    assert!(inactive_beans(&[cfg, ("src/main/java/Clock.java", profiled)]).is_empty());
}

#[test]
fn inactive_beans_leave_the_candidates_of_an_injection() {
    let host = MockHost::install();
    host.add_file("src/main/java/Smtp.java", &ON_PROPERTY.replace("class Smtp {}", "class Smtp implements Mailer {}"));
    host.add_file("src/main/java/Ses.java", "@org.springframework.stereotype.Service class Ses implements Mailer {}\n");
    host.add_file("src/main/resources/application.properties", "app.mail=ses\n");
    let mut navigator = Navigator::default();
    let project = host.invocation().project().unwrap();
    let index = navigator.indexes.get(&project, COMMAND_BUDGET);
    let world = World::new(index);
    let injection = |ty: &str, multiple: bool| model::Injection {
        ty: ty.into(),
        name: "mailer".into(),
        qualifier: None,
        multiple,
        range: Default::default(),
        owner: None,
        field_annotation: None,
    };
    let names = |multiple| -> Vec<String> { world.candidates(&injection("Mailer", multiple)).iter().map(|(_, b)| b.name.clone()).collect() };
    assert_eq!(names(false), ["ses"]);
    assert_eq!(names(true), ["ses"]);
}

#[test]
fn object_factory_and_sorted_set_unwrap_like_spring_dependency_types() {
    let text = "@org.springframework.stereotype.Service class A { A(ObjectFactory<Foo> f, SortedSet<Bar> b, Optional<List<Baz>> z) {} }\n";
    let model = model::parse("A.java", text).unwrap();
    assert_eq!(injected(&model).iter().map(|i| (i.0, i.2)).collect::<Vec<_>>(), [("Foo", false), ("Bar", true), ("Baz", true)]);
}

#[test]
fn bean_marker_icon_is_declared_in_the_manifest() {
    let manifest = include_str!("../plugin.toml");
    assert!(manifest.contains(&format!("icon = \"{BEAN_MARKER}\"")));
    assert!(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(BEAN_MARKER).is_file());
}

#[test]
fn conditional_on_missing_bean_build_files_memoized() {
    let host = MockHost::install();
    let config = "@org.springframework.context.annotation.Configuration class Cfg {\n @org.springframework.context.annotation.Bean @org.springframework.boot.autoconfigure.condition.ConditionalOnMissingBean Clock clock() { return null; }\n}\n";
    host.add_file("app/src/main/java/Cfg.java", config);
    host.add_file("lib/src/main/java/SystemClock.java", "@org.springframework.stereotype.Component class SystemClock extends Clock {}\n");
    let consumer = "@org.springframework.stereotype.Service class Consumer {\n @org.springframework.beans.factory.annotation.Autowired Clock clock;\n}\n";
    host.add_file("app/src/main/java/Consumer.java", consumer);
    let mut nav = warmed_navigator();
    let source = doc("/project/app/src/main/java/Consumer.java", consumer);
    let before = nav.line_markers(&source);
    assert!(before.iter().any(|m| m.line == 1 && m.target.as_ref().is_some_and(|t| t.path.ends_with("/SystemClock.java"))), "{before:?}");
    host.add_file("app/pom.xml", "<project/>");
    host.add_file("lib/pom.xml", "<project/>");
    assert_eq!(nav.line_markers(&source), before, "build modules are looked up once, not on every provider call");
}

#[test]
fn cross_file_navigation_with_600_models() {
    let host = MockHost::install();
    for n in 0..600 {
        host.add_file(&format!("src/main/java/p/Filler{n}.java"), &format!("package p; class Filler{n} {{}}\n"));
    }
    host.add_file("src/main/java/p/Repo.java", "package p; @org.springframework.stereotype.Repository class Repo {}\n");
    let consumer = "package p;\n@org.springframework.stereotype.Service class Consumer {\n @org.springframework.beans.factory.annotation.Autowired Repo repo;\n}\n";
    host.add_file("src/main/java/p/Consumer.java", consumer);
    let mut nav = warmed_navigator();
    let project = host.invocation().project().unwrap();
    assert!(nav.indexes.cached(&project).unwrap().files().len() > 600);
    let source = doc("/project/src/main/java/p/Consumer.java", consumer);
    let markers = nav.line_markers(&source);
    assert!(markers.iter().any(|m| m.line == 2 && m.target.as_ref().is_some_and(|t| t.path.ends_with("/Repo.java"))), "{markers:?}");
    let at = position(consumer, consumer.find("Repo repo").unwrap() + 1);
    assert!(nav.definition(&source, at).iter().any(|l| l.path.ends_with("/Repo.java")));
}

#[test]
fn same_document_version_is_applied_and_parsed_once() {
    let host = setup();
    let mut nav = warmed_navigator();
    let source = java("UserController.java", CONTROLLER);
    let markers = nav.line_markers(&source);
    let generation = nav.indexes.document(&source.path).unwrap().generation();
    nav.diagnostics(&source);
    nav.definition(&source, Position::new(5, 20));
    nav.completion(&source, Position::new(5, 20), None);
    assert_eq!(nav.indexes.document(&source.path).unwrap().generation(), generation, "one version is applied once");
    host.add_file("src/main/java/com/acme/UserServiceImpl.java", "class Changed {}");
    assert_eq!(nav.line_markers(&source), markers, "providers read the cache, never the disk");
}

#[test]
fn large_sql_seed_keeps_index_complete() {
    let host = MockHost::install();
    host.add_file("src/main/java/A.java", "@org.springframework.stereotype.Component class A {}\n");
    host.add_file("src/main/resources/data.sql", &"insert into t values (1);\n".repeat(30_000));
    host.add_file("src/test/resources/fixture.xml", "<root><a:b/></root>");
    host.add_file_bytes("src/main/resources/messages.properties", b"greeting=caf\xe9\n");
    let mut nav = warmed_navigator();
    let project = host.invocation().project().unwrap();
    let cache = nav.indexes.cached(&project).unwrap();
    assert!(cache.complete(), "{}", cache.state());
    assert!(cache.files().contains_key("/project/src/main/java/A.java"));
    host.add_file_bytes("src/main/java/B.java", b"class B { String s = \"caf\xe9\"; }");
    nav.indexes.changed(&project, "/project/src/main/java/B.java", false);
    assert!(!nav.indexes.cached(&project).unwrap().complete(), "an unreadable Java source still counts");
}

#[test]
fn check_configuration_runs_in_a_task() {
    let host = MockHost::install();
    host.add_file("src/main/java/A.java", "@org.springframework.stereotype.Component class A {}\n");
    Navigator::default().run_command("check-spring-configuration", &host.invocation(), None).unwrap();
    assert!(!host.tasks().is_empty());
}

#[test]
fn yaml_not_parsed_as_java() {
    let host = MockHost::install();
    let scheduled = "class Jobs { @org.springframework.scheduling.annotation.Scheduled(fixedRate = 1) void run(String a) {} }\n";
    host.add_file("src/main/java/Jobs.java", scheduled);
    host.add_file("src/main/resources/application.yml", scheduled);
    host.add_file("src/main/resources/application.properties", scheduled);
    let mut nav = Navigator::default();
    nav.run_command("check-spring-configuration", &host.invocation(), None).unwrap();
    let Report::Problems(files) = &nav.report else { panic!("problems report") };
    assert!(files.iter().any(|(path, _)| path.ends_with("Jobs.java")), "the Java source is checked");
    assert!(files.iter().all(|(path, _)| path.ends_with(".java")), "{files:?}");
}

#[test]
fn generate_setter_on_20kib_file() {
    let filler: String = (0..1200).map(|n| format!("    void m{n}() {{}}\n")).collect();
    let text = format!("package com.acme;\n\npublic class Orders {{\n    private int count;\n{filler}}}\n");
    assert!(text.len() > 20 * 1024);
    let host = command_host(&text);
    run(&host, "generate-setter", &[&["0"], &["ok"]]).unwrap();
    let result = host.editor_text();
    assert!(result.contains("    private Mailer mailer;\n") && result.contains("public void setMailer(Mailer mailer)"), "{}", &result[..400]);
}

#[test]
fn no_repository_bean_not_a_bean() {
    let java = "@org.springframework.data.repository.NoRepositoryBean\ninterface BaseRepository<T> extends JpaRepository<T, Long> {}\ninterface Users extends BaseRepository<User> {}\n";
    let names: Vec<String> = model::parse("A.java", java).unwrap().beans.iter().map(|b| b.name.clone()).collect();
    assert_eq!(names, ["users"]);
    let kotlin = "@org.springframework.data.repository.NoRepositoryBean\ninterface BaseRepository<T> : JpaRepository<T, Long>\ninterface Users : BaseRepository<User>\n";
    let names: Vec<String> = model::parse("A.kt", kotlin).unwrap().beans.iter().map(|b| b.name.clone()).collect();
    assert_eq!(names, ["users"]);
}

#[test]
fn target_classes_copy_ignored() {
    let host = MockHost::install();
    let beans = "<beans xmlns=\"http://www.springframework.org/schema/beans\"><bean id=\"worker\" class=\"a.Worker\"/></beans>";
    host.add_file("src/main/resources/beans.xml", beans);
    host.add_file("target/classes/beans.xml", beans);
    host.add_file("module/build/resources/main/application.properties", "spring.profiles.active=dev\n");
    host.add_file("src/main/java/a/build/Job.java", "package a.build; @org.springframework.stereotype.Component class Job {}\n");
    let mut nav = warmed_navigator();
    let project = host.invocation().project().unwrap();
    let cache = nav.indexes.cached(&project).unwrap();
    assert!(cache.complete(), "{}", cache.state());
    assert!(cache.files().contains_key("/project/src/main/resources/beans.xml"));
    assert!(cache.files().contains_key("/project/src/main/java/a/build/Job.java"), "a source package named build is kept");
    assert!(cache.files().keys().all(|p| !p.starts_with("/project/target/") && !p.contains("/module/build/")), "{:?}", cache.files().keys());
    nav.diagnostics(&doc("/project/target/classes/beans.xml", beans));
    assert!(!nav.indexes.cached(&project).unwrap().files().contains_key("/project/target/classes/beans.xml"));
}

#[test]
fn profiles_storage_denied_message() {
    let host = MockHost::install().with_capabilities(&["project.read", "editor.read"]);
    host.add_file("src/main/resources/application-prod.yml", "a: b\n");
    host.answer_next_prompt(Some(&["prod"]));
    assert!(warmed_navigator().run_command("change-active-profiles", &host.invocation(), None).is_ok());
    assert!(
        host.notifications().iter().any(|n| n.body == "Remembering active profiles needs the storage permission. Allow it in Settings › Plugins › Spring Beans Navigator."),
        "{:?}",
        host.notifications()
    );
    let denied_set = MockHost::install().with_capabilities(&["project.read", "editor.read"]);
    assert!(!commands::save_profiles(&["prod".to_owned()]).unwrap(), "a denied set is a notice, not an error");
    assert!(denied_set.notifications().iter().any(|n| n.body == "Remembering active profiles needs the storage permission. Allow it in Settings › Plugins › Spring Beans Navigator."));
    let allowed = MockHost::install();
    assert!(commands::save_profiles(&["prod".to_owned()]).unwrap());
    assert_eq!(allowed.storage()["activeProfiles"], "prod");
}

#[test]
fn reviewed_edits_show_changed_lines_with_three_lines_of_context() {
    let text: String = (1..=20).map(|n| format!("line {n}\n")).collect();
    let at = |line: u32| Position::new(line, 0);
    let edits = [
        TextEdit { range: Range::new(at(1), at(1)), text: "added A\n".into() },
        TextEdit { range: Range::new(at(15), at(16)), text: "replaced 16\n".into() },
    ];
    let (after, shown) = reviewed(&text, &edits).unwrap();
    assert!(after.contains("line 1\nadded A\nline 2\n") && after.contains("line 15\nreplaced 16\nline 17\n"));
    assert_eq!(shown, "Lines 1–5:\nline 1\nadded A\nline 2\nline 3\nline 4\n\nLines 14–20:\nline 13\nline 14\nline 15\nreplaced 16\nline 17\nline 18\nline 19");
    assert!(!shown.contains("line 9"));
    let overlapping = [edits[1].clone(), TextEdit { range: Range::new(at(15), at(17)), text: String::new() }];
    assert!(matches!(reviewed(&text, &overlapping), Err(Error::Stale)));
}

#[test]
fn providers_reuse_lookups_of_one_generation() {
    let host = setup();
    let mut nav = warmed_navigator();
    let source = java("UserController.java", CONTROLLER);
    let markers = nav.line_markers(&source);
    nav.diagnostics(&source);
    let built = nav.indexes.document(&source.path).unwrap().lookups(|| panic!("lookups rebuilt for an unchanged cache"));
    nav.definition(&source, Position::new(5, 20));
    nav.completion(&source, Position::new(5, 20), None);
    assert_eq!(nav.line_markers(&source), markers);
    let again = nav.indexes.document(&source.path).unwrap().lookups(|| panic!("lookups rebuilt for an unchanged cache"));
    assert!(Rc::ptr_eq(&built, &again));
    host.add_file("src/main/java/com/acme/Extra.java", "@org.springframework.stereotype.Component class Extra {}");
    nav.indexes.changed(&host.invocation().project().unwrap(), "/project/src/main/java/com/acme/Extra.java", false);
    let rebuilt = nav.indexes.document(&source.path).unwrap().lookups(|| Lookups::build(&BTreeMap::new(), false, &RefCell::default()));
    assert!(!Rc::ptr_eq(&built, &rebuilt), "a changed cache gets new lookups");
}


