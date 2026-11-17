use super::*;
use openidea_plugin::{language_features::LanguageFeatures, mock::MockHost, text::position};

#[test]
fn merged_plugin_routes_service_aware_definitions_and_gutter_commands() {
    let host = MockHost::install().with_capabilities(&["project.read", "editor.read"]);
    host.add_file("pom.xml", "<project><artifactId>spring-cloud-starter-openfeign</artifactId></project>");
    let client = "import org.springframework.cloud.openfeign.FeignClient;\nimport org.springframework.web.bind.annotation.GetMapping;\n@FeignClient(\"users\") interface Client { @GetMapping(\"/users\") String get(); }";
    let server = "import org.springframework.web.bind.annotation.RestController;\nimport org.springframework.web.bind.annotation.GetMapping;\n@RestController class Controller { @GetMapping(\"/users\") String list() { return \"\"; } }";
    let path = "/project/client/src/main/java/Client.java";
    host.add_file(path, client);
    for (module, name) in [("users", "users"), ("replica", "users"), ("admin", "admin")] {
        host.add_file(&format!("{module}/src/main/java/Controller.java"), server);
        host.add_file(&format!("{module}/src/main/resources/application.properties"), &format!("spring.application.name={name}\n"));
    }
    host.open_editor(path, "java", client, &[]);
    let editor = host.invocation().editor().unwrap();
    let source = editor.document().unwrap();
    let doc = Document { path: path.into(), language: "java".into(), text: source.text, version: source.version };
    let mut endpoints = Endpoints::default();
    let targets = endpoints.definition(&doc, position(client, client.find("get()").unwrap()));
    assert_eq!(targets.len(), 2);
    assert!(targets.iter().all(|l| !l.path.contains("/admin/")));
    assert!(endpoints.line_markers(&doc).is_empty());
    let action = endpoints.gutter_actions(&doc).remove(0);
    host.answer_next_prompt(Some(&["1"]));
    endpoints.run_command(&action.command, &host.invocation(), action.argument.as_deref()).unwrap();
    assert_eq!(host.opened_locations()[0].0, "/project/users/src/main/java/Controller.java");
    assert!(host.requests().is_empty() && host.process_runs().is_empty());
}
