use super::*;
use openidea_plugin::language_features::LanguageFeatures;
use openidea_plugin::{mock::MockHost, text::position};

const CLIENT: &str = r#"import org.springframework.cloud.openfeign.FeignClient;
import org.springframework.web.bind.annotation.GetMapping;
@FeignClient(name = "users")
interface UserClient {
    @GetMapping("/users/{id}") String find();
}
"#;
const SERVER: &str = r#"import org.springframework.web.bind.annotation.RestController;
import org.springframework.web.bind.annotation.GetMapping;
@RestController
class UserController {
    @GetMapping("/users/{id}") String get() { return ""; }
}
"#;
const CLIENT_PATH: &str = "/project/client/src/main/java/UserClient.java";
const USERS: &str = "/project/users/src/main/java/UserController.java";
const ADMIN: &str = "/project/admin/src/main/java/UserController.java";

fn setup() -> (MockHost, CloudTools, Document) {
    let host = MockHost::install().with_capabilities(&["project.read", "editor.read"]);
    host.add_file(CLIENT_PATH, CLIENT);
    host.add_file(USERS, SERVER);
    host.add_file(ADMIN, SERVER);
    host.add_file("users/src/main/resources/application.properties", "spring.application.name=users\n");
    host.add_file("admin/src/main/resources/application.yaml", "spring:\n  application:\n    name: admin\n");
    host.open_editor(CLIENT_PATH, "java", CLIENT, &[]);
    let editor = host.invocation().editor().unwrap();
    let source = editor.document().unwrap();
    let doc = Document { path: editor.path(), language: editor.language(), text: source.text, version: source.version };
    (host, CloudTools::default(), doc)
}

#[test]
fn same_route_in_two_services_resolves_only_the_named_service() {
    let (_host, mut tools, doc) = setup();
    let targets = tools.definition(&doc, position(CLIENT, CLIENT.find("find()").unwrap()));
    assert_eq!(targets.iter().map(|l| l.path.as_str()).collect::<Vec<_>>(), [USERS]);
    let markers = tools.line_markers(&doc);
    assert_eq!(markers.len(), 1);
    assert_eq!(markers[0].target.as_ref().unwrap().path, USERS);
}

#[test]
fn reverse_gutter_does_not_link_a_client_of_another_service() {
    let (_host, mut tools, _) = setup();
    let server = Document { path: ADMIN.into(), language: "java".into(), text: SERVER.into(), version: 1 };
    assert!(tools.line_markers(&server).is_empty());
}

fn targets(tools: &mut CloudTools, doc: &Document) -> Vec<String> {
    tools.definition(doc, position(&doc.text, doc.text.find("find()").unwrap())).into_iter().map(|l| l.path).collect()
}

#[test]
fn aliases_kotlin_and_http_shape_keep_the_same_service_constraint() {
    for annotation in [
        r#"@FeignClient("users")"#,
        r#"@FeignClient(value = "users")"#,
        r#"@FeignClient(name = "users", value = "users", contextId = "admin")"#,
    ] {
        let (_host, mut tools, mut doc) = setup();
        doc.text = CLIENT.replace(r#"@FeignClient(name = "users")"#, annotation);
        assert_eq!(targets(&mut tools, &doc), [USERS], "{annotation}");
        doc.text = doc.text.replace("GetMapping", "PostMapping");
        assert!(targets(&mut tools, &doc).is_empty());
    }
    let (host, mut tools, mut doc) = setup();
    doc.path = "/project/client/src/main/kotlin/UserClient.kt".into();
    doc.language = "kotlin".into();
    doc.text = "import org.springframework.cloud.openfeign.FeignClient\nimport org.springframework.web.bind.annotation.GetMapping\n@FeignClient(value = \"users\")\ninterface UserClient {\n @GetMapping(\"/users/{id}\") fun find(): String\n}".into();
    host.add_file("users/src/main/kotlin/OtherController.kt", "import org.springframework.web.bind.annotation.RestController\nimport org.springframework.web.bind.annotation.GetMapping\n@RestController class OtherController { @GetMapping(\"/other\") fun get(): String = \"\" }");
    assert_eq!(targets(&mut tools, &doc), [USERS]);
    doc.text = doc.text.replace("/users/{id}", "/other");
    assert_eq!(targets(&mut tools, &doc), ["/project/users/src/main/kotlin/OtherController.kt"]);
}

#[test]
fn unresolved_client_identity_and_url_override_keep_path_candidates() {
    for annotation in [
        r#"@FeignClient(name = "${service.name}")"#,
        r#"@FeignClient(name = SERVICE)"#,
        r#"@FeignClient(name = "users", value = "admin")"#,
        r#"@FeignClient(name = "users", name = "admin")"#,
        r#"@FeignClient(name = "users", url = "https://example.test")"#,
        r#"@FeignClient(name = "users", url = "${service.url:}")"#,
        r#"@FeignClient(name = "users" + SUFFIX)"#,
        "@FeignClient",
    ] {
        let (_host, mut tools, mut doc) = setup();
        doc.text = CLIENT.replace(r#"@FeignClient(name = "users")"#, annotation);
        assert_eq!(targets(&mut tools, &doc), [ADMIN, USERS], "{annotation}");
    }
}

#[test]
fn unknown_or_conflicting_module_identity_is_not_silently_excluded() {
    for config in [
        "",
        "spring.application.name=${APP_NAME:admin}\n",
        "spring.application.name=admin\nspring.application.name=users\n",
        "spring.application.name=admin\nspring.config.import=optional:configserver:\n",
    ] {
        let (host, mut tools, doc) = setup();
        host.add_file("admin/src/main/resources/application.yaml", "");
        host.add_file("admin/src/main/resources/application.properties", config);
        assert_eq!(targets(&mut tools, &doc), [ADMIN, USERS], "{config}");
    }
    for (file, config) in [
        ("application-dev.properties", "spring.application.name=users\n"),
        ("application.properties", "spring.application.name=users\n"),
        ("bootstrap.properties", "spring.application.name=users\n"),
        ("application.yaml", "spring.application.name: admin\n---\nspring.application.name: users\n"),
        ("application.yaml", "spring.application.name: admin\nspring.config.activate.on-profile: prod\n"),
        ("application-dev.yaml", "spring: {application: {name: users}}\n"),
        ("application-dev.yaml", "spring:\n  application: *shared\n"),
    ] {
        let (host, mut tools, doc) = setup();
        host.add_file(&format!("admin/src/main/resources/{file}"), config);
        assert_eq!(targets(&mut tools, &doc), [ADMIN, USERS], "{file}: {config}");
    }
}

#[test]
fn module_boundaries_do_not_inherit_parent_or_test_configuration() {
    let (host, mut tools, doc) = setup();
    host.add_file("src/main/resources/application.properties", "spring.application.name=unrelated\n");
    host.add_file("admin/src/test/resources/application.properties", "spring.application.name=users\n");
    host.add_file("users/src/main/resources/application-dev.properties", "spring.application.name=users\n");
    host.add_file("users/child/src/main/java/ChildController.java", SERVER);
    let found = targets(&mut tools, &doc);
    assert_eq!(found, ["/project/users/child/src/main/java/ChildController.java", USERS]);
    let (host, mut tools, doc) = setup();
    host.add_file("admin/src/main/resources/application.yaml", "");
    host.add_file("admin/src/main/resources/config/application.properties", "spring.application.name=admin\n");
    assert_eq!(targets(&mut tools, &doc), [USERS]);
}

#[test]
fn watched_and_unsaved_configuration_updates_invalidate_the_match() {
    let (host, mut tools, doc) = setup();
    assert_eq!(targets(&mut tools, &doc), [USERS]);
    let project = host.invocation().project().unwrap();
    let path = "/project/admin/src/main/resources/application.yaml";
    let text = "spring.application.name: users\n";
    host.add_file(path, text);
    tools.live_update(&project, path, Some(text));
    assert_eq!(targets(&mut tools, &doc), [ADMIN, USERS]);
    // Provider-supplied unsaved configuration supersedes cached disk content.
    let config = Document { path: path.into(), language: "yaml".into(), version: 2, text: "spring.application.name: admin\n".into() };
    tools.indexes(&config);
    assert_eq!(targets(&mut tools, &doc), [USERS]);
    tools.live_update(&project, path, None);
    assert_eq!(targets(&mut tools, &doc), [ADMIN, USERS]);
    assert!(host.requests().is_empty() && host.process_runs().is_empty());
}

#[test]
fn ambiguous_gutter_opens_selected_candidate_without_first_match_jump() {
    let (host, mut tools, doc) = setup();
    host.add_file("admin/src/main/resources/application.yaml", "spring.application.name: users\n");
    assert_eq!(targets(&mut tools, &doc), [ADMIN, USERS]);
    assert!(tools.line_markers(&doc).is_empty());
    let action = tools.gutter_actions(&doc).remove(0);
    assert_eq!(action.command, CHOOSE_TARGET);
    assert!(action.tooltip.contains("2 HTTP source targets"));
    host.answer_next_prompt(Some(&["1"]));
    tools.run_command(CHOOSE_TARGET, &host.invocation(), action.argument.as_deref()).unwrap();
    assert_eq!(host.opened_locations().len(), 1);
    assert_eq!(host.opened_locations()[0].0, USERS);
    assert!(host.requests().is_empty() && host.process_runs().is_empty());
    assert!(tools.run_command(CHOOSE_TARGET, &host.invocation(), action.argument.as_deref()).is_err());
}

#[test]
fn reverse_ambiguity_uses_chooser() {
    let (host, mut tools, _) = setup();
    host.add_file("client/src/main/java/OtherClient.java", &CLIENT.replace("UserClient", "OtherClient"));
    let doc = Document { path: USERS.into(), language: "java".into(), text: SERVER.into(), version: 1 };
    assert!(tools.line_markers(&doc).is_empty());
    assert_eq!(tools.gutter_actions(&doc).len(), 1);
}

#[test]
fn multiple_mappings_on_one_line_return_all_distinct_locations() {
    let (host, mut tools, mut doc) = setup();
    doc.text = CLIENT.replace(r#"@GetMapping("/users/{id}")"#, r#"@GetMapping({"/users/{id}", "/other"})"#);
    host.add_file("users/src/main/java/Other.java", &SERVER.replace("UserController", "Other").replace("/users/{id}", "/other"));
    let found = targets(&mut tools, &doc);
    assert_eq!(found.len(), 2);
    assert!(found.iter().any(|p| p == USERS));
    assert!(found.iter().any(|p| p.ends_with("/Other.java")));
    assert!(tools.line_markers(&doc).is_empty());
    assert_eq!(tools.gutter_actions(&doc).len(), 1);
}

#[test]
fn canceled_forged_and_stale_picker_tickets_never_open_a_target() {
    for change in ["cancel", "origin", "target", "config", "project", "forged", "selection"] {
        let (host, mut tools, doc) = setup();
        host.add_file("admin/src/main/resources/application.yaml", "spring.application.name: users\n");
        let action = tools.gutter_actions(&doc).remove(0);
        match change {
            "origin" => host.type_in_editor(" "),
            "target" => host.add_file(ADMIN, &SERVER.replace("/users/{id}", "/other")),
            "config" => host.add_file("admin/src/main/resources/application.yaml", "spring.application.name: admin\n"),
            "project" => host.set_project_root(Some("/other")),
            _ => {}
        }
        host.answer_next_prompt(if change == "cancel" {
            None
        } else if change == "selection" {
            Some(&["999"])
        } else {
            Some(&["0"])
        });
        let argument = if change == "forged" { Some("forged") } else { action.argument.as_deref() };
        let result = tools.run_command(CHOOSE_TARGET, &host.invocation(), argument);
        if change != "cancel" {
            assert!(result.is_err(), "{change}");
        } else {
            assert!(result.is_ok());
        }
        assert!(host.opened_locations().is_empty(), "{change}");
        assert!(host.requests().is_empty() && host.process_runs().is_empty());
    }
}
