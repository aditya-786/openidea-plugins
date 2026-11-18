//! Native owned Database sources and a non-secret legacy file fallback.
use crate::model::{Description, text};
use openidea_plugin::{Error, PluginDataSource, Project};
use serde_json::Value;
use toml_edit::{ArrayOfTables, DocumentMut, Table, value};

pub const FILE: &str = ".openidea/data-sources.toml";
pub fn source_id(project: &Project) -> Result<String, Error> {
    // Project-specific seed for operation/storage keys and new sources. Existing
    // managed Database ids are reused after a move to avoid duplicate sources.
    let hash = project
        .root()?
        .bytes()
        .fold(0xcbf29ce484222325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        });
    Ok(format!("aritti.ddev.{hash:016x}"))
}
const MARKER: &str = "Managed by aritti.ddev";

fn managed_id(id: &str) -> bool {
    id.strip_prefix("aritti.ddev.")
        .is_some_and(|suffix| suffix.len() == 16 && suffix.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn reuse_id(preferred: String, ids: &[String]) -> Result<String, Error> {
    if ids.contains(&preferred) {
        return Ok(preferred);
    }
    let candidates: std::collections::BTreeSet<_> =
        ids.iter().filter(|id| managed_id(id)).collect();
    match candidates.len() {
        0 => Ok(preferred),
        1 => Ok((*candidates.first().unwrap()).clone()),
        _ => Err("Multiple managed DDEV data sources match this moved project; resolve the ambiguity before updating. Sources were left unchanged.".into()),
    }
}

// Session-only comparison, never logged or persisted. Retaining the exact
// credential avoids a lossy hash and detects rotation as well as port changes.
#[derive(PartialEq, Eq)]
pub(crate) struct Applied {
    source: PluginDataSource,
    password: String,
}

fn desired(project: &Project, desc: &Description, ids: &[String]) -> Result<Applied, Error> {
    let mut source = source(project, desc)?;
    source.id = reuse_id(source.id, ids)?;
    Ok(Applied {
        source,
        password: connection(desc)?
            .get("password")
            .and_then(Value::as_str)
            .ok_or("DDEV reported no database password.")?
            .into(),
    })
}

impl crate::Main {
    pub(crate) fn configure_database(
        &mut self,
        project: &Project,
        desc: &Description,
        force: bool,
    ) -> Result<bool, Error> {
        let root = project.root()?;
        match project.data_sources() {
            Ok(ids) => {
                let desired = desired(project, desc, &ids)?;
                if !force
                    && ids.contains(&desired.source.id)
                    && self.database_states.get(&root) == Some(&desired)
                {
                    // The native host disconnects even on an identical upsert.
                    // Do not rewrite settings OR credentials on a status read.
                    return Ok(false);
                }
                // A partial write can disconnect or replace the source before
                // a later credential error. Never retain a successful cache
                // marker for that failed attempt.
                self.database_states.remove(&root);
                self.connected_databases.remove(&root);
                configure_native(project, desc, &ids)?;
                self.database_states.insert(root, desired);
                Ok(true)
            }
            Err(Error::Denied(_)) => {
                self.database_states.remove(&root);
                self.connected_databases.remove(&root);
                configure_file(project, desc)
            }
            Err(error) => Err(error),
        }
    }
}

pub fn connection(desc: &Description) -> Result<&Value, Error> {
    desc.raw
        .get("dbinfo")
        .filter(|v| v.is_object())
        .ok_or_else(|| {
            "DDEV reported no database connection. Start the project and refresh.".into()
        })
}

pub fn summary(desc: &Description) -> Result<String, Error> {
    let db = connection(desc)?;
    Ok(format!(
        "Database: {} {}\nHost: 127.0.0.1\nPort: {}\nName: {}\nUser: {}\n\nConfigure Database Data Source creates or updates this project's DDEV connection, saves its password in the secure Database store and connects. It needs the database.sources permission; without it, the connection is saved to a file (project.write) and you enter the password in Database.",
        text(db, "database_type"),
        text(db, "database_version"),
        db.get("published_port").unwrap_or(&Value::Null),
        text(db, "dbname"),
        text(db, "username")
    ))
}

/// True for a newly created native source or a changed fallback file.
#[cfg(test)]
pub fn configure(project: &Project, desc: &Description) -> Result<bool, Error> {
    match project.data_sources() {
        // Preserve the previous non-secret file workflow when this distinct
        // optional capability is unavailable. Never fall back after a secure
        // store failure: that error must remain visible.
        Err(Error::Denied(_)) => configure_file(project, desc),
        Ok(ids) => configure_native(project, desc, &ids),
        Err(error) => Err(error),
    }
}

pub fn source(project: &Project, desc: &Description) -> Result<PluginDataSource, Error> {
    let db = connection(desc)?;
    let driver = match text(db, "database_type") {
        "mysql" | "mariadb" => "mysql",
        "postgres" | "postgresql" => "postgres",
        _ => return Err("Unsupported DDEV database type.".into()),
    };
    let port = db
        .get("published_port")
        .and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok()))
        .and_then(|port| u16::try_from(port).ok())
        .filter(|port| *port > 0)
        .ok_or("DDEV has no published database port; start the project first.")?;
    let name = text(db, "dbname");
    let user = text(db, "username");
    if name.is_empty() || user.is_empty() {
        return Err("DDEV returned incomplete database connection details.".into());
    }
    Ok(PluginDataSource {
        id: source_id(project)?,
        name: format!("DDEV: {}", desc.name),
        driver: driver.into(),
        host: Some("127.0.0.1".into()),
        port: Some(port),
        database: Some(name.into()),
        user: Some(user.into()),
        ..Default::default()
    })
}

fn configure_native(project: &Project, desc: &Description, ids: &[String]) -> Result<bool, Error> {
    let applied = desired(project, desc, ids)?;
    let source = applied.source;
    let password = applied.password;
    let created = !ids.contains(&source.id);
    project.upsert_data_source(&source)?;
    project.set_data_source_credential(&source.id, &password)?;
    // Retire only our old, explicitly marked file entry after secure setup
    // succeeds. A denied write leaves it intact for manual cleanup.
    if let Err(error) = retire_legacy(project, &source.id)
        && !matches!(error, Error::Denied(_))
    {
        openidea_plugin::host::log(
            openidea_plugin::LogLevel::Warn,
            &format!("DDEV legacy source left unchanged: {error}"),
        );
    }
    Ok(created)
}

fn retire_legacy(project: &Project, id: &str) -> Result<(), Error> {
    let old = match project.read_file(FILE) {
        Ok(text) => text,
        Err(Error::NotFound(_)) => return Ok(()),
        Err(error) => return Err(error),
    };
    let mut doc = old
        .parse::<DocumentMut>()
        .map_err(|e| Error::from(e.to_string()))?;
    let Some(sources) = doc
        .get_mut("data-source")
        .and_then(|item| item.as_array_of_tables_mut())
    else {
        return Ok(());
    };
    let mut matching: Vec<_> = sources
        .iter()
        .enumerate()
        .filter(|(_, table)| table.get("id").and_then(|v| v.as_str()) == Some(id))
        .map(|(index, _)| index)
        .collect();
    if matching.is_empty() {
        // A legacy file moves with its project, unlike the path-derived seed.
        // Retire just one explicitly marked old entry after native setup.
        matching = sources
            .iter()
            .enumerate()
            .filter(|(_, table)| {
                table.get("comment").and_then(|v| v.as_str()) == Some(MARKER)
                    && table
                        .get("id")
                        .and_then(|v| v.as_str())
                        .is_some_and(managed_id)
            })
            .map(|(index, _)| index)
            .collect();
    }
    if matching.len() != 1 {
        return Ok(());
    }
    let index = matching[0];
    if sources
        .get(index)
        .and_then(|table| table.get("comment"))
        .and_then(|v| v.as_str())
        != Some(MARKER)
    {
        return Ok(());
    }
    sources.remove(index);
    project.write_file(FILE, &doc.to_string())
}

pub fn connect(project: &Project) -> Result<(), Error> {
    let id = reuse_id(source_id(project)?, &project.data_sources()?)?;
    project.connect_data_source(&id)
}

fn configure_file(project: &Project, desc: &Description) -> Result<bool, Error> {
    let id = source_id(project)?;
    let db = connection(desc)?;
    let driver = match text(db, "database_type") {
        "mysql" | "mariadb" => "mysql",
        "postgres" | "postgresql" => "postgres",
        _ => return Err("Unsupported DDEV database type.".into()),
    };
    let port = db
        .get("published_port")
        .and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok()))
        .filter(|p| (1..=65535).contains(p))
        .ok_or("DDEV has no published database port; start the project first.")?;
    let name = text(db, "dbname");
    let user = text(db, "username");
    if name.is_empty() || user.is_empty() {
        return Err("DDEV returned incomplete database connection details.".into());
    }
    let old = match project.read_file(FILE) {
        Ok(s) => s,
        Err(Error::NotFound(_)) => String::new(),
        Err(e) => return Err(e),
    };
    let mut doc = old
        .parse::<DocumentMut>()
        .map_err(|e| Error::from(format!("Cannot read {FILE}: {e}")))?;
    if !doc.contains_key("data-source") {
        doc["data-source"] = toml_edit::Item::ArrayOfTables(ArrayOfTables::new());
    }
    let sources = doc["data-source"]
        .as_array_of_tables_mut()
        .ok_or("Expected [[data-source]] entries; configuration was left unchanged.")?;
    let id = if sources
        .iter()
        .any(|table| table.get("id").and_then(|v| v.as_str()) == Some(&id))
    {
        id
    } else {
        let ids = sources
            .iter()
            .filter(|table| table.get("comment").and_then(|v| v.as_str()) == Some(MARKER))
            .filter_map(|table| table.get("id").and_then(|v| v.as_str()).map(str::to_owned))
            .collect::<Vec<_>>();
        reuse_id(id, &ids)?
    };
    let matching: Vec<_> = sources
        .iter()
        .enumerate()
        .filter(|(_, table)| table.get("id").and_then(|v| v.as_str()) == Some(id.as_str()))
        .map(|(index, _)| index)
        .collect();
    if matching.len() > 1 {
        return Err("Multiple data sources use the DDEV plugin id; resolve the duplicate entries before updating. Configuration was left unchanged.".into());
    }
    let existing = matching.first().copied();
    let index = match existing {
        Some(i) => {
            if sources
                .get(i)
                .and_then(|t| t.get("comment"))
                .and_then(|v| v.as_str())
                != Some(MARKER)
            {
                return Err("A data source already uses the DDEV plugin id but is not plugin-managed; left unchanged.".into());
            }
            i
        }
        None => {
            sources.push(Table::new());
            sources.len() - 1
        }
    };
    let source = sources.get_mut(index).unwrap();
    for (key, val) in [
        ("id", id.as_str()),
        ("name", &format!("DDEV: {}", desc.name)),
        ("driver", driver),
        ("host", "127.0.0.1"),
        ("database", name),
        ("user", user),
        ("comment", MARKER),
    ] {
        source[key] = value(val);
    }
    source["port"] = value(port as i64);
    source["urlOnly"] = value(false);
    source["url"] = value("");
    let updated = doc.to_string();
    if updated == old {
        return Ok(false);
    }
    project.write_file(FILE, &updated)?;
    Ok(true)
}
