//! Concrete command review. Source hints never establish the effective database.
use crate::*;
const LIMIT: usize = 128 << 10;
struct Review {
    project: Project,
    root: String,
    defaults: Option<String>,
    args: Vec<String>,
}
fn defaults(project: &Project) -> Result<Option<String>, Error> {
    match project.read_file("liquibase.properties") {
        Ok(text) if text.len() <= LIMIT => Ok(Some(text)),
        Ok(_) => Err("Root liquibase.properties exceeds the 128 KiB review limit; command not started.".into()),
        Err(Error::NotFound(_)) => Ok(None),
        Err(error) => Err(error),
    }
}
const URL_KEYS: &[&str] = &["url", "liquibase.command.url"];
const CHANGELOG_KEYS: &[&str] = &["changeLogFile", "liquibase.command.changelogFile", "liquibase.command.changeLogFile"];
/// A JDBC URL without credentials or parameters.
struct Target {
    url: String,
    /// "app on db.example:5432", "in-memory database app".
    label: String,
    kind: &'static str,
}
fn plain(value: &str, extra: &[u8]) -> bool {
    !value.is_empty() && value.bytes().all(|b| b.is_ascii_alphanumeric() || extra.contains(&b))
}
/// Parses common JDBC URLs; anything with user info, variables or an unknown shape gives `None`.
fn safe_jdbc(value: &str) -> Option<Target> {
    let value = value.trim();
    if value.len() > 1024 || value.contains('@') && !value.starts_with("jdbc:oracle:thin:@") {
        return None;
    }
    let server = |url: String, db: &str, host: &str, kind| {
        (plain(host, b".-:[]") && plain(db, b"._-/")).then(|| Target { url, label: format!("{db} on {host}"), kind })
    };
    for (prefix, kind) in [("jdbc:postgresql://", "PostgreSQL"), ("jdbc:mysql://", "MySQL"), ("jdbc:mariadb://", "MariaDB"), ("jdbc:db2://", "Db2"), ("jdbc:h2:tcp://", "H2")] {
        if let Some(rest) = value.strip_prefix(prefix) {
            let (host, db) = rest.split(['?', ';']).next()?.split_once('/')?;
            return server(format!("{prefix}{host}/{db}"), db, host, kind);
        }
    }
    if let Some(rest) = value.strip_prefix("jdbc:sqlserver://") {
        let mut parts = rest.split(';');
        let host = parts.next()?;
        let db = parts.find_map(|part| {
            let (key, db) = part.split_once('=')?;
            (key.eq_ignore_ascii_case("databaseName") || key.eq_ignore_ascii_case("database")).then_some(db)
        })?;
        return server(format!("jdbc:sqlserver://{host};databaseName={db}"), db, host, "SQL Server");
    }
    if let Some(rest) = value.strip_prefix("jdbc:oracle:thin:@") {
        let rest = rest.strip_prefix("//").unwrap_or(rest).split('?').next()?;
        let (host, db) = rest.split_once('/').or_else(|| rest.rsplit_once(':'))?;
        return server(format!("jdbc:oracle:thin:@{rest}"), db, host, "Oracle");
    }
    let embedded = |url: String, name: &str, label: String, kind| plain(name, b"._-/~").then(|| Target { url, label, kind });
    if let Some(name) = value.strip_prefix("jdbc:h2:mem:") {
        let name = name.split(';').next()?;
        return embedded(format!("jdbc:h2:mem:{name}"), name, format!("in-memory database {name}"), "H2");
    }
    if let Some(path) = value.strip_prefix("jdbc:h2:") {
        let path = path.split(';').next()?;
        let path = path.strip_prefix("file:").unwrap_or(path);
        return embedded(format!("jdbc:h2:{path}"), path, format!("file {path}"), "H2");
    }
    if let Some(path) = value.strip_prefix("jdbc:sqlite:") {
        let path = path.split('?').next()?;
        return embedded(format!("jdbc:sqlite:{path}"), path, format!("file {path}"), "SQLite");
    }
    None
}
/// The one value of `names`; `None` when missing or declared twice.
fn hint(text: &str, names: &[&str]) -> Option<String> {
    let keys = spring_text::config::keys("liquibase.properties", text);
    let hits: Vec<_> = keys.iter().filter(|key| names.contains(&key.key.as_str())).take(2).collect();
    let [key] = hits.as_slice() else { return None };
    Some(key.value.trim().to_owned())
}
fn changelog(text: &str) -> Option<String> {
    hint(text, CHANGELOG_KEYS).filter(|value| {
        value.len() <= 1024 && !value.starts_with('/') && !value.split('/').any(|p| matches!(p, "." | "..")) && plain(value, b"._-/")
    })
}
fn target(text: &str) -> Option<Target> {
    hint(text, URL_KEYS).and_then(|value| safe_jdbc(&value))
}
impl Review {
    fn capture(ctx: &Invocation, args: Vec<String>) -> Result<Self, Error> {
        let project = ctx.project().ok_or(Error::Stale)?;
        let root = project.root()?;
        if !root.starts_with('/') || root.len() > 4096 || root.chars().any(char::is_control) || root.contains('\\')
            || root.split('/').any(|p| matches!(p, "." | "..")) {
            return Err(Error::Stale);
        }
        if !host::projects()?.iter().any(|p| p.root().ok().as_deref() == Some(root.as_str())) { return Err(Error::Stale); }
        let defaults = defaults(&project)?;
        Ok(Self { project, root, defaults, args })
    }
    /// `title` holds `{db}`, replaced by the database label or "the configured database".
    fn prompt(&self, title: &str, effect: &str) -> (String, String) {
        let target = self.defaults.as_deref().and_then(target);
        let database = match (&self.defaults, &target) {
            (_, Some(target)) => format!("Database: {} ({}, from liquibase.properties)", target.label, target.kind),
            (None, None) => "Database: set by your Liquibase configuration (the project root has no liquibase.properties)".to_owned(),
            (Some(_), None) => "Database: not shown. liquibase.properties sets no plain JDBC URL here (it may use a variable or carry credentials), so Liquibase picks the database from its configuration.".to_owned(),
        };
        let changelog = self.defaults.as_deref().and_then(changelog).map_or_else(String::new, |path| format!("Changelog: {path}\n"));
        let title = title.replace("{db}", target.as_ref().map_or("the configured database", |target| target.label.as_str()));
        let body = format!(
            "{effect}\n\n{database}\n{changelog}Command: liquibase {}\nProject folder: {}\n\nEnvironment variables, JVM properties and Liquibase extensions can override liquibase.properties. Credentials are never shown.",
            self.args.join(" "),
            self.root
        );
        (title, body)
    }
    fn unchanged(&self, ctx: &Invocation) -> Result<(), Error> {
        if ctx.project().and_then(|p| p.root().ok()).as_deref() != Some(self.root.as_str())
            || !host::projects()?.iter().any(|p| p.root().ok().as_deref() == Some(self.root.as_str()))
            || defaults(&self.project)? != self.defaults {
            return Err(Error::Stale);
        }
        Ok(())
    }
}
/// What the user confirmed: the redacted source hints, never the effective database.
pub(crate) struct Reviewed {
    pub(crate) root: String,
    pub(crate) properties: Option<String>,
    /// Redacted JDBC target, `None` when unresolved.
    pub(crate) target: Option<String>,
    /// The changelog path from liquibase.properties, `None` when unresolved.
    pub(crate) changelog: Option<String>,
}
pub(crate) fn properties(project: &Project) -> Result<Option<String>, Error> { defaults(project) }
/// Shows the review and asks; `None` when declined. Nothing runs before this returns `Some`.
pub(crate) fn review(ctx: &Invocation, args: Vec<String>, title: &str, effect: &str) -> Result<Option<Reviewed>, Error> {
    let review = Review::capture(ctx, args)?;
    let (title, body) = review.prompt(title, effect);
    if !confirmed(&title, &body)? { return Ok(None); }
    review.unchanged(ctx)?;
    Ok(Some(Reviewed {
        target: review.defaults.as_deref().and_then(target).map(|target| target.url),
        changelog: review.defaults.as_deref().and_then(changelog),
        root: review.root,
        properties: review.defaults,
    }))
}
/// Runs `args` after the review; without a readable database URL the user also types the command name.
pub(crate) fn run(ctx: &Invocation, args: Vec<String>, title: &str, effect: &str) -> Result<(), Error> {
    let Some(reviewed) = review(ctx, args.clone(), title, effect)? else { return Ok(()) };
    if reviewed.target.is_none() {
        let word = args[0].as_str();
        let Some(answer) = host::input(&format!("The database could not be read from liquibase.properties. Type {word} to run it anyway"), None)? else {
            return Ok(());
        };
        if answer.trim() != word {
            host::notify(NotificationLevel::Info, TITLE, &format!("Nothing ran: you typed {:?}, not {word}.", answer.trim()));
            return Ok(());
        }
    }
    run_liquibase(args)
}
#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    #[test]
    fn prompt_leads_with_the_database_and_never_shows_credentials() {
        let host = MockHost::install();
        host.add_file("liquibase.properties", "url=jdbc:postgresql://db.example:5432/app?user=private&password=TOKEN_439\nchangeLogFile=db/master.yaml\nusername=private\npassword=TOKEN_439\n");
        let review = Review::capture(&host.invocation(), vec!["rollback-count".into(), "3".into()]).unwrap();
        let (title, body) = review.prompt("Roll back 3 changeset(s) in {db}?", "Rolls back.");
        assert_eq!(title, "Roll back 3 changeset(s) in app on db.example:5432?");
        assert!(body.starts_with("Rolls back.\n\nDatabase: app on db.example:5432 (PostgreSQL, from liquibase.properties)\nChangelog: db/master.yaml\nCommand: liquibase rollback-count 3\nProject folder: /project"), "{body}");
        for jargon in ["UNRESOLVED", "SHA256", "Contexts and labels"] { assert!(!body.contains(jargon), "{body}"); }
        for secret in ["private", "password=", "TOKEN_439", "?user="] { assert!(!body.contains(secret), "{secret}: {body}"); }
    }
    #[test]
    fn unknown_targets_use_plain_words() {
        let host = MockHost::install();
        let review = Review::capture(&host.invocation(), vec!["update".into()]).unwrap();
        let (title, body) = review.prompt("Update {db}?", "Applies.");
        assert_eq!(title, "Update the configured database?");
        assert!(body.contains("Database: set by your Liquibase configuration (the project root has no liquibase.properties)"), "{body}");
        host.add_file("liquibase.properties", "url=${DATABASE_URL}\n");
        let review = Review::capture(&host.invocation(), vec!["update".into()]).unwrap();
        let (_, body) = review.prompt("Update {db}?", "Applies.");
        assert!(body.contains("Database: not shown. liquibase.properties sets no plain JDBC URL here"), "{body}");
        assert!(!body.contains("Changelog:"), "{body}");
    }
    #[test]
    fn common_jdbc_urls_resolve_without_parameters() {
        for (url, label, kind, redacted) in [
            ("jdbc:mysql://host:3306/app?password=x", "app on host:3306", "MySQL", "jdbc:mysql://host:3306/app"),
            ("jdbc:sqlserver://sql.example:1433;databaseName=shop;password=x", "shop on sql.example:1433", "SQL Server", "jdbc:sqlserver://sql.example:1433;databaseName=shop"),
            ("jdbc:oracle:thin:@//ora.example:1521/ORCL", "ORCL on ora.example:1521", "Oracle", "jdbc:oracle:thin:@ora.example:1521/ORCL"),
            ("jdbc:oracle:thin:@ora.example:1521:XE", "XE on ora.example:1521", "Oracle", "jdbc:oracle:thin:@ora.example:1521:XE"),
            ("jdbc:h2:mem:test;DB_CLOSE_DELAY=-1", "in-memory database test", "H2", "jdbc:h2:mem:test"),
            ("jdbc:h2:file:./data/app;PASSWORD=secret", "file ./data/app", "H2", "jdbc:h2:./data/app"),
            ("jdbc:sqlite:app.db", "file app.db", "SQLite", "jdbc:sqlite:app.db"),
            ("jdbc:db2://db2.example:50000/SAMPLE", "SAMPLE on db2.example:50000", "Db2", "jdbc:db2://db2.example:50000/SAMPLE"),
        ] {
            let target = safe_jdbc(url).unwrap_or_else(|| panic!("{url}"));
            assert_eq!((target.label.as_str(), target.kind, target.url.as_str()), (label, kind, redacted));
        }
    }
    #[test]
    fn source_drift_foreign_roots_missing_scope_and_oversize_refuse_after_review() {
        let host = MockHost::install();
        let review = Review::capture(&host.invocation(), vec!["update".into()]).unwrap();
        host.add_file("liquibase.properties", "url=new\n");
        assert!(matches!(review.unchanged(&host.invocation()), Err(Error::Stale)));
        let review = Review::capture(&host.invocation(), vec!["update".into()]).unwrap();
        host.set_project_root(Some("/other"));
        assert!(matches!(review.unchanged(&host.invocation()), Err(Error::Stale)));
        host.set_project_root(None);
        assert!(Review::capture(&host.invocation(), vec!["update".into()]).is_err());
        host.set_project_root(Some("/project"));
        host.add_file("liquibase.properties", &" ".repeat(LIMIT + 1));
        assert!(Review::capture(&host.invocation(), vec!["update".into()]).is_err());
        assert!(host.terminals().is_empty());
    }
    #[test]
    fn ambiguous_or_credential_bearing_dynamic_sources_are_withheld() {
        for url in ["jdbc:postgresql://user:password@host/db", "${DATABASE_URL}", "jdbc:oracle:thin:scott/tiger@host:1521/XE", "jdbc:mysql://host/db%3Fpassword=secret", "jdbc:sqlserver://host;user=sa;password=x"] { assert!(safe_jdbc(url).is_none(), "{url}"); }
        assert!(target("url=jdbc:mysql://host/a\nliquibase.command.url=jdbc:mysql://host/b").is_none());
        assert!(changelog("changeLogFile=../private").is_none());
    }
}
