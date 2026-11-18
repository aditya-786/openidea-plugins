//! Explicit curated framework schema capture and nonsecret host source registration.
use super::{State, execute, key, owner, queue_tools, stamp, target_binding};
use crate::artisan_runner::Runner;
use openidea_plugin::{Error, Invocation, PluginDataSource, Project, host};
use serde_json::{Value, json};
fn text(value: &Value) -> Result<String, Error> {
    value
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 512 && !s.chars().any(char::is_control))
        .map(str::to_owned)
        .ok_or_else(|| Error::Invalid("Invalid schema metadata text.".into()))
}
pub(super) fn parse(raw: &str) -> Result<Vec<Value>, Error> {
    let value = crate::strict_json::parse(raw, super::MAX)?;
    let tables = value["tables"]
        .as_array()
        .filter(|t| t.len() <= 128)
        .ok_or("Expected up to 128 schema tables.")?;
    let connection = value.get("connection").map(text).transpose()?;
    let mut names = std::collections::BTreeSet::new();
    let mut rows = Vec::new();
    for table in tables {
        let name = text(&table["name"])?;
        let namespace = table["schema"].as_str().unwrap_or("");
        if namespace.len() > 512 || namespace.chars().any(char::is_control) {
            return Err(Error::Stale);
        }
        if !names.insert((namespace.to_owned(), name.clone())) {
            return Err("Duplicate schema table.".into());
        }
        let columns = table["columns"]
            .as_array()
            .filter(|c| c.len() <= 256)
            .ok_or("Expected up to 256 table columns.")?;
        let mut safe_columns = Vec::new();
        let mut column_names = std::collections::BTreeSet::new();
        for column in columns {
            let name = text(&column["name"])?;
            if !column_names.insert(name.clone()) {
                return Err("Duplicate schema column.".into());
            }
            let mut safe = json!({"name":name,"type":text(&column["type"])?,"nullable":column["nullable"].as_bool().ok_or("Expected column nullability.")?});
            if let Some(value) = column.get("fullType").filter(|v| !v.is_null()) {
                safe["fullType"] = json!(text(value)?);
            }
            for key in ["autoIncrement", "unsigned"] {
                if let Some(value) = column.get(key).filter(|v| !v.is_null()) {
                    safe[key] = json!(value.as_bool().ok_or("Expected explicit schema boolean.")?);
                }
            }
            for key in ["length", "precision", "scale"] {
                if let Some(value) = column.get(key).filter(|v| !v.is_null()) {
                    let number = value
                        .as_u64()
                        .and_then(|n| u32::try_from(n).ok())
                        .filter(|n| key == "scale" || *n > 0)
                        .ok_or("Invalid schema type dimension.")?;
                    safe[key] = json!(number);
                }
            }
            if let (Some(precision), Some(scale)) =
                (safe["precision"].as_u64(), safe["scale"].as_u64())
            {
                if scale > precision {
                    return Err("Schema scale exceeds precision.".into());
                }
            }
            if let Some(value) = column.get("values").filter(|v| !v.is_null()) {
                let values = value
                    .as_array()
                    .filter(|a| !a.is_empty() && a.len() <= 256)
                    .ok_or("Expected up to 256 enum/set constants.")?;
                let mut unique = std::collections::BTreeSet::new();
                let mut literals = Vec::new();
                for value in values {
                    let literal = value
                        .as_str()
                        .filter(|s| s.len() <= 512 && !s.chars().any(char::is_control))
                        .ok_or("Invalid enum/set constant.")?;
                    if !unique.insert(literal) {
                        return Err("Duplicate enum/set constant.".into());
                    }
                    literals.push(literal);
                }
                safe["values"] = json!(literals);
            }
            safe_columns.push(safe);
        }
        let indexes = table["indexes"]
            .as_array()
            .filter(|i| i.len() <= 64)
            .ok_or("Expected up to 64 table indexes.")?;
        let mut safe_indexes = Vec::new();
        for index in indexes {
            let fields = index["columns"]
                .as_array()
                .filter(|a| a.len() <= 64)
                .ok_or("Expected index columns.")?;
            let fields = fields.iter().map(text).collect::<Result<Vec<_>, _>>()?;
            if fields.iter().any(|field| !column_names.contains(field)) {
                return Err("Index names an unknown column.".into());
            }
            safe_indexes.push(json!({"name":text(&index["name"])?,"columns":fields,"unique":index["unique"].as_bool().ok_or("Expected index uniqueness.")?,"primary":index["primary"].as_bool().ok_or("Expected primary index state.")?}));
        }
        let safe =
            json!({"name":name,"schema":namespace,"columns":safe_columns,"indexes":safe_indexes});
        rows.push(json!({"key":format!("runtime:schema:{}",rows.len()),"cells":["Schema",if namespace.is_empty(){name.clone()}else{format!("{namespace}.{name}")},format!("{} columns · {} indexes · explicitly captured",columns.len(),indexes.len())],"schemaTable":safe,"schemaConnection":connection,"activationEnabled":false}));
    }
    if rows.is_empty() && connection.is_some() {
        rows.push(json!({"key":"runtime:schema:connection","cells":["Schema",connection,"No tables returned by the explicitly selected connection"],"schemaConnection":connection,"activationEnabled":false}));
    }
    Ok(rows)
}
fn source(project: &Project, raw: &str) -> Result<PluginDataSource, Error> {
    let value = crate::strict_json::parse(raw, 16384)?;
    let connection = text(&value["name"])?;
    let declared = text(&value["driver"])?;
    let driver = match declared.as_str() {
        "mysql" | "mariadb" => "mysql",
        "pgsql" => "postgres",
        "sqlite" => "sqlite",
        "sqlsrv" => "sqlserver",
        _ => return Err("Selected database driver is not supported for host registration.".into()),
    };
    let root = project.root()?;
    let database = text(&value["database"])?;
    let optional = |name: &str| -> Result<Option<String>, Error> {
        if value[name].is_null() {
            Ok(None)
        } else {
            text(&value[name]).map(Some)
        }
    };
    let (host, port, user) = if driver == "sqlite" {
        if database == ":memory:"
            || !database.starts_with(&format!("{root}/"))
            || database
                .strip_prefix(&format!("{root}/"))
                .unwrap_or("")
                .split('/')
                .any(|p| matches!(p, "" | "." | ".."))
            || !crate::owns_current_path(project, &database)
            || !project.exists(&database)?
        {
            return Err(
                "SQLite host registration requires an existing owned project database file.".into(),
            );
        }
        (None, None, None)
    } else {
        let host = optional("host")?.ok_or("Missing database host.")?;
        if host.contains(['/', '@', '?', '#']) {
            return Err("Database host is not a plain hostname or address.".into());
        }
        let port = match &value["port"] {
            Value::Null => None,
            Value::String(s) => Some(s.parse::<u16>().map_err(|_| Error::Stale)?),
            v => Some(u16::try_from(v.as_u64().ok_or(Error::Stale)?).map_err(|_| Error::Stale)?),
        };
        if port == Some(0) {
            return Err(Error::Stale);
        }
        (Some(host), port, optional("username")?)
    };
    Ok(PluginDataSource {
        id: format!("laravel-{}", crate::artisan_metadata::hash(&connection)),
        name: format!("Laravel · {connection}"),
        driver: driver.into(),
        host,
        port,
        database: Some(database),
        user,
        url: None,
        options: Vec::new(),
    })
}
fn connections(raw: &str) -> Result<Vec<Value>, Error> {
    let value = crate::strict_json::parse(raw, 128 * 1024)?;
    let entries = value["connections"]
        .as_array()
        .filter(|a| a.len() <= 32)
        .ok_or("Expected up to 32 declared database connections.")?;
    let default = value["default"].as_str().unwrap_or("");
    let mut names = std::collections::BTreeSet::new();
    let mut safe = Vec::new();
    for entry in entries {
        let name = text(&entry["name"])?;
        if name.len() > 128 || !names.insert(name.clone()) {
            return Err("Invalid or duplicate connection name.".into());
        }
        let available = entry["available"]
            .as_bool()
            .ok_or("Missing connection availability.")?;
        let mut row = json!({"name":name,"available":available,"isDefault":name==default});
        if available {
            for key in ["driver", "host", "database", "username"] {
                if !entry[key].is_null() {
                    row[key] = json!(text(&entry[key])?);
                }
            }
            if !entry["port"].is_null() {
                let port = entry["port"]
                    .as_u64()
                    .or_else(|| entry["port"].as_str()?.parse::<u64>().ok())
                    .and_then(|n| u16::try_from(n).ok())
                    .filter(|p| *p > 0)
                    .ok_or("Invalid database connection port.")?;
                row["port"] = json!(port);
            }
        } else {
            row["reason"] = json!(match entry["reason"].as_str() {
                Some("Structured connection settings require manual configuration.") =>
                    "Structured connection settings require manual configuration.",
                Some("Connection transport options require manual configuration.") =>
                    "Connection transport options require manual configuration.",
                _ => "Framework could not resolve this connection.",
            });
        }
        safe.push(row);
    }
    Ok(safe)
}
fn pick_connection(ctx: &Invocation, runner: Runner) -> Result<Option<Value>, Error> {
    let (project, root) = owner(ctx)?;
    let before = stamp(&project)?;
    let binding = target_binding(&project, runner)?;
    let args = queue_tools::probe_args("database-sources", &json!({}))?;
    let inventory = connections(&execute(&project, runner, &args)?)?;
    if inventory.is_empty() {
        return Err(crate::ui_text::text(
            "runtime.boundary.7be74b3613",
            "No declared database connections in this application.",
        )
        .into());
    }
    let choices: Vec<_> = inventory
        .iter()
        .map(|entry| {
            openidea_plugin::PickItem::new(
                entry["name"].as_str().unwrap(),
                &format!(
                    "{}{}",
                    entry["name"].as_str().unwrap(),
                    if entry["isDefault"] == true {
                        crate::ui_text::text("runtime.boundary.f1aeec6a94", " (default)")
                    } else {
                        ""
                    }
                ),
            )
            .description(if entry["available"] == true {
                entry["driver"].as_str().unwrap_or(crate::ui_text::text(
                    "runtime.boundary.4428861b89",
                    "Unknown driver",
                ))
            } else {
                crate::ui_text::text(
                    "runtime.boundary.6e4a48ea04",
                    "Unavailable: manual configuration required",
                )
            })
        })
        .collect();
    let Some(name) = host::pick(
        crate::ui_text::text(
            "runtime.boundary.9700459bfc",
            "Declared Laravel database connection",
        ),
        &choices,
    )?
    else {
        return Ok(None);
    };
    if owner(ctx)?.1 != root
        || stamp(&project)? != before
        || target_binding(&project, runner)? != binding
    {
        return Err(Error::Stale);
    }
    let selected = inventory
        .into_iter()
        .find(|entry| entry["name"] == name)
        .ok_or(Error::Stale)?;
    if selected["available"] != true {
        return Err(selected["reason"]
            .as_str()
            .unwrap_or(crate::ui_text::text(
                "runtime.boundary.8209a5d003",
                "Connection unavailable.",
            ))
            .into());
    }
    Ok(Some(selected))
}
const DATABASE_DOCKER_FORMAT: &str = r#"{"Id":{{json .Id}},"Image":{{json .Image}},"Running":{{json .State.Running}},"Paused":{{json .State.Paused}},"Restarting":{{json .State.Restarting}},"StartedAt":{{json .State.StartedAt}},"Project":{{json (index .Config.Labels "com.docker.compose.project")}},"Networks":{{json .NetworkSettings.Networks}},"Ports":{{json .NetworkSettings.Ports}}}"#;
fn docker_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
}
fn docker_inspect_args(socket: &str, id: &str) -> Vec<String> {
    [
        "--host",
        socket,
        "inspect",
        "--type",
        "container",
        "--format",
        DATABASE_DOCKER_FORMAT,
        id,
    ]
    .map(str::to_owned)
    .to_vec()
}
fn docker_run(project: &Project, args: &[String]) -> Result<String, Error> {
    let task = host::begin_task(
        crate::ui_text::text(
            "runtime.boundary.1c28a91752",
            "Inspect published Laravel database ports",
        ),
        true,
    );
    let child = host::start(&host::Command::new("docker").args(args), Some(project)).map_err(|error| ui_kit::denied::map(error, "Running Artisan in Docker", "process:docker", "Laravel"))?;
    if let Err(error) = child.close_stdin() {
        let _ = child.kill();
        return Err(error);
    }
    let result = crate::artisan_metadata::collect_limited(
        2 * 1024 * 1024,
        32,
        |timeout| child.wait(timeout),
        || task.is_cancelled(),
    );
    if result.is_err() {
        let _ = child.kill();
    }
    result
}
fn docker_container(raw: &str, id: &str) -> Result<Value, Error> {
    let value = crate::strict_json::parse(raw, 2 * 1024 * 1024)?;
    if !docker_id(id)
        || value["Id"] != id
        || value["Running"] != true
        || value["Paused"] != false
        || value["Restarting"] != false
    {
        return Err("Database mapping requires an exact running unpaused container.".into());
    }
    let project = value["Project"].as_str().unwrap_or("");
    if project.len() > 128 || project.chars().any(char::is_control) {
        return Err(Error::Stale);
    }
    let networks = value["Networks"]
        .as_object()
        .filter(|n| n.len() <= 32)
        .ok_or("Docker network inventory unavailable.")?;
    let mut safe_networks = Vec::new();
    for network in networks.values() {
        let id = text(&network["NetworkID"])?;
        let mut aliases = Vec::new();
        if let Some(values) = network["Aliases"].as_array() {
            if values.len() > 64 {
                return Err(Error::Stale);
            }
            for alias in values {
                aliases.push(text(alias)?);
            }
        }
        let ip = network["IPAddress"].as_str().unwrap_or("");
        let ipv6 = network["GlobalIPv6Address"].as_str().unwrap_or("");
        if ip.len() > 64
            || ipv6.len() > 64
            || ip.chars().any(char::is_control)
            || ipv6.chars().any(char::is_control)
        {
            return Err(Error::Stale);
        }
        safe_networks.push(json!({"id":id,"aliases":aliases,"ip":ip,"ipv6":ipv6}));
    }
    safe_networks.sort_by_key(|n| n["id"].as_str().unwrap_or("").to_owned());
    let ports = value["Ports"]
        .as_object()
        .filter(|p| p.len() <= 128)
        .ok_or("Docker published-port inventory unavailable.")?;
    let mut safe_ports = serde_json::Map::new();
    for (port, bindings) in ports {
        if port.len() > 32 {
            return Err(Error::Stale);
        }
        let mut safe = Vec::new();
        if !bindings.is_null() {
            let bindings = bindings
                .as_array()
                .filter(|a| a.len() <= 32)
                .ok_or(Error::Stale)?;
            for binding in bindings {
                safe.push(
                    json!({"host":text(&binding["HostIp"])?,"port":text(&binding["HostPort"])?}),
                );
            }
        }
        safe_ports.insert(port.clone(), json!(safe));
    }
    Ok(
        json!({"Id":id,"Image":text(&value["Image"])?,"StartedAt":text(&value["StartedAt"])?,"Project":project,"networks":safe_networks,"ports":safe_ports}),
    )
}
fn mapped_endpoint(
    connection: &Value,
    app: &Value,
    peers: &[Value],
) -> Result<(String, u16, Value), Error> {
    let driver = connection["driver"].as_str().unwrap_or("");
    let default =
        match driver {
            "mysql" | "mariadb" => 3306,
            "pgsql" => 5432,
            _ => return Err(
                "Docker native registration supports published MySQL/PostgreSQL TCP ports only."
                    .into(),
            ),
        };
    let port = connection["port"]
        .as_u64()
        .and_then(|n| u16::try_from(n).ok())
        .unwrap_or(default);
    let hostname = connection["host"]
        .as_str()
        .ok_or("Missing database host.")?;
    let loopback = hostname.eq_ignore_ascii_case("localhost")
        || hostname
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    let mut candidates = Vec::new();
    if loopback {
        candidates.push(app);
    } else {
        let project = app["Project"]
            .as_str()
            .filter(|p| !p.is_empty())
            .ok_or("Docker peer mapping requires a verified Compose project label.")?;
        for peer in peers {
            if peer["Project"] != project {
                continue;
            }
            let matches =
                peer["networks"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|network| {
                        let shared = app["networks"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .any(|owned| owned["id"] == network["id"]);
                        shared
                            && (network["ip"] == hostname
                                || network["ipv6"] == hostname
                                || network["aliases"].as_array().into_iter().flatten().any(
                                    |alias| {
                                        alias.as_str().is_some_and(|alias| {
                                            alias.eq_ignore_ascii_case(hostname)
                                        })
                                    },
                                ))
                    });
            if matches {
                candidates.push(peer);
            }
        }
    }
    if candidates.len() != 1 {
        return Err("Database host has no unique same-project shared-network container mapping; configure a native source manually.".into());
    }
    let peer = candidates[0];
    let key = format!("{port}/tcp");
    let bindings = peer["ports"][&key]
        .as_array()
        .ok_or("Database TCP port is not published to the host.")?;
    let mut endpoints = Vec::new();
    for binding in bindings {
        let host = binding["host"]
            .as_str()
            .ok_or(Error::Stale)?
            .parse::<std::net::IpAddr>()
            .map_err(|_| Error::Stale)?;
        if !host.is_loopback() && !host.is_unspecified() {
            continue;
        }
        let host = match host {
            std::net::IpAddr::V4(ip) => {
                if ip.is_unspecified() {
                    "127.0.0.1".into()
                } else {
                    ip.to_string()
                }
            }
            std::net::IpAddr::V6(ip) => {
                if ip.is_unspecified() {
                    "::1".into()
                } else {
                    ip.to_string()
                }
            }
        };
        let port = binding["port"]
            .as_str()
            .and_then(|p| p.parse::<u16>().ok())
            .filter(|p| *p > 0)
            .ok_or(Error::Stale)?;
        endpoints.push((host, port));
    }
    endpoints.sort_by_key(|(host, _)| host.contains(':'));
    endpoints.dedup();
    let Some(endpoint) = endpoints.first() else {
        return Err("Database has no proven loopback/wildcard published host port.".into());
    };
    if endpoints.iter().any(|(_, port)| *port != endpoint.1) {
        return Err("Database has ambiguous published host ports.".into());
    }
    Ok((endpoint.0.clone(), endpoint.1, peer.clone()))
}
fn docker_mapping(
    project: &Project,
    binding: &Value,
    connection: &Value,
    mut run: impl FnMut(&Project, &[String]) -> Result<String, Error>,
) -> Result<(Value, Value), Error> {
    let socket = binding["identity"]["socket"]
        .as_str()
        .filter(|s| {
            s.strip_prefix("unix://")
                .is_some_and(crate::hover::canonical)
        })
        .ok_or(Error::Stale)?;
    let id = binding["identity"]["container"]
        .as_str()
        .filter(|id| docker_id(id))
        .ok_or(Error::Stale)?;
    let app = docker_container(&run(project, &docker_inspect_args(socket, id))?, id)?;
    if app["StartedAt"] != binding["proof"]["StartedAt"]
        || app["Image"] != binding["proof"]["Image"]
    {
        return Err(Error::Stale);
    }
    let hostname = connection["host"].as_str().unwrap_or("");
    let loopback = hostname.eq_ignore_ascii_case("localhost")
        || hostname
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    let mut peers = Vec::new();
    if !loopback {
        let label = app["Project"]
            .as_str()
            .filter(|p| !p.is_empty())
            .ok_or("Docker peer mapping requires a verified Compose project label.")?;
        let args = vec![
            "--host".into(),
            socket.into(),
            "ps".into(),
            "--no-trunc".into(),
            "--filter".into(),
            format!("label=com.docker.compose.project={label}"),
            "--format".into(),
            "{{.ID}}".into(),
        ];
        let raw = run(project, &args)?;
        if raw.len() > 8192 {
            return Err(Error::Stale);
        }
        let ids: std::collections::BTreeSet<_> = raw.lines().filter(|id| !id.is_empty()).collect();
        if ids.len() > 64 || ids.iter().any(|id| !docker_id(id)) {
            return Err("Docker database peer inventory is invalid or too large.".into());
        }
        for peer_id in ids {
            peers.push(docker_container(
                &run(project, &docker_inspect_args(socket, peer_id))?,
                peer_id,
            )?);
        }
    }
    let (host, port, peer) = mapped_endpoint(connection, &app, &peers)?;
    let mut mapped = connection.clone();
    mapped["host"] = json!(host);
    mapped["port"] = json!(port);
    Ok((mapped, json!({"socket":socket,"app":app,"peer":peer})))
}
fn owned_source(project: &Project, id: &str) -> Result<(), Error> {
    if !source_id(id) || !project.data_sources()?.iter().any(|known| known == id) {
        return Err(Error::Stale);
    }
    Ok(())
}
fn source_id(id: &str) -> bool {
    id.strip_prefix("laravel-")
        .is_some_and(|hash| hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
}
impl State {
    /// Register an explicitly captured external connection through the same owned
    /// native source flow. Passwords never enter the review or source definition.
    pub(crate) fn register_external_source(&self,ctx:&Invocation,definition:&PluginDataSource,credential:Option<&str>,revalidate:impl FnOnce()->Result<(),Error>)->Result<(),Error>{
        self.register_external_source_inner(ctx,definition,credential,revalidate).map_err(ui_error)
    }
    fn register_external_source_inner(
        &self, ctx:&Invocation, definition:&PluginDataSource, credential:Option<&str>,
        revalidate:impl FnOnce()->Result<(),Error>,
    )->Result<(),Error>{
        let (project,root)=owner(ctx)?;
        if !source_id(&definition.id)||definition.id!=format!("laravel-{}",crate::artisan_metadata::hash(&definition.name))||definition.url.is_some()||!definition.options.is_empty(){return Err(Error::Stale);}
        let driver=match definition.driver.as_str(){"mysql"=>"mysql","postgres"=>"pgsql",_=>return Err(Error::Stale)};
        let validated=source(&project,&json!({"name":definition.name,"driver":driver,"host":definition.host,"port":definition.port,"database":definition.database,"username":definition.user}).to_string())?;
        if validated.driver!=definition.driver||validated.host!=definition.host||validated.port!=definition.port||validated.database!=definition.database||validated.user!=definition.user||credential.is_some_and(|value|value.len()>64*1024||value.contains('\0')){return Err(Error::Stale);}
        let available=||->Result<bool,Error>{let ids=project.data_sources()?;Ok(ids.len()<256&&!ids.contains(&definition.id))};
        if project.data_sources()?.len()>=256{return Err(Error::Invalid(crate::ui_text::t("cloud.databaseSourceLimit","At most 256 database sources can be registered for this project.")));}
        if project.data_sources()?.contains(&definition.id){return Err(Error::Invalid(crate::ui_text::t("cloud.databaseAlreadyRegistered","This Cloud source is already registered. Review or remove the existing source in Database before importing it again.")));}
        let preview=crate::ui_text::format("cloud.databaseReview","Project: {root}\nRegister this captured Cloud database source:\n{definition}\nCredentials are excluded from this review. Captured credentials are stored in the native credential store; if none was returned, configure them in Database. Registration does not connect or execute SQL.",&[("root",&root),("definition",&serde_json::to_string_pretty(definition).map_err(|_|Error::Stale)?)]);
        if !ui_kit::prompts::confirm_exact(crate::ui_text::text("cloud.databaseConfirm","Register captured Cloud database source?"),&preview)?{return Ok(());}
        if owner(ctx)?.1!=root||!available()?{return Err(Error::Stale);}
        revalidate()?;
        if owner(ctx)?.1!=root||!available()?{return Err(Error::Stale);}
        project.upsert_data_source(definition)?;
        if let Some(credential)=credential{if project.set_data_source_credential(&definition.id,credential).is_err(){
            return Err(Error::Failed(crate::ui_text::t("cloud.credentialPartial","Credential storage failed after source registration. The registered source may remain; review it in Database before connecting.")));
        }}
        Ok(())
    }
    /// Only Laravel-created sources belonging to this plugin and project are exposed.
    pub fn database_sources(&self, ctx: &Invocation) -> Result<Vec<String>, Error> {
        self.database_sources_ui_inner(ctx).map_err(ui_error)
    }
    fn database_sources_ui_inner(&self, ctx: &Invocation) -> Result<Vec<String>, Error> {
        let (project, _) = owner(ctx)?;
        let mut ids = project.data_sources()?;
        if ids.len() > 256 {
            return Err(crate::ui_text::text(
                "runtime.boundary.194e63b833",
                "Database source inventory exceeds 256 entries.",
            )
            .into());
        }
        ids.retain(|id| source_id(id));
        ids.sort();
        ids.dedup();
        Ok(ids)
    }
    pub fn database_source_rows(&self, ctx: &Invocation) -> Result<Vec<Value>, Error> {
        self.database_source_rows_ui_inner(ctx).map_err(ui_error)
    }
    fn database_source_rows_ui_inner(&self, ctx: &Invocation) -> Result<Vec<Value>, Error> {
        let (_, root) = owner(ctx)?;
        let project_key = crate::artisan_metadata::hash(&root);
        Ok(self.database_sources(ctx)?.into_iter().map(|id|json!({"key":format!("runtime:database:{project_key}:{id}"),"cells":[crate::ui_text::text("runtime.boundary.61074f1c95","Database"),id,crate::ui_text::text("runtime.boundary.0613cf7161","Registered plugin-owned source · Connect uses the native Database credential flow")],"activationEnabled":false})).collect())
    }
    /// Source IDs may come from command arguments; passwords must never do so.
    pub fn database_source_action(
        &self,
        ctx: &Invocation,
        action: &str,
        selected: Option<&str>,
    ) -> Result<(), Error> {
        self.database_source_action_ui_inner(ctx, action, selected)
            .map_err(ui_error)
    }
    fn database_source_action_ui_inner(
        &self,
        ctx: &Invocation,
        action: &str,
        selected: Option<&str>,
    ) -> Result<(), Error> {
        if !matches!(action, "connect" | "remove") {
            return Err(Error::Invalid(
                crate::ui_text::text(
                    "runtime.boundary.5b5414414c",
                    "Unknown database source action.",
                )
                .into(),
            ));
        }
        let (_, root) = owner(ctx)?;
        let ids = self.database_sources(ctx)?;
        let id = if let Some(id) = selected {
            if let Some(encoded) = id.strip_prefix("runtime:database:") {
                let (project_key, id) = encoded.split_once(':').ok_or(Error::Stale)?;
                if project_key != crate::artisan_metadata::hash(&root) {
                    return Err(Error::Stale);
                }
                id.to_owned()
            } else {
                id.to_owned()
            }
        } else {
            if ids.is_empty() {
                return Err(crate::ui_text::text(
                    "runtime.boundary.30ce967935",
                    "No registered Laravel database sources in this project.",
                )
                .into());
            }
            let choices: Vec<_> = ids
                .iter()
                .map(|id| openidea_plugin::PickItem::new(id, id))
                .collect();
            let Some(id) = host::pick(
                crate::ui_text::text("runtime.boundary.3ccc9a240d", "Laravel database source"),
                &choices,
            )?
            else {
                return Ok(());
            };
            id
        };
        if !ids.contains(&id) {
            return Err(Error::Stale);
        }
        if action == "remove"
            && !ui_kit::prompts::confirm_exact(
                crate::ui_text::text(
                    "runtime.boundary.262605f373",
                    "Remove Laravel database source?",
                ),
                &crate::ui_text::format(
                    "runtime.boundary.bae6343e32",
                    "Project: {root}\nRemove this plugin-owned source: {id}\nIts saved credentials will be removed too. The database and its data are not changed.",
                    &[("root", &(root).to_string()), ("id", &(id).to_string())],
                ),
            )?
        {
            return Ok(());
        }
        let (project, current) = owner(ctx)?;
        if current != root {
            return Err(Error::Stale);
        }
        owned_source(&project, &id)?;
        match action {
            "connect" => project.connect_data_source(&id),
            "remove" => project.remove_data_source(&id),
            _ => Err(Error::Stale),
        }
    }
    /// Webview/context selections must carry the project-bound inventory key.
    pub fn database_source_row_action(
        &self,
        ctx: &Invocation,
        action: &str,
        row_key: &str,
    ) -> Result<(), Error> {
        self.database_source_row_action_ui_inner(ctx, action, row_key)
            .map_err(ui_error)
    }
    fn database_source_row_action_ui_inner(
        &self,
        ctx: &Invocation,
        action: &str,
        row_key: &str,
    ) -> Result<(), Error> {
        if !row_key.starts_with("runtime:database:") {
            return Err(Error::Stale);
        }
        self.database_source_action(ctx, action, Some(row_key))
    }
    /// Internal/native integration only: never call with a webview/MCP/command JSON password.
    /// There is no password read API. Interactive users manage credentials through Connect.
    pub fn set_database_credential(
        &self,
        ctx: &Invocation,
        id: &str,
        password: &str,
    ) -> Result<(), Error> {
        self.set_database_credential_ui_inner(ctx, id, password)
            .map_err(ui_error)
    }
    fn set_database_credential_ui_inner(
        &self,
        ctx: &Invocation,
        id: &str,
        password: &str,
    ) -> Result<(), Error> {
        let (project, _) = owner(ctx)?;
        owned_source(&project, id)?;
        if password.len() > 64 * 1024 {
            return Err(Error::Invalid(
                crate::ui_text::text(
                    "runtime.boundary.b8d5f5efed",
                    "Credential exceeds the host size limit.",
                )
                .into(),
            ));
        }
        project.set_data_source_credential(id, password)
    }
    pub fn schema_snapshot(&self, ctx: &Invocation, runner: Runner) -> Result<Value, Error> {
        self.schema_snapshot_ui_inner(ctx, runner).map_err(ui_error)
    }
    fn schema_snapshot_ui_inner(&self, ctx: &Invocation, runner: Runner) -> Result<Value, Error> {
        let (project, _) = owner(ctx)?;
        self.schema_snapshot_for_project(&project, runner)
    }
    pub(crate) fn schema_snapshot_for_project(
        &self,
        project: &Project,
        runner: Runner,
    ) -> Result<Value, Error> {
        let root = project.root()?;
        if !crate::hover::canonical(&root)
            || !host::projects()?
                .iter()
                .any(|p| p.root().ok().as_deref() == Some(&root))
        {
            return Err(Error::Stale);
        }
        let target = target_binding(project, runner)?;
        let snapshot = self
            .snapshots
            .get(&key(&root, runner, "schema"))
            .filter(|s| s.target == target);
        let tables: Vec<_> = snapshot
            .into_iter()
            .flat_map(|s| s.rows.iter())
            .filter_map(|r| r.get("schemaTable").cloned())
            .collect();
        let connection = snapshot.and_then(|s| {
            s.rows.iter().find_map(|row| {
                row.get("schemaConnection")
                    .filter(|value| !value.is_null())
                    .cloned()
            })
        });
        Ok(
            json!({"tables":tables,"connection":connection,"capturedAt":snapshot.map(|s|s.captured),"runner":runner.name(),"provenance":"Explicit framework schema capture; refresh after database changes"}),
        )
    }
    /// Generating from a capture additionally requires unchanged application inputs.
    pub(crate) fn schema_for_generation(
        &self,
        project: &Project,
        runner: Runner,
    ) -> Result<Value, Error> {
        let value = self.schema_snapshot_for_project(project, runner)?;
        if let Some(snapshot) = self.snapshots.get(&key(&project.root()?, runner, "schema")) {
            if snapshot.identity != stamp(project)? {
                return Err(Error::Stale);
            }
        }
        Ok(value)
    }
    /// Explicit named discovery, selection and exact source review. Never exports credentials.
    pub fn register_database_source(&self, ctx: &Invocation, runner: Runner) -> Result<(), Error> {
        self.register_database_source_ui_inner(ctx, runner)
            .map_err(ui_error)
    }
    fn register_database_source_ui_inner(
        &self,
        ctx: &Invocation,
        runner: Runner,
    ) -> Result<(), Error> {
        self.register_database_with(
            ctx,
            runner,
            |project, args| execute(project, runner, args),
            docker_run,
        )
    }
    fn register_database_with(
        &self,
        ctx: &Invocation,
        runner: Runner,
        mut probe: impl FnMut(&Project, &[String]) -> Result<String, Error>,
        mut docker: impl FnMut(&Project, &[String]) -> Result<String, Error>,
    ) -> Result<(), Error> {
        let (project, root) = owner(ctx)?;
        let before = stamp(&project)?;
        let binding = target_binding(&project, runner)?;
        // Capability and trust are checked before running the application probe.
        project.data_sources()?;
        let args = queue_tools::probe_args("database-sources", &json!({}))?;
        let inventory = connections(&probe(&project, &args)?)?;
        if inventory.is_empty() {
            return Err(crate::ui_text::text(
                "runtime.boundary.7be74b3613",
                "No declared database connections in this application.",
            )
            .into());
        }
        let choices: Vec<_> = inventory
            .iter()
            .map(|entry| {
                openidea_plugin::PickItem::new(
                    entry["name"].as_str().unwrap(),
                    &format!(
                        "{}{}",
                        entry["name"].as_str().unwrap(),
                        if entry["isDefault"] == true {
                            crate::ui_text::text("runtime.boundary.f1aeec6a94", " (default)")
                        } else {
                            ""
                        }
                    ),
                )
                .description(if entry["available"] == true {
                    entry["driver"].as_str().unwrap_or(crate::ui_text::text(
                        "runtime.boundary.4428861b89",
                        "Unknown driver",
                    ))
                } else {
                    crate::ui_text::text(
                        "runtime.boundary.6e4a48ea04",
                        "Unavailable: manual configuration required",
                    )
                })
            })
            .collect();
        let Some(name) = host::pick(
            crate::ui_text::text(
                "runtime.boundary.6fe1526277",
                "Register declared Laravel database connection",
            ),
            &choices,
        )?
        else {
            return Ok(());
        };
        if owner(ctx)?.1 != root
            || stamp(&project)? != before
            || target_binding(&project, runner)? != binding
        {
            return Err(Error::Stale);
        }
        let selected = inventory
            .into_iter()
            .find(|entry| entry["name"] == name)
            .ok_or(Error::Stale)?;
        if selected["available"] != true {
            return Err(selected["reason"]
                .as_str()
                .unwrap_or(crate::ui_text::text(
                    "runtime.boundary.8209a5d003",
                    "Connection unavailable.",
                ))
                .into());
        }
        let (mapped, evidence) = if runner == Runner::Docker {
            let (mapped, proof) = docker_mapping(
                &project,
                binding.as_ref().ok_or(Error::Stale)?,
                &selected,
                &mut docker,
            )?;
            (mapped, Some(proof))
        } else {
            (selected.clone(), None)
        };
        let definition = source(&project, &mapped.to_string())?;
        let mapping=evidence.as_ref().map(|proof|crate::ui_text::format("runtime.boundary.b0e65ca1f0","\nOriginal container endpoint: {originalHost}:{originalPort}\nMapped host endpoint: {mappedHost}:{mappedPort}\nVerified app container: {appContainer}\nVerified database container: {databaseContainer}\n",&[("originalHost",&(selected["host"].as_str().unwrap_or("")).to_string()),("originalPort",&(selected["port"].as_u64().unwrap_or(if selected["driver"]=="pgsql"{5432}else{3306})).to_string()),("mappedHost",&(mapped["host"].as_str().unwrap_or("")).to_string()),("mappedPort",&(mapped["port"]).to_string()),("appContainer",&(proof["app"]["Id"].as_str().unwrap_or("")).to_string()),("databaseContainer",&(proof["peer"]["Id"].as_str().unwrap_or("")).to_string())])).unwrap_or_default();
        let preview = crate::ui_text::format(
            "runtime.boundary.1251588c01",
            "Project: {root}\nRegister this plugin-owned database source:\n{definition}{mapping}\nNo password or URL is captured. Set credentials in the native Database interface. Registration does not connect or execute SQL.",
            &[
                ("root", &(root).to_string()),
                (
                    "definition",
                    &(serde_json::to_string_pretty(&definition)
                        .map_err(|e| Error::Invalid(e.to_string()))?)
                    .to_string(),
                ),
                ("mapping", &(mapping).to_string()),
            ],
        );
        if !ui_kit::prompts::confirm_exact(
            crate::ui_text::text(
                "runtime.boundary.3c701a9b72",
                "Register Laravel database source?",
            ),
            &preview,
        )? {
            return Ok(());
        }
        if owner(ctx)?.1 != root
            || stamp(&project)? != before
            || target_binding(&project, runner)? != binding
        {
            return Err(Error::Stale);
        }
        let current = connections(&probe(&project, &args)?)?
            .into_iter()
            .find(|entry| entry["name"] == name)
            .ok_or(Error::Stale)?;
        if current != selected {
            return Err(Error::Stale);
        }
        if let Some(expected) = evidence {
            let (remapped, actual) = docker_mapping(
                &project,
                binding.as_ref().ok_or(Error::Stale)?,
                &current,
                &mut docker,
            )?;
            if actual != expected || remapped != mapped {
                return Err(Error::Stale);
            }
        }
        if owner(ctx)?.1 != root
            || stamp(&project)? != before
            || target_binding(&project, runner)? != binding
        {
            return Err(Error::Stale);
        }
        project.upsert_data_source(&definition)
    }
    /// Explicit named schema selection; reading schema_snapshot never discovers connections.
    pub fn capture_selected_schema(
        &mut self,
        ctx: &Invocation,
        runner: Runner,
    ) -> Result<(), Error> {
        self.capture_selected_schema_ui_inner(ctx, runner)
            .map_err(ui_error)
    }
    fn capture_selected_schema_ui_inner(
        &mut self,
        ctx: &Invocation,
        runner: Runner,
    ) -> Result<(), Error> {
        let Some(connection) = pick_connection(ctx, runner)? else {
            return Ok(());
        };
        self.capture_options_with(
            ctx,
            runner,
            "schema",
            json!({"connection":connection["name"]}),
            |project, args| execute(project, runner, args),
        )
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn lifecycle_host() -> (openidea_plugin::mock::MockHost, State, String) {
        let host = openidea_plugin::mock::MockHost::install()
            .with_capabilities(&["project.read", "database.sources"]);
        let id = format!("laravel-{}", crate::artisan_metadata::hash("default"));
        host.invocation()
            .project()
            .unwrap()
            .upsert_data_source(&PluginDataSource {
                id: id.clone(),
                name: "Laravel default".into(),
                driver: "postgres".into(),
                ..Default::default()
            })
            .unwrap();
        (host, State::default(), id)
    }
    #[test]
    fn database_source_lifecycle_is_owned_and_credential_safe() {
        let (host, state, id) = lifecycle_host();
        let ctx = host.invocation();
        let project = ctx.project().unwrap();
        project
            .upsert_data_source(&PluginDataSource {
                id: "other-plugin-workflow".into(),
                name: "Other".into(),
                driver: "sqlite".into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(state.database_sources(&ctx).unwrap(), vec![id.clone()]);
        state
            .set_database_credential(&ctx, &id, "private credential")
            .unwrap();
        assert!(
            !json!(state.database_source_rows(&ctx).unwrap())
                .to_string()
                .contains("private credential")
        );
        let row_key = state.database_source_rows(&ctx).unwrap()[0]["key"]
            .as_str()
            .unwrap()
            .to_owned();
        state
            .database_source_row_action(&ctx, "connect", &row_key)
            .unwrap();
        host.answer_next_prompt(None);
        state
            .database_source_action(&ctx, "remove", Some(&id))
            .unwrap();
        assert!(project.data_sources().unwrap().contains(&id));
        host.answer_next_prompt(Some(&["yes"]));
        state
            .database_source_action(&ctx, "remove", Some(&id))
            .unwrap();
        assert!(!project.data_sources().unwrap().contains(&id));
        assert!(
            project
                .data_sources()
                .unwrap()
                .contains(&"other-plugin-workflow".to_owned())
        );
        assert!(
            state
                .set_database_credential(&ctx, &id, "private credential")
                .is_err()
        );
        assert!(
            state
                .database_source_action(&ctx, "connect", Some("other-plugin-workflow"))
                .is_err()
        );
        assert!(host.view_messages().is_empty());
        assert!(
            host.notifications()
                .iter()
                .all(|n| !n.body.contains("private credential"))
        );
    }
    #[test]
    fn database_row_keys_reject_other_projects_even_with_same_source_id() {
        let (host, state, id) = lifecycle_host();
        let first = host.invocation();
        let row = state.database_source_rows(&first).unwrap()[0]["key"]
            .as_str()
            .unwrap()
            .to_owned();
        host.set_project_root(Some("/work/second"));
        let second = host.invocation();
        let project = second.project().unwrap();
        project
            .upsert_data_source(&PluginDataSource {
                id: id.clone(),
                name: "Second default".into(),
                driver: "postgres".into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(state.database_sources(&second).unwrap(), vec![id.clone()]);
        assert!(
            state
                .database_source_action(&second, "connect", Some(&row))
                .is_err()
        );
        assert!(
            state
                .database_source_action(&second, "remove", Some(&row))
                .is_err()
        );
        assert!(project.data_sources().unwrap().contains(&id));
        assert!(
            state
                .database_source_row_action(&second, "connect", &id)
                .is_err()
        );
        // A raw ID remains valid for an explicit native command in the current project.
        state
            .database_source_action(&second, "connect", Some(&id))
            .unwrap();
        let current = state.database_source_rows(&second).unwrap()[0]["key"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_ne!(row, current);
        state
            .database_source_row_action(&second, "connect", &current)
            .unwrap();
    }
    #[test]
    fn database_source_lifecycle_denies_capabilities_trust_and_foreign_project() {
        let (host, state, id) = lifecycle_host();
        host.set_project_root(Some("/work/other"));
        assert!(
            state
                .database_sources(&host.invocation())
                .unwrap()
                .is_empty()
        );
        assert!(
            state
                .database_source_action(&host.invocation(), "connect", Some(&id))
                .is_err()
        );
        let host = openidea_plugin::mock::MockHost::install().with_capabilities(&["project.read"]);
        assert!(state.database_sources(&host.invocation()).is_err());
        let host = openidea_plugin::mock::MockHost::install()
            .with_capabilities(&["project.read", "database.sources"]);
        host.set_restricted(true);
        assert!(state.database_sources(&host.invocation()).is_err());
    }
    fn named_inventory() -> String {
        json!({"default":"primary","connections":[{"name":"primary","available":true,"driver":"mysql","host":"db","port":3306,"database":"app","username":"user","password":"PRIVATE_DATABASE_PASSWORD","url":"mysql://PRIVATE_DSN"},{"name":"analytics","available":true,"driver":"pgsql","host":"localhost","port":5432,"database":"analytics","username":"reader"}]}).to_string()
    }
    #[test]
    fn named_connections_are_curated_selected_and_recaptured_before_registration() {
        let host = openidea_plugin::mock::MockHost::install()
            .with_capabilities(&["project.read", "database.sources"]);
        host.add_file("artisan", "<?php");
        host.add_file("composer.json", "{}");
        let ctx = host.invocation();
        let state = State::default();
        let entries = connections(&named_inventory()).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(!json!(entries).to_string().contains("PRIVATE_"));
        host.answer_next_prompt(Some(&["analytics"]));
        host.answer_next_prompt(Some(&["yes"]));
        let mut probes = 0;
        state
            .register_database_with(
                &ctx,
                Runner::Php,
                |_, args| {
                    assert_eq!(args[2], "database-sources");
                    probes += 1;
                    Ok(named_inventory())
                },
                |_, _| panic!("Local source must not inspect Docker"),
            )
            .unwrap();
        assert_eq!(probes, 2);
        let ids = ctx.project().unwrap().data_sources().unwrap();
        assert_eq!(
            ids,
            vec![format!(
                "laravel-{}",
                crate::artisan_metadata::hash("analytics")
            )]
        );
        assert!(host.view_messages().is_empty());
    }
    #[test]
    fn named_registration_cancel_stale_settings_and_permissions_never_write() {
        let host = openidea_plugin::mock::MockHost::install()
            .with_capabilities(&["project.read", "database.sources"]);
        host.add_file("artisan", "<?php");
        host.add_file("composer.json", "{}");
        let ctx = host.invocation();
        let state = State::default();
        host.answer_next_prompt(None);
        state
            .register_database_with(
                &ctx,
                Runner::Php,
                |_, _| Ok(named_inventory()),
                |_, _| panic!(),
            )
            .unwrap();
        assert!(ctx.project().unwrap().data_sources().unwrap().is_empty());
        host.answer_next_prompt(Some(&["analytics"]));
        host.answer_next_prompt(None);
        state
            .register_database_with(
                &ctx,
                Runner::Php,
                |_, _| Ok(named_inventory()),
                |_, _| panic!(),
            )
            .unwrap();
        assert!(ctx.project().unwrap().data_sources().unwrap().is_empty());
        host.answer_next_prompt(Some(&["analytics"]));
        host.answer_next_prompt(Some(&["yes"]));
        let mut count = 0;
        assert!(
            state
                .register_database_with(
                    &ctx,
                    Runner::Php,
                    |_, _| {
                        count += 1;
                        let mut value: Value = serde_json::from_str(&named_inventory()).unwrap();
                        if count == 2 {
                            value["connections"][1]["host"] = json!("changed.example");
                        }
                        Ok(value.to_string())
                    },
                    |_, _| panic!()
                )
                .is_err()
        );
        assert!(ctx.project().unwrap().data_sources().unwrap().is_empty());
        host.answer_next_prompt(Some(&["analytics"]));
        assert!(
            state
                .register_database_with(
                    &ctx,
                    Runner::Php,
                    |_, _| {
                        host.set_project_root(Some("/work/foreign"));
                        Ok(named_inventory())
                    },
                    |_, _| panic!()
                )
                .is_err()
        );
        let host = openidea_plugin::mock::MockHost::install().with_capabilities(&["project.read"]);
        host.add_file("artisan", "<?php");
        host.add_file("composer.json", "{}");
        assert!(
            state
                .register_database_with(
                    &host.invocation(),
                    Runner::Php,
                    |_, _| panic!("denied capability must not probe"),
                    |_, _| panic!()
                )
                .is_err()
        );
        let host = openidea_plugin::mock::MockHost::install()
            .with_capabilities(&["project.read", "database.sources"]);
        host.add_file("artisan", "<?php");
        host.add_file("composer.json", "{}");
        host.set_restricted(true);
        assert!(
            state
                .register_database_with(
                    &host.invocation(),
                    Runner::Php,
                    |_, _| panic!("restricted project must not probe"),
                    |_, _| panic!()
                )
                .is_err()
        );
    }
    fn docker_fixture(id: &str, alias: &str, port: Option<&str>) -> Value {
        json!({"Id":id,"Image":format!("sha256:{}","c".repeat(64)),"Running":true,"Paused":false,"Restarting":false,"StartedAt":"2026-11-10T00:00:00Z","Project":"shop","Networks":{"default":{"NetworkID":"network-owned","Aliases":[alias],"IPAddress":if alias=="app"{"172.18.0.2"}else{"172.18.0.3"},"GlobalIPv6Address":""}},"Ports":if let Some(port)=port{json!({"3306/tcp":[{"HostIp":"0.0.0.0","HostPort":port}]})}else{json!({})}})
    }
    #[test]
    fn docker_mapping_requires_unique_same_project_network_and_published_port() {
        let app_id = "a".repeat(64);
        let db_id = "b".repeat(64);
        let app_raw = docker_fixture(&app_id, "app", None);
        let peer_raw = docker_fixture(&db_id, "db", Some("13306"));
        let app = docker_container(&app_raw.to_string(), &app_id).unwrap();
        let peer = docker_container(&peer_raw.to_string(), &db_id).unwrap();
        let connection = json!({"name":"primary","driver":"mysql","host":"db","port":3306,"database":"app","username":"user"});
        let (host, port, _) = mapped_endpoint(&connection, &app, &[peer.clone()]).unwrap();
        assert_eq!((host, port), ("127.0.0.1".into(), 13306));
        let mut foreign = peer.clone();
        foreign["Project"] = json!("other");
        assert!(mapped_endpoint(&connection, &app, &[foreign]).is_err());
        let mut network = peer.clone();
        network["networks"][0]["id"] = json!("foreign-network");
        assert!(mapped_endpoint(&connection, &app, &[network]).is_err());
        let mut duplicate = peer.clone();
        duplicate["Id"] = json!("d".repeat(64));
        assert!(mapped_endpoint(&connection, &app, &[peer.clone(), duplicate]).is_err());
        let mut unpublished = peer.clone();
        unpublished["ports"] = json!({});
        assert!(mapped_endpoint(&connection, &app, &[unpublished]).is_err());
        let mut public = peer.clone();
        public["ports"]["3306/tcp"][0]["host"] = json!("192.0.2.1");
        assert!(mapped_endpoint(&connection, &app, &[public]).is_err());
        let mut stopped = peer_raw.clone();
        stopped["Running"] = json!(false);
        assert!(docker_container(&stopped.to_string(), &db_id).is_err());
        let mut external = connection.clone();
        external["host"] = json!("external.example");
        assert!(mapped_endpoint(&external, &app, &[peer]).is_err());
    }
    #[test]
    fn docker_peer_discovery_and_evidence_detect_port_or_restart_changes() {
        let host = openidea_plugin::mock::MockHost::install();
        let project = host.invocation().project().unwrap();
        let app_id = "a".repeat(64);
        let peer_id = "b".repeat(64);
        let app = docker_fixture(&app_id, "app", None);
        let peer = docker_fixture(&peer_id, "db", Some("13306"));
        let binding = json!({"identity":{"socket":"unix:///var/run/docker.sock","container":app_id},"proof":{"StartedAt":app["StartedAt"],"Image":app["Image"]}});
        let connection =
            json!({"name":"primary","driver":"mysql","host":"db","port":3306,"database":"app"});
        let run = |_: &Project, args: &[String]| {
            assert_eq!(&args[..2], &["--host", "unix:///var/run/docker.sock"]);
            if args[2] == "ps" {
                Ok(format!("{peer_id}\n"))
            } else if args.last() == Some(&app_id) {
                Ok(app.to_string())
            } else {
                Ok(peer.to_string())
            }
        };
        let (mapped, proof) = docker_mapping(&project, &binding, &connection, run).unwrap();
        assert_eq!(mapped["host"], "127.0.0.1");
        assert_eq!(proof["peer"]["Id"], peer_id);
        let mut changed_peer = peer.clone();
        changed_peer["Ports"]["3306/tcp"][0]["HostPort"] = json!("23306");
        let changed = docker_mapping(&project, &binding, &connection, |_, args| {
            if args[2] == "ps" {
                Ok(format!("{peer_id}\n"))
            } else if args.last() == Some(&app_id) {
                Ok(app.to_string())
            } else {
                Ok(changed_peer.to_string())
            }
        })
        .unwrap();
        assert_ne!((mapped, proof), changed);
        let mut restarted = binding.clone();
        restarted["proof"]["StartedAt"] = json!("old");
        assert!(docker_mapping(&project, &restarted, &connection, run).is_err());
        let mut remote = binding;
        remote["identity"]["socket"] = json!("tcp://remote:2375");
        assert!(
            docker_mapping(&project, &remote, &connection, |_, _| panic!(
                "remote daemon must not run"
            ))
            .is_err()
        );
    }
    #[test]
    fn schema_is_curated_and_bounded() {
        let fixture = json!({"password":"secret","tables":[{"name":"posts","schema":"main","columns":[{"name":"id","type":"integer","nullable":false,"default":"secret"}],"indexes":[{"name":"posts_pk","columns":["id"],"unique":true,"primary":true}],"rows":"secret"}]});
        let rows = parse(&fixture.to_string()).unwrap();
        assert_eq!(rows[0]["schemaTable"]["columns"][0]["name"], "id");
        assert!(!json!(rows).to_string().contains("secret"));
        let mut bad = fixture.clone();
        bad["tables"][0]["indexes"][0]["columns"] = json!(["missing"]);
        assert!(parse(&bad.to_string()).is_err());
        assert!(
            parse(&json!({"tables":vec![fixture["tables"][0].clone();129]}).to_string()).is_err()
        );
    }
    #[test]
    fn richer_schema_descriptors_are_exact_optional_and_exclude_defaults() {
        let fixture = json!({"tables":[{"name":"products","columns":[
            {"name":"id","type":"bigint","fullType":"bigint unsigned","nullable":false,"autoIncrement":true,"unsigned":true},
            {"name":"price","type":"decimal","fullType":"decimal(10,2)","nullable":false,"precision":10,"scale":2,"default":"secret"},
            {"name":"state","type":"enum","fullType":"enum('new','ready')","nullable":false,"values":["new","ready"]},
            {"name":"code","type":"varchar","fullType":"varchar(64)","nullable":true,"length":64},
            {"name":"opaque","type":"custom","nullable":true}
        ],"indexes":[]}]});
        let rows = parse(&fixture.to_string()).unwrap();
        let columns = &rows[0]["schemaTable"]["columns"];
        assert_eq!(columns[0]["autoIncrement"], true);
        assert_eq!(columns[0]["unsigned"], true);
        assert_eq!(columns[1]["fullType"], "decimal(10,2)");
        assert_eq!(columns[1]["scale"], 2);
        assert_eq!(columns[2]["values"], json!(["new", "ready"]));
        assert_eq!(columns[3]["length"], 64);
        assert!(columns[4].get("unsigned").is_none());
        assert!(columns[4].get("autoIncrement").is_none());
        assert!(!json!(rows).to_string().contains("secret"));
        for (field, value) in [
            ("scale", json!(-1)),
            ("precision", json!(0)),
            ("length", json!(4294967296u64)),
            ("unsigned", json!("false")),
            ("values", json!(["a", "a"])),
        ] {
            let mut bad = fixture.clone();
            bad["tables"][0]["columns"][1][field] = value;
            assert!(
                parse(&bad.to_string()).is_err(),
                "accepted malformed {field}"
            );
        }
        let mut bad = fixture;
        bad["tables"][0]["columns"][1]["scale"] = json!(11);
        assert!(parse(&bad.to_string()).is_err());
    }
    #[test]
    fn captured_schema_is_owned_target_bound_and_retains_last_success() {
        let host = openidea_plugin::mock::MockHost::install();
        host.add_file("artisan", "<?php");
        host.add_file("composer.json", "{}");
        let ctx = host.invocation();
        let mut state = State::default();
        assert!(
            state.schema_snapshot(&ctx, Runner::Php).unwrap()["tables"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let payload = json!({"tables":[{"name":"users","columns":[{"name":"id","type":"integer","nullable":false}],"indexes":[]}]});
        state
            .capture_options_with(&ctx, Runner::Php, "schema", json!({}), |_, args| {
                assert_eq!(args[2], "schema");
                Ok(payload.to_string())
            })
            .unwrap();
        assert_eq!(
            state.schema_snapshot(&ctx, Runner::Php).unwrap()["tables"][0]["name"],
            "users"
        );
        assert!(
            state
                .capture_options_with(&ctx, Runner::Php, "schema", json!({}), |_, _| Ok(
                    "{}".into()
                ))
                .is_err()
        );
        assert_eq!(
            state.schema_snapshot(&ctx, Runner::Php).unwrap()["tables"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
    #[test]
    fn selected_empty_schema_keeps_connection_provenance_without_fake_tables() {
        let host = openidea_plugin::mock::MockHost::install();
        host.add_file("artisan", "<?php");
        host.add_file("composer.json", "{}");
        let ctx = host.invocation();
        let mut state = State::default();
        state
            .capture_options_with(
                &ctx,
                Runner::Php,
                "schema",
                json!({"connection":"analytics"}),
                |_, _| Ok(json!({"connection":"analytics","tables":[]}).to_string()),
            )
            .unwrap();
        let snapshot = state.schema_snapshot(&ctx, Runner::Php).unwrap();
        assert_eq!(snapshot["connection"], "analytics");
        assert!(snapshot["tables"].as_array().unwrap().is_empty());
    }
    #[test]
    fn native_source_never_transfers_url_password_or_options() {
        let host = openidea_plugin::mock::MockHost::install();
        let project = host.invocation().project().unwrap();
        let definition=source(&project,&json!({"name":"primary","driver":"pgsql","host":"localhost","port":"5432","database":"app","username":"user","password":"secret","url":"postgres://secret","options":{"password":"secret"}}).to_string()).unwrap();
        assert_eq!(definition.driver, "postgres");
        assert!(
            !serde_json::to_string(&definition)
                .unwrap()
                .contains("secret")
        );
        assert!(
            source(
                &project,
                &json!({"name":"x","driver":"sqlite","database":"/foreign/app.sqlite"}).to_string()
            )
            .is_err()
        );
    }
}

// Exact authored failures are localized only at native UI delivery boundaries.
fn ui_error(error: Error) -> Error {
    let translate = |message: String| -> String {
        match message.as_str() {
            "Invalid schema metadata text." => {
                crate::ui_text::t("runtime.error.0d92987c3838", &message)
            }
            "Expected up to 128 schema tables." => {
                crate::ui_text::t("runtime.error.7d45609fb8b7", &message)
            }
            "Duplicate schema table." => crate::ui_text::t("runtime.error.53679fbd84e2", &message),
            "Expected up to 256 table columns." => {
                crate::ui_text::t("runtime.error.b407f80c7add", &message)
            }
            "Duplicate schema column." => crate::ui_text::t("runtime.error.d8143db540ec", &message),
            "Expected column nullability." => {
                crate::ui_text::t("runtime.error.51b65c76fc55", &message)
            }
            "Expected explicit schema boolean." => {
                crate::ui_text::t("runtime.error.a3a477a7ea2d", &message)
            }
            "Invalid schema type dimension." => {
                crate::ui_text::t("runtime.error.eb2cc871640d", &message)
            }
            "Schema scale exceeds precision." => {
                crate::ui_text::t("runtime.error.bb88e49453df", &message)
            }
            "Expected up to 256 enum/set constants." => {
                crate::ui_text::t("runtime.error.376ec13f95ab", &message)
            }
            "Invalid enum/set constant." => {
                crate::ui_text::t("runtime.error.28020d1aaf01", &message)
            }
            "Duplicate enum/set constant." => {
                crate::ui_text::t("runtime.error.99286eef8562", &message)
            }
            "Expected up to 64 table indexes." => {
                crate::ui_text::t("runtime.error.b8163819a8f2", &message)
            }
            "Expected index columns." => crate::ui_text::t("runtime.error.cacfd1e92862", &message),
            "Index names an unknown column." => {
                crate::ui_text::t("runtime.error.4fc988e4da40", &message)
            }
            "Expected index uniqueness." => {
                crate::ui_text::t("runtime.error.fd8adf8c7478", &message)
            }
            "Expected primary index state." => {
                crate::ui_text::t("runtime.error.58f17df4d2c1", &message)
            }
            "Selected database driver is not supported for host registration." => {
                crate::ui_text::t("runtime.error.b23c66b151d2", &message)
            }
            "SQLite host registration requires an existing owned project database file." => {
                crate::ui_text::t("runtime.error.7cc8947bcce0", &message)
            }
            "Missing database host." => crate::ui_text::t("runtime.error.346a8862590f", &message),
            "Database host is not a plain hostname or address." => {
                crate::ui_text::t("runtime.error.16b782bcaada", &message)
            }
            "Expected up to 32 declared database connections." => {
                crate::ui_text::t("runtime.error.06d4bb83b393", &message)
            }
            "Invalid or duplicate connection name." => {
                crate::ui_text::t("runtime.error.18182c302f6a", &message)
            }
            "Missing connection availability." => {
                crate::ui_text::t("runtime.error.381b53b495f5", &message)
            }
            "Invalid database connection port." => {
                crate::ui_text::t("runtime.error.b4b989478d83", &message)
            }
            "Database mapping requires an exact running unpaused container." => {
                crate::ui_text::t("runtime.error.cdd43be87b1c", &message)
            }
            "Docker network inventory unavailable." => {
                crate::ui_text::t("runtime.error.29a9ec7cd805", &message)
            }
            "Docker published-port inventory unavailable." => {
                crate::ui_text::t("runtime.error.4d79f5971c89", &message)
            }
            "Docker native registration supports published MySQL/PostgreSQL TCP ports only." => {
                crate::ui_text::t("runtime.error.2768c2d7c816", &message)
            }
            "Docker peer mapping requires a verified Compose project label." => {
                crate::ui_text::t("runtime.error.5a70a563eeaa", &message)
            }
            "Database host has no unique same-project shared-network container mapping; configure a native source manually." => {
                crate::ui_text::t("runtime.error.60fe72f76ce0", &message)
            }
            "Database TCP port is not published to the host." => {
                crate::ui_text::t("runtime.error.3c1e29c52160", &message)
            }
            "Database has no proven loopback/wildcard published host port." => {
                crate::ui_text::t("runtime.error.c44678984a2e", &message)
            }
            "Database has ambiguous published host ports." => {
                crate::ui_text::t("runtime.error.ef23774da262", &message)
            }
            "Docker database peer inventory is invalid or too large." => {
                crate::ui_text::t("runtime.error.4ed08851761f", &message)
            }
            _ => message,
        }
    };
    match error {
        Error::Invalid(message) => Error::Invalid(translate(message)),
        Error::Failed(message) => Error::Failed(translate(message)),
        other => other,
    }
}

#[cfg(test)]
#[path="runtime_cloud_registration_tests.rs"]
mod cloud_registration_tests;
