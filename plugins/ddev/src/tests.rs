use super::*;
use crate::model::raw_output;
use openidea_plugin::details::{Details, Pane};
use openidea_plugin::mock::MockHost;
use openidea_plugin::providers::{Document, Providers};
use openidea_plugin::views::{Body, ViewEvent, Views};
use openidea_plugin::{Position, ProcessOutput};
use serde_json::json;

const STOPPED: &str = include_str!("../tests/fixtures/ddev_describe.json");
const VERSION: &str = include_str!("../tests/fixtures/ddev_version.json");

fn output(stdout: &str) -> Result<ProcessOutput, Error> {
    Ok(ProcessOutput {
        status: Some(0),
        stdout: stdout.into(),
        stderr: String::new(),
    })
}

fn running() -> String {
    json!({"raw": {
        "name": "demo", "status": "running", "primary_url": "https://demo.ddev.site",
        "services": {
            "web": {"status": "running", "https_url": "https://demo.ddev.site"},
            "db": {"status": "running", "host_ports": "3306"},
            "redis": {"status": "running", "http_url": "http://localhost:9000"}
        },
        "mailpit_https_url": "https://demo.ddev.site:8026"
    }})
    .to_string()
}

fn finish_default_config(plugin: &mut Main, host: &MockHost) {
    let project = host.invocation().project().unwrap();
    let key =
        execution::terminal_key(&project, "config", &["config".into(), "--auto".into()]).unwrap();
    let event = host.finish_terminal(&key).unwrap();
    plugin.handle_events(&[event], &host.invocation());
}

fn mock() -> MockHost {
    let host = MockHost::install().with_capabilities(&["project.read", "process:ddev"]);
    host.set_project_root(Some("/work/my project"));
    host.add_file(CONFIG, "name: demo\ntype: php\n");
    host.on_process("ddev", |args, _| {
        if args[0] == "--version" {
            return output("ddev version v1.19.0\n");
        }
        assert_eq!(args[1], "--json-output");
        match args[0].as_str() {
            "describe" => output(&running()),
            "version" => output(VERSION),
            _ => panic!("unexpected process: {args:?}"),
        }
    });
    host
}

#[test]
fn upstream_fixtures_parse_stopped_and_debug_output() {
    for fixture in [
        STOPPED,
        include_str!("../tests/fixtures/ddev_describe2.json"),
        include_str!("../tests/fixtures/ddev_describe_w_debug.json"),
    ] {
        let desc = Description::parse(fixture).unwrap();
        assert_eq!(desc.name, "acol");
        assert!(!desc.status.is_empty());
    }
    assert_eq!(
        text(&raw_output(VERSION).unwrap(), "DDEV version"),
        "v1.19.0"
    );
    assert!(raw_output("noise\n{\"raw\":null}").is_err());
    assert!(Description::parse(r#"{"raw":{"name":"x"}}"#).is_err());
}

#[test]
fn services_prefer_https_and_support_mailpit_http_fallback() {
    let desc = Description::parse(&running()).unwrap();
    assert_eq!(
        desc.service("service:web").unwrap().url.as_deref(),
        Some("https://demo.ddev.site")
    );
    assert!(!desc.service("service:mailpit").unwrap().logs);
    let desc = Description::parse(
        &json!({"raw":{"name":"x", "status":"running",
        "mailpit_url":"http://localhost:8025", "mailpit_https_url":"bad",
        "services":{"web":{"https_url":"https://user:pass@host", "http_url":"http://host"},
            "--bad":{"http_url":"http://host"}, "bad name":{}}}})
        .to_string(),
    )
    .unwrap();
    assert_eq!(desc.services.len(), 3);
    assert_eq!(
        desc.service("service:web").unwrap().url.as_deref(),
        Some("http://host")
    );
    assert_eq!(
        desc.service("service:mailpit").unwrap().url.as_deref(),
        Some("http://localhost:8025")
    );
}

#[test]
fn renders_services_and_details_with_native_streaming_logs() {
    let host = mock();
    let mut plugin = Main::default();
    let view = plugin.render(VIEW, None, &host.invocation()).unwrap();
    let Body::Tree(items) = view.body else {
        panic!("expected tree")
    };
    assert_eq!(items.len(), 5);
    assert_eq!(items[0].key, PROJECT);
    assert!(
        items[1..]
            .iter()
            .all(|i| i.parent.as_deref() == Some(PROJECT))
    );
    let tabs = plugin
        .detail(VIEW, "service:db", &host.invocation())
        .unwrap();
    let Pane::Log(log) = &tabs.iter().find(|tab| tab.id == "logs").unwrap().body else {
        panic!("expected logs")
    };
    assert_eq!(log.program, "ddev");
    assert_eq!(
        log.args,
        ["logs", "demo", "--follow", "--tail=200", "--service=db"]
    );
    assert_eq!(log.toggles[0].args, ["--time"]);
    assert_eq!(
        plugin
            .detail(VIEW, "service:mailpit", &host.invocation())
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        plugin
            .detail(VIEW, PROJECT, &host.invocation())
            .unwrap()
            .len(),
        5
    );
    assert_eq!(
        host.process_runs().len(),
        1,
        "selection should reuse the snapshot"
    );
}

#[test]
fn toolbar_and_context_commands_target_selected_container() {
    let host = mock();
    let mut plugin = Main::default();
    plugin
        .handle(
            VIEW,
            ViewEvent::Select("service:db".into()),
            &host.invocation(),
        )
        .unwrap();
    plugin
        .handle(VIEW, ViewEvent::Button("shell".into()), &host.invocation())
        .unwrap();
    assert_eq!(plugin.terminal_launches[0].program, "ddev");
    assert_eq!(plugin.terminal_launches[0].args, ["ssh", "--service=db"]);
    plugin
        .run_command("logs", &host.invocation(), Some("service:redis"))
        .unwrap();
    assert_eq!(
        host.log_tabs()[0].log.args.last().unwrap(),
        "--service=redis"
    );
    assert!(
        plugin
            .run_command("shell", &host.invocation(), Some("service:--privileged"))
            .is_err()
    );
    assert!(
        plugin
            .run_command("logs", &host.invocation(), Some("service:mailpit"))
            .is_err()
    );
}

#[test]
fn opens_service_links_and_honors_picker_cancellation() {
    let host = mock();
    let mut plugin = Main::default();
    plugin
        .run_command("open-service", &host.invocation(), Some("service:mailpit"))
        .unwrap();
    assert_eq!(host.opened_urls(), ["https://demo.ddev.site:8026"]);
    plugin
        .run_command("open-service", &host.invocation(), None)
        .unwrap();
    assert_eq!(host.opened_urls().len(), 1);
    host.answer_next_prompt(Some(&["project"]));
    plugin
        .run_command("open-service", &host.invocation(), None)
        .unwrap();
    assert_eq!(host.opened_urls()[1], "https://demo.ddev.site");
    assert!(
        plugin
            .run_command("open-service", &host.invocation(), Some("service:db"))
            .is_err()
    );
}

#[test]
fn terminal_commands_preserve_arguments_and_confirm_destructive_actions() {
    let host = mock();
    let mut plugin = Main::default();
    host.on_process("ddev", |_, _| output(STOPPED));
    plugin
        .run_command("start", &host.invocation(), None)
        .unwrap();
    host.on_process("ddev", |args, _| {
        if args[0] == "--version" {
            output("ddev version v1.25.3\n")
        } else {
            output(&running())
        }
    });
    for command in ["restart", "stop"] {
        plugin
            .run_command(command, &host.invocation(), None)
            .unwrap();
        assert_eq!(plugin.terminal_launches.last().unwrap().args, [command]);
    }
    for command in ["share", "delete", "poweroff", "config"] {
        let before = plugin.terminal_launches.len();
        plugin
            .run_command(command, &host.invocation(), None)
            .unwrap();
        assert_eq!(
            plugin.terminal_launches.len(),
            before,
            "cancelled {command}"
        );
        host.answer_next_prompt(Some(&["ok"]));
        plugin
            .run_command(command, &host.invocation(), None)
            .unwrap();
        let expected = if command == "config" {
            vec!["config", "--auto"]
        } else {
            vec![command]
        };
        assert_eq!(plugin.terminal_launches.last().unwrap().args, expected);
    }
    assert!(
        host.process_runs()
            .iter()
            .all(|run| matches!(run.args[0].as_str(), "describe" | "--version")),
        "only status and installation reads should block the plugin"
    );
}

#[test]
fn refresh_reloads_cli_and_project_switch_discards_cache() {
    let host = mock();
    let mut plugin = Main::default();
    plugin.render(VIEW, None, &host.invocation()).unwrap();
    host.on_process("ddev", |_, _| output(STOPPED));
    plugin
        .run_command("refresh", &host.invocation(), None)
        .unwrap();
    let view = plugin.render(VIEW, None, &host.invocation()).unwrap();
    assert!(view.status.unwrap().contains("stopped"));
    host.set_project_root(Some("/work/another"));
    plugin.render(VIEW, None, &host.invocation()).unwrap();
    assert_eq!(host.process_runs().len(), 3);
}

#[test]
fn no_project_no_config_denied_and_failed_process_have_useful_states() {
    let host = mock();
    host.set_project_root(None);
    let Body::Tree(items) = Main::default()
        .render(VIEW, None, &host.invocation())
        .unwrap()
        .body
    else {
        panic!()
    };
    assert!(items[0].label.contains("Open a project"));
    assert!(host.process_runs().is_empty());
    assert!(
        Main::default()
            .run_command("start", &host.invocation(), None)
            .is_err()
    );
    let host = MockHost::install().with_capabilities(&["project.read"]);
    host.set_project_root(Some("/work"));
    let view = Main::default()
        .render(VIEW, None, &host.invocation())
        .unwrap();
    assert!(matches!(view.body, Body::Tree(_)));
    assert!(view.status.unwrap().contains("Not configured"));
    host.add_file(CONFIG, "name: demo");
    assert!(
        Main::default()
            .run_command("start", &host.invocation(), None)
            .is_err()
    );
    let host = mock();
    host.on_process("ddev", |_, _| {
        Ok(ProcessOutput {
            status: Some(1),
            stdout: String::new(),
            stderr: r#"{"level":"fatal","msg":"Docker is not running"}"#.into(),
        })
    });
    let view = Main::default()
        .render(VIEW, None, &host.invocation())
        .unwrap();
    assert!(view.status.unwrap().contains("Docker is not running"));
}

#[test]
fn native_services_status_is_inert_and_toolbar_icons_are_explicit() {
    let host = mock();
    host.set_project_root(None);
    let mut plugin = Main::default();
    let view = plugin.render(VIEW, None, &host.invocation()).unwrap();
    let Body::Tree(items) = view.body else { panic!("Services needs tree rows") };
    let key = items[0].key.clone();
    assert_eq!(key, "status:no-project");
    for event in [ViewEvent::Select(key.clone()), ViewEvent::Activate(key.clone())] {
        plugin.handle(VIEW, event, &host.invocation()).unwrap();
    }
    assert!(plugin.selected.is_none());
    assert!(matches!(plugin.detail(VIEW, &key, &host.invocation()).unwrap()[0].body, Pane::Message(_)));
    assert!(host.process_runs().is_empty() && host.terminals().is_empty());

    host.set_project_root(Some("/work/my project"));
    let view = plugin.render(VIEW, None, &host.invocation()).unwrap();
    assert_eq!(view.toolbar.len(), 8);
    assert!(!view.toolbar.iter().any(|button| button.id == "start"));
    assert!(view.toolbar.iter().all(|button| button.icon.is_some()));
    for (id, icon) in [("refresh", "refresh"), ("restart", "rerun"), ("shell", "terminal"), ("logs", "list"), ("open-service", "external-link"), ("actions", "more")] {
        assert_eq!(view.toolbar.iter().find(|button| button.id == id).unwrap().icon.as_deref(), Some(icon));
    }
    assert!(plugin.selected.as_deref() != Some("status:no-project"));
    let setup = plugin.render(support::SETUP, None, &host.invocation()).unwrap();
    assert!(setup.toolbar.iter().all(|button| button.icon.is_some()), "setup toolbar must stay compact too");
    assert!(setup.toolbar.iter().any(|button| button.id == "open-docs" && button.icon.as_deref() == Some("help")));
    assert!(!setup.toolbar.iter().any(|button| button.id == "report-issue"));
}

#[test]
fn unavailable_status_navigation_never_retries_runtime_commands() {
    for state in ["missing", "denied", "failed", "stale", "project-denied"] {
        let host = if state == "missing" {
            MockHost::install().with_capabilities(&["project.read"])
        } else if state == "denied" {
            mock().with_capabilities(&["project.read"])
        } else if state == "project-denied" {
            mock().with_capabilities(&[])
        } else {
            let host = mock();
            host.on_process("ddev", move |_, _| {
                if state == "stale" { Err(Error::Stale) }
                else { Ok(ProcessOutput {status:Some(1),stdout:String::new(),stderr:"Docker unavailable".into()}) }
            });
            host
        };
        let mut plugin = Main::default();
        let Body::Tree(items) = plugin.render(VIEW, None, &host.invocation()).unwrap().body else {panic!("Services status tree")};
        assert_eq!(items[0].key, if state == "missing" {"status:not-configured"} else {"status:unavailable"}, "{state}");
        let calls = host.process_runs().len();
        for event in [ViewEvent::Select(items[0].key.clone()), ViewEvent::Activate(items[0].key.clone())] {
            plugin.handle(VIEW,event,&host.invocation()).unwrap();
        }
        let tabs = plugin.detail(VIEW,&items[0].key,&host.invocation()).unwrap();
        assert!(matches!(tabs[0].body,Pane::Message(_)));
        assert_eq!(host.process_runs().len(),calls,"status navigation retried CLI in {state}");
        assert!(host.terminals().is_empty());
        assert!(plugin.selected.as_deref().is_none_or(|key|!key.starts_with("status:")));
    }
}

#[test]
fn update_check_is_explicit_and_handles_http_failure() {
    let host = mock().with_capabilities(&["project.read", "process:ddev", "net:api.github.com"]);
    host.on_fetch(|r| {
        assert_eq!(
            r.url,
            "https://api.github.com/repos/ddev/ddev/releases/latest"
        );
        Ok(openidea_plugin::HttpResponse {
            status: 200,
            headers: vec![],
            body: br#"{"tag_name":"v1.25.0"}"#.to_vec(),
        })
    });
    Main::default()
        .run_command("check-updates", &host.invocation(), None)
        .unwrap();
    assert!(
        host.notifications()[0]
            .body
            .contains("v1.25.0 is available")
    );
    host.on_fetch(|_| {
        Ok(openidea_plugin::HttpResponse {
            status: 403,
            headers: vec![],
            body: vec![],
        })
    });
    assert!(
        Main::default()
            .run_command("check-updates", &host.invocation(), None)
            .is_err()
    );
    let host = mock();
    assert!(
        Main::default()
            .run_command("check-updates", &host.invocation(), None)
            .is_err()
    );
}

fn complete(path: &str, marked: &str) -> Vec<openidea_plugin::providers::CompletionItem> {
    let cursor = marked.find('§').unwrap();
    let text = marked.replacen('§', "", 1);
    let at = openidea_plugin::text::position(&text, cursor);
    Main::default().completion(
        &Document {
            path: path.into(),
            language: "yaml".into(),
            text,
            version: 1,
        },
        at,
        None,
    )
}

#[test]
fn yaml_completion_is_scoped_and_preserves_existing_colons() {
    let path = "/work/.ddev/config.yaml";
    assert_eq!(complete(path, "webs§")[0].insert_text, "webserver_type: ");
    assert_eq!(
        complete(path, "webs§: nginx-fpm")[0].insert_text,
        "webserver_type"
    );
    assert!(complete(path, "name: demo\nna§").is_empty());
    assert_eq!(complete(path, "database:\n  ty§")[0].insert_text, "type: ");
    assert!(complete(path, "# na§").is_empty());
    assert!(complete("/work/config.yaml", "na§").is_empty());
    assert!(
        complete("/work/.ddev/config.local.yaml", "na§")
            .iter()
            .any(|i| i.label == "name")
    );
    assert_eq!(complete(path, "xdebug_enabled: tr§")[0].insert_text, "true");
    assert_eq!(
        complete(path, "webserver_type: ngi§")[0].insert_text,
        "nginx-fpm"
    );
    assert!(complete(path, "docroot: pu§").is_empty());
    assert!(
        Main::default()
            .completion(
                &Document {
                    path: path.into(),
                    text: "x".into(),
                    ..Default::default()
                },
                Position::new(100, 0),
                None
            )
            .is_empty()
    );
}

#[test]
fn version_check_accepts_plain_and_globally_enabled_json_output() {
    assert_eq!(
        installed_version("ddev version v1.25.3\n")
            .unwrap()
            .to_string(),
        "1.25.3"
    );
    assert_eq!(
        installed_version(r#"{"level":"info","msg":"ddev version v1.25.3"}"#)
            .unwrap()
            .to_string(),
        "1.25.3"
    );
    assert!(installed_version("not a version").is_err());
}

fn database_output(port: u16) -> String {
    let mut data: serde_json::Value = serde_json::from_str(&running()).unwrap();
    data["raw"]["dbinfo"] = json!({"database_type":"mariadb","database_version":"10.11", "published_port":port,
        "dbname":"db", "username":"db", "password":"secret-for-test"});
    data.to_string()
}

#[test]
fn database_setup_preserves_other_sources_and_never_writes_passwords() {
    let host = mock().with_capabilities(&["project.read", "process:ddev", "project.write"]);
    host.add_file(database::FILE,"# Keep my source\n[[data-source]]\nid = 'mine'\nname = 'Production'\ndriver = 'postgres'\nhost = 'prod'\n");
    host.on_process("ddev", |_, _| output(&database_output(32850)));
    let mut plugin = Main::default();
    plugin
        .run_command("setup-database", &host.invocation(), None)
        .unwrap();
    let project = host.invocation().project().unwrap();
    let text = project.read_file(database::FILE).unwrap();
    assert!(text.contains("# Keep my source") && text.contains("Production"));
    assert!(!text.contains("secret-for-test") && !text.contains("password"));
    let note = &host.notifications()[0].body;
    assert!(note.contains("database.sources permission. Allow it in Settings › Plugins › DDEV Integration."), "{note}");
    let doc: toml_edit::DocumentMut = text.parse().unwrap();
    let sources = doc["data-source"].as_array_of_tables().unwrap();
    assert_eq!(sources.len(), 2);
    assert_eq!(sources.get(1).unwrap()["port"].as_integer(), Some(32850));
    assert_eq!(sources.get(1).unwrap()["driver"].as_str(), Some("mysql"));
    assert!(
        !database::configure(&project, plugin.description(&project).unwrap()).unwrap(),
        "unchanged data should not be rewritten"
    );
    host.on_process("ddev", |_, _| output(&database_output(32851)));
    plugin
        .run_command("refresh", &host.invocation(), None)
        .unwrap();
    let doc: toml_edit::DocumentMut = project.read_file(database::FILE).unwrap().parse().unwrap();
    assert_eq!(doc["data-source"].as_array_of_tables().unwrap().len(), 2);
    assert_eq!(
        doc["data-source"]
            .as_array_of_tables()
            .unwrap()
            .get(1)
            .unwrap()["port"]
            .as_integer(),
        Some(32851)
    );
}

#[test]
fn database_setup_rejects_bad_configs_and_respects_write_denial() {
    let host = mock();
    host.on_process("ddev", |_, _| output(&database_output(32850)));
    assert!(
        Main::default()
            .run_command("setup-database", &host.invocation(), None)
            .is_err()
    );
    let host = mock().with_capabilities(&["project.read", "process:ddev", "project.write"]);
    host.on_process("ddev", |_, _| output(&database_output(32850)));
    for file in [
        "not toml [",
        "[[data-source]]\nid='aritti.ddev.database'\nname='User configured'\n",
    ] {
        let file = file.replace(
            "aritti.ddev.database",
            &database::source_id(&host.invocation().project().unwrap()).unwrap(),
        );
        host.add_file(database::FILE, &file);
        assert!(
            Main::default()
                .run_command("setup-database", &host.invocation(), None)
                .is_err()
        );
        assert_eq!(
            host.invocation()
                .project()
                .unwrap()
                .read_file(database::FILE)
                .unwrap(),
            file
        );
    }
}

#[test]
fn state_controls_match_original_and_reject_invalid_runtime_actions() {
    let host = mock();
    let mut plugin = Main::default();
    let view = plugin.render(VIEW, None, &host.invocation()).unwrap();
    let enabled = |view: &openidea_plugin::views::View, id: &str| {
        view.toolbar.iter().any(|b| b.id == id)
    };
    assert!(!enabled(&view, "start") && enabled(&view, "restart") && enabled(&view, "stop") && enabled(&view, "shell"));
    assert!(
        plugin
            .run_command("start", &host.invocation(), None)
            .is_err()
    );
    host.on_process("ddev", |_, _| output(STOPPED));
    plugin
        .run_command("refresh", &host.invocation(), None)
        .unwrap();
    let view = plugin.render(VIEW, None, &host.invocation()).unwrap();
    assert!(enabled(&view, "start") && !enabled(&view, "restart") && !enabled(&view, "stop") && !enabled(&view, "shell"));
    assert!(
        plugin
            .run_command("stop", &host.invocation(), None)
            .is_err()
    );
    assert_eq!(host.status_items()["ddev-status"].text, "DDEV: stopped");
    let err = plugin.run_command("stop", &host.invocation(), None).unwrap_err().to_string();
    assert!(err.contains("DDEV: Stop Project needs a running project"), "{err}");
}

#[test]
fn status_bar_item_is_hidden_in_projects_without_ddev() {
    let host = MockHost::install().with_capabilities(&["project.read", "process:ddev"]);
    host.set_project_root(Some("/work/plain"));
    let item = openidea_plugin::StatusItem { text: "DDEV: running".into(), tooltip: None, command: None };
    host::set_window_status_item("ddev-status", &item).unwrap();
    Main::default().render(VIEW, None, &host.invocation()).unwrap();
    assert!(!host.status_items().contains_key("ddev-status"));
}

#[test]
fn events_refresh_status_and_watch_setting_is_respected() {
    let host = mock();
    let mut plugin = Main::default();
    plugin.render(VIEW, None, &host.invocation()).unwrap();
    host.on_process("ddev", |_, _| output(STOPPED));
    plugin.handle_events(&[openidea_plugin::Event::Scheduled], &host.invocation());
    assert!(
        plugin
            .render(VIEW, None, &host.invocation())
            .unwrap()
            .status
            .unwrap()
            .contains("stopped")
    );
    host.set_setting("watchDdev", "false");
    host.on_process("ddev", |_, _| output(&running()));
    let reads = host.process_runs().len();
    plugin.handle_events(&[openidea_plugin::Event::Scheduled], &host.invocation());
    assert_eq!(host.process_runs().len(), reads);
    plugin.handle_events(
        &[openidea_plugin::Event::FileSaved(
            "/work/my project/.ddev/config.yaml".into(),
        )],
        &host.invocation(),
    );
    assert!(
        plugin
            .render(VIEW, None, &host.invocation())
            .unwrap()
            .status
            .unwrap()
            .contains("running")
    );
}

#[test]
fn guided_setup_opens_the_setup_form_and_help_command_is_gone() {
    let host = mock().with_capabilities(&["project.read"]);
    let mut plugin = Main::default();
    plugin.run_command("setup", &host.invocation(), None).unwrap();
    assert_eq!(host.revealed_views(), [support::SETUP.to_owned()]);
    let manifest = include_str!("../plugin.toml");
    assert!(!manifest.contains("id = \"help\""));
    assert!(!manifest.contains("command = \"help\""));
    assert!(!manifest.lines().find(|l| l.starts_with("description")).unwrap().contains("API 1.4"));
}

#[test]
fn configuration_and_help_work_without_process_permission() {
    let host = mock().with_capabilities(&["project.read"]);
    let mut plugin = Main::default();
    let view = plugin.render(VIEW, None, &host.invocation()).unwrap();
    assert!(matches!(view.body, Body::Tree(_)));
    assert!(
        !view.toolbar.iter().any(|b| b.id == "start")
    );
    let tabs = plugin.detail(VIEW, PROJECT, &host.invocation()).unwrap();
    assert!(tabs.iter().any(|t| t.id == "configuration"
        && matches!(&t.body,Pane::Text(text) if text.contains("name: demo"))));
    assert!(tabs.iter().any(|t| t.id == "help"));
    plugin
        .run_command("open-config", &host.invocation(), None)
        .unwrap();
    assert!(host.process_runs().is_empty());
}

#[test]
fn automatic_checks_are_quiet_when_current_throttled_and_optional() {
    let host = mock().with_capabilities(&[
        "project.read",
        "process:ddev",
        "storage",
        "net:api.github.com",
    ]);
    host.on_fetch(|_| {
        Ok(openidea_plugin::HttpResponse {
            status: 200,
            headers: vec![],
            body: br#"{"tag_name":"v1.19.0"}"#.to_vec(),
        })
    });
    let project = host.invocation().project().unwrap();
    runtime::automatic_updates(&project);
    assert_eq!(host.requests().len(), 1);
    assert!(host.notifications().is_empty());
    runtime::automatic_updates(&project);
    assert_eq!(host.requests().len(), 1);
    host.set_setting("checkForUpdates", "false");
    host::storage::delete(&runtime::update_key(&project).unwrap()).unwrap();
    runtime::automatic_updates(&project);
    assert_eq!(host.requests().len(), 1);
    let host = mock();
    runtime::automatic_updates(&host.invocation().project().unwrap());
    assert_eq!(host.requests().len(), 0);
}

#[test]
fn container_tools_preserve_quoted_arguments_and_cancel_cleanly() {
    let host = mock();
    let mut plugin = Main::default();
    plugin
        .run_command("composer", &host.invocation(), None)
        .unwrap();
    assert!(plugin.terminal_launches.is_empty());
    host.answer_next_prompt(Some(&["require 'vendor/my package' --dev"]));
    plugin
        .run_command("composer", &host.invocation(), None)
        .unwrap();
    assert_eq!(
        plugin.terminal_launches[0].args,
        ["composer", "require", "vendor/my package", "--dev"]
    );
    host.answer_next_prompt(Some(&["-r 'echo 1;' "]));
    plugin.run_command("php", &host.invocation(), None).unwrap();
    assert_eq!(
        plugin.terminal_launches[1].args,
        ["exec", "--raw", "--", "php", "-r", "echo 1;"]
    );
}

#[test]
fn refreshed_selection_survives_instance_reclaim() {
    let host = mock().with_capabilities(&["project.read", "process:ddev", "storage"]);
    Main::default()
        .handle(
            VIEW,
            ViewEvent::Select("service:db".into()),
            &host.invocation(),
        )
        .unwrap();
    let mut reclaimed = Main::default();
    reclaimed
        .run_command("shell", &host.invocation(), None)
        .unwrap();
    assert_eq!(reclaimed.terminal_launches[0].args, ["ssh", "--service=db"]);
}

#[test]
fn configure_uses_project_php_level_with_explicit_override_and_platform_fallback() {
    let host = mock();
    host.add_file(
        ".openidea/settings.toml",
        "[languages.php]\nlanguageLevel = '8.4'\n",
    );
    host.add_file(
        "composer.json",
        r#"{"config":{"platform":{"php":"8.3.5"}}}"#,
    );
    let mut plugin = Main::default();
    host.answer_next_prompt(Some(&["ok"]));
    plugin
        .run_command("config", &host.invocation(), None)
        .unwrap();
    assert_eq!(
        plugin.terminal_launches[0].args,
        ["config", "--auto", "--php-version", "8.4"]
    );
    let project = host.invocation().project().unwrap();
    let key =
        execution::terminal_key(&project, "config", &plugin.terminal_launches[0].args).unwrap();
    let exit = host.finish_terminal(&key).unwrap();
    plugin.handle_events(&[exit], &host.invocation());
    host.set_setting("phpVersion", r#""8.2""#);
    host.answer_next_prompt(Some(&["ok"]));
    plugin
        .run_command("config", &host.invocation(), None)
        .unwrap();
    assert_eq!(
        plugin.terminal_launches[1].args,
        ["config", "--auto", "--php-version", "8.2"]
    );
    host.set_setting("phpVersion", r#""""#);
    host.add_file(".openidea/settings.toml", "");
    assert_eq!(
        config_php_version(&host.invocation().project().unwrap())
            .unwrap()
            .as_deref(),
        Some("8.3")
    );
    host.set_setting("phpVersion", r#""8.2 --something""#);
    host.answer_next_prompt(Some(&["ok"]));
    assert!(
        plugin
            .run_command("config", &host.invocation(), None)
            .is_err()
    );
    assert_eq!(plugin.terminal_launches.len(), 2);
}

#[test]
fn database_credential_ids_are_scoped_to_the_project() {
    let host = mock();
    let first = database::source_id(&host.invocation().project().unwrap()).unwrap();
    host.set_project_root(Some("/work/another project"));
    let second = database::source_id(&host.invocation().project().unwrap()).unwrap();
    assert_ne!(first, second);
}

#[test]
fn status_action_menu_cancels_and_dispatches_only_listed_actions() {
    let host = mock();
    let mut plugin = Main::default();
    plugin
        .run_command("actions", &host.invocation(), None)
        .unwrap();
    assert!(plugin.terminal_launches.is_empty());
    host.answer_next_prompt(Some(&["shell"]));
    plugin
        .run_command("actions", &host.invocation(), None)
        .unwrap();
    assert_eq!(plugin.terminal_launches[0].args, ["ssh", "--service=web"]);
    host.answer_next_prompt(Some(&["unlisted"]));
    assert!(
        plugin
            .run_command("actions", &host.invocation(), None)
            .is_err()
    );
}

#[test]
fn background_work_respects_explicit_project_preferences() {
    let host = mock().with_capabilities(&[
        "project.read",
        "process:ddev",
        "project.write",
        "storage",
        "net:api.github.com",
    ]);
    host.add_file(".openidea/settings.toml", "[plugins.'aritti.ddev']\nwatchDdev=false\nautoConfigureDataSource=false\ncheckForUpdates=false\n");
    host.on_process("ddev", |_, _| output(&database_output(32850)));
    let mut plugin = Main::default();
    plugin.render(VIEW, None, &host.invocation()).unwrap();
    let reads = host.process_runs().len();
    plugin.handle_events(&[openidea_plugin::Event::Scheduled], &host.invocation());
    assert_eq!(host.process_runs().len(), reads);
    assert!(host.requests().is_empty());
    plugin
        .run_command("refresh", &host.invocation(), None)
        .unwrap();
    assert!(
        !host
            .invocation()
            .project()
            .unwrap()
            .exists(database::FILE)
            .unwrap()
    );
}

#[test]
fn recovery_notifications_offer_actions_and_repeat_only_after_recovery() {
    let host = mock().with_capabilities(&["project.read", "process:ddev", "storage"]);
    let project = host.invocation().project().unwrap();
    host::storage::set(&support::key(&project, "setup-tour").unwrap(), "shown").unwrap();
    let failing = |_: &[String], _: Option<&str>| {
        Ok(ProcessOutput {
            status: Some(1),
            stdout: String::new(),
            stderr: "Docker unavailable".into(),
        })
    };
    host.on_process("ddev", failing);
    let mut plugin = Main::default();
    plugin.render(VIEW, None, &host.invocation()).unwrap();
    assert_eq!(host.notifications_with_actions().len(), 1);
    assert_eq!(
        host.notifications_with_actions()[0]
            .actions
            .iter()
            .map(|a| a.command.as_str())
            .collect::<Vec<_>>(),
        ["refresh", "setup", "open-guide"]
    );
    assert!(
        plugin
            .run_command("refresh", &host.invocation(), None)
            .is_err()
    );
    assert_eq!(host.notifications_with_actions().len(), 1);
    host.on_process("ddev", |_, _| output(&running()));
    plugin
        .run_command("refresh", &host.invocation(), None)
        .unwrap();
    host.on_process("ddev", failing);
    assert!(
        plugin
            .run_command("refresh", &host.invocation(), None)
            .is_err()
    );
    assert_eq!(host.notifications_with_actions().len(), 2);
}

#[test]
fn failure_notice_stays_suppressed_when_reason_changes_until_actual_recovery() {
    for persistent in [false, true] {
        let capabilities = if persistent {
            vec!["project.read", "process:ddev", "storage"]
        } else {
            vec!["project.read", "process:ddev"]
        };
        let host = mock().with_capabilities(&capabilities);
        let project = host.invocation().project().unwrap();
        let mut plugin = Main::default();
        for reason in [
            "Docker unavailable",
            "Docker socket connection refused",
            "DDEV timed out",
        ] {
            host.on_process("ddev", move |_, _| Err(reason.into()));
            assert!(
                plugin
                    .run_command("refresh", &host.invocation(), None)
                    .is_err()
            );
        }
        assert_eq!(host.notifications_with_actions().len(), 1);
        if persistent {
            // Upgrade compatibility: older instances persisted an error hash.
            host::storage::set(
                &support::key(&project, "failure-notice").unwrap(),
                &support::fingerprint("an older error message"),
            )
            .unwrap();
            plugin = Main::default();
            host.on_process("ddev", |_, _| Err("New reason after idle reclaim".into()));
            assert!(
                plugin
                    .run_command("refresh", &host.invocation(), None)
                    .is_err()
            );
            assert_eq!(host.notifications_with_actions().len(), 1);
            // Avoid the unrelated first-use tour after recovery.
            host::storage::set(&support::key(&project, "setup-tour").unwrap(), "shown").unwrap();
        }
        host.on_process("ddev", |_, _| output(&running()));
        plugin
            .run_command("refresh", &host.invocation(), None)
            .unwrap();
        host.on_process("ddev", |_, _| Err("Docker unavailable again".into()));
        assert!(
            plugin
                .run_command("refresh", &host.invocation(), None)
                .is_err()
        );
        assert_eq!(host.notifications_with_actions().len(), 2);
    }
}

#[test]
fn setup_form_is_static_and_saves_only_reviewed_project_preferences() {
    let host = mock().with_capabilities(&["project.read", "project.write"]);
    host.add_file(
        ".openidea/settings.toml",
        "# My editor\n[editor]\nfontSize = 15\n[plugins.other]\nkeep = true\n",
    );
    let mut plugin = Main::default();
    assert!(matches!(
        plugin
            .render(support::SETUP, None, &host.invocation())
            .unwrap()
            .body,
        Body::Form(_)
    ));
    assert!(host.process_runs().is_empty());
    plugin
        .handle(
            support::SETUP,
            ViewEvent::Field("phpVersion".into(), "8.3".into()),
            &host.invocation(),
        )
        .unwrap();
    plugin
        .handle(
            support::SETUP,
            ViewEvent::Field("watchDdev".into(), "false".into()),
            &host.invocation(),
        )
        .unwrap();
    let project = host.invocation().project().unwrap();
    assert!(
        !project
            .read_file(".openidea/settings.toml")
            .unwrap()
            .contains("8.3")
    );
    plugin
        .handle(
            support::SETUP,
            ViewEvent::Button("save-settings".into()),
            &host.invocation(),
        )
        .unwrap();
    let text = project.read_file(".openidea/settings.toml").unwrap();
    assert!(
        text.contains("# My editor")
            && text.contains("fontSize = 15")
            && text.contains("keep = true")
    );
    assert_eq!(
        config_php_version(&project).unwrap().as_deref(),
        Some("8.3")
    );
    assert!(!runtime::enabled_for(&project, "watchDdev"));
}

#[test]
fn setup_save_handles_denial_invalid_php_and_malformed_settings() {
    let host = mock().with_capabilities(&["project.read"]);
    let mut plugin = Main::default();
    assert!(
        plugin
            .handle(
                support::SETUP,
                ViewEvent::Button("save-settings".into()),
                &host.invocation()
            )
            .is_err()
    );
    let host = mock().with_capabilities(&["project.read", "project.write"]);
    let mut plugin = Main::default();
    plugin
        .handle(
            support::SETUP,
            ViewEvent::Field("phpVersion".into(), "8.3 --flag".into()),
            &host.invocation(),
        )
        .unwrap();
    assert!(
        plugin
            .handle(
                support::SETUP,
                ViewEvent::Button("save-settings".into()),
                &host.invocation()
            )
            .is_err()
    );
    assert!(
        !host
            .invocation()
            .project()
            .unwrap()
            .exists(".openidea/settings.toml")
            .unwrap()
    );
    host.add_file(".openidea/settings.toml", "invalid = [");
    assert!(
        Main::default()
            .run_command("disable-update-checks", &host.invocation(), None)
            .is_err()
    );
    assert_eq!(
        host.invocation()
            .project()
            .unwrap()
            .read_file(".openidea/settings.toml")
            .unwrap(),
        "invalid = ["
    );
}

#[test]
fn configure_followup_opens_changed_configuration_once_after_reclaim() {
    let host = mock().with_capabilities(&["project.read", "process:ddev", "storage"]);
    host.answer_next_prompt(Some(&["ok"]));
    Main::default()
        .run_command("config", &host.invocation(), None)
        .unwrap();
    let mut reclaimed = Main::default();
    reclaimed.handle_events(
        &[openidea_plugin::Event::EditorFocused(Some(
            "file.php".into(),
        ))],
        &host.invocation(),
    );
    assert!(
        host.opened_locations().is_empty(),
        "do not open before a change"
    );
    host.add_file(CONFIG, "name: demo\ntype: php\nphp_version: '8.3'\n");
    reclaimed.handle_events(
        &[openidea_plugin::Event::FileSaved(
            "/work/my project/.ddev/config.yaml".into(),
        )],
        &host.invocation(),
    );
    assert!(
        host.opened_locations().is_empty(),
        "a write is not terminal success"
    );
    finish_default_config(&mut reclaimed, &host);
    assert_eq!(host.opened_locations().len(), 1);
    assert_eq!(
        host.opened_locations()[0].0,
        "/work/my project/.ddev/config.yaml"
    );
    reclaimed.handle_events(
        &[openidea_plugin::Event::FileSaved(
            "/work/my project/.ddev/config.yaml".into(),
        )],
        &host.invocation(),
    );
    assert_eq!(host.opened_locations().len(), 1);
}

#[test]
fn cancelled_configuration_does_not_arm_followup_and_checks_scope_by_project() {
    let host = mock().with_capabilities(&["project.read", "process:ddev", "storage"]);
    let mut plugin = Main::default();
    plugin
        .run_command("config", &host.invocation(), None)
        .unwrap();
    assert!(plugin.pending_configs.is_empty());
    host.answer_next_prompt(Some(&["ok"]));
    plugin
        .run_command("config", &host.invocation(), None)
        .unwrap();
    host.set_project_root(Some("/another"));
    host.add_file(CONFIG, "name: another\n");
    plugin.handle_events(
        &[openidea_plugin::Event::EditorFocused(None)],
        &host.invocation(),
    );
    assert!(host.opened_locations().is_empty());
}

#[test]
fn environment_check_reports_docker_failure_without_exposing_raw_output() {
    let host = mock();
    host.on_process("ddev",|_,_|Ok(ProcessOutput {status:Some(1),stdout:serde_json::json!({"raw":{"DDEV version":"v1.25.3","docker":"error","private-field":"secret"}}).to_string(),stderr:"Docker daemon unavailable".into()}));
    let mut plugin = Main::default();
    plugin
        .run_command("diagnose", &host.invocation(), None)
        .unwrap();
    let data = &plugin.diagnostics.as_ref().unwrap().1;
    assert!(data.contains("v1.25.3") && data.contains("docker: error") && data.contains("failed"));
    assert!(!data.contains("secret"));
    assert!(host.revealed_views().iter().any(|v| v == support::SETUP));
}

#[test]
fn update_notifications_have_release_and_disable_actions() {
    let host = mock().with_capabilities(&[
        "project.read",
        "process:ddev",
        "project.write",
        "net:api.github.com",
    ]);
    host.on_fetch(|_| {
        Ok(openidea_plugin::HttpResponse {
            status: 200,
            headers: vec![],
            body: br#"{"tag_name":"v1.25.0"}"#.to_vec(),
        })
    });
    let mut plugin = Main::default();
    plugin
        .run_command("check-updates", &host.invocation(), None)
        .unwrap();
    assert_eq!(
        host.notifications_with_actions()[0].actions[0].command,
        "open-releases"
    );
    plugin
        .run_command("disable-update-checks", &host.invocation(), None)
        .unwrap();
    assert!(!runtime::enabled_for(
        &host.invocation().project().unwrap(),
        "checkForUpdates"
    ));
}

#[test]
fn start_remains_available_without_description_when_ddev_is_installed() {
    let host = mock();
    host.on_process("ddev", |args, _| {
        if args[0] == "--version" {
            output("ddev version v1.25.3\n")
        } else {
            Err("describe failed before first start".into())
        }
    });
    let mut plugin = Main::default();
    let view = plugin.render(VIEW, None, &host.invocation()).unwrap();
    assert!(
        view.toolbar
            .iter()
            .find(|b| b.id == "start")
            .unwrap()
            .enabled
    );
    assert!(
        plugin
            .run_command("start", &host.invocation(), None)
            .is_ok()
    );
    assert_eq!(plugin.terminal_launches.last().unwrap().args, ["start"]);

    let host = mock();
    host.on_process("ddev", |_, _| Err("not installed".into()));
    let mut plugin = Main::default();
    assert!(!plugin.can_start(&host.invocation().project().unwrap()));
    assert!(
        plugin
            .run_command("start", &host.invocation(), None)
            .is_err()
    );
    assert!(plugin.terminal_launches.is_empty());
}

#[test]
fn upstream_error_output_cases_and_last_failure_precedence() {
    for (input, expected) in [
        (
            "{\"level\":\"warning\",\"msg\":\"Docker is slow\"}\n{\"level\":\"fatal\",\"msg\":\"project already exists\"}\n",
            Some("project already exists"),
        ),
        (
            "zsh:1: no such file or directory: /usr/local/bin/ddev\n",
            Some("zsh:1: no such file or directory: /usr/local/bin/ddev"),
        ),
        ("{\"level\":\"info\",\"msg\":\"Starting\"}\n", None),
        ("", None),
        (
            "{\"level\":\"error\",\"msg\":\"first\"}\n{\"level\":\"fatal\",\"msg\":\"last\"}\n",
            Some("last"),
        ),
    ] {
        assert_eq!(model::error_message(input).as_deref(), expected);
    }
}

#[test]
fn failed_status_uses_upstream_error_extraction() {
    let host = mock();
    host.on_process("ddev", |_, _| Ok(ProcessOutput {
        status: Some(1), stdout: String::new(),
        stderr: "{\"level\":\"error\",\"msg\":\"first\"}\n{\"level\":\"fatal\",\"msg\":\"last failure\"}".into(),
    }));
    let err = run_json(&host.invocation().project().unwrap(), "describe")
        .unwrap_err()
        .to_string();
    assert!(err.contains("last failure"));
    assert!(!err.contains("first"));
    assert!(!err.contains("\"level\""));
}

#[test]
fn runtime_commands_revalidate_cached_state_with_watching_disabled() {
    let host = mock();
    host.set_setting("watchDdev", "false");
    let mut plugin = Main::default();
    plugin.render(VIEW, None, &host.invocation()).unwrap();
    host.on_process("ddev", |_, _| output(STOPPED));
    for id in [
        "stop",
        "restart",
        "share",
        "shell",
        "composer",
        "php",
        "node",
        "open-service",
    ] {
        assert!(
            plugin.run_command(id, &host.invocation(), None).is_err(),
            "{id}"
        );
    }
    assert!(plugin.terminal_launches.is_empty());
    host.on_process("ddev", |_, _| output(&running()));
    plugin.render(VIEW, None, &host.invocation()).unwrap();
    assert!(
        plugin
            .run_command("start", &host.invocation(), None)
            .is_err()
    );
}

#[test]
fn removed_configuration_cannot_reuse_cached_runtime() {
    let host = mock();
    let mut plugin = Main::default();
    plugin.render(VIEW, None, &host.invocation()).unwrap();
    // Recreate the host's filesystem without the configuration, retaining the
    // plugin instance and its cached running description at the same root.
    let host = MockHost::install().with_capabilities(&["project.read", "process:ddev"]);
    host.set_project_root(Some("/work/my project"));
    host.set_setting("watchDdev", "false");
    let view = plugin.render(VIEW, None, &host.invocation()).unwrap();
    assert!(view.status.unwrap().contains("Not configured"));
    assert!(
        !view.toolbar.iter().any(|b| b.id == "stop")
    );
    assert!(
        plugin
            .run_command("shell", &host.invocation(), None)
            .is_err()
    );
    assert!(host.process_runs().is_empty());
    assert_eq!(
        host.context_keys()["running"],
        openidea_plugin::ContextValue::Flag(false)
    );
}

#[test]
fn override_compose_and_project_preference_saves_refresh_runtime() {
    for path in [
        ".ddev/config.local.yaml",
        ".ddev/docker-compose.redis.yaml",
        ".openidea/settings.toml",
        r"C:\work\.ddev\config.local.yaml",
    ] {
        let host = mock();
        if path.starts_with("C:") {
            host.set_project_root(Some(r"C:\work"));
        }
        host.set_setting("watchDdev", "false");
        let mut plugin = Main::default();
        plugin.render(VIEW, None, &host.invocation()).unwrap();
        host.on_process("ddev", |_, _| output(STOPPED));
        plugin.handle_events(
            &[openidea_plugin::Event::FileSaved(path.into())],
            &host.invocation(),
        );
        assert!(
            plugin
                .render(VIEW, None, &host.invocation())
                .unwrap()
                .status
                .unwrap()
                .contains("stopped"),
            "{path}"
        );
    }
    let host = mock();
    let mut plugin = Main::default();
    plugin.render(VIEW, None, &host.invocation()).unwrap();
    let reads = host.process_runs().len();
    plugin.handle_events(
        &[openidea_plugin::Event::FileSaved(
            "/work/config.local.yaml".into(),
        )],
        &host.invocation(),
    );
    assert_eq!(host.process_runs().len(), reads);
}

#[test]
fn cached_render_restores_contexts_and_window_status_stays_project_specific() {
    let host = mock();
    let mut plugin = Main::default();
    plugin.render(VIEW, None, &host.invocation()).unwrap();
    let reads = host.process_runs().len();
    host::set_context("running", false).unwrap();
    host::set_context("known", false).unwrap();
    plugin.render(VIEW, None, &host.invocation()).unwrap();
    assert_eq!(
        host.context_keys()["running"],
        openidea_plugin::ContextValue::Flag(true)
    );
    assert_eq!(
        host.context_keys()["known"],
        openidea_plugin::ContextValue::Flag(true)
    );
    assert_eq!(host.process_runs().len(), reads);
    host.add_project("/work/other");
    plugin.render(VIEW, None, &host.invocation()).unwrap();
    assert_eq!(host.status_items()["ddev-status"].text, "DDEV: running");
    assert_eq!(
        host.context_keys()["running"],
        openidea_plugin::ContextValue::Flag(true)
    );
}

#[test]
fn recovery_actions_do_not_require_a_successful_description() {
    for id in ["delete", "poweroff", "restart", "stop"] {
        let host = mock();
        host.on_process("ddev", |args, _| {
            if args[0] == "--version" {
                output("ddev version v1.25.3\n")
            } else {
                Err("describe failed".into())
            }
        });
        let mut plugin = Main::default();
        // Stop/restart recover directly; destructive actions still confirm.
        host.answer_next_prompt(Some(&[id]));
        plugin
            .run_command("actions", &host.invocation(), None)
            .unwrap();
        if matches!(id, "restart" | "stop") {
            assert_eq!(plugin.terminal_launches[0].args, [id]);
            continue;
        }
        assert!(plugin.terminal_launches.is_empty());
        host.answer_next_prompt(Some(&["ok"]));
        plugin.run_command(id, &host.invocation(), None).unwrap();
        assert_eq!(plugin.terminal_launches[0].args, [id]);
    }
    let host = mock().with_capabilities(&["project.read"]);
    let mut plugin = Main::default();
    for id in ["delete", "poweroff", "restart", "stop"] {
        assert!(plugin.run_command(id, &host.invocation(), None).is_err());
    }
    assert!(plugin.terminal_launches.is_empty());
}

#[test]
fn project_browser_falls_back_to_web_service_url() {
    let host = mock();
    host.on_process("ddev", |_, _| output(&json!({"raw": {"name":"demo", "status":"running", "services": {"web":{"https_url":"https://web.ddev.site", "http_url":"http://web.ddev.site"}}}}).to_string()));
    let mut plugin = Main::default();
    let view = plugin.render(VIEW, None, &host.invocation()).unwrap();
    assert!(
        view.toolbar
            .iter()
            .find(|b| b.id == "open-service")
            .unwrap()
            .enabled
    );
    plugin
        .run_command("open-service", &host.invocation(), Some(PROJECT))
        .unwrap();
    assert_eq!(host.opened_urls(), ["https://web.ddev.site"]);
}

#[test]
fn database_duplicate_managed_ids_are_left_untouched() {
    let host = mock().with_capabilities(&["project.read", "project.write", "process:ddev"]);
    let project = host.invocation().project().unwrap();
    let id = database::source_id(&project).unwrap();
    let table = format!(
        "[[data-source]]\nid = '{id}'\ncomment = 'Managed by aritti.ddev'\nname = 'Existing'\n"
    );
    let original = table.repeat(2);
    host.add_file(database::FILE, &original);
    let desc = Description::parse(&database_output(32850)).unwrap();
    assert!(
        database::configure(&project, &desc)
            .unwrap_err()
            .to_string()
            .contains("Multiple data sources")
    );
    assert_eq!(project.read_file(database::FILE).unwrap(), original);
}

#[test]
fn log_editor_identity_is_stable_and_scoped_to_project_and_service() {
    let host = mock();
    let mut plugin = Main::default();
    plugin
        .run_command("logs", &host.invocation(), Some(PROJECT))
        .unwrap();
    let first_key = host.log_tabs()[0].key.clone();
    plugin
        .run_command("logs", &host.invocation(), Some(PROJECT))
        .unwrap();
    assert_eq!(host.log_tabs().len(), 1);
    assert_eq!(host.log_tabs()[0].key, first_key);
    plugin
        .run_command("logs", &host.invocation(), Some("service:db"))
        .unwrap();
    host.set_project_root(Some("/work/another project"));
    let mut other: serde_json::Value = serde_json::from_str(&running()).unwrap();
    other["raw"]["name"] = json!("another");
    host.on_process("ddev", move |_, _| output(&other.to_string()));
    plugin
        .run_command("logs", &host.invocation(), Some(PROJECT))
        .unwrap();
    let tabs = host.log_tabs();
    assert_eq!(tabs.len(), 3);
    assert_ne!(tabs[0].key, tabs[1].key);
    assert_ne!(tabs[0].key, tabs[2].key);
    assert!(tabs[0].title.contains("demo"));
    assert!(tabs[2].title.contains("another"));
    assert_eq!(tabs[0].log.args[1], "demo");
    assert_eq!(tabs[2].log.args[1], "another");
    assert!(tabs.iter().all(|tab| tab.key.len() <= 128));
}

#[test]
fn failure_deduplication_is_per_project_without_storage() {
    let host = mock();
    let mut plugin = Main::default();
    let first = host.invocation().project().unwrap();
    let error = Error::from("Docker unavailable");
    plugin.report_failure(&first, &error);
    host.set_project_root(Some("/work/second"));
    let second = host.invocation().project().unwrap();
    plugin.report_failure(&second, &error);
    assert_eq!(host.notifications().len(), 2);
    plugin.clear_failure_notice(&second);
    plugin.report_failure(&first, &error);
    assert_eq!(
        host.notifications().len(),
        2,
        "another project's recovery cannot reset the first"
    );
    plugin.report_failure(&second, &error);
    assert_eq!(host.notifications().len(), 3);
}

#[test]
fn pending_configuration_followups_survive_other_projects_without_storage() {
    let host = mock();
    let mut plugin = Main::default();
    host.answer_next_prompt(Some(&["ok"]));
    plugin
        .run_command("config", &host.invocation(), None)
        .unwrap();
    host.set_project_root(Some("/work/second"));
    host.add_file(CONFIG, "name: second\n");
    host.answer_next_prompt(Some(&["ok"]));
    plugin
        .run_command("config", &host.invocation(), None)
        .unwrap();
    assert_eq!(plugin.pending_configs.len(), 2);
    host.set_project_root(Some("/work/my project"));
    host.add_file(CONFIG, "name: first-changed\n");
    finish_default_config(&mut plugin, &host);
    assert_eq!(
        host.opened_locations()[0].0,
        "/work/my project/.ddev/config.yaml"
    );
    assert_eq!(plugin.pending_configs.len(), 1);
    host.set_project_root(Some("/work/second"));
    host.add_file(CONFIG, "name: second-changed\n");
    finish_default_config(&mut plugin, &host);
    assert_eq!(
        host.opened_locations()[1].0,
        "/work/second/.ddev/config.yaml"
    );
    assert!(plugin.pending_configs.is_empty());
}

#[test]
fn unsaved_setup_drafts_remain_independent_between_projects() {
    let host = mock().with_capabilities(&["project.read"]);
    let mut plugin = Main::default();
    for (root, version) in [("/work/my project", "8.2"), ("/work/second", "8.3")] {
        host.set_project_root(Some(root));
        plugin
            .handle(
                support::SETUP,
                ViewEvent::Field("phpVersion".into(), version.into()),
                &host.invocation(),
            )
            .unwrap();
    }
    for (root, version) in [("/work/my project", "8.2"), ("/work/second", "8.3")] {
        host.set_project_root(Some(root));
        let view = plugin
            .render(support::SETUP, None, &host.invocation())
            .unwrap();
        let Body::Form(fields) = view.body else {
            panic!("expected form");
        };
        assert!(
            matches!(&fields.iter().find(|f| f.key == "phpVersion").unwrap().value, openidea_plugin::views::FieldValue::Text(value) if value == version)
        );
        assert!(
            !host
                .invocation()
                .project()
                .unwrap()
                .exists(".openidea/settings.toml")
                .unwrap()
        );
    }
    assert!(host.process_runs().is_empty());
}

#[test]
fn scheduled_recovery_resets_failure_deduplication_without_publishing_contexts() {
    let host = mock().with_capabilities(&["project.read", "process:ddev", "storage"]);
    let mut plugin = Main::default();
    let project = host.invocation().project().unwrap();
    plugin.report_failure(&project, &Error::from("Docker unavailable"));
    host.add_project("/work/my project");
    host.set_project_root(None);
    host.on_process("ddev", |args, _| {
        if args[0] == "--version" {
            output("ddev version v1.25.3\n")
        } else {
            output(&running())
        }
    });
    let contexts = host.context_keys();
    plugin.handle_events(&[openidea_plugin::Event::Scheduled], &host.invocation());
    assert_eq!(host.context_keys(), contexts);
    assert_eq!(
        host.notifications().len(),
        1,
        "background recovery should not broadcast a new project notice"
    );
    host.set_project_root(Some("/work/my project"));
    plugin.report_failure(&project, &Error::from("Docker unavailable"));
    assert_eq!(
        host.notifications().len(),
        2,
        "the same failure must notify again after actual background recovery"
    );
}

#[test]
fn notification_actions_cannot_modify_another_projects_preferences() {
    let host = mock().with_capabilities(&[
        "project.read",
        "project.write",
        "process:ddev",
        "net:api.github.com",
    ]);
    host.on_fetch(|_| {
        Ok(openidea_plugin::HttpResponse {
            status: 200,
            headers: vec![],
            body: br#"{"tag_name":"v1.25.3"}"#.to_vec(),
        })
    });
    let mut plugin = Main::default();
    plugin
        .run_command("check-updates", &host.invocation(), None)
        .unwrap();
    let notice = host.notifications_with_actions()[0].clone();
    let action = notice
        .actions
        .iter()
        .find(|a| a.command == "disable-update-checks")
        .unwrap();
    assert!(notice.notification.body.contains("/work/my project"));
    assert_eq!(action.argument.as_deref(), Some("project:/work/my project"));
    host.set_project_root(Some("/work/second"));
    assert!(
        plugin
            .run_command(
                &action.command,
                &host.invocation(),
                action.argument.as_deref()
            )
            .is_err()
    );
    assert!(
        !host
            .invocation()
            .project()
            .unwrap()
            .exists(".openidea/settings.toml")
            .unwrap()
    );
    host.set_project_root(Some("/work/my project"));
    plugin
        .run_command(
            &action.command,
            &host.invocation(),
            action.argument.as_deref(),
        )
        .unwrap();
    assert!(!runtime::enabled_for(
        &host.invocation().project().unwrap(),
        "checkForUpdates"
    ));
    plugin.report_failure(
        &host.invocation().project().unwrap(),
        &Error::from("failed"),
    );
    let recovery = host.notifications_with_actions().last().unwrap().clone();
    host.set_project_root(Some("/work/second"));
    for action in recovery.actions.iter().filter(|a| a.argument.is_some()) {
        assert!(
            plugin
                .run_command(
                    &action.command,
                    &host.invocation(),
                    action.argument.as_deref()
                )
                .is_err()
        );
    }
}

#[test]
fn global_help_links_and_services_work_without_an_open_project() {
    let host = MockHost::install().with_capabilities(&[]);
    host.set_project_root(None);
    let mut plugin = Main::default();
    for id in [
        "open-guide",
        "open-releases",
        "open-docs",
        "show-services",
    ] {
        plugin.run_command(id, &host.invocation(), None).unwrap();
    }
    assert_eq!(host.opened_urls().len(), 3);
    assert!(host.opened_urls().iter().any(|url| url == "https://docs.ddev.com/en/stable/"));
    assert!(host.revealed_views().iter().any(|id| id == VIEW));
    assert!(matches!(
        plugin.render(VIEW, None, &host.invocation()).unwrap().body,
        Body::Tree(ref items) if items.len() == 1 && items[0].key == "status:no-project"
    ));
    assert!(host.process_runs().is_empty());
}

#[test]
fn background_refresh_discards_selection_of_a_removed_service() {
    let host = mock().with_capabilities(&["project.read", "process:ddev", "storage"]);
    host.set_setting("checkForUpdates", "false");
    let mut plugin = Main::default();
    plugin
        .handle(
            VIEW,
            ViewEvent::Select("service:redis".into()),
            &host.invocation(),
        )
        .unwrap();
    assert_eq!(plugin.selected.as_deref(), Some("service:redis"));
    host.add_project("/work/my project");
    host.set_project_root(None);
    host.on_process("ddev", |_, _| output(&json!({"raw":{"name":"demo","status":"running","services":{"web":{"https_url":"https://demo.ddev.site"}}}}).to_string()));
    plugin.handle_events(&[openidea_plugin::Event::Scheduled], &host.invocation());
    host.set_project_root(Some("/work/my project"));
    let reads = host.process_runs().len();
    plugin.render(VIEW, None, &host.invocation()).unwrap();
    assert!(plugin.selected.is_none());
    assert_eq!(
        host.process_runs().len(),
        reads,
        "the scheduled snapshot should be reused"
    );
}

#[test]
fn php_and_node_use_explicit_raw_exec_for_literal_arguments() {
    let host = mock();
    let mut plugin = Main::default();
    host.answer_next_prompt(Some(&[
        r#"-r '$value="literal"; echo "$HOME"; echo "`whoami`";' '' '#literal'"#,
    ]));
    plugin.run_command("php", &host.invocation(), None).unwrap();
    assert_eq!(
        plugin.terminal_launches[0].args,
        [
            "exec",
            "--raw",
            "--",
            "php",
            "-r",
            r#"$value="literal"; echo "$HOME"; echo "`whoami`";"#,
            "",
            "#literal"
        ]
    );
    host.answer_next_prompt(Some(&[r#"-e 'console.log("$HOME;`literal`")' ''"#]));
    plugin
        .run_command("node", &host.invocation(), None)
        .unwrap();
    assert_eq!(
        plugin.terminal_launches[1].args,
        [
            "exec",
            "--raw",
            "--",
            "node",
            "-e",
            r#"console.log("$HOME;`literal`")"#,
            ""
        ]
    );
}

#[test]
fn startup_initializes_all_projects_without_global_context_or_watching() {
    let host = mock();
    host.add_project("/work/first");
    host.add_project("/work/second");
    host.set_project_root(None);
    host.set_setting("watchDdev", "false");
    host.set_setting("checkForUpdates", "false");
    let mut plugin = Main::default();
    plugin
        .activate(&openidea_plugin::Context {
            plugin_id: "aritti.ddev".into(),
            plugin_version: "0.1.0".into(),
            api_version: "1.3".into(),
            capabilities: vec!["project.read".into(), "process:ddev".into()],
        })
        .unwrap();
    let runs = host.process_runs();
    assert_eq!(runs.len(), 2);
    assert!(
        runs.iter()
            .all(|run| run.args == ["describe", "--json-output"])
    );
    assert_eq!(runs[0].cwd.as_deref(), Some("/work/first"));
    assert_eq!(runs[1].cwd.as_deref(), Some("/work/second"));
    assert!(host.context_keys().is_empty());
    assert!(host.status_items().is_empty());
    assert!(host.notifications().is_empty());
}

#[test]
fn setup_reset_reloads_saved_values_without_writing_or_resetting_other_projects() {
    let host = mock().with_capabilities(&["project.read"]);
    let saved = "[plugins.'aritti.ddev']\nphpVersion='8.1'\nwatchDdev=false\n";
    host.add_file(".openidea/settings.toml", saved);
    let mut plugin = Main::default();
    for (root, value) in [("/work/my project", "8.2"), ("/work/second", "8.3")] {
        host.set_project_root(Some(root));
        plugin
            .handle(
                support::SETUP,
                ViewEvent::Field("phpVersion".into(), value.into()),
                &host.invocation(),
            )
            .unwrap();
    }
    host.set_project_root(Some("/work/my project"));
    plugin
        .handle(
            support::SETUP,
            ViewEvent::Button("reset-settings".into()),
            &host.invocation(),
        )
        .unwrap();
    for (root, expected) in [("/work/my project", "8.1"), ("/work/second", "8.3")] {
        host.set_project_root(Some(root));
        let view = plugin
            .render(support::SETUP, None, &host.invocation())
            .unwrap();
        let Body::Form(fields) = view.body else {
            panic!("expected form");
        };
        assert!(
            matches!(&fields.iter().find(|field| field.key == "phpVersion").unwrap().value,openidea_plugin::views::FieldValue::Text(value) if value == expected)
        );
        assert_eq!(
            host.invocation()
                .project()
                .unwrap()
                .read_file(".openidea/settings.toml")
                .unwrap(),
            saved
        );
    }
    assert!(host.process_runs().is_empty());
}

#[test]
fn preference_updates_support_inline_toml_tables_and_preserve_other_keys() {
    for saved in [
        "# Preferences\nplugins = { 'aritti.ddev' = { watchDdev = true, unknown = 'keep' }, other = { enabled = true } }\n",
        "# Preferences\n[plugins]\n'aritti.ddev' = { watchDdev = true, unknown = 'keep' }\nother = { enabled = true }\n",
        "# Preferences\nplugins = { other = { enabled = true } }\n",
    ] {
        let host = mock().with_capabilities(&["project.read", "project.write"]);
        host.add_file(".openidea/settings.toml", saved);
        let project = host.invocation().project().unwrap();
        support::save_preferences(
            &project,
            &[
                ("watchDdev", toml_edit::value(false)),
                ("phpVersion", toml_edit::value("8.3")),
            ],
        )
        .unwrap();
        let updated = project.read_file(".openidea/settings.toml").unwrap();
        let doc = updated.parse::<toml_edit::DocumentMut>().unwrap();
        assert_eq!(
            doc["plugins"]["aritti.ddev"]["watchDdev"].as_bool(),
            Some(false)
        );
        assert_eq!(
            doc["plugins"]["aritti.ddev"]["phpVersion"].as_str(),
            Some("8.3")
        );
        assert_eq!(doc["plugins"]["other"]["enabled"].as_bool(), Some(true));
        if saved.contains("unknown") {
            assert_eq!(
                doc["plugins"]["aritti.ddev"]["unknown"].as_str(),
                Some("keep")
            );
        }
        assert!(updated.contains("# Preferences"));
    }
}

#[test]
fn saving_preferences_applies_database_maintenance_without_periodic_watching() {
    let host = mock().with_capabilities(&["project.read", "project.write", "process:ddev"]);
    host.add_file(".openidea/settings.toml", "[plugins.'aritti.ddev']\nwatchDdev=false\nautoConfigureDataSource=false\ncheckForUpdates=false\n");
    host.on_process("ddev", |_, _| output(&database_output(32850)));
    let mut plugin = Main::default();
    plugin
        .handle(
            support::SETUP,
            ViewEvent::Field("autoConfigureDataSource".into(), "true".into()),
            &host.invocation(),
        )
        .unwrap();
    assert!(
        !host
            .invocation()
            .project()
            .unwrap()
            .exists(database::FILE)
            .unwrap()
    );
    plugin
        .handle(
            support::SETUP,
            ViewEvent::Button("save-settings".into()),
            &host.invocation(),
        )
        .unwrap();
    assert!(
        host.invocation()
            .project()
            .unwrap()
            .exists(database::FILE)
            .unwrap()
    );
    assert!(!runtime::enabled_for(
        &host.invocation().project().unwrap(),
        "watchDdev"
    ));
    assert_eq!(host.process_runs().len(), 1);
}

#[test]
fn toolbar_uses_current_project_selection_before_the_first_render() {
    let host = mock().with_capabilities(&["project.read", "process:ddev", "storage"]);
    let mut plugin = Main::default();
    plugin
        .handle(
            VIEW,
            ViewEvent::Select("service:redis".into()),
            &host.invocation(),
        )
        .unwrap();
    host::storage::set("selection:/work/second", "service:db").unwrap();
    host.set_project_root(Some("/work/second"));
    plugin
        .handle(VIEW, ViewEvent::Button("shell".into()), &host.invocation())
        .unwrap();
    assert_eq!(plugin.terminal_launches[0].args, ["ssh", "--service=db"]);
    host.set_project_root(Some("/work/my project"));
    plugin
        .handle(VIEW, ViewEvent::Button("shell".into()), &host.invocation())
        .unwrap();
    assert_eq!(plugin.terminal_launches[1].args, ["ssh", "--service=redis"]);
}

#[test]
fn nested_foreign_and_empty_filesystem_events_do_not_refresh_root_project() {
    let host = mock();
    host.set_setting("watchDdev", "false");
    let mut plugin = Main::default();
    plugin.render(VIEW, None, &host.invocation()).unwrap();
    let reads = host.process_runs().len();
    for path in [
        "/work/my project/nested/.ddev/config.yaml",
        "nested/.ddev/config.local.yaml",
        "/other/.ddev/config.yaml",
        "/work/my project-two/.ddev/config.yaml",
        "/other/.openidea/settings.toml",
    ] {
        plugin.handle_events(
            &[openidea_plugin::Event::FileSaved(path.into())],
            &host.invocation(),
        );
        plugin.handle_events(
            &[openidea_plugin::Event::FilesChanged(
                openidea_plugin::FilesChanged {
                    modified: vec![path.into()],
                    ..Default::default()
                },
            )],
            &host.invocation(),
        );
    }
    plugin.handle_events(
        &[openidea_plugin::Event::FilesChanged(Default::default())],
        &host.invocation(),
    );
    assert_eq!(host.process_runs().len(), reads);
    plugin.handle_events(
        &[openidea_plugin::Event::FilesChanged(
            openidea_plugin::FilesChanged {
                modified: vec!["/work/my project/.ddev/config.local.yaml".into()],
                ..Default::default()
            },
        )],
        &host.invocation(),
    );
    assert_eq!(host.process_runs().len(), reads + 1);
}

#[test]
fn removing_configuration_directory_clears_contexts_even_with_watching_off() {
    let host = mock();
    let mut plugin = Main::default();
    plugin.render(VIEW, None, &host.invocation()).unwrap();
    let host = MockHost::install().with_capabilities(&["project.read"]);
    host.set_project_root(Some("/work/my project"));
    host.set_setting("watchDdev", "false");
    host::set_context("running", true).unwrap();
    host::set_context("configured", true).unwrap();
    plugin.handle_events(
        &[openidea_plugin::Event::FilesChanged(
            openidea_plugin::FilesChanged {
                removed: vec!["/work/my project/.ddev".into()],
                ..Default::default()
            },
        )],
        &host.invocation(),
    );
    assert_eq!(
        host.context_keys()["running"],
        openidea_plugin::ContextValue::Flag(false)
    );
    assert_eq!(
        host.context_keys()["configured"],
        openidea_plugin::ContextValue::Flag(false)
    );
    assert!(host.process_runs().is_empty());
}

#[test]
fn command_availability_contexts_distinguish_starting_database_and_cli_failure() {
    let host = mock();
    let mut plugin = Main::default();
    host.on_process("ddev", |_, _| {
        output(&json!({"raw":{"name":"demo","status":"starting"}}).to_string())
    });
    plugin.render(VIEW, None, &host.invocation()).unwrap();
    let flags = host.context_keys();
    assert_eq!(
        flags["available"],
        openidea_plugin::ContextValue::Flag(true)
    );
    assert_eq!(
        flags["can-start"],
        openidea_plugin::ContextValue::Flag(false)
    );
    assert_eq!(flags["running"], openidea_plugin::ContextValue::Flag(false));
    assert_eq!(
        flags["has-database"],
        openidea_plugin::ContextValue::Flag(false)
    );
    host.on_process("ddev", |_, _| output(&database_output(32850)));
    plugin
        .run_command("refresh", &host.invocation(), None)
        .unwrap();
    assert_eq!(
        host.context_keys()["has-database"],
        openidea_plugin::ContextValue::Flag(true)
    );
    host.on_process("ddev", |args, _| {
        if args[0] == "--version" {
            output("ddev version v1.25.3\n")
        } else {
            Err("describe unavailable".into())
        }
    });
    assert!(
        plugin
            .run_command("refresh", &host.invocation(), None)
            .is_err()
    );
    assert_eq!(
        host.context_keys()["available"],
        openidea_plugin::ContextValue::Flag(true)
    );
    assert_eq!(
        host.context_keys()["has-database"],
        openidea_plugin::ContextValue::Flag(false)
    );
    host.on_process("ddev", |_, _| Err("CLI unavailable".into()));
    assert!(
        plugin
            .run_command("refresh", &host.invocation(), None)
            .is_err()
    );
    assert_eq!(
        host.context_keys()["available"],
        openidea_plugin::ContextValue::Flag(false)
    );
}

#[test]
fn windows_events_match_root_case_and_separators_without_matching_other_roots() {
    let host = mock();
    host.set_project_root(Some(r"C:\Work"));
    host.set_setting("watchDdev", "false");
    let mut plugin = Main::default();
    plugin.render(VIEW, None, &host.invocation()).unwrap();
    let reads = host.process_runs().len();
    for path in [
        r"D:\Work\.ddev\config.yaml",
        r"c:\work\nested\.ddev\config.yaml",
    ] {
        plugin.handle_events(
            &[openidea_plugin::Event::FileSaved(path.into())],
            &host.invocation(),
        );
    }
    assert_eq!(host.process_runs().len(), reads);
    plugin.handle_events(
        &[openidea_plugin::Event::FileSaved(
            r"c:\work\.DDEV\CONFIG.LOCAL.YAML".into(),
        )],
        &host.invocation(),
    );
    assert_eq!(host.process_runs().len(), reads + 1);
}

#[test]
fn compose_service_names_with_leading_punctuation_use_unambiguous_option_values() {
    for service in ["_redis", ".cache", "--fixture"] {
        let host = mock();
        let mut services = serde_json::Map::new();
        services.insert(
            service.into(),
            json!({"status":"running","http_url":"http://localhost:9000"}),
        );
        let description =
            json!({"raw":{"name":"demo","status":"running","services":services}}).to_string();
        host.on_process("ddev", move |_, _| output(&description));
        let mut plugin = Main::default();
        let key = format!("service:{service}");
        let view = plugin.render(VIEW, None, &host.invocation()).unwrap();
        assert!(matches!(view.body,Body::Tree(ref rows) if rows.iter().any(|row|row.key==key)));
        plugin
            .run_command("shell", &host.invocation(), Some(&key))
            .unwrap();
        assert_eq!(
            plugin.terminal_launches[0].args,
            ["ssh", &format!("--service={service}")]
        );
        plugin
            .run_command("logs", &host.invocation(), Some(&key))
            .unwrap();
        assert_eq!(
            host.log_tabs()[0].log.args.last().unwrap(),
            &format!("--service={service}")
        );
        assert!(!host.log_tabs()[0].log.args.iter().any(|arg| arg == service));
    }
    for invalid in ["", "bad name", "x/y", "x;echo", "é"] {
        assert!(!model::safe_service_name(invalid));
    }
}

#[test]
fn cached_action_picker_republishes_window_context_without_cli_reads() {
    let host = mock();
    let mut plugin = Main::default();
    plugin.render(VIEW, None, &host.invocation()).unwrap();
    let reads = host.process_runs().len();
    host::set_context("running", false).unwrap();
    plugin
        .run_command("actions", &host.invocation(), None)
        .unwrap();
    assert_eq!(
        host.context_keys()["running"],
        openidea_plugin::ContextValue::Flag(true)
    );
    assert_eq!(host.process_runs().len(), reads);
}

#[test]
fn focusing_window_after_background_refresh_updates_cached_command_enablement() {
    let host = mock().with_capabilities(&["project.read", "process:ddev", "storage", "events"]);
    host.set_setting("checkForUpdates", "false");
    let mut plugin = Main::default();
    plugin.render(VIEW, None, &host.invocation()).unwrap();
    host.add_project("/work/my project");
    host.set_project_root(None);
    host.on_process("ddev", |_, _| output(STOPPED));
    plugin.handle_events(&[openidea_plugin::Event::Scheduled], &host.invocation());
    let reads = host.process_runs().len();
    assert_eq!(
        host.context_keys()["running"],
        openidea_plugin::ContextValue::Flag(true),
        "windowless refresh must not publish globally"
    );
    host.set_project_root(Some("/work/my project"));
    plugin.handle_events(
        &[openidea_plugin::Event::EditorFocused(Some(
            "file.php".into(),
        ))],
        &host.invocation(),
    );
    assert_eq!(
        host.context_keys()["running"],
        openidea_plugin::ContextValue::Flag(false)
    );
    assert_eq!(
        host.context_keys()["can-start"],
        openidea_plugin::ContextValue::Flag(true)
    );
    assert_eq!(
        host.process_runs().len(),
        reads,
        "focus should reuse the background snapshot"
    );
}

#[test]
fn live_project_selections_do_not_require_persistent_storage() {
    let host = mock().with_capabilities(&["project.read", "process:ddev"]);
    let mut plugin = Main::default();
    plugin
        .handle(
            VIEW,
            ViewEvent::Select("service:redis".into()),
            &host.invocation(),
        )
        .unwrap();
    host.set_project_root(Some("/work/second"));
    plugin
        .handle(
            VIEW,
            ViewEvent::Select("service:db".into()),
            &host.invocation(),
        )
        .unwrap();
    host.set_project_root(Some("/work/my project"));
    plugin
        .handle(VIEW, ViewEvent::Button("shell".into()), &host.invocation())
        .unwrap();
    assert_eq!(plugin.terminal_launches[0].args, ["ssh", "--service=redis"]);
    host.set_project_root(Some("/work/second"));
    plugin
        .handle(VIEW, ViewEvent::Button("shell".into()), &host.invocation())
        .unwrap();
    assert_eq!(plugin.terminal_launches[1].args, ["ssh", "--service=db"]);
}

#[test]
fn api14_native_database_uses_owned_sources_without_project_write() {
    let host = mock().with_capabilities(&["project.read", "process:ddev", "database.sources"]);
    host.on_process("ddev", |_, _| output(&database_output(32850)));
    let project = host.invocation().project().unwrap();
    let mut plugin = Main::default();
    plugin
        .run_command("setup-database", &host.invocation(), None)
        .unwrap();
    assert_eq!(
        project.data_sources().unwrap(),
        vec![database::source_id(&project).unwrap()]
    );
    assert!(!project.exists(database::FILE).unwrap());
    let desc = Description::parse(&database_output(32851)).unwrap();
    let source = database::source(&project, &desc).unwrap();
    assert_eq!(source.driver, "mysql");
    assert_eq!(source.port, Some(32851));
    assert!(
        !serde_json::to_string(&source)
            .unwrap()
            .contains("secret-for-test")
    );
    assert!(!database::configure(&project, &desc).unwrap());
    assert_eq!(project.data_sources().unwrap().len(), 1);
}

#[test]
fn api14_native_database_rejects_incomplete_credentials_and_invalid_ports() {
    let host = mock().with_capabilities(&["project.read", "process:ddev", "database.sources"]);
    let project = host.invocation().project().unwrap();
    for (field, invalid) in [
        ("published_port", json!(70000)),
        ("published_port", json!(0)),
        ("password", json!(null)),
        ("dbname", json!("")),
        ("database_type", json!("unknown")),
    ] {
        let mut data: serde_json::Value = serde_json::from_str(&database_output(32850)).unwrap();
        data["raw"]["dbinfo"][field] = invalid;
        let desc = Description::parse(&data.to_string()).unwrap();
        assert!(database::configure(&project, &desc).is_err(), "{field}");
        assert!(project.data_sources().unwrap().is_empty());
    }
}

#[test]
fn api14_target_registers_once_and_is_withdrawn_when_disabled_or_unconfigured() {
    let host = mock();
    let mut plugin = Main::default();
    plugin.render(VIEW, None, &host.invocation()).unwrap();
    let targets = host.run_targets();
    let target = targets.values().next().unwrap();
    assert_eq!(target.program, "ddev");
    assert_eq!(target.args, ["exec", "--raw"]);
    assert_eq!(target.workdir_args, ["--dir={cwd}", "--"]);
    assert_eq!(target.container_root.as_deref(), Some("/var/www/html"));
    assert_eq!(target.commands.len(), 1);
    let overrides = host.run_targets_with_overrides();
    let composer = &overrides.values().next().unwrap().commands[0];
    assert_eq!(composer.name, "composer");
    assert_eq!(composer.args, ["composer"]);
    assert_eq!(composer.workdir_args, Some(vec![]));
    host.set_setting("autoConfigureRunTarget", "false");
    plugin.render(VIEW, None, &host.invocation()).unwrap();
    assert!(host.run_targets().is_empty());
    host.set_setting("autoConfigureRunTarget", "true");
    host.set_setting("containerRoot", "\"/srv/app\"");
    plugin.render(VIEW, None, &host.invocation()).unwrap();
    assert_eq!(
        host.run_targets()
            .values()
            .next()
            .unwrap()
            .container_root
            .as_deref(),
        Some("/srv/app")
    );
    // Explicit withdrawal clears the registration and the plugin cache.
    host.set_setting("watchDdev", "false");
    plugin.sync_target(&host.invocation().project().unwrap(), None);
    assert!(host.run_targets().is_empty());
}

#[test]
fn api14_terminal_exit_refreshes_with_watch_disabled_after_instance_reclaim() {
    let host = mock();
    host.set_setting("watchDdev", "false");
    host.on_process("ddev", |args, _| {
        if args[0] == "--version" {
            output("ddev version v1.25.5")
        } else {
            output(STOPPED)
        }
    });
    let mut plugin = Main::default();
    plugin
        .run_command("start", &host.invocation(), None)
        .unwrap();
    let project = host.invocation().project().unwrap();
    let key = execution::terminal_key(&project, "start", &["start".into()]).unwrap();
    let event = host.finish_terminal(&key).unwrap();
    host.on_process("ddev", |_, _| output(&running()));
    let mut reclaimed = Main::default();
    reclaimed.handle_events(&[event], &host.invocation());
    assert_eq!(
        host.context_keys()["running"],
        openidea_plugin::ContextValue::Flag(true)
    );
    assert_eq!(host.status_items()["ddev-status"].text, "DDEV: running");
    assert!(
        host.run_targets()
            .values()
            .any(|target| target.id == "ddev")
    );
    assert_eq!(
        plugin.terminal_launches[0].env,
        [("DDEV_NONINTERACTIVE".into(), "true".into())]
    );
    assert!(
        host.process_runs()
            .iter()
            .all(|run| run.env == [("DDEV_NONINTERACTIVE".into(), "true".into())])
    );
}

#[test]
fn api14_config_exit_opens_even_an_unchanged_file_and_failure_clears_followup() {
    let host = mock().with_capabilities(&["project.read", "process:ddev", "storage"]);
    host.answer_next_prompt(Some(&["ok"]));
    let mut plugin = Main::default();
    plugin
        .run_command("config", &host.invocation(), None)
        .unwrap();
    let project = host.invocation().project().unwrap();
    let args = plugin.terminal_launches[0].args.clone();
    let key = execution::terminal_key(&project, "config", &args).unwrap();
    let event = host.finish_terminal(&key).unwrap();
    Main::default().handle_events(&[event], &host.invocation());
    assert_eq!(host.opened_locations().len(), 1);
    host.answer_next_prompt(Some(&["ok"]));
    plugin
        .run_command("config", &host.invocation(), None)
        .unwrap();
    let mut exit = host
        .finish_terminal(&key)
        .unwrap()
        .terminal_exited()
        .unwrap();
    exit.status = Some(2);
    plugin.handle_events(
        &[openidea_plugin::Event::Other {
            kind: "terminal-exited".into(),
            data: serde_json::to_string(&exit).unwrap(),
        }],
        &host.invocation(),
    );
    assert_eq!(host.opened_locations().len(), 1);
    assert!(plugin.pending_configs.is_empty());
    assert!(
        host.notifications()
            .iter()
            .any(|notification| notification.title == "DDEV command failed")
    );
}

#[test]
fn api14_terminal_completion_is_scoped_to_the_originating_project() {
    let host = mock();
    let first = host.invocation().project().unwrap();
    let key = execution::terminal_key(&first, "stop", &["stop".into()]).unwrap();
    let event = openidea_plugin::Event::Other {
        kind: "terminal-exited".into(),
        data: json!({"id":"operation", "key":key, "status":0,"closed":false}).to_string(),
    };
    host.set_project_root(Some("/work/second"));
    let reads = host.process_runs().len();
    assert!(!Main::default().terminal_events(&[event], &host.invocation()));
    assert_eq!(host.process_runs().len(), reads);
}

#[test]
fn api14_overview_and_inspection_do_not_expose_database_credentials() {
    let host = mock();
    host.on_process("ddev", |_, _| output(&database_output(32850)));
    let tabs = Main::default()
        .detail(VIEW, PROJECT, &host.invocation())
        .unwrap();
    assert_eq!(tabs[0].id, "overview");
    for tab in tabs {
        if let Pane::Text(text) = tab.body {
            assert!(!text.contains("secret-for-test"));
        }
    }
}

#[test]
fn api14_secure_database_migrates_only_the_marked_legacy_entry() {
    let host = mock().with_capabilities(&[
        "project.read",
        "project.write",
        "process:ddev",
        "database.sources",
    ]);
    let project = host.invocation().project().unwrap();
    let id = database::source_id(&project).unwrap();
    host.add_file(database::FILE, &format!("# User data\n[[data-source]]\nid='mine'\nname='Personal'\ndriver='mysql'\n\n[[data-source]]\nid='{id}'\ncomment='Managed by aritti.ddev'\nname='Old DDEV'\ndriver='mysql'\n"));
    database::configure(
        &project,
        &Description::parse(&database_output(32850)).unwrap(),
    )
    .unwrap();
    let text = project.read_file(database::FILE).unwrap();
    assert!(text.contains("Personal") && text.contains("# User data"));
    assert!(!text.contains("Old DDEV") && !text.contains("secret-for-test"));
    assert_eq!(project.data_sources().unwrap(), vec![id]);
}

#[test]
fn api14_invalid_mapping_withdraws_an_existing_target() {
    let host = mock();
    let mut plugin = Main::default();
    plugin.render(VIEW, None, &host.invocation()).unwrap();
    assert_eq!(host.run_targets().len(), 1);
    host.set_setting("containerRoot", "\"/srv/../escape\"");
    plugin.render(VIEW, None, &host.invocation()).unwrap();
    assert!(host.run_targets().is_empty());
    assert!(!support::valid_container_root("relative"));
    assert!(!support::valid_container_root("/bad\npath"));
}

#[test]
fn api14_status_picker_opens_a_service_link_directly() {
    let host = mock();
    let mut plugin = Main::default();
    host.answer_next_prompt(Some(&["open-url:service:mailpit"]));
    plugin
        .run_command("actions", &host.invocation(), None)
        .unwrap();
    assert_eq!(host.opened_urls(), ["https://demo.ddev.site:8026"]);
}

#[test]
fn parity_native_database_skips_identical_updates_and_detects_rotation_or_removal() {
    let host = mock().with_capabilities(&["project.read", "process:ddev", "database.sources"]);
    let project = host.invocation().project().unwrap();
    let root = project.root().unwrap();
    let mut plugin = Main::default();
    let original = Description::parse(&database_output(32850)).unwrap();
    assert!(
        plugin
            .configure_database(&project, &original, false)
            .unwrap()
    );
    plugin.connected_databases.insert(root.clone());
    assert!(
        !plugin
            .configure_database(&project, &original, false)
            .unwrap()
    );
    assert!(
        plugin.connected_databases.contains(&root),
        "unchanged source must not lose its live connection"
    );
    let mut rotated: serde_json::Value = serde_json::from_str(&database_output(32850)).unwrap();
    rotated["raw"]["dbinfo"]["password"] = json!("rotated-test-password");
    let rotated = Description::parse(&rotated.to_string()).unwrap();
    assert!(
        plugin
            .configure_database(&project, &rotated, false)
            .unwrap()
    );
    assert!(!plugin.connected_databases.contains(&root));
    assert!(
        !plugin
            .configure_database(&project, &rotated, false)
            .unwrap()
    );
    assert!(
        plugin
            .configure_database(
                &project,
                &Description::parse(&database_output(32851)).unwrap(),
                false
            )
            .unwrap()
    );
    project
        .remove_data_source(&database::source_id(&project).unwrap())
        .unwrap();
    assert!(
        plugin
            .configure_database(&project, &original, false)
            .unwrap(),
        "a removed source must be recreated"
    );
    assert!(
        plugin
            .configure_database(&project, &original, true)
            .unwrap(),
        "explicit setup can reapply user-edited owned settings"
    );
}

#[test]
fn parity_target_preferences_withdraw_stale_mapping_without_a_working_cli() {
    for preference in [
        "autoConfigureRunTarget=false",
        "containerRoot='/srv/changed'",
        "containerRoot='/srv/../invalid'",
    ] {
        let host = mock();
        let mut plugin = Main::default();
        plugin.render(VIEW, None, &host.invocation()).unwrap();
        assert_eq!(host.run_targets().len(), 1);
        host.add_file(
            ".openidea/settings.toml",
            &format!("[plugins.'aritti.ddev']\nwatchDdev=false\n{preference}\n"),
        );
        host.on_process("ddev", |_, _| Err("DDEV is temporarily unavailable".into()));
        plugin.handle_events(
            &[openidea_plugin::Event::FileSaved(
                "/work/my project/.openidea/settings.toml".into(),
            )],
            &host.invocation(),
        );
        assert!(host.run_targets().is_empty(), "{preference}");
        assert!(plugin.run_targets.is_empty());
    }
}

#[test]
fn parity_background_cli_failure_withdraws_an_existing_target() {
    let host = mock();
    let mut plugin = Main::default();
    plugin.render(VIEW, None, &host.invocation()).unwrap();
    assert_eq!(host.run_targets().len(), 1);
    host.on_process("ddev", |_, _| Err("Docker is unavailable".into()));
    plugin.refresh_background(&host.invocation().project().unwrap());
    assert!(host.run_targets().is_empty());
    host.on_process("ddev", |_, _| output(&running()));
    plugin.refresh_background(&host.invocation().project().unwrap());
    assert_eq!(host.run_targets().len(), 1, "recovery must register again");
}

#[test]
fn parity_action_picker_omits_database_setup_when_no_database_is_reported() {
    let host = mock();
    let mut plugin = Main::default();
    host.answer_next_prompt(Some(&["setup-database"]));
    let error = plugin
        .run_command("actions", &host.invocation(), None)
        .unwrap_err();
    assert!(matches!(error, Error::Invalid(_)), "{error}");
    assert!(runtime::allowed(
        &Description::parse(&database_output(32850)).unwrap(),
        "setup-database"
    ));
    assert!(!runtime::allowed(
        &Description::parse(&running()).unwrap(),
        "setup-database"
    ));
}

#[test]
fn multipass_moved_native_database_reuses_owned_identity_and_connects() {
    let host = mock().with_capabilities(&["project.read", "process:ddev", "database.sources"]);
    let original = host.invocation().project().unwrap();
    let desc = Description::parse(&database_output(32850)).unwrap();
    let moved_metadata = database::source(&original, &desc).unwrap();
    let old_id = moved_metadata.id.clone();
    host.set_project_root(Some("/work/renamed-project"));
    host.add_file(CONFIG, "name: demo\n");
    let moved = host.invocation().project().unwrap();
    assert_ne!(database::source_id(&moved).unwrap(), old_id);
    moved.upsert_data_source(&moved_metadata).unwrap();
    host.on_process("ddev", |_, _| output(&database_output(32850)));
    Main::default()
        .run_command("setup-database", &host.invocation(), None)
        .unwrap();
    assert_eq!(moved.data_sources().unwrap(), [old_id]);
    assert!(
        host.notifications()
            .last()
            .unwrap()
            .body
            .contains("secure credential")
    );
    assert!(!moved.exists(database::FILE).unwrap());
    database::connect(&moved).unwrap();
}

#[test]
fn multipass_moved_legacy_database_reuses_identity_and_user_options() {
    let host = mock().with_capabilities(&["project.read", "project.write", "process:ddev"]);
    let old_id = database::source_id(&host.invocation().project().unwrap()).unwrap();
    host.set_project_root(Some("/work/moved-project"));
    let project = host.invocation().project().unwrap();
    host.add_file(database::FILE, &format!("# preserved\n[[data-source]]\nid='{old_id}'\ncomment='Managed by aritti.ddev'\nname='Old DDEV'\nsslMode='verify-full'\n\n[[data-source]]\nid='personal'\nname='Personal'\ndriver='postgres'\n"));
    database::configure(
        &project,
        &Description::parse(&database_output(32850)).unwrap(),
    )
    .unwrap();
    let text = project.read_file(database::FILE).unwrap();
    let doc: toml_edit::DocumentMut = text.parse().unwrap();
    let sources = doc["data-source"].as_array_of_tables().unwrap();
    assert_eq!(sources.len(), 2);
    assert_eq!(
        sources.get(0).unwrap()["id"].as_str(),
        Some(old_id.as_str())
    );
    assert_eq!(
        sources.get(0).unwrap()["sslMode"].as_str(),
        Some("verify-full")
    );
    assert!(text.contains("# preserved") && text.contains("Personal"));
}

#[test]
fn multipass_ambiguous_native_sources_are_not_reassigned() {
    let host = mock().with_capabilities(&["project.read", "process:ddev", "database.sources"]);
    let project = host.invocation().project().unwrap();
    let desc = Description::parse(&database_output(32850)).unwrap();
    for id in [
        "aritti.ddev.1111111111111111",
        "aritti.ddev.2222222222222222",
    ] {
        let mut source = database::source(&project, &desc).unwrap();
        source.id = id.into();
        project.upsert_data_source(&source).unwrap();
    }
    let error = Main::default()
        .configure_database(&project, &desc, false)
        .unwrap_err();
    assert!(error.to_string().contains("ambiguity"));
    assert_eq!(project.data_sources().unwrap().len(), 2);
}

#[test]
fn multipass_native_setup_retires_a_single_moved_legacy_source() {
    let host = mock().with_capabilities(&[
        "project.read",
        "project.write",
        "process:ddev",
        "database.sources",
    ]);
    let old_id = database::source_id(&host.invocation().project().unwrap()).unwrap();
    host.set_project_root(Some("/work/new-location"));
    let project = host.invocation().project().unwrap();
    host.add_file(database::FILE, &format!("[[data-source]]\nid='{old_id}'\ncomment='Managed by aritti.ddev'\nname='Old DDEV'\n\n[[data-source]]\nid='personal'\nname='Personal'\ndriver='mysql'\n"));
    Main::default()
        .configure_database(
            &project,
            &Description::parse(&database_output(32850)).unwrap(),
            false,
        )
        .unwrap();
    let text = project.read_file(database::FILE).unwrap();
    assert!(!text.contains("Old DDEV"));
    assert!(text.contains("Personal"));
    assert_eq!(project.data_sources().unwrap().len(), 1);
}

#[test]
fn multipass_configure_rejects_changed_arguments_until_the_active_operation_finishes() {
    let host = mock();
    let mut plugin = Main::default();
    host.set_setting("phpVersion", "\"8.2\"");
    host.answer_next_prompt(Some(&["ok"]));
    plugin
        .run_command("config", &host.invocation(), None)
        .unwrap();
    let project = host.invocation().project().unwrap();
    let key =
        execution::terminal_key(&project, "config", &plugin.terminal_launches[0].args).unwrap();
    host.set_setting("phpVersion", "\"8.3\"");
    host.answer_next_prompt(Some(&["ok"]));
    assert!(
        plugin
            .run_command("config", &host.invocation(), None)
            .unwrap_err()
            .to_string()
            .contains("already in progress")
    );
    assert_eq!(plugin.terminal_launches.len(), 1);
    let exit = host.finish_terminal(&key).unwrap();
    plugin.handle_events(&[exit], &host.invocation());
    assert_eq!(host.opened_locations().len(), 1);
    host.answer_next_prompt(Some(&["ok"]));
    plugin
        .run_command("config", &host.invocation(), None)
        .unwrap();
    assert_eq!(plugin.terminal_launches[1].args.last().unwrap(), "8.3");
}

#[test]
fn multipass_old_config_completion_cannot_consume_a_new_followup_after_reclaim() {
    let host = mock();
    host.set_setting("phpVersion", "\"8.2\"");
    host.answer_next_prompt(Some(&["ok"]));
    let mut original = Main::default();
    original
        .run_command("config", &host.invocation(), None)
        .unwrap();
    let project = host.invocation().project().unwrap();
    let old_key =
        execution::terminal_key(&project, "config", &original.terminal_launches[0].args).unwrap();
    let mut reclaimed = Main::default();
    host.set_setting("phpVersion", "\"8.3\"");
    host.answer_next_prompt(Some(&["ok"]));
    reclaimed
        .run_command("config", &host.invocation(), None)
        .unwrap();
    let new_key =
        execution::terminal_key(&project, "config", &reclaimed.terminal_launches[0].args).unwrap();
    let old_exit = host.finish_terminal(&old_key).unwrap();
    reclaimed.handle_events(&[old_exit], &host.invocation());
    assert!(host.opened_locations().is_empty());
    assert_eq!(reclaimed.pending_configs.len(), 1);
    let new_exit = host.finish_terminal(&new_key).unwrap();
    reclaimed.handle_events(&[new_exit], &host.invocation());
    assert_eq!(host.opened_locations().len(), 1);
    assert!(reclaimed.pending_configs.is_empty());
}

#[test]
fn multipass_delayed_exit_of_a_finished_config_cannot_complete_a_new_identical_run() {
    let host = mock();
    host.answer_next_prompt(Some(&["ok"]));
    let mut first = Main::default();
    first
        .run_command("config", &host.invocation(), None)
        .unwrap();
    let project = host.invocation().project().unwrap();
    let key =
        execution::terminal_key(&project, "config", &first.terminal_launches[0].args).unwrap();
    let delayed = host.finish_terminal(&key).unwrap();
    let mut second = Main::default();
    host.answer_next_prompt(Some(&["ok"]));
    second
        .run_command("config", &host.invocation(), None)
        .unwrap();
    second.handle_events(&[delayed], &host.invocation());
    assert!(host.opened_locations().is_empty());
    assert_eq!(second.pending_configs.len(), 1);
    let current = host.finish_terminal(&key).unwrap();
    second.handle_events(&[current], &host.invocation());
    assert_eq!(host.opened_locations().len(), 1);
    assert!(second.pending_configs.is_empty());
}

#[test]
fn recheck_config_deadline_belongs_to_the_host_operation_not_its_reusable_key() {
    for (completed, age) in [(false, 120), (true, 120), (false, 1900), (true, 1900)] {
        let host = mock().with_capabilities(&["project.read", "process:ddev", "storage"]);
        host.answer_next_prompt(Some(&["ok"]));
        let mut original = Main::default();
        original
            .run_command("config", &host.invocation(), None)
            .unwrap();
        let project = host.invocation().project().unwrap();
        let key = execution::terminal_key(&project, "config", &original.terminal_launches[0].args)
            .unwrap();
        let storage = support::key(&project, "pending-config").unwrap();
        let mut saved: serde_json::Value =
            serde_json::from_str(&host::storage::get(&storage).unwrap().unwrap()).unwrap();
        let old_terminal = saved["terminal"].as_str().unwrap().to_owned();
        let old_deadline_start = runtime::now() - age;
        saved["requested"] = json!(old_deadline_start);
        host::storage::set(&storage, &saved.to_string()).unwrap();
        if completed {
            let _queued_exit = host.finish_terminal(&key).unwrap();
        }
        let mut reclaimed = Main::default();
        host.answer_next_prompt(Some(&["ok"]));
        let before = runtime::now();
        reclaimed
            .run_command("config", &host.invocation(), None)
            .unwrap();
        let saved: serde_json::Value =
            serde_json::from_str(&host::storage::get(&storage).unwrap().unwrap()).unwrap();
        if completed {
            assert_ne!(saved["terminal"].as_str(), Some(old_terminal.as_str()));
            assert!(saved["requested"].as_u64().unwrap() >= before);
        } else {
            assert_eq!(saved["terminal"].as_str(), Some(old_terminal.as_str()));
            assert_eq!(saved["requested"].as_u64(), Some(old_deadline_start));
        }
    }
}

#[test]
fn custom_setup_bridge_preserves_drafts_readonly_fields_and_project_authority() {
    let host=mock().with_capabilities(&["project.read","project.write"]);
    let saved="# Keep this\n[editor]\nfontSize=15\n[plugins.'aritti.ddev']\nphpVersion='8.1'\n";
    host.add_file(".openidea/settings.toml",saved);
    let mut plugin=Main::default();
    let request=|plugin:&mut Main, mut request:serde_json::Value| {
        request["channel"]=json!("ui-kit");request["sequence"]=json!(1);
        if request.get("project").is_none(){request["project"]=json!("/work/my project");}
        plugin.handle(support::SETUP,ViewEvent::Message(request.to_string()),&host.invocation()).unwrap();
        serde_json::from_str::<serde_json::Value>(&host.view_messages().last().unwrap().1).unwrap()
    };
    let initial=request(&mut plugin,json!({"op":"render"}));
    assert_eq!(initial["model"]["body"]["kind"],"form");
    assert_eq!(initial["model"]["iconButtons"].as_array().unwrap().len(),6);
    assert_eq!(initial["model"]["body"]["fields"].as_array().unwrap().len(),12);
    assert_eq!(initial["model"]["body"]["fields"][0]["value"]["kind"],"readonly-text");
    assert!(host.process_runs().is_empty());
    let project=host.invocation().project().unwrap();assert_eq!(project.read_file(".openidea/settings.toml").unwrap(),saved);
    let forged=request(&mut plugin,json!({"op":"event","event":{"kind":"field","key":"project","value":"/other"}}));
    assert!(forged.get("error").is_some());
    let edited=request(&mut plugin,json!({"op":"event","event":{"kind":"field","key":"phpVersion","value":"8.3"}}));
    assert!(edited.get("error").is_none());assert_eq!(project.read_file(".openidea/settings.toml").unwrap(),saved);
    let reset=request(&mut plugin,json!({"op":"event","event":{"kind":"button","key":"reset-settings"}}));
    assert!(reset["model"]["body"]["fields"].as_array().unwrap().iter().any(|field|field["key"]=="phpVersion"&&field["value"]["text"]=="8.1"));
    request(&mut plugin,json!({"op":"event","event":{"kind":"field","key":"phpVersion","value":"8.3"}}));
    let saved_reply=request(&mut plugin,json!({"op":"event","event":{"kind":"button","key":"save-settings"}}));
    assert!(saved_reply.get("error").is_none(),"{saved_reply}");
    let text=project.read_file(".openidea/settings.toml").unwrap();assert!(text.contains("Keep this")&&text.contains("fontSize=15"));
    assert_eq!(config_php_version(&project).unwrap().as_deref(),Some("8.3"));
    let before=host.process_runs().len();host.set_project_root(Some("/work/other"));
    let stale=request(&mut plugin,json!({"op":"event","event":{"kind":"button","key":"save-settings"}}));
    assert!(stale.get("error").is_some());assert_eq!(host.process_runs().len(),before);
    host.set_project_root(None);
    let empty=request(&mut plugin,json!({"op":"render","project":null}));
    assert_eq!(empty["model"]["body"]["kind"],"message");assert!(empty["model"]["toolbar"].as_array().unwrap().is_empty());
}

#[test]
fn run_target_validation_refuses_stopped_project_and_allows_running() {
    let launch = openidea_plugin::RunTargetLaunch {
        operation: openidea_plugin::RunTargetOperation::Run,
        program: "php".into(),
        cwd: "/work/my project".into(),
    };
    let host = mock();
    let mut plugin = Main::default();
    assert!(host.validate_run_target(&mut plugin, "ddev", &launch).is_ok());
    host.on_process("ddev", |_, _| output(STOPPED));
    let error = host
        .validate_run_target(&mut plugin, "ddev", &launch)
        .unwrap_err();
    assert!(error.contains("Run DDEV: Start"), "{error}");
}

const ROOT: &str = "/work/my project";

fn status_text(plugin: &mut Main, host: &MockHost) -> String {
    plugin.render(VIEW, None, &host.invocation()).unwrap().status.unwrap_or_default()
}

fn launch() -> openidea_plugin::RunTargetLaunch {
    openidea_plugin::RunTargetLaunch {
        operation: openidea_plugin::RunTargetOperation::Run,
        program: "php".into(),
        cwd: ROOT.into(),
    }
}

#[test]
fn second_render_runs_no_describe() {
    let host = mock();
    let mut plugin = Main::default();
    plugin.render(VIEW, None, &host.invocation()).unwrap();
    let runs = host.process_runs().len();
    plugin.refreshed_at.insert(ROOT.into(), 0);
    let status = status_text(&mut plugin, &host);
    plugin.render(VIEW, None, &host.invocation()).unwrap();
    plugin
        .detail(VIEW, PROJECT, &host.invocation())
        .unwrap();
    assert_eq!(host.process_runs().len(), runs);
    assert!(status.contains("refreshing"), "{status}");
}

#[test]
fn two_roots_cached_separately() {
    let host = mock();
    let mut plugin = Main::default();
    plugin.render(VIEW, None, &host.invocation()).unwrap();
    host.set_project_root(Some("/work/other"));
    plugin.render(VIEW, None, &host.invocation()).unwrap();
    let runs = host.process_runs().len();
    assert_eq!(runs, 2);
    host.set_project_root(Some(ROOT));
    plugin.render(VIEW, None, &host.invocation()).unwrap();
    host.set_project_root(Some("/work/other"));
    plugin.render(VIEW, None, &host.invocation()).unwrap();
    assert_eq!(host.process_runs().len(), runs);
    assert_eq!(plugin.snapshots.len(), 2);
}

#[test]
fn run_validation_uses_fresh_snapshot() {
    use openidea_plugin::RunTargetValidation;
    let host = mock();
    let mut plugin = Main::default();
    plugin.render(VIEW, None, &host.invocation()).unwrap();
    let runs = host.process_runs().len();
    plugin.validate(ROOT, "ddev", &launch()).unwrap();
    assert_eq!(host.process_runs().len(), runs);
    plugin.refreshed_at.insert(ROOT.into(), 0);
    plugin.validate(ROOT, "ddev", &launch()).unwrap();
    assert_eq!(host.process_runs().len(), runs + 1);
}

#[test]
fn activate_db_row_no_error() {
    let host = mock();
    let mut plugin = Main::default();
    plugin.render(VIEW, None, &host.invocation()).unwrap();
    plugin
        .handle(VIEW, ViewEvent::Activate("service:db".into()), &host.invocation())
        .unwrap();
    assert!(host.opened_urls().is_empty());
    plugin
        .handle(VIEW, ViewEvent::Activate("service:redis".into()), &host.invocation())
        .unwrap();
    assert_eq!(host.opened_urls(), ["http://localhost:9000"]);
}

#[test]
fn background_failure_with_snapshot_shows_error_not_refreshing() {
    let host = mock();
    let mut plugin = Main::default();
    plugin.render(VIEW, None, &host.invocation()).unwrap();
    host.on_process("ddev", |_, _| Err(Error::from("docker down")));
    plugin.refresh_background(&host.invocation().project().unwrap());
    plugin.refreshed_at.insert(ROOT.into(), 0);
    let status = status_text(&mut plugin, &host);
    assert!(status.contains("DDEV not reachable"), "{status}");
    assert!(!status.contains("refreshing"), "{status}");
}

#[test]
fn delete_question_names_the_project_and_its_path() {
    assert_eq!(
        delete_question(Some("demo"), "/work/demo"),
        "Delete DDEV project demo (/work/demo)? DDEV will delete its containers and database."
    );
    assert_eq!(
        delete_question(None, "/work/demo"),
        "Delete DDEV project /work/demo? DDEV will delete its containers and database."
    );
}
