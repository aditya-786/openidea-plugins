//! Explicit source-derived native Database setup. Never connects or executes SQL.
use super::*;
use openidea_plugin::PluginDataSource;
fn source(ctx: &Invocation) -> Result<(String, Document), Error> {
    let root = ctx.project().ok_or(Error::Stale)?.root()?;
    let editor = ctx
        .editor()
        .ok_or("Open a Spring Boot datasource configuration file first.")?;
    let current = editor.document().map_err(|error| ui_kit::denied::map(error, "Configure Database from Current Source", "editor.read", "Spring Boot Tools"))?;
    let doc = Document {
        path: editor.path(),
        language: editor.language(),
        text: current.text,
        version: current.version,
    };
    if source_scope(&doc.path).map(|(_, owner)| owner).as_deref() != Some(root.as_str()) {
        return Err(Error::Stale);
    }
    Ok((root, doc))
}
fn scalar(doc: &Document, keys: &[Key], name: &str) -> Result<Option<String>, Error> {
    let found: Vec<_> = keys.iter().filter(|k| k.key == name).collect();
    let key = match found.as_slice() {
        [] => return Ok(None),
        [key] => key,
        _ => {
            return Err(
                "Duplicate datasource keys cannot identify one source configuration.".into(),
            );
        }
    };
    let raw = doc
        .text
        .get(key.value_at..key.value_end)
        .ok_or(Error::Stale)?;
    if doc.language == "properties"
        && doc.text[key.value_end..]
            .split('\n')
            .next()
            .unwrap_or("")
            .trim_end_matches('\r')
            .bytes()
            .any(|b| matches!(b, b' ' | b'\t'))
    {
        return Err("Trailing datasource value whitespace requires manual setup to preserve credential meaning.".into());
    }
    if doc.language == "yaml"
        && (key.value.starts_with(['!', '[', '{'])
            || raw.contains("''")
            || (matches!(key.value.as_str(), "null" | "Null" | "NULL" | "~")
                && !doc.text[..key.value_at].ends_with(['\"', '\''])))
    {
        return Err(
            "Typed, tagged or encoded YAML datasource values require explicit plain strings."
                .into(),
        );
    }
    if key.value.len() > 4096
        || raw.contains('\\')
        || key.value.contains("${")
        || key.value.contains("#{")
        || key.value.chars().any(char::is_control)
        || key.value.starts_with(['&', '*', '|', '>'])
    {
        return Err("Datasource values must be plain static scalar values; resolve placeholders and encoded/aliased values first.".into());
    }
    if doc.language == "properties" && doc.text.get(key.at..key.end) != Some(name) {
        return Err("Escaped datasource key spellings are outside source setup.".into());
    }
    Ok(Some(key.value.clone()))
}
fn desired(root: &str, doc: &Document) -> Result<(PluginDataSource, Option<String>), Error> {
    if doc.text.len() > 128 * 1024
        || !is_config_file(&doc.path)
        || !matches!(doc.language.as_str(), "properties" | "yaml")
        || doc
            .text
            .lines()
            .any(|l| matches!(l.trim(), "---" | "..." | "#---" | "!---"))
        || doc
            .text
            .as_bytes()
            .iter()
            .enumerate()
            .any(|(i, b)| *b == b'\r' && doc.text.as_bytes().get(i + 1) != Some(&b'\n'))
    {
        return Err("Use one bounded Spring Boot properties or YAML document with explicit static datasource fields.".into());
    }
    let keys = config::keys(&doc.path, &doc.text);
    if keys.len() > 1024 {
        return Err("Configuration exceeds the source setup key limit.".into());
    }
    let url = scalar(doc, &keys, "spring.datasource.url")?
        .ok_or("This source file has no explicit spring.datasource.url.")?;
    let username = scalar(doc, &keys, "spring.datasource.username")?;
    let password = scalar(doc, &keys, "spring.datasource.password")?;
    let sql_server = if let Some(rest) = url.strip_prefix("jdbc:sqlserver://") {
        let mut parts = rest.split(';');
        let host = parts.next().unwrap_or("");
        let mut database = None;
        for field in parts.filter(|field| !field.is_empty()) {
            let (key, value) = field.split_once('=').ok_or_else(|| {
                Error::Invalid(
                    "SQL Server JDBC properties require explicit key/value fields.".into(),
                )
            })?;
            if !key.eq_ignore_ascii_case("databaseName") || database.is_some() {
                return Err(Error::Invalid("SQL Server target setup currently admits one databaseName property; transport/authentication/instance properties require manual Database setup.".into()));
            }
            database = Some(value);
        }
        Some(format!(
            "{host}/{}",
            database.ok_or_else(|| Error::Invalid(
                "SQL Server target setup requires an explicit databaseName.".into()
            ))?
        ))
    } else {
        None
    };
    let (driver, rest, default) = if let Some(rest) = url.strip_prefix("jdbc:postgresql://") {
        ("postgres", rest, 5432)
    } else if let Some(rest) = url.strip_prefix("jdbc:mysql://") {
        ("mysql", rest, 3306)
    } else if let Some(rest) = url.strip_prefix("jdbc:mariadb://") {
        ("mysql", rest, 3306)
    } else if let Some(rest) = url
        .strip_prefix("jdbc:clickhouse://")
        .or_else(|| url.strip_prefix("jdbc:ch://"))
        .or_else(|| url.strip_prefix("jdbc:clickhouse:http://"))
        .or_else(|| url.strip_prefix("jdbc:ch:http://"))
    {
        ("clickhouse", rest, 8123)
    } else if let Some(rest) = url.strip_prefix("jdbc:oracle:thin:@//") {
        ("oracle", rest, 1521)
    } else if let Some(rest) = sql_server.as_deref() {
        ("sqlServer", rest, 1433)
    } else {
        return Err("Source setup supports PostgreSQL, MySQL/MariaDB, ClickHouse HTTP, Oracle thin service and SQL Server databaseName JDBC forms.".into());
    };
    if rest.contains(['?', '#', '@', '%', '\\']) {
        return Err("URL parameters, embedded credentials and encoded/multiple targets need manual Database setup.".into());
    }
    let (authority, database) = rest
        .split_once('/')
        .ok_or("The JDBC URL must name a database.")?;
    if database.is_empty()
        || database.len() > 256
        || !database
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b))
    {
        return Err(
            "The source database name is outside the supported simple JDBC URL form.".into(),
        );
    }
    let (hostname, port) = if authority.starts_with('[') {
        let end = authority
            .find(']')
            .ok_or("Malformed bracketed JDBC host.")?;
        let host = &authority[1..end];
        if host.parse::<std::net::Ipv6Addr>().is_err() {
            return Err("Invalid IPv6 JDBC host.".into());
        }
        let tail = &authority[end + 1..];
        let port = if tail.is_empty() {
            default
        } else {
            tail.strip_prefix(':')
                .ok_or("Invalid JDBC authority.")?
                .parse::<u16>()
                .map_err(|_| Error::Invalid("Invalid JDBC port.".into()))?
        };
        (host, port)
    } else {
        let (host, port) = match authority.split_once(':') {
            Some((host, port)) => (
                host,
                port.parse::<u16>()
                    .map_err(|_| Error::Invalid("Invalid JDBC port.".into()))?,
            ),
            None => (authority, default),
        };
        if host.is_empty()
            || host.len() > 253
            || !host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".-".contains(&b))
        {
            return Err("Invalid simple JDBC host.".into());
        }
        (host, port)
    };
    if port == 0 {
        return Err("Database port must be positive.".into());
    }
    // Credentials never transfer to a different reviewed target. Password
    // rotation on the same source/target retains the owned source identity.
    use sha2::{Digest, Sha256};
    let identity =
        serde_json::to_vec(&(root, &doc.path, driver, hostname, port, database, &username))
            .map_err(|_| Error::Invalid("Cannot encode source target identity.".into()))?;
    let hash = format!("{:x}", Sha256::digest(identity));
    let hostname = if driver == "oracle" && hostname.contains(':') {
        format!("[{hostname}]")
    } else {
        hostname.to_owned()
    };
    let relative = doc
        .path
        .strip_prefix(&format!("{root}/"))
        .unwrap_or(&doc.path);
    let mut name = format!("Spring Boot source: {relative}");
    if name.len() > 256 {
        let mut end = 253;
        while !name.is_char_boundary(end) {
            end -= 1;
        }
        name.truncate(end);
        name.push_str("…");
    }
    Ok((
        PluginDataSource {
            id: format!("aritti.spring-boot.{hash}"),
            name,
            driver: driver.into(),
            host: Some(hostname),
            port: Some(port),
            database: Some(database.into()),
            user: username,
            ..Default::default()
        },
        password,
    ))
}
pub(crate) fn configure(ctx: &Invocation) -> Result<Option<String>, Error> {
    let (root, doc) = source(ctx)?;
    let (desired, password) = desired(&root, &doc)?;
    let project = ctx.project().ok_or(Error::Stale)?;
    let denied = |error| ui_kit::denied::map(error, "Configure Database from Current Source", "database.sources", "Spring Boot Tools");
    let ids = project.data_sources().map_err(denied)?;
    if !host::confirm(
        "Configure Database from this source file?",
        &format!(
            "{} the Spring Boot database connection from {}\nDriver: {}\nHost: {}\nPort: {}\nDatabase: {}\nUser: {}\nCredential: {}\n\nUses only the values in this file; other profiles and imported files are ignored. Nothing connects yet.",
            if ids.contains(&desired.id) {
                "Update"
            } else {
                "Create"
            },
            doc.path,
            desired.driver,
            desired.host.as_deref().unwrap_or(""),
            desired.port.unwrap_or(0),
            desired.database.as_deref().unwrap_or(""),
            desired.user.as_deref().unwrap_or("(driver default)"),
            if password.is_some() {
                "store source password securely"
            } else {
                "leave existing credential unchanged"
            }
        ),
    )? {
        return Err(Error::Cancelled);
    }
    let (current_root, current) = source(ctx)?;
    if current_root != root || current != doc {
        return Err(Error::Stale);
    }
    project.upsert_data_source(&desired).map_err(denied)?;
    if let Some(password) = password {
        project.set_data_source_credential(&desired.id, &password).map_err(denied)?;
    }
    Ok(Some("Database connection saved from this file. Open the Database window to connect.".into()))
}
#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    const SOURCE: &str = "spring.datasource.url=jdbc:postgresql://localhost:5544/shop\nspring.datasource.username=alice\nspring.datasource.password=private-secret\n";
    fn doc(text: &str) -> Document {
        Document {
            path: "/project/application.properties".into(),
            language: "properties".into(),
            text: text.into(),
            version: 1,
        }
    }
    #[test]
    fn source_database_descriptor_preserves_reviewed_target_and_excludes_password() {
        let (source, password) = desired("/project", &doc(SOURCE)).unwrap();
        assert_eq!(source.driver, "postgres");
        assert_eq!(source.host.as_deref(), Some("localhost"));
        assert_eq!(source.port, Some(5544));
        assert_eq!(source.database.as_deref(), Some("shop"));
        assert_eq!(source.user.as_deref(), Some("alice"));
        assert_eq!(password.as_deref(), Some("private-secret"));
        assert!(
            !serde_json::to_string(&source)
                .unwrap()
                .contains("private-secret")
        );
        let id = source.id;
        assert_ne!(
            desired("/project", &doc(&SOURCE.replace("5544", "5545")))
                .unwrap()
                .0
                .id,
            id
        );
        assert_eq!(
            desired(
                "/project",
                &doc(&SOURCE.replace("private-secret", "rotated-secret"))
            )
            .unwrap()
            .0
            .id,
            id
        );
        assert_ne!(
            desired("/project", &doc(&SOURCE.replace("alice", "bob")))
                .unwrap()
                .0
                .id,
            id
        );
        assert_ne!(desired("/other", &doc(SOURCE)).unwrap().0.id, id);
    }
    #[test]
    fn yaml_and_ipv6_source_targets_use_explicit_default_ports() {
        let source = "spring:\n  datasource:\n    url: 'jdbc:mysql://[::1]/shop'\n    username: alice\n    password: 'private-secret'\n";
        let mut d = doc(source);
        d.language = "yaml".into();
        d.path = "/project/application-dev.yml".into();
        let (desired, password) = desired("/project", &d).unwrap();
        assert_eq!(desired.host.as_deref(), Some("::1"));
        assert_eq!(desired.port, Some(3306));
        assert_eq!(password.as_deref(), Some("private-secret"));
    }
    #[test]
    fn ambiguous_dynamic_parameterized_and_encoded_values_do_not_create_targets() {
        for source in [
            SOURCE.replace("localhost:5544", "localhost:0"),
            SOURCE.replace("localhost:5544", "user:pass@localhost"),
            SOURCE.replace("/shop", "/shop?password=secret"),
            SOURCE.replace("private-secret", "${DB_PASSWORD}"),
            SOURCE.replace("private-secret", "private-secret "),
            format!("{SOURCE}spring.datasource.url=jdbc:mysql://localhost/other\n"),
            SOURCE.replace("private-secret", r"escaped\nsecret"),
            SOURCE.replace("jdbc:postgresql", "jdbc:h2"),
        ] {
            assert!(desired("/project", &doc(&source)).is_err(), "{source}");
        }
    }
    #[test]
    fn extended_native_driver_forms_preserve_target_and_refuse_transport_options() {
        for (url, driver, host, port) in [
            (
                "jdbc:clickhouse://localhost/shop",
                "clickhouse",
                "localhost",
                8123,
            ),
            ("jdbc:ch:http://[::1]/shop", "clickhouse", "::1", 8123),
            ("jdbc:oracle:thin:@//[::1]/shop", "oracle", "[::1]", 1521),
            (
                "jdbc:sqlserver://localhost;databaseName=shop",
                "sqlServer",
                "localhost",
                1433,
            ),
        ] {
            let source = desired("/project", &doc(&format!("spring.datasource.url={url}\n")))
                .unwrap()
                .0;
            assert_eq!(source.driver, driver);
            assert_eq!(source.host.as_deref(), Some(host));
            assert_eq!(source.port, Some(port));
            assert_eq!(source.database.as_deref(), Some("shop"));
        }
        for url in [
            "jdbc:sqlserver://localhost;databaseName=shop;encrypt=true",
            "jdbc:sqlserver://localhost;databaseName=shop;databaseName=other",
            "jdbc:clickhouse:https://localhost/shop",
            "jdbc:oracle:thin:@localhost:1521:shop",
        ] {
            assert!(desired("/project", &doc(&format!("spring.datasource.url={url}\n"))).is_err());
        }
        let mut source = doc(SOURCE);
        source.path = format!("/project/{}/application.properties", "é".repeat(200));
        let target = desired("/project", &source).unwrap().0;
        assert!(target.name.len() <= 256 && target.name.ends_with('…'));
    }
    #[test]
    fn registered_source_database_setup_is_confirmed_owned_and_cancel_safe() {
        let host = MockHost::install().with_capabilities(&[
            "project.read",
            "editor.read",
            "database.sources",
        ]);
        host.open_editor(
            "/project/application.properties",
            "properties",
            SOURCE,
            &[(0, 0, 0, 0)],
        );
        let project = host.invocation().project().unwrap();
        host.answer_next_prompt(None);
        assert!(
            BootTools::default()
                .run_command("configure-source-database", &host.invocation(), None)
                .is_err()
        );
        assert!(project.data_sources().unwrap().is_empty());
        host.answer_next_prompt(Some(&[]));
        let result = BootTools::default()
            .run_command("configure-source-database", &host.invocation(), None)
            .unwrap()
            .unwrap();
        assert!(!result.contains("private-secret"));
        assert_eq!(project.data_sources().unwrap().len(), 1);
        assert!(host.requests().is_empty() && host.process_runs().is_empty());
    }
    #[test]
    fn denied_database_sources_names_the_permission() {
        let host = MockHost::install().with_capabilities(&["project.read", "editor.read"]);
        host.open_editor("/project/application.properties", "properties", SOURCE, &[(0, 0, 0, 0)]);
        let error = BootTools::default().run_command("configure-source-database", &host.invocation(), None).unwrap_err().to_string();
        assert!(error.contains("Configure Database from Current Source needs the database.sources permission. Allow it in Settings › Plugins › Spring Boot Tools."), "{error}");
    }
}
