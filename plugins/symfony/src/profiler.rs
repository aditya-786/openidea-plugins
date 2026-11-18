//! Explicit project-owned profiler-index snapshots. No serialized collector decoding.
use crate::*;
use openidea_plugin::FileKind;
use serde_json::json;
use std::hash::BuildHasher;
pub const INDEX_FILE: &str = "var/cache/dev/profiler/index.csv";
pub const MAX_ROWS: usize = 100;
const MAX_BYTES: usize = 1024 * 1024;
const MAX_RECORDS: usize = 10_000;
const OUTSIDE_PROJECT: &str = "The profiler index is outside this project. Use Choose CSV Source… to pick one inside it, then Refresh.";
#[derive(Debug, PartialEq, Eq)]
pub struct Request {
    pub line: u32,
    pub token: String,
    pub ip: String,
    pub method: String,
    pub url: String,
    pub time: i64,
    pub status: String,
}
/// `YYYY-MM-DD HH:MM:SS` in UTC.
pub fn timestamp(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let rest = seconds.rem_euclid(86_400);

    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);

    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

// A single physical CSV record; multiline fields are deliberately unsupported.
fn fields(line: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut field = String::new();
    let mut chars = line.chars().peekable();
    let mut quoted = false;
    let mut closed = false;
    while let Some(c) = chars.next() {
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    quoted = false;
                    closed = true;
                }
            } else {
                field.push(c);
            }
        } else if c == ',' {
            out.push(std::mem::take(&mut field));
            closed = false;
        } else if c == '"' && field.is_empty() && !closed {
            quoted = true;
        } else if c == '"' || closed {
            return None;
        } else {
            field.push(c);
        }
    }
    if quoted {
        return None;
    }
    out.push(field);
    Some(out)
}
fn valid_token(token: &str) -> bool {
    (1..=64).contains(&token.len()) && token.bytes().all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}
/// Credentials/query/fragment never enter the snapshot or rendering model.
fn display_url(raw: &str) -> String {
    let query = raw.contains('?');
    let fragment = raw.contains('#');
    let mut base = raw.split(['?', '#']).next().unwrap_or("").to_owned();
    if let Some((scheme, rest)) = base.split_once("://") {
        if !matches!(scheme, "http" | "https") {
            return "Unsupported URL form (hidden)".into();
        }
        let end = rest.find('/').unwrap_or(rest.len());
        let authority = &rest[..end];
        base = format!(
            "{scheme}://{}{}",
            authority.rsplit('@').next().unwrap_or(""),
            &rest[end..]
        );
    } else if !base.starts_with('/') || base.starts_with("//") {
        return "Unsupported URL form (hidden)".into();
    }
    if query {
        base.push_str(" [query present; hidden]");
    }
    if fragment {
        base.push_str(" [fragment present; hidden]");
    }
    base
}
fn parse(text: &str) -> Result<(Vec<Request>, usize), &'static str> {
    if text.len() > MAX_BYTES {
        return Err("The profiler index is larger than 1 MiB. Clear old profiles with cache:clear, then Refresh.");
    }
    let mut requests = Vec::new();
    let mut seen = BTreeSet::new();
    let mut ambiguous = BTreeSet::new();
    for (n, line) in text.lines().enumerate() {
        if n >= MAX_RECORDS {
            return Err("The profiler index has more than 10,000 lines. Clear old profiles with cache:clear, then Refresh.");
        }
        if line.is_empty() {
            continue;
        }
        // Refuse malformed records rather than accidentally treating a multiline continuation as a request.
        if line.len() > 16 * 1024 || line.chars().any(|c| c.is_control() && c != '\t') {
            return Err("The profiler index has an unreadable line. Clear old profiles with cache:clear, then Refresh.");
        }
        let mut f = fields(line).ok_or("The profiler index is not valid CSV. Clear old profiles with cache:clear, then Refresh.")?;
        // Symfony 6.4+ appends virtual_type; older storage has seven columns.
        if f.len() == 8 {
            let kind=f.pop().unwrap();
            if !kind.is_empty() && kind != "request" { continue; }
        }
        let [token, ip, method, url, time, parent, status] = <[String; 7]>::try_from(f)
            .map_err(|_| "This file is not a Symfony profiler index. Use Choose CSV Source… to pick profiler/index.csv.")?;
        if !parent.is_empty() {
            continue;
        }
        if !valid_token(&token) {
            continue;
        }
        let identity = token.to_ascii_lowercase();
        if !seen.insert(identity.clone()) {
            ambiguous.insert(identity);
        }
        let Some(time) = time
            .parse::<i64>()
            .ok()
            .filter(|t| (0..=253402300799).contains(t))
        else {
            continue;
        };
        if !method.bytes().all(|b| b.is_ascii_uppercase())
            || method.is_empty()
            || method.len() > 16
            || status
                .parse::<u16>()
                .ok()
                .is_none_or(|s| !(100..=599).contains(&s))
        {
            continue;
        }
        requests.push(Request {
            line: n as u32,
            token,
            ip,
            method,
            url: display_url(&url),
            time,
            status,
        });
    }
    requests.retain(|r| !ambiguous.contains(&r.token.to_ascii_lowercase()));
    requests.sort_by_key(|r| std::cmp::Reverse(r.time));
    let total = requests.len();
    Ok((requests, total))
}
#[derive(Default)]
pub struct Snapshot {
    root: String,
    source: String,
    scope: String,
    rows: Option<Vec<Request>>,
    total: usize,
    page: usize,
    status: String,
    details: Option<crate::profiler_details::Details>,
    selected: Option<String>,
}
const PATH_SETTING: &str = "profilerIndexPath";
fn configured_source() -> Result<String, &'static str> {
    let value = match host::setting(PATH_SETTING) {
        None => INDEX_FILE.to_owned(),
        Some(raw) => {
            if raw.len() > 32 << 10 {
                return Err("The Profiler index path setting is longer than 32 KiB. Shorten it in Settings › Plugins › Symfony.");
            }
            serde_json::from_str::<String>(&raw)
                .map_err(|_| "The Profiler index path setting must be text. Fix it in Settings › Plugins › Symfony.")?
        }
    };
    normalized_source(&value)
}
fn normalized_source(value: &str) -> Result<String, &'static str> {
    let source = value.trim();
    let source = if source.is_empty() {
        INDEX_FILE
    } else {
        source
    };
    if source.len() > 4096
        || source.starts_with('/')
        || source.chars().any(char::is_control)
        || source.contains(['\\', ':', '*', '?', '[', ']', '{', '}'])
        || source.split('/').count() > 32
        || source
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
    {
        return Err(
            "The profiler index path must be relative to the project, at most 32 folders deep, without .., wildcards or a leading /.",
        );
    }
    Ok(source.into())
}
fn preference_key(root: &str) -> String {
    let hash = root.bytes().fold(0xcbf29ce484222325u64, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3));
    format!("profiler-source:{hash:016x}")
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SourcePreference { version: u8, root: String, source: String }
fn project_source(root: &str) -> Result<String, &'static str> {
    let raw = host::storage::get(&preference_key(root)).map_err(|_| "The saved profiler CSV source for this project can't be read. Use Choose CSV Source… to set it again.")?;
    let Some(raw) = raw else { return configured_source(); };
    if raw.len() > 32 << 10 { return Err("The saved profiler CSV source for this project is invalid. Use Choose CSV Source… to set it again."); }
    let value: SourcePreference = serde_json::from_str(&raw).map_err(|_| "The saved profiler CSV source for this project is invalid. Use Choose CSV Source… to set it again.")?;
    if value.version != 1 || value.root != root { return Err("The saved profiler CSV source for this project is invalid. Use Choose CSV Source… to set it again."); }
    if value.source.trim().is_empty() { return Err("The saved profiler CSV source for this project is invalid. Use Choose CSV Source… to set it again."); }
    normalized_source(&value.source)
}
/// Stores only a project-bound preference. Reading its CSV is a separate explicit action.
pub(crate) fn choose_source(ctx: &Invocation) -> Result<bool, Error> {
    let project = ctx.project().ok_or(Error::Stale)?;
    let root = project.root()?;
    if root.is_empty() || root.len() > 4096 { return Err(Error::Stale); }
    let key = preference_key(&root);
    let baseline = host::storage::get(&key)?;
    let initial = project_source(&root).ok();
    let Some(answer) = host::input("Profiler CSV path for this project (blank uses global/default setting)", initial.as_deref())? else { return Ok(false); };
    if project.root()? != root || host::storage::get(&key)? != baseline
        || host::project_of(&format!("{}/composer.json", root.trim_end_matches('/'))).and_then(|p|p.root().ok()).as_deref() != Some(root.as_str()) {
        return Err(Error::Stale);
    }
    if answer.len() > 32 << 10 { return Err(Error::Invalid("That path is too long (over 32 KiB).".into())); }
    if answer.trim().is_empty() { host::storage::delete(&key)?; }
    else {
        let source = normalized_source(&answer).map_err(|reason| Error::Invalid(reason.into()))?;
        host::storage::set(&key, &json!({"version":1,"root":root,"source":source}).to_string())?;
    }
    Ok(true)
}
fn discovered_sources(project: &Project, root: &str) -> Vec<String> {
    index::profiler_container_paths().into_iter().filter_map(|container| {
        let candidate = container.rsplit_once('/').map(|(parent, _)| format!("{parent}/profiler/index.csv")).unwrap_or_else(|| "profiler/index.csv".into());
        let source = normalized_source(&candidate).ok()?;
        if !owned_index(root, &container) || !owned_index(root, &source)
            || !project.stat(&container).is_ok_and(|stat| stat.kind == FileKind::File)
            || !project.stat(&source).is_ok_and(|stat| stat.kind == FileKind::File) { return None; }
        Some(source)
    }).collect::<BTreeSet<_>>().into_iter().collect()
}
/// Explicit bounded metadata discovery. No PHP execution, CSV reads or automatic adoption.
pub(crate) fn discover_source(ctx: &Invocation) -> Result<bool, Error> {
    let project = ctx.project().ok_or(Error::Stale)?;
    let root = project.root()?;
    if root.is_empty() || root.len() > 4096 { return Err(Error::Stale); }
    let key = preference_key(&root);
    let baseline = host::storage::get(&key)?;
    let candidates = discovered_sources(&project, &root);
    if candidates.is_empty() { return Err(Error::NotFound("No profiler data yet: load a page in dev mode, then try again, or use Choose CSV Source…".into())); }
    let items: Vec<_> = candidates.iter().enumerate().map(|(i, path)| openidea_plugin::PickItem::new(&i.to_string(), path)).collect();
    let Some(chosen) = host::pick("Choose a profiler index", &items)? else { return Ok(false); };
    let selected = chosen.parse::<usize>().ok().and_then(|i| candidates.get(i)).ok_or(Error::Stale)?;
    if project.root()? != root || host::storage::get(&key)? != baseline
        || discovered_sources(&project, &root) != candidates { return Err(Error::Stale); }
    host::storage::set(&key, &json!({"version":1,"root":root,"source":selected}).to_string())?;
    Ok(true)
}
fn index_path(root: &str, source: &str) -> String {
    format!("{}/{}", root.trim_end_matches('/'), source)
}
fn owned_index(root: &str, source: &str) -> bool {
    !root.is_empty()
        && host::project_of(&index_path(root, source))
            .and_then(|p| p.root().ok())
            .as_deref()
            == Some(root)
}
fn source_scope(root: &str, source: &str) -> String {
    let random = std::collections::hash_map::RandomState::new();
    format!(
        "{:016x}{:016x}",
        random.hash_one((root, source, "profiler source")),
        random.hash_one((root, source, "profiler rows"))
    )
}
impl Snapshot {
    pub fn select(&mut self, key: &str, ctx:&Invocation) {
        self.selected=None;
        let Some(root)=ctx.project().and_then(|p|p.root().ok()) else {return;};
        if root!=self.root || project_source(&root).ok().as_deref()!=Some(self.source.as_str()) {return;}
        if let Some(row)=self.rows.as_ref().and_then(|rows|rows.iter().find(|r|self.row_key(&r.token)==key)) {self.selected=Some(row.token.clone());}
    }
    /// The cached index rows as (token, method, redacted URL, status, time), newest first.
    pub(crate) fn requests(&self) -> Option<Vec<(String, String, String, String, i64)>> {
        self.rows.as_ref().map(|rows| rows.iter().map(|r| (r.token.clone(), r.method.clone(), r.url.clone(), r.status.clone(), r.time)).collect())
    }
    pub fn close_details(&mut self) { self.details = None; }
    /// Moves within the cached snapshot only; never re-reads the index.
    pub fn turn_page(&mut self, forward: bool) {
        let pages = self.rows.as_ref().map_or(0, |r| r.len().div_ceil(MAX_ROWS));
        self.page = if forward { (self.page + 1).min(pages.saturating_sub(1)) } else { self.page.saturating_sub(1) };
    }
    pub fn inspect(&mut self, ctx: &Invocation) -> Result<(), Error> {
        self.details = None;
        let project=ctx.project().ok_or(Error::Stale)?;
        let root=project.root()?;
        let source=project_source(&root).map_err(|_|Error::Stale)?;
        if root != self.root || source != self.source || !owned_index(&root,&source) {return Err(Error::Stale);}
        let rows=self.rows.as_ref().ok_or(Error::Stale)?;
        let items:Vec<_>=rows.iter().map(|r|PickItem::new(&r.token,&format!("{} {} {}",r.token,r.method,r.url))).collect();
        let token=if let Some(token)=&self.selected {token.clone()} else {
            let Some(token)=host::pick("Choose a request to inspect",&items)? else {return Ok(());};token
        };
        let prior=rows.iter().find(|r|r.token==token).ok_or(Error::Stale)?;
        let (fresh,_)=load(&project,&source).map_err(|_|Error::Stale)?;
        if project_source(&root).ok().as_deref()!=Some(source.as_str()) || !fresh.iter().any(|r|r==prior) {return Err(Error::Stale);}
        let details=crate::profiler_details::load(&project,&root,&source,&token)?;
        let (after,_)=load(&project,&source).map_err(|_|Error::Stale)?;
        if project_source(&root).ok().as_deref()!=Some(source.as_str()) || !after.iter().any(|r|r==prior) {return Err(Error::Stale);}
        self.details=Some(details);Ok(())
    }

    /// Metadata-only invalidation: never adopt new CSV records on filesystem events.
    pub(crate) fn invalidate_changes(&mut self, paths: &BTreeSet<&str>, roots: &BTreeSet<String>, overflow: bool) -> bool {
        if self.root.is_empty() { return false; }
        if !roots.contains(&self.root) {
            *self = Self::default();
            return true;
        }
        let target = index_path(&self.root, &self.source);
        let affected = overflow || paths.iter().any(|path| {
            let path = path.trim_end_matches('/');
            !path.is_empty() && (target == path || target.strip_prefix(path).is_some_and(|rest| rest.starts_with('/')) || self.details.is_some() && target.rsplit_once('/').is_some_and(|(parent,_)| path.starts_with(&format!("{parent}/"))))
        });
        if !affected || self.rows.is_none() { return false; }
        let status = "The profiler index changed. Refresh to see new requests.";
        let removed_details=self.details.take().is_some();
        if self.status == status { return removed_details; }
        self.status = status.into();
        true
    }
    pub fn refresh(&mut self, ctx: &Invocation) {
        self.details = None;
        let Some(project) = ctx.project() else {
            *self = Self::default();
            return;
        };
        let Ok(root) = project.root() else {
            *self = Self::default();
            return;
        };
        if root.is_empty() {
            *self = Self::default();
            return;
        }
        let source = match project_source(&root) {
            Ok(source) => source,
            Err(reason) => {
                *self = Self {
                    root,
                    status: reason.to_owned(),
                    ..Self::default()
                };
                return;
            }
        };
        if self.root != root || self.source != source {
            *self = Self {
                root: root.clone(),
                source: source.clone(),
                scope: source_scope(&root, &source),
                ..Self::default()
            };
        }
        let loaded = load(&project, &source);
        match loaded {
            Ok((rows, total)) => {
                self.rows = Some(rows);
                self.total = total;
                self.page = 0;
                self.status="Requests as of the last Refresh.".into();
            }
            Err(reason) => {
                self.status = format!(
                    "{reason}{}",
                    if self.rows.is_some() { " Showing the requests from the previous Refresh." } else { "" }
                )
            }
        }
    }
    /// Opens only the current owned index record, never the URL or decoded profile.
    pub fn open(&self, key: &str, ctx: &Invocation) -> Result<(), Error> {
        let project = ctx.project().ok_or(Error::Stale)?;
        let root = project.root()?;
        let source = project_source(&root).map_err(|_| Error::Stale)?;
        if root.is_empty() || root != self.root || source != self.source || self.scope.is_empty() {
            return Err(Error::Stale);
        }
        let prefix = format!("request:{}:", self.scope);
        let token = key
            .strip_prefix(&prefix)
            .filter(|token| valid_token(token))
            .ok_or(Error::Stale)?;
        let prior = self
            .rows
            .as_ref()
            .and_then(|rows| rows.iter().find(|r| r.token.to_ascii_lowercase() == token))
            .ok_or(Error::Stale)?;
        let path = index_path(&root, &source);
        if !owned_index(&root, &source) {
            return Err(Error::Stale);
        }
        let (rows, _) = load(&project, &source).map_err(|_| Error::Stale)?;
        // Equality includes the physical line and every retained displayed request fact.
        // Hidden URL-only/CSV-format changes are accepted; no exact byte identity is claimed.
        let current = rows
            .iter()
            .find(|r| r.token.to_ascii_lowercase() == token)
            .filter(|r| *r == prior)
            .ok_or(Error::Stale)?;
        if project_source(&root).ok().as_deref() != Some(source.as_str())
            || !owned_index(&root, &source)
        {
            return Err(Error::Stale);
        }
        host::open_location(&path, current.line, 0)
    }
    /// Opaque source-lease key; root/path/current record checks remain authoritative.
    pub(crate) fn row_key(&self, token: &str) -> String {
        format!("request:{}:{}", self.scope, token.to_ascii_lowercase())
    }
    pub fn render(&self, ctx: &Invocation) -> View {
        self.render_mode(ctx, ui_kit::web::is_rendering())
    }
    fn render_mode(&self, ctx: &Invocation, packaged: bool) -> View {
        let refresh = Button::new("refresh-profiler", "Refresh").icon("refresh");
        let Some(root) = ctx
            .project()
            .and_then(|p| p.root().ok())
            .filter(|r| !r.is_empty())
        else {
            return View::message("No project is open.").button(refresh);
        };
        let choose = Button::new("choose-profiler-source", "Choose CSV Source…").icon("folder-open");
        let discover = Button::new("discover-profiler-source", "Find Cached CSV Source").icon("search");
        let source = match project_source(&root) {
            Ok(source) => source,
            Err(reason) => return View::message(reason).button(refresh).button(choose).button(discover),
        };
        if self.root != root || self.source != source {
            return View::message(&format!("Refresh to read this project's profiler index ({source}).")).button(refresh).button(choose).button(discover);
        }
        if !owned_index(&root, &source) {
            return View::message(OUTSIDE_PROJECT).button(refresh).button(choose).button(discover);
        }
        if let Some(details)=&self.details { return details.view(); }
        let Some(all_rows) = self.rows.as_ref() else {
            return View::message(&self.status).button(refresh).button(choose).button(discover);
        };
        let start = (self.page * MAX_ROWS).min(all_rows.len());
        let rows = &all_rows[start..(start + MAX_ROWS).min(all_rows.len())];
        let columns = ["Time (UTC)", "Method", "Status", "URL", "IP", "Token"];
        let cells = |r: &Request| {
            vec![
                timestamp(r.time),
                r.method.clone(),
                r.status.clone(),
                r.url.clone(),
                r.ip.clone(),
                r.token.clone(),
            ]
        };
        let body = if packaged {
            let items:Vec<_>=rows.iter().map(|r|json!({"key":self.row_key(&r.token),"label":format!("{} {}",r.method,r.url),"cells":cells(r),"details":[{"label":"Details","value":"Use Inspect Profile to see its collectors"},{"label":"Source","value":self.source},{"label":"Source line","value":(r.line+1).to_string()},{"label":"Token","value":r.token},{"label":"URL","value":"Query, fragment and credentials are hidden"}]})).collect();
            Body::Other {
                kind: "resources".into(),
                data: json!({"columns":columns,"facets":[1,2],"items":items}).to_string(),
            }
        } else {
            Body::Table(Table {
                columns: columns.map(String::from).to_vec(),
                total_rows: rows.len() as u32,
                first_row: 0,
                rows: rows.iter().map(cells).collect(),
                editable: false,
            })
        };
        let mut view = View::new(body).button(refresh).button(choose).button(discover).button(Button::new("inspect-profile-details", "Inspect Profile").icon("info"));
        if self.page > 0 {
            view = view.button(Button::new("profiler-previous-page", "Newer Requests").icon("arrow-left"));
        }
        if start + rows.len() < all_rows.len() {
            view = view.button(Button::new("profiler-next-page", "Older Requests").icon("arrow-right"));
        }
        view.status(&format!(
            "{} Showing {}-{} of {} main requests (newest first, {} per page). Source: {}",
            self.status,
            if rows.is_empty() { 0 } else { start + 1 },
            start + rows.len(),
            self.total,
            MAX_ROWS,
            self.source
        ))
    }
}
fn load(project: &Project, source: &str) -> Result<(Vec<Request>, usize), &'static str> {
    let root = project.root().map_err(read_error)?;
    if !owned_index(&root, &source) {
        return Err(OUTSIDE_PROJECT);
    }
    let stat = project.stat(source).map_err(read_error)?;
    if stat.kind != FileKind::File {
        return Err("The profiler index path is not a file. Use Choose CSV Source… to fix it.");
    }
    if stat.size > MAX_BYTES as u64 {
        return Err("The profiler index is larger than 1 MiB. Clear old profiles with cache:clear, then Refresh.");
    }
    let text = project.read_file(source).map_err(read_error)?;
    parse(&text)
}
fn read_error(error: Error) -> &'static str {
    match error {
        Error::Denied(_) => "The IDE refused to read the profiler index. Check that it is inside the project, then Refresh.",
        Error::NotFound(_) => "No profiler data yet: load a page in dev mode, then Refresh.",
        _ => "The profiler index can't be read. Check the file, then Refresh.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    const ROW: &str = "abcdef,127.0.0.1,GET,https://user:password@example.test/page?token=secret#private,1700000000,,200\n";
    #[test]
    fn selected_profile_is_not_run_when_permission_is_denied_and_cancel_is_inert() {
        let host=MockHost::install();host.add_file(INDEX_FILE,ROW);host.add_file("var/cache/dev/profiler/ef/cd/abcdef","profile");
        let mut state=Snapshot::default();let ctx=host.invocation();state.refresh(&ctx);
        host.answer_next_prompt(None);state.inspect(&ctx).unwrap();assert!(host.process_runs().is_empty());
        state.select(&state.row_key("abcdef"),&ctx);
        assert!(matches!(state.inspect(&ctx),Err(Error::Invalid(m)) if m.contains("process:php permission. Allow it in Settings › Plugins › Symfony")));assert!(state.details.is_none());assert!(host.process_runs().is_empty());
        state.select("request:foreign:abcdef",&ctx);assert!(state.selected.is_none());
    }
    #[test]
    fn explicit_collector_adapter_requires_fresh_owned_request_and_has_inert_render() {
        let host=MockHost::install().with_capabilities(&["project.read","process:php","storage"]);
        host.add_file(INDEX_FILE,ROW);
        host.add_file("var/cache/dev/profiler/ef/cd/abcdef","profile");
        host.on_process("php",|args,stdin| {
            assert!(stdin.is_none());
            assert_eq!(&args[..2],["-n","-d"]);
            assert!(args.iter().any(|s|s.contains("'allowed_classes' => false")));
            assert_eq!(&args[args.len()-4..],["--","/project","/project/var/cache/dev/profiler/ef/cd/abcdef","abcdef"]);
            Ok(openidea_plugin::ProcessOutput{status:Some(0),stdout:r#"{"version":2,"token":"abcdef","collectors":[{"name":"time","class":"TimeDataCollector","metrics":{"duration":1.25}}]}"#.into(),stderr:String::new()})
        });
        let mut state=Snapshot::default();let ctx=host.invocation();state.refresh(&ctx);
        state.select(&state.row_key("abcdef"),&ctx);state.inspect(&ctx).unwrap();
        let Body::Table(table)=state.render(&ctx).body else {panic!()};assert_eq!(table.columns[0],"Collector");assert_eq!(table.rows.len(),2);
        assert_eq!(host.process_runs().len(),1);state.render(&ctx);assert_eq!(host.process_runs().len(),1);
        let paths=BTreeSet::from(["/project/var/cache/dev/profiler/ef/cd/abcdef"]);let roots=BTreeSet::from(["/project".into()]);
        assert!(state.invalidate_changes(&paths,&roots,false));assert!(state.details.is_none());
        state.inspect(&ctx).unwrap();assert!(state.details.is_some());
        assert!(state.invalidate_changes(&paths,&roots,false));assert!(state.details.is_none());
        assert_eq!(host.process_runs().len(),2);
        state.close_details();let Body::Table(table)=state.render(&ctx).body else {panic!()};assert_eq!(table.columns[0],"Time (UTC)");
        host.add_file(INDEX_FILE,"abcdef,ip,POST,/changed,1,,500\n");
        host.answer_next_prompt(Some(&["abcdef"]));assert!(matches!(state.inspect(&ctx),Err(Error::Stale)));assert_eq!(host.process_runs().len(),2);
    }
    #[test]
    fn modern_storage_virtual_type_preserves_requests_and_omits_command_profiles() {
        let (rows, total)=parse("abcdef,ip,GET,/page,1,,200,request\n123abc,ip,GET,/command,2,,200,command\nabc123,ip,GET,/legacy,3,,200\n").unwrap();
        assert_eq!(total,2);assert_eq!(rows.iter().map(|r|r.token.as_str()).collect::<Vec<_>>(),["abc123","abcdef"]);
        for token in ["a","custom-TOKEN_1"] {assert_eq!(parse(&format!("{token},ip,GET,/page,1,,200,request\n")).unwrap().0[0].token,token);}
        assert!(!valid_token("../outside"));
    }
    #[test]
    fn latest_bounded_unambiguous_rows_and_timestamp() {
        let text: String = (0..150)
            .map(|i| format!("{i:06x},ip,GET,/u{i},{},,200\n", 1000 + i))
            .collect();
        let (rows, total) = parse(&text).unwrap();
        assert_eq!((rows.len(), total), (150, 150));
        assert_eq!(rows[0].token, "000095");
        assert_eq!(timestamp(1700000000), "2023-11-14 22:13:20");
        assert!(parse(&(ROW.to_owned() + ROW)).unwrap().0.is_empty());
        assert!(
            parse("ABCDEF,ip,GET,/x,1,,200\nabcdef,ip,GET,/y,2,,200\n")
                .unwrap()
                .0
                .is_empty()
        );
        assert!(parse("../foo,ip,GET,/x,1,,200\n").unwrap().0.is_empty());
    }
    #[test]
    fn pages_walk_cached_rows_newest_first_without_rereading_the_index() {
        let text: String = (0..250).map(|i| format!("{i:06x},ip,GET,/u{i},{},,200\n", 1000 + i)).collect();
        let host = MockHost::install();
        host.add_file(INDEX_FILE, &text);
        let ctx = host.invocation();
        let mut state = Snapshot::default();
        state.refresh(&ctx);
        let first = |s: &Snapshot| {
            let View { body: Body::Table(t), toolbar: buttons, status, .. } = s.render(&ctx) else { panic!() };
            (t.rows.len(), t.rows[0][5].clone(), buttons.iter().map(|b| b.id.clone()).collect::<Vec<_>>(), status)
        };
        let (n, token, ids, status) = first(&state);
        assert_eq!((n, token.as_str()), (100, "0000f9"));
        assert!(ids.contains(&"profiler-next-page".to_owned()) && !ids.contains(&"profiler-previous-page".to_owned()));
        assert!(status.unwrap().contains("Showing 1-100 of 250"));
        host.add_file(INDEX_FILE, "malformed");
        state.turn_page(true);
        state.turn_page(true);
        state.turn_page(true);
        let (n, token, ids, status) = first(&state);
        assert_eq!((n, token.as_str()), (50, "000031"));
        assert!(!ids.contains(&"profiler-next-page".to_owned()) && ids.contains(&"profiler-previous-page".to_owned()));
        assert!(status.unwrap().contains("Showing 201-250 of 250"));
        state.turn_page(false);
        state.turn_page(false);
        state.turn_page(false);
        assert_eq!(first(&state).1, "0000f9");
    }
    #[test]
    fn csv_quoting_is_strict_and_subrequests_are_not_promoted() {
        let (r, _) =
            parse("abcdef,ip,GET,\"/search?a=1,2\",1,,200\n123abc,ip,GET,/fragment,2,abcdef,200\n")
                .unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].url, "/search [query present; hidden]");
        assert_eq!(
            fields("\"a\"\"b\",c"),
            Some(vec!["a\"b".into(), "c".into()])
        );
        for bad in [
            "abcdef,ip,GET,\"/a\n/b\",1,,200\n",
            "abcdef,ip,GET,\"/a\"oops,1,,200",
            "abcdef,ip,GET,/a,1,,200,request,extra",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }
    #[test]
    fn credentials_query_and_fragment_never_enter_snapshot() {
        let (r, _) = parse(ROW).unwrap();
        assert_eq!(
            r[0].url,
            "https://example.test/page [query present; hidden] [fragment present; hidden]"
        );
        for hidden in ["user", "password", "secret", "private"] {
            assert!(!r[0].url.contains(hidden));
        }
        assert_eq!(
            display_url("//user:password@host/a"),
            "Unsupported URL form (hidden)"
        );
        assert_eq!(
            display_url("javascript:secret"),
            "Unsupported URL form (hidden)"
        );
    }
    #[test]
    fn all_input_limits_reject_before_adopting_partial_data() {
        assert!(parse(&"x".repeat(MAX_BYTES + 1)).is_err());
        assert!(parse(&"\n".repeat(MAX_RECORDS + 1)).is_err());
        assert!(parse(&format!("abcdef,ip,GET,/{},1,,200", "x".repeat(16 * 1024))).is_err());
        assert!(
            parse("abcdef,ip,GET,/x,9223372036854775807,,200")
                .unwrap()
                .0
                .is_empty()
        );
    }
    #[test]
    fn discovery_requires_owned_existing_container_and_csv_and_explicit_choice() {
        let host = MockHost::install();
        let ctx = host.invocation();
        let project = ctx.project().unwrap();
        let root = project.root().unwrap();
        host.add_file(INDEX_FILE, ROW);
        assert!(discovered_sources(&project, &root).is_empty(), "CSV alone is not container discovery");
        assert!(discover_source(&ctx).is_err());
        host.add_file("var/cache/dev/App_KernelDevDebugContainer.xml", "<container/>");
        host.add_file("var/cache/dev/srcApp_KernelDevDebugContainer.xml", "<container/>");
        assert_eq!(discovered_sources(&project, &root), [INDEX_FILE]);
        host.set_setting("compiledContainers", "\"custom/cache/container.xml,missing/container.xml,../escape/container.xml\"");
        host.add_file("custom/cache/container.xml", "<container/>");
        host.add_file("custom/cache/profiler/index.csv", "malformed CSV remains selectable metadata");
        host.add_file("missing/profiler/index.csv", ROW);
        let expected = ["custom/cache/profiler/index.csv", INDEX_FILE];
        assert_eq!(discovered_sources(&project, &root), expected);
        host.answer_next_prompt(None);
        assert!(!discover_source(&ctx).unwrap());
        assert_eq!(project_source(&root).unwrap(), INDEX_FILE);
        host.answer_next_prompt(Some(&["99"]));
        assert!(discover_source(&ctx).is_err());
        host.answer_next_prompt(Some(&["0"]));
        assert!(discover_source(&ctx).unwrap());
        assert_eq!(project_source(&root).unwrap(), expected[0]);
        let mut snapshot = Snapshot::default();
        snapshot.refresh(&ctx);
        assert!(snapshot.rows.is_none(), "selection does not make malformed CSV valid");
        host.set_setting("compiledContainers", "\"container.xml\"");
        host.add_file("container.xml", "<container/>");
        host.add_file("profiler/index.csv", ROW);
        assert_eq!(discovered_sources(&project, &root), ["profiler/index.csv", INDEX_FILE]);
        assert!(host.requests().is_empty() && host.process_runs().is_empty());
    }

    #[test]
    fn project_source_preference_is_root_bound_bounded_and_recoverable() {
        let host = MockHost::install();
        let ctx = host.invocation();
        let root = ctx.project().unwrap().root().unwrap();
        host.set_setting(PATH_SETTING, "\"global/index.csv\"");
        assert_eq!(project_source(&root).unwrap(), "global/index.csv");
        host.answer_next_prompt(Some(&[" custom/profiler.csv "]));
        assert!(choose_source(&ctx).unwrap());
        assert_eq!(project_source(&root).unwrap(), "custom/profiler.csv");
        assert_eq!(project_source("/other").unwrap(), "global/index.csv");
        let prior = host::storage::get(&preference_key(&root)).unwrap();
        host.answer_next_prompt(None);
        assert!(!choose_source(&ctx).unwrap());
        host.answer_next_prompt(Some(&["../outside.csv"]));
        assert!(choose_source(&ctx).is_err());
        assert_eq!(host::storage::get(&preference_key(&root)).unwrap(), prior);
        for raw in ["null", "{}", r#"{"version":1,"root":"/other","source":"a.csv"}"#, r#"{"version":1,"root":"/project","source":"a.csv","source":"b.csv"}"#] {
            host::storage::set(&preference_key(&root), raw).unwrap();
            assert!(project_source(&root).is_err(), "{raw}");
        }
        host.answer_next_prompt(Some(&[""]));
        assert!(choose_source(&ctx).unwrap());
        assert_eq!(project_source(&root).unwrap(), "global/index.csv");
        assert!(host.requests().is_empty() && host.process_runs().is_empty());
    }

    #[test]
    fn invalidation_is_bounded_conservative_and_drops_closed_project_scope() {
        let host = MockHost::install();
        host.add_file(INDEX_FILE, ROW);
        let mut state = Snapshot::default();
        let ctx = host.invocation();
        state.refresh(&ctx);
        let root = ctx.project().unwrap().root().unwrap();
        let roots = BTreeSet::from([root]);
        assert!(state.invalidate_changes(&BTreeSet::new(), &roots, true));
        assert!(state.status.contains("profiler index changed"));
        assert!(!state.invalidate_changes(&BTreeSet::new(), &roots, true), "no duplicate invalidation");
        assert!(state.rows.is_some());
        assert!(state.invalidate_changes(&BTreeSet::new(), &BTreeSet::new(), false));
        assert!(state.rows.is_none());
        assert!(state.scope.is_empty());
        assert!(!state.invalidate_changes(&BTreeSet::new(), &BTreeSet::new(), true));
    }

    #[test]
    fn explicit_refresh_retains_last_good_only_for_matching_root() {
        let host = MockHost::install();
        host.add_file(INDEX_FILE, ROW);
        let mut state = Snapshot::default();
        let ctx = host.invocation();
        state.refresh(&ctx);
        assert_eq!(state.rows.as_ref().unwrap().len(), 1);
        host.add_file(INDEX_FILE, "malformed");
        state.refresh(&ctx);
        assert!(state.render(&ctx).status.unwrap().contains("previous Refresh"));
        assert_eq!(state.rows.as_ref().unwrap().len(), 1);
        host.set_project_root(Some("/other"));
        let other = host.invocation();
        assert!(matches!(state.render(&other).body, Body::Message(_)));
        state.refresh(&other);
        assert!(state.rows.is_none());
        host.set_project_root(None);
        state.refresh(&host.invocation());
        assert!(state.root.is_empty());
    }
    #[test]
    fn cached_render_does_not_adopt_file_changes_and_grant_loss_hides_data() {
        let host = MockHost::install();
        host.add_file(INDEX_FILE, ROW);
        let mut state = Snapshot::default();
        let ctx = host.invocation();
        state.refresh(&ctx);
        host.add_file(INDEX_FILE, "malformed");
        let view = state.render(&host.invocation());
        let Body::Table(table) = view.body else {
            panic!("cached table lost")
        };
        assert_eq!(table.rows[0][5], "abcdef");
        let host = host.with_capabilities(&[]);
        assert!(matches!(
            state.render(&host.invocation()).body,
            Body::Message(_)
        ));
        state.refresh(&host.invocation());
        assert!(state.rows.is_none());
        assert_eq!(
            read_error(Error::Denied("project.read".into())),
            "The IDE refused to read the profiler index. Check that it is inside the project, then Refresh."
        );
    }
    #[test]
    fn configured_paths_are_bounded_and_invalid_setting_never_falls_back_to_cached_default() {
        let host = MockHost::install();
        assert_eq!(configured_source().unwrap(), INDEX_FILE);
        for (raw, expected) in [
            ("  ", INDEX_FILE),
            (" cache/profiler.csv ", "cache/profiler.csv"),
        ] {
            host.set_setting(PATH_SETTING, &serde_json::to_string(raw).unwrap());
            assert_eq!(configured_source().unwrap(), expected);
        }
        let mut invalid = vec![
            "/outside.csv".to_owned(),
            "../outside.csv".into(),
            "cache/../index.csv".into(),
            "./index.csv".into(),
            "cache//index.csv".into(),
            "C:/cache.csv".into(),
            "cache\\index.csv".into(),
            "cache/*.csv".into(),
            "cache/index\n.csv".into(),
        ];
        invalid.push("x".repeat(4097));
        invalid.push(vec!["x"; 33].join("/"));
        for value in invalid {
            host.set_setting(PATH_SETTING, &serde_json::to_string(&value).unwrap());
            assert!(configured_source().is_err(), "{value}");
        }
        for raw in ["null", "42", "{}", "\"unterminated"] {
            host.set_setting(PATH_SETTING, raw);
            assert!(configured_source().is_err(), "{raw}");
        }
        host.set_setting(PATH_SETTING, &serde_json::to_string(INDEX_FILE).unwrap());
        host.add_file(INDEX_FILE, ROW);
        let mut state = Snapshot::default();
        state.refresh(&host.invocation());
        let key = state.row_key("abcdef");
        assert!(state.rows.is_some());
        host.set_setting(PATH_SETTING, r#""../other.csv""#);
        assert!(matches!(
            state.render_mode(&host.invocation(), true).body,
            Body::Message(_)
        ));
        assert!(state.open(&key, &host.invocation()).is_err());
        state.refresh(&host.invocation());
        assert!(state.rows.is_none());
        assert!(host.opened_locations().is_empty());
    }
    #[test]
    fn source_pair_and_opaque_keys_prevent_same_token_selection_retargeting() {
        let host = MockHost::install();
        host.add_file(INDEX_FILE, "abcdef,ip,GET,/default,1,,200\n");
        host.add_file("cache/profiler.csv", "abcdef,ip,GET,/custom,2,,201\n");
        let mut state = Snapshot::default();
        state.refresh(&host.invocation());
        let first = state.row_key("abcdef");
        state.refresh(&host.invocation());
        assert_eq!(
            state.row_key("abcdef"),
            first,
            "same source keeps selection identity"
        );
        host.set_setting(PATH_SETTING, r#""cache/profiler.csv""#);
        assert!(matches!(
            state.render_mode(&host.invocation(), true).body,
            Body::Message(_)
        ));
        assert!(state.open(&first, &host.invocation()).is_err());
        state.refresh(&host.invocation());
        let second = state.row_key("abcdef");
        assert_ne!(first, second);
        let Body::Other { data, .. } = state.render_mode(&host.invocation(), true).body else {
            panic!("packaged rows")
        };
        let data: serde_json::Value = serde_json::from_str(&data).unwrap();
        assert_eq!(data["items"][0]["key"], second);
        assert_eq!(
            data["items"][0]["details"][1]["value"],
            "cache/profiler.csv"
        );
        assert!(state.open(&first, &host.invocation()).is_err());
        state.open(&second, &host.invocation()).unwrap();
        assert_eq!(
            host.opened_locations(),
            [("/project/cache/profiler.csv".into(), 0, 0)]
        );
        host.add_file("cache/profiler.csv", "malformed");
        state.refresh(&host.invocation());
        assert!(state.status.contains("previous Refresh"));
        assert_eq!(state.row_key("abcdef"), second);
        assert!(state.open(&second, &host.invocation()).is_err());
        host.set_setting(PATH_SETTING, &serde_json::to_string(INDEX_FILE).unwrap());
        state.refresh(&host.invocation());
        assert_ne!(state.row_key("abcdef"), first);
        assert_ne!(state.row_key("abcdef"), second);
        assert!(state.open(&first, &host.invocation()).is_err());
        assert!(host.requests().is_empty() && host.process_runs().is_empty());
    }

    #[test]
    fn refresh_and_cached_details_refuse_index_owned_by_a_nested_project() {
        let host = MockHost::install();
        host.add_file(INDEX_FILE, ROW);
        let mut state = Snapshot::default();
        state.refresh(&host.invocation());
        assert!(matches!(
            state.render_mode(&host.invocation(), true).body,
            Body::Other { .. }
        ));
        host.add_project("/project/var/cache");
        assert!(
            matches!(
                state.render_mode(&host.invocation(), true).body,
                Body::Message(_)
            ),
            "cached rows/details must hide after source owner changes"
        );
        let mut cold = Snapshot::default();
        cold.refresh(&host.invocation());
        assert!(
            cold.rows.is_none(),
            "cold refresh must not adopt a nested project's index"
        );
        assert!(cold.status.contains("outside this project"));
        state.refresh(&host.invocation());
        assert!(matches!(
            state.render_mode(&host.invocation(), true).body,
            Body::Message(_)
        ));
        assert!(
            state
                .open(&state.row_key("abcdef"), &host.invocation())
                .is_err()
        );
        assert!(host.opened_locations().is_empty());
    }
    #[test]
    fn packaged_selected_details_are_source_backed_safe_and_filterable() {
        let host = MockHost::install();
        host.add_file(INDEX_FILE, ROW);
        let mut state = Snapshot::default();
        let ctx = host.invocation();
        state.refresh(&ctx);
        let Body::Other { kind, data } = state.render_mode(&ctx, true).body else {
            panic!("no resources")
        };
        assert_eq!(kind, "resources");
        let data: serde_json::Value = serde_json::from_str(&data).unwrap();
        assert_eq!(data["facets"], json!([1, 2]));
        assert_eq!(data["items"][0]["key"], state.row_key("abcdef"));
        assert_eq!(data["items"][0]["cells"][5], "abcdef");
        assert_eq!(
            data["items"][0]["details"][0]["value"],
            "Use Inspect Profile to see its collectors"
        );
        assert_eq!(data["items"][0]["details"][1]["value"], INDEX_FILE);
        assert!(!data.to_string().contains("password"));
        assert!(!data.to_string().contains("secret"));
    }
    #[test]
    fn missing_and_oversized_sources_have_distinct_truthful_states() {
        let host = MockHost::install();
        let mut state = Snapshot::default();
        state.refresh(&host.invocation());
        assert!(state.status.contains("No profiler data yet"));
        host.add_file(INDEX_FILE, &"x".repeat(MAX_BYTES + 1));
        state.refresh(&host.invocation());
        assert!(state.status.contains("1 MiB"));
        assert!(state.rows.is_none());
        assert!(
            !read_error(Error::Invalid("host missing-or-unreadable".into())).contains("No profiler data yet")
        );
    }
}
