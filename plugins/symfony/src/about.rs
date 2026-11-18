//! Application metadata from `composer.json` / `composer.lock` and the cached index. Never runs PHP.
use crate::index::Index;
use openidea_plugin::views::Body;
use openidea_plugin::Project;
use serde_json::Value;

const LOCK_LIMIT: usize = 4 << 20;
const COMPOSER_LIMIT: usize = 128 << 10;
const ENV_FILES: [&str; 5] = [".env", ".env.local", ".env.dev", ".env.test", ".env.prod"];
/// Packages whose installed version is worth showing.
const PACKAGES: [(&str, &str); 17] = [
    ("symfony/framework-bundle", "FrameworkBundle"),
    ("symfony/console", "Console"),
    ("symfony/twig-bundle", "Twig bundle"),
    ("twig/twig", "Twig"),
    ("doctrine/orm", "Doctrine ORM"),
    ("doctrine/doctrine-bundle", "Doctrine bundle"),
    ("symfony/security-bundle", "Security"),
    ("symfony/messenger", "Messenger"),
    ("symfony/mailer", "Mailer"),
    ("symfony/stimulus-bundle", "Stimulus bundle"),
    ("symfony/ux-twig-component", "UX Twig component"),
    ("symfony/ux-live-component", "UX Live component"),
    ("symfony/ux-turbo", "UX Turbo"),
    ("symfony/asset-mapper", "AssetMapper"),
    ("symfony/webpack-encore-bundle", "Webpack Encore"),
    ("api-platform/core", "API Platform"),
    ("easycorp/easyadmin-bundle", "EasyAdmin"),
];

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}

fn read(project: &Project, name: &str, limit: usize) -> Option<Value> {
    let text = project.read_file(name).ok()?;
    crate::strict_json::parse(&text, limit).ok()
}

fn text(value: &Value) -> Option<String> {
    value.as_str().filter(|text| text.len() <= 4096 && !text.chars().any(|c| c.is_control() && c != '\n')).map(str::to_owned)
}

fn group(title: &str, entries: Vec<(String, String)>) -> String {
    if entries.is_empty() {
        return String::new();
    }
    let fields: String = entries.into_iter().map(|(key, value)| format!("<dt>{}</dt><dd>{}</dd>", escape(&key), escape(&value))).collect();
    format!("<section class=\"about-section\"><h3 class=\"ui-group-header\">{}</h3><dl>{fields}</dl></section>", escape(title))
}

fn pair(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_owned(), value.into())
}

pub(crate) fn body(project: &Project, index: Option<&Index>) -> Body {
    let root = project.root().unwrap_or_default();
    let composer = read(project, "composer.json", COMPOSER_LIMIT).unwrap_or(Value::Null);
    let lock = read(project, "composer.lock", LOCK_LIMIT).unwrap_or(Value::Null);
    let installed = |name: &str| -> Option<String> {
        let found: Vec<_> = lock["packages"].as_array()?.iter().filter(|entry| entry["name"] == name).collect();
        match found.as_slice() {
            [entry] => text(&entry["version"]),
            _ => None,
        }
    };

    let title = text(&composer["name"]).unwrap_or_else(|| "Symfony application".to_owned());
    let description = text(&composer["description"]).unwrap_or_default();

    let mut application = vec![pair("Project", root)];
    application.extend(text(&composer["name"]).map(|name| pair("Package", name)));
    application.extend(text(&composer["version"]).map(|version| pair("Application version", version)));
    let mut sections = group("Application", application);

    let mut symfony = Vec::new();
    symfony.extend(installed("symfony/framework-bundle").map(|version| pair("Installed", version)));
    for name in ["symfony/framework-bundle", "symfony/symfony"] {
        if let Some(constraint) = text(&composer["require"][name]) {
            symfony.push(pair("Requirement", constraint));
            break;
        }
    }
    symfony.extend(text(&composer["require"]["php"]).map(|constraint| pair("PHP requirement", constraint)));
    if let Some(index) = index {
        symfony.push(pair("Compiled container", if index.compiled_container { "found" } else { "not found (run Warm Up Cache)" }));
        symfony.push(pair("Compiled routes", if index.compiled_routes { "found" } else { "not found" }));
    }
    sections.push_str(&group("Symfony", symfony));

    let versions = PACKAGES.iter().filter_map(|(package, label)| installed(package).map(|version| pair(label, version))).collect();
    sections.push_str(&group("Dependencies (lockfile)", versions));

    if let Some(index) = index {
        let counts = vec![
            pair("Routes", index.route_defs.len().max(index.routes.len()).to_string()),
            pair("Services", index.services.len().to_string()),
            pair("Templates", index.templates.len().to_string()),
            pair("Entities", index.entities.len().to_string()),
            pair("Console commands", index.commands.len().to_string()),
            pair("UX components", index.components.len().to_string()),
            pair("Stimulus controllers", index.stimulus.len().to_string()),
            pair("Parameters", index.parameters.len().to_string()),
        ];
        sections.push_str(&group("Source index", counts));
    }

    let files: Vec<String> = ENV_FILES.iter().filter(|file| project.exists(file).unwrap_or(false)).map(|file| (*file).to_owned()).collect();
    if !files.is_empty() {
        sections.push_str(&group("Environment files", vec![pair("Found", files.join(", ")), pair("Values", "never read")]));
    }

    Body::Html(format!(
        r#"<link rel="stylesheet" href="./components.css"><style>[data-symfony-about]{{padding:var(--ui-space-3);color:var(--ui-fg);min-width:0}}[data-symfony-about] h2{{font-size:18px;margin:0 0 var(--ui-space-2)}}.about-description,.about-note{{color:var(--ui-muted);overflow-wrap:anywhere}}.about-grid{{display:grid;grid-template-columns:repeat(auto-fit,minmax(min(100%,300px),1fr));gap:var(--ui-space-4)}}.about-section{{min-width:0}}.about-section h3{{font-size:var(--ui-text);font-weight:500;margin:var(--ui-space-3) 0}}.about-section dl{{display:grid;grid-template-columns:minmax(90px,1fr) minmax(0,2fr);gap:var(--ui-space-2) var(--ui-space-3);margin:0}}.about-section dt{{color:var(--ui-muted)}}.about-section dd{{margin:0;overflow-wrap:anywhere;white-space:pre-wrap}}</style><section data-symfony-about aria-label="Application information"><h2>{}</h2><p class="about-description">{}</p><div class="about-grid">{sections}</div><p class="about-note">Read from composer.json, composer.lock and the last project scan; no PHP runs. Use "Run bin/console about" for runtime details.</p></section>"#,
        escape(&title),
        escape(&description)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::host;
    use openidea_plugin::mock::MockHost;

    fn html(body: Body) -> String {
        let Body::Html(html) = body else { panic!("not html") };
        html
    }

    #[test]
    fn about_shows_project_versions_and_env_file_names_without_values() {
        let host = MockHost::install();
        host.add_file("composer.json", r#"{"name":"acme/blog","description":"A <b>blog</b>","require":{"php":">=8.2","symfony/framework-bundle":"^7.1"}}"#);
        host.add_file("composer.lock", r#"{"packages":[{"name":"symfony/framework-bundle","version":"v7.1.5"},{"name":"twig/twig","version":"v3.14.0"},{"name":"unknown/pkg","version":"1.0"}]}"#);
        host.add_file(".env", "APP_SECRET=hunter2\n");
        let project = host::projects().unwrap().remove(0);
        let page = html(body(&project, None));
        for expected in ["acme/blog", "v7.1.5", "v3.14.0", "^7.1", "&gt;=8.2", "FrameworkBundle", ".env"] {
            assert!(page.contains(expected), "{expected} in {page}");
        }
        assert!(page.contains("A &lt;b&gt;blog&lt;/b&gt;") && !page.contains("hunter2") && !page.contains("unknown/pkg"));
    }

    #[test]
    fn missing_or_malformed_metadata_still_renders() {
        let host = MockHost::install();
        host.add_file("composer.lock", "{not json");
        let project = host::projects().unwrap().remove(0);
        assert!(html(body(&project, None)).contains("Symfony application"));
    }
}
