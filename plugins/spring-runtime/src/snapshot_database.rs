//! Explicit native connection setup from one captured property source, not a
//! debugger datasource object or a claim about the currently active database.
use super::*;
use openidea_plugin::{PickItem, PluginDataSource};
use sha2::{Digest, Sha256};
struct Candidate {
    source: String,
    target: PluginDataSource,
    password: Option<String>,
}
fn scalar<'a>(source: &'a Source, key: &str) -> Result<Option<&'a str>, Error> {
    let values = source
        .props
        .iter()
        .filter(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
        .collect::<Vec<_>>();
    match values.as_slice() {
        [] => Ok(None),
        [value]
            if value.len() <= 4096
                && !value.chars().any(char::is_control)
                && !value.contains("${")
                && !value.contains("#{") =>
        {
            Ok(Some(value))
        }
        _ => Err(Error::Invalid(
            "Captured datasource fields are ambiguous or unsupported.".into(),
        )),
    }
}
fn masked(value: &str) -> bool {
    value == MASK || (!value.is_empty() && value.chars().all(|c| c == '*'))
}
fn target(scope: &Scope, source: &Source) -> Result<Option<Candidate>, Error> {
    if source.name.is_empty()
        || source.name.len() > 1024
        || source.name.chars().any(char::is_control)
        || source.props.len() > 4096
    {
        return Err(Error::Invalid(
            "Captured property source is outside the connection setup limits.".into(),
        ));
    }
    let Some(url) = scalar(source, "spring.datasource.url")?.filter(|value| !masked(value)) else {
        return Ok(None);
    };
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
        return Ok(None);
    };
    if rest.contains(['?', '#', '@', '%', '\\']) {
        return Err(Error::Invalid(
            "Encoded, parameterized or credential-bearing JDBC URLs require manual Database setup."
                .into(),
        ));
    }
    let (authority, database) = rest
        .split_once('/')
        .ok_or_else(|| Error::Invalid("Captured JDBC URL must name one database.".into()))?;
    if database.is_empty()
        || database.len() > 256
        || !database
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_-.".contains(&c))
    {
        return Err(Error::Invalid(
            "Captured JDBC database name is not a supported simple name.".into(),
        ));
    }
    let (hostname, port) = if let Some(rest) = authority.strip_prefix('[') {
        let (hostname, tail) = rest
            .split_once(']')
            .ok_or_else(|| Error::Invalid("Invalid captured IPv6 JDBC host.".into()))?;
        hostname
            .parse::<std::net::Ipv6Addr>()
            .map_err(|_| Error::Invalid("Invalid captured IPv6 JDBC host.".into()))?;
        let port = if tail.is_empty() {
            default
        } else {
            tail.strip_prefix(':')
                .ok_or_else(|| Error::Invalid("Invalid captured JDBC authority.".into()))?
                .parse::<u16>()
                .map_err(|_| Error::Invalid("Invalid captured JDBC port.".into()))?
        };
        (hostname, port)
    } else {
        let (hostname, port) = match authority.split_once(':') {
            Some((hostname, port)) => (
                hostname,
                port.parse::<u16>()
                    .map_err(|_| Error::Invalid("Invalid captured JDBC port.".into()))?,
            ),
            None => (authority, default),
        };
        if hostname.is_empty()
            || hostname.len() > 253
            || !hostname
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b".-".contains(&c))
        {
            return Err(Error::Invalid("Invalid captured JDBC hostname.".into()));
        }
        (hostname, port)
    };
    if port == 0 {
        return Err(Error::Invalid(
            "Captured JDBC port must be positive.".into(),
        ));
    }
    let user = scalar(source, "spring.datasource.username")?
        .filter(|value| !masked(value) && !value.is_empty())
        .map(str::to_owned);
    let password = scalar(source, "spring.datasource.password")?
        .filter(|value| !masked(value) && !value.is_empty())
        .map(str::to_owned);
    let identity =
        serde_json::to_string(&(scope, &source.name, driver, hostname, port, database, &user))
            .map_err(|_| Error::Invalid("Invalid captured connection identity.".into()))?;
    let hash = format!("{:x}", Sha256::digest(identity.as_bytes()));
    Ok(Some(Candidate {
        source: source.name.clone(),
        target: PluginDataSource {
            id: format!("aritti.spring-runtime.snapshot.{hash}"),
            name: format!("Spring snapshot: {}", presentation(&source.name, 220)),
            driver: driver.into(),
            host: Some(if driver == "oracle" && hostname.contains(':') {
                format!("[{hostname}]")
            } else {
                hostname.into()
            }),
            port: Some(port),
            database: Some(database.into()),
            user,
            ..Default::default()
        },
        password,
    }))
}
fn candidates(scope: &Scope, snapshot: &Snapshot) -> Result<Vec<Candidate>, Error> {
    if !snapshot.has("env") || snapshot.url != scope.url || snapshot.sources.len() > 256 {
        return Err(Error::Invalid(
            "A matching captured environment snapshot is required.".into(),
        ));
    }
    let mut output = Vec::new();
    let mut names = BTreeSet::new();
    for (index, source) in snapshot.sources.iter().enumerate() {
        if let Some(mut candidate) = target(scope, source)? {
            if candidate.password.is_none() {
                if let Some((owner, password)) = &snapshot.datasource_password {
                    if *owner == index {
                        candidate.password = Some(password.clone());
                    }
                }
            }
            if !names.insert(source.name.clone()) {
                return Err(Error::Invalid("Duplicate captured property-source names cannot identify one owned connection.".into()));
            }
            output.push(candidate);
        }
    }
    if output.is_empty() {
        return Err(Error::NotFound("No unambiguous unmasked supported JDBC target fields were captured in one property source. Use manual Database setup for other or unresolved targets.".into()));
    }
    Ok(output)
}
impl Runtime {
    pub(crate) fn configure_snapshot_database(
        &mut self,
        ctx: &Invocation,
    ) -> Result<Option<String>, Error> {
        let project = ctx
            .project()
            .ok_or_else(|| Error::Failed("Open the snapshot project first.".into()))?;
        let scope = Scope::current(&project)?;
        self.load_scope(scope.clone());
        let snapshot = self.snapshot.clone().ok_or_else(|| {
            Error::Failed("Refresh an Actuator environment snapshot first.".into())
        })?;
        let choices = candidates(&scope, &snapshot)?;
        let index = if choices.len() == 1 {
            0
        } else {
            let items = choices
                .iter()
                .enumerate()
                .map(|(index, c)| {
                    PickItem::new(
                        &format!("source:{index}"),
                        &format!(
                            "{} · {} / {}",
                            c.source,
                            c.target.host.as_deref().unwrap_or(""),
                            c.target.database.as_deref().unwrap_or("")
                        ),
                    )
                })
                .collect::<Vec<_>>();
            let Some(key) = host::pick("Choose captured datasource property source", &items)?
            else {
                return Ok(None);
            };
            key.strip_prefix("source:")
                .and_then(|key| key.parse::<usize>().ok())
                .filter(|index| *index < choices.len())
                .ok_or_else(|| Error::Invalid("Unknown captured datasource choice.".into()))?
        };
        if Scope::current(&project)? != scope
            || self.scope.as_ref() != Some(&scope)
            || self.snapshot.as_ref() != Some(&snapshot)
        {
            return Err(Error::Stale);
        }
        let candidate = &choices[index];
        let ids = allow(project.data_sources(), "database.sources")?;
        let message = format!(
            "{} an owned Database connection from this captured property source?\n\nProject: {}\nActuator: {}\nFetched Unix: {}\nProperty source: {}\nDriver: {}\nHost: {}\nPort: {}\nDatabase: {}\nUser: {}\nCredential: {}\n\nOnly exact fields from this source are used. Active runtime database identity, profile precedence and current values are unverified. Native driver defaults apply; review SSL/TLS and other transport settings in Database before connecting. No connection, schema read or SQL will run.",
            if ids.contains(&candidate.target.id) {
                "Update"
            } else {
                "Create"
            },
            scope.root,
            scope.url,
            snapshot.fetched_at,
            candidate.source,
            candidate.target.driver,
            candidate.target.host.as_deref().unwrap_or(""),
            candidate.target.port.unwrap_or(0),
            candidate.target.database.as_deref().unwrap_or(""),
            candidate
                .target
                .user
                .as_deref()
                .unwrap_or("driver default; no explicit captured user"),
            if candidate.password.is_some() {
                "store captured nonempty password securely (value hidden)"
            } else {
                "leave existing credential unchanged; no unmasked nonempty password captured"
            }
        );
        if !host::confirm("Configure Database from Runtime Snapshot", &message)? {
            return Ok(None);
        }
        if ctx.project().and_then(|p| p.root().ok()).as_deref() != Some(scope.root.as_str())
            || Scope::current(&project)? != scope
            || self.scope.as_ref() != Some(&scope)
            || self.snapshot.as_ref() != Some(&snapshot)
        {
            return Err(Error::Stale);
        }
        project.upsert_data_source(&candidate.target)?;
        if let Some(password) = &candidate.password {
            project.set_data_source_credential(&candidate.target.id, password)?;
        }
        Ok(Some("Owned Database connection configured from the reviewed captured property source. Open Database to connect explicitly; no connection or SQL was executed.".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    fn source(url: &str) -> Source {
        Source {
            name: "application.properties".into(),
            props: vec![
                ("spring.datasource.url".into(), url.into()),
                ("spring.datasource.username".into(), "alice".into()),
                (
                    "spring.datasource.password".into(),
                    "private-password".into(),
                ),
            ],
        }
    }
    fn setup() -> (MockHost, Runtime) {
        let host = MockHost::install();
        let scope = Scope::current(&host.invocation().project().unwrap()).unwrap();
        let snapshot = Snapshot {
            url: scope.url.clone(),
            fetched_at: 123,
            sources: vec![source("jdbc:postgresql://localhost:5544/shop")],
            ..Snapshot::default()
        };
        let runtime = Runtime {
            scope: Some(scope),
            snapshot: Some(snapshot),
            loaded: true,
            ..Runtime::default()
        };
        (host, runtime)
    }
    #[test]
    fn descriptor_uses_exact_captured_target_and_keeps_password_out_of_native_descriptor() {
        let (host, runtime) = setup();
        let scope = runtime.scope.unwrap();
        let snapshot = runtime.snapshot.unwrap();
        let choices = candidates(&scope, &snapshot).unwrap();
        let candidate = &choices[0];
        assert_eq!(candidate.target.driver, "postgres");
        assert_eq!(candidate.target.host.as_deref(), Some("localhost"));
        assert_eq!(candidate.target.port, Some(5544));
        assert_eq!(candidate.target.database.as_deref(), Some("shop"));
        assert_eq!(candidate.target.user.as_deref(), Some("alice"));
        assert!(!format!("{:?}", candidate.target).contains("private-password"));
        assert_eq!(host.fetches(), 0);
        let moved = target(&scope, &source("jdbc:postgresql://other:5544/shop"))
            .unwrap()
            .unwrap();
        assert_ne!(
            candidate.target.id, moved.target.id,
            "a changed target must not inherit an old owned credential"
        );
    }
    #[test]
    fn candidate_password_comes_from_memory_when_source_value_is_masked() {
        let (_host, runtime) = setup();
        let scope = runtime.scope.unwrap();
        let mut snapshot = runtime.snapshot.unwrap();
        snapshot.sources[0].props[2].1 = MASK.into();
        snapshot.datasource_password = Some((0, "s3cret".into()));
        let choices = candidates(&scope, &snapshot).unwrap();
        assert_eq!(choices[0].password.as_deref(), Some("s3cret"));
    }
    #[test]
    fn memory_password_attaches_only_to_the_source_it_came_from() {
        let (_host, runtime) = setup();
        let scope = runtime.scope.unwrap();
        let mut snapshot = runtime.snapshot.unwrap();
        snapshot.sources[0].props[2].1 = MASK.into();
        snapshot.datasource_password = Some((1, "s3cret".into()));
        let choices = candidates(&scope, &snapshot).unwrap();
        assert_eq!(choices[0].password, None);
    }
    #[test]
    fn explicit_confirmation_creates_owned_source_and_cancel_preserves_database() {
        let (host, mut runtime) = setup();
        let project = host.invocation().project().unwrap();
        host.answer_next_prompt(None);
        runtime
            .configure_snapshot_database(&host.invocation())
            .unwrap();
        assert!(project.data_sources().unwrap().is_empty());
        host.answer_next_prompt(Some(&["yes"]));
        runtime
            .run_command("configure-snapshot-database", &host.invocation(), None)
            .unwrap();
        assert_eq!(project.data_sources().unwrap().len(), 1);
        assert_eq!(host.fetches(), 0);
        assert!(host.requests().is_empty());
    }
    #[test]
    fn masked_ambiguous_parameterized_and_embedded_credentials_are_not_targets() {
        let (host, runtime) = setup();
        let scope = runtime.scope.unwrap();
        for url in [
            MASK,
            "jdbc:postgresql://user:pass@localhost/db",
            "jdbc:mysql://localhost/db?password=private",
            "jdbc:postgresql://localhost/db%2fname",
            "jdbc:mysql://localhost:0/db",
        ] {
            assert!(
                target(&scope, &source(url)).is_err()
                    || target(&scope, &source(url)).unwrap().is_none()
            );
        }
        let mut captured = source("jdbc:postgresql://localhost/db");
        captured.props.push((
            "spring.datasource.url".into(),
            "jdbc:postgresql://other/db".into(),
        ));
        assert!(target(&scope, &captured).is_err());
        assert_eq!(host.fetches(), 0);
    }
    #[test]
    fn known_ipv6_and_mariadb_targets_keep_ports_and_masked_passwords_are_not_copied() {
        let (_host, runtime) = setup();
        let scope = runtime.scope.unwrap();
        let mut captured = source("jdbc:mariadb://[::1]:3308/shop");
        captured.props[2].1 = MASK.into();
        let target = target(&scope, &captured).unwrap().unwrap();
        assert_eq!(target.target.host.as_deref(), Some("::1"));
        assert_eq!(target.target.port, Some(3308));
        assert_eq!(target.target.driver, "mysql");
        assert!(target.password.is_none());
    }
    #[test]
    fn native_driver_target_forms_are_exact_and_extended_transport_properties_are_not_dropped() {
        let (_host, runtime) = setup();
        let scope = runtime.scope.unwrap();
        for (url, driver, host, port, database) in [
            (
                "jdbc:clickhouse://db.local/analytics",
                "clickhouse",
                "db.local",
                8123,
                "analytics",
            ),
            (
                "jdbc:oracle:thin:@//oracle.local:1522/service.net",
                "oracle",
                "oracle.local",
                1522,
                "service.net",
            ),
            (
                "jdbc:oracle:thin:@//[::1]/service",
                "oracle",
                "[::1]",
                1521,
                "service",
            ),
            (
                "jdbc:sqlserver://sql.local:1444;databaseName=shop",
                "sqlServer",
                "sql.local",
                1444,
                "shop",
            ),
        ] {
            let target = target(&scope, &source(url)).unwrap().unwrap();
            assert_eq!(target.target.driver, driver);
            assert_eq!(target.target.host.as_deref(), Some(host));
            assert_eq!(target.target.port, Some(port));
            assert_eq!(target.target.database.as_deref(), Some(database));
        }
        assert!(
            target(
                &scope,
                &source("jdbc:sqlserver://sql.local;databaseName=shop;encrypt=true")
            )
            .is_err()
        );
        let mut long = source("jdbc:mysql://localhost/shop");
        long.name = "x".repeat(1024);
        assert!(target(&scope, &long).unwrap().unwrap().target.name.len() <= 256);
    }
    #[test]
    fn foreign_scope_snapshot_cannot_configure_a_connection() {
        let (host, mut runtime) = setup();
        host.set_project_root(Some("/other"));
        assert!(
            runtime
                .configure_snapshot_database(&host.invocation())
                .is_err()
        );
        assert!(
            host.invocation()
                .project()
                .unwrap()
                .data_sources()
                .unwrap()
                .is_empty()
        );
    }
}
