//! Bounded text-only source import into a vacant current-project child.
use super::*;
use openidea_plugin::file_actions::{FileAction, FileOperation};
use openidea_plugin::providers::Document;
use openidea_plugin::{FileKind, HttpRequest, Project};
use std::collections::BTreeMap;
use std::hash::BuildHasher;
use std::io::{Cursor, Read};
use std::time::{Duration, Instant};
const MAX_ZIP: usize = 1 << 20;
const MAX_FILE: usize = 128 << 10;
/// Read as `plugin.aritti.spring-initializr.importReady` by the Tools menu entry of Open Source Folder.
pub(crate) const IMPORT_READY: &str = "importReady";
const WRAPPERS: [&str; 4] = ["mvnw", "mvnw.cmd", ".mvn/wrapper/maven-wrapper.properties", ".mvn/wrapper/maven-wrapper.jar"];
#[derive(Clone)]
struct Plan {
    root: String,
    parent: String,
    target: String,
    query: String,
    metadata: serde_json::Value,
    selector: Option<String>,
    files: BTreeMap<String, String>,
    excluded: Vec<String>,
    at: Instant,
    issued: bool,
}
#[derive(Default)]
pub(crate) struct State {
    plan: Option<Plan>,
    pending: BTreeMap<String, Document>,
}
impl State {
    pub(crate) fn invalidate(&mut self) {
        if self.plan.take().is_some_and(|plan| plan.issued) {
            host::remove_context(IMPORT_READY);
        }
        self.pending.clear();
    }
}
fn normal(path: &str) -> bool {
    !path.ends_with('/')
        && path.starts_with('/')
        && path.len() <= 4096
        && !path.contains(['\\', '\0'])
        && !path.contains("//")
        && !path.chars().any(char::is_control)
        && !path.split('/').any(|p| matches!(p, "." | ".."))
}
fn relative(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 256
        && !path.starts_with('/')
        && path.bytes().all(|b| b.is_ascii_alphanumeric() || b"/._-".contains(&b))
        && path.split('/').all(|p| {
            let stem = p.split('.').next().unwrap_or("").to_ascii_uppercase();
            !p.is_empty()
                && !matches!(p, "." | "..")
                && !p.ends_with('.')
                && !matches!(p, ".git" | ".idea" | ".openidea" | ".vscode")
                && !matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                && !(stem.len() == 4 && (stem.starts_with("COM") || stem.starts_with("LPT")) && matches!(stem.as_bytes()[3], b'1'..=b'9'))
        })
}
fn owned(project: &Project, root: &str, parent: &str) -> Result<(), Error> {
    if project.root()? != root
        || !same_owner(root, root)
        || !normal(root)
        || root == "/"
        || !normal(parent)
        || !(parent == root || parent.starts_with(&format!("{root}/")))
    {
        return Err(Error::Stale);
    }
    let mut path = parent;
    loop {
        if !same_owner(root, path) {
            return Err(Error::Stale);
        }
        if project.stat(path)?.kind != FileKind::Directory {
            return Err("Destination ancestry must be ordinary project directories.".into());
        }
        if path == root {
            break;
        }
        path = path.rsplit_once('/').map(|(p, _)| p).filter(|p| p.len() >= root.len()).ok_or(Error::Stale)?;
    }
    Ok(())
}
fn same_owner(root: &str, path: &str) -> bool {
    host::project_of(path).and_then(|project| project.root().ok()).as_deref() == Some(root)
}
fn vacant(project: &Project, plan: &Plan) -> Result<(), Error> {
    owned(project, &plan.root, &plan.parent)?;
    if !normal(&plan.target) {
        return Err("Joined destination path exceeds the supported absolute-path limits.".into());
    }
    if !same_owner(&plan.root, &plan.target) {
        return Err(Error::Stale);
    }
    if project.exists(&plan.target)? {
        return Err("Destination already exists; no merge, overwrite or partial extraction is allowed.".into());
    }
    for name in plan.files.keys() {
        let path = format!("{}/{name}", plan.target);
        if !normal(&path) {
            return Err("Joined source path exceeds the supported absolute-path limits.".into());
        }
        if !same_owner(&plan.root, &path) {
            return Err(Error::Stale);
        }
        if project.exists(&path)? {
            return Err("A destination file/buffer already exists; no edit prepared.".into());
        }
    }
    Ok(())
}
// Reject ZIP64/multidisk/large entry counts before the library can allocate from
// attacker-controlled directory counters. Initializr's ordinary ZIP form only.
fn classic_entries(bytes: &[u8]) -> Option<usize> {
    let end = bytes.len().checked_sub(22)?;
    if bytes.get(end..end + 4)? != b"PK\x05\x06" {
        return None;
    }
    let u16at = |at: usize| Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?));
    let u32at = |at: usize| Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?));
    let count = u16at(end + 10)? as usize;
    if count == 0
        || count > 64
        || u16at(end + 4)? != 0
        || u16at(end + 6)? != 0
        || u16at(end + 8)? as usize != count
        || u16at(end + 20)? != 0
    {
        return None;
    }
    let start = u32at(end + 16)? as usize;
    let size = u32at(end + 12)? as usize;
    if start.checked_add(size)? != end || bytes.get(..4)? != b"PK\x03\x04" {
        return None;
    }
    let mut at = start;
    for _ in 0..count {
        if bytes.get(at..at + 4)? != b"PK\x01\x02"
            || u16at(at + 34)? != 0
            || u16at(at + 8)? & !(8 | 0x800 | 6) != 0
            || !matches!(u16at(at + 10)?, 0 | 8)
            || u32at(at + 24)? as usize > MAX_FILE
            || u32at(at + 42)? as usize >= start
        {
            return None;
        }
        let names = u16at(at + 28)? as usize;
        let extra = u16at(at + 30)? as usize;
        let comment = u16at(at + 32)? as usize;
        let mut ex = at.checked_add(46)?.checked_add(names)?;
        let extra_end = ex.checked_add(extra)?;
        while ex < extra_end {
            let kind = u16at(ex)?;
            let len = u16at(ex + 2)? as usize;
            if kind == 1 {
                return None;
            }
            ex = ex.checked_add(4)?.checked_add(len)?;
            if ex > extra_end {
                return None;
            }
        }
        at = extra_end.checked_add(comment)?;
        if at > end {
            return None;
        }
    }
    (at == end).then_some(count)
}
fn unpack(bytes: &[u8]) -> Result<(BTreeMap<String, String>, Vec<String>), Error> {
    let invalid = || {
        Error::Invalid("Archive is unsupported: use a small unencrypted Maven source ZIP with safe unique paths, regular UTF-8 text and only the explicitly omitted Maven wrappers.".into())
    };
    if bytes.len() > MAX_ZIP {
        return Err(invalid());
    }
    let count = classic_entries(bytes).ok_or_else(invalid)?;
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| invalid())?;
    if zip.is_empty() || zip.len() != count || zip.has_duplicate_names() {
        return Err(invalid());
    }
    let (mut files, mut excluded, mut all, mut size) = (BTreeMap::new(), Vec::new(), BTreeMap::new(), 0u64);
    let mut spellings: BTreeMap<String, String> = BTreeMap::new();
    for index in 0..zip.len() {
        let file = zip.by_index(index).map_err(|_| invalid())?;
        let raw = std::str::from_utf8(file.name_raw()).map_err(|_| invalid())?.to_owned();
        let name = raw.strip_suffix('/').unwrap_or(&raw).to_owned();
        let mut prefix = String::new();
        for part in name.split('/') {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(part);
            if let Some(old) = spellings.insert(prefix.to_ascii_lowercase(), prefix.clone()) {
                if old != prefix {
                    return Err(invalid());
                }
            }
        }
        if !relative(&name)
            || file.encrypted()
            || file.is_symlink()
            || !matches!(file.compression(), zip::CompressionMethod::Stored | zip::CompressionMethod::Deflated)
            || file.size() > MAX_FILE as u64
            || file.size() > file.compressed_size().saturating_mul(100).saturating_add(1024)
        {
            return Err(invalid());
        }
        let mode = file.unix_mode().unwrap_or(0);
        let kind = mode & 0o170000;
        if !matches!(kind, 0 | 0o100000 | 0o040000) || file.is_dir() && kind == 0o100000 || !file.is_dir() && kind == 0o040000 {
            return Err(invalid());
        }
        if all.insert(name.to_ascii_lowercase(), file.is_dir()).is_some() {
            return Err(invalid());
        }
        size = size.checked_add(file.size()).ok_or_else(invalid)?;
        if size > 512 << 10 {
            return Err(invalid());
        }
        if file.is_dir() {
            continue;
        }
        let declared = file.size();
        let mut data = Vec::new();
        file.take((MAX_FILE + 1) as u64).read_to_end(&mut data).map_err(|_| invalid())?;
        if data.len() > MAX_FILE || data.len() as u64 != declared {
            return Err(invalid());
        }
        if (name.starts_with(".mvn/") || WRAPPERS.iter().any(|wrapper| wrapper.eq_ignore_ascii_case(&name)))
            && !WRAPPERS.contains(&name.as_str())
        {
            return Err(invalid());
        }
        if WRAPPERS.contains(&name.as_str()) {
            excluded.push(name);
            continue;
        }
        if mode & 0o777 != 0 && mode & 0o777 != 0o644 {
            return Err(invalid());
        }
        let text = String::from_utf8(data).map_err(|_| invalid())?;
        if text.chars().any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t')) || files.len() >= 32 {
            return Err(invalid());
        }
        files.insert(name, text);
    }
    for (name, is_dir) in &all {
        let mut parent = name.as_str();
        while let Some((p, _)) = parent.rsplit_once('/') {
            if all.get(p) == Some(&false) {
                return Err(invalid());
            }
            parent = p;
        }
        if *is_dir
            && !matches!(name.as_str(), ".mvn" | ".mvn/wrapper")
            && !files.keys().any(|f| f.to_ascii_lowercase().starts_with(&format!("{name}/")))
        {
            return Err(invalid());
        }
    }
    if files.values().map(String::len).sum::<usize>() > 12 << 10 {
        return Err(
            "Source content exceeds the complete exact-preview budget; archive retained only in memory and no source was written.".into()
        );
    }
    Ok((files, excluded))
}
fn preview(plan: &Plan) -> String {
    let mut body = format!(
        "Source-only Maven import\nProject: {}\nNew folder: {}\nDownload: {START}/starter.zip?{}\n\nKnown wrapper exclusions (not created): {}\nNo executable modes/wrapper binaries are imported. Use installed Maven separately. No process is run.\n\nExact complete source manifest and contents:\n",
        plan.root,
        plan.target,
        plan.query,
        if plan.excluded.is_empty() { "none present".into() } else { plan.excluded.join(", ") }
    );
    for (name, text) in &plan.files {
        body.push_str(&format!("\n--- {name} ({} UTF-8 bytes) ---\n{text}\n--- end {name} ---\n", text.len()));
    }
    body
}
/// Creates every file of a vacant plan, refusing any path that already exists.
fn create_files(project: &Project, plan: &Plan) -> Result<(), Error> {
    for (created, (name, text)) in plan.files.iter().enumerate() {
        let path = format!("{}/{name}", plan.target);
        let result = if project.exists(&path)? { Err(format!("{path} already exists and wasn't overwritten.").into()) } else { project.write_file(&path, text) };
        match result {
            Err(Error::Invalid(error) | Error::Failed(error)) if created > 0 => {
                return Err(format!("Created {created} of {} files in {}, then stopped: {error}", plan.files.len(), plan.target).into());
            }
            result => result?,
        }
    }
    Ok(())
}
fn verify_visible(project: &Project, plan: &Plan) -> Result<(), Error> {
    if host::project_of(&plan.target).and_then(|p| p.root().ok()).as_deref() != Some(&plan.root) {
        return Err(Error::Stale);
    }
    owned(&project, &plan.root, &plan.parent)?;
    if project.stat(&plan.target)?.kind != FileKind::Directory {
        return Err(Error::Stale);
    }
    for (name, text) in &plan.files {
        let path = format!("{}/{name}", plan.target);
        if !normal(&path) {
            return Err("Joined source path exceeds the supported absolute-path limits.".into());
        }
        if !same_owner(&plan.root, &path) {
            return Err(Error::Stale);
        }
        let mut parent = path.rsplit_once('/').map(|(p, _)| p).ok_or(Error::Stale)?;
        while parent != plan.parent {
            if project.stat(parent)?.kind != FileKind::Directory {
                return Err(Error::Stale);
            }
            parent = parent.rsplit_once('/').map(|(p, _)| p).ok_or(Error::Stale)?;
        }
        if project.stat(&path)?.kind != FileKind::File || project.read_file(&path)? != *text {
            return Err("Some imported files are missing or were changed, so the folder wasn't opened. Open it with File › Open… instead.".into());
        }
    }
    Ok(())
}
impl Launcher {
    pub(crate) fn import_issued(&self) -> bool {
        self.imports.plan.as_ref().is_some_and(|p| p.issued)
    }
    pub(crate) fn prepare_import(&mut self, ctx: &Invocation) -> Result<Option<String>, Error> {
        self.imports.invalidate();
        let project = ctx.project().ok_or("Open a project; this import only creates a vacant child inside that project.")?;
        let root = project.root()?;
        if self.owner != format!("project:{root}") || !normal(&root) || root == "/" {
            return Err(Error::Stale);
        }
        let _ = self.changed();
        if self.server != START {
            return Err("Source import supports the declared start.spring.io server only.".into());
        }
        let selector = host::storage::get(&session::locator(&self.owner, "selector"))?;
        if let Some(raw) = &selector {
            let value = crate::strict_json::parse(raw, session::MAX_SELECTOR).map_err(|_| Error::Stale)?;
            if raw.len() > session::MAX_SELECTOR || value["owner"].as_str() != Some(&self.owner) || value["server"].as_str() != Some(START) {
                return Err(Error::Stale);
            }
        }
        if session::selected_server(&self.owner) != START {
            return Err(Error::Stale);
        }
        self.load_metadata(false);
        let metadata = self.metadata.clone().ok_or("Load server metadata first.")?;
        if self.problem().is_some() {
            return Err(self.problem().unwrap().into());
        }
        for (key, expected) in [("type", "maven-project"), ("language", "java"), ("packaging", "jar")] {
            if self.get(key) != expected {
                return Err("Source-only import currently supports Maven / Java / Jar only.".into());
            }
        }
        let boot = self.effective_boot();
        let java = self.get("javaVersion");
        for (key, selected) in [
            ("type", "maven-project"),
            ("language", "java"),
            ("packaging", "jar"),
            ("bootVersion", boot.as_str()),
            ("javaVersion", java.as_str()),
        ] {
            let option = metadata[key]["values"]
                .as_array()
                .and_then(|values| values.iter().find(|v| v["id"].as_str() == Some(selected)))
                .ok_or("Every import choice must be advertised by saved metadata.")?;
            if compatibility::declared_range(option).is_some_and(|range| compatibility::allows(&boot, range) != Some(true)) {
                return Err("Import choice compatibility is unknown or excluded.".into());
            }
        }
        if self.get("dependencies").split([',', ' ']).filter(|id| !id.is_empty()).count() > 64 {
            return Err("Source import supports at most 64 selected plain Boot-managed starters.".into());
        }
        for id in self.get("dependencies").split([',', ' ']).filter(|s| !s.is_empty()) {
            let dep = metadata["dependencies"]["values"]
                .as_array()
                .into_iter()
                .flatten()
                .flat_map(|g| g["values"].as_array().into_iter().flatten())
                .find(|d| d["id"].as_str() == Some(id))
                .ok_or("Import dependency ID is not advertised by metadata.")?;
            if compatibility::declared_range(dep).is_some_and(|r| compatibility::allows(&boot, r) != Some(true)) {
                return Err("Import dependency compatibility is unknown or excluded.".into());
            }
        }
        let _ = self.cli_args()?;
        let artifact = self.get("artifactId");
        if !relative(&artifact) || artifact.contains('/') || artifact.starts_with('.') {
            return Err("Artifact must be one ordinary ASCII directory name.".into());
        }
        let parent = host::pick_directory("Choose a parent INSIDE this project for a vacant source-only child", Some(&root))?
            .ok_or(Error::Cancelled)?;
        owned(&project, &root, &parent)?;
        let query = self.query();
        if host::storage::get(&session::locator(&self.owner, "selector"))? != selector || session::selected_server(&self.owner) != START {
            return Err(Error::Stale);
        }
        let mut plan = Plan {
            root,
            parent: parent.clone(),
            target: format!("{parent}/{artifact}"),
            query,
            metadata,
            selector,
            files: BTreeMap::new(),
            excluded: Vec::new(),
            at: Instant::now(),
            issued: false,
        };
        vacant(&project, &plan)?;
        let url = format!("{START}/starter.zip?{}", plan.query);
        let response = host::fetch(&HttpRequest::get(&url))?;
        if response.status != 200 || response.body.len() > MAX_ZIP {
            return Err("Source archive request failed or exceeded 1 MiB; no source was written.".into());
        }
        (plan.files, plan.excluded) = unpack(&response.body)?;
        let pom = plan.files.get("pom.xml").ok_or("Archive has no root pom.xml.")?;
        if !add_starters::generated_pom(pom, &self.get("groupId"), &artifact, &boot, &self.get("javaVersion")) {
            return Err("Generated POM does not match the reviewed coordinates, Boot parent, explicit Java version, JAR packaging or supported static Maven form.".into());
        }
        let selected = self.get("dependencies");
        let ids: Vec<_> = selected.split([',', ' ']).filter(|id| !id.is_empty()).collect();
        if !ids.is_empty() {
            let coords = starters::get(&format!("/dependencies?bootVersion={boot}"))?;
            if !add_starters::generated_starters(pom, &ids, &boot, &coords) {
                return Err("Returned POM does not prove the selected plain Boot-managed starters and scopes for this Boot version; no import prepared.".into());
            }
        }
        let package = format!("src/main/java/{}/", self.get("packageName").replace('.', "/"));
        if !plan.files.keys().any(|f| f.starts_with(&package) && f.ends_with(".java")) {
            return Err("No source file under the selected Java package; no incomplete project imported.".into());
        }
        if preview(&plan).len() > 16384 {
            return Err("Complete manifest preview exceeds 16 KiB; import refused without writing source.".into());
        }
        vacant(&project, &plan)?;
        self.import_identity(&plan)?;
        let reason = if self.granted.as_ref().is_some_and(|g| !g.iter().any(|c| c == "project.write")) {
            ui_kit::denied::message("Creating the files directly", "project.write", "Spring Initializr Launcher")
        } else {
            let body = format!(
                "Folder: {}\nFiles: {}\n\nExisting files are never overwritten. The Maven wrapper (mvnw) is left out; use your installed Maven. Nothing is built or run.",
                plan.target,
                plan.files.keys().cloned().collect::<Vec<_>>().join(", ")
            );
            let name = plan.target.rsplit('/').next().unwrap_or_default();
            if !ui_kit::prompts::confirm_exact(&format!("Create {} files in {name}?", plan.files.len()), &body)? {
                return Err(Error::Cancelled);
            }
            self.import_identity(&plan)?;
            vacant(&project, &plan)?;
            match create_files(&project, &plan) {
                Err(Error::Denied(_)) => ui_kit::denied::message("Creating the files directly", "project.write", "Spring Initializr Launcher"),
                result => {
                    result?;
                    self.imports.plan = Some(plan);
                    self.issue_import();
                    return Ok(None);
                }
            }
        };
        let count = plan.files.len();
        self.imports.plan = Some(plan);
        Ok(Some(format!("Downloaded {count} files for {}. {reason} Or press ⌥⏎ in any open file of this project and choose Preview Import Maven Sources… to create them.", self.get("artifactId"))))
    }
    /// Marks the import created and offers Open Folder.
    fn issue_import(&mut self) {
        let Some(plan) = self.imports.plan.as_mut() else {
            return;
        };
        self.imports.pending.clear();
        plan.issued = true;
        let _ = host::set_context(IMPORT_READY, true);
        let _ = host::notify_with_actions(
            NotificationLevel::Info,
            "Spring Initializr",
            &format!("Created {} files in {}.", plan.files.len(), plan.target),
            &[openidea_plugin::NotificationAction::new("Open Folder", "open-reviewed-import")],
        );
    }
    fn import_identity(&mut self, plan: &Plan) -> Result<(), Error> {
        if self.owner != format!("project:{}", plan.root)
            || self.server != START
            || self.query() != plan.query
            || self.metadata.as_ref() != Some(&plan.metadata)
            || host::storage::get(&session::locator(&self.owner, "selector"))? != plan.selector
            || plan.at.elapsed() > Duration::from_secs(900)
        {
            return Err(Error::Stale);
        }
        Ok(())
    }
    pub(crate) fn import_file_actions(&mut self, doc: &Document) -> Vec<FileAction> {
        let Some(plan) = &self.imports.plan else {
            return Vec::new();
        };
        if plan.issued
            || plan.at.elapsed() > Duration::from_secs(900)
            || doc.text.len() > 128 << 10
            || host::project_of(&doc.path).and_then(|p| p.root().ok()).as_deref() != Some(&plan.root)
            || self.owner != format!("project:{}", plan.root)
        {
            return Vec::new();
        }
        if self.imports.pending.len() >= 4 {
            self.imports.pending.clear();
        }
        let random = std::collections::hash_map::RandomState::new();
        let token = format!("import:{:016x}{:016x}", random.hash_one(0u8), random.hash_one(1u8));
        self.imports.pending.insert(token.clone(), doc.clone());
        vec![FileAction {
            title: "Preview Import Maven Sources…".into(),
            kind: Some("source".into()),
            data: Some(token),
            ..FileAction::default()
        }]
    }
    pub(crate) fn resolve_import(&mut self, doc: &Document, action: FileAction) -> Result<FileAction, Error> {
        let token = action.data.as_deref().ok_or(Error::Stale)?;
        let anchor = self.imports.pending.get(token).ok_or(Error::Stale)?;
        if anchor.path != doc.path || anchor.version != doc.version || anchor.language != doc.language || anchor.text != doc.text {
            return Err(Error::Stale);
        }
        let plan = self.imports.plan.clone().ok_or(Error::Stale)?;
        if plan.issued {
            return Err(Error::Stale);
        }
        self.import_identity(&plan)?;
        let project = host::project_of(&doc.path).ok_or(Error::Stale)?;
        if project.root()? != plan.root || project.read_file(&doc.path)? != doc.text {
            return Err(Error::Stale);
        }
        vacant(&project, &plan)?;
        if !ui_kit::prompts::confirm_exact("Create these project files?", &preview(&plan))? {
            return Err(Error::Cancelled);
        }
        self.import_identity(&plan)?;
        vacant(&project, &plan)?;
        if project.read_file(&doc.path)? != doc.text {
            return Err(Error::Stale);
        }
        self.issue_import();
        Ok(FileAction {
            title: "Import Maven sources".into(),
            kind: Some("source".into()),
            operations: plan
                .files
                .into_iter()
                .map(|(path, text)| FileOperation::Create { path: format!("{}/{path}", plan.target), text, overwrite: false })
                .collect(),
            ..FileAction::default()
        })
    }
    pub(crate) fn open_reviewed_import(&mut self, ctx: &Invocation) -> Result<Option<String>, Error> {
        let plan = self
            .imports
            .plan
            .clone()
            .filter(|p| p.issued)
            .ok_or("Nothing to open yet. Run Import Maven Project… first.")?;
        let project = ctx.project().ok_or(Error::Stale)?;
        verify_visible(&project, &plan)?;
        host::open_project(&plan.target, true)?;
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{file_actions::FileActions, mock::MockHost};
    use std::io::Write;
    const POM: &str = "<project>\n<modelVersion>4.0.0</modelVersion>\n<parent><groupId>org.springframework.boot</groupId><artifactId>spring-boot-starter-parent</artifactId><version>3.5.16</version><relativePath/></parent>\n<groupId>com.example</groupId><artifactId>demo</artifactId><version>0.0.1-SNAPSHOT</version>\n<properties><java.version>17</java.version></properties>\n<dependencies>\n</dependencies>\n</project>\n";
    fn metadata() -> Vec<u8> {
        serde_json::json!({"bootVersion":{"default":"3.5.16","values":[{"id":"3.5.16"}]},"javaVersion":{"values":[{"id":"17"}]},"type":{"values":[{"id":"maven-project"}]},"language":{"values":[{"id":"java"}]},"packaging":{"values":[{"id":"jar"}]},"dependencies":{"values":[]}}).to_string().into_bytes()
    }
    fn archive(entries: &[(&str, &[u8], u32)]) -> Vec<u8> {
        archive_method(entries, zip::CompressionMethod::Stored)
    }
    fn archive_method(entries: &[(&str, &[u8], u32)], method: zip::CompressionMethod) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes, mode) in entries {
            zip.start_file(*name, zip::write::SimpleFileOptions::default().compression_method(method).unix_permissions(*mode)).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }
    fn valid() -> Vec<u8> {
        archive(&[
            ("pom.xml", POM.as_bytes(), 0o644),
            ("src/main/java/com/example/demo/DemoApplication.java", b"package com.example.demo; class DemoApplication {}", 0o644),
            (".gitignore", b"target/\n", 0o644),
            ("mvnw", b"#!/bin/sh\necho wrapper", 0o755),
            (".mvn/wrapper/maven-wrapper.jar", &[0, 1, 255], 0o644),
        ])
    }
    fn setup(bytes: Vec<u8>) -> (MockHost, Launcher, Document) {
        setup_with(bytes, &["storage", "project.read", "editor.read", "net:start.spring.io"])
    }
    fn setup_with(bytes: Vec<u8>, capabilities: &[&str]) -> (MockHost, Launcher, Document) {
        let host = MockHost::install().with_capabilities(capabilities);
        host.add_file("README.md", "anchor");
        host.open_editor("/project/README.md", "markdown", "anchor", &[]);
        host.on_fetch(move |req| {
            Ok(openidea_plugin::HttpResponse {
                status: 200,
                body: if req.url.contains("metadata/client") { metadata() } else { bytes.clone() },
                ..Default::default()
            })
        });
        let d = host.invocation().editor().unwrap().document().unwrap();
        let doc = Document { path: "/project/README.md".into(), language: "markdown".into(), version: d.version, text: d.text };
        (host, Launcher::default(), doc)
    }
    fn prepared() -> (MockHost, Launcher, Document) {
        let (host, mut l, d) = setup(valid());
        host.answer_next_prompt(Some(&["/project"]));
        host.answer_next_prompt(Some(&["yes"]));
        l.run_command("prepare-import", &host.invocation(), None).unwrap();
        (host, l, d)
    }
    fn resolve(host: &MockHost, l: &mut Launcher, d: &Document) -> Result<FileAction, Error> {
        let action = l.file_actions(d, Range::default()).into_iter().find(|a| a.title.contains("Import Maven Sources")).unwrap();
        host.answer_next_prompt(Some(&["yes"]));
        l.resolve_file_action(d, action)
    }
    fn mock_apply(host: &MockHost, action: &FileAction) {
        for op in &action.operations {
            let FileOperation::Create { path, text, overwrite: false } = op else { panic!() };
            let mut parent = path.rsplit_once('/').unwrap().0;
            while parent != "/project" {
                host.add_dir(parent);
                parent = parent.rsplit_once('/').unwrap().0;
            }
            host.add_file(path, text);
        }
    }
    #[test]
    fn generated_pom_requires_the_reviewed_starter_parent_model() {
        let parent="<parent><groupId>org.springframework.boot</groupId><artifactId>spring-boot-starter-parent</artifactId><version>3.5.16</version><relativePath/></parent>";
        let bom="<dependencyManagement><dependencies><dependency><groupId>org.springframework.boot</groupId><artifactId>spring-boot-dependencies</artifactId><version>3.5.16</version><type>pom</type><scope>import</scope></dependency></dependencies></dependencyManagement>";
        let pom=POM.replace(parent,bom);
        let bytes=archive(&[("pom.xml",pom.as_bytes(),0o644),("src/main/java/com/example/demo/DemoApplication.java",b"package com.example.demo; class DemoApplication {}",0o644)]);
        let (host,mut l,_)=setup(bytes);host.answer_next_prompt(Some(&["/project"]));host.answer_next_prompt(Some(&["yes"]));
        assert!(l.run_command("prepare-import",&host.invocation(),None).is_err());assert!(l.imports.plan.is_none());
    }
    #[test]
    fn starter_echo_requires_exact_version_and_plain_managed_mapping() {
        let pom=POM.replace("<dependencies>\n","<dependencies>\n<dependency><groupId>org.springframework.boot</groupId><artifactId>spring-boot-starter-web</artifactId><scope>runtime</scope></dependency>\n");
        let good=serde_json::json!({"bootVersion":"3.5.16","dependencies":{"web":{"groupId":"org.springframework.boot","artifactId":"spring-boot-starter-web","scope":"runtime"}}});
        assert!(add_starters::generated_starters(&pom,&["web"],"3.5.16",&good));
        for (key,value) in [("version",serde_json::json!("1.0")),("bom",serde_json::json!("other")),("repository",serde_json::json!("other")),("scope",serde_json::json!(false)),("unexpected",serde_json::json!("fact"))] {
            let mut bad=good.clone();bad["dependencies"]["web"][key]=value;
            assert!(!add_starters::generated_starters(&pom,&["web"],"3.5.16",&bad));
        }
        let mut bad=good.clone();bad["bootVersion"]=serde_json::json!("4.0.0");
        assert!(!add_starters::generated_starters(&pom,&["web"],"3.5.16",&bad));
        assert!(!add_starters::generated_starters(&pom,&["unknown"],"3.5.16",&good));
    }
    #[test]
    fn selected_starter_must_be_present_in_returned_pom_with_declared_scope() {
        for (dependency, ok) in [("",false),("<dependency><groupId>org.springframework.boot</groupId><artifactId>spring-boot-starter-web</artifactId><scope>test</scope></dependency>",false),("<dependency><groupId>org.springframework.boot</groupId><artifactId>spring-boot-starter-web</artifactId></dependency>",true)] {
            let pom=POM.replace("<dependencies>\n",&format!("<dependencies>\n{dependency}\n"));
            let bytes=archive(&[("pom.xml",pom.as_bytes(),0o644),("src/main/java/com/example/demo/DemoApplication.java",b"package com.example.demo; class DemoApplication {}",0o644)]);
            let (host,mut l,_)=setup(bytes.clone());
            host.on_fetch(move |request| {
                let body=if request.url.contains("metadata/client") {let mut m:serde_json::Value=serde_json::from_slice(&metadata()).unwrap();m["dependencies"]=serde_json::json!({"values":[{"values":[{"id":"web"}]}]});m.to_string().into_bytes()}
                else if request.url.contains("/dependencies?") {br#"{"bootVersion":"3.5.16","dependencies":{"web":{"groupId":"org.springframework.boot","artifactId":"spring-boot-starter-web"}}}"#.to_vec()}
                else {bytes.clone()};
                Ok(openidea_plugin::HttpResponse{status:200,body,..Default::default()})
            });
            l.bind(&host.invocation()).unwrap();l.set("dependencies","web");
            host.answer_next_prompt(Some(&["/project"]));host.answer_next_prompt(Some(&["yes"]));
            let result=l.run_command("prepare-import",&host.invocation(),None);
            assert_eq!(result.is_ok(),ok,"{dependency}");assert_eq!(l.imports.plan.is_some(),ok);
            assert!(host.opened_projects().is_empty());
        }
    }
    #[test]
    fn generated_pom_accepts_exact_java_with_default_or_explicit_jar() {
        for packaging in ["", "<packaging>jar</packaging>"] {
            let pom=POM.replace("<java.version>17</java.version>","<java.version>21</java.version>")
                .replace("<properties>",&format!("{packaging}<properties>"));
            assert!(add_starters::generated_pom(&pom,"com.example","demo","3.5.16","21"));
        }
    }
    #[test]
    fn generated_pom_must_echo_selected_java_and_jar_choices() {
        for pom in [POM.replace("<java.version>17</java.version>","<java.version>21</java.version>"),
            POM.replace("<java.version>17</java.version>",""),
            POM.replace("<properties>","<packaging>war</packaging><properties>")] {
            let bytes=archive(&[("pom.xml",pom.as_bytes(),0o644),("src/main/java/com/example/demo/DemoApplication.java",b"package com.example.demo; class DemoApplication {}",0o644)]);
            let (host,mut l,_)=setup(bytes);
            host.answer_next_prompt(Some(&["/project"]));host.answer_next_prompt(Some(&["yes"]));
            assert!(l.run_command("prepare-import",&host.invocation(),None).is_err());
            assert!(l.imports.plan.is_none());assert!(host.opened_projects().is_empty());
        }
    }
    #[test]
    fn joined_destination_paths_are_bounded_before_preparation() {
        for length in [4092, 4080] {
            let (host, mut l, _) = setup(valid());
            let mut parent = "/project".to_string();
            while parent.len() < length {
                let size = (length - parent.len() - 1).min(200);
                if size == 0 { break; }
                parent.push('/'); parent.push_str(&"a".repeat(size)); host.add_dir(&parent);
            }
            assert_eq!(parent.len(), length);
            host.answer_next_prompt(Some(&[&parent]));
            host.answer_next_prompt(Some(&["yes"]));
            assert!(l.run_command("prepare-import", &host.invocation(), None).is_err());
            assert!(l.imports.plan.is_none());
            assert!(host.opened_projects().is_empty());
            if length == 4092 { assert!(!host.requests().iter().any(|r|r.url.contains("starter.zip"))); }
        }
    }
    #[test]
    fn memory_only_preparation_atomic_manifest_and_separate_reviewed_open() {
        let (host, mut l, d) = prepared();
        assert!(host.opened_projects().is_empty());
        assert!(host.process_runs().is_empty());
        let plan = l.imports.plan.as_ref().unwrap();
        let body = preview(plan);
        assert!(body.contains(POM));
        assert!(body.contains("maven-wrapper.jar"));
        assert!(body.contains("mvnw"));
        assert_eq!(plan.files.len(), 3);
        let before = host.fetches();
        let action = resolve(&host, &mut l, &d).unwrap();
        assert_eq!(action.operations.len(), 3);
        assert_eq!(host.fetches(), before);
        assert!(!host::project_of(&d.path).unwrap().exists("/project/demo/pom.xml").unwrap());
        assert!(l.run_command("open-reviewed-import", &host.invocation(), None).is_err());
        assert!(host.opened_projects().is_empty());
        mock_apply(&host, &action);
        host.answer_next_prompt(Some(&["yes"]));
        l.run_command("open-reviewed-import", &host.invocation(), None).unwrap();
        assert_eq!(host.opened_projects(), vec![("/project/demo".into(), true)]);
        assert_eq!(host.editor_text(), "anchor");
    }
    #[test]
    fn created_import_enables_open_folder_menu_and_offers_it_in_a_notification() {
        let (host, mut l, d) = prepared();
        assert!(!host.context_keys().contains_key(IMPORT_READY));
        let error = l.run_command("open-reviewed-import", &host.invocation(), None).unwrap_err();
        assert!(error.to_string().contains("Nothing to open yet"), "{error}");
        resolve(&host, &mut l, &d).unwrap();
        assert!(host.context_keys().contains_key(IMPORT_READY));
        let notified = host.notifications_with_actions();
        assert_eq!(notified.last().unwrap().actions[0].command, "open-reviewed-import");
        l.imports.invalidate();
        assert!(!host.context_keys().contains_key(IMPORT_READY));
    }
    #[test]
    fn cancellation_and_outside_or_colliding_parent_prevent_archive_fetch() {
        for mode in 0..3 {
            let (host, mut l, _) = setup(valid());
            match mode {
                0 => host.answer_next_prompt(None),
                1 => host.answer_next_prompt(Some(&["/outside"])),
                _ => {
                    host.add_dir("demo");
                    host.answer_next_prompt(Some(&["/project"]));
                }
            }
            assert!(l.run_command("prepare-import", &host.invocation(), None).is_err());
            assert!(host.requests().iter().all(|r| !r.url.contains("/starter.zip")));
            assert!(l.imports.plan.is_none());
            assert!(host.opened_projects().is_empty());
        }
    }
    #[test]
    fn nested_parent_owner_refuses_import_before_archive_fetch() {
        let (host, mut l, _) = setup(valid());
        host.add_dir("/project/nested");
        host.add_project("/project/nested");
        host.answer_next_prompt(Some(&["/project/nested"]));
        host.answer_next_prompt(Some(&["yes"]));
        assert!(l.run_command("prepare-import", &host.invocation(), None).is_err());
        assert!(host.requests().iter().all(|r| !r.url.contains("/starter.zip")));
        assert!(l.imports.plan.is_none());
        assert!(host.opened_projects().is_empty());
        assert!(host.process_runs().is_empty());
    }
    #[test]
    fn prepared_import_rechecks_destination_owner_before_returning_creates() {
        for nested in ["/project/generated", "/project/generated/demo/src"] {
            let (host, mut l, d) = setup(valid());
            host.add_dir("/project/generated");
            host.answer_next_prompt(Some(&["/project/generated"]));
            host.answer_next_prompt(Some(&["yes"]));
            l.run_command("prepare-import", &host.invocation(), None).unwrap();
            let action = l.file_actions(&d, Range::default()).remove(0);
            host.add_project(nested);
            host.answer_next_prompt(Some(&["yes"]));
            assert!(l.resolve_file_action(&d, action).is_err(), "{nested}");
            assert!(!l.imports.plan.as_ref().unwrap().issued);
            assert!(!host::project_of(&d.path).unwrap().exists("/project/generated/demo/pom.xml").unwrap());
            assert!(host.opened_projects().is_empty());
            assert!(host.process_runs().is_empty());
        }
    }
    #[test]
    fn reviewed_open_refuses_an_expected_file_reassigned_to_a_nested_project() {
        let (host, mut l, d) = prepared();
        let action = resolve(&host, &mut l, &d).unwrap();
        mock_apply(&host, &action);
        host.add_project("/project/demo/src");
        host.answer_next_prompt(Some(&["yes"]));
        assert!(l.run_command("open-reviewed-import", &host.invocation(), None).is_err());
        assert!(host.opened_projects().is_empty());
        assert!(host.process_runs().is_empty());
    }
    #[test]
    fn stale_anchor_form_project_collision_and_cancel_never_prepare_creates() {
        for mode in 0..5 {
            let (host, mut l, mut d) = prepared();
            let action = l.file_actions(&d, Range::default()).remove(0);
            match mode {
                0 => d.version += 1,
                1 => l.set("artifactId", "other"),
                2 => host.set_project_root(Some("/other")),
                3 => host.add_dir("demo"),
                _ => host.answer_next_prompt(None),
            }
            assert!(l.resolve_file_action(&d, action).is_err());
            assert!(host.opened_projects().is_empty());
            assert!(host.process_runs().is_empty());
        }
    }
    #[test]
    fn reviewed_open_refuses_partial_or_changed_manifest_and_cancel() {
        for mode in 0..3 {
            let (host, mut l, d) = prepared();
            let action = resolve(&host, &mut l, &d).unwrap();
            mock_apply(&host, &action);
            match mode {
                0 => host.add_file("demo/pom.xml", "changed"),
                1 => host.add_file("demo/src", "file in place of directory"),
                _ => host.set_project_root(Some("/other")),
            }
            assert!(l.run_command("open-reviewed-import", &host.invocation(), None).is_err());
            assert!(host.opened_projects().is_empty());
        }
    }
    const WRITABLE: [&str; 5] = ["storage", "project.read", "project.write", "editor.read", "net:start.spring.io"];
    fn read(path: &str) -> Option<String> {
        host::project_of(path).unwrap().read_file(path).ok()
    }
    #[test]
    fn with_project_write_the_command_creates_the_files_after_one_confirm_and_offers_open_folder() {
        let (host, mut l, d) = setup_with(valid(), &WRITABLE);
        host.answer_next_prompt(Some(&["/project"]));
        host.answer_next_prompt(Some(&["yes"]));
        assert_eq!(l.run_command("prepare-import", &host.invocation(), None).unwrap(), None);
        assert_eq!(read("/project/demo/pom.xml").as_deref(), Some(POM));
        assert_eq!(read("/project/demo/.gitignore").as_deref(), Some("target/\n"));
        assert!(read("/project/demo/mvnw").is_none());
        assert!(l.import_issued());
        assert!(host.context_keys().contains_key(IMPORT_READY));
        let notified = host.notifications_with_actions();
        assert!(notified.last().unwrap().notification.body.contains("Created 3 files in /project/demo"));
        assert_eq!(notified.last().unwrap().actions[0].command, "open-reviewed-import");
        assert!(l.file_actions(&d, Range::default()).is_empty());
        for dir in ["/project/demo", "/project/demo/src", "/project/demo/src/main", "/project/demo/src/main/java", "/project/demo/src/main/java/com", "/project/demo/src/main/java/com/example", "/project/demo/src/main/java/com/example/demo"] {
            host.add_dir(dir);
        }
        l.run_command("open-reviewed-import", &host.invocation(), None).unwrap();
        assert_eq!(host.opened_projects(), vec![("/project/demo".into(), true)]);
    }
    #[test]
    fn cancelling_the_create_confirm_writes_nothing() {
        let (host, mut l, _) = setup_with(valid(), &WRITABLE);
        host.answer_next_prompt(Some(&["/project"]));
        host.answer_next_prompt(None);
        assert!(matches!(l.run_command("prepare-import", &host.invocation(), None), Err(Error::Cancelled)));
        assert!(read("/project/demo/pom.xml").is_none());
        assert!(l.imports.plan.is_none());
    }
    #[test]
    fn creating_files_never_overwrites_an_existing_file() {
        let (_, mut l, _) = prepared();
        let plan = l.imports.plan.take().unwrap();
        let host = MockHost::install().with_capabilities(&WRITABLE);
        host.add_file("demo/pom.xml", "mine");
        let error = create_files(&host::project_of("/project/README.md").unwrap(), &plan).unwrap_err();
        assert!(error.to_string().contains("already exists"), "{error}");
        assert_eq!(read("/project/demo/pom.xml").as_deref(), Some("mine"));
    }
    #[test]
    fn without_project_write_the_import_waits_for_alt_enter_with_the_standard_message() {
        let (host, mut l, _) = setup(valid());
        host.answer_next_prompt(Some(&["/project"]));
        host.answer_next_prompt(Some(&["yes"]));
        let message = l.run_command("prepare-import", &host.invocation(), None).unwrap().unwrap();
        assert!(message.contains("needs the project.write permission. Allow it in Settings › Plugins › Spring Initializr Launcher."), "{message}");
        assert!(message.contains("Preview Import Maven Sources…"), "{message}");
        assert!(read("/project/demo/pom.xml").is_none());
        assert!(!l.import_issued());
        let (host, mut l, _) = setup(valid());
        l.granted = Some(vec!["storage".into()]);
        host.answer_next_prompt(Some(&["/project"]));
        assert!(l.run_command("prepare-import", &host.invocation(), None).unwrap().unwrap().contains("project.write"));
        assert!(l.imports.plan.is_some());
    }
    #[test]
    fn unsafe_names_binary_modes_duplicate_case_prefix_and_size_are_rejected() {
        for entries in [
            vec![("../escape", b"x".as_slice(), 0o644)],
            vec![("/absolute", b"x".as_slice(), 0o644)],
            vec![("C:/drive", b"x".as_slice(), 0o644)],
            vec![(".git/config", b"x".as_slice(), 0o644)],
            vec![("CON.txt", b"x".as_slice(), 0o644)],
            vec![("binary.jar", [0, 255].as_slice(), 0o644)],
            vec![("run.sh", b"echo unsafe".as_slice(), 0o755)],
            vec![("A/x", b"x".as_slice(), 0o644), ("a/y", b"y".as_slice(), 0o644)],
            vec![("a", b"x".as_slice(), 0o644), ("a/b", b"y".as_slice(), 0o644)],
        ] {
            assert!(unpack(&archive(&entries)).is_err(), "{entries:?}");
        }
        let large = "x".repeat((12 << 10) + 1);
        assert!(unpack(&archive(&[("large", large.as_bytes(), 0o644)])).is_err());
        let entries: Vec<_> = (0..65).map(|i| (format!("f{i}"), vec![0u8])).collect();
        let refs: Vec<_> = entries.iter().map(|(n, b)| (n.as_str(), b.as_slice(), 0o644)).collect();
        assert!(unpack(&archive(&refs)).is_err());
    }
    #[test]
    fn deflate_reads_crc_but_compression_bombs_and_unknown_maven_config_refuse() {
        let compressed = archive_method(&[("pom.xml", POM.as_bytes(), 0o644)], zip::CompressionMethod::Deflated);
        assert_eq!(unpack(&compressed).unwrap().0["pom.xml"], POM);
        let bomb = "x".repeat(20000);
        assert!(unpack(&archive_method(&[("mvnw", bomb.as_bytes(), 0o755)], zip::CompressionMethod::Deflated)).is_err());
        assert!(unpack(&archive(&[(".mvn/maven.config", b"-Punknown", 0o644)])).is_err());
    }
    #[test]
    fn corrupt_duplicate_zip64_multidisk_and_symlink_archives_refuse() {
        let base = archive(&[("a.txt", b"first", 0o644), ("b.txt", b"other", 0o644)]);
        let mut duplicate = base.clone();
        for i in 0..duplicate.len() - 5 {
            if &duplicate[i..i + 5] == b"b.txt" {
                duplicate[i] = b'a';
            }
        }
        assert!(unpack(&duplicate).is_err());
        let mut corrupt = base.clone();
        let at = corrupt.windows(5).position(|b| b == b"first").unwrap();
        corrupt[at] = b'X';
        assert!(unpack(&corrupt).is_err());
        for mode in 0..3 {
            let mut malformed = base.clone();
            let end = malformed.len() - 22;
            match mode {
                0 => malformed[end + 4] = 1,
                1 => {
                    malformed[end + 10] = 255;
                    malformed[end + 11] = 255;
                }
                _ => malformed.push(0),
            }
            assert!(unpack(&malformed).is_err());
        }
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        zip.add_symlink("link", "../outside", zip::write::SimpleFileOptions::default()).unwrap();
        assert!(unpack(&zip.finish().unwrap().into_inner()).is_err());
    }
    #[test]
    fn incompatible_missing_pom_or_wrong_coordinate_archive_never_adopts_plan() {
        for bytes in [
            archive(&[("README.md", b"missing project", 0o644)]),
            archive(&[("pom.xml", POM.replace("<artifactId>demo</artifactId>", "<artifactId>other</artifactId>").as_bytes(), 0o644)]),
            archive(&[("pom.xml", POM.as_bytes(), 0o644)]),
        ] {
            let (host, mut l, _) = setup(bytes);
            host.answer_next_prompt(Some(&["/project"]));
            host.answer_next_prompt(Some(&["yes"]));
            assert!(l.run_command("prepare-import", &host.invocation(), None).is_err());
            assert!(l.imports.plan.is_none());
            assert!(host.opened_projects().is_empty());
        }
    }
}
