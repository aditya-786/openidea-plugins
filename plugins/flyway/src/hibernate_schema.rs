//! Explicit, version-qualified compiled Hibernate metadata -> selected-dialect creation SQL.
//! Never called by providers. This is a trusted project-code adapter, not a sandbox.
use super::*;
use openidea_plugin::host::Command;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};
pub(crate) const ACTION: &str = "hibernate-ddl:";
const PROFILE: &str = "hibernate-6.6.49-h2-v1";
const HELPER: &str = include_str!("../adapters/OfflineHibernateSchema.java");
const LOCK: &str = include_str!("../adapters/hibernate-6.6.49-runtime.json");
const LOCK7: &str = include_str!("../adapters/hibernate-7.4.15-runtime.json");
const PROFILE6: &str = "hibernate-6.6.49-offline-v2";
const PROFILE7: &str = "hibernate-7.4.15-offline-v2";
#[derive(Clone, Copy)]
struct RuntimeProfile {
    id: &'static str,
    version: &'static str,
    lock: &'static str,
}
fn runtime_profile(id: &str) -> Option<RuntimeProfile> {
    match id {
        PROFILE => Some(RuntimeProfile {
            id: PROFILE,
            version: "6.6.49.Final",
            lock: LOCK,
        }),
        PROFILE6 => Some(RuntimeProfile {
            id: PROFILE6,
            version: "6.6.49.Final",
            lock: LOCK,
        }),
        PROFILE7 => Some(RuntimeProfile {
            id: PROFILE7,
            version: "7.4.15.Final",
            lock: LOCK7,
        }),
        _ => None,
    }
}
#[derive(Clone, Copy)]
struct Dialect {
    key: &'static str,
    label: &'static str,
    class: &'static str,
}
const DIALECTS: &[Dialect] = &[
    Dialect {
        key: "h2",
        label: "H2",
        class: "org.hibernate.dialect.H2Dialect",
    },
    Dialect {
        key: "postgresql",
        label: "PostgreSQL",
        class: "org.hibernate.dialect.PostgreSQLDialect",
    },
    Dialect {
        key: "mysql",
        label: "MySQL",
        class: "org.hibernate.dialect.MySQLDialect",
    },
    Dialect {
        key: "mariadb",
        label: "MariaDB",
        class: "org.hibernate.dialect.MariaDBDialect",
    },
    Dialect {
        key: "sqlserver",
        label: "SQL Server",
        class: "org.hibernate.dialect.SQLServerDialect",
    },
    Dialect {
        key: "azure-sqlserver",
        label: "Azure SQL Server",
        class: "org.hibernate.dialect.AzureSQLServerDialect",
    },
    Dialect {
        key: "oracle",
        label: "Oracle",
        class: "org.hibernate.dialect.OracleDialect",
    },
    Dialect {
        key: "cockroachdb",
        label: "CockroachDB",
        class: "org.hibernate.dialect.CockroachDialect",
    },
    Dialect {
        key: "db2",
        label: "DB2",
        class: "org.hibernate.dialect.DB2Dialect",
    },
    Dialect {
        key: "db2i",
        label: "DB2 for IBM i",
        class: "org.hibernate.dialect.DB2iDialect",
    },
    Dialect {
        key: "db2z",
        label: "DB2 for z/OS",
        class: "org.hibernate.dialect.DB2zDialect",
    },
    Dialect {
        key: "hsql",
        label: "HSQLDB",
        class: "org.hibernate.dialect.HSQLDialect",
    },
    Dialect {
        key: "hana",
        label: "SAP HANA",
        class: "org.hibernate.dialect.HANADialect",
    },
    Dialect {
        key: "spanner",
        label: "Cloud Spanner (Hibernate7)",
        class: "org.hibernate.dialect.SpannerDialect",
    },
    Dialect {
        key: "spanner-postgresql",
        label: "Cloud Spanner PostgreSQL (Hibernate7)",
        class: "org.hibernate.dialect.SpannerPostgreSQLDialect",
    },
    Dialect {
        key: "postgres-plus",
        label: "Postgres Plus",
        class: "org.hibernate.dialect.PostgresPlusDialect",
    },
    Dialect {
        key: "derby",
        label: "Derby (Hibernate6)",
        class: "org.hibernate.dialect.DerbyDialect",
    },
    Dialect {
        key: "tidb",
        label: "TiDB (Hibernate6)",
        class: "org.hibernate.dialect.TiDBDialect",
    },
];
fn supported_dialects(runtime: RuntimeProfile) -> Vec<Dialect> {
    DIALECTS
        .iter()
        .copied()
        .filter(|dialect| {
            if runtime.id == PROFILE {
                return dialect.key == "h2";
            }
            match dialect.key {
                "derby" | "tidb" => runtime.version.starts_with("6."),
                "spanner" | "spanner-postgresql" => runtime.version.starts_with("7."),
                _ => true,
            }
        })
        .collect()
}
const CAP: usize = 128 << 10;
fn hash(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}
fn payload(stdout: &str) -> Result<&str, Error> {
    if stdout.len() > 2 * CAP {
        return Err(Error::Stale);
    }
    stdout
        .rsplit_once("OPENIDEA_HIBERNATE_SCHEMA/1\n")
        .map(|(_, payload)| payload.trim())
        .filter(|payload| !payload.is_empty())
        .ok_or_else(|| "Metadata helper returned no complete protocol payload.".into())
}
fn json(text: &str) -> Result<Value, Error> {
    if text.len() > 2 * CAP {
        return Err(Error::Stale);
    }
    serde_json::from_str::<comparison::Unique>(text)
        .map(|value| value.0)
        .map_err(|_| "The metadata adapter returned incomplete or invalid JSON.".into())
}
fn relative(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 1024
        && !value.starts_with('/')
        && !value.contains(['\\', ':', '|', '*', '?', '[', ']', '{', '}'])
        && !value.chars().any(char::is_control)
        && !value
            .split('/')
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
}
fn name(value: &str) -> bool {
    value.len() <= 256
        && !["java.", "javax.", "jakarta.", "org.hibernate."]
            .iter()
            .any(|prefix| value.starts_with(prefix))
        && value.split('.').all(|part| {
            part.starts_with(|ch: char| ch.is_ascii_alphabetic() || ch == '_' || ch == '$')
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_$".contains(&byte))
        })
}
fn strings(value: Option<&Value>, max: usize, required: bool) -> Result<Vec<String>, Error> {
    let Some(value) = value else {
        return if required {
            Err(Error::Stale)
        } else {
            Ok(vec![])
        };
    };
    let array = value
        .as_array()
        .filter(|array| array.len() <= max)
        .ok_or(Error::Stale)?;
    let mut seen = BTreeSet::new();
    array
        .iter()
        .map(|value| {
            let text = value
                .as_str()
                .filter(|text| relative(text))
                .ok_or(Error::Stale)?;
            if !seen.insert(text) {
                return Err(Error::Stale);
            }
            Ok(text.into())
        })
        .collect()
}
#[derive(Clone)]
struct Class {
    name: String,
    source: String,
    root: String,
}
#[derive(Clone)]
struct Plan {
    runtime_profile: RuntimeProfile,
    dialect: Dialect,
    root: String,
    profile: String,
    source: String,
    jars: Vec<String>,
    runtime: usize,
    roots: Vec<String>,
    sources: Vec<String>,
    entities: Vec<Class>,
    converters: Vec<Class>,
}
fn classes(
    value: Option<&Value>,
    roots: &[String],
    max: usize,
    required: bool,
) -> Result<Vec<Class>, Error> {
    let Some(value) = value else {
        return if required {
            Err(Error::Stale)
        } else {
            Ok(vec![])
        };
    };
    let mut seen = BTreeSet::new();
    value
        .as_array()
        .filter(|items| items.len() <= max)
        .ok_or(Error::Stale)?
        .iter()
        .map(|entry| {
            let object = entry
                .as_object()
                .filter(|entry| entry.len() == 3)
                .ok_or(Error::Stale)?;
            let name = object
                .get("class")
                .and_then(Value::as_str)
                .filter(|value| name(value))
                .ok_or(Error::Stale)?;
            let source = object
                .get("source")
                .and_then(Value::as_str)
                .filter(|value| {
                    relative(value) && (value.ends_with(".java") || value.ends_with(".kt"))
                })
                .ok_or(Error::Stale)?;
            let root = object
                .get("classRoot")
                .and_then(Value::as_str)
                .filter(|root| roots.iter().any(|candidate| candidate == root))
                .ok_or(Error::Stale)?;
            if !seen.insert(name) {
                return Err(Error::Stale);
            }
            Ok(Class {
                name: name.into(),
                source: source.into(),
                root: root.into(),
            })
        })
        .collect()
}
fn read_plan(project: &Project, profile: &str) -> Result<Plan, Error> {
    if !relative(profile) {
        return Err("Use a project-relative Hibernate profile path.".into());
    }
    let root = project.root()?;
    if host::project_of(&format!("{root}/{profile}"))
        .and_then(|owner| owner.root().ok())
        .as_deref()
        != Some(&root)
    {
        return Err(Error::Stale);
    }
    let source = project.read_file(profile)?;
    if source.len() > 32 << 10 {
        return Err("Hibernate profile exceeds 32KiB.".into());
    }
    let value = json(&source)?;
    let object = value.as_object().ok_or(Error::Stale)?;
    let runtime_profile = runtime_profile(
        value
            .get("profile")
            .and_then(Value::as_str)
            .ok_or(Error::Stale)?,
    )
    .ok_or("Choose a documented pinned Hibernate runtime profile.")?;
    if object.keys().any(|key| {
        ![
            "profile",
            "dialect",
            "runtimeJars",
            "projectJars",
            "classRoots",
            "entities",
            "converters",
        ]
        .contains(&key.as_str())
    }) {
        return Err("The Hibernate profile has unknown options.".into());
    }
    let dialects = supported_dialects(runtime_profile);
    let dialect_key = match value.get("dialect") {
        Some(value) => value.as_str().ok_or(Error::Stale)?.to_owned(),
        None if runtime_profile.id == PROFILE => "h2".into(),
        None => host::pick(
            "Target SQL dialect (offline default version; no database is queried)",
            &dialects
                .iter()
                .map(|dialect| openidea_plugin::PickItem::new(dialect.key, dialect.label))
                .collect::<Vec<_>>(),
        )?
        .ok_or(Error::Cancelled)?,
    };
    let dialect = dialects
        .into_iter()
        .find(|dialect| dialect.key == dialect_key)
        .ok_or(
            "This dialect has not passed the pinned offline mapping contract for this ORM version.",
        )?;
    let mut jars = strings(value.get("runtimeJars"), 32, true)?;
    let runtime = jars.len();
    let lock = json(runtime_profile.lock)?;
    let lock = lock.as_object().ok_or(Error::Stale)?;
    let names: BTreeSet<_> = jars
        .iter()
        .filter_map(|path| path.rsplit('/').next())
        .collect();
    if jars.len() != lock.len()
        || names.len() != lock.len()
        || !lock.keys().all(|name| names.contains(name.as_str()))
    {
        return Err("runtimeJars must name every exact dependency in the selected bundled runtime lock, once each.".into());
    }
    jars.extend(strings(value.get("projectJars"), 16, false)?);
    if jars.iter().any(|jar| !jar.ends_with(".jar"))
        || jars.iter().collect::<BTreeSet<_>>().len() != jars.len()
    {
        return Err(Error::Stale);
    }
    let roots = strings(value.get("classRoots"), 4, true)?;
    if roots.is_empty() {
        return Err(Error::Stale);
    }
    let entities = classes(value.get("entities"), &roots, 32, true)?;
    if entities.is_empty() {
        return Err("Select at least one compiled annotated entity root.".into());
    }
    let converters = classes(value.get("converters"), &roots, 16, false)?;
    let sources: Vec<_> = entities
        .iter()
        .chain(&converters)
        .map(|entity| entity.source.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    for path in std::iter::once(profile)
        .chain(jars.iter().map(String::as_str))
        .chain(roots.iter().map(String::as_str))
        .chain(sources.iter().map(String::as_str))
    {
        let full = format!("{root}/{path}");
        if host::project_of(&full)
            .and_then(|owner| owner.root().ok())
            .as_deref()
            != Some(&root)
        {
            return Err("Every profile, classpath and declared source must belong to this project, excluding nested foreign projects.".into());
        }
        project.stat(path)?;
    }
    Ok(Plan {
        runtime_profile,
        dialect,
        root,
        profile: profile.into(),
        source,
        jars,
        runtime,
        roots,
        sources,
        entities,
        converters,
    })
}
fn helper() -> Result<(String, String), Error> {
    let directory = host::fs::plugin_dir()?;
    let name = format!("HibernateSchema_{}.java", &hash(HELPER)[..20]);
    let path = format!("{directory}/{name}");
    let entries = host::fs::read_dir(&directory)?;
    if entries.len() >= 4096 {
        return Err(Error::Stale);
    }
    if entries.iter().any(|entry| entry.name == name) {
        if host::fs::read_bytes(&path)? != HELPER.as_bytes() {
            return Err("Private metadata helper changed; no execution performed.".into());
        }
    } else {
        host::fs::write_bytes(&path, HELPER.as_bytes())?;
    }
    Ok((directory, path))
}
impl Plan {
    fn current(&self, project: &Project) -> Result<(), Error> {
        if project.root()? != self.root || project.read_file(&self.profile)? != self.source {
            return Err(Error::Stale);
        }
        for path in self
            .jars
            .iter()
            .chain(&self.roots)
            .chain(&self.sources)
            .chain(std::iter::once(&self.profile))
        {
            if host::project_of(&format!("{}/{path}", self.root))
                .and_then(|owner| owner.root().ok())
                .as_deref()
                != Some(&self.root)
            {
                return Err(Error::Stale);
            }
        }
        Ok(())
    }
    fn command(
        &self,
        directory: &str,
        helper: &str,
        mode: &str,
        proof: Option<&str>,
    ) -> Result<Command, Error> {
        let mut args = vec![
            "-Dfile.encoding=UTF-8".into(),
            "--class-path".into(),
            directory.into(),
            helper.into(),
            mode.into(),
            self.root.clone(),
            self.runtime_profile.id.into(),
            self.dialect.key.into(),
        ];
        let entities: Vec<_> = self
            .entities
            .iter()
            .map(|entity| format!("{}|{}", entity.name, entity.root))
            .collect();
        let converters: Vec<_> = self
            .converters
            .iter()
            .map(|entity| format!("{}|{}", entity.name, entity.root))
            .collect();
        for values in [
            &self.jars,
            &self.roots,
            &self.sources,
            &entities,
            &converters,
        ] {
            args.push(values.len().to_string());
            args.extend(values.iter().cloned());
        }
        if let Some(proof) = proof {
            args.push(proof.into());
        }
        if args.len() > 256 || args.iter().map(String::len).sum::<usize>() > 48 << 10 {
            return Err("Metadata invocation exceeds the bounded argument contract.".into());
        }
        let mut command = Command::new("java").args(args);
        for name in [
            "JAVA_TOOL_OPTIONS",
            "JDK_JAVA_OPTIONS",
            "_JAVA_OPTIONS",
            "JAVA_OPTS",
            "CLASSPATH",
        ] {
            command = command.env(name, "");
        }
        Ok(command)
    }
    /// Cheap change detector for resolve: jar and compiled-class stats plus source hashes,
    /// so saving a snapshot never relaunches java.
    fn fingerprint(&self, project: &Project) -> Result<String, Error> {
        self.current(project)?;
        let mut facts = Vec::new();
        let stat = |path: &str| project.stat(path).map(|stat| format!("{path}:{}:{:?}", stat.size, stat.modified));
        for jar in &self.jars {
            facts.push(stat(jar)?);
        }
        // ponytail: find_files skips gitignored class roots, so the declared entity/converter class
        // files and the root directories are stat'ed by name; an undeclared nested class rebuilt in an
        // ignored root goes unnoticed until the 15-minute snapshot expiry. Re-inspect in a task if that matters.
        for root in &self.roots {
            facts.push(stat(root)?);
            let mut files = project.find_files(&format!("{root}/**"), 4097)?;
            if files.len() > 4096 {
                return Err("Compiled class roots exceed 4096 files.".into());
            }
            files.sort();
            for file in files {
                facts.push(stat(&file)?);
            }
        }
        for class in self.entities.iter().chain(&self.converters) {
            let file = format!("{}/{}.class", class.root, class.name.replace('.', "/"));
            facts.push(match project.stat(&file) {
                Err(Error::NotFound(_)) => format!("{file}:absent"),
                other => other.map(|stat| format!("{file}:{}:{:?}", stat.size, stat.modified))?,
            });
        }
        for source in &self.sources {
            facts.push(format!("{source}:{}", hash(&project.read_file(source)?)));
        }
        Ok(hash(&facts.join("\n")))
    }
    fn inspect(&self, project: &Project, directory: &str, helper: &str) -> Result<String, Error> {
        self.current(project)?;
        let output = host::run_command(
            &self.command(directory, helper, "inspect", None)?,
            Some(project),
            None,
        )?;
        if output.status != Some(0) {
            return Err("Java metadata inspection failed; use a supported JDK and saved, owned compiled classes/runtime files. No project metadata code was requested.".into());
        }
        let proof = payload(&output.stdout)?.to_owned();
        self.validate(project, &proof)?;
        self.current(project)?;
        Ok(proof)
    }
    fn validate(&self, project: &Project, proof: &str) -> Result<(), Error> {
        let value = json(proof)?;
        if value.get("profile").and_then(Value::as_str) != Some(self.runtime_profile.id)
            || value.get("version").and_then(Value::as_str) != Some(self.runtime_profile.version)
            || value.get("dialect").and_then(Value::as_str) != Some(self.dialect.class)
            || value.get("java").and_then(Value::as_str).is_none()
        {
            return Err(Error::Stale);
        }
        let digest = value
            .get("fingerprint")
            .and_then(Value::as_str)
            .ok_or(Error::Stale)?;
        if digest.len() != 64
            || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
            || value
                .get("files")
                .and_then(Value::as_u64)
                .is_none_or(|files| files > 4096)
            || value
                .get("bytes")
                .and_then(Value::as_u64)
                .is_none_or(|bytes| bytes > 128 << 20)
        {
            return Err(Error::Stale);
        }
        let facts = value
            .get("facts")
            .and_then(Value::as_array)
            .ok_or(Error::Stale)?;
        if facts.len()
            != self.jars.len()
                + self.roots.len()
                + self.sources.len()
                + self.entities.len()
                + self.converters.len()
        {
            return Err(Error::Stale);
        }
        let lock = json(self.runtime_profile.lock)?;
        let mut index = 0;
        for (jar, fact) in self.jars.iter().zip(facts) {
            let prefix = format!("jar:{jar}:");
            let digest = fact
                .as_str()
                .and_then(|fact| fact.strip_prefix(&prefix))
                .ok_or(Error::Stale)?;
            if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(Error::Stale);
            }
            if index < self.runtime
                && lock
                    .get(jar.rsplit('/').next().ok_or(Error::Stale)?)
                    .and_then(Value::as_str)
                    != Some(digest)
            {
                return Err("Hibernate runtime artifact does not match the bundled official dependency lock.".into());
            }
            index += 1;
        }
        for root in &self.roots {
            let prefix = format!("classes:{root}:");
            let digest = facts[index]
                .as_str()
                .and_then(|fact| fact.strip_prefix(&prefix))
                .ok_or(Error::Stale)?;
            if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(Error::Stale);
            }
            index += 1;
        }
        for source in &self.sources {
            let text = project.read_file(source)?;
            if text.len() > CAP
                || facts[index].as_str() != Some(&format!("source:{source}:{}", hash(&text)))
            {
                return Err("Declared source has unsaved/encoded changes or differs from the inspected disk bytes; save UTF-8 source first.".into());
            }
            index += 1;
        }
        for (label, classes) in [("entity", &self.entities), ("converter", &self.converters)] {
            for class in classes {
                if facts[index].as_str() != Some(&format!("{label}:{}|{}", class.name, class.root))
                {
                    return Err(Error::Stale);
                }
                index += 1;
            }
        }
        Ok(())
    }
}
#[derive(Clone)]
pub(crate) struct Candidate {
    database_version: String,
    plan: Plan,
    proof: String,
    fingerprint: String,
    pub sql: String,
    key: String,
    at: Instant,
}
impl Candidate {
    pub fn key(&self, root: &str) -> Option<String> {
        (self.plan.root == root && self.at.elapsed() < Duration::from_secs(900))
            .then(|| self.key.clone())
    }
    pub fn provenance(&self) -> String {
        format!(
            "Profile: {profile}\nHibernate{version}; {dialect} creation SQL only, not a live schema comparison.\nDialect target database version: {database_version} (runtime default, not a connected database).\nProject: {}\nManifest: {}\nCaptured{}s ago\n{}\nCompiled classes are authoritative; source fingerprints do not prove source/bytecode equivalence. This snapshot does not include unselected persistence units, runtime profiles, implicit application configuration or live DB state.",
            self.plan.root,
            self.plan.profile,
            self.at.elapsed().as_secs(),
            self.proof,
            profile = self.plan.runtime_profile.id,
            version = self.plan.runtime_profile.version,
            dialect = self.plan.dialect.label,
            database_version = self.database_version
        )
    }
    pub fn fresh(&self, project: &Project, key: &str) -> Result<(), Error> {
        if self.key(&project.root()?).as_deref() != Some(key) {
            return Err(Error::Stale);
        }
        if self.plan.fingerprint(project)? != self.fingerprint || self.plan.validate(project, &self.proof).is_err() {
            return Err("Metadata inputs changed. Generate and review a new DDL snapshot.".into());
        }
        Ok(())
    }
    pub fn affected(&self, path: &str) -> bool {
        self.plan
            .jars
            .iter()
            .chain(&self.plan.roots)
            .chain(&self.plan.sources)
            .chain(std::iter::once(&self.plan.profile))
            .any(|input| {
                let full = format!("{}/{input}", self.plan.root);
                path == full
                    || path.starts_with(&format!("{full}/"))
                    || full.starts_with(&format!("{}/", path.trim_end_matches('/')))
            })
    }
}
impl Flyway {
    pub(crate) fn generate_hibernate_schema(
        &mut self,
        ctx: &Invocation,
    ) -> Result<Option<String>, Error> {
        self.hibernate_schema = None;
        let project = ctx
            .project()
            .ok_or("Open the project with the reviewed compiled model first.")?;
        let Some(path) = host::input(
            "Project-relative optional Hibernate adapter profile",
            Some(".openidea/hibernate-schema.json"),
        )?
        else {
            return Ok(None);
        };
        // Two java launches (compile plus Hibernate bootstrap) outlast the 5 s command limit.
        let _task = host::begin_task("Generating Hibernate schema SQL", false);
        match self.capture_hibernate_schema(&project, &path) {
            Err(Error::Denied(_)) => {
                host::notify(
                    NotificationLevel::Warning,
                    TITLE,
                    &ui_kit::denied::message("Hibernate schema generation", "process:java", "Flyway Migrations"),
                );
                Ok(None)
            }
            result => result,
        }
    }
    fn capture_hibernate_schema(&mut self, project: &Project, path: &str) -> Result<Option<String>, Error> {
        let plan = read_plan(project, path)?;
        let (directory, helper) = helper()?;
        plan.fingerprint(project)?;
        let proof = plan.inspect(&project, &directory, &helper)?;
        let review = format!(
            "Project: {}\nProfile: {}\nHelper SHA256: {}\nExact inspected runtime/classpath/source fingerprints:\n{}\n\nRun this trusted compiled project model with Hibernate{version}/{dialect} to capture creation SQL only? The selected dialect uses its default target database version; no live version is inferred.\nNo application main, Spring bootstrapping, persistence.xml or SessionFactory is requested. Hibernate JDBC access is disabled and the supplied provider rejects every Hibernate connection request; no database action is configured.\nIMPORTANT: Hibernate metadata can instantiate project attribute converters, custom types, generators and service contributors. Their constructors/static initialization are trusted project code and can perform arbitrary side effects, including their own I/O/connections. This is NOT a process or network sandbox. JARs and all compiled-root resources are inspected; declared source fingerprints do not prove the compiled classes were built from those sources. Save/build the desired model yourself first. Only the explicitly selected annotated entity/converter roots are admitted; no effective application model is claimed.\nKnown JVM option/classpath environment keys are reserved empty: inherited collisions fail before launch. Filesystem changes during execution cause output rejection; final filesystem races remain possible. Generated SQL stays in an instance-owned preview until a separate reviewed non-overwriting FileAction.",
            plan.root,
            plan.profile,
            hash(HELPER),
            proof,
            version = plan.runtime_profile.version,
            dialect = plan.dialect.class
        );
        if !ui_kit::prompts::confirm_exact("Generate offline Hibernate schema SQL?", &review)? {
            return Ok(None);
        }
        plan.current(&project)?;
        let output = host::run_command(
            &plan.command(&directory, &helper, "generate", Some(&proof))?,
            Some(&project),
            None,
        )?;
        if output.status != Some(0) {
            return Err("Hibernate metadata generation failed or its inputs changed. No SQL was adopted; verify the supported runtime and explicitly selected model.".into());
        }
        let value = json(payload(&output.stdout)?)?;
        if value.get("profile").and_then(Value::as_str) != Some(plan.runtime_profile.id)
            || value.get("version").and_then(Value::as_str) != Some(plan.runtime_profile.version)
            || value.get("dialect").and_then(Value::as_str) != Some(plan.dialect.class)
            || value.get("proof").and_then(Value::as_str) != Some(&proof)
            || value.get("hibernateJdbcRequests").and_then(Value::as_u64) != Some(0)
        {
            return Err(Error::Stale);
        }
        let database_version = value
            .get("databaseVersion")
            .and_then(Value::as_str)
            .filter(|version| {
                !version.is_empty() && version.len() <= 80 && !version.chars().any(char::is_control)
            })
            .ok_or(Error::Stale)?
            .to_owned();
        let sql = value
            .get("sql")
            .and_then(Value::as_str)
            .filter(|sql| {
                !sql.trim().is_empty()
                    && sql.len() <= CAP
                    && !sql
                        .chars()
                        .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t'))
            })
            .ok_or(Error::Stale)?
            .to_owned();
        plan.current(&project)?;
        plan.validate(&project, &proof)?;
        let fingerprint = plan.fingerprint(&project)?;
        let key = hash(&format!(
            "{}\0{}\0{}\0{}",
            plan.root, plan.source, proof, sql
        ));
        let entities = plan.entities.len();
        let dialect = plan.dialect.label;
        self.hibernate_schema = Some(Candidate {
            database_version,
            plan,
            proof,
            fingerprint,
            sql,
            key,
            at: Instant::now(),
        });
        Ok(Some(format!(
            "Captured {dialect} creation SQL for {entities} selected compiled entity roots. Use ‘Save Hibernate schema SQL as migration…’ in an editor to review the exact SQL and create one undoable migration. No SQL execution was requested by the adapter."
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{
        file_actions::{FileActions, FileOperation},
        mock::MockHost,
        providers::Document,
    };
    fn setup() -> (MockHost, Flyway, Document, String) {
        setup_profile(PROFILE, Some("h2"))
    }
    fn setup_profile(
        id: &str,
        selected_dialect: Option<&str>,
    ) -> (MockHost, Flyway, Document, String) {
        let runtime = runtime_profile(id).unwrap();
        let dialect = supported_dialects(runtime)
            .into_iter()
            .find(|dialect| dialect.key == selected_dialect.unwrap_or("h2"))
            .unwrap();
        let host =
            MockHost::install().with_capabilities(&["project.read", "process:java", "storage"]);
        let lock = json(runtime.lock).unwrap();
        let jars: Vec<_> = lock
            .as_object()
            .unwrap()
            .keys()
            .map(|name| format!("runtime/{name}"))
            .collect();
        let source = "package example; @jakarta.persistence.Entity class Customer {}";
        let mut profile = serde_json::json!({"profile":id,"runtimeJars":jars,"projectJars":[],"classRoots":["classes"],"entities":[{"class":"example.Customer","source":"src/Customer.java","classRoot":"classes"}],"converters":[]});
        if let Some(dialect) = selected_dialect {
            profile["dialect"] = dialect.into();
        }
        let profile = profile.to_string();
        host.add_file("profile.json", &profile);
        host.add_file("src/Customer.java", source);
        host.add_dir("classes");
        host.add_file("classes/example/Customer.class", "mock compiled artifact");
        for jar in &jars {
            host.add_file(
                jar,
                "mock runtime artifact; actual runtime probe is separate",
            );
        }
        let sql_path = "/project/src/main/resources/db/migration/V1__init.sql";
        host.add_file(
            "src/main/resources/db/migration/V1__init.sql",
            "select 1;\n",
        );
        host.open_editor(sql_path, "sql", "select 1;\n", &[]);
        let mut facts: Vec<_> = jars
            .iter()
            .map(|jar| {
                format!(
                    "jar:{jar}:{}",
                    lock.get(jar.rsplit('/').next().unwrap())
                        .unwrap()
                        .as_str()
                        .unwrap()
                )
            })
            .collect();
        facts.push(format!("classes:classes:{}", "a".repeat(64)));
        facts.push(format!("source:src/Customer.java:{}", hash(source)));
        facts.push("entity:example.Customer|classes".into());
        let proof = serde_json::json!({"profile":id,"version":runtime.version,"dialect":dialect.class,"java":"26.0.1","fingerprint":"b".repeat(64),"facts":facts,"files":19,"bytes":2000}).to_string();
        (
            host,
            Flyway::default(),
            Document {
                path: sql_path.into(),
                language: "sql".into(),
                version: 1,
                text: "select 1;\n".into(),
            },
            proof,
        )
    }
    fn reply(host: &MockHost, proof: &str) {
        let proof = proof.to_owned();
        let configuration = json(&proof).unwrap();
        host.on_process("java", move |args, _| {
            let payload = if args[4] == "inspect" { proof.clone() } else {
                assert_eq!(args[4], "generate"); assert_eq!(args.last().unwrap(), &proof);
                serde_json::json!({"profile":configuration["profile"],"version":configuration["version"],"dialect":configuration["dialect"],"proof":proof,"hibernateJdbcRequests":0,"databaseVersion":"fixture-default","sql":"create table customers (id bigint primary key);\n"}).to_string()
            };
            Ok(openidea_plugin::ProcessOutput { status: Some(0), stdout: format!("OPENIDEA_HIBERNATE_SCHEMA/1\n{payload}\n"), stderr: String::new() })
        });
    }
    #[test]
    fn reviewed_compiled_metadata_is_preview_only_until_an_atomic_migration_action() {
        let (host, mut plugin, doc, proof) = setup();
        reply(&host, &proof);
        host.answer_next_prompt(Some(&["profile.json"]));
        host.answer_next_prompt(Some(&[]));
        let result = plugin
            .generate_hibernate_schema(&host.invocation())
            .unwrap()
            .unwrap();
        assert!(result.contains("No SQL execution was requested"));
        let runs = host.process_runs();
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].args[4], "inspect");
        assert_eq!(runs[1].args[4], "generate");
        assert!(runs.iter().all(|run| {
            run.env
                .iter()
                .any(|(key, value)| key == "JAVA_TOOL_OPTIONS" && value.is_empty())
        }));
        let action = plugin
            .file_actions(&doc, openidea_plugin::Range::default())
            .into_iter()
            .find(|action| action.title == "Save Hibernate schema SQL as migration…")
            .unwrap();
        for answer in ["2", "initial schema", "src/main/resources/db/migration"] {
            host.answer_next_prompt(Some(&[answer]));
        }
        host.answer_next_prompt(Some(&[]));
        let resolved = plugin.resolve_file_action(&doc, action).unwrap();
        assert_eq!(
            resolved.operations,
            [FileOperation::Create {
                path: "/project/src/main/resources/db/migration/V2__initial_schema.sql".into(),
                text: "create table customers (id bigint primary key);\n".into(),
                overwrite: false
            }]
        );
        assert!(
            !host
                .invocation()
                .project()
                .unwrap()
                .exists("src/main/resources/db/migration/V2__initial_schema.sql")
                .unwrap()
        );
        assert_eq!(host.process_runs().len(), 2, "save compares fingerprints instead of running java");
    }
    #[test]
    fn hibernate_command_runs_in_a_task() {
        let (host, mut plugin, _, proof) = setup();
        reply(&host, &proof);
        host.answer_next_prompt(Some(&["profile.json"]));
        host.answer_next_prompt(Some(&[]));
        plugin.run_command("hibernate-schema", &host.invocation(), None).unwrap();
        assert!(!host.tasks().is_empty());
        assert_eq!(host.process_runs().len(), 2);
    }
    #[test]
    fn resolve_runs_no_java() {
        let (host, mut plugin, doc, proof) = setup();
        reply(&host, &proof);
        host.answer_next_prompt(Some(&["profile.json"]));
        host.answer_next_prompt(Some(&[]));
        plugin.generate_hibernate_schema(&host.invocation()).unwrap();
        let runs = host.process_runs().len();
        let save = |plugin: &mut Flyway| {
            let action = plugin.file_actions(&doc, openidea_plugin::Range::default()).into_iter().find(|action| action.title.contains("Hibernate")).unwrap();
            for answer in ["2", "initial schema", "src/main/resources/db/migration"] {
                host.answer_next_prompt(Some(&[answer]));
            }
            host.answer_next_prompt(Some(&[]));
            plugin.resolve_file_action(&doc, action)
        };
        assert!(save(&mut plugin).is_ok());
        assert_eq!(host.process_runs().len(), runs);
        host.add_file("classes/example/Customer.class", "recompiled artifact with other bytes");
        assert!(save(&mut plugin).is_err(), "a changed compiled class fingerprint invalidates the snapshot");
        assert_eq!(host.process_runs().len(), runs);
    }
    #[test]
    fn java_denied_message() {
        let (host, mut plugin, _, _) = setup();
        let host = host.with_capabilities(&["project.read", "storage"]);
        host.answer_next_prompt(Some(&["profile.json"]));
        assert_eq!(plugin.run_command("hibernate-schema", &host.invocation(), None).unwrap(), None);
        assert_eq!(
            host.notifications().last().unwrap().body,
            "Hibernate schema generation needs the process:java permission. Allow it in Settings › Plugins › Flyway Migrations."
        );
    }
    #[test]
    fn cancelling_trust_review_never_loads_project_metadata_and_stale_inputs_cannot_save() {
        let (host, mut plugin, doc, proof) = setup();
        reply(&host, &proof);
        host.answer_next_prompt(Some(&["profile.json"]));
        host.answer_next_prompt(None);
        assert!(
            plugin
                .generate_hibernate_schema(&host.invocation())
                .unwrap()
                .is_none()
        );
        assert!(plugin.hibernate_schema.is_none());
        assert_eq!(host.process_runs().len(), 1);
        host.answer_next_prompt(Some(&["profile.json"]));
        host.answer_next_prompt(Some(&[]));
        plugin
            .generate_hibernate_schema(&host.invocation())
            .unwrap();
        let candidate = plugin.hibernate_schema.clone().unwrap();
        let key = candidate.key("/project").unwrap();
        assert!(candidate.key("/other").is_none());
        host.add_file("classes/example/Customer.class", "recompiled artifact with other bytes");
        assert!(
            candidate
                .fresh(&host.invocation().project().unwrap(), &key)
                .is_err()
        );
        assert!(
            plugin
                .file_actions(&doc, openidea_plugin::Range::default())
                .iter()
                .any(|action| action.title.contains("Hibernate")),
            "enumeration does not spawn to guess freshness; resolve verifies"
        );
    }
    #[test]
    fn runtime_lock_source_buffer_ownership_and_profile_shape_are_required() {
        let (host, _, _, proof) = setup();
        let project = host.invocation().project().unwrap();
        let plan = read_plan(&project, "profile.json").unwrap();
        let mut changed = json(&proof).unwrap();
        changed["facts"][0] = Value::String(format!("jar:{}:{}", plan.jars[0], "0".repeat(64)));
        assert!(plan.validate(&project, &changed.to_string()).is_err());
        host.open_editor("/project/src/Customer.java", "java", "unsaved source", &[]);
        assert!(plan.validate(&project, &proof).is_err());
        assert!(
            !relative("../outside.jar")
                && !relative("/absolute.jar")
                && !relative("runtime/*".replace('*', "..").as_str())
        );
        host.add_file(
            "profile.json",
            "{\"profile\":\"hibernate-6.6.49-h2-v1\",\"profile\":\"other\"}",
        );
        assert!(read_plan(&project, "profile.json").is_err());
        assert!(host.process_runs().is_empty());
    }
    #[test]
    fn qualified_profiles_bind_runtime_and_dialect_and_latest_jpa_runtime() {
        for (id, dialect, expected_version) in [
            (PROFILE6, "postgresql", "6.6.49.Final"),
            (PROFILE6, "mysql", "6.6.49.Final"),
            (PROFILE7, "mariadb", "7.4.15.Final"),
            (PROFILE7, "spanner-postgresql", "7.4.15.Final"),
        ] {
            let (host, mut plugin, _, proof) = setup_profile(id, Some(dialect));
            reply(&host, &proof);
            host.answer_next_prompt(Some(&["profile.json"]));
            host.answer_next_prompt(Some(&[]));
            let message = plugin
                .generate_hibernate_schema(&host.invocation())
                .unwrap()
                .unwrap();
            assert!(message.contains("Captured"));
            let candidate = plugin.hibernate_schema.as_ref().unwrap();
            assert!(candidate.provenance().contains(expected_version));
            assert_eq!(candidate.plan.dialect.key, dialect);
            let mut wrong = json(&proof).unwrap();
            wrong["dialect"] = "org.hibernate.dialect.OracleDialect".into();
            assert!(
                candidate
                    .plan
                    .validate(&host.invocation().project().unwrap(), &wrong.to_string())
                    .is_err()
            );
            let runs = host.process_runs();
            assert_eq!(runs[0].args[6], id);
            assert_eq!(runs[0].args[7], dialect);
        }
    }
    #[test]
    fn explicit_dialect_chooser_can_cancel_and_does_not_admit_mapping_loss_presets() {
        let (host, _, _, _) = setup_profile(PROFILE6, None);
        host.answer_next_prompt(Some(&["postgresql"]));
        let plan = read_plan(&host.invocation().project().unwrap(), "profile.json").unwrap();
        assert_eq!(plan.dialect.key, "postgresql");
        assert!(host.process_runs().is_empty());
        host.answer_next_prompt(None);
        assert!(matches!(
            read_plan(&host.invocation().project().unwrap(), "profile.json"),
            Err(Error::Cancelled)
        ));
        let mut profile = json(
            &host
                .invocation()
                .project()
                .unwrap()
                .read_file("profile.json")
                .unwrap(),
        )
        .unwrap();
        profile["dialect"] = "spanner".into();
        host.add_file("profile.json", &profile.to_string());
        assert!(read_plan(&host.invocation().project().unwrap(), "profile.json").is_err());
        assert_eq!(
            supported_dialects(runtime_profile(PROFILE6).unwrap()).len(),
            16
        );
        assert_eq!(
            supported_dialects(runtime_profile(PROFILE7).unwrap()).len(),
            16
        );
    }

    #[test]
    fn real_generated_ddl_fixtures_are_pinned_per_dialect() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/hibernate-adapter/expected");
        for (version, dialect, marker) in [
            ("6.6.49", "postgresql", "create sequence customers_SEQ"),
            ("6.6.49", "mysql", "engine=InnoDB"),
            ("6.6.49", "mariadb", "engine=InnoDB"),
            ("7.4.15", "postgresql", "create sequence"),
            ("7.4.15", "mysql", "engine=InnoDB"),
            ("7.4.15", "mariadb", "engine=InnoDB"),
        ] {
            let sql =
                std::fs::read_to_string(dir.join(format!("hibernate-{version}-{dialect}.sql")))
                    .unwrap();
            assert!(
                sql.contains(marker) && sql.contains("uq_customer_email"),
                "{version} {dialect}"
            );
        }
    }
}
