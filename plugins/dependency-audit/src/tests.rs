use super::*;
use crate::manifest::{Dep, Ecosystem};
use crate::osv::tests::{JACKSON, dep};
use openidea_plugin::mock::MockHost;
use openidea_plugin::views::{Body, TreeItem, ViewEvent};
use openidea_plugin::{Event, HttpRequest, HttpResponse, Position, Range, Severity};
use serde_json::{Value, json};

pub const CAPS: &[&str] = &["project.read", "storage", "events", "net:api.osv.dev"];

pub const LODASH: &str = r#"{"id": "GHSA-35jh-r3h4-6jhm", "modified": "2024-01-01T00:00:00Z", "summary": "Command injection in lodash",
 "database_specific": {"severity": "HIGH"},
 "affected": [{"package": {"ecosystem": "npm", "name": "lodash"}, "ranges": [{"type": "SEMVER", "events": [{"introduced": "0"}, {"fixed": "4.17.21"}]}]}]}"#;

const VULNERABLE: &[(&str, &str, &str, &str)] = &[
    ("Maven", "com.fasterxml.jackson.core:jackson-databind", "2.12.3", "GHSA-57j2-w4cx-62h2"),
    ("npm", "lodash", "4.17.15", "GHSA-35jh-r3h4-6jhm"),
];

fn ok(body: String) -> Result<HttpResponse, Error> {
    Ok(HttpResponse { status: 200, headers: vec![], body: body.into_bytes() })
}

pub fn osv(req: &HttpRequest) -> Result<HttpResponse, Error> {
    match (req.method.as_str(), req.url.as_str()) {
        ("POST", "https://api.osv.dev/v1/querybatch") => {
            let body: Value = serde_json::from_slice(req.body.as_deref().unwrap()).unwrap();
            let results: Vec<Value> = body["queries"].as_array().unwrap().iter().map(|q| {
                let hit = VULNERABLE.iter().find(|(e, n, v, _)| q["package"]["ecosystem"] == *e && q["package"]["name"] == *n && q["version"] == *v);
                match hit {
                    Some((.., id)) => json!({"vulns": [{"id": id, "modified": if id.contains("57j2") { "2024-03-15T05:00:00Z" } else { "2024-01-01T00:00:00Z" }}]}),
                    None => json!({}),
                }
            }).collect();
            ok(json!({"results": results}).to_string())
        }
        ("GET", "https://api.osv.dev/v1/vulns/GHSA-57j2-w4cx-62h2") => ok(JACKSON.into()),
        ("GET", "https://api.osv.dev/v1/vulns/GHSA-35jh-r3h4-6jhm") => ok(LODASH.into()),
        _ => Ok(HttpResponse { status: 404, headers: vec![], body: vec![] }),
    }
}

pub fn posts(host: &MockHost) -> usize {
    host.requests().iter().filter(|r| r.method == "POST").count()
}

fn soon() -> std::time::Instant {
    std::time::Instant::now() + std::time::Duration::from_secs(45)
}

fn jackson() -> Dep {
    dep(Ecosystem::Maven, "com.fasterxml.jackson.core:jackson-databind", "2.12.3")
}

#[test]
fn audit_queries_osv_and_caches_results() {
    let host = MockHost::install().with_capabilities(CAPS);
    host.on_fetch(osv);
    let deps = [jackson(), dep(Ecosystem::Maven, "org.yaml:snakeyaml", "2.2")];

    let findings = osv::audit(&deps, true, 1_000, soon()).unwrap();
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].dep.name, "com.fasterxml.jackson.core:jackson-databind");
    assert_eq!(findings[0].fix().as_deref(), Some("2.12.7.1"));
    assert_eq!(host.requests().len(), 2, "one batch, one advisory");
    assert_eq!(host.requests()[0].headers, [("Content-Type".to_string(), "application/json".to_string())]);

    assert_eq!(osv::audit(&deps, true, 2_000, soon()).unwrap(), findings);
    assert_eq!(host.requests().len(), 2, "answered from the cache");

    osv::audit(&deps, true, 1_000 + osv::QUERY_TTL_SECS + 1, soon()).unwrap();
    assert_eq!(host.requests().len(), 3, "the query expired; the advisory is unchanged");
}

#[test]
fn offline_audit_uses_only_the_cache() {
    let host = MockHost::install().with_capabilities(CAPS);

    assert_eq!(osv::audit(&[jackson()], false, 0, soon()).unwrap(), vec![]);
    assert!(host.requests().is_empty());
}

#[test]
fn audit_sends_at_most_1000_queries_per_request() {
    let host = MockHost::install().with_capabilities(CAPS);
    host.on_fetch(osv);
    let deps: Vec<Dep> = (0..1001).map(|i| dep(Ecosystem::Npm, &format!("pkg{i}"), "1.0.0")).collect();

    osv::audit(&deps, true, 0, soon()).unwrap();
    assert_eq!(posts(&host), 2);
}

#[test]
fn vulnerability_lookup_denied_names_permission() {
    let _host = MockHost::install().with_capabilities(&["project.read", "storage", "events"]);

    let error = osv::audit(&[jackson()], true, 0, soon()).unwrap_err();

    assert!(error.to_string().contains("needs the net:api.osv.dev permission"), "{error}");
    assert!(error.to_string().contains("Settings › Plugins › Dependency Audit"), "{error}");
}

#[test]
fn audit_reports_osv_errors() {
    let host = MockHost::install().with_capabilities(CAPS);
    host.on_fetch(|_| Ok(HttpResponse { status: 503, headers: vec![], body: vec![] }));

    let error = osv::audit(&[jackson()], true, 0, soon()).unwrap_err();
    assert!(error.to_string().contains("503"), "{error}");
}

#[test]
fn a_failed_advisory_still_reports_its_id() {
    let host = MockHost::install().with_capabilities(CAPS);
    host.on_fetch(|r| if r.method == "GET" { Err(Error::Failed("timed out".into())) } else { osv(r) });

    let findings = osv::audit(&[jackson()], true, 0, soon()).unwrap();
    assert_eq!(findings[0].advisories[0].id, "GHSA-57j2-w4cx-62h2");
    assert_eq!(findings[0].advisories[0].severity, None);
    assert!(!host.storage().contains_key("adv:GHSA-57j2-w4cx-62h2"), "a failed advisory isn't cached");
}

#[test]
fn a_large_audit_stores_all_queries_under_one_key() {
    let host = MockHost::install().with_capabilities(CAPS);
    host.on_fetch(osv);
    let deps: Vec<Dep> = (0..1500).map(|i| dep(Ecosystem::Npm, &format!("pkg{i}"), "1.0.0")).collect();

    osv::audit(&deps, true, 0, soon()).unwrap();

    let storage = host.storage();
    let queries: Value = serde_json::from_str(&storage["queries"]).unwrap();
    assert_eq!(queries.as_object().unwrap().len(), 1500);
    assert!(storage.keys().all(|k| !k.starts_with("q:")));
}

#[test]
fn a_hostile_advisory_id_is_never_fetched_or_stored() {
    let host = MockHost::install().with_capabilities(CAPS);
    host.on_fetch(|r| {
        if r.method == "POST" {
            return ok(json!({"results": [{"vulns": [{"id": "../querybatch", "modified": "x"}]}]}).to_string());
        }
        osv(r)
    });

    let findings = osv::audit(&[jackson()], true, 0, soon()).unwrap();
    assert_eq!(findings[0].advisories[0].id, "../querybatch");
    assert!(host.requests().iter().all(|r| r.method == "POST"));
    assert!(host.storage().keys().all(|k| !k.starts_with("adv:")));
}

#[test]
fn audit_past_its_deadline_posts_nothing() {
    let host = MockHost::install().with_capabilities(CAPS);
    host.on_fetch(osv);

    let error = osv::audit(&[jackson()], true, 0, std::time::Instant::now()).unwrap_err();

    assert!(error.to_string().contains("timed out"), "{error}");
    assert!(host.requests().is_empty());
}

#[test]
fn after_a_failed_advisory_fetch_the_rest_are_not_attempted() {
    let host = MockHost::install().with_capabilities(CAPS);
    host.on_fetch(|r| match r.method.as_str() {
        "POST" => ok(json!({"results": [
            {"vulns": [{"id": "GHSA-aaaa-1", "modified": "m"}]},
            {"vulns": [{"id": "GHSA-aaaa-2", "modified": "m"}]},
        ]})
        .to_string()),
        _ => Err(Error::Failed("timed out".into())),
    });
    let deps = [jackson(), dep(Ecosystem::Maven, "com.example:other", "1.0")];

    let findings = osv::audit(&deps, true, 0, soon()).unwrap();

    assert_eq!(host.requests().iter().filter(|r| r.method == "GET").count(), 1);
    assert_eq!(findings.len(), 2);
    assert_eq!(findings[1].advisories[0].id, "GHSA-aaaa-2");
    assert!(findings[1].advisories[0].summary.is_empty());
}

pub const POM: &str = "<project>\n  <dependencies>\n    <dependency><groupId>org.yaml</groupId><artifactId>snakeyaml</artifactId><version>2.2</version></dependency>\n    <dependency><!-- 😀 --><groupId>com.fasterxml.jackson.core</groupId><artifactId>jackson-databind</artifactId><version>2.12.3</version></dependency>\n  </dependencies>\n</project>\n";
pub const PACKAGE: &str = "{\n  \"dependencies\": {\n    \"lodash\": \"^4.17.15\"\n  }\n}\n";
pub const PACKAGE_LOCK: &str = r#"{"lockfileVersion": 3, "packages": {"": {}, "node_modules/lodash": {"version": "4.17.15"}}}"#;

fn opened(host: &MockHost, path: &str) {
    DependencyAudit::default().handle_events(&[Event::FileOpened(path.into())], &host.invocation());
}

fn cached_problems(path: &str, text: &str) -> Vec<Diagnostic> {
    DependencyAudit::default().diagnostics(&document(path, "xml", text))
}

#[test]
fn opening_a_pom_publishes_a_diagnostic_on_the_dependency() {
    let host = MockHost::install().with_capabilities(CAPS);
    host.on_fetch(osv);
    host.add_file("pom.xml", POM);

    opened(&host, "/project/pom.xml");

    let problems = cached_problems("/project/pom.xml", POM);
    assert_eq!(problems.len(), 1);
    assert_eq!(problems[0].severity, Severity::Error);
    assert_eq!(problems[0].code.as_deref(), Some("GHSA-57j2-w4cx-62h2"));
    assert_eq!(problems[0].source.as_deref(), Some("OSV"));
    assert!(problems[0].message.contains("Upgrade to 2.12.7.1."), "{}", problems[0].message);
}

#[test]
fn diagnostic_range_counts_utf16_columns() {
    let host = MockHost::install().with_capabilities(CAPS);
    host.on_fetch(osv);
    host.add_file("pom.xml", POM);

    opened(&host, "/project/pom.xml");

    let range = cached_problems("/project/pom.xml", POM)[0].range;
    assert_eq!((range.start.line, range.start.character, range.end.character), (3, 84, 100));
}

#[test]
fn package_json_uses_the_sibling_lockfile() {
    let host = MockHost::install().with_capabilities(CAPS);
    host.on_fetch(osv);
    host.add_file("web/package.json", PACKAGE);
    host.add_file("web/package-lock.json", PACKAGE_LOCK);

    let mut plugin = DependencyAudit::default();
    plugin.handle_events(&[Event::FileOpened("/project/web/package.json".into())], &host.invocation());
    assert_eq!(plugin.diagnostics(&document("/project/web/package.json", "json", PACKAGE))[0].range.start.line, 2);
}

#[test]
fn auto_check_off_sends_nothing() {
    let host = MockHost::install().with_capabilities(CAPS);
    host.on_fetch(osv);
    host.set_setting("autoCheck", "false");
    host.add_file("pom.xml", POM);

    opened(&host, "/project/pom.xml");

    assert!(host.requests().is_empty());
    assert!(host.diagnostics().is_empty());
}

#[test]
fn other_files_are_ignored() {
    let host = MockHost::install().with_capabilities(CAPS);
    host.on_fetch(osv);

    DependencyAudit::default().handle_events(&[Event::FileSaved("/project/src/App.java".into())], &host.invocation());

    assert!(host.requests().is_empty());
}

#[test]
fn an_osv_outage_logs_and_keeps_quiet() {
    let host = MockHost::install().with_capabilities(CAPS);
    host.on_fetch(|_| Err(Error::Failed("offline".into())));
    host.add_file("pom.xml", POM);

    opened(&host, "/project/pom.xml");

    assert!(host.diagnostics().is_empty());
    assert!(host.notifications().is_empty());
    assert!(host.logs().iter().any(|(_, line)| line.contains("OSV check of /project/pom.xml: offline")));
}

#[test]
fn an_event_batch_stops_at_the_first_osv_failure() {
    let host = MockHost::install().with_capabilities(CAPS);
    host.on_fetch(|_| Err(Error::Failed("hung".into())));
    let events: Vec<Event> = (0..7)
        .map(|i| {
            host.add_file(&format!("m{i}/pom.xml"), POM);
            Event::FileOpened(format!("/project/m{i}/pom.xml"))
        })
        .collect();

    DependencyAudit::default().handle_events(&events, &host.invocation());

    assert_eq!(posts(&host), 1);
    assert_eq!(host.logs().iter().filter(|(_, line)| line.contains("OSV check of")).count(), 1);
}

fn document(path: &str, language: &str, text: &str) -> openidea_plugin::providers::Document {
    openidea_plugin::providers::Document { path: path.into(), language: language.into(), version: 1, text: text.into() }
}

fn line(n: u32) -> Range {
    Range::new(Position::new(n, 0), Position::new(n, 0))
}

fn reviewed_upgrades(host:&MockHost,doc:&Document,selection:Range)->Vec<CodeAction> {
    let mut plugin=DependencyAudit::default();
    plugin.code_actions(doc,selection).into_iter().filter(|a|a.title.starts_with("Upgrade ")).map(|action| {
        assert!(action.edits.is_empty()&&action.data.is_some(),"listing must not return unreviewed edits");
        host.answer_next_prompt(Some(&[]));
        plugin.resolve_code_action(doc,action).unwrap()
    }).collect()
}

#[test]
fn quick_fix_upgrades_to_the_nearest_fixed_version() {
    let host = MockHost::install().with_capabilities(CAPS);
    host.on_fetch(osv);
    host.add_file("pom.xml", POM);
    opened(&host, "/project/pom.xml");
    let sent = host.requests().len();

    let actions = reviewed_upgrades(&host,&document("/project/pom.xml", "xml", POM), line(3));

    assert_eq!(actions.len(), 1);
    assert_eq!(actions[0].title, "Upgrade jackson-databind to 2.12.7.1");
    assert_eq!(actions[0].kind.as_deref(), Some("quickfix"));
    let (path, edits) = &actions[0].edits[0];
    assert_eq!(path, "/project/pom.xml");
    assert_eq!(edits[0].text, "2.12.7.1");
    assert_eq!(edits[0].range.start, Position::new(3, 122));
    assert_eq!(host.requests().len(), sent, "no network on the edit path");
}

#[test]
fn quick_fix_keeps_the_npm_range_operator() {
    let host = MockHost::install().with_capabilities(CAPS);
    host.on_fetch(osv);
    host.add_file("package.json", PACKAGE);
    host.add_file("package-lock.json", PACKAGE_LOCK);
    opened(&host, "/project/package.json");

    let actions = reviewed_upgrades(&host,&document("/project/package.json", "json", PACKAGE), line(2));

    assert_eq!(actions[0].title, "Upgrade lodash to 4.17.21");
    assert_eq!(actions[0].edits[0].1[0].text, "^4.17.21");
}

#[test]
fn no_quick_fix_for_a_compound_range() {
    let host = MockHost::install().with_capabilities(CAPS);
    host.on_fetch(osv);
    let package = PACKAGE.replace("^4.17.15", ">=4.0.0 <5");
    host.add_file("package.json", &package);
    host.add_file("package-lock.json", PACKAGE_LOCK);
    opened(&host, "/project/package.json");

    let actions = reviewed_upgrades(&host,&document("/project/package.json", "json", &package), line(2));

    assert!(actions.is_empty());
}

#[test]
fn no_quick_fix_without_a_cached_result() {
    let host = MockHost::install().with_capabilities(CAPS);
    host.on_fetch(osv);

    let actions = reviewed_upgrades(&host,&document("/project/pom.xml", "xml", POM), line(3));

    assert!(actions.is_empty());
    assert!(host.requests().is_empty());
}

#[test]
fn no_quick_fix_off_the_dependency_line_or_in_lockfiles() {
    let host = MockHost::install().with_capabilities(CAPS);
    host.on_fetch(osv);
    host.add_file("pom.xml", POM);
    host.add_file("package-lock.json", PACKAGE_LOCK);
    opened(&host, "/project/pom.xml");
    opened(&host, "/project/package-lock.json");
    let mut plugin = DependencyAudit::default();

    assert!(plugin.code_actions(&document("/project/pom.xml", "xml", POM), line(2)).is_empty());
    assert!(plugin.code_actions(&document("/project/package-lock.json", "json", PACKAGE_LOCK), line(0)).is_empty());
}

#[test]
fn shared_property_quick_fix_refused() {
    let other = r#"{"id": "GHSA-aaaa-bbbb-cccc", "modified": "2024-01-01T00:00:00Z", "summary": "Other",
 "affected": [{"package": {"ecosystem": "Maven", "name": "com.example:other"}, "ranges": [{"type": "ECOSYSTEM", "events": [{"introduced": "0"}, {"fixed": "2.13.0"}]}]}]}"#;
    let pom = "<project>\n<properties><jackson.version>2.12.3</jackson.version></properties>\n<dependencies>\n<dependency><groupId>com.fasterxml.jackson.core</groupId><artifactId>jackson-databind</artifactId><version>${jackson.version}</version></dependency>\n<dependency><groupId>com.example</groupId><artifactId>other</artifactId><version>${jackson.version}</version></dependency>\n</dependencies>\n</project>\n";
    let host = MockHost::install().with_capabilities(CAPS);
    host.on_fetch(move |req| match (req.method.as_str(), req.url.as_str()) {
        ("POST", _) => ok(json!({"results": [
            {"vulns": [{"id": "GHSA-57j2-w4cx-62h2", "modified": "2024-03-15T05:00:00Z"}]},
            {"vulns": [{"id": "GHSA-aaaa-bbbb-cccc", "modified": "2024-01-01T00:00:00Z"}]},
        ]})
        .to_string()),
        ("GET", "https://api.osv.dev/v1/vulns/GHSA-57j2-w4cx-62h2") => ok(JACKSON.into()),
        _ => ok(other.into()),
    });
    host.add_file("pom.xml", pom);
    opened(&host, "/project/pom.xml");

    let selection = Range::new(Position::new(3, 0), Position::new(4, 0));
    let actions = reviewed_upgrades(&host,&document("/project/pom.xml", "xml", pom), selection);

    assert!(actions.is_empty(), "a property shared with another artifact must not be bumped");
}

#[test]
fn code_actions_in_a_large_lockfile_return_fast() {
    let host = MockHost::install().with_capabilities(CAPS);
    let packages: Vec<String> = (0..3000).map(|i| format!("    \"node_modules/pkg{i}\": {{\"version\": \"1.0.0\"}}")).collect();
    let lock = format!("{{\"lockfileVersion\": 3, \"packages\": {{\n{}\n}}}}", packages.join(",\n"));
    host.add_file("package-lock.json", &lock);
    let started = std::time::Instant::now();

    let actions = reviewed_upgrades(&host,&document("/project/package-lock.json", "json", &lock), line(1500));

    assert!(actions.is_empty());
    assert!(started.elapsed() < std::time::Duration::from_millis(100), "{:?}", started.elapsed());
}

fn scanned(host: &MockHost) -> DependencyAudit {
    host.on_fetch(osv);
    host.add_file("pom.xml", POM);
    host.add_file("web/package.json", PACKAGE);
    host.add_file("web/package-lock.json", PACKAGE_LOCK);
    let mut plugin = DependencyAudit::default();
    plugin.run_command("scan", &host.invocation(), None).unwrap();
    plugin
}

fn tree(plugin: &mut DependencyAudit, host: &MockHost) -> Vec<TreeItem> {
    match plugin.render("vulnerabilities", None, &host.invocation()).unwrap().body {
        Body::Tree(items) => items,
        other => panic!("a tree, got {other:?}"),
    }
}

#[test]
fn scan_lists_vulnerable_dependencies_in_the_view() {
    let host = MockHost::install().with_capabilities(CAPS);
    let mut plugin = scanned(&host);

    assert_eq!(host.project_notifications()[0].1.notification.body, "Vulnerable dependencies: 2 of 3 (3 files). See the Dependency Audit window.");
    assert!(host.refreshed_views().contains(&"vulnerabilities".to_string()));
    assert!(host.diagnostics().is_empty(), "scan does not publish a duplicate producer");
    assert_eq!(plugin.diagnostics(&document("/project/pom.xml", "xml", POM)).len(), 1);
    assert_eq!(plugin.diagnostics(&document("/project/web/package-lock.json", "json", PACKAGE_LOCK)).len(), 1);

    let labels: Vec<String> = tree(&mut plugin, &host).into_iter().map(|i| i.label).collect();
    assert_eq!(labels.iter().filter(|l| l.starts_with("lodash")).count(), 1, "the lockfile adds no duplicate");
    assert!(labels.contains(&"com.fasterxml.jackson.core:jackson-databind 2.12.3".to_string()));
    assert!(labels.contains(&"GHSA-35jh-r3h4-6jhm".to_string()));
}

#[test]
fn scan_does_not_resolve_builds_by_default() {
    let host = MockHost::install().with_capabilities(&["project.read", "storage", "events", "net:api.osv.dev", "process:mvn", "editor.read"]);

    scanned(&host);

    assert!(host.process_runs().is_empty(), "{:?}", host.process_runs());
}

#[test]
fn the_view_survives_a_new_instance() {
    let host = MockHost::install().with_capabilities(CAPS);
    let mut first = scanned(&host);
    let before = tree(&mut first, &host);

    assert_eq!(tree(&mut DependencyAudit::default(), &host), before);
}

#[test]
fn activating_items_opens_the_line_or_osv_dev() {
    let host = MockHost::install().with_capabilities(CAPS);
    let mut plugin = scanned(&host);
    let items = tree(&mut plugin, &host);
    let key = |label: &str| items.iter().find(|i| i.label == label).unwrap().key.clone();

    plugin.handle("vulnerabilities", ViewEvent::Activate(key("GHSA-57j2-w4cx-62h2")), &host.invocation()).unwrap();
    plugin
        .handle("vulnerabilities", ViewEvent::Activate(key("com.fasterxml.jackson.core:jackson-databind 2.12.3")), &host.invocation())
        .unwrap();

    assert_eq!(host.opened_urls(), ["https://osv.dev/vulnerability/GHSA-57j2-w4cx-62h2"]);
    assert_eq!(host.opened_locations(), [("/project/pom.xml".to_string(), 3, 0)]);
}

#[test]
fn before_a_scan_the_view_offers_to_scan() {
    let host = MockHost::install().with_capabilities(CAPS);

    let view = DependencyAudit::default().render("vulnerabilities", None, &host.invocation()).unwrap();

    assert!(matches!(view.body, Body::Message(_)));
    assert_eq!(view.toolbar[0].id, "scan");
}

#[test]
fn a_failed_scan_tells_the_user() {
    let host = MockHost::install().with_capabilities(CAPS);
    host.on_fetch(|_| Ok(HttpResponse { status: 503, headers: vec![], body: vec![] }));
    host.add_file("pom.xml", POM);

    let error = DependencyAudit::default().run_command("scan", &host.invocation(), None).unwrap_err();

    assert!(error.to_string().contains("OSV answered 503"));
}

#[test]
fn a_fresh_instance_opens_items_from_the_stored_rows() {
    let host = MockHost::install().with_capabilities(CAPS);
    let mut first = scanned(&host);
    let items = tree(&mut first, &host);
    let key = items.iter().find(|i| i.label == "GHSA-57j2-w4cx-62h2").unwrap().key.clone();

    DependencyAudit::default().handle("vulnerabilities", ViewEvent::Activate(key), &host.invocation()).unwrap();

    assert_eq!(host.opened_urls(), ["https://osv.dev/vulnerability/GHSA-57j2-w4cx-62h2"]);
}

#[test]
fn rendering_again_reuses_the_advisory_details_it_already_read() {
    let host = MockHost::install().with_capabilities(CAPS);
    let mut plugin = scanned(&host);
    let detail = |items: Vec<TreeItem>| items.into_iter().find(|i| i.label == "GHSA-35jh-r3h4-6jhm").unwrap().detail;
    let first = detail(tree(&mut plugin, &host));
    for stale in host::storage::keys("adv:").unwrap() {
        host::storage::delete(&stale).unwrap();
    }

    assert_eq!(first.as_deref(), Some("Command injection in lodash"));
    assert_eq!(detail(tree(&mut plugin, &host)), first);
}

#[test]
fn a_dependency_shared_by_two_manifests_is_marked_in_both_and_listed_once() {
    let host = MockHost::install().with_capabilities(CAPS);
    host.on_fetch(osv);
    host.add_file("a/pom.xml", POM);
    host.add_file("b/pom.xml", POM);
    let mut plugin = DependencyAudit::default();
    plugin.run_command("scan", &host.invocation(), None).unwrap();

    assert_eq!(plugin.diagnostics(&document("/project/a/pom.xml", "xml", POM)).len(), 1);
    assert_eq!(plugin.diagnostics(&document("/project/b/pom.xml", "xml", POM)).len(), 1);
    assert!(host.diagnostics().is_empty());
    let jackson = tree(&mut plugin, &host).into_iter().filter(|i| i.label.starts_with("com.fasterxml")).count();
    assert_eq!(jackson, 1);
}

#[test]
fn ignored_advisory_is_not_reported() {
    let host = MockHost::install().with_capabilities(CAPS);
    host.set_setting("ignoredAdvisories", r##""# accepted\nGHSA-35jh-r3h4-6jhm  # lodash, no fix yet\n""##);
    let mut plugin = scanned(&host);

    assert_eq!(host.project_notifications()[0].1.notification.body, "Vulnerable dependencies: 1 of 3 (3 files). See the Dependency Audit window.");
    let labels: Vec<String> = tree(&mut plugin, &host).into_iter().map(|i| i.label).collect();
    assert!(!labels.iter().any(|l| l.contains("lodash") || l == "GHSA-35jh-r3h4-6jhm"));
    assert!(labels.contains(&"GHSA-57j2-w4cx-62h2".to_string()));
    assert!(host.diagnostics().get("/project/web/package.json").is_none_or(|d| d.is_empty()));
}

#[test]
fn ignoring_an_advisory_hides_it_from_an_earlier_scan() {
    let host = MockHost::install().with_capabilities(CAPS);
    let mut plugin = scanned(&host);
    host.set_setting("ignoredAdvisories", r#""GHSA-57j2-w4cx-62h2""#);

    let labels: Vec<String> = tree(&mut plugin, &host).into_iter().map(|i| i.label).collect();
    assert!(labels.iter().any(|l| l.starts_with("lodash")));
    assert!(!labels.iter().any(|l| l.contains("jackson")));
}

#[test]
fn ignored_cve_alias_is_not_reported() {
    let host = MockHost::install().with_capabilities(CAPS);
    let lodash = LODASH.replacen(r#""summary""#, r#""aliases": ["CVE-2019-10744"], "summary""#, 1);
    host.on_fetch(move |req| match req.url.as_str() {
        "https://api.osv.dev/v1/vulns/GHSA-35jh-r3h4-6jhm" => ok(lodash.clone()),
        _ => osv(req),
    });
    host.add_file("pom.xml", POM);
    host.add_file("web/package.json", PACKAGE);
    host.add_file("web/package-lock.json", PACKAGE_LOCK);
    let mut plugin = DependencyAudit::default();
    plugin.run_command("scan", &host.invocation(), None).unwrap();

    host.set_setting("ignoredAdvisories", r#""cve-2019-10744""#);
    let labels: Vec<String> = tree(&mut plugin, &host).into_iter().map(|i| i.label).collect();
    assert!(!labels.iter().any(|l| l.contains("lodash")));
    assert!(labels.iter().any(|l| l.contains("jackson")));

    host.set_setting("ignoredAdvisories", r#""ghsa-57j2-w4cx-62h2""#);
    let labels: Vec<String> = tree(&mut plugin, &host).into_iter().map(|i| i.label).collect();
    assert!(labels.iter().any(|l| l.starts_with("lodash")), "the alias is no longer ignored");
    assert!(!labels.iter().any(|l| l.contains("jackson")), "lowercase ids match");
}

fn cve_lodash_osv(req: &HttpRequest) -> Result<HttpResponse, Error> {
    match req.url.as_str() {
        "https://api.osv.dev/v1/vulns/GHSA-35jh-r3h4-6jhm" => {
            ok(LODASH.replacen(r#""summary""#, r#""aliases": ["CVE-2019-10744"], "summary""#, 1))
        }
        _ => osv(req),
    }
}

fn lodash_audit(now: u64) -> Vec<crate::osv::Finding> {
    osv::audit(&[dep(Ecosystem::Npm, "lodash", "4.17.15")], true, now, soon()).unwrap()
}

#[test]
fn alias_matching_works_inside_audit() {
    let host = MockHost::install().with_capabilities(CAPS);
    host.on_fetch(cve_lodash_osv);
    host.set_setting("ignoredAdvisories", r#""CVE-2019-10744""#);

    assert!(lodash_audit(0).is_empty());
}

#[test]
fn old_cache_entry_without_aliases_is_refetched() {
    let host = MockHost::install().with_capabilities(CAPS);
    host.on_fetch(cve_lodash_osv);
    host.set_setting("ignoredAdvisories", r#""CVE-2019-10744""#);
    host::storage::set(
        "v:GHSA-35jh-r3h4-6jhm",
        r#"{"id":"GHSA-35jh-r3h4-6jhm","modified":"2024-01-01T00:00:00Z","summary":"old","severity":"HIGH","fixed":{}}"#,
    )
    .unwrap();
    let queries = json!({"npm:lodash@4.17.15": {"at": 0, "vulns": [["GHSA-35jh-r3h4-6jhm", "2024-01-01T00:00:00Z"]]}});
    host::storage::set("queries", &queries.to_string()).unwrap();

    assert!(lodash_audit(1).is_empty());
    assert!(host.requests().iter().any(|r| r.url.ends_with("/GHSA-35jh-r3h4-6jhm")), "fetched again");
    assert!(host.storage().keys().all(|k| !k.starts_with("v:")), "the old key is dropped");
}

#[test]
fn pypi_names_are_normalised() {
    let host = MockHost::install().with_capabilities(CAPS);
    host.on_fetch(osv);
    let upper = crate::manifest::parse("/p/requirements.txt", "Django==3.2\nmy_pkg.Name==1.0\n", None);
    let lower = crate::manifest::parse("/p/other/requirements.txt", "django==3.2\n", None);
    let deps: Vec<Dep> = upper.into_iter().chain(lower).collect();

    osv::audit(&deps, true, 1_000, soon()).unwrap();

    let body: Value = serde_json::from_slice(host.requests()[0].body.as_deref().unwrap()).unwrap();
    let names: Vec<&str> = body["queries"].as_array().unwrap().iter().map(|q| q["package"]["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["django", "my-pkg-name"]);
}

const CATALOG: &str = r#"[versions]
jackson = "2.12.3"

[libraries]
databind = { module = "com.fasterxml.jackson.core:jackson-databind", version.ref = "jackson" }
core = "com.fasterxml.jackson.core:jackson-databind:2.12.3"
"#;

#[test]
fn quick_fix_upgrades_version_catalog_entries() {
    let host = MockHost::install().with_capabilities(CAPS);
    host.on_fetch(osv);
    host.add_file("gradle/libs.versions.toml", CATALOG);

    opened(&host, "/project/gradle/libs.versions.toml");
    let catalog = document("/project/gradle/libs.versions.toml", "toml", CATALOG);

    for (selected, edited) in [(4, 1), (5, 5)] {
        let actions = reviewed_upgrades(&host,&catalog, line(selected));

        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].title, "Upgrade jackson-databind to 2.12.7.1");
        assert_eq!(actions[0].edits[0].1[0].range.start.line, edited);
    }
}

const SHARED_CATALOG: &str = r#"[versions]
jackson = "2.12.3"

[libraries]
annotations = { module = "com.fasterxml.jackson.core:jackson-annotations", version.ref = "jackson" }
databind = { module = "com.fasterxml.jackson.core:jackson-databind", version.ref = "jackson" }
"#;

#[test]
fn shared_catalog_version_quick_fix_refused() {
    let host = MockHost::install().with_capabilities(CAPS);
    host.on_fetch(osv);
    host.add_file("gradle/libs.versions.toml", SHARED_CATALOG);

    opened(&host, "/project/gradle/libs.versions.toml");
    let catalog = document("/project/gradle/libs.versions.toml", "toml", SHARED_CATALOG);

    let actions = reviewed_upgrades(&host,&catalog, line(4));

    assert!(actions.is_empty());
}

#[test]
fn requirements_versions_are_replaceable() {
    let text = "flask==2.0.0 # web\n";
    let deps = crate::manifest::parse("/p/requirements.txt", text, None);

    let (from, to) = deps[0].version_at.unwrap();
    assert_eq!(&text[from..to], "2.0.0");
}

fn resource_model(plugin: &mut DependencyAudit, host: &MockHost) -> Value {
    plugin
        .handle(
            "vulnerabilities",
            ViewEvent::Message(json!({"channel":"ui-kit","sequence":41,"op":"render"}).to_string()),
            &host.invocation(),
        )
        .unwrap();
    let response: Value = serde_json::from_str(&host.view_messages().last().unwrap().1).unwrap();
    assert!(response.get("error").is_none(), "{response}");
    response["model"].clone()
}

fn detail_value<'a>(row: &'a Value, label: &str) -> &'a str {
    row["details"].as_array().unwrap().iter().find(|d| d["label"] == label).unwrap()["value"].as_str().unwrap()
}

#[test]
fn packaged_resources_expose_domain_columns_evidence_and_original_activate_keys() {
    let host = MockHost::install().with_capabilities(CAPS);
    let mut plugin = scanned(&host);
    let requests = host.requests().len();
    let model = resource_model(&mut plugin, &host);
    assert_eq!(model["body"]["kind"], "resources");
    assert_eq!(
        model["body"]["columns"],
        json!(["Type", "Dependency", "Ecosystem", "Installed", "Fixed", "Severity", "Advisory", "Manifest"])
    );
    assert_eq!(model["body"]["facets"], json!([0, 2, 5, 7]));
    let items = model["body"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 5, "two dependency rows, their two advisory rows and one clean checked coordinate");
    let dependency = items.iter().find(|r| r["cells"][0] == "Dependency" && r["cells"][1] == "lodash").unwrap();
    assert_eq!(dependency["cells"][2], "npm");
    assert_eq!(dependency["cells"][3], "4.17.15");
    assert_eq!(dependency["cells"][4], "4.17.21");
    assert_eq!(dependency["cells"][5], "HIGH");
    let evidence = detail_value(dependency, "Declarations / lockfile evidence");
    assert!(evidence.contains("web/package.json:3"), "{evidence}");
    assert!(evidence.contains("web/package-lock.json:1") && evidence.contains("Lockfile entry"), "{evidence}");
    assert!(detail_value(dependency, "Report scan").contains(" UTC)"));
    assert!(detail_value(dependency, "Report coverage").contains("3 dependency files; 3 unique"));
    assert!(detail_value(dependency, "Ignore scope").contains("0 configured"));
    plugin.handle("vulnerabilities", ViewEvent::Activate(dependency["key"].as_str().unwrap().into()), &host.invocation()).unwrap();
    assert!(host.opened_locations()[0].0.ends_with("web/package.json"));
    let advisory = items.iter().find(|r| r["cells"][0] == "Advisory" && r["cells"][1] == "lodash").unwrap();
    assert_eq!(detail_value(advisory, "Fixed versions reported for this package"), "4.17.21");
    assert_eq!(detail_value(advisory, "Advisory modified"), "2024-01-01T00:00:00Z");
    plugin.handle("vulnerabilities", ViewEvent::Activate(advisory["key"].as_str().unwrap().into()), &host.invocation()).unwrap();
    assert_eq!(host.opened_urls(), ["https://osv.dev/vulnerability/GHSA-35jh-r3h4-6jhm"]);
    assert_eq!(host.requests().len(), requests, "rendering and selection do not fetch OSV");
}

#[test]
fn a_failed_rescan_keeps_snapshot_facts_and_marks_the_report_stale() {
    let host = MockHost::install().with_capabilities(CAPS);
    let mut plugin = scanned(&host);
    let project = host.invocation().project().unwrap();
    let previous = host::storage::get(&rows_key(&project).unwrap()).unwrap().unwrap();
    host::storage::delete("queries").unwrap();
    for key in host::storage::keys("adv:").unwrap() {
        host::storage::delete(&key).unwrap();
    }
    host.on_fetch(|_| Err(Error::Failed("offline".into())));
    assert!(plugin.run_command("scan", &host.invocation(), None).is_err());
    assert_eq!(host::storage::get(&rows_key(&project).unwrap()).unwrap().unwrap(), previous);
    let model = resource_model(&mut DependencyAudit::default(), &host);
    assert!(model["status"].as_str().unwrap().contains("last rescan failed"));
    assert!(model["status"].as_str().unwrap().contains("showing previous report"));
    let row = model["body"]["items"].as_array().unwrap().iter().find(|r| r["cells"][0] == "Advisory" && r["cells"][1] == "lodash").unwrap();
    assert_eq!(row["cells"][4], "4.17.21", "advisory facts survive separate cache eviction");
    assert!(detail_value(row, "GHSA-35jh-r3h4-6jhm summary").contains("Command injection"));
    assert!(detail_value(row, "Last rescan failed").contains("offline"));
    host.on_fetch(osv);
    plugin.run_command("scan", &host.invocation(), None).unwrap();
    assert!(last_failure(&project).is_none());
}

#[test]
fn legacy_scan_rows_render_with_honest_unknown_provenance_and_details() {
    let host = MockHost::install().with_capabilities(CAPS);
    let project = host.invocation().project().unwrap();
    host::storage::set(
        &rows_key(&project).unwrap(),
        &json!([{"path":"/project/package.json","line":2,"name":"example","version":"1.0.0","fix":null,"advisories":["GHSA-example"]}])
            .to_string(),
    )
    .unwrap();
    let model = resource_model(&mut DependencyAudit::default(), &host);
    let row = &model["body"]["items"][1];
    assert_eq!(row["cells"][2], "npm");
    assert_eq!(row["cells"][4], "Unknown (details unavailable)");
    assert_eq!(row["cells"][5], "Unknown");
    assert_eq!(detail_value(row, "Report scan"), "Not recorded (older report)");
    assert!(detail_value(row, "GHSA-example summary").contains("Details unavailable"));
    assert!(host.requests().is_empty());
}

#[test]
fn changed_ignore_settings_apply_to_snapshots_even_without_advisory_cache() {
    let host = MockHost::install().with_capabilities(CAPS);
    host.on_fetch(cve_lodash_osv);
    host.add_file("web/package.json", PACKAGE);
    host.add_file("web/package-lock.json", PACKAGE_LOCK);
    let mut plugin = DependencyAudit::default();
    plugin.run_command("scan", &host.invocation(), None).unwrap();
    for key in host::storage::keys("adv:").unwrap() {
        host::storage::delete(&key).unwrap();
    }
    host.set_setting("ignoredAdvisories", r#""CVE-2019-10744""#);
    assert!(stored_rows(&host.invocation().project().unwrap()).unwrap().unwrap().is_empty());
    assert!(matches!(plugin.render("vulnerabilities", None, &host.invocation()).unwrap().body, Body::Message(_)));
}

#[test]
fn activation_cannot_reuse_a_previous_projects_cached_targets() {
    let host = MockHost::install().with_capabilities(CAPS);
    let mut plugin = scanned(&host);
    let old_key = tree(&mut plugin, &host).into_iter().find(|r| r.label == "GHSA-35jh-r3h4-6jhm").unwrap().key;
    host.set_project_root(Some("/other"));
    plugin.handle("vulnerabilities", ViewEvent::Activate(old_key), &host.invocation()).unwrap();
    assert!(host.opened_urls().is_empty() && host.opened_locations().is_empty());
}

#[test]
fn failure_to_store_new_results_does_not_delete_a_previous_report() {
    let host = MockHost::install().with_capabilities(CAPS);
    let mut plugin = scanned(&host);
    let project = host.invocation().project().unwrap();
    let key = rows_key(&project).unwrap();
    let before = host.storage()[&key].clone();
    let host = host.with_capabilities(&["project.read", "net:api.osv.dev"]);
    let body = scan(&host.invocation().project().unwrap(), &mut live::Live::default(), Instant::now() + COMMAND_BUDGET).unwrap();
    assert!(body.contains("previous report remains"));
    assert_eq!(host.storage()[&key], before);
    let host = host.with_capabilities(CAPS);
    let model = resource_model(&mut plugin, &host);
    assert_eq!(model["body"]["items"].as_array().unwrap().len(), 5);
}

#[test]
fn large_resource_reports_fit_the_bridge_and_keep_unshortened_action_keys() {
    let host = MockHost::install().with_capabilities(CAPS);
    let row = json!({"path":"/project/package.json","line":2,"name":"example","version":"1.0.0","fix":null,"advisories":["GHSA-example"],"details":[{"id":"GHSA-example","modified":"m","summary":"🦀".repeat(3000),"severity":null,"fixed":{}}]});
    let rows: Vec<_> = (0..5001)
        .map(|i| {
            let mut row = row.clone();
            row["name"] = json!(format!("example-{i}"));
            row
        })
        .collect();
    let report: Report = serde_json::from_value(json!({"rows":rows})).unwrap();
    let Body::Other { data, .. } = resource_body("/project", &report, None) else { panic!("resources") };
    let value: Value = serde_json::from_str(&data).unwrap();
    assert!(data.len() < (4 << 20) + 4096, "leave room for the 8-MiB host message envelope");
    assert_eq!(value["totalRows"], 10002);
    let rows = value["items"].as_array().unwrap();
    assert!(rows.len() < 10002);
    assert_eq!(rows[0]["key"], "dep:/project/package.json|example-0 1.0.0");
    assert!(detail_value(&rows[0], "GHSA-example summary").ends_with("… (truncated)"));
    assert!(host.requests().is_empty());
}

#[test]
fn live_diagnostics_follow_unsaved_versions_without_network_or_cache_writes() {
    let host = MockHost::install().with_capabilities(CAPS);
    host.on_fetch(osv);
    host.add_file("pom.xml", POM);
    let mut plugin = DependencyAudit::default();
    plugin.handle_events(&[Event::FileOpened("/project/pom.xml".into())], &host.invocation());
    let requests = host.requests().len();
    let stored = host.storage();
    let shifted = format!("\n{POM}");
    let problems = plugin.diagnostics(&document("/project/pom.xml", "xml", &shifted));
    assert_eq!(problems.len(), 1);
    assert_eq!(problems[0].range.start.line, 4);
    assert!(problems[0].message.contains("Cached OSV query: Unix"));
    assert!(plugin.diagnostics(&document("/project/pom.xml", "xml", &POM.replace("2.12.3", "2.12.7.1"))).is_empty());
    assert!(host.diagnostics().is_empty(), "no duplicate manual producer");
    assert_eq!(host.requests().len(), requests);
    assert_eq!(host.storage(), stored, "provider never migrates or mutates storage");
}

#[test]
fn live_diagnostics_are_quiet_for_unknown_versions_and_dated_for_stale_positive_evidence() {
    let host = MockHost::install().with_capabilities(CAPS);
    host.on_fetch(osv);
    osv::audit(&[jackson()], true, 1_000, soon()).unwrap();
    host::storage::set("v:legacy", "untouched").unwrap();
    let sent = host.requests().len();
    let mut plugin = DependencyAudit::default();
    let problems = plugin.diagnostics(&document("/project/pom.xml", "xml", POM));
    assert_eq!(problems.len(), 1);
    assert!(problems[0].message.contains("Unix 1000; stale, rescan recommended"));
    assert!(plugin.diagnostics(&document("/project/pom.xml", "xml", &POM.replace("2.12.3", "2.13.0"))).is_empty());
    assert_eq!(host.storage().get("v:legacy").map(String::as_str), Some("untouched"));
    assert_eq!(host.requests().len(), sent);
    host.set_setting("ignoredAdvisories", r#""GHSA-57j2-w4cx-62h2""#);
    assert!(plugin.diagnostics(&document("/project/pom.xml", "xml", POM)).is_empty());
}

#[test]
fn lock_dependent_live_facts_are_invalidated_by_manifest_edits_and_watched_changes() {
    let host = MockHost::install().with_capabilities(CAPS);
    host.on_fetch(osv);
    host.add_file("package.json", PACKAGE);
    host.add_file("package-lock.json", PACKAGE_LOCK);
    let mut plugin = DependencyAudit::default();
    plugin.handle_events(&[Event::FileOpened("/project/package.json".into())], &host.invocation());
    assert_eq!(plugin.diagnostics(&document("/project/package.json", "json", PACKAGE)).len(), 1);
    let changed = PACKAGE.replace("^4.17.15", "^4.17.21");
    assert!(plugin.diagnostics(&document("/project/package.json", "json", &changed)).is_empty());
    plugin.handle_events(&[Event::FilesChanged(openidea_plugin::FilesChanged { modified: vec!["/project/package-lock.json".into()], ..Default::default() })], &host.invocation());
    assert!(plugin.diagnostics(&document("/project/package.json", "json", PACKAGE)).is_empty());
    assert_eq!(plugin.diagnostics(&document("/project/package-lock.json", "json", PACKAGE_LOCK)).len(), 1);
}

#[test]
fn unavailable_identity_oversized_documents_and_invalid_cache_are_quiet() {
    let host = MockHost::install().with_capabilities(&[]);
    assert!(cached_problems("/project/pom.xml", POM).is_empty());
    assert!(host.requests().is_empty());
    drop(host);
    let host = MockHost::install().with_capabilities(CAPS);
    host::storage::set("queries", "not JSON").unwrap();
    assert!(cached_problems("/project/pom.xml", POM).is_empty());
    let oversized = format!("{}{}", " ".repeat(512 << 10), POM);
    assert!(cached_problems("/project/pom.xml", &oversized).is_empty());
    assert!(host.requests().is_empty());
}

fn commit_draft(paths: &[&str]) -> openidea_plugin::CommitDraft {
    openidea_plugin::CommitDraft {
        changes: paths.iter().map(|path| openidea_plugin::CommitChange {
            repository: "/project".into(), path: (*path).into(), status: "modified".into(), ..Default::default()
        }).collect(), ..Default::default()
    }
}

#[test]
fn commit_review_uses_only_changed_files_and_explicitly_historical_scan_evidence() {
    use openidea_plugin::commit::CommitChecks;
    let host = MockHost::install().with_capabilities(CAPS);
    let mut plugin = scanned(&host);
    let requests = host.requests().len();
    let storage = host.storage();
    let problems = plugin.check("cached-dependencies", &commit_draft(&["/project/pom.xml"]), &host.invocation()).unwrap();
    assert_eq!(problems.len(), 1);
    assert!(problems[0].message.contains("Previous scan (Unix"));
    assert!(problems[0].message.contains("Historical evidence may differ from the selected commit"));
    assert_eq!(problems[0].location.as_ref().unwrap().path, "/project/pom.xml");
    assert!(plugin.check("cached-dependencies", &commit_draft(&["/project/App.java", "/other/pom.xml"]), &host.invocation()).unwrap().is_empty());
    assert_eq!(host.requests().len(), requests);
    assert_eq!(host.storage(), storage);
}

#[test]
fn commit_review_reports_missing_stale_and_uncovered_scan_evidence() {
    use openidea_plugin::commit::CommitChecks;
    let host = MockHost::install().with_capabilities(CAPS);
    let mut plugin = DependencyAudit::default();
    let draft = commit_draft(&["/project/pom.xml"]);
    assert!(plugin.check("cached-dependencies", &draft, &host.invocation()).unwrap()[0].message.contains("No cached"));
    let legacy = json!({"rows":[],"scanned_at":1000});
    host::storage::set("scan:/project", &legacy.to_string()).unwrap();
    assert!(plugin.check("cached-dependencies", &draft, &host.invocation()).unwrap()[0].message.contains("stale, incomplete"));
    let fresh = json!({"rows":[],"scanned_at":now(),"checked_files":["/project/other/pom.xml"]});
    host::storage::set("scan:/project", &fresh.to_string()).unwrap();
    assert_eq!(plugin.check("cached-dependencies", &draft, &host.invocation()).unwrap().len(), 1);
    let covered = json!({"rows":[],"scanned_at":now(),"checked_files":["/project/pom.xml"]});
    host::storage::set("scan:/project", &covered.to_string()).unwrap();
    assert!(plugin.check("cached-dependencies", &draft, &host.invocation()).unwrap().is_empty());
    assert!(host.requests().is_empty());
}

#[test]
fn paginated_batches_follow_only_pending_coordinates_and_keep_all_advisories() {
    let host = MockHost::install().with_capabilities(CAPS);
    host.on_fetch(|req| {
        if req.method == "GET" { return osv(req); }
        let body: Value = serde_json::from_slice(req.body.as_deref().unwrap()).unwrap();
        let queries = body["queries"].as_array().unwrap();
        let results: Vec<Value> = queries.iter().map(|q| match q["package"]["name"].as_str().unwrap() {
            "com.fasterxml.jackson.core:jackson-databind" if q["page_token"].is_null() => json!({"next_page_token":"jackson-page-2"}),
            "com.fasterxml.jackson.core:jackson-databind" => {
                assert_eq!(q["page_token"],"jackson-page-2");
                json!({"vulns":[{"id":"GHSA-57j2-w4cx-62h2","modified":"2024-03-15T05:00:00Z"}]})
            },
            "lodash" => {
                assert!(q["page_token"].is_null(), "completed coordinate must not be requested again");
                json!({"vulns":[{"id":"GHSA-35jh-r3h4-6jhm","modified":"2024-01-01T00:00:00Z"}]})
            },
            _ => json!({}),
        }).collect();
        ok(json!({"results":results}).to_string())
    });
    let deps = [jackson(), dep(Ecosystem::Npm,"lodash","4.17.15"), dep(Ecosystem::Npm,"clean","1.0.0")];
    let findings = osv::audit(&deps,true,now(),soon()).unwrap();
    assert_eq!(findings.len(),2);
    assert_eq!(findings[0].fix().as_deref(),Some("2.12.7.1"));
    assert_eq!(findings[1].fix().as_deref(),Some("4.17.21"));
    let requests: Vec<_> = host.requests().into_iter().filter(|r|r.method=="POST").collect();
    assert_eq!(requests.len(),2);
    let second: Value = serde_json::from_slice(requests[1].body.as_deref().unwrap()).unwrap();
    assert_eq!(second["queries"].as_array().unwrap().len(),1);
}

#[test]
fn pagination_deduplicates_advisory_ids_and_retains_newer_modified_details() {
    let host = MockHost::install().with_capabilities(CAPS);
    host.on_fetch(|req| {
        if req.method == "GET" { return osv(req); }
        let body: Value = serde_json::from_slice(req.body.as_deref().unwrap()).unwrap();
        if body["queries"][0]["page_token"].is_null() {
            ok(json!({"results":[{"vulns":[{"id":"GHSA-57j2-w4cx-62h2","modified":"2020"}],"next_page_token":"again"}]}).to_string())
        } else {
            ok(json!({"results":[{"vulns":[{"id":"GHSA-57j2-w4cx-62h2","modified":"2024-03-15T05:00:00Z"}]}]}).to_string())
        }
    });
    let findings=osv::audit(&[jackson()],true,now(),soon()).unwrap();
    assert_eq!(findings[0].advisories.len(),1);
    assert_eq!(findings[0].fix().as_deref(),Some("2.12.7.1"));
}

#[test]
fn repeated_continuations_and_page_errors_do_not_replace_cached_queries_or_report() {
    for fail_page in [false,true] {
        let host = MockHost::install().with_capabilities(CAPS);
        host.add_file("pom.xml",POM);
        let previous=json!({"rows":[],"scanned_at":1,"checked_files":["/project/pom.xml"]}).to_string();
        host::storage::set("scan:/project",&previous).unwrap();
        host::storage::set("queries","{}").unwrap();
        host.on_fetch(move |req| {
            let body: Value = serde_json::from_slice(req.body.as_deref().unwrap()).unwrap();
            if fail_page && !body["queries"][0]["page_token"].is_null() { return Err(Error::Failed("page unavailable".into())); }
            let results: Vec<_> = body["queries"].as_array().unwrap().iter().map(|_| json!({"next_page_token":"repeated"})).collect();
            ok(json!({"results":results}).to_string())
        });
        assert!(DependencyAudit::default().run_command("scan",&host.invocation(),None).is_err());
        assert_eq!(host.storage()["queries"],"{}");
        assert_eq!(host.storage()["scan:/project"],previous);
        assert!(last_failure(&host.invocation().project().unwrap()).is_some());
        assert_eq!(posts(&host),2);
    }
}

#[test]
fn discovery_caps_are_reported_as_partial_and_do_not_pass_commit_review() {
    use openidea_plugin::commit::CommitChecks;
    let host = MockHost::install().with_capabilities(CAPS);
    host.on_fetch(osv);
    for i in 0..201 { host.add_file(&format!("m{i:03}/pom.xml"),"<project/>"); }
    let mut plugin=DependencyAudit::default();
    plugin.run_command("scan",&host.invocation(),None).unwrap();
    let report=stored_report(&host.invocation().project().unwrap()).unwrap().unwrap();
    assert_eq!(report.checked_files.len(),200);
    assert!(report.discovery_limited);
    assert!(host.project_notifications()[0].1.notification.body.contains("Partial report"));
    assert!(report_status(&report,None).contains("discovery reached its limit"));
    let problems=plugin.check("cached-dependencies",&commit_draft(&["/project/m000/pom.xml"]),&host.invocation()).unwrap();
    assert_eq!(problems.len(),1);
}

#[test]
fn endless_unique_continuations_stop_at_a_bound_without_saving_partial_answers() {
    let host = MockHost::install().with_capabilities(CAPS);
    host.on_fetch(|req| {
        let body: Value = serde_json::from_slice(req.body.as_deref().unwrap()).unwrap();
        let next = body["queries"][0]["page_token"].as_str().and_then(|s|s.parse::<u32>().ok()).unwrap_or(0)+1;
        ok(json!({"results":[{"next_page_token":next.to_string()}]}).to_string())
    });
    let error=osv::audit(&[jackson()],true,now(),soon()).unwrap_err();
    assert!(error.to_string().contains("page bound"));
    assert_eq!(posts(&host),33);
    assert!(!host.storage().contains_key("queries"));
}

#[test]
fn packaged_selected_actions_validate_kind_membership_and_confirm_current_buffer() {
    let caps:Vec<_> = CAPS.iter().copied().chain(["editor.read","editor.write"]).collect();
    let host=MockHost::install().with_capabilities(&caps);
    let mut plugin=scanned(&host);
    host.open_editor("/project/pom.xml","xml",POM,&[]);
    let model=resource_model(&mut plugin,&host);
    let items=model["body"]["items"].as_array().unwrap();
    let dep=items.iter().find(|r|r["cells"][0]=="Dependency" && r["cells"][2]=="Maven").unwrap()["key"].as_str().unwrap().to_owned();
    let advisory=items.iter().find(|r|r["cells"][0]=="Advisory" && r["cells"][2]=="Maven").unwrap()["key"].as_str().unwrap().to_owned();
    assert!(dep.starts_with("dep:") && advisory.starts_with("advisory:"));
    let requests=host.requests().len();
    let mut command=|id:&str,key:&str| {
        plugin.handle("vulnerabilities",ViewEvent::Message(json!({"channel":"ui-kit","sequence":50,"project":"/project","targetView":"vulnerabilities","op":"command","id":id,"key":key}).to_string()),&host.invocation()).unwrap();
        serde_json::from_str::<Value>(&host.view_messages().last().unwrap().1).unwrap()
    };
    let denied=command("preview-upgrade",&advisory);
    assert!(denied.get("error").is_some(),"{denied}");
    assert_eq!(host.editor_text(),POM);
    let result=command("preview-upgrade",&dep); // no confirmation response => cancel
    assert!(result.get("error").is_none(),"{result}");
    assert_eq!(host.editor_text(),POM);
    host.answer_next_prompt(Some(&[]));
    let result=command("preview-upgrade",&dep);
    assert!(result.get("error").is_none(),"{result}");
    assert_eq!(host.editor_text(),POM.replace("2.12.3","2.12.7.1"));
    assert_eq!(host.editor_version(),2);
    let stale=command("preview-upgrade",&dep);
    assert!(stale.get("error").is_some(),"{stale}");
    let result=command("open-advisory",&advisory);
    assert!(result.get("error").is_none(),"{result}");
    assert_eq!(host.opened_urls(),["https://osv.dev/vulnerability/GHSA-57j2-w4cx-62h2"]);
    assert_eq!(host.requests().len(),requests);
}

fn selected_advisory(plugin:&mut DependencyAudit,host:&MockHost,name:&str)->String {
    let model=resource_model(plugin,host);
    model["body"]["items"].as_array().unwrap().iter().find(|r|r["cells"][0]=="Advisory" && r["cells"][1]==name).unwrap()["key"].as_str().unwrap().into()
}

#[test]
fn scoped_ignore_filters_every_surface_and_restores_retained_evidence_without_rescan() {
    use openidea_plugin::commit::CommitChecks;
    let caps:Vec<_>=CAPS.iter().copied().chain(["vcs.read"]).collect();
    let host=MockHost::install().with_capabilities(&caps);
    let mut plugin=scanned(&host);
    let selected=selected_advisory(&mut plugin,&host,"com.fasterxml.jackson.core:jackson-databind");
    let before=host.storage()["scan:/project"].clone();
    let requests=host.requests().len();
    let doc=document("/project/pom.xml","xml",POM);
    assert_eq!(plugin.diagnostics(&doc).len(),1);
    assert_eq!(plugin.code_actions(&doc,line(3)).iter().filter(|a|a.title.starts_with("Upgrade ")).count(),1);
    let draft=commit_draft(&["/project/pom.xml"]);
    assert_eq!(plugin.check("cached-dependencies",&draft,&host.invocation()).unwrap().len(),1);
    host.answer_next_prompt(Some(&["Temporarily accepted; tracked in SEC-42"]));
    host.answer_next_prompt(Some(&[]));
    plugin.run_command("ignore-finding",&host.invocation(),Some(&selected)).unwrap();
    assert_eq!(host.storage()["scan:/project"],before,"suppression must not delete snapshot facts");
    assert!(plugin.diagnostics(&doc).is_empty());
    assert!(plugin.code_actions(&doc,line(3)).is_empty());
    assert!(plugin.check("cached-dependencies",&draft,&host.invocation()).unwrap().is_empty());
    assert!(!tree(&mut plugin,&host).iter().any(|r|r.label.starts_with("com.fasterxml.jackson")));
    assert!(tree(&mut plugin,&host).iter().any(|r|r.label.starts_with("lodash")),"other package remains visible");
    assert!(plugin.run_command("ignore-finding",&host.invocation(),Some(&selected)).is_err(),"hidden/stale selections cannot add another record");
    let ignores=host.storage()["scoped-ignores:/project"].clone();
    assert!(ignores.contains("SEC-42") && ignores.contains("2.12.3"));
    // A new scan must retain ignored facts too, so restoring does not depend on an old instance.
    plugin.run_command("scan",&host.invocation(),None).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&host.storage()["scan:/project"]).unwrap()["rows"].as_array().unwrap().len(),2);
    assert!(host.project_notifications().last().unwrap().1.notification.body.contains("1 of 3"));
    let requests_after_scan=host.requests().len();
    let mut fresh=DependencyAudit::default();
    assert!(fresh.diagnostics(&doc).is_empty());
    host.answer_next_prompt(Some(&["0"]));
    host.answer_next_prompt(Some(&[]));
    fresh.run_command("review-ignores",&host.invocation(),None).unwrap();
    assert_eq!(fresh.diagnostics(&doc).len(),1);
    assert_eq!(fresh.code_actions(&doc,line(3)).iter().filter(|a|a.title.starts_with("Upgrade ")).count(),1);
    assert_eq!(fresh.check("cached-dependencies",&draft,&host.invocation()).unwrap().len(),1);
    assert!(tree(&mut fresh,&host).iter().any(|r|r.label.starts_with("com.fasterxml.jackson")));
    assert_eq!(host.requests().len(),requests_after_scan);
    assert!(host.requests().len()>=requests);
    assert_eq!(host.storage()["scoped-ignores:/project"],"[]");
}

#[test]
fn ignore_reason_confirmation_and_restore_cancellation_do_not_mutate_evidence() {
    let host=MockHost::install().with_capabilities(CAPS);
    let mut plugin=scanned(&host);
    let selected=selected_advisory(&mut plugin,&host,"lodash");
    let before=host.storage();
    plugin.run_command("ignore-finding",&host.invocation(),Some(&selected)).unwrap(); // reason cancelled
    assert_eq!(host.storage(),before);
    host.answer_next_prompt(Some(&["reviewing"])); // confirmation dismissed
    plugin.run_command("ignore-finding",&host.invocation(),Some(&selected)).unwrap();
    assert_eq!(host.storage(),before);
    for reason in ["  ".to_owned(),"x".repeat(513),"line\ncontrol".into()] {
        host.answer_next_prompt(Some(&[&reason]));
        assert!(plugin.run_command("ignore-finding",&host.invocation(),Some(&selected)).is_err());
        assert_eq!(host.storage(),before);
    }
    host.answer_next_prompt(Some(&["accepted temporarily"]));host.answer_next_prompt(Some(&[]));
    plugin.run_command("ignore-finding",&host.invocation(),Some(&selected)).unwrap();
    let ignored=host.storage();
    plugin.run_command("review-ignores",&host.invocation(),None).unwrap();
    assert_eq!(host.storage(),ignored);
    host.answer_next_prompt(Some(&["0"]));
    plugin.run_command("review-ignores",&host.invocation(),None).unwrap();
    assert_eq!(host.storage(),ignored);
    host.answer_next_prompt(Some(&["9999"]));
    assert!(plugin.run_command("review-ignores",&host.invocation(),None).is_err());
    assert_eq!(host.storage(),ignored);
}

#[test]
fn scoped_ignores_remain_separate_from_global_setting_and_project_identity() {
    let host=MockHost::install().with_capabilities(CAPS);
    let mut plugin=scanned(&host);
    let selected=selected_advisory(&mut plugin,&host,"lodash");
    host.answer_next_prompt(Some(&["accepted"]));host.answer_next_prompt(Some(&[]));
    plugin.run_command("ignore-finding",&host.invocation(),Some(&selected)).unwrap();
    let scopes=host.storage()["scoped-ignores:/project"].clone();
    host.set_setting("ignoredAdvisories",r#""GHSA-57j2-w4cx-62h2""#);
    assert!(stored_rows(&host.invocation().project().unwrap()).unwrap().unwrap().is_empty());
    host.answer_next_prompt(Some(&["0"]));host.answer_next_prompt(Some(&[]));
    plugin.run_command("review-ignores",&host.invocation(),None).unwrap();
    assert!(tree(&mut plugin,&host).iter().all(|r|!r.label.starts_with("com.fasterxml.jackson")),"restoration cannot override global setting");
    assert!(tree(&mut plugin,&host).iter().any(|r|r.label.starts_with("lodash")));
    host::storage::set("scoped-ignores:/project",&scopes).unwrap();
    host.set_project_root(Some("/another"));
    assert!(scoped::load("/another").is_empty());
    assert!(plugin.run_command("ignore-finding",&host.invocation(),Some(&selected)).is_err());
    assert!(plugin.run_command("review-ignores",&host.invocation(),None).unwrap().unwrap().contains("No ignored findings"));
    host.set_project_root(None);
    assert!(plugin.run_command("review-ignores",&host.invocation(),None).is_err());
    assert_eq!(host.storage()["scoped-ignores:/project"],scopes);
}

#[test]
fn alias_scopes_survive_detail_cache_eviction_and_recompute_visible_fix_recommendations() {
    let host=MockHost::install().with_capabilities(CAPS);
    let mut plugin=scanned(&host);
    let mut raw:Value=serde_json::from_str(&host.storage()["scan:/project"]).unwrap();
    let row=raw["rows"].as_array_mut().unwrap().iter_mut().find(|r|r["name"]=="lodash").unwrap();
    row["advisories"]=json!(["GHSA-35jh-r3h4-6jhm","GHSA-other"]);
    row["fix"]=json!("99.0.0");
    row["details"][0]["aliases"]=json!(["CVE-2026-1234"]);
    row["details"].as_array_mut().unwrap().push(json!({"id":"GHSA-other","summary":"Other issue","modified":"x","severity":"LOW","fixed":{"npm:lodash":["4.17.20"]}}));
    host::storage::set("scan:/project",&raw.to_string()).unwrap();
    let selected=selected_advisory(&mut plugin,&host,"lodash");
    host.answer_next_prompt(Some(&["accepted advisory only"]));host.answer_next_prompt(Some(&[]));
    plugin.run_command("ignore-finding",&host.invocation(),Some(&selected)).unwrap();
    for key in host::storage::keys("adv:").unwrap() {host::storage::delete(&key).unwrap();}
    let report=stored_report(&host.invocation().project().unwrap()).unwrap().unwrap();
    let row=report.rows.iter().find(|r|r.name=="lodash").unwrap();
    assert_eq!(row.advisories,["GHSA-other"]);
    assert_eq!(row.fix.as_deref(),Some("4.17.20"),"hidden advisory must not dictate visible upgrade action");
    assert!(scoped::matches(&scoped::load("/project"),"npm","lodash","4.17.15","CVE-2026-1234",&[]));
    // Restore works from retained snapshot details even after separate cache eviction.
    host.answer_next_prompt(Some(&["0"]));host.answer_next_prompt(Some(&[]));
    plugin.run_command("review-ignores",&host.invocation(),None).unwrap();
    let report=stored_report(&host.invocation().project().unwrap()).unwrap().unwrap();
    assert_eq!(report.rows.iter().find(|r|r.name=="lodash").unwrap().advisories.len(),2);
}

#[test]
fn packaged_ignore_and_empty_report_review_restore_use_declared_controls() {
    let host=MockHost::install().with_capabilities(CAPS);
    host.set_setting("ignoredAdvisories",r#""GHSA-35jh-r3h4-6jhm""#);
    let mut plugin=scanned(&host);
    let selected=selected_advisory(&mut plugin,&host,"com.fasterxml.jackson.core:jackson-databind");
    let requests=host.requests().len();
    host.answer_next_prompt(Some(&["Accepted until patch evaluation"]));host.answer_next_prompt(Some(&[]));
    plugin.handle("vulnerabilities",ViewEvent::Message(json!({"channel":"ui-kit","sequence":61,"project":"/project","targetView":"vulnerabilities","op":"command","id":"ignore-finding","key":selected}).to_string()),&host.invocation()).unwrap();
    let response:Value=serde_json::from_str(&host.view_messages().last().unwrap().1).unwrap();
    assert!(response.get("error").is_none(),"{response}");
    assert_eq!(response["model"]["body"]["kind"],"message");
    assert!(response["model"]["toolbar"].as_array().unwrap().iter().any(|b|b["key"]=="review-ignores"),"{response}");
    host.answer_next_prompt(Some(&["0"]));host.answer_next_prompt(Some(&[]));
    plugin.handle("vulnerabilities",ViewEvent::Message(json!({"channel":"ui-kit","sequence":62,"project":"/project","targetView":"vulnerabilities","op":"event","event":{"kind":"button","key":"review-ignores"}}).to_string()),&host.invocation()).unwrap();
    let response:Value=serde_json::from_str(&host.view_messages().last().unwrap().1).unwrap();
    assert!(response.get("error").is_none(),"{response}");
    assert_eq!(response["model"]["body"]["kind"],"resources");
    assert_eq!(response["model"]["body"]["items"].as_array().unwrap().len(),4);
    assert_eq!(host.requests().len(),requests);
}

#[test]
fn report_write_limit_preserves_a_real_last_good_report_and_reader_accepts_exact_boundary() {
    let host=MockHost::install().with_capabilities(CAPS);
    let mut plugin=scanned(&host);
    let project=host.invocation().project().unwrap();
    let requests=host.requests().len();
    let previous=host.storage()["scan:/project"].clone();
    let rows_before=tree(&mut plugin,&host).iter().map(|r|r.key.clone()).collect::<Vec<_>>();
    let mut report:Report=serde_json::from_str(&previous).unwrap();
    report.checked_files.push(String::new());
    let empty_size=serde_json::to_string(&report).unwrap().len();
    *report.checked_files.last_mut().unwrap()="x".repeat(MAX_REPORT_BYTES-empty_size+1);
    assert_eq!(serde_json::to_string(&report).unwrap().len(),MAX_REPORT_BYTES+1);
    assert!(persist_report(&project,&report).unwrap_err().to_string().contains("previous report retained"));
    assert_eq!(host.storage()["scan:/project"],previous,"oversized replacement never reaches storage_set");
    assert_eq!(tree(&mut plugin,&host).iter().map(|r|r.key.clone()).collect::<Vec<_>>(),rows_before);
    report.checked_files.last_mut().unwrap().pop();
    assert_eq!(serde_json::to_string(&report).unwrap().len(),MAX_REPORT_BYTES);
    persist_report(&project,&report).unwrap();
    assert_eq!(host.storage()["scan:/project"].len(),MAX_REPORT_BYTES);
    assert_eq!(stored_report(&project).unwrap().unwrap().rows.len(),2,"writer boundary must remain readable");
    assert_eq!(host.requests().len(),requests,"bounded persistence never queries OSV");
}

fn invalidations(host: &MockHost) -> Vec<(String,String)> {
    host.view_messages().into_iter().filter(|(_,message)|message==r#"{"channel":"ui-kit-refresh"}"#).collect()
}
#[test]
fn external_scan_success_and_failure_invalidate_report_but_cached_render_does_not() {
    let host=MockHost::install().with_capabilities(CAPS);
    let mut plugin=scanned(&host);
    assert_eq!(invalidations(&host),[(REPORT_VIEW.to_owned(),r#"{"channel":"ui-kit-refresh"}"#.to_owned())]);
    let before=host.storage()["scan:/project"].clone();let messages=invalidations(&host).len();let requests=host.requests().len();
    resource_model(&mut plugin,&host);resource_model(&mut plugin,&host);
    assert_eq!(invalidations(&host).len(),messages);assert_eq!(host.requests().len(),requests);
    host.add_file("extra/package.json",r#"{"dependencies":{"previously-unseen":"0.1.0"}}"#);
    host.on_fetch(|_|Err(Error::Failed("deterministic failure".into())));
    assert!(plugin.run_command("scan",&host.invocation(),None).is_err());
    assert_eq!(invalidations(&host).len(),messages+1);
    assert_eq!(host.storage()["scan:/project"],before);
    let model=resource_model(&mut plugin,&host);assert!(model["status"].as_str().unwrap().contains("rescan failed"));assert_eq!(model["body"]["items"].as_array().unwrap().len(),5);
}
#[test]
fn scoped_ignore_restore_and_upgrade_notify_committed_state_only() {
    let caps:Vec<_>=CAPS.iter().copied().chain(["editor.read","editor.write"]).collect();
    let host=MockHost::install().with_capabilities(&caps);let mut plugin=scanned(&host);
    let selected=selected_advisory(&mut plugin,&host,"com.fasterxml.jackson.core:jackson-databind");let before=invalidations(&host).len();
    plugin.run_command("ignore-finding",&host.invocation(),Some(&selected)).unwrap();assert_eq!(invalidations(&host).len(),before,"cancel is not committed");
    host.answer_next_prompt(Some(&["Tracked acceptance"]));host.answer_next_prompt(Some(&[]));plugin.run_command("ignore-finding",&host.invocation(),Some(&selected)).unwrap();assert_eq!(invalidations(&host).len(),before+1);
    host.answer_next_prompt(Some(&["0"]));host.answer_next_prompt(Some(&[]));plugin.run_command("review-ignores",&host.invocation(),None).unwrap();assert_eq!(invalidations(&host).len(),before+2);
    let model=resource_model(&mut plugin,&host);let key=model["body"]["items"].as_array().unwrap().iter().find(|row|row["cells"][0]=="Dependency"&&row["cells"][2]=="Maven").unwrap()["key"].as_str().unwrap();
    host.open_editor("/project/pom.xml","xml",POM,&[]);host.answer_next_prompt(Some(&[]));plugin.run_command("preview-upgrade",&host.invocation(),Some(key)).unwrap();assert_eq!(invalidations(&host).len(),before+3);
}
#[test]
fn missing_denied_and_foreign_report_owners_replace_old_packaged_body() {
    let host=MockHost::install().with_capabilities(CAPS);let mut plugin=scanned(&host);let model=resource_model(&mut plugin,&host);
    let key=model["body"]["items"][1]["key"].as_str().unwrap().to_owned();let original=host.storage()["scan:/project"].clone();
    let mut corrupt:Value=serde_json::from_str(&original).unwrap();corrupt["rows"][0]["path"]=json!("/foreign/private/pom.xml");host::storage::set("scan:/project",&corrupt.to_string()).unwrap();
    let model=resource_model(&mut plugin,&host);assert_eq!(model["body"]["kind"],"message");assert!(!model.to_string().contains("private"));assert!(plugin.targets.is_empty());
    assert!(plugin.run_command("open-advisory",&host.invocation(),Some(&key)).is_err());assert!(host.opened_urls().is_empty());
    host::storage::set("scan:/project",&original).unwrap();host.set_project_root(None);assert_eq!(resource_model(&mut plugin,&host)["body"]["kind"],"message");
    host.set_project_root(Some("/project"));let host=host.with_capabilities(&["project.read"]);assert_eq!(resource_model(&mut plugin,&host)["body"]["kind"],"message");
    assert!(plugin.targets.is_empty()&&plugin.display.is_empty());
}
#[test]
fn nested_source_and_lock_owners_are_not_inherited_from_parent_root() {
    let host=MockHost::install().with_capabilities(CAPS);let mut plugin=scanned(&host);let root=host.invocation().project().unwrap();
    assert!(owned_lock(&root,"/project/web/package.json").is_some());
    host.add_project("/project/web/package-lock.json");assert!(owned_lock(&root,"/project/web/package.json").is_none());
    assert!(read_deps(&root,"/foreign/package.json").is_err());
    host.add_project("/project/web");let model=resource_model(&mut plugin,&host);assert_eq!(model["body"]["kind"],"message");assert!(plugin.targets.is_empty());
}
#[test]
fn report_owner_proof_deduplicates_and_bounds_source_checks() {
    let host=MockHost::install().with_capabilities(CAPS);let _plugin=scanned(&host);
    let report=stored_report(&host.invocation().project().unwrap()).unwrap().unwrap();
    let mut paths=HashSet::new();for row in &report.rows {paths.insert(row.path.as_str());for source in &row.sources {paths.insert(source.path.as_str());}}
    let mut checked=HashSet::new();validate_report_sources(&report,|path|{assert!(checked.insert(path.to_owned()),"duplicate host lookup");true}).unwrap();assert_eq!(checked.len(),paths.len());
    let mut value:Value=serde_json::from_str(&host.storage()["scan:/project"]).unwrap();let mut rows=Vec::new();for n in 0..=MAX_REPORT_OWNER_PATHS {let mut row=value["rows"][0].clone();row["path"]=json!(format!("/project/{n}/pom.xml"));rows.push(row);}value["rows"]=json!(rows);let oversized:Report=serde_json::from_value(value).unwrap();let mut calls=0;assert!(validate_report_sources(&oversized,|_|{calls+=1;true}).is_err());assert_eq!(calls,0,"reject path-count overflow before VM imports");
}
#[test]
fn saved_lock_cache_clear_and_observed_project_switch_emit_no_foreign_model() {
    let host=MockHost::install().with_capabilities(CAPS);let mut plugin=scanned(&host);resource_model(&mut plugin,&host);host.set_setting("autoCheck","false");let before=invalidations(&host).len();let requests=host.requests().len();
    plugin.handle_events(&[Event::FileSaved("/project/web/package-lock.json".into())],&host.invocation());assert_eq!(invalidations(&host).len(),before+1);assert_eq!(host.requests().len(),requests);assert!(plugin.targets.is_empty());
    host.set_project_root(None);plugin.handle_events(&[Event::EditorFocused(None)],&host.invocation());assert_eq!(invalidations(&host).len(),before+2);assert_eq!(resource_model(&mut plugin,&host)["body"]["kind"],"message");
    assert!(invalidations(&host).iter().all(|(view,text)|view==REPORT_VIEW&&text==r#"{"channel":"ui-kit-refresh"}"#));
}
#[test]
fn existing_opted_in_open_check_invalidates_after_cache_commit_without_render_fetch() {
    let host=MockHost::install().with_capabilities(CAPS);let mut plugin=scanned(&host);let before=invalidations(&host).len();
    plugin.handle_events(&[Event::FileOpened("/project/pom.xml".into())],&host.invocation());assert_eq!(invalidations(&host).len(),before+1);
    let requests=host.requests().len();resource_model(&mut plugin,&host);assert_eq!(host.requests().len(),requests);assert_eq!(invalidations(&host).len(),before+1);
}

#[test]
fn editor_actions_are_lazy_and_upgrade_preview_is_exact_before_host_apply() {
    let host=MockHost::install().with_capabilities(CAPS);let mut plugin=scanned(&host);let doc=document("/project/pom.xml","xml",POM);let requests=host.requests().len();let before=host.storage();
    let actions=plugin.code_actions(&doc,line(3));assert!(actions.iter().any(|a|a.title.starts_with("Open OSV advisory")));assert!(actions.iter().any(|a|a.title.starts_with("Ignore ")));assert!(actions.iter().all(|a|a.edits.is_empty()&&a.data.is_some()));assert_eq!(host.storage(),before);assert!(host.opened_urls().is_empty());
    let action=actions.into_iter().find(|a|a.title.starts_with("Upgrade ")).unwrap();
    assert!(matches!(plugin.resolve_code_action(&doc,action.clone()),Err(Error::Cancelled)));
    host.answer_next_prompt(Some(&[]));let resolved=plugin.resolve_code_action(&doc,action).unwrap();assert_eq!(resolved.edits[0].1[0].text,"2.12.7.1");assert!(resolved.data.is_none());
    assert_eq!(host.requests().len(),requests);assert_eq!(host.storage(),before);
}
#[test]
fn editor_advisory_and_exact_ignore_resolve_without_network_or_text_edits() {
    let host=MockHost::install().with_capabilities(CAPS);let mut plugin=scanned(&host);let doc=document("/project/pom.xml","xml",POM);let requests=host.requests().len();
    let offered=plugin.code_actions(&doc,line(3));let open=offered.iter().find(|a|a.title.starts_with("Open OSV advisory")).unwrap().clone();
    host.answer_next_prompt(Some(&[]));let resolved=plugin.resolve_code_action(&doc,open).unwrap();assert!(resolved.edits.is_empty()&&resolved.data.is_none());assert_eq!(host.opened_urls(),["https://osv.dev/vulnerability/GHSA-57j2-w4cx-62h2"]);
    let ignore=offered.iter().find(|a|a.title.starts_with("Ignore ")).unwrap().clone();host.answer_next_prompt(Some(&["Accepted for fixture review"]));host.answer_next_prompt(Some(&[]));assert!(plugin.resolve_code_action(&doc,ignore).unwrap().edits.is_empty());
    assert!(plugin.code_actions(&doc,line(3)).is_empty());assert!(host.storage()["scoped-ignores:/project"].contains("Accepted for fixture review"));assert_eq!(host.requests().len(),requests);
    host.answer_next_prompt(Some(&["0"]));host.answer_next_prompt(Some(&[]));plugin.run_command("review-ignores",&host.invocation(),None).unwrap();assert!(!plugin.code_actions(&doc,line(3)).is_empty());assert_eq!(host.requests().len(),requests);
}
#[test]
fn stale_editor_version_source_and_forged_effect_cannot_return_an_edit() {
    let host=MockHost::install().with_capabilities(CAPS);let mut plugin=scanned(&host);let doc=document("/project/pom.xml","xml",POM);let original=plugin.code_actions(&doc,line(3)).into_iter().find(|a|a.title.starts_with("Upgrade ")).unwrap();
    let mut changed=doc.clone();changed.version+=1;assert!(plugin.resolve_code_action(&changed,original.clone()).is_err());
    host.add_file("pom.xml",&POM.replace("2.12.3","2.13.0"));assert!(plugin.resolve_code_action(&doc,original.clone()).is_err());host.add_file("pom.xml",POM);
    let mut forged=original.clone();let mut data:Value=serde_json::from_str(forged.data.as_ref().unwrap()).unwrap();data["effect"]=json!(["arbitrary"]);forged.data=Some(data.to_string());assert!(plugin.resolve_code_action(&doc,forged).is_err());
    let mut foreign=original;let mut data:Value=serde_json::from_str(foreign.data.as_ref().unwrap()).unwrap();data["root"]=json!("/foreign");foreign.data=Some(data.to_string());assert!(plugin.resolve_code_action(&doc,foreign).is_err());assert!(host.opened_urls().is_empty());
}
#[test]
fn editor_ignore_cancel_and_evicted_advice_leave_sources_and_scopes_untouched() {
    let host=MockHost::install().with_capabilities(CAPS);let mut plugin=scanned(&host);let doc=document("/project/pom.xml","xml",POM);let before=host.storage();let requests=host.requests().len();
    let actions=plugin.code_actions(&doc,line(3));let ignore=actions.iter().find(|a|a.title.starts_with("Ignore ")).unwrap().clone();
    assert!(matches!(plugin.resolve_code_action(&doc,ignore.clone()),Err(Error::Cancelled)));assert_eq!(host.storage(),before);
    host.answer_next_prompt(Some(&["reason without final approval"]));assert!(matches!(plugin.resolve_code_action(&doc,ignore),Err(Error::Cancelled)));assert_eq!(host.storage(),before);
    let upgrade=actions.into_iter().find(|a|a.title.starts_with("Upgrade ")).unwrap();host::storage::delete("queries").unwrap();
    assert!(matches!(plugin.resolve_code_action(&doc,upgrade),Err(Error::Stale)));assert_eq!(host.requests().len(),requests);assert!(host.opened_urls().is_empty());assert!(!host.storage().contains_key("scoped-ignores:/project"));
}

#[test]
fn dependency_row_exposes_advisories_and_exact_ignores_without_losing_upgrade() {
    let host=MockHost::install().with_capabilities(CAPS);let mut plugin=scanned(&host);
    let model=resource_model(&mut plugin,&host);
    let dep=model["body"]["items"].as_array().unwrap().iter().find(|row|row["cells"][0]=="Dependency"&&row["cells"][1]=="lodash").unwrap()["key"].as_str().unwrap().to_owned();
    let actions=model["actions"].as_array().unwrap().iter().filter(|action|action["kind"]=="dep").map(|action|action["id"].as_str().unwrap()).collect::<Vec<_>>();
    assert_eq!(actions,["preview-upgrade","open-declaration","open-advisory","ignore-finding"]);
    let open=model["actions"].as_array().unwrap().iter().filter(|action|action["id"]=="open-advisory").collect::<Vec<_>>();
    assert_eq!(open.iter().find(|action|action["kind"]=="advisory").unwrap()["replacesOpen"],true);
    assert_eq!(open.iter().find(|action|action["kind"]=="dep").unwrap()["replacesOpen"],false);
    let requests=host.requests().len();
    let command=|plugin:&mut DependencyAudit,id:&str| {
        plugin.handle("vulnerabilities",ViewEvent::Message(json!({"channel":"ui-kit","sequence":70,"project":"/project","targetView":"vulnerabilities","op":"command","id":id,"key":dep}).to_string()),&host.invocation()).unwrap();
        serde_json::from_str::<Value>(&host.view_messages().last().unwrap().1).unwrap()
    };
    assert!(command(&mut plugin,"open-advisory").get("error").is_none());
    assert_eq!(host.opened_urls(),["https://osv.dev/vulnerability/GHSA-35jh-r3h4-6jhm"]);
    assert!(command(&mut plugin,"ignore-finding").get("error").is_none()); // reason cancelled
    assert!(scoped::load("/project").is_empty());
    host.answer_next_prompt(Some(&["Accepted while evaluating patch"]));host.answer_next_prompt(Some(&[]));
    assert!(command(&mut plugin,"ignore-finding").get("error").is_none());
    let records=scoped::load("/project");assert_eq!(records.len(),1);assert!(host.storage()["scoped-ignores:/project"].contains("GHSA-35JH-R3H4-6JHM"));
    assert_eq!(host.requests().len(),requests);assert!(host.process_runs().is_empty());
}

#[test]
fn known_malicious_evidence_is_dated_visible_and_never_inherits_an_old_upgrade() {
    let host=MockHost::install().with_capabilities(CAPS);
    host.add_file("requirements.txt","0-8==0.0.0.1\n");
    let advisory=osv::parse_advisory(include_str!("../tests/fixtures/known-malicious-osv.json")).unwrap();
    let row=json!({"path":"/project/requirements.txt","line":0,"name":"0-8","version":"0.0.0.1","ecosystem":"PyPI","fix":"99.0","advisories":[advisory.id],"details":[advisory]});
    host::storage::set("scan:/project",&json!({"rows":[row],"scanned_at":123}).to_string()).unwrap();
    let report=stored_report(&host.invocation().project().unwrap()).unwrap().unwrap();
    assert!(report.rows[0].fix.is_none(),"historical fix must not override malicious-package review");
    let Body::Other{data,..}=resource_body("/project",&report,None) else{panic!()};
    assert!(data.contains("Known malicious"));
    assert!(data.contains("OpenSSF"));
    assert!(data.contains("reversing-labs"));
    assert!(data.contains("1970-01-01 00:02 UTC"));
    assert!(!data.contains("99.0"));
    assert!(host.requests().is_empty(),"render remains cache-only");
}

#[test]
fn report_lists_checked_coordinates_and_skipped_files() {
    let _host = MockHost::install();
    let row = json!({"path":"/project/package.json","line":0,"name":"bad","version":"1.0.0","fix":null,"advisories":["GHSA-x"],"ecosystem":"npm"});
    let report: Report = serde_json::from_value(json!({
        "rows":[row],
        "coordinates":[["npm","bad","1.0.0"],["npm","ok\u{7}<b>","2.0.0"]],
        "skipped_paths":["/project/pom.xml"]
    })).unwrap();
    let Body::Other { data, .. } = resource_body("/project", &report, None) else { panic!() };
    let value: Value = serde_json::from_str(&data).unwrap();
    let items = value["items"].as_array().unwrap();
    let kinds: Vec<_> = items.iter().map(|i| i["cells"][0].as_str().unwrap()).collect();
    assert_eq!(kinds, ["Dependency", "Advisory", "Checked", "Skipped file"]);
    assert_eq!(items[2]["cells"][1], "ok<b>");
    assert_eq!(items[2]["details"][0]["value"], "npm/ok<b>@2.0.0");
    assert_eq!(items[3]["cells"][7], "pom.xml");
}

#[test]
fn report_lists_private_notes_as_not_checked() {
    let _host = MockHost::install();
    let report: Report = serde_json::from_value(json!({
        "rows":[],
        "notes":[["Private, not queried","secret","npm","1.0.0","/project/package-lock.json"]]
    })).unwrap();
    let Body::Other { data, .. } = resource_body("/project", &report, None) else { panic!() };
    let value: Value = serde_json::from_str(&data).unwrap();
    let item = &value["items"][0];
    assert_eq!(item["cells"][0], "Private, not queried");
    assert_eq!(item["cells"][5], "Not checked");
    assert!(item["details"][1]["value"].as_str().unwrap().contains("never sent to OSV.dev"));
}

#[test]
fn report_lists_local_workspace_and_marker_notes() {
    let _host = MockHost::install();
    let report: Report = serde_json::from_value(json!({
        "rows":[],
        "notes":[["Local workspace package","a","npm","workspace:^","/project/package.json"],["Marker not evaluated","click","PyPI","8.0.0","/project/pyproject.toml"]]
    })).unwrap();
    let Body::Other { data, .. } = resource_body("/project", &report, None) else { panic!() };
    let value: Value = serde_json::from_str(&data).unwrap();
    let items = value["items"].as_array().unwrap();
    assert_eq!(items[0]["cells"][0], "Local workspace package");
    assert_eq!(items[0]["cells"][5], "Not checked");
    assert_eq!(items[1]["cells"][0], "Marker not evaluated");
    assert_eq!(value["totalRows"], 2);
}

#[test]
fn notify_project_targets_the_project_and_falls_back_globally_when_it_is_gone() {
    let host = MockHost::install();
    host.set_project_root(Some("/work/app"));
    let project = host.invocation().project().unwrap();
    notify_project(&project, NotificationLevel::Info, "T", "one");
    assert_eq!(host.project_notifications().len(), 1);
    assert!(host.notifications().is_empty());
    host.set_project_root(None);
    notify_project(&project, NotificationLevel::Info, "T", "two");
    assert_eq!(host.notifications().len(), 1);
}

#[test]
fn npm_graph_tab_inspect_button_renders_the_root_lockfile_tree() {
    let host=MockHost::install().with_capabilities(CAPS);
    host.add_file("package-lock.json",r#"{"lockfileVersion":3,"packages":{"":{"dependencies":{"a":"*"}},"node_modules/a":{"version":"1"}}}"#);
    let mut plugin=DependencyAudit::default();
    let send=|plugin:&mut DependencyAudit,sequence:u32,op:Value|{
        let mut request=json!({"channel":"ui-kit","sequence":sequence,"project":"/project","targetView":"npm-dependency-graph"});
        request.as_object_mut().unwrap().extend(op.as_object().unwrap().clone());
        plugin.handle("vulnerabilities",ViewEvent::Message(request.to_string()),&host.invocation()).unwrap();
        serde_json::from_str::<Value>(&host.view_messages().last().unwrap().1).unwrap()
    };
    let response=send(&mut plugin,1,json!({"op":"select-view","target":"npm-dependency-graph","targetView":"vulnerabilities"}));
    assert_eq!(response["model"]["activeView"],"npm-dependency-graph","{response}");
    let response=send(&mut plugin,2,json!({"op":"event","event":{"kind":"button","key":"inspect-npm-graph"}}));
    assert!(response.get("error").is_none(),"{response}");
    assert_eq!(response["model"]["body"]["kind"],"tree","{response}");
}

#[test]
fn gradle_division_keeps_dependencies() {
    let text = "def half = Runtime.runtime.availableProcessors() / 2\ndependencies {\n    implementation 'org.yaml:snakeyaml:2.2'\n}\n";

    let deps = manifest::parse("/project/build.gradle", text, None);

    assert_eq!(deps.len(), 1);
    assert_eq!(deps[0].name, "org.yaml:snakeyaml");
    let regex = "def r = /a'b/\ndependencies {\n    implementation 'org.yaml:snakeyaml:2.2'\n}\n";
    assert!(manifest::parse("/project/build.gradle", regex, None).is_empty());
}

#[test]
fn saving_readme_keeps_manifest_diagnostics() {
    let host = MockHost::install().with_capabilities(CAPS);
    host.on_fetch(osv);
    host.add_file("web/package.json", PACKAGE);
    host.add_file("web/package-lock.json", PACKAGE_LOCK);
    let mut plugin = DependencyAudit::default();
    plugin.handle_events(&[Event::FileOpened("/project/web/package.json".into())], &host.invocation());
    let package = document("/project/web/package.json", "json", PACKAGE);
    assert_eq!(plugin.diagnostics(&package).len(), 1);

    plugin.handle_events(&[Event::FileSaved("/project/README.md".into())], &host.invocation());

    assert_eq!(plugin.diagnostics(&package).len(), 1);
}

#[test]
fn pep440_post_and_rc_order() {
    use std::cmp::Ordering::{Greater, Less};

    assert_eq!(osv::compare("1.0.post1", "1.0"), Greater);
    assert_eq!(osv::compare("1.10rc1", "1.9"), Greater);
    assert_eq!(osv::compare("1.0.post1", "1.0rc1"), Greater);
    assert_eq!(osv::compare("1.0.post1", "1.0.1"), Less);
    assert_eq!(osv::compare("1.0rc1", "1.0"), Less);
    assert_eq!(osv::compare("1.0rc10", "1.0rc9"), Greater);
}

#[test]
fn quick_fix_unsaved_says_save_first() {
    let host = MockHost::install().with_capabilities(CAPS);
    host.on_fetch(osv);
    host.add_file("pom.xml", POM);
    opened(&host, "/project/pom.xml");
    let unsaved = format!("{POM}<!-- unsaved -->");
    let doc = document("/project/pom.xml", "xml", &unsaved);
    let mut plugin = DependencyAudit::default();
    let action = plugin.code_actions(&doc, line(3)).into_iter().find(|a| a.title.starts_with("Upgrade ")).unwrap();

    let error = plugin.resolve_code_action(&doc, action).unwrap_err();

    assert!(matches!(&error, Error::Invalid(message) if message == "Save the file first"), "{error:?}");
}

#[test]
fn import_ignores_missing_profile_message() {
    let host = MockHost::install().with_capabilities(CAPS);
    let mut plugin = DependencyAudit::default();

    let error = plugin.run_command("import-ignores", &host.invocation(), None).unwrap_err();

    assert!(matches!(&error, Error::Invalid(message) if message == "No profile file; export one first"), "{error:?}");
}

#[test]
fn event_checks_query_only_the_first_uncached_coordinates() {
    let host = MockHost::install().with_capabilities(CAPS);
    host.on_fetch(osv);
    let deps: Vec<Dep> = (0..1500).map(|i| dep(Ecosystem::Npm, &format!("pkg{i}"), "1.0.0")).collect();
    let mut budget = EVENT_COORDINATES;

    osv::audit_within(&deps, true, 0, soon(), &mut budget).unwrap();

    assert_eq!(posts(&host), 1);
    assert_eq!(budget, 0);
}

#[test]
fn a_failed_automatic_resolution_is_reported_once() {
    let host = MockHost::install().with_capabilities(&["project.read", "project.write", "editor.read", "storage"]);
    host.set_setting("autoResolveBuilds", "true");
    host.add_file("pom.xml", "<project/>");
    host.answer_next_prompt(Some(&["Run"]));
    let mut plugin = DependencyAudit::default();

    plugin.auto_resolve(&host.invocation(), Instant::now() + Duration::from_secs(60));
    plugin.auto_resolve(&host.invocation(), Instant::now() + Duration::from_secs(60));

    let notifications = host.notifications();
    assert_eq!(notifications.len(), 1);
    assert!(notifications[0].body.starts_with("Automatic build resolution didn't run:"), "{}", notifications[0].body);
}

#[test]
fn automatic_resolution_stays_silent_without_a_maven_or_gradle_build() {
    let host = MockHost::install().with_capabilities(&["project.read", "project.write", "editor.read", "process:mvn", "storage"]);
    host.set_setting("autoResolveBuilds", "true");
    let mut plugin = DependencyAudit::default();

    plugin.auto_resolve(&host.invocation(), Instant::now() + Duration::from_secs(60));

    assert!(host.notifications().is_empty());
}

#[test]
fn auto_resolve_is_skipped_when_little_time_remains() {
    let host = MockHost::install().with_capabilities(&["project.read", "project.write", "editor.read", "process:mvn", "storage"]);
    host.add_file("pom.xml", "<project/> ");
    host.answer_next_prompt(Some(&[]));

    DependencyAudit::default().auto_resolve(&host.invocation(), Instant::now() + Duration::from_secs(5));

    assert!(host.process_runs().is_empty());
}
#[test]
fn scan_without_dependency_files_lists_the_supported_files() {
    let host = MockHost::install().with_capabilities(CAPS);
    let mut plugin = DependencyAudit::default();
    plugin.run_command("scan", &host.invocation(), None).unwrap();
    let body = &host.project_notifications()[0].1.notification.body;
    assert!(body.starts_with("No supported dependency files found. Supported: pom.xml, ") && body.contains("package.json"), "{body}");
    let view = plugin.render("vulnerabilities", None, &host.invocation()).unwrap();
    assert!(matches!(&view.body, Body::Message(message) if message.starts_with("No supported dependency files found")), "{view:?}");
}

#[test]
fn row_commands_from_the_palette_pick_a_dependency_and_preview_opens_its_declaration() {
    let host = MockHost::install().with_capabilities(&["project.read", "storage", "editor.read", "editor.write"]);
    let mut plugin = DependencyAudit::default();
    assert!(plugin.run_command("open-declaration", &host.invocation(), None).unwrap_err().to_string().contains("Scan Project"));
    let row = Row {
        path: "/project/package.json".into(), line: 0, name: "lodash".into(), version: "4.17.15".into(), fix: Some("4.17.21".into()),
        advisories: vec!["GHSA-35jh-r3h4-6jhm".into()], ecosystem: Some("npm".into()),
        sources: vec![Declaration { path: "/project/package.json".into(), line: 2, evidence: format!("{} package.json:3", binding::DECLARED) }],
        details: vec![],
    };
    let key = format!("dep:{}", actions::base(&row));
    host::storage::set("scan:/project", &serde_json::to_string(&Report { rows: vec![row], ..Report::default() }).unwrap()).unwrap();
    host.answer_next_prompt(Some(&[&key]));
    plugin.run_command("open-declaration", &host.invocation(), None).unwrap();
    assert_eq!(host.opened_locations(), [("/project/package.json".to_owned(), 2, 0)]);
    host.open_editor("/project/other.json", "json", "{}", &[]);
    let message = plugin.run_command("preview-upgrade", &host.invocation(), Some(&key)).unwrap().unwrap();
    assert!(message.contains("Preview upgrade again"), "{message}");
    assert_eq!(host.opened_locations().len(), 2);
}

#[test]
fn report_age_shows_a_readable_utc_time() {
    assert_eq!(resources::utc(0), "1970-01-01 00:00 UTC");
    assert_eq!(resources::utc(1_795_427_400), "2026-11-23 09:50 UTC");
    assert_eq!(resources::utc(951_782_400), "2000-02-29 00:00 UTC");
}

#[test]
fn scan_notes_files_and_decorates_declarations_then_rescan_clears_them() {
    let host = MockHost::install().with_capabilities(CAPS);
    let mut plugin = scanned(&host);
    let notes: Vec<_> = host.file_notes().into_iter().map(|((_, path), (note, _))| (path, note)).collect();
    assert_eq!(notes, [
        ("/project/pom.xml".to_string(), "1 vulnerability (1 high or critical)".to_string()),
        ("/project/web/package.json".to_string(), "1 vulnerability (1 high or critical)".to_string()),
    ]);
    let decorations = host.decorations();
    let lodash = decorations.iter().find(|((_, path), _)| path == "/project/web/package.json").unwrap().1;
    assert_eq!(lodash[0].after.as_deref(), Some("  GHSA-35jh-r3h4-6jhm · fixed in 4.17.21"));
    assert_eq!(lodash[0].range.start, lodash[0].range.end);
    host.add_file("pom.xml", "<project/>");
    host.add_file("web/package.json", "{}");
    host.add_file("web/package-lock.json", "{}");
    plugin.run_command("scan", &host.invocation(), None).unwrap();
    assert!(host.file_notes().is_empty());
    assert!(host.decorations().values().all(Vec::is_empty));
}
